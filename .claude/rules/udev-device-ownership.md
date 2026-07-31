---
paths:
  - "scripts/lib/udev-camera-box.sh"
  - "scripts/setup-device.sh"
  - "scripts/verify-device.sh"
  - "scripts/create-usb-linux.sh"
---

# udev device ownership during an E2E burn run (#894)

## Who owns `/dev/videoN` during a recording

`recording-e2e.sh` deliberately STOPS production `camera-box.service` at `[2/8]` and runs its own
probe-featured `camera-box-burn-<RUN_ID>.service` instead — that service is what emits the E2E
capture burn. For the whole recording window, the BURN unit owns the capture device, not
production. Any code path that reacts to a `video4linux` hot-plug event MUST know this and never
blindly hand the device back to production while a burn unit is active — that is exactly the
`77/NOPERM` device-steal race #894 fixed.

## The rule: a udev `video4linux` "add" handler must be BURN-GATED, never unconditional

The fleet's rule (`/etc/udev/rules.d/99-camera-box.rules`) used to be:

```
ACTION=="add", SUBSYSTEM=="video4linux", RUN+="/bin/systemctl restart camera-box.service"
```

Any benign USB re-enumeration (a self-heal reset, a cable bounce, a hub power-cycle) fired this
UNCONDITIONALLY, restarting production mid-recording and stealing the device from the burn unit —
misreported downstream as `frozen_leg` (a camera fault), not a run-integrity artifact. The fixed
rule (`scripts/lib/udev-camera-box.sh`) instead `RUN+=` a small helper
(`/usr/local/bin/camera-box-udev-video-add.sh`) that does TWO things, in order:

1. **Unconditionally** re-asserts USB autosuspend `power/control=on` on the firing device's own USB
   ancestor (walked from udev's `$DEVPATH`). This runs every time, burn or no burn — see the next
   section for why.
2. **Conditionally** restarts production `camera-box.service` — ONLY when no
   `camera-box-burn-*.service` unit is currently `active` (checked via `systemctl list-units
   --type=service --state=active --plain 'camera-box-burn-*'`).

**Never write a NEW udev/hotplug handler for this device class that skips the burn-check.** The
glob `camera-box-burn-*` is the single source of truth for "an E2E measurement owns the device
right now" — any future automation that reacts to the same `video4linux` add event must query it
the same way, never assume production always owns the node.

## The second defect: autosuspend drifts back to `auto` after ANY re-enumeration

USB autosuspend is disabled ONLY by a one-shot `/etc/rc.local` loop at BOOT. A device that
re-enumerates LATER (self-heal reset, cable bounce) comes back at the kernel default `auto` — the
boot-time one-shot never re-applies. Measured fleet-wide (#894): the one box that stayed `on` had
**zero** re-enumerations that day; the two that had drifted to `auto` had 5 and 1 — an amplifying
feedback loop (`autosuspend_delay_ms=2000` is enough idle time to suspend a live capture device
during the exact window between production stopping and the burn unit opening the node).

This is WHY the helper's job 1 (autosuspend re-assert) is unconditional and independent of job 2
(the burn gate) — autosuspend must be fixed on EVERY re-enumeration, burn active or not, or the
feedback loop keeps compounding.

## `verify-device.sh`'s acceptance check (w)

`verify-device.sh` asserts BOTH: the installed rule points at the guarded helper
(`udev_camera_box_rule_is_burn_gated`), AND the LIVE grabber's `power/control` currently reads `on`
(`udev_camera_box_power_control_is_on`) — a box that silently drifted back to `auto` FAILS this
check instead of degrading invisibly. N/A (not a FAIL) when the box has no capture grabber fitted
at all (cam4, #828). Per `.claude/rules/provisioning-scripts.md`, this check is inserted BEFORE
`(q)` — never after.

## Testing without a rig — deterministic, not "wait for a spontaneous reset"

`tests/harness_udev_camera_box_894.rs` sources the real lib for its pure content-generator/parser
functions, and separately re-execs the GENERATED helper script under a nested, PATH-restricted bash
with fake `systemctl`/sysfs stand-ins to prove the generated script's ACTUAL runtime behavior — not
just that it contains the right substrings (the `imag-ssh-remote-tool-preflight.md` "fake the
remote, not the ssh" pattern).

**Live rig verification uses the SAME deterministic-trigger idea, for real, on a real box** — never
wait for cam1's own characterized over-rate defect to spontaneously self-heal during a live run
(rate-limited to ~360s between attempts, and slow/non-deterministic). Instead:

1. Start a throwaway unit literally named `camera-box-burn-<marker>.service`
   (`systemd-run --unit=... --collect /bin/sleep 120`) to simulate an in-flight E2E burn.
2. Fire `udevadm trigger --action=add --subsystem-match=video4linux` — a benign, non-destructive
   re-fire of the existing rule for the already-enumerated device (no unplug, no data loss).
3. Read back: `systemctl show -p MainPID -p ActiveEnterTimestamp camera-box` UNCHANGED (production
   not restarted) while the burn unit is active; the USB ancestor's `power/control` == `on`
   regardless.
4. Stop the throwaway unit, force `power/control` back to `auto`, trigger again — this time
   `MainPID`/`ActiveEnterTimestamp` MUST change (production restarts) and `power/control` returns
   to `on`.

Comparing `MainPID`/`ActiveEnterTimestamp` before/after is a more reliable restart signal than
`systemctl is-active` alone (which reads `active` in both the restarted-and-not-restarted cases).
Confirmed live on cam2 (2026-07-31) — see issue #894's own comment thread for the full transcript.

## Kernel-signature discriminator: real unplug vs self-heal/soft-reset

When forensically reading a box's kernel journal to tell "was this a genuine physical unplug or a
software-triggered re-enumeration (self-heal, a udev re-trigger, an authorize bounce)":

- **A real unplug** shows a `USB disconnect, device number N` line, followed by re-enumeration at a
  NEW device number.
- **A self-heal / soft reset** shows NO disconnect line at all — just a same-path re-enumeration
  (`Found UVC ...`, `authorized to connect`) at the SAME USB path, no device-number change.

Use this to distinguish a real cable/hardware event from a software-triggered reset when reading
`dmesg`/`journalctl -k` around a `frozen_leg`/`self_heal_reset` incident — see
`.claude/rules/gap-metric-reconciliation.md` and `#895`'s own self-heal-attribution mechanism for
the companion fix that keeps a self-heal-caused reset from being misreported as a camera fault in
the first place.
