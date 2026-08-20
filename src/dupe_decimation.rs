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

// ── (#1145) v2 tuning constants ──────────────────────────────────────────────

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

// ── (#889/#1145) victim-selection decision ────────────────────────────────────

/// (#1145) The per-captured-frame shed/emit decision, one of four actions. `would_emit` is the
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
pub fn dupe_shed_action(
    would_emit: bool,
    is_dupe: bool,
    already_deferred_this_boundary: bool,
    lag_intervals: u64,
    enough_unique_to_hold_target: bool,
) -> ShedAction {
    if !would_emit {
        return ShedAction::BlindShed;
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
    ShedAction::Emit { copy: true }
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
    /// (#1145) Trailing wall-clock timestamps of the recent UNIQUE (non-dupe) captures within
    /// [`UNIQUE_RATE_WINDOW_NS`]. Its length is the measured unique RATE — the robust "enough
    /// distinct content to hold 60 fps" signal that gates retirement vs the starvation copy valve.
    unique_capture_times: VecDeque<u64>,
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

    /// (#1145) Prune the trailing unique-capture window to entries within [`UNIQUE_RATE_WINDOW_NS`]
    /// of `now_ns`. Called on EVERY poll (dupe or unique) so the COUNT is honest at every instant —
    /// a dupe read must NOT see a stale-high count (that is the #1145-review 🔴 frozen-source
    /// blackout), and the honest count is what makes the tight over-rate-vs-starved separation
    /// predictable. A COUNT over the window (not an interval EMA) reads the true unique RATE
    /// regardless of per-frame jitter or dupe clustering.
    fn prune_unique_window(&mut self, now_ns: u64) {
        let cutoff = now_ns.saturating_sub(UNIQUE_RATE_WINDOW_NS);
        while let Some(&front) = self.unique_capture_times.front() {
            if front <= cutoff {
                self.unique_capture_times.pop_front();
            } else {
                break;
            }
        }
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
    fn enough_unique_to_hold_target(&self, now_ns: u64, interval_ns: u64) -> bool {
        if self.unique_capture_times.len() < retire_min_uniques(interval_ns) {
            return false;
        }
        match self.unique_capture_times.back() {
            Some(&last_unique_ns) => {
                now_ns.saturating_sub(last_unique_ns)
                    <= RETIRE_UNIQUE_FRESH_BOUND_INTERVALS.saturating_mul(interval_ns)
            }
            None => false,
        }
    }

    /// Feed ONE captured frame (`now_ns` wall-clock capture instant, `content_hash` from
    /// [`dupe_content_hash`], `queue_had_frame` from
    /// [`crate::capture_stall::frame_from_nonempty_queue`] — was this frame already buffered in the
    /// V4L2 queue?) through the pacing + dupe-preference gate. `interval_ns == 0` disables
    /// decimation entirely (mirrors [`crate::genlock_pacing::genlock_emit_gate`]'s own guard) —
    /// always emits, no hashing/state kept. Returns whether THIS captured frame should be
    /// emitted.
    pub fn poll(
        &mut self,
        now_ns: u64,
        interval_ns: u64,
        content_hash: u64,
        queue_had_frame: bool,
    ) -> bool {
        if interval_ns == 0 {
            return true;
        }
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
        if self
            .unique_capture_times
            .back()
            .is_some_and(|&back| back > now_ns)
        {
            self.unique_capture_times.clear();
        }

        let is_dupe = self.prev_hash == Some(content_hash);
        self.prev_hash = Some(content_hash);

        // (#1145) A unique capture updates the trailing unique-rate window; a dupe carries no new
        // distinct content so it only READS it. `enough_unique` is the robust "source can hold the
        // target without copies" signal (a near-full unique rate AND a recent unique) that separates
        // over-rate retirement from BOTH a genuine starved-source (pulldown — the copy valve holds
        // the grid) and a frozen source (no recent unique — the copy valve holds a frozen picture on
        // a live stream, never a blackout).
        if !is_dupe {
            self.unique_capture_times.push_back(now_ns);
        }
        self.prune_unique_window(now_ns);
        let enough_unique = self.enough_unique_to_hold_target(now_ns, interval_ns);

        match dupe_shed_action(
            would_emit,
            is_dupe,
            self.deferred_this_boundary,
            lag_intervals,
            enough_unique,
        ) {
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

    /// Drain the accumulated `(dupe_shed, blind_shed, dupe_emitted, retired)` counters for the
    /// periodic INFO log — see [`DupeShedLog::take`].
    pub fn take_shed_counts(&mut self) -> (u64, u64, u64, u64) {
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

    /// Drain the accumulated `(dupe_shed, blind_shed, dupe_emitted, retired)` counts and RESET.
    pub fn take(&mut self) -> (u64, u64, u64, u64) {
        let out = (
            self.dupe_shed,
            self.blind_shed,
            self.dupe_emitted,
            self.retired,
        );
        self.dupe_shed = 0;
        self.blind_shed = 0;
        self.dupe_emitted = 0;
        self.retired = 0;
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
    window_secs: u64,
) -> String {
    format!(
        "(#889) dupe-preferring decimation: {dupe_shed} dupe-victim shed / {blind_shed} \
         blind-pacing shed / {dupe_emitted} late-dupe copies emitted (#1111 grid-lock valve) / \
         {retired} boundaries retired (#1145 over-rate absorption) over the last ~{window_secs}s"
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
            dupe_shed_action(false, false, false, 0, true),
            ShedAction::BlindShed
        );
        assert_eq!(
            dupe_shed_action(false, true, false, 0, false),
            ShedAction::BlindShed
        );
    }

    #[test]
    fn fresh_on_time_dupe_at_boundary_is_deferred_not_emitted() {
        // An ON-TIME (lag == 0, surplus-regime) fresh dupe is the case #889 defers — a replacement
        // capture still lands inside the same interval, so the deferral is lag-neutral. Independent
        // of the unique-rate signal (deferral neither emits nor advances).
        assert_eq!(
            dupe_shed_action(true, true, false, 0, true),
            ShedAction::Defer
        );
        assert_eq!(
            dupe_shed_action(true, true, false, 0, false),
            ShedAction::Defer
        );
    }

    #[test]
    fn already_deferred_on_time_dupe_falls_back_to_copy() {
        // A SECOND consecutive dupe for the SAME boundary (lag == 0, already deferred once) emits as
        // a copy — bounded to one deferral (validated dupes are isolated pairs, never triples).
        assert_eq!(
            dupe_shed_action(true, true, true, 0, true),
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
                dupe_shed_action(true, true, false, lag, true),
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
                dupe_shed_action(true, true, false, lag, false),
                ShedAction::Emit { copy: true },
                "lag={lag}"
            );
        }
    }

    #[test]
    fn retirement_stops_above_the_lag_ceiling_even_with_enough_unique_1145() {
        // Past RETIRE_MAX_LAG_INTERVALS a genuine sustained deficit is building; the copy valve
        // fires (the panic floor) rather than retiring further, so the lag can never creep toward
        // the #707 resync bound.
        assert_eq!(
            dupe_shed_action(true, true, false, RETIRE_MAX_LAG_INTERVALS, true),
            ShedAction::Retire
        );
        assert_eq!(
            dupe_shed_action(true, true, false, RETIRE_MAX_LAG_INTERVALS + 1, true),
            ShedAction::Emit { copy: true }
        );
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
                dupe_shed_action(true, false, deferred, lag, enough),
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
        assert_eq!(log.take(), (2, 1, 1, 3));
        assert_eq!(log.take(), (0, 0, 0, 0), "take() must reset");
    }

    #[test]
    fn summary_names_all_counts_and_the_ticket_tags() {
        // (#1145 review 🔵) Distinctive multi-digit counts that do NOT appear as substrings of the
        // ticket tags (889/1111/1145) or each other, so each assertion actually pins its own count
        // rather than being satisfied by a digit from a ticket number.
        let s = dupe_shed_summary(41, 23, 67, 94, 36);
        assert!(s.contains("#889"));
        assert!(s.contains("#1111"), "names the late-dupe copy valve");
        assert!(s.contains("#1145"), "names the retirement mechanism");
        assert!(s.contains("41"), "names the dupe-victim shed count");
        assert!(s.contains("23"), "names the blind-pacing shed count");
        assert!(s.contains("67"), "names the emitted-copy count");
        assert!(s.contains("94"), "names the retired-boundaries count");
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
            if gate.poll(*now_ns, emit_interval_ns, *content_id, false) {
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
            let emit = gate.poll(*now_ns, emit_interval_ns, *content_id, false);
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
        let (_dupe_shed, _blind_shed, _dupe_emitted, retired) = gate.take_shed_counts();
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
            if gate.poll(*now_ns, emit_interval_ns, *content_id, false) {
                emits += 1;
            }
            total_skips += crate::genlock_pacing::boundary_skip_count(
                prev,
                gate.next_boundary_ns(),
                emit_interval_ns,
            );
        }
        let (dupe_shed, _blind_shed, _dupe_emitted, retired) = gate.take_shed_counts();

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
        let _ = gate.poll(start, emit_interval_ns, 1, false);
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
            if gate.poll(now, emit_interval_ns, content_id, true) {
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
                if gate.poll(*now_ns, emit_interval_ns, *content_id, false) {
                    emitted.push((*now_ns, *content_id));
                }
            }
            let (_dupe_shed, _blind_shed, _dupe_emitted, retired) = gate.take_shed_counts();
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
            if gate.poll(*now_ns, emit_interval_ns, *content_id, false) {
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
            if gate.poll(now_ns, emit_interval_ns, content_id, false) {
                emitted += 1;
            }
        }
        let (_dupe_shed, _blind_shed, dupe_emitted, retired) = gate.take_shed_counts();
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
            let _ = gate.poll(*now_ns, emit_interval_ns, *content_id, false);
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
            if gate.poll(now_ns, emit_interval_ns, frozen_hash, false) {
                frozen_emitted += 1;
            }
        }
        let (_dupe_shed, _blind_shed, dupe_emitted, _retired) = gate.take_shed_counts();
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
}
