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
//! - the USB capture-controller IRQ, on a stock (non-PREEMPT_RT) kernel, onto its OWN
//!   dedicated general core — the highest non-capture core (`smp_affinity`) — so the
//!   non-preemptible xhci hardirq that delivers URBs never shares a core with the
//!   painter/display load (issue 1198). On a PREEMPT_RT kernel the handler is threaded
//!   and sub-grab priority, so it is co-located on the isolated capture core instead
//!   (#289 intent, issue 899 defect 3),
//! - the painter / `--display` render / intercom threads OFF BOTH the capture core AND
//!   the dedicated capture-IRQ core, onto the remaining general cores, so neither the
//!   grab nor URB delivery can be stolen by generation.
//!
//! Why the dedicated IRQ core (issue 1198): before it, the non-RT capture IRQ set and
//! the painter set were the SAME general cores (e.g. both `[0,1,2]` on the 4-core cam
//! boxes). Isolating the capture CONSUMER thread on core 3 did nothing for URB
//! DELIVERY — the cam1 #528 HDMI-preview 1080p scale on `[0,1,2]` delayed the xhci
//! hardirq/softirq, the URB completed late, and the frame was dropped (58 fps captured
//! vs 60 emitted, gone entirely with the preview off). Giving the IRQ its own quiet
//! core the painter never touches removes that contention.
//!
//! The pure SELECTION + PARSING logic ([`parse_cpulist`], [`select_capture_core`],
//! [`select_painter_cores`], [`select_irq_reserved_core`], [`select_irq_target_cores`],
//! [`parse_capture_irqs`], [`smp_affinity_mask_hex`]) is unit-tested; the
//! syscall/`/proc`/`/sys` IO around it is thin glue.

/// Ops escape-hatch env var: force the capture core to an explicit index. UNSET
/// (the default) ⇒ auto-derive from `/sys` (the `isolcpus`-reserved core). Only
/// honoured when it names an online core; otherwise ignored and derivation wins.
const CAPTURE_CORE_ENV: &str = "CAMERA_BOX_CAPTURE_CORE";

/// Keywords for generic capture-IRQ discovery in `/proc/interrupts`. The
/// ShadowCast / NZXT capture cards are UVC-over-USB, so their data delivery is
/// the USB HOST-CONTROLLER IRQ (xHCI/EHCI/OHCI) plus, where present, a uvcvideo
/// line — never a hardcoded IRQ number. Deliberately NOT the bare `"usb"`: that
/// also matches unrelated `usbN` device lines (usbhid, usb-storage, a USB NIC),
/// which would drag non-capture interrupts onto the isolated core. The host
/// controller keywords already catch the `xhci_hcd:usbN` line the capture card sits on.
const CAPTURE_IRQ_KEYWORDS: &[&str] = &["xhci", "ehci", "ohci", "uvcvideo"];

/// Parse a Linux cpulist string (`/sys/devices/system/cpu/{online,isolated}`,
/// the kernel "0-3" / "3" / "0,2-3" comma+range format) into a sorted, deduped
/// vec of core indices. Whitespace and a trailing newline are tolerated; an
/// empty / unparseable field yields an empty vec.
pub fn parse_cpulist(s: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for part in s.trim().split(',') {
        // An empty / whitespace-only field falls through both arms (the `parse`
        // below returns Err) and contributes nothing — no explicit guard needed.
        let part = part.trim();
        if let Some((a, b)) = part.split_once('-') {
            if let (Ok(a), Ok(b)) = (a.trim().parse::<usize>(), b.trim().parse::<usize>()) {
                for c in a..=b {
                    out.push(c);
                }
            }
        } else if let Ok(c) = part.parse::<usize>() {
            out.push(c);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
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
    override_core: Option<usize>,
    isolated: &[usize],
    online: &[usize],
) -> Option<usize> {
    if let Some(c) = override_core {
        if online.contains(&c) {
            return Some(c);
        }
    }
    if let Some(&c) = isolated.iter().filter(|c| online.contains(c)).max() {
        return Some(c);
    }
    if online.len() > 1 {
        return online.iter().copied().max();
    }
    None
}

/// Pick the cores the painter / `--display` / intercom threads should run on:
/// every online core EXCEPT the capture core AND the dedicated capture-IRQ core
/// (`reserved_irq_core`, issue 1198), so generation/render/audio can never steal
/// from either the isolated grab or URB delivery. `reserved_irq_core` is `None`
/// on a PREEMPT_RT kernel (the IRQ is co-located on the capture core, so no
/// general core is reserved) and on any box too small to reserve one — in those
/// cases only the capture core is excluded. If excluding leaves nothing (a
/// single-core box), fall back to all online cores; the painter is never stranded
/// on an empty set.
pub fn select_painter_cores(
    capture_core: Option<usize>,
    reserved_irq_core: Option<usize>,
    online: &[usize],
) -> Vec<usize> {
    let cores: Vec<usize> = online
        .iter()
        .copied()
        .filter(|&c| Some(c) != capture_core && Some(c) != reserved_irq_core)
        .collect();
    if cores.is_empty() {
        online.to_vec()
    } else {
        cores
    }
}

/// Pick the ONE general core to DEDICATE to the USB capture IRQ on a non-RT
/// kernel (issue 1198), so URB delivery never shares a core with the painter /
/// display / intercom threads.
///
/// Returns the highest online NON-capture core, but ONLY when doing so still
/// leaves the painter at least one core to run on — i.e. there are at least TWO
/// general (non-capture) cores. With fewer than that (a 2-core box where
/// reserving one would strand the painter, or a 1-core box) it returns `None`,
/// and the caller keeps today's shared-general-core behaviour rather than ever
/// stranding the painter on an empty mask.
pub fn select_irq_reserved_core(capture_core: usize, online: &[usize]) -> Option<usize> {
    let general: Vec<usize> = online
        .iter()
        .copied()
        .filter(|&c| c != capture_core)
        .collect();
    if general.len() < 2 {
        return None;
    }
    general.into_iter().max()
}

/// Parse `/proc/interrupts` and return the IRQ numbers whose description matches
/// any of `keywords` (case-insensitive) — generically discovering the USB /
/// uvcvideo capture-controller IRQ(s) rather than hardcoding a number. Lines
/// with a non-numeric label (`NMI:`, `LOC:`, `ERR:`, the `CPUn` header) are
/// skipped. The result is sorted + deduped.
pub fn parse_capture_irqs(contents: &str, keywords: &[&str]) -> Vec<u32> {
    let lc: Vec<String> = keywords.iter().map(|k| k.to_ascii_lowercase()).collect();
    let mut out = Vec::new();
    for line in contents.lines() {
        let Some((label, rest)) = line.split_once(':') else {
            continue;
        };
        let Ok(irq) = label.trim().parse::<u32>() else {
            continue;
        };
        let desc = rest.to_ascii_lowercase();
        if lc.iter().any(|k| desc.contains(k.as_str())) {
            out.push(irq);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Build the `/proc/irq/<n>/smp_affinity` hex bitmask (lowercase, no `0x`) for a
/// set of cores: bit `c` set for each core `c`. `[3]` ⇒ `"8"`, `[0,1,2]` ⇒
/// `"7"`, `[]` ⇒ `"0"`.
pub fn smp_affinity_mask_hex(cores: &[usize]) -> String {
    let mut mask: u64 = 0;
    for &c in cores {
        if c < 64 {
            mask |= 1u64 << c;
        }
    }
    format!("{mask:x}")
}

/// Normalise a `/proc/irq/<n>/smp_affinity` mask string for comparison: drop the
/// comma CPU-group separators, lowercase, and strip leading zeros — so the
/// kernel's zero-padded / comma-grouped rendering (`"00000008"` or
/// `"00000000,00000008"`) compares equal to our compact `"8"`. An all-zero /
/// empty mask normalises to `"0"`.
fn normalize_affinity_mask(s: &str) -> String {
    let compact = s.trim().replace(',', "").to_ascii_lowercase();
    let trimmed = compact.trim_start_matches('0');
    if trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Detect a PREEMPT_RT kernel from `/proc/version` text plus the optional
/// `/sys/kernel/realtime` flag (issue 899). True iff the kernel is fully
/// realtime-preemptible: `proc_version` contains the marker string `PREEMPT_RT`,
/// OR `/sys/kernel/realtime` (present only on RT builds) trims to `"1"`.
///
/// The marker is `PREEMPT_RT` specifically — a plain substring test is sufficient
/// because the only preemption strings the kernel emits are `PREEMPT_RT` /
/// `PREEMPT_RT_FULL` (genuinely realtime, both contain it) vs `PREEMPT_DYNAMIC` /
/// `PREEMPT_VOLUNTARY` / `PREEMPT_LAZY` (NOT realtime, none contain `PREEMPT_RT`).
/// So the stock cam-box kernel (`PREEMPT_DYNAMIC`) correctly reads as NON-RT; a
/// bare `PREEMPT` substring would have wrongly classified it. On an RT kernel
/// hardirq/softirq handlers are threaded and schedulable, so routing the capture
/// IRQ onto the isolated core is defensible; on a non-RT kernel it is not (issue
/// 899 defect 3).
pub fn kernel_is_preempt_rt(proc_version: &str, sys_realtime: Option<&str>) -> bool {
    if proc_version.contains("PREEMPT_RT") {
        return true;
    }
    matches!(sys_realtime, Some(v) if v.trim() == "1")
}

/// Decide which cores the USB capture IRQ should be routed to, given the kernel
/// realtime status (issue 899 defect 3).
///
/// - **PREEMPT_RT kernel** → route ONTO the isolated capture core (`[capture_core]`):
///   the handler is a schedulable thread whose priority sits below the grab, so
///   co-locating URB delivery next to its consumer is the design intent (#289).
/// - **non-RT kernel** → route onto ONE dedicated general core
///   ([`select_irq_reserved_core`], the highest non-capture core), so the
///   non-preemptible hardirq neither steals from the prio-90 FIFO grab NOR shares
///   a core with the painter/display load that would delay URB delivery (issue
///   1198). On a box too small to reserve one without stranding the painter, it
///   falls back to today's behaviour — all general cores (`online` minus
///   `capture_core`), or `[capture_core]` when there is no OTHER online core (a
///   single-core box) — so the IRQ is never stranded on an empty mask.
pub fn select_irq_target_cores(
    is_preempt_rt: bool,
    capture_core: usize,
    online: &[usize],
) -> Vec<usize> {
    if is_preempt_rt {
        // RT: threaded handler below the grab's priority → co-locate on the core.
        return vec![capture_core];
    }
    // non-RT (issue 1198): dedicate ONE general core to the capture IRQ so the
    // non-preemptible xhci handler never shares a core with the painter/display
    // threads (which starved URB delivery on cam1's #528 preview box).
    if let Some(reserved) = select_irq_reserved_core(capture_core, online) {
        return vec![reserved];
    }
    // Too small to reserve one without stranding the painter → keep today's
    // behaviour: every general core, or the capture core if there is no other.
    let general: Vec<usize> = online
        .iter()
        .copied()
        .filter(|&c| c != capture_core)
        .collect();
    if general.is_empty() {
        vec![capture_core]
    } else {
        general
    }
}

// ---------------------------------------------------------------------------
// IO / syscall glue around the pure logic above (not unit-tested — reads /sys,
// /proc, calls sched_setaffinity).
// ---------------------------------------------------------------------------

/// The optional ops override from [`CAPTURE_CORE_ENV`].
fn env_capture_core() -> Option<usize> {
    std::env::var(CAPTURE_CORE_ENV)
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
}

/// Online cores from `/sys/devices/system/cpu/online`; falls back to
/// `sysconf(_SC_NPROCESSORS_ONLN)` if the file can't be read.
fn read_online_cores() -> Vec<usize> {
    match std::fs::read_to_string("/sys/devices/system/cpu/online") {
        Ok(s) => parse_cpulist(&s),
        Err(e) => {
            tracing::debug!("affinity: could not read cpu/online ({e}); using sysconf nproc");
            // SAFETY: sysconf is a pure query with no pointer arguments.
            let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
            if n > 0 {
                (0..n as usize).collect()
            } else {
                Vec::new()
            }
        }
    }
}

/// Isolated (`isolcpus`) cores from `/sys/devices/system/cpu/isolated`; empty if
/// the file is absent or no core is isolated.
fn read_isolated_cores() -> Vec<usize> {
    std::fs::read_to_string("/sys/devices/system/cpu/isolated")
        .map(|s| parse_cpulist(&s))
        .unwrap_or_default()
}

/// Whether the running kernel is PREEMPT_RT (issue 899), read from `/proc/version`
/// plus the RT-only `/sys/kernel/realtime` flag. Both reads are best-effort — an
/// absent/unreadable file just contributes nothing, so a stock kernel reads as
/// non-RT (the conservative default: route the capture IRQ off the grab core).
fn read_kernel_is_preempt_rt() -> bool {
    let proc_version = std::fs::read_to_string("/proc/version").unwrap_or_default();
    let sys_realtime = std::fs::read_to_string("/sys/kernel/realtime").ok();
    kernel_is_preempt_rt(&proc_version, sys_realtime.as_deref())
}

/// Pin the CURRENT thread to `cores` via `sched_setaffinity`. Returns whether the
/// syscall succeeded. Pinning the calling thread to a subset of cores needs no
/// privileges.
fn pin_current_thread(cores: &[usize]) -> bool {
    if cores.is_empty() {
        return false;
    }
    // SAFETY: `set` is zero-initialised then populated only via the libc CPU_SET
    // macro for in-range indices; sched_setaffinity(0, ...) targets the current
    // thread (always permitted) and reads `set` for the passed size only.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        for &c in cores {
            libc::CPU_SET(c, &mut set);
        }
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) == 0
    }
}

/// Pin the calling thread (the capture + NDI-emit hot path) onto the isolated
/// core. Call this ONCE from the capture thread before the grab loop.
pub fn pin_capture_thread() {
    let online = read_online_cores();
    let isolated = read_isolated_cores();
    match select_capture_core(env_capture_core(), &isolated, &online) {
        Some(core) => {
            if pin_current_thread(&[core]) {
                tracing::info!(
                    "#289 capture+emit thread pinned to isolated core {core} (isolated={isolated:?}, online={online:?})"
                );
            } else {
                tracing::warn!(
                    "#289 could not pin capture+emit thread to core {core} (sched_setaffinity failed)"
                );
            }
        }
        None => tracing::warn!(
            "#289 no isolated core derived (online={online:?}); capture+emit thread left unpinned"
        ),
    }
}

/// Pin the calling thread (a painter / `--display` render / intercom auxiliary
/// thread, `label` for the log) OFF the capture core, onto the general cores.
pub fn pin_off_capture_core(label: &str) {
    let online = read_online_cores();
    let isolated = read_isolated_cores();
    let capture = select_capture_core(env_capture_core(), &isolated, &online);
    // issue 1198: on a non-RT kernel the capture IRQ gets its own dedicated general
    // core; keep the painter OFF that core too. On a PREEMPT_RT kernel the IRQ is
    // co-located on the capture core (no general core reserved), so pass None and
    // the painter keeps all general cores.
    let reserved = match capture {
        Some(cc) if !read_kernel_is_preempt_rt() => select_irq_reserved_core(cc, &online),
        _ => None,
    };
    let cores = select_painter_cores(capture, reserved, &online);
    if pin_current_thread(&cores) {
        tracing::info!(
            "#289/1198 {label} thread pinned OFF the capture core to {cores:?} (capture core={capture:?}, reserved capture-IRQ core={reserved:?})"
        );
    } else {
        tracing::warn!("#289 {label} thread: could not pin to non-capture cores {cores:?}");
    }
}

/// Route the USB / uvcvideo capture-controller IRQ(s) onto the isolated capture
/// core via `/proc/irq/<n>/smp_affinity`, so URB delivery runs on the quiet core
/// next to the consumer instead of contending on the loaded general cores.
///
/// Invoked once as `camera-box --setup-irq-affinity` from the unit's
/// `ExecStartPre` (needs root to write `/proc/irq`). Idempotent and entirely
/// best-effort: every failure is logged and swallowed so it can NEVER block the
/// service from starting (managed MSI IRQs reject `smp_affinity` writes — the
/// cmdline `irqaffinity=0-2` path for those now ships via `setup-device.sh` STEP 10,
/// #303, riding the #295 safe-grub mechanism; it still needs a box reboot onto the
/// new cmdline to take effect).
pub fn setup_irq_affinity() {
    let online = read_online_cores();
    let isolated = read_isolated_cores();
    let Some(core) = select_capture_core(env_capture_core(), &isolated, &online) else {
        tracing::warn!(
            "#289 IRQ affinity: no isolated/capture core derived (online={online:?}); leaving IRQ routing untouched"
        );
        return;
    };
    let interrupts = match std::fs::read_to_string("/proc/interrupts") {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                "#289 IRQ affinity: could not read /proc/interrupts ({e}); leaving IRQ routing untouched"
            );
            return;
        }
    };
    let irqs = parse_capture_irqs(&interrupts, CAPTURE_IRQ_KEYWORDS);
    if irqs.is_empty() {
        tracing::warn!(
            "#289 IRQ affinity: no USB/uvcvideo capture IRQs found in /proc/interrupts; nothing to route"
        );
        return;
    }
    // issue 899 defect 3: on a stock (non-PREEMPT_RT) kernel the xhci hardirq
    // handler is NOT threaded, so routing it onto the isolated grab core steals
    // cycles from even the prio-90 FIFO grab. Route it onto the general cores
    // instead; only on a real PREEMPT_RT kernel (threaded, sub-grab priority) do
    // we co-locate it on the capture core, as #289 intended.
    let is_rt = read_kernel_is_preempt_rt();
    let target = select_irq_target_cores(is_rt, core, &online);
    let mask = smp_affinity_mask_hex(&target);
    // issue 1198: state the INTENDED core split at startup so a live box shows which
    // core we are about to give the capture IRQ and which cores the painter/display
    // got. This is INTENT, not proof — a managed MSI IRQ rejects the smp_affinity
    // write below and stays on the cmdline irqaffinity=0-2 default (shared with the
    // painter — the exact 1198 failure mode), so the actual outcome is reported AFTER
    // the write loop, and the real confirmation is the supervisor's live
    // smp_affinity_list read.
    let reserved = if is_rt {
        None
    } else {
        select_irq_reserved_core(core, &online)
    };
    // The logged split matches the routed target by construction (both derive from
    // the same pure selectors on the same inputs); guard against a future edit to
    // select_irq_target_cores' non-RT branch silently desynchronising the two.
    debug_assert!(
        reserved.map_or(true, |r| target == [r]),
        "#1198 logged reserved core {reserved:?} must match routed IRQ target {target:?}"
    );
    let painter = select_painter_cores(Some(core), reserved, &online);
    tracing::info!(
        "#1198 INTENDED core split: capture={core}, capture-IRQ={target:?}, painter/display={painter:?} (online={online:?}, preempt_rt={is_rt})"
    );
    tracing::info!(
        "#289/899 IRQ affinity: kernel preempt_rt={is_rt}; routing capture IRQs {irqs:?} to cores {target:?} (capture core={core}, smp_affinity={mask})"
    );
    let total = irqs.len();
    let mut applied = 0usize;
    let mut unchanged = 0usize;
    let mut failed = 0usize;
    for irq in irqs {
        let path = format!("/proc/irq/{irq}/smp_affinity");
        let prev = std::fs::read_to_string(&path).unwrap_or_default();
        let prev = prev.trim().to_string();
        // The kernel renders smp_affinity zero-padded / comma-grouped (e.g.
        // "00000008" or "00000000,00000008"), so compare NORMALISED bitmasks —
        // not the raw strings — to keep the idempotency fast-path + log honest.
        if normalize_affinity_mask(&prev) == normalize_affinity_mask(&mask) {
            tracing::info!("#289 IRQ {irq}: smp_affinity already {mask} — unchanged");
            unchanged += 1;
            continue;
        }
        match std::fs::write(&path, format!("{mask}\n")) {
            Ok(()) => {
                applied += 1;
                tracing::info!("#289/899 IRQ {irq}: smp_affinity {prev} -> {mask} (target cores {target:?}, rt={is_rt}, capture core {core})")
            }
            // Managed (kernel-affinity) MSI IRQs reject smp_affinity writes (EIO) — non-fatal:
            // the cmdline irqaffinity=0-2 path that covers those ships via setup-device.sh STEP 10
            // (#303, on the #295 safe-grub path); it takes effect after the box reboots onto it.
            Err(e) => {
                failed += 1;
                tracing::warn!(
                    "#289 IRQ {irq}: could not set smp_affinity to {mask} ({e}) — likely a managed IRQ (needs cmdline irqaffinity=0-2, #303)"
                )
            }
        }
    }
    // issue 1198: report whether the intended split was ACTUALLY programmed. A
    // managed-MSI rejection leaves the IRQ on the cmdline default, SHARED with the
    // painter — so a green "applied" line, not the intent line above, is the honest
    // in-journal signal; the supervisor's live smp_affinity_list read is the ground truth.
    if failed == 0 {
        tracing::info!(
            "#1198 core split APPLIED: capture-IRQ on cores {target:?} ({applied} written, {unchanged} already correct of {total}); painter/display on {painter:?}, capture on {core}"
        );
    } else {
        tracing::warn!(
            "#1198 core split NOT fully applied: {failed}/{total} capture IRQ(s) rejected the smp_affinity write (managed MSI → still on the cmdline irqaffinity default, SHARED with the painter — the 1198 failure mode); {applied} written, {unchanged} already correct. Needs the cmdline irqaffinity path (#303)."
        );
    }
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
    fn capture_core_skips_isolated_core_that_is_not_online() {
        // An isolated entry that is not online is ignored → highest ONLINE
        // isolated core wins (core 5 is isolated but offline → use 3).
        assert_eq!(select_capture_core(None, &[3, 5], &[0, 1, 2, 3]), Some(3));
    }

    #[test]
    fn painter_cores_exclude_capture_core() {
        // issue 1198: signature gained `reserved_irq_core`. With None reserved (the
        // PREEMPT_RT path, where the IRQ sits on the capture core) the painter still
        // gets every general core, exactly as before this ticket.
        assert_eq!(
            select_painter_cores(Some(3), None, &[0, 1, 2, 3]),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn painter_cores_exclude_capture_and_reserved_irq_core() {
        // issue 1198: the non-RT fleet case — painter excludes BOTH the capture core
        // (3) and the dedicated capture-IRQ core (2), leaving [0,1].
        assert_eq!(
            select_painter_cores(Some(3), Some(2), &[0, 1, 2, 3]),
            vec![0, 1]
        );
    }

    #[test]
    fn painter_cores_all_online_when_no_capture_core() {
        assert_eq!(
            select_painter_cores(None, None, &[0, 1, 2, 3]),
            vec![0, 1, 2, 3]
        );
    }

    #[test]
    fn painter_cores_fall_back_when_only_capture_core() {
        // Single-core box: can't separate, so the painter shares it.
        assert_eq!(select_painter_cores(Some(0), None, &[0]), vec![0]);
    }

    #[test]
    fn painter_cores_keep_higher_cores() {
        // issue 1198: with reserved=5 excluded too (capture=3), painter keeps the rest.
        assert_eq!(
            select_painter_cores(Some(3), Some(5), &[0, 1, 2, 3, 4, 5]),
            vec![0, 1, 2, 4]
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
    fn capture_irqs_match_is_case_insensitive() {
        // Mixed-case keyword AND mixed-case description both fold to lowercase.
        let interrupts = " 50:  1 0 0 0  PCI-MSI 2-edge  xHCI_HCD:usb4\n";
        assert_eq!(parse_capture_irqs(interrupts, &["XhCi"]), vec![50]);
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

    #[test]
    fn smp_mask_duplicate_cores_are_idempotent() {
        // OR semantics: a repeated core sets the bit once (not XOR/ADD).
        assert_eq!(smp_affinity_mask_hex(&[3, 3]), "8");
    }

    #[test]
    fn smp_mask_ignores_out_of_range_cores() {
        // Cores >= 64 don't fit a u64 mask and are skipped (no shift overflow).
        assert_eq!(smp_affinity_mask_hex(&[0, 64, 65]), "1");
    }

    #[test]
    fn normalize_mask_strips_zero_padding() {
        assert_eq!(normalize_affinity_mask("8"), "8");
        assert_eq!(normalize_affinity_mask("00000008"), "8");
    }

    #[test]
    fn normalize_mask_strips_comma_groups() {
        // The kernel comma-groups masks for >32 CPUs (high group first).
        assert_eq!(normalize_affinity_mask("00000000,00000008"), "8");
    }

    #[test]
    fn normalize_mask_lowercases() {
        assert_eq!(normalize_affinity_mask("FF"), "ff");
    }

    #[test]
    fn normalize_mask_zero_and_empty() {
        assert_eq!(normalize_affinity_mask("0"), "0");
        assert_eq!(normalize_affinity_mask("00000000"), "0");
        assert_eq!(normalize_affinity_mask(""), "0");
    }

    #[test]
    fn normalize_mask_round_trips_smp_hex() {
        // What we WRITE (smp_affinity_mask_hex) normalises to itself, so the
        // idempotency fast-path matches on the next ExecStartPre.
        let mask = smp_affinity_mask_hex(&[3]);
        assert_eq!(
            normalize_affinity_mask(&mask),
            normalize_affinity_mask("00000008")
        );
    }

    // --- issue 899: PREEMPT_RT detection + RT-conditional IRQ target -------------

    #[test]
    fn preempt_rt_detected_from_proc_version_token() {
        assert!(kernel_is_preempt_rt(
            "Linux version 6.8.0-rt #1 SMP PREEMPT_RT Thu ...",
            None
        ));
    }

    #[test]
    fn preempt_dynamic_is_not_preempt_rt() {
        // The stock cam-box kernel: PREEMPT_DYNAMIC must read as NON-RT (a bare
        // `PREEMPT` substring would misclassify it).
        assert!(!kernel_is_preempt_rt(
            "Linux version 6.8.0-134-generic #134-Ubuntu SMP PREEMPT_DYNAMIC ...",
            None
        ));
    }

    #[test]
    fn preempt_rt_detected_from_sys_realtime_flag() {
        assert!(kernel_is_preempt_rt("no preempt token here", Some("1\n")));
        assert!(!kernel_is_preempt_rt("no preempt token here", Some("0\n")));
        assert!(!kernel_is_preempt_rt("no preempt token here", None));
    }

    #[test]
    fn irq_target_on_rt_kernel_is_the_capture_core() {
        // RT: handler is threaded and sub-grab priority, so co-locate it on the
        // isolated core (#289 intent).
        assert_eq!(select_irq_target_cores(true, 3, &[0, 1, 2, 3]), vec![3]);
    }

    #[test]
    fn irq_target_on_non_rt_kernel_moves_off_the_grab_core() {
        // issue 899 defect 3: on a stock kernel the non-preemptible xhci handler must
        // run OFF the grab core. issue 1198 TIGHTENS this: it now goes onto ONE
        // dedicated general core (the highest non-capture core, 2) instead of all of
        // [0,1,2], so it no longer shares a core with the painter/display load.
        assert_eq!(select_irq_target_cores(false, 3, &[0, 1, 2, 3]), vec![2]);
    }

    #[test]
    fn irq_target_non_rt_single_core_falls_back_to_capture_core() {
        // A single online core (which is also the capture core) can't move the
        // IRQ off itself — never strand it on an empty mask.
        assert_eq!(select_irq_target_cores(false, 0, &[0]), vec![0]);
    }

    #[test]
    fn irq_target_non_rt_three_core_reserves_one() {
        // issue 1198: the 3-core rung — capture=2, general=[0,1]; reserve the highest
        // general core (1) for the IRQ, leaving [0] for the painter. Before 1198 this
        // returned [0,1] (all general).
        assert_eq!(select_irq_target_cores(false, 2, &[0, 1, 2]), vec![1]);
    }

    #[test]
    fn irq_target_non_rt_two_core_keeps_todays_general() {
        // issue 1198: the 2-core rung — capture=1, general=[0]; reserving the only
        // general core would strand the painter, so keep today's behaviour ([0]).
        assert_eq!(select_irq_target_cores(false, 1, &[0, 1]), vec![0]);
    }

    // --- issue 1198: dedicated capture-IRQ core (reserved-core selector) ----------

    #[test]
    fn reserved_core_is_highest_general_on_four_core_box() {
        // capture=3, general=[0,1,2] → reserve the highest general core, 2.
        assert_eq!(select_irq_reserved_core(3, &[0, 1, 2, 3]), Some(2));
    }

    #[test]
    fn reserved_core_on_three_core_box_still_reserves_one() {
        // capture=2, general=[0,1] → reserve 1, painter left with [0].
        assert_eq!(select_irq_reserved_core(2, &[0, 1, 2]), Some(1));
    }

    #[test]
    fn reserved_core_none_on_two_core_box() {
        // capture=1, general=[0] → reserving would strand the painter → None.
        assert_eq!(select_irq_reserved_core(1, &[0, 1]), None);
    }

    #[test]
    fn reserved_core_none_on_single_core_box() {
        // capture=0, general=[] → nothing to reserve → None.
        assert_eq!(select_irq_reserved_core(0, &[0]), None);
    }

    // --- issue 1198: the capture IRQ must not share a core with the painter -------

    #[test]
    fn irq_and_painter_cores_are_disjoint_on_the_fleet_1198() {
        // issue 1198 [green]: on the stock non-RT cam-box (capture=3, online=[0,1,2,3])
        // the capture-IRQ set and the painter set must NOT share a core, or the 1080p
        // #528 HDMI preview scaling on the painter cores delays the non-preemptible
        // xhci hardirq and starves URB delivery (58 fps captured vs 60 emitted).
        // Migrated from the RED commit to the new `reserved_irq_core` painter
        // signature; the disjointness assertion is unchanged.
        let online = [0usize, 1, 2, 3];
        let capture = 3usize;
        let reserved = select_irq_reserved_core(capture, &online);
        let irq = select_irq_target_cores(false, capture, &online);
        let painter = select_painter_cores(Some(capture), reserved, &online);
        assert!(
            irq.iter().all(|c| !painter.contains(c)),
            "capture IRQ {irq:?} and painter {painter:?} must be disjoint (issue 1198)"
        );
    }

    #[test]
    fn irq_and_painter_disjoint_across_rungs_and_never_empty() {
        // issue 1198: across the degradation rungs the painter and IRQ are never empty,
        // and whenever a core is reserved the two sets are disjoint.
        for online in [
            vec![0usize, 1, 2, 3],
            vec![0, 1, 2],
            vec![0, 1], // 2-core rung: reserved=None, IRQ+painter share [0], both non-empty
            vec![0, 1, 2, 3, 4, 5],
        ] {
            let capture = *online.iter().max().unwrap();
            let reserved = select_irq_reserved_core(capture, &online);
            let irq = select_irq_target_cores(false, capture, &online);
            let painter = select_painter_cores(Some(capture), reserved, &online);
            assert!(!irq.is_empty(), "irq never empty for {online:?}");
            assert!(!painter.is_empty(), "painter never empty for {online:?}");
            if reserved.is_some() {
                assert!(
                    irq.iter().all(|c| !painter.contains(c)),
                    "irq {irq:?} / painter {painter:?} overlap for {online:?}"
                );
            }
        }
    }
}
