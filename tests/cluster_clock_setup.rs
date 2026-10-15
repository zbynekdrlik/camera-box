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
//! These tests pin that the setup script points dantesync at the cluster master (so the fix
//! survives a reprovision), and that the mechanism + measured baseline + the offset bound are
//! documented in the repo (SETUP.md), per the #8 acceptance criteria.
//!
//! `scripts/setup.sh` used to be checked here too; it was retired in #563 and
//! `scripts/setup-device.sh` is now the sole provisioning path.
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

/// The dantesync systemd unit each setup script writes must aim at the venue's strih by NAME
/// (`strih.lan`) -- NOT the binary's default public NTP pool, and NOT a venue-specific IP.
///
/// Issue 1412: the old literal `10.77.9.202` is the SNV strih. In the Poprad venue nothing answers
/// there (strih-pp is 10.77.8.202), so the PP camboxes ran with no NTP at all, free-ran on their
/// RTC (cam7 ~150 s ahead of strih-pp) and stuttered on strih-pp's genlock FIFO. `strih.lan`
/// resolves to the strih of whichever venue the box is in. The old fear that a failed `.lan` resolve
/// "falls back to the public pool" does not hold: dantesync's `--ntp-server` CLI value overrides the
/// config and is resolved on every query, so an unresolvable name is a counted NTP failure, exactly
/// like an unreachable IP, and recovers once DNS answers. The literal is asserted exactly so a future
/// refactor pointing dantesync at any OTHER server cannot satisfy this guard.
#[test]
fn setup_scripts_point_dantesync_at_the_cluster_master() {
    // The venue strih's DNS name: strih-snv in SNV, strih-pp in Poprad.
    const MASTER: &str = "strih.lan";
    let script = "scripts/setup-device.sh";
    let body = read(script);
    assert!(
        body.contains(&format!(
            "ExecStart=/usr/local/bin/dantesync --ntp-server {MASTER}\n"
        )),
        "{script} must launch dantesync against the venue strih by name \
         (--ntp-server {MASTER}) so a camera in ANY venue disciplines to that venue's strih"
    );
    // The bare ExecStart with no server must NOT remain -- a bare line would let the daemon
    // fall back to the public-pool default and silently leave the camera on the wrong clock.
    assert!(
        !body.contains("ExecStart=/usr/local/bin/dantesync\n"),
        "{script} still has a BARE `ExecStart=/usr/local/bin/dantesync` (no --ntp-server)"
    );
    // A venue-specific IP must not come back (issue 1412: the SNV IP left every PP cambox clockless).
    assert!(
        !body.contains("dantesync --ntp-server 10."),
        "{script} points dantesync at a literal IP -- use {MASTER}, which resolves in every venue"
    );
}

/// #8 acceptance: the sync mechanism + reference clock + measured baseline + offset bound must be
/// documented in the repo (SETUP.md), tied to the measured offset.
#[test]
fn setup_md_documents_the_sync_mechanism_baseline_and_bound() {
    let raw = read("SETUP.md");
    let doc = raw.to_lowercase();
    assert!(
        doc.contains("dantesync") && doc.contains("strih"),
        "SETUP.md must document DanteSync as the mechanism with strih as the master"
    );
    assert!(
        doc.contains("ptp") || doc.contains("ntp"),
        "SETUP.md must name the NTP/PTP sync technology"
    );
    // The chosen offset bound must be documented AS the bound, with the exact value the guard
    // enforces (DEFAULT_BOUND_US=2000). Asserting "±2000" / "2000 µs" near "bound" — not a bare
    // "2000" substring that an unrelated number could satisfy.
    assert!(
        doc.contains("bound")
            && (raw.contains("±2000") || raw.contains("2000 µs") || raw.contains("2000 us")),
        "SETUP.md must document the ±2000 µs offset bound (matching DEFAULT_BOUND_US) as the bound"
    );
    // The frame-period rationale that justifies the bound must be present.
    assert!(
        raw.contains("16667") || doc.contains("16.7 ms") || doc.contains("frame period"),
        "SETUP.md must record the 16.7 ms frame-period rationale for the bound"
    );
    assert!(
        doc.contains("clock-offset-guard"),
        "SETUP.md must reference scripts/clock-offset-guard.sh as the regression check"
    );
    // The measured baseline must be real evidence: at least one per-node offset value in µs, not
    // merely the word "offset" appearing in prose. The baseline table reports values like "351 µs".
    let has_measured_us = raw
        .lines()
        .any(|l| l.contains("µs") && l.chars().any(|c| c.is_ascii_digit()));
    assert!(
        has_measured_us,
        "SETUP.md must record the measured per-node offset baseline (a numeric µs value) as the \
         #8 evidence, not just the word 'offset'"
    );
}
