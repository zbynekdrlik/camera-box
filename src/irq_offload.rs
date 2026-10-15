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
pub fn parse_device_irqs(_contents: &str) -> Vec<u32> {
    Vec::new()
}

/// The cores of a `/proc/irq/<n>/smp_affinity` hex mask (`"8"`, `"0000000c"`,
/// `"00000000,00000004"`), ascending. Unparseable text gives `None`.
pub fn mask_hex_to_cores(_mask: &str) -> Option<Vec<usize>> {
    None
}

/// The cores a non-capture device IRQ is moved to: the general cores minus the reserved
/// capture-IRQ core (on the 4-core fleet `[0, 1]`). `None` when there is no reserved core (a
/// PREEMPT_RT kernel, or a box too small to reserve one) or nothing would be left, so the caller
/// changes nothing.
pub fn select_offload_cores(
    _capture_core: usize,
    _reserved: Option<usize>,
    _online: &[usize],
) -> Option<Vec<usize>> {
    None
}

/// True when an IRQ whose affinity is `current` may run on `reserved`, i.e. it must be moved.
pub fn needs_offload(_current: &[usize], _reserved: usize) -> bool {
    false
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
        assert_eq!(parse_device_irqs(CAM5_INTERRUPTS), vec![0, 124, 129, 135, 136]);
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
        assert_eq!(select_offload_cores(3, Some(2), &[0, 1, 2, 3]), Some(vec![0, 1]));
    }

    #[test]
    fn no_reserved_core_means_nothing_moves() {
        assert_eq!(select_offload_cores(3, None, &[0, 1, 2, 3]), None);
        assert_eq!(select_offload_cores(1, Some(0), &[0, 1]), None);
    }

    #[test]
    fn an_irq_that_may_run_on_the_reserved_core_is_moved() {
        assert!(needs_offload(&[2], 2));
        assert!(needs_offload(&[0, 1, 2], 2));
        assert!(!needs_offload(&[0, 1], 2));
        assert!(!needs_offload(&[], 2));
    }
}
