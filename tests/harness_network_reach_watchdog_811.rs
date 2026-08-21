//! #811 — offline integration guard for `scripts/network-reach-alert-watchdog.sh`'s REPORT-ONLY
//! handling: a report-only box (resolume — a traveling CG box normally powered off) is probed,
//! classified and per-box state-tracked exactly like a paging box, but NEVER fires a Discord page
//! (nor a recovery ping). A normal paging box (strih) in the same pass still pages when confirmed
//! unreachable.
//!
//! Root cause (issue 811, owner re-approval 2026-08-21): resolume-snv carries no dantesync and its
//! NDI feed drifts (~+65 ms/h, issue 800); the owner brought it under fleet management, incl. the
//! dev1-side reachability watchdog. But resolume is a TRAVELING box whose absence is the NORMAL
//! state, so paging on its unreachability would be pure false-alarm noise — it must be report-only
//! until a supervisor flips it required (by removing its name from NETWORK_REACH_REPORT_ONLY_BOXES).
//!
//! The pure membership decision (`net_reach_box_is_report_only`) is covered exhaustively by
//! `tests/harness_network_reach_health_1001.rs`; this file covers the watchdog GLUE that actually
//! suppresses the page for a report-only box while still paging a normal box, so a wiring regression
//! is caught locally.
//!
//! Fully offline + deterministic (same fixture-shim + tempdir pattern as
//! `tests/harness_bundle_state_watchdog_732.rs`): `ping` is PATH-shimmed (a temp `bin/` prepended to
//! PATH), the box IPs are loopback with the OBS-WS / bundle ports pointed at CLOSED high ports
//! (refused instantly, so no timeout wait), the run is `--dry-run` (never a real notify), and the
//! state file is a per-test tempdir path.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn watchdog() -> PathBuf {
    let s = manifest_dir().join("scripts/network-reach-alert-watchdog.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

fn write_exec(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

/// A temp `bin/` with a `ping` shim + a shared state file, reused across passes within one test.
struct Rig {
    _dir: tempfile::TempDir,
    bin: PathBuf,
    state: PathBuf,
}

impl Rig {
    fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        // ping shim: succeed only for an IP listed in NET_REACH_TEST_PING_UP (space-separated).
        // The target IP is ping's LAST argument (`ping -c N -W T <ip>`).
        write_exec(
            &bin.join("ping"),
            "#!/usr/bin/env bash\nip=\"${*: -1}\"\ncase \" ${NET_REACH_TEST_PING_UP:-} \" in *\" $ip \"*) exit 0;; *) exit 1;; esac\n",
        );
        let state = dir.path().join("wd.state");
        Rig {
            _dir: dir,
            bin,
            state,
        }
    }

    /// Run ONE `--dry-run` pass. `ping_up` = space-list of IPs whose ping answers. strih (127.0.0.1)
    /// is a normal PAGING box; resolume (127.0.0.2) is REPORT-ONLY. OBS-WS/bundle ports are CLOSED
    /// high ports (refused instantly), so a box is REACHABLE only when its ping answers.
    fn pass(&self, ping_up: &str) -> String {
        let path_env = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = Command::new("bash")
            .arg(watchdog())
            .arg("--dry-run")
            .env("PATH", path_env)
            .env("NETWORK_REACH_BOXES", "strih|127.0.0.1 resolume|127.0.0.2")
            .env("NETWORK_REACH_REPORT_ONLY_BOXES", "resolume")
            .env("NETWORK_REACH_REFERENCE_HOSTS", "127.0.0.9")
            .env("NETWORK_REACH_OBS_WS_PORT", "65401") // closed → refused instantly
            .env("NETWORK_REACH_BUNDLE_PORT", "65402") // closed → refused instantly
            .env("NETWORK_REACH_TCP_TIMEOUT", "2")
            .env("NETWORK_REACH_PING_TIMEOUT", "1")
            .env("NETWORK_REACH_ALERT_STATE_FILE", &self.state)
            .env("NET_REACH_TEST_PING_UP", ping_up)
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
// The CORE contract: a REPORT-ONLY box (resolume) confirmed unreachable is logged but NEVER pages,
// while a normal PAGING box (strih) in the SAME pass still fires its (dry-run) alert. Reference
// 127.0.0.9 answers ping → the dev1-side-outage anchor passes so decisions are actually made.
// ---------------------------------------------------------------------------------------------
#[test]
fn report_only_box_never_pages_while_paging_box_still_does() {
    let rig = Rig::new();
    // pass 1: both strih + resolume unreachable (ping down, ports closed), reference up → both HOLD.
    let p1 = rig.pass("127.0.0.9");
    assert!(
        p1.contains("strih") && p1.contains("holding"),
        "pass1 strih must HOLD (2-pass confirm): {p1}"
    );
    assert!(
        p1.contains("resolume") && p1.contains("holding"),
        "pass1 resolume must HOLD (2-pass confirm): {p1}"
    );
    assert!(
        !p1.contains("WOULD alert"),
        "pass1 must not page anything before confirmation: {p1}"
    );

    // pass 2: both CONFIRMED. strih pages (dry-run); resolume is report-only → logged, NOT paged.
    let p2 = rig.pass("127.0.0.9");
    assert!(
        p2.contains("WOULD alert: strih"),
        "pass2: the PAGING box strih must fire its (dry-run) alert: {p2}"
    );
    assert!(
        p2.contains("[report-only]") && p2.contains("resolume CONFIRMED unreachable"),
        "pass2: the report-only box resolume must be logged CONFIRMED unreachable + report-only: {p2}"
    );
    assert!(
        !p2.contains("WOULD alert: resolume"),
        "pass2: the report-only box resolume must NEVER page: {p2}"
    );
}

// ---------------------------------------------------------------------------------------------
// The gather loop must MARK a report-only box as such (report_only=1) in its per-box probe line, so
// the report-only path is provably taken (and per-box state stays keyed independently).
// ---------------------------------------------------------------------------------------------
#[test]
fn report_only_box_probe_line_is_marked_report_only() {
    let rig = Rig::new();
    let p = rig.pass("127.0.0.9");
    let resolume_line = p
        .lines()
        .find(|l| l.contains("resolume (127.0.0.2)"))
        .unwrap_or_else(|| panic!("no resolume probe line: {p}"));
    assert!(
        resolume_line.contains("report_only=1"),
        "resolume must be marked report_only=1 in its probe line: {resolume_line}"
    );
    // strih is a normal paging box → report_only=0.
    let strih_line = p
        .lines()
        .find(|l| l.contains("strih (127.0.0.1)"))
        .unwrap_or_else(|| panic!("no strih probe line: {p}"));
    assert!(
        strih_line.contains("report_only=0"),
        "strih must be marked report_only=0 (a normal paging box): {strih_line}"
    );
}

// ---------------------------------------------------------------------------------------------
// A REACHABLE report-only box fires no recovery ping EVEN WITH a stale alerted latch seeded — a
// PAGING box in that state would fire exactly one; a report-only box must not (its recovery POST is
// gated off). Seeding the latch makes this actively prove the suppression, not pass vacuously.
// ---------------------------------------------------------------------------------------------
#[test]
fn reachable_report_only_box_never_recovery_pings_even_when_latched() {
    let rig = Rig::new();
    // Seed a stale alerted latch for resolume (as if a prior required-mode run had paged it).
    std::fs::write(&rig.state, "alerted_resolume=1\n").unwrap();
    // resolume (127.0.0.2) answers ping → REACHABLE; reference up too.
    let p = rig.pass("127.0.0.2 127.0.0.9");
    assert!(
        p.contains("resolume (127.0.0.2)") && p.contains("-> REACHABLE"),
        "resolume should classify REACHABLE when its ping answers: {p}"
    );
    assert!(
        !p.contains("WOULD send recovery"),
        "a report-only box must never fire a recovery ping, even with a stale alerted latch: {p}"
    );
    assert!(
        p.contains("[report-only]") && p.contains("resolume reachable"),
        "the report-only reachable branch must log its no-recovery note: {p}"
    );
    assert!(
        !p.contains("WOULD alert"),
        "no alert on a reachable pass: {p}"
    );
}
