---
paths:
  - "scripts/lib/camera-box-deadman.sh"
  - "scripts/lib/camera-box-free-device.sh"
  - "tests/harness_camera_box_deadman_772.rs"
  - "tests/harness_camera_box_free_device_772.rs"
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
