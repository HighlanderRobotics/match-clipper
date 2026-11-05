# Match clipper agent instructions

## Purpose

This standalone Rust CLI clips archived FRC YouTube livestreams using official TBA actual match timestamps and uploads individual videos through the user's existing YouTube OAuth authorization. Keep Lovat-specific imports and scouting analysis out of this repository.

## Layout

- `src/cli.rs`: argument parsing and help.
- `src/config.rs`: user configuration and paired legacy credentials.
- `src/logging.rs`: human/JSON stderr logs and diagnostic redaction.
- `Formula/frc-match-clipper.rb`: pinned Homebrew release.
- `src/clip.rs`: public metadata, deterministic clip planning, bounded section downloads, duration verification.
- `src/batch.rs`: sequential processing, output locking, atomic progress manifests, safe resume.
- `src/main.rs`: entry point, Google OAuth, resumable YouTube upload.

## Working rules

- Preserve existing local changes and user videos. Never commit credentials, `.env`, OAuth client/token JSON, downloaded media, manifests, or build artifacts.
- Do not print API keys, access/refresh tokens, OAuth responses containing secrets, or signed media URLs.
- Use actual match times, not scheduled times. Never assume a fixed offset across livestreams. Keep the signed `--offset-seconds` override and verify sample timing before publishing an entire event.
- Keep uploads unlisted by default. Do not change visibility, publish TBA links, delete remote videos, or upload to another account without a user request.
- Confirmed uploads must remain checkpointed before local cleanup. Never automatically retry an ambiguous upload response: inspect YouTube Studio to resolve it first.
- Do not overwrite a manifest with a changed source/timing plan. Do not remove a run lock until its owner has stopped.
- Keep local disk use bounded: section downloads, sequential uploads, remove only successfully uploaded task-generated clips unless `--keep-clips` is set.
- Download failures may be retried; uncertain uploads must stop. Respect provider quota/rate limits.
- Never launch event-wide publishing as part of automated tests.

## Validation

Run `cargo fmt --check`, `cargo test`, and `cargo clippy --all-targets -- -D warnings` after changes. Tests must cover planning, validation, interrupted processing, and duplicate prevention when those behaviors change. Integration tests should begin with `--dry-run`, then a local sample using `--no-upload`; verify the full match and post-roll before a user-authorized unlisted upload. Report network/quota restrictions separately from code test results.

The native tools `yt-dlp`, `ffmpeg`, `ffprobe`, and Node.js must already be installed. No Python packages are needed. For memory/disk-constrained environments use `CARGO_INCREMENTAL=0`; do not delete artifacts while Cargo is running.

## Packaging

Keep Cargo package includes explicit so media and credentials cannot enter archives. For Homebrew, commit and tag the tested source release first, then pin the formula to that tag and exact revision in a follow-up commit. Do not invent a license. Test an actual Homebrew installation and its `brew test` block. Push only the intended source branch and release tags; never push local backup refs that can contain media. Preserve the existing API keys and OAuth grant when changing configuration paths.
