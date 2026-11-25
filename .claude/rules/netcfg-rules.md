---
paths:
  - "scripts/netcfg-rules.sh"
  - "scripts/netcfg_rules.py"
  - "scripts/netcfg-venues.json"
  - "scripts/netcfg-export-baseline.json"
  - "tests/python/test_netcfg_rules_1554.py"
  - "tests/python/fixtures/netcfg_rules_1554/**"
---

# The venue network rule gate of the rig test (issue 1554, SNV + PP)

Owner 25.11.2026 (network#18): a network problem or a configuration change at SNV or PP must FAIL
the rig test, so it gets fixed. `scripts/netcfg-rules.sh --venue snv|pp` reads every MikroTik of
the venue once and exits 3 on any hard finding; the SNV full-path E2E runs it as the `[0/8] venue
network rules gate` right after the disk preflight, before the DanteSync gate.

## Files

- `scripts/netcfg_rules.py` -- the pure parse + rules + CLI (`devices` / `check` / `capture`). It
  reuses `netcfg_bridge_qos.py`'s section splitter (`_split_sections(raw, required=...)`) and its
  `print detail` parser. Every finding is `<STATUS>\t<1 hard|0 report-only>\t<message>`.
- `scripts/netcfg-venues.json` -- the ONLY per-venue knowledge: dial path, devices (`reads` full /
  router, `qos`, `dante_dhcp`, `cpu_bridge`, `ptp_mirror`), IGMP policy, Dante subnet, DHCP zone,
  `dante_dhcp_no_gateway`, the DSCP -> traffic class map. A new venue = a facts entry.
- `scripts/netcfg-rules.sh` -- ssh only: one call per device, `ssh -n`, `timeout` around the whole
  call (OUTSIDE `sshpass`, so ssh stays in the pty's foreground). SNV: key user `camerabox`
  (network#15). PP: `snv` through `-J companion-pp`, password by `sshpass -f ~/.secrets/pp-mikrotik-ro`
  (ROZHODNUTE 7167031660). `--from-dir DIR` reads `<DIR>/<device>.txt` instead (tests, replays).
- `scripts/netcfg-export-baseline.json` -- sha256 of each device's normalised export, per venue.

## The read (what may ever run on a device)

`:put "=== <section>"` markers around `/system identity print`, `/system routerboard print`,
`/interface bridge monitor [find] once`, `/interface bridge port print detail`,
`/interface bridge port monitor [find] once`, `/export terse`, closed by `:put "=== end"`. A
`router` device gets identity + routerboard + export only. Every command was run on the SNV
switches with the `camerabox` user first (25.11.2026), and the same commands are in the mikrotik
project's PP dumps of 25.11. **PP is a third party's network and its admin reads the logs:** a
failed command shows as "executing script from sshd failed". Never add a command to `COMMANDS`
before it ran clean on an SNV device, and never a write. `test_the_gather_uses_only_read_commands`
pins the shape.

## Rules (hard = FAIL)

- 1 drift: the normalised export (one canonical line per entry, sorted, comments dropped) hashed
  per device against the committed hash -> DRIFT. No committed hash = report-only UNKNOWN.
- 2 IGMP: SNV (`igmp: off`) any bridge `igmp-snooping=yes`; PP (`querier`) snooping with
  `igmp-querier: none` in the bridge monitor. A router read without the monitor = UNKNOWN.
- 3 RSTP: an in-bridge host port with `edge-port: no`; a bridge link whose `topology-changes` grew
  since the previous run. A bridge link = `frame-types=admit-only-vlan-tagged` OR a port that
  RECEIVES BPDUs (`tx-rx-bpdu` rx > 0: router_snv ether15, the lan uplink, is one). Growth on a
  `restricted-tcn=yes` port is a WARN (those TCs never reach the MAC tables). A counter that went
  back = a reboot, REBASE (report-only). First run = baseline, no finding.
- 4 HW offload: a static, enabled bridge port without `H`; a `cpu_bridge` device (PP Stage RB4011)
  only WARNs.
- 5 Dante QoS on a `qos` device: `trust-l3=keep` on every bridge port, a profile per expected DSCP
  with the venue traffic class (SNV 56->7 / 46->5, PP 56->7 / 46->6).
- 7 Dante DHCP on the `dante_dhcp` device: a DHCP server on the Dante subnet, its pool inside
  `dhcp_zone` (.100-.200; fixed Dante addresses are configured on the hosts below / above it), and
  where `dante_dhcp_no_gateway` (PP, owner decision) no `gateway` / `dns-server` on that network.
  A static lease INSIDE the pool is the server's own reservation and fine (SNV has eight).
- 8 PTP mirror (PP FOH-A facts): the mirror rule, `mirror-target`, the target outside the bridge,
  the return port's pvid and status.
- 10 routerboard `current-firmware != upgrade-firmware` -> WARN (reboot pending).
- An unreachable device or a read without `=== end` -> FAIL (fail-closed); an unparseable section
  -> UNKNOWN, report-only.

## The public repo boundary

The repo is PUBLIC and PP's configuration is Marcel's. So: the export TEXT is never committed --
`--capture` writes it to `~/.camera-box/netcfg-export/<venue>/<device>.rsc` on dev1 and only its
hash to the baseline JSON. A DRIFT line names the path + the entry key (`name=` / `interface=` /
`address=` ...) and changed/added/removed, never another value; it can only name entries when the
dev1 copy matches the committed hash. The test fixtures are EXCERPTS (the paths the rules read,
MAC addresses and WAN addresses dropped).

## Accepting a deliberate change

`scripts/netcfg-rules.sh --venue <v> --capture` on dev1, then a PR with the new
`scripts/netcfg-export-baseline.json`. A capture with one unreachable device writes nothing.

## State

`~/.camera-box/netcfg-rules-<venue>-tc.json` (`NETCFG_RULES_STATE_DIR`): the topology-change
counters of the previous run, written after every check. The self-hosted E2E runner must run as the
user that owns the key, the export copies and this file (newlevel on dev1).

## Open (issue 1554 checklist)

- PP: no PP rig CI exists, so `--venue pp` has no automatic caller yet; the PP baseline has not been
  captured; the first live PP read is the supervisor's step (owner: only during a PP rig run).
- Not built yet: 3 the restricted-tcn port list, 5 strict-priority queues + Dante queue drop growth,
  6 Dante host links (duplex, speed, flow control, link-down growth), 9 the multicast/broadcast
  flood-stall deltas, 4 the "Dante device learned on a CPU bridge" condition. Each needs a command
  verified on SNV first.
