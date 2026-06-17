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

use camera_box::probe::genlock::{
    genlock_decide, genlock_drop_cap, parse_preload, preload_to_ms, steady_state_depth,
    GenlockDecision, GENLOCK_DROP_CAP_RESERVE, GENLOCK_PRELOAD_DEFAULT, GENLOCK_PRELOAD_MAX,
    MAX_ASYNC_FRAMES,
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

// ---- vendored-source guard (the C patch must stay applied) ------------------

mod vendored_source {
    use camera_box::probe::genlock::GENLOCK_PRELOAD_MAX;
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
        // Both the inactive/flush path AND the overrun force-drain must re-arm the latch
        // (reset to false) so the delay line rebuilds after a flush or drain. Each path
        // resets last_frame_ts=0; the matching genlock_filled=false must accompany it. We
        // assert the latch-reset assignment appears at least twice (drain + inactive) on
        // top of the create-time init — the render-tick writeback uses gd.filled, not a
        // literal false.
        let raw = vendor_file(OBS_SOURCE);
        let resets = raw.matches("source->genlock_filled = false;").count();
        assert!(
            resets >= 3,
            "{OBS_SOURCE}: #102 — expected >=3 `source->genlock_filled = false;` re-arm \
             sites (create init + overrun drain + inactive/flush), found {resets}. A path \
             that drops the latch reset leaks an undelayed frame after that drain/flush."
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
        // The slider label + min(0) + step(1) are pinned; the max may be the literal
        // 128 OR the symbolic PROP_GENLOCK_PRELOAD_MAX (== 128, asserted separately).
        assert!(
            src.contains("PROP_GENLOCK_PRELOAD, \"Genlock preload (video delay)\", 0, 128, 1")
                || src.contains(
                    "PROP_GENLOCK_PRELOAD, \"Genlock preload (video delay)\", 0, PROP_GENLOCK_PRELOAD_MAX, 1"
                ),
            "{NDI_SOURCE}: #97 — the preload slider range/label changed; expected \
             (\"Genlock preload (video delay)\", 0, 128|PROP_GENLOCK_PRELOAD_MAX, 1)."
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
