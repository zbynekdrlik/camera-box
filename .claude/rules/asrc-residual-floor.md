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

**Whose job / how to fix (do NOT blind-fix in a worker lane):** a vendored ASRC change compiles at CI
only + needs a FULL-BUNDLE deploy + ≥2 h of `buffered_ms`-flat measurement — supervisor-only, and a
wrong-sign "fix" would DOUBLE the drift on the live broadcast rig. The decisive on-box discriminator
BEFORE deploying: drive `swr_set_compensation` from a KNOWN +18 ppm and watch whether `buffered_ms`
goes FLAT (sign was the only bug → negate the ppm fed to swresample / drive the ratio from the
compensator's corrected return) or still drains (net-zero → the corrected timeline must be consumed
directly). Tracked as issue 1325; the dev1 audio-lag watchdog's REPORT-ONLY buffered arm
(`audio_lag_decision.classify_buffered`) makes the drift visible between runs meanwhile.
