---
paths:
  - "scripts/lib/camera-box-deadman.sh"
  - "scripts/lib/camera-box-free-device.sh"
  - "scripts/lib/cam2-painter-deadman.sh"
  - "tests/harness_camera_box_deadman_772.rs"
  - "tests/harness_camera_box_free_device_772.rs"
  - "tests/harness_recording_e2e_cam2_painter_deadman_872.rs"
  - "tests/harness_cam2_painter_deadman_run_aware_1246.rs"
---

# On-box dead-man for PRODUCTION camera-box.service + the ExecStartPre device-free bake-in (#772)

The cancel-in-progress SIGKILL problem (recording-e2e.sh restores state ONLY in cleanup(), which a
killed run never reaches) has TWO independent on-box self-heals, both dev1-independent — the same
family as cam2-painter-deadman (#872/#1072), rig-restore (#844), startup-self-heal (#878):

- `scripts/lib/camera-box-deadman.sh` — a transient systemd timer armed (`$(camera_box_deadman_arm_cmds
  "$MIN")`) before EACH of the 4 camera-box stops in recording-e2e.sh (cam1 [2/8], the [2b/8]
  ALL_CAMBOX loop, cam2 non-sweep [3/8], AV_RESTART). Restores production between runs.
- `scripts/lib/camera-box-free-device.sh` — a helper + camera-box.service.d ExecStartPre drop-in
  (baked by setup-device.sh, checked by verify-device.sh's `(y)`) so EVERY start path frees
  /dev/video first.

## The KEY design difference from cam2-painter-deadman — and why it matters for ANY new deadman here

cam2-painter's `frame-probe` is `nohup`'d with `--duration-secs` and SELF-TERMINATES, so its
dead-man fires every 5 min guarded by `pgrep -x frame-probe` (a no-op while a run owns fb0, recovers
~5 min after frame-probe self-exits). **The camera-box BURN has NO self-exit** — it is a
`systemd-run --unit=camera-box-burn-<id> --property=Restart=on-failure` unit (systemd-owned, survives
the runner SIGKILL, runs FOREVER). A process/unit-presence guard would therefore keep a
camera-box-style dead-man PERMANENTLY disarmed. So the pattern flips:

- **DELAY the first fire past the whole run window** (`--on-active=<ceil(DURATION/60)+overhead>min`,
  computed in recording-e2e.sh; `CAMERA_BOX_DEADMAN_OVERHEAD_MIN` default 20). This is the
  SAFETY-CRITICAL invariant: the dead-man can then NEVER fire during a live measurement — worst case
  is a slower recovery (~run-length + overhead), never a corrupted verdict. Recovery timing is
  equivalent to cam2-painter's (which also only recovers at ~run-end, after frame-probe's
  --duration-secs). **The overhead margin is the ONE knob the invariant hangs on — VALIDATE + FLOOR
  it** (`CAMERA_BOX_DEADMAN_OVERHEAD_FLOOR_MIN=15`, the ~12-15 min worst-case pre-record overhead by
  the script's own retry accounting): a non-integer / negative / too-small env override is CLAMPED up
  to the floor, never trusted — a smaller margin silently moves the first fire INTO the recording.
- **Re-fire periodically** (`--on-unit-active=5min`) so a run killed AFTER the first fire still
  recovers.
- **SELF-DISARM once camera-box is confirmed active** (a FAILED start leaves it armed to retry) —
  so NO cleanup() disarm wiring is needed (a normal run's cleanup restores camera-box; the delayed
  first fire then finds it active and self-disarms). Idempotent re-arm clears any prior timer.
- The action **STOPS the stray burn UNIT** (`systemctl stop camera-box-burn-*`), never just `pkill`
  — a bare pkill trips the unit's `Restart=on-failure` and it respawns (the #894 device-steal
  fight). Then `pkill -9 -x camera-box-burn` (exact 15-char comm; `camera-box-burn` is exactly 15
  chars, distinct from the 10-char production `camera-box` — never `pkill -f`, the self-match
  footgun).
- It **NEVER touches frame-probe** — that is the cam2 fb0 painter (a DIFFERENT device); the
  ExecStartPre helper likewise must never reference frame-probe (verify-device's `(y)` parser
  rejects a helper that does). The rule of thumb: `camera-box`/`camera-box-burn` own /dev/video;
  `frame-probe` owns /dev/fb0 — a device-free for one must never touch the other's process.

Two more review-hardening lessons (adversarial review, 2026-08-17):

- **Do NOT add `camera-box-deadman*` to event-assert.sh's EVENT-mode stray-unit glob** (the way
  cam2-painter-deadman was in #1075). The painter's timer, still armed in EVENT, could RESURRECT the
  painter (harmful) — so it must be flagged/disarmed. The camera-box dead-man SELF-DISARMS when
  camera-box is active (which it always is in EVENT mode), so a pending timer is harmless there; the
  glob widening only manufactured a false EVENT-contract failure for ~20-50 min after every run,
  with no disarm path. The self-disarm is the reason it needs neither the glob nor a rig-mode disarm.
- **A time-delayed dead-man is at odds with an UNBOUNDED operator wait.** `AV_RESTART_GATE` /#109
  restart-survival modes block on an operator confirm between the before/after measurements, while
  cam1's timer keeps counting from its [2/8] arm and is the AV video source. RE-ARM cam1 at the top
  of `av_restart_record_and_emit_plan` (idempotent, best-effort ssh to `$CAM1_IP`) so its first fire
  resets past THIS call's record + the wait — otherwise a slow operator lets the timer fire
  mid-"after" measurement and kill cam1's burn (fail-closed: a wasted opt-in run, never a false PASS).

**Rule for a future on-box deadman: if the thing being stopped is replaced by something that
SELF-TERMINATES, guard on its presence (cam2-painter). If the replacement runs FOREVER (a
Restart=on-failure systemd-run unit), you cannot guard on presence — delay the first fire past the
run window instead, and never fire during the measurement.**

## The cam2-painter-deadman presence-guard must cover EVERY on-box owner of the measured devices, not just fb0 (#1246)

The "guard on presence" side above (cam2-painter, whose `frame-probe` self-terminates) had a
SUBTLER trap than "presence guard keeps a forever-runner disarmed". Its guard checked ONLY
`pgrep -x frame-probe` — the fb0 painter — on the premise "a live run always has a frame-probe".
That premise has a GAP: in the ALL_CAMBOX `[2b/8]` path the harness stops the deployed cam2-painter
(its frame-probe exits) and starts the `camera-box-burn-cam2-<id>` capture burn MANY
seconds/minutes BEFORE it launches its own `[3/8]` `/tmp/frame-probe` painter. A periodic deadman
fire in that no-frame-probe window passed the guard and ran `systemctl start cam2-painter` — whose
`Wants=camera-box.service` pulls production camera-box, whose `ExecStartPre=camera-box-free-capture-device.sh`
stops every `camera-box-burn-*` unit — KILLING the live burn mid-measurement (live 2026-08-31 run
1635844760: burn Started 19:01:02.787, deadman fired 19:01:49.544 in the gap, burn Stopped
19:01:49.624; the `[7b/8]` run-integrity check correctly failed an otherwise-green verdict).

**The fix, and the generalized rule: a presence-guard must key on EVERY on-box owner of the
device(s) the deadman's start would seize — not just the one the deadman itself paints.** cam2's
deadman now ALSO no-ops when a live capture burn owns `/dev/video`:
`pgrep -x camera-box-burn` (the exact 15-char comm `camera-box-free-capture-device.sh` itself
keys on via `pkill -9 -x camera-box-burn`) OR an active/activating `camera-box-burn-*` systemd unit
— blip-robust against the burn's `Restart=on-failure` auto-restart. The `frame-probe` guard stays
(fb0 owner).

**The unit check MUST use a unit-NAME pattern argument, never a DESCRIPTION-column grep (#1246
review 🔴, empirically reproduced).** The first cut wrote
`systemctl list-units --state=active,activating --plain --no-legend --type=service | grep -q camera-box-burn`
and it SELF-MATCHED: `systemd-run` with no `--description=` sets the transient unit's Description to
its own command line, `list-units --plain --no-legend` prints the DESCRIPTION column (unellipsized
when piped), and while the action runs `cam2-painter-deadman.service` is itself in the
active/activating set with a description containing `camera-box-burn` (from the `pgrep`/`grep`
tokens) — so the guard matched its OWN unit on every fire, `exit 0` before the start, permanently
disarming the deadman (the exact #872 dark-monitor failure, made permanent). The fix is the SAME
idiom the #772 `camera-box-deadman.sh` already uses: a unit-NAME **pattern argument**, which matches
NAMES only, never descriptions —
`systemctl list-units --state=active,activating --plain --no-legend --type=service "camera-box-burn-*" 2>/dev/null | grep -q .`
(double quotes are legal inside the single-quoted `/bin/bash -c '...'` action) — plus
`--description=cam2-painter-deadman` on the `systemd-run` arm as belt-and-braces. General lesson for
ANY on-box guard that greps `systemctl list-units`: filter by the unit-NAME glob argument, never
grep the free-text description column, or the guard's own transient unit (and its command line) can
satisfy it.

- **The #281 rig-active heartbeat is NOT usable here.** It is written on DEV1
  (`$XDG_RUNTIME_DIR/camera-box-rig-active`, `scripts/lib/rig-heartbeat.sh`) and read by the dev1
  rig-restore watchdog; the cam2-painter-deadman is a transient systemd unit ON cam2, deliberately
  dev1-independent, and cannot read a dev1 file. The burn's on-box presence IS the on-box
  equivalent of "a measurement claims the device right now".
- **TRADE-OFF (accepted):** on a SIGKILLed run the stray burn persists (systemd-owned,
  `Restart=on-failure`), so with the burn guard the cam2-painter-deadman DEFERS painter recovery to
  the #772 camera-box-deadman (armed at the same sites) stopping the stray burn first — ~15 min
  later than the old frame-probe-self-exit path. Acceptable: a corrupted live verdict is far worse
  than extra monitor-dark minutes on a killed run, and cam2-painter cannot coexist with a burn
  anyway (both conflict on the device via camera-box). The two deadmen COMPOSE (do not race): #772
  clears the stray burn + restores production, then the next cam2-painter-deadman tick sees the
  device free and restores the painter.
- **Tier-0 test it FUNCTIONALLY** (`tests/harness_cam2_painter_deadman_run_aware_1246.rs`, mirroring
  `harness_camera_box_deadman_772.rs`): source the lib, extract the `/bin/bash -c '...'` action, run
  it under fake `pgrep`/`systemctl` stubs (real `grep`) — assert the action does NOT `systemctl
  start cam2-painter` when a burn PROCESS or an active burn UNIT is present, and DOES when the device
  is free. Prove RED→GREEN at the bash level first (Tier-0 blocks the cargo compile), then rely on
  CI for the actual Rust run.

## Nested-quoting: embedding a `systemd-run ... /bin/bash -c '<action>'` via `$(...)` in an ssh string

The arm is emitted from an UNQUOTED heredoc (`cat <<ARM`, so `${CAMERA_BOX_DEADMAN_UNIT}`/`${first}`
params expand at emit time on dev1), and the action is SINGLE-QUOTED at the `systemd-run ...
/bin/bash -c '...'`. Layering rules that make it survive dev1-emit → box-ssh-shell → systemd
fire-time:

- Escape every runtime `\$` in the heredoc (`\$u`, so `$u` emits literally, not expanded to empty on
  dev1). The single quotes around the action then protect `$u` from the box's ssh shell; systemd
  stores it literally; fire-time bash expands it (the `while read -r u _` loop var).
- **Avoid `awk` inside the single-quoted action** — `awk '{print $1}'` needs its own single quotes,
  which collide with the action's outer single quotes. Use `while read -r u _` (no nested quotes).
- End the arm's last statement with `;` (it ends `fi;`) — `$(...)` strips the trailing newline, so
  without the `;` the following `systemctl stop camera-box` glues on as argv (the #744/#746 class).
- The `echo "..."` double quotes inside the arm output are for the BOX's shell, not dev1's outer
  `"..."` ssh string — command-substitution output is inserted literally, quotes and all, and never
  re-expanded. This is why the whole thing works even though it looks like triple-nested quoting.

Test it FUNCTIONALLY, not just `bash -n`: extract the `/bin/bash -c '...'` action and re-exec it
under fake `systemctl`/`pkill` stubs on `$PATH` (the "fake the remote, not the ssh" pattern from
`harness_udev_camera_box_894.rs`), asserting the exact calls (stop unit, pkill, start-if-inactive,
self-disarm, NO frame-probe) — see `harness_camera_box_deadman_772.rs::run_action_call_log`.

## Local verification reality (camera-box #477)

`cargo test` cannot RUN locally (the `# airuleset:build-ok` bypass is DISABLED for this repo). To
check the anchor-collision class after editing recording-e2e.sh, `cargo test --no-run` (compiles all
binaries, Tier-0 allowed) then EXECUTE the built binaries directly from `target/debug/deps/`
(running an ELF is not a `cargo` invocation, so the #477 block does not apply) — the static-anchor
tests read the scripts at RUNTIME, so a directly-run binary reflects the current script text.
