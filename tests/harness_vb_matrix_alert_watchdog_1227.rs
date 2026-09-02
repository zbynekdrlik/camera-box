//! #1227 — offline integration guard for `scripts/vb-matrix-alert-watchdog.sh`'s DECISION
//! COMPOSITION (`main`/`handle_box`): the RUNNING clear, the DOWN 2-pass confirm then alert, the
//! facet-absent UNKNOWN (a non-VB-Matrix box like imag, never a page), the box-down SKIP (#732/#1001
//! no-double-page), and the recovery ping. The pure decision core is covered exhaustively by
//! `tests/python/test_vb_matrix_decision.py`; this file covers the CALLER GLUE that drives the
//! alert/recovery side effects, so a wiring / discriminator regression is caught (per #414 — an
//! unattended production timer's novel logic must be tested, weighted like a correctness bug).
//!
//! Fully offline + deterministic: the `:8899` fetch is replaced via `VB_MATRIX_FETCH_CMD` (so
//! `require_tools` needs no curl), the run is `--dry-run` (never a real notify), and the state file
//! is a per-test tempdir path (`.claude/rules/ci-testing-gotchas.md` #975 tempdir isolation). The
//! probe echoes a fixture body the test sets per pass. Same fixture-shim pattern as
//! `tests/harness_asio_starve_alert_watchdog_1023.rs`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn watchdog() -> PathBuf {
    let s = manifest_dir().join("scripts/vb-matrix-alert-watchdog.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

fn write_exec(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

// Bundle-state JSON bodies for one box.
fn running_body() -> &'static str {
    r#"{"vb_matrix_running":"1","vb_matrix_name":"VBAudioMatrix_x64","vb_matrix_pid":"8144","vb_matrix_start":"2026-09-02T14:01:40"}"#
}
fn down_body() -> &'static str {
    r#"{"vb_matrix_running":"0","obs_version":"32.1.2"}"#
}
fn unknown_body() -> &'static str {
    // A box with no VB-Matrix install (imag): the gather omits the facet entirely.
    r#"{"obs_version":"32.1.2"}"#
}

/// A per-test rig: a tempdir with a fetch.sh that prints the fixture body the test sets in
/// `VB_MATRIX_TEST_BODY` (the literal "SKIP" makes it exit 1 = a fetch failure). The alert state
/// file persists across passes so the confirm / alert / recovery latches behave as on the real
/// 5-min timer.
struct Rig {
    _dir: tempfile::TempDir,
    fetch: PathBuf,
    state: PathBuf,
}

impl Rig {
    fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let fetch = dir.path().join("fetch.sh");
        write_exec(
            &fetch,
            "#!/usr/bin/env bash\nset -uo pipefail\n[ \"${VB_MATRIX_TEST_BODY:-}\" = SKIP ] && exit 1\nprintf '%s' \"${VB_MATRIX_TEST_BODY:-}\"\n",
        );
        Rig {
            state: dir.path().join("vb-matrix.state"),
            fetch,
            _dir: dir,
        }
    }

    /// Run ONE `--dry-run` pass with `body` as the box's bundle-state JSON (or "SKIP" = fetch fail).
    fn pass(&self, body: &str) -> String {
        let out = Command::new("bash")
            .arg(watchdog())
            .arg("--dry-run")
            .env(
                "VB_MATRIX_FETCH_CMD",
                format!("bash {}", self.fetch.display()),
            )
            .env("VB_MATRIX_TEST_BODY", body)
            .env("VB_MATRIX_ALERT_STATE_FILE", &self.state)
            .env("VB_MATRIX_BOXES", "stream|1.2.3.4")
            .current_dir(manifest_dir())
            .output()
            .expect("failed to run watchdog");
        assert!(
            out.status.success(),
            "watchdog pass exited non-zero: {}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    }
}

// ---------------------------------------------------------------------------------------------
// DOWN (install present, no process): pass 1 HOLDS (2-pass confirm), pass 2 ALERTS.
// ---------------------------------------------------------------------------------------------
#[test]
fn down_holds_one_pass_then_alerts() {
    let rig = Rig::new();
    let p1 = rig.pass(down_body());
    assert!(
        p1.contains("verdict=DOWN"),
        "pass1 should classify DOWN: {p1}"
    );
    assert!(
        p1.contains("holding") && !p1.contains("WOULD alert"),
        "pass1 must HOLD (2-pass confirm), not alert: {p1}"
    );
    let p2 = rig.pass(down_body());
    assert!(
        p2.contains("WOULD alert") && p2.contains("alert_now=1"),
        "pass2 must ALERT once confirmed: {p2}"
    );
}

// ---------------------------------------------------------------------------------------------
// RUNNING: never a page; after an alert, a RUNNING pass fires the recovery latch.
// ---------------------------------------------------------------------------------------------
#[test]
fn running_never_pages() {
    let rig = Rig::new();
    let p = rig.pass(running_body());
    assert!(
        p.contains("verdict=RUNNING"),
        "should classify RUNNING: {p}"
    );
    assert!(
        !p.contains("WOULD alert"),
        "a running VB-Matrix must never alert: {p}"
    );
}

#[test]
fn recovery_fires_after_an_alert_then_running() {
    let rig = Rig::new();
    rig.pass(down_body()); // confirm=1
    rig.pass(down_body()); // confirm=2 -> alerted latched
    let rec = rig.pass(running_body());
    assert!(
        rec.contains("WOULD send recovery"),
        "a RUNNING pass after an alert must fire the recovery latch: {rec}"
    );
}

// ---------------------------------------------------------------------------------------------
// UNKNOWN (facet absent = imag / old server): never a page.
// ---------------------------------------------------------------------------------------------
#[test]
fn facet_absent_is_unknown_never_pages() {
    let rig = Rig::new();
    let p = rig.pass(unknown_body());
    assert!(
        p.contains("verdict=UNKNOWN"),
        "should classify UNKNOWN: {p}"
    );
    assert!(
        p.contains("no page") && !p.contains("WOULD alert"),
        "an absent facet must never page: {p}"
    );
}

// ---------------------------------------------------------------------------------------------
// SKIP (fetch failed -> box/:8899 down): #732/#1001 territory, never a page.
// ---------------------------------------------------------------------------------------------
#[test]
fn fetch_failure_is_skip_never_pages() {
    let rig = Rig::new();
    let p = rig.pass("SKIP");
    assert!(p.contains("verdict=SKIP"), "should classify SKIP: {p}");
    assert!(
        !p.contains("WOULD alert"),
        "a box-down fetch failure must never page (deferred to #732/#1001): {p}"
    );
}

// ---------------------------------------------------------------------------------------------
// A single DOWN blip that recovers before the 2nd confirm never pages (confirm resets on RUNNING).
// ---------------------------------------------------------------------------------------------
#[test]
fn single_down_blip_then_running_never_alerts() {
    let rig = Rig::new();
    let p1 = rig.pass(down_body()); // confirm=1
    assert!(
        p1.contains("holding") && !p1.contains("WOULD alert"),
        "{p1}"
    );
    let p2 = rig.pass(running_body()); // resets confirm
    assert!(!p2.contains("WOULD alert"), "{p2}");
    let p3 = rig.pass(down_body()); // confirm back to 1, HOLD again (not 2)
    assert!(
        p3.contains("holding") && !p3.contains("WOULD alert"),
        "a recovered blip must reset the confirm counter, not carry it toward an alert: {p3}"
    );
}
