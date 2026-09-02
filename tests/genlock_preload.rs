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
    genlock_rearm_on_resume, ms_to_frames, parse_preload, preload_to_ms, steady_state_depth,
    GenlockDecision, GENLOCK_DROP_CAP_RESERVE, GENLOCK_PRELOAD_DEFAULT, GENLOCK_PRELOAD_MAX,
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

// ---- #259: fps_den==0 must NOT divide-by-zero (latent SIGFPE) -----------------
//
// The C genlock audit-log computes latency_frames =
//   (latency_ms*fps_num + (1000*fps_den)/2) / (1000*fps_den)
// guarded only by `have_vi = obs_get_video_info(&ovi) && ovi.fps_num != 0` — NOT
// fps_den. If obs_get_video_info ever returns true with fps_num!=0 && fps_den==0,
// that integer divide is a SIGFPE on the render thread (the sibling
// genlock_source_drop_cap already guards `ovi.fps_den != 0`). The C divide cannot be
// unit-tested directly, so we (a) pin the mirror's value contract — both ms<->frames
// helpers return 0, never panic, when fps_den==0 — and (b) assert the C have_vi guard
// checks fps_den (the vendored_source guard below). Found by the #257 (PR #258) review.

#[test]
fn ms_to_frames_is_zero_and_no_panic_when_fps_den_zero() {
    // The mirror of the C latency_frames divide. fps_den==0 (with ANY fps_num) must
    // yield 0 — the "fps unknown" fallback — never a divide-by-zero. (#259)
    assert_eq!(ms_to_frames(100, 30000, 0), 0);
    assert_eq!(ms_to_frames(3, 30, 0), 0);
    assert_eq!(ms_to_frames(0, 0, 0), 0);
    // fps_num==0 also yields 0 (no valid video info), the existing contract.
    assert_eq!(ms_to_frames(100, 0, 1), 0);
    // A valid pair still converts normally (3 ms @ 30 fps rounds to 0 frames).
    assert_eq!(ms_to_frames(1000, 30, 1), 30);
}

#[test]
fn preload_to_ms_is_zero_and_no_panic_when_fps_den_zero() {
    // The sibling ms helper divides by fps_num (not fps_den), so fps_den==0 makes the
    // numerator 0 — still 0, still no panic. Pin it so the contract can't regress. (#259)
    assert_eq!(preload_to_ms(30, 30000, 0), 0);
    assert_eq!(preload_to_ms(1, 30, 0), 0);
    assert_eq!(preload_to_ms(30, 0, 1), 0); // fps_num==0 path (existing)
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

    // #245/#292: a DEEP per-source latency is a deliberate video delay — the FIFO drop-cap
    // must hold its full frame-equivalent before the ms deadline releases the oldest frame,
    // or the overrun force-drain would cap the delay short and make the override INERT. This
    // pins genlock_drop_cap as a pure function of a frame count; the ACTUAL per-source depth
    // (budgeted at the worst-case source arrival fps, #292) is computed by
    // genlock_latency_depth_frames and exercised in
    // drop_cap_delivers_high_latency_at_max_source_arrival. Here we confirm the worst real
    // case — 2000 ms @ 60 fps source = 120 frames — still fits under the abs-max.
    #[test]
    fn drop_cap_accommodates_deep_per_source_latency() {
        use camera_box::probe::genlock::{
            genlock_drop_cap, GENLOCK_DROP_CAP_RESERVE, GENLOCK_PRELOAD_MAX,
            GENLOCK_SOURCE_LATENCY_MS_MAX, MAX_ASYNC_FRAMES,
        };
        // genlock_drop_cap given a 30-frame depth -> cap = 30 + RESERVE = 34 (above the floor).
        assert_eq!(genlock_drop_cap(true, 30), 30 + GENLOCK_DROP_CAP_RESERVE);
        // The per-source maximum (2000 ms) at the worst-case 60 fps source arrival = 120
        // frames -> cap = 124, comfortably under the abs-max (GENLOCK_PRELOAD_MAX + RESERVE =
        // 132): a source at the cap buffers its FULL 2 s delay without an overrun force-drain.
        let fmax = ms_to_frames(GENLOCK_SOURCE_LATENCY_MS_MAX, 60, 1);
        assert_eq!(fmax, 120);
        let abs_max = GENLOCK_PRELOAD_MAX + GENLOCK_DROP_CAP_RESERVE;
        assert_eq!(genlock_drop_cap(true, fmax), 120 + GENLOCK_DROP_CAP_RESERVE);
        assert!(
            120 + GENLOCK_DROP_CAP_RESERVE < abs_max,
            "2000 ms @ 60 fps (120 frames) must fit under the abs-max"
        );
        // A shallow/zero override keeps the historic 30-frame burst-tolerance floor.
        assert_eq!(genlock_drop_cap(true, 0), MAX_ASYNC_FRAMES);
        assert_eq!(genlock_drop_cap(true, 3), MAX_ASYNC_FRAMES); // 3 frames (100 ms global) < floor
    }

    // #292: the genlock ts-align release deadline holds every queued frame younger than
    // latency_ms, so the FIFO fills at the SOURCE ARRIVAL rate — which can EXCEED the
    // canvas OUTPUT rate. The stream box receives a 60 fps NDI feed from strih into a
    // 30 fps canvas (the "60→30 strih→stream" topology), so 1000 ms of delay parks ≈ 60
    // frames in the buffer, NOT 30. Budgeting the drop-cap at the CANVAS fps (the pre-#292
    // bug) undercounted the held depth ~2x, so the overrun force-drain capped a deep
    // latency at ~450 ms — the operator could not delay the stream the ~1 s needed to
    // A/V-align to the late mastered audio. The drop-cap depth MUST be budgeted at the
    // worst-case arrival rate. This is the test CI "never verified" (the old tests budgeted
    // at the canvas fps and so always passed while production capped at ~450 ms).
    #[test]
    fn drop_cap_delivers_high_latency_at_max_source_arrival() {
        use camera_box::probe::genlock::{
            genlock_drop_cap, genlock_latency_depth_frames, GENLOCK_MAX_SOURCE_FPS,
        };
        // The rig's worst case: a 30 fps STREAM canvas receiving a 60 fps NDI feed.
        const STREAM_CANVAS_FPS: u32 = 30;
        for &latency_ms in &[1000u32, 1500, 2000] {
            // Frames the FIFO actually holds at the worst-case arrival rate.
            let held = ms_to_frames(latency_ms, GENLOCK_MAX_SOURCE_FPS, 1);
            // The depth the drop-cap budgets, given the (slower) stream canvas fps.
            let depth = genlock_latency_depth_frames(latency_ms, STREAM_CANVAS_FPS, 1);
            let cap = genlock_drop_cap(true, depth);
            assert!(
                cap > held,
                "{latency_ms} ms @ {GENLOCK_MAX_SOURCE_FPS} fps arrival: drop-cap {cap} must \
                 exceed the {held} held frames to DELIVER the configured latency — a canvas-fps \
                 budget caps it at ~450 ms (#292)"
            );
        }
    }

    // #292: the per-source latency depth helper budgets at the worst-case SOURCE arrival
    // rate (GENLOCK_MAX_SOURCE_FPS = 60), so the FIFO holds the FULL configured delay even
    // when the canvas runs slower (30 fps stream). At the canvas rate alone the depth would
    // be half — the root cause of the ~450 ms production cap.
    #[test]
    fn latency_depth_frames_budgets_at_source_arrival_rate() {
        use camera_box::probe::genlock::{
            genlock_latency_depth_frames, GENLOCK_AUTO_PRELOAD_MIN, GENLOCK_MAX_SOURCE_FPS,
            GENLOCK_SOURCE_LATENCY_MS_MAX,
        };
        // 1000 ms into a 30 fps canvas must budget for the 60 fps arrival = 60 frames.
        assert_eq!(genlock_latency_depth_frames(1000, 30, 1), 60);
        // 1500 ms → 90 frames (the operator's A/V-align headroom above 1 s).
        assert_eq!(genlock_latency_depth_frames(1500, 30, 1), 90);
        // The per-source maximum (2000 ms) → 120 frames @ 60 fps.
        assert_eq!(
            genlock_latency_depth_frames(GENLOCK_SOURCE_LATENCY_MS_MAX, 30, 1),
            120
        );
        // A faster canvas (60 fps) yields the same depth — the arrival floor already covers it.
        assert_eq!(genlock_latency_depth_frames(1000, 60, 1), 60);
        // The shallow 3 ms floor never drops below the resilience minimum.
        assert_eq!(
            genlock_latency_depth_frames(3, 30, 1),
            GENLOCK_AUTO_PRELOAD_MIN
        );
        assert_eq!(GENLOCK_MAX_SOURCE_FPS, 60);
    }

    #[test]
    fn latency_default_and_floor_are_three_ms() {
        // #257: the genlock latency is a BUILD CONST — default AND floor are 3 ms (no env). The
        // legacy strtol parser ([`resolve_latency_ms`]) is kept as a pure helper; with neither arg
        // set it returns the parser's own reserve default (0, the historic "unset" sentinel), which
        // is decoupled from the new build const below.
        use camera_box::probe::genlock::GENLOCK_LATENCY_MS_MIN;
        assert_eq!(GENLOCK_LATENCY_MS_DEFAULT, 3);
        assert_eq!(GENLOCK_LATENCY_MS_MIN, 3);
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
    use camera_box::probe::genlock::{
        GENLOCK_MAX_SOURCE_FPS, GENLOCK_PRELOAD_MAX, GENLOCK_REARM_EMPTY_TICKS,
    };
    use std::path::PathBuf;

    pub fn vendor_file(rel: &str) -> String {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
    }

    fn squish(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// #942 hardening: strip `//` line comments and `/* ... */` block comments from a C/C++
    /// source string before slicing it for a branch-scoped negative check. Naive (no
    /// string/char-literal awareness), which is fine here -- it is only ever used to bound a
    /// slice for an ASCII substring search, never to reconstruct compilable source -- but it
    /// exists specifically because a vendored comment can EXPLAIN what a branch must NOT do
    /// using the exact same literal text a negative check searches for (see
    /// `dock_lock_corrector_is_monitor_only_by_build_default_942`'s own comment: "no
    /// cb_apply_lock_latency_ms(), no rebase()"), which would otherwise self-defeat the check.
    fn strip_cpp_comments(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let bytes = s.as_bytes();
        let mut i = 0;
        let mut in_line = false;
        let mut in_block = false;
        while i < bytes.len() {
            let c = bytes[i] as char;
            if in_line {
                if c == '\n' {
                    in_line = false;
                    out.push('\n');
                }
                i += 1;
                continue;
            }
            if in_block {
                if c == '*' && i + 1 < bytes.len() && bytes[i + 1] as char == '/' {
                    in_block = false;
                    i += 2;
                    continue;
                }
                i += 1;
                continue;
            }
            if c == '/' && i + 1 < bytes.len() && bytes[i + 1] as char == '/' {
                in_line = true;
                i += 2;
                continue;
            }
            if c == '/' && i + 1 < bytes.len() && bytes[i + 1] as char == '*' {
                in_block = true;
                i += 2;
                continue;
            }
            out.push(c);
            i += 1;
        }
        out
    }

    pub const OBS_SOURCE: &str = "vendor/obs-studio/libobs/obs-source.c";
    const OBS_INTERNAL: &str = "vendor/obs-studio/libobs/obs-internal.h";
    pub const OBS_API: &str = "vendor/obs-studio/libobs/obs.h";
    // #276/#278 multiview render-divisor: libobs render path + frontend projector + the burn renderer.
    pub const OBS_DISPLAY: &str = "vendor/obs-studio/libobs/obs-display.c";
    // #293: the pure, OBS-dependency-free skip decision (with the anti-starvation floor) that
    // render_display() calls — extracted so it is unit-testable (tests/obs_display_budget.rs).
    pub const OBS_DISPLAY_BUDGET: &str = "vendor/obs-studio/libobs/obs-display-budget.h";
    // #278: the graphics-thread loop publishes the per-tick start used by the adaptive skip.
    pub const OBS_VIDEO: &str = "vendor/obs-studio/libobs/obs-video.c";
    pub const OBSPROJECTOR: &str = "vendor/obs-studio/frontend/widgets/OBSProjector.cpp";
    pub const BURN_QR: &str = "vendor/distroav/src/burn-qr.hpp";
    pub const NDI_SOURCE: &str = "vendor/distroav/src/ndi-source.cpp";
    pub const NDI_BURN_FILTER: &str = "vendor/distroav/src/ndi-burn-filter.cpp";
    // #879: the aux-NDI-sender render-budget gate — the DistroAV filter send path + the
    // libobs core seam it delegates to.
    pub const NDI_FILTER: &str = "vendor/distroav/src/ndi-filter.cpp";
    pub const OBS_CORE_C: &str = "vendor/obs-studio/libobs/obs.c";
    pub const WINDOWS_GENLOCK_WF: &str = ".github/workflows/windows-genlock.yml";
    pub const WINDOWS_GENLOCK_FAST_WF: &str = ".github/workflows/windows-genlock-fast.yml";
    // #942: the LIVE dock's pure decision header + its OBS glue caller.
    pub const AV_SYNC_DOCK_AUDIO: &str = "vendor/av-sync-dock/src/camera-box-audio.hpp";
    pub const AV_SYNC_DOCK_OUTPUT: &str = "vendor/av-sync-dock/src/sync-test-output.cpp";
    // #803/#960: the per-source ASRC servo -- constants/struct live in the header, the
    // starvation guard's logic in the .c.
    pub const ASRC_COMPENSATOR_H: &str = "vendor/obs-studio/libobs/media-io/asrc-compensator.h";
    pub const ASRC_COMPENSATOR_C: &str = "vendor/obs-studio/libobs/media-io/asrc-compensator.c";

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
    fn no_genlock_env_is_read() {
        // #257: the genlock build is ENV-FREE — render tick + ts-align are build defaults, the
        // latency is a build const, preload is internal/auto. NONE of the old OBS_GENLOCK_* env
        // may be read in obs-source.c (a `git subtree pull` re-introducing one would re-open the
        // env model #257 removed).
        let src = squish(&vendor_file(OBS_SOURCE));
        for env in [
            "getenv(\"OBS_GENLOCK_PRELOAD_FRAMES\")",
            "getenv(\"OBS_GENLOCK_RESERVE_MS\")",
            "getenv(\"OBS_GENLOCK_LATENCY_MS\")",
            "getenv(\"OBS_GENLOCK_TS_ALIGN\")",
            "getenv(\"OBS_GENLOCK_WALL_CLOCK\")",
        ] {
            assert!(
                !src.contains(env),
                "{OBS_SOURCE}: #257 — {env} is BACK; the genlock build must be env-free \
                 (render tick + ts-align build defaults, latency build const, preload auto)."
            );
        }
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
    fn audit_log_have_vi_guards_fps_den() {
        // #259: the genlock audit-log latency_frames integer divide
        //   (latency_ms*fps_num + (1000*fps_den)/2) / (1000*fps_den)
        // SIGFPEs on the render thread if fps_den==0. The `have_vi` guard that gates it
        // must check BOTH fps_num != 0 AND fps_den != 0 (mirroring genlock_source_drop_cap,
        // the only other site that does this divide and DOES guard fps_den). Anchor on the
        // unique `have_vi` declaration so drop_cap's own fps_den guard can't satisfy this
        // (the test is RED until the audit guard itself adds fps_den).
        let raw = vendor_file(OBS_SOURCE);
        let pos = raw
            .find("have_vi =")
            .expect("#259: the genlock audit-log `have_vi` guard is gone — re-locate");
        let tail = &raw[pos..];
        let semi = tail
            .find(';')
            .expect("#259: the have_vi statement has no terminator");
        let stmt = squish(&tail[..semi]);
        assert!(
            stmt.contains("fps_num != 0") && stmt.contains("fps_den != 0"),
            "{OBS_SOURCE}: #259 — the genlock audit-log `have_vi` guard must check BOTH \
             fps_num != 0 AND fps_den != 0 before the latency_frames divide (it currently \
             guards fps_num only → SIGFPE when fps_den==0). Mirror genlock_source_drop_cap. \
             Got: `{stmt}`"
        );
    }

    #[test]
    fn fps_pair_read_is_tear_checked() {
        // #200: the genlock audit/preload path read the unlocked ovi fps pair directly via
        // obs_get_video_info(), which a concurrent obs_reset_video() can TEAR. The fix is a
        // single tear-checked snapshot helper (genlock_video_fps) used at all four fps-read
        // sites (drop_cap / preload_ms / frame_interval_ns / audit_log) — NO hot-path lock
        // (deadlock risk vs obs_reset_video), a value-seqlock instead. Guard the helper, its
        // agreement check, and that the four sites use it, so a subtree pull can't revert it.
        let src = squish(&vendor_file(OBS_SOURCE));
        assert!(
            src.contains("static bool genlock_video_fps("),
            "{OBS_SOURCE}: #200 — the tear-checked fps snapshot helper genlock_video_fps is \
             missing; the genlock audit/preload path reads ovi.fps_num/fps_den unlocked \
             (torn pair). Re-apply."
        );
        assert!(
            src.contains("a.fps_num == b.fps_num && a.fps_den == b.fps_den"),
            "{OBS_SOURCE}: #200 — genlock_video_fps no longer compares two back-to-back \
             snapshots for agreement (the value-seqlock that rejects a torn read); re-apply."
        );
        // 1 definition + the 4 call sites = >= 5 occurrences of `genlock_video_fps(`.
        let calls = src.matches("genlock_video_fps(").count();
        assert!(
            calls >= 5,
            "{OBS_SOURCE}: #200 — genlock_video_fps is used at fewer than the four genlock \
             fps-read sites (drop_cap/preload_ms/frame_interval_ns/audit_log); a site still \
             reads the ovi pair unlocked. Found {calls} occurrence(s) incl. the definition."
        );
    }

    #[test]
    fn timestamp_aligned_release_present() {
        // #136/#257: the genlock_fifo branch must offer the timestamp-aligned release path
        // (multi-source IN-SYNC). #257: ts-align is a BUILD DEFAULT (genlock_ts_align_enabled
        // returns true, no OBS_GENLOCK_TS_ALIGN env) but the per-frame wall-clock guard +
        // present_ts deadline stay. A subtree pull (#44) dropping them silently reverts the
        // desync fix. Mirror of src/probe/genlock.rs genlock_release.
        use camera_box::probe::genlock::{WALLCLOCK_TS_MAX_NS, WALLCLOCK_TS_MIN_NS};
        let src = squish(&vendor_file(OBS_SOURCE));
        assert!(
            src.contains("genlock_is_wallclock_ts(next_frame->timestamp)"),
            "{OBS_SOURCE}: #136 — the ts-align path no longer guards on \
             genlock_is_wallclock_ts(next_frame->timestamp) (the count-gate fallback for \
             non-camera sources); re-apply."
        );
        assert!(
            src.contains("genlock_present_ts(wall_now, preload, interval)"),
            "{OBS_SOURCE}: #136/#269 [3] — the presentation deadline (genlock_present_ts from the \
             hoisted single wall-clock read `wall_now`) is gone; re-apply."
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
    fn backward_clock_step_recovery_present_in_vendored_source() {
        // #147: the SINK ts-align release must RE-ANCHOR on a backward DanteSync wall-clock
        // step (NTP/PTP sawtooth) instead of HOLDing (freezing the program feed) forever.
        // A subtree pull (#44) dropping the guard silently reverts the freeze fix while the
        // Rust mirror unit tests stay green (the probe-gated mirror compiles to nothing in
        // the default gate). Mirror of src/probe/genlock.rs genlock_release_guarded.
        let src = squish(&vendor_file(OBS_SOURCE));
        // #269 [3]: the backward-step detection tests the MAX queued ts (a frame far AHEAD of
        // the real wall clock = captured before a backward step, impossible for a live cap).
        // The MAX (not array[0], the oldest) makes the trigger DEPTH-independent so every
        // genlock source re-anchors uniformly (no shallow-jumps-while-deep-freezes
        // cross-source desync). #1009: the compare is against wall_now + a RE-QUALIFIED
        // margin (max(3 intervals, 250 ms)), never one interval — the one-interval margin
        // sat only network-delay away from the sender's deliberate ceil-to-boundary future
        // bias and fired on a few ms of inter-box skew (the 2026-08-07 overnight −900 ms
        // hold collapse).
        assert!(
            src.contains("max_ts > wall_now + backward_margin"),
            "{OBS_SOURCE}: #147/#269 [3]/#1009 — the backward-clock-step detection (max queued \
             ts > wall_now + backward_margin) is gone; the ts-align release would FREEZE the \
             program feed on an NTP/PTP backward step (or recover non-uniformly, or regress to \
             the issue-1007 hair-trigger). Re-apply the re-anchor guard."
        );
        // #1009: the margin must be the re-qualified max(3 intervals, 250 ms) — in lock-step
        // with src/genlock_backlog.rs backward_step_margin_ns (Tier-0 unit-tested).
        for marker in [
            "#define GENLOCK_BACKWARD_STEP_MIN_MARGIN_NS 250000000ULL",
            "#define GENLOCK_BACKWARD_STEP_MARGIN_INTERVALS 3ULL",
            "static inline uint64_t genlock_backward_step_margin_ns(",
        ] {
            assert!(
                src.contains(marker),
                "{OBS_SOURCE}: #1009 — the backward-step trigger margin marker `{marker}` is \
                 gone; the guard reverted to the one-interval hair-trigger that collapsed the \
                 894 ms hold overnight. Re-apply (mirror: src/genlock_backlog.rs \
                 backward_step_margin_ns)."
            );
        }
        // #1009: the trigger must be SUSTAINED across consecutive due==0 ticks, never
        // single-tick.
        for marker in [
            "#define GENLOCK_BACKWARD_STEP_SUSTAIN_TICKS 3 ",
            "source->genlock_backward_pending_ticks++",
            "source->genlock_backward_pending_ticks >=",
        ] {
            assert!(
                src.contains(marker),
                "{OBS_SOURCE}: #1009 — the sustained-qualification marker `{marker}` is gone; \
                 a single-tick excursion would re-anchor again. Re-apply (mirror: \
                 src/genlock_backlog.rs BACKWARD_STEP_SUSTAIN_TICKS / BackwardStepGuard)."
            );
        }
        // #1009 SELF-HEAL: leaving the regime must ZERO the locked boundary (the existing
        // ACQUIRE state) so the configured hold is re-established — the pre-#1009 latch
        // clear left the FIFO consuming at the live edge FOREVER (the permanent absorbing
        // state of the overnight collapse).
        for marker in [
            "static void genlock_backward_regime_end(",
            "source->genlock_locked_next_boundary_ns = 0;",
            "genlock_backward_regime_end(source, reserve_ms);",
        ] {
            assert!(
                src.contains(marker),
                "{OBS_SOURCE}: #1009 — the regime-exit SELF-HEAL marker `{marker}` is gone; a \
                 backward-step regime would again leave the hold collapsed permanently (only an \
                 OBS relaunch cleared it). Re-apply (mirror: src/genlock_backlog.rs \
                 BackwardStepGuard SelfHeal)."
            );
        }
        // #1009: a PERSISTENT regime must re-warn on a bounded cadence and count every
        // re-anchored tick into the dedicated audit counter (backward_steps counts events
        // only — the entry-only WARN let the collapse run silent for 3+ hours).
        for marker in [
            "#define GENLOCK_BACKWARD_REGIME_WARN_AFTER_NS 2000000000ULL",
            "#define GENLOCK_BACKWARD_REGIME_WARN_INTERVAL_NS 5000000000ULL",
            "source->genlock_backward_regime_ticks++",
            "backward_regime_ticks=%llu",
        ] {
            assert!(
                src.contains(marker),
                "{OBS_SOURCE}: #1009 — the persistent-regime visibility marker `{marker}` is \
                 gone; a sustained hold-bypass would run silent/uncounted again. Re-apply \
                 (mirror: src/genlock_backlog.rs BACKWARD_REGIME_WARN_* / reanchor_ticks)."
            );
        }
        // The re-anchor must COUNT the event (the genlock_backward_steps audit counter).
        assert!(
            src.contains("source->genlock_backward_steps++"),
            "{OBS_SOURCE}: #147 — the genlock_backward_steps re-anchor counter increment is \
             gone; the backward-step recovery (or its audit signal) reverted. Re-apply."
        );
        // #269 [2]: the counter is LATCHED per event (genlock_in_backward_step) so one step
        // over N recovery ticks counts ONCE, not N — the increment is gated on the rising edge.
        assert!(
            src.contains("source->genlock_in_backward_step = true")
                && src.contains("source->genlock_in_backward_step = false"),
            "{OBS_SOURCE}: #269 [2] — the per-event backward-step latch (genlock_in_backward_step \
             set true on re-anchor, cleared on a benign/normal tick) is gone; the counter would \
             over-report (one step counted per tick) and the LOG_WARNING would spam at frame rate."
        );
        // The audit line must expose the counter so the rig deploy-verify can SEE recoveries.
        assert!(
            src.contains("backward_steps=%llu"),
            "{OBS_SOURCE}: #147 — the genlock-fifo audit line no longer prints \
             backward_steps=; the re-anchor signal is invisible to post-deploy verify. Re-apply."
        );
        // The struct fields must exist (lock-step with the increment, the latch + the audit print).
        let internal = squish(&vendor_file(OBS_INTERNAL));
        assert!(
            internal.contains("uint64_t genlock_backward_steps;"),
            "{OBS_INTERNAL}: #147 — the genlock_backward_steps source-struct field is gone; \
             re-apply (the #147 backward-step recovery counter)."
        );
        assert!(
            internal.contains("bool genlock_in_backward_step;"),
            "{OBS_INTERNAL}: #269 [2] — the genlock_in_backward_step latch field is gone; \
             re-apply (the per-event backward-step counter latch)."
        );
        // #1009: the sustained-qualification + regime-visibility fields must exist in
        // lock-step with the guard code and the Tier-0 mirror.
        for field in [
            "uint32_t genlock_backward_pending_ticks;",
            "uint64_t genlock_backward_regime_start_ns;",
            "uint64_t genlock_backward_last_warn_ns;",
            "uint64_t genlock_backward_regime_ticks;",
        ] {
            assert!(
                internal.contains(field),
                "{OBS_INTERNAL}: #1009 — the backward-step guard field `{field}` is gone; \
                 re-apply (mirror: src/genlock_backlog.rs BackwardStepGuard)."
            );
        }
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
        // #257: the OBS_GENLOCK_RESERVE_MS env + its parser are GONE (the latency is a build const);
        // the ms-granular RELEASE mechanism (genlock_present_ts_reserve) stays and is what the
        // per-source ms latency drives. The reserve default/max #defines are kept for the mirror.
        assert!(
            !src.contains("getenv(\"OBS_GENLOCK_RESERVE_MS\")"),
            "{OBS_SOURCE}: #257 — OBS_GENLOCK_RESERVE_MS env is BACK; the latency is a build const (no env)."
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
            src.contains("genlock_present_ts_reserve(wall_now, reserve_ms)"),
            "{OBS_SOURCE}: #184/#269 [3] — the ts-align render path no longer selects \
             genlock_present_ts_reserve (from the hoisted `wall_now`) when a reserve is \
             configured; the ms-reserve knob is inert. Re-apply."
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
            GENLOCK_LATENCY_MS_MIN,
        };
        let src = squish(&vendor_file(OBS_SOURCE));
        // #257: the genlock latency is a BUILD CONST (no env, no canonical/alias resolution). The
        // env reads + the parser are GONE; genlock_latency_ms() stays as the const accessor.
        assert!(
            !src.contains("getenv(\"OBS_GENLOCK_LATENCY_MS\")"),
            "{OBS_SOURCE}: #257 — OBS_GENLOCK_LATENCY_MS env is BACK; the latency is a build const."
        );
        assert!(
            !src.contains("genlock_parse_latency_ms_set"),
            "{OBS_SOURCE}: #257 — the env latency parser (genlock_parse_latency_ms_set) is BACK; \
             the latency is a build const (no env resolution)."
        );
        assert!(
            src.contains("static uint32_t genlock_latency_ms("),
            "{OBS_SOURCE}: #257 — genlock_latency_ms() (the build-const accessor) is gone; re-apply."
        );
        // #257: the build-const FLOOR (GENLOCK_LATENCY_MS_MIN = 3) must be defined.
        assert!(
            src.contains(&format!(
                "#define GENLOCK_LATENCY_MS_MIN {GENLOCK_LATENCY_MS_MIN}"
            )),
            "{OBS_SOURCE}: #257 — GENLOCK_LATENCY_MS_MIN drifted from the Rust mirror \
             ({GENLOCK_LATENCY_MS_MIN}); keep them in lock-step."
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
        // #257: ts-align is ALWAYS ON (build default) — genlock_ts_align_enabled returns true. The
        // render path still ALSO checks the per-source override (covered by the #245 guard's
        // `genlock_ts_align_enabled() || source->genlock_latency_ms > 0` assertion).
        // preload must be auto-derived (internal) on the ms path — the audit/display log
        // must surface the ms-primary latency with the frame-equivalent in parens (#235 ask).
        assert!(
            src.contains("latency_ms=%u (≈%llu frames @ %.3ffps)"),
            "{OBS_SOURCE}: #235 — the audit log no longer shows 'latency_ms=N (≈M frames)' \
             (ms primary, frames in parens); re-apply the single-knob display."
        );
    }

    #[test]
    fn per_source_latency_override_present_in_vendored_source() {
        // #245: the C side must restore PER-SOURCE latency (the #235 regression collapsed
        // latency to ONE global env knob). The release-deadline gate must pick the source's
        // OWN genlock_latency_ms when >0 else the global default, the setter/getter must
        // exist (and be EXPORTed from obs.h so DistroAV can resolve it), the per-source cap
        // must equal the Rust mirror, a per-source override must imply ts-align ON for that
        // source, the drop-cap must scale with the override, and the audit log must surface
        // the per-source value. A `git subtree pull` (#44) dropping any of these silently
        // reverts the per-source UX. Mirror of src/probe/genlock.rs effective_latency_ms +
        // GENLOCK_SOURCE_LATENCY_MS_MAX.
        use camera_box::probe::genlock::GENLOCK_SOURCE_LATENCY_MS_MAX;
        let src = squish(&vendor_file(OBS_SOURCE));
        // The render-path release deadline is PER-SOURCE (override wins, else global).
        assert!(
            src.contains(
                "source->genlock_latency_ms > 0 ? source->genlock_latency_ms : genlock_reserve_ms()"
            ),
            "{OBS_SOURCE}: #245 — the ts-align release deadline no longer picks the per-source \
             genlock_latency_ms override (else the global default); the per-source latency is \
             inert in the render path. Re-apply."
        );
        // A per-source override must imply ts-align ON for THAT source (else an override on
        // a box with global latency 0 would never reach the ms-reserve path).
        assert!(
            src.contains("genlock_ts_align_enabled() || source->genlock_latency_ms > 0"),
            "{OBS_SOURCE}: #245 — a per-source latency override no longer implies ts-align ON \
             for that source; an override is inert when the global gate is off. Re-apply."
        );
        // The runtime setter/getter must exist (mirror of obs_source_set/get_genlock_preload).
        assert!(
            src.contains("void obs_source_set_genlock_latency_ms(obs_source_t *source, uint32_t")
                && src.contains("uint32_t obs_source_get_genlock_latency_ms("),
            "{OBS_SOURCE}: #245 — the per-source latency setter/getter \
             (obs_source_set/get_genlock_latency_ms) is gone; re-apply."
        );
        // The C per-source cap MUST equal the Rust mirror constant (lock-step).
        assert!(
            src.contains(&format!(
                "#define GENLOCK_SOURCE_LATENCY_MS_MAX {GENLOCK_SOURCE_LATENCY_MS_MAX}"
            )),
            "{OBS_SOURCE}: #245 — GENLOCK_SOURCE_LATENCY_MS_MAX drifted from the Rust mirror \
             ({GENLOCK_SOURCE_LATENCY_MS_MAX}); keep them in lock-step."
        );
        // The drop-cap must scale with the per-source latency frame-equivalent (else a deep
        // override force-drains short and the delay can't build).
        assert!(
            src.contains("source->genlock_latency_ms * fps_num"),
            "{OBS_SOURCE}: #245 — genlock_source_drop_cap no longer scales with the per-source \
             latency frame-equivalent; a deep override (e.g. 2000 ms ≈ 60 frames) would \
             overrun force-drain before its delay builds. Re-apply. (#200 renamed the torn \
             ovi.fps_num read to the genlock_video_fps snapshot local fps_num.)"
        );
        // The audit log must surface the per-source override value (the rig-validation proof).
        assert!(
            src.contains("src_latency_ms=%u"),
            "{OBS_SOURCE}: #245 — the genlock audit log no longer prints the per-source \
             src_latency_ms field; the rig validation can't read per-source latency. Re-apply."
        );
        // The setter must be EXPORTed from obs.h (DistroAV resolves it by name at runtime).
        let api = squish(&vendor_file(OBS_API));
        assert!(
            api.contains(
                "EXPORT void obs_source_set_genlock_latency_ms(obs_source_t *source, uint32_t"
            ),
            "{OBS_API}: #245 — obs_source_set_genlock_latency_ms is not EXPORTed; DistroAV \
             cannot resolve the per-source latency setter. Re-apply the export."
        );
    }

    #[test]
    fn asrc_default_on_present_in_vendored_source() {
        // #912: issue 803 added the per-source ASRC servo (asrc_enabled bool, bzalloc
        // zero-inits it to false) but NOTHING in the vendored tree ever called
        // obs_source_set_asrc_enabled() — the servo shipped permanently inert, exactly the
        // "special command-line tweak nobody remembers to call" the user flagged. #912 makes
        // ASRC a BUILD DEFAULT (mirror of issue 257's render-tick/ts-align hard-lock): every
        // source created via obs_source_create_internal() starts with asrc_enabled = true, no
        // env, no per-source opt-in required. The setter/getter stay EXPORTed as an optional
        // override path (parity with obs_source_set_genlock_burn under the #257 FIFO default).
        let src = squish(&vendor_file(OBS_SOURCE));
        assert!(
            src.contains("source->asrc_enabled = true;"),
            "{OBS_SOURCE}: #912 — obs_source_create_internal no longer defaults asrc_enabled to \
             true; ASRC would silently ship OFF again (the forgettable-toggle problem #912 \
             exists to kill). Re-apply the build-default init."
        );
        // The setter/getter must still exist and be EXPORTed — #912 keeps them as an optional
        // override path, it does not remove the API (mirror of the #245/#257 latency + burn
        // setters staying live under their own hard-locked defaults).
        assert!(
            src.contains("void obs_source_set_asrc_enabled(obs_source_t *source, bool")
                && src.contains("bool obs_source_get_asrc_enabled("),
            "{OBS_SOURCE}: #912 — obs_source_set/get_asrc_enabled is gone; the build-default \
             change must keep the override API, not remove it."
        );
        let api = squish(&vendor_file(OBS_API));
        assert!(
            api.contains("EXPORT void obs_source_set_asrc_enabled(obs_source_t *source, bool"),
            "{OBS_API}: #912 — obs_source_set_asrc_enabled is not EXPORTed; DistroAV/any future \
             GUI override could not resolve the setter."
        );
    }

    #[test]
    fn asrc_starvation_guard_present_in_vendored_source() {
        // #960: the ASRC estimator (asrc-compensator.{h,c}) must reject a block whose
        // instantaneous ppm carries no real timing information (a starved/bursting source, the
        // live incident's -737,600ppm) rather than fold it into the EMA and rail the servo. A
        // `git subtree pull` (#44) or any future hand-edit reverting this guard would silently
        // reintroduce the diagnostic-poisoning/rail-on-garbage defect #960 fixed.
        let h = squish(&vendor_file(ASRC_COMPENSATOR_H));
        assert!(
            h.contains("#define ASRC_MAX_SANE_INSTANTANEOUS_PPM 100000.0"),
            "{ASRC_COMPENSATOR_H}: #960 — the starvation-guard sanity ceiling \
             (ASRC_MAX_SANE_INSTANTANEOUS_PPM) is missing or its value changed; re-apply/re-sync \
             with src/asrc_bench.rs's MAX_SANE_INSTANTANEOUS_PPM."
        );
        assert!(
            h.contains("uint32_t starved_block_count;"),
            "{ASRC_COMPENSATOR_H}: #960 — the starved_block_count telemetry field is missing from \
             struct asrc_compensator."
        );
        assert!(
            h.contains(
                "EXPORT bool asrc_compensator_should_log(struct asrc_compensator *c, double *cumulative_correction_ms_out, uint32_t *starved_block_count_out);"
            ),
            "{ASRC_COMPENSATOR_H}: #960 — asrc_compensator_should_log no longer reports \
             starved_block_count_out; the starved/invalid-block state can't reach the telemetry \
             log line anymore."
        );

        // issue #962: the rejection check now gates the WINDOWED (duration-weighted-summed) ppm
        // value, not a single block's own instantaneous ratio -- see
        // asrc_starvation_guard_gates_the_windowed_ppm_962 below for that literal-text anchor.
        // The vendored caller (obs-source.c) must actually surface the starved state in its
        // telemetry log line, not just compute it and drop it on the floor.
        let src = squish(&vendor_file(OBS_SOURCE));
        assert!(
            src.contains("starved_blocks=%u")
                && src.contains(
                    "asrc_compensator_should_log(&source->asrc, &cumulative_correction_ms, &starved_block_count)"
                ),
            "{OBS_SOURCE}: #960 — the asrc: telemetry log line no longer reports \
             starved_blocks=N; a starved source would silently poison the log again with no \
             indication anything was rejected."
        );
    }

    #[test]
    fn asrc_starvation_guard_gates_the_windowed_ppm_962() {
        // issue #962: the #960 starvation guard must gate the WINDOWED (duration-weighted-summed)
        // ppm value, not a single block's own instantaneous ratio -- per-block instantaneous ppm
        // is unmeasurable noise for small, bursty-delivery blocks (the live mbc incident). A
        // `git subtree pull` (#44) or hand-edit reverting to a per-block gate would silently
        // reintroduce the "small blocks 100% starved-rejected, servo neutral" #962 defect.
        let h = squish(&vendor_file(ASRC_COMPENSATOR_H));
        assert!(
            h.contains("#define ASRC_WINDOW_S 1.0"),
            "{ASRC_COMPENSATOR_H}: #962 — the measurement-window duration constant \
             (ASRC_WINDOW_S) is missing or its value changed; re-apply/re-sync with \
             src/asrc_bench.rs's WINDOW_S."
        );
        assert!(
            h.contains("double window_raw_s;")
                && h.contains("double window_master_s;")
                && h.contains("uint32_t window_block_count;"),
            "{ASRC_COMPENSATOR_H}: #962 — the window-accumulator fields (window_raw_s/\
             window_master_s/window_block_count) are missing from struct asrc_compensator."
        );

        let c = squish(&vendor_file(ASRC_COMPENSATOR_C));
        assert!(
            c.contains("if (fabs(window_ppm) > ASRC_MAX_SANE_INSTANTANEOUS_PPM) {")
                && c.contains("c->starved_block_count += window_block_count;"),
            "{ASRC_COMPENSATOR_C}: #962 — asrc_compensator_compensate no longer gates the \
             starvation ceiling on the WINDOWED ppm value; it would either fold small-bursty-block \
             garbage back into the EMA (no windowing at all) or reintroduce the #960 defect."
        );
    }

    #[test]
    fn asrc_uses_sliding_regression_estimator_1084() {
        // issue #1084: the inner rate estimator must be the sliding least-squares RATE regression
        // (over the #962 window points), NOT the pre-#1084 fixed-gain time-EMA -- the EMA's variance
        // under the 1 s window's endpoint wall-jitter was the global A/V-wander root cause. A
        // `git subtree pull` (#44) or hand-edit reverting to the EMA would silently reintroduce it.
        // Src authority + Tier-0 gate: tests/asrc_endpoint_jitter_1084.rs + src/asrc_bench.rs.
        let h = squish(&vendor_file(ASRC_COMPENSATOR_H));
        assert!(
            h.contains("#define ASRC_REGRESSION_SPAN_S 600.0")
                && h.contains("#define ASRC_REGRESSION_LOCK_SPAN_S 60.0")
                && h.contains("#define ASRC_REGRESSION_MIN_POINTS 30"),
            "{ASRC_COMPENSATOR_H}: #1084 — a regression constant (ASRC_REGRESSION_SPAN_S / \
             LOCK_SPAN_S / MIN_POINTS) is missing or its value changed; re-apply/re-sync with \
             src/asrc_bench.rs's REGRESSION_* constants."
        );
        assert!(
            h.contains("double reg_x[ASRC_REGRESSION_CAP];") && h.contains("bool reg_locked;"),
            "{ASRC_COMPENSATOR_H}: #1084 — the regression point-buffer fields (reg_x[]/reg_locked) \
             are missing from struct asrc_compensator; the estimator would have no state to fit."
        );
        let c = squish(&vendor_file(ASRC_COMPENSATOR_C));
        assert!(
            c.contains("c->estimated_ppm = slope * 1000000.0;"),
            "{ASRC_COMPENSATOR_C}: #1084 — asrc_compensator_compensate no longer sets estimated_ppm \
             from the least-squares slope; the inner estimator was reverted (probably back to the \
             EMA), which is the exact global A/V-wander defect #1084 fixed."
        );
        assert!(
            c.contains("asrc_regression_flush(c);"),
            "{ASRC_COMPENSATOR_C}: #1084 — the regression buffer is never FLUSHED on a level shift \
             (a #960 starved window or a non-positive master_block_s); a level shift would then \
             poison the slope for a full ASRC_REGRESSION_SPAN_S. Re-apply asrc_regression_flush()."
        );
        // The EMA is GONE -- a revert to the old fixed-gain smoothing must fail this test.
        assert!(
            !c.contains("exp(-window_master_s") && !c.contains("ASRC_TIME_CONSTANT_S"),
            "{ASRC_COMPENSATOR_C}: #1084 — the pre-#1084 time-EMA smoothing \
             (exp(-window_master_s / ASRC_TIME_CONSTANT_S)) is BACK; the endpoint-jitter variance \
             it caused is the #1084 root cause. The estimator must be the sliding regression only."
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
    fn ts_align_holds_field_and_sample_in_struct() {
        // #148: the ts-align source-early HOLD counter + the per-tick decision sample
        // (present_ts / due / head-skew) must live on obs_source so the 5s audit line can
        // surface them. A subtree pull dropping them silently reverts the debuggability fix.
        let hdr = squish(&vendor_file(OBS_INTERNAL));
        for field in [
            "genlock_holds",
            "genlock_last_present_ts",
            "genlock_last_due",
            "genlock_last_head_skew_ns",
        ] {
            assert!(
                hdr.contains(field),
                "{OBS_INTERNAL}: #148 field `{field}` missing from obs_source — the ts-align \
                 HOLD/underrun split + decision sample reverted; re-apply."
            );
        }
    }

    #[test]
    fn ts_align_hold_counts_as_hold_not_underrun() {
        // #148: the ts-align due==0 path (source early/stalled, frames queued) must
        // increment genlock_holds, NOT genlock_underruns (folding the two hid the #136
        // boundary churn). Verify the holds increment exists AND sits in the same
        // ts-align branch as the present_ts deadline (so it isn't some unrelated counter).
        let raw = vendor_file(OBS_SOURCE);
        assert!(
            raw.contains("source->genlock_holds++"),
            "{OBS_SOURCE}: #148 — the ts-align source-early HOLD counter \
             (source->genlock_holds++) is gone; the benign hold is mis-counted as an \
             underrun again. Re-apply."
        );
        // Anchor on the unique reserve-deadline call; the holds++ must be within the same
        // ts-align block (it follows the present_ts/due computation, before the present path).
        // #269 widened the window: the [3] max-ts scan + the [0]/[2] comments grew the block.
        // #401 widened it again: the phase-locked release cadence (backward-step branch
        // inverted to early-return + the UNLOCKED/DRIFT/STEADY cadence with its rationale
        // comments) moved the benign source-early holds++ deeper into the block — same
        // counter, same #148 classification, same ts-align block (guarded GREEN by
        // tests/genlock_release_cadence.rs on default features).
        let anchor = raw
            .find("genlock_present_ts_reserve(wall_now, reserve_ms)")
            .expect("#148/#269 [3]: the ts-align reserve deadline (from hoisted wall_now) is gone — re-locate");
        // #859: the window used to be a fixed `anchor + 10000` byte count, which is a PROXY for
        // "the same ts-align block" and had already been widened twice (#269, #401) purely
        // because the block grew. The slew-limited drain pushed holds++ to distance 10032 — 32
        // bytes past the cap — and the test failed for a reason that has nothing to do with what
        // it is asserting. Widening the number a third time would just re-arm the same trap, so
        // scope the window to the ENCLOSING FUNCTION instead: everything up to the next
        // top-level `static` definition. That is the real boundary the assertion means, and it
        // cannot rot as the function grows.
        let window_end = raw[anchor..]
            .find("\nstatic ")
            .map(|rel| anchor + rel)
            .unwrap_or(raw.len());
        assert!(
            raw[anchor..window_end].contains("source->genlock_holds++"),
            "{OBS_SOURCE}: #148 — genlock_holds++ is not in the ts-align decision block \
             (near the present_ts deadline). The source-early HOLD is still counted as an \
             underrun; re-apply."
        );
    }

    #[test]
    fn audit_log_emits_holds_and_ts_align_sample() {
        // #148: the periodic audit line must surface the new signals so a future ts-align
        // regression is debuggable from the log alone (comprehensive-logging).
        let src = squish(&vendor_file(OBS_SOURCE));
        for token in [
            "holds=%llu",
            "ts_present=%llu",
            "ts_due=%u",
            "ts_head_skew_ms=%lld",
        ] {
            assert!(
                src.contains(token),
                "{OBS_SOURCE}: #148 — the genlock audit line no longer emits `{token}`; the \
                 ts-align hold/decision signals are missing from the 5s log. Re-apply."
            );
        }
    }

    #[test]
    fn audit_log_emits_wall_qpc_drift_800() {
        // #800: the 5s audit line must carry the wall(RTC)-vs-monotonic(QPC) clock-domain drift
        // term (anchored at OBS start) so the leading remaining A/V-shift candidate — the two
        // clock domains the WALL-slaved video deadline and the QPC-slaved audio live in —
        // is answerable from the log alone. Mirror: src/jitter_audit.rs AuditSample.wall_qpc_drift_ms
        // + the std-only tests/genlock_wall_qpc_emit.rs anchor. Lock-stepped into BOTH
        // windows-genlock*.yml pwsh gates.
        let src = squish(&vendor_file(OBS_SOURCE));
        assert!(
            src.contains("static long long genlock_wall_qpc_drift_ms(void)"),
            "{OBS_SOURCE}: #800 — the genlock_wall_qpc_drift_ms() helper is gone; nothing \
             computes the wall(RTC)-vs-monotonic(QPC) drift. Re-apply."
        );
        assert!(
            src.contains("wall_qpc_drift_ms=%lld"),
            "{OBS_SOURCE}: #800 — the genlock audit line no longer emits `wall_qpc_drift_ms=%lld`; \
             the clock-domain drift is invisible in the 5s log. Re-apply."
        );
        assert!(
            src.contains("genlock_wall_qpc_drift_ms());"),
            "{OBS_SOURCE}: #800 — genlock_wall_qpc_drift_ms() is no longer passed to the audit \
             blog(); the term would print stale/garbage. Re-apply."
        );
    }

    #[test]
    fn fps_read_returns_cached_last_good_pair_on_a_tear() {
        // #269 [0]/[1]/[2]: genlock_video_fps must keep a LAST-GOOD cache and return it on
        // a persistent tear (not false/0), so genlock_source_drop_cap never collapses to the
        // 30-frame floor and genlock_frame_interval_ns never returns 0 (disengaging ts-align)
        // on a transient ovi-fps tear. Mirror of src/probe/genlock.rs genlock_fps_cached.
        let src = squish(&vendor_file(OBS_SOURCE));
        assert!(
            src.contains("static bool genlock_fps_cache_load("),
            "{OBS_SOURCE}: #269 [0]/[1]/[2] — the lock-free last-good fps cache reader \
             genlock_fps_cache_load is gone; a tear would again collapse the drop-cap / \
             zero the frame interval. Re-apply."
        );
        assert!(
            src.contains(
                "static pthread_mutex_t genlock_fps_cache_lock = PTHREAD_MUTEX_INITIALIZER"
            ),
            "{OBS_SOURCE}: #269 — the fps-cache writer mutex (genlock_fps_cache_lock) is gone; \
             concurrent publishers could corrupt the seqlock. Re-apply."
        );
        // The tear fallback must RETURN the cached pair (the whole point of the #269 fix),
        // not just compute it.
        assert!(
            src.contains("if (genlock_fps_cache_load(fps_num, fps_den)) return true;")
                || src.contains("if (genlock_fps_cache_load(fps_num, fps_den)) { return true; }"),
            "{OBS_SOURCE}: #269 [0]/[1]/[2] — genlock_video_fps no longer returns the cached \
             last-good pair on a persistent tear; it reverted to the bare false/0 return that \
             collapses the drop-cap and zeroes the frame interval. Re-apply."
        );
    }

    #[test]
    fn ts_align_reads_wall_clock_once_per_tick() {
        // #269 [3]: the ts-align tick must read the precise wall clock ONCE
        // (`const uint64_t wall_now = genlock_wall_now_ns();`) and reuse it for BOTH the
        // deadline and the head-skew sample — not call genlock_wall_now_ns() a second time
        // (GetSystemTimePreciseAsFileTime is non-trivial on Windows).
        let src = squish(&vendor_file(OBS_SOURCE));
        assert!(
            src.contains("const uint64_t wall_now = genlock_wall_now_ns();"),
            "{OBS_SOURCE}: #269 [3] — the single hoisted wall-clock read \
             (`const uint64_t wall_now = genlock_wall_now_ns();`) is gone; re-apply."
        );
        assert!(
            src.contains("(int64_t)(wall_now - source->async_frames.array[0]->timestamp)"),
            "{OBS_SOURCE}: #269 [3] — the ts-align head-skew sample no longer reuses the hoisted \
             `wall_now` (it calls genlock_wall_now_ns() a SECOND time per tick); re-apply."
        );
    }

    #[test]
    fn count_gate_build_fill_counts_as_hold_not_underrun() {
        // #269 [4]: the count-gate build-fill HOLD (`!gd.consume`, reached only with num>=1)
        // is benign — it must increment genlock_holds, NOT genlock_underruns. underruns are
        // now TRUE-EMPTY only (the single num==0 guard in get_closest_frame).
        let raw = vendor_file(OBS_SOURCE);
        // Anchor on the count-gate hold comment; genlock_holds++ must be in that branch.
        let anchor = raw
            .find("hold: still BUILDING the preload delay")
            .expect("#269 [4]: the count-gate build-fill hold branch is gone — re-locate");
        let window_end = (anchor + 1500).min(raw.len());
        assert!(
            raw[anchor..window_end].contains("source->genlock_holds++"),
            "{OBS_SOURCE}: #269 [4] — the count-gate build-fill hold no longer increments \
             genlock_holds (it is mis-counted as an underrun again). Re-apply."
        );
        // After the move there is EXACTLY ONE genlock_underruns++ left: the TRUE-EMPTY guard.
        let underruns = raw.matches("source->genlock_underruns++").count();
        assert_eq!(
            underruns, 1,
            "{OBS_SOURCE}: #269 [4] — expected exactly 1 `source->genlock_underruns++` (the \
             TRUE-EMPTY num==0 guard); found {underruns}. The build-fill hold was re-folded into \
             genlock_underruns, or a new spurious underrun site appeared."
        );
    }

    #[test]
    fn stale_ts_align_sample_reset_to_sentinel_off_path() {
        // #269 [5]: genlock_last_present_ts/_due/_head_skew_ns are written ONLY on a ts-align
        // tick but genlock_audit_log prints them unconditionally — so a fall-through / true-empty
        // tick printed a STALE sample. genlock_clear_ts_sample() must reset them to 0 on BOTH the
        // count-gate fall-through AND the true-empty path. Mirror of genlock_ts_audit_sample.
        let raw = vendor_file(OBS_SOURCE);
        let src = squish(&raw);
        assert!(
            src.contains("static inline void genlock_clear_ts_sample(obs_source_t *source)"),
            "{OBS_SOURCE}: #269 [5] — the ts-align sample reset helper genlock_clear_ts_sample \
             is gone; the 5s audit would reprint a stale present/due/skew. Re-apply."
        );
        let calls = raw.matches("genlock_clear_ts_sample(source);").count();
        assert!(
            calls >= 2,
            "{OBS_SOURCE}: #269 [5] — genlock_clear_ts_sample(source) is called {calls} time(s); \
             expected >=2 (the count-gate fall-through AND the true-empty path). A non-ts-align \
             tick would print a stale sample. Re-apply."
        );
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
    fn drop_cap_budgets_at_source_arrival_fps_in_vendored_source() {
        let src = squish(&vendor_file(OBS_SOURCE));
        // #292: genlock_source_drop_cap must budget the latency depth at the worst-case
        // SOURCE arrival rate (GENLOCK_MAX_SOURCE_FPS = 60), NOT the canvas output fps —
        // the FIFO fills at the arrival rate (a 60 fps feed into a 30 fps canvas), so a
        // canvas-fps budget undercounts the held depth ~2x and force-drains a deep latency
        // at ~450 ms. A subtree pull / revert that drops the define or the budget term
        // silently re-caps the operator's A/V-align delay.
        assert!(
            src.contains("#define GENLOCK_MAX_SOURCE_FPS 60"),
            "{OBS_SOURCE}: #292 GENLOCK_MAX_SOURCE_FPS define missing/changed; the drop-cap \
             reverted to a canvas-fps budget that caps latency at ~450 ms. The Rust mirror \
             is {GENLOCK_MAX_SOURCE_FPS}."
        );
        assert!(
            src.contains("source->genlock_latency_ms * GENLOCK_MAX_SOURCE_FPS"),
            "{OBS_SOURCE}: #292 — genlock_source_drop_cap no longer budgets latency_frames \
             at the source arrival rate (GENLOCK_MAX_SOURCE_FPS); a deep latency \
             force-drains at ~450 ms. Re-apply."
        );
        assert_eq!(
            GENLOCK_MAX_SOURCE_FPS, 60,
            "Rust mirror must equal the C define"
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

    // ---- #278 multiview ADAPTIVE budget-based decouple -------------------------

    #[test]
    fn display_render_adaptive_budget_gate_present() {
        // #278: render_display() must skip a throttled monitoring display BEFORE
        // render_display_begin() (no clear/present → no flicker, ~0 cost) when its measured
        // EWMA render cost would not fit the budget REMAINING after the program this tick —
        // the ADAPTIVE replacement for #276's fixed every-Nth skip (which a single 4-live-cam
        // multiview render overran). A subtree pull dropping this re-opens the 29%
        // program-renderSkip / 43fps regression measured on the rig.
        let src = squish(&vendor_file(OBS_DISPLAY));
        // #293: the skip decision is now the pure obs_display_should_skip() helper (with the
        // anti-starvation floor); render_display() must call it with the per-display skip
        // counter and bump that counter on a skip.
        assert!(
            src.contains("#include \"obs-display-budget.h\""),
            "{OBS_DISPLAY}: #293 — render_display() no longer includes obs-display-budget.h; the \
             pure, testable skip decision (with the anti-starvation floor) is gone."
        );
        assert!(
            src.contains(
                "if (obs_display_should_skip(effective_divisor, display->render_frame_counter, ewma, elapsed, budget,"
            ),
            "{OBS_DISPLAY}: #278/#293/#756 — render_display() no longer calls \
             obs_display_should_skip() with the frame_counter-carrying signature; the adaptive \
             budget-skip gate (or the #756 hard cadence floor) is gone and the multiview would \
             steal the 60fps program budget, or never actually throttle, again."
        );
        assert!(
            src.contains("display->render_frame_counter++;"),
            "{OBS_DISPLAY}: #756 — render_display() no longer bumps render_frame_counter every \
             tick; the hard cadence floor has nothing to count and a cheap monitoring display \
             would render every tick again (the imag-nb live regression)."
        );
        assert!(
            src.contains("display->render_consecutive_skips++;"),
            "{OBS_DISPLAY}: #293 — render_display() no longer bumps render_consecutive_skips on a \
             skip; the anti-starvation floor cannot count consecutive skips and the multiview \
             could freeze again."
        );
        assert!(
            src.contains("display->render_consecutive_skips = 0;"),
            "{OBS_DISPLAY}: #293 — render_display() no longer resets render_consecutive_skips after \
             a real render; the floor would force a render on a stale count."
        );
        assert!(
            src.contains("const uint64_t budget = interval - interval / 10;"),
            "{OBS_DISPLAY}: #278 — the 90% frame-budget (interval - interval/10) is gone."
        );
        assert!(
            src.contains("display->render_ewma_ns = prev ? (prev * 3 + dur) / 4 : dur;"),
            "{OBS_DISPLAY}: #278 — the per-display render-cost EWMA update is gone; the \
             budget gate can no longer learn a display is heavy."
        );
        assert!(
            src.contains(
                "void obs_display_set_render_divisor(obs_display_t *display, uint32_t divisor)"
            ),
            "{OBS_DISPLAY}: #278 — obs_display_set_render_divisor() impl missing; the frontend \
             cannot mark the multiview as a throttleable monitoring display."
        );

        // #293: the pure skip decision + its liveness floor live in obs-display-budget.h.
        let bud = squish(&vendor_file(OBS_DISPLAY_BUDGET));
        assert!(
            bud.contains("#define OBS_DISPLAY_MAX_CONSECUTIVE_SKIPS 3u"),
            "{OBS_DISPLAY_BUDGET}: #293 — the OBS_DISPLAY_MAX_CONSECUTIVE_SKIPS liveness-floor \
             constant is gone; an over-budget monitoring display could freeze forever."
        );
        assert!(
            bud.contains("return consecutive_skips < OBS_DISPLAY_MAX_CONSECUTIVE_SKIPS;"),
            "{OBS_DISPLAY_BUDGET}: #293 — the anti-starvation floor (skip an over-budget display \
             only while consecutive_skips < K) is gone; the #278 freeze returns."
        );
        assert!(
            bud.contains("if (render_divisor > 1 && (frame_counter % render_divisor) != 0)"),
            "{OBS_DISPLAY_BUDGET}: #756 — the hard cadence-floor term is gone; a monitoring \
             display cheap enough to always fit under budget would render every tick again \
             instead of throttling to 1/render_divisor (the imag-nb live regression)."
        );
    }

    #[test]
    fn display_render_adaptive_struct_fields_present() {
        // #278: the monitoring-display marker (render_divisor) + its render-cost EWMA must
        // live PER-INSTANCE on struct obs_display (NOT static — a static would lockstep every
        // projector). Read+written only on the graphics thread.
        let hdr = squish(&vendor_file(OBS_INTERNAL));
        assert!(
            hdr.contains("uint64_t render_ewma_ns;"),
            "{OBS_INTERNAL}: #278 — obs_display.render_ewma_ns field missing; re-apply."
        );
        assert!(
            hdr.contains("uint32_t render_divisor;"),
            "{OBS_INTERNAL}: #278 — obs_display.render_divisor field missing; re-apply."
        );
        assert!(
            hdr.contains("uint32_t render_consecutive_skips;"),
            "{OBS_INTERNAL}: #293 — obs_display.render_consecutive_skips field missing; the \
             anti-starvation floor has nowhere to count consecutive skips → the multiview can \
             freeze again."
        );
        assert!(
            hdr.contains("uint64_t graphics_frame_start_ns;"),
            "{OBS_INTERNAL}: #278 — obs_core_video.graphics_frame_start_ns field missing; the \
             adaptive skip cannot measure how much budget the program already used."
        );
        assert!(
            hdr.contains("uint64_t last_tick_total_ns;"),
            "{OBS_INTERNAL}: #1063 — obs_core_video.last_tick_total_ns field missing; the aux \
             budget gate loses its order-independent 'previous tick total' term, so an aux \
             ndi_filter that decides early in the tick under-throttles."
        );
        assert!(
            hdr.contains("uint32_t render_frame_counter;"),
            "{OBS_INTERNAL}: #756 — obs_display.render_frame_counter field missing; the hard \
             cadence floor has nowhere to count ticks and a cheap monitoring display would \
             render every tick again (the imag-nb live regression)."
        );
    }

    #[test]
    fn aux_ndi_sender_budget_gate_present_879() {
        // #879: the strih aux NDI senders (interkom/MULTIVIEW/Grading) are ndi_filter
        // republishes whose render+send must yield to the program (ndi_output) under
        // graphics-thread budget pressure — reusing the SAME pure decision the projector path
        // uses. A subtree pull dropping any of these re-opens the unconditional-aux-render
        // bypass (three full-scene renders every tick, ~13ms of the 33.3ms budget at idle).
        //
        // Executable parity + never-freeze invariants: tests/aux_sender_budget_879.rs (compiles
        // and runs the shipped C). These anchors additionally fail the Windows genlock build
        // loudly if the mechanism is reverted, mirrored into BOTH windows-genlock*.yml.
        let bud = squish(&vendor_file(OBS_DISPLAY_BUDGET));
        assert!(
            bud.contains(
                "static inline uint32_t obs_effective_render_divisor(uint32_t configured_divisor, uint64_t frame_interval_ns)"
            ),
            "{OBS_DISPLAY_BUDGET}: #879 — the pure canvas-rate effective-divisor helper the aux path reuses is gone; the aux senders would lose their budget derivation."
        );
        assert!(
            bud.contains("return derived < configured_divisor ? derived : configured_divisor;"),
            "{OBS_DISPLAY_BUDGET}: #879 — obs_effective_render_divisor no longer clamps to the configured upper bound (min(derived, configured))."
        );

        let api = squish(&vendor_file(OBS_API));
        assert!(
            api.contains(
                "EXPORT bool obs_aux_sender_should_skip(uint32_t render_divisor, uint32_t frame_counter,"
            ),
            "{OBS_API}: #879 — obs_aux_sender_should_skip() is not EXPORTed; the DistroAV filter cannot link the aux budget gate."
        );

        let core = squish(&vendor_file(OBS_CORE_C));
        assert!(
            core.contains(
                "const uint32_t effective_divisor = obs_effective_render_divisor(render_divisor, interval);"
            ),
            "{OBS_CORE_C}: #879 — obs_aux_sender_should_skip() no longer derives the canvas-rate effective divisor before delegating to obs_display_should_skip()."
        );
        assert!(
            core.contains(
                "const uint64_t consumed = (elapsed > last_tick_total) ? elapsed : last_tick_total;"
            ),
            "{OBS_CORE_C}: #1063 — obs_aux_sender_should_skip() no longer gates on max(elapsed, obs->video.last_tick_total_ns); the budget term is render-order-dependent again and an aux filter that decides early in the tick under-throttles."
        );

        let flt = squish(&vendor_file(NDI_FILTER));
        assert!(
            flt.contains(
                "if (obs_aux_sender_should_skip(f->render_divisor, f->render_frame_counter, f->render_ewma_ns,"
            ),
            "{NDI_FILTER}: #879 — ndi_filter_render_video() no longer gates its render+send on the budget decision; the aux senders bypass the render budget again and can steal the program's 30fps budget."
        );
        assert!(
            flt.contains(
                "f->render_ewma_ns = f->render_ewma_ns ? (f->render_ewma_ns * 3 + render_dur_879) / 4 : render_dur_879;"
            ),
            "{NDI_FILTER}: #879 — the per-filter render-cost EWMA update is gone; the budget gate can no longer learn an aux sender is heavy."
        );

        // #879 lock-step: BOTH windows-genlock workflows must still carry the assert step, or
        // the 150-min Windows genlock build silently loses the vendored-C guard (issue-912 rule).
        let wf = squish(&vendor_file(WINDOWS_GENLOCK_WF));
        assert!(
            wf.contains("Assert #879 aux NDI sender render-budget gate present"),
            "{WINDOWS_GENLOCK_WF}: #879 aux-sender budget-gate assert step gone — a vendored-C \
             revert would no longer fail the Windows build."
        );
        let wff = squish(&vendor_file(WINDOWS_GENLOCK_FAST_WF));
        assert!(
            wff.contains("Assert #879 aux NDI sender render-budget gate present"),
            "{WINDOWS_GENLOCK_FAST_WF}: #879 aux-sender budget-gate assert step gone — a \
             vendored-C revert would no longer fail the fast Windows build."
        );

        // #879: the inline #776 derivation in render_display() (obs-display.c) is a SECOND copy
        // of obs_effective_render_divisor()'s math; anchor it so projector vs aux cannot silently
        // diverge (the helper itself is parity-locked to the Rust authority separately).
        let disp = squish(&vendor_file(OBS_DISPLAY));
        assert!(
            disp.contains(
                "uint32_t derived = (uint32_t)((target_cell_interval_ns + interval / 2) / interval);"
            ),
            "{OBS_DISPLAY}: #879/#776 — render_display()'s inline effective-divisor derivation \
             changed; it must stay equivalent to obs_effective_render_divisor()."
        );
    }

    #[test]
    fn multiview_fps_audit_line_present_771() {
        // #771: MV fps observability. render_display() (obs-display.c) emits a per-projector
        // `multiview-audit:` line every ~5s with the ACTUAL measured render cadence (renders /
        // window), so the multiview fps is VISIBLE in the OBS log (drift-guard / rig-health-audit
        // / E2E preflight facet) and can be alarmed on a collapse. A subtree pull dropping any of
        // these silently loses the observability + the floor gate. Pure Tier-0 consumer:
        // src/mv_audit.rs (parser + target floor, byte-identical to obs_multiview_floor_fps()).
        let disp = squish(&vendor_file(OBS_DISPLAY));
        assert!(
            disp.contains(
                "\"multiview-audit: monitor=%u divisor=%u rendered_fps=%.1f target=%.0f floor=%.1f cx=%u cy=%u pre_mv_ms=%.2f pre_mv_max_ms=%.2f mv_ewma_ms=%.2f budget_ms=%.2f\""
            ),
            "{OBS_DISPLAY}: #771 — the multiview-audit blog line is gone; the multiview render fps \
             is no longer visible in the OBS log."
        );
        assert!(
            disp.contains("if (audit_elapsed >= MULTIVIEW_AUDIT_WINDOW_NS)"),
            "{OBS_DISPLAY}: #771 — the ~5s audit-window gate is gone; the multiview-audit line no \
             longer emits periodically."
        );
        assert!(
            disp.contains("display->render_audit_render_count++;"),
            "{OBS_DISPLAY}: #771 — the real-render counter is no longer bumped; rendered_fps would \
             always read 0."
        );
        assert!(
            disp.contains("display->render_audit_id = ++next_audit_id;"),
            "{OBS_DISPLAY}: #771 — the stable per-projector audit id assignment is gone; monitor=N \
             would be unstable."
        );

        let bud = squish(&vendor_file(OBS_DISPLAY_BUDGET));
        assert!(
            bud.contains("static inline double obs_multiview_floor_fps(double target_fps)"),
            "{OBS_DISPLAY_BUDGET}: #771/#776/#1212 — the pure target floor helper is gone (or grew \
             params back); the C log line and the Rust gate (src/mv_audit.rs) would diverge. #1212 \
             retired the issue-1110 area sentinel, so the helper takes only target_fps."
        );
        assert!(
            !bud.contains("MULTIVIEW_FLOOR_MAX_CALIBRATED_AREA_PX"),
            "{OBS_DISPLAY_BUDGET}: #1212 — the issue-1110 area sentinel constant is back; the floor \
             must be area-independent (a 4K MV holds median 30fps, floor 28), so the constant and its \
             report-only branch must be gone."
        );
        assert!(
            bud.contains("#define MULTIVIEW_AUDIT_WINDOW_NS 5000000000ULL"),
            "{OBS_DISPLAY_BUDGET}: #771 — the 5s audit-window constant is gone."
        );

        let hdr = squish(&vendor_file(OBS_INTERNAL));
        assert!(
            hdr.contains("uint32_t render_audit_id;"),
            "{OBS_INTERNAL}: #771 — obs_display.render_audit_id is missing; monitor=N has nowhere to live."
        );
        assert!(
            hdr.contains("uint64_t render_audit_window_start_ns;"),
            "{OBS_INTERNAL}: #771 — obs_display.render_audit_window_start_ns is missing; the audit window cannot track its start."
        );
        assert!(
            hdr.contains("uint32_t render_audit_render_count;"),
            "{OBS_INTERNAL}: #771 — obs_display.render_audit_render_count is missing; rendered_fps cannot be measured."
        );

        // #771 lock-step: BOTH windows-genlock workflows must carry the assert step, or the
        // Windows genlock build silently loses the vendored-C guard (issue-912 rule).
        let wf = squish(&vendor_file(WINDOWS_GENLOCK_WF));
        assert!(
            wf.contains("Assert #771 multiview-audit fps observability line present"),
            "{WINDOWS_GENLOCK_WF}: #771 multiview-audit assert step gone — a vendored-C revert \
             would no longer fail the Windows build."
        );
        let wff = squish(&vendor_file(WINDOWS_GENLOCK_FAST_WF));
        assert!(
            wff.contains("Assert #771 multiview-audit fps observability line present"),
            "{WINDOWS_GENLOCK_FAST_WF}: #771 multiview-audit assert step gone — a vendored-C \
             revert would no longer fail the fast Windows build."
        );
    }

    #[test]
    fn graphics_loop_publishes_per_tick_start() {
        // #278: render_display()'s budget math needs the tick start; the graphics loop must
        // publish os_gettime_ns() into obs->video.graphics_frame_start_ns each tick.
        let src = squish(&vendor_file(OBS_VIDEO));
        assert!(
            src.contains("obs->video.graphics_frame_start_ns = frame_start;"),
            "{OBS_VIDEO}: #278 — the graphics loop no longer publishes the per-tick start; the \
             adaptive monitoring skip loses its 'elapsed this tick' reference."
        );
        assert!(
            src.contains("obs->video.last_tick_total_ns = frame_time_ns;"),
            "{OBS_VIDEO}: #1063 — the graphics loop no longer publishes the COMPLETED tick total; \
             the aux budget gate's order-independent term goes stale and it under-throttles an aux \
             filter that decides early in the tick."
        );
    }

    #[test]
    fn display_render_divisor_api_exported() {
        // #276: obs_display_set_render_divisor must be EXPORTed so the frontend
        // (OBSProjector.cpp) can link it.
        let api = squish(&vendor_file(OBS_API));
        assert!(
            api.contains("EXPORT void obs_display_set_render_divisor(obs_display_t *display, uint32_t divisor)"),
            "{OBS_API}: #276 — obs_display_set_render_divisor not EXPORTed; the frontend cannot \
             throttle the multiview."
        );
    }

    #[test]
    fn multiview_projector_sets_render_divisor() {
        // #276: ONLY the Multiview projector is throttled (divisor 2); program output +
        // preview keep the default (every frame). The call must be gated on isMultiview.
        let src = squish(&vendor_file(OBSPROJECTOR));
        assert!(
            src.contains("if (isMultiview) obs_display_set_render_divisor(GetDisplay(), 2)"),
            "{OBSPROJECTOR}: #276 — the multiview projector no longer sets render_divisor=2; \
             the heavy multiview render would run every frame and break the 60fps program."
        );
    }

    // ---- #275 cheaper measurement burn (bulk-fill render) ----------------------

    #[test]
    fn burn_qr_render_uses_bulk_fills() {
        // #275: the per-pixel put_bgra nested loops made the strih genlock_burn render
        // 18.9ms > the 16.6ms 60fps budget. The render must use BULK fills — one memset per
        // row for the white backing + a tight 32-bit run-fill for black module runs —
        // producing IDENTICAL output bytes (white FF FF FF FF, black 00 00 00 FF, proven by
        // the burn_payload_parity render→decode test). A regression to per-pixel fills
        // re-breaks the 60fps measurement burn.
        let src = squish(&vendor_file(BURN_QR));
        assert!(
            src.contains("std::memset(row, 0xFF,"),
            "{BURN_QR}: #275 — the white backing is no longer a per-row memset; the burn \
             render reverted to the slow per-pixel path."
        );
        assert!(
            src.contains("const uint8_t black_px[4] = {0, 0, 0, 255};")
                && src.contains("std::memcpy(&black, black_px, 4);"),
            "{BURN_QR}: #275 — the portable black BGRA run-fill constant is gone; re-apply \
             the bulk black module-run fill."
        );
        // The render must NOT fall back to a per-pixel put_bgra fill loop inside render().
        assert!(
            !src.contains("put_bgra(buf, stride, frame_w, frame_h, (uint32_t)(ox + xx)"),
            "{BURN_QR}: #275 — the per-pixel white put_bgra loop is BACK; that is the slow \
             path #275 removed."
        );
    }

    // ---- #942 dock lock corrector is monitor-only by build default ----------

    #[test]
    fn dock_lock_corrector_is_monitor_only_by_build_default_942() {
        // #942: two independent actuators were writing the SAME live genlock_latency_ms_src knob
        // (the E2E gate, ground-truth-verified once per run; and this in-process corrector,
        // servoing against its own recent output with no ground truth) — a 20-run random walk
        // that never converged. The gate is now the only CONTINUOUS/closed-loop writer (a
        // separate, bounded snapshot-and-restore exception exists around a single delivery-
        // verify test run — scripts/obs_phase2.py::_snapshot_and_set_test_latency, #358/#691 —
        // which is not a second closed-loop actuator); the corrector keeps MEASURING
        // and DISPLAYING (offset/mad/implied correction) but must never itself write the knob.
        // Mirror of tests/genlock_preload.rs::vendored_source::asrc_default_on_present_in_vendored_source
        // (#912) — same hard-lock convention (build default, no env/WebSocket/per-source toggle).
        let audio = squish(&vendor_file(AV_SYNC_DOCK_AUDIO));
        assert!(
            audio.contains("bool CB_DOCK_LOCK_ACTUATION_ENABLED = false;"),
            "{AV_SYNC_DOCK_AUDIO}: #942 — CB_DOCK_LOCK_ACTUATION_ENABLED is missing or not \
             hard-locked false; the dock corrector must never write genlock_latency_ms_src while \
             the E2E gate is the sole writer."
        );
        assert!(
            audio.contains("bool cb_dock_lock_may_actuate()")
                && audio.contains("return CB_DOCK_LOCK_ACTUATION_ENABLED;"),
            "{AV_SYNC_DOCK_AUDIO}: #942 — the pure decision seam cb_dock_lock_may_actuate() is \
             missing; the caller has nothing to gate its actuator write on."
        );

        let output = squish(&vendor_file(AV_SYNC_DOCK_OUTPUT));
        // #955: the Write/Suggest/RailWarn/Quiet decision is now a pure extracted function
        // (cb_dock_lock_outcome(), tests/av_sync_dock_outcome_955.rs proves its OWN behavior
        // byte-identically); this file only needs to prove the CALLER actually wires the real
        // cb_dock_lock_may_actuate() seam into it -- not a hardcoded true/false -- and that the
        // resulting `switch` genuinely gates the write behind the Write case.
        assert!(
            output.contains(
                "camerabox::CbDockLockOutcome outcome = camerabox::cb_dock_lock_outcome( \
                 act, camerabox::cb_dock_lock_may_actuate(), est.offset_ms, current_ms); \
                 switch (outcome) { \
                 case camerabox::CbDockLockOutcome::Write: { \
                 const double delta_ms = (double)(act.new_delay_ms - current_ms); \
                 cb_apply_lock_latency_ms(act.new_delay_ms);"
            ),
            "{AV_SYNC_DOCK_OUTPUT}: #955 — either cb_dock_lock_outcome() is no longer called \
             with cb_dock_lock_may_actuate() as its actuation-permission argument, or \
             cb_apply_lock_latency_ms() is no longer the first statement immediately inside the \
             `case camerabox::CbDockLockOutcome::Write:` arm; the write may have moved outside \
             the gated case even though the pieces still exist somewhere else in the file."
        );
        // #942 hardening (unchanged intent, #955 update): the check above only proves the WRITE
        // arm's text exists — it doesn't prove the write is UNREACHABLE from anywhere else. Pin
        // that cb_apply_lock_latency_ms() is called with its real argument in EXACTLY one place
        // in the whole file (the Write case above) — the Suggest case's own explanatory comment
        // deliberately mentions the bare function NAME ("no cb_apply_lock_latency_ms()") to
        // document what it must NOT do, so anchor on the full call form with its real argument,
        // which that prose does not contain, instead of stripping comments.
        assert_eq!(
            output
                .matches("cb_apply_lock_latency_ms(act.new_delay_ms)")
                .count(),
            1,
            "{AV_SYNC_DOCK_OUTPUT}: #942/#955 — cb_apply_lock_latency_ms(act.new_delay_ms) must \
             appear in EXACTLY one place (the gated Write case); a second call site would \
             silently reintroduce the #942 dual-actuator write."
        );
        // The monitor-only Suggest case: strip comments first. Its own explanatory comment
        // literally reads "no cb_apply_lock_latency_ms(), no rebase()" to document what the
        // branch must NOT do -- a naive substring check on the raw text (with comments left in)
        // would find that prose and could mask a real regression, or misfire on an innocent
        // comment edit. See strip_cpp_comments()'s own doc comment for why.
        let output_nc = squish(&strip_cpp_comments(&vendor_file(AV_SYNC_DOCK_OUTPUT)));
        const SUGGEST_CASE_START: &str = "case camerabox::CbDockLockOutcome::Suggest: {";
        assert_eq!(
            output_nc.matches(SUGGEST_CASE_START).count(),
            1,
            "{AV_SYNC_DOCK_OUTPUT}: #955 — \"{SUGGEST_CASE_START}\" (comments stripped) is no \
             longer unique in this file; the branch-slice below would grab the wrong region."
        );
        let branch_pos = output_nc.find(SUGGEST_CASE_START).unwrap_or_else(|| {
            panic!(
                "{AV_SYNC_DOCK_OUTPUT}: #955 — the monitor-only \
                 `case camerabox::CbDockLockOutcome::Suggest:` arm is gone; cannot verify it \
                 stayed write-free."
            )
        });
        let after_branch_start = &output_nc[branch_pos + SUGGEST_CASE_START.len()..];
        const RAIL_WARN_CASE_START: &str = "case camerabox::CbDockLockOutcome::RailWarn:";
        let branch_end = after_branch_start
            .find(RAIL_WARN_CASE_START)
            .unwrap_or_else(|| {
                panic!(
                    "{AV_SYNC_DOCK_OUTPUT}: #955 — could not find the end of the monitor-only \
                 Suggest case (no following \"{RAIL_WARN_CASE_START}\"); cannot bound the slice \
                 to check it."
                )
            });
        let monitor_branch = &after_branch_start[..branch_end];
        assert!(
            !monitor_branch.contains("cb_apply_lock_latency_ms("),
            "{AV_SYNC_DOCK_OUTPUT}: #942 — the monitor-only Suggest case now calls \
             cb_apply_lock_latency_ms() -- it must only ever LOG the suggested correction, \
             never apply it (that would silently reintroduce the #942 dual-actuator write)."
        );
        assert!(
            !monitor_branch.contains("rebase("),
            "{AV_SYNC_DOCK_OUTPUT}: #942 — the monitor-only Suggest case now calls rebase() -- \
             rebase() assumes a real actuator move happened, which a monitor-only suggestion is \
             not."
        );
        assert!(
            output.contains("LOCK-CORRECT SUGGESTED genlock_latency_ms_src"),
            "{AV_SYNC_DOCK_OUTPUT}: #942 — the monitor-only path must still log the SUGGESTED \
             correction (offset/implied target) even though it is never applied."
        );
    }

    #[test]
    fn windows_genlock_workflows_gate_on_dock_lock_monitor_only_942() {
        // #942 hardening: this Linux-CI guard (above) can't compile on the Windows runner, so
        // BOTH production build workflows must re-assert the #942 hard-lock in pwsh BEFORE their
        // build — mirror of windows_genlock_workflows_gate_on_asrc_default_on (#912). Without
        // this test, deleting BOTH pwsh gate steps entirely is invisible to CI: nothing else
        // pins that the steps exist at all, only what they check while present.
        for (wf_const, wf_path) in [
            (WINDOWS_GENLOCK_WF, "windows-genlock.yml"),
            (WINDOWS_GENLOCK_FAST_WF, "windows-genlock-fast.yml"),
        ] {
            let wf = squish(&vendor_file(wf_const));
            assert!(
                wf.contains("bool CB_DOCK_LOCK_ACTUATION_ENABLED = false;"),
                "{wf_path}: #942 — the pwsh gate no longer asserts \
                 CB_DOCK_LOCK_ACTUATION_ENABLED is hard-locked false; re-add the pwsh #942 gate."
            );
            assert!(
                wf.contains("return CB_DOCK_LOCK_ACTUATION_ENABLED;"),
                "{wf_path}: #942 hardening — the pwsh gate no longer asserts \
                 cb_dock_lock_may_actuate()'s BODY still returns CB_DOCK_LOCK_ACTUATION_ENABLED \
                 directly (only checking the function EXISTS is not enough — its body could be \
                 rewritten to `return true;` and still pass); re-add the #942 body check."
            );
        }
    }
}

// ---- #97 GUI: DistroAV per-source preload slider + ms info-text --------------

mod distroav_source {
    use super::vendored_source::{
        vendor_file, NDI_BURN_FILTER, NDI_SOURCE, OBS_API, OBS_SOURCE, WINDOWS_GENLOCK_FAST_WF,
        WINDOWS_GENLOCK_WF,
    };

    fn squish(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn distroav_ui_is_exactly_the_whitelist() {
        // #257: the DistroAV NDI source UI is a HARD WHITELIST — ndi_source_getproperties exposes
        // EXACTLY source + Genlock + Latency(ms) + Measurement burn, and NOTHING else. The legacy
        // preload slider + the read-only latency/preload info labels + the ~12 forced knobs are
        // REMOVED from the UI (forced via GENLOCK_FORCED_SETTINGS instead). A subtree pull (#44) or a
        // regression re-adding any of them reverts the production-safe hard-lock.
        let src = squish(&vendor_file(NDI_SOURCE));
        // The whitelist const list + the four whitelist add-calls must be present.
        assert!(
            src.contains("GENLOCK_WHITELIST_PROPS"),
            "{NDI_SOURCE}: #257 — the GENLOCK_WHITELIST_PROPS whitelist is gone; re-apply the hard-lock UI."
        );
        assert!(
            src.contains("obs_properties_add_list(props, PROP_SOURCE")
                && src.contains("obs_properties_add_bool(props, PROP_GENLOCK_FIFO")
                && src.contains("obs_properties_add_int(props, PROP_GENLOCK_LATENCY_MS_SRC")
                && src.contains("obs_properties_add_bool(props, PROP_BURN"),
            "{NDI_SOURCE}: #257 — the whitelist UI must add EXACTLY source + Genlock + Latency(ms) + burn."
        );
        // The removed knobs + the old read-only labels + the legacy preload slider must be GONE
        // from the UI (added nowhere). The forcer still references some PROP_* names, so the guard
        // targets the UI add-calls, not the bare token.
        for gone in [
            "obs_properties_add_list(props, PROP_BEHAVIOR",
            "obs_properties_add_list(props, PROP_TIMEOUT",
            "obs_properties_add_list(props, PROP_BANDWIDTH",
            "obs_properties_add_list(props, PROP_SYNC",
            "obs_properties_add_list(props, PROP_LATENCY",
            "obs_properties_add_bool(props, PROP_FRAMESYNC",
            "obs_properties_add_bool(props, PROP_HW_ACCEL",
            "obs_properties_add_bool(props, PROP_FIX_ALPHA",
            "obs_properties_add_bool(props, PROP_AUDIO",
            "obs_properties_add_list(props, PROP_YUV_RANGE",
            "obs_properties_add_group(props, PROP_PTZ",
            "obs_properties_add_int_slider(props, PROP_GENLOCK_PRELOAD",
            "obs_properties_add_text(props, PROP_GENLOCK_LATENCY_MS",
            "obs_properties_add_text(props, PROP_GENLOCK_PRELOAD_MS",
            "apply_genlock_lockdown_visibility",
        ] {
            assert!(
                !src.contains(gone),
                "{NDI_SOURCE}: #257 — '{gone}' is BACK in the UI; the whitelist exposes only \
                 source/Genlock/Latency/burn (everything else is forced, not shown)."
            );
        }
    }

    #[test]
    fn forced_certified_const_table_present() {
        // #257: force_genlock_certified_settings is driven by the GENLOCK_FORCED_SETTINGS const
        // table — the COMPLEMENT of the whitelist — so a value can't drift and an upstream property
        // add/remove can't reintroduce a live knob. The table must pin every forced key (incl. PTZ),
        // and ndi_source_update must still CALL the forcer when genlock is on.
        // #767: PROP_BEHAVIOR is forced to KEEP_ACTIVE (was STOP_RESUME_LAST_FRAME) — a genlocked
        // source's NDI receiver must never tear down on hide (cold reconnect = slow wake + dropped
        // frames on a cut). The anti-assertion below keeps the sleep-on-hide value from returning.
        let src = squish(&vendor_file(NDI_SOURCE));
        assert!(
            src.contains("GENLOCK_FORCED_SETTINGS"),
            "{NDI_SOURCE}: #257 — the GENLOCK_FORCED_SETTINGS const table is gone; re-apply the forcer table."
        );
        for entry in [
            "{PROP_SYNC, false, PROP_SYNC_NDI_SOURCE_TIMECODE, false}",
            "{PROP_BEHAVIOR, false, PROP_BEHAVIOR_KEEP_ACTIVE, false}",
            "{PROP_BANDWIDTH, false, PROP_BW_HIGHEST, false}",
            "{PROP_LATENCY, false, PROP_LATENCY_NORMAL, false}",
            "{PROP_HW_ACCEL, true, 0, true}",
            "{PROP_AUDIO, true, 0, false}",
            "{PROP_FRAMESYNC, true, 0, false}",
            "{PROP_FIX_ALPHA, true, 0, false}",
            "{PROP_PTZ, true, 0, false}",
        ] {
            assert!(
                src.contains(entry),
                "{NDI_SOURCE}: #257 — GENLOCK_FORCED_SETTINGS no longer pins '{entry}'; that key \
                 could be left misconfigured on the genlock path. Re-apply the full forcer table."
            );
        }
        assert!(
            !src.contains("{PROP_BEHAVIOR, false, PROP_BEHAVIOR_STOP_RESUME_LAST_FRAME"),
            "{NDI_SOURCE}: #767 — the forced table pins PROP_BEHAVIOR back to \
             STOP_RESUME_LAST_FRAME; that re-enables receiver teardown on hide (cold reconnect \
             + dropped frames on every cut to a hidden camera). It must stay KEEP_ACTIVE."
        );
        assert!(
            src.contains("force_genlock_certified_settings(settings)"),
            "{NDI_SOURCE}: #257 — ndi_source_update no longer CALLS force_genlock_certified_settings."
        );
    }

    #[test]
    fn genlock_is_default_on() {
        // #257: genlock is DEFAULT ON — a newly-added NDI source is locked down by default.
        let src = squish(&vendor_file(NDI_SOURCE));
        assert!(
            src.contains("obs_data_set_default_bool(settings, PROP_GENLOCK_FIFO, true)"),
            "{NDI_SOURCE}: #257 — genlock is no longer DEFAULT ON (PROP_GENLOCK_FIFO default true)."
        );
    }

    #[test]
    fn burn_runtime_toggle_present() {
        // #257: the measurement burn is a per-source bool (PROP_BURN) applied LIVE in
        // ndi_source_update via obs_source_set_genlock_burn (no OBS_BURN_* env, no restart); the
        // burn filter reads the parent's flag each render.
        let src = squish(&vendor_file(NDI_SOURCE));
        assert!(
            src.contains("#define PROP_BURN \"genlock_burn\""),
            "{NDI_SOURCE}: #257 — PROP_BURN (\"genlock_burn\") define missing; re-apply the burn toggle."
        );
        assert!(
            src.contains("obs_properties_add_bool(props, PROP_BURN"),
            "{NDI_SOURCE}: #257 — the Measurement-burn whitelist UI bool is gone; re-apply."
        );
        assert!(
            src.contains("resolve_set_genlock_burn") && src.contains("obs_source_set_genlock_burn"),
            "{NDI_SOURCE}: #257 — ndi_source_update no longer resolves/applies obs_source_set_genlock_burn."
        );
        assert!(
            src.contains("obs_data_get_bool(settings, PROP_BURN)"),
            "{NDI_SOURCE}: #257 — ndi_source_update no longer reads the PROP_BURN setting."
        );
        assert!(
            src.contains("obs_data_set_default_bool(settings, PROP_BURN, false)"),
            "{NDI_SOURCE}: #257 — the burn default is not wired to false (OFF)."
        );
        // The libobs setter/getter + EXPORT must exist (mirror of genlock_fifo).
        let obs = squish(&vendor_file(OBS_SOURCE));
        assert!(
            obs.contains("void obs_source_set_genlock_burn(obs_source_t *source, bool")
                && obs.contains("bool obs_source_get_genlock_burn("),
            "{OBS_SOURCE}: #257 — the per-source burn setter/getter is gone; re-apply."
        );
        let api = squish(&vendor_file(OBS_API));
        assert!(
            api.contains("EXPORT void obs_source_set_genlock_burn(obs_source_t *source, bool")
                && api.contains("EXPORT bool obs_source_get_genlock_burn("),
            "{OBS_API}: #257 — obs_source_set/get_genlock_burn not EXPORTed; DistroAV + the burn \
             filter cannot resolve them. Re-apply the exports."
        );
        // The burn filter must read the parent's flag (runtime gate) and NOT read OBS_BURN_* env.
        let flt = squish(&vendor_file(NDI_BURN_FILTER));
        assert!(
            flt.contains("obs_source_get_genlock_burn"),
            "{NDI_BURN_FILTER}: #257 — the burn no longer reads the parent's genlock_burn flag; \
             the runtime gate is inert. Re-apply."
        );
        assert!(
            !flt.contains("getenv(\"OBS_BURN"),
            "{NDI_BURN_FILTER}: #257 — an OBS_BURN_* env read is BACK; the burn is a per-source bool (no env)."
        );
    }

    #[test]
    fn per_source_latency_int_editable_present() {
        // #245: the DistroAV source props must offer an EDITABLE per-source latency (ms) int
        // field (the #235 regression made latency a single GLOBAL env knob with no per-source
        // control). The field must exist with the 0..2000 range, be applied in
        // ndi_source_update via the runtime-resolved obs_source_set_genlock_latency_ms setter,
        // default to 0 (= follow global), and floor a negative scene value before the cast. A
        // subtree pull (#44) dropping any of these reverts the per-source UI. Mirror of the
        // libobs side guarded in per_source_latency_override_present_in_vendored_source.
        let src = squish(&vendor_file(NDI_SOURCE));
        // The editable per-source int field (NOT the read-only #235 label).
        assert!(
            src.contains("#define PROP_GENLOCK_LATENCY_MS_SRC \"genlock_latency_ms_src\""),
            "{NDI_SOURCE}: #245 — PROP_GENLOCK_LATENCY_MS_SRC define missing; re-apply the \
             editable per-source latency field."
        );
        // #257: the field range is [PROP_GENLOCK_LATENCY_MS_MIN, PROP_GENLOCK_SOURCE_LATENCY_MS_MAX]
        // (floor 3, not 0) and the default is PROP_GENLOCK_LATENCY_MS_DEFAULT (3).
        assert!(
            src.contains("obs_properties_add_int(props, PROP_GENLOCK_LATENCY_MS_SRC")
                && src.contains("PROP_GENLOCK_LATENCY_MS_MIN")
                && src.contains("PROP_GENLOCK_SOURCE_LATENCY_MS_MAX, 1"),
            "{NDI_SOURCE}: #257 — the editable per-source latency int (range [3, 2000]) is gone \
             or no longer floors at PROP_GENLOCK_LATENCY_MS_MIN; re-apply."
        );
        // The symbolic cap must equal the libobs cap (2000).
        assert!(
            src.contains("#define PROP_GENLOCK_SOURCE_LATENCY_MS_MAX 2000"),
            "{NDI_SOURCE}: #245 — PROP_GENLOCK_SOURCE_LATENCY_MS_MAX must be 2000 to match libobs."
        );
        // The runtime setter resolver + the apply in ndi_source_update.
        assert!(
            src.contains("resolve_set_genlock_latency_ms")
                && src.contains("obs_source_set_genlock_latency_ms"),
            "{NDI_SOURCE}: #245 — ndi_source_update no longer resolves/applies \
             obs_source_set_genlock_latency_ms; the per-source latency field is inert. Re-apply."
        );
        assert!(
            src.contains("obs_data_get_int(settings, PROP_GENLOCK_LATENCY_MS_SRC)"),
            "{NDI_SOURCE}: #245 — ndi_source_update no longer reads the \
             PROP_GENLOCK_LATENCY_MS_SRC setting; re-apply."
        );
        // #257: default is the floor 3 (PROP_GENLOCK_LATENCY_MS_DEFAULT), not 0 (no follow-global).
        assert!(
            src.contains("obs_data_set_default_int(settings, PROP_GENLOCK_LATENCY_MS_SRC, PROP_GENLOCK_LATENCY_MS_DEFAULT)"),
            "{NDI_SOURCE}: #257 — the per-source latency default is not wired to \
             PROP_GENLOCK_LATENCY_MS_DEFAULT (3); re-apply."
        );
        // #257: ndi_source_update floors the per-source latency at PROP_GENLOCK_LATENCY_MS_MIN (3).
        assert!(
            src.contains("ms < PROP_GENLOCK_LATENCY_MS_MIN"),
            "{NDI_SOURCE}: #257 — ndi_source_update no longer floors the latency at \
             PROP_GENLOCK_LATENCY_MS_MIN (3); re-apply (1 -> 3, 0 -> 3)."
        );
        // The C #defines for the floor/default must equal the libobs build consts.
        assert!(
            src.contains("#define PROP_GENLOCK_LATENCY_MS_MIN 3")
                && src.contains("#define PROP_GENLOCK_LATENCY_MS_DEFAULT 3"),
            "{NDI_SOURCE}: #257 — PROP_GENLOCK_LATENCY_MS_MIN/_DEFAULT must be 3 (the build const floor)."
        );
    }

    #[test]
    fn libobs_setter_floors_latency_at_three() {
        // #257: obs_source_set_genlock_latency_ms clamps to [GENLOCK_LATENCY_MS_MIN, MAX] — the
        // floor-3 behavior the spec pins (set 1 -> 3, 0 -> 3). The per-source field also inits to
        // the floor at source create.
        let obs = squish(&vendor_file(OBS_SOURCE));
        assert!(
            obs.contains("ms < GENLOCK_LATENCY_MS_MIN ? GENLOCK_LATENCY_MS_MIN"),
            "{OBS_SOURCE}: #257 — obs_source_set_genlock_latency_ms no longer floors at \
             GENLOCK_LATENCY_MS_MIN (3); the 1->3 / 0->3 clamp is gone. Re-apply."
        );
        assert!(
            obs.contains("source->genlock_latency_ms = GENLOCK_LATENCY_MS_MIN_INIT"),
            "{OBS_SOURCE}: #257 — the per-source latency no longer inits to the floor at create."
        );
    }

    #[test]
    fn windows_genlock_workflow_gates_on_the_hard_lock() {
        // #257: the Windows production build re-asserts the hard-lock tokens in pwsh BEFORE the
        // 150-min build (this Linux Rust guard can't compile on the runner). The legacy preload
        // slider must be GONE; the whitelist + floor-3 + genlock-default-on + per-source burn must
        // be gated.
        let wf = squish(&vendor_file(WINDOWS_GENLOCK_WF));
        assert!(
            wf.contains("GENLOCK_WHITELIST_PROPS"),
            "{WINDOWS_GENLOCK_WF}: #257 — the build no longer gates on the GENLOCK_WHITELIST_PROPS \
             hard-lock UI; re-add the pwsh #257 gate."
        );
        assert!(
            wf.contains("#define GENLOCK_LATENCY_MS_MIN 3"),
            "{WINDOWS_GENLOCK_WF}: #257 — the build no longer gates on the latency floor (3); re-add."
        );
        assert!(
            wf.contains("obs_data_set_default_bool(settings, PROP_GENLOCK_FIFO, true)"),
            "{WINDOWS_GENLOCK_WF}: #257 — the build no longer gates on genlock default-ON; re-add."
        );
        assert!(
            wf.contains("obs_source_get_genlock_burn"),
            "{WINDOWS_GENLOCK_WF}: #257 — the build no longer gates on the per-source burn; re-add."
        );
    }

    #[test]
    fn windows_genlock_workflow_gates_on_the_per_source_latency() {
        // #245: the Windows production build must re-assert the per-source latency tokens in
        // pwsh BEFORE the 150-min build (this Linux Rust guard can't compile on the runner),
        // so a subtree bump can't ship a build without the per-source latency control while
        // the version pin still passes.
        let wf = squish(&vendor_file(WINDOWS_GENLOCK_WF));
        assert!(
            wf.contains("obs_source_set_genlock_latency_ms"),
            "{WINDOWS_GENLOCK_WF}: #245 — the production build no longer asserts the per-source \
             latency API (obs_source_set_genlock_latency_ms); re-add the pwsh #245 gate."
        );
        assert!(
            wf.contains("obs_properties_add_int(props, PROP_GENLOCK_LATENCY_MS_SRC"),
            "{WINDOWS_GENLOCK_WF}: #245 — the production build no longer asserts the editable \
             per-source latency int field; re-add the pwsh #245 gate."
        );
        // #292: the build must also gate on the source-arrival-fps drop-cap budget, so a subtree
        // revert that re-caps a deep latency at ~450ms fails at the token, not silently in prod.
        assert!(
            wf.contains("#define GENLOCK_MAX_SOURCE_FPS 60")
                && wf.contains("source->genlock_latency_ms * GENLOCK_MAX_SOURCE_FPS"),
            "{WINDOWS_GENLOCK_WF}: #292 — the production build no longer gates on the \
             GENLOCK_MAX_SOURCE_FPS drop-cap arrival-fps budget; a revert re-caps latency at \
             ~450ms. Re-add the pwsh #292 gate."
        );
    }

    #[test]
    fn windows_genlock_fast_workflow_gates_on_the_per_source_latency() {
        // #249: the FAST hot-DLL workflow COMPILES the real vendored C/cpp (a broken patch fails the
        // build), but it had NO source-text token gate for #245 — only #136. A `git subtree pull`
        // that silently reverts #245 to inert-but-still-compiling would ship an INERT obs.dll un-gated
        // on the fast path (the slow windows-genlock.yml gained the #245 gate in #248). The fast path
        // must re-assert the same #245 tokens BEFORE its build, mirroring the slow gate.
        let wf = squish(&vendor_file(WINDOWS_GENLOCK_FAST_WF));
        assert!(
            wf.contains("obs_source_set_genlock_latency_ms"),
            "{WINDOWS_GENLOCK_FAST_WF}: #249/#245 — the FAST build does not assert the per-source \
             latency API (obs_source_set_genlock_latency_ms); a subtree pull could ship an inert \
             obs.dll on the fast path. Add the pwsh #245 gate, mirroring windows-genlock.yml."
        );
        assert!(
            wf.contains("obs_properties_add_int(props, PROP_GENLOCK_LATENCY_MS_SRC"),
            "{WINDOWS_GENLOCK_FAST_WF}: #249/#245 — the FAST build does not assert the editable \
             per-source latency int field; add the pwsh #245 gate."
        );
        // #292: the FAST build must also gate on the source-arrival-fps drop-cap budget (mirror the
        // slow gate), so a subtree pull can't hot-swap an obs.dll that re-caps latency at ~450ms.
        assert!(
            wf.contains("#define GENLOCK_MAX_SOURCE_FPS 60")
                && wf.contains("source->genlock_latency_ms * GENLOCK_MAX_SOURCE_FPS"),
            "{WINDOWS_GENLOCK_FAST_WF}: #292 — the FAST build does not gate on the \
             GENLOCK_MAX_SOURCE_FPS drop-cap arrival-fps budget; a subtree pull could hot-swap an \
             obs.dll that re-caps latency at ~450ms. Add the pwsh #292 gate, mirroring windows-genlock.yml."
        );
    }

    #[test]
    fn windows_genlock_workflows_gate_on_asrc_default_on() {
        // #912: BOTH the slow and fast Windows production builds must re-assert the ASRC
        // build-default token in pwsh BEFORE their build (this Linux Rust guard can't compile
        // on the runner) — a `git subtree pull` (#44) that silently reverts the default-on init
        // would ship an obs.dll where ASRC is inert again, un-gated, exactly the regression #912
        // exists to prevent. Mirror of every other #257-style hard-lock lock-step guard here.
        for (wf_const, wf_path) in [
            (WINDOWS_GENLOCK_WF, "windows-genlock.yml"),
            (WINDOWS_GENLOCK_FAST_WF, "windows-genlock-fast.yml"),
        ] {
            let wf = squish(&vendor_file(wf_const));
            assert!(
                wf.contains("source->asrc_enabled = true;"),
                "{wf_path}: #912 — the build no longer gates on the ASRC default-on init \
                 (source->asrc_enabled = true;); re-add the pwsh #912 gate."
            );
        }
    }

    #[test]
    fn windows_genlock_workflows_gate_on_asrc_starvation_guard() {
        // #960: BOTH the slow and fast Windows production builds must re-assert the ASRC
        // starvation-guard tokens in pwsh BEFORE their build (this Linux Rust guard can't compile
        // on the runner) — a `git subtree pull` (#44) or hand-edit that silently reverts the
        // guard would ship an obs.dll where a starved source rails the servo again, exactly the
        // #960 defect. Mirror of every other lock-step guard here (see #912's own version above).
        for (wf_const, wf_path) in [
            (WINDOWS_GENLOCK_WF, "windows-genlock.yml"),
            (WINDOWS_GENLOCK_FAST_WF, "windows-genlock-fast.yml"),
        ] {
            let wf = squish(&vendor_file(wf_const));
            assert!(
                wf.contains("ASRC_MAX_SANE_INSTANTANEOUS_PPM 100000.0"),
                "{wf_path}: #960 — the build no longer gates on the starvation-guard sanity \
                 ceiling (ASRC_MAX_SANE_INSTANTANEOUS_PPM); re-add the pwsh #960 gate."
            );
            assert!(
                wf.contains("starved_blocks=%u"),
                "{wf_path}: #960 — the build no longer gates on the starved_blocks telemetry \
                 field reaching the asrc: log line; re-add the pwsh #960 gate."
            );
        }
    }

    #[test]
    fn windows_genlock_workflow_gates_on_the_backward_step_recovery() {
        // #147: the slow production build must re-assert the backward-clock-step re-anchor
        // tokens in pwsh BEFORE the 150-min build (this Linux Rust guard can't compile on the
        // runner), so a subtree bump can't ship an obs.dll that FREEZES the program feed on an
        // NTP/PTP backward step while the version pin still passes (the #269 lock-step gotcha:
        // a vendored-C guard lives in the YAML too, not only this test).
        let wf = squish(&vendor_file(WINDOWS_GENLOCK_WF));
        assert!(
            wf.contains("source->genlock_backward_steps++"),
            "{WINDOWS_GENLOCK_WF}: #147 — the production build no longer asserts the \
             backward-step re-anchor counter (genlock_backward_steps); re-add the pwsh #147 gate."
        );
        assert!(
            wf.contains("max_ts > wall_now + backward_margin"),
            "{WINDOWS_GENLOCK_WF}: #147/#269 [3]/#1009 — the production build no longer asserts \
             the re-qualified backward-clock-step detection (max queued ts > wall_now + \
             backward_margin); re-add the pwsh #147/#1009 gate."
        );
        assert!(
            wf.contains("genlock_backward_regime_end("),
            "{WINDOWS_GENLOCK_WF}: #1009 — the production build no longer asserts the \
             regime-exit SELF-HEAL (genlock_backward_regime_end); a subtree bump could ship an \
             obs.dll whose hold-collapse is permanent again. Re-add the pwsh #1009 gate."
        );
    }

    #[test]
    fn windows_genlock_fast_workflow_gates_on_the_backward_step_recovery() {
        // #147: the FAST hot-DLL workflow compiles the real vendored C, but a `git subtree pull`
        // reverting the #147 guard to inert-but-still-compiling (e.g. dropping the re-anchor
        // branch) would hot-swap a FREEZING obs.dll un-gated. The fast path must re-assert the
        // same #147 tokens BEFORE its build, mirroring the slow gate.
        let wf = squish(&vendor_file(WINDOWS_GENLOCK_FAST_WF));
        assert!(
            wf.contains("source->genlock_backward_steps++"),
            "{WINDOWS_GENLOCK_FAST_WF}: #147 — the FAST build does not assert the backward-step \
             re-anchor counter (genlock_backward_steps); add the pwsh #147 gate, mirroring \
             windows-genlock.yml."
        );
        assert!(
            wf.contains("max_ts > wall_now + backward_margin"),
            "{WINDOWS_GENLOCK_FAST_WF}: #147/#269 [3]/#1009 — the FAST build does not assert \
             the re-qualified backward-clock-step detection (max queued ts > wall_now + \
             backward_margin); add the pwsh #147/#1009 gate."
        );
        assert!(
            wf.contains("genlock_backward_regime_end("),
            "{WINDOWS_GENLOCK_FAST_WF}: #1009 — the FAST build does not assert the regime-exit \
             SELF-HEAL (genlock_backward_regime_end); a hot-swapped obs.dll could revert to the \
             permanent hold-collapse. Add the pwsh #1009 gate, mirroring windows-genlock.yml."
        );
    }

    // ---- #276 + #275 build gates (the #269 YAML lock-step) ---------------------

    #[test]
    fn windows_genlock_workflow_gates_on_the_multiview_divisor() {
        // #278: the slow PRODUCTION build (which builds the frontend, where OBSProjector.cpp
        // lives) must re-assert the adaptive budget-skip tokens in pwsh BEFORE the 150-min
        // build — this Linux Rust guard can't compile on the runner, and a `git subtree pull`
        // could revert the decouple to inert-but-still-compiling, shipping an obs.dll/frontend
        // that lets the multiview steal the 60fps program budget again.
        let wf = squish(&vendor_file(WINDOWS_GENLOCK_WF));
        assert!(
            wf.contains("if (obs_display_should_skip(effective_divisor, display->render_frame_counter, ewma, elapsed, budget,"),
            "{WINDOWS_GENLOCK_WF}: #278/#293/#756 — the production build no longer asserts the \
             adaptive budget-skip gate with the frame_counter-carrying signature \
             (obs_display_should_skip call); re-add the pwsh gate."
        );
        assert!(
            wf.contains("return consecutive_skips < OBS_DISPLAY_MAX_CONSECUTIVE_SKIPS;"),
            "{WINDOWS_GENLOCK_WF}: #293 — the production build no longer asserts the anti-starvation \
             floor in obs-display-budget.h; re-add the pwsh gate (the multiview could freeze)."
        );
        assert!(
            wf.contains("if (render_divisor > 1 && (frame_counter % render_divisor) != 0)"),
            "{WINDOWS_GENLOCK_WF}: #756 — the production build no longer asserts the hard \
             cadence-floor term in obs-display-budget.h; re-add the pwsh gate (a cheap \
             monitoring display would never actually throttle)."
        );
        assert!(
            wf.contains("display->render_ewma_ns = prev ? (prev * 3 + dur) / 4 : dur;"),
            "{WINDOWS_GENLOCK_WF}: #278 — the production build no longer asserts the render-cost \
             EWMA update; re-add the pwsh #278 gate."
        );
        assert!(
            wf.contains("if (isMultiview) obs_display_set_render_divisor(GetDisplay(), 2)"),
            "{WINDOWS_GENLOCK_WF}: #278 — the production build no longer asserts the multiview \
             projector marking render_divisor=2; re-add the pwsh #278 gate."
        );
    }

    #[test]
    fn windows_genlock_fast_workflow_gates_on_the_multiview_divisor() {
        // #278: the FAST build compiles only libobs (obs.dll), so it can't COMPILE the
        // OBSProjector.cpp change — but it must still source-text assert the libobs adaptive
        // gate + the frontend call (a text guard, no build) so a subtree pull reverting #278
        // fails here on the fast path too, mirroring the slow gate (the #269 lock-step rule).
        let wf = squish(&vendor_file(WINDOWS_GENLOCK_FAST_WF));
        assert!(
            wf.contains(
                "if (obs_display_should_skip(effective_divisor, display->render_frame_counter, ewma, elapsed, budget,"
            ),
            "{WINDOWS_GENLOCK_FAST_WF}: #278/#293/#756 — the FAST build does not assert the \
             adaptive budget-skip gate with the frame_counter-carrying signature \
             (obs_display_should_skip call); add the pwsh gate, mirroring windows-genlock.yml."
        );
        assert!(
            wf.contains("return consecutive_skips < OBS_DISPLAY_MAX_CONSECUTIVE_SKIPS;"),
            "{WINDOWS_GENLOCK_FAST_WF}: #293 — the FAST build does not assert the anti-starvation \
             floor in obs-display-budget.h; add the pwsh gate (the multiview could freeze)."
        );
        assert!(
            wf.contains("if (render_divisor > 1 && (frame_counter % render_divisor) != 0)"),
            "{WINDOWS_GENLOCK_FAST_WF}: #756 — the FAST build does not assert the hard \
             cadence-floor term in obs-display-budget.h; add the pwsh gate (a cheap monitoring \
             display would never actually throttle)."
        );
        assert!(
            wf.contains("display->render_ewma_ns = prev ? (prev * 3 + dur) / 4 : dur;"),
            "{WINDOWS_GENLOCK_FAST_WF}: #278 — the FAST build does not assert the render-cost \
             EWMA update; add the pwsh #278 gate."
        );
        assert!(
            wf.contains("if (isMultiview) obs_display_set_render_divisor(GetDisplay(), 2)"),
            "{WINDOWS_GENLOCK_FAST_WF}: #278 — the FAST build does not assert the multiview \
             render_divisor=2 call; add the pwsh #278 gate."
        );
    }

    #[test]
    fn windows_genlock_workflow_gates_on_the_burn_bulk_fill() {
        // #275: the production build must assert the cheaper bulk-fill burn render (the
        // strih/stream genlock_burn at 60fps), so a subtree pull can't revert it to the slow
        // per-pixel path that overran the 60fps budget.
        let wf = squish(&vendor_file(WINDOWS_GENLOCK_WF));
        assert!(
            wf.contains("std::memset(row, 0xFF,"),
            "{WINDOWS_GENLOCK_WF}: #275 — the production build no longer asserts the bulk-fill \
             burn render (per-row white memset); re-add the pwsh #275 gate."
        );
    }

    #[test]
    fn windows_genlock_fast_workflow_gates_on_the_burn_bulk_fill() {
        // #275: the FAST build COMPILES burn-qr.hpp (DistroAV) — assert the bulk-fill token
        // too so a revert to per-pixel fills fails on the fast path, mirroring the slow gate.
        let wf = squish(&vendor_file(WINDOWS_GENLOCK_FAST_WF));
        assert!(
            wf.contains("std::memset(row, 0xFF,"),
            "{WINDOWS_GENLOCK_FAST_WF}: #275 — the FAST build does not assert the bulk-fill burn \
             render; add the pwsh #275 gate, mirroring windows-genlock.yml."
        );
    }
}
