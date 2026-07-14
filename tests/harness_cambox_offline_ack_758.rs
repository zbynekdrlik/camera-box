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
