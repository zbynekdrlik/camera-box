# Lipsync cross-check test asset (issue 930)

Not committed to git (binary media, repo convention) — `scripts/lipsync-asset.sh fetch`
re-derives it byte-identically from the pinned source below.

## Source (public domain, US federal government work)

- **File**: `Kamala Harris' speech during Celebrating America.ogv` (Wikimedia Commons)
- **Description URL**: https://commons.wikimedia.org/wiki/File:Kamala_Harris%27_speech_during_Celebrating_America.ogv
- **Direct media URL**: https://upload.wikimedia.org/wikipedia/commons/4/45/Kamala_Harris%27_speech_during_Celebrating_America.ogv
- **License**: Public domain — a work of the US federal government (Commons `extmetadata.LicenseShortName` = "Public domain"; Credit links to `https://twitter.com/VP/status/1352306327318573056`, the Office of the Vice President)
- **sha256** (whole source file, verified by `scripts/lipsync-asset.sh fetch`):
  `7ece8fe0ae7aba1374ca9951c0a8f0ca5a9816430d95a38880f93ef87c533b78`
- **Format**: Theora video (1280x720, ~59.94fps) + Vorbis audio (44.1kHz, stereo), 195.9s total,
  ~73MB.

## Why this clip

A single, well-lit, tight head-and-shoulders shot of one person talking directly to camera with
clear, unaccompanied speech — exactly SyncNet's own use case (a face-track + audio correlation
detector; band shots, graphics, or off-camera narration are NOT usable). Verified by eye
(`scripts/lipsync-asset.sh fetch` extracts a sample frame) before trusting the trim window below.

## Trim window (deterministic — `scripts/lipsync-asset.sh fetch` re-derives it every time)

`ffmpeg -y -ss 30 -i source.ogv -t 60 -vf scale=1280:720 -r 60 -c:v libx264 -pix_fmt yuv420p
-c:a aac -ar 44100 -ac 2 test.mp4` — 30s..90s (60s window: covers the well-lit single-face
segment sampled at `frame_30.jpg`/`frame_45.jpg`/`frame_60.jpg`/`frame_90.jpg` during this
ticket's own asset selection), re-encoded to H.264/AAC (broad decoder compatibility — SyncNet's
`run_pipeline.py`/`run_syncnet.py` and this repo's own `analyze_recording`/ffprobe-based tooling
both expect a standard MP4 container more reliably than Theora/Vorbis), scaled/resampled to a
fixed, known geometry+rate so every re-derivation is byte-identical given the same ffmpeg version.

60s at ~60fps also gives `scripts/lipsync-cross-check.sh` enough length to split into 3 x ~20s
SyncNet windows (matching `scripts/av_sync_measure.py`'s own `--secs 20` convention) — at least 2
confident windows are needed for `scripts/av_sync_calibrate.py --calibrate`'s SEM-shrinking
(issue 805's `aggregate_syncnet_windows`) to report a real 95% CI rather than a single
frame-quantized estimate.

## Baseline (intrinsic A/V sync of the trimmed asset itself)

`scripts/lipsync-asset.sh baseline` runs `scripts/av_sync_measure.py --media test.mp4` (no
`--grab`, no OBS connection) against the trimmed clip BEFORE it is ever played through the rig —
this is a genuine broadcast recording (network camera + boom/lav mic, professionally mixed), so
its own intrinsic offset is expected small but is NOT assumed zero; whatever this baseline
reports is the source asset's own residual, separate from anything the rig's playback/capture
chain adds. Record the baseline's `AV offset` line in the paired-run evidence comment on issue
930 alongside the cross-check result.
