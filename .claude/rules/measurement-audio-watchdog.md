---
paths:
  - "scripts/measurement-audio-alert-watchdog.sh"
  - "scripts/measurement_audio_decision.py"
  - "scripts/measurement_audio_meter_probe.py"
  - "systemd/measurement-audio-alert-watchdog.*"
  - "tests/python/test_measurement_audio_decision_1310.py"
---

# dev1-side mbc measurement-audio presence alert watchdog (#1310)

Closes the between-run detection gap behind the mbc **measurement-audio** chain reading DIGITAL
SILENCE: the chain (cam2 HDMI monitor speaker plays the QPSK marker → measurement mic → mbc Ableton
on `10.77.7.232` → Dante Virtual Soundcard → stream OBS ASIO input `mbc`) is the instrument the whole
A/V-sync leg reads. After a production it can go silent (mic off, Ableton mbc channel muted, Dante
route dropped) and NOTHING pages it — the only mbc-silence check today is the in-RUN #748 preflight
(`recording-e2e.sh [4b2/8]`), so a silent chain is invisible until the next full ~300 s E2E burns a
cycle discovering it (release E2E 34764817477 abort `max_volume -91.0 dB`, `n=120` samples flowing but
all zeros; the 2026-07-12 „mutnutý mikrofón prežil týždeň nepovšimnutý" incident). A member of the
production-critical dev1 watchdog class (umbrella **#1308**).

## Architecture (mirrors the fleet-watchdog family — #1307/#1226/#1290)

- `scripts/measurement_audio_decision.py` — the PURE kernel (no I/O, pytest Tier-0, the #1199
  python-mirror pattern). `classify(peak_db, box_reachable, meter_present, threshold_db)` →
  `SILENT` / `PRESENT` / `SKIP` / `UNKNOWN`; `analyze(probe_text, box_reachable, threshold_db)` reads
  the probe's `key=value` stdout. Everything the shell needs to DECIDE lives here.
- `scripts/measurement_audio_meter_probe.py` — the I/O half: connect stream OBS WS, subscribe the
  `InputVolumeMeters` high-volume event (bit `1<<16`), sample ~2 s, take the max `mbc` peak, convert
  to dBFS (clamped floor), print `meter_present=` + `peak_db=`. **No recording, no disk, no rig
  mutation.** Exit code = box-reachable (0 = WS connected). Mirrors obs_phase2's op-1/op-5 handshake.
- `scripts/measurement-audio-alert-watchdog.sh` — I/O + orchestration only, reusing (never
  re-implementing): `obs-watchdog-decision.sh` (2-pass `obs_watchdog_confirm` + the ONE #1308
  `watchdog_notify_key` time-bucket helper), `obs-fleet.sh` (`obs_fleet_host stream`),
  `rig-mode-state.sh` (`rig_mode_from_painter_snapshot` — the #1290 EVENT gate),
  `audio-presence-preflight.sh` (the threshold).

## Gotchas / invariants (do not regress)

- **The -60 dB SILENCE bar is SINGLE-SOURCED, never retyped.** `audio_preflight_default_threshold_db`
  (added to `scripts/lib/audio-presence-preflight.sh` by #1310) is the ONE source; the #748 gate's
  own default-arg sites and this watchdog both read it. The watchdog sources the lib and passes
  `--threshold-db "$(audio_preflight_default_threshold_db)"` to the pure decision; `classify`/`analyze`
  take the threshold as a REQUIRED argument (no hardcoded -60 default in python). SILENT uses strict
  `<` — byte-identical to `audio_preflight_is_silent` (exactly at the bar is PRESENT).
- **#1323 POLLUTED — the level CEILING, the counterpart of the silence floor, ALSO single-sourced.**
  The silence floor proves NOT-SILENT but cannot tell a decodable marker from a chain flooded by a
  loud FOREIGN signal (16.9.2026 data: watchdog peak plateau `-55..-61 dBFS` marker-only vs a flat
  `-5..-8 dBFS` flood, empty gap `-9..-20`). `audio_preflight_default_ceiling_db` (**-20 dBFS**, added
  to the lib by #1323) is the ONE source; the `[4b2/8]` preflight and this watchdog both READ it,
  never retype. `classify(peak_db, box_reachable, meter_present, threshold_db, ceiling_db=None)` →
  `POLLUTED` when `peak_db > ceiling` (strict `>` — exactly at the ceiling is PRESENT). `ceiling_db`
  defaults to `None` so the pre-#1323 4-arg / 3-arg callers keep the SILENT/PRESENT-only behaviour;
  the watchdog passes `--ceiling-db "$(audio_preflight_default_ceiling_db)"`. `require_tools` also
  fails LOUD if that getter is not sourced. POLLUTED shares the SILENT `"stream"` fault path / latch /
  `measurement-audio-stream` dedup base (the chain is unusable either way; recovery only on PRESENT),
  so the #1206 bucketed-key shape is UNCHANGED — no `test_notify_dedup_key_sweep_1206.py` change.
  Since the floor (-60) is far below the ceiling (-20), SILENT and POLLUTED are mutually exclusive
  (SILENT checked first). Applies to BOTH scales: the preflight's `max_volume` and the watchdog's
  `InputVolumeMeters` peak — the marker reads far below -20 on both, a flood far above.
- **`meter_present` distinguishes UNKNOWN from SILENT.** Digital silence is `mbc` PRESENT in the meter
  stream with all-zero levels → peak_db clamped to the floor (−100 dB) → SILENT. `mbc` never appearing
  in any event this window → `meter_present=0` → UNKNOWN (a renamed/removed input, or the meter event
  unavailable on the build), never a fabricated SILENT page. The probe's exit code (WS connect) drives
  SKIP separately — a dead stream box defers to #1001/#732.
- **An OBS-INPUT-LEVEL mute is UNKNOWN, not SILENT — and that is correct.** The three real chain-failure
  modes (measurement mic off, Ableton mbc channel muted, Dante route dropped) are all UPSTREAM of the
  stream-OBS input, so the input stays PRESENT in the meter with zero levels → SILENT (paged). Muting
  the `mbc` OBS INPUT itself typically EMPTIES its meter → `meter_present=0` → UNKNOWN (no page,
  fail-safe): an OBS-input mute is an operator toggle, not a chain failure. **Consequence for the
  live-verify:** silence the SIGNAL (Ableton channel mute / `SetInputVolume` to minimum on a present
  input), NEVER `SetInputMute`, or the verify reads UNKNOWN and fails to confirm the SILENT path
  (systemd README step 3b).
- **The -60 dB bar is the #748 VALUE, but the MEASUREMENT differs — both are peak dBFS.** #748 reads
  `max_volume` from an ffmpeg `volumedetect` over a real recording; this watchdog reads the live
  `InputVolumeMeters` peak multiplier → dBFS over a ~2 s sample. They are not identical
  instrumentation, but both are peak dBFS of the same `mbc` signal, so the shared -60 bar transfers.
  Do not assume identical measurement when reading the "same -60 dB bar as #748" prose.
- **TEST-premise, EVENT-gated (#1290) — but production-critical.** The QPSK marker only sounds in TEST,
  so EVENT → SKIP the whole check (no page) + clear the latch; TEST/UNKNOWN → proceed (fail-safe). The
  rig-mode gate is ORTHOGONAL to the fault-criticality axis: a silent measurement instrument means the
  NEXT production's A/V-sync can't be verified, so its FAULT is production-critical (time-bucketed
  re-ping), even though the check is only meaningful in TEST. `rig_mode_probe` is ONE cam2 ssh,
  `timeout` INSIDE `sshpass` (so a driver test's `sshpass()` function stub stays hermetic — never
  `timeout sshpass …`, `.claude/rules/win-ssh-vs-mcp.md`/#1290).
- **Production-critical TIME-BUCKETED re-ping** (owner ruling #1308): the `--dedup-key` is
  `measurement-audio-stream-<floor(now/REPING_INTERVAL_S)>` via the shared `watchdog_notify_key` on the
  notify line (the #1206 sweep `test_production_critical_watchdogs_actually_bucket_their_inline_key`
  requires the helper literally there). Allowlisted in `test_notify_dedup_key_sweep_1206.py`; recovery
  stays ONE machine-channel log line (#1206).
- **DETECTION ONLY** — the cure (unmute the mic / Ableton mbc channel, fix the Dante route) is a
  rig-ops call; this watchdog only pages the checklist. `require_tools` fails LOUD (python3, sshpass,
  the decision module readable, the threshold getter sourced) so a missing dependency can never SKIP
  every pass and go silent forever (#833).
- **Ships DISABLED.** The supervisor installs + live-verifies (mute mbc over WS → page → unmute →
  recovery) + enables the timer on dev1; this repo makes no box-side change.

## Tier-0 verification (worktree lane)

`python3 -m pytest tests/python/test_measurement_audio_decision_1310.py` (the pure decision + CLI) +
`tests/python/test_notify_dedup_key_sweep_1206.py` (the allowlist + bucketed-key sweep); `bash -n` +
`shellcheck -S warning scripts/measurement-audio-alert-watchdog.sh`; `cargo fmt --all --check` (the
`audio_preflight_default_threshold_db` harness test). A worktree-isolated lane may be blocked from a
`bash -c '…source…'` / PATH-stub dry-run (#1265) — the SUPERVISOR runs the sourced-lib dry-run driver
+ the `tests/harness_audio_presence_preflight.rs` getter test at integration/CI. The meter probe's
WS read has no local test path (needs a live OBS) — the supervisor's live-verify (mute → page →
unmute) covers it.
