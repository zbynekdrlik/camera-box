//! #758 — the CAMBOX_OFFLINE_ACK exclusion mechanism (`scripts/lib/cambox-offline-ack.sh`): a
//! named, loud, temporary exclusion for a box known-offline for a reason outside this harness's
//! control (e.g. cam7's V-mount battery discharging mid-run). Proven for REAL by sourcing the
//! lib and executing its functions against a real bash, not just reading its source text.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    manifest_dir().join("scripts/lib/cambox-offline-ack.sh")
}

struct Run {
    exit_code: i32,
    stdout: String,
}

fn run_sourced(env_ack: Option<&str>, body: &str) -> Run {
    let harness = format!("set -uo pipefail\n. {:?}\n{body}", script());
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&harness);
    if let Some(ack) = env_ack {
        cmd.env("CAMBOX_OFFLINE_ACK", ack);
    } else {
        cmd.env_remove("CAMBOX_OFFLINE_ACK");
    }
    let out = cmd.output().expect("failed to run bash harness");
    Run {
        exit_code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).trim().to_string(),
    }
}

#[test]
fn reason_returns_empty_when_no_ack_set_at_all() {
    let r = run_sourced(None, "cambox_offline_ack_reason cam7");
    assert_eq!(r.exit_code, 0);
    assert_eq!(
        r.stdout, "",
        "no CAMBOX_OFFLINE_ACK -> no reason for any box"
    );
}

#[test]
fn reason_returns_the_reason_for_an_acked_box() {
    let r = run_sourced(
        Some("cam7:vmount-battery-discharged-2026-07-14"),
        "cambox_offline_ack_reason cam7",
    );
    assert_eq!(r.exit_code, 0);
    assert_eq!(r.stdout, "vmount-battery-discharged-2026-07-14");
}

#[test]
fn reason_is_empty_for_a_box_not_named_in_the_ack() {
    let r = run_sourced(
        Some("cam7:vmount-battery-discharged-2026-07-14"),
        "cambox_offline_ack_reason cam5",
    );
    assert_eq!(r.exit_code, 0);
    assert_eq!(
        r.stdout, "",
        "cam5 is not acked -- must not fall through to cam7's reason"
    );
}

#[test]
fn reason_parses_multiple_comma_separated_entries() {
    let r = run_sourced(
        Some("cam7:vmount-battery-discharged-2026-07-14,cam5:hdmi-splitter-swap"),
        "cambox_offline_ack_reason cam5",
    );
    assert_eq!(r.exit_code, 0);
    assert_eq!(r.stdout, "hdmi-splitter-swap");
}

#[test]
fn reason_never_substring_matches_a_similar_box_name() {
    // "cam7" must not match "cam70" or "camera7" -- exact box-name match only.
    let r1 = run_sourced(Some("cam70:typo"), "cambox_offline_ack_reason cam7");
    assert_eq!(r1.stdout, "", "cam7 must not substring-match cam70's ack");
    let r2 = run_sourced(Some("camera7:typo"), "cambox_offline_ack_reason cam7");
    assert_eq!(r2.stdout, "", "cam7 must not substring-match camera7's ack");
}

#[test]
fn reason_accepts_a_bare_box_name_with_no_colon_reason() {
    let r = run_sourced(Some("cam7"), "cambox_offline_ack_reason cam7");
    assert_eq!(r.exit_code, 0);
    assert_eq!(
        r.stdout, "unspecified",
        "a bare box name (no ':reason') is still accepted as acked, with an unspecified reason"
    );
}

#[test]
fn is_acked_true_for_a_named_box_false_otherwise() {
    let ack = Some("cam7:vmount-battery-discharged-2026-07-14");
    let r_yes = run_sourced(
        ack,
        "cambox_offline_ack_is_acked cam7 && echo YES || echo NO",
    );
    assert_eq!(r_yes.stdout, "YES");
    let r_no = run_sourced(
        ack,
        "cambox_offline_ack_is_acked cam5 && echo YES || echo NO",
    );
    assert_eq!(r_no.stdout, "NO");
    let r_none = run_sourced(
        None,
        "cambox_offline_ack_is_acked cam7 && echo YES || echo NO",
    );
    assert_eq!(r_none.stdout, "NO", "no ack at all -> nothing is acked");
}

#[test]
fn note_message_names_the_box_and_the_reason_loudly() {
    let r = run_sourced(
        Some("cam7:vmount-battery-discharged-2026-07-14"),
        "cambox_offline_ack_note cam7",
    );
    assert_eq!(r.exit_code, 0);
    assert!(
        r.stdout.starts_with("[preflight] NOTE: cam7 EXCLUDED"),
        "stdout={}",
        r.stdout
    );
    assert!(
        r.stdout.contains("vmount-battery-discharged-2026-07-14"),
        "stdout={}",
        r.stdout
    );
}

#[test]
fn stale_ack_message_is_a_loud_error_naming_the_box() {
    let r = run_sourced(
        Some("cam7:vmount-battery-discharged-2026-07-14"),
        "cambox_offline_ack_stale_message cam7",
    );
    assert_eq!(r.exit_code, 0);
    assert!(r.stdout.starts_with("ERROR:"), "stdout={}", r.stdout);
    assert!(r.stdout.contains("STALE ACK"), "stdout={}", r.stdout);
    assert!(r.stdout.contains("cam7"), "stdout={}", r.stdout);
}

// #827 items 1+2 — the E2E workflow never set CAMBOX_OFFLINE_ACK at all (no way in from CI), and
// the existing stale-ack check conflated "reachable" with "healthy" (a box whose OS/network is up
// but whose service is stuck e.g. `activating` forever -- cam4's grabber card physically removed,
// 2026-07-27 -- could never be acked without hitting the STALE hard-fail). These three new pure
// functions fix both: `cambox_offline_ack_effective` wires in a repo-level default-ack file when
// CI sets no explicit override; `cambox_offline_ack_decide` is the single source of truth for the
// healthy/unhealthy/unreachable x acked/unacked decision matrix (unhealthy+acked -> exclude, never
// stale); `cambox_offline_ack_excluded_json` builds the JSON the harness merges into the verdict
// so an excluded-box run can never read back as full-fleet-clean.

fn write_temp_file(contents: &str) -> tempfile::NamedTempFile {
    use std::io::Write;
    let mut f = tempfile::NamedTempFile::new().expect("create temp file");
    f.write_all(contents.as_bytes()).expect("write temp file");
    f
}

#[test]
fn effective_prefers_the_explicit_ack_over_the_default_file() {
    let file = write_temp_file("cam4:grabber-card-removed\n");
    let r = run_sourced(
        None,
        &format!(
            "cambox_offline_ack_effective 'cam7:explicit-reason' {:?}",
            file.path()
        ),
    );
    assert_eq!(r.exit_code, 0);
    assert_eq!(
        r.stdout, "cam7:explicit-reason",
        "an explicit ack must win outright over the checked-in default file"
    );
}

#[test]
fn effective_falls_back_to_the_default_file_when_explicit_is_empty() {
    let file = write_temp_file("cam4:grabber-card-removed\ncam5:powered-off\n");
    let r = run_sourced(
        None,
        &format!("cambox_offline_ack_effective '' {:?}", file.path()),
    );
    assert_eq!(r.exit_code, 0);
    assert_eq!(r.stdout, "cam4:grabber-card-removed,cam5:powered-off");
}

#[test]
fn effective_ignores_blank_lines_and_comments_in_the_default_file() {
    let file = write_temp_file(
        "# rig-fleet.txt -- default ack list\n\ncam4:grabber-card-removed\n  \n# another comment\ncam5:powered-off\n",
    );
    let r = run_sourced(
        None,
        &format!("cambox_offline_ack_effective '' {:?}", file.path()),
    );
    assert_eq!(r.exit_code, 0);
    assert_eq!(r.stdout, "cam4:grabber-card-removed,cam5:powered-off");
}

#[test]
fn effective_is_empty_when_no_explicit_and_no_default_file_exists() {
    let r = run_sourced(
        None,
        "cambox_offline_ack_effective '' /nonexistent/rig-fleet.txt",
    );
    assert_eq!(r.exit_code, 0);
    assert_eq!(r.stdout, "");
}

#[test]
fn decide_healthy_and_unacked_is_ok() {
    let r = run_sourced(None, "cambox_offline_ack_decide healthy 0");
    assert_eq!(r.stdout, "ok");
}

#[test]
fn decide_healthy_and_acked_is_stale() {
    // The fleet came back healthy but the ack was never removed -- loud stale warning.
    let r = run_sourced(None, "cambox_offline_ack_decide healthy 1");
    assert_eq!(r.stdout, "stale");
}

#[test]
fn decide_unhealthy_and_acked_is_exclude() {
    // The #827 fix: cam4 is REACHABLE (its OS/network answers ping) but UNHEALTHY (service
    // stuck activating -- grabber card removed). Acked -> must EXCLUDE, never STALE.
    let r = run_sourced(None, "cambox_offline_ack_decide unhealthy 1");
    assert_eq!(r.stdout, "exclude");
}

#[test]
fn decide_unhealthy_and_unacked_is_fail() {
    let r = run_sourced(None, "cambox_offline_ack_decide unhealthy 0");
    assert_eq!(r.stdout, "fail");
}

#[test]
fn decide_unreachable_and_acked_is_exclude() {
    let r = run_sourced(None, "cambox_offline_ack_decide unreachable 1");
    assert_eq!(r.stdout, "exclude");
}

#[test]
fn decide_unreachable_and_unacked_is_fail() {
    let r = run_sourced(None, "cambox_offline_ack_decide unreachable 0");
    assert_eq!(r.stdout, "fail");
}

#[test]
fn excluded_json_is_empty_array_for_no_excluded_boxes() {
    let r = run_sourced(
        Some("cam4:grabber-card-removed"),
        "cambox_offline_ack_excluded_json ''",
    );
    assert_eq!(r.exit_code, 0);
    let v: serde_json::Value = serde_json::from_str(&r.stdout).expect("valid JSON");
    assert_eq!(v, serde_json::json!([]));
}

#[test]
fn excluded_json_names_each_excluded_box_with_its_reason() {
    let r = run_sourced(
        Some("cam4:grabber-card-removed,cam5:powered-off"),
        "cambox_offline_ack_excluded_json 'cam4 cam5'",
    );
    assert_eq!(r.exit_code, 0);
    let v: serde_json::Value = serde_json::from_str(&r.stdout).expect("valid JSON");
    assert_eq!(
        v,
        serde_json::json!([
            {"box": "cam4", "reason": "grabber-card-removed"},
            {"box": "cam5", "reason": "powered-off"}
        ])
    );
}

#[test]
fn excluded_json_reaches_the_verdict_output_via_jq_merge() {
    // (c) the exclusion list reaches the verdict output -- the exact merge recording-e2e.sh
    // performs on $REPORT_JSON after the verdict binary writes it.
    let verdict = write_temp_file(r#"{"overall_pass": true, "hops": {}}"#);
    let r = run_sourced(
        Some("cam4:grabber-card-removed"),
        &format!(
            "excluded=\"$(cambox_offline_ack_excluded_json 'cam4')\"; \
             jq --argjson excluded \"$excluded\" '.excluded_cams = $excluded' {:?}",
            verdict.path()
        ),
    );
    assert_eq!(r.exit_code, 0, "stdout={}", r.stdout);
    let v: serde_json::Value = serde_json::from_str(&r.stdout).expect("valid JSON");
    assert_eq!(v["overall_pass"], serde_json::json!(true));
    assert_eq!(
        v["excluded_cams"],
        serde_json::json!([{"box": "cam4", "reason": "grabber-card-removed"}])
    );
}
