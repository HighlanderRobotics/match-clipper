use crate::logging;
use crate::{
    authenticate_scope,
    cli::ClipArgs,
    clip::{self, Plan},
    load_google_client, upload,
};
use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Status {
    Planned,
    Clipped,
    Uploading,
    Uploaded,
    ClipFailed,
    UploadFailed,
}
#[derive(Debug, Serialize, Deserialize)]
struct Progress {
    match_key: String,
    status: Status,
    uploaded_video_id: Option<String>,
    error: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    plan: Plan,
    progress: Vec<Progress>,
}
async fn save(path: &Path, manifest: &Manifest) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, serde_json::to_vec_pretty(manifest)?).await?;
    tokio::fs::rename(tmp, path).await?;
    Ok(())
}
fn validate_manifest(m: &Manifest, plan: &Plan) -> Result<()> {
    if m.version != 1
        || m.plan != *plan
        || m.progress.len() != plan.clips.len()
        || m.progress
            .iter()
            .zip(&plan.clips)
            .any(|(p, c)| p.match_key != c.match_key)
    {
        bail!("Existing manifest has a different plan. Use the original timing/privacy options or a new --output directory");
    }
    for p in &m.progress {
        if p.status == Status::Uploaded
            && !p
                .uploaded_video_id
                .as_ref()
                .is_some_and(|id| clip::normalize_youtube_video_id(id).is_ok())
        {
            bail!("Uploaded entry {} has no valid video ID", p.match_key);
        }
        if matches!(p.status, Status::Uploading | Status::UploadFailed) {
            bail!("{} has an uncertain upload. Check YouTube Studio before resuming. If uploaded, set status to uploaded and uploaded_video_id in manifest.json; otherwise set status to clipped. This prevents duplicate uploads",p.match_key);
        }
    }
    Ok(())
}
fn needs_upload(no_upload: bool, manifest: &Manifest) -> bool {
    !no_upload
        && manifest
            .progress
            .iter()
            .any(|p| p.status != Status::Uploaded)
}
pub async fn run(args: ClipArgs) -> Result<()> {
    let http = Client::builder()
        .user_agent(concat!("frc-match-clipper/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(120))
        .build()?;
    let plan = clip::load_plan(&http, &args).await?;
    if args.dry_run {
        println!("{}", serde_json::to_string_pretty(&plan)?);
        return Ok(());
    }
    let dir = args
        .output
        .clone()
        .unwrap_or_else(|| Path::new("clips").join(&args.event).join(&plan.video_id));
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join("manifest.json");
    let lock = dir.join(".running");
    let mut lockfile=tokio::fs::OpenOptions::new().write(true).create_new(true).open(&lock).await.context("Another run owns this output directory. If a prior process crashed, remove .running after confirming it has stopped")?;
    use tokio::io::AsyncWriteExt;
    lockfile
        .write_all(std::process::id().to_string().as_bytes())
        .await?;
    let result = run_locked(&args, &http, &plan, &dir, &path).await;
    drop(lockfile);
    let cleanup = tokio::fs::remove_file(lock).await;
    result?;
    cleanup?;
    Ok(())
}
async fn run_locked(
    args: &ClipArgs,
    http: &Client,
    plan: &Plan,
    dir: &Path,
    path: &Path,
) -> Result<()> {
    let mut manifest = if tokio::fs::try_exists(path).await? {
        let m: Manifest = serde_json::from_slice(&tokio::fs::read(path).await?)?;
        validate_manifest(&m, plan)?;
        m
    } else {
        Manifest {
            version: 1,
            plan: plan.clone(),
            progress: plan
                .clips
                .iter()
                .map(|c| Progress {
                    match_key: c.match_key.clone(),
                    status: Status::Planned,
                    uploaded_video_id: None,
                    error: None,
                })
                .collect(),
        }
    };
    save(path, &manifest).await?;
    let google = if !needs_upload(args.no_upload, &manifest)
        && (args.no_upload || args.playlist.is_none())
    {
        None
    } else {
        Some(load_google_client()?)
    };
    // Authenticate before downloading the first clip; refresh again before each upload.
    if let Some(g) = &google {
        let token = authenticate_scope(http, g, args.playlist.is_some()).await?;
        if let Some(title) = &args.playlist {
            crate::playlist::sync(http, &token, dir, title, &[]).await?;
        }
    }
    let started = std::time::Instant::now();
    let mut resumed = 0;
    logging::info(
        "batch_start",
        &format!(
            "{} · {} matches · {} · output {}",
            plan.event,
            plan.clips.len(),
            plan.privacy,
            dir.display()
        ),
        json!({"matches":plan.clips.len(),"output":dir,"privacy":plan.privacy}),
    );
    if plan.skipped_without_actual_time > 0 {
        logging::warn(
            "missing_timestamps",
            &format!(
                "{} matches have no actual start timestamp and were skipped",
                plan.skipped_without_actual_time
            ),
            json!({"skipped":plan.skipped_without_actual_time}),
        );
    }
    let mut failed = 0;
    for (i, c) in plan.clips.iter().enumerate() {
        if manifest.progress[i].status == Status::Uploaded {
            if let (Some(g), Some(title)) = (&google, &args.playlist) {
                let token = authenticate_scope(http, g, true).await?;
                crate::playlist::sync(
                    http,
                    &token,
                    dir,
                    title,
                    &[manifest.progress[i].uploaded_video_id.clone().unwrap()],
                )
                .await?;
            }
            resumed += 1;
            logging::info(
                "clip_resumed",
                &format!(
                    "[{}/{}] {} · already uploaded; skipping",
                    i + 1,
                    plan.clips.len(),
                    c.match_key
                ),
                json!({"match":c.match_key,"videoId":manifest.progress[i].uploaded_video_id}),
            );
            continue;
        }
        let file = dir.join(format!("{}.mp4", c.match_key));
        if manifest.progress[i].status != Status::Clipped || !tokio::fs::try_exists(&file).await? {
            let clip_started = std::time::Instant::now();
            logging::info(
                "clip_start",
                &format!(
                    "[{}/{}] {} · downloading {}–{}s",
                    i + 1,
                    plan.clips.len(),
                    c.match_key,
                    c.start_seconds,
                    c.end_seconds
                ),
                json!({"match":c.match_key,"index":i+1,"total":plan.clips.len(),"startSeconds":c.start_seconds,"endSeconds":c.end_seconds}),
            );
            if let Err(e) = clip::download_clip(&plan.video_id, c, &file).await {
                manifest.progress[i].status = Status::ClipFailed;
                manifest.progress[i].error = Some(e.to_string());
                save(path, &manifest).await?;
                logging::error(
                    "clip_failed",
                    &format!("{} · {e:#}", c.match_key),
                    json!({"match":c.match_key}),
                );
                failed += 1;
                continue;
            }
            let bytes = tokio::fs::metadata(&file).await?.len();
            logging::info(
                "clip_ready",
                &format!(
                    "{} · clip ready · {:.1} MB · {:.1}s",
                    c.match_key,
                    bytes as f64 / 1048576.0,
                    clip_started.elapsed().as_secs_f64()
                ),
                json!({"match":c.match_key,"bytes":bytes,"seconds":clip_started.elapsed().as_secs_f64()}),
            );
            manifest.progress[i].status = Status::Clipped;
            manifest.progress[i].error = None;
            save(path, &manifest).await?;
        }
        if let Some(g) = &google {
            let token = authenticate_scope(http, g, args.playlist.is_some()).await?;
            manifest.progress[i].status = Status::Uploading;
            save(path, &manifest).await?;
            let title = format!(
                "{} {}",
                args.event,
                c.match_key
                    .strip_prefix(&format!("{}_", args.event))
                    .unwrap_or(&c.match_key)
                    .to_uppercase()
            );
            let description=format!("FIRST Robotics Competition {}\nMatch: {}\nSource: https://www.youtube.com/watch?v={}\nVOD range: {}–{} seconds",args.event,c.match_key,plan.video_id,c.start_seconds,c.end_seconds);
            match upload(http, &token, &file, &title, &description, &args.privacy).await {
                Ok(id) => {
                    manifest.progress[i].status = Status::Uploaded;
                    manifest.progress[i].uploaded_video_id = Some(id.clone());
                    save(path, &manifest).await?;
                    logging::info(
                        "upload_complete",
                        &format!(
                            "{} · uploaded · https://www.youtube.com/watch?v={id}",
                            c.match_key
                        ),
                        json!({"match":c.match_key,"videoId":id}),
                    );
                    if let Some(title) = &args.playlist {
                        crate::playlist::sync(http, &token, dir, title, &[id]).await?;
                    }
                    if !args.keep_clips {
                        tokio::fs::remove_file(&file).await?;
                    }
                }
                Err(e) => {
                    manifest.progress[i].status = if e.is::<crate::UploadNotStarted>() {
                        Status::Clipped
                    } else {
                        Status::UploadFailed
                    };
                    manifest.progress[i].error = Some(e.to_string());
                    save(path, &manifest).await?;
                    let message = if e.is::<crate::UploadNotStarted>() {
                        "Upload rejected before transfer; clip retained. Resolve the provider limit, then rerun the same command"
                    } else {
                        "Upload stopped; clip retained. Check YouTube Studio before retrying to prevent duplicates"
                    };
                    return Err(e.context(message));
                }
            }
        }
    }
    if failed > 0 {
        bail!("{failed} clips failed. Successful uploads are saved; rerun the same command to retry failed downloads");
    }
    let uploaded = manifest
        .progress
        .iter()
        .filter(|p| p.status == Status::Uploaded)
        .count();
    logging::info("batch_complete",&format!("Complete · {} matches · {uploaded} uploaded · {resumed} resumed · {:.1}s · manifest {}",plan.clips.len(),started.elapsed().as_secs_f64(),path.display()),json!({"matches":plan.clips.len(),"uploaded":uploaded,"resumed":resumed,"failed":failed,"seconds":started.elapsed().as_secs_f64(),"manifest":path}));
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn manifest() -> Manifest {
        Manifest {
            version: 1,
            plan: Plan {
                event: "2026cacg".into(),
                video_id: "abcdefghijk".into(),
                privacy: "unlisted".into(),
                clips: vec![clip::PlannedClip {
                    match_key: "2026cacg_qm1".into(),
                    start_seconds: 0,
                    end_seconds: 186,
                }],
                skipped_without_actual_time: 0,
            },
            progress: vec![Progress {
                match_key: "2026cacg_qm1".into(),
                status: Status::Planned,
                uploaded_video_id: None,
                error: None,
            }],
        }
    }
    #[test]
    fn completed_manifests_do_not_require_google_login() {
        let mut m = manifest();
        assert!(needs_upload(false, &m));
        assert!(!needs_upload(true, &m));
        m.progress[0].status = Status::Uploaded;
        m.progress[0].uploaded_video_id = Some("abcdefghijk".into());
        assert!(!needs_upload(false, &m));
    }
    #[test]
    fn resume_requires_same_plan() {
        let m = manifest();
        assert!(validate_manifest(&m, &m.plan).is_ok());
        let mut p = m.plan.clone();
        p.clips[0].start_seconds = 1;
        assert!(validate_manifest(&m, &p).is_err());
    }
    #[test]
    fn uncertain_upload_blocks_duplicates() {
        let mut m = manifest();
        for status in [Status::Uploading, Status::UploadFailed] {
            m.progress[0].status = status;
            assert!(validate_manifest(&m, &m.plan).is_err());
        }
        m.progress[0].status = Status::Uploaded;
        assert!(validate_manifest(&m, &m.plan).is_err());
        m.progress[0].uploaded_video_id = Some("abcdefghijk".into());
        assert!(validate_manifest(&m, &m.plan).is_ok());
    }
    #[tokio::test]
    async fn checkpoint_roundtrip() {
        let root = std::env::temp_dir().join(format!(
            "frc-clipper-test-{}-{}",
            std::process::id(),
            crate::random_string(8)
        ));
        tokio::fs::create_dir(&root).await.unwrap();
        let p = root.join("manifest.json");
        let m = manifest();
        save(&p, &m).await.unwrap();
        let loaded: Manifest = serde_json::from_slice(&tokio::fs::read(&p).await.unwrap()).unwrap();
        validate_manifest(&loaded, &m.plan).unwrap();
        assert!(!root.join("manifest.json.tmp").exists());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
