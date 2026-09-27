//! Issue 1357 — which CPU cores the vendored genlock render tick may be pinned to.
//!
//! `vendor/obs-studio/libobs/obs-video.c` pins the libobs graphics thread (the thread that drives
//! the genlock render tick through `video_sleep`) onto cores the kernel keeps free for it. Until
//! issue 1357 it read only `nohz_full` and fell back to a hardcoded `{10,11}` pair, so on strih-lx
//! (no isolated core at all) it pinned the thread onto two ordinary cores, and every thread the
//! graphics thread created inherited that mask (40 threads on 10-11 on 27.9.2026).
//!
//! The rule is now: pin only onto cores that are BOTH isolated (`/sys/devices/system/cpu/isolated`)
//! AND tickless (`/sys/devices/system/cpu/nohz_full`). Either list empty = no pin. There is no
//! fallback set.
//!
//! This module is the Tier-0 authority for that decision. The C port (`genlock_render_tick_pin_set`
//! and the cpulist parser `genlock_parse_cpulist_into_set`, inside the `camera-box issue 1357
//! render-tick pin BEGIN … END` block of obs-video.c) is held identical by the committed parity gate
//! `tests/genlock_render_tick_pin_1357.rs`, which lifts the block, compiles it and compares every
//! vector.
//!
//! The parser mirrors the SHIPPED C parser byte-for-byte on malformed input, which is why it does
//! not reuse [`crate::affinity::parse_cpulist`]: the C parser stops at the first character that is
//! not a digit or a separator (`"10 - 11"` is `{10}`, `"3,x,5"` is `{3}`), and it caps every number
//! at `CPU_SETSIZE` without overflowing.

/// glibc's `CPU_SETSIZE`: the number of CPUs a `cpu_set_t` can hold. A core at or above it is
/// dropped, exactly like the C `CPU_SET` guard.
pub const CPU_SETSIZE: usize = 1024;

/// Parse a Linux cpulist the way the vendored C `genlock_parse_cpulist_into_set` does.
///
/// Separators (space, tab, newline, comma) are skipped between entries. An entry is a number or a
/// `a-b` range; anything else ends the parse. A number keeps accumulating digits only while it is
/// below `CPU_SETSIZE` (the rest of its digits are consumed), so a corrupted read cannot overflow.
/// A reversed range (`5-3`) contributes nothing. The result is sorted and deduplicated.
pub fn parse_cpulist(s: &str) -> Vec<usize> {
    let b = s.as_bytes();
    let mut i = 0usize;
    let mut set = [false; CPU_SETSIZE];
    let number = |i: &mut usize| -> usize {
        let mut v = 0usize;
        while *i < b.len() && b[*i].is_ascii_digit() {
            if v < CPU_SETSIZE {
                v = v * 10 + usize::from(b[*i] - b'0');
            }
            *i += 1;
        }
        v
    };
    while i < b.len() {
        while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | b',') {
            i += 1;
        }
        if i >= b.len() || !b[i].is_ascii_digit() {
            break;
        }
        let first = number(&mut i);
        let mut last = first;
        if i < b.len() && b[i] == b'-' {
            i += 1;
            last = number(&mut i);
        }
        let mut c = first;
        while c <= last && c < CPU_SETSIZE {
            set[c] = true;
            c += 1;
        }
    }
    (0..CPU_SETSIZE).filter(|&c| set[c]).collect()
}

/// The cores the render tick may be pinned to: the isolated cores that are also `nohz_full`.
/// An empty result means "do not pin".
pub fn render_tick_pin_cores(isolated: &str, nohz_full: &str) -> Vec<usize> {
    let nohz = parse_cpulist(nohz_full);
    parse_cpulist(isolated)
        .into_iter()
        .filter(|c| nohz.contains(c))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strih_lx_with_no_isolated_core_is_never_pinned() {
        // 27.9.2026 strih-lx: both sysfs lists read as an empty line.
        assert!(render_tick_pin_cores("\n", "\n").is_empty());
        assert!(render_tick_pin_cores("", "").is_empty());
    }

    #[test]
    fn either_list_empty_means_no_pin_there_is_no_fallback_pair() {
        assert!(render_tick_pin_cores("2-11", "").is_empty());
        assert!(render_tick_pin_cores("", "10,11").is_empty());
        // An unconfigured nohz_full reads "(null)" on some kernels.
        assert!(render_tick_pin_cores("2-11", "(null)\n").is_empty());
    }

    #[test]
    fn the_historic_imag_layout_pins_the_tickless_pair() {
        assert_eq!(render_tick_pin_cores("2-11\n", "10-11\n"), vec![10, 11]);
        assert_eq!(render_tick_pin_cores("3", "3"), vec![3]);
    }

    #[test]
    fn a_tickless_core_that_is_not_isolated_is_not_a_pin_core() {
        assert_eq!(render_tick_pin_cores("0-3,8-11", "2-9"), vec![2, 3, 8, 9]);
    }

    #[test]
    fn the_parser_matches_the_c_parser_on_malformed_input() {
        assert_eq!(parse_cpulist("10 - 11"), vec![10]);
        assert_eq!(parse_cpulist("3,x,5"), vec![3]);
        assert_eq!(parse_cpulist("5-3"), Vec::<usize>::new());
        assert_eq!(parse_cpulist("1023,1024"), vec![1023]);
        assert_eq!(parse_cpulist("99999999999999999999,2"), vec![2]);
        assert_eq!(parse_cpulist("1022-5000"), vec![1022, 1023]);
        assert_eq!(parse_cpulist("\t4,\n6"), vec![4, 6]);
    }
}
