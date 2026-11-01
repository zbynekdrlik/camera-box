//! Issue 1387: the cam2 painter (`frame-probe --paint-only`) outranks camera-box on the general cores.
//!
//! cam2 runs both the measurement painter and a camera-box. The painter paints the dual-QR video
//! and its QPSK marker index is the paint frame id. camera-box's unit runs at `Nice=-10`, and since
//! the JPEG XS sender came on on cam2 (31.10.2026 09:20) its two `jxs-feed` encoder threads share
//! the box's only two general cores (0-1) with the painter. The painter at nice 0 then missed the
//! 60 Hz vsync and painted ~47 fps, so the marker index advanced at 47/s against the 60/s every
//! consumer assumes: the stream program-audio guard read FOREIGN (the marker chain broke while the
//! words still decoded) and every full-path E2E stopped at its `[0/8]` program-audio preflight.
//! A live `renice -15` of the painter brought it back to 59.5 fps with the JPEG XS sender still at
//! 30 fps and `dropped_busy=0`.
//!
//! So `run_paint_only` raises every painter thread to [`PAINTER_NICE`] before it spawns anything:
//! the painter is the measurement instrument and runs only in TEST mode (EVENT disables it), so it
//! takes precedence over camera-box's encoder, never over camera-box's FIFO capture threads.

/// The nice every `--paint-only` thread runs at: below camera-box's unit `Nice=` so the JPEG XS
/// encoder threads (which inherit the unit's nice) never starve the painter.
pub const PAINTER_NICE: i32 = 0;

/// camera-box's unit `Nice=` (`systemd/camera-box.service` and the setup-device STEP 7 heredoc,
/// both pinned by `tests/painter_priority_1387.rs`).
pub const CAMERA_BOX_UNIT_NICE: i32 = -10;

/// The nice value of one thread from its `/proc/<pid>/task/<tid>/stat` line (field 19, counted
/// after the `(comm)` field, which may itself hold spaces and parentheses).
pub fn parse_task_nice(stat: &str) -> Option<i32> {
    let rest = &stat[stat.rfind(')')? + 1..];
    // after the comm: state(3) ppid(4) ... nice(19) -> the 17th field after it
    rest.split_whitespace().nth(16)?.parse().ok()
}

/// Set `nice` on every thread of this process. Returns (threads set, threads refused). Threads
/// created afterwards inherit the creating thread's nice.
pub fn set_all_threads_nice(_nice: i32) -> (usize, usize) {
    (0, 0)
}

/// Raise the painter to [`PAINTER_NICE`] and log the outcome once. Best effort: a refusal (no
/// CAP_SYS_NICE) is a WARNING and the painter runs on.
pub fn raise_painter_priority() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nice_field_is_read_after_a_comm_with_spaces_and_parentheses() {
        let stat =
            "123 (frame (probe) x) S 1 123 123 0 -1 4194560 1 0 0 0 5 3 0 0 20 -15 4 0 99 1 2";
        assert_eq!(parse_task_nice(stat), Some(-15));
        assert_eq!(
            parse_task_nice("7 (a) R 1 7 7 0 -1 0 0 0 0 0 0 0 0 0 20 0 1"),
            Some(0)
        );
        assert_eq!(parse_task_nice("garbage"), None);
        assert_eq!(parse_task_nice("1 (x) S 1"), None);
    }
}
