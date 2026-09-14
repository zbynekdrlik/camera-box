---
paths:
  - "scripts/rig-dev-handover-check.sh"
  - "scripts/rig_dev_handover_decision.py"
  - "tests/python/test_rig_dev_handover_1312.py"
---

# "development" handover check (#1312)

`scripts/rig-dev-handover-check.sh` is the ONE dev1 command the supervisor runs the moment the
owner writes „development" (any wording) after a production — it verifies the whole rig and prints
ONE Slovak checklist of what the owner forgot to switch back into development state, ending with a
`zabudol si: …` line the supervisor pastes verbatim to the owner. Owner directive 14.9.2026.

## Architecture — thin orchestrator + pure engine (do not merge them)

- **`scripts/rig-dev-handover-check.sh`** is I/O ONLY. Per item it runs an EXISTING read-only probe
  (never a new probe), bounded with `timeout` and drain-safe, and writes the probe's merged
  stdout+stderr to `$WORKDIR/<name>.out` and its exit code to `<name>.rc`. It **NEVER mutates the
  rig** — no `--fix` (a `--fix` mode is a followup, not this script).
- **`scripts/rig_dev_handover_decision.py`** is the PURE engine: it parses the captures
  (watchdog `verdict=`/`-> REACHABLE` lines, exit codes, the rig-mode bare token), maps every item
  to `OK` / `FORGOT-BY-OWNER` / `FIXED-BY-ME` / `UNKNOWN` + a Slovak line, and decides the exit
  code (0 all-OK; 1 any FORGOT; 2 UNKNOWN-only). **ALL logic + ALL tests live here** — Tier-0 #557:
  no cargo, `tests/python/test_rig_dev_handover_1312.py` feeds captured dry-run fixtures to the
  pure functions. Verify a change with `python3 -m pytest tests/python/test_rig_dev_handover_1312.py`;
  verify the bash with `bash -n` + `shellcheck -S warning` (the orchestrator has no cargo/live
  path in a worktree — it runs at CI / on the live rig).

## Items and the probe each one REUSES (never re-derive a probe)

| item | probe (read-only) | good → OK | forgot → FORGOT | else → UNKNOWN |
|---|---|---|---|---|
| mic | `measurement-audio-alert-watchdog.sh --dry-run` | `verdict=PRESENT` | `verdict=SILENT` (owner: odmutuj `mbc` v Abletone) | SKIP/UNKNOWN/cam2-down |
| mode | `lib/rig-mode-state.sh` (cam2 painter probe) | `TEST` | `EVENT` (supervisor: `rig-mode.sh test`) | cam2 down |
| painter | `optical-chain-alert-watchdog.sh --dry-run` | `verdict=healthy`/`log-only:*` | `verdict=alert:*` | `nothing to decide` |
| burns | `obs_burn_filter.py sweep-check` (strih+stream) | exit 1 (≥1 burn ON) | exit 0 (no burns) | exit 2 |
| mapping | `set-ndi-mapping.py --verify-only` (strih) | exit 0 | exit 1 | exit 2 |
| pins | `latency_pins_verify.py` (strih/stream/imag) | exit 0 | exit 1 | exit 2 |
| clock | `dantesync-clock-alert-watchdog.sh --dry-run` | `verdict=OK` | NO_CLOCK/NO_DANTESYNC/MGMT_DEAD | SKIP/UNKNOWN |
| obs | `obs-liveness-watchdog.sh --dry-run` | `verdict=HEALTHY` | FPS-ZERO/WEDGED-.../WS-DEAD/... | no verdict |
| net | `network-reach-alert-watchdog.sh --dry-run` | `-> REACHABLE` | `-> UNREACHABLE` | no line |
| audiolag | `audio-lag-alert-watchdog.sh --dry-run` | `verdict=HEALTHY` | LAGGING/STALE/DRIFTING | SKIP/UNKNOWN |
| genlock | `genlock-lock-alert-watchdog.sh --dry-run` | `verdict=HEALTHY` | DEGRADED/UNLOCKED | **UNKNOWN facet = UNKNOWN, NEVER forgot** |
| dantesync | `dantesync-version-gate.sh` | exit 0 | exit 20 (drift) | exit 11 |
| cambox | `camera-box-version-gate.sh` | exit 0 | exit 20 (drift) | exit 11 |

## Gotchas / invariants

- **A DOWN box (cam2 is the standing one) must fail safe to UNKNOWN, never a hang, never a false
  OK.** Every probe is bounded with `timeout`; the mode/painter/mic items touch cam2 and read
  UNKNOWN when it is down. UNKNOWN is a non-zero exit (the check is not "clean"), but it is NEVER
  reported as FORGOT — only a probe that RAN and returned a bad verdict is FORGOT.
- **`scenes` + `Studio Mode` have no independent read-only probe** and are covered transitively by
  the `mode` item: `rig-mode.sh test` (the action the `mode=EVENT→FORGOT` line names) restores
  scenes, Studio Mode, burns AND painter in one shot. A dedicated read-only scene/studio probe is a
  followup, not this lane.
- **Reuse, never re-derive.** Every item runs an EXISTING probe. When a probe's dry-run verdict
  vocabulary changes, update the `good`/`forgot` token sets in `ITEMS` (rig_dev_handover_decision.py)
  and the fixtures in the test — do NOT reimplement the probe's grading here.
- The `#1310`/`#1307`/`#1299` watchdogs may be absent on an older base — a MISSING probe script is
  the RC_MISSING sentinel → UNKNOWN, so the orchestrator is forward-compatible (it picks a probe up
  the moment it exists).
