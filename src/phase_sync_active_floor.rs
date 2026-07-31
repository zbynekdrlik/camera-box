//! #893 — the "at least one ACTIVE camera sits at the phase-sync floor" gate term.
//!
//! ## Why this exists
//!
//! The strih per-camera genlock-latency pins are supposed to follow the convention
//! `crate::phase_sync` implements: the SLOWEST camera is pinned at [`PHASE_SYNC_FLOOR_MS`] and
//! every other camera is held back by exactly how much earlier it would otherwise present. On
//! 2026-07-30 the owner found the live rig had drifted from this: NOT ONE camera in
//! `CAMERA_ACTIVE_SET` (cam1-4) sat at the floor — the only pin still at 3ms belonged to `cam5`,
//! a RETIRED camera outside the active set. A human eyeballing "yes, something is at 3ms" was
//! fooled by a stale pin on a camera that isn't even installed.
//!
//! Per the owner's binding directive on #893 ("nech to je tiez v gate ... nech sa tu dalsie
//! tyzdne nekrutime vo veciach ktore uz si vedel"), this must never again be something that
//! silently drifts and gets rediscovered weeks later by eye — it is now a machine-checked term:
//!
//! ```text
//! min(pin[c] for c in CAMERA_ACTIVE_SET) == PHASE_SYNC_FLOOR_MS
//! ```
//!
//! ## Why this lives at the crate root, not `src/probe/`
//!
//! Same reasoning as `optical_floor.rs` / `window_gate.rs`: no I/O, no probe deps, so it
//! compiles + unit-tests on DEFAULT features (Tier-0 — see this repo's Local Build Policy). The
//! caller (a new `scripts/recording-e2e.sh` preflight, via the `phase-sync-active-floor-gate`
//! CLI binary) reads the LIVE pins over OBS WebSocket and hands them in; this module owns only
//! the decision, never the I/O.
//!
//! ## Why the module itself does the active-set filtering, not the caller
//!
//! The whole defect this ticket fixes is a call site reasoning over the WRONG population (a
//! literal camera range, or a persisted-but-stale file, instead of the currently active set) —
//! see `.claude/rules/camera-active-set.md`. Doing the filtering INSIDE
//! [`phase_sync_active_floor_verdict`] means a caller can safely pass the FULL pin table
//! (including retired cameras) and still get the correct verdict — the retired camera can never
//! again masquerade as satisfying this term.

use std::collections::BTreeMap;

use crate::phase_sync::PHASE_SYNC_FLOOR_MS;

/// The verdict for the "at least one active camera at the floor" gate term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveFloorVerdict {
    /// At least one camera in the active set sits exactly at [`PHASE_SYNC_FLOOR_MS`].
    Pass {
        /// The (or an) active camera holding the floor.
        floor_camera: String,
    },
    /// Every measured active camera sits ABOVE the floor — the convention is broken.
    Fail {
        /// The lowest pin among active cameras (still above the floor).
        min_active_ms: u32,
        /// Which active camera holds that (too-high) minimum.
        min_active_camera: String,
        /// Every active camera's pin, for a readable failure message.
        active_pins: BTreeMap<String, u32>,
    },
    /// None of the names in `active_set` had a pin in the input table at all — the caller
    /// couldn't read any active camera's pin (e.g. every WS read failed). Fails CLOSED: this is
    /// NOT the same as "term satisfied", and must never be silently treated as a pass.
    NoActiveCamerasMeasured,
}

impl ActiveFloorVerdict {
    /// True iff this verdict represents PASS.
    pub fn is_pass(&self) -> bool {
        matches!(self, ActiveFloorVerdict::Pass { .. })
    }
}

/// Decide the gate term over `pins` (camera name -> currently-configured `genlock_latency_ms_src`,
/// covering ANY camera the caller could read — active or retired) and `active_set` (the camera
/// names currently in `CAMERA_ACTIVE_SET`, e.g. `["cam1", "cam2", "cam3", "cam4"]`).
///
/// Filters `pins` down to ONLY the names present in `active_set` (never a literal range, per
/// `.claude/rules/camera-active-set.md`) before deciding — a retired camera's pin in `pins` is
/// simply ignored, exactly like today's live bug (`cam5` at the floor) can never again satisfy
/// this term.
pub fn phase_sync_active_floor_verdict(
    pins: &BTreeMap<String, u32>,
    active_set: &[String],
) -> ActiveFloorVerdict {
    let mut active_pins: BTreeMap<String, u32> = BTreeMap::new();
    for name in active_set {
        if let Some(&pin_ms) = pins.get(name) {
            active_pins.insert(name.clone(), pin_ms);
        }
    }

    let Some((min_camera, &min_ms)) = active_pins.iter().min_by_key(|(_, &v)| v) else {
        return ActiveFloorVerdict::NoActiveCamerasMeasured;
    };

    // #893 RED-commit stub (deliberately wrong -- proves the tests actually exercise the floor
    // comparison before the real fix lands in the next commit).
    let _ = PHASE_SYNC_FLOOR_MS;
    ActiveFloorVerdict::Fail {
        min_active_ms: min_ms,
        min_active_camera: min_camera.clone(),
        active_pins,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pins(pairs: &[(&str, u32)]) -> BTreeMap<String, u32> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    fn active(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn passes_when_the_slowest_active_camera_sits_at_the_floor() {
        // Mirrors the healthy 2026-07-09 calibration in #893's own evidence table.
        let p = pins(&[("cam5", 3), ("cam1", 3), ("cam4", 8), ("cam6", 13), ("cam3", 20)]);
        let v = phase_sync_active_floor_verdict(&p, &active(&["cam1", "cam3", "cam4"]));
        assert_eq!(
            v,
            ActiveFloorVerdict::Pass {
                floor_camera: "cam1".to_string()
            }
        );
    }

    #[test]
    fn fails_when_the_floor_is_held_by_a_camera_outside_the_active_set() {
        // The EXACT live #893 bug: cam5 (retired) sits at the floor, every ACTIVE camera is above
        // it. A naive "is anything at 3ms" check would wrongly pass this.
        let p = pins(&[
            ("cam1", 21),
            ("cam2", 16),
            ("cam3", 26),
            ("cam4", 55),
            ("cam5", 3),
            ("cam6", 62),
            ("cam7", 41),
        ]);
        let v = phase_sync_active_floor_verdict(&p, &active(&["cam1", "cam2", "cam3", "cam4"]));
        match v {
            ActiveFloorVerdict::Fail {
                min_active_ms,
                min_active_camera,
                active_pins,
            } => {
                assert_eq!(min_active_ms, 16);
                assert_eq!(min_active_camera, "cam2");
                // Retired cameras must never leak into the reported active table.
                assert!(!active_pins.contains_key("cam5"));
                assert!(!active_pins.contains_key("cam6"));
                assert!(!active_pins.contains_key("cam7"));
                assert_eq!(active_pins.len(), 4);
            }
            other => panic!("expected Fail, got {other:?}"),
        }
    }

    #[test]
    fn a_retired_camera_at_the_floor_never_counts_even_if_it_is_the_only_floor_pin() {
        let p = pins(&[("cam5", 3)]);
        let v = phase_sync_active_floor_verdict(&p, &active(&["cam1", "cam2", "cam3", "cam4"]));
        assert_eq!(v, ActiveFloorVerdict::NoActiveCamerasMeasured);
    }

    #[test]
    fn no_active_cameras_measured_at_all_fails_closed_never_a_silent_pass() {
        let p = pins(&[("cam5", 3), ("cam6", 13)]);
        let v = phase_sync_active_floor_verdict(&p, &active(&["cam1", "cam2", "cam3", "cam4"]));
        assert_eq!(v, ActiveFloorVerdict::NoActiveCamerasMeasured);
        assert!(!v.is_pass());
    }

    #[test]
    fn empty_active_set_fails_closed() {
        let p = pins(&[("cam1", 3)]);
        let v = phase_sync_active_floor_verdict(&p, &[]);
        assert_eq!(v, ActiveFloorVerdict::NoActiveCamerasMeasured);
    }

    #[test]
    fn a_single_active_camera_at_the_floor_passes() {
        let p = pins(&[("cam1", 3)]);
        let v = phase_sync_active_floor_verdict(&p, &active(&["cam1"]));
        assert!(v.is_pass());
    }

    #[test]
    fn re_enabling_a_retired_camera_flows_through_automatically() {
        // Mirrors tests/harness_camera_set.rs's own re-activation proof: adding a camera back to
        // the active set (no other code change) must make it eligible for this term immediately.
        let p = pins(&[("cam1", 21), ("cam5", 3)]);
        let without_cam5 = phase_sync_active_floor_verdict(&p, &active(&["cam1"]));
        assert!(matches!(without_cam5, ActiveFloorVerdict::Fail { .. }));
        let with_cam5 = phase_sync_active_floor_verdict(&p, &active(&["cam1", "cam5"]));
        assert_eq!(
            with_cam5,
            ActiveFloorVerdict::Pass {
                floor_camera: "cam5".to_string()
            }
        );
    }
}
