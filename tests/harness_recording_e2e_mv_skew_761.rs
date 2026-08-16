//! #761 — `scripts/recording-e2e.sh` must run `scripts/mv_skew_snapshot.py` (the per-camera
//! MV-clone-vs-main presentation-skew gatherer) AFTER the verdict JSON exists and BEFORE the
//! Discord report sends, fail-open (never affecting `$GATE`), and thread the resulting file into
//! `e2e_discord_report_send`'s new 6th arg. `scripts/lib/e2e-discord-report.sh` must forward it as
//! `--mv-skew-json`, guarded the same fail-open way as the #756 pins arg.
//!
//! Structural, source-text assertions — same discipline as tests/harness_recording_e2e_latency_pins_756.rs
//! (a read-only report step only the live rig can exercise end-to-end).

use std::fs;
use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn recording_e2e() -> String {
    let path = manifest_dir().join("scripts/recording-e2e.sh");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn report_lib() -> String {
    let path = manifest_dir().join("scripts/lib/e2e-discord-report.sh");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn mv_skew_snapshot_is_invoked_on_imag() {
    let s = recording_e2e();
    // Anchor on the INVOCATION form (never a bare token a comment could satisfy) — the
    // burn-target-enumeration.md lesson.
    assert!(
        s.contains("\"$HERE/mv_skew_snapshot.py\""),
        "#761: recording-e2e.sh must invoke scripts/mv_skew_snapshot.py"
    );
    assert!(
        s.contains("--host \"$IMAG_IP\"") && s.contains("--out \"$MV_SKEW_JSON\""),
        "#761: the MV-skew snapshot must target imag ($IMAG_IP) and write $MV_SKEW_JSON"
    );
}

#[test]
fn mv_skew_snapshot_runs_after_the_verdict_and_before_the_report_send() {
    let s = recording_e2e();
    let verdict_idx = s
        .find("\"$VERDICT_BIN\" \"${MERGE_ARGS[@]}\"")
        .expect("the merge recording-verdict execution must exist");
    let mv_idx = s
        .find("\"$HERE/mv_skew_snapshot.py\"")
        .expect("the MV-skew snapshot call must exist");
    let send_idx = s
        .find("e2e_discord_report_send \"$REPORT_JSON\"")
        .expect("the Discord report send call must exist");
    assert!(
        verdict_idx < mv_idx,
        "#761: the MV-skew snapshot must run AFTER the verdict is computed (it needs live rig state)"
    );
    assert!(
        mv_idx < send_idx,
        "#761: the MV-skew snapshot must run BEFORE the Discord report send (so it lands in the \
         SAME report, not a follow-up message)"
    );
}

#[test]
fn mv_skew_snapshot_failure_is_fail_open_never_touches_gate() {
    let s = recording_e2e();
    let idx = s
        .find("\"$HERE/mv_skew_snapshot.py\"")
        .expect("the MV-skew snapshot call must exist");
    let block = &s[idx.saturating_sub(200)..(idx + 500).min(s.len())];
    assert!(
        block.contains("MV_SKEW_JSON=\"\""),
        "#761: on failure, MV_SKEW_JSON must be reset to empty (so the composer omits the section \
         instead of pointing at a bogus/partial file): {block}"
    );
    assert!(
        !block.contains("exit 1") && !block.contains("GATE=1"),
        "#761: an MV-skew snapshot failure must NEVER affect the run's own exit code / $GATE \
         (fail-open, same discipline as the pins snapshot + the Discord report send): {block}"
    );
}

#[test]
fn discord_report_send_receives_the_mv_skew_json_as_sixth_arg() {
    let s = recording_e2e();
    assert!(
        s.contains(
            "e2e_discord_report_send \"$REPORT_JSON\" \"$RUN_ID\" \"$GATE\" \"$DURATION\" \
             \"$PINS_JSON\" \"$MV_SKEW_JSON\""
        ),
        "#761: e2e_discord_report_send must be called with the MV-skew JSON path as its 6th argument"
    );
}

#[test]
fn report_lib_forwards_mv_skew_json_flag_fail_open() {
    let s = report_lib();
    assert!(
        s.contains("mv_skew_json=\"${6:-}\""),
        "#761: e2e_discord_report_send must accept the MV-skew JSON path as its 6th positional arg"
    );
    // Forwarded ONLY when the file is non-empty AND exists (same guard as the #756 pins arg), so
    // the composer never opens a bogus/absent file.
    assert!(
        s.contains("--mv-skew-json")
            && s.contains("[ -n \"$mv_skew_json\" ] && [ -s \"$mv_skew_json\" ]"),
        "#761: the lib must forward --mv-skew-json only for a non-empty existing file (fail-open)"
    );
}
