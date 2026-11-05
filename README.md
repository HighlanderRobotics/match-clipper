# FRC Match Clipper

A Rust CLI that turns an archived YouTube livestream into one video per played FRC match and uploads each clip to YouTube. Match times come from The Blue Alliance. Successful uploads are checkpointed in a local manifest so interrupted runs do not upload them again.

## Setup

Install Rust, `yt-dlp`, FFmpeg (including `ffprobe`), and Node.js (used by yt-dlp's JavaScript extractor). Then:

```sh
cargo build --release
cp .env.example .env
```

Set `TBA_API_KEY` (or `TBA_KEY`) and `YOUTUBE_API_KEY`. For uploads, place an installed-application Google OAuth client in `client_secret.json`, enable YouTube Data API v3, and allow `http://127.0.0.1:8765/callback`. The first upload opens Google's authorization page. Later runs refresh `token.json` as needed. Keep these files private; they are ignored by Git.

## Clip an event livestream

```sh
# Inspect the plan before downloading or publishing.
frc-match-clipper clip https://www.youtube.com/live/N1hIhUTuO5w \
  --event 2026cacg --dry-run

# Verify a local sample. Match numbers also accept qm1 or a full match key.
frc-match-clipper clip https://www.youtube.com/live/N1hIhUTuO5w \
  --event 2026cacg --start-match 1 --stop-match 1 --no-upload

# Clip and upload every played match inside this stream's time interval.
frc-match-clipper clip https://www.youtube.com/live/N1hIhUTuO5w \
  --event 2026cacg
```

Use `target/release/frc-match-clipper` in place of the command if it is not on your PATH. Videos default to **unlisted**; use `--privacy private` or `--privacy public` explicitly to change that. Uploads go to the channel authorized by the cached Google token. The CLI does not publish links to TBA or modify Lovat.

The default clip starts eight seconds before the TBA actual start, includes 163 seconds of play/transition, and retains 15 seconds afterward. Use `--match-duration` for another season or event rules. Qualifications and playoffs are both included when their actual timestamps lie inside the stream. Missing actual timestamps are counted in the plan and skipped; estimates from scheduled times are never used.

### Check timing first

A stream's wall-clock start can differ from its VOD playback origin, and TBA's actual start can differ from the broadcast. Verify the sample's opening countdown and final clock before uploading a whole event. `--offset-seconds 139` moves every clip 139 seconds later; negative values move clips earlier. There is no universal offset. Plans reject offsets that place a selected match outside the stream or truncate play at the end of the recording.

Only completed archived streams with both start and end metadata are accepted. This is not a live recorder. Clip boundaries are re-encoded with `--force-keyframes-at-cuts`, and FFprobe checks the resulting duration within three seconds. Audio is included when available. The tool downloads sections sequentially rather than retaining an entire event VOD; a successful upload removes its local video unless `--keep-clips` is set. `--no-upload` always keeps clips.

### Resume

Outputs default to `clips/EVENT/VIDEO/`, or choose `--output DIR`. Each directory has an atomically written `manifest.json` with source, timing plan, clip status, and uploaded video IDs. Rerun the **same command/options** to resume. Confirmed uploads are skipped, saved clips are reused, and failed downloads are retried. Changing timing or privacy requires a new directory. `--no-upload` can be removed to upload previously verified local clips.

An `.running` file prevents concurrent runs in the same output directory. After a crash, confirm the old process has stopped before removing this lock. If an upload response is lost, the CLI stops instead of automatically risking a duplicate. Check YouTube Studio: if it succeeded, set the entry's `status` to `uploaded` and `uploaded_video_id` to its 11-character ID; otherwise set `status` to `clipped`, then resume. Do not delete the manifest to retry.

YouTube quota, provider throttling, and account upload limits can stop a large batch. Completed uploads remain checkpointed. This CLI does not bypass those restrictions.

## Upload an existing clip

```sh
frc-match-clipper upload match.mp4 --title 'CalGames QM1' \
  --description 'FRC match footage' --privacy unlisted
```

## Checks

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

Tests cover argument validation, URL validation, chronological match selection, timestamps and padding, missing/truncated matches, atomic manifest writes, safe resumption, and duplicate-upload protection. Network downloads and YouTube publishing require an integration test with an authorized account.

## Verified example

The CalGames Saturday 2026 stream (`N1hIhUTuO5w`) produced a 38-match plan. Its QM1 sample needed `--offset-seconds 139`: the default timestamp window ended during teleop. The corrected 186-second clip contained the entire match and about 13 seconds after the clock reached zero. It was uploaded unlisted in an end-to-end test. This verifies that sample, not every match's timing or another stream's offset.

```sh
frc-match-clipper clip https://www.youtube.com/live/N1hIhUTuO5w \
  --event 2026cacg --offset-seconds 139 --dry-run
```
