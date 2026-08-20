//! #764 — `setup-imag.sh` must PROVISION the on-imag OBS render watchdog so a freshly hardware'd
//! imag notebook comes up with the watchdog artifacts present, instead of them surviving only as a
//! hand-install on the one original box (the exact "provisioning gap hidden by a hand patch" shape
//! issue 840 already documented for imag-obs-start.sh/imag-obs-stop.sh).
//!
//! The watchdog is provisioned INSTALLED-BUT-DISABLED per the issue-791 agreed model (OBS keep-alive
//! is imag-obs.service's job since issue 882; this on-imag watchdog is a dormant, ready-to-enable-
//! after-issue-788 artifact that verify-imag.sh check (p) requires present-but-disabled). So the new
//! step must install the script + unit and leave the unit DISABLED — it must NOT `systemctl enable`
//! the watchdog.
//!
//! Same convention as tests/setup_imag_guards.rs: read the REAL script and assert its REAL contract.

use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

const SETUP: &str = "scripts/setup-imag.sh";

fn body() -> String {
    let p = manifest_dir().join(SETUP);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Slice the step-24 (watchdog provisioning) region: from its `step 24 "` banner to the NEXT step's
/// `step 25 "` banner (step 25, touchpad usability #779, was appended after it — so this slice no
/// longer runs to the "base provisioning DONE" echo). Every watchdog assertion is scoped to this
/// region so it cannot accidentally match another step's (e.g. step 25's) content or another unit's
/// enable/fetch elsewhere.
fn watchdog_region(body: &str) -> String {
    let start = body
        .find("step 24 \"")
        .expect("scripts/setup-imag.sh must have a `step 24 \"...\"` banner for the imag-obs-watchdog provisioning (#764)");
    let rest = &body[start..];
    let end = rest.find("step 25 \"").unwrap_or(rest.len());
    rest[..end].to_string()
}

/// TOTAL_STEPS must be 26 (after #791's step 26) and a `step 24` banner must announce the watchdog
/// provisioning, or the `[N/TOTAL]` progress display would be wrong and a dropped step would go unnoticed.
#[test]
fn setup_imag_provisions_watchdog_step_24_764() {
    let body = body();
    assert!(
        body.contains("TOTAL_STEPS=27"),
        "{SETUP}: TOTAL_STEPS must be 27 (step 24 imag-obs-watchdog #764 + step 25 touchpad usability #779 + step 26 imag-maxperf #791 + step 27 picom vsync compositor issue 1146) — the watchdog step must still be counted"
    );
    assert!(
        body.contains("step 24 \""),
        "{SETUP}: a `step 24` banner must announce the imag-obs-watchdog provisioning (#764)"
    );
}

/// The step must fetch the tracked script + unit from the repo and install them to the canonical
/// system paths verify-imag.sh check (p) reads (/usr/local/sbin/*.py, /etc/systemd/system/*.service),
/// then daemon-reload so the new unit is known to systemd.
#[test]
fn setup_imag_watchdog_step_fetches_script_and_unit_764() {
    let region = watchdog_region(&body());
    assert!(
        region.contains("scripts/imag-obs-watchdog.py")
            && region.contains("/usr/local/sbin/imag-obs-watchdog.py"),
        "{SETUP} step 24 must fetch scripts/imag-obs-watchdog.py and install it to /usr/local/sbin/imag-obs-watchdog.py (#764)"
    );
    assert!(
        region.contains("systemd/imag-obs-watchdog.service")
            && region.contains("/etc/systemd/system/imag-obs-watchdog.service"),
        "{SETUP} step 24 must fetch systemd/imag-obs-watchdog.service and install it to /etc/systemd/system/ (#764)"
    );
    assert!(
        region.contains("systemctl daemon-reload"),
        "{SETUP} step 24 must `systemctl daemon-reload` after writing the new unit (#764)"
    );
}

/// The watchdog must be installed but LEFT DISABLED (issue 791 model) — the step must NOT
/// `systemctl enable` it (that would contradict issue 791 and race imag-obs.service on relaunch),
/// and must explicitly `systemctl disable` it so `systemctl is-enabled` reports exactly 'disabled'
/// (not 'static'/'enabled') as verify-imag.sh check (p) requires.
#[test]
fn setup_imag_watchdog_installed_disabled_never_enabled_764() {
    let region = watchdog_region(&body());
    assert!(
        !region.contains("systemctl enable"),
        "{SETUP} step 24 must NOT `systemctl enable` the watchdog — it is installed-but-disabled per issue 791 (#764)"
    );
    assert!(
        region.contains("systemctl disable imag-obs-watchdog"),
        "{SETUP} step 24 must explicitly `systemctl disable imag-obs-watchdog` so is-enabled reports 'disabled', not 'static' (#764/issue 791)"
    );
}
