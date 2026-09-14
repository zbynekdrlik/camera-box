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
  `CAMERA_ACTIVE_SET` but still powered + running dantesync) + strih/stream/imag/resolume + **dev1
  itself** (the `local` node, #1313). resolume is traveling → paged only while `obs_fleet_is_home`. An
  OFF box → UNREACHABLE → SKIP (defers to #1001), never a page.
- **dev1 is the `local` node — the control box watches its OWN clock (#1313, the former dev1 blind
  spot, now CLOSED).** dev1 is NOT a probed cam/obs node, yet it runs dantesync and its clock feeds
  every dev1-hosted gate (`clock-offset-painter-gate.sh`, the recording-verdict wall references, every
  `date`-stamped E2E window). On 14.9.2026 dev1 sat NTP-only for ~a day unpaged (its `gm_allowlist` on
  the retired literal `10.77.9.184` + dantesync 1.8.53 — the 13.9. fleet allowlist patch AND the fleet
  roll both skipped dev1), its own journal shouting `[CLOCK-ALARM] NO DANTE CLOCK` every minute, and
  the dev1 watchdog that would have paged it runs ON dev1 and never looked at `127.0.0.1:8898`. Caught
  only because the E2E `[0/8]` version-parity gate lists dev1. The `local` arm closes it:
  - **`DANTE_CLOCK_LOCAL_NODES="dev1"`** (+ `DANTE_CLOCK_LOCAL_IP`, the loopback, Tier-0-overridable)
    builds a `dev1|127.0.0.1|local` roster triple, probed at `127.0.0.1:8898` with **NO ssh / TCP
    reach probe** — the box is UP by definition (the watchdog runs ON it).
  - Graded by the SAME verdicts via a pure **`analyze_local()`** (the ONE tested local-node policy
    point) / the DECIDE `--local 1` flag: it forces **`box_up=1`** (a dead `:8898` on the local box is
    **`NO_DANTESYNC`**, never SKIP — there is no "box down, defer #1001" case for the box we run on)
    and **`mgmt_ssh_ok=None`** (no ssh MANAGEMENT axis — we ARE the box — so `MGMT_DEAD` can never fire
    for dev1). Everything else (OK / NO_CLOCK / UNKNOWN / gm / storm / stale / version) is the SAME
    generic grading, so the local node can never disagree with a remote node about a lost clock.
  - The clock fault keeps its bucketed key **`dante-clock-dev1-<bucket>`** (the `NO_DANTESYNC` fault
    uses `dante-clock-nohttp-dev1-<bucket>`); version drift vs `DANTESYNC_VERSION_PIN` is REPORTED in
    the card, never a page. Recovery stays a machine-channel log line.
  - **Every fleet roll AND every config patch (`gm_allowlist` / `phase_slew`) MUST include dev1** —
    `dantesync-fleet-upgrade.sh … --local dev1`, "fleet N/N" counts dev1 (the `.claude/skills/ops`
    DanteSync rollout checklist). This watchdog is the between-rolls backstop, not a substitute for
    including dev1 in the roll itself.
- **NO_DANTESYNC — the `:8898`-down-but-box-up branch (#1308, the former blind spot, now CLOSED).**
  A box that is UP but whose `:8898` (dantesync HTTP) is dead used to read UNREACHABLE → SKIP, and
  `network-reach` (#1001) probes ping/`:4455`/`:8899`, NOT `:8898`, so a `:8898`-specific outage on a
  live box was paged by neither. Now, when `:8898` is unreachable, the orchestrator probes box
  UP-ness (a cheap TCP connect via `scripts/lib/watchdog-tcp-probe.sh` — the shared extracted form of
  network-reach's `probe_tcp`, reused not reinvented; cams `ssh :22`, OBS boxes `:4455`/`:8899`/`:22`
  up-iff-any, network-reach's REACHABLE-iff-ANY rule) and passes `--box-up` to `analyze`:
  - `box_up == 1` → **`NO_DANTESYNC`** (`reason=no_dantesync_http`) — the daemon crashed/wedged on a
    live box, a production-critical page with its OWN bucketed key (`dante-clock-nohttp-<box>`), 2-pass
    confirm. Cure: restart dantesync on that box.
  - `box_up == 0` (no probed port answered) OR `None` (up-ness unprobed / probe errored) → **SKIP**,
    exactly as before (defer #1001). false-page-safe: only a PROVEN-up box pages `NO_DANTESYNC`.
  The `NO_DANTESYNC` and `NO_CLOCK` faults use SEPARATE confirm latches (`http_<box>` vs `node_<box>`)
  that CLEAR each other on recovery (an `OK` pass clears both; a `NO_CLOCK` pass clears the http latch;
  a `NO_DANTESYNC` pass clears the clock latch) so a fault-type transition never leaks a stale latch.
- **Version reporting is REPORT-ONLY, never a page (#1308).** When `:8898` answers, `analyze` compares
  the daemon's `version` field against the ONE pin (`DANTESYNC_VERSION_PIN`, resolved from
  `scripts/dantesync-version-gate.sh` in a SUBSHELL so its `set -e` never leaks — the
  early-gate-pin-doctrine / dantesync-version-reading.md single pin). A mismatch surfaces as
  `version=<x> (pin <y>)` appended to the OK-log / NO_CLOCK-alert text; it NEVER changes the verdict
  and NEVER pages on its own (a stale-but-locked node still has the clock). An ABSENT `version` field,
  or an unresolvable pin, is SILENT. Seams: `DANTE_CLOCK_VERSION_PIN` (Tier-0 pin override),
  `DANTE_CLOCK_BOX_UP_CMD` (Tier-0 box-up probe stub, mirrors `DANTE_CLOCK_FETCH_CMD`),
  `DANTE_CLOCK_TCP_TIMEOUT`.
- **Ships DISABLED.** The supervisor installs + live-verifies + enables the timer on dev1; this repo
  makes no box-side change. Tier-0: pytest on the pure module + a stubbed `--dry-run` (fetch seam);
  no cargo (#557).

## Tier-0 verification (worktree lane)

`python3 -m pytest tests/python/test_dantesync_clock_decision_1307.py
tests/python/test_dantesync_clock_dev1_local_1313.py`; `bash -n` +
`shellcheck -S warning scripts/dantesync-clock-alert-watchdog.sh`; a stubbed `--dry-run` via
`DANTE_CLOCK_FETCH_CMD` + `RIG_GRANDMASTER_IP` + `DANTE_CLOCK_NODES` + `DANTE_CLOCK_NOW`. The #1313
dev1-`local` arm is driven end-to-end from `test_dantesync_clock_dev1_local_1313.py` (a pytest that
subprocesses the watchdog `--dry-run` with `DANTE_CLOCK_LOCAL_NODES="dev1"` + a fetch stub — the
subprocess runs INSIDE the python process, so the worktree-isolation guard never sees a `bash -c`;
a lane cannot run the same stub as a bare `bash -c`/inline-env shape, `.claude/rules/ci-testing-gotchas.md`).
