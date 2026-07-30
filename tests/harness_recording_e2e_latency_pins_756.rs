//! #756 Member 3 — `scripts/recording-e2e.sh` must run `scripts/latency_pins_snapshot.py` AFTER
//! the verdict JSON exists and BEFORE the Discord report sends, fail-open (never affecting
//! `$GATE`), and thread the resulting file into `e2e_discord_report_send`'s new 5th arg.
//!
//! Structural, source-text assertions — same discipline as the rest of this repo's harness suite
//! (see tests/harness_e2e_execute_verdict_703.rs) since this is a read-only preflight/report step
//! against a live rig that only the rig itself can exercise end-to-end.

use std::fs;
use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn recording_e2e() -> String {
    let path = manifest_dir().join("scripts/recording-e2e.sh");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn latency_pins_snapshot_is_invoked_with_all_three_hosts_and_the_verdict_json() {
    let s = recording_e2e();
    assert!(
        s.contains("latency_pins_snapshot.py"),
        "recording-e2e.sh must invoke scripts/latency_pins_snapshot.py"
    );
    assert!(
        s.contains("--strih-host \"$STRIH\"")
            && s.contains("--imag-host \"$IMAG_IP\"")
            && s.contains("--stream-host \"$STREAM\""),
        "must pass all three rig hosts to the pins snapshot"
    );
    assert!(
        s.contains("--verdict-json \"$REPORT_JSON\""),
        "must feed the pins snapshot THIS run's own verdict JSON (for the delivery-p50 -> \
         recommended-pins computation), not a stale/different one"
    );
}

#[test]
fn latency_pins_snapshot_runs_after_verdict_and_before_discord_send() {
    let s = recording_e2e();
    let verdict_idx = s
        .find("\"$VERDICT_BIN\" \"${MERGE_ARGS[@]}\"")
        .expect("the merge recording-verdict execution must exist");
    let pins_idx = s
        .find("latency_pins_snapshot.py")
        .expect("the pins snapshot call must exist");
    let send_idx = s
        .find("e2e_discord_report_send \"$REPORT_JSON\"")
        .expect("the Discord report send call must exist");
    assert!(
        verdict_idx < pins_idx,
        "the pins snapshot must run AFTER the verdict is computed (it needs this run's own \
         delivery-latency table)"
    );
    assert!(
        pins_idx < send_idx,
        "the pins snapshot must run BEFORE the Discord report send (so the pins land in the \
         SAME report, not a follow-up message)"
    );
}

#[test]
fn latency_pins_snapshot_failure_is_fail_open_never_touches_gate() {
    let s = recording_e2e();
    let idx = s
        .find("latency_pins_snapshot.py")
        .expect("the pins snapshot call must exist");
    let block = &s[idx.saturating_sub(400)..(idx + 600).min(s.len())];
    assert!(
        block.contains("PINS_JSON=\"\""),
        "on a pins-snapshot failure, PINS_JSON must be reset to empty (so the Discord report \
         composer omits the section entirely instead of pointing at a bogus/partial file): {block}"
    );
    assert!(
        !block.contains("exit 1") && !block.contains("GATE=1"),
        "a pins-snapshot failure must NEVER affect the run's own exit code / $GATE (fail-open, \
         same discipline as the Discord report send itself): {block}"
    );
}

#[test]
fn discord_report_send_receives_the_pins_json_path() {
    let s = recording_e2e();
    assert!(
        s.contains("e2e_discord_report_send \"$REPORT_JSON\" \"$RUN_ID\" \"$GATE\" \"$DURATION\" \"$PINS_JSON\""),
        "e2e_discord_report_send must be called with the pins JSON path as its 5th argument"
    );
}
