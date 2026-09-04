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
