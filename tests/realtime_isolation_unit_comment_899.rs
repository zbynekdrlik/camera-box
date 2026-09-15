//! issue 899 defect 2 — the `camera-box.service` unit must NOT carry a process-wide
//! `CPUSchedulingPolicy=fifo`, and its CPU-isolation comment must describe the honest
//! per-thread reality.
//!
//! The process-wide policy applied SCHED_FIFO prio 50 to the WHOLE process, so every
//! thread the binary did not re-pin inherited FIFO 50 on the isolated core (measured on
//! cam1: 27 FIFO threads on core 3) — not the SCHED_OTHER the design intended. Defect 2's
//! fix drops that policy entirely; the binary now raises SCHED_FIFO PER THREAD only on the
//! capture+emit hot path (src/affinity.rs `set_current_thread_realtime`), and every other
//! thread keeps the process default SCHED_OTHER. These static assertions pin the honest
//! state so it can't silently regress back to the process-wide policy or the old false
//! "idle slack" comment.

use std::fs;
use std::path::PathBuf;

fn service_text() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("systemd/camera-box.service");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// True if `needle` appears on a line that is NOT a `#` comment — a comment that merely
/// mentions the string (the honest note explains WHY the policy was removed, quoting it)
/// cannot satisfy or trip the assertion; only a real directive counts.
fn on_directive_line(text: &str, needle: &str) -> bool {
    text.lines()
        .any(|l| l.contains(needle) && !l.trim_start().starts_with('#'))
}

#[test]
fn unit_has_no_process_wide_cpuscheduling_policy_899() {
    let text = service_text();
    // The load-bearing defect-2 fix: the unit must carry NO process-wide scheduling
    // policy directive. If either of these returns as a real directive line, every thread
    // inherits SCHED_FIFO on the isolated core again — the exact pre-899 defect.
    assert!(
        !on_directive_line(&text, "CPUSchedulingPolicy"),
        "camera-box.service must NOT set a process-wide CPUSchedulingPolicy — it forces \
         EVERY thread to SCHED_FIFO on the isolated core (issue 899 defect 2); the binary \
         raises FIFO per-thread instead (src/affinity.rs set_current_thread_realtime)"
    );
    assert!(
        !on_directive_line(&text, "CPUSchedulingPriority"),
        "camera-box.service must NOT set a process-wide CPUSchedulingPriority (it is \
         meaningless once CPUSchedulingPolicy is dropped — issue 899 defect 2)"
    );
}

#[test]
fn unit_comment_does_not_repeat_the_old_false_idle_slack_claim() {
    let text = service_text();
    // A much older version of this comment claimed the non-re-pinned threads were
    // SCHED_OTHER and "only use core 3's idle slack" while the process-wide FIFO policy
    // was actually in force — a lie at the time. That exact phrasing must never come back.
    assert!(
        !text.contains("only use core 3's idle slack"),
        "the old false 'they only use core 3's idle slack' phrasing must stay removed \
         (issue 899); describe the real per-thread state instead"
    );
}

#[test]
fn unit_comment_states_the_per_thread_899_reality() {
    let text = service_text();
    // The corrected comment must name issue 899, state the per-thread FIFO reality, and
    // point at the staged runbook for the remaining kernel path.
    assert!(
        text.contains("issue 899"),
        "the CPU-isolation comment must reference issue 899 where the honest state is explained"
    );
    assert!(
        text.contains("PER THREAD") || text.contains("per-thread") || text.contains("per thread"),
        "the honest comment must state that SCHED_FIFO is now raised PER THREAD on the \
         capture+emit hot path (issue 899 defect 2), not process-wide"
    );
    assert!(
        text.contains("docs/runbooks/899-realtime-isolation.md"),
        "the comment must point at the staged 899 runbook for the remaining kernel path"
    );
}
