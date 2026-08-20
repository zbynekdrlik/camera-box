//! issue 1146 — persistence guards for the imag HDMI-tearing fix (picom vsync compositor + HDMI
//! xrandr primary), folded into `scripts/setup-imag.sh` and proven by `scripts/verify-imag.sh`.
//!
//! Root cause: imag drives two 60Hz outputs (eDP panel + HDMI projector) on independent crystals;
//! GL/scanout presentation vsyncs to only the primary CRTC, so a compositor-free direct scanout
//! (the #841 doctrine) does not guarantee the projector is the sync target -> the two clocks beat
//! -> a walking tear line on the projector, intermittently. The live fix (picom v10 glx vsync
//! compositor anchored on the projector by making HDMI the xrandr primary) was deployed by hand;
//! this ticket folds it into provisioning so it survives a reprovision + reboot.
//!
//! These are static-anchor asserts against the generator scripts (the same convention as
//! `setup_imag_guards.rs` / `setup_imag_obs_watchdog_764.rs`). The FACET behaviour of the shared
//! `scripts/lib/imag-display-path.sh` verdict is covered separately by
//! `harness_imag_display_path_780.rs` (picom_process/picom_service/hdmi_primary flip) and by
//! `drift_guard.rs` (the check_imag_report display-path mapping).

use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const SETUP: &str = "scripts/setup-imag.sh";
const VERIFY: &str = "scripts/verify-imag.sh";

// ---- setup-imag.sh: the picom provisioning step (issue 1146) ------------------------------------

#[test]
fn setup_imag_has_a_picom_step_1146() {
    let body = read(SETUP);
    assert!(
        body.contains("step 27 \""),
        "{SETUP}: a `step 27` banner must announce the picom vsync-compositor provisioning (issue 1146)"
    );
    // The step banner names picom so the operator sees WHAT step 27 does.
    let idx = body.find("step 27 \"").expect("step 27 banner must exist");
    let banner = &body[idx..(idx + 120).min(body.len())];
    assert!(
        banner.to_lowercase().contains("picom"),
        "{SETUP}: the step 27 banner must name picom: {banner}"
    );
}

#[test]
fn setup_imag_installs_picom_1146() {
    let body = read(SETUP);
    assert!(
        body.contains("apt-get install -y picom"),
        "{SETUP}: step 27 must apt-get install picom (the vsync compositor is the tear-free present)"
    );
}

#[test]
fn setup_imag_writes_picom_conf_with_glx_vsync_1146() {
    let body = read(SETUP);
    assert!(
        body.contains(r#"$USER_HOME/.config/picom/picom.conf"#),
        "{SETUP}: step 27 must write ~/.config/picom/picom.conf"
    );
    // The compositor's whole point: glx backend + vsync on, and the fullscreen Program projector
    // MUST stay composited (unredir-if-possible=false) or the tearing returns.
    assert!(
        body.contains("backend = \"glx\";") && body.contains("vsync = true;"),
        "{SETUP}: picom.conf must set the glx backend + vsync=true (issue 1146)"
    );
    assert!(
        body.contains("unredir-if-possible = false;"),
        "{SETUP}: picom.conf must keep the fullscreen projector composited (unredir-if-possible=false) or the tearing returns (issue 1146)"
    );
}

#[test]
fn setup_imag_writes_picom_user_service_1146() {
    let body = read(SETUP);
    assert!(
        body.contains(r#"$USER_HOME/.config/systemd/user/picom.service"#),
        "{SETUP}: step 27 must write the picom.service user systemd unit"
    );
    assert!(
        body.contains("ExecStart=/usr/bin/picom --config %h/.config/picom/picom.conf"),
        "{SETUP}: picom.service must ExecStart picom with the provisioned config (issue 1146)"
    );
    assert!(
        body.contains("WantedBy=graphical-session.target"),
        "{SETUP}: picom.service must be WantedBy=graphical-session.target so `enable` creates the wants symlink the drift facet reads (issue 1146)"
    );
}

#[test]
fn setup_imag_disables_picom_and_never_starts_it_live_1146() {
    let body = read(SETUP);
    // issue 1146 REVERT: the unit is provisioned DORMANT — the step must `systemctl --user
    // disable` picom.service (the compositor cost 21.57% render skips on the 25W envelope),
    // and still never `enable --now`/`start` anything live (enable-only convention).
    assert!(
        body.contains("systemctl --user disable picom.service"),
        "{SETUP}: step 27 must `systemctl --user disable picom.service` (dormant provisioning, issue 1146 revert)"
    );
    assert!(
        !body.contains("systemctl --user enable --now picom.service")
            && !body.contains("systemctl --user start picom.service"),
        "{SETUP}: step 27 must NEVER enable --now / start picom.service live — enable-only, it launches on the next graphical session (provisioning-scripts.md)"
    );
}

// ---- verify-imag.sh: the (z) display-path acceptance check (issue 1146) -------------------------

#[test]
fn verify_imag_sources_the_shared_display_path_lib_1146() {
    let body = read(VERIFY);
    assert!(
        body.contains("lib/imag-display-path.sh"),
        "{VERIFY}: must source the SHARED scripts/lib/imag-display-path.sh (never a driftable copy of the display-path verdict) — issue 1146"
    );
}

#[test]
fn verify_imag_has_a_display_path_acceptance_check_1146() {
    let body = read(VERIFY);
    // The (z) check must run the shared verdict and map DRIFT -> fail (a re-provision that lost
    // picom or the HDMI-primary, or picom that failed to come up after a reboot, must FAIL here).
    assert!(
        body.contains("imag_display_path_gather_remote_snippet")
            && body.contains("imag_display_path_verdict"),
        "{VERIFY}: the (z) check must gather + run the shared display-path verdict (issue 1146)"
    );
    assert!(
        body.contains(r#"fail "display-path/${dp_facet} DRIFT"#),
        "{VERIFY}: the (z) check must FAIL on a display-path DRIFT (picom off / panel primary / lost pin) — issue 1146"
    );
}
