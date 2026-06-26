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

**TWO missed-envs silently waste a run (verify BEFORE recording):**
1. **stream's OBS_BURN_QR must be ON** (#195). **The harness now AUTO-CHECKS this** before
   `[5/8] StartRecord` (the `[4b/8]` pre-record burn-ON gate): it runs `obs_burn_filter.py check`
   on strih+stream and ABORTS (exit 1) unless `kind_registered=True` AND `filter_on_input=True`
   — so a burns-OFF/pass-through OBS fails fast instead of wasting a full run. If it aborts,
   relaunch the box's OBS in test mode via `scripts/rig-mode.sh test`. The stream burn (911004)
   only fires if stream's OBS was LAUNCHED with `OBS_BURN_QR=1` — the running OBS does NOT pick up HKLM mid-session.
   Without it the stream recording has NO stream burn → strih→stream can't pair (latency=null,
   `strih_stream_source: two recordings ...`). Fix: relaunch stream OBS with the env set in the
   launching shell (see obs-ops skill: ExitOBS → force-kill → clear `.sentinel\*` → relaunch
   `cwd=bin\64bit` with `$env:OBS_BURN_QR=1; $env:OBS_BURN_RUN_ID=911004; $env:OBS_GENLOCK_WALL_CLOCK=1; $env:OBS_GENLOCK_LATENCY_MS=3`).
   (#235: `OBS_GENLOCK_LATENCY_MS=3` is THE single genlock latency knob — it implies ts-align on and
   auto-derives the internal FIFO depth, so `OBS_GENLOCK_RESERVE_MS`/`_TS_ALIGN`/`_PRELOAD_FRAMES` are
   no longer needed; `OBS_GENLOCK_RESERVE_MS=3` still works as the back-compat alias.)
   Verify the OBS log: `[burn] filter created: enabled=yes run_id=911004` AND `genlock: latency = 3 ms`.
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

## Rig TEST / EVENT Mode Switch (#247) — `scripts/rig-mode.sh`

THE deterministic, single-source-of-truth switch between TEST mode (QR/E2E measurement) and EVENT
mode (clean prod broadcast). Replaces the ad-hoc, context-dependent switching that caused #246 — a
burn left ON in the prod **Machine** env painted QR on the LIVE broadcast, and genlock left in a test
state. The settings below are PINNED in the script; do NOT improvise them.

Run from dev1 (ssh to the cam boxes is ALLOWED; ssh to the Windows boxes is DENIED, so the OBS side is
PRINTED as the exact `launch-obs-genlock.sh --mode` step to paste into the box's win-* MCP Shell):

```bash
scripts/rig-mode.sh test     # rig INTO test mode (paint QR + print OBS burns-ON step)
scripts/rig-mode.sh event    # rig BACK to clean broadcast (stop QR + print OBS burns-OFF step)
```

**TEST mode (pinned):**
- **cam2 (10.77.9.62):** `systemctl stop camera-box` (frees /dev/fb0) → launch the painter
  `frame-probe --paint-only --dual-qr --qr-size 700 --duration-secs N` (700px = the validated
  vernier), PID → `/run/rig-painter.pid`. Painter binary = `/usr/local/bin/frame-probe` from the CI
  `probe-tools-linux-amd64` artifact (`gh run download <run> -n probe-tools-linux-amd64` → scp to
  cam2); TEST mode **FAILS LOUD** if it is absent. For a measurement run add `PAINTER_EXTRA_FLAGS="--wall-clock --run-id <N>"`.
- **cam1 (10.77.9.61):** NOT reconfigured — runs its DEPLOYED service (already 30 fps / certified v4l2
  saturation=0 contrast=75), the recording convention.
- **strih + stream OBS:** `scripts/launch-obs-genlock.sh --box {strih|stream} --mode test --force` —
  burns ON in the **LAUNCH SHELL ONLY** (NEVER Machine): `OBS_BURN_QR=1`, `OBS_BURN_QR_PX=300`,
  `OBS_BURN_RUN_ID` = 911002 (strih) / 911004 (stream). Genlock env stays from Machine. Then confirm
  the PHASE2-PROBE scene + native-1080p recording (#225).

**EVENT mode (pinned — the #246 guard):**
- **cam2:** stop the painter via its PID file (NOT `pkill -f frame-probe` — a shell whose cmdline
  contains "frame-probe" would self-kill; `pkill -x frame-probe` is the safe fallback) →
  `systemctl start camera-box` → verify active + `--display` restored (re-holds /dev/fb0).
- **strih + stream OBS:** `scripts/launch-obs-genlock.sh --box {strih|stream} --mode event --force` —
  burns CLEARED in the launch shell AND asserted ABSENT from Machine; the wrapper **REFUSES to launch
  (exit 8)** if any `OBS_BURN_*` is set Machine-wide. PROD genlock latency stays from Machine
  (currently FRAME mode: `OBS_GENLOCK_LATENCY_MS=0`, per-source preload).

**Burn run_ids are the single source of truth** in `src/probe/recording_latency.rs`
(`BURN_RUN_ID_STRIH=911002`, `BURN_RUN_ID_STREAM=911004`, `BURN_RUN_ID_CAM1=911001`), mirrored by
`launch-obs-genlock.sh::burn_run_id_for_box`. **NEVER set `OBS_BURN_*` in Machine scope** — it
survives reboot and is exactly the #246 live-broadcast contamination. The cam-box root pw is `$CAM_PW`
(dev-rig LAN default, as in the sibling e2e scripts — override from your password store).

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

## Reporting Scope — NEVER Claim Partial as Full

"Done/working" for camera-box = full source→endpoint path (cam→strih OBS→stream OBS→endpoint).
NEVER label a loopback or a single hop as "E2E done/working".
State exactly which node/hop a result covers and what it does NOT prove.

**Backlog as of 2026-06-09:** #7 capstone gate, #8 clock sync (ROOT blocker), #9 CI runner,
#11 60fps terminal bar, #20 cam2 drop, #21 strih→stream loss, #22 OBS cleanup,
#23 per-hop latency gate, #24 cam1/3/4 coverage, #25 notify-on-red, #26 audio scope.

File discovered work as GitHub issues immediately, without asking.
