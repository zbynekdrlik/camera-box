---
paths:
  - "scripts/lib/remote-logging.sh"
  - "scripts/dev1-remote-log-install.sh"
  - "tests/harness_remote_logging_1311.rs"
---

# cambox off-box remote logging — netconsole + systemd-journal-upload (#1311, Finding 1 step 2)

## Why this exists (and why the #1309 on-stick journal is NOT the forensic path for a stick loss)

Two cambox USB boot sticks died within 24 h (cam1 13.9., cam2 14.9.), both going *half-dead* first:
RAM-resident daemons kept running while the root fs USB stick dropped off the bus. The cambox is a
read-only-root appliance with `/var/log` on a 50 MB **tmpfs** and `rsyslog` PURGED (#762), so the
only durable log is the #1309 on-**stick** persistent-journal partition — which lives on the very
stick that drops, so it is unreadable at exactly the moment it matters. **The #1309 persistent
journal stays as shipped, but it is NOT the forensic path for a STICK LOSS** — remote logging is.
Messages must leave the box in real time, before the fs is needed.

## Two complementary transports (never redundant)

- **netconsole (kernel path)** — the in-tree kernel module ships kernel `printk` over UDP from
  kernel memory + the NIC driver. It touches no filesystem and needs no userspace fork, so it keeps
  emitting through the exact half-dead state (fs gone, exec dead) that kills everything else — the
  ONLY transport that survives the death instant. Carries only kernel printk, fire-and-forget UDP.
- **systemd-journal-upload (rich path)** — part of systemd (the `systemd-journal-remote` package),
  buffers in memory and uploads the FULL structured journal (kernel + every service unit)
  incrementally, so the minutes-before-the-drop context survives. Never has to survive the death
  instant (netconsole's job); only has to have shipped the run-up. NO rsyslog reinstall (respects
  #762).

## Single source of truth + where it is wired

`scripts/lib/remote-logging.sh` holds every constant + the pure content-generators
(`remote_log_netconsole_setup_script_content` / `_service_unit_content` /
`remote_log_journal_upload_conf_content` / `_dropin_content`), the fail-closed
`remote_log_verdict`, the `remote_log_gather_remote_snippet`, and the pure
`remote_log_mac_from_neigh` parser (embedded into the on-box setup script via `declare -f`, so there
is ONE copy). All three provisioners consume it so they can never drift (the dscp-nft.sh /
mgmt-liveness.sh pattern):

- `setup-device.sh` — STEP 16 apt-installs `systemd-journal-remote`; the `[remote-logging]`
  enable-only sub-step (after STEP 17c, before STEP 18's ro flip) writes the netconsole setup
  script + oneshot + journal-upload conf + drop-in and `enable`s both (never a live start, per
  `provisioning-scripts.md`).
- `create-usb-linux.sh` — bakes the same files into the base image (host-side) + `systemctl enable
  cambox-netconsole systemd-journal-upload` in the chroot + the `systemd-journal-remote` apt line.
- `verify-device.sh` — the `(ak)` acceptance check (inserted BEFORE `(q)`, uses the shared gather
  snippet + verdict). journal-upload's ACTIVE state is deliberately NOT gated — it depends on the
  dev1 receiver being up (a separate supervisor step); enabled + correct config is the cambox bar.

## netconsole robustness — dynamic configfs target, MAC resolved at boot

The boot oneshot (`cambox-netconsole.service`, After=network-online) runs the generated setup
script, which `modprobe netconsole`, resolves the egress dev + local ip toward dev1 (`ip route
get`), resolves dev1's next-hop MAC with a bounded ping/neigh retry (`ip neigh show` →
`remote_log_mac_from_neigh`, which returns EMPTY on FAILED/INCOMPLETE so it keeps retrying rather
than writing a bogus MAC), then (idempotently) creates + enables the `/sys/kernel/config/netconsole/cambox`
dynamic target. Resolving the MAC at boot is robust to a dev1 NIC/MAC change (re-resolved every
boot) — chosen over a hard-coded `netconsole=` cmdline.

## ro-root cursor redirect (journal-upload)

`systemd-journal-upload`'s default `--save-state=/var/lib/...` is on the read-only root. The
`10-cambox-rostate.conf` drop-in clears the stock ExecStart and re-states it with
`--save-state=/run/systemd/journal-upload/state` + `RuntimeDirectory=systemd/journal-upload` (tmpfs,
writable). A runtime-only cursor is fine — the box dies anyway; on a clean reboot it resumes from
the current boot.

## dev1 RECEIVER = a SUPERVISOR step (this repo never touches dev1 services)

`scripts/dev1-remote-log-install.sh` is a PURE PLANNER (prints the plan + `--emit <target>` renders
one config; `--apply` as root is the supervisor's convenience). It stands up:

- **rsyslog imudp on :514** (dev1 already runs `rsyslog.service`; imudp is commented out in the
  stock `/etc/rsyslog.conf`) → a dedicated `cambox_netconsole` ruleset writing
  `/var/log/cambox/<src-ip>-kernel.log` (keyed by source IP — netconsole carries no syslog
  hostname; box↔IP map: `targets.md`) + a logrotate config.
- **systemd-journal-remote --listen-http on :19532** (plain HTTP — the cambox uploaders use
  `http://`) → `/var/log/journal/remote/<host>.journal`.

Verify after apply: `ss -lunp | grep :514` (netconsole sink up) and `ss -ltnp | grep :19532`
(journal sink up).

## MGMT_DEAD correlation aid (#1309/#1311)

When the dante-clock watchdog pages `MGMT_DEAD` / box-down for `<box>`, read the last 5 min of that
box's off-box logs on dev1 (replace `<ip>`/`<host>` from `targets.md`):

```bash
# kernel (netconsole): the RFC3339 timestamp is field 1 of each line
awk -v cutoff="$(date -d '5 minutes ago' '+%Y-%m-%dT%H:%M:%S')" '$1 >= cutoff' /var/log/cambox/<ip>-kernel.log
# journal (upload):
journalctl --file /var/log/journal/remote/*<host>*.journal --since '5 minutes ago'
```

The dev1 installer's default plan (`scripts/dev1-remote-log-install.sh` with no args) prints these
exact commands.

## Tier-0 verification (worktree worker cannot run the sourced-bash Rust harness)

`tests/harness_remote_logging_1311.rs` sources the lib + runs the dev1 installer + static-anchors
all three provisioners — a worktree-isolated worker CANNOT run it (the isolation guard refuses
`bash -c '…source lib…'`, per `ci-testing-gotchas.md`). The local net a worker CAN run:
`bash -n` + `shellcheck -S warning` on every `.sh`; `bash <scratch-file>` replicas that source the
lib and call each pure function (mac parser cases, verdict green/fail-closed, generator shapes,
`bash -n` on the generated on-box setup script); a `--emit`/plan render of the installer; and
`cargo fmt --all --check` (parses the `.rs`). The Rust harness runs at CI / for the supervisor.

## GOTCHA — network-online.target is INSTANT on a cambox (wait-online is masked), so every network-dependent unit must RETRY or order on systemd-networkd.service (#1311, live on cam2 15.9.2026)

A cambox MASKS `systemd-networkd-wait-online` (`setup-device.sh` STEP 11 / `create-usb-linux.sh`
`ln -sf /dev/null …/systemd-networkd-wait-online.service`, the #547 boot-stall fix). With
wait-online masked, `network-online.target` is reached INSTANTLY at boot — before
`systemd-networkd` has applied the static IP + default route. So a unit that orders only on
`network-online.target` starts BEFORE the box can actually reach dev1.

Both #1311 units hit this at every clean boot and stayed dead until a hand restart:

- `cambox-netconsole.service` (a oneshot) resolved the egress route to dev1 ONCE and `exit 1`ed on
  the first miss → dead for the whole boot.
- `systemd-journal-upload` kept the stock `Restart=on-failure` + default StartLimit → 5 instant
  "connection refused" restarts exhausted the limit → unit stayed `failed`.

**The fix, and the rule for ANY new network-dependent unit on a cambox:** do NOT trust
`network-online.target` on this box class — a network-dependent boot unit must EITHER retry the
resource in a bounded loop (the netconsole setup script now waits for the egress route via
`REMOTE_LOG_NC_ROUTE_RETRIES` × `…_ROUTE_RETRY_SLEEP_S` before giving up) AND/OR order on
`systemd-networkd.service` itself (`After=systemd-networkd.service` + `Wants=systemd-networkd.service`),
AND, for a long-lived `Restart=` unit, disable the restart-storm ceiling
(`StartLimitIntervalSec=0` + `Restart=always` + a spaced `RestartSec=`) so it keeps retrying until
the peer answers. The netconsole unit keeps its `network-online.target` lines as documented intent
(and `harness_remote_logging_1311.rs` pins them), but the retry loop + `systemd-networkd.service`
ordering are what actually make it survive a clean boot.

Tier-0 boot-order coverage: `tests/python/test_remote_logging_boot_order_1311.py` (a fake `ip` whose
`route get` misses the first N calls proves the retry arm; unit/drop-in text asserts). Make
`REMOTE_LOG_NC_CONFIGFS` env-overridable for that arm test.
