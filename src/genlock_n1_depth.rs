//! issue 1367 — the N==1 PIN-DERIVED DEPTH. The Tier-0 authority; the C `genlock_n1_*` helpers
//! (obs-source.c) mirror this in lock-step (tests/genlock_relock_selection_parity.rs +
//! tests/genlock_shallow_depth_parity_1367.rs; the unit tests live in genlock_n1_depth_tests.rs).
//!
//! WHY. A deep N==1 input (the stream `NDI 2ME PGM`, pin 987) has TWO absorbing depths. A strih OBS
//! restart empties the FIFO, the release keeps its locked boundary, GAP-RESYNCs onto the first
//! post-restart frame at `ceil(pin / interval)` frames (30), and then presents each startup-stall
//! DUPLICATE stamp one tick later on the STEADY path (+1 frame each). The #859 drain only fires
//! above `ceil + 2` (32), so 30 / 31 / 32 were all absorbing: live, four restarts landed on 32, 31,
//! 32, 31 frames — a whole-frame (~33 ms) A/V step per restart at a constant pin.
//!
//! THE RULE. Settle to `target = base + 1` frames, where `base = ceil((pin − 1 µs) / interval)`
//! is the resync depth and `+1` is the depth the healthy sender gap/dup tail produces anyway (a
//! gap+dup pair at `base` deepens by one; at `base + 1` or deeper it is neutral). Deeper than the
//! target → shed ONE frame ([`n1_shed_due`]); shallower → HOLD one tick ([`should_hold_n1_phase`]).
//! Both share the #859 drain throttle, and together they leave a one-frame dead-band around the
//! target.
//!
//! What carries the design, found in the issue-1355 bench (`genlock_grid_bench.rs`):
//! - DEPTH IS THE PRESENTED AGE AT THE TICK'S SCHEDULED INSTANT, never the queue length and never
//!   the processing wall. A render tick that runs late already holds the NEXT frame, so the queue
//!   reads one frame deep at the correct state (lowering the drain hysteresis instead churned 10
//!   sheds/h, 17–21 flips/h). And a late tick is not a lost slot: `video_sleep` (obs-video.c)
//!   counts one frame for any overrun below two intervals and schedules the next slot from the
//!   previous TARGET, so the next tick is a CATCH-UP. The presented age read at the processing
//!   wall of a late tick is one frame too deep for the whole overrun (review rounds 1-2: a margin
//!   on the processing wall only moved the misfire band). Read at the scheduled instant
//!   ([`n1_tick_wall_ns`] — `video_time`, the `sys_time` `async_tick` passes down, mapped into wall
//!   time) the age carries no lateness at all. An overrun of two intervals or more DOES skip slots
//!   (`count >= 2`); the next tick's scheduled instant then jumps with them, and the depth it reads
//!   is the genuinely deeper one the shed repays.
//! - BOTH HALVES READ THE ROUNDED DEPTH ([`n1_depth_frames`], `(age + interval/2) / interval`).
//!   Presented stamps and scheduled ticks sit on the same per-second grid (issue 1355), so the
//!   scheduled age is a whole number of frames plus the stamp's sub-100-ns floor and the tick's
//!   schedule phase (a tick after a wall-clock step sits off the grid until `GENLOCK_MAX_SLEW_NS`
//!   pulls it back, 2 ms per tick). Rounding keeps the READ exact for up to half a frame of that
//!   phase, and a shed needs the rounded depth at `target + 1` while a hold needs it at
//!   `target − 1`: the two decisions never share an edge (a shared edge limit-cycled hold/shed
//!   ~1100 each per hour in the bench).
//! - ONLY ON THE GRID ([`n1_tick_is_on_grid`], review round 3). A wall-clock STEP on the box
//!   leaves the scheduled ticks off the grid by the step, and `genlock_next_deadline` pulls them
//!   back only `GENLOCK_MAX_SLEW_NS` (2 ms) per tick. Read there, a settled conveyor reads one
//!   frame deep after a forward step of more than half a frame (a shed) and one frame shallow once
//!   back on the grid (a hold): a skip plus a duplicate where the boundary-keyed conveyor did
//!   nothing. Both halves therefore act only while the scheduled tick is within
//!   [`N1_ON_GRID_NS`] of a per-second grid point and defer otherwise. A normal or caught-up late
//!   tick is scheduled on its slot or at most `GENLOCK_MAX_SLEW_NS` after it (a tick that overran
//!   the next grid point by under 2 ms sleeps to that slot plus the clamped 2 ms), so it is on the
//!   grid; at that exact +2 ms edge the wall/monotonic read order can defer one tick. The same
//!   condition removes the early-phase case beyond 2 ms: an early tick past the pin's headroom to
//!   the next frame edge moves the release deadline a frame, so a sender gap/dup would cost a
//!   hold/shed pair. At pins whose headroom is under 2 ms (999 ms, 1000 ms) an early tick inside
//!   the window still can — only on the last one or two ticks of a backward-step slew.
//! - THE 1 µs PIN TOLERANCE. The integer interval (33_333_333) is ~1/3 ns short of a real frame,
//!   so a pin that IS a whole number of frames (100 ms) would count one frame too many and put the
//!   target on the drain's own edge (23 flips/h in the bench). A sub-microsecond excess is not a
//!   real extra frame.
//! - DEEP SOURCES ONLY. The rule acts only when `floor_frames + N1_DEEP_MARGIN_FRAMES <= base`,
//!   where the ARRIVAL FLOOR is `wall − newest queued stamp` at the processing wall (the
//!   achievable-floor reference of #1049). A shallow source (the `cg` feeds, the imag cameras at
//!   3 ms) has a depth decided by its ARRIVAL, not its pin, so a pin-derived target would fight the
//!   floor. The two-frame margin keeps a late tick's larger floor (one frame at most) from ever
//!   admitting a floor-dominated source. Since ROZHODNUTÉ 5827497952 a shallow source is held on
//!   its own PER-LOCK depth instead: `max(base, measured arrival floor) + 1`, latched after each lock
//!   ([`n1_shallow_track`], the section before the tests).
//!
//! N>=2 is untouched: `genlock_backlog::should_converge_phase` keeps its `source_multiple < 2`
//! early return byte for byte; the SOURCE wrapper routes an N==1 tick here instead (the C
//! `genlock_should_converge_phase`, the probe `ReleaseCadence::should_converge_phase`, the
//! issue-1355 bench), and the release port's N==1 STEADY branch calls [`should_hold_n1_phase`]
//! (the C `genlock_should_hold_n1_phase`). A separate module because `genlock_backlog.rs` is already
//! past the ~1000-line budget. Pure `std` + one crate constant — Tier-0 verifiable.

use crate::genlock_backlog::{DRAIN_MIN_TICK_INTERVAL, GENLOCK_N2_JITTER_BUDGET_NS};

/// issue 1367 — a pin within this many ns ABOVE a whole number of frame intervals counts as that
/// whole number (the integer interval is fractionally short of a real frame). Mirror of the C
/// `GENLOCK_N1_PIN_FRAME_TOLERANCE_NS`.
pub const N1_PIN_FRAME_TOLERANCE_NS: u64 = 1_000;

/// issue 1367 — the pin-derived depth must exceed the arrival floor by at least this many frames
/// for the N==1 rule to act (a DEEP source). Mirror of the C `GENLOCK_N1_DEEP_MARGIN_FRAMES`.
pub const N1_DEEP_MARGIN_FRAMES: u64 = 2;

/// issue 1367 (review round 3) — the N==1 rule acts only while the render tick's scheduled
/// instant is within this many ns of a grid point: the render tick's own per-tick slew clamp
/// (`GENLOCK_MAX_SLEW_NS`, obs-video.c), so a tick still slewing back after a wall-clock step is
/// off the grid and the first on-grid tick is at most one slew step away. Mirror of the C
/// `GENLOCK_N1_ON_GRID_NS`.
pub const N1_ON_GRID_NS: u64 = 2_000_000;

/// issue 1367 — the WALL instant the current render tick was SCHEDULED for: the processing wall
/// minus how far the processing runs behind the tick's scheduled monotonic instant
/// (`wall_now − (mono_now − scheduled_mono)`, saturating). The C passes `os_gettime_ns()` and
/// `obs->video.video_time`; a tick that has not yet reached its schedule (never, in OBS) reads the
/// processing wall. Mirror of the C `genlock_n1_tick_wall_ns`.
pub fn n1_tick_wall_ns(wall_now_ns: u64, mono_now_ns: u64, scheduled_mono_ns: u64) -> u64 {
    wall_now_ns.saturating_sub(mono_now_ns.saturating_sub(scheduled_mono_ns))
}

/// issue 1367 (review round 3) — is the scheduled tick `tick_wall_ns` within [`N1_ON_GRID_NS`]
/// of the grid point `grid_floor_ns`, where `grid_floor_ns` is the grid floor of
/// `tick_wall_ns + N1_ON_GRID_NS` (the only grid point that can be that close)? Pure; the caller
/// supplies the floor of its own grid. Mirror of the C `genlock_n1_tick_on_grid`.
pub fn n1_tick_on_grid(tick_wall_ns: u64, grid_floor_ns: u64) -> bool {
    let shifted = tick_wall_ns.saturating_add(N1_ON_GRID_NS);
    shifted >= grid_floor_ns && shifted - grid_floor_ns <= 2 * N1_ON_GRID_NS
}

/// issue 1367 (review round 3) — [`n1_tick_on_grid`] on the ONE per-second genlock grid the
/// render tick and the senders use ([`crate::genlock_grid::grid_floor_ns`]). Mirror of the C
/// `genlock_n1_tick_is_on_grid`.
pub fn n1_tick_is_on_grid(tick_wall_ns: u64, interval_ns: u64) -> bool {
    n1_tick_on_grid(
        tick_wall_ns,
        crate::genlock_grid::grid_floor_ns(tick_wall_ns.saturating_add(N1_ON_GRID_NS), interval_ns),
    )
}

/// issue 1367 — the depth, in frames, a deep N==1 source lands on after a GAP RESYNC:
/// `ceil((pin − N1_PIN_FRAME_TOLERANCE_NS) / interval)`. `interval_ns == 0` returns 0.
pub fn n1_base_frames(latency_ms: u32, interval_ns: u64) -> u64 {
    if interval_ns == 0 {
        return 0;
    }
    let reserve_ns = (latency_ms as u64).saturating_mul(1_000_000);
    reserve_ns
        .saturating_sub(N1_PIN_FRAME_TOLERANCE_NS)
        .div_ceil(interval_ns)
}

/// issue 1367 — the depth, in frames, a deep N==1 source settles on: [`n1_base_frames`] + 1.
pub fn n1_target_frames(latency_ms: u32, interval_ns: u64) -> u64 {
    n1_base_frames(latency_ms, interval_ns).saturating_add(1)
}

/// issue 1367 — the presented depth, ROUNDED to whole frames, of a frame stamped `stamp_ns` at the
/// tick's scheduled instant `tick_wall_ns`: `(tick_wall − stamp + interval/2) / interval`.
/// `interval_ns == 0` returns 0; a stamp ahead of the tick reads age 0.
pub fn n1_depth_frames(tick_wall_ns: u64, stamp_ns: u64, interval_ns: u64) -> u64 {
    if interval_ns == 0 {
        return 0;
    }
    tick_wall_ns
        .saturating_sub(stamp_ns)
        .saturating_add(interval_ns / 2)
        / interval_ns
}

/// issue 1367 — is this N==1 source DEEP (its pin, not its arrival, decides its depth)? True when
/// the arrival floor (`wall − newest queued stamp`, ns) in whole frames plus
/// [`N1_DEEP_MARGIN_FRAMES`] is at most [`n1_base_frames`]. `interval_ns == 0` is never deep.
pub fn n1_is_deep_source(arrival_floor_ns: u64, latency_ms: u32, interval_ns: u64) -> bool {
    if interval_ns == 0 {
        return false;
    }
    (arrival_floor_ns / interval_ns).saturating_add(N1_DEEP_MARGIN_FRAMES)
        <= n1_base_frames(latency_ms, interval_ns)
}

/// issue 1367 — the N==1 SHED half (the caller gates it on [`n1_tick_is_on_grid`]): shed one
/// frame when the last presented depth (read from the
/// locked boundary, `boundary == last presented + interval`, at this tick's scheduled instant) is
/// deeper than [`n1_target_frames`] on a deep source, throttled by the shared #859 counter. An
/// unlocked boundary (0) or a degenerate interval never sheds. Mirror of the C
/// `genlock_n1_shed_due`.
pub fn n1_shed_due(
    tick_wall_ns: u64,
    locked_boundary_ns: u64,
    arrival_floor_ns: u64,
    latency_ms: u32,
    interval_ns: u64,
    ticks_since_last_drain: u64,
) -> bool {
    interval_ns != 0
        && locked_boundary_ns != 0
        && n1_is_deep_source(arrival_floor_ns, latency_ms, interval_ns)
        && n1_depth_frames(tick_wall_ns, locked_boundary_ns, interval_ns)
            > n1_target_frames(latency_ms, interval_ns)
        && ticks_since_last_drain >= DRAIN_MIN_TICK_INTERVAL
}

/// issue 1367 — the N==1 HOLD half (the caller gates it on [`n1_tick_is_on_grid`]): on a STEADY
/// tick, HOLD one tick (a deliberate repeat, the
/// conveyor one frame deeper next tick) when presenting the queue head now would put a deep N==1
/// source SHALLOWER than [`n1_target_frames`] (both read at the tick's scheduled instant). Reads
/// the HEAD's age, not the boundary's: a startup DUPLICATE sits one interval below the boundary and
/// already deepens the conveyor when presented, so a boundary-based read would hold in front of it
/// and overshoot. Shares the #859 throttle (the caller resets it on a hold). Inert for
/// `source_multiple >= 2` (the N>=2 conveyor has its own #1049 shed), for a shallow source, and for
/// a degenerate interval. Mirror of the C `genlock_n1_hold_due`.
pub fn should_hold_n1_phase(
    tick_wall_ns: u64,
    head_stamp_ns: u64,
    arrival_floor_ns: u64,
    latency_ms: u32,
    interval_ns: u64,
    source_multiple: u32,
    ticks_since_last_drain: u64,
) -> bool {
    source_multiple < 2
        && interval_ns != 0
        && n1_is_deep_source(arrival_floor_ns, latency_ms, interval_ns)
        && n1_depth_frames(tick_wall_ns, head_stamp_ns, interval_ns)
            < n1_target_frames(latency_ms, interval_ns)
        && ticks_since_last_drain >= DRAIN_MIN_TICK_INTERVAL
}

// ---- issue 1367 (ROZHODNUTÉ 5827497952): the SHALLOW N==1 source's per-LOCK depth ----------------
//
// WHY. A SHALLOW N==1 source (a cg feed at a 3 ms pin: resolume `sp-*_video`, strih-lx `CG-obs`) has a
// depth set by its ARRIVAL, not its pin, so the deep rule above never acts on it and its depth
// FLOATED: live `sp-slow_video` stepped 66 / 100 / 133 ms between 5 s audits, and the Option-3 audio
// hold followed every step (72 audio re-placements in 42 min = the songplayer gate's 33 ms dropout).
//
// THE RULE. After each LOCK, measure the ARRIVAL FLOOR — the rounded age of the newest queued frame
// at the tick's scheduled instant — over [`N1_SHALLOW_SETTLE_TICKS`] on-grid present ticks and LATCH
// `D = max(base, p90 floor) + 1` ([`n1_shallow_target_frames`]): one frame of jitter headroom above
// the window's 90th-percentile arrival (design 5830750134 — the window MAX let one sender transient
// latch D 12 = 400 ms live; a window whose p10–p90 spread exceeds a frame re-measures instead, and D
// is clamped to `base + N1_SHALLOW_MAX_EXTRA_FRAMES` and reported). D then stays constant until the next relock (an ACQUIRE, a GAP RESYNC over
// a gap of at least [`N1_SHALLOW_RELOCK_GAP_NS`] = a sender restart, or a pin change), and the same
// one-frame hold / shed as the deep rule keeps the presented depth ON D
// ([`n1_shallow_hold_due`] / [`n1_shallow_shed_due`]). Stamps and scheduled ticks share the
// per-second grid, so the rounded floor already IS `ceil(arrival lag / interval)`; a raw `ceil` of
// the ns value would jump a frame on a 1 ns phase. A relock keeps the OLD D maintained while the new
// floor is measured, so a relock that finds the same floor changes nothing.
//
// The rule acts only while the source is NOT deep ([`n1_shallow_governs`]): a deep source keeps the
// pin rule above, whose target `base + 1` equals `max(base, floor_max) + 1` for any floor below the
// pin. While it governs, the caller suppresses the #859 queue-length drain (the shed covers
// depth > D, and a queue-length drain would fight it on a wide arrival spread).
//
// The imag guard: on a MIN-LATENCY box (the imag projection, "najmenšia možná latencia") a D above
// the pin-derived `base + 1` is REPORTED (`capped`) and NOT applied: the latch stores no depth, so
// the rule never governs that input and its conveyor keeps the pre-rule behaviour (the #859 drain
// included) — report instead of deepening, never a forced shallower depth that sheds/holds against
// the input's own arrival (review round 1).
//
// A DEEP source (the pin, not the arrival, decides its depth) latches the pin rule's own
// `base + 1`, whatever its floor did during the window, so the two rules can never disagree.
//
// Re-measure without a relock ([`n1_shallow_watch`]): a latched source whose rounded floor sits AT
// or OVER D for a whole settle window (its arrival rose, review round 1) — or, for a CLAMPED latch,
// two frames or more under D (the over-cap floor was a transient) — re-arms; so does one whose
// REALIZED depth stays under D for [`N1_SHALLOW_UNDER_TICKS`] (D is unreachable) or that takes
// [`N1_SHALLOW_CHURN_RELOCKS`] backlog relocks without a quiet gap (a relock storm against D,
// design 5830750134). An N==1 source with no depth, no window and no cap (it became N==1 without an
// ACQUIRE, e.g. a 60p sender switched to 30p) opens a window.

/// issue 1367 — the on-grid PRESENT ticks the arrival floor is measured over after a lock
/// (3 s at 30 fps, 1.5 s at 60 fps). Mirror of the C `GENLOCK_N1_SHALLOW_SETTLE_TICKS`.
pub const N1_SHALLOW_SETTLE_TICKS: u32 = 90;

/// issue 1367 — a GAP RESYNC whose missing-stamp gap (head stamp − locked boundary) reaches this is
/// a sender RESTART, i.e. a relock that re-measures the floor. A single lost frame is not. Mirror of
/// the C `GENLOCK_N1_SHALLOW_RELOCK_GAP_NS`.
pub const N1_SHALLOW_RELOCK_GAP_NS: u64 = 1_000_000_000;

/// issue 1367 (design 5830750134, the shallow-latch outlier) — the latched depth never exceeds
/// `base + N1_SHALLOW_MAX_EXTRA_FRAMES`. Sized from the live healthy latches: every resolume
/// `sp-*` / `NDI test` latch of 25.9.2026 had `floor_max_frames` 1–2 (D − base 1–2), and the
/// bench's 50–80 ms straddle band needs base + 3. The live outlier (`floor_max_frames=11` during a
/// sender transient → D 12, 400 ms) is what it bounds. An over-cap latch is CLAMPED and reported
/// (`capped`). Mirror of the C `GENLOCK_N1_SHALLOW_MAX_EXTRA_FRAMES`.
pub const N1_SHALLOW_MAX_EXTRA_FRAMES: u64 = 3;

/// issue 1367 (design 5830750134) — the settle window's floor histogram bins, RELATIVE to base:
/// bin k counts floors of `base + k` (a floor under base counts as bin 0 — it cannot change D), the
/// last bin every floor at or over `base + N1_SHALLOW_MAX_EXTRA_FRAMES` (a D over the cap). Mirror
/// of the C `GENLOCK_N1_SHALLOW_HIST_BINS`.
pub const N1_SHALLOW_HIST_BINS: usize = 4;
const _: () = assert!(N1_SHALLOW_HIST_BINS as u64 == N1_SHALLOW_MAX_EXTRA_FRAMES + 1);

/// issue 1367 (design 5830750134) — the latch reads this PERCENTILE of the window's floors, not the
/// max: a transient burst shorter than a tenth of the window cannot set D. Mirror of the C
/// `GENLOCK_N1_SHALLOW_LATCH_PERCENTILE`.
pub const N1_SHALLOW_LATCH_PERCENTILE: u32 = 90;

/// issue 1367 (design 5830750134) — a non-deep window whose p90 − p10 floor spread (in histogram
/// bins) exceeds this is a transient in progress: it does not latch, it re-measures. The healthy
/// spread is 0–1 (a lag straddling at most one frame edge: the live `CG-obs` 33–67 ms, the bench's
/// 50–80 ms band). The live song change on its 2-frame floor reads p10 = base + 1, p90 = the
/// over-cap bin: spread 2, rejected (a threshold of 2 let the bench's one-second transient through
/// to a clamped latch). Mirror of the C `GENLOCK_N1_SHALLOW_MAX_SPREAD_FRAMES`.
pub const N1_SHALLOW_MAX_SPREAD_FRAMES: u64 = 1;

/// issue 1367 (design 5830750134) — consecutive spread-rejected windows after which the next one
/// latches anyway (bounded by the cap), so a genuinely bimodal feed still gets a D. Mirror of the
/// C `GENLOCK_N1_SHALLOW_MAX_REJECTS`.
pub const N1_SHALLOW_MAX_REJECTS: u32 = 3;

/// issue 1367 (design 5830750134) — a latched D whose REALIZED depth (the presented frame's
/// rounded age at the scheduled tick) stays below it for this many consecutive on-grid present
/// ticks is unreachable: re-measure. Twice the settle window, so the hold's climb onto a capped D
/// (`N1_SHALLOW_MAX_EXTRA_FRAMES` holds × the 30-tick throttle = 90 ticks) never trips it. Mirror
/// of the C `GENLOCK_N1_SHALLOW_UNDER_TICKS`.
pub const N1_SHALLOW_UNDER_TICKS: u32 = 180;

/// issue 1367 (design 5830750134) — this many BACKLOG relocks while latched, with no
/// [`N1_SHALLOW_CHURN_QUIET_TICKS`] gap between them, is a relock storm against D (the live 400 ms
/// latch relocked ~3 per 5 s): re-measure. Mirror of the C `GENLOCK_N1_SHALLOW_CHURN_RELOCKS`.
pub const N1_SHALLOW_CHURN_RELOCKS: u32 = 3;

/// issue 1367 (design 5830750134) — on-grid present ticks without a backlog relock that clear the
/// churn count (an occasional stall's relock hours apart never re-measures). Mirror of the C
/// `GENLOCK_N1_SHALLOW_CHURN_QUIET_TICKS`.
pub const N1_SHALLOW_CHURN_QUIET_TICKS: u32 = 180;

/// issue 1367 — the per-source shallow-depth state (the C `genlock_shallow_*` fields).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShallowDepth {
    /// The latched depth D, frames (0 = none latched yet).
    pub target_frames: u64,
    /// The largest rounded arrival floor of the current measurement window, frames.
    pub floor_max_frames: u64,
    /// On-grid present ticks sampled in the current window.
    pub window_ticks: u32,
    /// A measurement window is open (after a relock, until the latch).
    pub measuring: bool,
    /// The last latch was capped by the min-latency guard (it stores no depth: report only).
    pub capped: bool,
    /// Consecutive on-grid present ticks whose rounded floor sat at or over the latched D.
    pub over_ticks: u32,
    /// On-grid ticks of the current window on which the source read DEEP (the latch takes the
    /// MAJORITY, so a stall on the one latch tick cannot decide it).
    pub deep_ticks: u32,
    /// The current window's floor histogram, relative to base ([`N1_SHALLOW_HIST_BINS`]). Cleared
    /// on the window's FIRST sample (lazily, so the rearm stays a five-field reset in C too).
    pub hist: [u32; N1_SHALLOW_HIST_BINS],
    /// Consecutive on-grid present ticks whose realized depth sat below the latched D.
    pub under_ticks: u32,
    /// Backlog relocks while latched, since the last quiet gap.
    pub churn_relocks: u32,
    /// On-grid present ticks since the last backlog relock (a quiet gap clears the churn count).
    pub churn_quiet_ticks: u32,
    /// Consecutive spread-rejected windows.
    pub rejects: u32,
}

/// issue 1367 — one PRESENT tick's inputs to [`n1_shallow_track`] (the C passes them as scalars).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShallowTick {
    /// This tick presented on the N==1 path (an N>=2 source clears the whole state).
    pub n1: bool,
    /// This present was an ACQUIRE or a sender-restart GAP RESYNC (or a pin change re-armed it).
    pub relock: bool,
    /// The scheduled tick is on the per-second grid (only those are sampled).
    pub on_grid: bool,
    /// The rounded arrival floor at the scheduled instant, frames. The RAW floor: the rise / fell
    /// watch and `floor_max_frames` read it (a content-dependent arrival change inside the budget
    /// never re-measures).
    pub floor_frames: u64,
    /// issue 1367 (ROZHODNUTÉ 5842640404) — the LATCH floor, frames: the newest received frame's
    /// receive-time arrival lag plus the arrival-jitter budget
    /// ([`n1_shallow_latch_floor_frames`]). Only the window histogram (the p90 latch + the spread
    /// reject) reads it, and not on a min-latency box (ROZHODNUTÉ 5842848307: raw floor there).
    pub latch_floor_frames: u64,
    /// The pin-derived base, frames ([`n1_base_frames`]).
    pub base_frames: u64,
    /// The source is DEEP now ([`n1_is_deep_source`] at the processing wall); the latch uses the
    /// window's majority of these.
    pub deep: bool,
    /// This OBS box is a min-latency (imag) box.
    pub min_latency_box: bool,
    /// The REALIZED depth: the rounded age of the frame this tick presents, at the scheduled
    /// instant (the C `genlock_n1_depth_frames(tick_wall, source->last_frame_ts, interval)`).
    pub realized_frames: u64,
    /// A BACKLOG relock happened since the previous call (the C `genlock_relocks` moved).
    pub backlog_relock: bool,
}

/// issue 1367 (ROZHODNUTÉ 5842640404) — the LATCH floor of the newest received frame, frames:
/// `ceil((arrival_lag + GENLOCK_N2_JITTER_BUDGET_NS) / interval)`, 0 when `interval_ns == 0`.
///
/// WHY. Since issue 1355 the scheduled ticks and the sender stamps share the per-second grid, so
/// the age read AT THE TICK is `ceil(lag / interval)` whole frames: where the arrival sits inside
/// the frame is lost there, and a budget added to it would add a frame on EVERY source (the
/// rejected always-+1 approach). The lag is therefore measured at RECEIVE time
/// (`genlock_wall_now_ns() − output->timestamp` at the C stamp-tracking site, the C
/// `genlock_rx_arrival_lag_ns`). SongPlayer's content-dependent send cost (+8…11 ms from black to
/// playing, songplayer 147) moved a sp-* feed whose idle lag sat within ~10 ms under a frame edge
/// across it at every song start: the latch (made on idle) was one frame short, the rise watch
/// re-measured and the audio slewed +33 ms. With the SAME 15 ms budget the N>=2 conveyor uses
/// (#1354) the latch adds a frame exactly when the idle arrival sits within 15 ms under a frame
/// edge, and the rise watch keeps the RAW tick floor, so a rise inside the budget never
/// re-measures while a genuine rise of more than the budget still does. Monotone in the lag, so
/// the histogram's p90 of these bins IS the budgeted p90 lag. Mirror of the C
/// `genlock_n1_shallow_latch_floor_frames`.
pub fn n1_shallow_latch_floor_frames(arrival_lag_ns: u64, interval_ns: u64) -> u64 {
    if interval_ns == 0 {
        return 0;
    }
    arrival_lag_ns
        .saturating_add(GENLOCK_N2_JITTER_BUDGET_NS)
        .div_ceil(interval_ns)
}

/// issue 1367 (design 5830750134) — the histogram bin of one rounded floor: `floor − base`,
/// saturating at 0 (a floor under base cannot change D) and at the last bin (over the cap). Mirror
/// of the C `genlock_n1_shallow_hist_bin`.
pub fn n1_shallow_hist_bin(floor_frames: u64, base_frames: u64) -> usize {
    floor_frames
        .saturating_sub(base_frames)
        .min(N1_SHALLOW_HIST_BINS as u64 - 1) as usize
}

/// issue 1367 (design 5830750134) — the `pct` percentile of a window's histogram, as a bin: the
/// first bin whose cumulative count reaches `pct %` of `window_ticks` (integer: `cum · 100 ≥
/// window · pct`). An empty window reads bin 0. Mirror of the C `genlock_n1_shallow_percentile_bin`.
pub fn n1_shallow_percentile_bin(
    hist: &[u32; N1_SHALLOW_HIST_BINS],
    window_ticks: u32,
    pct: u32,
) -> u64 {
    let need = u64::from(window_ticks) * u64::from(pct);
    let mut cum = 0u64;
    for (bin, &n) in hist.iter().enumerate() {
        cum += u64::from(n);
        if cum * 100 >= need {
            return bin as u64;
        }
    }
    N1_SHALLOW_HIST_BINS as u64 - 1
}

/// issue 1367 — the latched depth: `max(base, latch_floor) + 1` (a DEEP source: the pin rule's own
/// `base + 1`), where `latch_floor` is the window's p90 floor ([`n1_shallow_track`]). On a
/// min-latency box a depth above `base + 1` is not applied: `(0, true)` (report only). Design
/// 5830750134: any other depth above `base + N1_SHALLOW_MAX_EXTRA_FRAMES` is CLAMPED to it and
/// reported: `(base + 3, true)`. Returns `(depth, capped)`. Mirror of the C
/// `genlock_n1_shallow_target_frames`.
pub fn n1_shallow_target_frames(
    base_frames: u64,
    latch_floor_frames: u64,
    deep: bool,
    min_latency_box: bool,
) -> (u64, bool) {
    let pin_depth = base_frames.saturating_add(1);
    let d = if deep {
        pin_depth
    } else {
        base_frames.max(latch_floor_frames).saturating_add(1)
    };
    let clamp = base_frames.saturating_add(N1_SHALLOW_MAX_EXTRA_FRAMES);
    if min_latency_box && d > pin_depth {
        (0, true)
    } else if d > clamp {
        (clamp, true)
    } else {
        (d, false)
    }
}

/// issue 1367 — is a GAP RESYNC over this missing-stamp gap a sender restart (a relock)? Mirror of
/// the C `genlock_n1_shallow_gap_is_relock`.
pub fn n1_shallow_gap_is_relock(gap_ns: u64) -> bool {
    gap_ns >= N1_SHALLOW_RELOCK_GAP_NS
}

/// issue 1367 — open a new measurement window; the latched D (if any) stays maintained. Mirror of
/// the C `genlock_n1_shallow_rearm`.
pub fn n1_shallow_rearm(s: &mut ShallowDepth) {
    s.floor_max_frames = 0;
    s.window_ticks = 0;
    s.over_ticks = 0;
    s.deep_ticks = 0;
    s.measuring = true;
}

/// issue 1367 (review round 2) — the window's deep verdict: a strict MAJORITY of its sampled ticks
/// read deep. One tick (a stall still running on the latch tick) can never decide it. Mirror of the
/// C `genlock_n1_shallow_window_deep`.
pub fn n1_shallow_window_deep(deep_ticks: u32, window_ticks: u32) -> bool {
    u64::from(deep_ticks) * 2 > u64::from(window_ticks)
}

/// issue 1367 (design 5830750134) — one on-grid PRESENT tick of a LATCHED source (`target_frames !=
/// 0`, no window open): update the three re-measure watches and say whether the window re-opens.
/// - The FLOOR left D for a whole window ([`N1_SHALLOW_SETTLE_TICKS`] consecutive ticks): at or over
///   D (the arrival rose, review round 1) — or, for a CLAMPED latch, two frames or more under it
///   (the over-cap floor was a transient; a floor still at the clamp keeps it, so a genuinely slow
///   arrival never re-measures in a loop).
/// - The REALIZED depth stayed under D for [`N1_SHALLOW_UNDER_TICKS`]: D is unreachable.
/// - [`N1_SHALLOW_CHURN_RELOCKS`] backlog relocks without a [`N1_SHALLOW_CHURN_QUIET_TICKS`] gap: a
///   relock storm against D.
///
/// Mirror of the C `genlock_n1_shallow_watch`.
pub fn n1_shallow_watch(s: &mut ShallowDepth, t: &ShallowTick) -> bool {
    let floor_off = if s.capped {
        t.floor_frames.saturating_add(2) <= s.target_frames
    } else {
        t.floor_frames >= s.target_frames
    };
    s.over_ticks = if floor_off {
        s.over_ticks.saturating_add(1)
    } else {
        0
    };
    s.under_ticks = if t.realized_frames < s.target_frames {
        s.under_ticks.saturating_add(1)
    } else {
        0
    };
    if t.backlog_relock {
        s.churn_relocks = s.churn_relocks.saturating_add(1);
        s.churn_quiet_ticks = 0;
    } else {
        s.churn_quiet_ticks = s.churn_quiet_ticks.saturating_add(1);
        if s.churn_quiet_ticks >= N1_SHALLOW_CHURN_QUIET_TICKS {
            s.churn_relocks = 0;
            s.churn_quiet_ticks = 0;
        }
    }
    s.over_ticks >= N1_SHALLOW_SETTLE_TICKS
        || s.under_ticks >= N1_SHALLOW_UNDER_TICKS
        || s.churn_relocks >= N1_SHALLOW_CHURN_RELOCKS
}

/// issue 1367 — one PRESENT tick of the shallow-depth state ([`ShallowTick`]). An N>=2 tick clears
/// the whole state (it has its own conveyor rule). A relock, or an N==1 source with no depth, no
/// window and no cap, opens a window; a latched source re-opens one when [`n1_shallow_watch`] says
/// so. Only on-grid ticks are sampled.
///
/// ROZHODNUTÉ 5842640404: the histogram bins the BUDGETED latch floor
/// ([`ShallowTick::latch_floor_frames`], [`n1_shallow_latch_floor_frames`]); the rise / fell watch
/// and `floor_max_frames` keep the raw tick floor, so a content-dependent arrival rise inside the
/// arrival-jitter budget never re-measures. ROZHODNUTÉ 5842848307: on a min-latency (imag) box the
/// histogram bins the RAW floor (no budget): at a 60p canvas the 15 ms budget is ~90 % of a frame and
/// would ask every input for more than base + 1, which that box only reports (capped).
///
/// Design 5830750134 (the latch never latches an outlier): the window's floors go into a histogram
/// relative to base ([`n1_shallow_hist_bin`], cleared on the window's first sample) and the latch
/// reads its p90 ([`N1_SHALLOW_LATCH_PERCENTILE`]), not the max. A non-deep window whose p90 − p10
/// spread exceeds [`N1_SHALLOW_MAX_SPREAD_FRAMES`] is a transient in progress: it re-measures (the
/// old D stays maintained), at most [`N1_SHALLOW_MAX_REJECTS`] times in a row. The depth is clamped
/// by [`n1_shallow_target_frames`]. Returns true on the tick that LATCHES a D (or a capped report).
/// Mirror of the C `genlock_n1_shallow_track`.
pub fn n1_shallow_track(s: &mut ShallowDepth, t: ShallowTick) -> bool {
    if !t.n1 {
        *s = ShallowDepth::default();
        return false;
    }
    if t.relock || (s.target_frames == 0 && !s.measuring && !s.capped) {
        n1_shallow_rearm(s);
    }
    if !t.on_grid {
        return false;
    }
    if !s.measuring {
        if s.target_frames == 0 {
            s.over_ticks = 0;
            return false;
        }
        if !n1_shallow_watch(s, &t) {
            return false;
        }
        n1_shallow_rearm(s);
    }
    if s.window_ticks == 0 {
        s.hist = [0; N1_SHALLOW_HIST_BINS];
    }
    s.floor_max_frames = s.floor_max_frames.max(t.floor_frames);
    // ROZHODNUTÉ 5842640404: the histogram (the p90 latch and the spread reject) reads the budgeted
    // receive-lag floor; the watch above and floor_max read the raw tick floor. ROZHODNUTÉ
    // 5842848307: a min-latency (imag) box keeps the RAW floor there too -- no budget frame.
    let hist_floor = if t.min_latency_box {
        t.floor_frames
    } else {
        t.latch_floor_frames
    };
    let bin = n1_shallow_hist_bin(hist_floor, t.base_frames);
    s.hist[bin] = s.hist[bin].saturating_add(1);
    s.window_ticks = s.window_ticks.saturating_add(1);
    if t.deep {
        s.deep_ticks = s.deep_ticks.saturating_add(1);
    }
    if s.window_ticks < N1_SHALLOW_SETTLE_TICKS {
        return false;
    }
    let deep = n1_shallow_window_deep(s.deep_ticks, s.window_ticks);
    let high = n1_shallow_percentile_bin(&s.hist, s.window_ticks, N1_SHALLOW_LATCH_PERCENTILE);
    let low = n1_shallow_percentile_bin(&s.hist, s.window_ticks, 100 - N1_SHALLOW_LATCH_PERCENTILE);
    if !deep && high - low > N1_SHALLOW_MAX_SPREAD_FRAMES && s.rejects < N1_SHALLOW_MAX_REJECTS {
        s.rejects += 1;
        n1_shallow_rearm(s);
        return false;
    }
    let (d, capped) = n1_shallow_target_frames(
        t.base_frames,
        t.base_frames.saturating_add(high),
        deep,
        t.min_latency_box,
    );
    s.target_frames = d;
    s.capped = capped;
    s.measuring = false;
    s.rejects = 0;
    s.under_ticks = 0;
    s.churn_relocks = 0;
    s.churn_quiet_ticks = 0;
    true
}

/// issue 1367 — does the latched shallow depth govern this source now? A latched D on a source
/// that is NOT deep. Mirror of the C `genlock_n1_shallow_governs`.
pub fn n1_shallow_governs(
    target_frames: u64,
    arrival_floor_ns: u64,
    latency_ms: u32,
    interval_ns: u64,
) -> bool {
    target_frames != 0
        && interval_ns != 0
        && !n1_is_deep_source(arrival_floor_ns, latency_ms, interval_ns)
}

/// issue 1367 (ROZHODNUTÉ 5840479751) — the depth, in frames, the N==1 governor holds this source
/// at, or 0 when it holds none: [`n1_target_frames`] (`base + 1`) on a DEEP source, the latched D
/// while [`n1_shallow_governs`], else 0. The backlog relock's stale-anchor test reads it to know
/// where a CORRECT anchor sits (`relock_anchor_is_stale` in `genlock_backlog`). Built only from the
/// governor's own predicates, so the two can never disagree. The caller gates it on N==1 exactly
/// as the governor's source wrappers do. Mirror of the C `genlock_n1_expected_depth_frames`.
pub fn n1_expected_depth_frames(
    arrival_floor_ns: u64,
    latency_ms: u32,
    interval_ns: u64,
    shallow_target_frames: u64,
) -> u64 {
    if n1_is_deep_source(arrival_floor_ns, latency_ms, interval_ns) {
        n1_target_frames(latency_ms, interval_ns)
    } else if n1_shallow_governs(
        shallow_target_frames,
        arrival_floor_ns,
        latency_ms,
        interval_ns,
    ) {
        shallow_target_frames
    } else {
        0
    }
}

/// issue 1367 — the shallow SHED half (the caller gates it on [`n1_tick_is_on_grid`]): shed one frame
/// when the last presented depth (the locked boundary) sits deeper than the latched D, throttled by
/// the shared #859 counter. Design 5830750134: never while the NEWEST queued frame is already more
/// than D whole frames old (`arrival_floor / interval > D`) — a D the arrival cannot supply (a clamp
/// under a genuinely slow sender) would only skip a frame and run the queue dry every throttle
/// window. Mirror of the C `genlock_n1_shallow_shed_due`.
pub fn n1_shallow_shed_due(
    tick_wall_ns: u64,
    locked_boundary_ns: u64,
    arrival_floor_ns: u64,
    latency_ms: u32,
    interval_ns: u64,
    target_frames: u64,
    ticks_since_last_drain: u64,
) -> bool {
    locked_boundary_ns != 0
        && n1_shallow_governs(target_frames, arrival_floor_ns, latency_ms, interval_ns)
        && arrival_floor_ns / interval_ns <= target_frames
        && n1_depth_frames(tick_wall_ns, locked_boundary_ns, interval_ns) > target_frames
        && ticks_since_last_drain >= DRAIN_MIN_TICK_INTERVAL
}

/// issue 1367 — the shallow HOLD half (the caller gates it on [`n1_tick_is_on_grid`]): hold one
/// STEADY tick when presenting the queue head now would put the conveyor shallower than the latched
/// D. Called only from the N==1 STEADY branch. Mirror of the C `genlock_n1_shallow_hold_due`.
pub fn n1_shallow_hold_due(
    tick_wall_ns: u64,
    head_stamp_ns: u64,
    arrival_floor_ns: u64,
    latency_ms: u32,
    interval_ns: u64,
    target_frames: u64,
    ticks_since_last_drain: u64,
) -> bool {
    n1_shallow_governs(target_frames, arrival_floor_ns, latency_ms, interval_ns)
        && n1_depth_frames(tick_wall_ns, head_stamp_ns, interval_ns) < target_frames
        && ticks_since_last_drain >= DRAIN_MIN_TICK_INTERVAL
}

/// issue 1367 (design 5833339163, the song change) — the shallow GAP hold (the caller gates it on
/// [`n1_tick_is_on_grid`]): on a GAP RESYNC tick (the queue head is past the locked boundary —
/// upstream skipped stamps), HOLD instead of presenting while the head is still younger than the
/// latched D. The GAP RESYNC used to present that head at once at its arrival age, one frame (or
/// more) under D, and [`n1_shallow_hold_due`] then needed a throttle window per frame to climb back
/// (live resolume 25.9.2026 15:29: `video_delay_ms=67 audio_delay_ms=100` for ~10 s while the
/// sender skipped stamps across a song change). Held here, the head goes on air at D, so a skipped
/// stamp costs exactly the one repeat it costs anyway and the conveyor never leaves D. NOT
/// throttled: the hold bounds itself (the head ages a frame per tick, so it presents after at most
/// D ticks) and it never moves the conveyor off D, so it cannot limit-cycle with the shed. Only
/// while D governs, and never for a sender RESTART ([`n1_shallow_gap_is_relock`]: that relock
/// re-measures the floor, the old path).
///
/// A gap whose head is already DUPLICATED behind it (`next_stamp_ns`, the second queued frame's
/// stamp, `0` = none queued, is at or below the head's) is not missing content: a sender that
/// stamps at SEND time (strih-lx, the #1355 residual) labels a slow frame one slot late and the
/// next frame carries the same stamp, so the head IS the frame due now and the GAP RESYNC puts it
/// on air on time (the stamp age reads one frame short for that tick only). The guard only sees a
/// duplicate that is ALREADY QUEUED: when it has not arrived yet the hold fires on a late label and
/// costs one repeat plus a later shallow shed (review round 1: 0 at the live-calibrated strih-lx
/// jitter, 22 + 22 per 2 h at σ 4 ms, pinned by the grid bench). The stamps alone cannot tell that
/// case from a real skip (the next frame of a real skip has usually not arrived either); a real
/// fix needs per-frame arrival times. Mirror of the C `genlock_n1_shallow_gap_hold_due`.
#[allow(clippy::too_many_arguments)]
pub fn n1_shallow_gap_hold_due(
    tick_wall_ns: u64,
    head_stamp_ns: u64,
    next_stamp_ns: u64,
    locked_boundary_ns: u64,
    arrival_floor_ns: u64,
    latency_ms: u32,
    interval_ns: u64,
    target_frames: u64,
) -> bool {
    locked_boundary_ns != 0
        && (next_stamp_ns == 0 || next_stamp_ns > head_stamp_ns)
        && !n1_shallow_gap_is_relock(head_stamp_ns.saturating_sub(locked_boundary_ns))
        && n1_shallow_governs(target_frames, arrival_floor_ns, latency_ms, interval_ns)
        && n1_depth_frames(tick_wall_ns, head_stamp_ns, interval_ns) < target_frames
}

#[cfg(test)]
#[path = "genlock_n1_depth_tests.rs"]
mod tests;
