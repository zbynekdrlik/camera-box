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

**#728 GOTCHA (2026-07-12): cam1's grabber card is no longer a ShadowCast.** The user
physically swapped it with cam5's card — cam1 now has an **Elgato 4K S** on `/dev/video1`
(cam5 now has the ShadowCast 2, on `/dev/video0`). The description above is historically
accurate for when this proof was run (2026-06-17); read "cam1 ShadowCast" in older sections
of this skill as "whatever grabber card cam1 had AT THE TIME" — check the LIVE fleet table
in `targets.md` for the current assignment before assuming a specific model/device node.
`grabber_model_for_hostname`'s static table is likewise OPERATIONAL/historical only —
`capture_rate_health::resolve_grabber_model` resolves the actual RUNTIME card at boot.

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
the cam1 camera's MANUAL settings. The harness CANNOT read or set them: camera-box reads whatever
grabber card is CURRENTLY on cam1 (`/dev/video0` when it was the ShadowCast card; `/dev/video1`
since the 2026-07-12 #728 swap to an Elgato 4K S — check `targets.md`'s live fleet table, never
assume the device path), which does NOT expose the BMPCC's shutter/focus/exposure regardless of
which grabber model is plugged in. `recording-e2e.sh` PRINTS this checklist at startup; satisfy it
BEFORE every proof run:

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
    `classify_patch` (chromatic: Grayscale if chroma<`GRAYSCALE_CHROMA_MIN` / HueShift if hue
    err>30° / else Pass — neutral: NeutralTint if chroma>`NEUTRAL_CHROMA_MAX` / else Pass) +
    `summarize_node_colour` (strict-majority vote over sampled frames). A burn-covered patch is
    `Unsamplable` → SKIPPED, not charged (a real colour defect is global → still fails on the
    visible patches); fail-closed only when NOTHING is checkable. **Read the two constants'
    values from the CODE, never hardcode them here** — both have been recalibrated more than once
    against real rig data (`NEUTRAL_CHROMA_MAX` 48→72 on the 2026-06-30 recording;
    `GRAYSCALE_CHROMA_MIN` 40→24 on the #740 splitter/disk-incident recalibration, below) and will
    likely move again if the rig's physical setup changes.
  - **#364 calibration — gate on HUE + CHROMA, NOT brightness.** The level/sRGB-distance check was
    REMOVED: the rig's optical capture is ~7× DIM by physics (60 Hz monitor + 1/1000 s shutter samples
    ~1 ms mid-redraw), so a distance/level check only false-fails a correct camera. Dropping it is NOT
    weakening — every real fault (grayscale collapse, dead channel, hue shift, WB cast) still fails on
    hue/chroma. Locked by `real_rig_dim_capture_passes_while_grayscale_and_dead_channel_fail`
    (2026-06-30 fixture) and `splitter_and_disk_incident_dim_capture_passes_post_recalibration_740`
    (2026-07-14 fixture, current rig baseline) with genuine sampled values.
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

**#740 (RESOLVED 2026-07-14) — the RED and BLUE reference patches failed identically on EVERY node
(cam1-6, strih, stream) after a NAMED physical incident, not a code regression.** Two overlapping
rig incidents (#737 — cam2's dying disk corrupting its own HDMI display output that drives the
colour scale, fixed 2026-07-13 13:41; #729 — the downstream HDMI splitter's 4/6 dead ports,
re-cabled ~14:11 same day) settled the rig's achievable red/blue chroma onto a new, genuinely
dimmer, but STABLE baseline (red 33-36 / blue 37-38, vs the pre-incident 117-128). Every other
chromatic patch (green/cyan/magenta/yellow) stayed comfortably >90 throughout — red/blue were
always the rig's two weakest chromatic patches even pre-incident, just with more headroom over the
OLD 40 floor. `imag` (a physically separate apparatus) never faltered, confirming it wasn't a code
bug. Fixed by recalibrating `GRAYSCALE_CHROMA_MIN` 40→24 (same ~26% headroom ratio the old
threshold held over ITS baseline), with the full dated evidence trail in the constant's own doc
comment (`src/colour_verify.rs`). **Never loosen this threshold without equivalent dated,
named-mechanism evidence** — see the #740 GitHub thread + the doc comment for the full playbook.

**Diagnostic technique — building a chroma timeseries from historical local runs (reusable for
any "is this a physical rig change or a code regression" question).** Every `--colour-gate` run's
per-patch means are logged (ANSI-coloured) as `colour-dump (localized per-patch mean) patch=N
expected=... mean=... chroma=... hue=... frames=12` lines inside
`/tmp/recording-e2e-<RUN_ID>/{strih,stream}-extract-<RUN_ID>.log` — one line per patch (0-12, order
white/black/R/G/B/C/M/Y/g0/g64/g128/g192/g255, `PATCH_COLOURS`). To extract a timeseries across
many past runs (strip the ANSI codes FIRST or `grep "patch=N "` silently matches nothing):
```bash
for f in $(grep -rl "colour-dump" /tmp/recording-e2e-*/strih-extract-*.log 2>/dev/null); do
  mtime=$(stat -c '%y' "$f" | cut -d. -f1)
  clean=$(sed -r 's/\x1b\[[0-9;]*m//g' "$f")
  red=$(echo "$clean" | grep "patch=2 " | grep -oE "chroma=[0-9.]+" | head -1)   # patch=4 for blue
  echo "$mtime  RED:$red  $(basename "$f")"
done | sort
```
This is how #740's before/mid/post-incident phases (117-128 → 87-89 → 26-38, settling stable) were
proven from local `/tmp` history alone, with zero rig access needed — the `verdict-<RUN_ID>.json`
itself only carries the pass/fail COUNT (`colour_fail`), never the raw per-patch chroma, so the
`*-extract-*.log` files are the only source for the actual numbers.

## Rig TEST / EVENT Mode Switch (#247) — `scripts/rig-mode.sh`

THE deterministic, single-source-of-truth switch between TEST mode (QR/E2E measurement) and EVENT
mode (clean prod broadcast). Replaces the ad-hoc, context-dependent switching that caused #246 — a
burn left ON in the prod **Machine** env painted QR on the LIVE broadcast, and genlock left in a test
state. The settings below are PINNED in the script; do NOT improvise them.

Run from dev1 (ssh to the cam boxes is ALLOWED; ssh to strih/stream also WORKS as of #701, but
driving/verifying a live GUI OBS action is exactly what the win-* MCP is for, so the OBS side
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

**GOTCHA — `gh run cancel` on an ALL_CAMBOX run can cut the cleanup trap off MID-LOOP, before it
reaches every cambox (2026-07-12, #709 dispatch).** The bounded-per-step timeout guards above
protect against a HUNG step, but a GitHub Actions **cancellation** kills the whole runner PROCESS
directly — it does not wait for a bash trap's SEQUENTIAL per-cambox loop (`pkill -9 -f
'camera-box-burn-'` → restart, once per active box in `CAMBOX_SWEEP`) to finish all six iterations.
Live incident: cancelling a redundant (docs-only, no new verification value) ALL_CAMBOX run mid-sweep
left `camera-box-burn-cam3/4/5/6` still holding their video devices and those four boxes'
`camera-box.service` crash-looping ("Device or resource busy") — cam1/cam2 were ALREADY restored
(cleanup had reached them before the cancel landed), the burns were left ON on every strih NDI input
(visible-QR risk on a live broadcast), and cam2's own `camera-box.service` was ALSO crash-looping on
`/dev/video1` from a stray `camera-box-burn-cam2-*`. **Whenever you cancel an ALL_CAMBOX (or any
multi-cambox) run, do NOT trust the cancellation to have finished cleanup — explicitly re-verify
EVERY cambox** (`ps aux | grep camera-box-burn` + `systemctl is-active camera-box` on all 6 boxes),
EVERY strih NDI input's burn state (`obs_burn_filter.py check` on `NDI cam1`..`NDI cam6`, not just
the default program source), and the rig-busy-check, before considering the rig clean. Fix pattern:
`pkill -9 -f 'camera-box-burn-'` + `rm -f /tmp/camera-box-burn-*` + `systemctl restart camera-box`
per stray box, `obs_burn_filter.py remove` per stray-burn input.

**#712 (2026-07-12) — the cam3/4/5/6 loop is now PARALLEL, shrinking (not eliminating) this
window.** `cleanup()`'s cam3/4/5/6 restore now backgrounds all 4 boxes' ssh restores at once and
waits for them via `scripts/lib/cambox-parallel-restore.sh`'s `cambox_parallel_wait_and_report`, so
the loop's wall-clock is bounded by the SLOWEST single box instead of the SUM of 4 (a real
functional timing proof lives in `tests/harness_cambox_parallel_restore_712.rs`). This makes a
cancellation landing mid-loop far less likely, but a SIGKILL can still land inside the shorter
window — the manual re-verify steps above (`ps aux`/`systemctl is-active`/`obs_burn_filter.py
check`/`rig-busy-check` on EVERY box) are still the correct backstop after any cancelled
ALL_CAMBOX run, never assume the parallelization alone makes cleanup atomic.

**#713 (2026-07-12) — extended to ALL 6 boxes (cam1 + the cam3/4/5/6 loop + cam2/painter, one
shared `CAMBOX_PARALLEL_PIDS`/`LABELS` group, ONE `cambox_parallel_wait_and_report` call at the
end) — but this ALSO concentrates more simultaneous outbound ssh sessions from dev1, and tripped
the pre-existing `#675` SSH-connection-contention condition on the very first live run after it
shipped (all 6 boxes hit "restore failed/timed out" within ~2s of the group launching — too fast
for a genuine 30s timeout, consistent with an immediate connection-level rejection, not 6
independent hangs).** The rig ended up fully healthy anyway — the SEPARATE, unaffected `#684`
FINAL-verify pass (sequential, still runs after the parallel group) caught every box not-yet-
active and its own `#675` one-retry mechanism recovered all 6. **Lesson for the next widening of
this parallel group (or any similar N-way ssh fan-out from dev1 to the cam fleet): concentrating
MORE simultaneous outbound connections trades a smaller cancellation-stranding window for a HIGHER
chance of tripping `#675`'s connection contention on a normal (non-cancelled) exit — both are real
trade-offs, not free.** Filed as `#715` (root cause + a stagger/sshd-limit fix are NOT yet
investigated — a fresh gate-run's cleanup log is the way to check: grep for `WARNING #712 ...
failed/timed out` immediately after `#328 FREE cam1/cam2 capture devices FIRST`; its absence, or a
much smaller fraction of boxes hitting it, is the signal a fix landed).

## GPU/encode-contention correlation technique (#674, 2026-07-12) — epoch-join a nvidia-smi sampler against the harness's OWN real window boundaries

To test whether imag-nb's GPU/NVENC state correlates with a per-window judder-density metric
(`all_cambox_continuity.imag.segments[].optical_stuck_density`) WITHOUT touching any rig config:

1. **Sampler**: `scripts/imag-gpu-contention-sampler.sh` (committed) — a bounded-duration loop of
   `nvidia-smi --query-gpu=utilization.gpu,memory.used,encoder.stats.sessionCount
   --format=csv,noheader,nounits`, one CSV row per sample with a `date +%s.%3N` epoch timestamp.
   Arm it (`nohup ... &`, `IMAG_GPU_SAMPLE_DURATION_SECS=900` is a safe default) BEFORE triggering
   the gate run — you don't know exactly how long preflight/setup will take before `StartRecord`
   actually fires, so budget generously; a duration too short truncates the TAIL of the recording
   (confirmed live: a 900s window armed ~620s before `StartRecord` actually fired left the LAST
   ~29s of a ~306s recording uncovered — always check via the exact math below and disclose any
   gap honestly rather than silently extrapolating).
2. **Real epoch window boundaries — read `switch-schedule.json`, not the printed log lines.**
   `/tmp/recording-e2e-<RUN_ID>/switch-schedule.json` (still on dev1 after a self-hosted-runner
   gate run — check there BEFORE spending rig time on a fresh repro, mirrors the `#708` lesson)
   has each window's REAL `start_ns`/`end_ns` on dev1's own epoch clock (`n / 1e9` for seconds).
   Since dev1 and imag-nb are both DanteSync-synced sub-ms, a straight epoch-second join between
   the GPU CSV and these window bounds is valid at any sampling cadence coarser than ~10ms — no
   manual clock-offset correction needed, unlike the painter-vs-dev1 `#326` gate's own correction
   (that gate compares two DIFFERENT clocks' RAW offsets; this join compares two ALREADY-
   DanteSync-disciplined epoch clocks directly).
3. **Join**: for each window, filter GPU-CSV rows to `[start_ns/1e9, end_ns/1e9]`, average. Print
   samples-covered vs window-duration per row so a gap (see point 1) is visible in the output, not
   silently averaged over a partial window.
4. Result this run (2026-07-12): GPU util/VRAM/encoder-sessions were COMPLETELY FLAT throughout —
   REJECTS "GPU/encode contention builds up during the recording" as `#674`'s judder mechanism (no
   growth to correlate against a judder density that was ALSO flat, just already-elevated). Full
   writeup on `#674`'s own thread.

## Emit-rate-deficit / freeze-jump discriminator (#707 B1, 2026-07-14) — 3-layer read: LINK vs BOX-EMIT vs SDK

When `all_cambox_continuity` shows freeze+jump on a cambox (big deltas 7-12 in the window's
`presentation_cadence.delta_histogram`, i.e. frozen frames then a jump), locate the loss LAYER with
three signals instead of guessing:

1. **LINK** — the box→strih TCP transport sampler (`scripts/lib/transport-sampler.sh`, armed `[5b/8]`,
   harvested `[7c/8]` into the run dir as `transport-sampler-<cam>-<RUN_ID>.csv`). Columns:
   `epoch_ms,iface,send_q_bytes,retrans_total,rx_errors,rx_dropped,tx_errors,tx_dropped`. Over the
   window: `send_q_bytes max` (0 = TCP never backed up), `retrans_total` DELTA (0 = no wire loss),
   `tx_err/tx_drop` DELTA. All flat → LINK is CLEAN, loss is upstream of the socket. (`rx_dropped`
   is INBOUND, not the emit path — benign background multicast; ignore it.)
2. **BOX EMIT PATH** — the box's own `camera-box` 5s `Streaming:` journal report:
   `N fps emitted / M fps captured (S sent, C captured, D capture-dropped)`. **`captured`≈60 + `D`=0
   but `sent` << `captured` = the emit path fell behind capture** (frames captured, never emitted).
   That is the loss layer. The box's OWN `#707 genlock emit-gate SKIPPED …` diagnostic names the exact
   mechanism (the genlock emit gate leaps past boundary intervals on a clock discontinuity / stalled
   poll). `src/emit_rate_ring.rs` (prong 1) pins the exact 1s bucket — but see the deploy gotcha below.
3. **NDI SDK internal** — only if LINK clean AND emit≈capture (no box deficit) yet strih still froze.

**Confounds that fooled the first read (be honest, don't overstate):**
- The E2E **burn binary** (`camera-box-burn-<RUN_ID>`) runs alongside `camera-box` and oversubscribes
  the **3-core** cam boxes (load ~5.9) → ALL boxes drop to ~46fps emit DURING the test. That is a
  TEST-MEASUREMENT artifact, NOT the production mechanism. Re-check emit rate in PRODUCTION (no burn,
  post-cleanup): the genuine defect is the box that STAYS degraded (live #707: only CAM1 = 55.8fps +
  ~64 emit-gate skips/30s; cam3/4/5/2 = 60fps/0 skips, cam6 minor). Chronic if it survives a
  `systemctl restart camera-box`.
- The `#707 emit-gate SKIPPED` diagnostic is unthrottled (~10/s → rsyslogd ~37% + journald ~15% CPU on
  3 cores) — a logging-CPU feedback loop that WORSENS the emit stall (filed #752). Suspect it whenever
  a box shows rsyslogd/journald high in `ps` during an emit deficit.

## GOTCHA — the E2E harness does NOT deploy the main `camera-box` binary; new `src/main.rs` instrumentation won't be on the boxes after an E2E run

`recording-e2e.sh` `[2/8]`/`[2b/8]` deploys + runs the **burn binary** (`camera-box-burn-<RUN_ID>.service`
from `USE_PREBUILT_PROBE_DIR`) and `systemctl restart camera-box` — but the restarted service is the
**INSTALLED `/usr/local/bin/camera-box`** (the production build), NOT a HEAD build. So a change to
`src/main.rs` / any lib the main binary uses (e.g. the #707 `src/emit_rate_ring.rs` emit-1s report) is
CI-green but **NOT present on the cam boxes after an E2E gate run** — verify with
`strings /usr/local/bin/camera-box | grep <your-new-log-string>` (0 = not deployed). To exercise
main-binary instrumentation on the rig you must fleet-deploy camera-box (`scripts/deploy-fleet.sh` /
upgrade-fleet), separately from the E2E run. The strih/stream `obs.dll` (genlock) IS what a genlock
deploy updates; the cam-box main binary is a DIFFERENT artifact on a DIFFERENT deploy path.

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

**RESOLVED (#709, 2026-07-12) — imag-nb's OBS can silently never actually start a recording:
root cause was a GPU VRAM leak exhausting NVENC's encoder-init headroom, NOT a WebSocket/RPC
bug.** Live incident: `[5/8]` logged `10.77.9.182: recording STARTED` (obs-websocket's
`StartRecord` only calls the frontend API and never verifies the encoder actually initialized —
so a genuinely-failed start still returns `result:true`), then the SEPARATE #627 post-start
liveness poll caught it 4s later — `outputActive=[False, False]`, `outputBytes` STATIC.

**Diagnostic gotcha — imag-nb's OBS log uses LOCAL time (Europe/Prague, CEST, UTC+2), not UTC.**
The original write-up ("imag's OBS log showed ZERO lines of any kind in the window") was a
TIMEZONE MISREAD: grepping the log for the UTC failure timestamp (`04:21:33`) found nothing
because the log's own timestamps are LOCAL (`06:21:33`). Always confirm the box's actual TZ
(`timedatectl` / compare `date` vs `date -u`) and convert the CI run's UTC timestamps before
concluding a log is silent.

**Real root cause, found once the log was read at the correct (local) time:**
```
[obs-nvenc] init_encoder_h264: nv.nvEncInitializeEncoder(enc->session, &enc->params) failed: 10
    (NV_ENC_ERR_OUT_OF_MEMORY)
[obs-nvenc] init_encoder_h264: ... failed: 10 (NV_ENC_ERR_OUT_OF_MEMORY)   (2nd fallback attempt)
Already in non_texture encoder, can't fall back further!
```
imag's OBS had run 5 days without a restart; its render pipeline (6 continuous NDI genlock feeds +
the built-in Multiview) had leaked GPU VRAM up to 6872MiB of 8151MiB total (`nvidia-smi`), leaving
only ~1058MiB free — not enough for NVENC's recording-encoder session to initialize. Fix: restart
OBS on imag-nb (`pkill -9 -x obs`, matching `setup-imag.sh`'s own hot-swap relaunch pattern) —
confirmed live: VRAM dropped to ~302MiB used, and StartRecord then wrote real, growing bytes.
Prevention shipped: `scripts/lib/imag-gpu-guard.sh` + a new `[4e/8]` `recording-e2e.sh` preflight
(before `[5/8] StartRecord`) that reads free VRAM via nvidia-smi and fails fast with an actionable
message when it drops below `IMAG_GPU_MIN_FREE_MIB` (default 1500MiB) — see
`tests/harness_imag_gpu_guard.rs`.

**GOTCHA hit fixing #709 — a manual `pkill -9 -x obs` + relaunch on imag-nb that skips the
`saved_projectors`-strip step opens DUPLICATE projectors and regresses the render-budget gate.**
`setup-imag.sh`'s openbox autostart script strips `saved_projectors` from the scene-collection
JSON BEFORE every launch specifically so a restart never restores a stale saved projector pair
(see the `#522` note earlier in this file) — but that step lives ONLY in the autostart script, not
in a bare manual `pkill+relaunch` driven by hand over ssh. Skipping it let OBS restore ONE stale
Program+Multiview projector pair on launch, and then re-running `imag_scenes.py --projector`
(to reseed scenes) opened a SECOND pair — `wmctrl -l` showed FOUR projector windows (2×Program +
2×Multiview) instead of the intended two. Consequence: the compositor draws everything TWICE per
frame, which silently regressed imag's OWN `[4d/8]` render-budget gate (60.00fps/0% skip →
~57fps/2.5-10% skip, `#405/#406` gate) — a SEPARATE, self-inflicted failure mode from #709 itself,
that showed up as a NEW red gate on the very next CI rerun. Fix: close the duplicate pair
(`wmctrl -i -c <id>`, Linux-side — there is no WS request to close a projector, same limitation as
the Windows boxes) — or better, ALWAYS strip `saved_projectors` BEFORE any manual imag-nb OBS
relaunch, exactly mirroring the autostart script's own sequence:
```bash
pkill -9 -x obs; # wait for death
for f in ~/.config/obs-studio/basic/scenes/*.json; do
  python3 -c "import json,sys; p=sys.argv[1]; d=json.load(open(p)); d['saved_projectors']=[]; json.dump(d,open(p,'w'))" "$f"
done
DISPLAY=:0 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus XAUTHORITY=~/.Xauthority \
  taskset -c 2-11 obs &   # then re-seed via imag_scenes.py --host 127.0.0.1 [--projector]
```
Verified live: after the properly-sequenced restart, imag held 60.00fps / ~5ms render / 0% skip —
even better than the pre-incident baseline. This is the imag-nb-specific manifestation of the
obs-ops skill's existing "a force-kill relaunch restores a STALE saved config" gotcha — same class
of bug, worth checking `wmctrl -l` (window COUNT, not just presence) after ANY manual imag-nb OBS
restart, not just after a genlock hot-swap.

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

## A DIFFERENT per-node metric (`full_chain.loss`, the #186 headline) was NOT schedule-scoped until #706 — the bug class to watch for

The `all_cambox_continuity` gate above (#312) has ALWAYS been correctly schedule-windowed — but a
SEPARATE, older per-node metric — `full_chain.loss.<cam>` (`expected_count`/`present_count`/
`burn_unreadable`, the #186 headline verdict `node_verdict_with_optical` builds via
`in_window_burn_frames`) — was NOT: its window was the WHOLE-RECORDING cam2 optical span, a
leftover from before the ALL-CAMBOX sweep existed (when a single camera was continuously on
program for the whole test). In the fused ALL-CAMBOX sweep each `CAMERA_UNDER_TEST_NODES` entry is
on program for only its OWN ~1/6-to-2/6 share of the recording — so #186's un-scoped window folded
every OTHER camera's program time into "this node's window" and misclassified it BURN-UNREADABLE:
~47000 phantom ids fleet-wide (#706), ~7250-8530 PER camera. **The tell**, if you ever see this
recur on a NEW per-node metric someone adds to the ALL-CAMBOX sweep: `burn_unreadable[cam] ≈
expected_count[cam] × (fraction of the recording OTHER camboxes were on program)`, i.e.
`expected_count[cam]` sits near the WHOLE recording's frame count instead of near
`present_count[cam]`. The fix pattern (`scope_camera_window_to_own_schedule`,
`src/bin/recording-verdict.rs`): reuse the SAME `frame_gen_ts_anchor` + `place_frame_in_window`
the #312 sweep already uses to restrict the new metric's window to that camera's own schedule
window(s) — never invent a second attribution scheme, or the two gates can disagree. Duration-floor
/ optical-undecodable-rate style metrics (`optical_span_facts`) should usually stay
WHOLE-recording — those measure "did the session run long enough" / "did the shared optical read
stay clean", legitimately session-wide properties, not per-camera-program-time ones.

## #681 — a per-adjacent-pair delta walk is a BIASED estimator; use whole-window NET span instead

`window_segment`'s `gaps` count (via `painted_tick_gaps`, #625) went through TWO bugs on the SAME
data before it was trustworthy — know the difference so a THIRD variant never recurs.

- **#625 (fixed):** walking the RECORDED order let a benign delivery reorder (a one-frame-late
  60→30 straddle, `#133`/`#196`/`#216`) manufacture phantom gaps. Fix: sort the DISTINCT present
  values first (a monotone-at-the-source counter's sorted order recovers true delivery-order-
  independence).
- **#681 (fixed, live evidence RUN_ID 1783727115) — sorting was NOT enough.** Even sorted, summing
  each ADJACENT pair's shortfall independently (`delta/step - 1`, floored at 0 PER PAIR) is a
  BIASED estimator: it never credits a local delta SMALLER than `expected_step` (which the
  dual-QR Vernier's normal async even/odd sampling beat produces routinely — real numbers: 659
  pairs at delta=1 against 160 at delta=6, for expected_step=2) against a LATER catch-up jump. The
  per-pair floor silently drops the "ahead of schedule" credit every single time, so it compounds:
  a genuinely ~1% net-loss window reported **33-39% gaps** — a camera independently proven clean
  in a DEDICATED single-camera measurement minutes earlier still failed every window of the sweep.
  **Fix:** replace the per-pair sum with ONE whole-window net-span calculation:
  `expected_count = (last-first)/step + 1` minus the actual distinct `present_count`. This is a
  strict generalization (identical result whenever every delta happens to be `>= step`; only
  diverges, correctly, when some are smaller) and mirrors the SAME "avg tick step ≈ expected, zero
  NET loss" acceptance methodology the Dual-QR Vernier proof (top of this file) already uses.

**The generalizable lesson — for ANY future "count genuinely-missing slots from a sampled counter"
gate in this repo:** never sum a per-adjacent-pair shortfall with per-pair flooring; compute the
whole-window/whole-run NET (expected total minus actual distinct count). A per-pair floor is
asymmetric — it can only ever OVER-count, never under, because a locally-fast run's "credit" has
nowhere to go once its own pair is judged in isolation. This is the SAME class of bug the #588/#604
imag density terms and the #580v2 `is_live_no_copy` run-length gate were built to avoid on the
OTHER measurement path (imag's own optical beat) — #681 is the swept-cambox sibling of that same
lesson, discovered independently on a different code path.

**Diagnostic technique that found it:** don't trust the aggregate number alone — pull the RAW
distinct-value delta HISTOGRAM for the failing window (`sorted(set(ticks))`, then
`Counter(b-a for a,b in zip(seq,seq[1:]))`) and compare it against the SPAN-based net
(`(last-first)//step + 1 - len(set(ticks))`). A wide-but-small-magnitude histogram (many small
deltas) with a near-zero net is the signature of this bias; a genuinely large net loss shows up as
a real span-based deficit regardless of the local delta pattern.

**Phase-2 harness (`recording-e2e.sh ALL_CAMBOX=1`) — RUNS ON THE DEFAULT `VERDICT_ON_STREAM=1`
(#332).** The sweep (`switch_schedule.py` + `obs_phase2.py switch`) writes `switch-schedule.json`
and appends `--switch-schedule` to BOTH verdict paths: the legacy decode-on-dev1 `VERDICT_ARGS`
AND the DEFAULT per-box `MERGE_ARGS` (the `--merge-partials` step). The per-cambox
`all_cambox_continuity` is computed in the SHARED `build_and_print_verdict` (which `run_merge`
calls), so the merge path produces it IDENTICALLY to the fused/legacy path — the all-cambox verdict
now runs ON the stream box (#193, decode where the video lives), NOT forced onto dev1. The old guard
that forced `VERDICT_ON_STREAM=0` is GONE (#332). Just run `ALL_CAMBOX=1 bash scripts/recording-e2e.sh`
(default `VERDICT_ON_STREAM=1`). Sweep config: `CAMBOX_SWEEP` (default, since #312 items 1+3, #753
fleet growth 6→7 + the 1:1 mapping pivot 2026-07-14:
**`Cam 1:CAM1 Cam 2:CAM2 Cam 3:CAM3 Cam 4:CAM4 Cam 5:CAM5 Cam 6:CAM6 Cam 7:CAM7`** — ALL SEVEN
cameras, incl. cam2), `SEGMENT_SECS` (default 30).

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

**Reverse direction — pulling the small partial JSON + #186 pixel-proof dir BACK box→dev1, manually
driving a full `ZERO_LOSS_RESTART_GATE`-style before/restart/after protocol (#466, 2026-07-10).**
`win-strih`/`win-stream-snv` `FileDownload` is also base64-inline and hits the SAME size limit in
this direction, and a pixel-proof dir is a whole DIRECTORY, not one file. The reusable recipe:
1. `Start-Process` the `recording-verdict.exe --extract-partial ...` DETACHED (per the inline-dies
   -at-MCP-idle-timeout gotcha above), poll to completion.
2. `Compress-Archive -Path <box>:\camera-box\verdict-out\<node>-partial-<run>-pixels -DestinationPath
   ...-pixels.zip -Force` on the box (bundles the whole pixel-proof dir into one file).
3. Serve BOTH the partial JSON and the pixels zip from the box's OWN `verdict-out` dir via
   `Start-Process python -m http.server <port> --bind <box-ip> -WorkingDirectory <verdict-out>`
   (same HTTP-serve pattern as the forward direction, just run FROM the box instead of dev1).
4. `curl -fsS -o <dest> http://<box-ip>:<port>/<file>` from dev1 for each of the 4 files (2 boxes ×
   {partial JSON, pixels zip}).
5. **`unzip` on Linux warns "appears to use backslashes as path separators" for a Windows-built zip
   and returns exit 1 (a warning-level failure) even on a fully successful extract.** Never chain
   `unzip -q -o a.zip -d . && unzip -q -o b.zip -d .` — the FIRST unzip's exit-1 warning
   short-circuits the `&&` and the SECOND zip never runs, silently leaving that box's pixel-proof
   dir missing. Run each `unzip` as its OWN standalone command (or with `; true` after it), and
   verify the resulting directory populated afterward rather than trusting the exit code.
6. Feed both nodes' partials (+ imag's own partial, which extracts directly via plain ssh, no MCP
   needed) into one `recording-verdict --merge-partials ...` call on dev1 exactly as
   `recording-e2e.sh`'s own printed `[8/8d]` plan describes.

This full manual loop (steps 1-6, run TWICE bracketing a real `launch-obs-genlock.sh --force`
restart on both boxes) is how `ZERO_LOSS_RESTART_GATE`'s protocol was completed WITHOUT using the
harness's own built-in restart-confirmation pause (which needs a mid-script MCP action the harness
cannot synchronize with a single foreground invocation) — two independent full `recording-e2e.sh`
passes + a manual restart in between + the gate binary on the two resulting verdict JSONs
reproduces the exact same proof the built-in mode's internal helper would produce.

**More manual-dispatch gotchas found re-running this protocol (2026-07-10, #660/#466):**

- **NEVER set `PROBE_BIN_DIR` to a pre-downloaded CI artifact dir for a manual `recording-e2e.sh`
  dispatch.** `[1/8]` always builds fresh into `target/release` regardless of `PROBE_BIN_DIR`
  (there is no "skip the build" mode), and `[2/8]`'s cam1 upload then looks for a binary literally
  named `camera-box` in `$PROBE_BIN_DIR` — the CI `probe-tools-linux-amd64` artifact ships that
  same binary as `camera-box-probe` instead, so overriding the var produces
  `scp: stat local ".../camera-box": No such file or directory` and an early, silent-looking abort
  (cleanup trap fires, log ends after ~2 minutes with no obvious error unless you scroll up). Just
  leave `PROBE_BIN_DIR` at its default — the local probe-featured build here is the SANCTIONED
  exception to the Tier-0 no-local-`--features probe` policy (this script IS the E2E gate; CI runs
  the identical build step on the same self-hosted box).
- **Set `COLOUR_GATE=0` for any manual painter launch that doesn't pass `--colour-scale`.** A
  plain `frame-probe --paint-only --dual-qr` (no colour band painted) makes `--colour-gate`
  abort BOTH `--extract-partial` (`could not localize the dual-QR colour scale in ANY of N sampled
  frames`, no partial JSON written at all) and the final merge (`no carried colour summary`) — this
  is documented behavior (`recording-e2e.sh`'s own `COLOUR_GATE=0` comment), not a bug. Drop
  `--colour-gate` from every extract-partial AND the merge command consistently, or the merge will
  error before printing any verdict.
- **A `win-strih`/`win-stream-snv` Shell call running `recording-verdict.exe --extract-partial`
  inline routinely exceeds the MCP tool's own idle-timeout (~300s) for a strih/stream recording at
  30fps over ~300-330s** (the decode genuinely takes 4-6 minutes). Two outcomes are possible on
  timeout and you cannot tell which happened without checking: (a) the process keeps running
  server-side and finishes fine — `Test-Path` the expected output JSON a bit later; (b) the MCP
  timeout kills the child too — nothing gets written, silently. If a plain re-run also stalls,
  launch it DETACHED so it survives the tool call regardless: `Start-Process -FilePath
  recording-verdict.exe -ArgumentList '...' -RedirectStandardOutput out.log -RedirectStandardError
  err.log -PassThru` (capture `$proc.Id` to a file, since each Shell call is a FRESH PowerShell
  session — nothing in-memory survives between calls), then poll `Get-Process -Id <id>` /
  `Test-Path <output>` in short follow-up calls until it completes.
- **`FileDownload` on a JSON partial >~2MB errors "exceeds maximum allowed tokens" but still saves
  the RAW MCP response to a local `.txt` file** — that file is `{"result": "[task:...]
  base64:NNbytes:<payload>"}`; the actual content is BASE64 but prefixed with a `[task:...]
  base64:NNbytes:` header that must be stripped (`re.search(r"bytes:(.*)$", s,
  re.DOTALL).group(1)`) before `base64.b64decode` — decoding the raw `result` string directly
  fails with an "Invalid base64" padding error.

**CRITICAL — a leftover manually-launched cam2 painter (from debugging something else) SILENTLY
corrupts the NEXT real E2E run's optical measurement (2026-07-10/11, #466).** If you ever manually
launch `frame-probe`/`rig-mode.sh test` on cam2 for debugging (e.g. reproducing a DIFFERENT bug)
and don't cleanly kill it afterward, it keeps holding `/dev/dri/card1` (KMS master). The next
`recording-e2e.sh` run's OWN fresh painter launch then CANNOT acquire DRM master, silently falls
back to the tearing-prone fbdev `VsyncFb` path (`camera_box::probe::presenter: DRM/KMS unavailable
... falling back to fbdev`), and the monitor keeps showing the ORPHANED process's STALE run_id
content for the WHOLE recording — the merged verdict then shows cam2's optical read collapsed to
0 frames (100% dropout), which reads exactly like a catastrophic rig failure but is purely a
leftover-process artifact. **Before trusting ANY E2E run's optical result, check cam2's
`/tmp/painter.log` (or `/tmp/rig-painter.log` for a manual `rig-mode.sh` launch) for the "falling
back to fbdev" WARN — its absence (clean `presenter: using DRM/KMS page-flip` line) is your
confirmation the measurement is real.** Prevention: `fuser -v /dev/dri/card1` on cam2 before
launching anything — it should show only the permanent `cam2-painter` service (`#440`), never a
stray `frame-probe`; kill any stray with `kill <pid>` (or `pkill -x frame-probe`) BEFORE the next
launch, not after.

**imag's scene is ALWAYS pinned to `Cam 1` (cam1's feed) regardless of which `CAM=` you pick for
strih's SOURCE role.** A `CAM=cam4` run's cam4/strih/stream hops can be fully ZERO-loss while imag
STILL fails — because imag structurally always measures cam1's physical feed (Phase-1 provisioning,
`IMAG_PROG_SCENE` fixed constant). If cam1 has an active hardware defect, imag inherits it in EVERY
run no matter which camera is under test elsewhere — this is not a bug, don't try to route imag's
scene to a different camera to "fix" it (that would defeat its diagnostic purpose). Under
`ALL_CAMBOX=1` this is HARD-ENFORCED, not just a default: `scripts/recording-e2e.sh` line ~131
errors out if `CAMERA_NAME != cam1` when `ALL_CAMBOX=1` — every `all_cambox_continuity.imag`-based
gate run is ALWAYS a cam1-vs-imag measurement underneath.

**Don't misread `all_cambox_continuity.imag.segments[].cambox` as "which camera imag is
showing" — it's a borrowed TIME-WINDOW label, not imag's actual source (#674, 2026-07-12).**
imag's OWN OBS scene never moves off `Cam 1` during an `ALL_CAMBOX` sweep (previous paragraph) —
the per-segment `cambox: "CAM1"/"CAM3"/...` values are the SAME schedule-window boundaries the
general (strih-side) `all_cambox_continuity.segments` uses, reused here purely so the two arrays
line up 1:1 for reporting. So an `imag.segments[3].cambox == "CAM2"` entry does NOT mean imag was
looking at physical cam2 in that window — it was still looking at cam1, during the time slot
strih happened to have cam2 on program. Confirmed live: imag's `optical_stuck_density` is
TIME-elapsed-correlated (rises through the recording, same shape on every camera label) while the
general `all_cambox_continuity.segments` copies/gaps are PER-PHYSICAL-CAMERA-correlated (cam5/
cam2 bad in both their turns, others clean) — two different signatures, proving they measure two
different things. A SEPARATE naming collision compounds the risk: imag-nb's own OBS source named
`NDI CAM1` subscribes to the raw NDI advertisement `"CAM1 (usb)"` — literally physical box
hostname `cam1` (10.77.9.61) — which is UNRELATED to the schedule label `CAM1` (which maps to
physical **cam5** via `CAMBOX_SWEEP`, per this skill's own delivery-latency section below). Two
completely different "CAM1"s in the same investigation; keep them straight.

**`#656`/`#663` self-heal firing on the E2E harness's OWN burn-enabled SOURCE-camera deploy USED TO
kill that camera's digital burn measurement for the REST OF THE RUN, with no respawn — fixed by
#668 (STATUS: shipped, not just filed).** `recording-e2e.sh`'s `[2/8]`/`[2b/8]` deploy now runs
under a real TRANSIENT `systemd-run --unit=camera-box-burn-<RUN_ID>[-<cam>] --collect
--property=Restart=on-failure` unit for every camera under an ALL_CAMBOX sweep (cam1's own
dedicated path AND the `_ccn`/`_cunit` loop for cam2-6) — a mid-recording `#656`/`#663` self-heal
exit (code 77) now respawns instead of dying silently. If you still see a burn-id sequence stop
dead partway through (`#186` verdict shows everything after classified `BURN-UNREADABLE`), it is
NOT this now-fixed class — investigate fresh.

**Finding a PAST run's cam-box capture-rate telemetry (`Streaming: X fps emitted / Y fps
captured`) — `journalctl -u camera-box-burn-<RUN_ID>` is EMPTY of application output; the real
source is `/tmp/cbox-burn.log`, and it is OVERWRITTEN before every new burn (2026-07-12, #674
confirmation, #716 filed).** During an actual gate-run recording, the PERSISTENT
`camera-box.service` is stopped (to free the capture device) and the transient burn unit above
takes over — but `systemd-run`'s `--property=StandardOutput=append:/tmp/cbox-burn.log
--property=StandardError=append:/tmp/cbox-burn.log` redirects the binary's own stdout/stderr
DIRECTLY to that file, never to journald. `journalctl -u camera-box-burn-<RUN_ID>` only ever shows
systemd lifecycle lines (`Started`/`Deactivated`/`Consumed ... CPU time`) — zero `Streaming:`/WARN
content, on ANY invocation, past or present; this is NOT the #693 stale-cross-restart class (the
data was never in journald to begin with — invocation-ID scoping doesn't help). Worse: `[2/8]`
does `rm -f /tmp/cbox-burn.log` before EVERY new burn, so **only the MOST RECENT run's raw fps log
survives on the box at any moment** — with `full-path-e2e` firing roughly every 30-45 min on this
repo, a specific past run's telemetry is often already gone by the time you go looking for it.
`$OUTDIR/cam1-capture-stats.txt` (a coarse END-OF-RUN total: `v4l2_dropped=N`,
`frames_captured=N`, scp'd back to dev1 already) survives per-run but has no per-window
granularity. **#716 (open)** proposes persisting `/tmp/cbox-burn.log` to dev1 per run the same
way. Until it lands: if you need a SPECIFIC past run's fps history and it's not the latest one,
it's gone — don't waste time hunting journald or CI logs for it (checked both, neither has it).
Workaround for "pick a currently-healthy `CAM=` before starting a NEW test" (a live, forward-
looking check, not a past-run lookup): `journalctl -u camera-box | grep -iE 'Streaming:|WARN'` on
each candidate box IS valid for this — that reads the PERSISTENT service, which only runs
between/before tests, not during one.

**Killing a `python.exe` process on a `win-*` MCP box by NAME can kill the MCP's OWN backend and
drop the tool's transport.** The `win-strih`/`win-stream-snv` MCP server's own remote agent runs AS
a `python.exe` process on the box (`python -m remoteos --transport streamable...`) alongside
anything else named python (e.g. a helper `http.server` you started for a file transfer). `Get-Process
-Name python | Stop-Process` or `taskkill /F /IM python.exe /T` kills BOTH — the MCP call itself then
reports "transport dropped mid-call". Before broad-killing by process NAME on one of these boxes,
check `Get-CimInstance Win32_Process -Filter "Name='python.exe'" | Select CommandLine` and kill only
the specific PID(s) whose command line is what YOU started (e.g. `-m http.server`), never a name-wide
kill.

**PowerShell `Start-Process -ArgumentList` gotcha — an array element containing a SPACE (a Windows
path like `D:\_REC\2026-07-10 17-10-31.mkv`) gets word-split into TWO argv entries unless it is
ITSELF wrapped in escaped double quotes inside the array literal.** `@('--strih', 'D:\_REC\2026-07-10
17-10-31.mkv', ...)` passes the exe two separate args (`D:\_REC\2026-07-10` and
`17-10-31.mkv"`) — `recording-verdict.exe` then fails with `error: unexpected argument '17-10-31.mkv'
found`. Fix: quote the space-containing element itself — `@('--strih', '"D:\_REC\2026-07-10
17-10-31.mkv"', ...)` — so `Start-Process` sees it as one quoted token in the built command line.
This is DIFFERENT from the already-documented "quote space-bearing paths in the whole `-ArgumentList`
STRING" gotcha elsewhere in this skill — it applies the same fix to the ARRAY form of `-ArgumentList`.

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

## Discord full-report after every full-path E2E run (#711) — the field-mapping decisions

`scripts/e2e_discord_report.py` (pure, fixture-tested — `tests/python/test_e2e_discord_report.py`)
composes the Slovak per-run Discord report from the merged verdict JSON. It reads ONLY existing
fields — no new measurement was added — so the value is entirely in which JSON block answers which
of the user's 6 required questions. Read this BEFORE changing the verdict JSON schema (a renamed/
moved field silently breaks the report with no compile-time signal — only the fixture tests catch
it) or before re-deriving this mapping from scratch:

- **"Zero-loss cesta do STREAMU" (per camera)** → `full_chain.loss.camN` (the #186 headline
  burn-id-contiguity gate — `real_drops`/`burn_unreadable`/`present_count`/`expected_count`).
- **"Zero-loss cesta do IMAG" (per camera)** → `all_cambox_continuity.imag.segments`, aggregated
  per `cambox` label (a camera cycles through several segments — sum `copies`/`gaps`/`undecodable`,
  AND every segment's `pass` for that cambox). Falls back to the single combined
  `full_chain.loss.imag` node (`imag_optical_beat_pass`) on a non-`ALL_CAMBOX` run, where imag has
  no per-camera breakdown at all.
- **"Latencia — stabilita + minimálna latencia do imag"** → `all_cambox_latency` (the SOURCE-side
  `cam2→camera-capture` hop, 5 cams, cam2 excluded — see the section above this one, "measures
  SOURCE-side `d_X`"). **There is NO measured camera→imag latency field in this codebase today.**
  `all_cambox_latency`'s minimum is reported as an honest FLOOR (imag receives the SAME camera NDI
  before strih ever touches it, so imag's real latency is ≥ this number, plus imag's own unmeasured
  receive/hold time) — never claimed as the actual imag number. If a real camera→imag latency hop
  is ever added, point this section at it directly instead of the floor.
- **"Video sync NDI kamier v strih OBS (delivery-latency spread, per-camera holds)"** →
  `all_cambox_delivery_latency` (the RECEIVER-side `strih_burn − camera_burn` hop, all 6 cams incl.
  cam2, #286/#624) — the issue's own parenthetical names this block directly, word for word.
- **A/V UNKNOWN reasons** — never a bare "UNKNOWN": `candidates==0` → `"tichá stopa"` (the literal
  phrase issue #711 requires for a silent audio track); `candidates>0` but `cluster_samples==0` →
  `"nedostatok konzistentných vzoriek"`. Distinguishing these two matters — #709's real run had
  BOTH in the same JSON (cam2 measured, the other 5 candidates-present-but-unclustered) and a
  single generic "UNKNOWN" would have hidden that cam2's mic path was fine while the others simply
  didn't have a usable window.
- **#714 (2026-07-12) — SUPERSEDES the old "only cam2 can ever be Measured" framing.** The 5
  non-cam2 cameras' UNKNOWN was root-caused (not just described) as pure sample-density starvation
  — live data showed their per-window candidate counts ARE non-zero and roughly proportional to
  window duration (the dual-QR ticks genuinely decode fine in every camera's own window, per the
  rig's shared-HDMI-splitter optical-injection topology), they just can't accumulate
  `MIN_AV_SAMPLES` real matches in a single ~30-60s slot (cam2's own whole-300s pool only clears
  the floor at ~1 real match per ~10-13s). `av_window::derive_camera_av_sync` now gives every
  such camera a DERIVED estimate (cam2's own offset re-centered on this camera's own `#286`
  delivery-latency delta vs the run's mean) — reported as `verdict=="derived"`, NEVER conflated
  with `"measured"`. The Discord composer renders it as `"ODVODENÉ <value>"`. A camera is a bare
  `"unknown"` now ONLY when there's no `#286` delivery sample to derive from either (e.g. no
  `--strih` recording supplied, or that camera never appeared in the sweep at all).
- **Known-blocker ticket hints** (`KNOWN_BLOCKER_HINTS` in the composer) are annotations ONLY — the
  technical description on each line (e.g. "Kontinuita medzi kamerami (stream): FAIL") is always
  accurate on its own from the JSON; the `#707`/`#588`/`#604`/`#689`/`#641` pointers are a
  convenience that WILL go stale as those tickets close. Update the dict when they do; a stale
  pointer degrades to "a still-correct FAIL with an outdated ticket number", never a false claim.

**Delivery mechanism** — `scripts/lib/e2e-discord-report.sh`, called from `[8/8]`'s
`E2E_EXECUTE_VERDICT=1` branch in `recording-e2e.sh` (the ONE code path both the CI PR gate and a
manual supervisor-driven run share). Reuses the bot-token `#notifications` POST this skill's own
"Discord CI Notifications" section documents — fail-open by design (runs under `set +e`, restores
the caller's `errexit` before returning), so a Discord outage can never fail the real gate.
**Verify delivery the same way as everywhere else in this skill: the created-message `id`, not
`fetch_messages`** (this channel isn't allowlisted for the in-session Discord plugin) — `curl -H
"Authorization: Bot $DISCORD_BOT_TOKEN" ".../channels/<id>/messages?limit=N"` reads recent messages
back directly when you need to eyeball real content (used to verify #711 end-to-end against a live
CI run before closing the issue).

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

**Precondition — GONE since #747 (2026-07-13): the gate now WARMS each input itself.** The old
"Multiview projector must be open" precondition (#276) was silently INVALIDATED the same day it
was written by #730/#508's Multiview decoupling (see the GOTCHA below) — a decoupled Multiview
renders low-bandwidth `MV Cam N` TWIN clones, not these raw main inputs, so keeping it open no
longer keeps `NDI cam4`/`NDI cam6` etc. rendering at all. The fix: before sampling EACH source,
`frozen-camera-gate.py` now sets that input's wrapping strih `Cam N` scene (see
`scripts/strih_mv_scenes.py`'s naming convention — `_scene_for_input('NDI cam5') == 'Cam 5'`) to
PREVIEW (Studio Mode, always on on strih/stream) and settles `--warm-settle` seconds (default
3.0, `FROZEN_CAM_WARM_SETTLE_S`) BEFORE taking that source's samples — this genuinely opens/
refreshes the DistroAV receiver. Frozen = repeated identical hash WHILE ACTIVATED; a null/
identical-cold read is no longer possible because nothing is ever sampled cold. The original
preview scene is restored once every source has been sampled. The companion
`scripts/warm_cam_scenes.py` (new `[4f/8]` step) does the SAME cycle-and-restore once, right
before `[5/8] StartRecord`, so the `[6/8]` ALL_CAMBOX sweep's first cut to each camera isn't
ALSO a cold connect.

**Sources checked by default:** `NDI cam1, NDI cam2, NDI cam3, NDI cam4, NDI cam5, NDI cam6,
NDI cam7` (the raw strih inputs; #753 fleet growth 6→7 extended this).

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
FROZEN_CAM_SOURCES="NDI cam1,NDI cam2,NDI cam3,NDI cam4,NDI cam5,NDI cam6,NDI cam7"  # sources to check
FROZEN_GATE_BIN=/path/to/frozen-camera-gate  # binary path (auto-discovered via PROBE_BIN_DIR)
```

**Binary discovery in frozen-camera-gate.py:**
1. `--verdict-bin` CLI arg or `FROZEN_GATE_BIN` env var
2. `$PROBE_BIN_DIR/frozen-camera-gate` (same dir the E2E harness uses for probe tools)
3. `<repo>/target/release/frozen-camera-gate` (local dev build)

**CI artifact:** `frozen-camera-gate` is built (default features) alongside the probe tools
and uploaded into the `probe-tools-linux-amd64` artifact.

**GOTCHA — ROOT-CAUSED + FIXED (#747, 2026-07-13): "FROZEN" in the FAIL message did NOT always
mean "genuinely stuck repeating the same frame" — read this if you ever see it again post-fix.**
`_screenshot_hash` returns `None` on BOTH failure shapes: (a) a real frozen NDI source (>3
identical hashes in a row), AND (b) a hard `GetSourceScreenshot` RPC/decode failure for that
source — the fail-closed `< 2 successful samples = FROZEN` branch fires identically for either
cause, and the FAIL message reads the same either way (`FROZEN: NDI cam4, NDI cam6`). **Check the
RAW timeline JSON printed just before the FAIL line** to tell them apart: a genuinely-stuck camera
shows the SAME real hash repeated; an availability failure shows `null` for EVERY sample.

**Root cause (confirmed live):** `NDI cam4`/`NDI cam6` were the ONLY main inputs with
`GetSourceActive.videoShowing=false` at gate time — a decoupled Multiview (#730/#508) no longer
gives them a rendering surface, so a not-showing DistroAV source genuinely does not render at
all (not a receiver/network fault). 3 consecutive full E2E attempts (including one on a pure-docs
commit — proving it was rig state, not the code under test) reproduced `[null]*8` for both;
putting either camera's scene on PREVIEW made it `videoShowing=true` within 5s with real
screenshot data. **Fixed** by the per-source warm-up described above — see the "Precondition"
paragraph. Verified on the first post-fix gate run: `[4c/8]` PASS with full live hash timelines
for every input including cam4/cam6.

**Side-effect the fix introduced (tracked on #747 itself, next dispatch's item, NOT re-opened
here):** the warm-up phases add real wall-clock time (5 sources × (settle + samples×cadence) ≈
50s) between `[3/8]`'s painter launch and `[5/8]` StartRecord. A painter started with a fixed
`--duration-secs` can now expire BEFORE `StopRecord` if that budget doesn't account for the added
warm-up time — the tail of the recording reads fully undecodable (`painted tick == none`), not a
new zero-loss defect. If you see a run's LAST one or two ALL_CAMBOX windows FULLY undecodable
(100% of frames, not a partial rate), check the painter's `--duration-secs` against the actual
elapsed wall-clock from painter-launch to StopRecord before treating it as a real regression.

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
- **#620 — CLOSED: #604's local-density constants re-grounded against real recordings; the real
  data landed a DIFFERENT finding than expected.** Two qualifying recordings (avg_step≈1.003,
  local_stuck_density 12.2-12.8% in the existing 180-pair window) turned up once real rig runs
  existed. Two things worth remembering for the NEXT calibration:
  - **Verify the dispatch's framing against the data before trusting it.** The task briefing
    claimed "both went green through the merge gate despite 12% local density" — the REAL verdict
    JSONs show `imag_optical_beat_pass: false` / `overall_pass: false` on BOTH under the
    UNCHANGED constants. Always re-derive PASS/FAIL from the actual JSON, never from a summary a
    prior comment/dispatch asserted.
  - **A "12% local density" real anchor did NOT turn out to be a short isolated burst.** Gap-
    between-duplicates analysis on the raw per-frame tick sequence (`imag-partial-<run>.json`'s
    `frames[].tick`, in `frame_index` order) revealed a striking non-random spike at EXACTLY 15
    adjacent pairs apart (~4 Hz at 60fps) recurring through nearly the WHOLE ~367s span in both
    recordings — a pervasive periodic defect, not a bounded event. Consequence: the WHOLE-window
    #588 term (`imag_optical_stuck_density`, 4.7%/6.3%) already failed both independently of #604's
    local term. **Reusable technique for the next density-gate calibration:** (1) compute the
    gap distribution between consecutive Δ0 pairs (`stuck_idx[i] - stuck_idx[i-1]`) — a sharp spike
    at one gap value means a PERIODIC defect, a flat/uniform distribution means genuinely random
    noise; (2) sweep `max_local_stuck_density` across several window widths (e.g. 30/60/180/
    600/1200 pairs) — a smooth monotonic decrease with no knee means the defect is pervasive
    (any window width works), a sharp peak at one width means a genuinely bounded burst (that
    width is the one to keep). Both are cheap Python re-implementations of the Rust ring-buffer
    algorithm run directly against the partial JSON — no rig access needed, no new recording.
  - The window (180 pairs) and ceiling (5%) VALUES were kept UNCHANGED — the real numbers
    CONFIRMED rather than moved them (real judder local density sits ~2.4-2.6x above the ceiling;
    the only known healthy anchor, 572001, sits ~50x below it). The root cause of the periodic
    defect itself was filed separately as #656 (out of scope for a calibration ticket) — a
    calibration ticket's job is to get the THRESHOLD right, not to fix what it measures.
- **#656 CLOSED (2026-07-10)** — root cause was cam1's ShadowCast 2 silently delivering ~64fps
  instead of its negotiated 60.000fps (fixed live via a USB unbind/rebind reset, no physical
  access). Two permanent preventions shipped: an appliance-side WARN (`src/capture_rate_health.rs`)
  once captured fps sustains >1% deviation for 30s, and an E2E preflight
  (`scripts/lib/capture-rate-guard.sh`) that greps the source camera's journal for that WARN and
  fails fast before burning a 30-min run on an already-defective grabber.
- **#660 CLOSED (2026-07-10, PR #662)** — the imag-optical-tail stale-QR defect (LAST ~15-30
  frames occasionally decoding a FROZEN, valid QR carrying a run_id from a PRIOR invocation).
  Root cause: `probe::kms::KmsPresenter` never touches `/dev/fb0` (it drives the CRTC through its
  own DRM dumb buffers) — on `Drop` (painter self-exit), releasing DRM master lets the kernel's
  fbdev-emulation client regain the CRTC and reveal whatever `/dev/fb0`'s UNMANAGED memory last
  held (a prior `VsyncFb` fallback write, or camera-box's own `--display` module's last frame).
  Fixed: `KmsPresenter::Drop` now blanks `/dev/fb0` BEFORE releasing master/fbcon (`src/fb_blank.rs`
  + `probe::fb::blank_fbdev`). Live-verified 3 ways (CI E2E gate showing the tail genuinely
  undecodable not frozen, `/tmp/painter.log` logging the blank firing, and a full manual
  restart-survival re-dispatch showing neither before/after run reproduces it). This is the SAME
  general class as #131/#135 (`.claude/skills/display`'s phantom/latched-framebuffer history) —
  next time a display/painter handoff shows stale content, check whether the NEW writer actually
  clears the OLD writer's buffer before revealing it, the same question that cracked this one.
- **cam1 ShadowCast 2 grabber judder is NOT permanently fixed by #656's USB reset — it recurred
  same-day, filed as #663.** #656's `authorized` 0→1 toggle fixes the symptom for a while, but the
  underlying quantized-~64fps drift can return within hours; when it does, expect BOTH (a) a
  gross version (captured fps visibly >61 in `journalctl -u camera-box | grep Streaming:`, fixed
  the same way) and (b) a SUBTLER version that still averages ~60fps but fails imag's `#588`
  systematic-judder gate (Δ0 duplication density over its 1% ceiling) — the existing
  `capture-rate-guard.sh` E2E preflight only catches (a), not (b). If a restart-survival or
  full-path E2E run fails ONLY on imag's `#588` judder gate (never on delivery/burn contiguity),
  re-check cam1's capture rate and USB-reset it before assuming a code regression.
- **#674 CLOSED (2026-07-12) — the "subtler version" above is CONFIRMED, with a precise number:
  imag's `#588` judder is a faithful pixel-level relay of cam1's OWN chronic ~64fps-captured/
  60fps-emitted rate, not a downstream/software artifact.** Freshly measured across an ENTIRE
  5-min recording (`/tmp/cbox-burn.log` for `RECORDING_E2E_RUN_ID=1740128460` — see the
  cam-box-telemetry-retrieval note above): captured fps rock-steady 63.9-64.0 in EVERY window,
  zero capture-dropped, zero self-heal/WARN lines anywhere — this is a CHRONIC, always-on
  condition (also confirmed live outside any recording, twice, at different times), not an
  intermittent "episode". The math: (64.0-60.0)/60.0=6.67% lines up almost exactly with imag's
  measured 6.76-7.04% judder density for that same run. Matches #685's already-established
  mechanism precisely ("internal resampling → duplicate-frame bursts" from the ShadowCast 2's
  free-running USB clock) — self-heal correctly did NOT fire (6.67% sits inside the post-#685
  `SHADOWCAST2_CAPTURE_RATE_TOLERANCE_PCT=10.0` envelope; that's intended, not a gap). **Residual
  open lead, NOT chased further:** a DIFFERENT run in the same investigation (`1790862887`) showed
  a DECLINING judder shape (6.98%→0.47%→back to 4.3% within one 5-min recording) that a purely
  chronic/always-on mechanism does not by itself explain — its own raw fps log was already
  overwritten by the time it was checked (the exact #716 gap), so this specific run's cam1 rate
  could not be verified. If `#716` lands and a future dispatch has spare E2E budget, checking a
  fresh decline-shaped run's own `/tmp/cbox-burn.log` (or, better, `capture-rate-selfheal.state`'s
  `last_heal_epoch_s`, per the diagnostic two entries below) against its own judder shape would
  close that residual question.
- **Diagnosing an imag freeze/stall during an ALL_CAMBOX sweep: always check CAM1's own self-heal
  state, never whichever camera strih's sweep window LABEL happens to show at that moment (#670,
  2026-07-11).** imag's PROGRAM-feeding input (`NDI CAM1`) is FIXED to cam1's own feed for the
  WHOLE recording — the ALL_CAMBOX `[6/8]` loop only ever calls `obs_phase2.py switch --host
  "$STRIH"`, it never touches imag. So a stall on imag that happens to land inside, say, the
  "CAM2 window" of strih's OWN independent sweep schedule has NOTHING to do with CAM2 — that's
  purely two unrelated schedules overlapping by chance. A real incident (#670, filed as a
  "mysterious ~2.8s NDI-receiver stall ~26-29s after a re-route") was mis-investigated by checking
  `/tmp/cbox-burn-cam2.log` (cam2's own burn log) instead of cam1's, because the freeze fell inside
  the CAM2-labeled window — cam2's log looked clean, so the freeze was wrongly declared a novel
  genlock/NDI bug. The actual cause: `cat /run/camera-box/capture-rate-selfheal.state` on cam1
  itself showed `last_heal_epoch_s` matching the freeze's start epoch to the EXACT SECOND — a
  routine `#656`/`#663` self-heal event on cam1's known-recurring ShadowCast 2 grabber defect.
  **Two reusable diagnostics for this class of "imag froze" report:** (1) SSH/MCP into cam1 and
  read `/run/camera-box/capture-rate-selfheal.state` (tmpfs, survives until the next heal or
  reboot) — an exact epoch match against the freeze's `gen_ts_ns` settles it in one command, no
  packet capture or vendor-source spelunking needed; (2) even WITHOUT cam1 access, the imag
  partial JSON itself is diagnostic: if cam1's OWN corner burn (`run_id=911001`, `BURN_RUN_ID_CAM1`)
  PERMANENTLY disappears from `frames[].payloads` at/after the freeze (grep for the last
  frame_index carrying `911001`) while the OPTICAL tick (cam2's dual-QR) keeps advancing normally
  afterward, that is the exact signature `#668`'s fix commit documents for a self-heal killing the
  ad-hoc E2E harness's un-supervised burn process — NOT a genlock/NDI-receiver defect. Never accept
  "which camera does the window label say" as the camera to investigate for an imag stall; imag
  only ever watches cam1.
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
strih/stream siblings BY DEFAULT.** `recording-verdict-on-strih.sh` / `-on-stream.sh` default to
pure PLANNER mode (a MCP-pasteable plan) — historically because ssh/scp to the Windows boxes was
believed DENIED on this rig; #701 proved plain OpenSSH+password ssh/scp actually WORKS against
strih/stream specifically with the `targets.md` creds, and #703 already wired an opt-in
`--execute` mode into both scripts that runs the SAME shape of flow imag-nb always used (see
each script's own header). Planner mode stays the default for a manual/`workflow_dispatch`
operator run; `--execute` is what the REQUIRED CI merge gate uses (`E2E_EXECUTE_VERDICT=1`).
imag-nb is a plain Ubuntu box, same access class as cam1/cam2 (`targets.md`'s "Linux OBS Targets"
row, SSH `newlevel`/`newlevel`) — bash CAN ssh/scp it directly, so the on-imag helper deploys the
verdict binary (skip if already present+executable), runs `--extract-partial imag` over ssh, and
scp's the small partial (+ `#186` pixel-proof dir) back to dev1 itself, in the SAME script
invocation. Don't
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

**imag's camera mapping is a CLEAN 1:1 — and since #753 (2026-07-14) strih's is too.** Before the
pivot, strih's program input showing cam1 used to be `NDI cam5` (a historical drift-corrected
offset mapping — see `#399` in this file's sibling sections / `.claude/skills/genlock`, marked
HISTORY there now). imag's Phase-1 provisioning (`setup-imag.sh`, #458) pins `NDI CAM1`..`NDI
CAM7` → `CAMx (usb)` 1:1 fresh; strih's binding user directive rebind (2026-07-14) brought strih
to the SAME 1:1 shape. So both boxes' input showing cam1 (the SOURCE camera that films cam2's
monitor) are now simply `NDI CAM1`/scene `Cam 1` on imag and `NDI cam1`/scene `Cam 1` on strih
(`IMAG_PROG_SOURCE`/`IMAG_PROG_SCENE`, `STRIH_PROG_SOURCE`/`STRIH_PROG_SCENE` — all four now agree
in SHAPE, though imag keeps its own uppercase `CAMx (usb)`/`NDI CAMx` naming convention, still
maintained independently of strih's lowercase `NDI camx`).

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

**GOTCHA — rqrr's `Perspective::map` (geometry.rs:55) has ANOTHER internal panic besides the
already-guarded `scan >= 1`: `assert!(x <= i32::MAX as f64)` on a near-degenerate detected grid —
and it CAN fire on the PRIMARY full-frame decode pass, not just tiles (#673, 2026-07-11 live
incident).** `decode_qr_luma_all` (used by every real-frame decode) called the bare, unprotected
`rqrr_decode_all` — a real stream recording crashed a decode worker thread mid-`--extract-partial`,
aborting the whole run with zero output. Fixed: both of `decode_qr_luma_all`'s rqrr calls now go
through the existing `rqrr_decode_all_catch` (previously tile-retry-only), with a WARN log +
opt-in `QR_DECODE_PANIC_DUMP_DIR` env var that dumps the exact panic-triggering frame as a PNG for
a future real-pixel regression fixture (an extensive synthetic-repro attempt — ~130,000+ combined
tries across 6 strategies — could not reproduce the exact assert; see #673 for the full list of
what was tried, useful if this recurs and you're tempted to re-attempt synthesis).

**`IMAG_PROG_SOURCE`/`IMAG_PROG_SCENE` CAN be overridden to follow a non-cam1 SOURCE camera for a
restart-survival dispatch — this is a DELIBERATE exception to the "imag always pinned to cam1"
rule stated earlier in this file, not a contradiction of it (2026-07-11, #466/#674).** When cam1's
own hardware defect (#656/#663 class) is ACTIVELY blocking every attempt (imag inherits it since
it's structurally pinned to cam1), and that defect is ALREADY separately tracked + accepted as a
physical-hardware cost — overriding `IMAG_PROG_SOURCE=NDI CAM4`/`IMAG_PROG_SCENE=Cam 4` (env vars
to both `recording-e2e.sh` AND whatever routed imag's OBS program scene beforehand — imag's OWN
OBS state persists across strih/stream restarts since only strih/stream get restarted) isolates
the restart-survival QUESTION (does OBS restart cause delivery regression?) from cam1's UNRELATED
hardware reliability question. This produced the cleanest full-chain measurement in the whole
EPIC's history — but ALSO revealed a NEW, narrower finding: imag can still fail the `#588` judder
gate even on a proven-healthy source camera, confirmed NOT cam-hardware-related (checked the
source camera's own capture-rate log for the exact window — clean). Root cause not yet found,
tracked on #674. Lesson: don't assume "imag failed" always means "cam1's hardware, again" —
verify the ACTUAL source camera's health for the ACTUAL measurement window before concluding.

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

## RESOLVED (#734, 2026-07-13) — a manual `rig-mode.sh test` right before the CI E2E gate no longer breaks `[3/8]`/#431; the collision is self-healed

**Superseded the GOTCHA that used to live here** ("avoid a manual `rig-mode.sh test` invocation
before a push" — that workaround is no longer needed; both orders now work).

**Root cause, confirmed** (reproduced 2/2 the same night, then root-caused): `rig-mode.sh test`
and the CI harness's own `[3/8]` step both launch a `frame-probe --paint-only` process on cam2
(same pidfile, same fb0) — but `[3/8]` never killed a PRE-EXISTING one before waiting for
`/dev/fb0` and launching its own. A manual `rig-mode.sh test` process left running holds BOTH
`/dev/fb0` AND the audio-marker's ALSA device (`hw:CARD=PCH,DEV=3`) EXCLUSIVELY:
- the `fuser -s /dev/fb0` wait loop merely times out after 15s (busy or not, no failure branch)
  and launches a SECOND frame-probe anyway;
- the #420 "RUNNING" self-check reads the DEVICE-scoped `/proc/asound/PCH/pcm3p/sub0/status` —
  this reports `state: RUNNING` from the OLD still-alive process regardless of whether the run's
  own NEW process ever managed to open the device;
- the #431 emission-growth check polls THIS run's own `--marker-log` file
  (`/tmp/av-markers.csv`), which the new process never writes to if its own `--audio-marker` open
  failed on the busy device.

Net effect: `PASS: #420 ... RUNNING` immediately followed by `FAIL: #431 ... has NOT GROWN` —
device-scoped check passes on the OLD process, file-scoped check fails on the silently-broken NEW
one.

**Fix**: `[3/8]`'s `_cam2_kill_existing` step (`scripts/recording-e2e.sh`) unconditionally
`pkill -x frame-probe`s BEFORE the fb0-fuser wait loop, then polls `pgrep -x frame-probe` (bounded,
~10s) until it reports nothing — applied to BOTH the ALL_CAMBOX and plain single-camera
`_cam2_prep` branches. Deliberately does NOT try to REUSE a foreign running painter instead of
killing it: it paints a DIFFERENT `--run-id` than the gate's own `$RUN_ID`, and
`recording-verdict`'s decode only trusts markers/QR burns carrying its OWN run id — reusing a
stale process's output would silently record the wrong run's content.

**Verified two ways**: Rust harness tests
(`tests/harness_recording_e2e_cam2_kill_existing_painter_734.rs`, bound to the dedicated
`_cam2_kill_existing` step — NOT a bare `pkill -x frame-probe` substring search, which ALSO
matches a pre-existing, unrelated `on_fail_cmd` argument in the same block and would false-GREEN
even pre-fix); AND live on the rig — restored TEST mode (painter confirmed RUNNING), then ran the
exact kill+verify-dead commands directly against it: `pkill -x frame-probe` killed it, the
`pgrep -x frame-probe` poll confirmed it gone after 1 cycle (0.5s), `fuser -s /dev/fb0` then
correctly reported fb0 free — the exact precondition that broke `[3/8]` twice is now cleanly
self-healed. **No more need to avoid running `rig-mode.sh test` before a push.**

## TOPOLOGY FACT — ONE camera + HDMI splitter feeds ALL cam boxes the IDENTICAL signal (owner-corrected 2026-07-10)

The optical leg is: cam2 paints its monitor → **ONE physical camera** films that monitor → the
camera's output goes through an **HDMI splitter** → EVERY cam box's USB capture card receives the
IDENTICAL video signal. There are NOT separate cameras per box. Consequences:
- A "per-camera optical degradation" (one box decodes, another doesn't) is IMPOSSIBLE as optics —
  the difference is always in the BOX (USB capture card, delivery rate, decode). Never claim
  lenses/focus/optics for a per-box decode difference (the #642 mistake: "cam1+cam3 optical read
  degraded" was actually both boxes' ShadowCast USB grabbers over-delivering ~62-64fps instead of
  60, creating source-side duplicate/corrupt frames; fixed by remote USB reset — unbind →
  `authorized` 0→1 → rebind, no physical access needed).
- Capture-rate health check first: `journalctl -u camera-box | grep Streaming:` — captured fps
  must be ≈ the configured rate (60). Over-delivery (62-64fps) = defective USB grabber state →
  USB-reset it (#656 prevention adds an automatic WARN + E2E preflight for this).

## CORRECTION (2026-07-12, #708) — the `all_cambox_continuity` schedule labels ARE 1:1 physical box numbers; the prior GOTCHA table here was WRONG

**PRE-2026-07-14 HISTORY — the mechanism below (inversion-cancellation) is now MOOT.** #753's
binding user directive (2026-07-14) pivoted strih's NDI-input→camera mapping itself to a literal
1:1 (no more inversion at ANY layer — see the genlock skill's "strih NDI Input → Camera Mapping"
section and `set-ndi-mapping.py`'s module docstring). The CONCLUSION this section proves —
**schedule label CAMN == physical box camN, no translation needed** — is still true today, now
for the simpler reason that there is no inversion left anywhere in the chain to cancel. The
INVESTIGATION below (three cross-checked methods proving the OLD inverted mapping's
cancellation) is kept as historical record of how that was established; do not re-derive it, and
do not expect the CAMBOX_SWEEP pairing it references (`Cam 5:CAM1` etc) to still exist — see
`recording-e2e.sh`'s CAMBOX_SWEEP default for the current 1:1 pairs.

An earlier version of this section (written during #707) claimed `label CAM1 -> physical cam5`
etc. and warned to "translate before drawing any per-box conclusion". **That table was
empirically WRONG — verified live 2026-07-12 during #708 via three independent, cross-checked
methods:**

1. **SSH `hostname` on all 6 boxes directly**: `10.77.9.61`=`CAM1`, `.62`=`CAM2`, `.63`=`CAM3`,
   `.64`=`CAM4`, `.65`=`CAM5`, `.66`=`cam6` — i.e. box IP N has hostname `CAMN` (or `camN`), no
   reshuffle.
2. **`os_hostname()` (src/main.rs) feeds the NDI sender name directly** (`src/ndi.rs`: sender =
   `<hostname> (usb)`) — so box camN broadcasts NDI name `CAMN (usb)`, confirmed by the ORIGINAL
   (correct, still-standing) "strih NDI Input -> Camera Mapping (INVERTED)" table earlier in the
   genlock skill: `NDI src "CAM1 (usb)" == real camera CAM1 (10.77.9.61)`.
3. **Live `GetSceneItemList` on strih for every `Cam N` scene** (2026-07-12) proves each scene
   name contains the SAME-NUMBERED `NDI camN` input 1:1 (`Cam 5` → `['ASIO zvuk','NDI cam5']`,
   `Cam 4` → `['NDI cam4']`, etc.) — there is NO scene-level inversion. The ONLY inversion in the
   whole chain is the INPUT-OBJECT-NAME → SENDER binding (`scripts/set-ndi-mapping.py`
   `DEFAULT_MAP`, e.g. input `"NDI cam5"` receives sender `"CAM1 (usb)"` = box cam1) — a
   DIFFERENT layer from the schedule label, and the inversions there exactly CANCEL the
   `CAMBOX_SWEEP` scene/label pairing (`Cam 5:CAM1`, `Cam 1:CAM3`, `Cam 3:CAM4`, `Cam 2:CAM2`,
   `Cam 4:CAM5`, `Cam 6:CAM6`) — so **schedule label CAMN, box camN, and the NDI sender name
   `CAMN (usb)` are always the SAME physical machine.** Cross-checked a 4th way against a real CI
   run's own `[seg N/10] <LABEL> via '<scene>' switched at <ns>` log lines (run 29174543633):
   `CAM1 via 'Cam 5'`, `CAM3 via 'Cam 1'`, `CAM4 via 'Cam 3'`, `CAM2 via 'Cam 2'`, `CAM5 via 'Cam 4'`,
   `CAM6 via 'Cam 6'` — exactly matches `CAMBOX_SWEEP`, confirming `label CAMN == box camN` with
   no translation needed.

**Do NOT re-apply the deleted "CAM1→cam5" table.** If `CAMBOX_SWEEP`'s scene:label PAIRING itself
is ever edited to use a non-identity mapping (e.g. `Cam 3:CAM1`), the label would then differ from
the box — always re-derive from the CURRENT `CAMBOX_SWEEP` value + a live `GetSceneItemList` check
(method 3 above) rather than trusting any hardcoded table, this one included.

## Diagnosing `all_cambox_continuity` copies/gaps growth — accounting vs genlock vs REAL box defect (#707, 2026-07-12)

When `copies`/`gaps` look elevated or growing across a run, don't guess — run this triage in order
(each step rules out one whole class of cause; all three were needed to reach ground truth once):

1. **Read `src/probe/recording_segments.rs::window_segment` + its unit tests first** — it is
   decimation-aware (`expected_step`), order-independent for `gaps` (#625), and windows are fully
   isolated (`prev_recorded` resets per window) — an accounting bug here would show as a FIXED
   pattern every run, not noisy per-run variance.
2. **Pull live `genlock-fifo audit` lines from BOTH strih's per-camera ingests AND stream's PGM
   ingest**, scoped to the exact StartRecord→StopRecord wall-clock window (find it via
   `gh run view <id> --log | grep -iE 'StartRecord|StopRecord'`):
   ```powershell
   Select-String -Path $log -Pattern "genlock-fifo audit" |
     Where-Object { $_.Line -match "^HH:(MM_RANGE)" }
   ```
   Group by source, diff `underruns`/`holds`/`overruns`/`backward_steps`/`depth` first-vs-last in
   the window. Flat + zero-growth across the whole window on BOTH boxes = genlock/FIFO is clean,
   the cause is elsewhere.
3. **Pull live `journalctl -u camera-box --since <window start> --until <window end> | grep -E
   "Streaming:|DEFECTIVE|WARN"` on the PHYSICAL boxes** (translate schedule labels first, see
   above) for the exact window. A `captured` fps that stays healthy (~60) while `emitted` sags
   below it — sometimes with an explicit `#666 emit-delivery-rate DEFECTIVE` WARN in the log — is
   a REAL box-level emit-pipeline defect (the #656/#663/#665/#666 family), not a measurement bug
   and not a genlock issue. `#665`/`#666` are the standing tracker for this defect class; a fresh
   `all_cambox_continuity` red run that traces to it is that defect's materialization, not a new
   root cause — link back to them instead of re-diagnosing from scratch.
4. **Two DIRECT diagnostics now exist (#707, 2026-07-12) — check these FIRST, before falling back
   to fps-delta archaeology.** `journalctl -u camera-box | grep '#707'` on the affected box for the
   SAME window: `#707 NDI blocking send STALL on '<sender>'` means the SYNCHRONOUS
   `NDIlib_send_send_video_v2` call itself blocked (network/receiver backpressure — confirms the
   #656/#663/#665/#666 family's long-standing "network/genlock-gate hiccup" suspicion for THIS
   occurrence); `#707 genlock emit-gate SKIPPED N boundary interval(s)` means a clock discontinuity
   (a DanteSync NTP/PTP step, or a stalled poll) leapt `ndi::genlock_emit_gate`'s boundary past
   frame(s) that were never emitted. Neither firing during a confirmed emit-rate deficit rules both
   out and points elsewhere (capture-side queuing, scheduling). See `src/send_stall.rs` /
   `ndi::boundary_skip_count` for the pure decisions.
5. **Painter-common-source discriminator (#707, 2026-07-13) — two cheap checks, EXISTING data,
   before hypothesizing a per-box defect.** If a breach window's `copies`/`gaps` COULD be a
   monitor/painter presentation slip (since every cambox films the SAME shared monitor, see the
   TOPOLOGY FACT above) rather than a per-box USB/emit defect, both checks below run against data
   already on disk — no new recording:
   - **imag's own continuous optical Δ0 rate during the OTHER camera's exact breach window.** imag
     watches the shared monitor continuously via cam1 regardless of which cambox strih's schedule
     had in program. Extract `RecordingFrame.tick` per frame from `imag-partial-<run>.json`
     (`"tick"` field = the JSON serialization of `RecordingFrame::tick`, already excludes node
     burns — see `src/probe/recording.rs`), walk consecutive present ticks, count Δ0 in a ±15s
     margin around the breach window (use the SAME `start_ns`/`end_ns` from `switch-schedule.json`,
     NOT a hand-picked wall-clock guess). A monitor-side event should burst imag's rate ABOVE its
     own baseline at that instant; no burst = the monitor almost certainly did NOT glitch there.
   - **The painter's OWN `flip_ts_ns` log is DIRECT ground truth, stronger than imag's inference** —
     see "Painter ground-truth CSV lifecycle" above for the CSV schema. Compute the flip-to-flip
     interval for every tick (`flip_ts_ns[i] - flip_ts_ns[i-1]`); a genuinely missed vblank shows as
     an interval ~2x the ~16.7ms (60Hz) median, a double-flip as ~0. Zero anomalies + <1ms stdev in
     the exact breach window (even though the SAME recording's `all_cambox_continuity` failed hard
     that instant) is a DIRECT refutation of the painter as the common source — not an absence of
     correlation, an absence of the physical event itself.
   - **SUPERSEDED (2026-07-13) — don't hand-reimplement `window_segment` in Python; RE-RUN the
     real binary via `--merge-partials` instead, and it EXACTLY reproduces the live verdict,
     including the #726 `presentation_cadence` per-window breakdown for free.** A hand
     re-derivation hits two silent traps: (1) `all_cambox_continuity`'s `SegmentFrame`s come from
     the **STREAM partial's `frames`, never strih's** (`segment_frames_from_recording(stream_frames,
     ...)` in `src/bin/recording-verdict.rs` — a comment right above the `MERGE_ARGS` construction
     in `scripts/recording-e2e.sh` says so explicitly: "the all-cambox segmentation reads the
     SINGLE continuous stream recording's frames, carried in `stream=$STREAM_PARTIAL`"); reading
     strih's partial instead gets `frames`/`undecodable` right (both tolerant of the anchor) but
     `copies` silently wrong. (2) `gaps` (`painted_tick_gaps`, #625/#681) is a WHOLE-WINDOW
     **net-span** calculation (`expected_count(from first/last tick at `expected_step`) −
     present_count(distinct sorted values)`) — it is NOT an event-by-event pairing and has NO
     adjacency/order dependence, so it is exquisitely sensitive to using a version of the binary
     whose `painted_tick_gaps` logic predates a fix (an old cached artifact, e.g. from `/tmp/probe-
     tools-*` downloaded days earlier, silently produced `gaps=287` instead of the correct `15` for
     an identical input — no error, just a wrong-but-plausible number). **The reliable recipe:**
     download the CURRENT CI run's `probe-tools-linux-amd64` artifact (`gh run download <run-id>
     -n probe-tools-linux-amd64`, `chmod +x recording-verdict`), then feed it the run's own
     on-disk `strih-partial-<run>.json` + `stream-partial-<run>.json` + `switch-schedule.json`
     with the SAME flags `scripts/recording-e2e.sh`'s `MERGE_ARGS` uses (all default-valued run
     ids — `--burn-cam1-run-id 911001` ... `--burn-cam6-run-id 911011 --burn-strih-run-id 911002
     --burn-stream-run-id 911004 --cam2-run-id <RUN_ID> --capture-fps 30 --strih-emit-fps 30
     --stream-capture-fps 30 --imag-capture-fps 60 --min-secs 300 --av-expected-ms 0`):
     ```bash
     recording-verdict --merge-partials strih=strih-partial-<RUN>.json \
       --merge-partials stream=stream-partial-<RUN>.json \
       --min-secs 300 --capture-fps 30 --strih-emit-fps 30 --stream-capture-fps 30 \
       --imag-capture-fps 60 --cam2-run-id <RUN> \
       --burn-cam1-run-id 911001 --burn-cam2-run-id 911009 --burn-cam3-run-id 911008 \
       --burn-cam4-run-id 911007 --burn-cam5-run-id 911010 --burn-cam6-run-id 911011 \
       --burn-strih-run-id 911002 --burn-stream-run-id 911004 --av-expected-ms 0 \
       --switch-schedule switch-schedule.json --json out.json
     ```
     Verified byte-exact against 5 independent recordings' original `verdict-<run>.json` segments
     (every `frames`/`undecodable`/`copies`/`gaps`/`first_tick`/`last_tick` matched). This is now
     the go-to technique for ANY question that needs per-window sub-detail the top-level verdict
     JSON doesn't carry (e.g. the `presentation_cadence` breakdown below) — never hand-reimplement.
   - **Discriminator result (2026-07-13, #707) — the "balanced dup-then-catchup-skip" optical
     sampling-beat hypothesis is CONTRADICTED by the data.** `presentation_cadence`
     (`src/presentation_cadence.rs`, #726) already computes the EXACT signature a phase-drift beat
     would produce: `paired_events` = adjacent `(duplicate, catchup)` pairs, i.e. `Δtick=0`
     IMMEDIATELY followed by `Δtick = 2×expected_step`. Recomputed all 5 available runs (50
     windows total) via the recipe above: **`paired_events=0` AND `catchup_steps=0` in EVERY
     SINGLE window**, including the two large spike windows (CAM1 33/33, CAM6 15/15) where
     `copies` happens to exactly equal `gaps`. Also checked the broader claim "`copies`≈`gaps`
     always" directly: across the 50 windows, only 15/32 non-clean windows are EXACTLY balanced
     (47%) — the rest show a genuine ±1-3 imbalance (e.g. `copies=2 gaps=0`, `copies=0 gaps=2`,
     `copies=1 gaps=3`) that a uniform physical beat should not produce. Note `painted_tick_gaps`
     is a whole-window NET-SPAN diff (`expected_count − distinct_present_count`), NOT an
     event-pairing computation — a window where `copies` happens to equal `gaps` is an ARITHMETIC
     COINCIDENCE of that window's specific tick span/distinct-count, not evidence of temporal
     pairing on its own; `presentation_cadence`'s `paired_events` is the actual, purpose-built test
     for temporal pairing, and it is uniformly zero. Conclusion: the two big spikes are real,
     structurally distinct events (a 33-tick and 15-tick contiguous span, `copies` and `gaps`
     genuinely equal), but NOT explained by an immediate dup+catchup optical beat — cause remains
     open. Full evidence on #707's own thread.
   - **Event-anatomy result (2026-07-13, #707) — no forced mechanism.** Per-event tables (frame
     index / tick / ±5-frame context / wall-clock spacing) for both spikes vs 3 small-residual
     windows: the delta-histogram SHAPE is identical everywhere (dominated by Δ=1 + a periodic
     Δ=6/7/8 catchup cluster that stays roughly CONSTANT ~130-140/window regardless of duplicate
     count 1-33 — decoupled from the duplicates), and ZERO outlier/backward deltas anywhere. The
     two spikes are TIGHT TEMPORAL BURSTS (33 duplicates in 5.3s of a 30.2s window, 15 in 6.4s),
     front-loaded into the first ~1/5-1/6 of their window — NOT a slow drift spread evenly across
     it. Small-residual duplicates by contrast are isolated, 8-14s apart. No mechanism proposed;
     tables posted honestly to #707.
   - **Post-switch receiver bandwidth-ramp hypothesis (2026-07-13, #707) — REFUTED, code AND
     data.** See the genlock skill's `PROP_BANDWIDTH` note (searchable there) for the code-level
     half: strih's camera NDI sources are ALWAYS `PROP_BW_HIGHEST`, no bandwidth-mode-switching
     mechanism exists on strih at all (the one that does, #501's `genlock_monitor` twin-scene
     low-bandwidth override, is imag-nb-only). Data-level: strih's own `genlock-fifo audit 'NDI
     camN'` `received` counter increments by EXACTLY 300 every 5s sample (a flat 60.0fps) straight
     through both switch instants and both entire duplicate bursts, with `depth` never moving off
     its steady-state value — zero evidence of an arrival-rate dip or ramp anywhere.
   - **Net result after 3 independent mechanism tests tonight, all refuted**: painter/monitor
     presentation slip, optical sampling beat, post-switch bandwidth ramp. The two spikes remain
     real, structurally clean (no real-loss signature at all — see above), genuinely unexplained.
     `send_stall.rs` + `boundary_skip_count` (already deployed fleet-wide) are armed for the next
     natural recurrence; this section + the `--merge-partials`/event-anatomy techniques are the
     starting point for whoever picks this up next.
   - **HYPOTHESIS 4 (decoder MISREADS) — REFUTED with near-certainty, AND the real root cause found
     + fixed (2026-07-13, #707).** Raw recordings for both spike runs were already purged (checked
     live on strih+stream via MCP — only sparse UNDECODABLE-classified pixel PNGs remain, none at
     the copy/gap frame indices), so pixel re-decode was impossible. Instead: the QR wire format
     (`src/probe/payload.rs`) carries a CRC32-checked `(run_id, frame_id, gen_ts_ns)`, and the
     painter's own ground-truth CSV (`painter-<run>.csv`, `tick,gen_ts_ns,flip_ts_ns`) logs every
     REAL refresh. Per `painter::paint_one_frame`, at refresh `T` with `gen_ts_ns=G_T`, BOTH Vernier
     halves (`vernier_ids(T)`) are re-stamped with `G_T` — so the full set of (frame_id, gen_ts_ns)
     pairs a CORRECT decode could ever produce is fully computable from the CSV alone (2 pairs per
     real tick). Cross-checked all decoded cam2-optical payloads across all 5 available verdicts:
     **92939/92939 matched a real painted tick exactly — zero hallucinated/misread values.** H4
     refuted (false-positive odds for a random CRC32 match: ~2^-32/payload, negligible over ~93k
     checks).
   - **But every one of the 33+15 burst frames had exactly ONE optical payload (always the "held"
     half), never two** — root cause: the #207 fast/robust decode gate
     (`decode_qr_luma_all_fast_then_robust_grouped_pathed`, `src/probe/qr.rs`) only ever checked
     digital NODE BURNS before skipping the #202 robust tiled retry — never the dual-QR Vernier's
     own two-half completeness. A frame where the plain pass read the (easy, digital) burns fine
     but missed the actively-repainting Vernier half took the FAST path anyway, silently skipping
     the ALREADY-PROVEN robust recovery. **Fixed** (commits `f25031088`→`6e8fff279`, TDD RED→GREEN,
     8 new tests, CI green incl. `cargo clippy --all-features -D warnings` and
     `cargo nextest --all-features`): added a third `min_distinct_optical` gate dimension —
     `decode_qr_luma_all_fast_then_robust_grouped_pathed_optical`/
     `decode_recording_frame_with_grouped_burns_optical`/
     `analyze_recording_with_grouped_burns_optical`, wired into BOTH `extract_partial`'s
     "strih"/"stream" decode-in-place arms AND the legacy fused `main()` path via
     `args.cam2_pin().map(|run_id| (run_id, 2))`.
   - **Measured effect on a real rig run (RECORDING_E2E_RUN_ID `1492415599`, 2026-07-13): residual
     collapsed from up to 1863 copies/1862 gaps (this ticket's worst historical run) to 5 copies /
     7 gaps total across 10 windows** — no more large single-window spikes (max 1/window). Still
     not literally zero (`all_cambox_continuity.overall_pass` stays `false` on the strict
     `copies==0 && gaps==0` bar) — whether to accept a small calibrated tolerance for this residual
     is a product decision raised to the user on #707's own thread, not decided here.
   - **USER DECISION (2026-07-13): NO tolerance — every residual deviation gets its own documented
     reason instead. #707 EVENT-FORENSICS tooling built + fixture-tested the same day** (live
     harvesting resumes once cam2's disk is fixed, #737): `src/residual_events.rs` (Tier-0 pure
     module) locates a `Copy` event on every recorded-order adjacent tick repeat and a `Gap` event
     on a backward jump or a forward jump beyond `|Δ|>10` (this section's own outlier threshold,
     reused as the discriminator) — reported as `residual_events[]` on `CamboxSegment` /
     `SegmentedContinuity` in the verdict JSON, each with frame index, tick values, wall-clock
     epoch second, and switch-schedule offset. The stream box's `--extract-partial`, when given the
     same `--switch-schedule` the merge already consumes, now flags each event's ±2-frame
     neighbourhood for #186 pixel proof (`recording-e2e.sh` pushes the schedule to the stream box
     for this). `scripts/event-forensics-dossier.py` (offline, given the verdict JSON + already-
     pulled strih genlock-FIFO-audit lines + per-camera journal lines) groups matching log lines
     under each event by wall-clock second, ready for whoever picks up the next real residual —
     don't hand-grep, run this first. The Discord report (#711) now prints "Odchýlky s dôvodmi: N
     s dôkazmi / M otvorených" every run.
   - **Hit the `gh workflow run` (`workflow_dispatch`) plan-only trap myself despite it already
     being fully documented** (see "`gh workflow run`... is the LEGACY plan-only soak" section
     below, and `CLAUDE.md`'s own GOTCHA at "a live-triggered E2E gate run can race ahead of a
     mid-cycle fleet redeploy" — both already correct on disk). **Root cause of MY mistake: I
     trusted the project CLAUDE.md text embedded in my system-prompt snapshot instead of doing a
     live `Read` of the actual file before acting** — a PRIOR session had already corrected that
     CLAUDE.md section (and cross-referenced this skill's own canonical section below) sometime
     after my snapshot was taken, but I never re-read the live file to pick up the fix. Recovered
     via `gh run rerun <the pull_request-triggered run>` per the existing playbook, exactly as
     documented. **Lesson: for a critical live-workflow gotcha on a SHARED dev1 checkout (other
     sessions can and do correct CLAUDE.md concurrently), live-`Read` the actual file rather than
     trusting the system-prompt snapshot, especially right before an action (like triggering CI)
     that a stale gotcha could make expensive to get wrong.**

## RESOLVED (#708, 2026-07-12) — strih's OWN `full_chain.loss.strih.real_drops` periodic 4-frame residual was a per-source-counter accounting artifact, NOT loss

**#741 RESOLVED (2026-07-15, commit `4346f4993`) — the residual case WAS the #708 window-boundary
class after all; the 2026-07-13 note below (kept for the methodology-failure lesson) reached the
WRONG conclusion off a data-source bug.** Re-running the ACTUAL `recording-verdict` binary against
fresh partial-JSON data found every flagged id landing within **1-32ms** of a real
`--switch-schedule` boundary — the OPPOSITE of the stale "~4.2s before, well outside the 1s guard"
claim below. The 2026-07-13 investigation's timestamp lookup used **strih-partial's own local
`gen_ts_ns`** for the 911002 burn; the CORRECT source is the **stream-partial's forwarded 911002
payloads** (strih's burn is read downstream, from the stream recording — same as
`full_chain.loss.strih` itself computes it). Using the wrong file introduced a systematic offset
that made real ~30ms-from-boundary events look like ~4.2s-before-boundary events, sending the whole
"periodic ~30s timer" theory down a dead end.

**The REAL root cause:** `attribute_window_indices()` computed each frame's window placement via
the GUARD-filtered `place_frame_in_window` (the guard exists for CONTENT attribution — see
`RESOLVED #708` below). A genuine program switch changes the active render source within ~30ms of
the boundary; the settle-time guard band is ~1s — so the last frame before a real cut and the
first frame after it are, by construction, ALWAYS inside the guard band on their own side, both
reading back `None`. `crossed_window_boundary` only fires on `(Some, Some)` with differing window
indices, so it could never fire for the exact case it exists to except.

**Fix:** a new `raw_window_index()` (`src/probe/recording_segments.rs`) — the same schedule-
interval lookup `place_frame_in_window` does internally, minus the guard — replaces
`place_frame_in_window` as `attribute_window_indices`'s placement source. `window_of` has exactly
ONE consumer (`crossed_window_boundary`), so content attribution elsewhere is unaffected. New test
`node_verdict_with_optical_strih_backward_jump_within_the_real_transition_guard_is_zero_loss_741`
uses a REALISTIC `guard_ns` (the existing #708 test uses `guard_ns: 0`, which structurally can
never produce the `Guard`/`None` case — why the original fix looked complete while leaving this
gap).

**Reusable lesson (why the 2026-07-13 note below got it backwards):** when correlating a flagged
node's frame against `switch-schedule.json`, always use the SAME data source the metric itself
reads from — for `full_chain.loss.strih` that is the **stream-partial's forwarded 911002
payloads**, never a different node's own local partial, even one that also happens to carry a
911002-tagged burn. Two different files carrying "the same run_id" are not interchangeable
timestamp sources unless you've confirmed they're the SAME decode pass.

---

**STALE, WRONG CONCLUSION (2026-07-13) — kept only for the methodology-failure lesson above, do
NOT reuse this analysis:** a residual 5-real-drop case survived #708's fix on a run that DID
supply `--switch-schedule`, with the exact `first_id > last_id` non-monotonic tell this section
describes — CONFIRMED on a second independent run to be a periodic ~30s-interval phenomenon,
NOT the #708 window-boundary-placement class. Two runs ~20 min apart both flagged 5 ids at
near-identical `frame_index` (within ±35 frames). Cross-referencing each flagged frame's
`gen_ts_ns` against `switch-schedule.json` shows every occurrence sits ~4.2-4.3s BEFORE the NEXT
window's start — well outside the 1s transition guard, so #708's window-placement suppression is
NOT the relevant class here. Elapsed-time gaps between events are consistent near-multiples of the
~30.2s window length (a fixed-period ~30s timer/health-check/self-heal cycle landing at roughly the
same phase within whichever window it falls in, firing in only 5 of 10 windows because its own
period doesn't exactly match the schedule's). Root cause NOT YET FOUND — see #741 for the full
per-run frame_index/timing correlation before re-deriving this from scratch.

`full_chain.loss.strih` (a DIFFERENT metric from `all_cambox_continuity` above — see the "A
DIFFERENT per-node metric" section elsewhere in this file) periodically flagged exactly 4
`real_drop` ids per ~300s ALL_CAMBOX run, always landing in a schedule window ~9.5-10.3s after a
program switch. **Root cause (dated, pre-#730): strih's 911002 render-tick burn is emitted by SIX
INDEPENDENT free-running DistroAV filter instances — one per raw `NDI camN` input** (`BURN_TARGETS`
attaches it to every input so whichever gets cut to program already carries it), and the
always-open Multiview projector (#365's own precondition, at the time) kept ALL SIX rendering
continuously — so all 6 counters free-ran the WHOLE recording regardless of which was on-air, and
their numeric ranges routinely OVERLAP (proven live: one run's cam5 range `66709..=67840`
overlapped cam6's very next window `66934..=68067`). `burn_contiguity_in_window_with_step`'s
backward-jump check (`id < prev` ⇒ `RealDrop`) was built for ONE genuine monotonic counter — it
misread the EXPECTED counter-instance discontinuity at every program switch as a reorder fault.

**STALE as of #730/#508 (2026-07-13) — see the #747 GOTCHA above:** the Multiview projector no
longer keeps every raw `NDI camN` input rendering (it shows low-bandwidth `MV Cam N` twins
instead), so this "all 6 free-run continuously" premise may no longer hold going forward — if
#741's investigation resumes, re-verify the burn-filter's actual per-input render-activity
assumption against the CURRENT (post-decoupling) Multiview wiring before reusing this analysis.

**The decisive discriminator, done OFFLINE with zero new rig time** (per the dispatch's own
instruction — cheapest first): pull the flagged ids' exact values from the verdict JSON's
`full_chain.loss.strih.classified[]` (NOT `frame_index` — that field is an internal sort-position,
NOT the video frame the id was decoded at; use `.id`), then grep BOTH the same run's already-local
`strih-partial-<run>.json` AND `stream-partial-<run>.json` (`frames[].payloads[]` where
`run_id==911002`) for that exact `frame_id`. If it's present in BOTH (own recording AND
downstream), it was never lost anywhere — SPURIOUS. This is the diagnostic-time twin of the
already-shipped #356 cross-recording reconciliation fix (same "does the downstream recording
prove delivery" idea, applied by hand during investigation instead of automatically in the
verdict). **Two locally-cached CI runs' full partials (multi-MB JSON + pixel-proof dirs) survive
in `/tmp/recording-e2e-<run>/` on dev1 for a long time after the run — check there BEFORE
spending rig time on a fresh repro; the evidence may already exist.**

**The fix** (`src/probe/burn_contiguity.rs` + `src/bin/recording-verdict.rs`): a NEW `window_of`
parameter (computed by reusing the EXISTING `frame_gen_ts_anchor`/`place_frame_in_window` #706/#312
already use, so it can never disagree with them) suppresses the backward-jump/decimation-excess
classification ONLY when the previous and current present frame are CONFIRMED to sit in two
DIFFERENT `--switch-schedule` windows. A backward jump WITHIN one window (the SAME counter
instance, which can never legitimately go backward) still FAILS. An UNKNOWN window on either side
never suppresses. Scoped strictly to `node=="strih"` (the one node proven to have this
multi-counter mechanism — `stream`'s 911004 burn is a single continuous counter on one fixed
input). See `burn_contiguity_in_window_with_step_and_schedule`'s doc comment for the full
reasoning; 9 new unit tests lock it, RED-then-GREEN in commit order (`c57f204a8`/`d8f2a5b8b`).

**Reusable lesson for the NEXT per-node burn oddity:** before assuming a flagged id is a genuine
drop, check whether that node's burn is attached to MULTIPLE concurrently-active sources (any
`BURN_TARGETS`-style fan-out) rather than one single fixed input — a multi-instance free-running
counter's numeric ranges are NOT globally ordered by wall-clock time, so any contiguity check
built assuming "one monotonic counter" will misfire at every source-switch boundary. The tell:
grouping the flagged run's payloads by switch-schedule window and checking each window's own
`min(id)..=max(id)` range for OVERLAP with a NON-adjacent (by camera identity) window.

## Cheap standalone repro of ONE recording-e2e.sh step (#627) — don't run the full 30-min harness

To test a hypothesis about ONE specific step of `recording-e2e.sh` (e.g. "does this OBS setting
change right before StartRecord destabilize the encoder?"), you do NOT need to run the full
harness (min `DURATION=300` = 5 min, plus deploy/setup overhead). Call the SAME `obs_phase2.py`
subcommands the harness itself calls, directly from dev1, against the idle rig:

1. `python3 scripts/obs_phase2.py rig-busy-check --strih-host <strih> --stream-host <stream>
   --password ""` first — confirm idle (busy=false) before touching prod OBS state.
2. Exercise the real code path directly, e.g. `prod-scene --host <stream> --program-scene PRO
   --test-latency-source "NDI 2ME PGM" --test-latency-ms <N>` (omit `--upstream` to skip the
   unrelated preload-force and isolate just the one setting under test) immediately followed by
   `record --host <stream> --action start` (the #627 liveness check reports pass/fail in ~4s).
   `record --action stop` right after to end the test recording; `teardown --host <stream>`
   restores whatever was snapshotted.
3. Vary the ONLY variable under test (e.g. the gap via `sleep N` between the two calls) across a
   handful of attempts — each cycle is ~10-15s, not 5+ minutes.
4. To prove a LOGIC change actually behaves correctly (not just "the syntax parses"), stub the
   relevant remote command locally: `systemctl() { ... }; export -f systemctl` inside a `bash -c`
   that sources the lib and invokes the generated snippet — lets you force every branch (always
   active / recovers after retry / never recovers) without needing the rig to actually fail.
5. Clean up any small test recordings you create afterward (`Remove-Item` via the win-* MCP —
   they're easy to spot by their fresh `LastWriteTime`).

This was how #627's `genlock_latency_ms_src` transition hypothesis was tested live (9 varied
attempts in a few minutes total, 0 reproductions) — much cheaper than 9 full 30-min E2E runs.

**#710 — the #627 liveness check now TOLERATES a leading cold-start `False` sample.** A FRESH OBS
process's NVENC/CUDA cold-init can take slightly longer than one liveness poll (`OBS_RECORD_
LIVENESS_POLL_S`, default 2s) to flip `outputActive` to `True` — live repro on imag-nb verifying
#709: `active=[False, True] bytes=[0, 623840]` was a GENUINE recording, but the old "any `False` =
DEAD" rule false-aborted it. `_record_liveness_verdict` (`scripts/obs_phase2.py`) now walks to the
FIRST `True` sample before applying the death checks: a LEADING run of `False` is tolerated as long
as everything from the first `True` onward stays `True`. A `True→False` transition anywhere
(started then died) or an all-`False` window (never started) still fail hard, unchanged — #627's
original protection is fully preserved. Only matters on the FIRST `StartRecord` right after a fresh
OBS process restart; a warm, already-running OBS is unaffected.

## `_assert_program_nonblack` can false-positive on dim (not black) production content — #677

`obs_phase2.py`'s `_assert_program_nonblack` (the [4/8] black-frame self-check before
StartRecord) uses `OBS_NONBLACK_MIN_MEAN=20` as its floor — but that floor was tuned for the
#312 dual-QR TEST monitor scene specifically ("mean ~105 when settled" per its own code comment).
Calling `prod-scene` against ordinary production camera content that happens to be legitimately
dim (mean < 20, but with real bright pixels — peak 231 observed) reports FALSE "renders BLACK"
and aborts the run naming an NDI/genlock problem that doesn't actually exist. Confirmed live
2026-07-11 while investigating #627: 3/3 `prod-scene` calls against the idle stream box's normal
'PRO' scene false-failed the black-check, while the recording that followed each false-fail was
a healthy ~11MB file. Not yet fixed (filed as #677) — if you hit "renders BLACK" during a normal
(non-dual-QR-monitor) run, don't assume the source is actually dead; check the reported
`mean=`/`peak=` values first.

## A recurring bug class: `journalctl -u <unit> -n N` reads across a service RESTART, false-failing a preflight/gate on a stale line

`journalctl -u <unit>` is NOT scoped to the currently-running process instance — it spans the
unit's WHOLE history, so a bare `-n N` lookback can still contain a WARN/error line from a PRIOR
process instance that was just killed by a routine restart. This exact bug has recurred FIVE+
times in this codebase, each time as "a currently-healthy node/box false-fails a gate because the
gate read a stale line": DanteSync journal reads (#550/#591/#595/#607), the #679 log-throttle
variant that hit the DanteSync PTP-lock POSITION comparison (#686), and — live, 2026-07-11 —
`recording-e2e.sh`'s `[0/8]` capture-rate preflight (#656/#693): cam1's `camera-box.service` was
bounced by a routine `cleanup()` restart, and the OLD process instance's `#656 DEFECTIVE` WARN
(logged 2s before the restart) was still inside the NEW instance's `-n 200` lookback and false-
failed the **required merge gate**, even though the new process's own captured rate was already
healthy (confirmed live via a fresh `journalctl` read).

**The fix pattern, once you hit this**: scope the read to the CURRENT process instance —
`systemctl show -p InvocationID --value <unit>` gives the running instance's UUID;
`journalctl _SYSTEMD_INVOCATION_ID=<uuid>` returns ONLY that instance's lines, so a stale line
from a killed prior instance can never leak in again (see `scripts/lib/capture-rate-guard.sh`'s
`capture_rate_journalctl_cmd`, and `scripts/dantesync-gate.sh`'s HTTP-status-first fix for the
DanteSync variant, which sidesteps the journal entirely by preferring dantesync#47's own
`:8898/status` network endpoint). **Three more call sites have the SAME theoretical exposure but
no live incident yet — filed as #694, not fixed**: `scripts/deploy-fleet.sh` (emit-ok + fatal
checks), `scripts/verify-device.sh` (device acceptance), `scripts/upgrade-fleet-ndi.sh` (emit-ok
+ fatal checks). If ANY of those ever false-fails on a stale line, this is the fix pattern.

## "Fleet-synchronized" kernel/USB anomaly across all 6 cam boxes → check the E2E harness's OWN restart first (#687)

Any kernel/USB message that fires at nearly the SAME wall-clock second across ALL 6 cam boxes
(observed: `uvcvideo ... Non-zero status (-71) in video completion handler` on every box, #687) is
NOT automatically evidence of a mysterious external network/avahi/MCP trigger — check whether it
is simply a SIDE EFFECT of `camera-box.service` being restarted fleet-wide first.
`scripts/recording-e2e.sh` / `scripts/rig-mode.sh` explicitly `systemctl restart camera-box` on
each involved cambox as part of E2E test setup/cleanup (`scripts/lib/camera-box-restart-verify.sh`,
`rig-mode.sh:242`, `recording-e2e.sh:552/828/878/1253`), SSH'd to each box in SEQUENCE (never in
parallel) — this is why the restarts (and any USB-open-quirk side effect) land within a ~10-45s
window across the fleet rather than the literal same second, and why the pattern recurs
irregularly (matching however often E2E gate cycles have been dispatched, not a fixed cadence).
**Confirm/rule out fast**: `journalctl --since '<T-5s>' --until '<T+5s>'` on the box at the exact
anomaly timestamp — if `systemd[1]: Starting camera-box.service` appears in the SAME second, the
anomaly is that restart's side effect, done (#687's EPROTO -71 is the well-known harmless
first-open UVC-driver quirk: it fires once right as the freshly-opened `/dev/videoN` settles, with
no re-enumeration or speed downgrade following). **Journalctl retention gotcha (bit this
investigation once)**: a cam box's journald only retains the CURRENT boot's log
(`journalctl --list-boots` shows a single boot) — evidence from a multi-day-old incident is
usually already gone if the box rebooted since; reproduce fresh instead of chasing stale
timestamps. Don't reach for the #656/#663/#685 capture-rate self-heal mechanism as the explanation
without checking first — it leaves its OWN distinct signature (`authorized`/`unbind` USB-reset log
lines), absent from a plain harness-triggered restart.

## `gh workflow run` (`workflow_dispatch`) on `full-path-e2e.yml` is the LEGACY plan-only soak — USELESS for a real red-set delta (#717, 2026-07-12)

`full-path-e2e.yml` branches its whole behavior on `github.event_name`: `E2E_EXECUTE_VERDICT` and
`ALL_CAMBOX` are BOTH `${{ github.event_name == 'pull_request' && '1' || '0' }}` — so a
`workflow_dispatch` run (manually fired via `gh workflow run "Full-path E2E ..." --ref dev` or the
GitHub UI "Run workflow" button) ALWAYS runs with `E2E_EXECUTE_VERDICT=0` and single-camera
`ALL_CAMBOX=0`: it stays in the OLD plan-print-only mode (never decodes strih/stream, never
computes a real verdict) and therefore "succeeds" trivially almost every time. **This is a trap
when trying to get a fresh post-fix verdict on a commit that's already pushed** — triggering via
`workflow_dispatch` burns ~15 min of rig time and reports a meaningless green.

**The required gate only computes a REAL verdict on a `pull_request` (synchronize) event** — i.e.
the automatic run that fires when you push to the branch a PR tracks (PR #704 → `dev`). To get a
FRESH real verdict on an ALREADY-PUSHED commit without creating a new push (e.g. after a
fleet/rig-side change with no code diff, like a manual binary deploy), use:

```bash
gh run rerun <the-original-pull_request-run-id>
```

`gh run rerun` preserves the ORIGINAL trigger's event context (`github.event_name` stays
`pull_request`), so `E2E_EXECUTE_VERDICT`/`ALL_CAMBOX` are correctly `1` again — unlike a fresh
`gh workflow run`, which always dispatches as `workflow_dispatch`. Find the run id via
`gh run list --branch dev --workflow "Full-path E2E (recording-based · hardware · self-hosted dev1)" --json databaseId,event,headSha` and pick the one with `"event": "pull_request"` matching your
commit.

**Two same-commit `pull_request` runs 25 minutes apart can disagree for reasons OUTSIDE your own
diff** — a live #717 verification saw the first attempt compute a real verdict cleanly
(`--colour-gate` localized fine on both strih+stream) while a re-run of the SAME commit 25 min
later hit `--colour-gate: could not localize the dual-QR colour scale in ANY of 12 sampled frames`
on BOTH boxes (filed as #718 — a new, unrelated blocker). Don't assume a second run's failure is
caused by your own change just because it's the same commit; diff the TWO runs' logs
(`gh api repos/OWNER/REPO/actions/jobs/<job-id>/logs`, or `gh api .../attempts/<N>/jobs` to fetch a
specific earlier attempt after a `gh run rerun`) before concluding anything.

## GOTCHA — `timeout CMD` cannot invoke a shell FUNCTION (only an executable)

`timeout SECS win_ssh_run ...` (or any `timeout`-wrapped call to a sourced bash function like
`win_ssh_run`/`win_ssh_upload` from `scripts/lib/win-ssh-exec.sh`) fails with `timeout: failed to
run command 'win_ssh_run': No such file or directory` — `timeout` `execvp()`s its argument
directly; it never goes through the calling shell, so a shell FUNCTION (not a file on `$PATH`) is
invisible to it. This bites specifically when adding a NEW bounded win_ssh_run call somewhere that
didn't have one before (confirmed live, #748, run 29281776692) — every EXISTING sibling call in
`recording-e2e.sh` either has no `timeout` wrapper, or already routes through the fix below.

**Fix:** wrap in `bash -c`, re-sourcing the lib INSIDE that subshell (so bash, not `execvp`,
resolves the function):
```bash
timeout "$T" bash -c '. "$1"; win_ssh_run "$2" "$3" "$4" "$5"' _ \
  "$HERE/lib/win-ssh-exec.sh" "$USER" "$PW" "$HOST" "$PS_CMD"
```
(positional args after the placeholder `_` land as `$1`.. inside the `bash -c` script — avoids
quoting hazards from interpolating them directly into the script string.)

## GOTCHA — OBS-WS `StopRecord`'s RPC reply can race the mp4 muxer's own finalize (moov atom)

`obs_phase2.py record --host <box> --action stop` returns as soon as the `StopRecord` WebSocket
RPC replies with the file path — this is NOT proof the mp4 is fully written. Reading the file
(e.g. `ffmpeg -i <path> -af volumedetect`) less than ~1s later can hit `moov atom not found` /
`Invalid data found when processing input` — the muxer hadn't finished writing the trailing moov
atom + closing the handle yet. This is INVISIBLE on the real ~300s recording (plenty of natural
delay before `[7/8]`'s later download/decode) but bites hard on any SHORT probe recording read
immediately after its own stop (confirmed live, #748's audio-presence preflight, run 29282790031).

**Fix — a BOUNDED RETRY on the read** (same shape as the `[4c/8]` frozen-camera gate's
reconnect-race retry), NOT a longer blind sleep before the first read (`no-timeout-band-aids.md`):
a transient finalize-race clears within 1-2 retries (a 3s settle between attempts is plenty,
live-verified); a genuine failure (ffmpeg missing, wrong path) fails identically on every attempt
and still surfaces the same operator message once attempts are exhausted — the retry never masks
a real problem, only absorbs the transient one.

## imag MV cells are SAME-SOURCE since 2026-07-15 — the #501 low-bw twins are GONE (do not resurrect)

The "MV Cam N" cells on imag nest the SAME full-bw `NDI CAMx` main inputs the program uses —
the `#501` `MV CAMx` genlock_monitor twin receivers are GONE (scenes + seed; final state after a
same-night detour, see #783/#784). THE REAL story of why twins ever seemed necessary: EVERY
"7×fullHD collapses imag" measurement (incl. the 53-59fps FAILs) was taken while the box secretly
ran ALL OBS threads on ONE core (latent isolcpus, #784). With a clean cmdline + full scheduler,
same-source measures: all 7 fullHD arrivals exactly 60fps, render 60fps / 4.8ms / 0 skip, CPU
18%, load 2.8 — twins are pure waste there. Consequences for gate/tooling: imag has NO `MV CAMx`
inputs any more —
`imag_latency_enforce.py` finds 9 NDI inputs (7 mains + 2 resolume), not 16; any check/metric
enumerating imag `MV CAM*` inputs (#761-era) must be re-pointed or it silently no-ops.

**Seed transform-preserve (#783):** `imag_scenes.py seed()` sets a scene item's transform ONLY
when the item was just created — an EXISTING item's transform is the OPERATOR'S (hand-tuned
LED-wall crop) and is never touched. Never re-add an unconditional `SetSceneItemTransform` to a
boot/relaunch path — a hard reboot's autostart seed wiping the operator's transforms is the exact
2026-07-15 incident this guards.

**imag touchpad (`#779`):** libinput `Scrolling Pixel Distance` hard-caps at **50** (BadValue
above it) — 50 is the gentlest scroll possible; persisted in
`/etc/X11/xorg.conf.d/30-touchpad-tap.conf` together with Tapping/TappingDrag/NaturalScrolling.

## GOTCHA — latentná GRUB zmena sa aktivuje až REBOOTOM; isolcpus = single-core OBS (#784, 2026-07-15)

`/etc/default/grub.d/98-imag-isolation.cfg` (isolcpus=2-11, z #484 experimentu 4.7.). KOREKCIA
PO OVERENÍ BOOT HISTÓRIE: izolácia bola AKTÍVNA minimálne od 14.7. 21:31 (boot -5..-2 všetky s
isolcpus v cmdline) — box na nej "fungoval" celé dni, lebo ľahká architektúra (klonové MV bunky +
spiace mains) sa zmestila na JEDNO 4,5 GHz jadro a všetky metriky vyzerali zdravo. Kolaps
neprišiel aktiváciou míny rebootom, ale KOMBINÁCIOU: studený štart všetkých prijímačov na jednom
jadre (relock búrka) + následný same-source pokus (7×fullHD decode na to isté jadro). **isolcpus jadrá plánovač nebalancuje**, takže OBS s
`taskset -c 2-11` skončil s ~200 vláknami na JEDNOM jadre (cpu2) — full-bw NDI decode padol na
~24 fps/kameru, SDK fronta narástla na ~2 s lag; low-bw klony (lacný decode) pritom išli 60 fps.
Sieť/hodiny/GL/render metriky boli ČISTÉ — jediné priame tely: (1) `ps -L -o tid,comm,psr` →
všetky vlákna psr=rovnaké číslo; (2) susedné jadrá na 800 MHz pri "vyťaženom" procese;
(3) full-bw arrival « low-bw arrival na tom istom boxe. Diagnostika arrival rate: delta
`received=` z genlock-fifo audit riadkov za N sekúnd. Fix: cfg zmazaný + update-grub (definitívne
po ďalšom reboote); interim `taskset -a -pc <neizolované jadrá>` na bežiaci obs okamžite uvoľní.
**Pravidlo: isolcpus NIKDY pre viacvláknový proces s range maskou** — len s explicitným
per-vlákno pinovaním. Po KAŽDOM reboote rig boxu over `/proc/cmdline` proti očakávaniu
(drift-guard fazeta = #780 balík). A pri "reboot všetko rozbil": `ls -la /etc/default/grub.d/` —
mtime súborov prezradí míny čakajúce od posledného bootu.
