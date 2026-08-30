//! #668 — the recording-e2e.sh burn-enabled camera deploy must SURVIVE a mid-test #656/#663
//! self-heal, not silently lose its digital-burn measurement for the rest of the run.
//!
//! ## The bug
//!
//! `[2/8]` (cam1, the resolved SOURCE camera) and `[2b/8]` (cam2/cam3/cam4/cam5/cam6 under
//! `ALL_CAMBOX=1`) used to deploy the probe-featured burn-enabled `camera-box` binary as a plain
//! `nohup $BIN >log 2>&1 &` process — NOT a systemd unit. `src/capture_rate_selfheal.rs`'s #663
//! self-heal is designed AROUND "the process exits(SELF_HEAL_EXIT_CODE=77), systemd's
//! `Restart=always` picks it back up" (see that module's own header doc) — but a bare nohup'd
//! process has NOTHING watching it, so a real #656 capture-rate defect firing mid-test silently
//! killed that camera's digital-burn measurement for the rest of the recording window with no
//! respawn. Live evidence (#668): RUN_ID 1783724370, cam1's burn ids stopped dead at id 4738 the
//! instant the self-heal's USB reset exited it, ~290s before the recording ended.
//!
//! ## The fix these tests lock (static read of the shell script — no rig, no ssh)
//!
//! Both deploy sites now launch the burn binary under a TRANSIENT `systemd-run` unit with
//! `Restart=on-failure` (validated live on cam1, 2026-07-11: a killed/exited process under this
//! exact `systemd-run` invocation respawns within `RestartSec`, and an explicit `systemctl stop`
//! is never followed by a restart — the correct behavior for test teardown). cleanup() now stops
//! that unit explicitly, BEFORE its existing belt-and-suspenders `pkill -9`, so a torn-down test
//! can never leave a unit racing to respawn.
//!
//! Same PURE-string model as the sibling `harness_recording_e2e_*` files: read the real
//! orchestration script and assert on its source — no ssh to a live rig required for CI.

use std::fs;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// The body of cleanup() — from `cleanup()` to the `\ntrap ` that installs it (mirrors
/// `tests/harness_recording_e2e_cleanup_resilient.rs`'s own slice).
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

/// HEADLINE (#668): the `[2/8]` cam1 deploy must launch the burn binary under a `systemd-run`
/// transient unit with `Restart=on-failure` — a bare `nohup ... &` (with nothing supervising it)
/// is exactly the bug: a mid-test self-heal exit kills it for good with no respawn.
#[test]
fn cam1_burn_deploy_uses_a_systemd_run_unit_with_restart_on_failure_668() {
    let s = read("scripts/recording-e2e.sh");
    let deploy_start = s
        .find("CAM1_BURN_BIN=\"/tmp/camera-box-burn-${RUN_ID}\"")
        .expect("#668: expected the [2/8] cam1 burn-binary deploy block");
    let deploy_end = s[deploy_start..]
        .find("sleep 4")
        .map(|i| deploy_start + i)
        .expect("#668: expected the deploy block to end at its post-launch `sleep 4`");
    let block = &s[deploy_start..deploy_end];

    assert!(
        !block.contains("nohup $CAM1_BURN_BIN"),
        "#668: the [2/8] cam1 deploy must NOT launch the burn binary as a bare, unsupervised \
         `nohup ... &` any more — that is exactly the bug (no respawn after a mid-test self-heal \
         exit). Block:\n{block}"
    );
    assert!(
        block.contains("systemd-run") && block.contains("--unit="),
        "#668: the [2/8] cam1 deploy must launch the burn binary under a named transient \
         systemd-run unit (so cleanup() can target it by name and the self-heal's own \
         Restart=always expectation has something real to hold). Block:\n{block}"
    );
    assert!(
        block.contains("Restart=on-failure"),
        "#668: the transient unit must set Restart=on-failure — the #663 self-heal deliberately \
         exits(77) expecting systemd to bring the process back; without Restart=, a mid-test \
         self-heal permanently kills this camera's digital-burn measurement. Block:\n{block}"
    );
    // The binary's own env (run_id, genlock fps, capture stats path, NDI runtime dir) must still
    // reach the process — via --setenv= now, not a bare `VAR=val` prefix.
    for var in [
        "CAMERA_BOX_GENLOCK_FPS",
        "CAMERA_BOX_BURN_RUN_ID",
        "CAMERA_BOX_CAPTURE_STATS",
        "NDI_RUNTIME_DIR_V6",
    ] {
        assert!(
            block.contains(&format!("--setenv={var}=")),
            "#668: the systemd-run invocation must still pass {var} via --setenv= — dropping an \
             env var here would silently break the burn/capture-stats/NDI runtime. Block:\n{block}"
        );
    }
}

/// HEADLINE (#668): the `[2b/8]` ALL_CAMBOX loop (cam2/cam3/cam4/cam5/cam6) must use the SAME
/// systemd-run + Restart=on-failure wrapping as cam1 — every camera under test is equally exposed
/// to a mid-run #656/#663 self-heal, not just the primary SOURCE camera.
#[test]
fn all_cambox_loop_burn_deploy_uses_a_systemd_run_unit_with_restart_on_failure_668() {
    let s = read("scripts/recording-e2e.sh");
    let loop_start = s
        .find("for _cn_ip_burn in")
        .expect("#312: recording-e2e.sh must define the [2b/8] ALL_CAMBOX deploy loop");
    let loop_end = s[loop_start..]
        .find("\n  done\n")
        .map(|i| loop_start + i)
        .expect("#312: the [2b/8] loop must be closed by its own `done`");
    let loop_body = &s[loop_start..loop_end];

    assert!(
        !loop_body.contains("nohup $_cbin"),
        "#668: the [2b/8] ALL_CAMBOX loop must NOT launch any camera's burn binary as a bare, \
         unsupervised `nohup ... &` any more (the same no-respawn bug as cam1). Loop body:\n{loop_body}"
    );
    assert!(
        loop_body.contains("systemd-run") && loop_body.contains("--unit=") && loop_body.contains("Restart=on-failure"),
        "#668: the [2b/8] loop must launch every camera's burn binary under a transient \
         systemd-run unit with Restart=on-failure (same fix as cam1's [2/8] deploy). Loop body:\n{loop_body}"
    );
    // cam2's CAMERA_BOX_NO_DISPLAY opt-out (#291/#312) must still be wired through, now as a
    // --setenv= rather than a bare env-var prefix.
    assert!(
        loop_body.contains("--setenv=CAMERA_BOX_NO_DISPLAY=1"),
        "#668: cam2's NO_DISPLAY opt-out must still reach the process via --setenv= under the \
         new systemd-run wrapping. Loop body:\n{loop_body}"
    );
}

/// HEADLINE (#668): cleanup() must explicitly STOP each camera's transient burn unit BEFORE the
/// existing `pkill -9` — an explicit `systemctl stop` is never followed by an automatic restart
/// (verified live), so ordering the stop first guarantees a torn-down test never leaves a unit
/// racing to respawn against the cleanup's own pkill/restart sequence.
#[test]
fn cleanup_stops_the_transient_burn_units_before_the_pkill_668() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));

    // cam1's restore block: from the top of cleanup() to the ALL_CAMBOX loop's own start.
    let cam1_end = body
        .find("for _ccn in $(camera_active_secondary_set)")
        .expect("#668: expected the cam3/4/5/6 ALL_CAMBOX restore loop after cam1's block");
    let cam1_block = &body[..cam1_end];
    let cam1_stop = cam1_block
        .find("systemctl stop camera-box-burn-${RUN_ID}")
        .expect("#668: cleanup() must explicitly stop cam1's transient burn unit by name");
    let cam1_pkill = cam1_block
        .find("pkill -9 -f 'camera-box-burn-[a-z0-9]'")
        .expect("#668: cleanup() must still force-kill as a belt-and-suspenders backstop");
    assert!(
        cam1_stop < cam1_pkill,
        "#668: cam1's `systemctl stop` of its transient unit must come BEFORE the `pkill -9` — \
         stopping the unit first guarantees Restart=on-failure can never race a respawn back in. \
         Block:\n{cam1_block}"
    );

    // cam3/4/5/6 restore loop: bounded to its own `done`, mirroring the sibling cleanup test.
    let loop_start = body
        .find("for _ccn in $(camera_active_secondary_set)")
        .expect("#624/#312: cleanup() must have the cam3/4/5/6 ALL_CAMBOX restore loop");
    let painter_region_start = body
        .find("root@\"$PAINTER_IP\" \"pkill -x frame-probe")
        .expect("cleanup() must have the cam2/painter restore block");
    let loop_region = &body[loop_start..painter_region_start];
    let loop_stop = loop_region
        .find("systemctl stop camera-box-burn-")
        .expect("#668: the cam3/4/5/6 restore loop must stop each camera's transient burn unit");
    let loop_pkill = loop_region
        .find("pkill -9 -f 'camera-box-burn-[a-z0-9]'")
        .expect("#668: the cam3/4/5/6 restore loop must still force-kill as a backstop");
    assert!(
        loop_stop < loop_pkill,
        "#668: the cam3/4/5/6 loop's `systemctl stop` must come BEFORE its `pkill -9`. \
         Loop region:\n{loop_region}"
    );

    // cam2/painter restore block.
    let painter_region = &body[painter_region_start..];
    let painter_stop = painter_region
        .find("systemctl stop camera-box-burn-cam2-")
        .expect("#668: cam2's restore block must stop its own transient burn unit by name");
    let painter_pkill = painter_region
        .find("pkill -9 -f 'camera-box-burn-[a-z0-9]'")
        .expect("#668: cam2's restore block must still force-kill as a backstop");
    assert!(
        painter_stop < painter_pkill,
        "#668: cam2's `systemctl stop` of its transient burn unit must come BEFORE its `pkill -9`. \
         Block:\n{painter_region}"
    );
}

/// HEADLINE (issue 1198): both burn-transient systemd-run launch sites must arm the issue-1193
/// over-rate self-heal env — a burn session that opens the ShadowCast grabber in its sustained
/// over-rate mode (~61.3 fps) must self-heal exactly like production, instead of degrading the
/// whole run's cadence windows for lack of the env (three failed runs before this fix, 2026-08-30).
#[test]
fn both_burn_deploy_sites_arm_grabber_overrate_selfheal_1198() {
    let s = read("scripts/recording-e2e.sh");

    // [2/8] cam1 burn-binary deploy block — same slice as the #668 sibling test above.
    let deploy_start = s
        .find("CAM1_BURN_BIN=\"/tmp/camera-box-burn-${RUN_ID}\"")
        .expect("issue 1198: expected the [2/8] cam1 burn-binary deploy block");
    let deploy_end = s[deploy_start..]
        .find("sleep 4")
        .map(|i| deploy_start + i)
        .expect("issue 1198: expected the deploy block to end at its post-launch `sleep 4`");
    let cam1_block = &s[deploy_start..deploy_end];
    assert!(
        cam1_block.contains("--setenv=CAMERA_BOX_GRABBER_OVERRATE_SELFHEAL=1"),
        "issue 1198: the [2/8] cam1 burn deploy's systemd-run invocation must set \
         --setenv=CAMERA_BOX_GRABBER_OVERRATE_SELFHEAL=1 — without it a burn session that opens \
         the grabber in its sustained over-rate mode never self-heals and stays degraded for the \
         rest of the run (unlike production, which arms this via its own canary drop-in). \
         Block:\n{cam1_block}"
    );

    // [2b/8] ALL_CAMBOX loop burn deploy block.
    let loop_start = s
        .find("for _cn_ip_burn in")
        .expect("issue 1198: recording-e2e.sh must define the [2b/8] ALL_CAMBOX deploy loop");
    let loop_end = s[loop_start..]
        .find("\n  done\n")
        .map(|i| loop_start + i)
        .expect("issue 1198: the [2b/8] loop must be closed by its own `done`");
    let loop_body = &s[loop_start..loop_end];
    assert!(
        loop_body.contains("--setenv=CAMERA_BOX_GRABBER_OVERRATE_SELFHEAL=1"),
        "issue 1198: the [2b/8] ALL_CAMBOX loop's systemd-run invocation must ALSO set \
         --setenv=CAMERA_BOX_GRABBER_OVERRATE_SELFHEAL=1 for every camera-under-test box, not \
         just cam1 — every leg is equally exposed to the over-rate mode. Loop body:\n{loop_body}"
    );
}
