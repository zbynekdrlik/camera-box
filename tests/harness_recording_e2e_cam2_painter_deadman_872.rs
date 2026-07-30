//! #872 — every place that stops the PERMANENT `cam2-painter.service` must first arm an ON-BOX
//! dead-man timer that restarts it, and `cleanup()` must disarm that timer.
//!
//! ## The bug
//!
//! `scripts/recording-e2e.sh` stops the #863 always-on painter in three places (`_cam2_prep` on
//! both branches of the ALL_CAMBOX split, and the `[3/8]` fb0-free step) and restarts it in
//! exactly one: `cleanup()`, the bash `EXIT` trap. A SIGKILLed run never runs that trap, so the
//! painter stays stopped and cam2's interkom return monitor goes dark indefinitely. This is not a
//! rare path — `full-path-e2e.yml`'s concurrency group is `cancel-in-progress: true`, so ANY push
//! to `dev` cancels an in-flight hardware run. Live on 2026-07-29: stopped at 21:31:56, found
//! `inactive`/`enabled` at 01:03 — 3.5 hours dark across three subsequent runs.
//!
//! ## The fix
//!
//! Arm `systemd-run --on-active=<N>min --unit=cam2-painter-deadman systemctl start cam2-painter`
//! on the box BEFORE stopping the unit, and disarm it in `cleanup()` next to the existing start.
//! Normal exit is unchanged; a killed run self-heals on the box with no dev1 involvement.
//!
//! Bounding matters (the #869 lesson): each arm of the branch must be checked inside its OWN byte
//! range, or the sibling arm's correct call satisfies the assertion vacuously.

use std::fs;
use std::path::PathBuf;

fn read_harness() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/recording-e2e.sh");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// (start, end) of the ALL_CAMBOX arm of the `_cam2_prep` branch — same anchors the #869 test
/// uses, kept in lock-step with it deliberately.
fn all_cambox_prep_arm(s: &str) -> (usize, usize) {
    let anchor = s
        .find("_cam2_marker_check=\"\"")
        .expect("#872: expected the _cam2_marker_check initialiser that precedes the branch");
    let then = s[anchor..]
        .find("if [ \"${ALL_CAMBOX:-0}\" = \"1\" ]; then")
        .map(|i| anchor + i)
        .expect("#872: expected the ALL_CAMBOX arm of the _cam2_prep branch");
    let else_at = s[then..]
        .find("\nelse\n")
        .map(|i| then + i)
        .expect("#872: expected the else arm to bound the ALL_CAMBOX arm");
    (then, else_at)
}

/// (start, end) of the NON-sweep (else) arm of the same branch.
fn non_sweep_prep_arm(s: &str) -> (usize, usize) {
    let (_, else_at) = all_cambox_prep_arm(s);
    let end = s[else_at..]
        .find("\nfi\n")
        .map(|i| else_at + i)
        .expect("#872: expected the closing fi of the _cam2_prep branch");
    (else_at, end)
}

#[test]
fn all_cambox_prep_arms_the_painter_deadman_before_stopping_it_872() {
    let s = read_harness();
    let (start, end) = all_cambox_prep_arm(&s);
    let arm = &s[start..end];
    let armed = arm
        .find("cam2_painter_deadman_arm_cmds")
        .unwrap_or_else(|| panic!("#872: the ALL_CAMBOX _cam2_prep must arm the on-box dead-man restart before stopping the permanent painter — a SIGKILLed run never reaches cleanup(). Got arm:\n{arm}"));
    let stopped = arm
        .find("systemctl stop cam2-painter")
        .expect("#872: expected the existing #869 painter stop in the ALL_CAMBOX arm");
    assert!(
        armed < stopped,
        "#872: the dead-man must be ARMED BEFORE the stop, so a kill between the two still \
         self-heals (armed at {armed}, stopped at {stopped})"
    );
}

#[test]
fn non_sweep_prep_arms_the_painter_deadman_before_stopping_it_872() {
    let s = read_harness();
    let (start, end) = non_sweep_prep_arm(&s);
    let arm = &s[start..end];
    let armed = arm
        .find("cam2_painter_deadman_arm_cmds")
        .unwrap_or_else(|| panic!("#872: the non-sweep _cam2_prep stops the permanent painter too and needs the same dead-man arm. Got arm:\n{arm}"));
    let stopped = arm
        .find("systemctl stop cam2-painter")
        .expect("#872: expected the painter stop in the non-sweep arm");
    assert!(
        armed < stopped,
        "#872: dead-man must be armed before the stop in the non-sweep arm too \
         (armed at {armed}, stopped at {stopped})"
    );
}

#[test]
fn cleanup_disarms_the_painter_deadman_after_restarting_the_painter_872() {
    let s = read_harness();
    let started = s
        .find("systemctl start cam2-painter")
        .expect("#872: expected cleanup()'s existing painter restart");
    let disarm = s
        .find("cam2_painter_deadman_disarm_cmds")
        .unwrap_or_else(|| panic!("#872: cleanup() must DISARM the dead-man timer once it has restarted the painter itself, or the timer fires later and restarts an already-running unit"));
    assert!(
        disarm > started,
        "#872: disarm belongs AFTER the restart in cleanup(), so the timer is only cancelled once \
         the painter is genuinely back (start at {started}, disarm at {disarm})"
    );
}

/// The dead-man must never fire while a run is genuinely still in progress. Two guards:
/// the delay comfortably exceeds the longest run, AND the action refuses to start the permanent
/// painter while the harness's own `frame-probe` still owns the framebuffer. Without the second,
/// a run that outlives the delay gets TWO painters on one /dev/fb0 under different run-ids —
/// verbatim the #440 artifact the stop exists to prevent.
#[test]
fn deadman_refuses_to_start_the_painter_while_a_frame_probe_is_running_872() {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/lib/cam2-painter-deadman.sh");
    let s = fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
    assert!(
        s.contains("pgrep -x frame-probe"),
        "#872: the dead-man action must check for a live frame-probe before starting the \
         permanent painter, or a run outliving the delay gets two painters on one framebuffer"
    );
    let guard = s
        .find("pgrep -x frame-probe")
        .expect("#872: expected the frame-probe guard");
    let start = s
        .find("systemctl start cam2-painter'")
        .expect("#872: expected the guarded start inside the dead-man action");
    assert!(
        guard < start,
        "#872: the frame-probe check must precede the start (guard {guard}, start {start})"
    );
}

#[test]
fn deadman_delay_comfortably_exceeds_the_longest_run_872() {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/lib/cam2-painter-deadman.sh");
    let s = fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
    let line = s
        .lines()
        .find(|l| l.starts_with("CAM2_PAINTER_DEADMAN_MINUTES="))
        .expect("#872: expected the delay default");
    let mins: u32 = line
        .split(":-")
        .nth(1)
        .and_then(|t| t.trim_end_matches("}\"").trim_end_matches('}').parse().ok())
        .unwrap_or_else(|| panic!("#872: could not parse the delay from {line:?}"));
    assert!(
        mins >= 60,
        "#872: a live run holds the painter stopped for 25-35 min (stop -> recording -> per-box \
         decode -> cleanup); a delay of {mins} min risks firing MID-RUN and starting a second \
         painter. Keep it well clear of the longest run."
    );
}

#[test]
fn deadman_lib_is_sourced_by_the_harness_872() {
    let s = read_harness();
    assert!(
        s.contains("lib/cam2-painter-deadman.sh"),
        "#872: the harness must source scripts/lib/cam2-painter-deadman.sh — the arm/disarm text \
         is single-sourced there, never duplicated inline at each of the three stop sites"
    );
}
