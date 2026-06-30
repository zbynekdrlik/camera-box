---
name: e2e
description: >
  E2E zero-loss measurement and acceptance criteria for camera-box. Load when running
  or evaluating the QR harness, making zero-loss claims, running recording-verdict,
  interpreting E2E results, or reporting on the full cam→strih→stream path.
---

# E2E Zero-Loss Testing

## Acceptance Criteria — HARD BAR

**Never claim "zero loss / stable latency" without this complete proof.**
Past claims of "everything works, zero loss, low latency" were untrustworthy/false (2026-06-17).

**Loss measurement:**
1. From **RECORDED OBS PROGRAM OUTPUT only** — never an NDI tap for loss counting.
   NDI-to-dev1 is an unreliable extra link; frames counted only when OBS shows them in program out.
2. **Dual-QR Vernier:** cam2 paints 2 QRs (left=even tick, right=odd tick; tick=max resolves
   60→30 beat). QR size 700px (480px gave 76% undecodable; 700px → ~0%).
3. **HARD-FAIL bar (#186 headline gate) — PASS = EVERY node's burn-id sequence CONTIGUOUS (no
   missing id; a BURN-UNREADABLE id also FAILS) AND (when `--cam1-capture-stats` is given)
   cam2→cam1 V4L2 capture-drop = 0.** No thresholds, no "0.02% negligible", no explaining-away.
   The per-recording undecodable / 60→30-beat metrics AND the analyzed span (`--min-secs`) are
   **DIAGNOSTIC only — they do NOT gate the headline** (an old overstatement was "PASS = 0
   undecodable AND 0 copy AND 0 gap AND span≥300s" — that conflated diagnostics with the gate).
4. Every undecodable/anomaly frame must be **extracted as real pixels and shown** — black = real
   lost/empty frame = FAIL; blurred QR = decode miss (fix decoder to 0). Prove with pixels.
5. Duration ≥300s to claim zero-loss; ideal 1800s (30min).

**Latency — exact per-hop required:**
- Burn emit-time INTO each frame (OBS-side QR at the compositor render tick, where genlock build
  already has `genlock_wall_now_100ns`/`genlock_emit_timecode_100ns`).
- `record_start` read over network (websocket/ssh) is WRONG — biased by network latency.
- Stable DEFINED latency required, not just "low".

**Per-frame latency CONTINUOUS-LINE proof (#209) — the literal line, not just p50/p99:**
- `recording-verdict --latency-csv <path>` writes one row per delivered stream frame:
  `frame_id,gen_ts_ns,flip_ts_ns,cam1_strih_ms,strih_stream_ms,cam1_stream_ms`
  (defaults to `latency-per-frame.csv` BESIDE the `--json` summary — the JSON's own directory,
  NOT `--out-dir` — whenever `--json` is given, so it sits next to the summary). Column contract
  = `LatencyCsvRow::HEADER` in `src/probe/recording_latency.rs` (single source of truth; the
  Python plotter asserts it equals its `EXPECTED_HEADER` via a cross-boundary test).
- Each hop is paired WITHIN one stream frame (co-located burns, same as `chain_hop_samples_from_stream`),
  so the per-frame points MATCH the summary-stat hops — the CSV is the per-frame expansion, not a
  second measurement. An absent hop burn → empty cell → a GAP in that hop's line (= a lost frame).
- Plot: `scripts/latency-line-report.py --csv <path>` → one continuous line per hop (x=s since run
  start, y=ms). Flat line = stable latency; gap = lost frame; creeping slope = drift. Auto-shares
  the PNG via airuleset (LAN URL). Needs the cam1/strih/stream burns in the stream recording (#174).

**Restart-survival (MANDATORY):** re-run and STILL PASS after (a) OBS restart AND (b) PC restart
of strih+stream. Historically everything diverged after a restart; without this, zero-loss is NOT achieved.

**Constraint:** NEVER rebuild/redeploy prod OBS or install OBS plugins before a live event.

## Dual-QR Vernier Zero-Loss Proof (2026-06-17)

Trustworthy E2E zero-loss proof from RECORDED OBS OUTPUT.

**Rig:** cam2 paints to real monitor via /dev/fb0 DRM page-flip (vblank-locked 60Hz) →
broadcast camera (30fps) films monitor → cam1 ShadowCast → camera-box → "CAM1 (usb)" NDI →
strih OBS PHASE2-PROBE program → stream OBS program. Both record locally.

**Dual-QR Vernier (the method):** `frame-probe --paint-only --dual-qr` renders TWO QRs at 60Hz:
LEFT=latest even tick, RIGHT=latest odd tick. ONE half changes per refresh (anti-blur, always ≥1
cleanly decodable). Decode BOTH per recorded frame; `tick = max(left_even, right_odd)` = exact
60Hz instant the 30fps camera captured.

**Proof metric:** over 330s, `avg tick step = EXACTLY 2.0000` → frame count conserved → zero NET
loss. Jitter balanced: step 0↔4 = 9/9, step 1↔3 = 49/49 → sampling beat artifact (nets to zero).

**Honest bar:** report "zero NET loss" (avg-step=2.0 + balance), NOT "0 anomaly frames" —
there ARE step-events (9 dup + 58 skip strih). The step-events are rig sampling-beat artifacts,
not chain loss.

**Painter MUST paint at the CAPTURE rate (#290).** The painter's tick rate must equal the capture
rate or each painted id covers ≥1 extra camera frame and no per-frame timing resolves. The default
is the pure `default_paint_fps(mode, capture_fps, presenter, paint_only, synth_ndi)` in
`src/probe/run.rs`: **`--paint-only` is a REAL-presenter path** (`run_paint_only` → `run_painter` →
`open_presenter` — it DOES open a presenter, the old comment claiming otherwise was wrong) so it
defaults to the full `capture_fps` (60), NOT the sub-capture coverage 12. Only the fbdev single-box
loopback gate + the presenter-less `--synth-ndi` keep the 12fps coverage default. Under KMS the
painter is vblank-locked at the monitor refresh (`--paint-fps` ignored); on the fbdev fallback
`--paint-fps` is what forces the rate. `rig-mode.sh test` launches with an explicit `--paint-fps 60`
(`PAINTER_FPS` pinned constant). NOTE: the dual-QR `vernier_ids(tick)` ALREADY emits a distinct
`logical_id == refresh_tick` per refresh — so the bug was the RATE, never the dual-QR id logic.

**Vernier methodology at 60fps capture is an OPEN decision (#310, needs-decision).** The Vernier
(`tick = max(left_even, right_odd)`) was designed to resolve the 60→30 BEAT between a 60Hz painter
and a 30fps camera. At 60fps painter + 60fps capture (#11) that beat is gone, so whether to keep
dual-QR (still anti-blur + unique per frame), switch to a single full-rate QR, or keep dual-QR for
redundancy is a measurement-correctness decision for the USER — do NOT redesign it unilaterally.

**Analysis tools on dev1:** `.qr_dual.py` (split L/R decode), `.e2e_report.py`
(2-panel PNG: continuity line slope-2 + deviation band). NOTE: the old multitap-tap report
(`scripts/e2e-report.py`) was removed with the tap harness (#210); the recording-proof path
renders its report via `scripts/recording-e2e-report.py` (in repo).

## Camera Pre-Run Checklist (#220) — cam1 optical settings the harness CANNOT auto-set

The cam2→cam1 OPTICAL injection leg (cam1 broadcast camera filming the cam2 monitor QR) depends on
the cam1 camera's MANUAL settings. The harness CANNOT read or set them: camera-box reads
`/dev/video0` (the ShadowCast capture card), which does NOT expose the BMPCC's shutter/focus/
exposure. `recording-e2e.sh` PRINTS this checklist at startup; satisfy it BEFORE every proof run:

- **Shutter FAST: ≥ 1/500 s (ideally 1/1000)** — a slow shutter integrates a full 60Hz monitor
  refresh and SMEARS the dual-QR Vernier mid-change (one half changes per refresh), so the optical
  read of the cam2 monitor QR goes borderline and drops. A **1/60** shutter caused the **#216 ~175s
  optical-read gap** (the DIGITAL burns were unaffected — drawn post-capture — so the chain stayed
  0 real loss; the gap was purely the test's optical-INJECTION leg). **≥1/500 is the #216
  conclusion and SUPERSEDES the old 1/250 spec.**
- **Manual focus, locked on the cam2 monitor** — no autofocus hunting mid-run.
- **Fixed exposure / manual gain** — no auto-exposure drift.

(Optional later: a first-N-seconds optical-read-rate gate that aborts early with "check camera
shutter/focus" if the cam2 QR decode rate is low — fail fast instead of after a 30-min run.)

## QR-tap harness — REMOVED (#210)

The old live-NDI-tap QR harness (`scripts/multitap-e2e.sh` + the `multitap-probe` bin) is GONE: an
NDI tap samples a different surface than what is DELIVERED, so it produced false sampling artifacts.
The proof path is now `scripts/recording-e2e.sh` — decode the RECORDED OBS program output (see the
Recording-Proof Run Recipe below). The #68 contiguity / leading-discard / wall-clock fixes
(`endpoint_sequence_check`, `decompose_missing`, `--lead-discard-secs`) live on in the kept probe
code (`src/probe/`) and are exercised by `recording-verdict`.

Historical #68 steady-state (cam2→strih→stream, the old tap harness): 0.38% per-frame loss on BOTH
hops, VERDICT=FAIL (correctly); genlock-on-both-hops pending (#8).

## cam1 Cannot Run the QR Harness

cam1 (10.77.9.61) has NO /dev/fb0 (only fbcon), ALL HDMI connectors disconnected,
/dev/video0 held by prod camera-box → QR test is inherently a **cam2 proxy for cam→strih**.
cam1/3/4 each need an HDMI-out→capture loopback to be QR-verified directly (#24).

## Recording-Proof Run Recipe (recording-e2e.sh, the 4-node 0-loss proof)

The DEFINITIVE proof = `scripts/recording-e2e.sh` (records strih+stream OBS programs,
emits the on-stream verdict plan). Run:
`RUN_ID=N DURATION=300 USE_PREBUILT_PROBE_DIR=<CI probe-tools-linux dir> VERDICT_ON_STREAM=1 NDI_RUNTIME_DIR_V6=/usr/lib/ndi nohup bash scripts/recording-e2e.sh`.
Use the FRESH CI probe-tools (linux for cam1/cam2 deploy, windows verdict.exe) at HEAD —
`gh run download <run> -n probe-tools-{linux,windows}-amd64`; symlink `camera-box-probe`→`camera-box`.

**TWO things silently waste a run (verify BEFORE recording):**
1. **stream's measurement burn must be ON** (#195/#257). **The harness AUTO-CHECKS this** before
   `[5/8] StartRecord` (the `[4b/8]` pre-record burn-ON gate): it runs `obs_burn_filter.py check`
   on strih+stream and ABORTS (exit 1) unless the per-source `genlock_burn=true` — so a
   burns-OFF/pass-through OBS fails fast instead of wasting a full run. (Post-#257 the DistroAV QR
   burn EFFECT filter is ALWAYS registered; the `genlock_burn` bool gates whether it RENDERS.)
   Without the burn the stream recording has NO stream burn → strih→stream can't pair (latency=null,
   `strih_stream_source: two recordings ...`). Fix — turn it ON over OBS WebSocket, **NO relaunch,
   NO env**: `scripts/rig-mode.sh test` (both boxes) or
   `scripts/obs_burn_filter.py add --host <ip> --input "<program input>"`. The burn run_id comes
   from the box's host role (strih=911002 bottom-left / stream=911004 bottom-right), NOT env. Verify
   the OBS log: `[burn] ON  genlock_burn=true on '<input>'`.
   **#334 gotcha — a DISABLED effect filter renders NOTHING even with `genlock_burn=true`.** The
   DistroAV QR burn is an OBS EFFECT filter; if `GetSourceFilterList` shows it `filterEnabled=False`
   on the program input, OBS never calls its `video_render`, so the burn is absent from the recording
   even though the C++ setter fired (`genlock: measurement burn ON`) and `genlock_burn=true`. This is
   how strih(911002)+stream(911004) went missing from an all-cambox run. `obs_burn_filter.py` now
   gates on it: `check` reports `burn_on=True` only when `genlock_burn=true AND filter present AND
   filter_enabled` (prints `filter_enabled=<bool>`), and `add` unconditionally re-enables a
   present-but-disabled filter (`SetSourceFilterEnabled filterEnabled:true`). When `[4b/8]` passes,
   confirm the printed line includes `filter_enabled=True`, not just `filter_on_input=True`.
   (#235/#257: genlock latency is the per-source DistroAV UI int (floor 3, prod=3 ms); the render
   tick + ts-align are BUILD DEFAULTS. There is NO `OBS_GENLOCK_*` / `OBS_BURN_*` env any more — the
   old `$env:OBS_BURN_QR=1; OBS_GENLOCK_LATENCY_MS=3` launch model is GONE (#257/#261); OBS launches
   env-free via `scripts/launch-obs-genlock.sh --box {strih|stream}`.)
2. **stream RECORDING is native 1080p, NOT 4K** (#225, FIXED 2026-06-24). The OBS canvas is 1080p.
   The recording USED to reuse the 4K-rescaled streaming encoder (`RecEncoder=none` + stream
   `Rescale=3840x2160`) → recorded 4K → upscale softened the small (~300px) burns → cam1 over-counted
   (#226). **Fixed persistently in the `Stream_Obs` profile:** `[AdvOut] RecEncoder=obs_nvenc_h264_tex`
   (dedicated rec encoder) + `RecRescale=false` → records native 1920×1080; the stream encoder stays
   `Rescale=true RescaleRes=3840x2160` (prod → restreamer unchanged). See obs-ops skill "Recording
   Output = native 1080p" for the full config + apply path. **Still `ffprobe` the recording dims
   before trusting a run** (must be `1920x1080`) — a regression here silently softens the burns.
   (Profile-param changes don't hot-apply to a running output — they take effect at OBS launch.)

**DanteSync gate prerequisite (the harness can't HTTP-fetch :8898):** read `\\.\pipe\dantesync`
on strih+stream via the win-* MCP (PipeDirection.In; strip the 4-byte header to the leading `{`),
write `{ntp_offset_us,is_locked,mode,...}` to `/tmp/recording-e2e-<RUN_ID>/dante-{strih,stream}.json`
BEFORE launching the harness. Then the gate passes all 4 nodes.

**dev1↔painter clock-offset gate — ALL_CAMBOX sweep ONLY (#326):** the all-cambox sweep stamps
each program-switch WINDOW boundary on dev1's CLOCK_REALTIME, while `recording-verdict
--switch-schedule` partitions by the cam2-painter burn `gen_ts_ns`. Two different machines' clocks
— if dev1 drifts from the painter past the verdict's switch-guard (`DEFAULT_TRANSITION_GUARD_NS`,
1 s), frames near every boundary are mis-attributed to the WRONG cambox window (silent #312 false
FAIL / false PASS). `scripts/clock-offset-painter-gate.sh` (invoked by recording-e2e.sh in the
`ALL_CAMBOX=1` path, after the DanteSync/version gates, before the sweep) reads dev1's local
`journalctl -u dantesync` + the painter's over SSH, computes `|dev1 - painter|` (both DanteSync
offsets on the shared strih basis), and FAILS FAST (exit 20) if it exceeds the guard (default
200 ms = 1/5 of the 1 s switch-guard). Bypass with `SKIP_CLOCK_OFFSET_ASSERT=1` (on by default).
The pure comparator (`painter_offset_check`) lives in `clock-offset-guard.sh` and is unit-tested
in `tests/clock_offset_guard.rs`; the gate flow is unit-tested no-rig in
`tests/clock_offset_painter_gate.rs` by feeding `DEV1_DANTE_JOURNAL` / `PAINTER_DANTE_JOURNAL`
fixture files (same "pre-fetch status to a file" trick the DanteSync gate uses for Windows nodes).

**Decode on stream.lan (#193), NOT dev1:** the strih recording lives on the strih box; copy it
DIRECTLY strih→stream box (`New-PSDrive \\10.77.9.204\C$` + Copy-Item, ~751MB in ~7s, NEVER via
dev1). Run the verdict as a DETACHED `Start-Process -PassThru -RedirectStandardOutput` and POLL
the json (the MCP Shell idle-timeout (~30-300s) kills an inline 4K decode — a 300s 4K stream
decode takes ~7min). QUOTE space-bearing paths in `-ArgumentList` (`"\`"$path\`""`) or the
`_NLMEDIA stream` path word-splits. Verdict args: `--strih <1080p strih rec> --stream <stream rec>
--cam2-run-id <RUN_ID> --burn-strih-run-id 911002 --burn-stream-run-id 911004 --burn-cam1-run-id
911001 --latency-csv <path>`. cam1 is read from the CLEAN 1080p strih recording (#133), strih/stream
from the stream recording.

**Continuous-line graph (#209):** `python3 scripts/latency-line-report.py --csv <lat.csv> --out X.png`
auto-shares a LAN URL. The CSV's burn hops (cam1_strih/strih_stream/cam1_stream) must be UNBROKEN
(0 empty cells); cam2_cam1 gaps honestly where the optical read failed.

**Diagnosing cam1 "REAL DROP":** if the missing ids are PERIODIC (a ~N-emit beat: deltas cluster on
N, 2N, 3N…) AND present==expected AND the strih genlock FIFO shows `overruns=0` AND the pixels show
the cam1 burn PRESENT-but-blurred → it's a burn-readability DECODE-MISS, NOT chain loss (#226).
A genuine drop is randomly spaced + FIFO overruns>0. ALWAYS view the flagged frames' pixels
(download the `pp/cam1-missing/*.png`, decode the base64, Read it) before claiming loss.

## Per-camera COLOUR gate (#364) — `recording-verdict --colour-gate`

The zero-loss verdict proves frame DELIVERY only — it never checks COLOUR, so a camera that goes
grayscale / hue-shifted / white-balance-cast delivers every frame and still PASSES. `--colour-gate`
is the HARD per-camera colour gate (sibling of the #363 optical read): `NodeVerdict.colour_fail`
gates `is_zero()` alongside contiguity + `optical_undecodable`.

- **Where the logic lives (one source of truth):** the #367 painter blits a known-sRGB colour scale
  as a VERTICAL column in the CENTRAL GAP between the two dual-QR halves (`src/colour_scale.rs` —
  `colour_scale_patches(canvas_w, canvas_h, qr_size, top_margin)` + `PATCH_COLOURS`, 13 patches).
  The gap is derived from the SAME formula `qr::blit_qr_bgra`/`render_qr_dual_bgra` use (half =
  canvas_w/2; each QR centered in its half; top-anchored at `top_margin` over `qr_size` tall) by the
  `dual_qr_gap()` helper, so painter and gate compute IDENTICAL rects. At default 1920×1080 / qr 700
  / tm 24 the column is x∈[840,1080), y∈[24,724). The gate iterates the SAME table:
  - `src/colour_verify.rs` (Tier-0, default features — the JUDGEMENT, mutation-tested): sampler +
    `classify_patch` (Grayscale if chroma<40 / HueShift if hue err>30° / OutOfTolerance if sRGB
    dist>96 / NeutralTint if a neutral patch chroma>48) + `summarize_node_colour` (strict-majority
    vote over sampled frames). A burn-covered patch is `Unsamplable` → SKIPPED, not charged (a real
    colour defect is global → still fails on the visible patches); fail-closed only when NOTHING is
    checkable.
  - `src/probe/colour_sample.rs` (probe, CI-only — the I/O glue): `node_burn_exclusions` +
    `extract_recording_colour_summary` (ffmpeg input-seek, N evenly-spaced RGB frames).
- **#364 rig finding — why the column moved to the central gap:** the original BOTTOM-band scale
  (y=960..1080) was CROPPED off the bottom by the camera's framing of the cam2 monitor — it never
  reached the recording, so the gate had nothing to sample (painted fb0 was clean; only the bottom
  strip was out of frame). The dual-QR halves ARE captured (they decode), so the gap between them is
  reliably in frame. The column ends at the QR bottom (~724), ABOVE all three bottom-anchored burns
  (cam1 `qr::cam1_burn_origin` top row ~736; strih/stream `burn_geom::corner_placement` top row ~738
  — side `0.28·h`≈302, margin `40/1080·h`≈40), so the burns no longer overlap any patch at all —
  `node_burn_exclusions` is now belt-and-braces (no patch loses pixels). If you change `qr_size`,
  `top_margin`, the gap, or a burn, re-confirm the Tier-0 tests (`cargo test --lib colour
  # airuleset:build-ok`) still show no patch intersecting a QR half or burn.
- **Run it:** add `--colour-gate` (off by default → delivery-only runs unchanged; rig TEST mode
  paints the scale so enable it there). `--colour-samples N` (default 12) bounds cost. Fused /
  on-host only — in `--merge-partials` mode the recording is not on the host so it ERRORS LOUDLY
  (never silently skips a requested gate); the cross-box carry-through is #377.
- **Tolerances are a STRICT REAL signal — NEVER loosen to force a pass** (strict-test mandate). Hue
  is the sharp discriminator (exposure/compression robust). The recorded fixture (with a bad-colour
  variant) is what locks the bar end-to-end on the rig.

## Rig TEST / EVENT Mode Switch (#247) — `scripts/rig-mode.sh`

THE deterministic, single-source-of-truth switch between TEST mode (QR/E2E measurement) and EVENT
mode (clean prod broadcast). Replaces the ad-hoc, context-dependent switching that caused #246 — a
burn left ON in the prod **Machine** env painted QR on the LIVE broadcast, and genlock left in a test
state. The settings below are PINNED in the script; do NOT improvise them.

Run from dev1 (ssh to the cam boxes is ALLOWED; ssh to the Windows boxes is DENIED, so the OBS side
is PRINTED as the exact step to paste into the box's win-* MCP Shell — the WS burn toggle
`obs_burn_filter.py add|remove` and, only if OBS is wedged, the env-free
`launch-obs-genlock.sh --box {strih|stream} --force` relaunch):

```bash
scripts/rig-mode.sh test     # rig INTO test mode (paint QR + print OBS burns-ON step)
scripts/rig-mode.sh event    # rig BACK to clean broadcast (stop QR + print OBS burns-OFF step)
```

**TEST mode (pinned):**
- **cam2 (10.77.9.62):** free /dev/fb0 WITHOUT killing capture+emit (#291). cam2 does THREE
  independent things — DISPLAY (`--display` → /dev/fb0/HDMI), CAPTURE (/dev/video0) and EMIT (NDI to
  strih); ONLY display grabs fb0. So TEST mode installs a TRANSIENT systemd drop-in
  (`/run/systemd/system/camera-box.service.d/zz-rig-test-no-display.conf` — overrides ExecStart to
  run camera-box WITHOUT `--display`) then `daemon-reload` + `restart camera-box`: display output
  stops (fb0 freed) while capture+emit KEEP RUNNING → **cam2 stays a measurable camera in test mode**
  (the old `systemctl stop camera-box` killed all three and wrongly dropped cam2). The drop-in lives
  in /run (tmpfs) so a reboot auto-reverts; EVENT mode removes it. The unit's Restart=always now
  respawns the no-display command, so a restart can never re-grab fb0. → launch the painter
  `frame-probe --paint-only --dual-qr --qr-size 700 --paint-fps 60 --duration-secs N` (700px = the
  validated vernier; --paint-fps 60 matches the 60fps capture, #290), PID → `/run/rig-painter.pid`. Painter binary = `/usr/local/bin/frame-probe` from the CI
  `probe-tools-linux-amd64` artifact (`gh run download <run> -n probe-tools-linux-amd64` → scp to
  cam2); TEST mode **FAILS LOUD** if it is absent. For a measurement run add `PAINTER_EXTRA_FLAGS="--wall-clock --run-id <N>"`.
- **cam1 (10.77.9.61):** NOT reconfigured — runs its DEPLOYED service (already 30 fps / certified v4l2
  saturation=0 contrast=75), the recording convention.
- **strih + stream OBS:** the measurement burn is toggled ON over OBS WebSocket — **NO relaunch,
  NO env**: `rig-mode.sh test` runs `scripts/obs_burn_filter.py add` on both boxes (per-source
  `genlock_burn=true`; the QR EFFECT filter is always registered, the bool gates its render). The
  burn run_id is fixed by the box role — strih 911002 (bottom-left) / stream 911004 (bottom-right) —
  NOT env. Relaunch the box's OBS only if it is wedged or pass-through:
  `scripts/launch-obs-genlock.sh --box {strih|stream} --force` (env-free; genlock latency is the
  build const, floor 3 ms). Then confirm the PHASE2-PROBE scene + native-1080p recording (#225).

**EVENT mode (pinned — the #246 guard):**
- **cam2:** stop the painter via its PID file (NOT `pkill -f frame-probe` — a shell whose cmdline
  contains "frame-probe" would self-kill; `pkill -x frame-probe` is the safe fallback) → REMOVE the
  transient no-display drop-in TEST mode installed (#291) → `daemon-reload` + `restart camera-box`
  (a `restart`, not a bare `start`, since TEST mode reconfigured rather than stopped it) → verify
  active + `--display` restored (re-holds /dev/fb0).
- **strih + stream OBS:** the measurement burn is toggled OFF over OBS WebSocket — the #246 guard,
  **NO relaunch, NO env**: `rig-mode.sh event` runs `scripts/obs_burn_filter.py remove` on both
  boxes (per-source `genlock_burn=false`; the EFFECT filter stays registered, pass-through — no QR
  on the live broadcast). PROD genlock latency is the per-source DistroAV UI int (prod=3 ms),
  unchanged by the burn toggle.

**Burn run_ids are the single source of truth** in `src/probe/recording_latency.rs`
(`BURN_RUN_ID_STRIH=911002`, `BURN_RUN_ID_STREAM=911004`, `BURN_RUN_ID_CAM1=911001`), mirrored by
the box host role in `scripts/obs_burn_filter.py`. **Post-#257 there is NO `OBS_BURN_*` env at
all** — the burn is the per-source `genlock_burn` bool flipped over OBS WebSocket, so the old #246
`OBS_BURN_*`-in-Machine-scope contamination (a stray env surviving a reboot and painting QR on the
live broadcast) is structurally impossible. The #246 guard is now a WS-STATE read: EVENT mode
asserts `genlock_burn=false` on the program inputs (`obs_burn_filter.py check`) and the harness
cleanup verifies it. The cam-box root pw is `$CAM_PW` (dev-rig LAN default, as in the sibling e2e
scripts — override from your password store).

## Testing the E2E harness scripts (sourceability gotcha)

`scripts/recording-e2e.sh` runs TOP-TO-BOTTOM under `set -euo pipefail` (NO `BASH_SOURCE != $0`
source guard), so a test CANNOT `. recording-e2e.sh` to unit a single function — sourcing it would
execute the whole harness (ping preflight, gates, deploys). To behaviorally test a piece of its
logic, MIRROR the snippet in an inline `bash -c` and run the four states (the #178 set+e-region
test and the #195 burn-gate test both do this: `tests/harness_recording_e2e_paths.rs`). Pass inputs
as `bash -c '<body>' bash "$arg1" "$arg2"` ($0,$1,…) to avoid quoting hazards. By contrast,
`scripts/recording-fetch-windows.sh` and `scripts/genlock-manifest.sh` DO have a `BASH_SOURCE != $0`
guard, so their pure functions can be sourced directly (see `urlencode_name` / `tests/genlock_manifest.rs`).
Structural guards (the script CONTAINS the gate/flag) complement — but don't replace — a behavioral
mirror of the decision logic.

**Heredoc-authoring footgun (rig-mode.sh `painter_*_remote`, #291):** the remote-bash builders use an
UNQUOTED `cat <<REMOTE` heredoc so build-time locals (`$cbbin`, `$dropin`) expand. That means
backticks AND `$( … )` in the heredoc body — **including in COMMENTS** — run on the LOCAL (dev1)
shell while building the string, not on the cam. A comment like `` # use `systemctl show` not `systemctl cat` ``
silently runs `systemctl cat` on dev1 ("Too few arguments") and emits a MANGLED comment to the cam.
Rules for editing these heredocs: remote runtime vars/substitutions MUST be `\$`-escaped (`\$(systemctl
is-active …)`, `\$i`); local vars are bare (`$cbbin`); and **never put backticks or `$()` in a heredoc
comment** — use single quotes (`'systemctl show'`). Verify after editing: `bash -c '. scripts/rig-mode.sh;
painter_launch_remote … >/dev/null; painter_stop_remote … >/dev/null'` must print ZERO stderr.

**Why a systemd drop-in, not loopback's manual `nohup camera-box`:** `loopback-e2e.sh` runs a manual
no-display camera-box (stop service → `nohup /usr/local/bin/camera-box &`), fine for a single-box
loopback. rig-mode TEST (#291) instead installs a `/run` ExecStart drop-in so cam2 stays UNDER systemd
with all its directives (Nice=-10, SCHED_FIFO, CPUAffinity, NDI env, Restart=always) — cam2 must emit
PRODUCTION-quality NDI to strih during the whole test, and Restart=always must respawn the no-display
command (never re-grab fb0). Use the drop-in for rig-mode; the manual nohup is loopback-only.

**Any camera-box "restore" MUST clear the #291 drop-in first (#309).** The TEST-mode no-display
drop-in is single-sourced in **`scripts/lib/rig-test-dropin.sh`** — the `RIG_TEST_DROPIN` path
constant + the pure `rig_test_dropin_clear_cmds` builder (`rm -f` drop-in + `rmdir` + `daemon-reload`,
idempotent). `rig-mode.sh` (EVENT restore), `recording-e2e.sh` (cleanup cam2 restore) and
`loopback-e2e.sh` (remote cleanup, carried in via `build_remote_env`'s `RIG_TEST_DROPIN_CLEAR=%q` then
`eval`'d) all source it and clear BEFORE their `systemctl restart/start camera-box`. Reason: a leftover
`rig-mode.sh test` drop-in would otherwise make a sibling harness's plain restart bring camera-box back
WITHOUT `--display` (dark interkom return monitor) while the operator believes broadcast is restored —
a #246-class silent test-state leak. NEVER hard-code the drop-in path in a script; add the sourced
helper + the clear-before-restore call to any NEW path that restarts cam2's camera-box.

## obs_phase2 ops + cleanup MUST be timeout-bounded (#328) — a hung OBS op can strand a cam device

**`obs_phase2.py` `_rpc` had NO overall wall-clock deadline (the #328 ~28-min hang).** Its read loop
drains op-5 EVENTS until the matching op-7 response; while OBS renegotiates an NDI source it FLOODS
events, so every `recv()` keeps succeeding within the 10s socket timeout yet the response never
arrives → the loop spins forever (a healthy WS, not a dead one). Now bounded by **`OBS_OP_TIMEOUT_S`
(default 60s, env-overridable)** via the pure `_rpc_timed_out(elapsed, timeout)` helper; past the
deadline `_rpc` raises `TimeoutError` (fail loud — non-zero exit for prod-scene/setup/switch, caught
+warned inside teardown's best-effort guard). `ignore_err` suppresses a normal failed RPC but NEVER
the timeout (a hang is always fatal). If you add a legitimately-slow OBS op, raise the env knob — do
NOT remove the bound.

**`recording-e2e.sh` cleanup() FREES the cam capture devices FIRST, independent of OBS (#328/#281).**
The cam1/cam2 device restore (`pkill -9 -f 'camera-box-burn-'` → `rm -f /tmp/camera-box-burn-*` →
`systemctl restart camera-box`) leads cleanup(); the OBS record-stop/teardown runs AFTER, and EVERY
cam ssh + EVERY `obs_phase2.py`/`obs_burn_filter.py` call is wrapped in `timeout`
(`CLEANUP_SSH_TIMEOUT`=30s, `OBS_CLEANUP_TIMEOUT`=90s, env-overridable). Reason: in the #312 run the
OBS teardown ran first and hung, so the trap never reached the cam1 restore and the #174 burn binary
kept holding /dev/video0 → prod camera-box crash-looped ("Device or resource busy"). Guards:
`tests/harness_recording_e2e_cleanup_resilient.rs` (cam-free-before-OBS ordering + every OBS call
timeout-bounded + pkill -9), `tests/python/test_obs_phase2_timeout.py` (the pure deadline helper).
**Any NEW blocking obs-websocket/ssh call in a cleanup/trap MUST be timeout-wrapped and ordered after
the cam-device free.**

## Painter ground-truth CSV lifecycle + recording orphan guard (#355 / #359)

**frame-probe writes the painter ground-truth `/tmp/painter.csv` ONLY on its clean `--duration-secs`
self-exit** (`src/probe/run.rs`: the main thread waits out `cfg.duration`, signals stop, joins, THEN
`std::fs::write(paint_log)`). So a painter `pkill`'d early NEVER writes a fresh CSV. `recording-e2e.sh`
must therefore (#359): (1) `rm -f /tmp/painter.csv` on the painter box BEFORE launch (no stale
leftover survives); (2) NOT kill the painter at `[7/8]` — instead WAIT for its self-exit
(`PAINTER_LAUNCH_EPOCH + DURATION + 60` + grace; poll `pgrep -x frame-probe` gone AND a non-empty CSV
with remote mtime ≥ `RUN_START_EPOCH`), backstop-kill only on overrun; (3) after the scp pull, a
**FAIL-LOUD freshness gate** (`set +e` is active → explicit `exit 1`) rejecting a missing/empty CSV,
span << `DURATION`, or gen_ts offset by hours. The bug it kills: a stale CSV (14.9h offset, 40s span)
silently pulled → fake catastrophic verdict FAIL that was a measurement artifact, not real loss. CSV
format = `tick,gen_ts_ns,flip_ts_ns` (gen_ts_ns = CLOCK_REALTIME epoch ns under `--wall-clock`).
Guard: `tests/harness_recording_e2e_painter_freshness.rs`.

**`obs_phase2.py record --action start` orphan guard MUST poll to idle, never a flat sleep (#355).**
A large orphan recording (a prior aborted run's MP4 — the live 24.5 GB one) takes many seconds to
FINALIZE after `StopRecord`; `StartRecord` returns `{code:500}` while the output is still active and
aborts the run. The start branch reads `GetRecordStatus`, on an active orphan logs a LOUD `WARN`
(box + timecode), `StopRecord`s, then `_wait_record_idle()` POLLS `GetRecordStatus` every
`OBS_RECORD_FINALIZE_POLL_S` (2 s) until `outputActive=False`, bounded by
`OBS_RECORD_FINALIZE_TIMEOUT_S` (120 s) — FAIL LOUD (`SystemExit`) on timeout, never a doomed
`StartRecord`. Guard: `tests/python/test_obs_phase2_record.py`.

## Local test verification gotchas (Tier-0)

- **Python harness tests:** run with `python3 -m pytest tests/python/ -p no:html` — the repo's
  `pytest_html` plugin pulls `jinja2` which isn't installed locally and errors collection without
  `-p no:html`.
- **Rust harness tests are pure static reads** (no probe, default features) — to verify RED→GREEN
  locally without the CI-only `cargo test` (run), `cargo test --no-run --test <name>` (Tier-0
  allowed) then EXECUTE the produced `target/debug/deps/<name>-<hash>` binary directly.

## Reporting Scope — NEVER Claim Partial as Full

"Done/working" for camera-box = full source→endpoint path (cam→strih OBS→stream OBS→endpoint).
NEVER label a loopback or a single hop as "E2E done/working".
State exactly which node/hop a result covers and what it does NOT prove.

**Backlog as of 2026-06-09:** #7 capstone gate, #8 clock sync (ROOT blocker), #9 CI runner,
#11 60fps terminal bar, #20 cam2 drop, #21 strih→stream loss, #22 OBS cleanup,
#23 per-hop latency gate, #24 cam1/3/4 coverage, #25 notify-on-red, #26 audio scope.

File discovered work as GitHub issues immediately, without asking.

## QR painter on/off for ANY frame-loss verification — USE rig-mode.sh (don't fumble it)

The dual-QR painter on cam2 is REQUIRED for every frame-loss / sync verification (it's the optical
tick the cameras film). Without it the cameras show only the operator view → NOTHING decodes (a
blank/operator capture is NOT "the QR is broken/overexposed" — the QR simply isn't painted).

- **Turn QR ON:**  `scripts/rig-mode.sh test`   → switches cam2 (.62) camera-box to no-display
  (frees /dev/fb0, KEEPS capture+emit — #291) + launches `frame-probe --paint-only --dual-qr
  --qr-size 700` + toggles the genlock_burn ON.
- **Turn QR OFF (back to broadcast):** `scripts/rig-mode.sh event` → stops the painter (pidfile),
  removes the no-display drop-in + restarts camera-box on cam2 (--display restored), burns OFF.
  ALWAYS run this before a live event.
- In TEST mode cam2 (.62) PAINTS the monitor on fb0 AND its camera-box stays running in no-display
  mode (still captures /dev/video0 + emits NDI — #291), instead of being fully stopped. So cam2 is
  no longer auto-dropped as a camera in test mode. Whether cam2's emitted NDI actually carries the
  vernier QR depends on the rig HW (its /dev/video0 ShadowCast seeing the painted monitor via the
  split HDMI) — confirm that on the rig (decode cam2's NDI for the QR); the switch alone does NOT
  prove it. cam1/3/4 also film the QR as before.

**GOTCHA — check the painter with `pgrep -x frame-probe` (EXACT name), NEVER `pgrep -f frame-probe`.**
`pgrep -f` matches the whole cmdline → it matches YOUR OWN shell/ssh command that contains the string
"frame-probe" → false "painter still running / respawning" readings (cost real time 2026-06-28; the
rig-mode script header warns about exactly this self-match). Use `pgrep -x frame-probe` or
`ps -C frame-probe` to see the REAL painter process.

**4-camera sync measurement (content alignment, not just FIFO phase):** QR painter ON → screenshot
the strih "Multiview" scene (all cams in ONE frame = simultaneous) at high res → zxing-decode each
camera's cam2 painter `frame_id` → equal frame_id across cameras = they captured the same painted
instant = synced. Proven 2026-06-28: cam1/3/4 13/14 rounds spread 0, max spread 2 frames, at uniform
3ms latency. cv2 QRCodeDetector is too weak for the filmed QR — use zxing-cpp (`pip install
--break-system-packages zxing-cpp`). head_skew from the genlock FIFO audit is NOT content alignment.

## All-Cambox per-SEGMENT continuity (#312 Phase-1) — `recording-verdict --switch-schedule`

The all-cambox E2E (#312) switches each active cambox into strih PROGRAM sequentially (~30s each);
ALL camboxes capture the SAME painted source via the HDMI splitter, so ONE continuous stream
recording must stay continuity-clean across every window — any cambox that drops shows as a break
in ITS window. Phase-1 attributes per cambox by a SCHEDULE (no cambox-id in the pixels yet — that
is Phase-2, a DistroAV burn-filter change held off the 2.5h windows-genlock build).

- **Harness (drives the rig, supervisor — NOT this verdict code):** sequential
  `SetCurrentProgramScene` switch to each active cambox over strih OBS WS, logging
  `(cambox, switch_wall_ns)`; ONE continuous stream recording. Writes the switch-schedule JSON.
- **Verdict (CI-testable, `src/probe/recording_segments.rs` + `recording-verdict`):**
  `recording-verdict --stream <rec> --switch-schedule <schedule.json>` partitions the decoded
  stream frames into per-cambox windows by burn `gen_ts_ns ∈ [start_ns,end_ns)` (minus a 1s
  transition guard on EACH boundary — `--switch-guard-ns`), runs the per-window painted-tick
  continuity (`window_segment`) per window, and emits `all_cambox_continuity` in the `--json`
  summary (per-cambox `frames/undecodable/copies/gaps/pass` + `overall_pass`). It GATES the headline.
- **Schedule JSON:** an ordered, non-overlapping array of `{"cambox":<label>,"start_ns":<i64>,"end_ns":<i64>}`
  on the burn `gen_ts_ns` timeline. Validation rejects overlap / out-of-order / start≥end / empty.
- **`expected_step`** (the painted-tick by-design step) is a PARAMETER, NOT hardcoded: default
  `round(--refresh-hz / --stream-capture-fps)` = 60/30 = 2 for the stream recording; override with
  `--switch-expected-step`. Keeps the check rate-agnostic (cam→strih step 1, strih→stream step 2).
- **Coverage honesty (#301):** a scheduled cambox with ZERO in-window frames FAILs — an absent box
  (e.g. CAM3 down) never reads as a pass. Active set today = CAM1/2/4.

**Phase-2 harness (`recording-e2e.sh ALL_CAMBOX=1`) — RUNS ON THE DEFAULT `VERDICT_ON_STREAM=1`
(#332).** The sweep (`switch_schedule.py` + `obs_phase2.py switch`) writes `switch-schedule.json`
and appends `--switch-schedule` to BOTH verdict paths: the legacy decode-on-dev1 `VERDICT_ARGS`
AND the DEFAULT per-box `MERGE_ARGS` (the `--merge-partials` step). The per-cambox
`all_cambox_continuity` is computed in the SHARED `build_and_print_verdict` (which `run_merge`
calls), so the merge path produces it IDENTICALLY to the fused/legacy path — the all-cambox verdict
now runs ON the stream box (#193, decode where the video lives), NOT forced onto dev1. The old guard
that forced `VERDICT_ON_STREAM=0` is GONE (#332). Just run `ALL_CAMBOX=1 bash scripts/recording-e2e.sh`
(default `VERDICT_ON_STREAM=1`). Sweep config: `CAMBOX_SWEEP` (default **`Cam 5:CAM1 Cam 1:CAM4`** —
the non-painter CAPTURE boxes; **CAM2/.62 the dual-QR PAINTER is EXCLUDED (#333)** — while painting
it emits no camera NDI so its window is empty/frames=0 by construction; CAM3/.63 down #301 also
excluded), `SEGMENT_SECS` (default 30). To prove the painter box itself, a DIFFERENT box must paint
that run (override `$CAMBOX_SWEEP`). A swept box that yields `frames=0` FAILs with an explicit
`CamboxSegment.note` painter/no-emit diagnostic (#333), so an empty window is never misread as chain
loss. Switch boundaries are dev1 epoch-ns (DanteSync-slaved to the painter = the burn `gen_ts_ns`
timeline); a runtime dev1↔painter offset assertion is filed as #326.

**Design gotcha — do NOT reuse `burn_contiguity` for the painted tick (it false-passes at step 1).**
The painted tick is a per-painted-FRAME counter sampled at the cambox rate, NOT a free-running
render-tick counter, so `burn_contiguity_in_window_with_step` is the wrong tool two ways: (1) its
`PerRenderTick` rate IGNORES forward gaps at `expected_step == 1` (a render counter legitimately
ticks faster than frames), masking a real step-1 drop; (2) its `PerEmittedFrame` rate carries the
#226 "duplicate ⇒ BURN-UNREADABLE" reclassification — for a NODE BURN a duplicate id is a
delivered-but-misdecoded frame (not a drop), but for the PAINTED tick a duplicate is a STALE/FROZEN
copy and the tick missing behind a *non-adjacent* freeze is a REAL drop, which that reclassification
silently clears (a FALSE PASS on a zero-loss verdict — caught in code review, ticks `100,101,100,103`).
So `window_segment` computes the painted-tick continuity DIRECTLY: a forward skip beyond
`expected_step` (integer-division excess, crediting `None` frames) or a backward jump is a `gap`; a
repeated tick is a `copy`; `undecodable` is the direct `None` count. It mirrors `burn_contiguity`'s
definitions but treats a duplicate as a copy, never burn-unreadable. Regression-locked by
`non_adjacent_freeze_hiding_a_real_drop_still_fails`.

## Clippy gotcha — `doc_lazy_continuation` under `--all-features`

CI runs `cargo clippy --all-targets --all-features -- -D warnings`. The `doc_lazy_continuation` lint
fires when a doc-comment (`//!` / `///`) line STARTS with a markdown bullet char (`+ ` / `- ` / `* `)
without a blank doc line before the list, or on a wrapped bullet whose continuation is mis-indented.
A `tests/*.rs` doc comment is linted by `clippy --all-targets` even when the test body is
`#![cfg(feature = "probe")]` (the doc is always compiled). Fix: put a blank `//!`/`///` line before a
list, keep bullets single-line, and never start a prose line with `+ `/`- `/`* `.

## Generic rig-step restore wrapper — `scripts/lib/with-rig-restore.sh` (#281 Part A)

**The problem:** recording-e2e.sh has a full `cleanup()` trap; ad-hoc rig steps (a quick deploy, a
single probe run, an MCP-driven scene change) have NO restore net — they can strand the rig in
TEST/BURN state if they crash or are killed.

**The fix:** source the wrapper before any rig step that touches rig state:

```bash
. scripts/lib/with-rig-restore.sh
with_rig_restore [--on-failure] <restore_cmd> -- <step_cmd> [args...]
```

- **Default (no flag):** restore always runs — success OR failure (use when the rig must be left clean unconditionally).
- **`--on-failure`:** restore only when step exits non-zero or is killed (use when the step's own success is the "done" state).
- Restore runs **exactly once** (`_wr_done` idempotency guard — safe when both the exit-code path and a signal trap both trigger).
- Step exit code preserved and returned to the caller.
- `restore_cmd` is `eval`'d — can be a compound shell expression with pipes/variables.
- Arms **HUP / INT / TERM** traps; re-raises the signal after restoring so the caller sees the die.
- **Pure shell — no rig, no ssh** — test with `bash -c '. lib; with_rig_restore ...'`.

10 behavioral tests in `tests/harness_with_rig_restore.rs`. The `_wr_restore_once` helper is defined
inside `with_rig_restore()` as a shell-level function (bash scoping quirk) so trap strings can name it.

**Limitation:** nested `with_rig_restore` calls are NOT supported (inner call overwrites the global
`_wr_restore_once` and breaks the outer guard). Use one wrapper per rig step.

## Resumable / idempotent per-box decode — `--skip-if-exists` on planner scripts (#281 Part B)

`recording-verdict-on-strih.sh` and `recording-verdict-on-stream.sh` accept:

```bash
--skip-if-exists <partial-path>
```

If the partial JSON already exists on dev1 (durable state from a prior run, #208), the planner
prints `SKIP` and exits 0 instead of re-emitting the decode plan. Makes re-dispatched decode workers
idempotent — the same planner call is safe to issue twice.

Pass it BEFORE other flags. When the file is absent, the flag is silently consumed and the plan
emits normally. 5 tests in `tests/harness_verdict_done_marker.rs`.
