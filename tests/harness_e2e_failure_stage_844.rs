//! #844 — the failed-gate Discord alert must report the STAGE that actually failed, and must never
//! claim a frame-loss/latency verdict for a run that produced no verdict. Before this, the
//! `if: failure()` step in `.github/workflows/full-path-e2e.yml` hardcoded a single
//! "cam2→cam1→strih→stream frame-loss/latency gate breached" message that fired on ANY job
//! failure — including a `[0/8]` preflight abort that never recorded a frame.
//!
//! The stage is DERIVED from the run's durable on-runner artifacts (the same
//! `verdict-<RUN_ID>.json` the #703 fail-closed guard already trusts, plus the downloaded
//! recordings), by the pure, side-effect-free helper `scripts/lib/e2e-failure-stage.sh`. This test
//! drives that helper across every bucket and asserts the workflow step now uses it.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    manifest_dir().join("scripts/lib/e2e-failure-stage.sh")
}

fn read_workflow() -> String {
    std::fs::read_to_string(manifest_dir().join(".github/workflows/full-path-e2e.yml"))
        .expect("read full-path-e2e.yml")
}

/// Run a bash snippet with the helper sourced; return (exit_code, stdout, stderr).
fn run(body: &str) -> (i32, String, String) {
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(". \"$LIB\"\nset +e\n{body}"))
        .env("LIB", lib_script())
        .output()
        .expect("run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Same, but reproduces PRODUCTION exactly: `set -euo pipefail` BEFORE sourcing+calling — the
/// workflow step (`.github/workflows/full-path-e2e.yml`) runs the helper under that strict shell.
/// This is the only way to catch a regression that would abort the step (an unbound var / a
/// nonzero mid-function) and SWALLOW the alert entirely in CI — a `set +e` harness cannot.
fn run_strict(body: &str) -> (i32, String, String) {
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!("set -euo pipefail\n. \"$LIB\"\n{body}"))
        .env("LIB", lib_script())
        .output()
        .expect("run bash harness (strict)");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Every message, whatever the bucket, must be a clear FAILED alert carrying the short SHA and the
/// run URL — the operator-facing invariants that never change.
fn assert_common(msg: &str) {
    assert!(msg.contains("FAILED"), "must announce FAILED: {msg}");
    assert!(msg.contains("abc1234"), "must carry the short SHA: {msg}");
    assert!(
        msg.contains("https://x/run"),
        "must carry the run URL: {msg}"
    );
}

#[test]
fn empty_run_id_reports_startup_abort_never_a_breach() {
    let (code, out, err) = run(r#"e2e_failure_stage_content "" "" abc1234 https://x/run"#);
    assert_eq!(code, 0, "stderr={err}");
    assert_common(&out);
    assert!(
        !out.to_lowercase().contains("breach"),
        "a run with no artifacts must NOT claim a frame-loss breach: {out}"
    );
    assert!(
        out.to_lowercase().contains("no frame-loss measurement")
            || out.to_lowercase().contains("no run artifacts"),
        "must state no measurement was taken: {out}"
    );
}

#[test]
fn preflight_abort_no_recording_no_verdict_reports_no_measurement() {
    let (code, out, err) = run(
        r#"d=$(mktemp -d); e2e_failure_stage_content "$d" 999 abc1234 https://x/run; rm -rf "$d""#,
    );
    assert_eq!(code, 0, "stderr={err}");
    assert_common(&out);
    assert!(
        !out.to_lowercase().contains("breach"),
        "a run that never recorded must NOT claim a breach: {out}"
    );
    assert!(
        out.to_lowercase().contains("no frame-loss measurement"),
        "must state no frame-loss measurement was taken: {out}"
    );
    assert!(
        out.to_lowercase().contains("preflight"),
        "must name the preflight/deploy pre-recording stage: {out}"
    );
}

#[test]
fn recordings_present_but_no_verdict_reports_decode_stage_never_a_breach() {
    let (code, out, err) = run(
        r#"d=$(mktemp -d); echo x > "$d/strih-999.mkv"; e2e_failure_stage_content "$d" 999 abc1234 https://x/run; rm -rf "$d""#,
    );
    assert_eq!(code, 0, "stderr={err}");
    assert_common(&out);
    assert!(
        !out.to_lowercase().contains("breach"),
        "recordings-but-no-verdict must NOT claim a breach: {out}"
    );
    assert!(
        out.to_lowercase().contains("no verdict"),
        "must state no verdict was produced: {out}"
    );
    assert!(
        out.to_lowercase().contains("decode"),
        "must name the decode/verdict stage: {out}"
    );
}

#[test]
fn failing_verdict_is_the_only_bucket_that_claims_a_frame_loss_breach() {
    let (code, out, err) = run(
        r#"d=$(mktemp -d); echo '{"overall_pass":false}' > "$d/verdict-999.json"; e2e_failure_stage_content "$d" 999 abc1234 https://x/run; rm -rf "$d""#,
    );
    assert_eq!(code, 0, "stderr={err}");
    assert_common(&out);
    assert!(
        out.to_lowercase().contains("breach") && out.to_lowercase().contains("frame-loss"),
        "a genuinely failing verdict IS a frame-loss breach: {out}"
    );
    assert!(
        out.to_uppercase().contains("VERDICT"),
        "must name the verdict stage: {out}"
    );
}

#[test]
fn passing_verdict_with_a_later_step_failure_never_claims_a_breach() {
    let (code, out, err) = run(
        r#"d=$(mktemp -d); echo '{"overall_pass":true}' > "$d/verdict-999.json"; e2e_failure_stage_content "$d" 999 abc1234 https://x/run; rm -rf "$d""#,
    );
    assert_eq!(code, 0, "stderr={err}");
    assert_common(&out);
    assert!(
        !out.to_lowercase().contains("breach"),
        "a PASSING verdict must never be reported as a breach: {out}"
    );
    assert!(
        out.to_lowercase().contains("passed"),
        "must say the verdict itself passed: {out}"
    );
}

#[test]
fn malformed_or_empty_verdict_json_is_flagged_untrustworthy_never_a_breach() {
    // The `*)` case — the ticket's CENTRAL safety property: a verdict file that exists but is
    // unreadable (jq missing, malformed JSON) or missing the overall_pass key must NEVER be read
    // as a benign pass NOR falsely announced as a breach — it is "no trustworthy verdict".
    for fixture in ["not json{", "{}"] {
        let (code, out, err) = run(&format!(
            r#"d=$(mktemp -d); printf '%s' '{fixture}' > "$d/verdict-999.json"; e2e_failure_stage_content "$d" 999 abc1234 https://x/run; rm -rf "$d""#
        ));
        assert_eq!(code, 0, "fixture={fixture:?} stderr={err}");
        assert_common(&out);
        assert!(
            !out.to_lowercase().contains("breach"),
            "an unreadable verdict must NOT claim a breach (fixture={fixture:?}): {out}"
        );
        assert!(
            out.to_lowercase().contains("unreadable")
                || out.to_lowercase().contains("no trustworthy"),
            "must flag the verdict untrustworthy (fixture={fixture:?}): {out}"
        );
    }
}

#[test]
fn helper_never_swallows_the_alert_under_production_strict_shell() {
    // #844 review-focus: production sources+calls the helper under `set -euo pipefail`. A future
    // regression (a dropped `|| printf` guard, an unbound var) would ABORT the step and post NO
    // alert at all — invisible to a `set +e` harness. Lock it: under the strict shell every bucket
    // must still exit 0 and emit its line. Cover the two riskiest paths (failing + malformed
    // verdict) plus the no-artifact path.
    let cases = [
        (
            r#"d=$(mktemp -d); echo '{"overall_pass":false}' > "$d/verdict-999.json"; e2e_failure_stage_content "$d" 999 abc1234 https://x/run; rm -rf "$d""#,
            "breach",
        ),
        (
            r#"d=$(mktemp -d); printf '%s' 'not json{' > "$d/verdict-999.json"; e2e_failure_stage_content "$d" 999 abc1234 https://x/run; rm -rf "$d""#,
            "no trustworthy",
        ),
        (
            r#"e2e_failure_stage_content "" "" abc1234 https://x/run"#,
            "no frame-loss measurement",
        ),
    ];
    for (body, needle) in cases {
        let (code, out, err) = run_strict(body);
        assert_eq!(
            code, 0,
            "strict shell must not swallow the alert (body={body}): stderr={err}"
        );
        assert!(
            out.to_lowercase().contains(needle),
            "strict-shell output must still carry '{needle}': {out}"
        );
    }
}

#[test]
fn workflow_failure_step_uses_the_stage_helper_not_a_hardcoded_breach_claim() {
    let s = read_workflow();
    assert!(
        s.contains("scripts/lib/e2e-failure-stage.sh") && s.contains("e2e_failure_stage_content"),
        "#844: the failure-alert step must source scripts/lib/e2e-failure-stage.sh and call \
         e2e_failure_stage_content"
    );
    assert!(
        !s.contains("frame-loss/latency gate breached"),
        "#844: the workflow must no longer hardcode an unconditional 'frame-loss/latency gate \
         breached' claim — it moves into the helper, emitted ONLY for a genuinely failing verdict"
    );
}
