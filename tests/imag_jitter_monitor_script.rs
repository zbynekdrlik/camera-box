//! #674 — content-assertion guards for the imag-nb genlock-FIFO audit delta reporter deliverable:
//! `scripts/imag-jitter-monitor.sh` (the periodic reporter), `scripts/mark-imag-restart.sh` (the
//! restart-correlation marker, run from dev1), and the two systemd units. Mirrors the repo's
//! established sourced-script content-assertion pattern (e.g. `tests/harness_frozen_camera_gate.rs`).

use std::path::PathBuf;

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn on_noncomment_line(body: &str, needle: &str) -> bool {
    body.lines()
        .any(|l| l.contains(needle) && !l.trim_start().starts_with('#'))
}

#[test]
fn monitor_script_uses_set_euo_pipefail() {
    let body = read("scripts/imag-jitter-monitor.sh");
    assert!(
        body.lines()
            .take(5)
            .any(|l| l.trim() == "set -euo pipefail"),
        "imag-jitter-monitor.sh must fail loud (script-failure-policy)"
    );
}

#[test]
fn monitor_script_sources_the_offset_state_lib() {
    let body = read("scripts/imag-jitter-monitor.sh");
    assert!(
        on_noncomment_line(&body, "lib/imag-jitter-state.sh"),
        "imag-jitter-monitor.sh must source scripts/lib/imag-jitter-state.sh for the resumable \
         byte-offset bookkeeping (#674)"
    );
    assert!(
        on_noncomment_line(&body, "imag_jitter_next_offset"),
        "imag-jitter-monitor.sh must CALL imag_jitter_next_offset, not just source the lib (#674)"
    );
}

#[test]
fn monitor_script_calls_genlock_jitter_report_and_persists_the_new_offset() {
    let body = read("scripts/imag-jitter-monitor.sh");
    assert!(
        on_noncomment_line(&body, "\"$JITTER_BIN\""),
        "imag-jitter-monitor.sh must invoke the existing #272 genlock-jitter-report binary — no \
         second delta-summarizer implementation (#674)"
    );
    assert!(
        on_noncomment_line(&body, "> \"$STATE_FILE\""),
        "imag-jitter-monitor.sh must persist the new offset to STATE_FILE every run so the NEXT \
         run resumes correctly (#674)"
    );
}

#[test]
fn mark_imag_restart_writes_the_same_journald_tag_the_monitor_uses() {
    let body = read("scripts/mark-imag-restart.sh");
    assert!(
        body.lines()
            .take(5)
            .any(|l| l.trim() == "set -euo pipefail"),
        "mark-imag-restart.sh must fail loud (script-failure-policy)"
    );
    assert!(
        on_noncomment_line(&body, "logger -t imag-jitter-monitor"),
        "mark-imag-restart.sh must write its RESTART-MARKER under the SAME syslog identifier \
         (imag-jitter-monitor) the periodic reporter uses, so both show up in one \
         `journalctl -t imag-jitter-monitor` view (#674)"
    );
    assert!(
        on_noncomment_line(&body, "RESTART-MARKER"),
        "the marker line must be greppable/distinct from the periodic delta reports (#674)"
    );
    assert!(
        body.contains("strih | stream"),
        "mark-imag-restart.sh must validate --box is strih or stream, mirroring \
         launch-obs-genlock.sh's own box validation"
    );
}

#[test]
fn launch_obs_genlock_plan_references_the_restart_marker() {
    let body = read("scripts/launch-obs-genlock.sh");
    // The reference lives INSIDE the `cat <<PLAN ... PLAN` heredoc body (printed operator-facing
    // text, itself styled with a leading `#` for readability) — a plain substring check, not
    // `on_noncomment_line`, since that heredoc content is data, not a real bash comment to skip.
    assert!(
        body.contains("mark-imag-restart.sh"),
        "launch-obs-genlock.sh's printed relaunch plan must reference mark-imag-restart.sh as a \
         follow-up step, so every real strih/stream restart gets correlated on imag-nb (#674)"
    );
}

#[test]
fn systemd_units_exist_with_a_five_minute_cadence() {
    let service = read("systemd/imag-jitter-monitor.service");
    assert!(
        service.contains("ExecStart="),
        "imag-jitter-monitor.service must define ExecStart"
    );
    assert!(
        service.contains("imag-jitter-monitor.sh"),
        "imag-jitter-monitor.service must run the #674 monitor script"
    );
    let timer = read("systemd/imag-jitter-monitor.timer");
    assert!(
        timer.contains("OnUnitActiveSec=5min"),
        "imag-jitter-monitor.timer must fire every 5 minutes (#674 telemetry cadence)"
    );
}
