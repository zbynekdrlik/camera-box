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

use crate::genlock_backlog::DRAIN_MIN_TICK_INTERVAL;

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
// `D = max(base, floor_max) + 1` ([`n1_shallow_target_frames`]): one frame of jitter headroom above
// the worst arrival seen. D then stays constant until the next relock (an ACQUIRE, a GAP RESYNC over
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
// Re-measure without a relock (review round 1): a latched source whose rounded floor sits AT or
// OVER D for a whole settle window (its arrival rose — the frame at D is the newest or not there
// yet) re-arms; and an N==1 source with no depth, no window and no cap (it became N==1 without an
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
/// bins) exceeds this is a transient in progress: it does not latch, it re-measures. The live
/// healthy spread is 0–1 (a lag straddling one frame edge). Mirror of the C
/// `GENLOCK_N1_SHALLOW_MAX_SPREAD_FRAMES`.
pub const N1_SHALLOW_MAX_SPREAD_FRAMES: u64 = 2;

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
    /// The rounded arrival floor at the scheduled instant, frames.
    pub floor_frames: u64,
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

/// issue 1367 — the latched depth: `max(base, floor_max) + 1` (a DEEP source: the pin rule's own
/// `base + 1`). On a min-latency box a depth above `base + 1` is not applied: `(0, true)` (report
/// only). Returns `(depth, capped)`. Mirror of the C `genlock_n1_shallow_target_frames`.
pub fn n1_shallow_target_frames(
    base_frames: u64,
    floor_max_frames: u64,
    deep: bool,
    min_latency_box: bool,
) -> (u64, bool) {
    let cap = base_frames.saturating_add(1);
    let d = if deep {
        cap
    } else {
        base_frames.max(floor_max_frames).saturating_add(1)
    };
    if min_latency_box && d > cap {
        (0, true)
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

/// issue 1367 — one PRESENT tick of the shallow-depth state ([`ShallowTick`]). An N>=2 tick clears
/// the whole state (it has its own conveyor rule). A relock, or an N==1 source with no depth, no
/// window and no cap, opens a window; a latched source whose rounded floor sat at or over D for
/// [`N1_SHALLOW_SETTLE_TICKS`] on-grid ticks re-opens one (its arrival rose). Only on-grid ticks are
/// sampled. Returns true on the tick that LATCHES a D (or a capped report). Mirror of the C
/// `genlock_n1_shallow_track`.
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
        if s.target_frames == 0 || t.floor_frames < s.target_frames {
            s.over_ticks = 0;
            return false;
        }
        s.over_ticks = s.over_ticks.saturating_add(1);
        if s.over_ticks < N1_SHALLOW_SETTLE_TICKS {
            return false;
        }
        n1_shallow_rearm(s);
    }
    s.floor_max_frames = s.floor_max_frames.max(t.floor_frames);
    s.window_ticks = s.window_ticks.saturating_add(1);
    if t.deep {
        s.deep_ticks = s.deep_ticks.saturating_add(1);
    }
    if s.window_ticks < N1_SHALLOW_SETTLE_TICKS {
        return false;
    }
    let (d, capped) = n1_shallow_target_frames(
        t.base_frames,
        s.floor_max_frames,
        n1_shallow_window_deep(s.deep_ticks, s.window_ticks),
        t.min_latency_box,
    );
    s.target_frames = d;
    s.capped = capped;
    s.measuring = false;
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

/// issue 1367 — the shallow SHED half (the caller gates it on [`n1_tick_is_on_grid`]): shed one frame
/// when the last presented depth (the locked boundary) sits deeper than the latched D, throttled by
/// the shared #859 counter. Mirror of the C `genlock_n1_shallow_shed_due`.
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

#[cfg(test)]
#[path = "genlock_n1_depth_tests.rs"]
mod tests;
