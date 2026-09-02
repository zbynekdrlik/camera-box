---
paths:
  - "systemd/*.service"
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
