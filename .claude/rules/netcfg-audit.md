---
paths:
  - "scripts/netcfg-audit.sh"
  - "scripts/lib/netcfg-audit.sh"
  - "scripts/netcfg-drift-alert-watchdog.sh"
  - "scripts/netcfg-baseline.json"
  - "systemd/netcfg-drift-alert-watchdog.*"
  - "tests/harness_netcfg_*.rs"
---

# Venue-switch config-drift audit + baseline (`netcfg` facet, #797)

Turns the 2026-07-17/18 burst-gap / 18:41-transport-collapse investigation into a durable,
re-checkable artifact. The bug-hunt threads on #797 were retracted (the "OBS caps at 50 fps" receive
cap was a measurement artifact — a rate computed from the ~5.017 s `genlock-fifo audit` counter over
a 6 s wall window aliases 60→50.17), and the one real transport defect — foh2 10G-trunk→edge
microburst egress tail-drop — was fixed by raising `shared-buffers` 40%→80%. What remained: the venue
MikroTik chain had **no machine-checkable record of its healthy state**, so this facet captures a
checked-in baseline + REPORTS drift.

## Topology (the audited chain)

All-MikroTik, ssh user `admin`, password = the fleet deploy pw (**never committed**; memory
`cam-fleet-deploy-credential`). router_snv RB4011iGS+ = `10.77.8.1` (ROS 7.22); CRS310-8G+2S+
switches foh1_audio `10.77.9.2`, stage_av `10.77.9.3`, foh1_video `10.77.9.4`, foh2_video
`10.77.9.5` (ROS 7.23.3, all `shared-buffers=80%`). 10G SFP+ trunks between them; port comments
`etherN`=`basic`/`dante`, `sfp-sfpplusN`=`trunk::fohN`.

## Files

- `scripts/lib/netcfg-audit.sh` — the PURE parse + drift-classify core (no I/O; Tier-0 unit-tested by
  `tests/harness_netcfg_audit_797.rs`). Functions: `netcfg_parse_field`/`netcfg_parse_stat` (RouterOS
  human-output parsers, start-anchored, thousands-space stripped), `netcfg_normalize_rate`/`_version`,
  `netcfg_classify_match`/`_rate`/`_drop_rate`, `netcfg_drift_verdict`,
  `netcfg_port_is_designated` (#1110 — exact `node|port` membership in the drop-sampler always-probe set).
- `scripts/netcfg-audit.sh` — the read-only orchestrator. `--capture` writes the baseline, `--check`
  (default) diffs live-vs-baseline (exit 0=CLEAN, 3=DRIFT, 2=error), `--json` prints a snapshot.
  **It NEVER issues a config write** (read-only ops tool).
- `scripts/netcfg-baseline.json` — the checked-in baseline (the latency-pins-baseline.json pattern:
  REPORT-ONLY source of truth, seeded from a live read, updated only via a PR — never auto-overwritten).
- `scripts/netcfg-drift-alert-watchdog.sh` + `systemd/netcfg-drift-alert-watchdog.{timer,service}` —
  the dev1-side, report-only Discord alert on CONFIRMED drift (shared `obs_watchdog_confirm`/
  `_alert_throttle`, same framework as network-reach/cadence/imag-obs). Hourly cadence (config drift
  is low-frequency). Ships DISABLED — see enable below.

## What is DRIFT-checked vs report-only

- **Baseline-diffed (a hard-drift page):** per-switch `shared_buffers` + `ros` version (exact match),
  per-port link `rate` (a baselined port negotiating SLOWER = `DEGRADED`; faster = informational
  `FASTER`), per-port `comment`/role.
- **Live drop-RATE probe (a hard-drift page):** any port whose live cumulative `tx-drop-queue1-packet`
  is nonzero is re-probed with two `print stats` reads `NETCFG_DROP_WINDOW` (6 s) apart; a rate above
  `NETCFG_DROP_THRESHOLD` (1/s) → `DROPPING` (the microburst-tail-drop signature — check shared-buffers
  / the uplink step-down). This is a LIVE rate, deliberately NOT a cumulative-counter baseline diff.
  **Designated always-probe set (#1110):** the `node|port` tokens in `NETCFG_DROP_PROBE_PORTS` (default
  `foh1_video|ether2` — strih-lx's USB 2.5 GbE uplink egress since the M4 cut-over, issue 1242,
  live-verified 2026-09-23 via `/interface bridge host print where mac-address=<strih-lx MAC>`; it
  was `foh2_video|sfp-sfpplus2`, the Windows strih PC's 10 G DAC, until 20.9.2026) are
  re-probed on EVERY `--check` regardless of cumulative growth, so a HEALTHY suspect uplink (dq1 flat
  at 0, which the growth-gate would otherwise never sample) still yields a fresh live delta — the next
  starvation episode is caught. `DROPPING` on a designated port pages exactly like any other; a CLEAN
  designated probe surfaces a report-only `sampled` line carrying the cumulative dq1 delta (the
  sampler always leaves a trace; the strih-lx tail-drop comes in BURSTS every ~10–60 s, so one clean
  6 s window is NOT a clean uplink). A designated port with NO link (`running!=true`) is NOT probed —
  it reports `[report-only designated-down] … the designation is stale` (pure predicate
  `netcfg_designated_port_linked`), because a dead port's flat counter used to print "strih uplink
  probe clean" for days after strih moved. **Re-point the default whenever strih is re-cabled.** Set
  `NETCFG_DROP_PROBE_PORTS=` (empty) to restore the pre-#1110 growth-gated-only behaviour. The
  designation lives in this ENV default (checked-in, PR-reviewed) — NOT in `netcfg-baseline.json`,
  which stays pure captured-state; the port itself is already in the baseline's per-port diff.
- **Report-only (surfaced, never pages):** `ABSENT` (a baselined port with no live link — a device
  unplugged between events must not page), `RESET` (drop counters went backwards = the switch rebooted),
  `UNKNOWN` (an unreadable field). The cumulative `dq1`/`fcs`/`running` fields in the baseline are a
  point-in-time REFERENCE snapshot, not drift-checked.

## Running it

```bash
NETCFG_SWITCH_PW=<mikrotik-admin-pw> scripts/netcfg-audit.sh --check     # exit 3 on drift
NETCFG_SWITCH_PW=<pw> scripts/netcfg-audit.sh --capture                  # re-seed the baseline (PR the diff)
```

When the switch config LEGITIMATELY changes (a planned ROS upgrade, a re-tuned buffer, a relocated
device), re-run `--capture` and commit the new `scripts/netcfg-baseline.json` in a PR — same discipline
as `scripts/latency-pins-baseline.json`. The check then goes CLEAN again.

## Enabling the dev1 timer (one-time, ships DISABLED)

The `admin` password is NOT committed. On dev1:

```bash
mkdir -p ~/.config/camera-box && printf 'NETCFG_SWITCH_PW=%s\n' '<mikrotik-admin-pw>' > ~/.config/camera-box/netcfg-drift.env
chmod 600 ~/.config/camera-box/netcfg-drift.env
cp systemd/netcfg-drift-alert-watchdog.{timer,service} ~/.config/systemd/user/
systemctl --user daemon-reload && systemctl --user enable --now netcfg-drift-alert-watchdog.timer
```

The `.service` loads the password via `EnvironmentFile=-%h/.config/camera-box/netcfg-drift.env` (the
leading `-` makes it optional — with no file the pass runs, logs "NETCFG_SWITCH_PW is empty", and exits
WITHOUT paging, so a mis-provisioned dev1 never spams).

## Gotchas

- **`python3 - <<'PY'` consumes stdin for the PROGRAM**, so a heredoc-programmed python cannot ALSO
  read piped data from stdin — the flat gather data is passed as a FILE (argv), never a pipe. (Cost me
  an empty first baseline.)
- **`/interface ethernet print stats` UNFILTERED is columnar** (multi-column, `100 054` thousands
  spaces); `... where name=X` is clean single-column. The snapshot gather uses a `:foreach` +
  RouterOS `get` (which returns RAW integers, no thousands-space) + `monitor ... as-value` for rate —
  ONE ssh call per switch, not one-per-port. `netcfg_parse_stat` (thousands-space stripping) is used
  by the drop-RATE probe path, which reads the human `print stats where name=`.
- **ROS version drift is real and expected on a planned upgrade** — the switches drifted 7.22→7.23.3
  while the router stayed 7.22. A version bump correctly reads as DRIFT until you re-`--capture` +
  PR the baseline (that is the intended "record the change deliberately" flow, not a false alarm).
- **Scope boundary:** this facet is switch/router config + counter drift ONLY. The still-open
  mDNS-discovery flakiness (CAM1 invisible to imag) and the #787 status-page relock/arrival charting
  are separate, out of this facet.

- **`_nc_ssh` carries `ssh -n` — NEVER remove it (#1110 hotfix, 2026-08-25):** `_nc_drop_rate_verdict` runs ssh INSIDE the `--check` drop-probe `while read` loop fed by a herestring; an ssh without `-n` consumes the loop's remaining stdin, so the loop silently dies after the FIRST probed port and later nodes (the designated strih-uplink included) are never probed. Live repro: bash -x showed exactly one probe call, then loop end. Pinned by `nc_ssh_is_stdin_safe_for_while_read_loops` in tests/harness_netcfg_audit_797.rs.

## Finding (issue 1242, 23.9.2026): the strih-lx 2.5 G uplink tail-drops the genlocked camera burst

`foh1_video ether2` → strih-lx (USB RTL8156, 2.5 Gb/s) carries all seven ~170 Mb/s NDI camera
streams. Genlock makes every sender emit its ~350 KB frame on the SAME 60 Hz boundary, so the frames
arrive together over the 10 G trunks and overflow the CRS310 egress queue. Measured: dq1 3.79 M packets
(5.5 GB) on that port, vs **0** lifetime drops on the old Windows strih's 10 G DAC port
(`foh2_video sfp-sfpplus2`, 73.9 G packets). A burst of ~2500 dropped packets is NOT recovered by
NDI's reliable UDP: the affected receivers get no video for ~380 ms (`recv-timing #797` n≈277 on
2–5 inputs AT ONCE, `cap_max` 66–100 ms = the 100 ms SDK capture timeout). The genlock FIFO then
underruns (burn-id repeats) and converge-sheds. These are the multi-frame residual windows (CAM7 11/11,
CAM4 10/10). Smaller bursts (≤~650 pkts) cost no frames. The cure is uplink capacity (a faster NIC
into a 10 G port), not a receiver/FIFO parameter. The strih-lx host itself drops nothing
(UDP `RcvbufErrors`=0, softnet drops 0).

Discriminator recipe for the next "late-arrival burst on strih" question:
1. One recv-timing shortfall on ≥2 inputs in the SAME 5-s interval, with no sender-side emit deficit
   (burn log `Streaming:` 300 sent) = transport. One input only, with low cap_max = that sender
   restarting/stalling.
2. Sample `:put [/interface ethernet get ether2 tx-drop-queue1-packet]` (dq1, the counter the audit
   uses) on 10.77.9.4 at ~1 Hz alongside the strih-lx OBS log. The shortfall second should hold the
   big drop burst. On 23.9. every ether2 drop was in queue1, so the total `tx-drop-packet` counter read
   the same.
3. On strih-lx (read-only): `/proc/net/snmp` Udp `RcvbufErrors` and `/proc/net/softnet_stat` col 2.
   Nonzero would mean the receive side itself is too slow. Zero means the loss is upstream.
