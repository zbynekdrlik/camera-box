---
paths:
  - "scripts/lib/strih-housekeeping.sh"
  - "scripts/lib/strih-nic-irq.sh"
  - "scripts/lib/obs-box-background.sh"
  - "systemd/strih-nic-irq-affinity.service"
  - "scripts/strih-obs-start.sh"
  - "tests/python/test_strih_housekeeping_1425.py"
---

# Housekeeping off the OBS CPUs on a strih box (issue 1425, design 7106973291)

OBS runs on the CPUs in `/etc/strih-isolated-cpus.conf` (strih-snv 2-15). Everything else that wakes up
often belongs on the rest: the box fact `STRIH_HOUSEKEEPING_CPUS` (strih-snv `0-1`, every other box `none`
until measured). It must equal online minus the OBS mask (`obs_box_background_cpus`); verify `(hk-cpus)`
FAILs otherwise.

## The split: the rig NIC IRQs alone on one CPU (design 7116063669, 23.11.2026)

With every housekeeping task and the NIC IRQs on 0-1, dstack read the first rx holes in ~35 h on the
first OBS run (kernel arrival stamps: the RX softirq waited on 0-1, next to the program-audio sampler at
up to 83 % of a core). So the ONE fact is split, never a second fact:
- `strih_housekeeping_irq_cpus [CPUS]` = the LAST CPU of the set (strih-snv `1`): the rig NIC IRQs alone,
  the oneshot's `@STRIH_HOUSEKEEPING_IRQ_CPUS@` (env override `STRIH_HOUSEKEEPING_IRQ_CPUS`), the RSS
  count (oneshot + udev rule = 1 on strih-snv; atlantic has no channel control, so a no-op there);
- `strih_housekeeping_task_cpus [CPUS]` = the rest (strih-snv `0`): rustdesk + the display watch drop-ins,
  `/etc/strih-housekeeping-cpus.conf` (the launcher's own move), `rig-background.slice`;
- a one-CPU set gives that CPU to both; `none` gives `none` to both. Pure halves:
  `obs_box_cpulist_last` / `obs_box_cpulist_rest` in `obs-box-background.sh`.
- **The slice:** `obs_box_background_cpus ONLINE OBS [EXCLUDE]` keeps EXCLUDE out. setup-strih passes
  `strih_housekeeping_irq_cpus` as `obs_box_background_slice`'s 3rd argument, verify-strih as the
  baseline gather's 5th (`BG_EXCLUDE`, the `bg_exclude` key, `obs_box_background_grade`'s 18th): the
  baseline `background` row then wants exactly the task CPUs. imag passes nothing (unchanged).
- **The sampler** is not in the slice: it is the operator's `--user` unit with its own `CPUAffinity=`
  (`strih_program_audio_cpus`); on the AMD strih-snv that is the background set, so it now reads `0`
  too (the IRQ CPU kept out when the housekeeping lib is loaded, i.e. setup + verify). No `Nice=`: the
  program-audio-guard rule measured nice 10 costing it audio; it stays at normal priority.
- Trade-off: all RX queues' softirq on one Zen 5 core (~25 % ksoftirqd measured at the rig rate); the
  tasks share CPU 0 at nice 10.

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

## Verify item 16 (`strih_housekeeping_grade_report`)

`(hk-cpus)` (also the launcher's file = the task CPUs), `(hk-rss)` (current combined = the IRQ CPU
count), `(hk-irq)` (every rig-NIC IRQ's `smp_affinity_list` as a set = the IRQ CPUs), `(hk-hooks)` (the udev rule + the executable hook as rendered), `(hk-rx)`
(REPORT-ONLY NOTE: rx_missed_errors / rx_dropped delta over `STRIH_HOUSEKEEPING_SAMPLE_S`), `(hk-sysctl)`
(our leftover drop-in FAILs, else the running value), `(hk-slice)` (`systemctl show -p AllowedCPUs
rig-background.slice` names no IRQ CPU; empty / unread FAILs), `(hk-unit)` for rustdesk, the display watch
and the launcher: the RUNNING main process's `Cpus_allowed_list` = the task CPUs + nice (`/proc/<pid>/stat` field 19, read after the last `) `). A unit not running
FAILs; OBS down is a NOTE for the launcher row.

## Deploy (supervisor)

Off-production (the -L drops the rig link once on the date master; schedule it like a dantesync change):
setup-strih (writes everything; enable-only for the oneshot), then `systemctl restart
strih-nic-irq-affinity.service` once (the IRQs to the IRQ CPU 1; atlantic keeps its queues), restart OBS
for the launcher row, then verify-strih item 16. The split's own deploy: setup-strih rewrites the slice
(daemon-reload applies its AllowedCPUs to the running slice), the two drop-ins (a running rustdesk / display
watch is restarted onto 0), the launcher file, the udev rule (count 1) and the oneshot script; the sampler
unit is rewritten (`CPUAffinity=0`) and try-restarted when it runs; the IRQs move at the oneshot restart (or
the next NM `up`), the launcher at the next OBS start. Read before/after: `grep . /proc/irq/*/smp_affinity_list` for the NIC's IRQs, `ethtool
-l <nic>`, `sysctl net.core.netdev_budget_usecs`, `ethtool -S <nic> | grep -i miss`, `carrier_changes`, each
unit's `Cpus_allowed_list`.

## Known limit

Not read on the box from this lane: whether atlantic reapplies an affinity hint on a later open, and the
journal of the oneshot / hook at the first boot with the rule (an `RSS queues N -> 2` line there = the
udev rule did not take effect at add).

Tests: `tests/python/test_strih_housekeeping_1425.py`.
