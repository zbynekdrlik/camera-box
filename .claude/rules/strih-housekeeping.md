---
paths:
  - "scripts/lib/strih-housekeeping.sh"
  - "scripts/lib/strih-nic-irq.sh"
  - "scripts/lib/obs-box-background.sh"
  - "systemd/strih-nic-irq-affinity.service"
  - "systemd/strih-nic-irq-rebalance.*"
  - "scripts/strih-obs-start.sh"
  - "tests/python/test_strih_housekeeping_1425.py"
---

# Housekeeping off the OBS CPUs on a strih box (issue 1425, design 7106973291)

OBS runs on the CPUs in `/etc/strih-isolated-cpus.conf` (strih-snv 2-15). Everything else that wakes up
often belongs on the rest: the box fact `STRIH_HOUSEKEEPING_CPUS` (strih-snv `0-1`, every other box `none`
until measured). It must equal online minus the OBS mask (`obs_box_background_cpus`); verify `(hk-cpus)`
FAILs otherwise.

## The spread: the rig NIC IRQs over every housekeeping CPU by load (design 7119179275, 24.11.2026)

History: everything on 0-1 gave dstack's first rx holes (the RX softirq waited next to the program-audio
sampler at up to 83 % of a core); the next split (design 7116063669) put all IRQs alone on CPU 1, and one
CPU could not carry them: CPU 1 ~86 % softirq (ksoftirqd/1 ~67 %), `time_squeeze` +~110/s, enp7s0
`InDroppedDma` 3146, dlink rx_loss 528 -> 800 in 2 min, CPU 0 ~55 % idle. Per-IRQ rates then: 136 17.0k/s,
140 9.5k/s, 137 7.2k/s, 138 4.0k/s, 134 3.9k/s, the rest < 0.3k/s. The ONE fact now reads:
- `strih_housekeeping_irq_cpus [CPUS]` = EVERY CPU of the set, canonical (strih-snv `0-1`): the oneshot's
  `@STRIH_HOUSEKEEPING_IRQ_CPUS@` (env override `STRIH_HOUSEKEEPING_IRQ_CPUS`), the RSS count (oneshot +
  udev rule = 2 on strih-snv; atlantic has no channel control, so a no-op there);
- `strih_housekeeping_task_cpus [CPUS]` = the FIRST CPU (strih-snv `0`): rustdesk + the display watch
  drop-ins, `/etc/strih-housekeeping-cpus.conf` (the launcher's own move), `rig-background.slice`;
- a one-CPU set gives that CPU to both; `none` gives `none` to both. Pure halves:
  `obs_box_cpulist_canon` / `obs_box_cpulist_first` in `obs-box-background.sh`.
- **The placement (the oneshot):** it samples `/proc/interrupts` over `IRQ_RATE_SAMPLE_S` (2 s, a whole
  number of seconds or FATAL; 0 in tests = the delta of two reads) and puts each IRQ on ONE CPU, greedily:
  heaviest first to the CPU with the least assigned rate, ties to the lower CPU. **An idle sample keeps
  the placement (finding 26.11.2026):** a total under `IRQ_LOAD_MIN_PER_S` (default 20000/s, env
  override, a whole number or FATAL; loaded reads were ~120k/s and ~42k/s) is no load to balance by. A
  genlock deploy stops OBS, and a run in that window (the NM `up` hook, a hand restart) sampled tiny,
  wrong-ordered rates and put 7 of 9 heavy queues on CPU 0 (time_squeeze ~117/s, InDroppedDma 4501 ->
  32451, Dante holes, release E2E red). Under the floor every IRQ already on ONE housekeeping CPU keeps
  it (logged `kept`, never written), and only an IRQ outside the set goes to the CPU holding the fewest
  IRQs, in IRQ order, ties to the lower CPU: at boot nothing is kept, so that is the round-robin in IRQ
  order. setup-strih step 11b is enable-only (never restarts the oneshot), so the fix lives in the
  script, where every trigger runs it. An IRQ with no `/proc/interrupts` row counts as rate 0 (logged). It
  logs each IRQ's rate and CPU and the assigned load per CPU, and writes an affinity ONLY when it differs
  (every move costs a short loss burst, measured 24.11.2026). The placement holds until the next run (boot,
  NM `up`, the re-balance below); a later traffic change can unbalance it, the `(hk-softnet)` NOTE shows that.
- **The delayed re-balance (design 7119686502):** at boot no traffic flows, so the oneshot's sample reads
  every rate 0 and places round-robin. `strih-nic-irq-rebalance.timer` (`OnBootSec=5min`, no `Persistent`,
  `WantedBy=timers.target`) starts `strih-nic-irq-rebalance.service` (`Type=oneshot`, NO RemainAfterExit,
  no `[Install]`, the same script, `After=strih-nic-irq-affinity.service`) ONCE per boot, when the strih
  OBS pulls the cameras + the program. Their text is `strih_housekeeping_rebalance_{service,timer}_text`
  in the housekeeping lib, byte-identical to the committed `systemd/` files (parity test), carrying our
  header line. `strih_housekeeping_install` writes them if changed (daemon-reload then) and ENABLES the
  timer when it is not (never started: it fires at the next boot); `none` disables + stops our timer and
  removes both (header-checked). Rejected: a periodic re-balance (each move is a loss burst, no hysteresis
  data); delaying the boot oneshot (it must take the IRQs off the OBS CPUs at boot). Trade-off: a boot whose
  rig carries no traffic 5 min later keeps the round-robin until the next NM `up` or a hand restart (since
  the 26.11.2026 load floor that run keeps the boot placement instead of balancing on tiny rates, and an
  idle run never repairs a bad placement: only a run under load does).
- **Rejected:** irqbalance with the OBS CPUs banned (it rebalances every ~10 s, and each move measured
  rx_loss +144 / ~29 JXS lost_slices per camera); CPU 1 alone with RPS/GRO tuning (unmeasured on atlantic, no
  headroom at 86 %).
- **The slice:** `obs_box_background_cpus ONLINE OBS [EXCLUDE]` keeps EXCLUDE out. EXCLUDE =
  `strih_housekeeping_slice_exclude` = the IRQ CPUs that are not task CPUs (strih-snv `1`, the IRQ-only
  CPUs; `none` for a one-CPU set, which would otherwise leave the slice no CPU). setup-strih reads it once
  (`HK_SLICE_EXCL`, fail-loud) and passes it as `obs_box_background_slice`'s 3rd argument; verify-strih (a
  FAIL row when unreadable) as the baseline gather's 5th (`BG_EXCLUDE`, the `bg_exclude` key,
  `obs_box_background_grade`'s 18th): the baseline `background` row then wants exactly the task CPU. imag
  passes nothing (unchanged).
- **The sampler** is not in the slice: it is the operator's `--user` unit with its own `CPUAffinity=`
  (`strih_program_audio_cpus`). On a box with housekeeping CPUs it is the OBS mask
  (`/etc/strih-isolated-cpus.conf`, strih-snv 2-15: 14 CPUs with ~9 idle) with `Nice=10`
  (`strih_program_audio_nice`, the line rendered right under `CPUAffinity=`), so it never shares a CPU
  with the RX softirq again. The earlier "nice 10 cost it audio" finding (program-audio-guard rule) was
  measured on dev1, whose CPUs other projects' CI loaded; here nice 10 only yields to OBS on CPUs that are
  mostly idle. The housekeeping lib must be loaded (setup + verify); a `none` box keeps the E-cores / the
  background set at normal priority.
- Trade-off: two Zen 5 cores carry the RX softirq; the tasks share CPU 0 (with its IRQs) at nice 10.
- **The E2E on-box recording decode is not in the slice either** (finding 7178516822): on a box with
  `/etc/strih-housekeeping-cpus.conf` it runs on the OBS mask under SCHED_IDLE + nice 19 + ionice idle
  (`LOWPRIO_SNIPPET`, `.claude/rules/recording-verdict-on-strih-snv.md`); the slice's one task CPU gave it
  one CPU beside the RX softirq.

## What moves, and how

- **The rig NIC's IRQs + RSS** -- the step-11b oneshot (`strih-nic-irq-affinity.service`, the script from
  `_strih_nic_irq_affinity_script_template` in `scripts/lib/strih-nic-irq.sh`), generalised, never a second
  unit. With housekeeping CPUs: `ethtool -L <nic> combined <count>` ONLY when the current count differs,
  then every IRQ in `/sys/class/net/<nic>/device/msi_irqs`, read AFTER the change (the driver re-requests
  its vectors), pinned and read back as a CPU set; every IRQ tried, then one FATAL naming the failures.
  **A driver with no channel control** (atlantic on strih-snv, 23.11.2026: `ethtool -l` = `netlink error:
  Operation not supported`) keeps its queues (logged, `(hk-rss)` a NOTE) and only its IRQs move; an MSI
  vector listed in msi_irqs with no `/proc/irq/<n>` entry (143 there, never requested) is skipped. A
  USB NIC (no msi_irqs) gets its xhci IRQ pinned and RSS left alone. `none` = the issue-1317 E-core xhci
  placement, unchanged. Step 11b runs on any box with housekeeping CPUs (`obs_box_step_applies 11b`).
- **`ethtool -L` is a ring re-init = a link drop (the issue-1411 atlantic class).** So a udev rule
  (`71-strih-housekeeping-rss.rules`, `DRIVERS==<STRIH_NIC_DRIVER>`, `TEST=="device/msi_irqs"`) sets the count
  at `add`, while the link is still down; the boot oneshot's check is then a no-op. The script logs
  `carrier_changes A -> B` around any -L it does (waiting up to `CARRIER_WAIT_S`, 5 s, for the late drop).
  A verify hint that restarts the oneshot is off-production only.
- **A link reset re-requests the IRQs with the driver's own affinity.** The NetworkManager dispatcher hook
  `60-strih-housekeeping-irq` re-runs the oneshot's script on `up` of a NIC of the fact driver (a VLAN has
  no device/driver link and is skipped).
- **No `net.core.netdev_budget_usecs` change** (dropped 23.11.2026): kernel 7.0 at HZ 1000 refuses anything
  below 2000 us with EINVAL (`sysctl -w` 500 / 1001..1999 read on strih-snv 7.0.0-38), and the box already
  runs 2000. A leftover `/etc/sysctl.d/60-strih-housekeeping.conf` of ours is removed; `(hk-sysctl)` FAILs it
  and otherwise reports the running value.
- `CPUAffinity=<cpus>` + `Nice=10` drop-ins: `rustdesk.service` (system) and `strih-display-watch.service`
  (the operator's `--user` unit). A RUNNING unit whose drop-in changed is restarted; a stopped or
  not-yet-installed one (rustdesk is step 16b, the display watch 16d) is never started.
- **The stat / grep / tail loop dstack's exec snoop saw is ours:** `strih_obs_shutdown_watch` in
  `strih-obs-start.sh` (issue 1512), run by the launcher = `strih-obs.service`'s MainPID. The launcher reads
  `/etc/strih-housekeeping-cpus.conf` and moves ITSELF (`taskset -cp` + `renice --priority 10`, `-n` fallback)
  right before the watch, after OBS was forked, so OBS keeps its own mask. Takes effect at the next OBS start.
- `none` removes ours (header-checked: our header on line 1, or line 2 after `#!/bin/sh`); a foreign file of
  the name is named and left; the live budget stays until the next boot.

## GOTCHA: a live `ethtool -G` on the atlantic rig NIC took strih-snv off the network (24.11.2026)

Under E2E load the AQC113 (`atlantic`, RX ring 2048 of 8184, rx-usecs 112) dropped at DMA level:
InDroppedDma went from 3272 to 30178, and the strih Dante input lost packets that never reached its
socket. dstack confirmed the cause in the NIC, not in the Dante RX path. A boot-oneshot step that
raised the ring to the pre-set maximum was written and applied live (09:38 local). After
`ethtool -G enp7s0 rx 8184` the NIC did not come back (ARP INCOMPLETE for 8+ minutes). It was
reverted before any deploy. Never change the atlantic ring on a live rig NIC again until the failure
is understood on the box's console. Keep the ring out of the NM profile too (the issue-1411 flap
loop). The drop finding stays open on issue 1425.

## Verify item 16 (`strih_housekeeping_grade_report`)

`(hk-cpus)` (also the launcher's file = the task CPUs), `(hk-rss)` (current combined = the IRQ CPU
count), `(hk-irq)` (every rig-NIC IRQ's `smp_affinity_list` = exactly ONE CPU of the housekeeping set:
placement only, the balance is traffic-dependent and never graded), `(hk-hooks)` (the udev rule + the executable hook as rendered), `(hk-rx)`
(REPORT-ONLY NOTE: rx_missed_errors / rx_dropped delta over `STRIH_HOUSEKEEPING_SAMPLE_S`), `(hk-softnet)`
(REPORT-ONLY NOTE over the SAME sample: each housekeeping CPU's `time_squeeze` increase from
`/proc/net/softnet_stat` -- one row per CPU, column 3 hex, column 13 the CPU id, else the row index -- and
the NIC's `InDroppedDma` increase from `ethtool -S`, named "not exposed" when the driver has none), `(hk-sysctl)`
(our leftover drop-in FAILs, else the running value), `(hk-slice)` (`systemctl show -p AllowedCPUs
rig-background.slice` names no IRQ-only CPU; empty / unread FAILs), `(hk-unit)` for rustdesk, the display watch
and the launcher: the RUNNING main process's `Cpus_allowed_list` = the task CPUs + nice (`/proc/<pid>/stat` field 19, read after the last `) `). A unit not running
FAILs; OBS down is a NOTE for the launcher row. `(hk-unit) sampler`: the running program-audio sampler on
the OBS mask at nice 10; not running (EVENT mode) is a NOTE. `(hk-rebalance)`: both re-balance units as rendered
(drift FAILs), the timer enabled (FAILs otherwise), and the service's run this boot from `systemctl show -p
ExecMainStartTimestampMonotonic -p Result -p ExecMainStatus` (systemd keeps it per boot): success + exit 0 =
PASS, a failed run FAILs naming `journalctl -b -u strih-nic-irq-rebalance.service`; not run yet (0) is
graded by the timer's `ActiveState/SubState`: `active/waiting` a NOTE (it runs at 5 min), `inactive/*` a
NOTE when the timer file is newer than the boot (`btime` of `/proc/stat`: setup enables without starting, so
the boot it was installed in never runs it), a FAIL when it predates the boot, anything else
(`active/elapsed`, unreadable) FAILs: the timer fired and the service never ran. `none` also
`reset-failed`s a removed service.

## Deploy (supervisor)

Off-production (the -L drops the rig link once on the date master; schedule it like a dantesync change):
setup-strih (writes everything; enable-only for the oneshot), then `systemctl restart
strih-nic-irq-affinity.service` once (the IRQs spread over 0-1 by a 2 s rate sample; atlantic keeps its queues), restart OBS
for the launcher row, then verify-strih item 16. The spread's own deploy (from the CPU-1 split): setup-strih leaves the slice, the two
drop-ins and the launcher file unchanged (still CPU 0), rewrites the udev rule (count 2; atlantic ignores it) and the
oneshot script; the sampler
unit is rewritten (`CPUAffinity=2-15` + `Nice=10`, design 7119179275) and try-restarted when it runs; the IRQs move at the oneshot restart (or
the next NM `up`), the launcher at the next OBS start. Read before/after: `grep . /proc/irq/*/smp_affinity_list` for the NIC's IRQs, `ethtool
-l <nic>`, `sysctl net.core.netdev_budget_usecs`, `ethtool -S <nic> | grep -iE 'miss|InDroppedDma'`, `carrier_changes`, each
unit's `Cpus_allowed_list`, the oneshot's journal (`journalctl -u strih-nic-irq-affinity` = each IRQ's rate + CPU),
`mpstat -P 0,1 10 1` (%soft per CPU) and `/proc/net/softnet_stat` column 3 (or verify's `(hk-softnet)` NOTE). The re-balance deploy: setup-strih writes the two units and enables the
timer (nothing starts, nothing moves); it first runs at the next boot (`journalctl -b -u
strih-nic-irq-rebalance.service` = each IRQ's rate + CPU then; `systemctl list-timers strih-nic-irq-rebalance.timer`
before), `(hk-rebalance)` reads a NOTE until then.

## Known limit

Not read on the box from this lane: whether atlantic reapplies an affinity hint on a later open, and the
journal of the oneshot / hook at the first boot with the rule (an `RSS queues N -> 2` line there = the
udev rule did not take effect at add). No lock serialises the placement script: an NM `up` around the 5-min
re-balance (or the boot oneshot) can run two placements at once, both writing `smp_affinity` (each writes
only an IRQ whose target differs; the last run wins).

Tests: `tests/python/test_strih_housekeeping_1425.py`. A sampled read (the oneshot's /proc/interrupts rate,
the `(hk-softnet)` softnet_stat / InDroppedDma deltas) is tested with a `sleep` stub first on PATH
(`SWAP_SLEEP`) that copies an "after" fixture over the file being read: the code's own sleep becomes the
moment the counters move, with no extra env seam and no wall-clock wait. Write greedy expectations from a
hand-run of the algorithm (ties to the lower CPU decide the light IRQs).

## Router entry (moved from CLAUDE.md, issue 1552)

- Housekeeping off the OBS CPUs on a strih box (issue 1425: box fact `STRIH_HOUSEKEEPING_CPUS`, strih-snv 0-1; the step-11b oneshot cuts RSS + places every rig-NIC IRQ on ONE housekeeping CPU, balanced by a 2 s rate sample (design 7119179275), the tasks + slice on the first CPU, the program-audio sampler on the OBS mask at nice 10, a udev rule sets RSS at add so no link drop after boot, an NM up hook re-pins, `strih-nic-irq-rebalance.timer` re-balances once 5 min after boot (design 7119686502, verify `(hk-rebalance)`); no netdev_budget_usecs change (kernel 7.0 refuses < 2000); rustdesk + display-watch drop-ins; the launcher moves its own shutdown watch; verify item 16 `(hk-*)` rows) → `.claude/rules/strih-housekeeping.md` (auto-loads on its `paths:`)
