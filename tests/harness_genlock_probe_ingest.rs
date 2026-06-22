//! Regression guard for #63 + #149 — the Phase-2/3 probe `ndi_source` must be configured
//! EXACTLY like the certified production genlock camera inputs so the full-path zero-loss /
//! latency gate (#7/#133/#108) measures the SAME config that ships in production.
//!
//! ## #63 origin (proven live on strih 10.77.9.202, 2026-06-15)
//!
//! The genlock build runs a wall-clock-slaved render tick (`OBS_GENLOCK_WALL_CLOCK=1`)
//! AND a per-source pure-FIFO consumption path (`obs_source_set_genlock_fifo`, camera-box
//! #42). The PRODUCTION camera inputs (`NDI cam1/3/5`) run with `genlock_fifo` ENABLED (the
//! FIFO bypass: exactly one queued frame per render tick). The probe originally inherited the
//! DistroAV default `genlock_fifo` disabled → took the normal async timestamp-cursor path →
//! rendered BLACK. Fix: pin `genlock_fifo=true` on the probe (still required, still asserted).
//!
//! ## #149 correction (verified live on strih 10.77.9.202, 2026-06-22)
//!
//! `ndi_sync` was pinned to 1 (PROP_SYNC_NDI_TIMESTAMP, receiver-side timestamp) under the
//! pre-#136 belief that the camera-box sender's wall-clock-epoch SOURCE timecode would go
//! out-of-bounds vs the monotonic compositor cursor. That belief is OBSOLETE:
//!
//!   * #136 (timestamp-aligned release, now deployed with TS_ALIGN=1 on both boxes) REQUIRES
//!     the frame to carry the wall-clock SOURCE timecode (`is_wallclock_ts` on
//!     `next_frame->timestamp`, src/ndi.rs). With `ndi_sync=1` the frame carries the
//!     RECEIVER's monotonic ts, so the #136 ts-align path silently NO-OPS — the harness then
//!     "proves" a path it never exercised.
//!   * The live PRODUCTION genlock cam inputs (`NDI cam1/3/5`) ALL run `ndi_sync=2`
//!     (NDI_SOURCE_TIMECODE, source timing) — read live 2026-06-22. The probe on `ndi_sync=1`
//!     was therefore measuring a DIFFERENT timing regime than production.
//!
//! So the certified config is `ndi_sync=2` (source timing) + `genlock_fifo=true`. The probe
//! MUST mirror it. (A live smoke on source timing confirmed the probe renders + decodes at
//! every tap and the #149 self-verify guard passes against prod.)
//!
//! ## Fix layer: CONFIG / HARNESS (no prod OBS rebuild)
//!
//! Pin `genlock_fifo=true` and `ndi_sync=2` whenever obs_phase2.py creates OR re-points the
//! probe input, AND self-verify the probe's locked baseline equals the matching prod genlock
//! input before measuring (the #149 machine-guard). This reads the script statically (it does
//! NOT run python or touch OBS). If anyone drops genlock_fifo, or reverts ndi_sync back to 1
//! (re-introducing the #149 bug), this fails.

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

/// #149: the probe input MUST use `ndi_sync = 2` (NDI_SOURCE_TIMECODE — SOURCE timing),
/// mirroring the certified production genlock cam inputs (`NDI cam1/3/5`, all ndi_sync=2,
/// verified live 2026-06-22) AND driving the #136 timestamp-aligned release the harness
/// claims to prove (ts-align needs the wall-clock SOURCE timecode on the frame; ndi_sync=1
/// would carry the receiver's monotonic ts and silently no-op it). Reverting to 1
/// re-introduces the #149 bug (harness measures the wrong timing regime), so this fails.
#[test]
fn phase2_probe_input_uses_ndi_source_timecode_sync() {
    let py = obs_py();
    assert!(
        py.contains("\"ndi_sync\": 2"),
        "#149 regression: obs_phase2.py must set ndi_sync=2 (NDI_SOURCE_TIMECODE, SOURCE \
         timing) on the probe input, mirroring the certified prod genlock cam inputs (all \
         ndi_sync=2) and exercising the #136 ts-align path. ndi_sync=1 (receiver timestamp) \
         measures a different timing regime than prod and silently no-ops #136 ts-align."
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
    // #149: ndi_sync MUST be 2 (source timing), mirroring the certified prod cam inputs.
    assert!(
        py.contains("\"genlock_fifo\": True") && py.contains("\"ndi_sync\": 2"),
        "#63/#149: _PROBE_NDI_SETTINGS must carry genlock_fifo=True and ndi_sync=2 (source \
         timing, mirroring the certified prod genlock cam inputs)."
    );
    assert!(
        py.matches("**_PROBE_NDI_SETTINGS").count() >= 2,
        "#63: _PROBE_NDI_SETTINGS must be spread into BOTH the CreateInput path and the \
         SetInputSettings re-point (reuse) path — otherwise a reused dormant probe input \
         keeps its old (black) genlock-disabled config on the next run."
    );
}

/// Review #3: setup() has a THIRD SetInputSettings (the full-NDI-name resolve re-point) that
/// sets only ndi_source_name and relies on overlay=True to MERGE-preserve the genlock keys.
/// Every SetInputSettings on the probe input must use overlay=True; a full-replace
/// (overlay=False) on the resolve re-point would silently drop genlock config → black render.
#[test]
fn phase2_probe_setinputsettings_never_full_replaces() {
    let py = obs_py();
    let su = py.find("def setup(").expect("setup() not found");
    let end = py[su..].find("\ndef ").map(|i| su + i).unwrap_or(py.len());
    let body = &py[su..end];
    // Within setup(), there must be NO overlay: False on the probe input — every probe
    // SetInputSettings merges (overlay True), so genlock keys are never clobbered.
    assert!(
        !body.contains("\"overlay\": False") && !body.contains("'overlay': False"),
        "#63: no probe SetInputSettings in setup() may use overlay=False — a full replace \
         drops the genlock_fifo/ndi_sync config the prior calls set, sending the probe black."
    );
}
