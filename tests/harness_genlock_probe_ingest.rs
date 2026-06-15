//! Regression guard for #63 — the Phase-2/3 probe `ndi_source` rendered BLACK (0 decoded)
//! at strih/stream on the live genlock OBS build, so the full-path zero-loss gate (#7)
//! could never certify the live endpoint.
//!
//! ## Root cause (proven live on strih 10.77.9.202, 2026-06-15)
//!
//! The genlock build runs a wall-clock-slaved render tick (`OBS_GENLOCK_WALL_CLOCK=1`)
//! AND a per-source pure-FIFO consumption path (`obs_source_set_genlock_fifo`, camera-box
//! #42). The PRODUCTION camera inputs (`NDI cam1/3/5`) run with:
//!   - `genlock_fifo = ENABLED`  (the FIFO bypass: exactly one queued frame per render tick)
//!   - `ndi_sync     = 1`        (PROP_SYNC_NDI_TIMESTAMP — the NDI *receiver*-side
//!                                timestamp, monotonic on the receiving box's clock)
//! …and they render perfectly.
//!
//! `obs_phase2.py` created/updated the probe input (`phase2-probe-src`) with ONLY
//! `ndi_source_name` + `ndi_bw_mode`, so it inherited the DistroAV defaults:
//!   - `genlock_fifo = disabled`  (log: "genlock: FIFO frame consumption disabled for
//!                                 source 'phase2-probe-src'")
//!   - `ndi_sync     = 2`         (PROP_SYNC_NDI_SOURCE_TIMECODE — the *sender*-supplied
//!                                timecode; the camera-box sender stamps a wall-clock-epoch
//!                                boundary timecode in 100ns, src/ndi.rs:792, while
//!                                `timestamp` is left 0).
//!
//! With FIFO disabled the probe takes the normal async timestamp-cursor path, which advances
//! `last_frame_ts` by `sys_offset` derived from `obs->video.video_time` (the MONOTONIC
//! timebase). Reconciling a wall-clock-epoch *source timecode* against a monotonic-epoch
//! cursor leaves every frame out of bounds → discarded → BLACK. The cameras escape this two
//! ways at once: FIFO bypasses the cursor entirely, and sync_mode=1 uses the receiver
//! timestamp instead of the sender timecode.
//!
//! ## Fix layer: CONFIG / HARNESS (no prod OBS rebuild)
//!
//! Make the probe input match the proven-working camera config: set `genlock_fifo=true` and
//! `ndi_sync=1` (NDI_TIMESTAMP) whenever obs_phase2.py creates OR re-points the probe input.
//! This is light and non-destructive — it does not touch the vendored genlock code, which is
//! correct: the genlock build does not black-out non-FIFO sources by design; the probe was
//! simply mis-configured relative to every other source on the genlocked compositor.
//!
//! This test reads the script statically (it does NOT run python or touch OBS). If anyone
//! drops the genlock_fifo / NDI-timestamp sync from the probe input setup, the live probe
//! goes black again and this fails.

use std::fs;

fn obs_py() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/obs_phase2.py");
    fs::read_to_string(path).expect("read scripts/obs_phase2.py")
}

/// The probe input MUST be configured with `genlock_fifo=true` so the wall-clock-slaved
/// genlock render tick consumes exactly one of its frames per tick — the same FIFO bypass
/// the production camera inputs (`NDI cam1/3/5`) use. Without it the probe takes the normal
/// async cursor path and renders black on the genlock build (#63).
#[test]
fn phase2_probe_input_enables_genlock_fifo() {
    let py = obs_py();
    assert!(
        py.contains("\"genlock_fifo\": True"),
        "#63 regression: obs_phase2.py must set genlock_fifo=True on the probe input so the \
         genlock build's wall-clock render tick consumes its frames (FIFO bypass, like the \
         live camera inputs). Without it the probe renders BLACK (0 decoded) on strih/stream."
    );
}

/// The probe input MUST use `ndi_sync = 1` (PROP_SYNC_NDI_TIMESTAMP — the NDI receiver-side
/// timestamp), matching the live camera inputs. The DistroAV default is 2
/// (NDI_SOURCE_TIMECODE), which binds the async cursor to the sender's wall-clock-epoch
/// timecode and goes out-of-bounds against the monotonic compositor cursor → black (#63).
#[test]
fn phase2_probe_input_uses_ndi_receiver_timestamp_sync() {
    let py = obs_py();
    assert!(
        py.contains("\"ndi_sync\": 1"),
        "#63 regression: obs_phase2.py must set ndi_sync=1 (PROP_SYNC_NDI_TIMESTAMP, the NDI \
         receiver-side timestamp) on the probe input, matching the live camera inputs. The \
         DistroAV default (2, NDI_SOURCE_TIMECODE) binds the cursor to the sender's \
         wall-clock-epoch timecode and renders BLACK on the genlock build."
    );
}

/// Both the CreateInput path (first run) and the SetInputSettings re-point path (reuse, #22)
/// must apply the genlock probe settings — otherwise a reused dormant input keeps the old
/// black config on the next run. The settings are kept DRY in one `_PROBE_NDI_SETTINGS`
/// dict that is spread (`**_PROBE_NDI_SETTINGS`) into BOTH call sites; assert that.
#[test]
fn phase2_probe_genlock_config_applied_on_create_and_reuse() {
    let py = obs_py();
    assert!(
        py.contains("_PROBE_NDI_SETTINGS = {"),
        "#63: the probe genlock NDI settings (genlock_fifo + ndi_sync) must live in one \
         shared _PROBE_NDI_SETTINGS dict so create and reuse can't drift apart."
    );
    // The genlock-critical keys must be in that shared dict (so both paths get them).
    assert!(
        py.contains("\"genlock_fifo\": True") && py.contains("\"ndi_sync\": 1"),
        "#63: _PROBE_NDI_SETTINGS must carry genlock_fifo=True and ndi_sync=1."
    );
    assert!(
        py.matches("**_PROBE_NDI_SETTINGS").count() >= 2,
        "#63: _PROBE_NDI_SETTINGS must be spread into BOTH the CreateInput path and the \
         SetInputSettings re-point (reuse) path — otherwise a reused dormant probe input \
         keeps its old (black) genlock-disabled config on the next run."
    );
}
