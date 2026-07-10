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
   The per-recording undecodable / 60→30-beat metrics are **DIAGNOSTIC only — they do NOT gate the
   headline** (an old overstatement was "PASS = 0 undecodable AND 0 copy AND 0 gap AND span≥300s" —
   that conflated diagnostics with the gate).
   **#373 nuance on the analyzed span (`--min-secs`)**: a SHORT-BUT-REAL run is NOT failed merely for
   a short diagnostic NOTE — BUT a **COLLAPSED / partial optical span DOES gate the headline now**.
   The headline ANDs each node's `is_zero()` (delivery + #363 optical + #364 colour) with a
   **duration FLOOR**: the analyzed OPTICAL span (the cam2 dual-QR FIRST..=LAST decoded-frame window,
   `NodeVerdict.optical_span_frames / capture_fps`) must be `>= min_secs` (default 300 s). Reason: a
   green-cast / dying cam2 read shrinks the span to a handful of frames (or 0); over that truncated
   span `optical_undecodable==0` and the burn window is trivially contiguous, so `is_zero()` ALONE
   vacuously PASSES (a fake green — live repro: `nodes.strih.analyzed_secs=0.0`, `overall_pass=true`).
   The floor rejects ONLY the collapsed read; it never fails a genuine `>=min_secs` run. The PASS/FAIL
   decision is the pure Tier-0 `recording_span_gate` module (`span_secs` + `analyzed_span_long_enough`);
   `is_zero()` itself is UNCHANGED (still the per-node delivery gate — the duration floor is a
   run-level headline term). Per-node JSON carries `analyzed_secs`/`span_ok`/`min_secs`.
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

**Vernier methodology at 60fps capture — RESOLVED (#310 closed).** The Vernier
(`tick = max(left_even, right_odd)`) was designed to resolve the 60→30 BEAT between a 60Hz painter
and a 30fps camera. At 60fps painter + 60fps capture (#11) that beat is gone. The production
zero-loss gate has since moved OFF the optical dual-QR read entirely onto the DIGITAL burn-ID
sequence-contiguity check (`src/probe/burn_contiguity.rs` #186 + `recording_segments.rs::segment_continuity`
#312, run across the whole active fleet) — so #310 and the re-gate-on-optical decision (#363) are
both CLOSED. The dual-QR still emits per-frame unique ids (anti-blur), but it is no longer the gate;
do NOT re-open the optical-vs-digital question unilaterally (see #95).

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
Re-confirmed LIVE 2026-07-09 (via linux-cam1/cam3/cam4 MCP): all three STILL have no `/dev/fb*`
and every `card0-HDMI-A-*` connector reports `disconnected` — cam3's SSH is reachable now
(contrary to an earlier 2026-07-05 note that it was down); worth re-checking whenever this
hardware gap is revisited.

## `recording-e2e.sh` now drives cam1, cam3, cam4, cam5, OR cam6 as the SOURCE camera (#24 item 1, PR #631; #312 item 1 added cam5/cam6)

`CAM=cam1|cam3|cam4|cam5|cam6` (env var, defaults to `cam1` for back-compat) selects which
physical box plays the SOURCE-camera role for the single-node full-path launch (the box filming
cam2's monitor + carrying the #174 capture burn). Resolution chain: `camera_resolve("$CAM")`
(pre-existing, `scripts/camera-set.sh`) sets `CAMERA_IP`/`CAMERA_SOURCE`; `camera_strih_route
("$CAMERA_NAME")` (same file) resolves which strih OBS scene/NDI-input shows that camera
(mirrors `scripts/set-ndi-mapping.py`'s fixed 6-distinct pins).

**cam2 is DELIBERATELY, PERMANENTLY excluded from `camera_strih_route()`** — never "add it
later", this is a structural fact, not a TODO: `recording-e2e.sh`'s `$CAMERA_NAME` (SOURCE role)
and `$PAINTER_IP` (cam2's own fixed IP) are TWO SEPARATE selections that would collide if cam2
could be picked as SOURCE — a real `/dev/video0` + `/dev/fb0` device conflict on the SAME
physical box. This does NOT mean cam2 is unmeasurable: `#312` wires cam2 in as a "camera under
test" for the ALL_CAMBOX sweep's digital-burn contiguity check through a COMPLETELY SEPARATE
path (its own `BURN_RUN_ID_CAM2`, deployed via the `[2b/8]` loop keyed off `$PAINTER_IP` directly,
never through `camera_strih_route`) — see "All-Cambox per-SEGMENT continuity" below. Two
different questions, two different mechanisms: "which box faces cam2's monitor as SOURCE" vs
"is cam2's OWN capture chain zero-loss".

`recording-verdict` (Rust) reads `--burn-cam1/2/3/4/5/6-run-id` as six independent roles
(`CAMERA_UNDER_TEST_NODES`) with per-role CLI defaults matching the shell's own reserved
constants (911001/911009/911008/911007/911010/911011 for cam1/2/3/4/5/6) — deploying the
resolved camera under ITS OWN matching id is all that's needed. `ALL_CAMBOX=1` + a non-cam1
`CAM` is rejected loudly (ALL_CAMBOX's own secondary-camera loop already deploys
cam2/cam3/cam4/cam5/cam6 at fixed IPs — picking one of them as the primary too would
double-deploy the same physical box). **When adding a new camera-under-test burn id in
general, see `.claude/skills/recording-decode`'s `NODE_BURN_RUN_IDS` GOTCHA — reserving the
`BURN_RUN_ID_*` constant is NOT enough by itself, there are ~5 other sites that need it too.**

**Known follow-up gap (#632, NOT yet fixed):** `recording-verdict.rs`'s fast/robust decode-path
gate (`args_expected_burns_for`/`decode_for`) and its `cam2→cam1` report label are STILL
hardcoded to cam1's burn id (911001) — harmless for correctness (the robust path is a strict
superset, the #186 0-miss guarantee holds), but a live run with `CAM=cam3`/`CAM=cam4` will hit
a ~10x slower decode (every frame takes the expensive robust path since cam1's id is never
found) and the report will mislabel the hop `cam2→cam1` even when cam3/cam4 was actually under
test. Fix before running the item-3 live 4-camera sweep, or budget the extra decode time.

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

**Pulling a LARGE partial JSON (multi-MB) back via win-* MCP `FileDownload` overflows the inline
tool result (2026-07-09).** For a strih/stream partial in the multi-MB range, the raw call errors
with "result (N characters) exceeds maximum allowed tokens" and instead SAVES the tool result to
a file at the path the error message gives you — that file is JSON `{"result": "[task:<id>]
base64:<N>bytes:<b64-data>"}`, NOT a plain base64 string. Extract with a regex, not a bare
`base64 -d`:
```python
import json, base64, re
d = json.load(open(saved_path))
b64 = re.search(r'base64:\d+bytes:(.*)$', d['result'], re.DOTALL).group(1)
open(dest_path, 'wb').write(base64.b64decode(b64))
```
(A bare `jq -r '.result' file | base64 -d` fails with "invalid input" because of the leading
`[task:...] base64:Nbytes:` prefix.)

**Running `recording-verdict.exe --extract-partial` INLINE via win-* MCP `Shell` dies at the ~30-300s
MCP idle-timeout (2026-07-09) — launch it DETACHED with `Start-Process` and POLL instead.** A
foreground `& "recording-verdict.exe" ...` blocking call gets killed by the MCP tool's own idle
timeout partway through a multi-minute decode (a ~360s recording decodes in ~5-8 min), and — unlike
a bare shell timeout — the underlying process appears to die WITH the aborted MCP call (confirmed:
`Get-Process` showed nothing running afterward, no partial output file). Fix (mirrors the
`obs_phase2.py record` detached pattern already documented above): launch via `Start-Process
-RedirectStandardOutput <log> -RedirectStandardError <log> -PassThru -NoNewWindow` (returns
immediately with a real, independently-running PID even though the LAUNCHING Shell call may itself
still hit its own timeout/abort — that's fine, the spawned process survives it), then poll with a
separate cheap `Shell` call (`Get-Content <log> -Tail N` + `Get-Item <partial.json>` +
`Get-Process -Id <pid>`) every few minutes until the partial JSON appears. Quote space-bearing
paths (`` `"$rec`" `` inside the `-ArgumentList` string) exactly as the existing stream-decode
gotcha above already warns.

**A `RecordingPartial` schema-version bump (`schema_version: N`) needs redeploying `recording-verdict`
on EVERY decode box, not just strih/stream.** The merge step hard-rejects a stale partial
("schema_version 2 is not supported, this build expects 3") — this includes **imag-nb**, which
decodes over plain `ssh`/`scp` (not a win-* MCP box) and is easy to forget since its own deploy
step (`recording-verdict-on-imag.sh`) SKIPS re-uploading when a same-named binary is already
present+executable ("skipping upload") — it does NOT check whether that binary is stale relative
to the schema bump. Before trusting an imag `--extract-partial` after ANY Rust change to the
`RecordingPartial` struct, `scp` a fresh CI-built `recording-verdict` to imag-nb first
(`scp <linux-probe-tools>/recording-verdict newlevel@10.77.9.182:/home/newlevel/recording-verdict`)
and re-run its extract, rather than assuming a same-named binary is current.

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
    `classify_patch` (chromatic: Grayscale if chroma<40 / HueShift if hue err>30° / else Pass —
    neutral: NeutralTint if chroma>48 / else Pass) + `summarize_node_colour` (strict-majority vote
    over sampled frames). A burn-covered patch is `Unsamplable` → SKIPPED, not charged (a real colour
    defect is global → still fails on the visible patches); fail-closed only when NOTHING is checkable.
  - **#364 calibration — gate on HUE + CHROMA, NOT brightness.** The level/sRGB-distance check was
    REMOVED: the rig's optical capture is ~7× DIM by physics (60 Hz monitor + 1/1000 s shutter samples
    ~1 ms mid-redraw), so a distance/level check only false-fails a correct camera. REAL per-patch
    measurement of a genuine cam1 chain frame: every patch correct hue (≤19.6°) + chroma (≥50
    chromatic, ≤38 neutral cast), 9/13 failed ONLY on level. Dropping it is NOT weakening — every real
    fault (grayscale collapse, dead channel, hue shift, WB cast) still fails on hue/chroma. Locked by
    `real_rig_dim_capture_passes_while_grayscale_and_dead_channel_fail` with genuine sampled values
    (real→PASS 13/13, grayscale→FAIL, dead-red→FAIL). `NEUTRAL_CHROMA_MAX`=48 (real cast maxes 38).
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
- **`bin/recording-verdict` is `required-features=["probe"]` → NOT compiled on default features.**
  So `cargo check` / `cargo clippy --all-targets` (default) SKIP it entirely — local cheap checks do
  NOT compile-verify ANY edit to that binary; CI (`--all-features`) is the only compile gate.
  Consequences: (1) put the GATE DECISION in a pure crate-root module (e.g. `recording_span_gate`
  #373, `colour_verify` #364, `reannounce` #297) so RED→GREEN is observable on default features via
  `cargo test --lib <mod> # airuleset:build-ok`; the probe-gated binary just calls it. (2) A
  test-ONLY free function at the binary's MODULE scope triggers `dead_code` in the NON-test bin build
  (CI `-D warnings`) — keep test helpers INSIDE `mod tests` (a `fn` in `mod tests` calling
  `super::…`), never a module-level wrapper used only by tests (#374: the `node_verdict` test helper
  lives in `mod tests`; the headline path uses `node_verdict_with_optical`). `cargo fmt` DOES format
  the probe binary (rustfmt doesn't compile), so `cargo fmt --all --check` still catches its layout.

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
  (e.g. CAM3 down) never reads as a pass. Active set today (#312 items 1+3) = ALL SIX cameras
  (CAM1/2/3/4/5/6) — see the updated Phase-2 paragraph below.

**Phase-2 harness (`recording-e2e.sh ALL_CAMBOX=1`) — RUNS ON THE DEFAULT `VERDICT_ON_STREAM=1`
(#332).** The sweep (`switch_schedule.py` + `obs_phase2.py switch`) writes `switch-schedule.json`
and appends `--switch-schedule` to BOTH verdict paths: the legacy decode-on-dev1 `VERDICT_ARGS`
AND the DEFAULT per-box `MERGE_ARGS` (the `--merge-partials` step). The per-cambox
`all_cambox_continuity` is computed in the SHARED `build_and_print_verdict` (which `run_merge`
calls), so the merge path produces it IDENTICALLY to the fused/legacy path — the all-cambox verdict
now runs ON the stream box (#193, decode where the video lives), NOT forced onto dev1. The old guard
that forced `VERDICT_ON_STREAM=0` is GONE (#332). Just run `ALL_CAMBOX=1 bash scripts/recording-e2e.sh`
(default `VERDICT_ON_STREAM=1`). Sweep config: `CAMBOX_SWEEP` (default, since #312 items 1+3:
**`Cam 5:CAM1 Cam 1:CAM3 Cam 3:CAM4 Cam 2:CAM2 Cam 4:CAM5 Cam 6:CAM6`** — ALL SIX cameras, incl.
cam2), `SEGMENT_SECS` (default 30).

**cam2's #333 exclusion was CORRECTED by #312 — do NOT reintroduce it.** #333 originally excluded
CAM2/.62 (the dual-QR painter) reasoning "while painting it emits no camera NDI so its window is
empty/frames=0 by construction". That went stale the moment `#291` landed: cam2's camera-box
daemon keeps CAPTURING + EMITTING its own NDI feed throughout a TEST run (`CAMERA_BOX_NO_DISPLAY=1`
frees only its framebuffer for the separate painter process) — so cam2's OWN chain is now measured
too, via its own reserved digital burn (`BURN_RUN_ID_CAM2`, see `.claude/skills/recording-decode`).
If you ever see code/docs saying "cam2 is excluded from the sweep, it can't also be captured" —
that claim is WRONG and stale; verify live (`systemctl is-active camera-box` on cam2 during a
TEST-mode run) before trusting an old comment over the current `#291`/`#312` behavior.

CAM3/.63 down #301 is excluded only when genuinely unreachable — the default sweep above assumes
all six are up; drop a box from `$CAMBOX_SWEEP` if it's known-down that run. To prove the painter
box's OPTICAL role specifically (not its digital chain), a DIFFERENT box must paint that run
(override the painter's IP/setup separately — the sweep's inclusion of "Cam 2:CAM2" is about cam2's
digital chain, unrelated to which box paints the shared optical tick). A swept box that yields
`frames=0` FAILs with an explicit `CamboxSegment.note` painter/no-emit diagnostic (#333), so an
empty window is never misread as chain loss. Switch boundaries are dev1 epoch-ns (DanteSync-slaved
to the painter = the burn `gen_ts_ns` timeline); a runtime dev1↔painter offset assertion is filed
as #326.

## `all_cambox_latency`/`cross_camera_spread_ms` (#624) measures SOURCE-side `d_X` — it can NEVER prove #286's receiver-side phase-sync claim (2026-07-09)

**`cross_camera_spread_ms` (the `#624 ALL-CAMBOX per-camera cam2->camera latency` block in
`recording-verdict.rs`, calling `cam2_cam1_samples_from_burn`/`_from_flip`) computes
`camera_burn.gen_ts_ns − cam2.gen_ts_ns` per camera — each camera's own SOURCE-SIDE
photon-to-CAPTURE latency (`d_X`, the #286 root-cause quantity).** This is the right metric for
#624's own question ("is the raw per-camera capture latency spread small enough that a live
program CUT between two cameras won't visibly break lipsync") — but it is computed **entirely on
the source side and never touches strih's receiver-side `genlock_latency_ms_src`**, the exact knob
#286's phase-sync fix adjusts. Re-measuring `cross_camera_spread_ms` after applying differentiated
per-source receiver latencies can **never** show a collapse (barring real physical drift in the
cameras themselves) — it isn't sensitive to what the fix changes. A live re-run confirmed this: the
differentiated offsets were verified still applied throughout, yet the spread didn't move in the
expected direction at all (see #286's 2026-07-09 re-verification comment for the real numbers).

**#286's own Verify criterion needs a DIFFERENT metric: `strih_burn.gen_ts_ns − camera_burn.gen_ts_ns`
(the DELIVERY latency, which DOES include the genlock receiver hold)** — this is exactly what
`src/probe/recording_latency.rs::n_camera_strih_samples` / `n_camera_median_latency_ms` already
compute (doc comment: *"the #286 phase-sync measurement input to `compute_phase_sync_offsets`"*,
2 unit tests). Before claiming ANY #286 phase-sync re-verification proves or disproves the fix,
confirm which of the two quantities is actually being read — `all_cambox_latency`/
`cross_camera_spread_ms` (source `d_X`, existing #624 gate, wrong tool for this) vs.
`all_cambox_delivery_latency`/`cross_camera_spread_ms` (delivery latency, the right tool).

**WIRED (2026-07-09, same-day follow-up PR):** `n_camera_strih_samples` is now called from
`recording-verdict.rs`'s ALL_CAMBOX `--switch-schedule` block, reading the **STRIH recording**
(not stream — each camera's own digital capture burn rides into strih's PROGRAM output during its
own cut-in window, co-located with strih's own render burn in the same recorded frame; no window
partitioning needed, since only the currently-cut-in camera's burn is ever present in a strih
frame). Reported as `all_cambox_delivery_latency` — per-camera `HopLatency` JSON keyed by
`CAMERA_UNDER_TEST_NODES` label (all SIX cameras, **including cam2** — cam2 has its own digital
capture burn + its own `--switch-schedule` window, so it needs no optical read to be measured
here, unlike the `all_cambox_latency` OPTICAL-INJECTION sweep which structurally excludes it) plus
a `cross_camera_spread_ms`/`spread_gate_pass` summary (reusing `switch_latency::spread_verdict`,
the same 16ms threshold as #624 — **report-only, does NOT fold into `all_pass`**, since #286 is
not yet a proven/closed standing requirement). Absent (`null`) when no `--strih` recording was
supplied.

**#286 CLOSED (2026-07-09) — proven live on all 6 cameras.** The first live re-verification run
with this field found only 1 of 6 cameras producing samples — root cause + fix below. The
second re-verification run (RUN_ID 1783619061, with the fix) measured real
`all_cambox_delivery_latency` samples for all 6 cameras (~1800 each): cam1=71.08ms,
cam2=72.15ms, cam3=71.00ms, cam4=68.80ms, cam5=77.70ms, cam6=78.95ms — cross-camera spread
10.16ms, comfortably under the 16ms/half-frame threshold. Full evidence on the closed ticket.

**GOTCHA — `all_cambox_delivery_latency` needs strih's OWN burn on EVERY strih NDI input during
the sweep, not just the single default program source (fixed in `recording-e2e.sh`).** The
metric pairs each camera's own capture burn against **strih's own render-time burn** (911002) —
but the `[4b/8]` pre-record burn-ON gate used to only ever turn that burn on for the ONE default
`STRIH_PROG_SOURCE` (`NDI cam5` under the plain single-camera path). During any OTHER camera's
`--switch-schedule` window, its strih input never had the burn filter enabled, so strih's burn
was never drawn into those frames — the pairing legitimately found zero samples for the other 5
cameras (confirmed live). Fixed by extending the shared `BURN_TARGETS` array (the SAME array the
ON-gate and the `cleanup()` OFF-clear loop both iterate — see the `#252` design comment in
`recording-e2e.sh`) to cover all six canonical strih NDI inputs under `ALL_CAMBOX=1`, excluding
whichever one is already the current default. If you add a NEW per-camera strih-burn-dependent
metric in the future, this coverage is now automatic — no further harness change needed.

**GOTCHA — do NOT feed `all_cambox_delivery_latency` numbers straight back into
`phase_sync_calibrate.py --measured-json` for a "final" recalibration once differentiated
offsets are ALREADY applied.** The script's offset math (`compute_phase_sync_offsets`) expects a
CLEAN baseline measurement — each camera's natural cam→strih latency before any differentiated
per-source hold was layered on. Feeding it delivery-latency numbers measured WHILE non-uniform
holds are already live corrects an already-corrected system (confirmed: re-feeding this run's
numbers back in proposed a DIFFERENT offset set, 11/10/11/13/4/3ms, that would only make sense
starting from a reset). If the measured cross-camera spread already passes (as it did for #286),
there's no need to chase a "more optimal" recalibration — only reset every source to the floor
and re-sweep first if you genuinely need a from-scratch baseline.

**Reusable technique — transferring a multi-MB file dev1↔win-* MCP box without blowing up
context or the MCP call size.** `win-strih`/`win-stream-snv` `FileUpload`'s `data_base64` param
inlines the WHOLE file as a tool-call argument — fine for a few KB (a CSV), but a multi-MB binary
(e.g. a freshly CI-built `recording-verdict.exe`) as base64 text is far too large to pass inline.
Instead, serve it over the LAN: `python3 -m http.server <port> --bind <dev1's LAN IP>` from the
directory holding the file (dev1 and the rig Windows boxes share the SAME `10.77.9.x` subnet —
confirm dev1's LAN IP via `ip -4 addr show`, NOT `100.x` tailscale or `172.x` docker), then on the
Windows box: `Invoke-WebRequest -Uri "http://<dev1-ip>:<port>/<file>" -OutFile <dest>`. Verify the
`Length` matches the source file's size after both `FileUpload`-of-something-small (CSV) and this
HTTP-pull-of-something-large (the `.exe`) to confirm the transfer landed intact. Pick an unused
port (`curl -s -o /dev/null -w '%{http_code}' http://<ip>:<port>/<file>` — `Address already in
use` means try another).

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

## Verdict ACCOUNTING seam — headline COUNT vs pass/FAIL are separate (safe place to fix over-counts)

The recording-verdict has TWO independent layers; know which you are touching:

- **Pass/FAIL gate** = `NodeVerdict::is_zero()` = `contiguity.is_contiguous() && optical_undecodable==0 && colour_fail==0`, and `is_contiguous()` = `first_id.is_some() && missing_ids.is_empty()` (`burn_contiguity.rs`). **Only `missing_ids` decides zero/not-zero.**
- **Headline COUNT** = `total_real` / `total_burn_unreadable` = counts over `NodeVerdict.classified[].kind` (`RealDrop` vs `BurnUnreadable`). This is presentation only.

**The safe seam:** re-classifying an id `RealDrop`↔`BurnUnreadable` in `classified` changes the HEADLINE COUNT but NEVER `is_zero()` (a BURN-UNREADABLE id is still in `missing_ids` → still non-contiguous → still FAILs). So an over-count fix that ONLY edits `classified[].kind` and NEVER touches `missing_ids` **cannot create a false ZERO**. This is how #356 was made strict-safe. If you ever need to make a node PASS, you must change `missing_ids` — that is the dangerous path, gate it hard.

**#356 cross-recording reconciliation** (`src/burn_reconcile.rs`, pure Tier-0 kernel): in the shared verdict loop, for the cam1 node only and only when `strih_data.is_some()`, a cam1 id classified `RealDrop` from the clean upstream STRIH recording that IS decoded in the DOWNSTREAM stream recording (`burn_ids_in(stream_frames, burn_cam1_run_id)`) was proven delivered → downgrade to `BurnUnreadable`. SAFETY: an id absent from the stream recording (genuine loss OR 30fps-decimated) is NEVER downgraded — stays `RealDrop`. Gate on `strih_data.is_some()` is load-bearing: without it cam1 falls back to reading the stream recording itself and every id is vacuously "present" → mass false downgrade.

**Any change in `build_and_print_verdict` applies to BOTH fused and merge** — `merge_of_partials_reproduces_the_fused_verdict` asserts the two JSONs are byte-identical. So (a) never make the merge path diverge from fused, and (b) that test's synthetic `window(n, with_stream, cam1_gap_at)` data must stay self-consistent: a cam1 gap that is present downstream is NOT a real drop under #356, so to test a GENUINE real drop inject the gap into BOTH the strih and stream `window()` calls (`Some(g)` on both).

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

## #365 frozen-camera gate — pre-record freshness check (NDI inputs, not Multiview tile)

**Purpose:** Block a recording run if any camera feed is frozen (stuck NDI repeating the same
frame), which would produce an undecodable or loss-inflated recording and waste the whole run.

**Step position:** `[4c/8]` — immediately after the burn-ON gate `[4b/8]`, before `StartRecord [5/8]`.

**Why raw NDI inputs, not the Multiview tile:**
The Multiview tiles on strih contain animating overlays (AbleSet spinner, CG text) that update
continuously even when the underlying camera NDI is stuck. Hashing a Multiview tile would
**false-pass** a genuinely frozen camera. Instead we hash the raw OBS source named `NDI cam1`,
`NDI cam2`, etc. via `GetSourceScreenshot {sourceName: "NDI cam1", imageFormat: "png",
imageWidth: 320}`. A live-but-dark camera still has sensor noise (hash changes every sample at
mean luma ~5–7); a frozen NDI repeats identical PNG bytes → STATIC → FAIL.

**Precondition (#276):** the Multiview projector must be OPEN on strih so all raw NDI inputs
are rendering. A source that is not rendering produces all-black frames (identical hashes) and
is correctly detected as FROZEN — the right fail-safe to prevent wasting a run on a dead feed.

**Sources checked by default:** `NDI cam1, NDI cam2, NDI cam3, NDI cam5` (the raw strih inputs).

**Threshold:** > 3 consecutive identical hashes = FROZEN. A run of ≤ 3 identical is allowed
(e.g. sensor temporarily settling). Fail-closed: < 2 successful samples also = FROZEN.

**Components:**
- `src/frozen_camera.rs` — pure Tier-0 Rust decision (`frozen_cameras(&timelines, threshold)`),
  unit-tested with `cargo test --lib frozen_camera # airuleset:build-ok`.
- `src/bin/frozen-camera-gate.rs` — thin CLI binary (default features, no probe deps): reads
  per-camera hash timeline as JSON from stdin, exits 0 (PASS) or 1 (FROZEN + names).
- `scripts/frozen-camera-gate.py` — Python OBS-WS harness: connects to strih, captures
  `--samples` screenshots at `--cadence` seconds, builds the timeline, calls the Rust binary.
- `tests/harness_frozen_camera_gate.rs` — content-assert guards: recording-e2e.sh references
  the gate, the Python uses `json.dumps`, the skill documents it.
- `tests/python/test_frozen_camera_gate.py` — pure Python unit tests (`_hash_png`,
  `_extract_png`, `_build_timeline_json`).

**Env overrides in recording-e2e.sh:**
```bash
FROZEN_CAM_THRESHOLD=3      # consecutive-static threshold (default 3)
FROZEN_CAM_SAMPLES=8        # samples to collect (default 8 → ~8 s window)
FROZEN_CAM_SOURCES="NDI cam1,NDI cam2,NDI cam3,NDI cam5"  # sources to check
FROZEN_GATE_BIN=/path/to/frozen-camera-gate  # binary path (auto-discovered via PROBE_BIN_DIR)
```

**Binary discovery in frozen-camera-gate.py:**
1. `--verdict-bin` CLI arg or `FROZEN_GATE_BIN` env var
2. `$PROBE_BIN_DIR/frozen-camera-gate` (same dir the E2E harness uses for probe tools)
3. `<repo>/target/release/frozen-camera-gate` (local dev build)

**CI artifact:** `frozen-camera-gate` is built (default features) alongside the probe tools
and uploaded into the `probe-tools-linux-amd64` artifact.

---

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

`scripts/recording-verdict-on-imag.sh` (#462) shares the exact same `--skip-if-exists` contract.

---

## imag-nb — the 3rd recorded node (EPIC #466 Topology v2, #461/#462/#463)

`recording-e2e.sh` now records+decodes imag-nb (10.77.9.182, the 60fps low-latency IMAG box)
alongside strih+stream: `[0/8]` reachability, `[4d/8]` render-budget (`--box imag=…:60`),
`[5/8]`-`[7/8]` StartRecord/StopRecord over OBS WS, `[8/8c]` decode+merge as a third
`--merge-partials imag=...` partial. imag's zero-loss proof is the cam2 OPTICAL tick sequence
(60fps captures the 60Hz painter, no 60→30 beat like strih/stream's hops) ANDed with its OWN
911003 digital corner burn's contiguity when present (`recording-verdict --imag`, #463).

**#580v2 — imag's optical gate is RUN-LENGTH no-copy/liveness, NOT surplus — and the digital burn
is the fail-closed delivery authority.** The FIRST #580 fix (`surplus <= 0` optical gate) shipped
BROKEN: it FALSE-FAILED the genuinely-zero run 572001 (two free-running same-rate clocks leave an
unavoidable tiny CLOCK RESIDUAL — real numbers, post-#575 trim + #576: expected_count=21870,
frames_count=21867, present=21851, missing=19, surplus=**+3**, avg_step≈1.000137, digital burn
0-missing) AND, two adversarial Opus reviews proved, FAKE-GREENED a content FREEZE (skips ≡ dups
conserve the frame count → `surplus ≈ 0`, endpoints unchanged → `avg_step ≈ 1`). The committed
572001 fixture had been SIGN-FLIPPED to −3 (trimmed range − UNtrimmed frames), which is why the unit
tests were green while the real binary false-failed — **unit fixtures are NOT sufficient; the
supervisor re-decode of 572001 is the real proof.** The honest gate uses RUN-LENGTH:
- **Optical (`OpticalBeatVerdict::is_live_no_copy`)** = the read genuinely ADVANCES (a LOOSE
  liveness band, `avg_step` rounds to the expected step — rejects frozen/blank + gross rate/alias)
  AND the MAX consecutive Δtick==0 run ≤ `IMAG_OPTICAL_MAX_STUCK_RUN` (K=3). A benign beat dups a
  tick at most ONCE in a row (run ≤ 1); a copy/freeze runs into the hundreds — so run-length catches
  the freeze the aggregates AND the render-free-running burn all miss. `surplus`/`avg_step`/
  `is_net_zero` are now DIAGNOSTIC ONLY. Distributed real drops are NOT the optical leg's job (it's
  a validity gate) — they show as a burn gap.
- **Digital burn = the SOLE per-frame delivery authority, HARDENED + FAIL-CLOSED.** `imag_burn_ok`
  now requires the burn genuinely PRESENT (`burn_present_ok`: `present_count >= optical_frames *
  MIN_BURN_PRESENT_FRACTION` — frame-scale to frame-scale, `step` plays NO role here; an earlier
  draft divided by `step` and was adversarially proven fail-open, loosening the floor to 16.7% of
  the recording at the real rig's step 3 instead of the intended 50% — external optical-frame
  reference — folds in #584 frozen-burn + #585 absent-burn) AND contiguous (`burn_step_contiguity`)
  AND `calibrate_burn_step` CLAMPED to ≤ `IMAG_BURN_RENDER_STEP * 2` with tie-to-SMALLER (a
  tie-to-larger draft was also adversarially proven fail-open — it can MASK a real drop outright,
  not just mis-count it — so the original safer smaller-delta tie-break stays). A recording with
  NO burn now FAILS fail-closed (was a vacuous `optional_signal_ok(None)==true` pass).
- **imag JSON fields:** `imag_optical_beat_pass` (the GATE), `imag_optical_max_stuck_run` (the
  supervisor reads this from the live 572001 re-decode to validate K — if the real value exceeds K,
  K is re-grounded, not loosened), `imag_burn_present_ok`; `imag_optical_beat_net_zero` is now the
  DIAGNOSTIC `is_net_zero` (surplus<=0), NOT the gate. A beat-compensated PASS is reported HONESTLY
  (never "CONTIGUOUS", never a false "surplus ≤ 0" claim); a fail prints a reason (frozen "did NOT
  advance" / "COPY/FREEZE" / burn "ABSENT or below the present floor") — never a silent verdict.
- **Honest limitation (120Hz makes it rigorous):** on the 60/60 rig the run-length guard is a
  HEURISTIC (robust because the beat physically cannot produce a long Δ0 run, but a heuristic). A
  120Hz monitor makes copy-detection RIGOROUS (at 2:1 a copy shows as Δ0 where it must be 2).
- **#588 — CLOSED: the systematic short-run "catch-up judder" gap, fixed by a 4th orthogonal
  Δ0-DENSITY term.** A systematic short-run stutter (many Δ0 runs each ≤K, dups balanced by
  catch-up skips so `avg_step≈1`/`surplus≈0`) evaded run-length (`no_stuck_copy` sees only the
  LONGEST single run), the whole-window aggregates, AND the render-free-running digital burn
  alike. Fix: `OpticalBeatVerdict::no_stuck_density` — `stuck_pairs / total_pairs` over the SAME
  chronological window, ANDed into `is_live_no_copy`. **Reusable pattern: share ONE `.windows(2)`
  walk across orthogonal metrics via a stats struct** (`stuck_run_stats` → `StuckRunStats{max_run,
  stuck_pairs, total_pairs}`) so a new detector on an existing walk costs no extra decode — apply
  this whenever a new gate term can be derived from data a prior term already walks.
  **Real-data-anchoring pattern for a new heuristic threshold: never invent from a synthetic
  fixture (the #580 regression's exact mistake) — anchor to a live number ALREADY confirmed in
  the codebase** (here, run 572001's live-measured healthy density ≈0.10%, from the SAME
  re-decode #580v2 already used) and lock the margin with an explicit order-of-magnitude test
  (`stuck_density_ceiling_sits_between_healthy_and_judder_588`). **Small-window deferral pattern
  for any density/rate metric:** a `MIN_PAIRS`-style floor (300 pairs here) makes the new term a
  no-op below the floor, so it can never false-fail a legitimately short window (mirrors
  `MIN_IDS_FOR_STEP_CALIBRATION` / `burn_present_ok`'s `< 2` guard) — apply this pattern to any
  future rate-based gate term on `imag_tick_gate.rs`.
- **#604 — CLOSED: a LOCALIZED (sub-span) judder diluted below the whole-window density ceiling,
  fixed by a 5th orthogonal SLIDING-WINDOW density term.** #588's density is a whole-RECORDING
  aggregate; a judder confined to a short sub-span of an otherwise-healthy recording got diluted
  under 1% by the surrounding clean frames and slipped through. Fix:
  `OpticalBeatVerdict::no_localized_stuck_density` — `max_local_stuck_density()` slides a FIXED
  180-pair (~3s@60fps) window over the SAME chronological sequence, judged against a separate
  LOOSER 5% local ceiling (a short window is naturally noisier than the whole recording), ANDed
  into `is_live_no_copy` alongside the whole-window term. **New reusable pattern: a sliding-window
  metric is a SEPARATE second pass, not a free extension of the shared `.windows(2)` walk** —
  `stuck_run_stats`'s single walk gives a running TOTAL (max-run, whole-window density) cheaply,
  but a bounded LOCAL maximum needs its own pass with a fixed-capacity ring buffer
  (`VecDeque<bool>` sized to the window, add-entering/drop-leaving per step) so it stays O(n) time
  / O(window) memory instead of materializing a `Vec<bool>` the size of the whole recording first.
  **Calibration without live data:** #604's own issue text explicitly forbade inventing thresholds
  from a synthetic fixture, but no live localized-judder recording existed to calibrate against —
  resolved by reusing the SAME real anchor #588 already established (572001's live-measured
  healthy density) plus an analytically-modeled judder burst (same K=3-run block construction as
  #588's `catch_up_judder_ticks`, just confined to fewer blocks so it dilutes below the
  whole-window ceiling while still reading ~25% in its own local window) — honestly documented as
  reasoned-not-measured, with a follow-up issue (#620) filed to re-ground it if real footage ever
  surfaces. The 120Hz upgrade remains the fully rigorous long-term fix for the whole class of
  run-length/density heuristics.
- **Methodology lesson (2026-07-07, both from PR #587's post-CI review round): verify a dispatched
  design-spec's formula against the ACTUAL field semantics / physical model — don't just transcribe
  it.** The #580 design comment's shorthand `present_count >= (frames_count/step) * fraction` was
  implemented LITERALLY and shipped a fail-open: `present_count` (`BurnStepContiguity::present_count`)
  is a DISTINCT-id count that is FRAME-scale (one id per captured frame, regardless of the render
  step spacing between consecutive id VALUES), so dividing the frame-scale `reference_frames` by
  `step` before comparing against a frame-scale `present_count` silently loosened a 50%-intended
  floor to 16.7% at the real rig's step 3. A correctness reviewer caught it by tracing what
  `present_count` is actually COMPUTED from (a `BTreeSet::len()` over one id per frame) rather than
  trusting the design comment's algebra. Same session, a SEPARATE tie-break direction
  (`calibrate_burn_step`) was flipped mid-development to fix a cosmetic diagnostic over-count, without
  re-deriving whether the new direction could instead MASK a real drop (it could — proven
  adversarially with a concrete counter-example). **The general rule: when a design spec states a
  formula in terms of a named field, re-derive what that field is COMPUTED FROM in the actual code
  before trusting the formula's scale/units — a terse spec comment can silently mismatch the
  implementation's real semantics, and unit fixtures built to match the (wrong) implementation will
  not catch it (exactly the failure mode that shipped the original #580 sign-flipped fixture).**

**`recording-verdict-on-imag.sh` ACTUALLY EXECUTES — it does NOT just print a plan, unlike its
strih/stream siblings.** `recording-verdict-on-strih.sh` / `-on-stream.sh` are pure PLANNERS
because ssh/scp to the Windows boxes is DENIED on this rig (the win-* MCP is the only path). imag-nb
is a plain Ubuntu box, same access class as cam1/cam2 (`targets.md`'s "Linux OBS Targets" row, SSH
`newlevel`/`newlevel`) — bash CAN ssh/scp it directly, so the on-imag helper deploys the verdict
binary (skip if already present+executable), runs `--extract-partial imag` over ssh, and scp's the
small partial (+ `#186` pixel-proof dir) back to dev1 itself, in the SAME script invocation. Don't
be misled by the on-strih/on-stream "printed plan" pattern when writing a similar helper for a
Linux/ssh-reachable box — check the access class first.

**Gotcha — a NEW call site inside the `VERDICT_ON_STREAM=1` per-box branch runs under `set -e`
(re-enabled at the top of that branch).** A bare (unguarded) command there — like the on-imag
extract call — `set -e`-ABORTS THE WHOLE SCRIPT on any failure (imag unreachable, a stale/missing
deployed binary, a transient ssh hiccup), including the strih/stream plan printout the operator
still needs below it. Caught in self-review on #462 (commit message: "[8/8c] imag extract must not
set -e-abort the per-box plan"). Fix pattern (matches the `#178` StopRecord-region discipline
elsewhere in this same script): `cmd && echo ok || echo "WARNING: ..." >&2` — the compound `&&`/`||`
list is exempt from `set -e`, so a failure degrades gracefully (the optional artifact — here
`$IMAG_PARTIAL` — simply stays absent, and a downstream `if [ -f "$IMAG_PARTIAL" ]` guard omits it
from the merge). Apply this to ANY new fallible call added inside that branch, not just imag's.

**imag's camera mapping is a CLEAN 1:1, unlike strih's drift-corrected `#399` mapping.** strih's
program input showing cam1 is `NDI cam5` (a historical drift-correction — see `#399` in this file's
sibling sections / `.claude/skills/genlock`). imag's Phase-1 provisioning (`setup-imag.sh`, #458)
pins `NDI CAM1`..`NDI CAM6` → `CAMx (usb)` 1:1 fresh, so imag's input showing cam1 (the SOURCE
camera that films cam2's monitor) is simply `NDI CAM1` / scene `Cam 1`
(`IMAG_PROG_SOURCE`/`IMAG_PROG_SCENE` in both `recording-e2e.sh` and `rig-mode.sh`). Don't assume
the two boxes' camera→input naming matches — they're maintained independently.

**`rig-mode.sh` extends `obs_burn_targets()`/`toggle_burn` to imag for free** — the array's own
`#252` design comment explicitly anticipated "a third box", so TEST mode turning imag's 911003 burn
ON and EVENT mode turning it OFF needed no new toggle logic, only a third `printf` line. TEST mode
additionally calls a NEW `set_imag_test_program()` (reuses `obs_phase2.py switch` — the same
lightweight `SetCurrentProgramScene` + non-black self-check the all-cambox sweep uses) to route
imag's PROGRAM onto the cam1 scene; EVENT mode does NOT scene-switch imag (mirrors strih/stream —
rig-mode never scene-switches those either).

**imag's 911003 digital burn is FREE-RUNNING at exactly 2x the recorded rate — gate it with
`imag_tick_gate::burn_step_contiguity`, never strict 1:1 (#480).** Confirmed on a live 300s rig
recording: only EVEN burn ids were present (~50%), a clean deterministic alternation, not
scattered loss. Root cause: imag's OBS runs Studio Mode ON, and the Studio-Mode "Program" monitor
widget re-renders the active scene as a SEPARATE display draw, independent of the main output
render that reaches the recording — so the DistroAV burn filter's `frame_id` counter
(`vendor/distroav/src/ndi-burn-filter.cpp`, bumped every `video_render` call) advances TWICE per
recorded output frame. This is DIFFERENT from strih's own free-running burn (#360, an IRREGULAR
render-tick step gap-ignored entirely) — imag's step is a clean, reproducible 2, so
`node_verdict_for_imag` (`src/bin/recording-verdict.rs`) models it with the decimation-aware
excess-gap check (`camera_box::imag_tick_gate::burn_step_contiguity`, `IMAG_BURN_RENDER_STEP = 2`)
instead: a forward gap of exactly 2 is expected, a LARGER gap still charges the excess as a real
drop. The optical cam2 tick stays the unchanged hard 1:1 proof. This is a Rust-only fix — the
vendored `distroav.so` filter itself was NOT touched (would need a rebuild + fresh
`genlock_build_sha_imag` pin + live re-verification). If a future imag OBS change ever turns
Studio Mode off, or the vendored filter starts gating the counter to the program pass only, this
step model needs re-deriving from a fresh live recording, not assumed.

---

## "Is gate X CI-automatic or rig-manual?" — read `docs/strict-gate-coverage.md` first (EPIC #406)

Before answering any question about whether a strict gate (render budget, colour, frozen-camera,
delivery contiguity, restart-survival, phase-sync, …) runs automatically vs needs a manual rig
E2E dispatch, read `docs/strict-gate-coverage.md` — it is a verified-against-a-real-CI-log table of
every gate kernel, its unit tests, its CI job, and its rig wiring. Re-deriving this from scratch
duplicates a full audit that has already been done. The short version: every kernel's DECISION
LOGIC is Tier-0 unit-tested and runs on every push (Tier A) — but `full-path-e2e.yml` (the real
full-flow rig gate that exercises the ACTUAL system, not just the decision logic) is
`workflow_dispatch`-only, so a regression in the system itself (not the gate logic) is only caught
by a manual dispatch. That remaining automation gap is infra/operator-scheduling work, not a code
fix — see the doc for why.

## GOTCHA — ad-hoc `record --action start` outside the harness MUST carry a bounded stop (2× live incident, 2026-07-10)

Both overnight "mystery" recordings that RIG_BUSY-deadlocked the #647 CI gate were OUR OWN: OBS
logs pinned each `Recording Start` to a WebSocket client from dev1 (10.77.9.165) — once during a
worker's live [0/8]-gate verification window (03:07), once at another worker's session start
(04:40). Nobody at the rig touches recording (owner's standing statement). `recording-e2e.sh`'s
cleanup trap now stops ONLY recordings the harness itself started (#649 flags) — which means an
AD-HOC `obs_phase2.py record --action start` (outside the harness) is covered by NOTHING. Rule:
never fire an ad-hoc StartRecord without an explicit bounded stop in the SAME session (foreground
sleep → StopRecord, or an armed re-check), and always StopRecord before ending a session that
started one. A leftover recording blocks every subsequent CI gate run as RIG_BUSY until someone
manually diagnoses it (rig-busy-gate.sh now prints per-box timecode + stray-vs-broadcast hints,
#649 item 3).
