//! (#889) dupe-preferring decimation for the genlock capture->emit gate.
//!
//! Root cause (rig-validated, issue 889): a fast/over-rate USB grabber (ShadowCast 2 measured
//! ~64.14 fps captured against a 60 Hz HDMI source) runs its own capture clock faster than the
//! genlock target rate and repeats its internal buffer to keep up — an exact BYTE-IDENTICAL
//! duplicate frame roughly once every ~15 captures, always an ISOLATED pair (never a triple),
//! every other captured frame genuinely unique (camera sensor noise + painter motion). The
//! pre-existing genlock decimation gate (`genlock_pacing::genlock_emit_gate`) decides purely from
//! WALL-CLOCK TIME which captured frame to emit at each target-rate boundary — it has no notion
//! of frame CONTENT, so it sometimes keeps the grabber's dupe (because it happened to be the
//! frame that crossed the boundary) and drops the unique tick captured just before it. That is
//! the exact mechanism behind the per-cambox-window `copies`/`gaps` failures this ticket fixes.
//!
//! The fix: pacing still decides WHEN a frame must be shed (unchanged —
//! [`crate::genlock_pacing::genlock_emit_gate`]); this module decides WHICH captured frame is the victim.
//! [`DecimationGate::poll`] prefers to shed a captured frame that is content-identical to the
//! immediately preceding capture (a grabber dupe), deferring emission by exactly ONE more
//! capture — bounded to a single deferral per boundary so the emitted rate is never affected
//! (validated: dupes are always isolated pairs, never triples, so a second consecutive dupe is
//! not expected on real hardware; the bound protects every model regardless). When the frame
//! that crossed the boundary is NOT a dupe, or a dupe was already deferred once for this
//! boundary, behavior is IDENTICAL to the pre-fix blind pacing drop.
//!
//! (#1111) A deferral holds the wall-clock boundary for one extra capture, which is lag-neutral
//! ONLY in the on-time/surplus regime (the replacement capture still lands inside the SAME
//! interval). At a genuine over-rate like ~62 fps, a dupe often arrives while the gate is already
//! in the CATCH-UP regime (the frame is late); deferring THERE holds the boundary while the wall
//! clock runs on, ratcheting the gate's lag +1 interval per deferral until it trips
//! `genlock_pacing::genlock_emit_gate`'s #707 resync (~9 boundaries leapt at once) — the issue-1110 CAM1
//! judder. So the deferral is gated on `genlock_pacing::genlock_emit_on_time`: a dupe is deferred only when
//! on-time; a LATE dupe is EMITTED instead (a repeated frame — invisible, and the mathematically
//! unavoidable ~2 copies/s when a ~58-unique-fps grabber must feed a steady 60), keeping the emit
//! grid locked to wall-clock. That emitted-copy is counted in [`DupeShedLog`] for live visibility.
//!
//! (#1145) SUPERSEDED at a genuine over-rate: the ~2 copies/s above are the floor ONLY when the
//! source's UNIQUE rate is genuinely below the target (a 58-unique grabber, a 50->60 pulldown). A
//! plain over-rate on a true-60 source has ~60 unique fps, so ZERO copies are needed and the LATE
//! dupe above was a jitter-driven bug: it presents as the strih 15fps-judder. v2 RETIRES a late
//! over-rate dupe instead (shed it AND advance the already-stale boundary, emitting nothing —
//! [`dupe_shed_action`] / [`ShedAction::Retire`]), gated on a measured trailing UNIQUE rate so a
//! genuinely starved OR frozen source still falls back to the late-dupe copy valve above.
//! `genlock_pacing::genlock_emit_on_time` is retained only as the lag==0 equivalence anchor;
//! production keys on the numeric `genlock_pacing::genlock_lag_intervals` instead.
//!
//! Default ON, every grabber model, no env knob (the standing "a needed feature is always on,
//! never a forgettable toggle" rule) — self-neutralizing on a healthy card: shedding only
//! happens when the pacing gate would shed ANYWAY (over-rate forcing a drop), and dupe
//! preference only changes WHICH captured frame within that already-required shed is the
//! victim.
//!
//! Linux-gated in lock-step with capture/ndi (calls into [`crate::genlock_pacing::genlock_emit_gate`] and
//! is shaped around a raw V4L2 YUYV422 frame); pure logic, unit-tests Tier-0 on the Linux `test`
//! CI job (default features).

use std::collections::VecDeque;
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
    dupe_content_sig(frame, width, height, stride).0
}

/// (#1145 round 3) Pixel stride (in whole PIXELS) between adjacent luma (Y) samples along each of
/// the [`DUPE_HASH_SAMPLE_ROWS`] sampled rows for the noise-tolerant signature lattice. `8` → for a
/// 1920-wide frame, 8 rows × (1920/8) = 1920 lattice points; small enough that a painted QR/burn
/// flip lands decisively on tens of points, sparse enough to stay cheap at 60 fps 1080p (a few
/// thousand byte reads/frame, no allocation beyond the small `Vec`).
pub const DUPE_SIG_PIXEL_STRIDE: usize = 8;

/// (#1145 round 3) The per-point ABSOLUTE luma-diff (after median-offset compensation) at/above
/// which a sampled lattice point counts as CHANGED between two frames. `48` sits deliberately in
/// the wide gap between two physically-separated magnitudes: per-point optical sensor NOISE on the
/// rig path is σ≈2–8 luma (48 is ≥5σ above it, so a genuine noisy re-sample crosses it essentially
/// never), while a painted QR/burn MODULE flip swings ≈100–180 luma even after optical contrast
/// loss (48 ≤ half that swing, so a genuinely-different painted frame crosses it decisively).
/// Calibration value (the ≥5σ / ≤½-swing margins are order-of-magnitude, not tuned); the live E2E
/// re-measure (uniformity ≥0.95 AND clean QR-contiguity) validates it. See [`frames_are_content_dupes`].
pub const NOISY_DUPE_DIFF_THETA: i32 = 48;

/// (#1145 round 3) The maximum number of CHANGED lattice points ([`NOISY_DUPE_DIFF_THETA`]) for two
/// frames to be classified a NOISY content-dupe. `6` (~0.3 % of a ~1920-point lattice) is a small
/// slack for a handful of hot/outlier pixels while staying FAR below the tens of points a real QR
/// flip moves — so the false-POSITIVE direction (calling a genuinely-different painted frame a dupe,
/// which would DROP a real unique) needs a QR flip that somehow touches ≤6 sampled points, which the
/// [`DUPE_SIG_PIXEL_STRIDE`] density makes impossible for the rig's burn geometry. Calibration value;
/// biased hard to false-NEGATIVE (a miss just falls back to today's heuristic — status quo).
pub const NOISY_DUPE_MAX_CHANGED: usize = 6;

/// (#1145 round 3) Single-pass content signature: the exact FNV fingerprint (BYTE-identical to
/// [`dupe_content_hash`]'s historical value, so a buffer-repeat dupe like CAM1's still short-circuits
/// on it) PLUS a luma (Y) lattice for the NOISE-TOLERANT compare a marginal jittery over-rate card
/// (CAM2) needs — its surplus is a noisy optical RE-SAMPLE of the same painted frame, NOT a
/// byte-identical repeat, so exact equality misses it (#1145 root cause). Samples the SAME
/// [`DUPE_HASH_SAMPLE_ROWS`] evenly-spaced rows the hash reads (stride-honoring — never padding
/// bytes), taking the Y byte (even offset in YUYV422) every [`DUPE_SIG_PIXEL_STRIDE`] pixels into
/// the lattice. A degenerate (zero width/height/stride) frame → `(0, empty)`; an empty lattice
/// compares not-dupe in [`frames_are_content_dupes`] (fail-safe).
pub fn dupe_content_sig(
    frame: &[u8],
    width: usize,
    height: usize,
    stride: usize,
) -> (u64, Vec<u8>) {
    let row_bytes = width * 2; // YUYV422: 2 bytes/pixel
    if height == 0 || row_bytes == 0 || stride == 0 {
        return (0, Vec::new());
    }
    let mut hasher = FnvHasher::new();
    let mut luma: Vec<u8> = Vec::new();
    let step = (height / DUPE_HASH_SAMPLE_ROWS).max(1);
    let mut y = 0usize;
    while y < height {
        let row_start = y * stride;
        let row_end = row_start + row_bytes;
        // Exact fingerprint: hash the real pixel bytes of this row (clamped to the buffer), IDENTICAL
        // to the historical `dupe_content_hash` byte-for-byte (verified by its retained tests).
        if row_end <= frame.len() {
            hasher.write(&frame[row_start..row_end]);
        } else if row_start < frame.len() {
            hasher.write(&frame[row_start..]);
        }
        // Luma lattice: the Y byte (even offset) every DUPE_SIG_PIXEL_STRIDE pixels along this row.
        let mut x = 0usize;
        while x < width {
            let px = row_start + x * 2;
            if px < frame.len() {
                luma.push(frame[px]);
            }
            x += DUPE_SIG_PIXEL_STRIDE;
        }
        y += step;
    }
    (hasher.finish(), luma)
}

/// (#1145 round 3) NOISE-TOLERANT content-dupe test over two luma lattices from [`dupe_content_sig`]
/// (same length). Two captures of the SAME painted frame differ only by per-point sensor NOISE (a
/// handful of points, if any, cross [`NOISY_DUPE_DIFF_THETA`]); two DIFFERENT painted frames differ
/// in the burn/QR region (many points cross it). So: `is_dupe = changed_count ≤ `[`NOISY_DUPE_MAX_CHANGED`].
///
/// The per-point diff is compensated by the MEDIAN of all diffs first — a calibration-free global
/// exposure / display-PWM-backlight-beat offset (a same-frame re-capture can be uniformly a few luma
/// brighter/darker). The median is robust to the QR outliers (they are a minority, so a real flip
/// still stands out AFTER the subtraction — a bidirectional flip keeps the median near 0). Mismatched
/// or empty lattices → NOT a dupe (fail-safe: the caller then keeps today's exact-hash behavior).
///
/// Asymmetric by design: a false-NEGATIVE (missing a noisy dupe) only reverts to the pre-existing
/// heuristic shed (status quo); a false-POSITIVE would DROP a genuine unique (a real gap), so both
/// thresholds are set well inside the physical margin and the caller arms this ONLY under sustained
/// over-rate and never on two consecutive frames.
pub fn frames_are_content_dupes(prev: &[u8], now: &[u8]) -> bool {
    if prev.is_empty() || prev.len() != now.len() {
        return false;
    }
    let mut diffs: Vec<i32> = now
        .iter()
        .zip(prev.iter())
        .map(|(&a, &b)| a as i32 - b as i32)
        .collect();
    diffs.sort_unstable();
    let median = diffs[diffs.len() / 2];
    let changed = diffs
        .iter()
        .filter(|&&d| (d - median).abs() >= NOISY_DUPE_DIFF_THETA)
        .count();
    changed <= NOISY_DUPE_MAX_CHANGED
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

// ── (#1145) v2 tuning constants ──────────────────────────────────────────────

/// (#1167) The largest corrupted-slot make-up DEFICIT the gate carries. Each corrupted buffer
/// dropped in `src/capture.rs::process_frame` (before the emit gate) removes a would-be-emitted
/// GOOD frame from an over-rate stream, so its 60 fps slot would be absorbed by the over-rate
/// shed machinery ([`ShedAction::Retire`] / [`ShedAction::Drain`] — advance the boundary, emit
/// nothing) instead of filled → emit under-runs by exactly the corrupted rate (the strih FIFO
/// hold → cam1 align sawtooth). [`DecimationGate::note_corrupted_frame`] accrues one unit of this
/// deficit per corrupted drop; [`DecimationGate::poll`] reclaims it 1:1 by converting the NEXT
/// slot-skipping Retire/Drain into a copy emit (the nearest good frame). Bounded to `8` (the
/// #707/#1131 resync catch-up bound): beyond a burst this size the source is genuinely starved
/// and the existing #1111 copy valve / `enough_unique` handoff carries it, so the make-up never
/// forces a long tail of copies after corruption stops.
pub const CORRUPTED_MAKEUP_MAX_DEFICIT: u64 = 8;

/// (#1145) The largest boundary lag (in whole emit-boundary intervals) at which a stale
/// over-rate content-dupe is RETIRED rather than emitted as a copy. Chosen well BELOW the #707
/// resync trigger ([`crate::genlock_pacing::GENLOCK_MAX_CATCHUP_INTERVALS`] = 8): retirement drains
/// the dupe-driven lag at up to the dupe rate, so at a genuine over-rate the lag never approaches
/// the resync bound (measured peak ~4 across seeds/jitter in the off-rig sim). Beyond this ceiling
/// a genuine sustained deficit is building, so the late-dupe valve emits a copy instead — the
/// panic floor that keeps the emit grid boundary-locked. `4` gives 0 emitted copies at the rig
/// takt (61.x) with realistic jitter AND a comfortable margin (4) to the resync bound.
pub const RETIRE_MAX_LAG_INTERVALS: u64 = 4;

/// (#1145) Trailing wall-clock window over which the UNIQUE (non-dupe) capture arrivals are counted
/// to decide whether the source carries enough distinct content to hold a steady 60 fps without
/// fabricating copies. 2 s is long enough to integrate out per-frame jitter and dupe clustering
/// (a windowed COUNT, unlike an interval EMA, reads the true unique RATE regardless of local
/// spacing) yet short enough that retirement engages within ~2 s of a sustained over-rate.
pub const UNIQUE_RATE_WINDOW_NS: u64 = 2_000_000_000;

/// (#1145) Margin (in unique captures) subtracted from the window's theoretical full count
/// (`UNIQUE_RATE_WINDOW_NS / interval_ns`, e.g. ~120 at a 60 fps target) to derive the retirement
/// floor ([`retire_min_uniques`]). The source must be delivering nearly the full target's worth of
/// DISTINCT content over the trailing window for retirement to engage; below it the source is
/// genuinely starved (a sub-60 source padded to 60 by DUPLICATION — a 50->60 pulldown) and the
/// late-dupe copy valve stays engaged to hold the emit grid at the target (keeping the strih FIFO
/// locked AND leaving the content-dupes in the recording for the duplication-masked pulldown
/// detector). The count is pruned by `now_ns` on EVERY poll (honest at every instant). A true-60
/// over-rate source's honestly-pruned count dips to ~115 at dupe instants under heavy jitter, while
/// a 50-fps pulldown reads ~100/2 s — so the floor MUST sit between them, and cannot ALSO be above a
/// 57.9-unique source (which reads ~114-117, overlapping the jittery-60 case: a 2 s windowed COUNT
/// genuinely cannot separate 60-unique-with-jitter from 57.9-unique). We prioritize the RIG (unique
/// 60): `6` -> floor 114 at 60 fps == 57 fps, which reliably retires the rig even at 30 % jitter,
/// keeps a real pulldown (~100) on the copy valve, and puts the retire/copy boundary at ~57 unique
/// fps — deliberately aligned with the #666 EMIT-rate-deficit floor (5 % of 60), so any source whose
/// honest emit would trip #666 (< 57 fps) gets copies to hold 60, and any source above it emits its
/// honest rate. Parametric on `interval_ns` (#1145 review): follows a non-60 emit target instead of
/// silently no-oping.
pub const RETIRE_UNIQUE_COUNT_MARGIN: u64 = 6;

/// (#1145 review 🔴) Freshness bound for retirement, in whole emit intervals: retirement engages only
/// when the MOST RECENT unique capture arrived within this many intervals of `now`. A genuinely
/// FROZEN source (a dead painter / wedged upstream feeding a still — the #1052/#365 frozen-input
/// class) delivers 100% content-dupes: no unique ever refreshes the window, so its stale count stays
/// high and — without this bound — retirement would fire forever and collapse the NDI emit to ~0 fps
/// (a total output BLACKOUT, strictly worse than a frozen picture). The freshness bound makes a
/// freeze fall back to the late-dupe copy valve within ~this many intervals (a frozen PICTURE on a
/// LIVE, FIFO-fed stream — the pre-#1145 behavior). `5` intervals (~83 ms at 60 fps) is safely above
/// the largest gap since a unique during healthy over-rate operation (an isolated dupe pair sits ~2-3
/// intervals after a unique) yet kills a freeze promptly.
pub const RETIRE_UNIQUE_FRESH_BOUND_INTERVALS: u64 = 5;

/// (#1145) The minimum UNIQUE captures within [`UNIQUE_RATE_WINDOW_NS`] for retirement to engage at
/// the given emit `interval_ns` — the window's theoretical full count minus
/// [`RETIRE_UNIQUE_COUNT_MARGIN`]. `interval_ns == 0` (genlock off) never retires.
pub fn retire_min_uniques(interval_ns: u64) -> usize {
    if interval_ns == 0 {
        return usize::MAX;
    }
    (UNIQUE_RATE_WINDOW_NS / interval_ns).saturating_sub(RETIRE_UNIQUE_COUNT_MARGIN) as usize
}

/// (#1145 v3) Minimum captures in the trailing [`UNIQUE_RATE_WINDOW_NS`] before the OCCUPANCY floor
/// (below) is consulted — a small-sample guard so a cold start (few captures) can never satisfy the
/// ratio. `30` ≈ half a second of captures.
pub const RETIRE_OCCUPANCY_MIN_SAMPLES: usize = 30;

/// (#1145 v3) The GAP-IMMUNE occupancy floor: the minimum `unique / total` capture ratio (percent)
/// in the trailing window for [`DecimationGate::enough_unique_to_hold_target`] to hold, an OR
/// supplement to the ABSOLUTE count floor ([`retire_min_uniques`]). A capture HICCUP transiently
/// depresses the absolute count (the 2 s window spans dead time with no captures), forcing a genuine
/// over-rate card onto the #1111 copy valve for ~the gap duration — the surplus then exports into the
/// strih FIFO (the #1145 v3 residual). The unique/total RATIO is gap-immune (a gap admits NO captures,
/// so BOTH counts drop equally). `95` is #666-safe: this arm is gated on `sustained_over_rate` (capture
/// takt below [`RETIRE_MIN_TAKT_INTERVAL_NS`] = capture rate `> 60.3`), so `unique >= 0.95 × total`
/// with `total`-rate `> 60.3` guarantees the retired emit (= the unique rate) stays `>= 0.95 × 60 = 57`
/// (the #666 emit-deficit floor) — an under-rate / starved source (NOT over-rate) never reaches this
/// arm, so retiring can never drop it below 57. A 50->60 pulldown (~0.83 ratio) stays on the copy valve.
pub const RETIRE_OCCUPANCY_MIN_PERCENT: u64 = 95;

// ── (#1145 v2) queue-DEPTH-bounded drain: absorb the over-rate takt CONTINUOUSLY ──────────────
//
// The merged v1 shed/retire keys on [`crate::genlock_pacing::genlock_lag_intervals`] (BOUNDARY
// staleness). When the emit loop is send-bound (~60 fps) and the card captures 61.x, the loop
// processes the OLDEST buffered V4L2 frame each poll and `now` (realtime) lands right on the
// advancing boundary, so the lag reads ~0 the whole time — v1 is BLIND to the growing
// capture->emit QUEUE RESIDENCE. The residence sawtooths (delivery lag 67->167 ms, issue
// 1110/1130) until the 4-deep V4L2 buffer overflow-drops in a burst, and THAT burst is what the
// #1142 uniformity gate reads at ~0.77-0.89 on cam1. v2 measures the residence DIRECTLY
// (`now_monotonic - capture_monotonic`) and sheds the oldest frame once it exceeds a small
// target, draining the over-rate one frame at a time instead of letting it accumulate — GATED on
// a sustained-over-rate capture takt so a healthy 60.00 card (and a #1131 buffered-drain
// stall-recovery on one) is byte-identical to v1.

/// (#1145 v2) The over-rate capture takt threshold, as the minimum EMA capture INTERVAL below which
/// the card is "sustained over-rate": `1e9 / 60.3` ns (~16.584 ms). Integer form to keep it a plain
/// `const`. The ticket names this bound explicitly ("pri sustained over-rate takt >60.3"): a 60.00
/// card reads an EMA interval of ~16.667 ms (ABOVE this — NOT over-rate → the whole depth-shed is
/// OFF, so a healthy card and a transient #1131 stall-recovery on one stay byte-identical to v1),
/// while a 61.x card reads ~16.3 ms (below → over-rate → depth-shed engages). Deliberately at 60.3
/// (not 60.0) so ordinary sub-frame jitter on a genuine 60.00 card never trips it.
pub const RETIRE_MIN_TAKT_INTERVAL_NS: u64 = 1_000_000_000 * 10 / 603;

/// (#1145 v2) Right-shift for the integer EMA that smooths the capture takt: `new = old + ((sample
/// - old) >> SHIFT)`. `8` gives a ~2^8 = 256-frame (~4 s at 60 fps) time constant — long enough to
/// integrate out per-frame V4L2 dequeue jitter into the true sustained takt, short enough that a
/// card that starts drifting is classified over-rate within a few seconds. Init-seeded to the first
/// observed interval so there is no long cold-start (see [`DecimationGate::note_capture_takt`]).
pub const TAKT_EMA_SHIFT: u32 = 8;

/// (#1145 v3) The largest inter-capture interval that is FOLDED into the capture-takt EMA
/// ([`DecimationGate::note_capture_takt`]) — `3×` the 60 fps emit interval (50 ms). A genuine takt
/// change shows in EVERY sample (~8-25 ms at an over-rate); a delivery HICCUP (a blocked V4L2
/// dequeue — a CPU/#752/USB stall) shows as ONE huge outlier that is NOT a takt change. Folding
/// that outlier into the ~256-frame EMA poisons it: at the 61.5 fps rig takt the EMA sits ~0.32 ms
/// below [`RETIRE_MIN_TAKT_INTERVAL_NS`], so a single `>~99 ms` hiccup flips `sustained_over_rate`
/// off and the τ≈256-frame recovery holds it off for ~7 s (500 ms gap) / ~12 s (1.5 s gap) —
/// disarming depth-Drain, FastDrain AND the round-3 noisy-dupe compare, so the over-rate surplus
/// leaks into the strih FIFO (the #1145 v3 residual). A sample above this bound is SKIPPED (not
/// folded), while `prev_capture_mono_ns` still advances so the NEXT interval is measured cleanly
/// from the post-gap capture. `3×` (not 2×) leaves headroom above the worst legitimate over-rate +
/// USB jitter sample (≤ ~2× nominal) while still excluding any genuine multi-interval stall.
pub const TAKT_GAP_EXCLUDE_NS: u64 = 3 * (1_000_000_000 / 60);

/// (#1145 v3 review 🟡 F1) How many CONSECUTIVE over-[`TAKT_GAP_EXCLUDE_NS`] inter-capture samples
/// distinguish a one-off delivery HICCUP (skip the lone outlier) from a GENUINE sustained rate
/// COLLAPSE (a card dropping below ~20 fps — every interval over-bound). At/above this count the takt
/// EMA is RESET so `sustained_over_rate` disarms (a collapsed card is NOT over-rate) and re-seeds when
/// it recovers, instead of latching the over-rate drains on forever. `3` catches a genuine collapse
/// in ~3 frames while a lone hiccup (exactly ONE over-bound sample) never reaches it — the B.1 fix is
/// fully preserved. A collapsed `< 20 fps` card is itself an alarm-class failure owned by the
/// grabber-STUCK self-heal; this just keeps the over-rate arming honest through it.
pub const TAKT_GAP_SUSTAINED_COUNT: u32 = 3;

/// (#1145 v2) The queue-residence depth (in whole emit intervals) at/above which the oldest queued
/// frame is SHED to drain one interval of delivery latency — the target the over-rate is held to.
/// `2`: an emitted frame then carries at most ~1 interval of queue residence (fresh), and the
/// capture-stage residence never climbs toward the 4-deep V4L2 overflow, so the downstream FIFO is
/// fed fresh content too. A healthy 60.00 card sits at residence ~0-1 and (being NOT over-rate) never
/// reaches this arm regardless. Calibration value; the live E2E re-measure tunes it.
pub const QUEUE_DEPTH_SHED_INTERVALS: u64 = 2;

/// (#1145 v2) A DETECTED content-dupe drains one interval EARLIER than [`QUEUE_DEPTH_SHED_INTERVALS`]
/// — shedding a byte-identical re-sample is always content-safe (its neighbour carries the same
/// painted frame), so draining it at residence `>= 1` keeps the queue shallower with ZERO risk of
/// dropping a distinct painted frame. `1`. Only reached when already over-rate.
pub const QUEUE_DEPTH_DUPE_SHED_INTERVALS: u64 = 1;

/// (#1145 v2) Sanity ceiling on the computed queue-residence depth: a bogus/huge
/// `capture_monotonic` (or a clock-domain mismatch) must never be read as a runaway depth that
/// force-sheds far beyond the real 4-deep V4L2 buffer. `8` == the #707/#1131 resync catch-up bound
/// ([`crate::genlock_pacing::GENLOCK_MAX_CATCHUP_INTERVALS`]); a real residence cannot exceed the
/// buffer depth, so clamping here only defends against a garbage timestamp.
pub const QUEUE_DEPTH_SANE_MAX_INTERVALS: u64 = 8;

/// (#1145 v2) The queue-residence depth of a captured frame, in whole emit intervals: how long the
/// frame sat between its CAPTURE instant (`capture_mono_ns`, the V4L2 buffer's `CLOCK_MONOTONIC`
/// timestamp) and the instant the loop PROCESSED it (`now_mono_ns`, `monotonic_clock_ns()`), divided
/// by `interval_ns`. This is a DURATION, so it is measured on the monotonic clock (immune to the
/// DanteSync/NTP realtime steps the emit boundary is gridded to). `0` (no drain) when the signal is
/// unavailable: `interval_ns == 0` (genlock off), `capture_mono_ns == 0` (the FrameInfo "no real
/// measurement" sentinel), or `now_mono_ns <= capture_mono_ns` (a monotonic non-advance / bogus
/// stamp). Clamped to [`QUEUE_DEPTH_SANE_MAX_INTERVALS`] so a garbage timestamp can never force a
/// runaway shed. (#1145 v2 review 🔵) The residence trusts `capture_mono_ns` to be the SAME
/// `CLOCK_MONOTONIC` domain as `now_mono_ns` — the repo-wide #286 assumption for the V4L2 buffer
/// timestamp; a device stamping a lower-epoch domain would read a huge residence, but the
/// `QUEUE_DEPTH_SANE_MAX_INTERVALS` clamp bounds the consequence to a bounded (not unbounded) drain.
pub fn queue_depth_intervals(now_mono_ns: u64, capture_mono_ns: u64, interval_ns: u64) -> u64 {
    if interval_ns == 0 || capture_mono_ns == 0 || now_mono_ns <= capture_mono_ns {
        return 0;
    }
    ((now_mono_ns - capture_mono_ns) / interval_ns).min(QUEUE_DEPTH_SANE_MAX_INTERVALS)
}

// ── (#889/#1145) victim-selection decision ────────────────────────────────────

/// (#1145) The per-captured-frame shed/emit decision, one of five actions. `would_emit` is the
/// PACING gate's verdict (did this capture cross the wall-clock boundary?); `is_dupe` whether it is
/// a byte-identical content dupe of the immediately preceding capture; `lag_intervals` how many
/// whole boundary intervals `now` sits PAST the pending boundary
/// ([`crate::genlock_pacing::genlock_lag_intervals`]); `enough_unique_to_hold_target` whether the
/// trailing-window UNIQUE rate proves the source can hold a steady 60 fps without copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShedAction {
    /// Emit this captured frame and ADVANCE the boundary one interval. `copy == true` marks a
    /// content-dupe emitted as the late-dupe valve (a repeated frame downstream — the starvation
    /// floor / double-dupe guard); `false` for a genuinely unique tick.
    Emit { copy: bool },
    /// #889 on-time deferral: HOLD the boundary (do NOT advance) and shed this content-dupe — the
    /// next capture re-evaluates against the SAME still-pending boundary, so the dupe is replaced
    /// by a unique that still lands inside the interval (lag-neutral).
    Defer,
    /// #1145 stale-boundary retirement: shed this content-dupe AND advance the boundary one
    /// interval, emitting nothing. The boundary the dupe crossed is already stale (`lag >= 1` — the
    /// downstream hold for it already happened), so retiring it costs no new downstream artifact,
    /// sacrifices no unique, AND drains the dupe-driven lag.
    Retire,
    /// #1145 v2 queue-DEPTH drain: shed the OLDEST (this) frame AND advance the boundary one
    /// interval, emitting nothing — the sustained-over-rate absorption. Fires when the capture takt
    /// is genuine over-rate AND this frame's queue RESIDENCE (`now_monotonic - capture_monotonic`)
    /// has exceeded the depth target, so the delivery-latency sawtooth is drained one frame at a
    /// time instead of accumulating into a burst. Distinct from [`Retire`](Self::Retire): Retire keys
    /// on BOUNDARY lag and only sheds a content-dupe; Drain keys on the queue RESIDENCE and, above
    /// the target, sheds the oldest frame regardless (its downstream tick has already passed, so it
    /// is a controlled single-frame drop that pre-empts the uncontrolled V4L2 overflow-drop).
    ///
    /// (#1145 v2 review 🔵) #1131 interaction for an OVER-RATE card: a transient-stall buffered
    /// drain on an over-rate card now DRAINS (sheds the oldest one at a time) rather than emitting
    /// every buffered frame — the intended bound-latency behavior (emitting the whole burst would
    /// just re-judder). This is a SINGLE-frame drop, never a grid-resync leap, so #1131's
    /// leap-past-and-discard-a-run is still avoided. A 60.00 card is unaffected (not over-rate →
    /// never drains → emits every buffered frame, byte-identical to v1 — constraint c).
    Drain,
    /// (#1145 v2.1) DEEP-backlog accelerated drain: shed this content-dupe AND advance the boundary
    /// by TWO intervals (retire an EXTRA already-stale boundary), emitting nothing — the accelerated
    /// convergence of a deep emit-grid backlog (the delivery-latency lag the owner's painter-QR
    /// measured at 12+ frames after a reconnect / restart / burn toggle). Fires ONLY at a sustained
    /// over-rate when the grid lag is DEEP (`lag > `[`RETIRE_MAX_LAG_INTERVALS`], == 2x the
    /// [`QUEUE_DEPTH_SHED_INTERVALS`] target), where v2 would emit the late dupe as a COPY (no grid
    /// advance) — so the deep backlog drained only at the send-slack rate (~0.3 frame/s, the owner's
    /// measured ~35 s). Distinct from [`Retire`](Self::Retire): Retire advances ONE interval (the
    /// steady over-rate absorption, lag <= 4); FastDrain advances TWO (drain up to 2 slots per emit
    /// interval) — the extra boundary is ALSO already stale (lag > 4 >> 2), so no new downstream gap
    /// and no unique dropped (only the dupe is shed; the +2 is guarded in [`DecimationGate::poll`] so
    /// it never advances the grid into the future). Only DUPES take this path, so the issue-1131
    /// "never drop a unique while uniques exist" constraint holds and the emit rate stays >= the #666
    /// floor.
    FastDrain,
    /// Between boundaries (`!would_emit`): blind-shed, boundary unchanged — the pre-existing pacing
    /// decimation drop.
    BlindShed,
}

/// (#889/#1111/#1145) Decide the [`ShedAction`] for one captured frame. Pure — driven entirely by
/// its inputs, so the whole cadence policy is testable off real hardware.
///
/// - `!would_emit` (between boundaries) -> [`ShedAction::BlindShed`] (unchanged blind pacing).
/// - unique tick -> [`ShedAction::Emit`]`{ copy: false }` (unchanged).
/// - content-dupe, `lag == 0` (on-time/surplus): #889 -> [`ShedAction::Defer`] once; a second dupe
///   for the SAME boundary (`already_deferred`) -> [`ShedAction::Emit`]`{ copy: true }` (the bounded
///   one-deferral guard — validated dupes are isolated pairs).
/// - content-dupe, `1 <= lag <= `[`RETIRE_MAX_LAG_INTERVALS`], AND `enough_unique_to_hold_target`:
///   #1145 -> [`ShedAction::Retire`]. The boundary is already stale, and the source has enough
///   distinct content that shedding this dupe won't drop the emit below 60 — so retire it (0 copies,
///   0 dropped uniques, drains lag).
/// - content-dupe otherwise (NOT enough unique — genuine starvation; OR `lag > `the retire ceiling
///   — a sustained deficit building): [`ShedAction::Emit`]`{ copy: true }` — the #1111 late-dupe
///   valve, now a starvation floor that holds the emit grid boundary-locked at 60.
///
/// (#1145 v2) BEFORE all of the above, a sustained-over-rate QUEUE-DEPTH drain runs — this is the
/// arm that actually bounds the delivery-latency sawtooth the lag-based v1 could not see.
/// `queue_depth_intervals` is the frame's monotonic queue residence ([`queue_depth_intervals`]);
/// `sustained_over_rate` whether the capture takt EMA is genuine over-rate (a healthy 60.00 card is
/// FALSE here, so this whole block is skipped and the card is byte-identical to v1 — including a
/// #1131 buffered-drain stall-recovery on one). When over-rate:
/// - residence `>= `[`QUEUE_DEPTH_SHED_INTERVALS`] -> [`ShedAction::Drain`]: shed the OLDEST (this)
///   frame regardless of dupeness — a controlled single-frame drop that drains one interval of
///   latency and pre-empts the uncontrolled V4L2 overflow-drop (the burst that shows as judder).
/// - a DETECTED content-dupe at residence `>= `[`QUEUE_DEPTH_DUPE_SHED_INTERVALS`] ->
///   [`ShedAction::Drain`]: drains one interval EARLIER, always content-safe (a byte-identical
///   re-sample carries no distinct painted frame).
/// (#1167) Should this poll RECLAIM a corrupted-induced slot — convert a slot-skipping over-rate
/// shed into a copy emit of the nearest good frame? True iff a make-up is owed
/// (`corrupted_makeup_deficit > 0`) AND the base [`ShedAction`] would advance the boundary while
/// emitting NOTHING for a slot that a captured frame IS available to fill: [`ShedAction::Retire`]
/// (a stale-boundary dupe retirement) or [`ShedAction::Drain`] (a queue-depth over-rate drop).
///
/// Deliberately NOT the other actions: [`ShedAction::Emit`] already fills the slot;
/// [`ShedAction::Defer`] HOLDS the boundary so the next unique still fills it (no slot lost);
/// [`ShedAction::BlindShed`] is a between-boundaries drop (no slot to fill); and
/// [`ShedAction::FastDrain`] is the deep-backlog accelerated convergence (issue-1145 v2.1) — a
/// corruption make-up there would fight the backlog drain, so a deep backlog converges first and
/// the deficit is reclaimed once steady Retire/Drain resume. Pure — the whole make-up policy is
/// Tier-0 testable off hardware.
pub fn corrupted_makeup_reclaims(action: ShedAction, corrupted_makeup_deficit: u64) -> bool {
    corrupted_makeup_deficit > 0 && matches!(action, ShedAction::Retire | ShedAction::Drain)
}

pub fn dupe_shed_action(
    would_emit: bool,
    is_dupe: bool,
    already_deferred_this_boundary: bool,
    lag_intervals: u64,
    enough_unique_to_hold_target: bool,
    queue_depth_intervals: u64,
    sustained_over_rate: bool,
) -> ShedAction {
    if !would_emit {
        return ShedAction::BlindShed;
    }
    // (#1145 v2) sustained-over-rate queue-DEPTH drain — the continuous over-rate absorption that
    // keeps the delivery latency flat (see the module + `queue_depth_intervals` docs). Gated on
    // `sustained_over_rate` so a healthy 60.00 card never reaches it (byte-identical to v1); shed
    // the oldest frame once its queue residence exceeds the target, one frame at a time.
    if sustained_over_rate {
        // (#1145 v2 review 🔵) The FIRST arm INTENTIONALLY sheds the oldest regardless of
        // `enough_unique_to_hold_target` — when the residence has already reached the target the
        // latency MUST be bounded, so bounding it overrides the "keep content-dupes for the
        // duplication-masked pulldown detector" invariant the second arm + retirement preserve. In
        // practice this only bites a genuinely-starved source captured at an over-rate takt (rare);
        // there the bounded-latency win outranks preserving a dupe the detector could read.
        if queue_depth_intervals >= QUEUE_DEPTH_SHED_INTERVALS {
            return ShedAction::Drain;
        }
        // The SECOND arm drains a DETECTED dupe one interval earlier — content-safe, and it DOES
        // preserve the pulldown invariant (`enough_unique_to_hold_target` gate), so a starved source
        // never loses its dupes here.
        if is_dupe
            && enough_unique_to_hold_target
            && queue_depth_intervals >= QUEUE_DEPTH_DUPE_SHED_INTERVALS
        {
            return ShedAction::Drain;
        }
    }
    if !is_dupe {
        return ShedAction::Emit { copy: false };
    }
    if lag_intervals == 0 {
        if already_deferred_this_boundary {
            return ShedAction::Emit { copy: true };
        }
        return ShedAction::Defer;
    }
    if enough_unique_to_hold_target && lag_intervals <= RETIRE_MAX_LAG_INTERVALS {
        return ShedAction::Retire;
    }
    // (#1145 v2.1) DEEP backlog (lag > RETIRE_MAX_LAG_INTERVALS == 2x the depth target) at a
    // sustained over-rate with enough distinct content: this is a TRANSIENT grid backlog to drain
    // (a reconnect / restart / burn-toggle left the emit grid behind), NOT a genuine sustained
    // deficit. v2 emitted these late dupes as COPIES (no grid advance), so a deep backlog drained
    // only at the send-slack rate. Retire the dupe AND advance TWO stale boundaries (FastDrain), so
    // the delivery-latency backlog converges at ~2x the dupe rate — single-digit seconds instead of
    // ~35 s. Gated on `sustained_over_rate` so a healthy 60.00 card (and a non-over-rate deficit)
    // is UNAFFECTED (below), and on `enough_unique_to_hold_target` so a genuinely starved OR frozen
    // source still emits the copy (the panic floor). Only dupes take this path — no unique dropped.
    if sustained_over_rate
        && enough_unique_to_hold_target
        && lag_intervals > RETIRE_MAX_LAG_INTERVALS
    {
        return ShedAction::FastDrain;
    }
    ShedAction::Emit { copy: true }
}

// ── (#889) per-stream gate (boundary + dupe-preference state) ────────────────

/// (#1145 v3 review 🔵 F4) Drop every front entry `<= cutoff` from a trailing-timestamp window.
/// Shared by the unique-rate AND all-captures windows so they prune in lock-step by construction.
fn prune_before(times: &mut VecDeque<u64>, cutoff: u64) {
    while let Some(&front) = times.front() {
        if front <= cutoff {
            times.pop_front();
        } else {
            break;
        }
    }
}

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
    /// (#1145) Trailing wall-clock timestamps of the recent UNIQUE (non-dupe) captures within
    /// [`UNIQUE_RATE_WINDOW_NS`]. Its length is the measured unique RATE — the robust "enough
    /// distinct content to hold 60 fps" signal that gates retirement vs the starvation copy valve.
    unique_capture_times: VecDeque<u64>,
    /// (#1145 v3) Trailing wall-clock timestamps of ALL captures (dupe or unique) within
    /// [`UNIQUE_RATE_WINDOW_NS`], pruned in lock-step with [`unique_capture_times`](Self::unique_capture_times).
    /// The `unique / all` RATIO is the GAP-IMMUNE occupancy floor (see [`RETIRE_OCCUPANCY_MIN_PERCENT`]):
    /// a capture hiccup admits NO captures, so it depresses BOTH counts equally and the ratio holds,
    /// whereas the absolute unique COUNT drops below [`retire_min_uniques`] for ~the gap duration.
    all_capture_times: VecDeque<u64>,
    /// (#1145 v2) The MONOTONIC capture instant of the previous frame, to derive the capture
    /// takt (inter-capture interval) that feeds [`takt_ema_interval_ns`]. `0` = uninitialized.
    prev_capture_mono_ns: u64,
    /// (#1145 v2) Integer EMA of the capture takt (inter-capture interval, ns), smoothed with
    /// [`TAKT_EMA_SHIFT`]. Init-seeded to the first observed interval (no long cold-start). Below
    /// [`RETIRE_MIN_TAKT_INTERVAL_NS`] means sustained over-rate — the gate that enables the
    /// queue-depth drain (a healthy 60.00 card reads above it and never drains). `0` = not yet seen.
    takt_ema_interval_ns: u64,
    /// (#1145 v3 review 🟡 F1) Run length of CONSECUTIVE over-[`TAKT_GAP_EXCLUDE_NS`] inter-capture
    /// samples. A one-off hiccup is exactly ONE; at [`TAKT_GAP_SUSTAINED_COUNT`] the takt EMA is reset
    /// so a genuine sub-20 fps COLLAPSE disarms `sustained_over_rate` instead of latching it forever.
    /// Reset to 0 by any in-bound sample.
    consecutive_takt_gaps: u32,
    /// (#1145 v2.1) How many EXTRA boundary intervals the MOST RECENT [`poll`](Self::poll) advanced
    /// beyond the normal single-interval step — i.e. the INTENTIONAL retirement of already-stale
    /// boundaries by [`ShedAction::FastDrain`] (`1` when the +2 fast-drain fired, `0` otherwise).
    /// `main.rs` reads it via [`last_poll_intentional_extra_advance`](Self::last_poll_intentional_extra_advance)
    /// and DEDUCTS it from the `#707` [`crate::genlock_pacing::boundary_skip_count`] diagnostic, so an
    /// intentional fast-drain is NOT miscounted as an un-emitted-content boundary SKIP (the sick-leg
    /// / clock-step signature `leg-health-guard.sh` hard-fails on). Reset to 0 at the start of every
    /// poll.
    last_poll_fast_drain_extra: u64,
    /// (#1145 round 3) The CURRENT frame's luma signature lattice ([`dupe_content_sig`]), staged by
    /// [`note_frame_luma`](Self::note_frame_luma) immediately BEFORE [`poll`](Self::poll) and consumed
    /// (`take()`) inside it. `None` = no lattice noted for this poll → the noise-tolerant compare is
    /// SKIPPED and `prev_luma` is cleared (the fail-safe path: no data → not-dupe → today's exact-hash
    /// behavior, so every one of the pre-round-3 tests that never call `note_frame_luma` is unchanged).
    pending_luma: Option<Vec<u8>>,
    /// (#1145 round 3) The luma lattice noted at the PREVIOUS poll, for the noise-tolerant compare
    /// against the current one. Rotated from `pending_luma` each poll; cleared on a poll with no
    /// pending lattice.
    prev_luma: Option<Vec<u8>>,
    /// (#1145 round 3) Did the PREVIOUS poll classify a NOISY (not byte-identical) content-dupe? The
    /// comparator NEVER classifies two CONSECUTIVE frames as noisy-dupes (an exact byte-identical dupe
    /// is exempt — it is proven): a ~61 fps grabber's surplus is ~1 isolated dupe/s, never a run, so a
    /// run of "dupes" would be a slow content FADE the sparse-diff cannot tell from a still — this cap
    /// hard-bounds even a mis-tuned comparator to shedding at most every other frame and kills the
    /// fade-chaining false-positive class.
    prev_was_noisy_dupe: bool,
    /// (#1167) Pending corrupted-slot make-up deficit: how many GOOD frames a corrupted-buffer drop
    /// (in `src/capture.rs`, before the emit gate) has removed from the stream that the gate still
    /// owes a slot-fill for. Accrued by [`note_corrupted_frame`](Self::note_corrupted_frame),
    /// reclaimed 1:1 in [`poll`](Self::poll) by converting the next slot-skipping
    /// [`ShedAction::Retire`] / [`ShedAction::Drain`] into a copy emit (the nearest good frame), so
    /// an over-rate box with corruption holds the target emit instead of under-running by the
    /// corrupted rate. Bounded by [`CORRUPTED_MAKEUP_MAX_DEFICIT`]. `0` = nothing owed.
    corrupted_makeup_deficit: u64,
    /// (#1167) Set by [`note_corrupted_frame`](Self::note_corrupted_frame): the NEXT inter-capture
    /// takt sample spans the dropped (corrupted) frame, so it is a GAP, not a takt change — exclude
    /// it from the over-rate arming EMA ([`note_capture_takt`](Self::note_capture_takt)), mirroring
    /// the #1145 v3 delivery-hiccup handling so a corrupted drop cannot flip `sustained_over_rate`
    /// off. Consumed (cleared) on the next takt fold.
    pending_takt_gap: bool,
}

impl DecimationGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// The pacing boundary state AFTER the most recent [`poll`](Self::poll) call — main.rs reads
    /// this before/after each poll to feed the pre-existing `#707`
    /// [`crate::genlock_pacing::boundary_skip_count`] diagnostic, unchanged by this ticket.
    pub fn next_boundary_ns(&self) -> u64 {
        self.next_boundary_ns
    }

    /// (#1145 v2.1) How many EXTRA boundary intervals the MOST RECENT [`poll`](Self::poll) advanced
    /// beyond a normal single step — the INTENTIONAL stale-boundary retirement of a fast-drain (`1`
    /// when the +2 fired, else `0`). main.rs DEDUCTS this from the `#707`
    /// [`crate::genlock_pacing::boundary_skip_count`] before recording, so an intentional fast-drain
    /// is never miscounted as an un-emitted-content boundary SKIP (the sick-leg / clock-step signal
    /// `leg-health-guard.sh` hard-fails on). Read it right after `poll`, alongside `next_boundary_ns`.
    pub fn last_poll_intentional_extra_advance(&self) -> u64 {
        self.last_poll_fast_drain_extra
    }

    /// (#1145 round 3) Stage this frame's luma signature lattice ([`dupe_content_sig`]'s second
    /// element) for the NEXT [`poll`](Self::poll) to consume — call it immediately BEFORE `poll`,
    /// mirroring how `content_hash` is computed for the same frame. `poll` compares it to the
    /// previous frame's lattice with the noise-tolerant [`frames_are_content_dupes`] test to catch a
    /// marginal over-rate card's noisy re-sample dupes (which the exact `content_hash` misses). NOT
    /// calling it (any pre-round-3 caller) leaves the pending lattice `None`, so the noisy path is
    /// skipped and behavior is byte-identical to the exact-hash-only gate — the self-neutralizing
    /// fail-safe. Takes the lattice BY VALUE ([`dupe_content_sig`] returns a fresh `Vec`), so the
    /// per-frame capture loop moves it in with no extra copy.
    pub fn note_frame_luma(&mut self, luma: Vec<u8>) {
        self.pending_luma = Some(luma);
    }

    /// (#1167) Register that ONE captured buffer was dropped for content corruption
    /// (`V4L2_BUF_FLAG_ERROR` / a short buffer) in `src/capture.rs::process_frame` BEFORE it could
    /// reach [`poll`](Self::poll). At an over-rate that removes a would-be-emitted GOOD frame from
    /// the stream, so its 60 fps slot would otherwise be absorbed by the over-rate shed machinery
    /// (a [`ShedAction::Retire`] / [`ShedAction::Drain`] that advances the boundary but emits
    /// nothing) → the emit rate falls below target by exactly the corrupted rate (a strih genlock
    /// FIFO hold → the cam1 align sawtooth). This accrues a bounded make-up DEFICIT
    /// ([`CORRUPTED_MAKEUP_MAX_DEFICIT`]) that [`poll`](Self::poll) reclaims 1:1 on the next
    /// slot-skipping shed by emitting the nearest good frame as a copy instead. Corrupted CONTENT
    /// is never forwarded — the make-up emits a subsequent GOOD frame, never the corrupted one.
    /// Call it (from `main.rs`) exactly once per corrupted-buffer drop, only while genlock
    /// decimation is active. The corrupted-spanning inter-capture takt sample is also marked as a
    /// GAP so it is excluded from the over-rate arming EMA (mirrors the #1145 v3 hiccup handling —
    /// keeps `sustained_over_rate` armed through the drop).
    pub fn note_corrupted_frame(&mut self) {
        self.corrupted_makeup_deficit =
            (self.corrupted_makeup_deficit + 1).min(CORRUPTED_MAKEUP_MAX_DEFICIT);
        // (#1167) the next inter-capture takt sample spans this dropped frame — mark it a GAP so it
        // is excluded from the over-rate arming EMA (mirrors the #1145 v3 hiccup handling).
        self.pending_takt_gap = true;
    }

    /// (#1167) The pending corrupted-slot make-up deficit — how many corrupted-induced slots the
    /// gate still owes a fill for (see [`note_corrupted_frame`](Self::note_corrupted_frame)). `0`
    /// when nothing is owed. Diagnostic/read-only, exercised by the #1167 tests.
    pub fn corrupted_makeup_deficit(&self) -> u64 {
        self.corrupted_makeup_deficit
    }

    /// (#1145) Prune the trailing unique-capture window to entries within [`UNIQUE_RATE_WINDOW_NS`]
    /// of `now_ns`. Called on EVERY poll (dupe or unique) so the COUNT is honest at every instant —
    /// a dupe read must NOT see a stale-high count (that is the #1145-review 🔴 frozen-source
    /// blackout), and the honest count is what makes the tight over-rate-vs-starved separation
    /// predictable. A COUNT over the window (not an interval EMA) reads the true unique RATE
    /// regardless of per-frame jitter or dupe clustering.
    fn prune_unique_window(&mut self, now_ns: u64) {
        let cutoff = now_ns.saturating_sub(UNIQUE_RATE_WINDOW_NS);
        // (#1145 v3) prune BOTH the unique-rate AND the all-captures windows with the SAME cutoff on
        // every poll, so the occupancy ratio is honest at every instant.
        prune_before(&mut self.unique_capture_times, cutoff);
        prune_before(&mut self.all_capture_times, cutoff);
    }

    /// (#1145) Does the trailing window prove the source carries enough DISTINCT content to hold a
    /// steady emit at `interval_ns` after shedding its surplus dupes — the signal that gates
    /// retirement vs the late-dupe copy valve? TWO conditions, both required:
    /// - COUNT: at least [`retire_min_uniques`] uniques in the trailing [`UNIQUE_RATE_WINDOW_NS`]
    ///   (a near-full-target unique rate) — separates a genuine over-rate from a starved sub-target
    ///   source (a 50->60 pulldown), which stays on the copy valve.
    /// - FRESHNESS (#1145 review 🔴): the most recent unique arrived within
    ///   [`RETIRE_UNIQUE_FRESH_BOUND_INTERVALS`] emit intervals of `now`. A genuinely FROZEN source
    ///   (100% content-dupes — a dead painter / wedged upstream) never refreshes the window, so its
    ///   stale COUNT stays high; without the freshness gate retirement would fire forever and
    ///   collapse the emit to ~0 fps (a total BLACKOUT). Freshness makes a freeze fall back to the
    ///   copy valve (a frozen picture on a LIVE stream — the pre-#1145 behavior).
    fn enough_unique_to_hold_target(
        &self,
        now_ns: u64,
        interval_ns: u64,
        sustained_over_rate: bool,
    ) -> bool {
        // FRESHNESS gate (unchanged, #1145 review 🔴): a genuinely FROZEN source (no recent unique —
        // a dead painter / wedged upstream) must fall to the #1111 copy valve, never retire into a
        // ~0 fps BLACKOUT. Checked FIRST so neither floor below can override a stale window.
        let fresh = match self.unique_capture_times.back() {
            Some(&last_unique_ns) => {
                now_ns.saturating_sub(last_unique_ns)
                    <= RETIRE_UNIQUE_FRESH_BOUND_INTERVALS.saturating_mul(interval_ns)
            }
            None => return false,
        };
        if !fresh {
            return false;
        }
        // ABSOLUTE COUNT floor (unchanged, #666-aligned): a near-full-target unique COUNT in the
        // trailing window == an absolute `>= 57`-unique/s guarantee.
        if self.unique_capture_times.len() >= retire_min_uniques(interval_ns) {
            return true;
        }
        // (#1145 v3) OCCUPANCY floor — the GAP-IMMUNE supplement. A capture hiccup transiently
        // depresses the absolute count above (the window spans dead time), forcing a genuine
        // over-rate card onto the copy valve for ~the gap duration (the surplus then exports into
        // the strih FIFO). The `unique / all` ratio is gap-immune. Gated on `sustained_over_rate`
        // (capture rate `> 60.3`) so `unique >= 95% × total` keeps the retired emit `>= 57` (the
        // #666 floor) — an under-rate / starved source never reaches this arm. See
        // [`RETIRE_OCCUPANCY_MIN_PERCENT`].
        if sustained_over_rate {
            let total = self.all_capture_times.len();
            if total >= RETIRE_OCCUPANCY_MIN_SAMPLES
                && (self.unique_capture_times.len() as u64) * 100
                    >= (total as u64) * RETIRE_OCCUPANCY_MIN_PERCENT
            {
                return true;
            }
        }
        false
    }

    /// (#1145 v2) Fold this frame's MONOTONIC capture instant into the capture-takt EMA (the
    /// inter-capture interval smoothed with [`TAKT_EMA_SHIFT`]). Init-seeded to the first observed
    /// interval so there is no long cold-start. `capture_mono_ns == 0` (the FrameInfo "no real
    /// measurement" sentinel) is skipped — it carries no interval; a non-advancing / backward
    /// monotonic stamp is likewise ignored (a genuine capture clock only moves forward).
    fn note_capture_takt(&mut self, capture_mono_ns: u64) {
        if capture_mono_ns == 0 {
            return;
        }
        // (#1167) a corrupted frame was dropped since the previous good capture (it never reached
        // poll), so THIS inter-capture interval spans the missing sample — a known benign GAP,
        // exactly like a #1145 v3 delivery hiccup. Do NOT fold it (folding it would pull the takt
        // EMA up and risk disarming `sustained_over_rate`); just advance the baseline so the NEXT
        // interval is measured cleanly. A lone dropped frame is not a rate collapse, so the
        // consecutive-gap collapse counter is reset (this is a known miss, not a #1145 v3 F1 gap).
        let pending_takt_gap = core::mem::take(&mut self.pending_takt_gap);
        if self.prev_capture_mono_ns != 0 && capture_mono_ns > self.prev_capture_mono_ns {
            if pending_takt_gap {
                self.consecutive_takt_gaps = 0;
                self.prev_capture_mono_ns = capture_mono_ns;
                return;
            }
            let interval = capture_mono_ns - self.prev_capture_mono_ns;
            // (#1145 v3) gap-excluded fold: a delivery HICCUP (a blocked V4L2 dequeue) produces ONE
            // huge inter-capture interval that is NOT a takt change — fold it and it poisons the
            // ~256-frame EMA, flipping `sustained_over_rate` off for ~7-12 s and disarming every
            // over-rate drain (the #1145 v3 residual). A genuine rate change shows in EVERY sample;
            // a one-off gap in ONE, so SKIP a lone outlier (never folded), but still advance
            // `prev_capture_mono_ns` below so the NEXT interval is measured cleanly. See
            // [`TAKT_GAP_EXCLUDE_NS`].
            if interval <= TAKT_GAP_EXCLUDE_NS {
                self.consecutive_takt_gaps = 0;
                self.takt_ema_interval_ns = if self.takt_ema_interval_ns == 0 {
                    interval // seed
                } else {
                    let e = self.takt_ema_interval_ns as i128;
                    (e + ((interval as i128 - e) >> TAKT_EMA_SHIFT)) as u64
                };
            } else {
                // (#1145 v3 review 🟡 F1) over-bound sample. A HICCUP is exactly ONE such sample; a
                // GENUINE rate COLLAPSE (a card dropping to < 20 fps — every interval over-bound) must
                // NOT latch `sustained_over_rate` on forever (that would keep the unique-blind
                // depth-Drain arm + the occupancy floor armed for a non-over-rate source). After
                // [`TAKT_GAP_SUSTAINED_COUNT`] CONSECUTIVE over-bound samples it is no longer a lone
                // hiccup — RESET the EMA so `sustained_over_rate` disarms and re-seeds cleanly when the
                // card recovers. K >= 2 fully preserves the one-off-hiccup fix (a hiccup never reaches K).
                self.consecutive_takt_gaps = self.consecutive_takt_gaps.saturating_add(1);
                if self.consecutive_takt_gaps >= TAKT_GAP_SUSTAINED_COUNT {
                    self.takt_ema_interval_ns = 0;
                }
            }
        }
        self.prev_capture_mono_ns = capture_mono_ns;
    }

    /// (#1145 v2) Is the capture takt genuine SUSTAINED over-rate — the gate that enables the
    /// queue-depth drain? True iff the EMA capture interval is below [`RETIRE_MIN_TAKT_INTERVAL_NS`]
    /// (`1e9 / 60.3`). A healthy 60.00 card reads ~16.667 ms (ABOVE → false → the depth-drain is off
    /// and the card is byte-identical to v1, including a #1131 buffered-drain stall-recovery); a
    /// 61.x card reads ~16.3 ms (below → true → drain engages). `0` (not yet seen) → false.
    fn sustained_over_rate(&self) -> bool {
        self.takt_ema_interval_ns != 0 && self.takt_ema_interval_ns < RETIRE_MIN_TAKT_INTERVAL_NS
    }

    /// Feed ONE captured frame (`now_ns` wall-clock capture instant, `content_hash` from
    /// [`dupe_content_hash`], `queue_had_frame` from
    /// [`crate::capture_stall::frame_from_nonempty_queue`] — was this frame already buffered in the
    /// V4L2 queue?) through the pacing + dupe-preference gate. `interval_ns == 0` disables
    /// decimation entirely (mirrors [`crate::genlock_pacing::genlock_emit_gate`]'s own guard) —
    /// always emits, no hashing/state kept. Returns whether THIS captured frame should be
    /// emitted.
    ///
    /// (#1145 v2) `now_mono_ns` (`monotonic_clock_ns()`) and `capture_mono_ns`
    /// (`FrameInfo::capture_monotonic_100ns * 100`, `0` = no measurement) are the MONOTONIC clocks
    /// the queue-depth drain needs: their difference is this frame's queue residence
    /// ([`queue_depth_intervals`]), and consecutive `capture_mono_ns` values feed the capture-takt
    /// EMA ([`note_capture_takt`](Self::note_capture_takt)). Both are monotonic (a duration + an
    /// interval), so they are immune to the DanteSync realtime steps `now_ns` is gridded to. Pass `0`
    /// for both to disable the v2 depth-drain (e.g. a test exercising only the pre-v2 arms).
    pub fn poll(
        &mut self,
        now_ns: u64,
        interval_ns: u64,
        content_hash: u64,
        queue_had_frame: bool,
        now_mono_ns: u64,
        capture_mono_ns: u64,
    ) -> bool {
        if interval_ns == 0 {
            return true;
        }
        // (#1145 v2.1) reset the per-poll intentional-extra-advance accounting; only the FastDrain
        // arm sets it (see the field doc — it keeps a fast-drain out of the #707 skip diagnostic).
        self.last_poll_fast_drain_extra = 0;
        // (#1145 v2) fold the capture takt + measure this frame's monotonic queue residence.
        self.note_capture_takt(capture_mono_ns);
        let sustained_over_rate = self.sustained_over_rate();
        let queue_depth = queue_depth_intervals(now_mono_ns, capture_mono_ns, interval_ns);
        // (#1131) `queue_had_frame` — did THIS captured frame come from a NON-EMPTY V4L2 queue (the
        // driver already had it buffered, per `capture_stall::frame_from_nonempty_queue`)? A buffered
        // frame PROVES a real captured frame exists to fill the next un-emitted boundary, so the gate
        // must catch up one interval and never grid-resync past it (the #1131 multi-slot-skip judder,
        // whose 0-capture-dropped signature confirms the frames exist). A frame from an empty queue
        // (the loop genuinely waited — a device/clock gap) keeps the pre-existing #131 resync.
        let (would_emit, candidate_next) = crate::genlock_pacing::genlock_emit_gate(
            now_ns,
            self.next_boundary_ns,
            interval_ns,
            queue_had_frame,
        );
        // (#1145) numeric boundary staleness: 0 when on-time/surplus (deferring a dupe is
        // lag-neutral) or between boundaries; >= 1 once the crossed boundary is already stale (its
        // downstream hold already happened) so a dupe crossing it can RETIRE it. Shares the boundary
        // math with `genlock_emit_gate` above.
        let lag_intervals = crate::genlock_pacing::genlock_lag_intervals(
            now_ns,
            self.next_boundary_ns,
            interval_ns,
        );

        // (#1145 review 🔵) A BACKWARD DanteSync clock step (#131) leaves the window's pre-step
        // timestamps "in the future" (`> now_ns`), which would block pruning for the step's duration
        // and inflate the count in the aggressive (retire-forcing) direction. Clear the window so a
        // capture after a backward step re-latches from scratch — mirrors `genlock_emit_gate`'s own
        // backward re-latch.
        // (#1145 v3 review 🔵 F3) clear BOTH windows when EITHER holds a future-timestamped entry, so
        // a backward step can never leave `unique_capture_times` populated while `all_capture_times`
        // was cleared (which would read a >100% occupancy ratio from mixed clock epochs). Symmetric
        // by construction.
        let backward_step = self
            .unique_capture_times
            .back()
            .is_some_and(|&back| back > now_ns)
            || self
                .all_capture_times
                .back()
                .is_some_and(|&back| back > now_ns);
        if backward_step {
            self.unique_capture_times.clear();
            self.all_capture_times.clear();
        }

        let exact_dupe = self.prev_hash == Some(content_hash);
        self.prev_hash = Some(content_hash);
        // (#1145 round 3) noise-tolerant content-dupe detection. A marginal jittery over-rate card
        // (CAM2, the painter box) delivers surplus dupes that are noisy optical RE-SAMPLES of the
        // same painted frame — NOT byte-identical repeats like CAM1's steady buffer-repeat — so the
        // exact hash above reads `is_dupe=false` and the dupe is EMITTED as a "unique" (a held
        // painted-id downstream = Δ1), leaving the over-rate unabsorbed so a later frame is
        // force-shed (a skipped painted-id = Δ3): the balanced Δ1/Δ3 aliasing churn the #1142
        // uniformity gate REDs. Compare this frame's luma lattice to the previous one with the
        // noise-tolerant test. ARMED only under `sustained_over_rate` (a healthy 60.00 card never
        // consults it → byte-identical to the exact-hash-only gate) and NEVER on two consecutive
        // frames (a run would be a content fade, not an isolated grabber dupe). No staged lattice
        // (any pre-round-3 caller) → clear prev + not-dupe (fail-safe = today's exact behavior).
        let this_luma = self.pending_luma.take();
        let noisy_dupe = !exact_dupe
            && sustained_over_rate
            && !self.prev_was_noisy_dupe
            && match (self.prev_luma.as_deref(), this_luma.as_deref()) {
                (Some(prev), Some(now)) => frames_are_content_dupes(prev, now),
                _ => false,
            };
        self.prev_luma = this_luma;
        self.prev_was_noisy_dupe = noisy_dupe;
        // (#1145 round 3 [green]) fold the noise-tolerant result into `is_dupe`. Exact FIRST (a
        // byte-identical dupe is proven regardless of the lattice — CAM1 unchanged); the noisy path
        // only ADDS detections under sustained over-rate, so a marginal card's re-sample dupes are
        // now shed as PROVEN dupes (retire / dupe-drain) instead of emitted-as-unique + a
        // compensating shed — killing the Δ1/Δ3 churn. A noisy dupe is also NOT counted below in the
        // unique-rate window (it carries no distinct content), correcting `enough_unique`.
        let is_dupe = exact_dupe || noisy_dupe;

        // (#1145) A unique capture updates the trailing unique-rate window; a dupe carries no new
        // distinct content so it only READS it. `enough_unique` is the robust "source can hold the
        // target without copies" signal (a near-full unique rate AND a recent unique) that separates
        // over-rate retirement from BOTH a genuine starved-source (pulldown — the copy valve holds
        // the grid) and a frozen source (no recent unique — the copy valve holds a frozen picture on
        // a live stream, never a blackout).
        if !is_dupe {
            self.unique_capture_times.push_back(now_ns);
        }
        // (#1145 v3) EVERY capture (dupe or unique) feeds the ALL-captures window — the denominator
        // of the gap-immune occupancy floor in `enough_unique_to_hold_target`.
        self.all_capture_times.push_back(now_ns);
        self.prune_unique_window(now_ns);
        let enough_unique =
            self.enough_unique_to_hold_target(now_ns, interval_ns, sustained_over_rate);

        let action = dupe_shed_action(
            would_emit,
            is_dupe,
            self.deferred_this_boundary,
            lag_intervals,
            enough_unique,
            queue_depth,
            sustained_over_rate,
        );
        // (#1167) corrupted-slot make-up: a corrupted buffer dropped in `src/capture.rs` before the
        // gate removed a would-be-emitted GOOD frame from an over-rate stream, so this otherwise
        // slot-skipping over-rate shed (Retire / Drain — advance the boundary, emit nothing) would
        // leave a hole the strih genlock FIFO holds through (the cam1 align sawtooth). While a
        // make-up is owed, RECLAIM the slot: emit the current GOOD frame (a byte/optical dupe in the
        // Retire case = a repeat of the nearest good frame; a fresh good frame in the Drain case) as
        // a copy and consume one deficit unit — fills the slot with the nearest good frame (issue's
        // invariant: a single-slot dupe is acceptable, a skipped slot never is). Corrupted CONTENT is
        // never forwarded — only a subsequent GOOD frame is emitted. Bounded 1:1 to the corrupted
        // count, so the genuine over-rate latency drain beyond the deficit is untouched. Counted as a
        // #1111 copy (`record_dupe_emitted`) — cross-reference the `corrupted` count on the Streaming
        // line to attribute it. Advances the boundary one interval, exactly as Retire/Drain/Emit do.
        if corrupted_makeup_reclaims(action, self.corrupted_makeup_deficit) {
            self.corrupted_makeup_deficit -= 1;
            self.next_boundary_ns = candidate_next;
            self.deferred_this_boundary = false;
            self.shed_log.record_dupe_emitted();
            return true;
        }
        match action {
            ShedAction::BlindShed => {
                // The ORIGINAL blind pacing drop (between boundaries) -- boundary unchanged
                // (candidate_next == the pending boundary here), deferral state untouched.
                self.next_boundary_ns = candidate_next;
                self.shed_log.record_shed(false);
                false
            }
            ShedAction::Defer => {
                // #889 on-time deferral: shed the dupe, keep the SAME boundary pending -- the next
                // captured frame is re-evaluated against it (bounded to one deferral).
                self.deferred_this_boundary = true;
                self.shed_log.record_shed(true);
                false
            }
            ShedAction::Retire => {
                // (#1145) stale-boundary retirement: shed the dupe AND advance the already-stale
                // boundary one interval, emitting nothing. No copy, no unique sacrificed; drains the
                // dupe-driven lag so it never reaches the #707 resync.
                self.next_boundary_ns = candidate_next;
                self.deferred_this_boundary = false;
                self.shed_log.record_retired();
                false
            }
            ShedAction::Drain => {
                // (#1145 v2) queue-depth drain: shed the OLDEST (this) frame AND advance the
                // boundary one interval, emitting nothing — the sustained-over-rate absorption. The
                // next-oldest buffered frame re-evaluates against the advanced boundary, so the
                // over-rate is drained one frame per over-rate frame (continuous), holding the queue
                // residence at the target instead of accumulating into a V4L2-overflow burst.
                self.next_boundary_ns = candidate_next;
                self.deferred_this_boundary = false;
                self.shed_log.record_drained();
                false
            }
            ShedAction::FastDrain => {
                // (#1145 v2.1) deep-backlog accelerated drain: shed the dupe AND advance TWO
                // intervals when the EXTRA boundary is also already stale (never into the future —
                // `candidate_next` is boundary+1; the extra one is boundary+2, still <= now when the
                // lag is deep). Retires two stale boundaries per dropped dupe -> the delivery-lag
                // backlog converges ~2x faster; drops no EXTRA frame (only the dupe), so no unique is
                // dropped and the emit grid never overshoots wall-clock. Falls back to a single-slot
                // advance if the +2 would overshoot (a defensive guard; unreachable in the deep band).
                let fast_next = if candidate_next.saturating_add(interval_ns) <= now_ns {
                    candidate_next + interval_ns
                } else {
                    candidate_next
                };
                // (#1145 v2.1 review 🟡) record the INTENTIONAL extra boundary advance (1 when the +2
                // fired, 0 on the fallback) so main.rs can deduct it from the #707 boundary-skip
                // diagnostic — an intentional stale-boundary retirement must NOT read as the sick-leg
                // / clock-step SKIP that `leg-health-guard.sh` hard-fails on.
                self.last_poll_fast_drain_extra =
                    (fast_next.saturating_sub(candidate_next)) / interval_ns;
                self.next_boundary_ns = fast_next;
                self.deferred_this_boundary = false;
                self.shed_log.record_fast_drained();
                false
            }
            ShedAction::Emit { copy } => {
                self.next_boundary_ns = candidate_next;
                self.deferred_this_boundary = false;
                if copy {
                    // (#1111) The late-dupe valve fired: a content-dupe was EMITTED (a copy). Under
                    // v2 this now means genuine starvation (the trailing unique rate cannot hold 60
                    // -- a sub-60 source padded by duplication) or the bounded double-dupe guard, NOT
                    // the routine over-rate case (those dupes retire). Count it so a live box shows
                    // the valve engaging only for a genuine deficit.
                    self.shed_log.record_dupe_emitted();
                }
                true
            }
        }
    }

    /// Drain the accumulated `(dupe_shed, blind_shed, dupe_emitted, retired, drained, fast_drained)`
    /// counters for the periodic INFO log — see [`DupeShedLog::take`].
    pub fn take_shed_counts(&mut self) -> (u64, u64, u64, u64, u64, u64) {
        self.shed_log.take()
    }
}

// ── (#889) mechanism-visibility log (comprehensive-logging) ──────────────────

/// Per-run accumulator proving the mechanism is live on a real box: counts how many captured
/// frames were shed because they were the preferred-dupe victim vs the pre-fix blind pacing
/// drop, PLUS (#1111) how many content-dupes were EMITTED as the late-dupe release valve (a copy
/// passed downstream), drained on the SAME 5s Streaming-report cadence as
/// [`crate::emit_skip_log::EmitGateSkipLog`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DupeShedLog {
    dupe_shed: u64,
    blind_shed: u64,
    dupe_emitted: u64,
    retired: u64,
    drained: u64,
    fast_drained: u64,
}

impl DupeShedLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record ONE captured frame that was shed (never emitted) this poll: `dupe` when it was
    /// preferred as a content-duplicate victim (the #889 on-time deferral), otherwise the ORIGINAL
    /// blind pacing drop (between boundaries).
    pub fn record_shed(&mut self, dupe: bool) {
        if dupe {
            self.dupe_shed = self.dupe_shed.saturating_add(1);
        } else {
            self.blind_shed = self.blind_shed.saturating_add(1);
        }
    }

    /// (#1111) Record ONE content-dupe that was EMITTED (a copy) rather than shed — the late-dupe
    /// valve keeping the emit grid boundary-locked at a genuine deficit. See [`DecimationGate::poll`].
    pub fn record_dupe_emitted(&mut self) {
        self.dupe_emitted = self.dupe_emitted.saturating_add(1);
    }

    /// (#1145) Record ONE over-rate content-dupe that was RETIRED (shed while advancing the
    /// already-stale boundary) — the mechanism that absorbs the over-rate takt without emitting a
    /// copy. See [`DecimationGate::poll`].
    pub fn record_retired(&mut self) {
        self.retired = self.retired.saturating_add(1);
    }

    /// (#1145 v2) Record ONE frame SHED by the queue-depth drain (shed the oldest while advancing the
    /// boundary, under sustained over-rate) — the mechanism that absorbs the over-rate takt
    /// CONTINUOUSLY, keeping the delivery-latency sawtooth flat. See [`DecimationGate::poll`].
    pub fn record_drained(&mut self) {
        self.drained = self.drained.saturating_add(1);
    }

    /// (#1145 v2.1) Record ONE deep-backlog FAST-drain (a content-dupe shed while advancing TWO
    /// stale boundaries, under sustained over-rate at lag > `RETIRE_MAX_LAG_INTERVALS`) — the
    /// mechanism that converges a deep delivery-latency backlog in single-digit seconds instead of
    /// the send-slack-limited ~35 s. See [`DecimationGate::poll`] / [`ShedAction::FastDrain`].
    pub fn record_fast_drained(&mut self) {
        self.fast_drained = self.fast_drained.saturating_add(1);
    }

    /// Drain the accumulated `(dupe_shed, blind_shed, dupe_emitted, retired, drained, fast_drained)`
    /// counts and RESET.
    pub fn take(&mut self) -> (u64, u64, u64, u64, u64, u64) {
        let out = (
            self.dupe_shed,
            self.blind_shed,
            self.dupe_emitted,
            self.retired,
            self.drained,
            self.fast_drained,
        );
        self.dupe_shed = 0;
        self.blind_shed = 0;
        self.dupe_emitted = 0;
        self.retired = 0;
        self.drained = 0;
        self.fast_drained = 0;
        out
    }
}

/// The periodic INFO line proving the mechanism is live: printed on every 5s Streaming-report
/// window (while genlock decimation is active) so a live box shows the mechanism working —
/// never suppressed on an all-zero window (a healthy card legitimately shows 0/0, which is the
/// self-neutralizing behavior by design, not the mechanism being off).
pub fn dupe_shed_summary(
    dupe_shed: u64,
    blind_shed: u64,
    dupe_emitted: u64,
    retired: u64,
    drained: u64,
    fast_drained: u64,
    window_secs: u64,
) -> String {
    format!(
        "(#889) dupe-preferring decimation: {dupe_shed} dupe-victim shed / {blind_shed} \
         blind-pacing shed / {dupe_emitted} late-dupe copies emitted (#1111 grid-lock valve) / \
         {retired} boundaries retired (#1145 over-rate absorption) / {drained} depth-drained \
         (#1145 v2 over-rate absorption) / {fast_drained} fast-drained (#1145 v2.1 deep-backlog \
         convergence) over the last ~{window_secs}s"
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

    // ── dupe_shed_action ───────────────────────────────────────────────────

    #[test]
    fn between_boundaries_blind_sheds_regardless_of_dupe() {
        // would_emit == false: lag/enough are irrelevant between boundaries -> BlindShed.
        assert_eq!(
            dupe_shed_action(false, false, false, 0, true, 0, false),
            ShedAction::BlindShed
        );
        assert_eq!(
            dupe_shed_action(false, true, false, 0, false, 0, false),
            ShedAction::BlindShed
        );
    }

    #[test]
    fn fresh_on_time_dupe_at_boundary_is_deferred_not_emitted() {
        // An ON-TIME (lag == 0, surplus-regime) fresh dupe is the case #889 defers — a replacement
        // capture still lands inside the same interval, so the deferral is lag-neutral. Independent
        // of the unique-rate signal (deferral neither emits nor advances).
        assert_eq!(
            dupe_shed_action(true, true, false, 0, true, 0, false),
            ShedAction::Defer
        );
        assert_eq!(
            dupe_shed_action(true, true, false, 0, false, 0, false),
            ShedAction::Defer
        );
    }

    #[test]
    fn already_deferred_on_time_dupe_falls_back_to_copy() {
        // A SECOND consecutive dupe for the SAME boundary (lag == 0, already deferred once) emits as
        // a copy — bounded to one deferral (validated dupes are isolated pairs, never triples).
        assert_eq!(
            dupe_shed_action(true, true, true, 0, true, 0, false),
            ShedAction::Emit { copy: true }
        );
    }

    #[test]
    fn late_over_rate_dupe_is_retired_not_emitted_as_a_copy_1145() {
        // (#1145) A LATE fresh dupe (lag >= 1 — the crossed boundary is already stale) at a genuine
        // over-rate (`enough_unique_to_hold_target`) is RETIRED: shed the dupe AND advance the
        // stale boundary, emitting nothing. This is the fix — the pre-fix valve emitted a copy here
        // (the strih 15fps-judder), retirement drains the lag at no downstream cost.
        for lag in 1..=RETIRE_MAX_LAG_INTERVALS {
            assert_eq!(
                dupe_shed_action(true, true, false, lag, true, 0, false),
                ShedAction::Retire,
                "lag={lag}"
            );
        }
    }

    #[test]
    fn late_dupe_without_enough_unique_is_emitted_as_a_copy_1145() {
        // (#1145) The late-dupe copy valve is now restricted to GENUINE STARVATION: when the source
        // does NOT carry enough distinct content to hold 60 (`!enough_unique` — a sub-60 source
        // padded by duplication, a 50->60 pulldown), a late dupe EMITS a copy exactly as before, so
        // the emit grid stays boundary-locked at 60 and the recording keeps the content-dupes the
        // duplication-masked pulldown detector reads.
        for lag in 1..=(RETIRE_MAX_LAG_INTERVALS + 3) {
            assert_eq!(
                dupe_shed_action(true, true, false, lag, false, 0, false),
                ShedAction::Emit { copy: true },
                "lag={lag}"
            );
        }
    }

    #[test]
    fn retirement_stops_above_the_lag_ceiling_even_with_enough_unique_1145() {
        // Past RETIRE_MAX_LAG_INTERVALS WITHOUT a sustained over-rate (the 7th arg = false), a
        // genuine sustained deficit is building; the copy valve fires (the panic floor) rather than
        // the ordinary +1 retirement, so a non-over-rate lag can never creep toward the #707 resync
        // bound. (At a SUSTAINED over-rate the deep band instead FastDrains — see
        // `over_rate_deep_grid_backlog_converges_in_single_digit_seconds_1145`; that is the #1145
        // v2.1 accelerated convergence, gated on over-rate so this non-over-rate panic floor is
        // unchanged.)
        assert_eq!(
            dupe_shed_action(true, true, false, RETIRE_MAX_LAG_INTERVALS, true, 0, false),
            ShedAction::Retire
        );
        assert_eq!(
            dupe_shed_action(
                true,
                true,
                false,
                RETIRE_MAX_LAG_INTERVALS + 1,
                true,
                0,
                false
            ),
            ShedAction::Emit { copy: true }
        );
    }

    #[test]
    fn fast_drain_band_decision_pins_1145() {
        // (#1145 v2.1) Positive + negative unit pins of the deep-backlog FastDrain band, so a
        // band-boundary regression fails with a one-line decision assertion instead of an opaque
        // convergence-time message (review 🔵). All at a SUSTAINED over-rate (7th arg = true).
        //
        // At/below the ceiling stays the ordinary +1 Retire (below 2x-target byte-identical):
        assert_eq!(
            dupe_shed_action(true, true, false, RETIRE_MAX_LAG_INTERVALS, true, 0, true),
            ShedAction::Retire,
            "lag == ceiling under over-rate is still the +1 Retire, not FastDrain"
        );
        // ABOVE the ceiling with enough distinct content -> the +2 FastDrain (the deep-backlog band):
        assert_eq!(
            dupe_shed_action(
                true,
                true,
                false,
                RETIRE_MAX_LAG_INTERVALS + 1,
                true,
                0,
                true
            ),
            ShedAction::FastDrain,
            "a deep over-rate backlog with enough unique fast-drains"
        );

        // (#1145 v2.1 review 🟡) The frozen/starved (#1052/#365) protection of the NEW band: a
        // SUSTAINED-OVER-RATE source that does NOT carry enough distinct content (a ShadowCast
        // capturing 61.x of a FROZEN picture — the realistic frozen case IS over-rate) must NOT
        // fast-drain; it stays on the #1111 copy valve so the emit grid holds a frozen PICTURE on a
        // live stream instead of blacking out. This is the exact combination the v1 review flagged
        // 🔴 and demanded be pinned; without the `enough_unique_to_hold_target` gate this assertion
        // fails.
        for lag in (RETIRE_MAX_LAG_INTERVALS + 1)..=(RETIRE_MAX_LAG_INTERVALS + 4) {
            assert_eq!(
                dupe_shed_action(true, true, false, lag, false, 0, true),
                ShedAction::Emit { copy: true },
                "frozen/starved over-rate at deep lag={lag} must emit a copy, never fast-drain"
            );
        }
    }

    #[test]
    fn non_dupe_at_boundary_emits_unchanged() {
        // A genuine unique tick always emits (copy: false), regardless of lag / deferral / unique
        // flags — retirement and the copy valve only ever act on content-dupes.
        for (deferred, lag, enough) in [
            (false, 0u64, true),
            (true, 0, true),
            (false, 3, false),
            (false, 9, true),
        ] {
            assert_eq!(
                dupe_shed_action(true, false, deferred, lag, enough, 0, false),
                ShedAction::Emit { copy: false },
                "deferred={deferred} lag={lag} enough={enough}"
            );
        }
    }

    // ── DupeShedLog ────────────────────────────────────────────────────────

    #[test]
    fn shed_log_counts_and_resets_on_take() {
        let mut log = DupeShedLog::new();
        log.record_shed(true);
        log.record_shed(true);
        log.record_shed(false);
        log.record_dupe_emitted();
        log.record_retired();
        log.record_retired();
        log.record_retired();
        log.record_drained();
        log.record_drained();
        log.record_fast_drained();
        log.record_fast_drained();
        log.record_fast_drained();
        log.record_fast_drained();
        assert_eq!(log.take(), (2, 1, 1, 3, 2, 4));
        assert_eq!(log.take(), (0, 0, 0, 0, 0, 0), "take() must reset");
    }

    #[test]
    fn summary_names_all_counts_and_the_ticket_tags() {
        // (#1145 review 🔵) Distinctive multi-digit counts that do NOT appear as substrings of the
        // ticket tags (889/1111/1145) or each other, so each assertion actually pins its own count
        // rather than being satisfied by a digit from a ticket number.
        let s = dupe_shed_summary(41, 23, 67, 94, 58, 72, 36);
        assert!(s.contains("#889"));
        assert!(s.contains("#1111"), "names the late-dupe copy valve");
        assert!(
            s.contains("#1145"),
            "names the retirement + depth-drain mechanisms"
        );
        assert!(s.contains("41"), "names the dupe-victim shed count");
        assert!(s.contains("23"), "names the blind-pacing shed count");
        assert!(s.contains("67"), "names the emitted-copy count");
        assert!(s.contains("94"), "names the retired-boundaries count");
        assert!(s.contains("58"), "names the depth-drained count (#1145 v2)");
        assert!(
            s.contains("72"),
            "names the fast-drained count (#1145 v2.1)"
        );
        assert!(
            s.contains("depth-drained"),
            "names the v2 depth-drain mechanism"
        );
        assert!(
            s.contains("fast-drained"),
            "names the v2.1 fast-drain mechanism"
        );
        assert!(s.contains("~36s"), "names the window seconds");
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
        // REAL 60fps genlock emit boundary math (`genlock_pacing::genlock_emit_gate`, unchanged by this
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
            if gate.poll(*now_ns, emit_interval_ns, *content_id, false, 0, 0) {
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

    // ── (#1111/#1145) over-60 excess-dupe grabber: no SKIPPED-boundary jumps, no unique dropped ─

    /// (#1111 lineage, behavior updated by #1145) A GENKI ShadowCast 2 grabber delivering ~62 fps
    /// with a byte-identical internal-buffer dupe ~every 15 captures — an EXCESS-dupe pattern whose
    /// UNIQUE rate is genuinely sub-target (62 - 62/15 = ~57.9 unique fps, NOT the rig's true-60).
    /// Before #1111 every #889 dupe DEFERRAL ratcheted the lag until it tripped the #707 resync
    /// (~9-boundary leaps, `#707 SKIPPED boundaries` WARN, strih genlock-FIFO relock). #1111 stopped
    /// the resync; it then EMITTED the late dupes as ~2 copies/s to hold a steady 60.
    ///
    /// #1145 SUPERSEDES the "hold 60 via copies" behavior for THIS input: 57.9 unique fps is a
    /// genuine sub-target deficit but sits ABOVE the #666 emit-deficit floor (57 fps), so v2 RETIRES
    /// the surplus dupes and emits the HONEST ~57.9 fps (all unique, zero copies) rather than
    /// fabricating copies — the strih FIFO absorbs the gentle, EVENLY-SPREAD 2.1 fps underrun exactly
    /// as it would the 2.1 copies/s (same downstream visual), and there is no lag leap to relock it.
    /// The LOAD-BEARING guarantees are unchanged and still asserted: ZERO #707 skips and NOT ONE
    /// unique tick dropped. (A source BELOW 57 unique fps — a real 50->60 pulldown — still gets the
    /// copy valve; see `starved_source_still_emits_copies_to_hold_60_not_retired_1145`.)
    #[test]
    fn over_rate_excess_dupe_input_stays_boundary_locked_without_skips_1145() {
        // ~8 s of the validated ShadowCast pattern: 62 fps captured, an isolated dupe every 15th.
        let seconds = 8usize;
        let captures = synthetic_889_capture_sequence(62.0, 62 * seconds, 15);
        let emit_interval_ns = 1_000_000_000u64 / 60;

        // Excludes the ~2 s unique-rate-window warm-up (before retirement engages a dupe emits a
        // copy) from the steady-state copy assertion, mirroring the #889/#1145 tests' WARMUP note.
        const WARMUP_NS: u64 = 3_000_000_000;
        let mut gate = DecimationGate::new();
        let mut emitted: Vec<(u64, u64)> = Vec::new();
        let mut total_skips: u64 = 0;
        for (now_ns, content_id, _is_dupe) in &captures {
            // EXACT src/main.rs wiring: snapshot the boundary, poll, then measure the #707 skip.
            let prev_boundary_ns = gate.next_boundary_ns();
            let emit = gate.poll(*now_ns, emit_interval_ns, *content_id, false, 0, 0);
            let next_boundary_ns = gate.next_boundary_ns();
            total_skips += crate::genlock_pacing::boundary_skip_count(
                prev_boundary_ns,
                next_boundary_ns,
                emit_interval_ns,
            );
            if emit {
                emitted.push((*now_ns, *content_id));
            }
        }
        let (_dupe_shed, _blind_shed, _dupe_emitted, retired, _drained, _fast_drained) =
            gate.take_shed_counts();
        let emitted_ids: Vec<u64> = emitted.iter().map(|(_, id)| *id).collect();

        // (1) LOAD-BEARING (#1111): a 62 fps over-rate + frequent dupes must NOT trip the #707 resync
        // — the boundary grid never leaps. Before #1111 this is ~18 (two ~9-interval leaps over 8 s).
        assert_eq!(
            total_skips, 0,
            "over-60 capture must stay boundary-locked (zero #707 SKIPPED boundaries); got \
             {total_skips} skipped interval(s) — the #889 dupe-deferral lag ratchet is back"
        );

        // (2) #1145: the surplus dupes are RETIRED (not emitted as copies), so the emitted rate is
        // the HONEST unique rate ~57.9 (all distinct). Past the warm-up the emitted stream carries
        // ZERO content-duplicates (retirement, not the copy valve).
        let steady_copies = emitted
            .iter()
            .filter(|(now_ns, _)| *now_ns >= WARMUP_NS)
            .map(|(_, id)| *id)
            .collect::<Vec<u64>>()
            .windows(2)
            .filter(|w| w[0] == w[1])
            .count();
        assert_eq!(
            steady_copies, 0,
            "no content-copy may be emitted at a 57.9-unique source above the #666 floor once the \
             window has filled; got {steady_copies} steady-state copies (should all be retired)"
        );
        assert!(
            retired > 0,
            "the surplus dupes must be retired; retired {retired}"
        );
        let emit_rate = emitted_ids.len() as f64 / seconds as f64;
        assert!(
            (57.0..=59.0).contains(&emit_rate),
            "emitted rate must trend to the honest ~57.9 unique fps (retired surplus dupes); got \
             {emit_rate:.2} fps ({} emitted over {seconds}s)",
            emitted_ids.len()
        );

        // (3) LOAD-BEARING: not one unique tick is dropped (a dropped unique = a genlock-FIFO gap).
        // Retirement sheds only the grabber's OWN dupes, never a genuine unique frame. Skip the
        // cold-start warm-up (the opening capture or two are blind-decimated by simulation-start
        // phase, unrelated to this fix — same WARMUP note as the #889 test above).
        const WARMUP_CAPTURES: usize = 4;
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
            "no unique tick may be dropped by the over-60 gate; missing ids: {missing:?}"
        );
    }

    /// (#1111) Regression guard for the acceptance clause "behavior for an exact-60.0 input
    /// (cam3/cam4 ezcap/NZXT) byte-identical". A perfectly-60.0, dupe-free capture stream never
    /// engages the dupe-preference path at all, so the `on_time` gate added for #1111 is inert:
    /// every capture but the cold-start one emits, zero boundaries are skipped, and zero dupe
    /// victims are shed — identical with or without the fix.
    #[test]
    fn exact_60_input_is_boundary_locked_and_never_defers_1111() {
        // dupe_period 10_000 (> the 480 captures) => a dupe-free exact-60.0 stream.
        let captures = synthetic_889_capture_sequence(60.0, 480, 10_000);
        let emit_interval_ns = 1_000_000_000u64 / 60;

        let mut gate = DecimationGate::new();
        let mut emits = 0usize;
        let mut total_skips = 0u64;
        for (now_ns, content_id, is_dupe) in &captures {
            assert!(!is_dupe, "exact-60 fixture must be dupe-free");
            let prev = gate.next_boundary_ns();
            if gate.poll(*now_ns, emit_interval_ns, *content_id, false, 0, 0) {
                emits += 1;
            }
            total_skips += crate::genlock_pacing::boundary_skip_count(
                prev,
                gate.next_boundary_ns(),
                emit_interval_ns,
            );
        }
        let (dupe_shed, _blind_shed, _dupe_emitted, retired, _drained, _fast_drained) =
            gate.take_shed_counts();

        assert_eq!(total_skips, 0, "exact-60 input never skips a boundary");
        assert_eq!(
            dupe_shed, 0,
            "exact-60 dupe-free input never sheds a dupe victim"
        );
        assert_eq!(
            retired, 0,
            "exact-60 dupe-free input never retires a boundary (#1145 acts only on dupes)"
        );
        assert_eq!(
            emits, 479,
            "exact-60 emits every capture but the cold-start one (480 captures -> 479 emitted)"
        );
    }

    /// (#1131) The sick-grabber judder, end-to-end through the production `DecimationGate::poll`
    /// wiring. The emit poll is blocked for ~10 emit-intervals (a send/processing hiccup on a sick
    /// box); the V4L2 driver buffers the real captured frames meanwhile (0 capture-dropped — the
    /// live symptom's signature), and on resume the loop drains them back-to-back at ~the same wall
    /// clock, each flagged `queue_had_frame = true` (they returned from a non-empty queue). Every
    /// drained frame must EMIT and `boundary_skip_count` must stay 0 — vs the queue-blind gate,
    /// which emits 1 and skips ~9 (`#707 SKIPPED ... 9 boundary interval(s)`). RED before the
    /// `!queue_had_frame` resync guard, GREEN after.
    #[test]
    fn buffered_drain_after_a_stall_emits_every_frame_zero_skip_1131() {
        let emit_interval_ns = 1_000_000_000u64 / 60;
        let mut gate = DecimationGate::new();

        // Warm the gate to a latched boundary with one on-time capture, then read where it sits.
        let start = 1_000_000_000u64;
        let _ = gate.poll(start, emit_interval_ns, 1, false, 0, 0);
        let boundary = gate.next_boundary_ns();
        assert!(boundary > 0, "gate latched a boundary");

        // Block ~10 intervals, then drain 6 buffered frames (unique content) at ~the same wall
        // clock, all from a NON-EMPTY queue (queue_had_frame = true).
        let block = 10u64;
        let buffered = 6u64;
        let resume = boundary + block * emit_interval_ns;
        let mut emitted = 0u64;
        let mut total_skips = 0u64;
        for k in 0..buffered {
            let now = resume + k;
            let content_id = 100 + k; // all unique — a real captured burst, not dupes
            let prev = gate.next_boundary_ns();
            if gate.poll(now, emit_interval_ns, content_id, true, 0, 0) {
                emitted += 1;
            }
            total_skips += crate::genlock_pacing::boundary_skip_count(
                prev,
                gate.next_boundary_ns(),
                emit_interval_ns,
            );
        }
        assert_eq!(
            emitted, buffered,
            "every buffered captured frame must emit through DecimationGate::poll, not be \
             leaped-past and discarded — the #1131 judder"
        );
        assert_eq!(
            total_skips, 0,
            "no boundary may be SKIPPED while buffered captured frames are available (#1131)"
        );
    }

    // ── (#1145) over-rate cadence: stale dupes retired, not emitted as content-copies ──────────

    /// (#1145) A deterministic over-rate-with-jitter capture stream reproducing the live
    /// ShadowCast cam1/cam2 pattern: a true-60 Hz source captured at `takt_fps` (isolated
    /// content-dupes at the over-rate delta — the grabber repeats its internal buffer once per
    /// surplus slot), with each capture's PROCESSING timestamp carrying `jitter_frac` of
    /// pseudo-random scheduling jitter (a seeded LCG, so the whole sequence is reproducible off
    /// rig). Returns `(now_ns, content_id)` in capture order; a dupe repeats the previous
    /// `content_id` (content-dupeness is a hash property, INDEPENDENT of the jittered timestamp —
    /// which is why the stream carries both a periodic dupe pattern AND independent timing jitter).
    fn synthetic_over_rate_with_jitter(
        takt_fps: f64,
        jitter_frac: f64,
        seed: u64,
        seconds: usize,
    ) -> Vec<(u64, u64)> {
        let over_rate = takt_fps - 60.0;
        let dupe_period = if over_rate > 0.01 {
            (takt_fps / over_rate).round() as usize
        } else {
            10_000_000
        };
        let count = (takt_fps * seconds as f64) as usize;
        let nominal_interval_ns = 1_000_000_000.0 / takt_fps;
        let mut lcg: u64 = seed;
        let mut now: f64 = 0.0;
        let (mut next_id, mut prev_id): (u64, u64) = (0, 0);
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            lcg = lcg
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let unit = ((lcg >> 11) as f64) / ((1u64 << 53) as f64); // [0, 1)
            let jitter_ns = (unit * 2.0 - 1.0) * jitter_frac * nominal_interval_ns;
            if i > 0 {
                now += nominal_interval_ns + jitter_ns;
            }
            let is_dupe = i > 0 && dupe_period > 0 && i % dupe_period == dupe_period - 1;
            let content_id = if is_dupe {
                prev_id
            } else {
                let id = next_id;
                next_id += 1;
                id
            };
            prev_id = content_id;
            out.push((now.max(0.0) as u64, content_id));
        }
        out
    }

    #[test]
    fn over_rate_stale_dupes_retired_not_emitted_as_content_copies_1145() {
        // (#1145) RED before the fix / GREEN after. cam1/cam2's ShadowCast drifts its capture takt
        // to ~61.3 fps against a true-60 Hz source; realistic V4L2 dequeue timestamp jitter
        // routinely pushes an isolated on-time deferral over the boundary hair-trigger, so the next
        // content-dupe arrives LATE and the pre-existing late-dupe valve EMITS it as a
        // content-duplicate (a delta-0 repeat downstream) — the exact copy that, paired with the
        // compensating dropped-unique, presents as the strih 15fps-judder the presentation-cadence
        // uniformity gate REDs. v2 retires every stale over-rate dupe (shed it AND advance the
        // already-stale boundary) instead, so the emitted 60 fps stream carries NO content-copy.
        //
        // Summed across several seeds, and past a warm-up window (the unique-rate window fills over
        // ~2 s before retirement can engage), the emitted stream must contain ZERO
        // consecutive-identical content ids. RED: the current valve emits 6-11 copies per seed.
        // GREEN: zero.
        let emit_interval_ns = 1_000_000_000u64 / 60;
        const WARMUP_NS: u64 = 4_000_000_000; // exclude the ~2 s unique-rate-window fill + margin
        let mut total_copies_after_warmup = 0usize;
        let mut total_retired = 0u64;
        for seed in [1u64, 7, 3, 42, 99] {
            let captures = synthetic_over_rate_with_jitter(61.3, 0.20, seed, 20);
            let mut gate = DecimationGate::new();
            let mut emitted: Vec<(u64, u64)> = Vec::new();
            for (now_ns, content_id) in &captures {
                if gate.poll(*now_ns, emit_interval_ns, *content_id, false, 0, 0) {
                    emitted.push((*now_ns, *content_id));
                }
            }
            let (_dupe_shed, _blind_shed, _dupe_emitted, retired, _drained, _fast_drained) =
                gate.take_shed_counts();
            total_retired += retired;
            let post: Vec<u64> = emitted
                .iter()
                .filter(|(now_ns, _)| *now_ns >= WARMUP_NS)
                .map(|(_, id)| *id)
                .collect();
            total_copies_after_warmup += post.windows(2).filter(|w| w[0] == w[1]).count();
        }
        assert_eq!(
            total_copies_after_warmup, 0,
            "over-rate stale dupes must be retired, not emitted as content-duplicates (the copies \
             that present as the strih 15fps-judder); got {total_copies_after_warmup} emitted \
             content-copies across 5 seeds"
        );
        // The mechanism actually engaged (not merely a no-op): stale over-rate dupes were retired.
        assert!(
            total_retired > 0,
            "retirement must engage at over-rate (retired {total_retired} boundaries across 5 seeds)"
        );
    }

    #[test]
    fn over_rate_retirement_holds_60_without_skips_1145() {
        // (#1145) At the rig over-rate (unique rate == 60), retiring every stale dupe keeps the
        // emitted rate at ~60 (all unique) with ZERO #707 boundary skips — no lag ratchet, no
        // resync leap, and no unique tick dropped in steady state.
        let emit_interval_ns = 1_000_000_000u64 / 60;
        let seconds = 20usize;
        let captures = synthetic_over_rate_with_jitter(61.3, 0.20, 1, seconds);
        let mut gate = DecimationGate::new();
        let mut emitted = 0usize;
        let mut total_skips = 0u64;
        for (now_ns, content_id) in &captures {
            let prev = gate.next_boundary_ns();
            if gate.poll(*now_ns, emit_interval_ns, *content_id, false, 0, 0) {
                emitted += 1;
            }
            total_skips += crate::genlock_pacing::boundary_skip_count(
                prev,
                gate.next_boundary_ns(),
                emit_interval_ns,
            );
        }
        assert_eq!(
            total_skips, 0,
            "retirement must never trip the #707 resync at a genuine over-rate; got {total_skips} \
             skipped boundary interval(s)"
        );
        let emit_rate = emitted as f64 / seconds as f64;
        assert!(
            (59.0..=60.5).contains(&emit_rate),
            "emitted rate must hold ~60 (all unique) at over-rate; got {emit_rate:.2} fps"
        );
    }

    #[test]
    fn starved_source_still_emits_copies_to_hold_60_not_retired_1145() {
        // (#1145) A GENUINELY STARVED source — a 50 Hz source padded to a 60 fps capture by
        // DUPLICATION (a 5:6 pulldown: an exact content-dupe every 6th capture, unique rate ~50 <
        // the 59-fps retire floor) — must NOT be retired: retiring would silently drop the emit to
        // 50 fps (a strih-FIFO underrun) and STRIP the content-dupes the duplication-masked pulldown
        // detector reads. Retirement stays OFF; the late-dupe copy valve holds the emit grid at 60
        // and leaves the dupes in the stream, byte-identical to the pre-#1145 behavior.
        let emit_interval_ns = 1_000_000_000u64 / 60;
        let capture_interval_ns = 1_000_000_000u64 / 60; // padded 60 fps capture
        let seconds = 20u64;
        let count = 60 * seconds;
        let mut gate = DecimationGate::new();
        let (mut next_id, mut prev_id): (u64, u64) = (0, 0);
        let mut emitted = 0usize;
        for i in 0..count {
            let now_ns = i * capture_interval_ns;
            let is_dupe = i > 0 && i % 6 == 5; // 5:6 pulldown
            let content_id = if is_dupe {
                prev_id
            } else {
                let id = next_id;
                next_id += 1;
                id
            };
            prev_id = content_id;
            if gate.poll(now_ns, emit_interval_ns, content_id, false, 0, 0) {
                emitted += 1;
            }
        }
        let (_dupe_shed, _blind_shed, dupe_emitted, retired, _drained, _fast_drained) =
            gate.take_shed_counts();
        assert_eq!(
            retired, 0,
            "a starved (sub-60-unique) source must NEVER be retired — that would drop the emit rate \
             and blind the pulldown detector; retired {retired}"
        );
        assert!(
            dupe_emitted > 0,
            "the late-dupe copy valve must stay engaged for a starved source (it holds the emit grid \
             at 60 and keeps the content-dupes in the recording); dupe_emitted {dupe_emitted}"
        );
        let emit_rate = emitted as f64 / seconds as f64;
        assert!(
            (59.0..=60.5).contains(&emit_rate),
            "a starved source must still emit a steady ~60 (via copies), not silently drop; got \
             {emit_rate:.2} fps"
        );
    }

    #[test]
    fn frozen_source_falls_back_to_copies_never_a_blackout_1145() {
        // (#1145 review 🔴) A genuinely FROZEN source (100% byte-identical captures — a dead painter
        // / wedged upstream feeding a still) must NOT collapse the emit: without the freshness gate,
        // the stale unique-rate window keeps `enough_unique` TRUE forever, so every frozen dupe
        // RETIRES (advancing the boundary without emitting) and the NDI emit falls to ~0 fps (a total
        // BLACKOUT — strictly worse than a frozen picture on a broadcast rig). The freshness gate
        // makes a freeze fall back to the late-dupe copy valve within a few intervals, holding a
        // steady ~60 fps of copies (a frozen PICTURE on a LIVE, FIFO-fed stream — the pre-#1145
        // behavior). RED before the freshness gate (emit ~0.2 fps), GREEN after.
        let emit_interval_ns = 1_000_000_000u64 / 60;
        // 5 s of a healthy over-rate stream (retirement engages), then 5 s frozen (all one hash).
        let captures = synthetic_over_rate_with_jitter(61.3, 0.20, 1, 5);
        let mut gate = DecimationGate::new();
        // drive the healthy warm-up so retirement is fully engaged before the freeze.
        let mut last_now = 0u64;
        for (now_ns, content_id) in &captures {
            let _ = gate.poll(*now_ns, emit_interval_ns, *content_id, false, 0, 0);
            last_now = *now_ns;
        }
        let _ = gate.take_shed_counts(); // reset counters; measure only the frozen span
                                         // now freeze for 5 s: same hash every capture at the ~61.3 fps takt.
        let cap_interval_ns = (1_000_000_000.0f64 / 61.3) as u64;
        let frozen_hash = 999_999_999u64;
        let frozen_captures = (61.3 * 5.0) as u64;
        let mut frozen_emitted = 0usize;
        for i in 1..=frozen_captures {
            let now_ns = last_now + i * cap_interval_ns;
            if gate.poll(now_ns, emit_interval_ns, frozen_hash, false, 0, 0) {
                frozen_emitted += 1;
            }
        }
        let (_dupe_shed, _blind_shed, dupe_emitted, _retired, _drained, _fast_drained) =
            gate.take_shed_counts();
        let frozen_emit_rate = frozen_emitted as f64 / 5.0;
        assert!(
            frozen_emit_rate >= 55.0,
            "a frozen source must keep emitting a steady ~60 fps of copies (a frozen picture on a \
             live stream), NEVER collapse to a blackout; got {frozen_emit_rate:.2} fps emitted over \
             the frozen span"
        );
        assert!(
            dupe_emitted > 0,
            "the frozen span must fall back to the late-dupe copy valve; dupe_emitted {dupe_emitted}"
        );
    }

    // ── (#1145 v2) queue-depth drain ───────────────────────────────────────

    #[test]
    fn queue_depth_intervals_guards_and_math_1145() {
        let i = 1_000_000_000u64 / 60; // ~16.667 ms
                                       // interval 0 (genlock off) -> 0
        assert_eq!(queue_depth_intervals(10 * i, 5 * i, 0), 0);
        // capture_mono 0 (no measurement sentinel) -> 0
        assert_eq!(queue_depth_intervals(10 * i, 0, i), 0);
        // now <= capture (non-advancing / bogus monotonic) -> 0
        assert_eq!(queue_depth_intervals(5 * i, 5 * i, i), 0);
        assert_eq!(queue_depth_intervals(4 * i, 5 * i, i), 0);
        // 2.5 intervals of residence -> 2 (whole intervals)
        assert_eq!(queue_depth_intervals(1000 + 5 * i / 2, 1000, i), 2);
        // a garbage-huge residence is clamped to the sane max (never a runaway shed)
        assert_eq!(
            queue_depth_intervals(1_000_000 * i, 1, i),
            QUEUE_DEPTH_SANE_MAX_INTERVALS
        );
    }

    #[test]
    fn depth_drain_is_a_distinct_shed_action_1145() {
        assert_ne!(ShedAction::Drain, ShedAction::Retire);
        assert_ne!(ShedAction::Drain, ShedAction::BlindShed);
        assert_ne!(ShedAction::Drain, ShedAction::Emit { copy: false });
    }

    #[test]
    fn depth_drain_only_fires_under_sustained_over_rate_1145() {
        // NOT over-rate: even a deep queue never drains — a healthy 60.00 card (and a #1131
        // buffered-drain stall-recovery on one) is byte-identical to v1. A unique at depth 3 emits.
        assert_eq!(
            dupe_shed_action(true, false, false, 0, true, 3, false),
            ShedAction::Emit { copy: false },
            "not over-rate -> no depth drain, even at depth 3"
        );
        // OVER-RATE + residence >= QUEUE_DEPTH_SHED_INTERVALS: shed the OLDEST (this) frame
        // regardless of dupeness — the sawtooth-bounding drain (a controlled single-frame drop).
        assert_eq!(
            dupe_shed_action(
                true,
                false,
                false,
                0,
                true,
                QUEUE_DEPTH_SHED_INTERVALS,
                true
            ),
            ShedAction::Drain,
            "over-rate + depth>=target -> drain the oldest (even a non-dupe)"
        );
        // OVER-RATE + a DETECTED dupe at the lower dupe-shed threshold: drain one interval earlier
        // (content-safe — the neighbour carries the same painted frame).
        assert_eq!(
            dupe_shed_action(
                true,
                true,
                false,
                0,
                true,
                QUEUE_DEPTH_DUPE_SHED_INTERVALS,
                true
            ),
            ShedAction::Drain,
            "over-rate + detected dupe at the dupe-shed depth -> drain"
        );
        // OVER-RATE but residence below BOTH thresholds -> falls through to the pre-v2 arms
        // (a unique emits; a fresh on-time dupe defers).
        assert_eq!(
            dupe_shed_action(true, false, false, 0, true, 0, true),
            ShedAction::Emit { copy: false }
        );
        assert_eq!(
            dupe_shed_action(true, true, false, 0, true, 0, true),
            ShedAction::Defer
        );
    }

    // ── (#1145 v2) end-to-end: the delivery-latency sawtooth REPRODUCER + the depth-drain fix ──

    struct QueueSim {
        /// Post-warmup (>8 s) maximum queue RESIDENCE any processed frame reached, in whole emit
        /// intervals — the sawtooth's height. v1 lets this grow toward the V4L2 overflow; v2 bounds it.
        max_residence_post: u64,
        /// Post-warmup V4L2 overflow-drops (queue was full when a capture arrived) — the burst that
        /// shows as judder. v2 pre-empts these with a controlled continuous drain.
        overflow_steady: u64,
        emits: u64,
        drained: u64,
    }

    /// Drive the REAL [`DecimationGate::poll`] with a capture->process queue whose consumer rate
    /// depends on the shed decision (an EMITted frame costs ~one interval — the NDI send; a SHED
    /// frame is cheap), so the loop's max emit rate sits BETWEEN 60.00 and the over-rate. A healthy
    /// 60.00 card keeps up (and recovers from a transient stall); an over-rate card cannot recover,
    /// so its queue residence grows into the sawtooth this ticket fixes. Dupes are NOT byte-detected
    /// (every capture gets a distinct hash — the realistic ShadowCast-noise worst case where the
    /// depth-drain, not the dupe-shed, must carry the absorption). wall == mono in the sim.
    ///
    /// (#1145 v2 review 🔵) Why `send_cost` is just UNDER the interval (max emit ~60.5/s) + a
    /// one-shot stall trigger, NOT `send_cost >= interval`: the field mechanism is that the loop's
    /// max emit rate sits BETWEEN 60.00 and the over-rate — a 60.00 card RECOVERS from a transient
    /// perturbation while an over-rate card CANNOT, so the over-rate residence only ever grows after
    /// a perturbation and then never drains. A `send_cost >= interval` model would make even the
    /// 60.00 card unable to keep up, destroying the constraint-c separation this harness must show.
    /// The stall is the realistic trigger (a CPU/#752 hiccup); the over-rate is what sustains it.
    fn run_queue_sim(capture_fps: f64, stall_at_frame: u64, secs: f64) -> QueueSim {
        let cap_int = (1e9 / capture_fps) as u64;
        let src_int = 1_000_000_000u64 / 60;
        let emit_int = 1_000_000_000u64 / 60;
        let send_cost = emit_int * 991 / 1000; // ~16.5 ms -> max emit ~60.5/s (between 60.0 and 61.x)
        let shed_cost = 1_000_000u64; // 1 ms (hash only)
        let stall_extra = emit_int * 6; // one deterministic CPU hiccup
        const MAXQ: usize = 4; // V4L2 buffers (capture.rs: Stream::with_buffers(.., 4))
        const WARMUP_NS: u64 = 8_000_000_000; // ignore the first 8 s (takt EMA warmup + stall settle)
        let n = (capture_fps * secs) as u64;

        let mut gate = DecimationGate::new();
        let mut queue: VecDeque<(u64, u64)> = VecDeque::new();
        let mut next_cap = 0u64;
        let mut wall = 0u64;
        let (mut max_residence_post, mut overflow_steady, mut emits) = (0u64, 0u64, 0u64);

        loop {
            // admit all captures that have arrived by the loop's wall clock (drop if the queue is full)
            while next_cap < n {
                let cap_ns = next_cap * cap_int;
                if cap_ns > wall {
                    break;
                }
                let src_id = cap_ns / src_int;
                if queue.len() >= MAXQ {
                    if cap_ns > WARMUP_NS {
                        overflow_steady += 1;
                    }
                } else {
                    queue.push_back((cap_ns, src_id));
                }
                next_cap += 1;
            }
            if queue.is_empty() {
                if next_cap >= n {
                    break;
                }
                wall = next_cap * cap_int; // the loop waits for the next capture
                continue;
            }
            let (cap_ns, src_id) = queue.pop_front().unwrap();
            let now = wall;
            let queue_had_frame = now.saturating_sub(cap_ns) < cap_int / 2;
            let residence = now.saturating_sub(cap_ns) / emit_int;
            if now > WARMUP_NS {
                max_residence_post = max_residence_post.max(residence);
            }
            // a distinct hash per capture -> is_dupe is always false (dupes NOT byte-detected)
            let content_hash = src_id.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(now);
            let emit = gate.poll(now, emit_int, content_hash, queue_had_frame, now, cap_ns);
            let mut cost = shed_cost;
            if emit {
                cost = send_cost;
                if next_cap == stall_at_frame {
                    cost += stall_extra;
                }
                emits += 1;
            }
            wall += cost;
            if next_cap >= n && queue.is_empty() {
                break;
            }
        }
        let (_d, _b, _e, _r, drained, _fast_drained) = gate.take_shed_counts();
        QueueSim {
            max_residence_post,
            overflow_steady,
            emits,
            drained,
        }
    }

    #[test]
    fn over_rate_queue_depth_drain_bounds_the_sawtooth_1145() {
        // The fix: at a sustained over-rate (cam1 ShadowCast ~61.5 fps vs a 60 fps source) the
        // queue-depth drain absorbs the surplus CONTINUOUSLY, so the delivery-latency sawtooth stays
        // bounded (residence <= QUEUE_DEPTH_SHED_INTERVALS) and the V4L2 buffer never overflow-drops
        // in a burst. Without the drain (the lag-based v1) the residence grows toward the 4-deep
        // overflow and bursts — this assertion is the RED that the drain turns GREEN.
        let s = run_queue_sim(61.5, 120, 30.0);
        assert!(
            s.drained > 0,
            "the depth drain must engage at a sustained over-rate; drained={}",
            s.drained
        );
        assert!(
            s.max_residence_post <= QUEUE_DEPTH_SHED_INTERVALS,
            "over-rate delivery latency must stay bounded at the depth target; \
             max post-warmup residence {} intervals (target {})",
            s.max_residence_post,
            QUEUE_DEPTH_SHED_INTERVALS
        );
        assert_eq!(
            s.overflow_steady, 0,
            "the continuous drain must pre-empt every V4L2 overflow-drop burst; \
             steady overflow-drops {}",
            s.overflow_steady
        );
    }

    #[test]
    fn over_rate_depth_drain_holds_emit_rate_above_the_666_floor_1145() {
        // (#1145 v2 review 🟡) Zero-loss is the project's HARD acceptance bar, so the DRAIN path —
        // which at a genuine over-rate sheds the OLDEST frame regardless of dupeness (the noise-blind
        // oldest-drop) — must never collapse the emit rate below the #666 emit-deficit floor
        // (5% of 60 == 57 fps). This is the drain-path counterpart of the retirement path's own
        // `over_rate_retirement_holds_60_without_skips_1145` guard, closing the review-found asymmetry:
        // it pins the emit rate + bounded residence against a future constant retune.
        for &fps in &[61.5_f64, 62.0] {
            let s = run_queue_sim(fps, 120, 30.0);
            let emit_fps = s.emits as f64 / 30.0;
            assert!(
                emit_fps >= 57.0,
                "the depth drain must hold the emit rate above the #666 floor (57 fps); \
                 got {emit_fps:.2} fps at {fps} capture (drained={})",
                s.drained
            );
            assert!(
                s.max_residence_post <= QUEUE_DEPTH_SHED_INTERVALS,
                "residence must stay bounded at {fps} capture; max {} intervals",
                s.max_residence_post
            );
            assert_eq!(
                s.overflow_steady, 0,
                "no V4L2 overflow-drop burst at {fps} capture; steady overflow {}",
                s.overflow_steady
            );
        }
    }

    #[test]
    fn healthy_60fps_never_depth_drains_even_through_a_stall_1145() {
        // Constraint (c) + #1131: a healthy 60.00 card is NOT over-rate, so the depth drain NEVER
        // fires — even when a transient stall pushes its queue residence past the depth target
        // (a #1131 buffered-drain, which must emit all buffered frames, not shed them). The takt
        // gate keeps v2 provably inert here, so behaviour is byte-identical to v1.
        let s = run_queue_sim(60.0, 120, 30.0);
        assert_eq!(
            s.drained, 0,
            "a 60.00 card must NEVER depth-drain (takt gate off); drained={}",
            s.drained
        );
        // and it still emits a full ~60 fps (no frames sacrificed by v2).
        assert!(
            s.emits >= (60.0 * 30.0 * 0.98) as u64,
            "a healthy card must keep emitting ~60 fps; emits={}",
            s.emits
        );
    }

    // ── (#1145 v2.1) fast-drain: accelerated grid-backlog convergence ─────────────────────────

    /// (#1145 v2.1) Result of [`run_grid_backlog_sim`].
    struct GridBacklogSim {
        /// Wall (monotonic) seconds from the injected backlog until the emit-grid lag returns to
        /// parity (<= 1 interval) — the "time to parity" the ticket's LIVE CONVERGENCE DATA names.
        time_to_parity_s: f64,
        emit_fps: f64,
        /// Fraction of emitted-frame boundary steps that advanced exactly ONE interval (the uniform
        /// 60 fps cadence) DURING and after the accelerated drain.
        uniformity: f64,
        /// (#1145 v2.1) How many times the FastDrain arm engaged over the run — 0 on a healthy 60.00
        /// card and in steady over-rate with no backlog (the byte-identical proof), > 0 when a deep
        /// grid backlog was accelerated.
        fast_drained: u64,
        /// (#1145 v2.1 review 🔵) The emit rate measured ONLY within the drain window
        /// (inject..converged), so a sub-#666-floor dip confined to the ~single-digit-second drain is
        /// not diluted by the steady-state remainder of the run (the full-run `emit_fps` was).
        drain_window_emit_fps: f64,
        /// (#1145 v2.1 review 🟡) Accumulated NET `#707` boundary skips after injection == the count
        /// `main.rs` would feed `emit_skip_log` == `boundary_skip_count` MINUS the intentional
        /// fast-drain extra advance, summed per poll. Must stay 0 (well under `leg-health-guard.sh`'s
        /// sick-leg threshold) so an intentional fast-drain never trips the #707 clock-step alarm.
        net_707_skips_after_inject: u64,
    }

    /// (#1145 v2.1) Drive the REAL [`DecimationGate::poll`] with a send-bound emit loop whose
    /// MONOTONIC capture takt (residence + takt + capture instants — CONTINUOUS) is SEPARATE from
    /// the REALTIME emit-grid clock (`now_ns`, which grids the boundary). A downstream reconnect /
    /// burn-toggle adds a one-time REALTIME forward offset (`backlog` intervals) — the emit grid
    /// falls behind == delivery lag — WITHOUT disrupting the cam-box's monotonic capture takt, so
    /// `sustained_over_rate` stays TRUE and residence stays low (the faithful reconnect scenario the
    /// two-clock split of #1145 v2 makes representable). Measures wall time until the grid lag
    /// returns to parity, the emit rate, and the emitted-cadence uniformity. Dupes are modelled as
    /// isolated content-PAIRS (a dupe repeats the previous content id — the same model as
    /// [`synthetic_over_rate_with_jitter`]). wall == monotonic; realtime == monotonic + offset.
    fn run_grid_backlog_sim(capture_fps: f64, backlog_intervals: u64, secs: f64) -> GridBacklogSim {
        let cap_int = (1e9 / capture_fps) as u64;
        let emit_int = 1_000_000_000u64 / 60;
        let send_cost = emit_int * 995 / 1000; // ~0.5% slack -> unblocked max emit ~60.3/s
        let shed_cost = 1_000_000u64; // 1 ms (hash only)
        const MAXQ: usize = 4;
        const WARMUP_NS: u64 = 6_000_000_000; // establish the takt EMA before injecting the backlog
        let n = (capture_fps * secs) as u64;

        let mut gate = DecimationGate::new();
        let mut queue: VecDeque<u64> = VecDeque::new(); // capture-monotonic instants
        let mut next_cap = 0u64;
        let mut mono = 0u64;
        let mut rt_off: i64 = 0;
        let (mut injected, mut inject_mono, mut converged_at): (bool, u64, Option<u64>) =
            (false, 0, None);
        let mut emits = 0u64;
        let mut emits_in_window = 0u64; // emits during inject..converged (review 🔵 #4)
        let mut net_707_skips = 0u64; // boundary_skip_count - fast-drain extra, after inject (🟡 #1)
        let mut last_emit_bidx: Option<u64> = None;
        let (mut uni_ok, mut uni_tot) = (0u64, 0u64);
        let (mut next_id, mut prev_id): (u64, u64) = (0, 0);

        loop {
            while next_cap < n {
                let cap_ns = next_cap * cap_int;
                if cap_ns > mono {
                    break;
                }
                if queue.len() < MAXQ {
                    queue.push_back(cap_ns);
                }
                next_cap += 1;
            }
            if queue.is_empty() {
                if next_cap >= n {
                    break;
                }
                mono = next_cap * cap_int; // wait for the next capture
                continue;
            }
            if !injected && mono > WARMUP_NS {
                rt_off = (backlog_intervals * emit_int) as i64; // reconnect: grid falls behind
                inject_mono = mono;
                injected = true;
            }
            let cap_ns = queue.pop_front().unwrap();
            let now_mono = mono;
            let now_rt = (mono as i64 + rt_off) as u64;
            let over_rate = capture_fps - 60.0;
            let dupe_period = if over_rate > 0.01 {
                (capture_fps / over_rate).round() as u64
            } else {
                u64::MAX
            };
            let is_dupe = dupe_period != u64::MAX && next_cap % dupe_period == dupe_period - 1;
            let cid = if is_dupe {
                prev_id
            } else {
                let id = next_id;
                next_id += 1;
                id
            };
            prev_id = cid;
            let content_hash = cid.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let prev_boundary = gate.next_boundary_ns();
            // poll: now_ns (boundary / lag) is REALTIME; residence + takt are MONOTONIC.
            let emit = gate.poll(now_rt, emit_int, content_hash, true, now_mono, cap_ns);
            // (#1145 v2.1 review 🟡) exactly what main.rs feeds emit_skip_log: the raw #707 skip
            // MINUS the fast-drain's intentional extra advance. Accumulate after injection.
            if injected && converged_at.is_none() {
                let raw_skip = crate::genlock_pacing::boundary_skip_count(
                    prev_boundary,
                    gate.next_boundary_ns(),
                    emit_int,
                );
                net_707_skips +=
                    raw_skip.saturating_sub(gate.last_poll_intentional_extra_advance());
            }
            let bidx = gate.next_boundary_ns() / emit_int;
            let mut cost = shed_cost;
            if emit {
                cost = send_cost;
                emits += 1;
                if injected && converged_at.is_none() {
                    emits_in_window += 1;
                }
                if let Some(prev) = last_emit_bidx {
                    uni_tot += 1;
                    if bidx.saturating_sub(prev) == 1 {
                        uni_ok += 1;
                    }
                }
                last_emit_bidx = Some(bidx);
            }
            mono += cost;
            if injected && converged_at.is_none() {
                let rt = (mono as i64 + rt_off) as u64;
                let lag = crate::genlock_pacing::genlock_lag_intervals(
                    rt,
                    gate.next_boundary_ns(),
                    emit_int,
                );
                if lag <= 1 {
                    converged_at = Some(mono);
                }
            }
            if next_cap >= n && queue.is_empty() {
                break;
            }
        }
        let (_ds, _bl, _cp, _ret, _drn, fast_drained) = gate.take_shed_counts();
        let time_to_parity_s = converged_at.map_or(f64::NAN, |w| (w - inject_mono) as f64 / 1e9);
        let uniformity = if uni_tot > 0 {
            uni_ok as f64 / uni_tot as f64
        } else {
            1.0
        };
        // (#1145 v2.1 review 🔵 #4) emit rate measured ONLY across the drain window (inject..converged),
        // undiluted by steady state. NaN if it never converged (nothing to bound).
        let drain_window_emit_fps = match converged_at {
            Some(w) if w > inject_mono => emits_in_window as f64 / ((w - inject_mono) as f64 / 1e9),
            _ => f64::NAN,
        };
        GridBacklogSim {
            time_to_parity_s,
            emit_fps: emits as f64 / secs,
            uniformity,
            fast_drained,
            drain_window_emit_fps,
            net_707_skips_after_inject: net_707_skips,
        }
    }

    #[test]
    fn over_rate_deep_grid_backlog_converges_in_single_digit_seconds_1145() {
        // (#1145 v2.1) RED before the fix / GREEN after. The merged v2 retires over-rate dupes only
        // while lag <= RETIRE_MAX_LAG_INTERVALS (4); ABOVE that a late dupe EMITS a copy (no grid
        // advance), so a deep emit-grid backlog (the owner's painter-QR delivery lag, 12+ frames
        // after a reconnect / restart / burn toggle) catches up ONLY via the send-slack — the
        // owner's measured ~0.3 frame/s (~35 s live). The fast-drain RETIRES those deep late dupes
        // and advances TWO stale boundaries per retire, converging the backlog in single-digit
        // seconds.
        //
        // Measured against the REAL poll (send-bound loop, realtime/monotonic split): the current v2
        // takes ~15.3 s for a 24-frame backlog and ~7.3 s for a 12-frame one (RED — over the bounds
        // below); the fast-drain takes ~9.3 s and ~5.3 s (GREEN).
        let deep = run_grid_backlog_sim(61.5, 24, 120.0);
        assert!(
            deep.time_to_parity_s <= 12.0,
            "a 24-frame grid backlog must converge in single-digit-ish seconds at a sustained \
             over-rate; time_to_parity {:.2}s (v2 baseline ~15.3s)",
            deep.time_to_parity_s
        );
        let twelve = run_grid_backlog_sim(61.5, 12, 120.0);
        assert!(
            twelve.time_to_parity_s <= 6.5,
            "a 12-frame grid backlog must converge fast; time_to_parity {:.2}s (v2 baseline ~7.3s)",
            twelve.time_to_parity_s
        );
        // The HARD zero-loss bar holds through the accelerated drain: emit stays above the #666
        // floor (57 fps) and the emitted 60 fps cadence stays uniform (>= 0.95) — the ticket's
        // "without cadence damage".
        for s in [&deep, &twelve] {
            assert!(
                s.emit_fps >= 57.0,
                "emit rate must stay above the #666 floor during the accelerated drain; got {:.2}",
                s.emit_fps
            );
            assert!(
                s.uniformity >= 0.95,
                "emitted cadence uniformity must stay >= 0.95 during the accelerated drain; got {:.3}",
                s.uniformity
            );
            // (#1145 v2.1 review 🔵 #4) the #666 floor holds WITHIN the drain window itself, not just
            // averaged over the whole run (a sub-floor dip confined to the ~single-digit-second drain
            // would otherwise be diluted ~13:1 and pass trivially).
            assert!(
                s.drain_window_emit_fps >= 57.0,
                "emit rate within the drain window must stay above the #666 floor; got {:.2}",
                s.drain_window_emit_fps
            );
            // (#1145 v2.1 review 🟡 #1) an intentional fast-drain must NOT register as a #707
            // un-emitted-content boundary SKIP (the sick-leg / clock-step signal leg-health-guard.sh
            // hard-fails on) — main.rs deducts the fast-drain's extra advance, so the NET count stays 0.
            assert_eq!(
                s.net_707_skips_after_inject, 0,
                "fast-drain must not inflate the #707 boundary-skip diagnostic; net skips {}",
                s.net_707_skips_after_inject
            );
            // the mechanism actually engaged (not merely a no-op faster-by-luck).
            assert!(
                s.fast_drained > 0,
                "the v2.1 fast-drain must engage on a deep over-rate backlog; fast_drained={}",
                s.fast_drained
            );
        }
    }

    #[test]
    fn fast_drain_never_engages_on_a_healthy_60fps_card_1145() {
        // Constraint: a healthy 60.00 card is NOT over-rate, so the fast-drain NEVER fires even when
        // a reconnect leaves it with the SAME deep grid backlog — the takt gate keeps v2.1 provably
        // inert, so behaviour is byte-identical to v2. (The 60.00 card's own slow slack-only
        // convergence is a separate, pre-existing issue, not this ticket's over-rate scope.)
        let s = run_grid_backlog_sim(60.0, 24, 120.0);
        assert_eq!(
            s.fast_drained, 0,
            "a 60.00 card must NEVER fast-drain (takt gate off); fast_drained={}",
            s.fast_drained
        );
    }

    #[test]
    fn fast_drain_is_inert_in_steady_over_rate_without_a_backlog_1145() {
        // Constraint: below the 2x-target grid lag (steady over-rate, NO injected backlog -> lag
        // stays ~0), the fast-drain never fires -> byte-identical to v2. `backlog_intervals = 0`
        // exercises exactly that (an over-rate card with no reconnect event).
        let s = run_grid_backlog_sim(61.5, 0, 120.0);
        assert_eq!(
            s.fast_drained, 0,
            "steady over-rate with no deep backlog must NEVER fast-drain; fast_drained={}",
            s.fast_drained
        );
    }

    // ── (#1145 round 3) noise-tolerant content-dupe detection ──────────────────

    /// Tiny deterministic LCG for repeatable per-capture "sensor noise" in the round-3 sims.
    fn r3_lcg(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *state >> 33
    }

    /// Render a small YUYV422 frame for painted `frame_id` with per-capture noise (`seed`): a static
    /// grey gradient background + a "QR/burn" region whose modules derive from `frame_id` (each id
    /// increment flips ~half its modules — what makes two DIFFERENT painted frames diverge across the
    /// sampled lattice while two SAME-id captures differ only by noise). Y (luma) at even byte
    /// offsets; chroma neutral. `sigma` = ± per-byte luma noise amplitude.
    fn r3_render(
        frame_id: u64,
        seed: u64,
        w: usize,
        h: usize,
        stride: usize,
        sigma: i32,
    ) -> Vec<u8> {
        let mut buf = vec![0u8; stride * h];
        let mut st = seed ^ 0x1234_5678;
        for y in 0..h {
            for x in 0..w {
                let mut yv: i32 = 90 + (y as i32 * 3 % 40);
                if (4..28).contains(&y) && (4..60).contains(&x) {
                    // per-module bit from a splitmix avalanche of (id, y, x) so each module flips
                    // ~independently (~half per id increment) — a popcount-parity model flips ALL
                    // modules together or NONE (parity is position-independent), a degenerate "QR".
                    let mut z = frame_id ^ ((y as u64) << 20) ^ ((x as u64) << 40);
                    z = z.wrapping_mul(0x9E37_79B9_7F4A_7C15);
                    z ^= z >> 29;
                    z = z.wrapping_mul(0xBF58_476D_1CE4_E5B9);
                    let bit = (z >> 33) & 1;
                    yv = if bit == 1 { 235 } else { 16 };
                }
                let noise = (r3_lcg(&mut st) % (2 * sigma as u64 + 1)) as i32 - sigma;
                yv = (yv + noise).clamp(0, 255);
                let px = y * stride + x * 2;
                buf[px] = yv as u8;
                buf[px + 1] = 128;
            }
        }
        buf
    }

    struct R3Sim {
        uniformity: f64,
        copies: u64,
        skips: u64,
        emit_fps: f64,
        emitted_ids: Vec<u64>,
    }

    /// Drive the REAL [`DecimationGate::poll`] (send-bound loop, monotonic residence, realtime==
    /// monotonic — no reconnect) with a marginal over-rate card. `byte_identical` = a dupe re-renders
    /// with the SAME noise seed (a CAM1 buffer-repeat → identical bytes → the exact hash catches it);
    /// else a FRESH seed (a CAM2 noisy optical re-sample → distinct hash → only the luma comparator
    /// can catch it). `with_luma` = call [`note_frame_luma`](DecimationGate::note_frame_luma) before
    /// each poll (production wiring) vs not (the legacy path). Measures the emitted painted-id cadence
    /// decimated 60→30 (the `presentation_cadence` uniformity metric) plus the pre-decimation Δ1
    /// copies (a held id) and Δ3 skips (a skipped id).
    fn run_r3_sim(
        capture_fps: f64,
        secs: f64,
        sigma: i32,
        byte_identical: bool,
        with_luma: bool,
    ) -> R3Sim {
        let (w, h, stride) = (64usize, 32usize, 160usize);
        let cap_int = (1e9 / capture_fps) as u64;
        let emit_int = 1_000_000_000u64 / 60;
        let send_cost = emit_int * 995 / 1000; // ~0.5% slack -> unblocked max emit ~60.3/s
        let shed_cost = 1_000_000u64; // 1 ms (hash + compare only)
        const MAXQ: usize = 4;
        let n = (capture_fps * secs) as u64;
        let over_rate = capture_fps - 60.0;
        let dupe_period = if over_rate > 0.01 {
            (capture_fps / over_rate).round() as u64
        } else {
            u64::MAX
        };

        let mut gate = DecimationGate::new();
        let mut queue: VecDeque<(u64, u64, u64, Vec<u8>)> = VecDeque::new(); // (cap_mono, id, hash, luma)
        let mut next_cap = 0u64;
        let mut mono = 0u64;
        let mut jitter = 0xABCDu64;
        let mut next_id = 0u64;
        let mut prev_id = 0u64;
        let mut prev_seed = 0u64;
        let mut emitted_ids: Vec<u64> = Vec::new();
        let mut emits = 0u64;

        loop {
            while next_cap < n {
                let base = next_cap * cap_int;
                let span = (cap_int / 3).max(1);
                let jit = (r3_lcg(&mut jitter) % span) as i64 - (span / 2) as i64;
                let cap_ns = (base as i64 + jit).max(0) as u64;
                if cap_ns > mono {
                    break;
                }
                let is_dupe = dupe_period != u64::MAX && next_cap % dupe_period == dupe_period - 1;
                let (pid, seed) = if is_dupe {
                    let s = if byte_identical {
                        prev_seed
                    } else {
                        next_cap.wrapping_mul(0x9E37).wrapping_add(0x5151)
                    };
                    (prev_id, s)
                } else {
                    let id = next_id;
                    next_id += 1;
                    (id, next_cap.wrapping_mul(0x9E37))
                };
                prev_id = pid;
                prev_seed = seed;
                let frame = r3_render(pid, seed, w, h, stride, sigma);
                let (hash, luma) = dupe_content_sig(&frame, w, h, stride);
                if queue.len() < MAXQ {
                    queue.push_back((cap_ns, pid, hash, luma));
                }
                next_cap += 1;
            }
            if queue.is_empty() {
                if next_cap >= n {
                    break;
                }
                mono = next_cap * cap_int; // wait for the next capture
                continue;
            }
            let (cap_mono, pid, hash, luma) = queue.pop_front().unwrap();
            let now_mono = mono;
            let now_rt = mono; // rig realtime == monotonic (no reconnect offset)
            if with_luma {
                gate.note_frame_luma(luma);
            }
            let emit = gate.poll(now_rt, emit_int, hash, true, now_mono, cap_mono);
            let mut cost = shed_cost;
            if emit {
                cost = send_cost;
                emits += 1;
                emitted_ids.push(pid);
            }
            mono += cost;
            if next_cap >= n && queue.is_empty() {
                break;
            }
        }

        // pre-decimation Δ1 copies (step 0 = held id) / Δ3 skips (step 2 = skipped id).
        let mut copies = 0u64;
        let mut skips = 0u64;
        for pair in emitted_ids.windows(2) {
            match pair[1] as i64 - pair[0] as i64 {
                0 => copies += 1,
                2 => skips += 1,
                _ => {}
            }
        }
        // decimate 60->30 (the downstream recording cadence) — uniformity = frac(step == 2).
        let kept: Vec<u64> = emitted_ids.iter().step_by(2).copied().collect();
        let (mut uni, mut tot) = (0u64, 0u64);
        for pair in kept.windows(2) {
            tot += 1;
            if pair[1] as i64 - pair[0] as i64 == 2 {
                uni += 1;
            }
        }
        R3Sim {
            uniformity: if tot > 0 {
                uni as f64 / tot as f64
            } else {
                1.0
            },
            copies,
            skips,
            emit_fps: emits as f64 / secs,
            emitted_ids,
        }
    }

    #[test]
    fn frames_are_content_dupes_catches_noise_rejects_flip_1145() {
        let (w, h, stride) = (64usize, 32usize, 160usize);
        // two noisy captures of the SAME painted frame -> a content-dupe (noise below theta).
        let (_, la) = dupe_content_sig(&r3_render(100, 1, w, h, stride, 4), w, h, stride);
        let (_, lb) = dupe_content_sig(&r3_render(100, 2, w, h, stride, 4), w, h, stride);
        assert!(
            frames_are_content_dupes(&la, &lb),
            "two noisy captures of the SAME painted frame must be a content-dupe"
        );
        // a DIFFERENT painted frame (QR flip) -> NOT a dupe, even with noise.
        let (_, lc) = dupe_content_sig(&r3_render(101, 3, w, h, stride, 4), w, h, stride);
        assert!(
            !frames_are_content_dupes(&la, &lc),
            "two DIFFERENT painted frames (a QR/burn flip) must NOT be a content-dupe"
        );
        // a global exposure offset on the SAME frame -> still a dupe (the median compensates).
        let bright: Vec<u8> = lb
            .iter()
            .map(|&v| (v as i32 + 20).clamp(0, 255) as u8)
            .collect();
        assert!(
            frames_are_content_dupes(&la, &bright),
            "a uniform exposure offset on the same painted frame must still read as a content-dupe"
        );
        // fail-safe: mismatched / empty lattices are NOT dupes.
        assert!(!frames_are_content_dupes(&[], &[]));
        assert!(!frames_are_content_dupes(&la, &lb[..lb.len() - 1]));
    }

    #[test]
    fn dupe_content_sig_hash_matches_legacy_and_lattice_nonempty_1145() {
        let (w, h, stride) = (64usize, 32usize, 160usize);
        let f = r3_render(7, 9, w, h, stride, 3);
        let (sig_hash, lattice) = dupe_content_sig(&f, w, h, stride);
        assert_eq!(
            sig_hash,
            dupe_content_hash(&f, w, h, stride),
            "dupe_content_sig's hash must be byte-identical to the legacy dupe_content_hash"
        );
        assert!(!lattice.is_empty(), "the luma lattice must be populated");
        assert_eq!(
            dupe_content_sig(&[], 0, 0, 0),
            (0u64, Vec::new()),
            "a degenerate frame must sign to (0, empty)"
        );
    }

    #[test]
    fn marginal_over_rate_noisy_dupes_content_detection_holds_uniformity_1145() {
        // (#1145 round 3) RED before the [green] `is_dupe` wiring / GREEN after. A marginal jittery
        // over-rate card (CAM2, the painter box) whose surplus dupes are NOISY optical re-samples:
        // the exact content_hash misses them, so each emits as a "unique" (a held painted-id = Δ1)
        // and forces a compensating shed (a skipped painted-id = Δ3) — the balanced-pair churn the
        // #1142 uniformity gate REDs (live CAM2 0.93-0.95; the off-rig sim lands ~0.94 at 61.3).
        //
        // BASELINE — the legacy exact-hash-only path (no note_frame_luma) CHURNS. Pins the mechanism
        // and holds in BOTH [red] and [green] (the legacy path never changes).
        let base = run_r3_sim(61.3, 20.0, 4, false, false);
        assert!(
            base.uniformity < 0.95,
            "legacy exact-hash-only path MUST churn on noisy re-samples (mechanism pin); \
             uniformity {:.4}",
            base.uniformity
        );
        assert!(
            base.copies > 0 && base.skips > 0,
            "legacy path's churn MUST carry the balanced Δ1 copies / Δ3 skips pairs; \
             copies={} skips={}",
            base.copies,
            base.skips
        );
        // FIXED — content-compare detection armed via note_frame_luma holds the cadence. FAILS on
        // [red] (poll ignores the lattice), PASSES on [green].
        let fixed = run_r3_sim(61.3, 20.0, 4, false, true);
        assert!(
            fixed.uniformity >= 0.95,
            "content-compare detection MUST hold uniformity >= 0.95 on the marginal noisy card; \
             got {:.4} (baseline {:.4})",
            fixed.uniformity,
            base.uniformity
        );
        assert!(
            fixed.skips * 4 < base.skips.max(1),
            "content-compare detection MUST collapse the compensating unique-skips (Δ3); \
             fixed skips={} vs baseline {}",
            fixed.skips,
            base.skips
        );
        // The over-rate absorption must NOT collapse the emit rate — shedding PROVEN dupes keeps it
        // above the #666 emit-deficit floor (57 fps); the uniformity gate alone catches a rate drop
        // only indirectly, so pin it directly.
        assert!(
            fixed.emit_fps >= 57.0,
            "noise-tolerant detection must hold the emit rate above the #666 floor (57 fps); \
             got {:.2}",
            fixed.emit_fps
        );
    }

    #[test]
    fn cam1_byte_identical_and_healthy_60_unchanged_by_note_frame_luma_1145() {
        // (#1145 round 3) note_frame_luma must NOT change a card the exact hash already handles: a
        // CAM1 byte-identical buffer-repeat (the exact short-circuit fires FIRST) and a healthy 60.00
        // card (never `sustained_over_rate` → the comparator is gated OFF). Drive each WITH and
        // WITHOUT note_frame_luma; the emitted painted-id sequences must be IDENTICAL — the
        // byte-untouched proof, differentially in one binary (the legacy WITHOUT variant IS the
        // pre-round-3 behavior by construction).
        let cam1_with = run_r3_sim(64.0, 20.0, 4, true, true);
        let cam1_without = run_r3_sim(64.0, 20.0, 4, true, false);
        assert_eq!(
            cam1_with.emitted_ids, cam1_without.emitted_ids,
            "CAM1 byte-identical dupes: note_frame_luma must not change the emitted cadence (the \
             exact hash short-circuits first)"
        );
        assert!(
            cam1_with.uniformity >= 0.95,
            "CAM1 byte-identical must decimate cleanly; uniformity {:.4}",
            cam1_with.uniformity
        );
        let h_with = run_r3_sim(60.0, 20.0, 4, false, true);
        let h_without = run_r3_sim(60.0, 20.0, 4, false, false);
        assert_eq!(
            h_with.emitted_ids, h_without.emitted_ids,
            "healthy 60.00: note_frame_luma must be inert (never sustained_over_rate)"
        );
    }

    // ── (#1145 v3) arming-signal robustness through a capture hiccup ──────────

    #[test]
    fn takt_ema_survives_a_capture_gap_1145() {
        // (#1145 v3) RED before the gap-excluded takt fold / GREEN after. The 61.5-fps capture EMA
        // sits at ~16.26ms, the sustained_over_rate threshold (RETIRE_MIN_TAKT_INTERVAL_NS) at
        // ~16.584ms — a 0.32ms margin. A SINGLE dequeue hiccup (a blocked V4L2 dequeue, NOT a takt
        // change) folds one huge sample into the ~256-frame EMA and disarms sustained_over_rate for
        // ~7s (a 500ms gap), during which depth-Drain, FastDrain AND the round-3 noisy-dupe compare
        // are ALL dead → the over-rate surplus leaks into the strih FIFO (the #1145 v3 residual).
        // A genuine takt change shows in EVERY sample; a delivery gap in ONE — so the fold must skip
        // the outlier. RED: current folds it and stays disarmed for hundreds of post-gap frames.
        let cap_int = (1_000_000_000.0f64 / 61.5) as u64; // ~16.26 ms over-rate takt
        let mut gate = DecimationGate::new();
        let mut t = 0u64;
        for _ in 0..800 {
            t += cap_int;
            gate.note_capture_takt(t);
        }
        assert!(
            gate.sustained_over_rate(),
            "a warm 61.5 fps capture EMA must arm sustained_over_rate"
        );
        // ONE 500 ms dequeue hiccup — a blocked dequeue, NOT a rate change.
        t += 500_000_000;
        gate.note_capture_takt(t);
        // then steady over-rate again; sustained_over_rate must SURVIVE (re-arm within a few frames).
        let mut rearmed_within = None;
        for k in 1..=8u64 {
            t += cap_int;
            gate.note_capture_takt(t);
            if gate.sustained_over_rate() {
                rearmed_within = Some(k);
                break;
            }
        }
        assert!(
            rearmed_within.is_some(),
            "sustained_over_rate must survive a single dequeue hiccup (gap-excluded takt fold); it \
             stayed disarmed for >8 post-gap frames — the arming-poisoning residual disarms every \
             over-rate drain for seconds"
        );
    }

    #[test]
    fn takt_ema_disarms_on_a_sustained_collapse_1145() {
        // (#1145 v3 review 🟡 F1) the counterpart to the hiccup test: a SUSTAINED rate COLLAPSE (a
        // card dropping to ~15 fps — EVERY interval over TAKT_GAP_EXCLUDE_NS) must DISARM
        // `sustained_over_rate`, never latch it on forever. B.1's one-off gap-exclude alone would keep
        // skipping every sample and never re-learn (the review-found latch); the consecutive-gap
        // counter RESETS the EMA after TAKT_GAP_SUSTAINED_COUNT so a collapsed (non-over-rate) card
        // stops arming the over-rate drains. RED on the pre-F1 one-sided exclude, GREEN with the counter.
        let cap_int = (1_000_000_000.0f64 / 61.5) as u64;
        let mut gate = DecimationGate::new();
        let mut t = 0u64;
        for _ in 0..800 {
            t += cap_int;
            gate.note_capture_takt(t);
        }
        assert!(
            gate.sustained_over_rate(),
            "a warm 61.5 fps EMA must arm sustained_over_rate"
        );
        // sustained ~15 fps: every interval ~66 ms, all above the 50 ms exclude bound.
        let slow_int = 1_000_000_000u64 / 15;
        let mut disarmed_within = None;
        for k in 1..=8u64 {
            t += slow_int;
            gate.note_capture_takt(t);
            if !gate.sustained_over_rate() {
                disarmed_within = Some(k);
                break;
            }
        }
        assert!(
            disarmed_within.is_some(),
            "a sustained sub-20fps collapse must disarm sustained_over_rate (F1 consecutive-gap \
             reset); it stayed armed for >8 collapsed frames — the one-sided gap-exclude latch"
        );
    }

    /// (#1145 v3) Drive the REAL [`DecimationGate::poll`] through a send-bound over-rate loop with
    /// CAM1-style byte-identical dupes at the over-rate cadence (a true-60 source captured faster),
    /// real monotonic clocks (wall == mono, no reconnect offset), and ONE injected dequeue GAP after
    /// a 10 s warm-up. Returns the copy-valve emissions (a DUPE that EMITTED) in the 8 s window AFTER
    /// the gap — the surplus that, once a hiccup disarms the cam-side drains, leaks downstream into
    /// the strih FIFO. Send-bound: an EMITted frame costs ~one interval (the NDI send), a SHED frame
    /// is cheap, so the loop cannot keep up with the over-rate and the queue rides full.
    fn run_hiccup_copy_export(capture_fps: f64, seed: u64, gap_ns: u64) -> (u64, u64) {
        let ei = 1_000_000_000u64 / 60;
        let ci = (1e9 / capture_fps) as u64;
        let send_cost = ei * 995 / 1000; // ~16.58 ms -> send-bound max emit ~60.3/s
        let shed_cost = 1_000_000u64; // 1 ms (hash only)
        const MAXQ: usize = 4; // V4L2 buffers (capture.rs)
        let warm_ns = 10_000_000_000u64;
        let post_ns = 8_000_000_000u64;
        let over = capture_fps - 60.0;
        let dupe_period = if over > 0.01 {
            (capture_fps / over).round() as u64
        } else {
            u64::MAX
        };
        let mut gate = DecimationGate::new();
        let mut queue: VecDeque<(u64, u64, bool)> = VecDeque::new(); // (cap_mono, hash, is_dupe)
        let (mut next_cap, mut mono, mut jit, mut nid, mut prev_hash) =
            (0u64, 0u64, seed, 0u64, 0u64);
        let mut nc = 0u64;
        let (mut gap_done, mut post_start) = (false, 0u64);
        let end = warm_ns + post_ns + 4_000_000_000;
        let mut post_copies = 0u64;
        let mut post_emits = 0u64;
        loop {
            while next_cap <= mono {
                // inject ONE gap the first time a capture would land at/after the warm mark.
                if !gap_done && next_cap >= warm_ns {
                    next_cap += gap_ns;
                    gap_done = true;
                    post_start = next_cap;
                }
                let cap = next_cap;
                jit = jit
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let span = (ci / 3).max(1);
                let j = ((jit >> 33) % span) as i64 - (span / 2) as i64;
                let cap_j = (cap as i64 + j).max(0) as u64;
                if cap_j > mono {
                    break;
                }
                let is_dupe = dupe_period != u64::MAX && nc % dupe_period == dupe_period - 1;
                let h = if is_dupe {
                    prev_hash
                } else {
                    let x = nid.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
                    nid += 1;
                    x
                };
                prev_hash = h;
                if queue.len() < MAXQ {
                    queue.push_back((cap_j, h, is_dupe));
                }
                nc += 1;
                next_cap += ci;
            }
            if queue.is_empty() {
                if next_cap > end {
                    break;
                }
                mono = next_cap; // the loop waits for the next capture
                continue;
            }
            let (cap, h, is_dupe) = queue.pop_front().unwrap();
            let now = mono;
            if post_start > 0 && now >= post_start + post_ns {
                break; // past the measurement window
            }
            // (#1145 v3 review 🔵 F5) `queue_had_frame=true` on every poll is a harness
            // simplification: the REAL loop would pass `false` for the FIRST post-gap frame (its
            // dequeue genuinely blocked for the gap), letting the #131/#1131 resync clear most of the
            // deep lag. Passing `true` keeps the gate in the deep-lag catch-up regime (the more
            // demanding case for this test); the pinned copy-export outcome holds either way (verified
            // via the scratch route with both variants).
            let emit = gate.poll(now, ei, h, true, now, cap);
            if post_start > 0 && now >= post_start {
                // a DUPE that EMITTED is a copy-valve emission (Emit{copy:true}) — the surplus that
                // leaks downstream when the cam-side drains are disarmed.
                if is_dupe && emit {
                    post_copies += 1;
                }
                if emit {
                    post_emits += 1;
                }
            }
            mono += if emit { send_cost } else { shed_cost };
            if next_cap > end && queue.is_empty() {
                break;
            }
        }
        (post_copies, post_emits)
    }

    #[test]
    fn over_rate_copy_export_survives_a_capture_hiccup_1145() {
        // (#1145 v3) RED before B.1 (gap-excluded takt fold) + B.2 (occupancy-relative unique floor)
        // / GREEN after. A single dequeue hiccup poisons BOTH arming signals (the takt EMA disarms
        // sustained_over_rate; the absolute unique-count floor drops below `retire_min_uniques` for
        // ~the gap duration), so every over-rate dupe hits the late-dupe COPY valve instead of being
        // retired — those copies ride at wire rate into the strih FIFO (the ±5-frame cam1 wobble the
        // qr-align gate REDs). With the fix the drains stay armed through the hiccup and the surplus
        // is retired at SOURCE, so ~ZERO copies are exported. Summed across seeds past the gap.
        let gap_ns = 500_000_000u64; // a 500 ms hiccup
        let mut total_post_copies = 0u64;
        for seed in [1u64, 7, 3, 42, 99] {
            total_post_copies += run_hiccup_copy_export(61.5, seed, gap_ns).0;
        }
        assert!(
            total_post_copies <= 5,
            "a single capture hiccup must NOT disarm the cam-side over-rate drains (B.1+B.2); the \
             surplus must be retired at source, not exported as copy-valve dupes into the strih \
             FIFO. Got {total_post_copies} post-gap copy-valve emissions across 5 seeds (RED: the \
             arming-poisoning residual exports ~10/seed)"
        );
    }

    #[test]
    fn steady_over_rate_no_hiccup_never_over_sheds_1145() {
        // (#1145 v3) Anti-over-shed pin: with NO hiccup, the arming retunes (B.1 gap-excluded fold,
        // B.2 occupancy floor) must be provably INERT — a steady over-rate card is byte-identical to
        // the pre-v3 behaviour (the drains never disarm anyway, so nothing new fires). Two directions
        // (review 🔵 F2): ZERO copy-valve export AND a held emit rate — so a future regression that
        // either starts spuriously shedding OR over-sheds (e.g. a mistaken open-loop credit shedder,
        // or Drain firing every frame) is caught. `gap_ns == 0` = the same harness, no dead time.
        let (mut total_copies, mut min_emit_fps) = (0u64, f64::MAX);
        let post_secs = 8.0; // the harness's post-window length
        for seed in [1u64, 7, 3, 42, 99] {
            let (copies, emits) = run_hiccup_copy_export(61.5, seed, 0);
            total_copies += copies;
            min_emit_fps = min_emit_fps.min(emits as f64 / post_secs);
        }
        assert!(
            total_copies <= 5,
            "a steady over-rate card with NO hiccup must export ~0 copy-valve dupes (the retirement \
             path absorbs the surplus at source); got {total_copies} across 5 seeds"
        );
        assert!(
            min_emit_fps >= 57.0,
            "the arming retunes must NOT over-shed in steady state — the emit rate must stay >= the \
             #666 floor (57 fps); got {min_emit_fps:.2} fps (a catastrophic over-shed reads far below)"
        );
    }

    /// (#1167) Drive the REAL [`DecimationGate::poll`] with a TRUE-60 source over-captured at 62 fps
    /// (2 dupes/s so the unique rate is exactly 60) and inject a corrupted-buffer drop at the live
    /// ~0.8/s rate via [`DecimationGate::note_corrupted_frame`] (a corrupted buffer never reaches
    /// `poll` — `src/capture.rs::process_frame` drops it before the callback). Residence is 0
    /// (`now_mono == capture_mono`) to isolate the over-rate RETIRE path; the takt EMA still arms
    /// `sustained_over_rate`. Returns (emits, corrupted, dupe_emitted).
    ///
    /// The invariant (#1167): while ANY captured frame is buffered, every 60 fps emit slot must be
    /// filled with the nearest good frame — a single-slot dupe is acceptable, a skipped slot never
    /// is. Without the make-up, each corrupted drop removes a would-be-emitted good frame and the
    /// over-rate absorption skips its slot → emit under-runs by exactly the corrupted rate
    /// (measured 59.13 fps == the live "~59.1"). With the make-up, the deficit is reclaimed 1:1 so
    /// emit holds the same ~60 as the no-corruption control.
    fn run_corrupted_over_rate_1167(
        dupe_period: usize,
        corrupt_period: usize,
        secs: f64,
    ) -> (u64, u64, u64) {
        let cap_fps = 62.0f64;
        let cap_int = (1e9 / cap_fps) as u64;
        let emit_int = 1_000_000_000u64 / 60;
        let n = (cap_fps * secs) as usize;
        let mut gate = DecimationGate::new();
        let (mut emits, mut corrupted) = (0u64, 0u64);
        let (mut prev_id, mut next_id) = (0u64, 0u64);
        for i in 0..n {
            let now = i as u64 * cap_int;
            let is_corrupt =
                i > 0 && corrupt_period > 0 && i % corrupt_period == corrupt_period - 1;
            if is_corrupt {
                // Corrupted buffer: dropped before the gate (never polled), exactly as
                // `src/capture.rs::process_frame` does — main.rs then calls note_corrupted_frame.
                gate.note_corrupted_frame();
                corrupted += 1;
                continue;
            }
            let is_dupe = i > 0 && dupe_period > 0 && i % dupe_period == dupe_period - 1;
            let content_id = if is_dupe {
                prev_id
            } else {
                let v = next_id;
                next_id += 1;
                v
            };
            prev_id = content_id;
            let content_hash = content_id.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            // now_mono == capture_mono == now -> residence 0 (isolate the retire path); the takt
            // EMA still reads the 62 fps over-rate and arms `sustained_over_rate`.
            if gate.poll(now, emit_int, content_hash, true, now, now) {
                emits += 1;
            }
        }
        let (_ds, _bl, dupe_emitted, _r, _d, _fd) = gate.take_shed_counts();
        (emits, corrupted, dupe_emitted)
    }

    #[test]
    fn over_rate_plus_corrupted_holds_target_emit_1167() {
        let secs = 30.0;
        // TRUE-60 source over-captured at 62 fps: dupe every 31st -> 2 dupes/s -> unique rate 60.
        let (control_emits, control_corrupt, _c_copies) = run_corrupted_over_rate_1167(31, 0, secs);
        // Same source + a corrupted-buffer drop every 77th capture (~0.8/s, the live cam1 rate).
        let (corrupt_emits, corrupted, makeup_copies) = run_corrupted_over_rate_1167(31, 77, secs);

        assert_eq!(control_corrupt, 0, "control run must inject no corruption");
        let control_fps = control_emits as f64 / secs;
        let corrupt_fps = corrupt_emits as f64 / secs;

        // (1) BASELINE: the over-rate itself holds ~60 with no corruption (the honest 60-unique
        // rate). Passes with or without the fix — it establishes the target the corrupted run
        // must also reach.
        assert!(
            (59.8..=60.05).contains(&control_fps),
            "over-rate control must hold ~60 fps (60-unique source); got {control_fps:.3} fps"
        );

        // (2) THE FIX (#1167): the corrupted run must reclaim EVERY corrupted-induced slot, holding
        // the same ~60 as the control. WITHOUT the make-up the over-rate absorption skips each
        // corrupted slot and this under-runs by exactly the corrupted count (measured 59.13 fps ==
        // the live "~59.1") — the RED this test pins. `corrupted > 0` guards a mis-modelled fixture.
        assert!(
            corrupted > 0,
            "the corrupted run must actually inject corruption"
        );
        assert!(
            control_emits.saturating_sub(corrupt_emits) <= 1,
            "every corrupted-induced slot must be reclaimed (a single-slot dupe is acceptable, a \
             skipped slot never is): control {control_emits} emits vs corrupted {corrupt_emits} \
             emits (deficit {} over {corrupted} corrupted); WITHOUT the make-up the deficit == the \
             corrupted count",
            control_emits.saturating_sub(corrupt_emits)
        );
        assert!(
            corrupt_fps >= 59.8,
            "an over-rate box WITH corruption must still hold ~60 fps emit; got {corrupt_fps:.3} \
             fps ({corrupt_emits} emits, {corrupted} corrupted) — the emit under-runs by the \
             corrupted rate when the corrupted slot is not made up"
        );

        // (3) The make-up fires as copies of the nearest good frame (reusing the #1111 copy
        // counter): a non-zero make-up count is the mechanism proof.
        assert!(
            makeup_copies >= corrupted.saturating_sub(1),
            "the corrupted slots must be made up with copies of the nearest good frame; \
             {makeup_copies} copies emitted for {corrupted} corrupted slots"
        );
    }

    #[test]
    fn no_corruption_is_byte_identical_1167() {
        // The #1167 fields/logic must be INERT with no corruption: the over-rate control emits the
        // same 60-unique rate and emits ZERO make-up copies (the #1111 valve stays at its genuine
        // starvation semantics — 0 here).
        let secs = 30.0;
        let (emits, corrupted, makeup_copies) = run_corrupted_over_rate_1167(31, 0, secs);
        assert_eq!(corrupted, 0);
        assert_eq!(
            makeup_copies, 0,
            "no corruption -> no make-up copy (the #1167 path is inert without note_corrupted_frame)"
        );
        let fps = emits as f64 / secs;
        assert!(
            (59.8..=60.05).contains(&fps),
            "no-corruption over-rate holds ~60 fps; got {fps:.3}"
        );
    }

    #[test]
    fn corrupted_makeup_reclaims_only_slot_skipping_sheds_when_owed_1167() {
        // No deficit -> never reclaim, for EVERY action (byte-identical to today).
        for a in [
            ShedAction::Retire,
            ShedAction::Drain,
            ShedAction::FastDrain,
            ShedAction::Defer,
            ShedAction::BlindShed,
            ShedAction::Emit { copy: false },
            ShedAction::Emit { copy: true },
        ] {
            assert!(
                !corrupted_makeup_reclaims(a, 0),
                "deficit 0 must never reclaim ({a:?})"
            );
        }
        // Deficit owed: reclaim ONLY the slot-skipping over-rate sheds (Retire / Drain).
        assert!(corrupted_makeup_reclaims(ShedAction::Retire, 1));
        assert!(corrupted_makeup_reclaims(ShedAction::Drain, 3));
        // FastDrain (deep-backlog convergence), Defer (boundary held -> slot still filled),
        // BlindShed (between boundaries) and Emit (already fills the slot) are NEVER reclaimed.
        assert!(!corrupted_makeup_reclaims(ShedAction::FastDrain, 3));
        assert!(!corrupted_makeup_reclaims(ShedAction::Defer, 3));
        assert!(!corrupted_makeup_reclaims(ShedAction::BlindShed, 3));
        assert!(!corrupted_makeup_reclaims(
            ShedAction::Emit { copy: false },
            3
        ));
        assert!(!corrupted_makeup_reclaims(
            ShedAction::Emit { copy: true },
            3
        ));
    }

    #[test]
    fn note_corrupted_frame_accrues_a_bounded_deficit_1167() {
        let mut gate = DecimationGate::new();
        assert_eq!(gate.corrupted_makeup_deficit(), 0);
        gate.note_corrupted_frame();
        gate.note_corrupted_frame();
        assert_eq!(gate.corrupted_makeup_deficit(), 2);
        // Bounded by CORRUPTED_MAKEUP_MAX_DEFICIT so a corruption burst cannot force a runaway copy
        // tail after corruption stops.
        for _ in 0..50 {
            gate.note_corrupted_frame();
        }
        assert_eq!(
            gate.corrupted_makeup_deficit(),
            CORRUPTED_MAKEUP_MAX_DEFICIT
        );
    }
}
