//! Regression guard for #81 — a downstream OBS output that emits ~0 frames for a
//! whole 30-min run must FAIL FAST + LOUD with a DISTINCT verdict, not present as a
//! silent under-min-frames mystery 30 minutes later.
//!
//! ## Root cause (proven live on stream.lan 10.77.9.204, 2026-06-16)
//!
//! The stream box's OBS suffered a GPU device-removed crash: at 03:33:39 its log
//! shows `device_texture_create (D3D11): Failed to create 2D texture (887A0005)` +
//! `Device Removed Reason: 887A0007` (DXGI_ERROR_DEVICE_REMOVED / DEVICE_RESET, a
//! TDR on the RTX 4060) fired 6071× every ~5s and OBS NEVER recovered. The
//! compositor could not make textures, so the stream NDI Main Output emitted 0
//! decodable frames — the harness's stream tap saw captured≈0 for the entire 1800s
//! run. The PHASE2-PROBE genlock ingest FIFO was HEALTHY throughout (received kept
//! climbing). So the fault is purely downstream of the genlock ingest: a dead GPU,
//! not a camera-box/genlock/DistroAV bug.
//!
//! ## Hardening (this guard)
//!
//! `check_tap_liveness` looks at each tap's captured-frame count in an EARLY window
//! (the run's first ~30s) and, if any tap captured at-or-below a near-zero floor
//! while its peers captured plenty, returns a DISTINCT `DeadOutput` verdict naming
//! that tap and pointing at a dead downstream OBS / GPU device-removed — so the
//! harness aborts early with a clear cause instead of running the full duration and
//! reporting a generic under-min-frames Fail.

#![cfg(feature = "probe")]

use camera_box::probe::liveness::{check_tap_liveness, LivenessVerdict, TapLiveness};

/// The #81 scenario: cam + strih taps captured plenty in the first 30s, but the
/// stream tap captured essentially nothing (its downstream OBS was wedged by the
/// GPU device-removed crash). The liveness pre-check MUST flag the stream tap as a
/// DEAD OUTPUT — a distinct verdict, not a generic low-frame fail.
#[test]
fn dead_stream_output_is_flagged_distinctly() {
    let taps = vec![
        TapLiveness {
            name: "cam".to_string(),
            captured_in_window: 870,
        },
        TapLiveness {
            name: "strih".to_string(),
            captured_in_window: 868,
        },
        TapLiveness {
            name: "stream".to_string(),
            captured_in_window: 0,
        },
    ];
    // first-window length 30s; a tap with <= dead_floor captured frames in the
    // window is dead.
    let verdict = check_tap_liveness(&taps, 30, /* dead_floor */ 2);
    match verdict {
        LivenessVerdict::DeadOutput { tap, message } => {
            assert_eq!(tap, "stream", "the wedged tap must be named");
            // The message must be the DISTINCT dead-output diagnosis, not a generic
            // "too few frames" — it must point an operator at the dead downstream
            // OBS / GPU device-removed so #81 can never recur as a silent mystery.
            let m = message.to_lowercase();
            assert!(
                m.contains("emitting nothing") || m.contains("dead"),
                "message must name the dead-output condition, got: {message}"
            );
            assert!(
                m.contains("obs") || m.contains("gpu") || m.contains("device-removed"),
                "message must point at the downstream OBS / GPU, got: {message}"
            );
        }
        other => panic!("expected DeadOutput verdict for the stalled stream tap, got {other:?}"),
    }
}

/// A healthy run (every tap capturing well above the dead floor) must NOT be
/// flagged — the pre-check is a fast-fail for dead outputs, never a false alarm on
/// a normal run.
#[test]
fn all_taps_alive_is_not_flagged() {
    let taps = vec![
        TapLiveness {
            name: "cam".to_string(),
            captured_in_window: 870,
        },
        TapLiveness {
            name: "strih".to_string(),
            captured_in_window: 868,
        },
        TapLiveness {
            name: "stream".to_string(),
            captured_in_window: 855,
        },
    ];
    assert!(matches!(
        check_tap_liveness(&taps, 30, 2),
        LivenessVerdict::AllAlive
    ));
}

/// Exactly at the dead floor counts as dead (inclusive `<=`): a tap that delivered
/// only a frame or two in 30s is not emitting, and an operator tightening the floor
/// must get the stricter behaviour, not an off-by-one escape.
#[test]
fn at_dead_floor_is_flagged() {
    let taps = vec![
        TapLiveness {
            name: "cam".to_string(),
            captured_in_window: 800,
        },
        TapLiveness {
            name: "stream".to_string(),
            captured_in_window: 2,
        },
    ];
    assert!(matches!(
        check_tap_liveness(&taps, 30, 2),
        LivenessVerdict::DeadOutput { .. }
    ));
}

/// The dead-output check must only fire when at least one PEER tap is alive — if
/// EVERY tap captured ~0 (e.g. the painter never started, or the whole rig is
/// down), that is a different failure (a setup / source problem), not a single dead
/// downstream output, and the generic min-frames Fail should own it. The distinct
/// "downstream OBS dead" verdict would mis-diagnose a whole-rig outage.
#[test]
fn all_taps_dead_is_not_a_single_dead_output() {
    let taps = vec![
        TapLiveness {
            name: "cam".to_string(),
            captured_in_window: 0,
        },
        TapLiveness {
            name: "stream".to_string(),
            captured_in_window: 0,
        },
    ];
    assert!(
        matches!(check_tap_liveness(&taps, 30, 2), LivenessVerdict::AllAlive),
        "a whole-rig outage is not a single dead downstream output; leave it to the \
         generic min-frames Fail rather than mis-blaming one tap's OBS"
    );
}
