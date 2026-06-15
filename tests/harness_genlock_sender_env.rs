//! Regression guard for #66 — the bare `scripts/multitap-e2e.sh` reported ~49% cam→strih
//! frame loss (often a fully BLACK strih program, 0 decoded), while a MANUAL run against the
//! running camera-box *service* was zero-loss. The two differed only in HOW camera-box was
//! started.
//!
//! ## Root cause (proven live on cam2 10.77.9.62 → strih 10.77.9.202, 2026-06-15)
//!
//! The deployed camera-box runs as a systemd service that gets `CAMERA_BOX_GENLOCK_FPS=30`
//! from the drop-in `/etc/systemd/system/camera-box.service.d/genlock.conf` (#50). With that
//! env set, `src/main.rs` enables genlock decimation + external pacing: the 60 fps capture is
//! decimated to 30 fps emitted on DanteSync wall-clock boundaries, which the genlocked OBS
//! (`genlock_fifo`, one frame per wall-clock render tick) consumes 1:1 → zero loss.
//!
//! The harness's step [2/5] started camera-box MANUALLY as
//! `nohup /usr/local/bin/camera-box` WITHOUT that env, so `genlock_fps` is `None`: NO
//! decimation, NO external pacing, and the sender emits the full ~60 fps capture with
//! per-frame (non-boundary) timecodes. strih's 30 fps genlock FIFO cannot reconcile that
//! free-running 60 fps stream against its wall-clock render tick → ~half the frames are
//! dropped at ingest (and on the genlock build it commonly renders fully black, 0 decoded).
//!
//! The "post-restart cadence settling" hypothesis in the issue is DISPROVEN: with the env
//! present, cam→strih is 0.0% loss in every 5 s bucket from t+0 s onward (no settling
//! transient). The defect was purely that the manual sender wasn't genlock-decimating like
//! the deployed one. Measured curve (genlock sender, 90 s from restart): 0 dropped / 2382
//! single-copy frames; full chain cam→stream ZERO-LOSS, abs p99 ≈ 289 ms.
//!
//! ## Fix layer: HARNESS (no device/firmware change)
//!
//! The harness must start the manual camera-box with the SAME genlock rate the deployed
//! service uses, so the measured sender faithfully reproduces production. The rate lives in
//! one place (`scripts/camera-set.sh` → `GENLOCK_FPS`, default 30, env-overridable to match
//! the live drop-in) and is interpolated into the remote launch in multitap-e2e.sh.
//!
//! This test reads the scripts statically (it runs neither bash nor camera-box). If anyone
//! drops the genlock env from the manual launch, the bare harness goes lossy/black again and
//! this fails. RED before the fix (the launch had only NDI_RUNTIME_DIR_V6), GREEN after.

use std::fs;

fn multitap() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/multitap-e2e.sh");
    fs::read_to_string(path).expect("read scripts/multitap-e2e.sh")
}

fn camera_set() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/camera-set.sh");
    fs::read_to_string(path).expect("read scripts/camera-set.sh")
}

/// The manual camera-box launch in multitap-e2e.sh MUST export `CAMERA_BOX_GENLOCK_FPS` so the
/// sender genlock-decimates exactly like the deployed systemd service (#50 drop-in). Without
/// it the manual sender free-runs at the capture rate (~60 fps) and strih's 30 fps genlock
/// FIFO drops ~half the frames / renders black (#66).
#[test]
fn manual_camera_box_launch_sets_genlock_fps() {
    let s = multitap();
    // Locate the manual launch: `nohup /usr/local/bin/camera-box`. The env must be on (or
    // exported into) that same command — a bare launch with only NDI_RUNTIME_DIR_V6 is the bug.
    let launch = s
        .find("nohup /usr/local/bin/camera-box")
        .expect("manual camera-box launch (nohup /usr/local/bin/camera-box) not found");
    // The genlock env must appear in the remote command BEFORE the manual launch (it is set on
    // the same `(VAR=… nohup … &)` line, so it precedes the binary on that command).
    let env_pos = s
        .find("CAMERA_BOX_GENLOCK_FPS=")
        .expect("#66 regression: multitap-e2e.sh must set CAMERA_BOX_GENLOCK_FPS on the manual \
                 camera-box launch so the sender genlock-decimates like the deployed service; \
                 without it strih's 30fps genlock FIFO drops ~49% of frames / renders black.");
    assert!(
        env_pos < launch,
        "#66 regression: CAMERA_BOX_GENLOCK_FPS (@{env_pos}) must be set on the same command \
         that launches camera-box (@{launch}) — a manual launch without genlock decimation \
         emits the full ~60fps capture and strih's 30fps genlock FIFO drops ~half the frames."
    );
}

/// The genlock rate must be sourced from the single `GENLOCK_FPS` knob (default 30), not a
/// bare literal scattered in the launch — so it stays in lock-step with the deployed drop-in
/// and an operator can override it (e.g. when the live OBS rate changes for the #11 60fps step).
#[test]
fn multitap_uses_genlock_fps_knob_not_a_bare_literal() {
    let s = multitap();
    assert!(
        s.contains("CAMERA_BOX_GENLOCK_FPS=${GENLOCK_FPS")
            || s.contains("CAMERA_BOX_GENLOCK_FPS=\"$GENLOCK_FPS\"")
            || s.contains("CAMERA_BOX_GENLOCK_FPS=$GENLOCK_FPS"),
        "#66: multitap-e2e.sh must drive CAMERA_BOX_GENLOCK_FPS from the GENLOCK_FPS knob \
         (single source of truth, env-overridable) so it tracks the deployed genlock rate."
    );
}

/// `GENLOCK_FPS` is the single source of truth for the harness genlock rate and lives in the
/// shared `camera-set.sh` (alongside the camera map), defaulting to 30 to match the live
/// `genlock.conf` drop-in. Asserting it lives there keeps the rate from drifting per-script.
#[test]
fn camera_set_defines_genlock_fps_default_30() {
    let cs = camera_set();
    assert!(
        cs.contains("GENLOCK_FPS=\"${GENLOCK_FPS:-30}\"")
            || cs.contains("GENLOCK_FPS=${GENLOCK_FPS:-30}"),
        "#66: camera-set.sh must define GENLOCK_FPS defaulting to 30 (matching the deployed \
         camera-box service drop-in CAMERA_BOX_GENLOCK_FPS=30) as the single source of truth \
         for the harness genlock emit rate."
    );
}
