---
paths:
  - "scripts/drm-latency-measure.sh"
  - "scripts/drm_latency_report.py"
  - "tests/python/test_drm_latency_report_1152.py"
---

# DRM-latency measurement tooling (#1152 M3 — optical differential via the cam2 grabber)

Measures the render-tick→HDMI-glass latency + jitter of imag's Program output, DORMANT (X projector)
vs ENABLED (the M1/M2 in-OBS DRM-lease output), to decide the M4 permanent flip. cam2's grabber
physically taps imag's HDMI (projection-tap, issue 781/1196), so cam2 `/dev/video0` IS the imag
scanout; imag's Program carries the QR burn whose `gen_ts_ns` is the emit wall clock. `drm-latency-measure.sh`
grabs a short clip off cam2 with per-frame capture wall-ts, STREAMED over the ssh pipe straight into
a dev1-local file; `drm_latency_report.py` decodes each frame and pairs capture-ts vs emit-ts. The
DORMANT−ENABLED DELTA (delta = ENABLED − DORMANT) cancels the grabber's fixed offset — the delta is
the answer, not the absolute number.

## GOTCHA — `-t N` alongside `-copyts` writes an EMPTY capture; bound the grab by `-frames:v`

With `-copyts` the stream's timestamps stay at EPOCH scale (~1.78e9 s), and ffmpeg's `-t N` output
duration is evaluated against those timestamps — so recording "stops" immediately and the capture
file is EMPTY (0 frames, no error; proven live in the M3 campaign). Bound the grab by
`-frames:v $((seconds * fps))` instead — a frame count is timestamp-independent. The orchestrator's
`test_sh_grab_is_frame_bounded_never_duration_bounded` pins this.

## GOTCHA — a raw grab cannot fit cam2's /tmp; STREAM the NUT over the ssh pipe to dev1

Raw YUYV 1080p60 is ~4.15 MB/frame (~250 MB/s) — a 20 s grab overflowed cam2's /tmp live ("No space
left on device"). The orchestrator therefore never writes a remote file: the remote ffmpeg emits
`-c:v copy -f nut -` to stdout and the dev1-side executor redirects the ssh pipe into the local
capture (`ssh … bash -s > local.nut`), no scp step. Consequence: the remote program's stdout IS the
NUT stream — EVERY progress echo goes to `>&2`, and the restore fn redirects its whole body (`} >&2`)
because the spliced `camera_box_verify_active_cmds` block prints its success line to stdout.

## Fleet auth is by PASSWORD — the `CAM_PW` sshpass seam

The cam boxes authenticate by password, not keys. `CAM_PW` set → the executor wraps ssh in
`sshpass -p "$CAM_PW"`; unset → plain ssh. The plan prints the UNEXPANDED `"$CAM_PW"` reference —
the value is never printed (test-pinned).

## GOTCHA — `ffmpeg -use_wallclock_as_timestamps 1` REQUIRES `-copyts`, or the epoch is silently rebased to ~0

`-use_wallclock_as_timestamps 1` stamps each captured packet's PTS with the CLOCK_REALTIME instant
it arrived. But on a **copy-mux** (`-c:v copy`) ffmpeg applies `ts_offset = -start_time` UNLESS
`-copyts` is set — the v4l2 demuxer's `start_time` is the first frame's wallclock, so WITHOUT
`-copyts` every frame's PTS is rebased to 0–N seconds and `ffprobe frame=pts_time` reads ~0 instead
of an epoch ~1.78e9. The pairing `capture_ts − emit_ts` is then ≈ −1.76e18 ns for every frame, and
the delta does NOT save you (each run rebases to its own first-frame instant). Empirically confirmed
on dev1's ffmpeg (a copy-mux of a stream starting at 1.7e9 s emitted pts 0.000000 without `-copyts`).
**Always grab with `-copyts` (and `-avoid_negative_ts disabled`).** `drm_latency_report._ffprobe_capture_ts_ns`
hard-fails "epoch lost" when the first pts < 1e9 s — a deliberate loud guard, never a silent nonsense number.

## GOTCHA — `-fps_mode passthrough` is an OUTPUT option (must come AFTER `-i`)

`-fps_mode` (and its old alias `-vsync`) before `-i` is a FATAL ffmpeg error ("Option fps_mode cannot
be applied to input url"). In `_extract_frames` it sits after `-i` so the Nth PNG lines up 1:1 with
the Nth `ffprobe` pts entry. A bad ffprobe pts row is kept as a **None placeholder** (never dropped —
dropping shifts every later frame's timestamp by one, a ~16.7ms error); `records_from_capture`
excludes a None-ts frame in lock-step.

## Orchestrator shape + safety invariants

- `drm-latency-measure.sh` is a **pure-builder + source-guard planner** (mirrors
  `scripts/deploy-genlock-fleet.sh`): the builders (`drm_latency_cam2_program` / `_burn_cmd` /
  `_scp_cmd`) print command text and take no network, so `tests/python/test_drm_latency_report_1152.py`
  sources it with no rig. PLAN/dry-run is the DEFAULT; `--execute` performs rig I/O.
- **Rig state is an INPUT `--label`, NEVER a knob** — the tool must never write
  `~/.camera-box/drm-output.json` (the ENABLE flip is the M4 supervisor runbook). The test pins this
  with a real write-pattern assertion (a bare "the script mentions drm-output.json" check is vacuous —
  the script legitimately names it in comments/echo).
- **Fail-closed teardown:** the cam2 remote program restarts camera-box via a `trap … EXIT` reusing
  `camera_box_verify_active_cmds` even if the grab fails; the dev1 side turns the burn OFF via its own
  `trap … EXIT` and warns LOUDLY if the `remove` fails (a leaked live burn is the #246/#938/#1011
  class). Both the remote ffmpeg and the local ssh are `timeout`-bounded (a wedged V4L2 read must not
  hang the campaign with cam2's production service stopped).

## Decode reuse + Tier-0

- REUSE `mv_skew_snapshot.parse_payload` / `tick_map` / `RESERVED_RUN_IDS` and
  `qr_screenshot_check.decode_qr_codes_from_image_bytes` — never a second copy. Exclude
  `RESERVED_RUN_IDS` (esp. `AUX_TICK_RUN_ID` 911013, whose `gen_ts_ns` is always 0).
- The PURE core (build_records / pair_latencies / percentile[nearest-rank] / summarize /
  run_summary / delta_table / select_run_id / format_*) imports stdlib only — heavy imports
  (cv2, mv_skew_snapshot) are LOCAL inside the impure functions. Tier-0 tests the pure core with
  synthetic records; the impure ffmpeg/cv2 glue has NO local test path (the two 🔴s the review caught
  both lived there) — pin it with static-text anchors + extra manual rigor, and expect the first rig
  campaign to be its first real exercise. `run --records <json>` (produced by `--out-records`) replays
  the pure path with no ffmpeg/cv2.
