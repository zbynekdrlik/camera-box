//! Reproducibility + documentation guard for the cluster clock sync (#8).
//!
//! #8 requires the chosen sync mechanism (DanteSync: strih = master) to be applied THROUGH the
//! device setup path "so a re-provisioned camera gets sync automatically — not a manual one-off
//! edit" (Script Failure Policy). The live cameras are disciplined because their dantesync unit
//! carries `--ntp-server strih.lan` (verified read-only 2026-06-15: cam1 +333 us, cam2 +351 us,
//! cam4 -40 us, all PTP NANO-locked). But that arg was applied out-of-band: the setup scripts
//! wrote a BARE `ExecStart=/usr/local/bin/dantesync`, and the dantesync binary's built-in default
//! NTP server is a PUBLIC pool (sk.pool.ntp.org / time.cloudflare.com), NOT the cluster master.
//! A camera re-provisioned from the old setup script would therefore sync to a DIFFERENT clock
//! than the rest of the cluster and silently break genlock — the exact regression #8 guards.
//!
//! These tests pin that BOTH setup scripts point dantesync at the cluster master (so the fix
//! survives a reprovision), and that the mechanism + measured baseline + the offset bound are
//! documented in the repo (SETUP.md), per the #8 acceptance criteria.
//!
//! RED before the fix (bare `dantesync` in both scripts; no SETUP.md sync section); GREEN after.

use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The dantesync systemd unit each setup script writes must aim at the cluster master, not the
/// binary's default public NTP pool. The live units use `--ntp-server strih.lan`.
#[test]
fn setup_scripts_point_dantesync_at_the_cluster_master() {
    for script in ["scripts/setup.sh", "scripts/setup-device.sh"] {
        let body = read(script);
        assert!(
            body.contains("dantesync --ntp-server strih.lan")
                || body.contains("dantesync --ntp-server ${")
                || body.contains("dantesync --ntp-server $"),
            "{script} must launch dantesync against the cluster master (--ntp-server strih.lan), \
             not a bare `dantesync` that defaults to a public NTP pool and desyncs the camera \
             from the genlock master"
        );
        // The bare ExecStart with no server must NOT remain — a bare line would let the daemon
        // fall back to the public-pool default and silently leave the camera on the wrong clock.
        assert!(
            !body.contains("ExecStart=/usr/local/bin/dantesync\n"),
            "{script} still has a BARE `ExecStart=/usr/local/bin/dantesync` (no --ntp-server) — \
             that desyncs a re-provisioned camera from the cluster master. Point it at strih.lan."
        );
    }
}

/// #8 acceptance: the sync mechanism + reference clock + measured baseline + offset bound must be
/// documented in the repo (SETUP.md), tied to the measured offset.
#[test]
fn setup_md_documents_the_sync_mechanism_baseline_and_bound() {
    let doc = read("SETUP.md").to_lowercase();
    assert!(
        doc.contains("dantesync") && doc.contains("strih"),
        "SETUP.md must document DanteSync as the mechanism with strih as the master"
    );
    assert!(
        doc.contains("ptp") || doc.contains("ntp"),
        "SETUP.md must name the NTP/PTP sync technology"
    );
    // The chosen offset bound (2000 us / 2 ms) and the frame-period rationale must be recorded.
    assert!(
        doc.contains("2000") || doc.contains("2 ms") || doc.contains("2ms"),
        "SETUP.md must document the 2000 us (2 ms) offset bound the guard enforces"
    );
    assert!(
        doc.contains("clock-offset-guard"),
        "SETUP.md must reference scripts/clock-offset-guard.sh as the regression check"
    );
    // The measured baseline (the cameras' steady-state offset class) must be recorded as evidence.
    assert!(
        doc.contains("offset"),
        "SETUP.md must record the measured offset baseline as the #8 evidence"
    );
}
