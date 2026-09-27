//! #484 — the genlock render-tick thread pin, as reworked by issue 1357.
//!
//! The libobs graphics thread drives the wall-clock-slaved genlock render tick
//! (`genlock_next_deadline` -> `video_sleep`, obs-video.c). #484 pinned it onto imag-nb's reserved
//! `nohz_full` cores under a LOW SCHED_FIFO priority so its wakeups are not jittered by kernel
//! housekeeping — the analogue of camera-box's `src/affinity.rs` (#289) capture-thread pin.
//!
//! Issue 1357 (live on strih-lx 23.9 and 27.9.2026): the pin fell back to a hardcoded `{10,11}` on a
//! box with no isolated core, and every thread the graphics thread created inherited that mask (and,
//! with an rtprio grant, SCHED_FIFO) — the whole NDI receive path squeezed onto two cores. So now:
//! the pin cores are the kernel's isolated cores that are ALSO nohz_full (none means no pin, and
//! there is no fallback pair), and the pin is held only while the tick SLEEPS (`video_sleep` around
//! `os_sleepto_ns`), so no thread is ever created under it; FIFO carries `SCHED_RESET_ON_FORK`.
//!
//! CRITICAL SAFETY (unchanged): the priority is LOW (~10) and every syscall failure is logged loud
//! and the thread keeps running SCHED_OTHER — never abort, never retry-loop, never hang.
//!
//! This is a SOURCE-level guard (same convention as `tests/obs_updater_disabled.rs`): a future
//! `/update-av-stack` `git subtree pull` that silently drops the rework fails CI here. The behaviour
//! itself is executed by `tests/genlock_render_tick_pin_1357.rs` (the block lifted and compiled:
//! parity, a syscall trace, and real-thread inheritance).

use std::path::PathBuf;

const OBS_VIDEO: &str = "vendor/obs-studio/libobs/obs-video.c";

fn vendor_file(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Collapse every run of ASCII whitespace to a single space so the assertions survive reformatting
/// (e.g. an upstream merge re-indenting a line). Mirrors `obs_updater_disabled.rs::squish`.
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The pin must set CPU affinity via `pthread_setaffinity_np` and the scheduler via
/// `sched_setscheduler(SCHED_FIFO | SCHED_RESET_ON_FORK)` — the reset flag makes the kernel give any
/// thread created under FIFO plain SCHED_OTHER (issue 1357).
#[test]
fn render_tick_thread_is_pinned_sched_fifo_to_isolated_cores() {
    let src = squish(&vendor_file(OBS_VIDEO));

    assert!(
        src.contains("pthread_setaffinity_np(pthread_self(), sizeof(p->pin), &p->pin)"),
        "{OBS_VIDEO}: the genlock render-tick pin must narrow the graphics thread to the pin cores \
         via `pthread_setaffinity_np(pthread_self(), ...)`. A `git subtree pull` upstream bump \
         likely dropped it; re-apply the #484 + issue 1357 patch."
    );
    assert!(
        src.contains("sched_setscheduler(0, SCHED_FIFO | SCHED_RESET_ON_FORK, &param)"),
        "{OBS_VIDEO}: the render tick must go realtime via \
         `sched_setscheduler(0, SCHED_FIFO | SCHED_RESET_ON_FORK, &param)` — without the reset flag \
         a thread created under FIFO inherits it (issue 1357)."
    );
    assert!(
        !src.contains("sched_setscheduler(0, SCHED_FIFO, &param)"),
        "{OBS_VIDEO}: a bare SCHED_FIFO without SCHED_RESET_ON_FORK is the issue 1357 leak"
    );
}

/// The FIFO priority must be LOW (the whole safety point). A high-priority FIFO thread in the
/// ~106-thread OBS process can lock out kernel housekeeping and hang the headless box.
#[test]
fn render_tick_fifo_priority_is_low() {
    let src = squish(&vendor_file(OBS_VIDEO));

    assert!(
        src.contains("#define GENLOCK_RT_PRIORITY 10"),
        "{OBS_VIDEO}: #484 pin must use a LOW SCHED_FIFO priority (`#define GENLOCK_RT_PRIORITY \
         10`) — a HIGH-priority FIFO thread can starve kernel housekeeping and HANG a headless \
         box, the exact failure the ticket's safety note forbids."
    );
    assert!(
        src.contains("param.sched_priority = GENLOCK_RT_PRIORITY;"),
        "{OBS_VIDEO}: #484 pin must apply the LOW `GENLOCK_RT_PRIORITY` to `sched_param` — not a \
         hardcoded high number."
    );
    // The pin must NEVER reach for the maximum FIFO priority (99 / sched_get_priority_max).
    assert!(
        !src.contains("sched_get_priority_max"),
        "{OBS_VIDEO}: #484 pin must NOT use the MAX FIFO priority — a max-prio render-tick thread \
         can hang the headless box (the ticket's hard safety constraint)."
    );
}

/// WARN-and-CONTINUE: a failed affinity/scheduler call must log a WARNING and keep running
/// SCHED_OTHER — never abort, never hang. This is the safety invariant that makes shipping the pin
/// acceptable at all.
///
/// The abort-freedom check is scoped to the `genlock_pin_render_tick_thread` FUNCTION BODY, not the
/// whole ~1400-line file — a file-wide `!src.contains("abort()")` would still pass even if the
/// function itself grew an `exit()`/`assert()` escape hatch on a failure path (neither token is
/// `abort()` specifically), and a naive `!A || !B` shape (A = the warning string present, B =
/// `abort()` present) is VACUOUSLY true whenever `abort()` never appears anywhere in the file,
/// silently not checking the intended condition at all — found in review (PR #542).
#[test]
fn render_tick_pin_is_warn_and_continue_never_aborts() {
    let raw = vendor_file(OBS_VIDEO);

    let fn_start = raw
        .find("static void genlock_pin_render_tick_thread(void)")
        .expect("genlock_pin_render_tick_thread must be defined");
    let fn_end = raw[fn_start..]
        .find("\n}\n")
        .map(|rel| fn_start + rel)
        .expect("genlock_pin_render_tick_thread must have a closing brace");
    let body = squish(&raw[fn_start..fn_end]);

    // On failure it logs at WARNING level and states it continues SCHED_OTHER — checked from BOTH
    // failure branches (affinity AND scheduler), not just "appears somewhere in the function".
    let warn_count = body.matches("continuing SCHED_OTHER").count();
    assert!(
        warn_count >= 2,
        "{OBS_VIDEO}: #484 pin must WARN-and-CONTINUE — BOTH the affinity-pin failure branch and \
         the SCHED_FIFO failure branch must log `continuing SCHED_OTHER` (found {warn_count} \
         occurrence(s) inside genlock_pin_render_tick_thread), never abort/retry-loop/hang (the \
         ticket's CRITICAL SAFETY requirement, mirroring the robust fallback in src/affinity.rs \
         #289)."
    );
    // No hard-abort/exit/assert path anywhere INSIDE the pin function itself.
    for banned in ["abort(", "exit(", "assert("] {
        assert!(
            !body.contains(banned),
            "{OBS_VIDEO}: genlock_pin_render_tick_thread must never call `{banned}...)` on a \
             failure path — a headless box must keep running SCHED_OTHER, never abort/exit/assert."
        );
    }
}

/// Issue 1357: the pin cores are DERIVED from BOTH `/sys/devices/system/cpu/isolated` and
/// `/sys/devices/system/cpu/nohz_full`, and there is NO hardcoded fallback set. The old `{10,11}`
/// fallback pinned strih-lx (no isolated core at all) onto two ordinary cores and leaked that mask
/// to 40 threads.
#[test]
fn render_tick_cores_derive_from_isolated_and_nohz_full_with_no_fallback() {
    let src = squish(&vendor_file(OBS_VIDEO));

    for path in [
        "/sys/devices/system/cpu/isolated",
        "/sys/devices/system/cpu/nohz_full",
    ] {
        assert!(
            src.contains(path),
            "{OBS_VIDEO}: the render-tick pin must read {path} (issue 1357: isolated AND nohz_full)"
        );
    }
    assert!(
        !src.contains("CPU_SET(10, &set)") && !src.contains("CPU_SET(11, &set)"),
        "{OBS_VIDEO}: the hardcoded {{10,11}} fallback must be gone — a box with no isolated core \
         is not pinned at all (issue 1357)"
    );
    assert!(
        src.contains("render-tick thread not pinned: no isolated cores"),
        "{OBS_VIDEO}: an unpinned render tick must say so in ONE log line (issue 1357)"
    );
    assert!(
        !src.contains("pinned to the isolated nohz_full cores"),
        "{OBS_VIDEO}: the old unconditional \"pinned to the isolated nohz_full cores\" line lied \
         on strih-lx and must be gone"
    );
}

/// Issue 1357: the pin is held only while the tick sleeps — `video_sleep` narrows before
/// `os_sleepto_ns` and restores right after, so no thread is ever created under the pin.
#[test]
fn the_pin_wraps_only_the_tick_sleep() {
    let raw = vendor_file(OBS_VIDEO);
    let start = raw
        .find("static inline void video_sleep(")
        .expect("video_sleep must be defined");
    let end = raw[start..]
        .find("\n}\n")
        .map(|i| start + i)
        .expect("video_sleep must have a closing brace");
    let body = &raw[start..end];
    let begin = body
        .find("genlock_tick_pin_sleep_begin();")
        .expect("video_sleep must call genlock_tick_pin_sleep_begin() (issue 1357)");
    let sleep = body
        .find("os_sleepto_ns(t)")
        .expect("video_sleep must still sleep with os_sleepto_ns(t)");
    let finish = body
        .find("genlock_tick_pin_sleep_end();")
        .expect("video_sleep must call genlock_tick_pin_sleep_end() (issue 1357)");
    assert!(
        begin < sleep && sleep < finish,
        "{OBS_VIDEO}: the pin must be taken right before os_sleepto_ns and dropped right after it"
    );
    assert_eq!(
        body.matches("os_sleepto_ns(").count(),
        1,
        "{OBS_VIDEO}: video_sleep must sleep in exactly one place, inside the pin window"
    );
}

/// The pin must be CALLED from the graphics thread (obs_graphics_thread), AFTER the thread is
/// named — so it is THIS thread (the one that runs `video_sleep` -> the genlock tick) that gets
/// pinned, and the pin is Linux-only (the _WIN32/__APPLE__ builds don't run on imag-nb).
#[test]
fn pin_is_invoked_from_the_graphics_thread_linux_only() {
    let raw = vendor_file(OBS_VIDEO);

    let def_idx = raw
        .find("genlock_pin_render_tick_thread(void)")
        .expect("obs-video.c must DEFINE genlock_pin_render_tick_thread (#484)");
    let name_idx = raw
        .find(r#"os_set_thread_name("libobs: graphics thread")"#)
        .expect("obs_graphics_thread must still name the graphics thread");
    let call_idx = raw
        .find("genlock_pin_render_tick_thread();")
        .expect("obs_graphics_thread must CALL genlock_pin_render_tick_thread() (#484)");

    assert!(
        def_idx < call_idx,
        "{OBS_VIDEO}: genlock_pin_render_tick_thread must be defined before it is called"
    );
    assert!(
        name_idx < call_idx,
        "{OBS_VIDEO}: the #484 pin must be invoked from obs_graphics_thread AFTER \
         os_set_thread_name — it pins the graphics thread itself (the genlock tick driver)"
    );

    // Linux-only guard: the pin is wrapped in a __linux__ conditional (the Windows/macOS builds
    // that strih/stream use have no equivalent and don't run on imag-nb).
    let squished = squish(&raw);
    assert!(
        squished.contains("#if defined(__linux__)"),
        "{OBS_VIDEO}: the #484 pin must be guarded `#if defined(__linux__)` — it is a Linux-only \
         (imag-nb) addition; the vendored Windows/macOS builds must be unaffected."
    );
}
