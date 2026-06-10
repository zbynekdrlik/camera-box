//! Regression guard for #30 (intermittent first-run `decode=0`).
//!
//! Root cause was step ORDER in `scripts/multitap-e2e.sh`: the OBS `ndi_source`
//! was wired to `CAM2 (usb)` BEFORE camera-box's NDI sender was restarted, so a
//! cold run could bind OBS to the sender that was then torn down → strih/stream
//! rendered black and every downstream tap decoded 0 for the whole run. The fix
//! brings the camera-box sender up FIRST, then wires OBS to the live sender.
//!
//! This test pins that invariant: if anyone reorders OBS setup back ahead of the
//! camera-box restart, it fails. RED on the pre-#30 order, GREEN on the fix.

use std::fs;

#[test]
fn camera_box_sender_starts_before_obs_setup() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/multitap-e2e.sh");
    let script = fs::read_to_string(path).expect("read scripts/multitap-e2e.sh");

    let sender = script
        .find("systemctl stop camera-box")
        .expect("camera-box (re)start step not found");
    let obs_setup = script
        .find("obs_phase2.py setup")
        .expect("OBS setup step not found");

    assert!(
        sender < obs_setup,
        "#30 regression: the camera-box NDI sender must be (re)started BEFORE the OBS \
         ndi_source is wired to it (camera-box restart @{sender} must precede OBS setup \
         @{obs_setup}); a cold run otherwise binds OBS to a sender that gets torn down → \
         first-run decode=0."
    );
}
