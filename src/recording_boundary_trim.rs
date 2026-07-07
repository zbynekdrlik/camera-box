//! #575 — recording START/STOP boundary artifact trim.
//!
//! Surfaced by the #572 investigation (run 554307): a recording's FIRST few frames (the
//! genlock-fifo pre-roll flush draining its backlog before the feed catches up to the live,
//! contiguous run) and its LAST few frames (mux finalization at StopRecord holding the final
//! frame while OBS drains/closes the muxer) can inject non-real-time optical/burn gaps that are
//! NOT pipeline loss. The zero-loss verdict currently counts these boundary artifacts against
//! zero-loss.
//!
//! This module is the PURE decision that trims them: given every (frame_index, decoded value)
//! sample for a signal (the cam2 optical tick OR a digital burn), drop the samples whose
//! frame_index falls within a SMALL, BOUNDED lead/tail window of the recording's own frame-index
//! range, before the values are handed to a contiguity check
//! ([`crate::imag_tick_gate::tick_contiguity`] / [`crate::imag_tick_gate::burn_step_contiguity`]).
//!
//! **Why trimming by frame POSITION (not by decoded VALUE) is the safe design:** the boundary
//! artifact is a property of WHERE in the recording a sample landed (the literal first/last few
//! frames), not of what value it carries. Trimming a fixed, small, named number of frames at each
//! edge can only ever discard samples physically pinned to the recording's start/stop instant —
//! it can never reach into the middle of a recording and mask a genuine drop there, no matter
//! what values that drop involves. This is what makes the trim BOUNDED and non-masking (see the
//! `genuine_drop_just_past_the_lead_edge_is_never_masked` test below).
//!
//! Sibling of `recording_span_gate.rs` / `imag_tick_gate.rs` (Tier-0, no probe deps) so it
//! unit-tests on default features; the probe-gated `bin/recording-verdict` extracts each
//! `RecordingFrame`'s `frame_index` + decoded value and feeds them here.

/// #575 — how many frames at the very START of a recording to exclude from contiguity analysis
/// (the genlock-fifo pre-roll flush window; confirmed live at ~34ms / frame_index <= 2 on run
/// 554307 — 3 frames covers it with margin).
pub const BOUNDARY_TRIM_LEAD_FRAMES: u64 = 3;

/// #575 — how many frames at the very END of a recording to exclude from contiguity analysis
/// (the mux-finalization tail-drain window at StopRecord; confirmed live at the last ~3 frames
/// on run 554307).
pub const BOUNDARY_TRIM_TAIL_FRAMES: u64 = 3;

/// #575 — trim the recording start/stop boundary from a decoded (frame_index, value) sample set.
///
/// `samples`: every sample decoded for ONE signal (the cam2 optical tick, or one digital burn)
/// anywhere in the recording — any order, duplicates allowed.
///
/// `first_frame_index` / `last_frame_index`: the RECORDING's OWN frame-index bounds (e.g. the
/// first and last decoded [`RecordingFrame`](crate::probe::recording::RecordingFrame) across the
/// WHOLE recording) — deliberately NOT this signal's own first/last decoded value. Anchoring on
/// the recording's bounds (not the signal's) is what makes this a frame-POSITION trim rather than
/// a value-range trim.
///
/// `lead_frames` / `tail_frames`: how many frames to exclude at each edge.
///
/// Returns the surviving values, ready to hand to a contiguity check. An empty result (recording
/// shorter than `lead_frames + tail_frames`) is a safe degenerate case — the downstream
/// contiguity check already treats an empty input as "nothing proven, not a pass" (mirrors
/// [`crate::imag_tick_gate::tick_contiguity`] / `burn_step_contiguity`'s own empty-input rule).
pub fn trim_boundary_samples(
    samples: &[(u64, u32)],
    first_frame_index: u64,
    last_frame_index: u64,
    lead_frames: u64,
    tail_frames: u64,
) -> Vec<u32> {
    // Samples with `frame_index < lead_cutoff` (the lead boundary window) or
    // `frame_index > tail_cutoff` (the tail boundary window) are excluded. Saturating math: a
    // recording shorter than `lead_frames + tail_frames` can make `lead_cutoff > tail_cutoff`,
    // which correctly excludes EVERYTHING (nothing survives a trim window bigger than the whole
    // recording) rather than underflowing/panicking.
    let lead_cutoff = first_frame_index.saturating_add(lead_frames);
    let tail_cutoff = last_frame_index.saturating_sub(tail_frames);
    samples
        .iter()
        .filter(|&&(idx, _)| idx >= lead_cutoff && idx <= tail_cutoff)
        .map(|&(_, v)| v)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lead_boundary_artifact_is_trimmed() {
        // frame_index 0..=2 carry a rogue value that would create a huge phantom gap if kept;
        // the clean contiguous run resumes at frame_index 3. Trimmed (lead=3), only the clean
        // run's values survive.
        let samples: Vec<(u64, u32)> = vec![(0, 1)]
            .into_iter()
            .chain((3..10u64).map(|i| (i, 1000 + i as u32)))
            .collect();
        let kept = trim_boundary_samples(&samples, 0, 9, 3, 3);
        assert!(
            !kept.contains(&1),
            "the rogue lead-boundary value must be trimmed, not fed into the contiguity check: {kept:?}"
        );
        assert_eq!(
            kept,
            (3u64..=6).map(|i| 1000 + i as u32).collect::<Vec<_>>(),
            "only frame_index 3..=6 survive the lead(3)+tail(3) trim over a 0..=9 recording"
        );
    }

    #[test]
    fn a_tail_boundary_artifact_is_trimmed() {
        // frame_index 7..=9 (the last 3 of a 0..=9 recording) carry a rogue value that would
        // create a phantom gap at the end; trimmed (tail=3), only the clean run survives.
        let samples: Vec<(u64, u32)> = (0..7u64)
            .map(|i| (i, 1000 + i as u32))
            .chain(vec![(9, 5000)])
            .collect();
        let kept = trim_boundary_samples(&samples, 0, 9, 3, 3);
        assert!(
            !kept.contains(&5000),
            "the rogue tail-boundary value must be trimmed: {kept:?}"
        );
        assert_eq!(
            kept,
            (3u64..=6).map(|i| 1000 + i as u32).collect::<Vec<_>>(),
            "only frame_index 3..=6 survive the lead(3)+tail(3) trim over a 0..=9 recording"
        );
    }

    #[test]
    fn a_genuine_drop_just_past_the_lead_edge_is_never_masked() {
        // A real dropped instant at frame_index 4 (well inside the KEPT window once the lead-3
        // frames are excluded) must survive the trim untouched — the trim only removes samples
        // PHYSICALLY PINNED to frame_index 0..=2, never anything past the boundary.
        let samples: Vec<(u64, u32)> = (0..10u64)
            .filter(|&i| i != 4) // frame_index 4 never decoded anything -> a real gap
            .map(|i| (i, 1000 + i as u32))
            .collect();
        let kept = trim_boundary_samples(&samples, 0, 9, 3, 3);
        // Values 1003 (i=3) and 1005 (i=5) are both present and 1000+4=1004 is absent — a real
        // gap the downstream contiguity check must still see.
        assert!(
            kept.contains(&1003) && kept.contains(&1005) && !kept.contains(&1004),
            "a genuine mid-recording drop must never be masked by the boundary trim: {kept:?}"
        );
    }

    #[test]
    fn a_genuine_drop_just_before_the_tail_edge_is_never_masked() {
        // A real dropped instant at frame_index 5 (well inside the kept window before the
        // tail-3 frames 7..=9 are excluded) must survive untouched.
        let samples: Vec<(u64, u32)> = (0..10u64)
            .filter(|&i| i != 5)
            .map(|i| (i, 1000 + i as u32))
            .collect();
        let kept = trim_boundary_samples(&samples, 0, 9, 3, 3);
        assert!(
            kept.contains(&1004) && kept.contains(&1006) && !kept.contains(&1005),
            "a genuine mid-recording drop must never be masked by the boundary trim: {kept:?}"
        );
    }

    #[test]
    fn zero_trim_is_a_passthrough() {
        let samples: Vec<(u64, u32)> = (0..5u64).map(|i| (i, i as u32)).collect();
        let kept = trim_boundary_samples(&samples, 0, 4, 0, 0);
        assert_eq!(kept, vec![0, 1, 2, 3, 4], "lead=0,tail=0 must trim nothing");
    }

    #[test]
    fn a_recording_shorter_than_the_trim_window_yields_an_empty_but_safe_result() {
        // A 4-frame recording can't survive a lead(3)+tail(3) trim at all -- everything is
        // excluded. This must never PANIC and must never fabricate a value; the downstream
        // contiguity check already treats empty as "nothing proven, not a pass".
        let samples: Vec<(u64, u32)> = (0..4u64).map(|i| (i, i as u32)).collect();
        let kept = trim_boundary_samples(&samples, 0, 3, 3, 3);
        assert!(
            kept.is_empty(),
            "a recording shorter than the trim window must yield nothing, never panic: {kept:?}"
        );
    }

    #[test]
    fn empty_input_is_handled() {
        assert!(trim_boundary_samples(&[], 0, 0, 3, 3).is_empty());
    }
}
