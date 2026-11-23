---
paths:
  - "scripts/lib/strih-housekeeping.sh"
  - "scripts/lib/strih-nic-irq.sh"
  - "systemd/strih-nic-irq-affinity.service"
  - "scripts/strih-obs-start.sh"
  - "tests/python/test_strih_housekeeping_1425.py"
---

# Housekeeping off the OBS CPUs on a strih box (issue 1425, design 7106973291)

OBS runs on the CPUs in `/etc/strih-isolated-cpus.conf` (strih-snv 2-15). Everything else that wakes up
often belongs on the rest: the box fact `STRIH_HOUSEKEEPING_CPUS` (strih-snv `0-1`, every other box `none`
until measured). It must equal online minus the OBS mask (`obs_box_background_cpus`); verify `(hk-cpus)`
FAILs otherwise. The same CPUs carry `rig-background.slice`.

## What moves, and how

- **The rig NIC's IRQs + RSS** -- the step-11b oneshot (`strih-nic-irq-affinity.service`, the script from
  `_strih_nic_irq_affinity_script_template` in `scripts/lib/strih-nic-irq.sh`), generalised, never a second
  unit. With housekeeping CPUs: `ethtool -L <nic> combined <count>` ONLY when the current count differs,
  then every IRQ in `/sys/class/net/<nic>/device/msi_irqs`, read AFTER the change (the driver re-requests
  its vectors), pinned and read back as a CPU set; every IRQ tried, then one FATAL naming the failures. A
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
- `net.core.netdev_budget_usecs = 500` (`/etc/sysctl.d/60-strih-housekeeping.conf`, set live too).
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

`(hk-cpus)`, `(hk-rss)` (current combined = the count), `(hk-irq)` (every rig-NIC IRQ's
`smp_affinity_list` as a set), `(hk-hooks)` (the udev rule + the executable hook as rendered), `(hk-rx)`
(REPORT-ONLY NOTE: rx_missed_errors / rx_dropped delta over `STRIH_HOUSEKEEPING_SAMPLE_S`), `(hk-sysctl)`
(file + live), `(hk-unit)` for rustdesk, the display watch and the launcher: the RUNNING main process's
`Cpus_allowed_list` + nice (`/proc/<pid>/stat` field 19, read after the last `) `). A unit not running
FAILs; OBS down is a NOTE for the launcher row.

## Deploy (supervisor)

Off-production (the -L drops the rig link once on the date master; schedule it like a dantesync change):
setup-strih (writes everything; enable-only for the oneshot), then `systemctl restart
strih-nic-irq-affinity.service` once (8 -> 2 queues, IRQs to 0-1), restart OBS for the launcher row, then
verify-strih item 16. Read before/after: `grep . /proc/irq/*/smp_affinity_list` for the NIC's IRQs, `ethtool
-l <nic>`, `sysctl net.core.netdev_budget_usecs`, `ethtool -S <nic> | grep -i miss`, `carrier_changes`, each
unit's `Cpus_allowed_list`.

## Known limit

Not read on the box from this lane: whether atlantic reapplies an affinity hint on a later open, and the
journal of the oneshot / hook at the first boot with the rule (an `RSS queues N -> 2` line there = the
udev rule did not take effect at add).

Tests: `tests/python/test_strih_housekeeping_1425.py`.
