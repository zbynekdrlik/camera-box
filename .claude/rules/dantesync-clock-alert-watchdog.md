---
paths:
  - "scripts/dantesync-clock-alert-watchdog.sh"
  - "scripts/dantesync_clock_decision.py"
  - "systemd/dantesync-clock-alert-watchdog.*"
  - "tests/python/test_dantesync_clock_decision_1307.py"
---

# dev1 dante-clock alert watchdog (#1307) — headless-fleet clock-loss paging

The dev1-side watchdog that closes the 2026-09-13 silent fleet-clock failure (the Yamaha AIC128-D
grandmaster's DHCP lease moved off `10.77.9.184`, every node's `gm_allowlist` stopped matching, the
whole fleet ran NTP-only for hours and nobody noticed until the release E2E gate failed). The
camboxes cam1–7 are HEADLESS — their only path to a human is dev1 → Discord. First member of the
production-critical watchdog class (umbrella **#1308**).

## Architecture (mirrors the fleet-watchdog family — #1299/#1226/#1001)

- `scripts/dantesync_clock_decision.py` — the PURE kernel (no I/O, pytest Tier-0, the #1199
  python-mirror pattern). `analyze(status_json, box_reachable, grandmaster_ip)` →
  OK / NO_CLOCK / SKIP(unreachable) / UNKNOWN(unparseable); `dedup_key(base, now, interval)` → the
  time-bucketed key; `grandmaster_change(prev, cur)` → GM move detection. Everything the shell needs
  to DECIDE lives here so the grading + cadence are unit-tested.
- `scripts/dantesync-clock-alert-watchdog.sh` — I/O only: curl `:8898/status`, `getent` (via
  `rig-grandmaster.sh`), state-dir persistence, `airuleset notify`. Reuses `obs-watchdog-decision.sh`
  (2-pass confirm), `obs-fleet.sh` (`obs_fleet_is_home` resolume home-gate + `obs_fleet_host`),
  `camera-set.sh` (`camera_resolve` cam IPs), `rig-grandmaster.sh` (`rig_grandmaster_ip`,
  DNS `video-clock.lan` → grandmaster IP, fail-loud).

## Gotchas / invariants (do not regress)

- **The #1119 storm signal is dantesync's OWN `ntp_step_storm` boolean (its 120/h alarm), NOT a
  camera-box numeric literal.** `dantesync-gate.sh` has no numeric steps-per-hour constant to reuse —
  its storm verdict IS the boolean (`ntp_master_step_storm_verdict`). Page on the boolean; carry
  `ntp_steps_last_hour` in the reason text only. Never introduce a `> 120` literal here.
- **The grading MIRRORS `clock-offset-guard.sh`'s field semantics** (`ptp_locked_from_pipe_json`:
  `is_locked` + `mode` in {NANO,LOCK}; `gm_matches_expected`: `gm_source_ip` == the grandmaster;
  `ntp_master_step_storm_verdict`; AND `pipe_json_freshness_verdict`: `updated_ts` age). Keep the
  field NAMES identical to the `:8898/status` schema so it can never disagree with the E2E gate; a
  schema change updates both sides.
- **`updated_ts` FRESHNESS is graded (`reason=stale`), not just the instantaneous fields.** A wedged
  dantesync (HTTP thread alive, servo/`updated_ts` frozen) serving a stuck `is_locked:true` is a
  SILENT clock loss the E2E gate already fails on — so the watchdog pages it too. `DANTE_CLOCK_FRESHNESS_S`
  default 300 s (mirrors the gate's `DANTESYNC_OFFSET_FRESHNESS_S`), generous over the ~30 s
  `updated_ts` cadence. An ABSENT `updated_ts` (or `now` not passed) → freshness not graded (never a
  false stale page); only a PRESENT-and-old `updated_ts` fires. `now` is injected (`DANTE_CLOCK_NOW`)
  so the check is deterministic + unit-tested.
- **gm identity is report-first (false-page-safe).** Page `wrong_gm` ONLY on a PRESENT-and-different
  `gm_source_ip` (the #834 foreign-master case). A gm that is simply ABSENT while the node is
  otherwise locked is OK — mirrors the gate's `DANTESYNC_GATE_GM_ENFORCE=0` default; a genuinely lost
  clock is `is_locked=false`, never gm-absent-but-locked.
- **Production-critical TIME-BUCKETED re-ping** (owner ruling ROZHODNUTÉ #1307): the `--dedup-key` is
  `dante-clock-<box>-<floor(now/REPING_INTERVAL_S)>` — within a bucket an identical state edits the
  card, every new bucket re-pings. This is the DELIBERATE exception to
  `.claude/rules/watchdog-notify-dedup.md`'s one-ping-per-incident rule, allowlisted in
  `tests/python/test_notify_dedup_key_sweep_1206.py`. Recovery stays ONE machine-channel log line.
- **`CLOCK_ALARM_FIELD` is a single constant** (dantesync#114 forward-compat): `clock_alarm.active` is
  authoritative for NO_CLOCK when present; the derived `is_locked`/gm/storm check folds in as the
  cross-check (page if EITHER fires). Absent field → derived check only.
- **Node roster = the WHOLE powered dantesync fleet**, cam1–7 (incl. cam5–7 retired from
  `CAMERA_ACTIVE_SET` but still powered + running dantesync) + strih/stream/imag/resolume. resolume is
  traveling → paged only while `obs_fleet_is_home`. An OFF box → UNREACHABLE → SKIP (defers to #1001),
  never a page.
- **Ships DISABLED.** The supervisor installs + live-verifies + enables the timer on dev1; this repo
  makes no box-side change. Tier-0: pytest on the pure module + a stubbed `--dry-run` (fetch seam);
  no cargo (#557).

## Tier-0 verification (worktree lane)

`python3 -m pytest tests/python/test_dantesync_clock_decision_1307.py`; `bash -n` +
`shellcheck -S warning scripts/dantesync-clock-alert-watchdog.sh`; a stubbed `--dry-run` via
`DANTE_CLOCK_FETCH_CMD` + `RIG_GRANDMASTER_IP` + `DANTE_CLOCK_NODES` + `DANTE_CLOCK_NOW` (inline
literal env prefixes — a worktree-isolated lane cannot run `bash -c`/variable-value shapes,
`.claude/rules/ci-testing-gotchas.md`).
