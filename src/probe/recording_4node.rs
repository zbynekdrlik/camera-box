//! 4-node recording-based full-path verdict (#105 / #7) — the cam1 grab node.
//!
//! The full-path E2E records the QR-bearing program at FOUR nodes and decides
//! per-hop loss + latency from those recordings ONLY (never an NDI tap):
//!
//! ```text
//!   node 1  cam2     QR GENERATED  → painter `tick,gen_ts_ns` CSV (--paint-log)
//!   node 2  cam1     camera GRAB   → cam1 grab recording (camera-box --record-grab)
//!   node 3  strih    OBS PROGRAM   → strih.mkv
//!   node 4  stream   OBS PROGRAM   → stream.mp4
//! ```
//!
//! `recording_verdict.rs` already covers nodes 1/3/4:
//!   - cam→strih ([`crate::probe::recording_verdict::cam_strih_assessment`], honest)
//!   - strih→stream ([`crate::probe::recording_verdict::strih_stream_verdict`], strict)
//!
//! This module adds the cam1 grab node so the chain is localized end-to-end:
//!   - **cam2→cam1** (optical + grab) — [`cam1_optical_assessment`]: readability +
//!     in-range phantom faults vs the painter ground truth. The free-running 60→30
//!     camera beat overlaps loss without a clean cam-side per-frame reference, so
//!     this is an HONEST assessment (never a zero-loss claim), exactly like
//!     cam→strih — it is the OPTICAL hop and the dual-QR beat is intrinsic to it.
//!   - **cam1→strih** (STRICT) — [`cam1_strih_verdict`]: every tick cam1 GRABBED and
//!     emitted must reach strih's recording. cam1's grab and strih's program both
//!     carry the SAME camera frames (cam1 grab → NDI → strih records), so the camera
//!     beat is common and cancels — a direct offset-immune tick-SET compare is exact.
//!     A cam1 tick absent at strih is a real cam1→strih DROP (FAIL); a strih tick
//!     absent at cam1 is a phantom / reorder (FAIL). This is the strict digital hop
//!     the user asked for: cam1 RECORDS its grab so the optical loss is separated
//!     from the digital cam1→strih loss.
//!
//! The engine is pure (operates on [`FrameTick`] slices) and unit-tested with
//! synthetic tick streams; the ffmpeg decode + sidecar join is the I/O glue in the
//! `recording-verdict` binary.

use crate::probe::recording_verdict::{
    cam_strih_assessment, strih_stream_verdict, CamStrihAssessment, FrameTick, StrihStreamVerdict,
    VerdictConfig,
};

/// cam2→cam1 OPTICAL-hop assessment (honest, NOT a zero-loss claim).
///
/// cam1 films cam2's monitor: this is the OPTICAL hop, where the un-genlocked
/// 60→30 camera beat is intrinsic and overlaps loss without a clean per-frame
/// reference (a tick the camera never sampled and a frame cam1 dropped both present
/// as a missing painted tick). So the cam1 grab ALONE cannot certify cam2→cam1
/// zero-loss — exactly the cam→strih situation. What IS provable: every tick cam1
/// grabbed must be a tick the painter actually displayed; a cam1 tick the painter
/// NEVER displayed (within the painter's covered range) is a real corruption /
/// phantom-id fault. This reuses [`cam_strih_assessment`] verbatim (same math, same
/// limitation contract) — the only difference is the node label in the report.
pub fn cam1_optical_assessment(
    cam1: &[FrameTick],
    painter_ticks: &[u32],
    cfg: &VerdictConfig,
) -> CamStrihAssessment {
    cam_strih_assessment(cam1, painter_ticks, cfg)
}

/// cam1→strih STRICT zero-loss hop verdict.
///
/// cam1's grab recording and strih's OBS-program recording carry the SAME camera
/// frames (cam1 grab → NDI "CAM1 (usb)" → strih genlock-ingests + records its
/// program), so the 60→30 camera beat is COMMON to both and cancels in a direct
/// tick-SET compare — identical to the strih→stream hop. The compare is on the tick
/// SEQUENCES (offset-immune): the two independent recordings never start on the same
/// camera frame, so a positional pair-up would compare different optical instants.
///
/// A cam1-only tick (present at cam1, absent at strih, within the overlap) is a frame
/// strih DROPPED on the cam1→strih hop = real loss. A strih-only tick is a phantom /
/// reorder strih has that cam1 does not. PASS = both empty AND ≥2 compared ticks.
///
/// This is [`strih_stream_verdict`] applied to the (cam1, strih) pair — the proven
/// offset-immune set compare, reused so the strict-hop logic has ONE implementation.
/// The returned [`StrihStreamVerdict`]'s `strih_only_ticks` are the CAM1-only ticks
/// (strih dropped them) and `stream_only_ticks` are the STRIH-only ticks (phantoms);
/// the binary relabels them for the report.
pub fn cam1_strih_verdict(
    cam1: &[FrameTick],
    strih: &[FrameTick],
    cfg: &VerdictConfig,
) -> StrihStreamVerdict {
    // upstream = cam1 (its "strih_only" = cam1-only = strih dropped),
    // downstream = strih (its "stream_only" = strih-only = phantom).
    strih_stream_verdict(cam1, strih, cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ft(frame_index: u64, tick: Option<u32>) -> FrameTick {
        FrameTick { frame_index, tick }
    }

    fn cfg() -> VerdictConfig {
        VerdictConfig {
            min_secs: 0.0,
            ..VerdictConfig::default()
        }
    }

    #[test]
    fn cam1_strih_clean_hop_passes() {
        // cam1 and strih carry the SAME ordered tick set (offset start) — a clean
        // cam1→strih digital hop. The two recordings start on different camera frames
        // (strih starts one tick later), but the overlapping tick set is identical.
        let cam1 = vec![
            ft(0, Some(100)),
            ft(1, Some(102)),
            ft(2, Some(104)),
            ft(3, Some(106)),
            ft(4, Some(108)),
        ];
        let strih = vec![
            ft(0, Some(102)),
            ft(1, Some(104)),
            ft(2, Some(106)),
            ft(3, Some(108)),
        ];
        let v = cam1_strih_verdict(&cam1, &strih, &cfg());
        assert!(
            v.is_pass(),
            "identical overlapping tick set ⇒ clean cam1→strih hop"
        );
        assert!(v.strih_only_ticks.is_empty(), "no cam1-tick was dropped");
        assert!(v.stream_only_ticks.is_empty(), "no phantom at strih");
        assert!(v.compared_ticks >= 2);
    }

    #[test]
    fn cam1_strih_drop_fails_and_names_the_dropped_tick() {
        // strih is MISSING tick 104 that cam1 grabbed and emitted (within the overlap
        // span) — a real cam1→strih DROP. The hop MUST fail and name 104 as cam1-only
        // (the "strih_only_ticks" field, = ticks present at the upstream cam1 only).
        let cam1 = vec![
            ft(0, Some(100)),
            ft(1, Some(102)),
            ft(2, Some(104)), // grabbed + emitted
            ft(3, Some(106)),
            ft(4, Some(108)),
        ];
        let strih = vec![
            ft(0, Some(100)),
            ft(1, Some(102)),
            // 104 dropped on the cam1→strih hop
            ft(2, Some(106)),
            ft(3, Some(108)),
        ];
        let v = cam1_strih_verdict(&cam1, &strih, &cfg());
        assert!(!v.is_pass(), "a dropped cam1 tick must FAIL the strict hop");
        assert_eq!(
            v.strih_only_ticks,
            vec![104],
            "the dropped tick is named as cam1-only (= strih lost it)"
        );
    }

    #[test]
    fn cam1_strih_phantom_at_strih_fails() {
        // strih shows tick 105 that cam1 never grabbed (within overlap) — a phantom /
        // reorder. The strict hop must FAIL and name 105 as strih-only.
        let cam1 = vec![
            ft(0, Some(100)),
            ft(1, Some(102)),
            ft(2, Some(104)),
            ft(3, Some(106)),
        ];
        let strih = vec![
            ft(0, Some(100)),
            ft(1, Some(102)),
            ft(2, Some(105)), // phantom: cam1 never had 105
            ft(3, Some(104)),
            ft(4, Some(106)),
        ];
        let v = cam1_strih_verdict(&cam1, &strih, &cfg());
        assert!(
            !v.is_pass(),
            "a phantom strih tick must FAIL the strict hop"
        );
        assert!(
            v.stream_only_ticks.contains(&105),
            "the phantom tick is named as strih-only"
        );
    }

    #[test]
    fn cam1_optical_in_range_phantom_is_a_fault_but_never_claims_zero() {
        // cam1 grabbed tick 7 the painter NEVER displayed (within the painter's
        // covered [0,10] range) — a provable optical/corruption fault. The assessment
        // flags it AND must never set claims_zero_loss (the optical hop with the beat
        // can't be certified zero from the cam1 grab alone).
        let painter: Vec<u32> = vec![0, 2, 4, 6, 8, 10];
        let cam1 = vec![
            ft(0, Some(0)),
            ft(1, Some(2)),
            ft(2, Some(7)), // never painted, inside [0,10] ⇒ phantom
            ft(3, Some(8)),
        ];
        let a = cam1_optical_assessment(&cam1, &painter, &cfg());
        assert!(!a.claims_zero_loss, "optical hop never claims zero-loss");
        assert_eq!(a.unknown_ticks, vec![7], "the never-painted in-range tick");
    }

    #[test]
    fn cam1_optical_out_of_painter_range_is_uncertain_not_a_fault() {
        // A cam1 tick OUTSIDE the painter's covered range is uncertain (partial
        // painter capture), reported separately, NEVER a fault.
        let painter: Vec<u32> = vec![100, 102, 104];
        let cam1 = vec![ft(0, Some(100)), ft(1, Some(108)), ft(2, Some(104))];
        let a = cam1_optical_assessment(&cam1, &painter, &cfg());
        assert!(
            a.unknown_ticks.is_empty(),
            "108 is out-of-range, not phantom"
        );
        assert_eq!(a.out_of_painter_range_ticks, vec![108]);
    }
}
