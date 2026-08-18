//! issue 899 — the `camera-box.service` unit's CPU-isolation comment must be HONEST.
//!
//! An earlier version claimed the threads the binary does not re-pin are `SCHED_OTHER` and "only
//! use core 3's idle slack". That is factually wrong on the live fleet: `CPUSchedulingPolicy=fifo`
//! applies to the WHOLE process, so every such thread inherits SCHED_FIFO prio 50 on the isolated
//! core (measured on cam1: 27 FIFO threads on core 3). The ticket requires the false comment be
//! corrected "either way". These static assertions pin the honest state so it can't silently
//! regress back to the false claim.

use std::fs;
use std::path::PathBuf;

fn service_text() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("systemd/camera-box.service");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

#[test]
fn unit_comment_does_not_repeat_the_false_sched_other_claim() {
    let text = service_text();
    // The exact false assertions from the earlier comment must be gone. The grab-thread siblings
    // are FIFO 50 (inherited from the process-wide CPUSchedulingPolicy=fifo), not SCHED_OTHER,
    // and they do NOT merely "use core 3's idle slack".
    assert!(
        !text.contains("are SCHED_OTHER while the capture thread"),
        "the false 'threads ... are SCHED_OTHER while the capture thread is SCHED_FIFO prio 90' \
         claim must be removed from camera-box.service (issue 899)"
    );
    assert!(
        !text.contains("only use core 3's idle slack"),
        "the false 'they only use core 3's idle slack' claim must be removed (issue 899): the \
         non-re-pinned threads are FIFO 50 on the isolated core, not idle-slack SCHED_OTHER"
    );
}

#[test]
fn unit_comment_states_the_honest_899_reality() {
    let text = service_text();
    // The corrected comment must name issue 899 AND state the true inherited policy (FIFO prio 50).
    assert!(
        text.contains("issue 899"),
        "the CPU-isolation comment must reference issue 899 where the honest state is explained"
    );
    assert!(
        text.contains("SCHED_FIFO prio 50"),
        "the honest comment must state the non-re-pinned threads inherit SCHED_FIFO prio 50 (the \
         real state), not SCHED_OTHER"
    );
    // And it must point at the staged runbook for the two deeper resolution paths.
    assert!(
        text.contains("docs/runbooks/899-realtime-isolation.md"),
        "the comment must point at the staged 899 runbook for the PREEMPT_RT / per-thread paths"
    );
}
