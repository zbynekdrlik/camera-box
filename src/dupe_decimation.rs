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
//! The fix: pacing still decides WHEN a frame must be shed (unchanged —
//! [`crate::ndi::genlock_emit_gate`]); this module decides WHICH captured frame is the victim.
//! [`DecimationGate::poll`] prefers to shed a captured frame that is content-identical to the
//! immediately preceding capture (a grabber dupe), deferring emission by exactly ONE more
//! capture — bounded to a single deferral per boundary so the emitted rate is never affected
//! (validated: dupes are always isolated pairs, never triples, so a second consecutive dupe is
//! not expected on real hardware; the bound protects every model regardless). When the frame
//! that crossed the boundary is NOT a dupe, or a dupe was already deferred once for this
//! boundary, behavior is IDENTICAL to the pre-fix blind pacing drop.
//!
//! Default ON, every grabber model, no env knob (the standing "a needed feature is always on,
//! never a forgettable toggle" rule) — self-neutralizing on a healthy card: shedding only
//! happens when the pacing gate would shed ANYWAY (over-rate forcing a drop), and dupe
//! preference only changes WHICH captured frame within that already-required shed is the
//! victim.
//!
//! Linux-gated in lock-step with capture/ndi (calls into [`crate::ndi::genlock_emit_gate`] and
//! is shaped around a raw V4L2 YUYV422 frame); pure logic, unit-tests Tier-0 on the Linux `test`
//! CI job (default features).

use std::hash::Hasher;

// ── (#889) content-dupe detection ─────────────────────────────────────────────

/// How many rows of a captured YUYV422 frame [`dupe_content_hash`] samples — a FEW rows, not
/// the whole frame (cheap, mirrors [`crate::capture::mean_chroma`]'s row-sampling cost
/// discipline), spread evenly across the frame height. Validated on the rig (#889): a fast
/// grabber's internal buffer repeat reproduces the frame byte-for-byte (sampled rows included);
/// real camera sensor noise + painter motion makes every non-dupe frame's content differ even
/// in a small sampled subset, so byte-exact equality over these rows alone is a reliable
/// "same vs different" test.
const DUPE_HASH_SAMPLE_ROWS: usize = 8;

/// Cheap content fingerprint for grabber-dupe detection. Samples up to
/// [`DUPE_HASH_SAMPLE_ROWS`] rows evenly spaced across the frame height, honoring `stride` (the
/// V4L2 mmap buffer is `stride * height`, NOT `width * 2 * height` — the same gotcha
/// [`crate::capture::mean_chroma`] guards) so a row-padded device never hashes padding bytes.
/// FNV-1a: collision RESISTANCE is not the goal here (only "same vs different" on real
/// hardware, never adversarial safety), just a fast, deterministic, well-distributed fold. A
/// degenerate (zero width/height/stride) frame hashes to 0 — harmless: a zero-size frame never
/// reaches the NDI send path, so two degenerate frames comparing "equal" has no observable
/// effect.
pub fn dupe_content_hash(frame: &[u8], width: usize, height: usize, stride: usize) -> u64 {
    let row_bytes = width * 2; // YUYV422: 2 bytes/pixel
    if height == 0 || row_bytes == 0 || stride == 0 {
        return 0;
    }
    let mut hasher = FnvHasher::new();
    let step = (height / DUPE_HASH_SAMPLE_ROWS).max(1);
    let mut y = 0usize;
    while y < height {
        let row_start = y * stride;
        let row_end = row_start + row_bytes;
        if row_end <= frame.len() {
            hasher.write(&frame[row_start..row_end]);
        } else if row_start < frame.len() {
            hasher.write(&frame[row_start..]);
        }
        y += step;
    }
    hasher.finish()
}

/// Minimal FNV-1a (64-bit) — no extra crate dependency for a "same vs different" fingerprint.
struct FnvHasher(u64);

impl FnvHasher {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Hasher for FnvHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

// ── (#889) victim-selection decision ──────────────────────────────────────────

/// The dupe-preferring decimation decision: given the PACING gate's verdict for this captured
/// frame (`would_emit` — did it cross the target-rate wall-clock boundary?), whether it is a
/// content dupe of the immediately preceding capture, and whether a dupe was ALREADY deferred
/// for the CURRENT pending boundary, decide whether to emit it now.
///
/// - `would_emit == false` (still between boundaries): unchanged blind pacing — never emit.
/// - `would_emit == true` and this frame is a FRESH dupe (not yet deferred this boundary): SHED
///   it instead of emitting — the caller must NOT advance its boundary state, so the very next
///   captured frame is re-evaluated against the SAME still-pending boundary.
/// - `would_emit == true` and (NOT a dupe, OR a dupe was already deferred once this boundary):
///   emit — either genuinely unique, or the bounded one-deferral fallback (validated dupes are
///   always isolated pairs, so a second consecutive dupe is not expected on real hardware; the
///   bound protects every grabber model against ever starving emission indefinitely).
///
/// Returns `(emit, deferred_as_dupe)`. The caller advances its boundary state IFF
/// `!deferred_as_dupe` — see [`DecimationGate::poll`] for the wiring.
pub fn dupe_preferring_decimate(
    would_emit: bool,
    is_dupe: bool,
    already_deferred_this_boundary: bool,
) -> (bool, bool) {
    if !would_emit {
        return (false, false);
    }
    if is_dupe && !already_deferred_this_boundary {
        return (false, true);
    }
    (true, false)
}

// ── (#889) per-stream gate (boundary + dupe-preference state) ────────────────

/// Owns the per-box decimation bookkeeping: the pacing boundary state (mirrors what
/// `src/main.rs` tracked as a bare `next_boundary_ns` local before this ticket) PLUS the
/// dupe-preference state (previous captured frame's content hash + whether a dupe was already
/// deferred for the currently-pending boundary). [`poll`](Self::poll) is the single
/// per-captured-frame call: pure math driven entirely by its inputs, so behavior is fully
/// testable off real hardware — feed synthetic capture timestamps + content hashes, collect
/// which get emitted.
#[derive(Debug, Default, Clone)]
pub struct DecimationGate {
    next_boundary_ns: u64,
    prev_hash: Option<u64>,
    deferred_this_boundary: bool,
    shed_log: DupeShedLog,
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

    /// Feed ONE captured frame (`now_ns` wall-clock capture instant, `content_hash` from
    /// [`dupe_content_hash`]) through the pacing + dupe-preference gate. `interval_ns == 0`
    /// disables decimation entirely (mirrors [`crate::ndi::genlock_emit_gate`]'s own guard) —
    /// always emits, no hashing/state kept. Returns whether THIS captured frame should be
    /// emitted.
    pub fn poll(&mut self, now_ns: u64, interval_ns: u64, content_hash: u64) -> bool {
        if interval_ns == 0 {
            return true;
        }
        let (would_emit, candidate_next) =
            crate::ndi::genlock_emit_gate(now_ns, self.next_boundary_ns, interval_ns);

        let is_dupe = self.prev_hash == Some(content_hash);
        self.prev_hash = Some(content_hash);

        let (emit, deferred) =
            dupe_preferring_decimate(would_emit, is_dupe, self.deferred_this_boundary);

        if deferred {
            // Shed the dupe, keep the SAME boundary pending -- the next captured frame is
            // re-evaluated against it (bounded to one deferral, see the module doc).
            self.deferred_this_boundary = true;
            self.shed_log.record_shed(true);
        } else {
            self.next_boundary_ns = candidate_next;
            if would_emit {
                self.deferred_this_boundary = false;
            }
            if !emit {
                // The ORIGINAL blind pacing drop (between boundaries) -- unchanged pre-fix.
                self.shed_log.record_shed(false);
            }
        }
        emit
    }

    /// Drain the accumulated `(dupe_shed, blind_shed)` counters for the periodic INFO log — see
    /// [`DupeShedLog::take`].
    pub fn take_shed_counts(&mut self) -> (u64, u64) {
        self.shed_log.take()
    }
}

// ── (#889) mechanism-visibility log (comprehensive-logging) ──────────────────

/// Per-run accumulator proving the mechanism is live on a real box: counts how many captured
/// frames were shed because they were the preferred-dupe victim vs the pre-fix blind pacing
/// drop, drained on the SAME 5s Streaming-report cadence as
/// [`crate::emit_skip_log::EmitGateSkipLog`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DupeShedLog {
    dupe_shed: u64,
    blind_shed: u64,
}

impl DupeShedLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record ONE captured frame that was shed (never emitted) this poll: `dupe` when
    /// [`dupe_preferring_decimate`] preferred it as a content-duplicate victim, otherwise the
    /// ORIGINAL blind pacing drop (between boundaries, or the bounded fallback).
    pub fn record_shed(&mut self, dupe: bool) {
        if dupe {
            self.dupe_shed = self.dupe_shed.saturating_add(1);
        } else {
            self.blind_shed = self.blind_shed.saturating_add(1);
        }
    }

    /// Drain the accumulated `(dupe_shed, blind_shed)` counts and RESET.
    pub fn take(&mut self) -> (u64, u64) {
        let out = (self.dupe_shed, self.blind_shed);
        self.dupe_shed = 0;
        self.blind_shed = 0;
        out
    }
}

/// The periodic INFO line proving the mechanism is live: printed on every 5s Streaming-report
/// window (while genlock decimation is active) so a live box shows the mechanism working —
/// never suppressed on an all-zero window (a healthy card legitimately shows 0/0, which is the
/// self-neutralizing behavior by design, not the mechanism being off).
pub fn dupe_shed_summary(dupe_shed: u64, blind_shed: u64, window_secs: u64) -> String {
    format!(
        "(#889) dupe-preferring decimation: {dupe_shed} dupe-victim shed / {blind_shed} \
         blind-pacing shed over the last ~{window_secs}s"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── dupe_content_hash ──────────────────────────────────────────────────

    #[test]
    fn identical_frames_hash_equal() {
        let frame = vec![0x42u8; 4 * 2 * 8]; // width=4 (row_bytes=8), height=8
        let a = dupe_content_hash(&frame, 4, 8, 8);
        let b = dupe_content_hash(&frame, 4, 8, 8);
        assert_eq!(a, b, "identical byte content must hash identically");
    }

    #[test]
    fn differing_frames_hash_differently() {
        let width = 4;
        let height = 8;
        let stride = width * 2;
        let mut a = vec![0x10u8; width * 2 * height];
        let b = vec![0x11u8; width * 2 * height];
        let hash_a = dupe_content_hash(&a, width, height, stride);
        let hash_b = dupe_content_hash(&b, width, height, stride);
        assert_ne!(
            hash_a, hash_b,
            "genuinely different content must not collide"
        );
        // Sanity: flipping ONE sampled byte also changes the hash.
        a[0] = 0xFF;
        let hash_a2 = dupe_content_hash(&a, width, height, stride);
        assert_ne!(
            hash_a, hash_a2,
            "a single changed sampled byte must change the hash"
        );
    }

    #[test]
    fn honors_stride_padding_never_samples_garbage() {
        // width=2 (row_bytes=4), but the device pads each row to stride=16. The pad bytes must
        // never affect the hash -- only the real width*2 pixel bytes per row are sampled.
        let width = 2;
        let height = 4;
        let stride = 16;
        let mut frame_a = vec![0u8; stride * height];
        let mut frame_b = vec![0u8; stride * height];
        for y in 0..height {
            let row = y * stride;
            frame_a[row..row + 4].copy_from_slice(&[1, 2, 3, 4]);
            frame_b[row..row + 4].copy_from_slice(&[1, 2, 3, 4]);
            // Differing PADDING only (bytes beyond row_bytes=4, within stride=16).
            frame_a[row + 4] = 0xAA;
            frame_b[row + 4] = 0xBB;
        }
        let hash_a = dupe_content_hash(&frame_a, width, height, stride);
        let hash_b = dupe_content_hash(&frame_b, width, height, stride);
        assert_eq!(
            hash_a, hash_b,
            "padding bytes beyond the real row width must never affect the hash"
        );
    }

    #[test]
    fn degenerate_frame_is_zero_and_never_panics() {
        assert_eq!(dupe_content_hash(&[], 0, 0, 0), 0);
        assert_eq!(dupe_content_hash(&[1, 2, 3], 4, 1, 0), 0);
    }

    // ── dupe_preferring_decimate ───────────────────────────────────────────

    #[test]
    fn between_boundaries_never_emits_regardless_of_dupe() {
        assert_eq!(
            dupe_preferring_decimate(false, false, false),
            (false, false)
        );
        assert_eq!(dupe_preferring_decimate(false, true, false), (false, false));
    }

    #[test]
    fn fresh_dupe_at_boundary_is_deferred_not_emitted() {
        assert_eq!(dupe_preferring_decimate(true, true, false), (false, true));
    }

    #[test]
    fn already_deferred_dupe_falls_back_to_blind_emit() {
        assert_eq!(dupe_preferring_decimate(true, true, true), (true, false));
    }

    #[test]
    fn non_dupe_at_boundary_emits_unchanged() {
        assert_eq!(dupe_preferring_decimate(true, false, false), (true, false));
        assert_eq!(dupe_preferring_decimate(true, false, true), (true, false));
    }

    // ── DupeShedLog ────────────────────────────────────────────────────────

    #[test]
    fn shed_log_counts_and_resets_on_take() {
        let mut log = DupeShedLog::new();
        log.record_shed(true);
        log.record_shed(true);
        log.record_shed(false);
        assert_eq!(log.take(), (2, 1));
        assert_eq!(log.take(), (0, 0), "take() must reset");
    }

    #[test]
    fn summary_names_both_counts_and_the_889_tag() {
        let s = dupe_shed_summary(4, 12, 5);
        assert!(s.contains("#889"));
        assert!(s.contains('4'));
        assert!(s.contains("12"));
        assert!(s.contains('5'));
    }

    // ── the regression test: DecimationGate end-to-end on the validated pattern ─────

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
        // ticket) and assert the fix: zero dupes ever emitted, zero unique ticks shed in
        // steady state.
        let captures = synthetic_889_capture_sequence(64.14, 3 * 65, 15);
        let emit_interval_ns = 1_000_000_000u64 / 60;

        // The pacing gate's `next_boundary_ns` starts uninitialized (0) and latches its FIRST
        // boundary one interval after the very first capture's timestamp -- so the first
        // capture or two can land BEFORE that first boundary and get blind-decimated purely by
        // simulation-start phase, independent of dupe preference (this happens identically
        // whether or not dupe-awareness is on -- it is inherent to any genlock cold start, not
        // something this fix changes or is expected to fix). Excluding the first few captures'
        // ids from the "every unique tick must be emitted" requirement keeps the assertion about
        // the fix's actual invariant (steady-state behavior), not an unrelated simulation-start
        // artifact.
        const WARMUP_CAPTURES: usize = 4;

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

        // "no unique tick shed" (steady state): every distinct unique content id generated AFTER
        // the cold-start warm-up must appear in the emitted output -- the validated rig evidence
        // shows the dupe rate (~4.18/s) covers the over-rate shedding demand (~4.14/s) almost
        // exactly, so dupes alone should account for every required steady-state shed.
        let all_unique_ids: std::collections::BTreeSet<u64> = captures
            .iter()
            .skip(WARMUP_CAPTURES)
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
