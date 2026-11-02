//! issue 1367: keep every OTHER device IRQ off the reserved capture-IRQ core.
//!
//! The issue-1198 split gives the xhci capture IRQ ONE reserved general core
//! (`affinity::select_irq_reserved_core`). The kernel cmdline `irqaffinity=0-2` still lets the
//! kernel place any other device IRQ on that core, and it did: the cambox NIC IRQ (`enp1s0`) sat on
//! the reserved core on cam3 and cam5. In an E2E the network softirq there delayed the capture URB
//! completion; moving the NIC IRQ off it cut the capture-phase re-seeds of the cam5 leg from 22-31
//! per run to 2 (15.10.2026). `affinity::setup_irq_affinity` uses these pure decisions to move every
//! non-capture device IRQ whose affinity includes the reserved core onto the painter/housekeeping
//! cores, at every camera-box start (no GRUB change, no reboot).

/// Every numbered IRQ in `/proc/interrupts` (device IRQs; `NMI:`, `LOC:` and the header are
/// skipped), sorted and unique.
pub fn parse_device_irqs(contents: &str) -> Vec<u32> {
    let mut out: Vec<u32> = contents
        .lines()
        .filter_map(|line| line.split_once(':'))
        .filter_map(|(label, _)| label.trim().parse::<u32>().ok())
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// The cores of a `/proc/irq/<n>/smp_affinity` hex mask (`"8"`, `"0000000c"`,
/// `"00000000,00000004"`), ascending. Unparseable text gives `None`.
pub fn mask_hex_to_cores(mask: &str) -> Option<Vec<usize>> {
    let groups: Vec<&str> = mask.trim().split(',').collect();
    let mut cores = Vec::new();
    for (gi, group) in groups.iter().rev().enumerate() {
        let g = group.trim();
        if g.is_empty() || g.len() > 8 {
            return None;
        }
        let bits = u32::from_str_radix(g, 16).ok()?;
        for b in 0..32 {
            if bits & (1u32 << b) != 0 {
                cores.push(gi * 32 + b as usize);
            }
        }
    }
    cores.sort_unstable();
    Some(cores)
}

/// The cores a non-capture device IRQ is moved to: the online cores minus the capture core, minus
/// every isolated core and minus the reserved capture-IRQ core (on the 4-core fleet `[0, 1]`). This
/// is the housekeeping set `scripts/lib/cambox-housekeeping-cpus.sh` derives for systemd PID 1, so
/// the IRQs and PID 1 share one rule. `None` when there is no reserved core (a PREEMPT_RT kernel, or
/// a box too small to reserve one) or nothing would be left, so the caller changes nothing.
pub fn select_offload_cores(
    capture_core: usize,
    reserved: Option<usize>,
    isolated: &[usize],
    online: &[usize],
) -> Option<Vec<usize>> {
    let reserved = reserved?;
    let cores: Vec<usize> = online
        .iter()
        .copied()
        .filter(|&c| c != capture_core && c != reserved && !isolated.contains(&c))
        .collect();
    if cores.is_empty() {
        None
    } else {
        Some(cores)
    }
}

/// True when an IRQ whose affinity is `current` may run on `reserved`, i.e. it must be moved.
pub fn needs_offload(current: &[usize], reserved: usize) -> bool {
    current.contains(&reserved)
}

/// The planned offload: every non-capture device IRQ whose current affinity includes the reserved
/// core, with the mask text it had (`moves`), and how many were already off it (`kept`). An IRQ whose
/// mask cannot be read or parsed is left alone (counted nowhere).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct OffloadPlan {
    pub moves: Vec<(u32, String)>,
    pub kept: usize,
}

/// Plan the offload from the `/proc/interrupts` text. `read_mask(irq)` returns that IRQ's
/// `smp_affinity` text (`None` = unreadable). The capture IRQs are NEVER moved: they were just put
/// ON the reserved core by the issue-1198 split.
pub fn plan_offload(
    interrupts: &str,
    capture_irqs: &[u32],
    reserved: usize,
    read_mask: impl Fn(u32) -> Option<String>,
) -> OffloadPlan {
    let mut plan = OffloadPlan::default();
    for irq in parse_device_irqs(interrupts) {
        if capture_irqs.contains(&irq) {
            continue;
        }
        let Some(prev) = read_mask(irq) else {
            continue;
        };
        let Some(current) = mask_hex_to_cores(&prev) else {
            continue;
        };
        if needs_offload(&current, reserved) {
            plan.moves.push((irq, prev.trim().to_string()));
        } else {
            plan.kept += 1;
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAM5_INTERRUPTS: &str = "\
            CPU0       CPU1       CPU2       CPU3
   0:         34          0          0          0  IR-IO-APIC    2-edge      timer
 124:          0          0          0          0  IR-PCI-MSI-0000:00:0d.0    0-edge      xhci_hcd
 129:          0          0 1014450152          0  IR-PCI-MSI-0000:00:14.0    0-edge      xhci_hcd
 135:          0          0 1188503325          0  IR-PCI-MSIX-0000:01:00.0    0-edge      enp1s0
 136:          0          0          0          0  IR-PCI-MSIX-0000:02:00.0    0-edge      enp2s0
 NMI:          0          0          0          0   Non-maskable interrupts
 LOC:   12345678   12345678   12345678   12345678   Local timer interrupts
";

    #[test]
    fn every_numbered_irq_is_a_device_irq() {
        assert_eq!(
            parse_device_irqs(CAM5_INTERRUPTS),
            vec![0, 124, 129, 135, 136]
        );
    }

    #[test]
    fn masks_in_every_kernel_rendering_decode_to_cores() {
        assert_eq!(mask_hex_to_cores("4"), Some(vec![2]));
        assert_eq!(mask_hex_to_cores("00000007\n"), Some(vec![0, 1, 2]));
        assert_eq!(mask_hex_to_cores("00000001,00000004"), Some(vec![2, 32]));
        assert_eq!(mask_hex_to_cores("0"), Some(vec![]));
        assert_eq!(mask_hex_to_cores("zz"), None);
        assert_eq!(mask_hex_to_cores(""), None);
    }

    #[test]
    fn the_four_core_fleet_offloads_onto_0_and_1() {
        assert_eq!(
            select_offload_cores(3, Some(2), &[3], &[0, 1, 2, 3]),
            Some(vec![0, 1])
        );
    }

    #[test]
    fn a_spare_isolated_core_never_takes_an_offloaded_irq() {
        // the housekeeping rule of cambox-housekeeping-cpus.sh: isolated 2,3, capture 3, reserved 1
        assert_eq!(
            select_offload_cores(3, Some(1), &[2, 3], &[0, 1, 2, 3]),
            Some(vec![0])
        );
    }

    #[test]
    fn no_reserved_core_means_nothing_moves() {
        assert_eq!(select_offload_cores(3, None, &[3], &[0, 1, 2, 3]), None);
        assert_eq!(select_offload_cores(1, Some(0), &[1], &[0, 1]), None);
    }

    #[test]
    fn an_irq_that_may_run_on_the_reserved_core_is_moved() {
        assert!(needs_offload(&[2], 2));
        assert!(needs_offload(&[0, 1, 2], 2));
        assert!(!needs_offload(&[0, 1], 2));
        assert!(!needs_offload(&[], 2));
    }

    fn masks<'a>(table: &'a [(u32, &'a str)]) -> impl Fn(u32) -> Option<String> + 'a {
        move |irq| {
            table
                .iter()
                .find(|(n, _)| *n == irq)
                .map(|(_, m)| m.to_string())
        }
    }

    #[test]
    fn the_capture_irq_on_the_reserved_core_is_never_planned_off_it() {
        // 129 = the capture xhci IRQ the issue-1198 split just put on core 2
        let plan = plan_offload(
            CAM5_INTERRUPTS,
            &[129],
            2,
            masks(&[(0, "1"), (124, "7"), (129, "4"), (135, "4"), (136, "3")]),
        );
        assert!(plan.moves.iter().all(|(irq, _)| *irq != 129), "{plan:?}");
        assert_eq!(
            plan.moves,
            vec![(124, "7".to_string()), (135, "4".to_string())]
        );
        assert_eq!(plan.kept, 2);
    }

    #[test]
    fn an_unreadable_or_garbled_mask_is_left_alone() {
        let plan = plan_offload(
            CAM5_INTERRUPTS,
            &[129],
            2,
            masks(&[(0, "zz"), (135, "00000004\n")]),
        );
        assert_eq!(plan.moves, vec![(135, "00000004".to_string())]);
        assert_eq!(plan.kept, 0);
    }
}
