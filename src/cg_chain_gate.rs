//! #1301 — CG chain (SongPlayer-originated content) burn-id contiguity + hold gate.
//!
//! The camera chain proves pixel-level frame contiguity for content that ORIGINATES at the cam2
//! optical painter (the dual-QR Vernier) and for the digital node hops (cam1/strih/stream/imag).
//! Content that originates in **SongPlayer** — the song-lyrics / CG source — and flows
//! `SongPlayer → cg OBS (RESOLUME-SNV) → strih → stream` had no burn-id identity in the decode
//! path, so a dropped SongPlayer-originated frame was invisible. This module is the pure decision
//! core that closes that gap, mirroring [`crate::imag_tick_gate`] + reusing
//! [`crate::burn_hold`].
//!
//! ## Roles (the load-bearing modeling decision)
//!
//! - **SongPlayer ([`crate::probe::recording_latency::BURN_RUN_ID_SONGPLAYER`] = 911014)** is a
//!   chain ORIGIN role — exactly like the cam2 painter is the origin of the camera chain. It is
//!   the content SOURCE whose burn id is tracked THROUGH the chain; it is NOT a camera-under-test
//!   hop, so 911014 is never added to `CAMERA_UNDER_TEST_NODES` / `OPTICAL_INJECTION_NODES`.
//!   SongPlayer itself paints this burn (sender half = zbynekdrlik/songplayer#151), NOT the OBS
//!   burn filter.
//! - **cg OBS ([`crate::probe::recording_latency::BURN_RUN_ID_CG`] = 911015)** is a HOP node —
//!   like strih/stream. It composites its OWN corner burn (`Corner::BottomCenterRight`) and
//!   forwards the SongPlayer content downstream.
//!
//! Both ids ARE tick-excluded ([`crate::probe::recording::NODE_BURN_RUN_IDS`]) and excluded from
//! every `all_burns` array, since they can ride into the strih/stream recordings during a CG run
//! and must never hijack the cam2 Vernier tick (the #463/#312 gotcha).
//!
//! ## What a hop proves
//!
//! Per recording (`cg_obs` / `strih` / `stream`): the SongPlayer burn-id sequence is CONTIGUOUS
//! (`first..=last`, presence-only — duplicated from [`crate::probe::burn_contiguity`] because the
//! whole `probe` module is CI-only, so a crate-root decision cannot depend on it) AND does not
//! REPEAT past [`crate::burn_hold::MAX_HOLD_FRAMES`] (a frozen / re-delivered rendered image), and
//! the cg OBS burn is likewise present + contiguous + within the hold bound (the cg OBS box
//! rendered continuously). A dropped SongPlayer frame (kill/restart SongPlayer mid-run) shows as a
//! missing id at the cg OBS hop and — being upstream content — the SAME missing id at strih and
//! stream.
//!
//! ## LIVE status — REPORT-ONLY first cut (issue 1301)
//!
//! [`gates_overall_pass`] returns **`false`**: `cg_chain` is fully computed, serialized into the
//! verdict JSON, and the run's `overall_pass` is PROVABLY unaffected (the camera-chain gate is
//! untouched). The gate is held report-only until a REAL captured cg-OBS frame carrying the
//! SongPlayer burn exists (songplayer#151 has not shipped, so the decode fixture is a GENERATED
//! frame for now) AND a green CG_CHAIN run calibrates the hold / decimation behaviour LIVE — the
//! `verdict-gate-seam-calibration.md` + `pattern-change-needs-decode-fixture.md` "report-only
//! until the real fixture exists" precedent. Flip the one-line seam to `true` to gate LIVE.
//!
//! This is the PURE decision core; it compiles + unit-tests on DEFAULT features. The probe-gated
//! consumer (`src/bin/recording-verdict.rs`) feeds it the recorded-ORDER `(frame_index, id)` pairs
//! from `recording_latency::burn_ids_with_frame_index_in`, already #575-boundary-trimmed via
//! `recording_boundary_trim::trim_boundary_pairs` (so a recording-boundary freeze never false-fires
//! the hold term, exactly like the imag leg and the node-burn hold path).

use crate::burn_hold::{burn_hold_distribution, hold_gate_pass, MAX_HOLD_FRAMES};
use std::collections::BTreeSet;

/// Presence-only `first..=last` contiguity of a burn-id sequence. Mirrors
/// [`crate::probe::burn_contiguity::NodeContiguity`]'s shape + `is_contiguous` rule (so the
/// probe-gated caller can serialize it straight into the existing reporting shape) — duplicated
/// here only because the probe module is CI-only.
#[derive(Debug, Clone, PartialEq)]
pub struct HopContiguity {
    /// Lowest decoded id (`None` ⇒ nothing decoded).
    pub first_id: Option<u32>,
    /// Highest decoded id.
    pub last_id: Option<u32>,
    /// Count of DISTINCT ids present.
    pub present_count: u32,
    /// `last - first + 1` (the integers that SHOULD be present over the span).
    pub expected_count: u32,
    /// The integers in `first..=last` that did NOT decode (the dropped generations).
    pub missing_ids: Vec<u32>,
}

impl HopContiguity {
    /// Contiguous iff at least one id decoded AND no id is missing in the span — mirrors
    /// `NodeContiguity::is_contiguous` / [`crate::imag_tick_gate::TickContiguity::is_contiguous`]'s
    /// "empty is not a pass" rule.
    pub fn is_contiguous(&self) -> bool {
        self.first_id.is_some() && self.missing_ids.is_empty()
    }
}

/// Presence-only contiguity over a burn-id list. Empty input ⇒ `first_id == None` ⇒ NOT
/// contiguous (nothing proven).
pub fn hop_contiguity(_ids: &[u32]) -> HopContiguity {
    // [red] stub — not yet implemented (GREEN fills the real first..=last/missing computation).
    HopContiguity {
        first_id: None,
        last_id: None,
        present_count: 0,
        expected_count: 0,
        missing_ids: Vec::new(),
    }
}

/// True when the measured max-hold is within [`MAX_HOLD_FRAMES`] (mirrors
/// [`crate::burn_hold::hold_gate_pass`] with the node-burn bound; `None` measured ⇒ PASS, nothing
/// to prove).
fn hold_within_bound(measured_max_hold: Option<u32>) -> bool {
    hold_gate_pass(measured_max_hold, Some(MAX_HOLD_FRAMES))
}

/// One recording's CG-chain verdict: the SongPlayer-origin contiguity + hold AND the cg-OBS-hop
/// burn contiguity + hold, on that hop's recording.
#[derive(Debug, Clone, PartialEq)]
pub struct CgHop {
    /// Which recording: `"cg_obs"` / `"strih"` / `"stream"`.
    pub hop: String,
    /// SongPlayer-origin (911014) contiguity on this recording.
    pub songplayer: HopContiguity,
    /// Longest consecutive-recorded-frame run of one SongPlayer id (`None` ⇒ none decoded).
    pub songplayer_max_hold: Option<u32>,
    /// cg-OBS-hop (911015) contiguity on this recording.
    pub cg: HopContiguity,
    /// Longest consecutive-recorded-frame run of one cg id (`None` ⇒ none decoded).
    pub cg_max_hold: Option<u32>,
}

impl CgHop {
    /// The SongPlayer origin stayed contiguous + did not freeze on this recording.
    pub fn songplayer_ok(&self) -> bool {
        self.songplayer.is_contiguous() && hold_within_bound(self.songplayer_max_hold)
    }
    /// The cg OBS box's own burn stayed contiguous + did not freeze on this recording.
    pub fn cg_ok(&self) -> bool {
        self.cg.is_contiguous() && hold_within_bound(self.cg_max_hold)
    }
    /// This hop passes iff BOTH the SongPlayer origin and the cg-OBS hop are clean.
    pub fn pass(&self) -> bool {
        self.songplayer_ok() && self.cg_ok()
    }
}

/// Build one hop's verdict from its recorded-ORDER `(frame_index, id)` pairs for the SongPlayer
/// burn and the cg burn (already #575-boundary-trimmed by the probe glue). The hold term uses
/// [`crate::burn_hold::burn_hold_distribution`] (recording-adjacent run lengths), so a decode gap
/// breaks a run rather than merging two holds.
pub fn cg_hop(hop: &str, songplayer_pairs: &[(u64, u32)], cg_pairs: &[(u64, u32)]) -> CgHop {
    let sp_ids: Vec<u32> = songplayer_pairs.iter().map(|&(_, id)| id).collect();
    let cg_ids: Vec<u32> = cg_pairs.iter().map(|&(_, id)| id).collect();
    CgHop {
        hop: hop.to_string(),
        songplayer: hop_contiguity(&sp_ids),
        songplayer_max_hold: burn_hold_distribution(hop, songplayer_pairs).measured_max_hold(),
        cg: hop_contiguity(&cg_ids),
        cg_max_hold: burn_hold_distribution(hop, cg_pairs).measured_max_hold(),
    }
}

/// The whole CG chain across its (optional) three recordings.
#[derive(Debug, Clone, PartialEq)]
pub struct CgChainVerdict {
    /// cg OBS's OWN recording (the authoritative origin hop).
    pub cg_obs: Option<CgHop>,
    /// strih's recording (the SongPlayer content forwarded downstream).
    pub strih: Option<CgHop>,
    /// stream's recording (chain endpoint).
    pub stream: Option<CgHop>,
}

impl CgChainVerdict {
    /// Any CG recording was supplied (⇒ this is a CG_CHAIN run and the section is reported).
    pub fn any_input(&self) -> bool {
        self.cg_obs.is_some() || self.strih.is_some() || self.stream.is_some()
    }

    fn hops(&self) -> impl Iterator<Item = &CgHop> {
        [
            self.cg_obs.as_ref(),
            self.strih.as_ref(),
            self.stream.as_ref(),
        ]
        .into_iter()
        .flatten()
    }

    /// The whole chain is contiguous iff at least one hop was supplied AND EVERY supplied hop
    /// passes (SongPlayer origin contiguous + cg hop contiguous, both within the hold bound). No
    /// input ⇒ `false` (nothing proven), so this is never a vacuous pass.
    pub fn contiguous(&self) -> bool {
        self.any_input() && self.hops().all(|h| h.pass())
    }
}

/// #1301 report-only / LIVE seam — mirrors [`crate::burn_hold::gates_overall_pass`] /
/// [`crate::presentation_cadence::gates_overall_pass`] (kept a one-line flip). Returns **`false`**:
/// the `cg_chain` verdict is computed + serialized but does NOT fold into the run's `overall_pass`,
/// so the camera-chain gate is provably unaffected. Held false until a REAL captured cg-OBS frame
/// with the SongPlayer burn (songplayer#151) + a green CG_CHAIN run calibrate it LIVE
/// (pattern-change-needs-decode-fixture.md / verdict-gate-seam-calibration.md §5).
pub fn gates_overall_pass() -> bool {
    false
}

/// How the `cg_chain` verdict folds into the run's `overall_pass`. Report-only
/// ([`gates_overall_pass`] `== false`) ⇒ ALWAYS contributes PASS (never changes the camera-chain
/// result). LIVE ⇒ contributes the chain's own `contiguous()`. Mirrors
/// [`crate::imag_leg_gate::content_folds_into_overall_pass`].
pub fn folds_into_overall_pass(cg_chain_contiguous: bool) -> bool {
    cg_chain_contiguous || !gates_overall_pass()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hop_contiguity_clean_run_is_contiguous() {
        let c = hop_contiguity(&[5, 6, 7, 8, 9]);
        assert_eq!(c.first_id, Some(5));
        assert_eq!(c.last_id, Some(9));
        assert_eq!(c.present_count, 5);
        assert_eq!(c.expected_count, 5);
        assert!(c.missing_ids.is_empty());
        assert!(c.is_contiguous());
    }

    #[test]
    fn hop_contiguity_flags_a_dropped_id() {
        // id 7 is dropped (SongPlayer killed/restarted mid-run).
        let c = hop_contiguity(&[5, 6, 8, 9]);
        assert_eq!(c.missing_ids, vec![7]);
        assert!(!c.is_contiguous());
    }

    #[test]
    fn hop_contiguity_empty_is_not_a_pass() {
        let c = hop_contiguity(&[]);
        assert_eq!(c.first_id, None);
        assert!(!c.is_contiguous());
    }

    #[test]
    fn hop_contiguity_ignores_recorded_order_and_duplicates() {
        // presence-only + order-independent (a reorder / oversample is not a drop).
        let c = hop_contiguity(&[8, 6, 5, 7, 6, 9, 5]);
        assert!(c.is_contiguous());
        assert_eq!(c.present_count, 5);
    }

    #[test]
    fn cg_hop_clean_passes() {
        let sp: Vec<(u64, u32)> = (0..10u64).map(|i| (i, 100 + i as u32)).collect();
        let cg: Vec<(u64, u32)> = (0..10u64).map(|i| (i, 200 + i as u32)).collect();
        let h = cg_hop("cg_obs", &sp, &cg);
        assert!(h.songplayer_ok());
        assert!(h.cg_ok());
        assert!(h.pass());
    }

    #[test]
    fn cg_hop_dropped_songplayer_id_fails_on_songplayer_not_cg() {
        // SongPlayer id 104 dropped; cg burn stays clean. The hop must fail on the SP side only.
        let sp: Vec<(u64, u32)> = vec![(0, 100), (1, 101), (2, 102), (3, 103), (4, 105), (5, 106)];
        let cg: Vec<(u64, u32)> = (0..6u64).map(|i| (i, 200 + i as u32)).collect();
        let h = cg_hop("cg_obs", &sp, &cg);
        assert!(!h.songplayer_ok(), "SongPlayer gap (missing 104) must fail");
        assert_eq!(h.songplayer.missing_ids, vec![104]);
        assert!(h.cg_ok(), "cg burn is clean");
        assert!(!h.pass());
    }

    #[test]
    fn cg_hop_frozen_songplayer_past_hold_bound_fails() {
        // One SongPlayer id held on 6 consecutive recorded frames (> MAX_HOLD_FRAMES=4): a freeze.
        // The id SET stays contiguous (no missing id), so only the hold term catches it.
        let mut sp: Vec<(u64, u32)> = vec![(0, 100), (1, 101)];
        for i in 2..8u64 {
            sp.push((i, 102)); // 6 frames all carrying id 102
        }
        sp.push((8, 103));
        let cg: Vec<(u64, u32)> = (0..9u64).map(|i| (i, 200 + i as u32)).collect();
        let h = cg_hop("cg_obs", &sp, &cg);
        assert!(
            h.songplayer.is_contiguous(),
            "SET is contiguous (100..=103)"
        );
        assert_eq!(h.songplayer_max_hold, Some(6));
        assert!(!h.songplayer_ok(), "hold 6 > 4 must fail the hop");
        assert!(!h.pass());
    }

    #[test]
    fn cg_hop_recorded_gap_breaks_a_hold_run_never_merges_it() {
        // Same id on frame_index 2 and 4 but NOT 3 (an undecodable frame in between) ⇒ two runs of
        // 1, not one hold of 2 — burn_hold_distribution's recording-adjacency rule.
        let sp: Vec<(u64, u32)> = vec![(0, 100), (1, 101), (2, 102), (4, 102), (5, 103)];
        let cg: Vec<(u64, u32)> = vec![(0, 200), (1, 201), (2, 202), (4, 202), (5, 203)];
        let h = cg_hop("stream", &sp, &cg);
        assert_eq!(
            h.songplayer_max_hold,
            Some(1),
            "recorded gap breaks the run"
        );
        // 102 appears twice but is a single present id ⇒ SET still contiguous.
        assert!(h.songplayer.is_contiguous());
    }

    fn clean_hop(hop: &str) -> CgHop {
        let sp: Vec<(u64, u32)> = (0..30u64).map(|i| (i, 1000 + i as u32)).collect();
        let cg: Vec<(u64, u32)> = (0..30u64).map(|i| (i, 2000 + i as u32)).collect();
        cg_hop(hop, &sp, &cg)
    }

    #[test]
    fn verdict_all_three_clean_is_contiguous() {
        let v = CgChainVerdict {
            cg_obs: Some(clean_hop("cg_obs")),
            strih: Some(clean_hop("strih")),
            stream: Some(clean_hop("stream")),
        };
        assert!(v.any_input());
        assert!(v.contiguous());
    }

    #[test]
    fn verdict_dropped_songplayer_frame_propagates_to_every_hop() {
        // A SongPlayer frame dropped upstream shows as the SAME missing id at cg_obs AND is
        // forwarded identically to strih + stream (acceptance criterion).
        let bad: Vec<(u64, u32)> = vec![(0, 1000), (1, 1001), (2, 1003), (3, 1004)]; // id 1002 dropped
        let cg: Vec<(u64, u32)> = (0..4u64).map(|i| (i, 2000 + i as u32)).collect();
        let v = CgChainVerdict {
            cg_obs: Some(cg_hop("cg_obs", &bad, &cg)),
            strih: Some(cg_hop("strih", &bad, &cg)),
            stream: Some(cg_hop("stream", &bad, &cg)),
        };
        assert!(!v.contiguous());
        for h in [&v.cg_obs, &v.strih, &v.stream] {
            assert_eq!(h.as_ref().unwrap().songplayer.missing_ids, vec![1002]);
        }
    }

    #[test]
    fn verdict_no_input_is_never_a_vacuous_pass() {
        let v = CgChainVerdict {
            cg_obs: None,
            strih: None,
            stream: None,
        };
        assert!(!v.any_input());
        assert!(
            !v.contiguous(),
            "no input ⇒ nothing proven ⇒ not contiguous"
        );
    }

    #[test]
    fn report_only_seam_never_changes_overall_pass() {
        assert!(!gates_overall_pass(), "issue 1301 ships REPORT-ONLY");
        // While report-only, a FAILING cg chain still contributes PASS (camera chain unaffected).
        assert!(folds_into_overall_pass(false));
        assert!(folds_into_overall_pass(true));
    }
}
