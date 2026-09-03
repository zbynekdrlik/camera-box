---
paths:
  - "scripts/lib/rig-mode-state.sh"
  - "scripts/splitter-port-alert-watchdog.sh"
  - "tests/harness_rig_mode_state_1290.rs"
  - "tests/harness_splitter_port_event_gate_1290.rs"
---

# Gating a TEST-premise dev1 watchdog on rig EVENT mode (#1290)

The dev1 splitter-port watchdog (#739) paged the owner's phone 5× during a LIVE broadcast
(2026-09-03 19:20–20:45): cam4/5/6/7 had no camera connected, read grayscale, and its
sibling-anchor `DEAD_PORT` verdict fired because cam1/2/3 (real cameras) read colour. That
discriminator ("a proven-good sibling on the SAME camera+splitter proves the shared camera is
delivering, so a grey box is a dead port") is a **TEST-rig premise** — the rig feeds ONE camera
through an HDMI splitter to EVERY cambox. In **EVENT/production** each cambox has its OWN real
camera, so a camera-less cambox is legitimately black and the sibling anchor says nothing about it.

## The EVENT signal = the DURABLE cam2 painter state — NOT the ledger, NOT the #281 heartbeat

`rig-mode.sh event` STOPS+DISABLES `cam2-painter.service` (#892) and removes `/run/rig-painter.pid`;
`rig-mode.sh test` enable-`--now`s it (its steady state since #1008/#937 is the ENABLED service).
So **painter_expected = pidfile present OR `cam2-painter.service` is-enabled** is the durable,
non-staling TEST/EVENT discriminator — and `scripts/lib/optical-chain-health.sh` ALREADY owns it
(`optical_chain_painter_expected_from_snapshot` + `optical_chain_painter_probe_remote_snippet`,
`.claude/rules/optical-chain-health-watchdog.md`). The new `scripts/lib/rig-mode-state.sh`
**self-sources** it (ONE definition of painter_expected, ONE cam2 probe snippet) and adds a
`RIG_MODE_PROBE_OK` reachability sentinel → a **3-state** `rig_mode_from_painter_snapshot`:

- **UNKNOWN** — no sentinel (cam2 ssh failed / empty / partial). The mode is UNREADABLE → the
  caller behaves EXACTLY as today. Fail-safe: an unreadable mode must NEVER silence a real
  TEST-mode fault, and must NEVER be read as EVENT.
- **TEST** — reachable AND painter_expected.
- **EVENT** — reachable AND NOT painter_expected.

**Rejected alternatives (do not switch to them):** the rig-test ledger (`rig_test_ledger_*`,
`/run/camera-box-rig-tests.jsonl`) tracks WHAT a harness STARTED (a kill-by-PID cleanup target),
not the MODE — it misreads an idle-but-in-TEST rig, a partially-cleaned ledger, or a rebooted TEST
rig (tmpfs cleared, painter respawned). The **#281 rig-heartbeat** (`rig_heartbeat_active`) is
stale-after-10-min BY DESIGN — an idle-but-in-TEST rig would misread as EVENT and silence a real
dead port (`optical-chain-health.sh` explicitly warns against it for this discriminator); the
heartbeat is the E2E-WINDOW axis (#1117), a different concern from the EVENT/production axis.

## The gate (splitter-port-alert-watchdog.sh)

`main()` computes `RIG_MODE` ONCE per pass (`rig_mode_probe` = one cam2 ssh, same shape as
`probe_box`, overridable wholesale in tests) and passes it to `handle_box`. In **EVENT** mode
`handle_box` logs each box's would-be verdict report-only
(`<box> <verdict> skip: rig in EVENT mode — TEST-premise verdict, no page (#1290)`) + clears the
per-box confirm/throttle and returns before the DEAD_PORT confirm/notify path. TEST/UNKNOWN are
byte-unchanged. No systemd change (one extra cam2 ssh is well inside the unit's 120s budget); no
python mirror (the gate lives entirely in the bash orchestrator).

## Per-watchdog TEST-premise audit — only splitter-port needed the gate

| Watchdog | TEST-premise? | EVENT gate | Why |
|---|---|---|---|
| **splitter-port** (#739) | YES | **ADDED** | Sibling-anchor DEAD_PORT assumes one camera via the splitter to every cambox; false in EVENT. |
| **optical-chain** (#860) | already handled | none (verified) | Already gates on the SAME painter_expected signal — EVENT (painter disabled) → `skip`, never pages. |
| **grabber-stuck** (#1128) | NO | none | Pages on a specific over-rate (~62.5 fps) + persistent-corrupted HARDWARE signature a camera-less cambox never produces; a stuck grabber on-air is a genuine fault in both modes. |
| **cadence** (#794) | NO | none | Pages on a source delivering non-60 cadence (genlock/timing fault); a camera-less cambox at 60-fps-black reads OK, a frozen source is UNKNOWN (deferred #1052), a real 50-fps camera on-air IS a fault. |
| **frozen-input** (#1052/#1069) | NO | none | Pages on `received=` NOT advancing (wedged receiver / dead feed); designed to work in BOTH modes, scope self-filters to sources strih actually receives; a frozen production camera IS an owner-actionable fault — an EVENT gate here would MASK it. |

**The discriminator for "does a watchdog need an EVENT gate":** its paging verdict must depend on
the shared-camera-via-splitter TEST topology (splitter-port), OR the painter injection leg that
EVENT deliberately disables (optical-chain). A watchdog that pages on a genuine
hardware/timing/receiver fault equally real in production must NOT be EVENT-gated — that would
silence a real fault in the exact window it matters (the same reasoning as the #1117 gap-b audit
for E2E-window suppression). This is distinct from the E2E-WINDOW (#1117 `rig_heartbeat`) axis: a
splitter-port cam2-grey-mid-E2E edge (transient painter between states, cam2-painter stopped but
still enabled → classified TEST) is a possible follow-up (mirror optical-chain's rig_busy veto),
deliberately out of #1290's EVENT-mode scope.

## Tier-0

`rig-mode-state.sh` is a pure source-only lib (no `set -e`); verify RED→GREEN by sourcing it and
calling `rig_mode_from_painter_snapshot` over the five cases, and by the driver end-to-end (source
the watchdog `--dry-run`, override `probe_box`/`sshpass`/`rig_mode_probe`, run `main` twice). A
worktree worker may find `bash -c '…source…'` blocked (#1265) — the SUPERVISOR runs the Rust
harnesses (`tests/harness_rig_mode_state_1290.rs`, `tests/harness_splitter_port_event_gate_1290.rs`)
at CI; local net is `bash -n` + `shellcheck -S warning` + `cargo fmt --all --check`.
