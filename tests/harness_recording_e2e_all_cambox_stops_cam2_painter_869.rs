//! #869 — the ALL_CAMBOX sweep must stop the PERMANENT `cam2-painter.service` before launching its
//! own painter, exactly as the non-sweep branch already does.
//!
//! ## The bug
//!
//! `scripts/recording-e2e.sh` sets `_cam2_prep` in two branches. The non-sweep (else) branch stops
//! the unit; the ALL_CAMBOX branch was only `rm -f /tmp/painter.csv /tmp/av-markers.csv;`. `#734`'s
//! `pkill -x frame-probe` + bounded death-wait cannot cover that gap, because the `#863` unit is
//! `Restart=always` / `RestartSec=2`: systemd brings its painter back INSIDE the ~10 s wait window,
//! the loop exits on timeout, and the harness launches its own painter alongside it. Two painters
//! flip one framebuffer under DIFFERENT run-ids — verbatim the `#440` artifact, whose TEST-mode
//! counterpart is locked by
//! `tests/rig_mode.rs::test_mode_stops_cam2_painter_service_before_launching_emitter`. The recording
//! then carries the run's OWN run-id on only a fraction of refreshes, so `all_cambox_continuity`
//! reports held images as copies/gaps across EVERY cambox — a test-rig artifact that reads as
//! fleet-wide frame loss.
//!
//! ## The fix
//!
//! The ALL_CAMBOX `_cam2_prep` also stops the unit (guarded, so a box without it is unaffected), and
//! it does so BEFORE `#734`'s kill+verify — otherwise the kill races a restart the stop has not yet
//! disabled. `#291` (ALL_CAMBOX must NOT stop `camera-box` on cam2, a measured node whose
//! capture+emit stay alive) is unrelated and stays as it is: the painter unit is the process being
//! replaced.

use std::fs;
use std::path::PathBuf;

fn read_harness() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/recording-e2e.sh");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Absolute byte offset of the ALL_CAMBOX arm of the `_cam2_prep` branch, and its end (the `else`).
/// Bounding matters: the else-branch's own `systemctl stop cam2-painter` would satisfy an unbounded
/// whole-file search even if the sweep arm were never fixed.
fn all_cambox_prep_arm_bounds(s: &str) -> (usize, usize) {
    let anchor = s
        .find("_cam2_marker_check=\"\"")
        .expect("#869: expected the _cam2_marker_check initialiser that precedes the branch");
    let then = s[anchor..]
        .find("if [ \"${ALL_CAMBOX:-0}\" = \"1\" ]; then")
        .map(|i| anchor + i)
        .expect("#869: expected the ALL_CAMBOX arm of the _cam2_prep branch");
    let else_at = s[then..]
        .find("\nelse\n")
        .map(|i| then + i)
        .expect("#869: expected the else arm to bound the ALL_CAMBOX arm");
    (then, else_at)
}

#[test]
fn all_cambox_prep_stops_the_permanent_cam2_painter_service_869() {
    let s = read_harness();
    let (start, end) = all_cambox_prep_arm_bounds(&s);
    let arm = &s[start..end];
    assert!(
        arm.contains("systemctl stop cam2-painter"),
        "#869: the ALL_CAMBOX _cam2_prep must stop the permanent cam2-painter.service — #734's \
         pkill cannot win against Restart=always/RestartSec=2. Got arm:\n{arm}"
    );
    assert!(
        arm.contains("2>/dev/null || true"),
        "#869: the stop must be GUARDED so a box without the unit is unaffected (same shape as the \
         non-sweep else-branch). Got arm:\n{arm}"
    );
}

#[test]
fn all_cambox_painter_service_stop_precedes_the_734_kill_verify_869() {
    let s = read_harness();
    let (start, end) = all_cambox_prep_arm_bounds(&s);
    let stop_pos = s[start..end]
        .find("systemctl stop cam2-painter")
        .map(|i| start + i)
        .expect("#869: expected the guarded cam2-painter stop in the ALL_CAMBOX arm");
    let kill_pos = s
        .find("_cam2_kill_existing=")
        .expect("#869: expected #734's _cam2_kill_existing step");
    assert!(
        stop_pos < kill_pos,
        "#869: the painter-service stop must come BEFORE #734's kill+death-wait, or the kill races \
         a systemd restart (stop_pos={stop_pos} kill_pos={kill_pos})"
    );
}
