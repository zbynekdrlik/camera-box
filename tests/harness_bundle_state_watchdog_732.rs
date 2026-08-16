//! #732 — offline integration guard for `scripts/bundle-state-alert-watchdog.sh`'s DECISION
//! COMPOSITION (`main`/`handle_box`): the 2-pass confirm → auto-restart + throttled alert path, the
//! HEALTHY no-action + recovery-ping path, and the BOX_UNREACHABLE deferral to the #1001
//! network-reach watchdog. The pure decision core is covered exhaustively by
//! `tests/harness_bundle_state_health_732.rs`; this file covers the GLUE that actually drives the
//! restart/alert side effects, so a latch-ordering or wiring regression is caught locally.
//!
//! Fully offline + deterministic: `curl` and `ping` are PATH-shimmed (a temp `bin/` prepended to
//! `PATH`), the box IP is `127.0.0.1` with an OBS-WS port that is closed (refused instantly, so no
//! timeout wait), the run is `--dry-run` (never a real ssh/notify), and the state file is a per-test
//! tempdir path. Same fixture-shim + tempdir-isolation pattern as the repo's existing harnesses
//! (`.claude/rules/ci-testing-gotchas.md` #836 executable fixtures / #975 tempdir isolation).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn watchdog() -> PathBuf {
    let s = manifest_dir().join("scripts/bundle-state-alert-watchdog.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

fn write_exec(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

/// A temp `bin/` with `curl` + `ping` shims and a state file, reused across passes within one test.
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
        // curl shim: BUNDLE_STATE_TEST_CURL=up -> print a JSON body + exit 0; else exit 1 (down).
        write_exec(
            &bin.join("curl"),
            "#!/usr/bin/env bash\nif [ \"${BUNDLE_STATE_TEST_CURL:-down}\" = up ]; then printf '{\"ok\":1}'; exit 0; else exit 1; fi\n",
        );
        // ping shim: succeed only for an IP listed in BUNDLE_STATE_TEST_PING_UP (space-separated).
        // The target IP is ping's LAST argument (`ping -c N -W T <ip>`).
        write_exec(
            &bin.join("ping"),
            "#!/usr/bin/env bash\nip=\"${*: -1}\"\ncase \" ${BUNDLE_STATE_TEST_PING_UP:-} \" in *\" $ip \"*) exit 0;; *) exit 1;; esac\n",
        );
        let state = dir.path().join("wd.state");
        Rig {
            _dir: dir,
            bin,
            state,
        }
    }

    /// Run ONE `--dry-run` pass. `ping_up` = space-list of IPs whose ping answers; `curl_up` picks
    /// the curl shim's HEALTHY(true)/DOWN(false) mode. Returns combined stdout+stderr (the watchdog
    /// logs to stderr).
    fn pass(&self, ping_up: &str, curl_up: bool) -> String {
        let path_env = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = Command::new("bash")
            .arg(watchdog())
            .arg("--dry-run")
            .env("PATH", path_env)
            .env("BUNDLE_STATE_BOXES", "fakebox|127.0.0.1")
            .env("BUNDLE_STATE_REFERENCE_HOSTS", "127.0.0.9")
            .env("BUNDLE_STATE_OBS_WS_PORT", "45999") // closed → refused instantly
            .env("BUNDLE_STATE_TCP_TIMEOUT", "2")
            .env("BUNDLE_STATE_ALERT_STATE_FILE", &self.state)
            .env("BUNDLE_STATE_TEST_PING_UP", ping_up)
            .env(
                "BUNDLE_STATE_TEST_CURL",
                if curl_up { "up" } else { "down" },
            )
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
// DOWN (box up, :8899 down): pass 1 holds (2-pass confirm), pass 2 acts (restart + alert).
// ---------------------------------------------------------------------------------------------
#[test]
fn down_path_holds_one_pass_then_restarts_and_alerts() {
    let rig = Rig::new();
    // box (127.0.0.1) + reference (127.0.0.9) both answer ping; :8899 curl is DOWN.
    let p1 = rig.pass("127.0.0.1 127.0.0.9", false);
    assert!(p1.contains("-> DOWN"), "pass1 should classify DOWN: {p1}");
    assert!(
        p1.contains("holding"),
        "pass1 must HOLD (2-pass confirm), not act: {p1}"
    );
    assert!(
        !p1.contains("WOULD auto-restart"),
        "pass1 must NOT restart before confirmation: {p1}"
    );

    let p2 = rig.pass("127.0.0.1 127.0.0.9", false);
    assert!(p2.contains("-> DOWN"), "pass2 should classify DOWN: {p2}");
    assert!(
        p2.contains("WOULD auto-restart"),
        "pass2 (confirmed) must attempt the auto-restart: {p2}"
    );
    assert!(
        p2.contains("schtasks /run /tn \"BundleStateServer\""),
        "the restart command must be the session-agnostic schtasks /run form: {p2}"
    );
    assert!(
        !p2.contains("/it"),
        "restart must never use the interactive /it form: {p2}"
    );
    assert!(
        p2.contains("WOULD alert") && p2.contains("alert_now=1"),
        "pass2 (confirmed) must fire the (throttled) alert: {p2}"
    );
}

// ---------------------------------------------------------------------------------------------
// HEALTHY (box up, :8899 serving JSON): never acts.
// ---------------------------------------------------------------------------------------------
#[test]
fn healthy_path_takes_no_action() {
    let rig = Rig::new();
    let p = rig.pass("127.0.0.1 127.0.0.9", true);
    assert!(p.contains("-> HEALTHY"), "should classify HEALTHY: {p}");
    assert!(
        !p.contains("WOULD auto-restart") && !p.contains("WOULD alert"),
        "a HEALTHY box must never restart or alert: {p}"
    );
}

// ---------------------------------------------------------------------------------------------
// A HEALTHY pass after a box we PAGED for fires exactly one recovery ping (the alerted latch).
// ---------------------------------------------------------------------------------------------
#[test]
fn recovery_ping_fires_when_a_paged_box_comes_back() {
    let rig = Rig::new();
    // seed the state as if we had already paged fakebox.
    fs::write(&rig.state, "alerted_fakebox=1\n").unwrap();
    let p = rig.pass("127.0.0.1 127.0.0.9", true);
    assert!(p.contains("-> HEALTHY"), "should classify HEALTHY: {p}");
    assert!(
        p.contains("WOULD send recovery"),
        "a HEALTHY pass on a previously-paged box must fire the recovery ping: {p}"
    );
}

// ---------------------------------------------------------------------------------------------
// BOX_UNREACHABLE (box fully down, reference up): defers to the #1001 watchdog, never restarts.
// ---------------------------------------------------------------------------------------------
#[test]
fn fully_unreachable_box_defers_and_never_restarts() {
    let rig = Rig::new();
    // ONLY the reference (127.0.0.9) answers ping; the box (127.0.0.1) is fully dark; :8899 down.
    let p = rig.pass("127.0.0.9", false);
    assert!(
        p.contains("BOX_UNREACHABLE"),
        "a fully-dark box must classify BOX_UNREACHABLE: {p}"
    );
    assert!(
        p.contains("deferring"),
        "it must defer to the network-reach watchdog: {p}"
    );
    assert!(
        !p.contains("WOULD auto-restart"),
        "must never restart against a fully-unreachable box: {p}"
    );
}

// ---------------------------------------------------------------------------------------------
// dev1-side-outage anchor: nothing reachable at all -> nothing to decide (no false action).
// ---------------------------------------------------------------------------------------------
#[test]
fn dev1_side_outage_decides_nothing() {
    let rig = Rig::new();
    // Neither the box nor the reference answers ping; :8899 down -> anchor fails.
    let p = rig.pass("", false);
    assert!(
        p.contains("nothing to decide this pass"),
        "a total dev1-side outage must decide nothing: {p}"
    );
    assert!(
        !p.contains("WOULD auto-restart") && !p.contains("WOULD alert"),
        "no restart/alert when dev1's own path to the rig is down: {p}"
    );
}
