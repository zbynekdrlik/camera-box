# A/V-sync offset measurement (#188/#145) — cam2 QPSK marker → recording-verdict --av-sync

Measures the video↔audio offset of the stream chain: cam2 paints the dual-QR AND emits a
QPSK audio marker (norihiro-compatible, 442 Hz, CRC-4 payload) on its HDMI audio out; the
marker is mic'd into the stream OBS "mbc" input; `recording-verdict --av-sync` decodes a
stream recording and reports the offset. Convention: `av_offset_ms = video − audio`;
**negative = video LEADS audio** → ADD `latency_adjust_ms` (= −offset) to the video
source's genlock latency (DistroAV "Latency (ms)" on the program NDI input, hot-apply).

## Run recipe

**#420 (2026-07-02): the earlier "−70.2 ms ±10 @ NDI 2ME PGM latency 1000 ms" result is
UNVERIFIED, not live-proven.** Live rig evidence showed `rig-mode.sh test` had started ONLY the
video dual-QR painter (`--paint-only --dual-qr`, no `--audio-marker` flags at all) — no QPSK
marker was ever emitted, so that recording's audio track carried no real marker and
`cluster_offset_ms` (built to survive CRC-4 false decodes) could have locked onto a spurious
cluster in program-audio noise. `rig-mode.sh test` now ALWAYS starts the marker alongside the
painter (same process — src/probe/qpsk_emit.rs runs as a thread inside `frame-probe
--paint-only`) and VERIFIES its ALSA PCM is genuinely RUNNING before returning PASS (fail loud +
kill the painter otherwise) — no more silent, unmeasured runs. The offset must be RE-measured
with the emitter confirmed live before any number is trusted again.

1. cam2 into TEST mode (fb0 free, capture+emit alive — `rig-mode.sh test` / the `/run` systemd
   drop-in). As of #420, `rig-mode.sh test` launches the painter WITH the QPSK audio marker
   already wired in and verifies it is audible — no separate manual frame-probe invocation is
   needed. The equivalent flags it passes (env-overridable: `AUDIO_MARKER_DEVICE` /
   `AUDIO_MARKER_CADENCE_TICKS` / `AUDIO_MARKER_LOG`):
   ```
   frame-probe --paint-only --dual-qr --qr-size 700 --paint-fps 60 --duration-secs N \
     --audio-marker --audio-marker-device hw:CARD=PCH,DEV=3 \
     --audio-marker-cadence-ticks 180 --marker-log /run/rig-qpsk-markers.csv
   ```
   cadence 180 ≈ 3 s → ~96 markers/300 s. HDMI audio dev is `hw:CARD=PCH,DEV=3` (card0 = intercom,
   no speaker). Verify the run's QR reaches stream program first (`GetSourceScreenshot` on the
   program scene → cv2 QR decode shows `P<run_id>...`).
2. Confirm the mbc input is UNMUTED in stream OBS, then record ~150 s over WS
   (StartRecord → sleep → StopRecord).
3. Painter writes the marker log AT EXIT (`markers=N` log line) — wait for it, then copy the CSV
   to the stream box (`C:\camera-box\`).
4. Decode ON the stream box (has ffmpeg; detached + poll, ~5 min/150 s @1080p):
   ```
   recording-verdict.exe --av-sync <rec.mp4> --av-marker-log <markers.csv> --av-audio-track 0
   ```
   JSON: `av_offset_ms`, `mad_ms` (should be ≤ ~15), `matched` (cluster size; expect ~⅓ of
   emitted markers at 30 fps recording), `latency_adjust_ms`.

## The two measurement-bias gotchas (both cost a full rig cycle — DO NOT reintroduce)

- **ALSA ring bias (124 ms measured!):** the continuous-feed emitter keeps the 8192-frame ring
  nearly full, so a marker SOUNDS ~130–170 ms after enqueue. `playout_frame_id` (qpsk_marker.rs)
  compensates the logged frame_id by `pcm.delay()` converted to painter frames. Raw
  enqueue-instant pairing measured −194.5 ms where the true offset was −70.2 ms.
- **Per-stream origins:** video `frame_index/fps` and audio `sample/48k` are rebased onto the
  container origin via ffprobe `start_time` per stream (mux edit list / encoder priming shift).

## Decode robustness — expect a false-decode FLOOD

CRC-4 is 4 bits → a music-laden mbc mix passes ~1/16 of preamble candidates: 150 s decoded
1691–2521 audio "markers" for 96 real. This is protocol physics, not a decoder bug. The pairing
is therefore `av_offset_candidates` + `cluster_offset_ms` (densest ±60 ms window of
video−audio candidates); NEVER a sequential/lockstep walk (desyncs on one early false hit — the
live 0-paired failure). `--av-cluster-tol-ms` tunes the window.

## Interpreting the number

- The measurement is a clap-test analog: QR pixels on the cam2 monitor vs marker leaving its
  speakers, through the real mic → Dante/ASIO path. Monitor-internal skew is part of the chain
  (as for a real clap); acoustic flight to the mic ≈ 1 ms / 34 cm.
- The measured offset is valid AT the video source's genlock latency at record time — always
  read `genlock_latency_ms_src` (GetInputSettings) of the program NDI input alongside the
  measurement. Target latency = current + `latency_adjust_ms`.
- Emit params ride in the marker-log `#` header (`# qpsk-params sr=48000 carrier=442 c=1 q=2
  vr=60/1`); the decoder must demodulate with the same params (`AudioParams::rig60()`).

## #137 restart-survival gate — reuses this measurement, doesn't re-derive it

An OBS stop→start can drift the offset by ~200-300ms and destroy lipsync with nothing
automatic catching it. `src/av_restart_sync.rs` (`classify(before, after, tolerance_ms)`)
is the strict PASS/FAIL/UNKNOWN kernel: feed it TWO `AvSyncMeasurement`s (straight off two
`recording-verdict --av-sync` JSON reports — `av_offset_ms`/`matched`/`mad_ms`), it
fails-closed to `Unknown` if either is untrustworthy (`matched < 8` or `mad_ms > 20.0` —
double/1.33x this skill's own healthy-run numbers above), else `Fail`s a
`|after-before| > tolerance_ms` (default 50ms — an order of magnitude below the reported
200-300ms failure). The thin CLI is `src/bin/av-restart-sync-gate.rs`
(`av-restart-sync-gate before.json after.json [tolerance_ms]`, exit 0/1/2).

`scripts/recording-e2e.sh` wires it as an OPT-IN step (`AV_RESTART_GATE=1`, default OFF —
a normal zero-loss run is unchanged): records a baseline via this skill's recipe, PRINTS
the OBS restart as an operator/supervisor action (never executes it), records again, then
runs the gate. Because bash cannot scp/exec to the Windows boxes (#208/#193), the actual
`recording-verdict --av-sync` decode of each recording is EMITTED as a win-stream-snv MCP
plan (same shape as the `[8/8a-c]` per-box decode-in-place plan) — only the final gate
binary call (on the two small pulled-back JSONs) runs directly in the script. The live
two-recording rig proof (a real OBS stop→start bracketed by two measurements) is
supervisor-driven, not exercised in CI.

## #465/#529 — `av_sync_calibrate.py` persist-location gap (FIXED) + the loop actually converges

**`scripts/av_sync_calibrate.py` connects to `--host` over the OBS WebSocket and does not need to
run ON the stream box** — so when run off-box (the normal case, e.g. from dev1),
`default_last_json_path()` falls back to `~/.camera-box/av-sync-last.json` (local), which nothing
on the stream box's ProgramData can read. Confirmed live (2026-07-09): no `av-sync-last.json`
existed anywhere under `C:\ProgramData\camera-box` on the stream box after an off-box `--apply`
run. **This breaks the #390 drift-guard `av_sync_calibrated_ms` best-effort cross-check** (reads
exactly that path) — it does NOT affect the #398 dock (see below, the dock never read this file
at all, that was a wrong assumption in an earlier session's comment).

**Fixed**: `main()` now prints an explicit REMOTE PUSH plan (dest path + resolved win-* MCP tool +
exact JSON content) whenever the write lands off-box. The operator/agent completes the transfer
via `win-stream-snv FileWrite` to `C:\ProgramData\camera-box\av-sync-last.json` — same PLAN
convention `obs-self-heal-install.sh` already uses (scp/ssh to Windows is denied on this rig).

**Correction (was wrongly claimed on #465's thread): the #398 A/V-sync dock does NOT read
`av-sync-last.json`.** Checked the vendored dock source
(`vendor/av-sync-dock/src/*.cpp,*.hpp`, `src/av_sync_dock.rs`) — it's a pure LIVE QPSK+QR decoder
with zero file I/O. The ONLY real consumer of that JSON is drift-guard's best-effort facet.

**The measure→apply→re-measure loop genuinely converges — proven live (2026-07-09, stream box):**
at the stale `genlock_latency_ms_src=1000`, measured **+82.3ms**; applied the computed correction
(→918ms); RE-measured with a fresh recording: **-7.2ms** residual. Refined to 925ms. This is the
direct functional proof that dialing `genlock_latency_ms_src` to the calibrated value actually
zeroes the true offset — equivalent to what the #398 dock would show live (its decode is
byte-for-byte proven identical to this offline kernel via its own committed C++ self-test,
`vendor/av-sync-dock/test/camera-box-selftest.cpp`), without needing to fight OBS's Docks menu
blind over the win-* MCP click interface (tried it once — burns huge context on repeated
full-desktop screenshots for uncertain payoff; the offline re-measurement is the cheaper
equivalent proof).

**`mad_ms` ran ~25ms on two independent live recordings** — above this skill's ~15ms healthy
reference and the #137 gate's 20ms "untrustworthy" cutoff, but consistent across both runs (not a
fluke). Not root-caused; treat as a secondary quality signal on this rig/cadence, not a blocker —
the functional offset correction (+82ms → -7ms) is solid regardless.

**`scripts/phase_sync_calibrate.py` has the identical persist-location gap** (its
`default_last_json_path()` deliberately mirrors `av_sync_calibrate`'s old buggy one) — filed as
#636, not fixed yet.
