//! #778 — the captured `systemd/imag-obs-watchdog.service` unit's Description must be ALARM-ONLY,
//! never the misleading "reboot-recover" text.
//!
//! The on-imag watchdog (`scripts/imag-obs-watchdog.py`) is alarm-only and NEVER reboots the box
//! (its own docstring: "THIS WATCHDOG NEVER REBOOTS THE BOX AND NEVER STOPS OUTPUTS"). An audit
//! before an event read the old "detect wedge, snapshot, reboot-recover" Description and worried
//! the notebook would reboot mid-stream. The unit is captured into the repo (single source of
//! truth, fetched by setup-imag.sh — bundled with issue 764's provisioning path) BORN with the
//! correct Description, so the misleading text is never committed anywhere.
//!
//! Same convention as the other unit/script guards: read the REAL file and assert its REAL
//! contract via string checks.

use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

const UNIT: &str = "systemd/imag-obs-watchdog.service";

fn body() -> String {
    let p = manifest_dir().join(UNIT);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The unit file must exist in the repo (captured) AND its Description must state the watchdog is
/// alarm-only / never reboots — never the misleading "reboot-recover" text (#778).
#[test]
fn imag_obs_watchdog_unit_description_is_alarm_only_never_reboots_778() {
    let body = body();
    let desc = body
        .lines()
        .find(|l| l.trim_start().starts_with("Description="))
        .expect("systemd/imag-obs-watchdog.service must have a Description= line");
    let d = desc.to_ascii_lowercase();
    assert!(
        !d.contains("reboot-recover"),
        "{UNIT}: Description must NOT say 'reboot-recover' — the script is alarm-only and never \
         reboots (#778): {desc:?}"
    );
    assert!(
        d.contains("alarm") && d.contains("never reboot"),
        "{UNIT}: Description must state ALARM-ONLY and NEVER reboots (#778): {desc:?}"
    );
}

/// The unit must never introduce ANY reboot behaviour — the HARD standing rule is that the imag
/// watchdog is alarm-only and never reboots the box. No directive may invoke a reboot/shutdown.
#[test]
fn imag_obs_watchdog_unit_has_no_reboot_action_778() {
    let d = body().to_ascii_lowercase();
    assert!(
        !d.contains("systemctl reboot")
            && !d.contains("/sbin/reboot")
            && !d.contains("shutdown -r")
            && !d.contains("systemctl poweroff"),
        "{UNIT}: must never invoke a reboot/shutdown — the watchdog is alarm-only (#778)"
    );
}

/// The captured unit must run the tracked alarm-only script and carry an [Install] section, so that
/// setup-imag.sh can install it and `systemctl is-enabled` reports exactly 'disabled' (not
/// 'static') for the installed-but-disabled model (issue 791) that verify-imag.sh check (p)
/// enforces. Without [Install], is-enabled reports 'static' and the acceptance gate would fail.
#[test]
fn imag_obs_watchdog_unit_runs_tracked_script_and_is_installable_disabled_778() {
    let body = body();
    assert!(
        body.contains("ExecStart=/usr/bin/python3 /usr/local/sbin/imag-obs-watchdog.py"),
        "{UNIT}: ExecStart must run /usr/bin/python3 /usr/local/sbin/imag-obs-watchdog.py"
    );
    assert!(
        body.contains("[Install]") && body.contains("WantedBy="),
        "{UNIT}: must have an [Install] WantedBy= section so `systemctl is-enabled` reports \
         'disabled' (not 'static') for the installed-but-disabled model (issue 791)"
    );
}
