use anyhow::{bail, Context, Result};
use std::{collections::BTreeMap, path::PathBuf};

pub const HELP: &str = "FRC Match Clipper\n\n  frc-match-clipper clip LIVESTREAM --event EVENT [OPTIONS]\n  frc-match-clipper upload FILE --title TITLE [--description TEXT] [--privacy unlisted]\n\nclip options:\n  --output DIR            Clips and resumable manifest (default: clips/EVENT/VIDEO)\n  --offset-seconds N      VOD timeline correction, signed (default: 0)\n  --lead-in N             Seconds before match (default: 8)\n  --match-duration N      Match including transition (default: 163)\n  --post-roll N           Seconds after match (default: 15)\n  --start-match KEY       First match key or qm number (optional)\n  --stop-match KEY        Last match key or qm number (optional)\n  --privacy VALUE         private, unlisted, public (default: unlisted)\n  --no-upload             Keep clips without publishing\n  --keep-clips            Retain local clips after successful uploads\n  --dry-run               Print clip plan without downloading/uploading\n\nRequires yt-dlp, ffmpeg, TBA_API_KEY (or TBA_KEY), YOUTUBE_API_KEY.\nUploads use client_secret.json and cached token.json in the current directory.\n";

#[derive(Debug, Clone)]
pub struct ClipArgs {
    pub livestream: String,
    pub event: String,
    pub output: Option<PathBuf>,
    pub offset: i64,
    pub lead_in: i64,
    pub duration: i64,
    pub post_roll: i64,
    pub start: Option<String>,
    pub stop: Option<String>,
    pub privacy: String,
    pub no_upload: bool,
    pub keep_clips: bool,
    pub dry_run: bool,
}
#[derive(Debug)]
pub enum Command {
    Help,
    Clip(ClipArgs),
    Upload {
        file: PathBuf,
        title: String,
        description: String,
        privacy: String,
    },
}
fn privacy(value: String) -> Result<String> {
    if !["private", "unlisted", "public"].contains(&value.as_str()) {
        bail!("--privacy must be private, unlisted, or public");
    }
    Ok(value)
}
pub fn parse(args: Vec<String>) -> Result<Command> {
    if args.is_empty() || args.iter().any(|s| s == "--help" || s == "-h") {
        return Ok(Command::Help);
    }
    let command = args[0].as_str();
    if !["clip", "upload"].contains(&command) {
        bail!("Expected clip or upload; see --help");
    }
    let mut options = BTreeMap::new();
    let mut input = None;
    let mut i = 1;
    while i < args.len() {
        let key = &args[i];
        if key.starts_with("--") {
            let is_switch = ["--no-upload", "--keep-clips", "--dry-run"].contains(&key.as_str());
            let value = if is_switch {
                "true".to_owned()
            } else {
                i += 1;
                args.get(i)
                    .filter(|v| !v.starts_with("--"))
                    .cloned()
                    .with_context(|| format!("Missing value after {key}"))?
            };
            if options.insert(key.clone(), value).is_some() {
                bail!("Duplicate option {key}");
            }
        } else if input.replace(key.clone()).is_some() {
            bail!("Only one input is accepted");
        }
        i += 1;
    }
    let allowed: &[&str] = if command == "clip" {
        &[
            "--event",
            "--output",
            "--offset-seconds",
            "--lead-in",
            "--match-duration",
            "--post-roll",
            "--start-match",
            "--stop-match",
            "--privacy",
            "--no-upload",
            "--keep-clips",
            "--dry-run",
            "--youtube",
        ]
    } else {
        &["--title", "--description", "--privacy"]
    };
    for key in options.keys() {
        if !allowed.contains(&key.as_str()) {
            bail!("Unknown option {key}");
        }
    }
    let p = privacy(
        options
            .remove("--privacy")
            .unwrap_or_else(|| "unlisted".into()),
    )?;
    if command == "upload" {
        return Ok(Command::Upload {
            file: input.context("Missing video file")?.into(),
            title: options.remove("--title").context("Missing --title")?,
            description: options
                .remove("--description")
                .unwrap_or_else(|| "FRC match footage".into()),
            privacy: p,
        });
    }
    let event = options.remove("--event").context("Missing --event")?;
    if event.len() < 5
        || !event
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        || !event[..4].bytes().all(|b| b.is_ascii_digit())
    {
        bail!("Invalid TBA event key");
    }
    let youtube = options.remove("--youtube");
    if input.is_some() && youtube.is_some() {
        bail!("Use LIVESTREAM or --youtube, not both");
    }
    let mut number = |key: &str, default: i64| -> Result<i64> {
        options
            .remove(key)
            .map(|s| s.parse().with_context(|| format!("Invalid {key}")))
            .unwrap_or(Ok(default))
    };
    let offset = number("--offset-seconds", 0)?;
    let lead_in = number("--lead-in", 8)?;
    let duration = number("--match-duration", 163)?;
    let post_roll = number("--post-roll", 15)?;
    if !(0..=300).contains(&lead_in)
        || !(1..=900).contains(&duration)
        || !(0..=300).contains(&post_roll)
        || offset.unsigned_abs() > 86400
    {
        bail!("Invalid timing: lead-in/post-roll 0–300, duration 1–900, offset within one day");
    }
    Ok(Command::Clip(ClipArgs {
        livestream: input.or(youtube).context("Missing livestream URL or ID")?,
        event,
        output: options.remove("--output").map(PathBuf::from),
        offset,
        lead_in,
        duration,
        post_roll,
        start: options.remove("--start-match"),
        stop: options.remove("--stop-match"),
        privacy: p,
        no_upload: options.contains_key("--no-upload"),
        keep_clips: options.contains_key("--keep-clips"),
        dry_run: options.contains_key("--dry-run"),
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }
    #[test]
    fn defaults_and_signed_offsets() {
        let Command::Clip(c) = parse(args(
            "clip abcdefghijk --event 2026cagle --offset-seconds -139",
        ))
        .unwrap() else {
            panic!()
        };
        assert_eq!((c.offset, c.duration, c.post_roll), (-139, 163, 15));
        assert!(!c.no_upload);
    }
    #[test]
    fn reject_ambiguous_or_invalid_arguments() {
        for s in [
            "clip x --event ../oops",
            "clip x --event 2026cagle --wat 1",
            "clip x --event 2026cagle --event 2026cacg",
            "clip x y --event 2026cagle",
            "clip x --event 2026cagle --lead-in -1",
            "clip x --event 2026cagle --privacy secret",
            "clip x --event 2026cagle --output",
        ] {
            assert!(parse(args(s)).is_err(), "{s}");
        }
    }
    #[test]
    fn upload_and_offline_plan_flags() {
        assert!(matches!(
            parse(args("upload a.mp4 --title test")).unwrap(),
            Command::Upload { .. }
        ));
        let Command::Clip(c) = parse(args(
            "clip --youtube abcdefghijk --event 2026cacg --no-upload --dry-run",
        ))
        .unwrap() else {
            panic!()
        };
        assert!(c.no_upload && c.dry_run);
    }
}
