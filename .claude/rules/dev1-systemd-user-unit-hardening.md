---
paths:
  - "systemd/*.service"
  - "systemd/*.timer"
---

# dev1 `--user` systemd units — `PrivateTmp`/`ProtectHome`-class hardening is currently INERT (#1277)

**Live-verified (2026-09-02, while building `rig-lease-server.service`):** on dev1, under a
`--user`-manager unit, systemd's namespace-isolation directives (`PrivateTmp=`, `ProtectHome=`, and
the same family) do **NOT** actually engage — systemd silently **skips** the mount-namespace setup
rather than failing the unit. Reproduced directly: a process run under a `--user` unit with
`PrivateTmp=yes` still saw the REAL host `/var/tmp` (a marker file written outside any namespace was
still visible), and `ProtectHome=read-only` still exposed `/home/newlevel/devel` unrestricted. Root
cause: dev1 runs `kernel.apparmor_restrict_unprivileged_userns=1` (systemd 255, Ubuntu 24.04) — an
UNPRIVILEGED `--user` manager cannot create the mount namespace these directives depend on, and
systemd degrades gracefully (no error, no unit failure) instead of refusing to start.

**Consequence for any dev1 `--user` unit's OWN comments/README claims:** never assert these
directives provide REAL isolation on dev1 without re-checking live at the time — a comment claiming
"PrivateTmp isolates this from X" can be simply FALSE on this box today, even though the exact same
directive genuinely WOULD isolate a `system`-level unit (root-owned, `WantedBy=multi-user.target`,
e.g. `bkshading-relay.service`/`camera-box.service`) or a future dev1 whose kernel policy changes.
Keep the directives declared (harmless, and correct if this unit is ever promoted to a system unit,
or the kernel policy is loosened) but describe them as **declared intent**, not an active guarantee,
in any accompanying comment or README — see `rig-lease-server.service`'s own
`VERIFIED-INERT ON DEV1 TODAY` comment for the worked wording.

**Two directives this does NOT apply to:** `NoNewPrivileges=` (a per-process `prctl`, not a mount
namespace — engages normally regardless of userns policy) and `StartLimitIntervalSec=`/
`StartLimitBurst=` (unit-level restart throttling, no namespacing involved at all).

**Recheck trigger:** if dev1's kernel policy or systemd version ever changes (`sysctl
kernel.apparmor_restrict_unprivileged_userns`, `systemd --version`), re-verify this live before
trusting any existing `--user` unit's hardening comments — the finding is a property of the BOX's
current configuration, not of systemd's design, and could silently start (or stop) being true.

## An in-checkout ExecStart script must be committed 100755 — and "enabled" is not "running" (issue 1381)

A dev1 unit whose `ExecStart=%h/devel/camera-box/scripts/<x>.sh` runs the script DIRECTLY needs git
mode 100755. `audio-mixer-alert-watchdog.sh` was committed 100644: from its install (28.9.2026) every
pass failed `status=203/EXEC`, and the production-critical FOH-audio alert never ran once. The timer
read `enabled` the whole time, so nobody noticed. Guarded now by
`tests/python/test_systemd_exec_scripts_executable_1381.py`, which walks every `systemd/*.service`.
A new script: `git add` it, then `git update-index --chmod=+x <path>` before committing.

**After enabling ANY dev1 watchdog, read its first pass:**
`journalctl --user -u <unit>.service --since -10min` must show the script's own `pass end` line and
`Finished`, never `status=203/EXEC` or `Failed with result`. A quick sweep over every timer's service:
`journalctl --user -u <svc> --since -3h | grep -c 'Failed with result'`.

## A `--user` timer + oneshot: three systemd 255 facts, each checked on dev1 (issue 1399)

- **Every run of a oneshot logs `Starting ...` + `Finished ...` at info level** from the user manager:
  a 30 s timer writes ~5800 such lines a day. `LogLevelMax=notice` on the SERVICE drops them (the
  directive also filters the manager's messages about the unit), and `SyslogLevel=notice` raises the
  program's own unprefixed stdout/stderr so it stays; a failed run (`Main process exited`, `Failed with
  result`) is still logged. Checked with `systemd-run --user -p Type=oneshot -p LogLevelMax=notice -p
  SyslogLevel=notice /bin/echo x`. Worked example: `systemd/strih-satellite-watch.service`.
- **`OnUnitActiveSec=` alone never fires before the service ran once**; pair it with `OnActiveSec=`. A
  user timer's default `AccuracySec` is 1 min, so a 30 s period needs `AccuracySec=1s`.
- **`systemctl show -p LastTriggerUSec --value --timestamp=unix <x>.timer` prints a LOCAL DATE, not
  `@<secs>`** (the flag formats `*Timestamp` properties, not this one). To grade a timer's last run, read
  the oneshot service's `ExecMainStartTimestamp` with `--timestamp=unix` instead: it prints `@<secs>`
  and survives after the run while the timer keeps the service loaded.
