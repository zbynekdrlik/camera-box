//! (#889) dupe-preferring decimation for the genlock capture->emit gate.
//!
//! Root cause (rig-validated, issue 889): a fast/over-rate USB grabber (ShadowCast 2 measured
//! ~64.14 fps captured against a 60 Hz HDMI source) runs its own capture clock faster than the
//! genlock target rate and repeats its internal buffer to keep up — an exact BYTE-IDENTICAL
//! duplicate frame roughly once every ~15 captures, always an ISOLATED pair (never a triple),
//! every other captured frame genuinely unique (camera sensor noise + painter motion). The
//! pre-existing genlock decimation gate (`ndi::genlock_emit_gate`) decides purely from
//! WALL-CLOCK TIME which captured frame to emit at each target-rate boundary — it has no notion
//! of frame CONTENT, so it sometimes keeps the grabber's dupe (because it happened to be the
//! frame that crossed the boundary) and drops the unique tick captured just before it. That is
//! the exact mechanism behind the per-cambox-window `copies`/`gaps` failures this ticket fixes.
//!
//! THIS COMMIT is the pre-fix baseline: [`DecimationGate::poll`] wraps
//! [`crate::ndi::genlock_emit_gate`] verbatim (pacing alone decides emit/decimate — content is
//! accepted but ignored), matching `src/main.rs`'s existing inline logic byte-for-byte. The next
//! commit adds the dupe-preference decision on top, with NO test changes.
//!
//! Linux-gated in lock-step with capture/ndi (calls into [`crate::ndi::genlock_emit_gate`] and
//! is shaped around a raw V4L2 YUYV422 frame); pure logic, unit-tests Tier-0 on the Linux `test`
//! CI job (default features).

/// Owns the per-box decimation boundary state for ONE capture stream (mirrors what
/// `src/main.rs` tracked as a bare `next_boundary_ns` local before this ticket).
/// [`poll`](Self::poll) is the per-captured-frame call.
#[derive(Debug, Default, Clone)]
pub struct DecimationGate {
    next_boundary_ns: u64,
}

impl DecimationGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// The pacing boundary state AFTER the most recent [`poll`](Self::poll) call — main.rs reads
    /// this before/after each poll to feed the pre-existing `#707`
    /// [`crate::ndi::boundary_skip_count`] diagnostic, unchanged by this ticket.
    pub fn next_boundary_ns(&self) -> u64 {
        self.next_boundary_ns
    }

    /// Feed ONE captured frame (`now_ns` wall-clock capture instant, `content_hash` — not yet
    /// used, this commit) through the pacing gate. `interval_ns == 0` disables decimation
    /// entirely (mirrors [`crate::ndi::genlock_emit_gate`]'s own guard) — always emits, no state
    /// kept. Returns whether THIS captured frame should be emitted.
    pub fn poll(&mut self, now_ns: u64, interval_ns: u64, _content_hash: u64) -> bool {
        if interval_ns == 0 {
            return true;
        }
        let (emit, next) =
            crate::ndi::genlock_emit_gate(now_ns, self.next_boundary_ns, interval_ns);
        self.next_boundary_ns = next;
        emit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (#889) validated synthetic capture sequence: the rig's raw V4L2 grab on cam1 showed a
    /// fast grabber (measured 64.14 fps against a 60 Hz source) repeating its OWN internal
    /// buffer roughly once every 15 captures, always an ISOLATED pair (never a triple), every
    /// other capture unique (camera sensor noise + painter motion). Builds `count` synthetic
    /// captures at `capture_fps`; returns `(now_ns, content_id, is_dupe_ground_truth)` in
    /// capture order. `content_id` is a strictly-increasing u64 for every REAL (non-dupe) tick;
    /// a dupe capture repeats the immediately preceding capture's `content_id`.
    fn synthetic_889_capture_sequence(
        capture_fps: f64,
        count: usize,
        dupe_period: usize,
    ) -> Vec<(u64, u64, bool)> {
        let interval_ns = (1_000_000_000.0 / capture_fps) as u64;
        let mut out = Vec::with_capacity(count);
        let mut next_id: u64 = 0;
        let mut prev_id: u64 = 0;
        for i in 0..count {
            let now_ns = i as u64 * interval_ns;
            let is_dupe = i > 0 && dupe_period > 0 && i % dupe_period == dupe_period - 1;
            let content_id = if is_dupe {
                prev_id
            } else {
                let id = next_id;
                next_id += 1;
                id
            };
            prev_id = content_id;
            out.push((now_ns, content_id, is_dupe));
        }
        out
    }

    #[test]
    fn dupe_preferring_gate_never_emits_a_dupe_and_never_sheds_a_unique_tick_889() {
        // (#889) rig validation: cam1's ShadowCast measured 64.14 fps captured against a 60 Hz
        // source, 4.18 byte-identical dupes/s (an isolated pair roughly every 15 captures),
        // every non-dupe frame unique. Simulate ~3 seconds of that exact pattern against the
        // REAL 60fps genlock emit boundary math (`ndi::genlock_emit_gate`, unchanged by this
        // ticket) and assert the fix: zero dupes ever emitted, zero unique ticks shed.
        let captures = synthetic_889_capture_sequence(64.14, 3 * 65, 15);
        let emit_interval_ns = 1_000_000_000u64 / 60;

        let mut gate = DecimationGate::new();
        let mut emitted_ids: Vec<u64> = Vec::new();
        let mut emitted_a_dupe = false;
        for (now_ns, content_id, is_dupe) in &captures {
            if gate.poll(*now_ns, emit_interval_ns, *content_id) {
                emitted_ids.push(*content_id);
                if *is_dupe {
                    emitted_a_dupe = true;
                }
            }
        }

        assert!(
            !emitted_a_dupe,
            "dupe-preferring decimation must never emit a grabber-dupe frame; emitted \
             {emitted_ids:?}"
        );

        // "no unique tick shed": every distinct unique content id in the capture stream must
        // appear in the emitted output — the validated rig evidence shows the dupe rate
        // (~4.18/s) covers the over-rate shedding demand (~4.14/s) almost exactly, so dupes
        // alone should account for every required shed.
        let all_unique_ids: std::collections::BTreeSet<u64> = captures
            .iter()
            .filter(|(_, _, is_dupe)| !is_dupe)
            .map(|(_, id, _)| *id)
            .collect();
        let emitted_set: std::collections::BTreeSet<u64> = emitted_ids.iter().copied().collect();
        let missing: Vec<u64> = all_unique_ids.difference(&emitted_set).copied().collect();
        assert!(
            missing.is_empty(),
            "dupe-preferring decimation must not drop a unique tick when dupes alone cover the \
             shedding demand (validated #889: dupe rate ~4.18/s ~= over-rate delta ~4.14/s); \
             missing unique ids: {missing:?}"
        );
    }
}
