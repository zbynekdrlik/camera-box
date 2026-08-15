---
paths:
  - "scripts/rig-mode.sh"
  - "scripts/lib/cam2-painter-handoff.sh"
  - "scripts/lib/cam2-painter-restore-verify.sh"
  - "scripts/lib/cam2-painter-restore-retry.sh"
  - "scripts/lib/cam2-painter-deadman.sh"
  - "systemd/cam2-painter.service"
  - "tests/harness_cam2_painter_steady_state_handoff.rs"
  - "tests/harness_cam2_painter_coordination.rs"
---

# cam2 painter lifecycle — WHO paints /dev/fb0 (+ emits the QPSK marker) in each state (#1008/#937)

Two painters exist on cam2, and getting "who is painting now" wrong is how the rig goes silently
dark. Both are `frame-probe --paint-only --dual-qr` writing `/dev/fb0` (KMS/DRM in practice) and,
since #984, emitting the QPSK marker default-ON.

- **PERMANENT `cam2-painter.service`** (#863) — the DURABLE steady-state painter. `Restart=always`,
  `WantedBy=multi-user.target`; when `enabled` it survives reboot; `--marker-log
  /run/rig-qpsk-markers.csv` in its ExecStart (#1008) so it writes the SAME growing marker CSV the
  offline verdict + the "must-stay-alive" liveness check read. **This is what the standing
  rig-test-mode-must-stay-alive rule requires** — supervised, self-healing, boot-persistent.
- **TRANSIENT painter** launched by `rig-mode.sh test`'s `painter_launch_remote` — a `nohup
  frame-probe --duration-secs N` used ONLY for the at-mode-set CHAIN VERIFICATION window
  (freshly-resolved marker device #725, marker-log growth #431, optical non-black #901). It is
  unsupervised and MUST NOT be left as steady state (it was — the #1008/#937 bug: a 2h nohup that
  expired silently).

## The lifecycle (do not break this ordering)

- **`rig-mode.sh test`**: (1) `painter_launch_remote` STOPS the permanent unit first (#440 — two
  painters racing fb0 make the displayed QR alternate run_ids, desyncing the marker), launches the
  transient painter, verifies the whole chain. (2) At the END, `do_test` calls
  `cam2_painter_steady_state_handoff_cmds` (`scripts/lib/cam2-painter-handoff.sh`): stop the
  transient via its pidfile, `systemctl enable --now cam2-painter.service`, FAIL LOUD unless it is
  active + genuinely painting (presenter-aware #464) + marker CSV growing. **Steady state ends on
  the PERMANENT unit, never the nohup.**
- **`rig-mode.sh event`**: `painter_stop_remote` STOPS **and DISABLES** the permanent unit (#892 —
  EVENT must never leave a QR that can return via a restart or a reboot onto the LIVE broadcast).
  So `test` must `enable --now` (not just `start`) to re-arm it after any prior EVENT cycle.
- **`recording-e2e.sh` measurement**: STOPS the permanent unit (arms the #872 on-box dead-man so a
  SIGKILLed run self-heals), runs its OWN measurement painter, and `cleanup()` restarts +
  `cam2_painter_restore_verify_cmds` + disarms the dead-man. This is now the ONLY time the unit
  yields fb0. The handoff above composes with it cleanly.
- **dev1-side `optical-chain-alert-watchdog.sh`** (#860) pages when a painter is EXPECTED (pidfile
  present OR `cam2-painter.service` enabled) but strih's program reads black — so an enabled unit
  makes `painter_expected` correctly true in TEST mode.

## Gotchas

- The handoff builder embeds `$(audio_marker_emission_check_cmds ...)` inside its heredoc — mind
  the #744/#746 trailing-newline-strip rule (keep a literal line after the `$(...)`).
- Adding logic to `do_test` uses the #675 sourced-lib pattern (a new `_cmds` builder + one
  `cam_ssh "$(...)"` line) so no `tests/rig_mode.rs` static anchor is touched. Always re-run the
  FULL `cargo test` suite after any `rig-mode.sh` edit (anchor-collision class).
- `--marker-log` may be added to the base unit ExecStart WITHOUT tripping the provisioning test's
  `!out.contains("--audio-marker")` assertion (marker-log ≠ audio-marker; the marker stays
  default-ON, flag-free).

## Recovering a dead standing painter after a run (#1072)

Three composed recovery seams keep the standing painter from staying dark after an E2E run — the
combination unifies worst-case recovery to ~5 min on BOTH the clean-cleanup and SIGKILL paths:

- **cleanup restore is a bounded RETRY, not one-shot** (`scripts/lib/cam2-painter-restore-retry.sh`,
  `cam2_painter_restore_retry_cmds`). It runs AFTER the anchored `systemctl start cam2-painter
  2>/dev/null || true` + adjacent `$(cam2_painter_restore_verify_cmds)` (those two lines are pinned
  by #863/#872/#312/#713/coordination — never edit them; the retry is a #675 sourced-lib appended
  via ONE `$(...)` line). It sets the remote var `_cprr_ok`; cleanup wraps
  `$(cam2_painter_deadman_disarm_cmds)` in `if [ -n "$_cprr_ok" ]` so a FAILED restore LEAVES the
  dead-man armed. **Budget gotcha:** the whole cam2 cleanup ssh is wrapped in
  `timeout "$CLEANUP_SSH_TIMEOUT"` (30s) — keep the retry bounded (CAM2_PAINTER_RESTORE_RETRIES=3,
  ~3s poll each; the common already-active case adds ~0s). If a pure-failure retry does overrun the
  timeout, the SIGKILL is a SAFE fail: the `if [ -n "$_cprr_ok" ]` never runs, so the dead-man stays
  armed and self-heals — never "fix" this by extending CLEANUP_SSH_TIMEOUT.
- **the #872 dead-man is now PERIODIC** (`--on-active` + `--on-unit-active`, window 5 min, not a
  one-shot 90 min). Mid-run safety is the `pgrep -x frame-probe` guard (every fire during a live run
  is a no-op — frame-probe is armed+launched within ~25s in the SAME `_cam2_prep` ssh command and
  runs the whole run via `--duration-secs`), NOT a long delay. A short one-shot would be WRONG here
  (fires once, cannot recover a run killed after it fired) — periodic is what makes the short window
  safe. Residual: `--av-sync` mode's brief kill→relaunch frame-probe gap is a low-probability
  measurement artifact only, never a dark rig.
- **the [0/8] optical-chain preflight self-heals ONCE** (`scripts/lib/optical-chain-preflight.sh`):
  when a standing painter is EXPECTED but DEAD after the grace re-probe, it does EXACTLY ONE
  `systemctl start $service` over ssh + re-probe before the existing `exit 1` fail-closed backstop.
  No retry loop — the periodic on-box dead-man is the net; this is one fast recovery so a
  previous run's leftover-dead painter does not waste the whole gate run.
