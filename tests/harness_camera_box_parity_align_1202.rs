//! issue 1202 — pre-gate auto-align of the active cam fleet to the run's candidate camera-box
//! build, so the `[0/8]` camera-box version-parity gate's existing `--candidate-pin` accept passes
//! without a manual `deploy-fleet` on the version-parity treadmill.
//!
//! ROOT CAUSE the align fixes: `camera-box-version-gate.sh` (#875/#1136) pins the fleet's
//! `/usr/local/bin/camera-box` to `origin/main`, with a candidate-pin accept that passes only when
//! the whole active fleet is uniformly ON this run's candidate. During active dev `origin/main`
//! lags `dev` by dozens of builds, so the candidate-pin accept is the only passing path — but each
//! dev commit bumps the candidate, leaving the fleet one build behind (candidate-1). `[2/8]`/`[2b/8]`
//! scp the candidate binary only to a transient `/tmp` burn path (never `/usr/local/bin/camera-box`)
//! and run AFTER the gate. So the gate refuses every run until a manual `deploy-fleet` (live killed
//! runs 32883434208 / 32892551674).
//!
//! These tests pin the PURE decision `cambox_align_action CANDIDATE ENTRY...` (scripts/lib/
//! camera-box-parity-align.sh) that decides align-vs-refuse. Only `ALIGN` (every active box read AND
//! uniform on ONE version != candidate) authorises a deploy; MIXED / UNKNOWN / NOACTIVE / NOCANDIDATE
//! / already-OK never deploy — so "versions differing BETWEEN boxes stays REFUSED" is preserved in
//! the align itself (and doubly by the untouched gate downstream).
//!
//! RED before issue 1202 (the lib's `cambox_align_action` is a stub that always prints MIXED, so
//! every non-MIXED case fails); GREEN after the real decision lands.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Source the lib under the caller's REAL `set -euo pipefail` (recording-e2e.sh's own opts — a
/// sourced-lib decision function must be safe there, ci-testing-gotchas.md #1133) and run
/// `cambox_align_action CANDIDATE ENTRY...`, returning its printed verdict (trimmed).
/// `ack` (may be empty) is exported as CAMBOX_OFFLINE_ACK for the acked-exclusion cases.
fn action(ack: &str, candidate: &str, entries: &[&str]) -> String {
    let lib = manifest_dir().join("scripts/lib/camera-box-parity-align.sh");
    assert!(lib.exists(), "{} not found", lib.display());
    // Build the argument list: candidate first, then each name=version entry, each single-quoted.
    let mut args = String::new();
    for a in std::iter::once(candidate).chain(entries.iter().copied()) {
        args.push_str(" '");
        args.push_str(&a.replace('\'', r"'\''"));
        args.push('\'');
    }
    let harness = format!("set -euo pipefail\n. \"$LIB\"\ncambox_align_action{args}\n");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", &lib)
        .env("CAMBOX_OFFLINE_ACK", ack)
        .output()
        .expect("failed to run cambox_align_action harness");
    assert!(
        out.status.success(),
        "harness exited non-zero for candidate={candidate:?} entries={entries:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn align_action_all_boxes_already_on_candidate_is_ok() {
    assert_eq!(
        action("", "1.7.0-dev.551", &["cam3=1.7.0-dev.551"]),
        "OK",
        "a fleet already on the candidate needs no align (the gate passes via candidate-pin)"
    );
    assert_eq!(
        action(
            "",
            "1.7.0-dev.551",
            &["cam3=1.7.0-dev.551", "cam4=1.7.0-dev.551"]
        ),
        "OK"
    );
}

#[test]
fn align_action_uniform_stale_fleet_is_align() {
    // The exact live treadmill shape: fleet uniformly one build behind the candidate.
    assert_eq!(
        action("", "1.7.0-dev.551", &["cam3=1.7.0-dev.550"]),
        "ALIGN",
        "a fleet uniformly on ONE stale build != candidate must auto-align to the candidate"
    );
    assert_eq!(
        action(
            "",
            "1.7.0-dev.552",
            &["cam3=1.7.0-dev.551", "cam4=1.7.0-dev.551"]
        ),
        "ALIGN"
    );
}

#[test]
fn align_action_mixed_fleet_is_refused_never_aligned() {
    // Versions differing BETWEEN boxes must NEVER auto-deploy — mixed protection (issue 1202 HARD
    // constraint). The untouched gate then refuses it.
    assert_eq!(
        action(
            "",
            "1.7.0-dev.552",
            &["cam3=1.7.0-dev.550", "cam4=1.7.0-dev.551"]
        ),
        "MIXED",
        "a fleet with versions differing BETWEEN boxes must stay REFUSED, never auto-aligned"
    );
}

#[test]
fn align_action_any_unread_box_is_unknown_never_aligned() {
    // An unread box (empty version) — even if every OTHER box agrees — must fail closed: deploying
    // would target an unreachable box. The gate then fails CLOSED (UNKNOWN=11).
    assert_eq!(
        action("", "1.7.0-dev.552", &["cam3=1.7.0-dev.551", "cam4="]),
        "UNKNOWN",
        "a uniform-but-partially-unread fleet must NOT auto-align (fail closed)"
    );
    // Unknown takes precedence even when the read boxes also disagree.
    assert_eq!(
        action(
            "",
            "1.7.0-dev.552",
            &["cam3=1.7.0-dev.550", "cam4=1.7.0-dev.549", "cam5="]
        ),
        "UNKNOWN"
    );
}

#[test]
fn align_action_empty_candidate_is_nocandidate() {
    assert_eq!(
        action("", "", &["cam3=1.7.0-dev.550"]),
        "NOCANDIDATE",
        "no resolvable candidate -> no align; the gate decides"
    );
}

#[test]
fn align_action_all_acked_offline_is_noactive() {
    assert_eq!(
        action(
            "cam3:card-swap,cam4:battery",
            "1.7.0-dev.551",
            &["cam3=", "cam4="]
        ),
        "NOACTIVE",
        "every listed box acked-offline -> nothing to align (the gate vacuous-passes)"
    );
}

#[test]
fn align_action_excludes_acked_box_from_the_uniformity_check() {
    // An acked-offline box is excluded from the align decision exactly as the gate excludes it.
    // cam4 acked+unread -> ignored; cam3 on the candidate -> OK (no align needed).
    assert_eq!(
        action(
            "cam4:battery",
            "1.7.0-dev.551",
            &["cam3=1.7.0-dev.551", "cam4="]
        ),
        "OK",
        "an acked box must not force UNKNOWN, and must not break the active-fleet verdict"
    );
    // cam4 acked (any version) -> ignored; cam3 uniformly stale -> ALIGN off cam3 alone.
    assert_eq!(
        action(
            "cam4:battery",
            "1.7.0-dev.552",
            &["cam3=1.7.0-dev.551", "cam4=1.7.0-dev.400"]
        ),
        "ALIGN",
        "an acked box's version must not enter the uniformity check"
    );
}
