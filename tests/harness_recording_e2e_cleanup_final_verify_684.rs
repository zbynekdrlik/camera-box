//! #684 — recording-e2e.sh cleanup() must end with an INDEPENDENT, unconditional
//! camera-box.service verify pass for every box the run touched (source cam always;
//! cam2/painter always; cam3-4 under ALL_CAMBOX=1 -- #827: cam5/cam6/cam7 retired) -- not just
//! the early-cleanup restore sites #675 already covers.
//!
//! ## The bug (live incident, 2026-07-11)
//!
//! cam1's `camera-box.service` was found INACTIVE after BOTH the PR #680 and PR #683 "Full-path
//! E2E" gate runs, despite the #675 early-cleanup restore already existing (`camera_box_verify_
//! active_cmds` immediately after cam1's `systemctl restart camera-box`, live-diagnosed in
//! `scripts/lib/camera-box-restart-verify.sh`). For the cam1 RUN_ID 1573931971 run (#682), the
//! deploy-launched `camera-box-burn-1573931971.service` itself deactivated cleanly ~11 minutes
//! after launch (`journalctl` on cam1, 2026-07-11: "Started ... 08:46:00" / "Deactivated
//! successfully ... 08:57:10" -- consistent with cleanup() actually having run), yet
//! camera-box.service was never observed restarting until a human found it down 55 minutes
//! later and hand-started it. Something between the early #675 restore and the true end of
//! cleanup() left the source cam down with no trace of a second attempt.
//!
//! ## The fix these tests lock (static read of the shell script -- no rig, no ssh)
//!
//! cleanup() now calls `camera_box_verify_active_cmds` a SECOND, INDEPENDENT time as the very
//! LAST thing it does -- after every other teardown step (the OBS-scene restore + the #246/#257
//! burn-clear loop) -- for the source camera, cam2/painter, and (under ALL_CAMBOX=1) cam3-6.
//! Called STANDALONE (no preceding stop/pkill/rm, since nothing in this pass tears anything
//! down): a healthy box costs one quick idempotent SSH round trip; a box the early restore
//! missed gets a genuine independent restart attempt.

use std::fs;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// The body of cleanup() -- from `cleanup()` to the `\ntrap ` that installs it (same slice the
/// sibling #675 cleanup tests use).
fn cleanup_body(s: &str) -> String {
    let start = s
        .find("cleanup()")
        .expect("recording-e2e.sh must define cleanup()");
    let end = s[start..]
        .find("\ntrap ")
        .map(|i| start + i)
        .expect("recording-e2e.sh must install the cleanup trap after cleanup()");
    s[start..end].to_string()
}

/// cleanup() must call camera_box_verify_active_cmds AGAIN, after the #246/#257 burn-clear loop
/// (the last existing teardown step) -- proving the final verify is genuinely positioned at the
/// true end of cleanup(), not just another copy of the early #675 restore.
#[test]
fn cleanup_has_a_final_independent_verify_after_the_burn_clear_loop() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));

    let burn_clear_start = body
        .find("#246/#257 clear + verify OBS burns OFF")
        .expect("cleanup() must have the #246/#257 burn-clear loop");
    let final_region = &body[burn_clear_start..];

    let verify_count = final_region
        .matches("camera_box_verify_active_cmds")
        .count();
    assert!(
        verify_count >= 1,
        "#684: cleanup() must call camera_box_verify_active_cmds AGAIN, after the burn-clear \
         loop (the final existing teardown step) -- an independent verify covering whatever \
         silently left the source cam down between the early #675 restore and the true end of \
         cleanup(). Region after burn-clear:\n{final_region}"
    );
    assert!(
        final_region.contains("$CAM1_IP"),
        "#684: the final verify must cover the SOURCE camera (CAM1_IP). Region:\n{final_region}"
    );
}

/// The final verify must ALSO cover cam2/painter and, under ALL_CAMBOX=1, every camera in
/// camera_active_secondary_set() (#827: cam5/cam6/cam7 retired, but reversibly -- see
/// scripts/camera-set.sh) -- every box this run's deploy could have stopped camera-box.service on.
#[test]
fn cleanup_final_verify_covers_painter_and_all_cambox_secondaries() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));
    let burn_clear_start = body
        .find("#246/#257 clear + verify OBS burns OFF")
        .expect("cleanup() must have the #246/#257 burn-clear loop");
    let final_region = &body[burn_clear_start..];

    assert!(
        final_region.contains("$PAINTER_IP"),
        "#684: the final verify must cover cam2/painter. Region:\n{final_region}"
    );
    assert!(
        final_region.contains("ALL_CAMBOX")
            && final_region.contains("camera_active_secondary_set")
            && final_region.contains("camera_secondary_ip"),
        "#684/#827: the final verify must cover every ACTIVE secondary camera under ALL_CAMBOX=1, \
         derived from camera_active_secondary_set(). Region:\n{final_region}"
    );
}

/// The final pass must be STANDALONE (no preceding stop/pkill/rm on the same remote command) --
/// it only checks + at-most-once-restarts, it never tears anything down again. Distinguishes it
/// from the early #675 restore (which DOES stop the burn unit + pkill first).
#[test]
fn cleanup_final_verify_is_standalone_not_a_repeat_teardown() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));
    let burn_clear_start = body
        .find("#246/#257 clear + verify OBS burns OFF")
        .expect("cleanup() must have the #246/#257 burn-clear loop");
    let final_region = &body[burn_clear_start..];
    let verify_idx = final_region
        .find("camera_box_verify_active_cmds")
        .expect("#684: final region must contain a verify call");
    // Look at the text on the SAME remote-command line as the final verify call -- it must not
    // ALSO pkill/stop the burn unit again (that already happened in the early #675 restore).
    let line_start = final_region[..verify_idx]
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    let line = &final_region[line_start..verify_idx];
    assert!(
        !line.contains("pkill") && !line.contains("systemctl stop camera-box-burn"),
        "#684: the FINAL verify pass must be standalone (no repeat pkill/stop-burn-unit) -- that \
         teardown already happened in the early #675 restore. Line before final verify: {line:?}"
    );
}
