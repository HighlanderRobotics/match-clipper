# FRC Match Clipper

A Rust CLI that turns an archived YouTube livestream into one video per played FRC match and uploads each clip to YouTube. Match times come from The Blue Alliance. Successful uploads are checkpointed in a local manifest so interrupted runs do not upload them again.

## Install with Homebrew

```sh
brew tap HighlanderRobotics/match-clipper https://github.com/HighlanderRobotics/match-clipper.git
brew install HighlanderRobotics/match-clipper/frc-match-clipper
frc-match-clipper --version
```

The tap lives in this repository. Use both commands the first time: Homebrew's default repository naming differs from this source repository. Homebrew builds the pinned release from source and installs Rust as a build dependency, plus FFmpeg, yt-dlp, and Node.js as runtime dependencies. After installation, the command is on your normal PATH; you do not run it through Cargo.

Update with `brew update` followed by `brew upgrade frc-match-clipper`. Uninstall with `brew uninstall frc-match-clipper`; configuration and uploaded videos are retained.

Alternatively, with a current Rust toolchain and the runtime tools installed:

```sh
cargo install --locked --git https://github.com/HighlanderRobotics/match-clipper.git --tag v0.2.2
```

## Configure once

`frc-match-clipper config-path` prints the configuration directory. It defaults to `~/.config/frc-match-clipper` (or `$XDG_CONFIG_HOME/frc-match-clipper`). Put these files there:

- `.env`: `TBA_API_KEY` (or `TBA_KEY`) and `YOUTUBE_API_KEY`, one `NAME=value` per line.
- `client_secret.json`: an installed-application Google OAuth client with YouTube Data API v3 enabled and `http://127.0.0.1:8765/callback` permitted.

The first upload opens Google's authorization page and writes `token.json` with owner-only permissions. Later uploads refresh the token as needed. Keep the configuration directory private (`chmod 700`) and `.env`/OAuth files private (`chmod 600`). These files are never part of the package.

Use `--config-dir DIR` or `FRC_CLIPPER_CONFIG_DIR` to select another directory. Shell environment variables take precedence over `.env`. For existing checkouts, a working-directory `.env` and paired `client_secret.json`/`token.json` continue to work when the user configuration directory has no client file.

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
  --event 2026cacg --offset-seconds 139 --playlist "CalGames 2026 Saturday"
```

Each upload includes the official event/match name, alliance team numbers (including B/C aliases and their FMS numbers, such as `8033B (9987)`), final TBA scores, a TBA match link, the source stream and clip interval, and FRC 8033 / source-code attribution. Uploads stop before publishing if required teams or final scores are missing.

Videos default to **unlisted**; use `--privacy private` or `--privacy public` explicitly to change that. Uploads go to the channel authorized by the cached Google token. The CLI does not publish links to TBA or modify Lovat.

The default clip starts eight seconds before the TBA actual start, includes 163 seconds of play/transition, and retains 15 seconds afterward. Use `--match-duration` for another season or event rules. Qualifications and playoffs are both included when their actual timestamps lie inside the stream. Missing actual timestamps are counted in the plan and skipped; estimates from scheduled times are never used.

### Check timing first

A stream's wall-clock start can differ from its VOD playback origin, and TBA's actual start can differ from the broadcast. Verify the sample's opening countdown and final clock before uploading a whole event. `--offset-seconds 139` moves every clip 139 seconds later; negative values move clips earlier. There is no universal offset. Plans reject offsets that place a selected match outside the stream or truncate play at the end of the recording.

Only completed archived streams with both start and end metadata are accepted. This is not a live recorder. Clip boundaries are re-encoded with `--force-keyframes-at-cuts`, and FFprobe checks the resulting duration within three seconds. For newly ended livestreams still served as DASH fragments, an empty FFmpeg section triggers a bounded fragment download and local trim. Temporary fragments are removed on completion, failure, or cancellation. On macOS, `--hardware-encode` selects VideoToolbox H.264 at 4 Mbps instead of CPU encoding; the same timing and duration checks apply. Encoding choice can change when resuming because it does not change clip boundaries. Audio is included when available. The tool downloads sections sequentially rather than retaining an entire event VOD; a successful upload removes its local video unless `--keep-clips` is set. `--no-upload` always keeps clips.

### Resume

Outputs default to `clips/EVENT/VIDEO/`, or choose `--output DIR`. Each directory has an atomically written `manifest.json` with source, timing plan, clip status, and uploaded video IDs. Rerun the **same command/options** to resume. Confirmed uploads are skipped, saved clips are reused, and failed downloads are retried. Changing timing or privacy requires a new directory. `--no-upload` can be removed to upload previously verified local clips.

An `.running` file prevents concurrent runs in the same output directory. After a crash, confirm the old process has stopped before removing this lock. If an upload response is lost, the CLI stops instead of automatically risking a duplicate. Check YouTube Studio: if it succeeded, set the entry's `status` to `uploaded` and `uploaded_video_id` to its 11-character ID; otherwise set `status` to `clipped`, then resume. Do not delete the manifest to retry.

YouTube quota, provider throttling, and account upload limits can stop a large batch. Completed uploads remain checkpointed. This CLI does not bypass those restrictions. `uploadLimitExceeded` means the channel rejected a new upload before video transfer; it is not a failed confirmation. The clip stays `clipped` and can be retried after the channel limit clears. A failed/uncertain transfer still stops for manual reconciliation.

## Upload an existing clip

```sh
frc-match-clipper upload match.mp4 --title 'CalGames QM1' \
  --description 'FRC match footage' --privacy unlisted
```

## Logging

Logs go to stderr, leaving `--dry-run` JSON on stdout available for scripts. Human logs show local timestamps, match progress, download sizes/timings, coarse upload progress, resume decisions, and a completion summary. FFmpeg chatter is suppressed; downloader failures include a short diagnostic with URLs redacted. Terminal output uses color; redirected output does not. `NO_COLOR` and `--no-color` disable it explicitly.

```sh
frc-match-clipper --log-format json clip LIVESTREAM --event EVENT 2>run.jsonl
frc-match-clipper --verbose clip LIVESTREAM --event EVENT
frc-match-clipper --quiet clip LIVESTREAM --event EVENT
```

JSON logs contain `timestamp` (UTC), `level`, `event`, `message`, and structured `fields`. Quiet mode retains warnings/errors. Verbose mode adds timings/context without exposing credentials or signed media URLs. Resolve failed/uncertain uploads using the manifest instructions above, then rerun the same command.

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

## Playlists

Add `--playlist "Event day"` to a clip command. The CLI creates a playlist with the same `--privacy` as the videos (unlisted by default) and adds each confirmed upload in match order. Use `--privacy public --playlist "Madtown Throwdown 2026 Saturday"` for a public playlist and public clips. `playlist.json` checkpoints its ID. Resuming reads existing playlist membership before inserting, so already added videos are skipped. An uncertain playlist creation stops for manual reconciliation instead of creating a duplicate. Keep the same title and privacy on resume. Each stream/output directory has its own playlist.

Playlist management requires Google's `youtube.force-ssl` permission in addition to the earlier upload grant. The first playlist run opens a consent page; existing upload-only credentials are preserved until authorization succeeds. Use the same Google account/channel as before. `--no-upload` and `--dry-run` create no playlist.
