---
paths:
  - "scripts/lib/remote-logging.sh"
  - "scripts/lib/strih-cambox-log-sink.sh"
  - "scripts/cambox-ro-units-apply.sh"
  - "tests/python/test_remote_log_sink_1394.py"
  - "tests/python/test_strih_cambox_log_sink_1394.py"
  - "tests/harness_remote_logging_1311.rs"
  - "scripts/lib/cambox-ro-units.sh"
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
is ONE copy). Its consumers can never drift (the dscp-nft.sh / mgmt-liveness.sh pattern):

- `setup-device.sh` — STEP 16 apt-installs `systemd-journal-remote`; the `[remote-logging]`
  enable-only sub-step (after STEP 17c, before STEP 18's ro flip) writes the netconsole setup
  script + oneshot + journal-upload conf + drop-in and `enable`s both (never a live start, per
  `provisioning-scripts.md`).
- `verify-device.sh` — the `(ak)` acceptance check (inserted BEFORE `(q)`, uses the shared gather
  snippet + verdict, graded against THIS box's sink). journal-upload's ACTIVE state is deliberately
  NOT gated — it depends on the sink's receiver being up; enabled + correct config is the cambox bar.
- `scripts/lib/cambox-ro-units.sh` + `scripts/cambox-ro-units-apply.sh` — re-point a LIVE box (below).
- `create-usb-linux.sh` bakes NONE of it since issue 1394 (only the `systemd-journal-remote`
  package stays in the chroot install): the image is the same for every box, so it cannot know the
  box's venue sink; setup-device always runs next, with the box name.

## The SINK is the cambox's OWN venue strih (issue 1394, owner ROZHODNUTE 9.10.2026)

Until 9.10.2026 the sink was dev1, hard-coded as `10.77.9.200` — a DHCP pool address leased to a
phone (8.10), so systemd-journal-upload failed on all seven boxes, netconsole could not arm, and
`(ak)` passed anyway because it graded against the same constant. The owner ruled: no rig service
and no rig data on dev1 ("strih nb, which has the performance and the disk"), no router change for
dev1.

- `remote_log_sink_ip CAMERA` = the `STRIH_IP` of the ONE strih-ROLE fact file
  (`scripts/strih-boxes/*.env`) whose `STRIH_CAMERAS` lists the camera, parsed through
  `strih_box_parse_file` in a subshell (the caller's shell options and the loader's globals stay as
  they were). None, two, a bad name or a non-IPv4 address = rc 1 + a named stderr line, never a
  guess. Today: cam2/cam3/cam5 -> 10.77.9.202 (strih-snv), cam1/cam4/cam6/cam7 -> 10.77.8.202
  (strih-pp, on the PP /23 the camboxes sit on: netconsole needs the sink's next-hop MAC on the
  same L2). A stream-role box is never a sink.
- There is NO sink constant in the lib. The generators (`remote_log_netconsole_setup_script_content
  SINK`, `remote_log_journal_upload_conf_content SINK`) and `remote_log_verdict BLOCK SINK` take it
  as an argument; no / a non-IPv4 sink = refusal / a fail-closed verdict. Only the ports are
  declared there (`REMOTE_LOG_NETCONSOLE_PORT` 514, `REMOTE_LOG_JOURNAL_PORT` 19532), and the
  receiver reads the same two.
- `setup-device.sh` resolves it right after the box name, before the first write (`fail` when none).
- Moving a camera between venues = the two fact files (the dantesync-fleet rule); the next
  re-point (below) or setup-device run follows.

## netconsole robustness — dynamic configfs target, MAC resolved at boot

The boot oneshot (`cambox-netconsole.service`, After=network-online) runs the generated setup
script, which `modprobe netconsole`, resolves the egress dev + local ip toward the sink (`ip route
get`), resolves the sink's next-hop MAC with a bounded ping/neigh retry (`ip neigh show` →
`remote_log_mac_from_neigh`, which returns EMPTY on FAILED/INCOMPLETE so it keeps retrying rather
than writing a bogus MAC), then (idempotently) creates + enables the `/sys/kernel/config/netconsole/cambox`
dynamic target. Resolving the MAC at boot is robust to a sink NIC/MAC change (re-resolved every
boot) — chosen over a hard-coded `netconsole=` cmdline.

## ro-root cursor redirect (journal-upload)

`systemd-journal-upload`'s default `--save-state=/var/lib/...` is on the read-only root. The
`10-cambox-rostate.conf` drop-in clears the stock ExecStart and re-states it with
`--save-state=/run/systemd/journal-upload/state` + `RuntimeDirectory=systemd/journal-upload` (tmpfs,
writable). A runtime-only cursor is fine — the box dies anyway; on a clean reboot it resumes from
the current boot.

## The RECEIVER = the venue strih: setup-strih step 16k, verify-strih item 46 (issue 1394)

`scripts/lib/strih-cambox-log-sink.sh` (sourced by strih-provision.sh), on every strih-ROLE box
(`obs_box_step_applies 16k`; the stream role is skipped):

- **systemd-journal-remote** (apt `systemd-journal-remote`) behind its stock socket :19532. A
  drop-in `systemd-journal-remote.service.d/10-cambox-sink.conf` switches it to
  `--listen-http=-3 --output=/var/log/journal/remote/` (the stock unit listens HTTPS, the uploaders
  send http://). The HARD cap is `/etc/systemd/journal-remote.conf.d/10-cambox-sink.conf`
  (`MaxUse=4G`, `KeepFree=20G`, the smaller wins; `MaxFileSize=128M`, `MaxFiles=200`,
  `SplitMode=host`, `Seal=false`) -- a conf.d file, never the package's conffile (an edited conffile
  makes dpkg ask on the next upgrade).
- **cambox-netconsole-sink.service** = a DEDICATED rsyslog instance (`rsyslogd -n -iNONE -f
  /etc/rsyslog.d/cambox-netconsole-sink/rsyslog.conf`, `User=syslog` +
  `AmbientCapabilities=CAP_NET_BIND_SERVICE`, `LogsDirectory=cambox`). imudp on :514; one file per
  camera the box lists (`/var/log/cambox/camN-kernel.log`, the sender matched by its
  `camera_resolve` address = netconsole's source) and `other-senders.log` for anything else: a
  fixed set of files, nothing dropped. No workDirectory, no disk queue. The box's own
  `rsyslog.service` is never touched.
  - **Why a subdirectory of /etc/rsyslog.d:** Ubuntu ENFORCES an AppArmor profile on
    `/usr/sbin/rsyslogd` that reads only `/etc/rsyslog.conf` + `/etc/rsyslog.d/**` and writes
    `/var/log/**` + `/var/spool/rsyslog/**` (dev1, 15.10.2026: `rsyslogd -N1 -f <scratch file>` =
    "Permission denied"). The box's own instance loads only `/etc/rsyslog.d/*.conf`, so the
    subdirectory is read by this instance alone. To validate a rendered config on dev1, run a COPY
    of the binary (the profile attaches by path): `cp /usr/sbin/rsyslogd x && ./x -N1 -f <conf>`.
- **cambox-netconsole-rotate.timer** (every 10 min) runs logrotate with its own config
  (`/etc/camera-box/cambox-log-sink/logrotate.conf`) and state: each file rotated past 16M, 4
  compressed copies, `copytruncate`, `su syslog syslog`. Bound: (cameras + 1) x 5 files of at most
  16M each plus 10 min of growth.
- **Idle priority, never the production OBS:** every receiver unit carries `Nice=19`,
  `CPUSchedulingPolicy=idle`, `IOSchedulingClass=idle`, and on a hybrid CPU `CPUAffinity=` the
  E-cores (`strih_program_audio_ecores`, the strih_lx_lowprio_prefix reading). strih-snv is AMD (no
  `cpu_atom`): no affinity line, every cpu.
- **Install (step 16k) is enable-only:** packages only when missing, every file through
  `strih_session_apps_put` (written only when its bytes or mode differ), a daemon-reload after a unit
  change, `enable` of the socket + sink + rotate timer, a `try-restart` of a RUNNING unit whose files
  changed; a stopped unit is never started. The supervisor starts them once (runbook below).
- **verify-strih item 46** (`strih_cambox_log_sink_grade_report`, 3 rows): `(log-sink-files)`
  packages + every file byte-identical to its renderer; `(log-sink-units)` the three units enabled +
  active (a FAIL names the `systemctl start`); `(log-sink-listen)` UDP :514 and TCP :19532.
- setup-strih / verify-strih are at their 1000-line budget: the step and the item are one call each.

## Re-pointing a LIVE cambox (no setup-device re-run)

`scripts/cambox-ro-units-apply.sh` (the issue-1394 read-only-root apply) resolves every box's sink
with `remote_log_sink_ip` BEFORE the first box is touched (none = exit 2, nothing touched) and runs
`cambox_ro_units_apply_program SINK` per box: the same ONE verified rw window
(`.claude/rules/ro-window.md`) also rewrites the netconsole setup script (0755), journal-upload.conf
and the upload drop-in where issue 1311 put them (each only when it differs; a box without the
netconsole unit gets a NOTE and none of them), then after the close the netconsole restart picks up
the script and systemd-journal-upload restarts when its files changed. `--venue <name>` dials a
venue's cameras at their dev1 address (`dantesync_fleet_venue_camera_rows`: the Poprad boxes are
10.76.9.x through companion-pp). `CAMBOX_RO_UNITS_FS_ROOT` is the test seam for the two files
outside the unit dir.

### Supervisor runbook (the live apply is not a lane's job)

1. **strih-snv** (10.77.9.202) and **strih-pp** (10.77.8.202, dial `strih-pp.lan` from dev1): run
   setup-strih (`--box <box>`; the strih-snv genlock deploy runs it anyway), or only step 16k from a
   checkout; then `sudo systemctl start systemd-journal-remote.socket cambox-netconsole-sink.service
   cambox-netconsole-rotate.timer`; then `sudo ./scripts/verify-strih.sh --box <box>` -> item 46 =
   3 PASS. Read back: `ss -lunp | grep ':514 '` names rsyslogd, `ss -ltnp | grep ':19532 '` names
   systemd.
2. **The camboxes** (no E2E holding the rig lease; never a reboot):
   `scripts/cambox-ro-units-apply.sh --plan --active` (SNV: cam2 cam3 cam5) and `--plan --venue pp`
   (cam1 cam4 cam6 cam7), read the `PLAN ... off-box log sink` lines, then the same with `--apply`.
   Each box: `OK: cambox-netconsole.service armed` + `systemd-journal-upload.service restarted -> <sink>`.
3. **Read-back:** on the strih box `ls -l /var/log/cambox/` (a `camN-kernel.log` appears with the
   next kernel line; `echo test > /dev/kmsg` on a cambox forces one) and
   `ls /var/log/journal/remote/` (one `remote-<host>*.journal` per box within a minute);
   `./scripts/verify-device.sh <CAMn>` `(ak)` passes against the venue sink.
4. The 505 MB of old journals on dev1 (`/var/log/journal/remote`, 24.-30.9.) move to strih-snv and
   are then deleted from dev1, as the 9.10.2026 ROZHODNUTE on the ticket plans (a supervisor step).

## MGMT_DEAD correlation aid (#1309/#1311)

When the dante-clock watchdog pages `MGMT_DEAD` / box-down for `<box>`, read the last 5 min of that
box's off-box logs ON ITS VENUE STRIH (`remote_log_sink_ip <box>`; replace `<host>` from
`targets.md`):

```bash
# kernel (netconsole): the RFC3339 timestamp is field 1 of each line
awk -v cutoff="$(date -d '5 minutes ago' '+%Y-%m-%dT%H:%M:%S')" '$1 >= cutoff' /var/log/cambox/<box>-kernel.log
# journal (upload):
journalctl --file /var/log/journal/remote/*<host>*.journal --since '5 minutes ago'
```

## Tier-0 verification

pytest (all run from a worktree): `tests/python/test_remote_log_sink_1394.py` (the resolver over the
real and synthetic fact files, the generators, the verdict, the consumers' wiring, no dev1 left),
`tests/python/test_cambox_log_sink_repoint_1394.py` (the re-point on the issue-1407 fake box + the
CLI), `tests/python/test_strih_cambox_log_sink_1394.py` (the receiver lib: renderers,
`systemd-analyze verify` of the units, the install against fake systemctl/dpkg-query/apt-get, the
grader, the wiring and budgets).

### The Rust harness

`tests/harness_remote_logging_1311.rs` sources the lib + static-anchors the provisioners. It is
std-only, so it runs locally with no cargo (issue 1394): `CARGO_MANIFEST_DIR=<worktree> rustc
--edition 2021 --test tests/harness_remote_logging_1311.rs -o <scratch>/t && <scratch>/t`, from a
script FILE in a worktree lane (the isolation guard refuses an inline `$VAR` path), plus
`clippy-driver --edition 2021 --test -D warnings` the same way and `rustfmt --check` on the file.

## GOTCHA — network-online.target is INSTANT on a cambox (wait-online is masked), so every network-dependent unit must RETRY or order on systemd-networkd.service (#1311, live on cam2 15.9.2026)

A cambox MASKS `systemd-networkd-wait-online` (`setup-device.sh` STEP 11 / `create-usb-linux.sh`
`ln -sf /dev/null …/systemd-networkd-wait-online.service`, the #547 boot-stall fix). With
wait-online masked, `network-online.target` is reached INSTANTLY at boot — before
`systemd-networkd` has applied the static IP + default route. So a unit that orders only on
`network-online.target` starts BEFORE the box can actually reach its sink.

Both #1311 units hit this at every clean boot and stayed dead until a hand restart:

- `cambox-netconsole.service` (a oneshot) resolved the egress route to the sink ONCE and `exit 1`ed on
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

## The netconsole oneshot RETRIES a failed arm (issue 1394, live 8.10.2026)

The in-script retry above is bounded (~60 s route + ~60-90 s MAC), and once it gave up the oneshot
stayed `failed` until the next boot: cam1's netconsole had been failed since 4.10. 06:24Z (the reason
was rotated away), no kernel printk reached the sink (then dev1), and the box read `degraded`. The unit now carries
`Restart=on-failure` + `RestartSec=30` + `RestartSteps=4` + `RestartMaxDelaySec=600` in `[Service]`
and `StartLimitIntervalSec=0` in `[Unit]`, so a failed arm retries until the sink answers, the delay
growing from 30 s to 10 min (a box whose sink is down or away would otherwise write a failed arm
into the issue-1309 on-box journal every 30 s forever). The
start-limit lift is a safeguard: at RestartSec=30 the default limit cannot trip.
- **systemd 255 allows `Restart=on-failure` on `Type=oneshot`** (with `RemainAfterExit=yes`);
  `always` / `on-success` are refused for a oneshot. `tests/python/test_cambox_ro_units_1394.py` runs
  `systemd-analyze verify` on the generated unit (ExecStart swapped to `/bin/true`) and proves the
  verifier refuses `Restart=always`.
- **A unit in its retry loop reads `activating (auto-restart)`, not `failed`**, so it no longer makes
  the box `degraded`. `(ak)` still FAILs it (not active), now with the unit's systemd `Result`; a
  `failed` unit is named `FAILED (Result=exit-code)` from the new `NC_SVC_RESULT` gather line.
- `verify-device (ar)` grades the unit FILE byte-for-byte against the generator and the loaded
  `Restart=` (a unit written without a daemon-reload runs the old one). A live box gets the new unit
  through `scripts/cambox-ro-units-apply.sh` (`.claude/rules/provisioning-scripts.md`); a box that
  never got the issue-1311 unit gets a NOTE there and needs its setup-device re-run.
