//! #70 — genlock FIFO preload reserve: the REAL zero-loss fix.
//!
//! The genlock FIFO (#42) consumed exactly one queued frame per wall-clock render
//! tick with ZERO slack, so any NDI arrival jitter emptied the queue → underrun →
//! a dropped/repeated frame (~0.38%/frame measured on each OBS hop by the #68/#69
//! QR instrument). The fix keeps a small jitter buffer: hold consumption until the
//! queue is deeper than `preload`, then consume one per tick.
//!
//! Two facets are guarded here:
//!  1. The PURE decision logic (preload parse/clamp + consume decision) is mirrored
//!     in `camera_box::probe::genlock` and unit-tested — the testable core of the
//!     fix, independent of OBS.
//!  2. A vendored-source guard (same convention as tests/obs_updater_disabled.rs)
//!     asserts the C patch is present in vendor/obs-studio so a future
//!     `git subtree pull` (#44) can't silently revert it.

#![cfg(feature = "probe")]

use camera_box::probe::genlock::{
    genlock_build_drain, genlock_decide, genlock_drop_cap, genlock_empty_run_next,
    genlock_rearm_on_resume, parse_preload, preload_to_ms, steady_state_depth, GenlockDecision,
    GENLOCK_DROP_CAP_RESERVE, GENLOCK_PRELOAD_DEFAULT, GENLOCK_PRELOAD_MAX,
    GENLOCK_REARM_EMPTY_TICKS, MAX_ASYNC_FRAMES,
};

// ---- pure decision logic ---------------------------------------------------

#[test]
fn missing_or_empty_env_uses_default() {
    assert_eq!(parse_preload(None), GENLOCK_PRELOAD_DEFAULT);
    assert_eq!(parse_preload(Some("")), GENLOCK_PRELOAD_DEFAULT);
    assert_eq!(parse_preload(Some("   ")), GENLOCK_PRELOAD_DEFAULT);
}

#[test]
fn valid_value_is_parsed() {
    assert_eq!(parse_preload(Some("0")), 0);
    assert_eq!(parse_preload(Some("1")), 1);
    assert_eq!(parse_preload(Some("2")), 2);
    assert_eq!(parse_preload(Some("5")), 5);
}

#[test]
fn clamp_boundary_at_cap() {
    // Pin the clamp boundary so a mutated comparison can't survive: just below
    // the cap stays itself, exactly at the cap stays itself, above clamps down.
    // The cap is 128 (#97 raised it from 28 to allow ~1 s of video delay).
    assert_eq!(GENLOCK_PRELOAD_MAX, 128);
    assert_eq!(parse_preload(Some("127")), 127);
    assert_eq!(parse_preload(Some("128")), GENLOCK_PRELOAD_MAX); // == 128, unchanged
    assert_eq!(parse_preload(Some("129")), GENLOCK_PRELOAD_MAX); // clamped to 128
                                                                 // The old 28 boundary is now well inside range and must pass through unchanged.
    assert_eq!(parse_preload(Some("28")), 28);
    assert_eq!(parse_preload(Some("30")), 30); // ~1 s @ 30 fps, the headline use case
}

#[test]
fn out_of_range_is_clamped_not_default() {
    // Above the cap → clamp to MAX (NOT silently fall back to default).
    assert_eq!(parse_preload(Some("999")), GENLOCK_PRELOAD_MAX);
    // Negative / garbage → default (mirrors the C strtol guard).
    assert_eq!(parse_preload(Some("-1")), GENLOCK_PRELOAD_DEFAULT);
    assert_eq!(parse_preload(Some("abc")), GENLOCK_PRELOAD_DEFAULT);
    assert_eq!(parse_preload(Some("3x")), GENLOCK_PRELOAD_DEFAULT);
}

#[test]
fn matches_c_strtol_quirks() {
    // The mirror must replicate the C strtol path EXACTLY (the test crate's
    // purpose is to prove the C contract). Two pathological inputs where naive
    // Rust parsing diverges from C:
    //  1. An i64-OVERFLOWING magnitude: C strtol saturates to LONG_MAX, which
    //     passes `v >= 0` and hits the `v > MAX` clamp ⇒ MAX (NOT default).
    assert_eq!(
        parse_preload(Some("99999999999999999999")),
        GENLOCK_PRELOAD_MAX
    );
    //  2. A TRAILING non-digit: C strtol leaves `*end != '\0'` ⇒ default. (A
    //     trailing space must NOT be trimmed-then-accepted.)
    assert_eq!(parse_preload(Some("5 ")), GENLOCK_PRELOAD_DEFAULT);
    // Leading whitespace IS skipped by strtol (and so by the mirror).
    assert_eq!(parse_preload(Some("  2")), 2);
    // A leading '+' sign is accepted by strtol.
    assert_eq!(parse_preload(Some("+3")), 3);
    // A negative magnitude that overflows is still negative-intent ⇒ default.
    assert_eq!(
        parse_preload(Some("-99999999999999999999")),
        GENLOCK_PRELOAD_DEFAULT
    );
    // A lone sign with no digits ⇒ no conversion ⇒ default.
    assert_eq!(parse_preload(Some("-")), GENLOCK_PRELOAD_DEFAULT);
    assert_eq!(parse_preload(Some("+")), GENLOCK_PRELOAD_DEFAULT);
}

#[test]
fn non_genlock_drop_cap_is_max_async_frames() {
    // #97: a NON-genlock source keeps libobs' fixed drop-cap (MAX_ASYNC_FRAMES =
    // 30) regardless of any preload value — those sources never deliberately
    // buffer, so the per-source scaling must NOT apply to them (no memory impact on
    // regular sources, #89). The Rust mirror constant must equal the libobs literal
    // read from the vendored source so the two can't silently diverge.
    assert_eq!(MAX_ASYNC_FRAMES, 30);
    assert_eq!(
        vendored_source::max_async_frames(),
        MAX_ASYNC_FRAMES,
        "libobs MAX_ASYNC_FRAMES changed; update the Rust MAX_ASYNC_FRAMES mirror"
    );
    // Non-genlock cap is fixed at MAX_ASYNC_FRAMES for ANY preload.
    assert_eq!(genlock_drop_cap(false, 0), MAX_ASYNC_FRAMES);
    assert_eq!(genlock_drop_cap(false, 1), MAX_ASYNC_FRAMES);
    assert_eq!(
        genlock_drop_cap(false, GENLOCK_PRELOAD_MAX),
        MAX_ASYNC_FRAMES
    );
}

#[test]
fn genlock_drop_cap_scales_with_preload_and_clamps() {
    // #97: a genlock source's drop-cap = max(MAX_ASYNC_FRAMES, preload + RESERVE),
    // so a deliberately delayed source can hold its full buffer without an overrun
    // force-drain, while a shallow preload keeps the pre-#97 30-frame burst-tolerance
    // floor. The cap MUST sit strictly above the steady-state depth (#102: the
    // consume-when-queued gate parks at `preload`) plus headroom, or normal jitter
    // trips it.
    assert_eq!(GENLOCK_DROP_CAP_RESERVE, 4);
    for preload in [0u32, 1, 2, 30, 100, GENLOCK_PRELOAD_MAX] {
        let cap = genlock_drop_cap(true, preload);
        assert!(
            cap > steady_state_depth(preload),
            "preload={preload}: drop-cap {cap} must exceed steady-state depth {} or \
             normal jitter force-drains the buffer",
            steady_state_depth(preload)
        );
        // The cap NEVER drops below the pre-#97 fixed floor — that floor absorbed
        // NDI catch-up bursts and must be preserved (review finding).
        assert!(
            cap >= MAX_ASYNC_FRAMES,
            "preload={preload}: drop-cap {cap} fell below the MAX_ASYNC_FRAMES floor \
             ({MAX_ASYNC_FRAMES}) — a 6x cut in NDI burst tolerance"
        );
    }
    // Shallow preloads (preload + RESERVE < 30) are floored at MAX_ASYNC_FRAMES, so
    // the production default preload=1 keeps the full 30-frame burst buffer, NOT 5.
    assert_eq!(genlock_drop_cap(true, 0), MAX_ASYNC_FRAMES);
    assert_eq!(genlock_drop_cap(true, 1), MAX_ASYNC_FRAMES); // NOT 5
    assert_eq!(genlock_drop_cap(true, 25), MAX_ASYNC_FRAMES); // 25+4=29 < 30 -> floored
                                                              // At/above the floor crossover the cap is exactly preload + RESERVE.
    assert_eq!(genlock_drop_cap(true, 26), 30); // 26+4 = 30 == floor
    assert_eq!(genlock_drop_cap(true, 30), 34); // ~1 s delay -> 34, above the floor
    assert_eq!(genlock_drop_cap(true, 100), 104);
    assert_eq!(
        genlock_drop_cap(true, GENLOCK_PRELOAD_MAX),
        GENLOCK_PRELOAD_MAX + GENLOCK_DROP_CAP_RESERVE
    );
    // The absolute max is GENLOCK_PRELOAD_MAX + RESERVE (= 132) and the preload
    // input is itself clamped to MAX upstream, so the cap never exceeds 132 even if
    // a caller passes an unclamped value.
    let abs_max = GENLOCK_PRELOAD_MAX + GENLOCK_DROP_CAP_RESERVE;
    assert_eq!(abs_max, 132);
    assert_eq!(genlock_drop_cap(true, 1_000), abs_max);
    assert_eq!(genlock_drop_cap(true, u32::MAX), abs_max); // saturating, no overflow
}

#[test]
fn preload_to_ms_conversion() {
    // ms = frames * 1000 * fps_den / fps_num. The headline case: ~1 s of delay.
    assert_eq!(preload_to_ms(30, 30, 1), 1000); // 30 frames @ 30 fps = exactly 1000 ms
    assert_eq!(preload_to_ms(60, 60, 1), 1000); // 60 frames @ 60 fps = 1000 ms
    assert_eq!(preload_to_ms(1, 30, 1), 33); // one frame @ 30 fps = 33.33 -> 33 (floor)
    assert_eq!(preload_to_ms(0, 30, 1), 0); // no preload -> no delay
                                            // NTSC fractional rate 30000/1001 (≈ 29.97 fps): 30 frames ≈ 1001 ms.
    assert_eq!(preload_to_ms(30, 30000, 1001), 1001);
    // 59.94 fps (60000/1001): 60 frames ≈ 1001 ms.
    assert_eq!(preload_to_ms(60, 60000, 1001), 1001);
    // No valid video info (fps_num == 0) -> 0 (caller shows "fps unknown"), no panic.
    assert_eq!(preload_to_ms(30, 0, 1), 0);
    // No overflow at the cap: 128 * 1000 * 1001 fits comfortably in u64.
    // 128*1000*1001/30000 = 128128000/30000 = 4270.93 -> 4270 (floor).
    assert_eq!(preload_to_ms(GENLOCK_PRELOAD_MAX, 30000, 1001), 4270);
}

#[test]
fn default_is_one_frame() {
    // preload=1 → one frame of reserve = one frame of latency per hop.
    assert_eq!(GENLOCK_PRELOAD_DEFAULT, 1);
}

// ---- #102: consume-when-queued + one-time startup fill (the loss fix) --------
//
// The #70 gate (`depth > preload` → repeat) HELD and repeated the last frame on
// every arrival-jitter dip below the preload reserve, losing one DISTINCT frame
// each time. At a deep #97 preload (=1 s) it was catastrophic: after any drain
// the FIFO had to REFILL PAST the whole reserve (~30 repeats) before one new
// frame escaped (11.6% @ preload=1 → 34.3% @ preload=30, underrun-dominated 990
// vs 72 overruns on the live stream box). #102 makes a deep preload a CLEAN delay
// line: BUILD to preload once at startup (the delay), then consume a distinct
// frame EVERY tick a frame is queued (never repeat-on-hold). Repeat only on a
// TRUE empty (depth==0). The pure decision is `genlock_decide(depth, preload,
// filled) -> { consume, filled }`, mirrored from the C genlock branch.

#[test]
fn build_phase_fills_to_preload_then_latches() {
    // Before the delay line is full (`filled == false`) the FIFO BUILDS: it holds
    // (consume=false) until the queue is deeper than `preload`, establishing the
    // ~preload-frame delay. The moment depth EXCEEDS preload it latches filled and
    // consumes. This one-time startup fill is the ONLY place repeats are emitted.
    let preload = 30;
    // Filling: every depth at/below preload holds and stays unfilled.
    for depth in 0..=preload as usize {
        let d = genlock_decide(depth, preload, false);
        assert_eq!(
            d,
            GenlockDecision {
                consume: false,
                filled: false
            },
            "build phase: depth {depth} <= preload {preload} must HOLD (fill the delay) \
             and stay unfilled"
        );
    }
    // depth > preload → delay established → latch filled AND consume this tick.
    let d = genlock_decide(preload as usize + 1, preload, false);
    assert_eq!(
        d,
        GenlockDecision {
            consume: true,
            filled: true
        },
        "build phase: depth preload+1 means the {preload}-frame delay is buffered — \
         latch filled and emit the first (delayed) distinct frame"
    );
}

#[test]
fn steady_state_consumes_every_queued_distinct_frame() {
    // THE FIX: once filled, consume a distinct frame on EVERY tick a frame is
    // queued (depth >= 1) — NEVER repeat-on-hold while a distinct frame is
    // available. A jitter dip below the preload reserve still delivers a distinct
    // frame; the reserve simply shrinks and refills naturally. This is what takes
    // the loss to ~0 at ANY preload depth.
    for preload in [0u32, 1, 2, 30, 128] {
        for depth in 1..=(preload as usize + 5) {
            let d = genlock_decide(depth, preload, true);
            assert!(
                d.consume,
                "steady state (filled): preload={preload} depth={depth} — a queued \
                 distinct frame MUST be consumed, never held/repeated (the #102 fix)"
            );
            assert!(d.filled, "steady state stays filled while frames flow");
        }
    }
}

#[test]
fn steady_state_repeats_only_on_true_empty() {
    // The ONLY hold in steady state is a genuine empty queue (depth==0): there is
    // no distinct frame to deliver, so the compositor repeats the last one. This is
    // an unavoidable underrun, NOT a repeat-while-a-frame-is-queued. `filled` stays
    // true — a transient empty does not re-trigger the whole startup refill.
    for preload in [0u32, 1, 30, 128] {
        let d = genlock_decide(0, preload, true);
        assert_eq!(
            d,
            GenlockDecision {
                consume: false,
                filled: true
            },
            "steady state: empty FIFO (depth 0, preload={preload}) holds (true \
             underrun) but stays filled — no full-reserve refill run"
        );
    }
}

#[test]
fn deep_preload_never_repeats_while_a_frame_is_queued() {
    // The regression guard for the headline #102 symptom: at the production
    // preload=30, once filled, a depth WELL BELOW the reserve (the jitter dip the
    // old gate repeated on) must STILL consume a distinct frame. The old
    // `depth > preload` gate returned HOLD here (depth 5 <= preload 30) → the
    // ~34% loss. The new gate consumes.
    let preload = 30;
    for depth in [1usize, 2, 5, 15, 29, 30, 31] {
        let d = genlock_decide(depth, preload, true);
        assert!(
            d.consume,
            "deep preload regression: preload={preload} depth={depth} below the \
             reserve must consume a distinct frame, not repeat (the old gate's bug)"
        );
    }
    // Only a true empty holds, even at a deep preload.
    assert!(!genlock_decide(0, preload, true).consume);
}

#[test]
fn preload_zero_consumes_whenever_queued_once_filled() {
    // preload=0 means no delay line: it fills immediately (depth>0 latches filled)
    // and then consumes whenever a frame is queued — i.e. the classic 1-per-tick
    // FIFO with NO repeat-on-hold. (Build phase: depth 0 holds-unfilled, depth 1
    // latches+consumes.)
    assert_eq!(
        genlock_decide(0, 0, false),
        GenlockDecision {
            consume: false,
            filled: false
        }
    );
    assert_eq!(
        genlock_decide(1, 0, false),
        GenlockDecision {
            consume: true,
            filled: true
        }
    );
    // Once filled, consume whenever queued; hold only on empty.
    assert!(genlock_decide(1, 0, true).consume);
    assert!(!genlock_decide(0, 0, true).consume);
}

#[test]
fn steady_state_depth_is_preload_plus_one_at_the_decision_instant() {
    // #102: the FIFO builds to preload+1 (the latch fires when depth first EXCEEDS
    // preload), then at each tick the producer adds one (-> preload+1) and the gate
    // consumes one (-> preload). So the DECISION-instant depth is preload+1, leaving
    // `preload` frames of reserve after consuming — the SAME single-tick jitter
    // tolerance #70 gave (it takes preload+1 consecutive missed deliveries to reach a
    // true empty). The drop-cap must clear this preload+1 peak.
    assert_eq!(steady_state_depth(0), 1);
    assert_eq!(steady_state_depth(1), 2);
    assert_eq!(steady_state_depth(30), 31);
}

#[test]
fn single_tick_jitter_below_the_reserve_still_consumes_no_repeat() {
    // The #102 jitter-tolerance guard: once filled and parked at the decision-instant
    // depth preload+1, a single missed delivery drops the depth to `preload` (>= 1),
    // which STILL consumes a distinct frame — never a repeat (the old #70 gate
    // repeated here, losing the frame). It takes preload+1 consecutive missed
    // deliveries to drain to a true empty (depth 0), the only repeat case.
    for preload in [1u32, 2, 30] {
        let p = preload as usize;
        // parked at preload+1; lose one delivery -> depth preload (>= 1) -> still consume.
        assert!(genlock_decide(p, preload, true).consume);
        // every non-empty depth below the reserve still consumes (no repeat-on-hold).
        for depth in 1..=p {
            assert!(
                genlock_decide(depth, preload, true).consume,
                "preload={preload} depth={depth}: a queued frame below the reserve must \
                 still consume, never repeat (the old #70 gate's bug)"
            );
        }
        // only a fully drained queue (depth 0) repeats.
        assert!(!genlock_decide(0, preload, true).consume);
    }
}

// ---- #116: drain-to-target at build latch + on preload change ----------------
//
// The #102 build latch (`genlock_decide`) latched `filled=true` the instant
// `queue_depth > preload`, at WHATEVER depth the NDI startup burst left in the
// queue, and never trimmed down. So two inputs with different startup bursts froze
// at different depths (different latency → camera time-jump on switch), a preload
// DECREASE re-latched at the old deep depth (the lower delay never took effect —
// "only goes up"), and a restart's random arrival phase gave a non-deterministic
// depth. #116 adds `genlock_build_drain(queue_depth, preload)`: at the build latch
// (and re-armed on a preload change), erase the OLDEST `queue_depth - target`
// frames so every input settles at exactly `target = steady_state_depth(preload) =
// preload + 1`, regardless of startup burst → equal cams, restart-deterministic,
// bidirectional preload. The drain fires ONLY at the build latch / preload change —
// NEVER in steady state (the #102 consume-when-queued 0-loss gate is untouched).

#[test]
fn build_drain_trims_burst_to_target_depth() {
    // The headline #116 fix: when the build latch fires (queue_depth > preload), the
    // FIFO must erase the excess oldest frames so it settles at exactly the target
    // depth (preload + 1), NOT freeze at the startup-burst depth. The drain count is
    // `queue_depth - target`. After the drain the FIFO holds `target` frames; the
    // same-tick consume then leaves `preload` (the steady-state reserve).
    for preload in [0u32, 1, 2, 30, 128] {
        let target = steady_state_depth(preload) as usize; // preload + 1
                                                           // A queue exactly at target needs no trim (the latch fires at preload+1).
        assert_eq!(
            genlock_build_drain(target, preload),
            0,
            "preload={preload}: depth already at target {target} → drain 0"
        );
        // A deep startup burst trims down to target.
        for burst_extra in [1usize, 4, 10, 50] {
            let depth = target + burst_extra;
            assert_eq!(
                genlock_build_drain(depth, preload),
                burst_extra,
                "preload={preload}: build latch at depth {depth} must drain \
                 {burst_extra} oldest frames to reach target {target}"
            );
        }
    }
}

#[test]
fn build_drain_is_zero_below_the_latch_and_in_steady_state() {
    // The drain fires ONLY at the build latch (queue_depth > preload). Below the
    // latch (still building) there is nothing to trim. And a queue at/below target
    // must never trim (no negative / wraparound drain).
    for preload in [0u32, 1, 30, 128] {
        // Still building (depth <= preload): no drain.
        for depth in 0..=preload as usize {
            assert_eq!(
                genlock_build_drain(depth, preload),
                0,
                "preload={preload} depth={depth}: still building, nothing to drain"
            );
        }
        // At target (preload+1): exactly the latch instant, depth == target → 0.
        assert_eq!(
            genlock_build_drain(steady_state_depth(preload) as usize, preload),
            0
        );
    }
}

#[test]
fn two_different_bursts_settle_at_same_target_depth() {
    // Symptom 1: cameras with IDENTICAL preload but DIFFERENT startup bursts froze at
    // different depths (different latency). After the build drain BOTH settle at the
    // identical target depth → equal per-cam latency, no time-jump on switch.
    let preload = 1; // the live default (all inputs preload=1)
    let target = steady_state_depth(preload) as usize;
    // cam A: shallow burst (depth 2), cam B: deep burst (depth 6) — the live spread.
    let depth_a = 2usize; // already at target
    let depth_b = 6usize; // the deep NDI burst (live: cam5 depth 6)
    let settled_a = depth_a - genlock_build_drain(depth_a, preload);
    let settled_b = depth_b - genlock_build_drain(depth_b, preload);
    assert_eq!(settled_a, target, "cam A settles at target");
    assert_eq!(
        settled_b, target,
        "cam B (deep burst) settles at SAME target"
    );
    assert_eq!(
        settled_a, settled_b,
        "both cams settle at identical depth regardless of startup burst (#116 symptom 1)"
    );
}

#[test]
fn preload_decrease_drains_immediately_to_new_lower_target() {
    // Symptom 2 ("only goes up"): a preload DECREASE re-armed the latch but the deep
    // queue immediately re-latched filled at the OLD depth — the lower delay never
    // took effect. With the build drain, after the re-arm the next build latch trims
    // the deep queue straight down to the NEW (lower) target → the delay drops at
    // once. Model: deep queue at the old steady depth, preload lowered, re-armed.
    let old_preload = 30u32; // ~1 s delay
    let new_preload = 5u32; // operator dials it DOWN
    let new_target = steady_state_depth(new_preload) as usize; // 6
                                                               // The FIFO is parked deep at the old steady depth (old preload+1 = 31).
    let deep_depth = steady_state_depth(old_preload) as usize; // 31
                                                               // After the preload-change re-arm, the build latch fires at deep_depth >
                                                               // new_preload and drains down to the NEW target.
    let drained = genlock_build_drain(deep_depth, new_preload);
    let settled = deep_depth - drained;
    assert_eq!(
        settled, new_target,
        "preload decrease 30→5 must drain the deep FIFO straight to the new lower \
         target {new_target} (not stay stuck at the old {deep_depth}) — the delay \
         actually DROPS (#116 symptom 2, bidirectional knob)"
    );
    assert!(
        settled < deep_depth,
        "the depth (delay) must DECREASE, not stay/grow — the 'only goes up' bug"
    );
}

#[test]
fn preload_increase_builds_up_to_new_higher_target() {
    // The increase path: dialing preload UP must rebuild to the new (deeper) target.
    // On an increase the current depth is BELOW the new preload, so the build latch
    // holds (genlock_decide build branch) until the FIFO fills past the new preload —
    // no drain (nothing to trim while building UP). The drain is 0 until depth
    // exceeds the new preload; at the new latch instant (depth = new target) it's 0.
    let new_preload = 30u32; // dialed UP from a shallow value
    let new_target = steady_state_depth(new_preload) as usize; // 31
                                                               // While filling up to the deeper delay (depth <= new preload): no drain, hold.
    for depth in 0..=new_preload as usize {
        assert_eq!(
            genlock_build_drain(depth, new_preload),
            0,
            "increase: building up at depth {depth} (<= preload {new_preload}) → no drain"
        );
        // genlock_decide still HOLDS (building) at these depths when not yet filled.
        assert!(!genlock_decide(depth, new_preload, false).consume);
    }
    // The latch fires at the new target depth (preload+1) with 0 drain.
    assert_eq!(genlock_build_drain(new_target, new_preload), 0);
    assert!(genlock_decide(new_target, new_preload, false).consume);
}

#[test]
fn rebuild_from_empty_settles_at_identical_deterministic_depth() {
    // Symptom 3 (restart non-determinism): each boot the random NDI arrival phase
    // left a different startup-burst depth, which froze at a different latency. With
    // the build drain, ANY burst depth ≥ target settles at the SAME deterministic
    // target → restart-deterministic latency. Sweep a range of possible startup
    // bursts; every one must settle identically.
    let preload = 1u32;
    let target = steady_state_depth(preload) as usize;
    let mut settled_depths = std::collections::BTreeSet::new();
    for burst in target..=target + 40 {
        // any boot-time burst depth
        let settled = burst - genlock_build_drain(burst, preload);
        settled_depths.insert(settled);
    }
    assert_eq!(
        settled_depths.len(),
        1,
        "every possible startup burst must settle at ONE deterministic depth; got {settled_depths:?}"
    );
    assert_eq!(
        *settled_depths.iter().next().unwrap(),
        target,
        "the single deterministic settle depth must be the target (preload+1)"
    );
}

#[test]
fn steady_state_consume_gate_unchanged_by_116() {
    // PRESERVE #102: the steady-state consume-when-queued behavior (the proven
    // 0-loss gate) is UNCHANGED. The drain is a SEPARATE function that fires only at
    // the build latch; it must NEVER alter the steady-state consume decision, and in
    // steady state (filled=true) there is no build latch so no drain is taken by the
    // caller. genlock_decide's steady branch still consumes on every queued frame.
    for preload in [0u32, 1, 2, 30, 128] {
        for depth in 1..=(preload as usize + 5) {
            // Steady state: still consume every queued distinct frame (the #102 fix).
            assert!(
                genlock_decide(depth, preload, true).consume,
                "preload={preload} depth={depth}: #102 steady consume-when-queued must \
                 be untouched by #116"
            );
        }
        // True empty still holds, filled stays set (no startup refill re-trigger).
        assert_eq!(
            genlock_decide(0, preload, true),
            GenlockDecision {
                consume: false,
                filled: true
            }
        );
    }
}

// ---- #126: reconnect re-arm (upstream OBS restart → rebuild the reserve) -----
//
// On an upstream (strih) OBS restart the downstream (stream) NDI source underruns to
// EMPTY, but DistroAV's default KEEP_CONTENT blocks the NULL-emit reset path, and an
// underrun (not an overrun) never fires the cache_video force-drain reset — so
// `genlock_filled` stays TRUE. The #102 steady branch then consumes 1/tick the moment
// the queue refills WITHOUT rebuilding the preload reserve, so the ~26-frame video
// delay silently collapses to ~0 (A/V drift) until a manual nudge.
//
// #126 tracks consecutive true-empty (underrun) ticks; when frames RESUME after a
// SUSTAINED empty run (>= GENLOCK_REARM_EMPTY_TICKS) it re-arms `genlock_filled=false`
// so the existing #102 build path + #116 drain rebuild the reserve to exactly
// `preload+1` deterministically — no manual nudge. The threshold is large enough that
// normal jitter (esp. shallow cam preload=1) NEVER spuriously re-arms.

#[test]
fn sustained_empty_then_resume_rearms_and_rebuilds_deep_preload() {
    // The headline scenario: deep preload=26 (the ~0.9s stream-box video delay). A
    // real reconnect sustains true-empties past the threshold; on resume the source
    // re-arms and the build path + #116 drain rebuild the reserve to preload+1 = 27.
    let preload = 26u32;
    let target = steady_state_depth(preload) as usize; // 27

    // Steady state, then a sustained disconnect: the empty-run counter climbs.
    let mut empty_run = 0u32;
    for _ in 0..GENLOCK_REARM_EMPTY_TICKS {
        // No re-arm WHILE empty (ready_async_frame is not even reached at num==0);
        // the counter just accumulates per the empty-tick model.
        empty_run = genlock_empty_run_next(empty_run, /*consumed=*/ false);
    }
    assert_eq!(empty_run, GENLOCK_REARM_EMPTY_TICKS);

    // Frames RESUME (queue refills): the re-arm decision is taken on the resume tick.
    let filled_before_resume = true;
    let rearm = genlock_rearm_on_resume(empty_run, filled_before_resume);
    assert!(
        rearm,
        "a sustained empty (>= threshold) then resume must re-arm"
    );

    // Re-armed: filled=false → the FIFO re-enters the #102 BUILD phase. It holds while
    // depth <= preload, then latches at the first tick depth > preload, and #116 drains
    // any startup burst straight down to the deterministic target (preload+1).
    let filled = !rearm; // false
                         // Build phase holds until past preload.
    for depth in 0..=preload as usize {
        assert!(
            !genlock_decide(depth, preload, filled).consume,
            "depth={depth}: must HOLD (rebuild) while filling back to the preload reserve"
        );
    }
    // First tick past preload: latch + consume; the deep refill burst is drained to target.
    let d = genlock_decide(preload as usize + 1, preload, filled);
    assert!(
        d.consume && d.filled,
        "latch fires once depth exceeds preload again"
    );
    // Any refill burst (>= target) settles at the deterministic target = preload+1 = 27.
    for burst in target..=target + 40 {
        let settled = burst - genlock_build_drain(burst, preload);
        assert_eq!(
            settled, target,
            "burst={burst}: the rebuilt reserve must settle at preload+1 ({target})"
        );
    }
}

#[test]
fn brief_empty_then_resume_does_not_rearm_no_rebuild_hold() {
    // The jitter-safety guard (the dangerous direction): a BRIEF empty (1..3 ticks),
    // e.g. ordinary NDI arrival jitter at the shallow cam preload=1, must NEVER re-arm.
    // A spurious re-arm would force a ~preload-frame rebuild HOLD on every jitter blip.
    for preload in [1u32, 2, 26] {
        for brief in 1..GENLOCK_REARM_EMPTY_TICKS.min(4) {
            let mut empty_run = 0u32;
            for _ in 0..brief {
                empty_run = genlock_empty_run_next(empty_run, false);
            }
            assert_eq!(empty_run, brief);
            assert!(
                !genlock_rearm_on_resume(empty_run, true),
                "preload={preload}: a brief empty of {brief} (< threshold \
                 {GENLOCK_REARM_EMPTY_TICKS}) must NOT re-arm — no spurious rebuild hold"
            );
            // Because filled STAYS true, the #102 steady gate keeps consuming on resume
            // (no rebuild hold). The reserve is preserved across the transient dip.
            assert!(
                genlock_decide(1, preload, /*filled=*/ true).consume,
                "preload={preload}: after a brief dip the steady gate keeps consuming \
                 (no rebuild) — the reserve is preserved"
            );
        }
    }
}

#[test]
fn empty_run_counter_resets_on_each_distinct_consume() {
    // The counter only sustains across CONSECUTIVE empties; any consumed frame zeroes
    // it. So a queue that flickers empty/non-empty (jitter) can never accumulate to the
    // threshold — only a genuine sustained disconnect can.
    let mut empty_run = 0u32;
    // 10 cycles of {1 empty, 1 consume} — jitter that never sustains.
    for _ in 0..10 {
        empty_run = genlock_empty_run_next(empty_run, false); // empty
        assert!(
            !genlock_rearm_on_resume(empty_run, true),
            "1 empty never re-arms"
        );
        empty_run = genlock_empty_run_next(empty_run, true); // consume
        assert_eq!(empty_run, 0, "a consume must reset the empty-run counter");
    }
}

#[test]
fn rearm_preserves_102_steady_consume_and_116_drain() {
    // PRESERVE #102 + #116: the re-arm reuses the EXISTING build+drain path and adds NO
    // new draining logic. In steady state with no sustained empty, behavior is exactly
    // the proven #102 gate; the #116 drain is unchanged.
    for preload in [0u32, 1, 2, 26, 30, 128] {
        // No sustained empty → no re-arm → #102 steady consume-when-queued is untouched.
        assert!(!genlock_rearm_on_resume(0, true));
        for depth in 1..=(preload as usize + 5) {
            assert!(
                genlock_decide(depth, preload, true).consume,
                "preload={preload} depth={depth}: #102 steady consume must survive #126"
            );
        }
        // True empty in steady state still holds, filled stays set (no spurious refill).
        assert_eq!(
            genlock_decide(0, preload, true),
            GenlockDecision {
                consume: false,
                filled: true
            }
        );
        // #116 build drain unchanged: trims to target = preload+1.
        let target = steady_state_depth(preload) as usize;
        assert_eq!(genlock_build_drain(target + 7, preload), 7);
        assert_eq!(genlock_build_drain(target, preload), 0);
    }
}

// ---- #235: ONE user-facing genlock latency knob (ms), frames in parens ------
//
// #235 consolidates the two confusing latency knobs (OBS_GENLOCK_PRELOAD_FRAMES +
// OBS_GENLOCK_RESERVE_MS, where reserve overrode preload ONLY under TS_ALIGN) into a
// SINGLE canonical ms knob OBS_GENLOCK_LATENCY_MS, with OBS_GENLOCK_RESERVE_MS aliased
// for back-compat (so prod reserve=3 maps cleanly to latency_ms=3). preload becomes an
// INTERNAL auto-derived FIFO depth, never a competing latency knob. The display is
// "N ms (≈ M frames @ Ffps)" — ms primary, frames in parens. These pure-logic tests
// pin the resolution/aliasing, the ms↔frames display math, the auto-preload depth, and
// the label format; the vendored-source guard below pins the C/cpp side.
mod single_latency_knob {
    use camera_box::probe::genlock::{
        effective_latency_ms, format_latency_label, genlock_auto_preload, ms_to_frames,
        preload_to_ms, resolve_latency_ms, GENLOCK_AUTO_PRELOAD_MIN, GENLOCK_LATENCY_MS_DEFAULT,
        GENLOCK_LATENCY_MS_MAX,
    };

    // #245: per-source latency override — a source's OWN latency_ms (>0) beats the global
    // default; 0 follows the global. Mirror of the C release-deadline gate in obs-source.c
    // ready_async_frame: reserve_ms = source->genlock_latency_ms > 0 ?
    // source->genlock_latency_ms : genlock_reserve_ms(). This restores the per-source
    // control #235 removed (the live-event regression): one global default, each NDI
    // source free to hold a DIFFERENT latency.
    #[test]
    fn per_source_latency_overrides_global_when_set() {
        // A source with its OWN latency holds THAT value regardless of the global default.
        assert_eq!(effective_latency_ms(1000, 3), 1000);
        assert_eq!(effective_latency_ms(50, 3), 50);
        // Two sources, one global default -> DIFFERENT resolved latencies (the #245 ask).
        assert_eq!(effective_latency_ms(1000, 33), 1000);
        assert_eq!(effective_latency_ms(33, 33), 33);
        // A source left at 0 follows the global default (incl. global 0 = whole-frame path).
        assert_eq!(effective_latency_ms(0, 3), 3);
        assert_eq!(effective_latency_ms(0, 0), 0);
    }

    #[test]
    fn latency_default_is_zero_disabled() {
        // Neither knob set ⇒ 0 = no ms latency ⇒ the whole-frame preload fallback path
        // (full back-compat with a deploy that sets neither env).
        assert_eq!(GENLOCK_LATENCY_MS_DEFAULT, 0);
        assert_eq!(resolve_latency_ms(None, None), 0);
    }

    #[test]
    fn canonical_latency_knob_is_used_when_set() {
        // OBS_GENLOCK_LATENCY_MS is THE knob: when set & valid it is the resolved latency.
        assert_eq!(resolve_latency_ms(Some("3"), None), 3);
        assert_eq!(resolve_latency_ms(Some("10"), None), 10);
        assert_eq!(resolve_latency_ms(Some("0"), None), 0); // explicit 0 = disabled
    }

    #[test]
    fn reserve_ms_is_the_back_compat_alias() {
        // OBS_GENLOCK_RESERVE_MS still works as the alias when the canonical knob is
        // unset — so existing deploys/scripts/the #128 wrapper that set RESERVE_MS keep
        // working. Prod's reserve=3 maps cleanly to latency_ms=3.
        assert_eq!(resolve_latency_ms(None, Some("3")), 3);
        assert_eq!(resolve_latency_ms(None, Some("10")), 10);
    }

    #[test]
    fn canonical_knob_wins_over_the_alias() {
        // If BOTH are set, the canonical OBS_GENLOCK_LATENCY_MS takes precedence over the
        // deprecated alias — no ambiguous dual-knob precedence.
        assert_eq!(resolve_latency_ms(Some("5"), Some("3")), 5);
        // A canonical 0 (explicit disable) still wins over a non-zero alias: the user who
        // sets the new knob owns the value, even to disable it.
        assert_eq!(resolve_latency_ms(Some("0"), Some("3")), 0);
    }

    #[test]
    fn invalid_canonical_falls_back_to_the_alias() {
        // An unset/junk canonical knob falls through to the alias (same strtol contract as
        // parse_reserve_ms: empty/junk/negative ⇒ treated as unset for the fall-through).
        assert_eq!(resolve_latency_ms(Some(""), Some("3")), 3);
        assert_eq!(resolve_latency_ms(Some("   "), Some("3")), 3);
        assert_eq!(resolve_latency_ms(Some("abc"), Some("3")), 3);
        assert_eq!(resolve_latency_ms(Some("-1"), Some("3")), 3);
        // strtol quirk parity: a trailing non-digit makes the canonical "unset" ⇒ alias.
        assert_eq!(resolve_latency_ms(Some("5 "), Some("3")), 3);
    }

    #[test]
    fn latency_is_clamped_to_max() {
        assert_eq!(GENLOCK_LATENCY_MS_MAX, 100);
        assert_eq!(resolve_latency_ms(Some("100"), None), 100);
        assert_eq!(
            resolve_latency_ms(Some("101"), None),
            GENLOCK_LATENCY_MS_MAX
        ); // clamp
        assert_eq!(
            resolve_latency_ms(Some("99999"), None),
            GENLOCK_LATENCY_MS_MAX
        ); // overflow
           // The alias is clamped on the same scale.
        assert_eq!(
            resolve_latency_ms(None, Some("250")),
            GENLOCK_LATENCY_MS_MAX
        );
    }

    #[test]
    fn ms_to_frames_is_the_inverse_of_preload_to_ms() {
        // The display frame-equivalent: frames ≈ round(ms * fps_num / (1000 * fps_den)).
        // 30fps (30000/1001): one frame ≈ 33.4ms.
        assert_eq!(ms_to_frames(0, 30000, 1001), 0);
        assert_eq!(ms_to_frames(33, 30000, 1001), 1); // ~one frame, rounds to 1
        assert_eq!(ms_to_frames(34, 30000, 1001), 1);
        assert_eq!(ms_to_frames(100, 30000, 1001), 3); // ~3 frames
                                                       // 30/1 exact: one frame = 33.33ms, so 3ms ≈ 0 frames (sub-frame, the whole point).
        assert_eq!(ms_to_frames(3, 30, 1), 0);
        assert_eq!(ms_to_frames(33, 30, 1), 1);
        assert_eq!(ms_to_frames(50, 30, 1), 2); // 1.5 frames rounds to 2
                                                // 60fps: one frame = 16.67ms.
        assert_eq!(ms_to_frames(17, 60, 1), 1);
        assert_eq!(ms_to_frames(50, 60, 1), 3);
        // fps unknown ⇒ 0 (caller shows "fps unknown", never divides by zero).
        assert_eq!(ms_to_frames(33, 0, 1), 0);
    }

    #[test]
    fn ms_to_frames_round_trips_a_whole_frame_count() {
        // A whole-frame latency converts to ms via preload_to_ms and back to the SAME
        // frame count — the display is self-consistent for the operator.
        for (fps_num, fps_den) in [(30000u32, 1001u32), (30, 1), (60, 1), (25, 1)] {
            for frames in [1u32, 2, 3, 5, 10] {
                let ms = preload_to_ms(frames, fps_num, fps_den);
                assert_eq!(
                    ms_to_frames(ms as u32, fps_num, fps_den),
                    frames,
                    "whole-frame round-trip {frames}f @ {fps_num}/{fps_den} ({ms}ms)"
                );
            }
        }
    }

    #[test]
    fn auto_preload_keeps_a_min_resilience_depth() {
        // preload is now INTERNAL: the ms deadline holds latency, the FIFO only needs a
        // small jitter/dropout buffer. Auto-derive a depth of at least the min (>=1 frame
        // so the #110 0-loss floor holds) regardless of latency_ms — the depth does NOT
        // add latency (the ms reserve governs the held delay, not the FIFO depth).
        // Pin the min depth at exactly 1 (>= 1 frame so the #110 0-loss floor holds; equals
        // the historical default preload, preserving the validated prod behavior).
        assert_eq!(
            GENLOCK_AUTO_PRELOAD_MIN, 1,
            "min depth must be exactly 1 frame (the #110 0-loss floor)"
        );
        assert_eq!(genlock_auto_preload(0), GENLOCK_AUTO_PRELOAD_MIN);
        assert_eq!(genlock_auto_preload(3), GENLOCK_AUTO_PRELOAD_MIN); // prod latency_ms=3
        assert_eq!(genlock_auto_preload(10), GENLOCK_AUTO_PRELOAD_MIN);
        assert_eq!(genlock_auto_preload(100), GENLOCK_AUTO_PRELOAD_MIN);
        // Whatever the latency, the auto depth is never below the min (the resilience floor).
        for ms in [0u32, 1, 2, 3, 5, 33, 100] {
            assert!(
                genlock_auto_preload(ms) >= GENLOCK_AUTO_PRELOAD_MIN,
                "auto preload for {ms}ms must be >= the min resilience depth"
            );
        }
    }

    #[test]
    fn label_is_ms_primary_with_frames_in_parens() {
        // The user's exact ask: "genlock latency = N ms (≈ M frames @ 30fps)" — ms first,
        // frame-equivalent in parentheses. 30000/1001 ≈ 29.970 fps.
        let l = format_latency_label(3, 30000, 1001);
        assert!(l.contains("3 ms"), "ms is primary: {l}");
        assert!(l.contains('('), "frame-equivalent is parenthesized: {l}");
        assert!(l.contains("frame"), "frame-equivalent is shown: {l}");
        // 3ms @ 30fps ≈ 0 frames (sub-frame) — the headline that the operator no longer
        // has to know about whole frames.
        assert!(l.contains("0 frame"), "3ms is sub-frame (≈0 frames): {l}");
        // The ms value precedes the '(' (ms primary, frames secondary).
        let paren = l.find('(').expect("has a paren");
        let ms_pos = l.find("3 ms").expect("has the ms value");
        assert!(
            ms_pos < paren,
            "ms value must come BEFORE the parenthesized frames: {l}"
        );
    }

    #[test]
    fn label_shows_the_frame_equivalent_for_a_multi_frame_latency() {
        // 100ms @ 30fps ≈ 3 frames — the parens carry the real frame context.
        let l = format_latency_label(100, 30, 1);
        assert!(l.contains("100 ms"), "{l}");
        assert!(l.contains("3 frame"), "100ms ≈ 3 frames @ 30fps: {l}");
    }

    #[test]
    fn label_handles_unknown_fps() {
        // No valid video info yet ⇒ a clear "fps unknown" label, never a divide-by-zero.
        let l = format_latency_label(3, 0, 1);
        assert!(l.contains("3 ms"), "ms still shown when fps unknown: {l}");
        assert!(
            l.to_lowercase().contains("fps unknown") || l.contains("? frame"),
            "fps-unknown label is explicit: {l}"
        );
    }
}

// ---- vendored-source guard (the C patch must stay applied) ------------------

mod vendored_source {
    use camera_box::probe::genlock::{GENLOCK_PRELOAD_MAX, GENLOCK_REARM_EMPTY_TICKS};
    use std::path::PathBuf;

    pub fn vendor_file(rel: &str) -> String {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
    }

    fn squish(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    const OBS_SOURCE: &str = "vendor/obs-studio/libobs/obs-source.c";
    const OBS_INTERNAL: &str = "vendor/obs-studio/libobs/obs-internal.h";
    const OBS_API: &str = "vendor/obs-studio/libobs/obs.h";
    pub const NDI_SOURCE: &str = "vendor/distroav/src/ndi-source.cpp";
    pub const WINDOWS_GENLOCK_WF: &str = ".github/workflows/windows-genlock.yml";

    /// Read the libobs `#define MAX_ASYNC_FRAMES <n>` literal from the vendored
    /// source so the preload-cap invariant tracks upstream instead of being
    /// hard-coded twice.
    pub fn max_async_frames() -> u32 {
        let src = vendor_file(OBS_SOURCE);
        for line in src.lines() {
            let l = line.trim();
            if let Some(rest) = l.strip_prefix("#define MAX_ASYNC_FRAMES ") {
                return rest
                    .split_whitespace()
                    .next()
                    .and_then(|t| t.parse::<u32>().ok())
                    .unwrap_or_else(|| {
                        panic!("could not parse MAX_ASYNC_FRAMES from {OBS_SOURCE}")
                    });
            }
        }
        panic!("MAX_ASYNC_FRAMES not found in {OBS_SOURCE}");
    }

    #[test]
    fn preload_env_is_read() {
        let src = squish(&vendor_file(OBS_SOURCE));
        assert!(
            src.contains("getenv(\"OBS_GENLOCK_PRELOAD_FRAMES\")"),
            "{OBS_SOURCE}: #70 genlock preload patch missing — OBS_GENLOCK_PRELOAD_FRAMES \
             is no longer read. A `git subtree pull` (#44) likely reverted it; re-apply."
        );
    }

    #[test]
    fn fifo_consumes_when_queued_with_startup_fill() {
        let src = squish(&vendor_file(OBS_SOURCE));
        // #102: the genlock branch must use the NEW consume-when-queued decision
        // (genlock_decide) — BUILD to preload once, then consume a distinct frame on
        // every tick a frame is queued, repeating only on a true empty. The old
        // hard-hold gate (`genlock_should_consume(num, preload)` → repeat whenever
        // depth<=preload) is GONE: it lost a distinct frame on every jitter dip
        // (11.6%→34.3% on the live rig). A subtree pull (#44) re-introducing the old
        // gate would silently revert the loss fix.
        assert!(
            src.contains(
                "genlock_decide(source->async_frames.num, preload, source->genlock_filled)"
            ),
            "{OBS_SOURCE}: #102 — the genlock_fifo branch no longer uses the \
             consume-when-queued decision (genlock_decide with the startup-fill latch). \
             The #70 hard-hold repeat-on-underrun gate is back; re-apply the #102 fix."
        );
        // The old hard-hold gate string must NOT be back (it caused the loss).
        assert!(
            !src.contains("genlock_should_consume(source->async_frames.num, preload)"),
            "{OBS_SOURCE}: #102 — the old hard-hold gate genlock_should_consume is back; \
             it repeats-on-hold and loses a distinct frame per jitter dip. Use \
             genlock_decide (consume-when-queued)."
        );
        // The one-time startup-fill latch must be present (the delay-line build).
        assert!(
            src.contains("genlock_filled"),
            "{OBS_SOURCE}: #102 — the startup-fill latch (genlock_filled) is missing; \
             without it the FIFO cannot BUILD the preload delay then switch to \
             consume-when-queued. Re-apply."
        );
        // The audit counters that prove underruns → ~0 must still be wired.
        assert!(
            src.contains("source->genlock_underruns++"),
            "{OBS_SOURCE}: #70/#102 — the genlock underrun audit counter is gone; re-apply."
        );
    }

    #[test]
    fn timestamp_aligned_release_present() {
        // #136: the genlock_fifo branch must offer the timestamp-aligned release path
        // (multi-source IN-SYNC) — env-gated (OBS_GENLOCK_TS_ALIGN, default OFF) + a
        // per-frame wall-clock guard, presenting the frame captured at
        // present_ts = wall_now - preload*interval. A subtree pull (#44) dropping it
        // silently reverts the desync fix. Mirror of src/probe/genlock.rs genlock_release.
        use camera_box::probe::genlock::{WALLCLOCK_TS_MAX_NS, WALLCLOCK_TS_MIN_NS};
        let src = squish(&vendor_file(OBS_SOURCE));
        assert!(
            src.contains("getenv(\"OBS_GENLOCK_TS_ALIGN\")"),
            "{OBS_SOURCE}: #136 — the timestamp-aligned release gate (OBS_GENLOCK_TS_ALIGN) is gone; re-apply."
        );
        assert!(
            src.contains("genlock_is_wallclock_ts(next_frame->timestamp)"),
            "{OBS_SOURCE}: #136 — the ts-align path no longer guards on \
             genlock_is_wallclock_ts(next_frame->timestamp) (the count-gate fallback for \
             non-camera sources); re-apply."
        );
        assert!(
            src.contains("genlock_present_ts(genlock_wall_now_ns(), preload, interval)"),
            "{OBS_SOURCE}: #136 — the presentation deadline (genlock_present_ts from the real \
             wall clock genlock_wall_now_ns) is gone; re-apply."
        );
        // #136 boundary-churn fix: the C genlock_present_ts BODY must carry the +interval/2
        // half-interval bias (mirror of src/probe/genlock.rs genlock_present_ts'
        // .saturating_add(interval_ns / 2)). Without asserting the body, a subtree pull (#44)
        // could revert it to `return base;` while every Rust unit test AND the call-site guard
        // above stay green — silently reintroducing the ~3 fps boundary hold/drop churn on the
        // deep-preload chained strih->stream PGM feed.
        assert!(
            src.contains("return base + interval_ns / 2"),
            "{OBS_SOURCE}: #136 — genlock_present_ts lost the +interval/2 boundary-churn bias \
             (`return base + interval_ns / 2`); the deployed DLL would silently regress to the \
             boundary hold/drop churn the fix removed. Re-apply the half-interval tolerance."
        );
        // The C wall-clock bounds MUST equal the Rust mirror constants (lock-step).
        assert!(
            src.contains(&format!("{WALLCLOCK_TS_MIN_NS}ULL")),
            "{OBS_SOURCE}: #136 — GENLOCK_WALLCLOCK_TS_MIN_NS drifted from the Rust mirror \
             WALLCLOCK_TS_MIN_NS ({WALLCLOCK_TS_MIN_NS}); keep them in lock-step."
        );
        assert!(
            src.contains(&format!("{WALLCLOCK_TS_MAX_NS}ULL")),
            "{OBS_SOURCE}: #136 — GENLOCK_WALLCLOCK_TS_MAX_NS drifted from the Rust mirror \
             WALLCLOCK_TS_MAX_NS ({WALLCLOCK_TS_MAX_NS}); keep them in lock-step."
        );
    }

    #[test]
    fn ms_reserve_release_path_present_in_vendored_source() {
        // #184: the ts-align release must offer a sub-frame MS-GRANULAR reserve — the
        // lowest-latency lever (held latency ≈ reserve_ms, not a whole 33ms preload
        // frame). Env-gated (OBS_GENLOCK_RESERVE_MS, default 0 = disabled = the #136
        // frame path unchanged). A subtree pull (#44) dropping it silently reverts the
        // lowest-latency capability. Mirror of src/probe/genlock.rs
        // genlock_present_ts_reserve / parse_reserve_ms.
        use camera_box::probe::genlock::{GENLOCK_RESERVE_MS_DEFAULT, GENLOCK_RESERVE_MS_MAX};
        let src = squish(&vendor_file(OBS_SOURCE));
        // The env gate + parse must exist.
        assert!(
            src.contains("getenv(\"OBS_GENLOCK_RESERVE_MS\")"),
            "{OBS_SOURCE}: #184 — the ms-reserve env gate (OBS_GENLOCK_RESERVE_MS) is gone; re-apply."
        );
        assert!(
            src.contains("genlock_parse_reserve_ms"),
            "{OBS_SOURCE}: #184 — genlock_parse_reserve_ms (the ms-reserve parser) is gone; re-apply."
        );
        // The C defaults/cap MUST equal the Rust mirror constants (lock-step).
        assert!(
            src.contains(&format!(
                "#define GENLOCK_RESERVE_MS_DEFAULT {GENLOCK_RESERVE_MS_DEFAULT}"
            )),
            "{OBS_SOURCE}: #184 — GENLOCK_RESERVE_MS_DEFAULT drifted from the Rust mirror \
             ({GENLOCK_RESERVE_MS_DEFAULT}); keep them in lock-step."
        );
        assert!(
            src.contains(&format!(
                "#define GENLOCK_RESERVE_MS_MAX {GENLOCK_RESERVE_MS_MAX}"
            )),
            "{OBS_SOURCE}: #184 — GENLOCK_RESERVE_MS_MAX drifted from the Rust mirror \
             ({GENLOCK_RESERVE_MS_MAX}); keep them in lock-step."
        );
        // The reserve present_ts helper must exist AND its body must be the pure ms delay
        // (wall - reserve_ms*1e6, NO +interval/2 bias). Without the body assert, a subtree
        // pull could neuter it to `return wall_now_ns;` while every other guard stays green.
        assert!(
            src.contains("static inline uint64_t genlock_present_ts_reserve("),
            "{OBS_SOURCE}: #184 — genlock_present_ts_reserve (the ms-granular deadline) is gone; re-apply."
        );
        assert!(
            src.contains("(uint64_t)reserve_ms * 1000000ULL"),
            "{OBS_SOURCE}: #184 — genlock_present_ts_reserve lost its ms->ns delay \
             ((uint64_t)reserve_ms * 1000000ULL); the deployed DLL would no longer apply the \
             configured reserve. Re-apply the pure ms delay."
        );
        // The render path must actually USE the reserve deadline when reserve_ms > 0
        // (otherwise the knob is inert — exactly the #119 stale-bytes class of bug).
        assert!(
            src.contains("genlock_present_ts_reserve(genlock_wall_now_ns(), reserve_ms)"),
            "{OBS_SOURCE}: #184 — the ts-align render path no longer selects \
             genlock_present_ts_reserve when a reserve is configured; the ms-reserve knob is inert. Re-apply."
        );
    }

    #[test]
    fn single_latency_knob_present_in_vendored_source() {
        // #235: the C side must read the canonical OBS_GENLOCK_LATENCY_MS knob, KEEP the
        // OBS_GENLOCK_RESERVE_MS back-compat alias, imply ts-align ON when the ms knob is
        // set, and auto-derive the internal FIFO depth. A subtree pull (#44) dropping any
        // of these silently reverts the single-knob UX. Mirror of src/probe/genlock.rs
        // resolve_latency_ms / genlock_auto_preload.
        use camera_box::probe::genlock::{
            GENLOCK_AUTO_PRELOAD_MIN, GENLOCK_LATENCY_MS_DEFAULT, GENLOCK_LATENCY_MS_MAX,
        };
        let src = squish(&vendor_file(OBS_SOURCE));
        // The canonical knob is read.
        assert!(
            src.contains("getenv(\"OBS_GENLOCK_LATENCY_MS\")"),
            "{OBS_SOURCE}: #235 — the canonical latency knob (OBS_GENLOCK_LATENCY_MS) is no \
             longer read; re-apply the single-knob resolution."
        );
        // The back-compat alias parser is STILL present (RESERVE_MS keeps working).
        assert!(
            src.contains("getenv(\"OBS_GENLOCK_RESERVE_MS\")"),
            "{OBS_SOURCE}: #235 — the OBS_GENLOCK_RESERVE_MS back-compat alias is gone; \
             existing deploys / the #128 wrapper would break. Re-apply."
        );
        // The resolution + the canonical-or-alias resolver must exist.
        assert!(
            src.contains("genlock_parse_latency_ms_set")
                && src.contains("static uint32_t genlock_latency_ms("),
            "{OBS_SOURCE}: #235 — the single-knob resolver (genlock_parse_latency_ms_set + \
             genlock_latency_ms, canonical-wins-then-alias) is gone; re-apply."
        );
        // The C latency default/cap MUST equal the Rust mirror constants (lock-step).
        assert!(
            src.contains(&format!(
                "#define GENLOCK_LATENCY_MS_DEFAULT {}",
                if GENLOCK_LATENCY_MS_DEFAULT == 0 {
                    "GENLOCK_RESERVE_MS_DEFAULT".to_string()
                } else {
                    GENLOCK_LATENCY_MS_DEFAULT.to_string()
                }
            )),
            "{OBS_SOURCE}: #235 — GENLOCK_LATENCY_MS_DEFAULT drifted from the Rust mirror \
             ({GENLOCK_LATENCY_MS_DEFAULT}); keep them in lock-step."
        );
        assert!(
            src.contains(&format!(
                "#define GENLOCK_LATENCY_MS_MAX {}",
                if GENLOCK_LATENCY_MS_MAX == 100 {
                    "GENLOCK_RESERVE_MS_MAX".to_string()
                } else {
                    GENLOCK_LATENCY_MS_MAX.to_string()
                }
            )),
            "{OBS_SOURCE}: #235 — GENLOCK_LATENCY_MS_MAX drifted from the Rust mirror \
             ({GENLOCK_LATENCY_MS_MAX}); keep them in lock-step."
        );
        // The auto-preload min MUST equal the Rust mirror constant (lock-step).
        assert!(
            src.contains(&format!(
                "#define GENLOCK_AUTO_PRELOAD_MIN {GENLOCK_AUTO_PRELOAD_MIN}"
            )),
            "{OBS_SOURCE}: #235 — GENLOCK_AUTO_PRELOAD_MIN drifted from the Rust mirror \
             ({GENLOCK_AUTO_PRELOAD_MIN}); keep them in lock-step."
        );
        // ts-align must be IMPLIED ON when the ms latency knob is set (no separate user gate).
        assert!(
            src.contains("genlock_latency_ms() > 0"),
            "{OBS_SOURCE}: #235 — genlock_ts_align_enabled no longer implies ts-align ON when \
             the latency knob is set; the user would still need OBS_GENLOCK_TS_ALIGN. Re-apply."
        );
        // preload must be auto-derived (internal) on the ms path — the audit/display log
        // must surface the ms-primary latency with the frame-equivalent in parens (#235 ask).
        assert!(
            src.contains("latency_ms=%u (≈%llu frames @ %.3ffps)"),
            "{OBS_SOURCE}: #235 — the audit log no longer shows 'latency_ms=N (≈M frames)' \
             (ms primary, frames in parens); re-apply the single-knob display."
        );
    }

    #[test]
    fn build_latch_drains_burst_to_target_in_vendored_source() {
        // #116: the genlock_fifo branch of ready_async_frame must DRAIN the excess
        // oldest frames at the build latch (and after a preload-change re-arm) so every
        // input settles at exactly the target depth (preload+1), regardless of the NDI
        // startup burst. Without this, a deep burst freezes at a deep depth (unequal
        // per-cam latency, non-deterministic restart, "preload only goes up"). A subtree
        // pull (#44) reverting the drain would silently bring the bug back.
        let src = squish(&vendor_file(OBS_SOURCE));
        // The pure drain decision must be called from the render path (mirror of the
        // Rust genlock_build_drain — the C name is genlock_build_drain).
        assert!(
            src.contains("genlock_build_drain(source->async_frames.num, preload)"),
            "{OBS_SOURCE}: #116 — ready_async_frame no longer computes the build-latch \
             drain (genlock_build_drain) to trim a startup burst down to the target \
             depth. Cameras freeze at unequal depths and preload becomes one-directional. \
             Re-apply the #116 drain."
        );
        // The drain must erase the OLDEST frames using the same da_erase(.,0) +
        // remove_async_frame idiom the async_unbuffered path uses (so each dropped
        // frame is freed once, no leak/double-free).
        assert!(
            src.contains("da_erase(source->async_frames, 0)")
                && src.contains("remove_async_frame(source, dropped)"),
            "{OBS_SOURCE}: #116 — the build-latch drain must erase the oldest frames via \
             da_erase(async_frames,0) + remove_async_frame(source, dropped) (the same \
             free idiom as the async_unbuffered drain) so no frame leaks or double-frees. \
             Re-apply."
        );
        // The drain count must come from the target = steady_state_depth(preload) =
        // preload + 1 contract, surfaced in a comment/identifier so the intent is pinned.
        assert!(
            src.contains("genlock_build_drain"),
            "{OBS_SOURCE}: #116 — the build-drain helper genlock_build_drain is missing."
        );
    }

    #[test]
    fn reconnect_rearm_on_sustained_empty_in_vendored_source() {
        // #126: on an upstream OBS restart the downstream NDI source underruns to empty
        // but genlock_filled stays TRUE (KEEP_CONTENT blocks the NULL-emit reset; an
        // underrun never fires the overrun force-drain), so the #102 steady gate consumes
        // 1/tick on reconnect WITHOUT rebuilding the preload reserve — the video delay
        // silently collapses to ~0 until a manual nudge. The fix tracks consecutive
        // true-empty ticks (genlock_empty_run) and, on resume after a SUSTAINED empty run
        // (>= GENLOCK_REARM_EMPTY_TICKS), re-arms genlock_filled=false so the existing
        // build path + #116 drain rebuild the reserve. A subtree pull (#44) reverting this
        // would silently bring the A/V-drift-on-restart bug back.
        let src = squish(&vendor_file(OBS_SOURCE));
        // The consecutive-empty counter must be incremented at the true-empty
        // (num==0) underrun site so a sustained gap is detectable.
        assert!(
            src.contains("source->genlock_empty_run++"),
            "{OBS_SOURCE}: #126 — the consecutive true-empty counter \
             (genlock_empty_run) is no longer incremented at the num==0 underrun; the \
             reconnect re-arm can't detect a sustained gap. Re-apply the #126 fix."
        );
        // The re-arm decision must be taken on resume (in ready_async_frame), guarded by
        // the threshold so brief jitter NEVER re-arms (the shallow-preload spurious-hold
        // hazard the issue calls out).
        assert!(
            src.contains("GENLOCK_REARM_EMPTY_TICKS"),
            "{OBS_SOURCE}: #126 — the re-arm threshold GENLOCK_REARM_EMPTY_TICKS is \
             missing; without a high threshold normal jitter would spuriously re-arm and \
             force a ~preload-frame rebuild hold on every blip. Re-apply."
        );
        // The re-arm must reset the latch (filled=false) so the EXISTING build path
        // rebuilds — no new draining logic is added (#102/#116 preserved).
        assert!(
            src.contains("genlock_empty_run >= GENLOCK_REARM_EMPTY_TICKS"),
            "{OBS_SOURCE}: #126 — the sustained-empty re-arm guard \
             (genlock_empty_run >= GENLOCK_REARM_EMPTY_TICKS) is gone; re-apply."
        );
        // The counter must reset on a consume so jitter (flicker empty/non-empty) can
        // never accumulate to the threshold — only a genuine sustained disconnect can.
        assert!(
            src.contains("source->genlock_empty_run = 0"),
            "{OBS_SOURCE}: #126 — genlock_empty_run is never reset on a consume; a \
             flickering queue would creep to the threshold and spuriously re-arm. Re-apply."
        );
        // The threshold must be ~1 s @ 30 fps (30 ticks) — large enough that a real
        // disconnect, not jitter, is required to trip it. Pin the value to the mirror.
        assert!(
            src.contains("#define GENLOCK_REARM_EMPTY_TICKS 30"),
            "{OBS_SOURCE}: #126 — GENLOCK_REARM_EMPTY_TICKS must be 30 (~1 s @ 30 fps); \
             the Rust mirror is {GENLOCK_REARM_EMPTY_TICKS}."
        );
        assert_eq!(
            GENLOCK_REARM_EMPTY_TICKS, 30,
            "Rust mirror must equal the C re-arm threshold"
        );
        // (review) Pin the relative ORDER: the re-arm guard must READ genlock_empty_run
        // BEFORE the resume-site reset zeroes it. A subtree pull that reordered them
        // (reset above the guard) would pass the token-presence checks above yet silently
        // break the re-arm (the counter is always 0 at the read → never re-arms). Find the
        // guard position in the squished source, then the FIRST reset that follows it (the
        // resume-site reset) — the guard index must be the smaller.
        let guard_at = src
            .find("genlock_empty_run >= GENLOCK_REARM_EMPTY_TICKS")
            .expect("the #126 re-arm guard must be present");
        let reset_after_guard = src[guard_at..]
            .find("genlock_empty_run = 0")
            .map(|i| guard_at + i)
            .expect(
                "a genlock_empty_run reset must follow the #126 re-arm guard (the resume-site \
                 reset)",
            );
        assert!(
            guard_at < reset_after_guard,
            "{OBS_SOURCE}: #126 — the re-arm guard (genlock_empty_run >= …) must READ the \
             counter BEFORE the resume-site reset zeroes it; they appear reordered, which \
             makes the counter always 0 at the read → the FIFO never re-arms after a \
             reconnect. Re-order so the guard precedes the reset."
        );
    }

    #[test]
    fn empty_run_field_in_struct() {
        // #126: the consecutive true-empty counter must be a per-source field (read +
        // written by the A/V thread under async_mutex, same as genlock_filled, #93 lesson).
        let hdr = squish(&vendor_file(OBS_INTERNAL));
        assert!(
            hdr.contains("genlock_empty_run"),
            "{OBS_INTERNAL}: #126 — the per-source consecutive-empty counter field \
             genlock_empty_run is missing from obs_source; re-apply."
        );
    }

    #[test]
    fn genlock_bypasses_get_closest_frame_bootstrap_shortcut() {
        // #102 (review fix): get_closest_frame's `!last_frame_ts` bootstrap short-circuit
        // must EXCLUDE genlock sources, so a genlock FIFO always routes through
        // ready_async_frame/genlock_decide and rebuilds the preload delay after an overrun
        // drain (cache_video resets last_frame_ts=0 AND genlock_filled=false) or a source
        // resume — instead of leaking one undelayed distinct frame (a ~preload-frame phase
        // jump). The guard pins the `&& !source->genlock_fifo` exclusion.
        let src = squish(&vendor_file(OBS_SOURCE));
        assert!(
            src.contains("!source->last_frame_ts && !source->genlock_fifo"),
            "{OBS_SOURCE}: #102 — get_closest_frame's bootstrap bypass no longer excludes \
             genlock sources; the post-overrun/post-resume delay-line rebuild is silently \
             skipped and one undelayed frame leaks. Re-apply the `&& !source->genlock_fifo` \
             exclusion."
        );
        // Every path that empties/re-bootstraps the FIFO must re-arm the latch (reset to
        // false) so the delay line rebuilds. There are exactly FOUR `genlock_filled = false`
        // sites: create-init, overrun force-drain (cache_video), inactive/flush
        // (obs_source_output_video_internal), and the runtime preload-change re-arm
        // (obs_source_set_genlock_preload). The render-tick writeback uses gd.filled, not a
        // literal false, so it does not count. Assert >=4 so deleting ANY single re-arm site
        // turns this RED (a >=3 floor would let one deletion slip — review finding).
        let raw = vendor_file(OBS_SOURCE);
        let resets = raw.matches("source->genlock_filled = false;").count();
        assert!(
            resets >= 4,
            "{OBS_SOURCE}: #102 — expected >=4 `source->genlock_filled = false;` re-arm \
             sites (create init + overrun drain + inactive/flush + preload-change), found \
             {resets}. A path that drops the latch reset leaks an undelayed frame after that \
             drain/flush/resize."
        );
    }

    #[test]
    fn audit_counters_exist_in_struct() {
        let hdr = squish(&vendor_file(OBS_INTERNAL));
        for field in [
            "genlock_frames_received",
            "genlock_frames_consumed",
            "genlock_underruns",
            "genlock_overruns",
            "genlock_peak_depth",
        ] {
            assert!(
                hdr.contains(field),
                "{OBS_INTERNAL}: #70 audit field `{field}` missing from obs_source — \
                 an upstream subtree merge dropped the genlock audit state; re-apply."
            );
        }
    }

    #[test]
    fn peak_depth_updated_on_the_producer_push_path() {
        // #99 point 2: genlock_peak_depth must be folded in on the PRODUCER side too (the
        // obs_source_output_video_internal push path), not only on the consumer side
        // (ready_async_frame). Otherwise a producer burst that drains before the next render
        // tick under-reports the high-water mark. The producer update sits right after the
        // `genlock_frames_received++` at the da_push_back site, gated by `source->genlock_fifo`.
        let raw = vendor_file(OBS_SOURCE);
        // The producer push site is uniquely identified by genlock_frames_received++ (it is
        // ONLY incremented there). Require a peak update within the same genlock_fifo block.
        let recv_pos = raw
            .find("source->genlock_frames_received++")
            .expect("genlock_frames_received++ producer site missing — #70/#99 reverted");
        // Window from the receive increment through the rest of that genlock_fifo block (the
        // peak update sits inside the same `if (source->genlock_fifo) { ... }` block, after a
        // documenting comment — allow ample room so the comment can't push it out of range).
        let window_end = (recv_pos + 1400).min(raw.len());
        let window = &raw[recv_pos..window_end];
        assert!(
            window.contains("source->genlock_peak_depth = depth")
                || window.contains("genlock_peak_depth = (uint32_t)source->async_frames.num"),
            "{OBS_SOURCE}: #99 point 2 — genlock_peak_depth is NOT updated on the producer push \
             path (near genlock_frames_received++). The peak under-reports a producer burst that \
             drains before the next render tick; re-apply the producer-side peak update."
        );
        // The consumer-side update must ALSO still exist (both sites contribute the max).
        assert!(
            raw.contains("source->genlock_peak_depth = (uint32_t)source->async_frames.num"),
            "{OBS_SOURCE}: #70 — the consumer-side genlock_peak_depth update (ready_async_frame) \
             went missing; re-apply."
        );
    }

    // ---- #97: per-source preload field + runtime set/get API + raised cap -------

    #[test]
    fn per_source_preload_field_in_struct() {
        let hdr = squish(&vendor_file(OBS_INTERNAL));
        // The preload is now a PER-SOURCE field (not a global static), read by the
        // A/V thread under async_mutex (#93 UAF lesson). A subtree pull that drops
        // it would silently revert the runtime video-delay control (#97).
        assert!(
            hdr.contains("uint32_t genlock_preload"),
            "{OBS_INTERNAL}: #97 per-source `uint32_t genlock_preload` field missing \
             from obs_source — the runtime video-delay control reverted; re-apply."
        );
    }

    #[test]
    fn per_source_preload_api_declared() {
        let api = squish(&vendor_file(OBS_API));
        assert!(
            api.contains("obs_source_set_genlock_preload(obs_source_t *source, uint32_t")
                || api.contains("obs_source_set_genlock_preload(obs_source_t *source, uint32_t)"),
            "{OBS_API}: #97 obs_source_set_genlock_preload not exported — DistroAV \
             cannot apply the per-source preload; re-apply."
        );
        assert!(
            api.contains("obs_source_get_genlock_preload(const obs_source_t *source)"),
            "{OBS_API}: #97 obs_source_get_genlock_preload not exported; re-apply."
        );
    }

    #[test]
    fn ready_async_frame_reads_per_source_preload_under_lock() {
        let src = squish(&vendor_file(OBS_SOURCE));
        // The render path must read the PER-SOURCE field, not the global static
        // (genlock_preload_frames()). The whole render path runs under async_mutex,
        // so the read is already serialised with the set/get API (#93 lesson).
        assert!(
            src.contains("source->genlock_preload"),
            "{OBS_SOURCE}: #97 — ready_async_frame no longer reads the per-source \
             source->genlock_preload (reverted to the global static?); re-apply."
        );
    }

    #[test]
    fn set_get_api_lock_async_mutex() {
        let src = squish(&vendor_file(OBS_SOURCE));
        // The set/get API mutates/reads a field the A/V thread reads, so it MUST take
        // async_mutex (no unlocked mutation — the #93 UAF lesson the spec calls out).
        assert!(
            src.contains("void obs_source_set_genlock_preload(obs_source_t *source, uint32_t"),
            "{OBS_SOURCE}: #97 obs_source_set_genlock_preload impl missing; re-apply."
        );
        // The setter clamps to [0, GENLOCK_PRELOAD_MAX] and locks async_mutex.
        let setter_start = vendor_file(OBS_SOURCE)
            .find("void obs_source_set_genlock_preload(")
            .expect("setter not found");
        let setter = &vendor_file(OBS_SOURCE)[setter_start..];
        let setter_body = squish(&setter[..setter.find("\n}").map(|i| i + 2).unwrap_or(400)]);
        assert!(
            setter_body.contains("async_mutex"),
            "{OBS_SOURCE}: #97 — obs_source_set_genlock_preload does not take \
             async_mutex around the write the A/V thread reads (the #93 UAF lesson). \
             Re-apply the lock."
        );
    }

    #[test]
    fn preload_cap_raised_to_128() {
        let src = squish(&vendor_file(OBS_SOURCE));
        assert!(
            src.contains("#define GENLOCK_PRELOAD_MAX 128"),
            "{OBS_SOURCE}: #97 GENLOCK_PRELOAD_MAX must be 128 (≈1 s+ of video delay \
             at the per-source drop-cap); the Rust mirror is {GENLOCK_PRELOAD_MAX}."
        );
        assert_eq!(GENLOCK_PRELOAD_MAX, 128, "Rust mirror must equal the C cap");
    }

    #[test]
    fn per_source_drop_cap_present_in_cache_video() {
        let src = squish(&vendor_file(OBS_SOURCE));
        // cache_video must use the per-source drop-cap helper (genlock source =>
        // preload + RESERVE) instead of the bare MAX_ASYNC_FRAMES literal, so a
        // deliberately-delayed source can park its full buffer without an overrun.
        assert!(
            src.contains("genlock_drop_cap"),
            "{OBS_SOURCE}: #97 — cache_video no longer uses the per-source \
             genlock_drop_cap (preload+RESERVE); a deep preload force-drains every \
             refill and the delayed source FREEZES. Re-apply."
        );
        // (review) The per-source cap MUST keep a floor at MAX_ASYNC_FRAMES so a
        // shallow preload (the production default 1) doesn't cut burst tolerance from
        // 30 to 5. The C helper carries an explicit `< MAX_ASYNC_FRAMES` floor.
        assert!(
            src.contains("want < MAX_ASYNC_FRAMES"),
            "{OBS_SOURCE}: #97 — genlock_source_drop_cap dropped the MAX_ASYNC_FRAMES \
             floor; a shallow preload cuts NDI burst tolerance 6x (30 -> 5). Re-apply."
        );
    }

    #[test]
    fn setter_clamps_unsigned_not_via_long() {
        // (review) The setter takes a uint32_t and MUST clamp the unsigned value
        // directly — round-tripping through `long` inverts the upper clamp to 0 on
        // Windows LLP64 (32-bit long) for values above LONG_MAX.
        let src = squish(&vendor_file(OBS_SOURCE));
        assert!(
            src.contains("genlock_clamp_preload_u32(frames)"),
            "{OBS_SOURCE}: #97 — obs_source_set_genlock_preload no longer clamps via \
             the unsigned genlock_clamp_preload_u32; a (long) cast inverts the clamp on \
             LLP64. Re-apply."
        );
        assert!(
            !src.contains("genlock_clamp_preload((long)frames)"),
            "{OBS_SOURCE}: #97 — the setter is back to genlock_clamp_preload((long)frames), \
             which inverts the upper clamp to 0 on Windows LLP64. Use the u32 clamp."
        );
    }

    #[test]
    fn audit_log_includes_ms() {
        let src = squish(&vendor_file(OBS_SOURCE));
        // The genlock-fifo audit line must carry the ms equivalent of the preload
        // (preload=N (=M ms @ Ffps)) so the live delay is visible in the OBS log.
        assert!(
            src.contains("ms @") || src.contains("ms@"),
            "{OBS_SOURCE}: #97 — the genlock-fifo audit log no longer prints the ms \
             equivalent of the preload; re-apply the ms field."
        );
    }
}

// ---- #97 GUI: DistroAV per-source preload slider + ms info-text --------------

mod distroav_source {
    use super::vendored_source::{vendor_file, NDI_SOURCE, WINDOWS_GENLOCK_WF};

    fn squish(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn preload_slider_property_present() {
        let src = squish(&vendor_file(NDI_SOURCE));
        // The int slider (0..128) is the runtime video-delay control, shown next to
        // the genlock-fifo checkbox.
        assert!(
            src.contains("#define PROP_GENLOCK_PRELOAD"),
            "{NDI_SOURCE}: #97 PROP_GENLOCK_PRELOAD define missing; re-apply the slider."
        );
        assert!(
            src.contains("obs_properties_add_int_slider(props, PROP_GENLOCK_PRELOAD"),
            "{NDI_SOURCE}: #97 — the genlock-preload int slider is gone from the NDI \
             source properties; re-apply."
        );
        // The slider min(0) + step(1) are pinned; the max may be the literal 128 OR the
        // symbolic PROP_GENLOCK_PRELOAD_MAX (== 128, asserted separately). #235 relabeled
        // the slider from "Genlock preload (video delay)" to the internal/legacy
        // "Genlock preload (internal FIFO depth — legacy frame control)" (preload is no
        // longer a user latency knob — the ms latency knob holds the delay), so the guard
        // pins the range/step and that the label marks it INTERNAL/LEGACY, not the exact
        // old wording.
        assert!(
            src.contains("PROP_GENLOCK_PRELOAD, \"Genlock preload (internal FIFO depth — legacy frame control)\", 0, 128, 1")
                || src.contains(
                    "PROP_GENLOCK_PRELOAD, \"Genlock preload (internal FIFO depth — legacy frame control)\", 0, PROP_GENLOCK_PRELOAD_MAX, 1"
                ),
            "{NDI_SOURCE}: #97/#235 — the preload slider range/label changed; expected the \
             internal/legacy label (\"Genlock preload (internal FIFO depth — legacy frame \
             control)\", 0, 128|PROP_GENLOCK_PRELOAD_MAX, 1)."
        );
        // The symbolic cap must equal the libobs cap (128).
        assert!(
            src.contains("#define PROP_GENLOCK_PRELOAD_MAX 128"),
            "{NDI_SOURCE}: #97 — PROP_GENLOCK_PRELOAD_MAX must be 128 to match libobs."
        );
    }

    #[test]
    fn preload_ms_infotext_present() {
        let src = squish(&vendor_file(NDI_SOURCE));
        // A read-only info-text property below the slider shows the live ms, updated
        // by a modified_callback that reads obs_get_video_info().
        assert!(
            src.contains("PROP_GENLOCK_PRELOAD_MS"),
            "{NDI_SOURCE}: #97 — the preload ms info-text property is missing; re-apply."
        );
        assert!(
            src.contains("obs_get_video_info"),
            "{NDI_SOURCE}: #97 — the ms info-text callback no longer reads \
             obs_get_video_info() for the fps; re-apply."
        );
        assert!(
            src.contains("obs_property_set_modified_callback"),
            "{NDI_SOURCE}: #97 — the preload slider has no modified_callback to \
             recompute the ms label; re-apply."
        );
    }

    #[test]
    fn single_latency_label_present() {
        // #235: the NDI source properties must show the SINGLE genlock-latency display —
        // a read-only "genlock latency = N ms (≈ M frames @ Ffps)" (ms primary, frames in
        // parens), sourced from the resolved env latency (OBS_GENLOCK_LATENCY_MS, alias
        // OBS_GENLOCK_RESERVE_MS). A subtree pull (#44) dropping it reverts the #235 UX.
        let src = squish(&vendor_file(NDI_SOURCE));
        assert!(
            src.contains("#define PROP_GENLOCK_LATENCY_MS"),
            "{NDI_SOURCE}: #235 — the PROP_GENLOCK_LATENCY_MS read-only latency label \
             property define is missing; re-apply."
        );
        assert!(
            src.contains("obs_properties_add_text(props, PROP_GENLOCK_LATENCY_MS"),
            "{NDI_SOURCE}: #235 — the read-only genlock-latency info-text property is gone; \
             re-apply the single-knob display."
        );
        // The resolver must read the canonical knob AND fall back to the reserve alias.
        assert!(
            src.contains("getenv(\"OBS_GENLOCK_LATENCY_MS\")")
                && src.contains("getenv(\"OBS_GENLOCK_RESERVE_MS\")"),
            "{NDI_SOURCE}: #235 — resolve_genlock_latency_ms no longer reads the canonical \
             OBS_GENLOCK_LATENCY_MS + the OBS_GENLOCK_RESERVE_MS alias; re-apply."
        );
        // The label must be ms-primary with the frame-equivalent in parens (the #235 ask).
        assert!(
            src.contains("genlock latency = %ld ms (≈ %llu frames @ %.3f fps)"),
            "{NDI_SOURCE}: #235 — the latency label is no longer 'N ms (≈ M frames @ Ffps)' \
             (ms primary, frames in parens); re-apply format_genlock_latency_label."
        );
    }

    #[test]
    fn preload_applied_in_update() {
        let src = squish(&vendor_file(NDI_SOURCE));
        // ndi_source_update must apply the slider value via the runtime API (resolved
        // at runtime like set_genlock_fifo, so the plugin still builds against stock
        // SDK headers and loads on any OBS).
        assert!(
            src.contains("obs_source_set_genlock_preload"),
            "{NDI_SOURCE}: #97 — ndi_source_update no longer applies the per-source \
             preload via obs_source_set_genlock_preload; the slider is inert. Re-apply."
        );
        assert!(
            src.contains("PROP_GENLOCK_PRELOAD)"),
            "{NDI_SOURCE}: #97 — ndi_source_update no longer reads the \
             PROP_GENLOCK_PRELOAD setting; re-apply."
        );
    }

    #[test]
    fn ms_label_set_on_first_open_and_negative_floored() {
        let src = squish(&vendor_file(NDI_SOURCE));
        // (review) The ms label must be set from the current settings at property-build
        // time (shared formatter), so it shows on first dialog open before any callback
        // fires — not the bare "↳ delay" placeholder.
        assert!(
            src.contains("format_preload_ms_label"),
            "{NDI_SOURCE}: #97 — the shared ms-label formatter is gone; the label no \
             longer shows on first dialog open. Re-apply."
        );
        assert!(
            src.contains("obs_source_get_settings(s->obs_source)"),
            "{NDI_SOURCE}: #97 — the initial ms label is not seeded from the source \
             settings at build time; it stays the placeholder until interaction. Re-apply."
        );
        // (review) A negative scene-JSON preload must be floored at 0 before the
        // uint32_t cast, or -1 wraps to UINT32_MAX and clamps to MAX delay.
        assert!(
            src.contains("if (pl < 0) pl = 0;"),
            "{NDI_SOURCE}: #97 — ndi_source_update no longer floors a negative preload \
             at 0 before the uint32_t cast; -1 would wrap to the MAX delay. Re-apply."
        );
    }

    #[test]
    fn preload_default_derives_from_env_for_back_compat() {
        // #97 (review): ndi_source_getdefaults must derive the PROP_GENLOCK_PRELOAD
        // default from OBS_GENLOCK_PRELOAD_FRAMES, NOT hardcode 1 — else the #70 env
        // mechanism is silently reverted to 1 on every scene load for DistroAV sources.
        let src = squish(&vendor_file(NDI_SOURCE));
        assert!(
            src.contains("genlock_preload_env_default")
                && src.contains("getenv(\"OBS_GENLOCK_PRELOAD_FRAMES\")"),
            "{NDI_SOURCE}: #97 — the genlock-preload default no longer derives from \
             OBS_GENLOCK_PRELOAD_FRAMES; a hardcoded default overwrites the libobs env \
             seed on scene load (#70 back-compat regression). Re-apply."
        );
        // The default must NOT be the bare literal 1 (the reverted form).
        assert!(
            src.contains("obs_data_set_default_int(settings, PROP_GENLOCK_PRELOAD, genlock_preload_env_default())"),
            "{NDI_SOURCE}: #97 — PROP_GENLOCK_PRELOAD default is not wired to \
             genlock_preload_env_default(); re-apply the env back-compat fix."
        );
    }

    #[test]
    fn windows_genlock_workflow_gates_on_the_preload_slider() {
        // Mirror the lock-step convention (tests/distroav_source_config_lock.rs): the
        // Windows production build re-asserts the #97 source tokens in pwsh BEFORE the
        // 150-min build, since this Linux Rust guard can't compile on the runner.
        let wf = squish(&vendor_file(WINDOWS_GENLOCK_WF));
        assert!(
            wf.contains("obs_source_set_genlock_preload"),
            "{WINDOWS_GENLOCK_WF}: #97 — the production build no longer asserts the \
             per-source preload API; a subtree bump could ship a build without the \
             runtime video-delay control while the version pin still passes. Re-add \
             the pwsh #97 gate."
        );
        assert!(
            wf.contains("obs_properties_add_int_slider(props, PROP_GENLOCK_PRELOAD"),
            "{WINDOWS_GENLOCK_WF}: #97 — the production build no longer asserts the \
             preload slider property; re-add the pwsh #97 gate."
        );
    }
}
