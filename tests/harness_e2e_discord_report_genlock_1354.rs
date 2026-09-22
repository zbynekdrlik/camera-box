//! #1354 scope 3 — static-anchor pins for the per-input genlock-conveyor DELTA report section.
//!
//! The section (`_section_genlock_conveyor` in scripts/e2e_discord_report.py) is REPORT-ONLY: it is
//! wired into the FULL report (`compose_report`) ONLY, never into the Discord summary
//! (`compose_summary` / `--json-chunks`), so the PASS 3-line summary stays byte-identical. The
//! per-input holds/relocks/converge_sheds delta comes from scripts/genlock_audit_snapshot.py,
//! threaded through scripts/lib/e2e-discord-report.sh as a fail-open `--genlock-audit-json` 7th arg
//! (the same guarded shape as the #756 pins / #761 mv-skew args).
//!
//! Structural, source-text assertions — the same discipline as
//! tests/harness_recording_e2e_mv_skew_761.rs (a report-only step the pure pytest already exercises;
//! this pins the WIRING so the section can never silently leak into the summary or lose its
//! fail-open guard). The behaviour itself is covered by tests/python/test_e2e_discord_report_genlock_1354.py.

use std::fs;
use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn report_py() -> String {
    let path = manifest_dir().join("scripts/e2e_discord_report.py");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn report_lib() -> String {
    let path = manifest_dir().join("scripts/lib/e2e-discord-report.sh");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn genlock_conveyor_section_is_defined_and_wired_into_compose_report() {
    let s = report_py();
    assert!(
        s.contains("def _section_genlock_conveyor("),
        "#1354: e2e_discord_report.py must define the _section_genlock_conveyor formatter"
    );
    // The section is appended inside compose_report (between its def and the next top-level def).
    let report_start = s
        .find("def compose_report(")
        .expect("#1354: compose_report must exist");
    let report_body = &s[report_start..];
    let report_end = report_body
        .find("\ndef chunk_for_discord(")
        .expect("#1354: chunk_for_discord must follow compose_report");
    let report_body = &report_body[..report_end];
    assert!(
        report_body.contains("_section_genlock_conveyor(verdict, meta)"),
        "#1354: compose_report must append the genlock-conveyor section"
    );
}

#[test]
fn genlock_conveyor_section_is_report_only_never_in_the_summary() {
    let s = report_py();
    // compose_summary (the #1127 Discord summary) must NEVER call the section — the summary is the
    // byte-identical PASS 3-line output, and a report-only metric must never render there.
    let summary_start = s
        .find("def compose_summary(")
        .expect("#1354: compose_summary must exist");
    let summary_body = &s[summary_start..];
    let summary_end = summary_body
        .find("\ndef compose_report(")
        .expect("#1354: compose_report must follow compose_summary");
    let summary_body = &summary_body[..summary_end];
    assert!(
        !summary_body.contains("_section_genlock_conveyor"),
        "#1354: the genlock-conveyor section must be REPORT-ONLY — never wired into compose_summary \
         (that would break the byte-identical Discord summary contract)"
    );
}

#[test]
fn report_lib_forwards_genlock_audit_json_flag_fail_open() {
    let s = report_lib();
    assert!(
        s.contains("genlock_audit_json=\"${7:-}\""),
        "#1354: e2e_discord_report_send must accept the genlock-audit JSON path as its 7th positional arg"
    );
    // Forwarded ONLY when the file is non-empty AND exists (same fail-open guard as the #761 mv-skew
    // arg), so the composer never opens a bogus/absent file.
    assert!(
        s.contains("--genlock-audit-json")
            && s.contains("[ -n \"$genlock_audit_json\" ] && [ -s \"$genlock_audit_json\" ]"),
        "#1354: the lib must forward --genlock-audit-json only for a non-empty existing file (fail-open)"
    );
}
