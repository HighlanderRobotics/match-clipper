use crate::cli::ClipArgs;
use anyhow::{bail, Context, Result};
use chrono::DateTime;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{env, path::Path, process::Stdio};
use tokio::process::Command;
use url::Url;

#[derive(Debug, Deserialize, Clone)]
pub struct TbaMatch {
    pub key: String,
    pub actual_time: Option<i64>,
}
#[derive(Deserialize)]
struct YoutubeResponse {
    items: Vec<YoutubeVideo>,
}
#[derive(Deserialize)]
struct YoutubeVideo {
    #[serde(rename = "liveStreamingDetails")]
    live: Option<LiveDetails>,
}
#[derive(Deserialize)]
struct LiveDetails {
    #[serde(rename = "actualStartTime")]
    start: Option<String>,
    #[serde(rename = "actualEndTime")]
    end: Option<String>,
}
#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct PlannedClip {
    pub match_key: String,
    pub start_seconds: i64,
    pub end_seconds: i64,
}
#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct Plan {
    pub event: String,
    pub video_id: String,
    pub privacy: String,
    pub clips: Vec<PlannedClip>,
    pub skipped_without_actual_time: usize,
}
pub fn normalize_youtube_video_id(input: &str) -> Result<String> {
    let id = if input.contains("://") {
        let url = Url::parse(input).context("Invalid YouTube URL")?;
        if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
            bail!("Use an HTTPS YouTube URL");
        }
        match url.host_str().unwrap_or_default() {
            "youtu.be" => url
                .path_segments()
                .and_then(|mut p| p.next())
                .map(str::to_owned),
            "youtube.com" | "www.youtube.com" | "m.youtube.com" => url
                .query_pairs()
                .find(|(k, _)| k == "v")
                .map(|(_, v)| v.into_owned())
                .or_else(|| {
                    let mut p = url.path_segments()?;
                    if ["live", "embed"].contains(&p.next()?) {
                        p.next().map(str::to_owned)
                    } else {
                        None
                    }
                }),
            _ => None,
        }
        .context("Not a supported YouTube livestream URL")?
    } else {
        input.to_owned()
    };
    if id.len() != 11
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        bail!("Invalid YouTube video ID");
    }
    Ok(id)
}
pub async fn load_plan(http: &Client, args: &ClipArgs) -> Result<Plan> {
    let video_id = normalize_youtube_video_id(&args.livestream)?;
    let tba_key = env::var("TBA_API_KEY")
        .or_else(|_| env::var("TBA_KEY"))
        .context("Set TBA_API_KEY or TBA_KEY")?;
    let youtube_key = env::var("YOUTUBE_API_KEY").context("Set YOUTUBE_API_KEY")?;
    let matches: Vec<TbaMatch> = http
        .get(format!(
            "https://www.thebluealliance.com/api/v3/event/{}/matches",
            args.event
        ))
        .header("X-TBA-Auth-Key", tba_key)
        .send()
        .await
        .context("TBA request failed")?
        .error_for_status()?
        .json()
        .await?;
    // Do not include the API-key-bearing request URL in error messages.
    let response = http
        .get("https://www.googleapis.com/youtube/v3/videos")
        .query(&[
            ("part", "liveStreamingDetails"),
            ("id", video_id.as_str()),
            ("key", youtube_key.as_str()),
        ])
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("YouTube metadata request failed"))?;
    if !response.status().is_success() {
        bail!("YouTube metadata returned {}", response.status());
    }
    let data: YoutubeResponse = response.json().await.context("Invalid YouTube metadata")?;
    let details = data
        .items
        .into_iter()
        .next()
        .and_then(|v| v.live)
        .context("Video is not an archived livestream")?;
    let timestamp = |s: Option<String>| -> Result<i64> {
        Ok(DateTime::parse_from_rfc3339(
            &s.context("Livestream start/end timestamp missing; wait for the stream to finish")?,
        )?
        .timestamp())
    };
    let start = timestamp(details.start)?;
    let end = timestamp(details.end)?;
    plan_matches(args, video_id, start, end, &matches)
}
fn full_key(event: &str, key: &str) -> String {
    if key.starts_with(&format!("{event}_")) {
        key.to_owned()
    } else if key.bytes().all(|b| b.is_ascii_digit()) {
        format!("{event}_qm{key}")
    } else {
        format!("{event}_{key}")
    }
}
pub fn plan_matches(
    args: &ClipArgs,
    video_id: String,
    stream_start: i64,
    stream_end: i64,
    matches: &[TbaMatch],
) -> Result<Plan> {
    if stream_end <= stream_start {
        bail!("Invalid livestream interval");
    }
    let mut rows: Vec<_> = matches
        .iter()
        .filter(|m| m.key.starts_with(&format!("{}_", args.event)))
        .filter_map(|m| m.actual_time.map(|t| (m, t)))
        .filter(|(_, t)| *t >= stream_start && *t < stream_end)
        .collect();
    rows.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.key.cmp(&b.0.key)));
    let bound = |key: &Option<String>, default: usize| -> Result<usize> {
        match key {
            Some(k) => rows
                .iter()
                .position(|(m, _)| m.key == full_key(&args.event, k))
                .with_context(|| format!("Match {k} has no actual start inside this livestream")),
            None => Ok(default),
        }
    };
    if rows.is_empty() {
        bail!("No played matches fall inside the livestream; check event and stream");
    }
    let first = bound(&args.start, 0)?;
    let last = bound(&args.stop, rows.len() - 1)?;
    if first > last {
        bail!("Start match occurs after stop match");
    }
    let stream_duration = stream_end - stream_start;
    let mut clips = Vec::new();
    for (m, t) in &rows[first..=last] {
        let zero = t - stream_start + args.offset;
        if zero < 0 || zero >= stream_duration {
            bail!("Offset moves {} outside the VOD", m.key);
        }
        let start = (zero - args.lead_in).max(0);
        let end = (zero + args.duration + args.post_roll).min(stream_duration);
        if end - zero < args.duration {
            bail!(
                "Livestream ends before {} finishes; use another VOD or narrow match range",
                m.key
            );
        }
        // Results are deliberately not used as match end: scoring often posts well after play stops.
        clips.push(PlannedClip {
            match_key: m.key.clone(),
            start_seconds: start,
            end_seconds: end,
        });
    }
    Ok(Plan {
        event: args.event.clone(),
        video_id,
        privacy: args.privacy.clone(),
        clips,
        skipped_without_actual_time: matches.iter().filter(|m| m.actual_time.is_none()).count(),
    })
}
pub async fn download_clip(video_id: &str, clip: &PlannedClip, path: &Path) -> Result<()> {
    let partial = path.with_extension("partial.mp4");
    let status = Command::new("yt-dlp")
        .args([
            "--ignore-config",
            "--no-playlist",
            "--no-progress",
            "--no-part",
            "--force-overwrites",
            "--js-runtimes",
            "node",
            "--socket-timeout",
            "20",
            "--retries",
            "2",
            "--download-sections",
        ])
        .arg(format!("*{}-{}", clip.start_seconds, clip.end_seconds))
        .args([
            "--force-keyframes-at-cuts",
            "--downloader-args",
            "ffmpeg_o:-c:v libx264 -preset veryfast -crf 23 -threads 2 -r 30 -loglevel error -nostats",
            "-f",
            "bestvideo[vcodec^=avc1][height<=720]+bestaudio[ext=m4a]/best[ext=mp4][height<=720]/best[height<=720]",
            "--merge-output-format",
            "mp4",
            "--remux-video",
            "mp4",
            "-o",
        ])
        .arg(&partial)
        .arg("--")
        .arg(format!("https://www.youtube.com/watch?v={video_id}"))
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .status();
    let result = tokio::time::timeout(std::time::Duration::from_secs(900), status).await;
    match result {
        Ok(Ok(s)) if s.success() => {}
        _ => {
            let _ = tokio::fs::remove_file(&partial).await;
            bail!("Download failed or timed out for {}", clip.match_key);
        }
    }
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(&partial)
        .output()
        .await
        .context("ffprobe is required")?;
    let duration = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<f64>()
        .unwrap_or(0.0);
    let expected = (clip.end_seconds - clip.start_seconds) as f64;
    if !output.status.success() || !duration.is_finite() || (duration - expected).abs() > 3.0 {
        let _ = tokio::fs::remove_file(&partial).await;
        bail!(
            "Clip duration {duration:.1}s differs from planned {expected:.1}s for {}",
            clip.match_key
        );
    }
    tokio::fs::rename(partial, path).await?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn args() -> ClipArgs {
        let crate::cli::Command::Clip(c) = crate::cli::parse(
            "clip abcdefghijk --event 2026cacg"
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
        )
        .unwrap() else {
            panic!()
        };
        c
    }
    fn m(n: i64, t: Option<i64>) -> TbaMatch {
        TbaMatch {
            key: format!("2026cacg_qm{n}"),
            actual_time: t,
        }
    }
    #[test]
    fn youtube_urls() {
        for u in [
            "abcdefghijk",
            "https://youtu.be/abcdefghijk?t=3",
            "https://www.youtube.com/live/abcdefghijk",
            "https://www.youtube.com/watch?v=abcdefghijk",
        ] {
            assert_eq!(normalize_youtube_video_id(u).unwrap(), "abcdefghijk");
        }
        for u in [
            "x",
            "https://youtube.com.evil/live/abcdefghijk",
            "http://youtube.com/watch?v=abcdefghijk",
            "https://evil.youtube.com/watch?v=abcdefghijk",
            "../../token.json",
        ] {
            assert!(normalize_youtube_video_id(u).is_err());
        }
    }
    #[test]
    fn chronological_plan_and_padding() {
        let p = plan_matches(
            &args(),
            "abcdefghijk".into(),
            1000,
            3000,
            &[
                m(2, Some(1600)),
                m(1, Some(1100)),
                m(3, None),
                m(4, Some(4000)),
            ],
        )
        .unwrap();
        assert_eq!(p.clips.len(), 2);
        assert_eq!(
            p.clips[0],
            PlannedClip {
                match_key: "2026cacg_qm1".into(),
                start_seconds: 92,
                end_seconds: 278
            }
        );
        assert_eq!(p.skipped_without_actual_time, 1);
    }
    #[test]
    fn boundaries_and_offsets() {
        let mut a = args();
        a.start = Some("2".into());
        a.offset = -20;
        let p = plan_matches(
            &a,
            "abcdefghijk".into(),
            1000,
            3000,
            &[m(1, Some(1100)), m(2, Some(1600))],
        )
        .unwrap();
        assert_eq!(p.clips[0].start_seconds, 572);
        a.stop = Some("1".into());
        assert!(plan_matches(
            &a,
            "abcdefghijk".into(),
            1000,
            3000,
            &[m(1, Some(1100)), m(2, Some(1600))]
        )
        .is_err());
    }
    #[test]
    fn reject_missing_or_truncated_matches() {
        assert!(plan_matches(
            &args(),
            "abcdefghijk".into(),
            1000,
            1100,
            &[m(1, Some(1050))]
        )
        .is_err());
        assert!(plan_matches(&args(), "abcdefghijk".into(), 1000, 3000, &[m(1, None)]).is_err());
    }
}
