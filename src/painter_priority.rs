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
pub const PAINTER_NICE: i32 = -15;

/// camera-box's unit `Nice=` (`systemd/camera-box.service` and the setup-device STEP 7 heredoc,
/// both pinned by `tests/painter_priority_1387.rs`).
pub const CAMERA_BOX_UNIT_NICE: i32 = -10;
const _: () = assert!(PAINTER_NICE < CAMERA_BOX_UNIT_NICE);
const _: () = assert!(PAINTER_NICE >= -20 && PAINTER_NICE <= 19);

/// The nice value of one thread from its `/proc/<pid>/task/<tid>/stat` line (field 19, counted
/// after the `(comm)` field, which may itself hold spaces and parentheses).
pub fn parse_task_nice(stat: &str) -> Option<i32> {
    let rest = &stat[stat.rfind(')')? + 1..];
    // after the comm: state(3) ppid(4) ... nice(19) -> the 17th field after it
    rest.split_whitespace().nth(16)?.parse().ok()
}

/// What one per-thread `setpriority` did.
enum NiceResult {
    Set,
    Refused,
    /// The thread ended between the `/proc/self/task` listing and the call (ESRCH).
    Gone,
}

/// Set `nice` on every thread of this process. Returns (threads set, threads refused); a thread
/// that ended meanwhile counts as neither. Threads created afterwards inherit the creating
/// thread's nice.
pub fn set_all_threads_nice(nice: i32) -> (usize, usize) {
    let (mut set, mut refused) = (0usize, 0usize);
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
        return (0, 0);
    };
    for task in tasks.flatten() {
        let Some(tid) = task
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        match set_thread_nice(tid, nice) {
            NiceResult::Set => set += 1,
            NiceResult::Refused => refused += 1,
            NiceResult::Gone => {}
        }
    }
    (set, refused)
}

#[cfg(target_os = "linux")]
fn set_thread_nice(tid: u32, nice: i32) -> NiceResult {
    // SAFETY: on Linux, setpriority(PRIO_PROCESS, tid, ..) sets the nice of that one thread; it
    // takes plain integers and touches no memory of ours.
    if unsafe { libc::setpriority(libc::PRIO_PROCESS, tid as libc::id_t, nice) } == 0 {
        return NiceResult::Set;
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) => NiceResult::Gone,
        _ => NiceResult::Refused,
    }
}

#[cfg(not(target_os = "linux"))]
fn set_thread_nice(_tid: u32, _nice: i32) -> NiceResult {
    NiceResult::Refused
}

/// Raise the painter to [`PAINTER_NICE`] and log the outcome once. Best effort: a refusal (no
/// CAP_SYS_NICE) is a WARNING and the painter runs on.
pub fn raise_painter_priority() {
    let (set, refused) = set_all_threads_nice(PAINTER_NICE);
    if refused == 0 && set > 0 {
        tracing::info!(
            "#1387 painter priority: {set} thread(s) at nice {PAINTER_NICE}, below camera-box's \
             unit nice {CAMERA_BOX_UNIT_NICE}, so the JPEG XS encoder never starves the 60 Hz paint"
        );
    } else {
        tracing::warn!(
            "#1387 painter priority: nice {PAINTER_NICE} refused on {refused} of {} thread(s) (needs \
             CAP_SYS_NICE / root) -- the painter may miss vsync beside camera-box's JPEG XS encoder",
            set + refused
        );
    }
}

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
