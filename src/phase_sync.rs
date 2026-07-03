//! Pure Tier-0 4-camera MUTUAL phase-sync offset kernel (#286).
//!
//! WHY crate root, not `src/probe/`: same pure-seam pattern as `src/qpsk_marker.rs` /
//! `src/colour_scale.rs` — NO I/O, NO probe deps, so it compiles + unit-tests on DEFAULT
//! features (Tier-0). The probe-gated MEASUREMENT (`src/probe/recording_latency.rs`'s
//! `n_camera_strih_samples` / `n_camera_median_latency_ms`) feeds this kernel a measured
//! cam→strih latency per camera; `scripts/phase_sync_calibrate.py` mirrors this exact math in
//! Python (Rust and Python can't share code across the OBS-WebSocket boundary) and applies the
//! result as each camera's `genlock_latency_ms_src` over OBS WS, reusing the #427 (#188 Task 6)
//! `av_sync_calibrate.py` snapshot/verify/rollback pattern.
//!
//! ## The problem (#286)
//!
//! With 4 cameras live, genlock ts-align (#257) slaves each strih NDI source to the wall clock
//! INDEPENDENTLY — it does NOT equalize different grab cards' capture→emit latency, so a FASTER
//! camera presents its captured instant EARLIER than a SLOWER one and the 4 are not mutually
//! phase-aligned (a cut between two cameras shows two different instants).
//!
//! ## The fix
//!
//! Measure each camera's cam→strih latency (ms), find the SLOWEST, and set each camera's
//! per-source genlock latency so ALL 4 release the same captured instant at the same wall-clock
//! time: the slowest camera gets the FLOOR (the lowest safe per-source latency); every faster
//! camera gets `floor + (slowest_latency − own_latency)` — held back by exactly how much earlier
//! it would otherwise present. This is `qpsk_marker::required_delay_ms`'s sign+clamp shape
//! applied per-camera against a shared reference (the slowest camera) instead of a single
//! video/audio pair.

/// Floor for the per-source genlock latency (ms) — mirrors
/// `probe::genlock::GENLOCK_LATENCY_MS_MIN` (the DistroAV `PROP_GENLOCK_LATENCY_MS_MIN` clamp).
/// Duplicated as a bare Tier-0 constant (not imported) so this module stays probe-dep-free —
/// the same choice `qpsk_marker::required_delay_ms` makes by hardcoding `.clamp(3, 2000)`. Keep
/// both, and the Python mirror's `PHASE_SYNC_FLOOR_MS`, in lock-step.
pub const PHASE_SYNC_FLOOR_MS: u32 = 3;

/// Cap for the per-source genlock latency (ms) — mirrors
/// `probe::genlock::GENLOCK_SOURCE_LATENCY_MS_MAX` (the DistroAV per-source override ceiling,
/// a deliberate video delay, not the smaller sub-frame jitter-reserve cap). See
/// [`PHASE_SYNC_FLOOR_MS`] for why this is a duplicated literal, not an import.
pub const PHASE_SYNC_CAP_MS: u32 = 2000;

/// One camera's computed per-source genlock-latency offset (ms) to phase-align it with the
/// group's slowest camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CameraOffset {
    /// Caller-supplied identifier for the camera (a burn run_id, an index, whatever the caller
    /// uses to key its own source-name lookup) — passed through unchanged so the caller can map
    /// the result back to a strih NDI source without a second lookup.
    pub camera_id: u32,
    /// The per-source genlock latency (ms) to apply on this camera's strih NDI source, clamped
    /// to `[PHASE_SYNC_FLOOR_MS, PHASE_SYNC_CAP_MS]`.
    pub offset_ms: u32,
}

/// Compute each camera's per-source genlock-latency offset so all cameras in `measured` release
/// the SAME captured instant at the SAME wall-clock time.
///
/// `measured` is `(camera_id, cam→strih latency in ms)` — the SLOWEST (max latency) camera is
/// pinned at [`PHASE_SYNC_FLOOR_MS`]; every other camera gets
/// `PHASE_SYNC_FLOOR_MS + (slowest_latency − own_latency)`, rounded to the nearest ms and
/// clamped to `[PHASE_SYNC_FLOOR_MS, PHASE_SYNC_CAP_MS]`. A tie at the max latency maps every
/// tied camera to the floor (they are ALL "the slowest"). Output order matches input order.
/// Non-finite (`NaN`/`inf`) latencies are skipped entirely (never fabricate an offset from a
/// bad measurement) — the skipped camera is simply absent from the returned `Vec`, so the
/// caller can detect it by comparing lengths rather than silently applying a wrong number.
///
/// Empty input → empty output (no cameras, nothing to align). A single camera → that camera
/// alone is "the slowest" → floor (there is nothing else to phase against).
pub fn compute_phase_sync_offsets(measured: &[(u32, f64)]) -> Vec<CameraOffset> {
    let finite: Vec<(u32, f64)> = measured
        .iter()
        .copied()
        .filter(|(_, ms)| ms.is_finite())
        .collect();
    let Some(slowest) =
        finite
            .iter()
            .map(|(_, ms)| *ms)
            .fold(None, |acc: Option<f64>, v| match acc {
                Some(a) if a >= v => Some(a),
                _ => Some(v),
            })
    else {
        return Vec::new();
    };
    finite
        .into_iter()
        .map(|(camera_id, latency_ms)| {
            let raw = (PHASE_SYNC_FLOOR_MS as f64 + (slowest - latency_ms)).round();
            let offset_ms = raw.clamp(PHASE_SYNC_FLOOR_MS as f64, PHASE_SYNC_CAP_MS as f64) as u32;
            CameraOffset {
                camera_id,
                offset_ms,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slowest_camera_maps_to_the_floor() {
        // camera 2 is the slowest (80ms) -> floor. camera 1 (50ms) is 30ms faster -> floor+30.
        let out = compute_phase_sync_offsets(&[(1, 50.0), (2, 80.0)]);
        assert_eq!(out.len(), 2);
        assert_eq!(
            out[0],
            CameraOffset {
                camera_id: 1,
                offset_ms: 33
            }
        );
        assert_eq!(
            out[1],
            CameraOffset {
                camera_id: 2,
                offset_ms: 3
            }
        );
    }

    #[test]
    fn faster_camera_gets_floor_plus_the_deficit() {
        let out = compute_phase_sync_offsets(&[(10, 100.0), (20, 90.0), (30, 80.0)]);
        assert_eq!(out.len(), 3);
        // slowest = 100.0 (camera 10) -> floor.
        assert_eq!(
            out[0],
            CameraOffset {
                camera_id: 10,
                offset_ms: 3
            }
        );
        // camera 20 is 10ms faster -> floor + 10.
        assert_eq!(
            out[1],
            CameraOffset {
                camera_id: 20,
                offset_ms: 13
            }
        );
        // camera 30 is 20ms faster -> floor + 20.
        assert_eq!(
            out[2],
            CameraOffset {
                camera_id: 30,
                offset_ms: 23
            }
        );
    }

    #[test]
    fn tie_at_the_max_maps_every_tied_camera_to_the_floor() {
        let out = compute_phase_sync_offsets(&[(1, 80.0), (2, 80.0), (3, 50.0)]);
        assert_eq!(
            out[0],
            CameraOffset {
                camera_id: 1,
                offset_ms: 3
            }
        );
        assert_eq!(
            out[1],
            CameraOffset {
                camera_id: 2,
                offset_ms: 3
            }
        );
        assert_eq!(
            out[2],
            CameraOffset {
                camera_id: 3,
                offset_ms: 33
            }
        );
    }

    #[test]
    fn clamp_rails_hold() {
        // A camera far faster than the slowest would need an offset above the 2000ms cap.
        let out = compute_phase_sync_offsets(&[(1, 0.0), (2, 5000.0)]);
        assert_eq!(out[0].camera_id, 1);
        assert_eq!(out[0].offset_ms, PHASE_SYNC_CAP_MS, "clamp high");
        assert_eq!(
            out[1],
            CameraOffset {
                camera_id: 2,
                offset_ms: PHASE_SYNC_FLOOR_MS
            }
        );
    }

    #[test]
    fn single_camera_is_its_own_slowest_and_gets_the_floor() {
        let out = compute_phase_sync_offsets(&[(7, 123.4)]);
        assert_eq!(
            out,
            vec![CameraOffset {
                camera_id: 7,
                offset_ms: PHASE_SYNC_FLOOR_MS
            }]
        );
    }

    #[test]
    fn all_equal_latencies_all_map_to_the_floor() {
        let out = compute_phase_sync_offsets(&[(1, 42.0), (2, 42.0), (3, 42.0)]);
        assert!(out.iter().all(|o| o.offset_ms == PHASE_SYNC_FLOOR_MS));
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn empty_input_yields_empty_output() {
        assert!(compute_phase_sync_offsets(&[]).is_empty());
    }

    #[test]
    fn non_finite_measurements_are_skipped_never_fabricated() {
        let out = compute_phase_sync_offsets(&[(1, 50.0), (2, f64::NAN), (3, 80.0)]);
        // camera 2's NaN measurement must never produce an offset — it's simply absent.
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|o| o.camera_id != 2));
        assert_eq!(
            out.iter().find(|o| o.camera_id == 3).unwrap().offset_ms,
            PHASE_SYNC_FLOOR_MS
        );
        assert_eq!(out.iter().find(|o| o.camera_id == 1).unwrap().offset_ms, 33);
    }

    #[test]
    fn output_order_matches_input_order() {
        let out = compute_phase_sync_offsets(&[(9, 10.0), (1, 20.0), (5, 15.0)]);
        assert_eq!(
            out.iter().map(|o| o.camera_id).collect::<Vec<_>>(),
            vec![9, 1, 5]
        );
    }
}
