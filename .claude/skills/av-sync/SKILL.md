# A/V-sync offset measurement (#188/#145) — cam2 QPSK marker → recording-verdict --av-sync

Measures the video↔audio offset of the stream chain: cam2 paints the dual-QR AND emits a
QPSK audio marker (norihiro-compatible, 442 Hz, CRC-4 payload) on its HDMI audio out; the
marker is mic'd into the stream OBS "mbc" input; `recording-verdict --av-sync` decodes a
stream recording and reports the offset. Convention: `av_offset_ms = video − audio`;
**negative = video LEADS audio** → ADD `latency_adjust_ms` (= −offset) to the video
source's genlock latency (DistroAV "Latency (ms)" on the program NDI input, hot-apply).

## The `latency_adjust_ms = −offset` (1:1) model did NOT hold on a live recalibration — verify, don't assume one move lands inside ±20ms (2026-07-14, #689)

The convention above says: set the video source's genlock hold by `−offset`, expecting the next
run's offset to be ~0. A live supervisor recalibration on run 581523199 contradicted a clean 1:1
response: cam2 measured **−43.16ms at stream hold 925**, the hold was moved **+43ms to 968**
(exactly `−offset`), yet the NEXT run measured cam2 at **+24.98ms** — a +68ms swing for a +43ms
move (it OVERSHOT ~0 by ~25ms and flipped sign), still outside the ±20ms bound. Whether that ~25ms
is a genuinely-non-unity slope or run-to-run measurement variance in the baseline is unresolved on
one data point — but the operational takeaway is firm: **do NOT assume a single `−offset` hold
nudge will land the measured leg inside ±20ms.** After any hold change, re-run the fused gate and
READ cam2's `av_offset_ms` back; expect to iterate (this run needs a further −~15ms nudge toward
hold ~950), or fall back to the operator dock (#690, which was NOT demoted for exactly this reason).
Every camera's honest post-move offset is now readable directly from `all_cambox_av_sync.<cam>.effective_offset_ms`
(#689/#714 — measured for cam2, sound derived for the sample-starved cameras, null only for a genuine unknown).

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
convention `obs-self-heal-install.sh` already uses (historically because scp/ssh to Windows was
believed denied on this rig; #701 proved plain scp/ssh actually works against strih/stream with
the targets.md creds — for a short in-memory JSON blob like this one either path works fine, this
skill just hasn't been migrated off the original MCP-FileWrite plan).

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

## #634 — the dock now audit-logs its lock/unlock/offset-update transitions

The dock still has zero FILE I/O (the correction above stands), but it now `blog()`s to the
standard OBS log whenever `RollingOffsetCluster`'s estimate changes state — this is what an
operator/agent should grep for to diagnose a live desync (like the closed #529) after the fact,
instead of re-deriving it from a fresh recording:

```
av-sync-dock: LOCKED offset=<ms>ms source=cluster matched=<n> mad=<ms>ms
av-sync-dock: UPDATED offset=<ms>ms source=cluster matched=<n> mad=<ms>ms
av-sync-dock: UNLOCKED last_offset=<ms>ms source=cluster
```

**Correction (2026-07-11, #690): the `idx=<N>` field shown above in older versions of this doc was
dropped in commit `56079f033`** ("drop misleading idx from audit log... the audit-log push() runs
on EVERY CRC-4-accepted marker candidate... the idx8 printed alongside a LOCKED/UPDATED line was
not reliably 'the frame this lock belongs to'"). The three lines above are the CURRENT,
authoritative format — verified against the live `sync-test-output.cpp` glue, not just the doc.

`camerabox::CbLockAuditTracker` (`vendor/av-sync-dock/src/camera-box-audio.hpp`, pure/OBS-free)
owns the transition classification (Locked/Updated/Unlocked, with an "Updated" only firing when
the offset moves beyond a stable tolerance so a healthy lock doesn't spam a line every marker);
`sync-test-output.cpp`'s glue is a thin `push()` + `blog()` switch. TDD'd the SAME twin-harness way
as the cluster estimator itself: `tests/av_sync_dock_audit_log.rs` compiles+runs a tiny
`c++ -std=c++17` program against the real header — reuse this pattern for any FUTURE dock logic
that needs RED→GREEN proof without a rig.

**Correction to the general "frontend C++ is invisible to PR CI" framing (CLAUDE.md's genlock
gotcha applies to the OBS core/frontend, NOT this dock):** `vendor/av-sync-dock/**` has its OWN
automatic pre-merge compile gate — `windows-genlock-fast.yml`'s "Configure/Compile-check
av-sync-dock" job fires on every `dev` push touching this tree and does a REAL MSVC build of the
whole plugin against the genlocked OBS SDK (see `#188` in that workflow). So a dock-only change
gets a genuine compile proof on the SAME push-to-dev cycle as everything else — dispatching the
full 150-min `windows-genlock.yml` afterward is a belt-and-braces extra proof, not the only gate.

## #689/#690 (2026-07-11) — RECORDED AUDIO CAN BE SILENT even when OBS meters show activity; check volume BEFORE trusting a marker recording

A fresh, validly-executed cam2-only measurement attempt (with the #691 harness-stomp fix confirmed
holding — `NDI 2ME PGM` stayed at the calibrated **925ms** throughout, untouched by any E2E run)
decoded to `Error: too few clustered A/V pairs to estimate (audio markers 0, video ticks 6900,
candidates 0, need 4 within ±60 ms)` — **zero** audio candidates, despite the video-side dual-QR
decode working perfectly (7060/7060 frames, 6900 ticks). Root-caused, not guessed:

- `ffprobe` confirmed the recorded MP4 has exactly ONE audio stream (`aac 48000Hz 2ch`, matching
  the marker log's `sr=48000` header) — ruled out a wrong `--av-audio-track` index.
- `mbc`/`fallback repro` are both assigned to OBS audio tracks 1-6 (`GetInputAudioTracks` → all
  `true`) — ruled out a track-routing misconfiguration.
- **`ffmpeg -af volumedetect` over the whole recorded audio stream measured `mean_volume: -91.0 dB`,
  `max_volume: -91.0 dB`** — i.e. the recorded track is essentially digital silence, not just a
  quiet/masked marker. This is despite the live OBS Audio Mixer showing meter motion on `mbc`
  during the test — whatever the on-screen meters were showing was NOT what ended up muxed into
  the recording.

**Mandatory pre-flight for any future av-sync recording (cam2-only OR fused ALL_CAMBOX): before
committing to the full ~150-300s run, record a short 15-20s clip first and run**
```
ffmpeg -i <clip.mp4> -af volumedetect -f null - 2>&1 | grep -E "mean_volume|max_volume"
```
**and confirm it reads meaningfully above the noise floor (e.g. > -50dB) before trusting a marker
will be present.** This costs under a minute and would have caught the #689 failure immediately
instead of burning a full ~15 min record+decode cycle for a doomed recording. Full raw evidence:
issue #689 comment 2026-07-11 (`recording-verdict --av-sync` output, `ffprobe`/`GetInputAudioTracks`/
`volumedetect` results).

**The #398/#690 dock is ALSO blocked by both a stale deployed build AND this same audio-silence
class of issue** — the dock's `on_start_stop()` binds to `obs_get_video()`/`obs_get_audio()` (the
box's PROGRAM canvas + the GLOBAL MASTER AUDIO MIX, the same bus Recording/Stream use), so it is
exposed to the identical "meters show something, the actual bus carries nothing" trap. Confirmed
live 2026-07-11: BOTH strih's and stream's deployed `obs-audio-video-sync-dock.dll` predate the
#398 decode-lock fix (`125c0c617`, 2026-07-03 17:32 — after both DLLs were built) and the #634
audit-logging feature (2026-07-10) entirely; clicking Start against a real, confirmed-live cam2
signal for >4 minutes left Latency/Index/Audio Frequency/Video Index/Audio Index all at `-` the
whole time. Rebuild+redeploy tracked in #698 — do NOT attempt to verify the dock's live lock again
until that lands AND the volumedetect pre-flight above is clean.

## #689 (2026-07-12) — segment-by-segment remote diagnosis: cam2's HDA/HDMI-audio path checked via ELD, not just ALSA PCM state

When a recorded marker is silent (`-91dB`, above) and the ALSA PCM `state=RUNNING`/`owner_pid`
check (the #690 finding above) already proved the SOFTWARE side is healthy, the next remotely-
checkable segment is whether the physical HDMI sink (the monitor) is genuinely negotiated as a
real audio-capable device — not assumed. On cam2 (`ssh root@10.77.9.62`):

```bash
cat /proc/asound/PCH/eld#2.*        # one file per pin×converter combo (usually 32+ entries)
amixer -c PCH contents              # includes the raw ELD bytes + 'HDMI/DP,pcm=N Jack' on/off
```

Exactly ONE `eld#` entry should read `monitor_present 1` / `eld_valid 1`, with `monitor_name`,
`speakers=[0x1] FL/FR`, `sad0_coding_type LPCM`, and real sample-rate/bit-depth lists — cross-check
`monitor_name` against the vendor's own published spec (web search) to confirm the monitor
genuinely ships with built-in speakers (some monitors declare HDMI-audio-passthrough capability in
their EDID without a physical speaker — the ELD alone doesn't prove speaker hardware exists).
`amixer -c PCH contents`'s `'HDMI/DP,pcm=N Jack'` control should read `on` for the SAME device
number the marker emitter targets (`hw:CARD=PCH,DEV=N`), and every `IEC958 Playback Switch`
control should read `on` (the only software mute point HDMI audio has on this card — if this
doesn't exist or reads `off`, that IS a remotely-fixable cause, unmute it and re-test before
concluding "physical"). Cross-check `dmesg -T | grep -iE 'hdmi|jack|audio'` + `journalctl -k
--since "<window>" | grep -i hdmi` for any hotplug/EDID-change event correlated with when the
silence started — no events across the whole uptime rules out a recent cable/monitor-state change
as the cause. If ALL of this reads healthy (as it did 2026-07-12), the remaining candidates are
100% physical (monitor's own OSD volume/mute — its speaker hardware is real per the spec check
above, but its volume is controlled by physical buttons with no software knob) or need Dante
Controller GUI access (not remotely reachable via SSH — check whether a win-* MCP target has it
installed before ruling this segment fully unreachable).

## #690 — before attempting the dock's live-lock verification, check `mbc` is reachable FIRST

`mbc` (Master Broadcast Console, 10.77.9.232) is "often OFF outside broadcasts" (`targets.md`) —
the dock (same as any #689-style measurement) needs its LIVE master-mix audio, so attempting a
live-lock test while it's off just burns a cycle for a guaranteed no-lock. Cheap one-shot check
BEFORE any rig-mode.sh test / dock-Start attempt:
```bash
timeout 5 bash -c "echo > /dev/tcp/10.77.9.232/22" 2>&1 && echo "MBC UP" || echo "MBC UNREACHABLE"
```
"No route to host" = genuinely off (not a firewall/credential issue) — same failure shape #689's
2026-07-11/12 comments already documented for this exact host. Confirmed live 2026-07-12 (#710/
#712/#690 dispatch): `mbc` stayed unreachable across the whole session (checked repeatedly,
09:40-10:03 CEST) — combined with `rig-busy-check` staying idle the whole time, this was purely an
mbc-power block, not a live-broadcast-timing one. The one-page Slovak operator procedure for the
dock (`docs/operator-av-sync-dock-sk.md`) was written from source (`vendor/av-sync-dock/src/
sync-test-dock.cpp`'s `on_start_stop()` + `data/locale/en-US.ini`'s exact UI label text) and
existing documented findings — but the actual "dock locks on real signal, updates live, survives a
camera switch" proof (with a screenshot) is STILL not re-confirmed as of 2026-07-12; #690 stays
OPEN. Re-attempt the live-lock test whenever `mbc` is reachable AND the rig is idle.

## #725/#689 (2026-07-13) — which cam2 HDMI pin is REALLY live: read `codec_cvt_nid`, don't trust
## a remembered `eld#X.Y` index or `aplay -l`'s cached device name

The #725 finding ("pin shuffled from DEV=3 to DEV=7 after a reboot") recorded a SPECIFIC `eld#2.4`
index as corresponding to DEV=7 at the time it was written — but ELD pin/converter numbering can
re-shuffle on a LATER reboot too, so that specific `eld#X.Y` → `DEV=N` mapping is a snapshot, not a
durable fact. `aplay -l`'s device NAME (e.g. `device 3: HDMI 0 [BenQ GL2480]`) is ALSO not reliable
evidence of which pin is currently live — that name string looks like it can be a load-time cache
that doesn't necessarily update on a later EDID renegotiation.

The reliable, direct answer is inside the ELD entry that currently reads `monitor_present 1` /
`eld_valid 1`:
```bash
cat /proc/asound/PCH/eld#2.4        # (or whichever index currently shows monitor_present=1)
# ...
# codec_cvt_nid   0x3                <- THIS is the converter/PCM device number, in hex
```
`codec_cvt_nid` directly names the PCM converter node — `0x3` means the live sink is
`hw:CARD=PCH,DEV=3`, regardless of which `eld#` index it currently lives at or what `aplay -l`
happens to have cached. Cross-check against `aplay -l` for a sanity read, but trust `codec_cvt_nid`
when they disagree. Live 2026-07-13: found `codec_cvt_nid=0x3` (i.e. DEV=3, the hardcoded default
every marker path already uses) was CORRECTLY live again, even though #725 had found DEV=7 live the
evening before — the pin re-shuffled AGAIN sometime between the two checks (plausibly the owner's
live rig work or a jack replug that same night). #725's underlying bug (no DYNAMIC resolution, only
a hardcoded default) is still real and unfixed — this is just the fast, reliable way to check
"is the hardcoded default CURRENTLY correct" before assuming a silent/off-timing recording.

## #689 (2026-07-13) — NEVER derive a `hold_new` recommendation from ONE gate run; the measurement
## itself has real run-to-run noise comparable to the ±20ms gate tolerance

3 consecutive full-path-e2e gate runs, ~30-35 min apart, same unchanged 925ms hold, same unchanged
code, produced cam2 `av_offset_ms` of −33.9 / −64.25 / −21.4 ms (`mad_ms` 22.85 / 15.29 / 32.72 —
only ONE of three cleared this project's own mad≤20ms trust bar). All three breach ±20ms, so "the
hold is off" is a safe conclusion — but the exact magnitude swings by >40ms across barely an hour of
otherwise-idle rig time, which is NOT explainable by hold drift (confirmed live via `GetInputSettings`
unchanged at 925 throughout) or by #725's pin question (confirmed DEV=3 correctly live throughout,
via the `codec_cvt_nid` check above). Before ever recommending (let alone applying) a `hold_new`
from a SINGLE run's number, pull at least 2-3 independent full-gate runs spread over the session and
report the SPREAD, not just the latest value — a single-run recommendation risks overshooting or
undershooting by the same margin the spread itself shows. Filed #733 to audit whether this is a real
clustering-algorithm (`src/av_window.rs`) sensitivity issue (candidate multimodality, 33ms video-tick
quantization interacting with the ±60ms `av_cluster_tol_ms` window) rather than a genuine physical
drift.

## #733 (RESOLVED, 2026-07-13) — the clustering window WAS too wide; a much tighter estimator ships, but the run-to-run swing is PROVEN chain-level, not algorithmic

The #689 finding above was investigated end to end. Method: pull the RAW pre-clustering candidate
data (not just the final verdict) straight off the stream box — `C:\camera-box\verdict-out\
stream-partial-<RUN_ID>.json` carries `frames` (every decoded video tick) + `av_sync.emit_log` +
`av_sync.audio_markers` for that run (the box keeps these even though only the summary JSON/PNG get
uploaded as a GH artifact). Reimplement `window_ticks`/`av_offset_candidates`/`cluster_offset_ms`
exactly (any language) and cross-validate against the published `av_offset_ms`/`matched`/`mad_ms`
before trusting the reproduction.

Two real, shipped findings from that data:
1. **Same-`frame_id` duplicate detections** (37-84ms apart, present in all 3 runs) were inflating
   the sample count — fixed via `qpsk_marker::av_offset_candidates_deduped` (Tier-0, TDD).
2. **The old ±60ms cluster window was wide enough to blend two nearby sub-clusters** — binning one
   run's raw candidates in 10ms bins showed two separate density bumps ~70ms apart that the 120ms
   window swallowed into one noisier blend (that run's outlier `mad_ms=32.7`). A **tight-first
   sweep** (successively widen `cluster_tol_ms` from 15ms up until `MIN_AV_SAMPLES` clears) on the
   SAME deduped data showed each run individually has a MUCH more precise true cluster (mad_ms
   7-9ms at ±15ms, vs 15-33ms at the old ±60ms default) — `--av-cluster-tol-ms` tightened to 25ms.

**The real conclusion — and the reusable lesson**: even at that MUCH tighter precision, the 3 runs'
offsets STILL disagreed by ~50ms (-20.83/-59.67/-6.10ms). A materially more precise estimator did
NOT converge the runs toward one number — proving the instability is genuinely chain-level (ALSA
ring-delay compensation, acoustic path variance, the mbc mastering chain's own state, or similar),
not a clustering artifact. **Do NOT pool raw candidates across separate runs/sessions to
manufacture one "average" number when you haven't first checked whether the underlying quantity is
even stable across them** — pooling here would have silently discarded 2/3 of the real (disagreeing)
signal as if it were noise. When #689 (or a future hold-derivation ticket) is next worked: use the
tightened defaults (mad_ms 7-13ms is the new "healthy" reference), gather a genuinely long baseline
(many runs across a wide time span), and look for correlation with anything observable before
trusting an average — one night's 3 runs is still not enough.

The LIVE dock's own `DOCK_CLUSTER_TOL_MS` (`src/av_sync_dock.rs`) was deliberately left at 60ms —
it's mirrored byte-for-byte into the vendored C++ dock and needs the ~150min genlock build cycle to
verify safely. Filed #735 to evaluate it separately with the SAME tight-first-sweep methodology,
against the live dock's own continuous-rolling candidate stream.

## `gh pr edit` / `gh issue edit --body` can fail on a GraphQL error UNRELATED to your edit — use the REST PATCH instead

On this repo, `gh pr edit 704 --body "..."` (or `-F file`) can return:
```
GraphQL: Projects (classic) is being deprecated in favor of the new Projects experience... (repository.pullRequest.projectCards)
```
and the edit **silently does NOT apply** (the command exits 1, body unchanged) — this is `gh`'s own
GraphQL mutation trying to also fetch/refresh a deprecated `projectCards` field unrelated to the body
edit itself, not a problem with your content. Workaround — call the REST API directly, which has no
such field:
```bash
python3 -c "import json; json.dump({'body': open('body.md').read()}, open('body.json','w'))"
gh api repos/OWNER/REPO/pulls/<N> -X PATCH --input body.json
```
Confirm with `gh pr view <N> --json body -q '.body'` afterward — don't assume the plain `gh pr edit`
error means your content was rejected; it may just mean this unrelated GraphQL field choked.
