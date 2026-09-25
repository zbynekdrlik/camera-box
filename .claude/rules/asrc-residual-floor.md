---
paths:
  - "vendor/obs-studio/libobs/media-io/asrc-compensator.*"
  - "src/asrc_bench.rs"
  - "scripts/av_step_decision.py"
  - "scripts/av-step-alert-watchdog.sh"
  - "scripts/dantesync-fleet-upgrade.sh"
  - "scripts/dantesync-version-gate.sh"
---

# ASRC residual floor on `stream` = Dante-GM-vs-UTC frequency offset (≈ +8 ppm), not a defect

The stream OBS log line `asrc: source 'mbc' estimated=<X>ppm applied=<X>ppm … starved_blocks=N` is
the per-source ASRC servo (#803/#912) reporting the audio-clock-vs-wall-clock rate mismatch it is
absorbing. Two facts about reading `<X>` (established live 2026-09-03, dantesync#109 / #1265):

- **A steady ≈ +7…+8 ppm is the PHYSICAL FLOOR, not "DVS still not on the GM".** The OS wall clock
  (`genlock_wall_now_ns`) is steered to UTC PHASE by dantesync's NTP path; the Dante audio arriving
  through the DVS/VSC is clocked by the Yamaha grandmaster's own free-running oscillator. The two
  domains differ by ≈ 8 ppm on this rig. dantesync measures the SAME quantity independently: its
  phase-slew integrator's held DC — `[PHASE-SLEW] e=… f_phase=-7.97ppm (P=+0.00 I=-7.97) f_ptp=+21.1ppm`
  — is by design "the Dante-vs-UTC drift" (`dantesync/src/phase_slew.rs`, the `PHASE_DEADBAND_US`
  doc: the integrator's absorbed DC keeps being applied to hold the clock on-phase against it).
  **Cross-check: `|estimated| ≈ |f_phase|`** (7.6 vs 8.0–9.5 within servo noise). Pushing this floor
  to 0 would mean disciplining the Dante grandmaster to UTC — out of scope and not desired (Dante is
  the audio master; the ASRC exists to bridge exactly this).
- **A steady ≈ −17…−19 ppm was the DVS/PTP port-collision signature** (dantesync ≤1.8.52 bound its
  IGMP-join sockets to UDP 319/320 at boot, so DVS `ptp.exe` failed `WSAEADDRINUSE` and free-ran on
  the PC crystal). Fixed in dantesync 1.8.53 (`join_multicast` binds an ephemeral port). Live
  acceptance on `stream`: `Get-NetUDPEndpoint -LocalPort 319,320` → both `ptp.exe`; residual moved
  −18 → +7.6 ppm with a sign flip the moment DVS re-bound. A recurrence of a large NEGATIVE residual
  (or `ptp.exe` missing from 319/320) = re-check the port ownership first.

**Reading recipe (read-only, production-safe):** stream OBS log `Select-String "asrc: source 'mbc'"`
(last N lines) + `C:\ProgramData\dantesync\dantesync.log` `Select-String "\[PHASE-SLEW\]"` (the
`f_phase=` DC) + `Get-NetUDPEndpoint -LocalPort 319,320`. Compare the two ppm magnitudes; a MISMATCH
between them (or a value far outside ±10) is the thing worth chasing, not the non-zero itself.

**Per-launch level history in one call (issue 1355):** plain ssh `cd /d "%APPDATA%\obs-studio\logs" &&
findstr /c:"source 'mbc' estimated" *.txt` (and `/c:"audio buffering"`) returns every session's lines
prefixed by the log file name, so each launch's captured `target=`, level median and OBS audio
buffering (64 or 85 ms, random per launch) come out of one read — no PowerShell. Quote the command
in bash DOUBLE quotes: inside a single-quoted bash string the `''` around `mbc` collapses and findstr
matches nothing. The `mbc` sync offset in force lives in `basic\scenes\Stream_Obs.json` (`sync`, ns),
readable with a `.ps1` via `-EncodedCommand`. Password via `$PW`, never literal.

**Related, distinct axes:** `audio_ts_lag_ms` (#1226, audio timeline lag = health) and the av-sync
dock `measured offset` STEP (#1265/#1267, A/V latency step) are different signals — the dock is
`state=STALE` in EVENT mode (no QPSK marker), so A/V-offset evidence only accrues in TEST mode.
`DANTESYNC_VERSION_PIN` in `scripts/dantesync-version-gate.sh` must be bumped WITH the fleet roll
(a canary-only upgrade leaves the E2E dantesync version gate reporting DRIFT on the mixed fleet).

## Addendum (issue 1265, 14.–15.9.2026): the floor is `−f_phase`, not a fixed +8 ppm — it read −17.5…−18.2 ppm on 14.9.

The 13.9. reboot left stream's `asrc: source 'mbc' estimated=` at −17.48 / −17.71 / −18.24 ppm, which
looks exactly like the pre-1.8.53 DVS/PTP port-collision signature. It was NOT: the cross-check above
held — dantesync's own `[PHASE-SLEW] … f_phase=+7.1…+16.6 ppm (I=+12.8…+16.6) f_ptp=−7…+1.3 ppm [TRK]`
sat at the same magnitude with the opposite sign, and `Get-NetUDPEndpoint -LocalPort 319,320` showed
only `dantesync.exe`. So read the value as `estimated ≈ −f_phase` (the integrator's absorbed DC
moves with the grandmaster's oscillator state after every reboot/relock) and compare magnitudes,
never against the literal "+8". The A/V gate stayed green through it (15.9.: residual medians −26.9 /
−12.0 / −49.8 ms at pins 927/927/905, all inside ±90; stream `audio_ts_lag_ms` in its 107 ms low
mode). Only a residual whose magnitude does NOT match `|f_phase|` — with `ptp.exe` (or anything but
dantesync) on 319/320 — is the collision class.

## Addendum (issue 1325, 16.9.2026): the ASRC MEASURES the floor but does NOT hold it out of `buffered_ms`

Distinct from the "is the floor magnitude the #109 collision?" question above: even when the floor
is genuine (`estimated ≈ −f_phase`, `ptp.exe` owns 319/320 — the healthy DVS bind, live-confirmed
16.9.), the stream `mbc` source's mix buffer STILL drains at the full uncorrected rate. Live: `#800
'mbc' buffered_ms` drains ~1.1 ms/min then JUMPS +20…+57 ms when OBS re-buffers, and the ASRC's OWN
`cumulative_correction` (1.06 ms/60s) EQUALS the drain rate — the servo computes the right magnitude
but it never reaches the buffer, so every dock/E2E A/V-offset reading inherits a ±30–50 ms sawtooth.

Root cause traced in code: `asrc_compensator_compensate()` returns a corrected timeline via its OWN
model `corrected = raw / (1 + applied/1e6)` (a slow source, applied<0, DIVIDES → stretches raw UP to
master; this return is what `src/asrc_bench.rs` / the issue-804 harness validate as "locked"). But
`asrc_process_audio()` in `obs-source.c` **DISCARDS that corrected return** and feeds only
`applied_ppm` to `swr_set_compensation`, whose contract (measured in
`scripts/asrc-quality-bench/RESULTS-1016.md`: requested +50 ppm → out/in=+50 ppm) is `output = input
× (1 + applied/1e6)` — the RECIPROCAL ratio sign of the compensator's own lock model. The bench never
catches it because it asserts against `compensate()`'s RETURN, never against a real swresample-fed
buffer. The live drain is 1× (not the 2× a pure sign flip predicts), so the net effect is
compensation-does-not-reach-the-buffer, not a clean inversion.

**FIX LANDED (issue 1325 FIX lane, 16.9.2026) — TWO coupled defects, both in `asrc_process_audio()`
(`obs-source.c`):** (1) the servo measured `master_block_s` against `genlock_wall_now_ns()` = the
dantesync-SLEWED system clock, but the OBS audio mixer thread paces on `os_gettime_ns()` (QPC,
`media-io/audio-io.c`) and `buffered_ms` is balanced against THAT — so the servo saw `|estimated| =
f_phase` and "corrected" a drift the QPC-paced mixer never sees; now it measures against
`os_gettime_ns()`. (2) The compensator's lock model `corrected = raw/(1+applied/1e6)` (applied<0 =
slow source = STRETCH) is the RECIPROCAL sign of the swresample-native wrapper (`output =
input·(1+ppm/1e6)`), so feeding `applied_ppm` un-negated COMPRESSED a slow source and drained the
buffer; the call now passes `-applied_ppm`. The on-box discriminator that decided it was
`SetAsrcOuterBiasPpm` +10 (comment 5703688577): the drain tracked `applied` 1:1, `drain ≈ −applied +
5 ppm`, flat at `applied ≈ +5` (stretch) — so BOTH master-only (compress 5 → drain 10) and sign-only
(stretch 18 vs a mixer needing 5 → grow 13) are individually wrong; both together flatten it. Tier-0
sign gate: `src/asrc_compensation_quantization.rs::servo_applied_ppm_to_sample_delta` (parity:
applied<0 ⇒ POSITIVE sample_delta); anchor: `tests/genlock_preload.rs::vendored_source::
asrc_servo_master_clock_is_os_gettime_ns_and_sign_negated_1325` + both `windows-genlock*.yml`.

**CROSS-CHECK CHANGE — the `|estimated| ≈ |f_phase|` reading above is PRE-#1325 HISTORY on a FIXED
build.** It held ONLY while the servo measured against the slewed wall clock. After the #1325 fleet
deploy, mbc `estimated` reads the true source-vs-MIXER residual (≈ **−5 ppm**, the source is ~5 ppm
slower than the QPC mixer), NOT `−f_phase`. So on a FIXED (post-#1325-deploy) build: a healthy mbc is
`estimated ≈ −5 ppm` with `buffered_ms` FLAT; a large `|estimated| ≈ |f_phase|` (~18 ppm) would now
mean the fix REGRESSED (the servo is back on the wall clock) — no longer the "healthy floor". The
`|estimated| ≈ |f_phase|` cross-check in the sections above still applies to reading OLD/mixed-fleet
logs from PRE-fix builds. The vendored change is CI-compile + fast-DLL deploy (obs.dll) strih/stream
+ ≥2 h `buffered_ms`-flat measurement — supervisor-only; the dev1 audio-lag watchdog's REPORT-ONLY
buffered arm (`audio_lag_decision.classify_buffered`) makes the drift visible between runs meanwhile.


## Live acceptance of the fix (17.9.2026, release 1.7.0-dev.634)

obs.dll `ca46fc166` on stream since 16.9. 22:51 CEST: `mbc` `buffered_ms` over 22:51–00:52 (n=122) slope −0.033 ms/min
(−0.5 ppm), 98–106 ms, largest step 6 ms, zero refills; `asrc: source 'mbc' estimated` −6…−7.2 ppm steady with
`applied = estimated` (the negated value stretches into swresample). So the HEALTHY post-fix mbc reading is a
single-digit-ppm `estimated` (source vs the MIXER clock) and a FLAT buffer — a reading whose magnitude equals
dantesync's `f_phase` again means the deployed obs.dll regressed to the system-clock master. The `#806` outer-loop
bias is folded in BEFORE the negation: `+bias` now COMPRESSES (drains) — re-derive its sign before re-enabling
`av_sync_measure.py --outer-loop`. The asio-starve default list still names `ASIO Input Capture` (VB-Matrix, retired
on this rig, task Disabled) — trim it before enabling that watchdog.

## Addendum (issue 1335, 17.9.2026): a FLAT `buffered_ms` HELD at its setpoint is now the healthy reading — the servo holds LEVEL, not just RATE

The #1325 fix (above) made `applied` actually reach the buffer, so the drain went to ≈ −0.5 ppm — but
the servo was still a pure RATE loop with no LEVEL feedback, so a residual it structurally can't
remove (the 600 s regression window lagging a wandering true rate, ≈ 0.8 ppm mean) still drifted the
buffer ~3 ms/h (105→68 over 12.5 h) toward an underrun. #1335 adds a slow LEVEL integral inside
`asrc_compensator_compensate` (driven by `buffered_ms`) that nulls exactly that residual.

**Reading a POST-#1335-deploy healthy mbc:** `buffered_ms` sits FLAT at the level it locked to (the
integral holds it, oscillating ~±2 ms with a ~3.9 h period — an I-only loop is bounded, not
critically damped, by design), and the `asrc:` line gains `level=<ms> target=<ms> integral=<ppm>
(#1335)`. Healthy `integral` is small (single-digit-tenths of a ppm, ≪ the ±3 ppm clamp) — a
`|integral|` pinned near 3 ppm means the residual it is fighting is far larger than the ~0.8 ppm the
design expects (chase THAT, e.g. a genuine rate defect, not the integral). `estimated` is unchanged
from #1325 (the true source-vs-MIXER residual, ≈ −5…−7 ppm); `level=`/`target=` should track within
a few ms. A MONOTONICALLY draining `buffered_ms` (the pre-#1335 symptom) on a deployed #1335 build
means the level integral regressed or isn't reaching the buffer. The `#806` outer-loop bias is folded
in BEFORE both the level integral and the #1325 negation — re-derive its sign before re-enabling it.

## Addendum (issue 1355, 23.9.2026): `target=` is now ABSOLUTE — the same value every launch

Before #1355, the `asrc: source 'mbc' … level=… target=…` line's `target=` was whatever depth the
mixer had at the first lock (58.9 … 126.1 ms across 10 launches), and every launch held its own A/V
level. On a #1355 build, `target=` is `ASRC_LEVEL_TARGET_MS` (100 ms) + the source's sync offset
(+24 ms on 23.9. ⇒ `mbc` `target=124.0`). It must read IDENTICAL across relaunches with the same
offset. This holds only for a mixed, non-genlock source: a `genlock_fifo` source (e.g. `fallback
repro`, depth ≈ its ~976 ms hold) and a MONITOR_ONLY source keep the old depth-at-lock `target=`.
After a launch, `level=` walks to it in minutes (P term + restore; a 30 ms walk ≈ 10 min) and then
holds it within a few ms.

Reading a POST-#1355-deploy healthy source: `target=` = 100 + offset, `level=` within ~±5 ms of it
after ~15 min, `restore=0`, `fallbacks=0 (#1355)` at the end of the line. A `LOG_WARNING … level
target … UNREACHABLE … fell back to the live depth … (#1355)` line (and `fallbacks=` > 0) means this
source's buffer did NOT follow the stretch for 40 min. Its A/V level is then the per-launch value
again. Chase why the depth cannot move there, for example OBS audio buffering or a source whose
placement is not contiguous. Do not raise the bound. The one-time A/V shift at the first #1355
deploy is `(100 + offset) − the level that launch had`, and the E2E calibrated pin/audio offset
absorbs it once.

## Addendum (issue 1367, 24.9.2026): read `level_avg=`, not `level=`, for the true buffer depth

`level=` is ONE raw `buffered_ms` reading, taken on the callback that closes the 1 s window. The
mixer drains the buffer in 1024-sample ticks (21.33 ms), so that reading lands at a random point
of a ~21 ms sawtooth: the 24.9. production `level=` samples had sd 6.44 ms against 6.16 ms predicted
for a uniform tick phase, while the 20-min block means stayed within ±2 ms of `target=`. A
`level=` spread of ±10 ms is therefore tick phase, never buffer wander.

On a #1367 build the `asrc:` line ends `… fallbacks=%u (#1355) level_avg=<ms> (#1367)`. That field
is the MEAN of every callback's `buffered_ms` over the last closed window, and it is also what the
level loop now holds. The window is not a whole number of ticks, so a residual of a few tenths of a
ms remains. Read the depth from it:

- **Healthy mbc:** `level_avg=` within about ±2 ms of `target=` once settled. The rate chatter is
  the bench's PREDICTION, not yet a live measurement: `applied − estimated` sd ≈ 0.4–0.9 ppm at
  1–2 ms bursty delivery, down from ≈ 3.6–3.7 ppm on the single reading in the same bench. Live, the
  single reading gave ≈ 7 ppm sd, so about half of the live chatter is outside the bench model
  (real delivery jitter, estimate noise). Confirm the post-deploy value in the 1-h live acceptance
  before quoting it as a health bound.
- **The 1-h live acceptance and the 8-h soak** (the goal's A/V item) are judged on `level_avg=`.
  A `level=` excursion alone is not a finding.
- **No consumer parses `level=`.** asio-starve-health reads `starved_blocks=` and cg-chain-verify
  reads `estimated=`, both by name, and `level_avg=` does not contain the substring `level=`. A new
  parser reading the depth should key on `level_avg=`.
- **Discriminator:** a steady `level_avg=` far from `target=` with `restore=0 fallbacks=0` points at
  a real level-loop problem. `level_avg=` tracking `target=` while `level=` jumps ±10 ms is the
  normal sawtooth.

## Addendum (issue 1372 part A, 25.9.2026): the Windows mixer clock is disciplined now — the reading moves again

Since the #1325 fix the servo measures against `os_gettime_ns()`, the mixer clock. On Windows that
was raw QPC, so mbc read ≈ −6 ppm (Dante vs the QPC crystal), and a reading of `|estimated| ≈
|f_phase|` then meant the obs.dll had regressed to the wall clock. Issue 1372 part A makes the
Windows `os_gettime_ns()` run at the dantesync-disciplined SYSTEM-TIME rate
(`windows-disciplined-media-clock.md`). After that deploy:

- `estimated ≈ −f_phase` is the EXPECTED reading again (the mixer runs at the wall rate), not a
  regression.
- Tell the two apart by the deployed build (`GENLOCK_BUILD_SHA.txt` contains the issue-1372 change),
  never by the ppm value alone.
- The health signal is still a flat `buffered_ms` / `level_avg=` near `target=`.
