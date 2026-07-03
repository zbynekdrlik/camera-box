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
//! one place (`scripts/camera-set.sh` → `GENLOCK_FPS`, env-overridable to match the live
//! drop-in) and is interpolated into the remote launch in multitap-e2e.sh.
//!
//! #11 mixed 60/30 update: the default is now **60** — cam1 emits 60fps NDI. Topology v2 (#459,
//! EPIC #466, SUPERSEDES the #11 downstream-decimation framing above): strih is now cut-to-stream
//! only at 30fps and DECIMATES that 60fps camera feed to its own 30fps canvas on ingest (the 60fps
//! LED-wall IMAG role moved to the separate imag-nb box, #458/#463) — the decimation is no longer
//! downstream on the stream box. cam1's rate is UNAFFECTED: the drop-in is still
//! `CAMERA_BOX_GENLOCK_FPS=60`. A default of 30 here would shadow the emit rate back to 30 on a
//! fleet deploy (deploy-fleet.sh sources this), re-introducing the loss.
//!
//! This test reads the scripts statically (it runs neither bash nor camera-box). If anyone
//! drops the genlock env from the manual launch, the bare harness goes lossy/black again and
//! this fails. RED before the fix (the launch had only NDI_RUNTIME_DIR_V6), GREEN after.

use std::fs;

fn camera_set() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/camera-set.sh");
    fs::read_to_string(path).expect("read scripts/camera-set.sh")
}

/// `GENLOCK_FPS` is the single source of truth for the harness genlock rate and lives in the
/// shared `camera-set.sh` (alongside the camera map). #11 mixed 60/30: it defaults to **60** to
/// match the live cam1 `genlock.conf` drop-in (`CAMERA_BOX_GENLOCK_FPS=60` — cam1 emits 60fps).
/// Topology v2 (#459): the 60→30 decimation now happens on strih's OWN ingest (strih is
/// cut-to-stream only at 30fps; the 60fps IMAG role moved to imag-nb, #458/#463), not downstream
/// on the stream box — cam1's emit rate is UNAFFECTED. Asserting it lives there keeps the rate
/// from drifting per-script — and a 30 default would silently shadow cam1's emit back to 30 on a
/// fleet deploy (deploy-fleet.sh sources this), re-introducing the #66 loss.
#[test]
fn camera_set_defines_genlock_fps_default_60() {
    let cs = camera_set();
    assert!(
        cs.contains("GENLOCK_FPS=\"${GENLOCK_FPS:-60}\"")
            || cs.contains("GENLOCK_FPS=${GENLOCK_FPS:-60}"),
        "#459 (was #11): camera-set.sh must define GENLOCK_FPS defaulting to 60 (matching the \
         deployed cam1 drop-in CAMERA_BOX_GENLOCK_FPS=60 — cam1 emits 60fps, unaffected by the \
         strih topology move; strih now decimates 60->30 on its own ingest) as the single source \
         of truth for the harness genlock emit rate."
    );
}
