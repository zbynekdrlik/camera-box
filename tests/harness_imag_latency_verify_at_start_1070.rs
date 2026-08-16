//! #1070 — latency-pin verify-at-start on imag's OWN OBS start/supervision path (1061 residual).
//!
//! Issue 1061 delivered the latency-pin verify-at-start for the two genlock boxes launched by
//! `scripts/launch-obs-genlock.sh` (strih + stream, its STEP 3b). imag OBS is NOT launched by that
//! script — it runs under systemd supervision on Linux (`.claude/rules/imag-obs-supervision.md`),
//! and imag has no airuleset checkout / no Discord credentials, so alerting for imag MUST run from
//! dev1 (the #882 topology). This wires a REPORT-ONLY latency-pin drift check into the existing
//! dev1-side imag supervision watchdog (`scripts/imag-obs-alert-watchdog.sh`): on a HEALTHY
//! (OBS-up) pass it runs `latency_pins_verify.py --box imag --host <ip>` (read-only WS) and, on
//! drift, fires a THROTTLED Discord report — NEVER overwriting (per-source latency is the
//! operator's A/V-align domain; imag is always the 3ms floor).
//!
//! Pure-shell / behavioral tests — no rig, no OBS, no real ssh, no real WS. The reachability probe
//! is a fixed `sshpass` stub; `python3` is a stub that distinguishes the verify call
//! (`latency_pins_verify`) from the notify call (`notify`) so drift can be simulated deterministically.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    manifest_dir().join("scripts/imag-obs-alert-watchdog.sh")
}

fn read_script() -> String {
    fs::read_to_string(script()).expect("read imag-obs-alert-watchdog.sh")
}

// ================================================================================================
// Content: the watchdog runs latency_pins_verify --box imag, REPORT-ONLY, and reports via notify.
// ================================================================================================

#[test]
fn watchdog_runs_latency_pins_verify_for_imag() {
    let body = read_script();
    assert!(
        body.contains("latency_pins_verify.py"),
        "#1070: the imag supervision watchdog must run scripts/latency_pins_verify.py"
    );
    assert!(
        body.contains("--box imag"),
        "#1070: the verify must be invoked for the imag box (--box imag)"
    );
}

#[test]
fn latency_verify_is_report_only_never_overwrites() {
    let body = read_script();
    // REPORT-ONLY: the watchdog must never SET/enforce/overwrite per-source latency. It must not
    // call the self-healing enforcer nor issue a SetInputSettings write — per-source latency is the
    // operator's A/V-align domain (imag is always the 3ms floor, but a re-tune is a PR to the
    // baseline, never a forced write here).
    assert!(
        !body.contains("imag_latency_enforce"),
        "#1070: the verify-at-start must be REPORT-ONLY — never call the imag_latency_enforce \
         self-healer (that would overwrite the operator's pins). Got:\n{body}"
    );
    assert!(
        !body.contains("SetInputSettings"),
        "#1070: REPORT-ONLY — the watchdog must never write pins over WS (SetInputSettings)."
    );
}

#[test]
fn latency_drift_reports_through_the_same_notify_path() {
    let body = read_script();
    assert!(
        body.contains("airuleset.py") && body.contains("notify --body"),
        "#1070: a latency drift must be reported through the SAME airuleset.py notify path #882 \
         already uses"
    );
}

// ================================================================================================
// Behavioral: run main() with a stubbed sshpass (reachability probe) + a stubbed python3 that
// returns drift/on-baseline for the verify call and records the notify call.
// ================================================================================================

/// Build a fake-bin dir: `sshpass` echoes the fixed reachability reply, `python3` distinguishes
/// the latency-verify call (returns `verify_rc`, printing a drift line when rc==1) from the notify
/// call (records to the marker, exit 0).
fn fake_bins(dir: &Path, ssh_reply: &str, verify_rc: i32, notify_marker: &Path) {
    let sshpass = dir.join("sshpass");
    fs::write(&sshpass, format!("#!/bin/sh\necho '{ssh_reply}'\nexit 0\n")).expect("write sshpass");
    set_exec(&sshpass);

    let drift_line = if verify_rc == 1 {
        // Mirror latency_pins_verify.py's own drift output shape (stderr), so a sig built from it
        // is realistic.
        "echo 'LATENCY-PIN DRIFT input=\"NDI cam1\" got=20ms want=3ms (tol +/-0ms)' 1>&2"
    } else {
        "true"
    };
    let python3 = dir.join("python3");
    fs::write(
        &python3,
        format!(
            "#!/bin/sh\ncase \"$*\" in\n  *latency_pins_verify*) {drift_line}; exit {verify_rc} ;;\n  *notify*) echo \"CALLED: $*\" >> {marker}; exit 0 ;;\n  *) exit 0 ;;\nesac\n",
            marker = notify_marker.display()
        ),
    )
    .expect("write python3 stub");
    set_exec(&python3);
}

fn set_exec(p: &Path) {
    let mut perm = fs::metadata(p).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    fs::set_permissions(p, perm).unwrap();
}

struct Harness {
    _tmp: tempfile::TempDir,
    bin_dir: PathBuf,
    state_file: PathBuf,
    marker_file: PathBuf,
}

impl Harness {
    fn new(ssh_reply: &str, verify_rc: i32) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bin_dir = tmp.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let state_file = tmp.path().join("state");
        let marker_file = tmp.path().join("notify-calls.log");
        fake_bins(&bin_dir, ssh_reply, verify_rc, &marker_file);
        Harness {
            _tmp: tmp,
            bin_dir,
            state_file,
            marker_file,
        }
    }

    fn run_main(&self) -> (i32, String) {
        let path = format!(
            "{}:{}",
            self.bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = Command::new("bash")
            .arg("-c")
            .arg(". \"$SCRIPT\"\nmain")
            .env("SCRIPT", script())
            .env("IMAG_OBS_ALERT_STATE_FILE", &self.state_file)
            .env("AIRULESET_NOTIFY", "/dev/null/does-not-matter")
            .env("PATH", path)
            .output()
            .expect("run bash harness");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn notify_calls(&self) -> String {
        fs::read_to_string(&self.marker_file).unwrap_or_default()
    }

    fn notify_call_count(&self) -> usize {
        self.notify_calls().lines().count()
    }
}

#[test]
fn healthy_pass_with_drift_fires_a_report_only_alert() {
    let h = Harness::new("OBS_REACHABLE", 1); // OBS up + verify reports drift
    let (code, err) = h.run_main();
    assert_eq!(code, 0, "stderr={err}");
    assert_eq!(
        h.notify_call_count(),
        1,
        "#1070: a latency-pin drift on a HEALTHY imag pass must fire exactly ONE report. \
         notify calls:\n{}",
        h.notify_calls()
    );
    let calls = h.notify_calls();
    assert!(
        calls.to_lowercase().contains("latency"),
        "#1070: the drift report must be a LATENCY report (distinct from the OBS-down alert). \
         Got:\n{calls}"
    );
}

#[test]
fn healthy_pass_on_baseline_does_not_alert() {
    let h = Harness::new("OBS_REACHABLE", 0); // OBS up + verify on baseline
    let (code, err) = h.run_main();
    assert_eq!(code, 0, "stderr={err}");
    assert_eq!(
        h.notify_call_count(),
        0,
        "#1070: imag on the agreed baseline must NEVER alert. notify calls:\n{}",
        h.notify_calls()
    );
}

#[test]
fn obs_down_pass_reports_the_down_alert_not_a_latency_alert() {
    // When OBS is down the verify cannot run (no WS) — the existing #882 down-alert must fire, and
    // the report must be the OBS-DOWN one, never a latency report.
    let h = Harness::new("OBS_PROCESS_ABSENT", 2); // verify would fail-closed, but must not run
    let (code, err) = h.run_main();
    assert_eq!(code, 0, "stderr={err}");
    assert_eq!(
        h.notify_call_count(),
        1,
        "#882: an OBS-down pass must still fire the down alert. notify calls:\n{}",
        h.notify_calls()
    );
    let calls = h.notify_calls();
    assert!(
        !calls.to_lowercase().contains("latency"),
        "#1070: an OBS-down pass must NOT emit a latency report (the verify needs a live WS). \
         Got:\n{calls}"
    );
}

#[test]
fn repeated_drift_passes_are_throttled() {
    let h = Harness::new("OBS_REACHABLE", 1);
    for _ in 0..4 {
        h.run_main();
    }
    assert_eq!(
        h.notify_call_count(),
        1,
        "#1070: 4 consecutive drift passes with the same drift signature must alert only ONCE \
         (throttled, reusing obs_watchdog_alert_throttle). notify calls:\n{}",
        h.notify_calls()
    );
}
