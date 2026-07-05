//! #484 — the genlock render-tick thread must be pinned SCHED_FIFO to the isolated core pair.
//!
//! imag-nb's kernel cmdline reserves cpu10,11 (`nohz_full=10,11` inside the `isolcpus=2-11` P-core
//! block, #483) for exactly ONE timing-critical thread: the libobs graphics thread that drives the
//! wall-clock-slaved genlock render tick (`genlock_next_deadline` -> `video_sleep`, obs-video.c).
//! #484 pins THAT thread onto those cores under SCHED_FIFO at a LOW priority so its wakeups are not
//! jittered by kernel housekeeping — the direct analogue of camera-box's `src/affinity.rs` (#289)
//! capture-thread pin.
//!
//! CRITICAL SAFETY: the pin is WARN-and-CONTINUE. A high-priority runaway FIFO thread in a
//! ~106-thread OBS process can lock out kernel housekeeping and HANG a headless box (worse than the
//! frame hitches this prevents), so the priority is LOW (~10) and every syscall failure is logged
//! loud and the thread keeps running SCHED_OTHER — never abort, never retry-loop, never hang.
//!
//! This is a SOURCE-level guard (same convention as `tests/obs_updater_disabled.rs`), NOT a runtime
//! test: the pin lives in the vendored genlock C. It runs Tier-0 (default features — just reads the
//! file), so a future `/update-av-stack` `git subtree pull` that silently drops the pin fails CI
//! here. The C itself is compiled by `linux-genlock.yml` (vendored OBS + DistroAV); the live
//! cyclictest/chrt verification on imag-nb is the supervisor's post-merge rig step.

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

/// The pin must set CPU affinity to the isolated cores via `pthread_setaffinity_np` and the
/// FIFO scheduler via `sched_setscheduler(SCHED_FIFO)` — the two syscalls that make up the pin.
#[test]
fn render_tick_thread_is_pinned_sched_fifo_to_isolated_cores() {
    let src = squish(&vendor_file(OBS_VIDEO));

    assert!(
        src.contains("pthread_setaffinity_np(pthread_self(), sizeof(set), &set)"),
        "{OBS_VIDEO}: #484 genlock render-tick pin missing — the graphics thread must set its CPU \
         affinity to the isolated cores via `pthread_setaffinity_np(pthread_self(), ...)`. A \
         `git subtree pull` upstream bump likely dropped it; re-apply the #484 patch."
    );
    assert!(
        src.contains("sched_setscheduler(0, SCHED_FIFO, &param)"),
        "{OBS_VIDEO}: #484 genlock render-tick pin missing — the graphics thread must go realtime \
         via `sched_setscheduler(0, SCHED_FIFO, &param)`. Re-apply the #484 patch."
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
#[test]
fn render_tick_pin_is_warn_and_continue_never_aborts() {
    let src = squish(&vendor_file(OBS_VIDEO));

    // On failure it logs at WARNING level and states it continues SCHED_OTHER.
    assert!(
        src.contains("continuing SCHED_OTHER"),
        "{OBS_VIDEO}: #484 pin must WARN-and-CONTINUE — on any syscall failure it must log that it \
         is `continuing SCHED_OTHER`, never abort/retry-loop/hang (the ticket's CRITICAL SAFETY \
         requirement, mirroring the robust fallback in src/affinity.rs #289)."
    );
    // No hard-abort path in the pin: it must not exit/abort the process on a pin failure.
    assert!(
        !src.contains("genlock: could NOT pin") || !src.contains("abort()"),
        "{OBS_VIDEO}: #484 pin must never abort() on failure — a headless box must keep running."
    );
}

/// The pinned core set must be DERIVED from the kernel's reserved `nohz_full` cpulist (robust,
/// mirroring src/affinity.rs reading /sys), with a hardcoded {10,11} fallback tying it to #483's
/// `nohz_full=10,11` reservation so the pin still lands on a box where /sys is unreadable.
#[test]
fn render_tick_cores_derive_from_nohz_full_with_hardcoded_fallback() {
    let src = squish(&vendor_file(OBS_VIDEO));

    assert!(
        src.contains("/sys/devices/system/cpu/nohz_full"),
        "{OBS_VIDEO}: #484 pin must derive its target cores from \
         /sys/devices/system/cpu/nohz_full (the #483-reserved cpu10,11) — robust like \
         src/affinity.rs reading /sys, not a bare hardcode."
    );
    assert!(
        src.contains("CPU_SET(10, &set)") && src.contains("CPU_SET(11, &set)"),
        "{OBS_VIDEO}: #484 pin must fall back to the hardcoded {{10,11}} pair (#483's \
         nohz_full=10,11 reservation) when /sys/devices/system/cpu/nohz_full is unreadable/empty, \
         so the pin still lands on a fresh box."
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
