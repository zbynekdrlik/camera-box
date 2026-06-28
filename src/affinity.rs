//! CPU + IRQ affinity for the realtime capture path (#289).
//!
//! `isolcpus=<N>` on the kernel cmdline RESERVES a core (the scheduler stops
//! balancing general tasks onto it) but pins NOTHING to it. On the cam boxes the
//! reserved core sat idle while the SCHED_FIFO capture/emit thread ran on the
//! loaded general cores (0-2) alongside USB kworkers, rsyslogd, ssh, and (on the
//! painter box) the QR painter — so the grab still wobbled under box load
//! (#289: CAM2 55-60fps wobble + underruns + 28ms head_skew).
//!
//! This module derives the isolated core ROBUSTLY (from `/sys`, not a hardcoded
//! `3`) and pins:
//! - the CAPTURE + EMIT hot thread ONTO the isolated core (alone, immune to box load),
//! - the painter / `--display` render / intercom threads OFF the isolated core (onto 0-2),
//!   so generation can never steal from capture,
//! - the USB capture-controller IRQ onto the isolated core too (`smp_affinity`),
//!   so URB delivery isn't preempted by the loaded general cores.
//!
//! The pure SELECTION + PARSING logic ([`parse_cpulist`], [`select_capture_core`],
//! [`select_painter_cores`], [`parse_capture_irqs`], [`smp_affinity_mask_hex`]) is
//! unit-tested; the syscall/`/proc`/`/sys` IO around it is thin glue.

/// Parse a Linux cpulist string (`/sys/devices/system/cpu/{online,isolated}`,
/// the kernel "0-3" / "3" / "0,2-3" comma+range format) into a sorted, deduped
/// vec of core indices. Whitespace and a trailing newline are tolerated; an
/// empty / unparseable field yields an empty vec.
pub fn parse_cpulist(_s: &str) -> Vec<usize> {
    // stub — implemented in the GREEN commit (#289)
    Vec::new()
}

/// Pick the core the CAPTURE + EMIT hot thread should run on (and the core the
/// USB capture IRQ is routed to).
///
/// Order: an explicit `override_core` that is actually online wins; else the
/// highest ONLINE isolated core (the `isolcpus`-reserved one — isolated cores
/// stay online); else, when there is no isolated core, the highest online core
/// (the "last online core" fallback) PROVIDED there is more than one online
/// core (never strand a single-core box). `None` ⇒ leave the thread unpinned.
pub fn select_capture_core(
    _override_core: Option<usize>,
    _isolated: &[usize],
    _online: &[usize],
) -> Option<usize> {
    // stub — implemented in the GREEN commit (#289)
    None
}

/// Pick the cores the painter / `--display` / intercom threads should run on:
/// every online core EXCEPT the capture core, so generation/render/audio can
/// never steal from the isolated capture core. If excluding the capture core
/// would leave nothing (single-core box), fall back to all online cores.
pub fn select_painter_cores(_capture_core: Option<usize>, _online: &[usize]) -> Vec<usize> {
    // stub — implemented in the GREEN commit (#289)
    Vec::new()
}

/// Parse `/proc/interrupts` and return the IRQ numbers whose description matches
/// any of `keywords` (case-insensitive) — generically discovering the USB /
/// uvcvideo capture-controller IRQ(s) rather than hardcoding a number. Lines
/// with a non-numeric label (`NMI:`, `LOC:`, `ERR:`, the `CPUn` header) are
/// skipped. The result is sorted + deduped.
pub fn parse_capture_irqs(_contents: &str, _keywords: &[&str]) -> Vec<u32> {
    // stub — implemented in the GREEN commit (#289)
    Vec::new()
}

/// Build the `/proc/irq/<n>/smp_affinity` hex bitmask (lowercase, no `0x`) for a
/// set of cores: bit `c` set for each core `c`. `[3]` ⇒ `"8"`, `[0,1,2]` ⇒
/// `"7"`, `[]` ⇒ `"0"`.
pub fn smp_affinity_mask_hex(_cores: &[usize]) -> String {
    // stub — implemented in the GREEN commit (#289)
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cpulist_range() {
        assert_eq!(parse_cpulist("0-3"), vec![0, 1, 2, 3]);
    }

    #[test]
    fn parse_cpulist_single() {
        assert_eq!(parse_cpulist("3"), vec![3]);
    }

    #[test]
    fn parse_cpulist_mixed_list_and_range() {
        assert_eq!(parse_cpulist("0,2-3"), vec![0, 2, 3]);
    }

    #[test]
    fn parse_cpulist_empty() {
        assert_eq!(parse_cpulist(""), Vec::<usize>::new());
    }

    #[test]
    fn parse_cpulist_trailing_newline_and_spaces() {
        // /sys files end with a newline; tolerate spaces too.
        assert_eq!(parse_cpulist("0-2\n"), vec![0, 1, 2]);
        assert_eq!(parse_cpulist(" 1 , 3 "), vec![1, 3]);
    }

    #[test]
    fn parse_cpulist_dedupes_and_sorts() {
        assert_eq!(parse_cpulist("3,0,3,1"), vec![0, 1, 3]);
    }

    #[test]
    fn capture_core_prefers_isolated() {
        // The cam-box case: isolcpus=3, 4 cores online → capture goes on core 3.
        assert_eq!(select_capture_core(None, &[3], &[0, 1, 2, 3]), Some(3));
    }

    #[test]
    fn capture_core_highest_isolated_when_several() {
        assert_eq!(select_capture_core(None, &[2, 3], &[0, 1, 2, 3]), Some(3));
    }

    #[test]
    fn capture_core_falls_back_to_last_online_when_no_isolated() {
        assert_eq!(select_capture_core(None, &[], &[0, 1, 2, 3]), Some(3));
    }

    #[test]
    fn capture_core_none_on_single_core_without_isolation() {
        // Single online core, nothing isolated → don't strand the box.
        assert_eq!(select_capture_core(None, &[], &[0]), None);
    }

    #[test]
    fn capture_core_honours_valid_override() {
        assert_eq!(select_capture_core(Some(2), &[3], &[0, 1, 2, 3]), Some(2));
    }

    #[test]
    fn capture_core_ignores_offline_override() {
        // An override naming a core that isn't online is ignored → derive instead.
        assert_eq!(select_capture_core(Some(9), &[3], &[0, 1, 2, 3]), Some(3));
    }

    #[test]
    fn painter_cores_exclude_capture_core() {
        assert_eq!(select_painter_cores(Some(3), &[0, 1, 2, 3]), vec![0, 1, 2]);
    }

    #[test]
    fn painter_cores_all_online_when_no_capture_core() {
        assert_eq!(select_painter_cores(None, &[0, 1, 2, 3]), vec![0, 1, 2, 3]);
    }

    #[test]
    fn painter_cores_fall_back_when_only_capture_core() {
        // Single-core box: can't separate, so the painter shares it.
        assert_eq!(select_painter_cores(Some(0), &[0]), vec![0]);
    }

    #[test]
    fn painter_cores_keep_higher_cores() {
        assert_eq!(
            select_painter_cores(Some(3), &[0, 1, 2, 3, 4, 5]),
            vec![0, 1, 2, 4, 5]
        );
    }

    #[test]
    fn capture_irqs_finds_usb_controllers_skips_non_numeric() {
        let interrupts = "\
            CPU0       CPU1       CPU2       CPU3
  0:         15          0          0          0   IO-APIC    2-edge      timer
 16:          0          0          0          0   IO-APIC   16-fasteoi   ehci_hcd:usb1
130:       4011          0          0          0   PCI-MSI 327680-edge    xhci_hcd
131:          0          0          0          0   PCI-MSI 327681-edge    xhci_hcd:usb3
NMI:          0          0          0          0   Non-maskable interrupts
LOC:    1000000    1000000    1000000    1000000   Local timer interrupts
";
        assert_eq!(
            parse_capture_irqs(interrupts, &["xhci", "ehci", "ohci", "uvcvideo", "usb"]),
            vec![16, 130, 131]
        );
    }

    #[test]
    fn capture_irqs_matches_uvcvideo() {
        let interrupts = " 42:   10  0  0  0  PCI-MSI 1-edge  uvcvideo\n";
        assert_eq!(parse_capture_irqs(interrupts, &["uvcvideo"]), vec![42]);
    }

    #[test]
    fn capture_irqs_empty_when_no_match() {
        let interrupts = " 0: 15 0 0 0 IO-APIC 2-edge timer\n";
        assert_eq!(
            parse_capture_irqs(interrupts, &["xhci", "usb"]),
            Vec::<u32>::new()
        );
    }

    #[test]
    fn smp_mask_isolated_core_three() {
        assert_eq!(smp_affinity_mask_hex(&[3]), "8");
    }

    #[test]
    fn smp_mask_general_cores() {
        assert_eq!(smp_affinity_mask_hex(&[0, 1, 2]), "7");
    }

    #[test]
    fn smp_mask_single_and_empty() {
        assert_eq!(smp_affinity_mask_hex(&[0]), "1");
        assert_eq!(smp_affinity_mask_hex(&[]), "0");
    }

    #[test]
    fn smp_mask_disjoint_cores() {
        assert_eq!(smp_affinity_mask_hex(&[0, 3]), "9");
    }
}
