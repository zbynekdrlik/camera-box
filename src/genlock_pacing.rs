//! (#1113) Genlock capture→emit pacing gate — the pure wall-clock decimation math extracted
//! verbatim from `src/ndi.rs` (surfaced by the issue-1111 review: `ndi.rs` had grown to ~2555
//! lines, ~2.5× the ~1000-line budget). This is a cohesive, dependency-free cluster that only
//! shared the `ndi` module by history, not by need — it has zero NDI/FFI dependency.
//!
//! The cam box captures faster than the genlock target rate (a ShadowCast grabber free-runs
//! ~61–64 fps) and DECIMATES the capture stream onto a wall-clock target-rate grid before
//! NDI-emitting to the downstream genlock FIFO. This module owns the four cooperating pure-`u64`
//! pieces of that grid:
//!
//! - [`genlock_emit_gate`] — the wall-clock decimation grid: emit the first capture at/after each
//!   boundary; #707 B1 catches up a bounded buffered-drain one interval at a time and grid-resyncs
//!   only past a real clock STEP (`> `[`GENLOCK_MAX_CATCHUP_INTERVALS`]) — and #1131 further
//!   suppresses that resync entirely while a captured frame is still buffered in the V4L2 queue
//!   (`queue_had_frame`), so a sick/wobbly grabber's late buffered frames are never leaped-past and
//!   discarded in a run (the multi-slot-skip judder).
//! - [`genlock_emit_on_time`] (#1111) — is this an ON-TIME/surplus crossing vs a LATE catch-up
//!   crossing? Shares [`genlock_latched_boundary`] with the gate so the two never disagree; this is
//!   the signal `crate::dupe_decimation`'s #889 dupe-preferring shed needs to stay boundary-neutral.
//! - [`boundary_skip_count`] (#707) — how many whole emit-boundary intervals were SKIPPED
//!   (never emitted), the #707 SKIPPED-boundaries diagnostic.
//!
//! `cfg(target_os = "linux")` in lock-step with `crate::ndi` (whose NDI-timecode grid,
//! [`crate::ndi::next_boundary_100ns`] / [`crate::ndi::fps_from_frame_rate`], this pacing gate
//! complements but does not depend on). Pure logic — Tier-0 testable on the Linux `test` CI job
//! (default features): the sibling-module precedent of `genlock_stamp` / `dupe_decimation`.

/// NDI sender wrapper - optimized for low latency
/// Genlock decimation gate (#11): given the current wall-clock time `now_ns`, the
/// next emit boundary `next_boundary_ns` (0 = uninitialized), the boundary
/// `interval_ns` (1e9 / target_fps), and `queue_had_frame` (#1131 — did THIS frame
/// come from a NON-EMPTY V4L2 queue, i.e. already buffered? see
/// [`crate::capture_stall::frame_from_nonempty_queue`]), decide whether THIS captured
/// frame should be emitted — it is the first capture at/after a boundary — and return
/// the updated next boundary. The faster capture (e.g. 60 fps) is decimated onto the DanteSync
/// wall-clock boundaries of the slower genlock/broadcast rate (e.g. 30 fps) so a
/// downstream genlocked OBS consumes exactly one frame per render tick (zero loss).
/// Pure + fully mutation-tested; the capture loop wires it to `wall_clock_ns()`.
///
/// Grid note: this gate aligns on a continuous epoch-relative grid
/// (`now_ns % interval_ns`), which is INDEPENDENT of the per-second-reset grid
/// used for the stamped NDI timecode in [`crate::ndi::next_boundary_100ns`]. With an integer
/// `interval_ns` (e.g. 33_333_333 for 30 fps) the two grids differ only by the
/// per-second truncation residue (~10 ns/s, < 2e-8 rate error → under one frame
/// per hour) — harmless for the OBS-FIFO decimation this drives; the grids are
/// not required to coincide. `interval_ns == 0` disables the gate and is the
/// guarded divisor case, matching [`crate::ndi::next_boundary_100ns`] / [`crate::ndi::fps_from_frame_rate`]
/// which also guard a zero divisor rather than panicking.
/// #707 B1 — the largest lag (in whole emit-boundary intervals) the emit gate absorbs by
/// CATCHING UP one interval at a time (emitting each merely-late buffered frame for its own
/// boundary) before it gives up and grid-resyncs (leaps forward, dropping the intervening
/// boundaries). Grounded in the capture path: `src/capture.rs` opens a V4L2 mmap stream 4 buffers
/// deep (`Stream::with_buffers(.., 4)`), so a CPU-starvation stall (the #752 log storm) can buffer
/// AT MOST ~4 real captured frames before the device drops (capture-dropped increments) — a lag
/// this size or smaller is a bounded buffered-drain whose frames must all be emitted, NOT a
/// discontinuity. 8 = 2× that depth (margin for a slightly-deeper transient) while staying far
/// below a genuine wall-clock STEP: the DanteSync sawtooth is sub-ms (< 1 interval, never even
/// reaches the resync branch), and a real cold-boot NTP acquire steps by seconds (hundreds of
/// intervals >> 8) → still grid-resyncs, exactly as before (#131). See
/// [`genlock_emit_gate`]'s resync branch.
///
/// #1131 — this fixed bound is now the resync trigger ONLY for a frame that came from an EMPTY
/// queue (`!queue_had_frame` — the loop genuinely waited for it: a device stall that produced
/// nothing, or a real clock STEP). A frame that came from a NON-EMPTY queue (buffered — a real
/// captured frame is being drained late, the 0-capture-dropped sick-grabber case) NEVER resyncs
/// however large the lag: it catches up one interval so no buffered captured frame is discarded.
pub const GENLOCK_MAX_CATCHUP_INTERVALS: u64 = 8;

pub fn genlock_emit_gate(
    now_ns: u64,
    next_boundary_ns: u64,
    interval_ns: u64,
    queue_had_frame: bool,
) -> (bool, u64) {
    // Guard the divisor: a zero interval means genlock is off — never emit, and
    // never modulo/divide by zero (would panic on this pub API surface).
    if interval_ns == 0 {
        return (false, next_boundary_ns);
    }
    // Initialize (or keep) the next absolute wall-clock boundary.
    //
    // #131: guard a BACKWARD clock step, symmetric to the forward resync below.
    // The boundary is latched from CLOCK_REALTIME; on a cold boot dantesync can
    // acquire NTP late and step the realtime clock BACKWARD well below the latched
    // boundary. The latched boundary is then many intervals in the future
    // (`boundary - now > interval`), so `now < boundary` would stay true forever
    // and the gate would wedge at emit=false (0 NDI emitted) until a warm restart.
    // Re-latch to the rewound clock (same formula as the init / forward-resync
    // branches) so emit resumes within one interval, exactly as a restart does.
    let boundary = genlock_latched_boundary(now_ns, next_boundary_ns, interval_ns);
    if now_ns < boundary {
        // Between boundaries — decimate this capture (do not emit).
        return (false, boundary);
    }
    // Crossed the boundary: emit this (freshest) frame, advance the boundary.
    let mut next = boundary + interval_ns;
    if next <= now_ns {
        // Fell behind. #707 B1: distinguish a bounded jitter / CPU-starvation BUFFERED-DRAIN (the
        // freeze mechanism) from a genuine large wall-clock discontinuity (a DanteSync step). The
        // V4L2 capture queue is 4 buffers deep (`capture.rs`), so a starvation stall can buffer at
        // most ~4 REAL captured frames before the device drops; the loop drains them one-per-poll
        // at ~the same late wall clock, and EACH must be emitted (advance ONE interval to fill its
        // own boundary). The OLD unconditional grid-resync below leaped past those boundaries and
        // decimated the whole burst but its first frame — dropping ~N-1 of every buffered drain,
        // which IS the measured 60->44fps emit collapse and the strih underrun-then-jump freeze.
        // Only a lag beyond GENLOCK_MAX_CATCHUP_INTERVALS (which a 4-deep queue can never produce
        // as buffered frames — it must be a real clock STEP) grid-resyncs, as before (#131), so an
        // NTP/PTP jump never triggers a pathologically long stale catch-up.
        let lag_intervals = (now_ns - boundary) / interval_ns; // >= 1 here (next <= now_ns)
        if lag_intervals > GENLOCK_MAX_CATCHUP_INTERVALS && !queue_had_frame {
            // #1131: resync (leap the grid forward past the intervening boundaries) ONLY when this
            // frame did NOT come from a buffered V4L2 queue — i.e. the loop genuinely WAITED for it
            // (an EMPTY queue: a device stall that produced nothing, or a real wall-clock STEP), so
            // those boundaries had NO captured content and skipping them is honest (#131 cold-boot
            // resync preserved). When `queue_had_frame` is set, a real captured frame WAS buffered
            // and is being drained right now: a lag beyond the fixed catch-up bound is a late
            // BUFFERED-DRAIN (0 capture-dropped — the frames exist), NOT a discontinuity, so we
            // catch up ONE interval below (fill the next un-emitted boundary) instead of leaping
            // past the buffered frames and discarding them in a run (the issue-1131 multi-slot
            // judder). The next buffered frame re-evaluates against the following boundary, draining
            // the whole backlog one-per-frame with ZERO skipped boundaries.
            next = now_ns - (now_ns % interval_ns) + interval_ns;
        }
        // else: keep next = boundary + interval_ns — emit this (fresh, merely-late) frame for the
        // next un-emitted boundary; the emit rate self-heals one frame at a time (no permanent
        // un-emitted boundary → no emit-rate deficit).
    }
    (true, next)
}

/// #1111 — the wall-clock boundary [`genlock_emit_gate`] latches for `now_ns`, factored out so
/// [`genlock_emit_on_time`] computes the IDENTICAL boundary without duplicating the #131
/// backward-step / init re-latch formula. The caller guards `interval_ns != 0`.
fn genlock_latched_boundary(now_ns: u64, next_boundary_ns: u64, interval_ns: u64) -> u64 {
    if next_boundary_ns == 0 || next_boundary_ns > now_ns + interval_ns {
        now_ns - (now_ns % interval_ns) + interval_ns
    } else {
        next_boundary_ns
    }
}

/// #1111 — is `now_ns` an ON-TIME boundary crossing (the "surplus" regime), as opposed to a LATE
/// catch-up crossing? True iff the capture has reached the pending boundary AND the NEXT boundary
/// is still in the future (`boundary + interval > now`). It is FALSE both between boundaries
/// (`now < boundary` — [`genlock_emit_gate`] returns emit=false) and once the gate has fallen
/// behind (`boundary + interval <= now`, the catch-up / #707-resync regime where
/// [`genlock_emit_gate`] emits a merely-late frame).
///
/// This is the signal `dupe_decimation`'s issue-889 dupe-preferring shed needs to stay
/// boundary-neutral (#1111). DEFERRING a dupe (holding the boundary for the next capture) only
/// avoids lag in the on-time/surplus case — the deferred frame is then replaced by a capture that
/// still lands inside the SAME interval, so the boundary advances exactly once for the pair.
/// Deferring a LATE dupe instead holds the boundary while the wall clock keeps running, ratcheting
/// the gate's lag by one interval per deferral until it trips the #707 resync (the issue-1110 CAM1
/// judder). So the shed defers a dupe only when this returns true; a late dupe is emitted instead
/// (a repeated frame, invisible — and mathematically unavoidable when a 58-unique-fps source must
/// feed a steady 60), which keeps the emit grid locked to wall-clock. Shares
/// [`genlock_latched_boundary`] with [`genlock_emit_gate`] so the two never disagree on where the
/// boundary sits.
///
/// (#1145) SUPERSEDED as the production shed signal: `dupe_decimation` now keys on the NUMERIC
/// [`genlock_lag_intervals`] (a late over-rate dupe RETIRES rather than emitting a copy). This
/// predicate is retained only as the `lag == 0` equivalence anchor (`genlock_emit_on_time(...) ==
/// (genlock_lag_intervals(...) == 0 && now >= boundary)`) and its own tests; no production path
/// calls it any more.
pub fn genlock_emit_on_time(now_ns: u64, next_boundary_ns: u64, interval_ns: u64) -> bool {
    if interval_ns == 0 {
        return false;
    }
    let boundary = genlock_latched_boundary(now_ns, next_boundary_ns, interval_ns);
    now_ns >= boundary && boundary + interval_ns > now_ns
}

/// #1145 — how many WHOLE emit-boundary intervals `now_ns` sits PAST the pending boundary: `0`
/// when the capture is on-time/surplus (`genlock_emit_on_time` is true) OR still between
/// boundaries (`now < boundary`, where [`genlock_emit_gate`] returns emit=false), and `>= 1` once
/// the gate has fallen behind (the catch-up / #707-resync regime). Shares
/// [`genlock_latched_boundary`] with [`genlock_emit_gate`] / [`genlock_emit_on_time`] so all three
/// agree on where the boundary sits.
///
/// This is the numeric "boundary staleness" signal `crate::dupe_decimation`'s #1145 stale-boundary
/// retirement keys on. [`genlock_emit_on_time`] answers only the binary lag==0 question (defer a
/// dupe on-time); retirement additionally needs the numeric lag to bound itself well below the
/// #707 resync trigger ([`GENLOCK_MAX_CATCHUP_INTERVALS`]): a dupe crossing a boundary at lag `>= 1`
/// is crossing an ALREADY-STALE boundary (the downstream hold for it already happened one interval
/// ago), so shedding the dupe AND advancing that boundary retires the accounting debt at no new
/// downstream cost. The caller guards `interval_ns != 0`.
pub fn genlock_lag_intervals(now_ns: u64, next_boundary_ns: u64, interval_ns: u64) -> u64 {
    if interval_ns == 0 {
        return 0;
    }
    let boundary = genlock_latched_boundary(now_ns, next_boundary_ns, interval_ns);
    if now_ns >= boundary {
        (now_ns - boundary) / interval_ns
    } else {
        0
    }
}

/// #707 — how many WHOLE emit-boundary intervals were SKIPPED (never emitted) between the
/// `next_boundary_ns` this capture loop held BEFORE calling [`genlock_emit_gate`] and the
/// boundary it returned. A normal `emit=false` poll (decimated, between boundaries) leaves the
/// boundary unchanged (0 skipped); a normal `emit=true` poll advances by exactly ONE
/// `interval_ns` (0 skipped — that's the expected single-frame cadence, not a skip). Anything
/// LARGER than one interval means [`genlock_emit_gate`]'s own forward-resync branch fired — a
/// clock discontinuity (a DanteSync NTP/PTP step correction, or a stalled poll) leapt the wall
/// clock past one or more boundaries that were therefore NEVER emitted, which is the exact
/// mechanism a #666/#707-class transient emit-rate deficit would show if a clock step is its
/// cause. This is the missing direct evidence for that specific hypothesis (as
/// [`crate::send_stall`]'s doc comment covers the sibling "blocking network send" hypothesis) —
/// two independent, non-overlapping diagnostics for the same still-open emit-rate-deficit
/// family, so a future recurrence can show WHICH mechanism (or neither) actually fired.
///
/// `old_boundary_ns == 0` is the gate's own "uninitialized" sentinel (see
/// [`genlock_emit_gate`]'s own init branch) — the very first call always advances from 0 to a
/// real boundary and must NEVER be misread as a multi-interval skip, so it always reports 0.
/// A backward step (`new_boundary_ns <= old_boundary_ns`, e.g. the gate's own backward-jump
/// re-latch, or a decimated poll where the boundary is simply unchanged) also reports 0 — this
/// counts only FORWARD skips, the only direction that actually drops un-emitted boundaries.
pub fn boundary_skip_count(old_boundary_ns: u64, new_boundary_ns: u64, interval_ns: u64) -> u64 {
    if interval_ns == 0 || old_boundary_ns == 0 || new_boundary_ns <= old_boundary_ns {
        return 0;
    }
    let advanced = new_boundary_ns - old_boundary_ns;
    (advanced / interval_ns).saturating_sub(1)
}

/// (#1167 v4) The NDI genlock emit timecode (100ns units) for the `repeat_index`-th STARVATION
/// last-frame repeat — the boundary `repeat_index` whole send-fps frames BEFORE the current frame's
/// boundary `base_timecode_100ns` (from [`crate::genlock_stamp::genlock_emit_timecode_100ns`]). When
/// the grabber under-captures, empty-queue 60fps boundaries pass unfilled;
/// [`crate::dupe_decimation::DecimationGate::poll`] reports how many to fill by re-emitting the
/// current GOOD frame, and each repeat MUST carry its own consecutive boundary timecode — a shared
/// timecode would collapse the repeats into ONE slot in the downstream genlock FIFO. One send-fps
/// frame is `10_000_000 / fps` in 100ns units (`10_000_000` = one second in 100ns units). `fps <= 0`
/// returns the base timecode unchanged (guarded divisor, mirroring the module's other timecode math).
/// `repeat_index` is 1-based: `1` = one frame before the current, up to
/// [`crate::dupe_decimation::STARVATION_REPEAT_MAX`].
pub fn starvation_repeat_timecode_100ns(
    base_timecode_100ns: i64,
    repeat_index: u64,
    fps: i64,
) -> i64 {
    if fps <= 0 {
        return base_timecode_100ns;
    }
    let frame_interval_100ns = 10_000_000 / fps;
    base_timecode_100ns - (repeat_index as i64) * frame_interval_100ns
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- genlock_emit_gate (#11) --------------------------------------------
    // interval for 30 fps in ns.
    const I30: u64 = 1_000_000_000 / 30; // 33_333_333

    #[test]
    fn genlock_gate_init_does_not_emit_and_sets_next_boundary() {
        // next_boundary 0 => initialize to the next boundary above `now`, no emit.
        let now = 5 * I30 + 1000; // just past the 5th boundary
        let (emit, next) = genlock_emit_gate(now, 0, I30, false);
        assert!(!emit, "init frame must not emit");
        assert_eq!(next, now - (now % I30) + I30);
        assert!(next > now, "boundary must be strictly after now");
    }

    #[test]
    fn genlock_emit_on_time_true_only_for_a_surplus_crossing_1111() {
        // (#1111) ON-TIME (surplus regime): at/just-past the boundary, the next boundary still
        // in the future -> true. This is the ONLY regime in which dupe_decimation may defer a
        // dupe (lag-neutral, a replacement capture lands in the SAME interval).
        let boundary = 7 * I30;
        assert!(genlock_emit_on_time(boundary, boundary, I30));
        assert!(genlock_emit_on_time(boundary + 5, boundary, I30));
        assert!(genlock_emit_on_time(boundary + I30 - 1, boundary, I30));
    }

    #[test]
    fn genlock_emit_on_time_false_between_boundaries_and_when_late_1111() {
        let boundary = 10 * I30;
        // Between boundaries (now < boundary): genlock_emit_gate returns emit=false here.
        assert!(!genlock_emit_on_time(boundary - 5, boundary, I30));
        // LATE / catch-up (now >= boundary + interval): deferring a dupe here is the lag ratchet.
        assert!(!genlock_emit_on_time(boundary + I30, boundary, I30));
        assert!(!genlock_emit_on_time(boundary + 3 * I30, boundary, I30));
        // interval 0 (genlock off) is never on-time.
        assert!(!genlock_emit_on_time(12_345, 6_789, 0));
    }

    #[test]
    fn genlock_emit_on_time_agrees_with_gate_on_a_surplus_crossing_1111() {
        // Shares genlock_latched_boundary with genlock_emit_gate, so on a surplus crossing the
        // predicate is TRUE exactly when the gate emits with its next boundary strictly ahead.
        let boundary = 4 * I30;
        for delta in [0u64, 5, I30 - 1] {
            let now = boundary + delta;
            let (emit, next) = genlock_emit_gate(now, boundary, I30, false);
            assert_eq!(
                genlock_emit_on_time(now, boundary, I30),
                emit && next > now,
                "delta={delta}"
            );
        }
    }

    #[test]
    fn genlock_lag_intervals_zero_on_time_and_between_boundaries_positive_when_late_1145() {
        let boundary = 9 * I30;
        // Between boundaries (now < boundary): lag 0 (nothing is stale yet).
        assert_eq!(genlock_lag_intervals(boundary - 5, boundary, I30), 0);
        // On-time / surplus (now in [boundary, boundary+interval)): lag 0.
        assert_eq!(genlock_lag_intervals(boundary, boundary, I30), 0);
        assert_eq!(genlock_lag_intervals(boundary + I30 - 1, boundary, I30), 0);
        // Late / catch-up: exactly how many WHOLE intervals past the boundary.
        assert_eq!(genlock_lag_intervals(boundary + I30, boundary, I30), 1);
        assert_eq!(
            genlock_lag_intervals(boundary + 2 * I30 + 7, boundary, I30),
            2
        );
        assert_eq!(genlock_lag_intervals(boundary + 5 * I30, boundary, I30), 5);
        // interval 0 (genlock off) reports 0, never divides by zero.
        assert_eq!(genlock_lag_intervals(12_345, 6_789, 0), 0);
    }

    #[test]
    fn genlock_lag_intervals_is_zero_exactly_when_on_time_or_before_boundary_1145() {
        // lag==0 <=> (genlock_emit_on_time OR now < boundary). Cross-checks the two share the same
        // latched boundary, so retirement (lag>=1) and the #889 defer (lag==0) never disagree.
        let boundary = 6 * I30;
        for delta in [-3i64, 0, 5, I30 as i64 - 1, I30 as i64, 3 * I30 as i64] {
            let now = (boundary as i64 + delta) as u64;
            let lag = genlock_lag_intervals(now, boundary, I30);
            let on_time = genlock_emit_on_time(now, boundary, I30);
            let before = now < boundary;
            assert_eq!(lag == 0, on_time || before, "delta={delta}");
        }
    }

    #[test]
    fn genlock_gate_skips_between_boundaries() {
        // now strictly before the pending boundary => decimate, boundary unchanged.
        let boundary = 10 * I30;
        let now = boundary - 5; // just before
        let (emit, next) = genlock_emit_gate(now, boundary, I30, false);
        assert!(!emit, "capture before the boundary must be decimated");
        assert_eq!(next, boundary, "boundary must not move when skipping");
    }

    #[test]
    fn genlock_gate_emits_at_boundary_and_advances_one_interval() {
        // now exactly at the boundary => emit, advance by exactly one interval.
        let boundary = 7 * I30;
        let (emit, next) = genlock_emit_gate(boundary, boundary, I30, false);
        assert!(emit, "capture at the boundary must emit");
        assert_eq!(next, boundary + I30, "advance exactly one interval");
    }

    #[test]
    fn genlock_gate_emits_just_after_boundary() {
        let boundary = 7 * I30;
        let now = boundary + 100; // first capture after the boundary
        let (emit, next) = genlock_emit_gate(now, boundary, I30, false);
        assert!(emit);
        assert_eq!(next, boundary + I30);
    }

    #[test]
    fn genlock_gate_zero_interval_does_not_panic_and_never_emits() {
        // interval_ns == 0 (genlock off) must NOT panic (no modulo/divide by
        // zero) and must never emit, leaving the boundary untouched.
        let (emit, next) = genlock_emit_gate(123_456_789, 0, 0, false);
        assert!(!emit);
        assert_eq!(next, 0);
        let (emit2, next2) = genlock_emit_gate(999, 555, 0, false);
        assert!(!emit2);
        assert_eq!(next2, 555);
    }

    // #707 — boundary_skip_count: the "was a boundary skipped" diagnostic decision.

    #[test]
    fn skip_count_zero_on_uninitialized_sentinel() {
        // old_boundary_ns == 0 is the gate's own "uninitialized" sentinel — the very first
        // call's huge jump from 0 must never read as a skip.
        assert_eq!(boundary_skip_count(0, 100 * I30, I30), 0);
    }

    #[test]
    fn skip_count_zero_on_unchanged_boundary_decimated_poll() {
        // A decimated (emit=false) poll leaves the boundary unchanged.
        let boundary = 5 * I30;
        assert_eq!(boundary_skip_count(boundary, boundary, I30), 0);
    }

    #[test]
    fn skip_count_zero_on_normal_single_interval_advance() {
        // The expected steady-state emit=true advance: exactly one interval, no skip.
        let old = 5 * I30;
        let new = old + I30;
        assert_eq!(boundary_skip_count(old, new, I30), 0);
    }

    #[test]
    fn skip_count_reports_a_multi_interval_forward_jump() {
        // A clock step (or a stalled poll) that leapt the boundary forward by 6 intervals in
        // one call means 5 boundaries were never emitted.
        let old = 10 * I30;
        let new = old + 6 * I30;
        assert_eq!(boundary_skip_count(old, new, I30), 5);
    }

    #[test]
    fn skip_count_zero_on_backward_step() {
        // A backward clock step re-latches to a SMALLER boundary — never a forward skip.
        let old = 100 * I30;
        let new = 40 * I30;
        assert_eq!(boundary_skip_count(old, new, I30), 0);
    }

    #[test]
    fn skip_count_zero_when_interval_is_zero_genlock_off() {
        assert_eq!(boundary_skip_count(5 * I30, 200 * I30, 0), 0);
    }

    #[test]
    fn skip_count_matches_genlock_emit_gate_forward_resync_live() {
        // End-to-end: drive genlock_emit_gate through a real forward clock STEP (a lag beyond
        // GENLOCK_MAX_CATCHUP_INTERVALS, so it grid-resyncs rather than #707 catching up) and
        // confirm boundary_skip_count reads the same skip the gate's own resync branch took.
        let boundary = 10 * I30;
        let jump = GENLOCK_MAX_CATCHUP_INTERVALS + 4; // 12 intervals — past the catch-up bound
        let jumped_now = boundary + jump * I30 + 500; // 12+ intervals past the pending boundary
        let (emit, next) = genlock_emit_gate(jumped_now, boundary, I30, false);
        assert!(emit, "a capture at/after the boundary always emits");
        assert_eq!(
            boundary_skip_count(boundary, next, I30),
            jump,
            "beyond the catch-up bound the gate's forward-resync must be visible as a {jump}-boundary skip"
        );
    }

    #[test]
    fn emit_gate_catches_up_a_buffered_drain_without_dropping_707_b1() {
        // #707 B1 — the CAM1 FREEZE mechanism. At a matched capture==genlock rate the box
        // captures a steady 60fps (0 capture-dropped), but a CPU-starvation stall (the #752 log
        // storm) delays the emit-thread poll for a few intervals; V4L2 then holds the captured
        // frames (queue depth 4, `capture.rs` `Stream::with_buffers(.., 4)`) and the loop drains
        // them back-to-back at ~the SAME late wall clock.
        //
        // OLD gate: the first drained frame emits, then the forward-resync LEAPS the boundary grid
        // past the rest, so every following buffered frame lands BETWEEN boundaries and is
        // DECIMATED (emit=false) — captured-but-never-emitted. A 4-frame drain => 1 emit + 3
        // dropped => the measured 60->44fps emit collapse and the strih underrun-then-jump freeze.
        //
        // FIXED gate (bounded catch-up): a lag within GENLOCK_MAX_CATCHUP_INTERVALS advances ONE
        // interval per emit, so each buffered (fresh, merely-late) frame fills the next un-emitted
        // boundary — all 4 emit, no drop. Drives the exact drain and asserts every buffered frame
        // emits.
        let i = I30;
        let b0 = 10 * i; // an aligned pending boundary (grid-aligned so the arithmetic is exact)
        let stall_intervals = 4u64; // the emit poll was starved ~4 intervals ...
        let depth = 4u64; // ... and V4L2 held its 4-deep queue meanwhile (capture.rs)
                          // Resume: the loop drains `depth` buffered frames in a tight loop, all at ~the same wall
                          // clock `resume` (a few ns apart — well within one interval).
        let resume = b0 + stall_intervals * i;
        let mut next_boundary = b0;
        let mut emitted = 0u64;
        for k in 0..depth {
            let now = resume + k; // k ns apart — all inside the same interval
            let (emit, nb) = genlock_emit_gate(now, next_boundary, i, false);
            next_boundary = nb;
            if emit {
                emitted += 1;
            }
        }
        assert_eq!(
            emitted,
            depth,
            "every buffered capture in a bounded starvation drain must EMIT (fill its own \
             boundary), not be leaped-past and decimated — a {depth}-frame drain that emits only 1 \
             is the #707 B1 freeze (1 emit + {} captured-but-never-emitted)",
            depth - 1
        );
    }

    #[test]
    fn genlock_gate_advance_is_boundary_plus_interval_not_resync() {
        // Misaligned boundary, now exactly at it (and < boundary+interval, so the
        // resync branch must NOT fire): next must be boundary + interval exactly.
        // With an aligned boundary the resync value coincides with boundary+interval
        // and masks a '+' -> '-' mutation; a misaligned boundary distinguishes them
        // (resync would give 8*I30, the correct advance gives 8*I30 + 5).
        let boundary = 7 * I30 + 5;
        let now = boundary;
        let (emit, next) = genlock_emit_gate(now, boundary, I30, false);
        assert!(emit);
        assert_eq!(next, boundary + I30);
        assert_ne!(next, now - (now % I30) + I30); // != the resync-realigned value
    }

    #[test]
    fn genlock_gate_resyncs_when_far_behind() {
        // A lag BEYOND GENLOCK_MAX_CATCHUP_INTERVALS is a genuine wall-clock discontinuity (a real
        // clock step, not a bounded buffered-drain): emit, then grid-resync forward to just-after
        // `now`, dropping the intervening boundaries (no buffered captures existed to fill them).
        let boundary = 3 * I30;
        let lag = GENLOCK_MAX_CATCHUP_INTERVALS + 4; // safely past the catch-up bound
        let now = boundary + lag * I30 + 17;
        let (emit, next) = genlock_emit_gate(now, boundary, I30, false);
        assert!(emit);
        let realigned = now - (now % I30) + I30;
        assert_eq!(
            next, realigned,
            "beyond the catch-up bound must resync forward, not creep one interval"
        );
        assert!(next > now);
        assert_ne!(next, boundary + I30);
    }

    #[test]
    fn genlock_gate_catches_up_within_the_bound_instead_of_resyncing() {
        // #707 B1 — a MODERATE lag (within GENLOCK_MAX_CATCHUP_INTERVALS, i.e. a bounded V4L2
        // buffered-drain) must advance exactly ONE interval — emit the merely-late frame for the
        // next un-emitted boundary — NOT grid-resync (which would decimate the rest of the drain,
        // the freeze). A misaligned boundary distinguishes the +interval advance (boundary+I) from
        // the resync-realigned value.
        let boundary = 7 * I30 + 5;
        let lag = GENLOCK_MAX_CATCHUP_INTERVALS; // exactly at the bound → still catch up (> is the resync gate)
        let now = boundary + lag * I30 + 11;
        let (emit, next) = genlock_emit_gate(now, boundary, I30, false);
        assert!(emit);
        assert_eq!(
            next,
            boundary + I30,
            "within the catch-up bound: advance ONE interval (fill the next boundary), never leap"
        );
        assert_ne!(
            next,
            now - (now % I30) + I30,
            "must NOT resync-realign within the catch-up bound"
        );
    }

    #[test]
    fn genlock_gate_recovers_after_backward_clock_step() {
        // #131 regression: dantesync acquires NTP late on a cold boot and steps
        // CLOCK_REALTIME BACKWARD below the already-latched boundary. The latched
        // boundary is then many intervals AHEAD of `now` (boundary - now >> interval).
        //
        // Pre-fix the gate had only a FORWARD resync, so `now < boundary` stayed true
        // on every subsequent frame -> emit=false FOREVER -> 0 NDI emitted (capture
        // unaffected). Only a warm `systemctl restart` (next_boundary_ns=0) recovered.
        //
        // Post-fix a symmetric BACKWARD-step guard must re-latch the boundary to the
        // rewound clock and resume emitting within ~1 interval.
        let boundary = 100 * I30; // latched at the pre-rewind ("future") time T
                                  // Clock steps backward by ~3 min worth of intervals (Δ >> interval).
        let rewound = boundary - 90 * I30; // boundary - now == 90 intervals >> interval

        // First frame after the backward step must NOT wedge: it re-latches and the
        // returned boundary must be just above the rewound `now`, not the stale future T.
        let (_emit0, nb0) = genlock_emit_gate(rewound, boundary, I30, false);
        assert!(
            nb0 <= rewound + I30,
            "backward step must re-latch the boundary to the rewound clock \
             (got {nb0}, expected <= {})",
            rewound + I30
        );
        assert_ne!(
            nb0, boundary,
            "must not keep the stale future boundary after a backward step"
        );

        // Drive a few more frames at the rewound clock and confirm emit resumes
        // within ~1 interval (i.e. the gate is no longer wedged at 0fps).
        let mut next_b = nb0;
        let mut emitted = 0;
        for k in 0..3u64 {
            let now = rewound + k * I30 + I30; // step forward one interval each frame
            let (emit, nb) = genlock_emit_gate(now, next_b, I30, false);
            next_b = nb;
            if emit {
                emitted += 1;
            }
        }
        assert!(
            emitted >= 1,
            "emit must resume within ~1 interval after a backward clock step, \
             got {emitted} emits (pre-fix: 0 forever => wedged at 0fps)"
        );
    }

    #[test]
    fn genlock_gate_emits_at_30fps_over_a_60fps_capture_stream() {
        // Drive ~1s of 60 fps captures through the gate; exactly ~30 must emit
        // (the 60->30 decimation), one per wall boundary.
        let cap_interval = 1_000_000_000u64 / 60;
        let mut next_b = 0u64;
        let mut emitted = 0;
        let start = 1_000_000_000u64; // arbitrary absolute ns
        for k in 0..60u64 {
            let now = start + k * cap_interval;
            let (emit, nb) = genlock_emit_gate(now, next_b, I30, false);
            next_b = nb;
            if emit {
                emitted += 1;
            }
        }
        assert!(
            (29..=31).contains(&emitted),
            "60fps capture must decimate to ~30 emits, got {emitted}"
        );
    }

    // #1131 — queue-occupancy-aware catch-up: never grid-resync (multi-slot skip) while a real
    // captured frame is buffered in the V4L2 queue (the issue-1131 judder; the symptom's 0
    // capture-dropped signature PROVES the frames exist, they were just delivered late).

    #[test]
    fn emit_gate_never_resyncs_a_buffered_drain_while_frames_available_1131() {
        // A wall-clock lag BEYOND GENLOCK_MAX_CATCHUP_INTERVALS on a frame that came from a
        // NON-EMPTY queue (`queue_had_frame == true` — the driver already had it buffered): a real
        // captured frame exists to fill the next un-emitted boundary, so the gate must catch up
        // ONE interval (emit for boundary+interval), NEVER grid-resync past it. A misaligned
        // boundary distinguishes the +interval advance from the resync-realigned value.
        let boundary = 7 * I30 + 5;
        let lag = GENLOCK_MAX_CATCHUP_INTERVALS + 3; // 11 intervals — well past the resync bound
        let now = boundary + lag * I30 + 11;
        let (emit, next) = genlock_emit_gate(now, boundary, I30, true);
        assert!(emit, "a capture at/after the boundary always emits");
        assert_eq!(
            next,
            boundary + I30,
            "a buffered frame past the catch-up bound must catch up ONE interval, not grid-resync \
             (else the buffered captured frames behind it are leaped-past and discarded — #1131)"
        );
        assert_ne!(
            next,
            now - (now % I30) + I30,
            "must NOT resync-realign while a buffered frame is available"
        );
    }

    #[test]
    fn emit_gate_buffered_drain_emits_every_frame_with_zero_skip_1131() {
        // End-to-end: the CAM1 judder mechanism. The emit poll is blocked for `block` intervals
        // (a long send/processing hiccup); the V4L2 driver buffers the real captured frames
        // meanwhile (0 capture-dropped), and on resume the loop drains them back-to-back at ~the
        // same wall clock. With the queue-occupancy signal set (buffered frames), EVERY drained
        // frame must EMIT and boundary_skip_count must stay 0 — vs the queue-blind gate which
        // emits 1 and skips `block-1`.
        let i = I30;
        let b0 = 100 * i;
        let block = 10u64; // wall clock advanced ~10 intervals during the block (> the 8 bound)
        let buffered = 6u64; // 6 real captured frames waiting in the queue on resume
        let resume = b0 + block * i;
        let mut next_boundary = b0;
        let mut emitted = 0u64;
        let mut total_skip = 0u64;
        for k in 0..buffered {
            let now = resume + k; // buffered drain: all within one interval
            let prev = next_boundary;
            let (emit, nb) = genlock_emit_gate(now, next_boundary, i, true); // queue non-empty
            total_skip += boundary_skip_count(prev, nb, i);
            next_boundary = nb;
            if emit {
                emitted += 1;
            }
        }
        assert_eq!(
            emitted, buffered,
            "every buffered captured frame must emit (fill its own boundary), not be leaped-past \
             and discarded — a {buffered}-frame drain that emits fewer is the #1131 multi-slot judder"
        );
        assert_eq!(
            total_skip, 0,
            "no boundary may be SKIPPED while buffered captured frames are available (#1131)"
        );
    }

    #[test]
    fn emit_gate_still_resyncs_a_genuine_gap_when_queue_empty_1131() {
        // The #131 cold-boot / true-gap case is UNCHANGED: a frame from an EMPTY queue
        // (`queue_had_frame == false` — the loop genuinely WAITED, the device produced nothing)
        // with a lag beyond the catch-up bound still grid-resyncs (an honest skip — those
        // boundaries had no captured content). This is exactly today's behaviour.
        let boundary = 3 * I30;
        let lag = GENLOCK_MAX_CATCHUP_INTERVALS + 4;
        let now = boundary + lag * I30 + 17;
        let (emit, next) = genlock_emit_gate(now, boundary, I30, false);
        assert!(emit);
        assert_eq!(
            next,
            now - (now % I30) + I30,
            "an empty-queue frame past the catch-up bound must still resync (honest skip, #131)"
        );
        assert!(
            boundary_skip_count(boundary, next, I30) >= 1,
            "the true gap is an honest skip"
        );
    }

    // --- starvation_repeat_timecode_100ns (#1167 v4) ------------------------
    #[test]
    fn starvation_repeat_timecode_steps_back_one_frame_per_index_1167() {
        // 60 fps -> one frame is 10_000_000/60 = 166_666 (100ns units). Repeat k lands k frames
        // before the base boundary, strictly decreasing + distinct so the FIFO gives each its own slot.
        let base = 123_456_789i64;
        let frame = 10_000_000i64 / 60;
        assert_eq!(starvation_repeat_timecode_100ns(base, 1, 60), base - frame);
        assert_eq!(
            starvation_repeat_timecode_100ns(base, 2, 60),
            base - 2 * frame
        );
        assert_eq!(
            starvation_repeat_timecode_100ns(base, 4, 60),
            base - 4 * frame
        );
        // strictly monotone-decreasing in the repeat index (distinct consecutive slots).
        let t1 = starvation_repeat_timecode_100ns(base, 1, 60);
        let t2 = starvation_repeat_timecode_100ns(base, 2, 60);
        assert!(
            t2 < t1 && t1 < base,
            "each earlier repeat gets a strictly earlier timecode"
        );
    }

    #[test]
    fn starvation_repeat_timecode_guards_zero_fps_1167() {
        // fps <= 0 (guarded divisor) returns the base unchanged, never divides by zero.
        assert_eq!(starvation_repeat_timecode_100ns(999, 3, 0), 999);
        assert_eq!(starvation_repeat_timecode_100ns(999, 3, -5), 999);
    }
}
