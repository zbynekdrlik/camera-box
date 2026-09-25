//! Issue 1367 (ROZHODNUTÉ 5827497952) — a std-only static guard that the SHALLOW N==1 per-lock depth
//! is WIRED into the vendored release cadence, not just defined. The pure helpers are held to
//! `src/genlock_n1_depth.rs` by `tests/genlock_relock_selection_parity.rs`; this proves the C
//! actually calls them:
//!
//! 1. the SHED (the N==1 route of `genlock_should_converge_phase`) and the HOLD
//!    (`genlock_should_hold_n1_phase`) both carry the shallow half next to the deep one;
//! 2. the #859 queue-length drain stays out while the shallow depth governs;
//! 3. a lock is an ACQUIRE or a sender-restart GAP RESYNC, and a pin change re-arms the measurement;
//! 4. the present tail samples the floor and latches D (`genlock_shallow_latch`), AFTER the audio
//!    tracker, which follows the latched D (`genlock_video_delay_lock_ms`);
//! 5. the min-latency (imag) box marker, the latch log line and the audit tokens exist.
//!
//! Std-only on purpose: it runs under `cargo test` AND standalone
//! (`CARGO_MANIFEST_DIR=<repo> rustc --test --edition 2021 tests/genlock_shallow_depth_wiring_1367.rs`).
//! Every anchor is whitespace-squished, the same form the pwsh gates in both `windows-genlock*.yml`
//! use.

use std::fs;
use std::path::PathBuf;

const OBS_SOURCE: &str = "vendor/obs-studio/libobs/obs-source.c";

fn squished() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(OBS_SOURCE);
    let raw = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The body of a static function, from its definition to the next top-level `static`.
fn body<'a>(src: &'a str, sig: &str) -> &'a str {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("issue 1367: {OBS_SOURCE} no longer defines `{sig}`"));
    let rest = &src[start + sig.len()..];
    let end = rest.find(" static ").unwrap_or(rest.len());
    &rest[..end]
}

fn assert_in(hay: &str, needle: &str, why: &str) {
    assert!(
        hay.contains(needle),
        "issue 1367: `{needle}` is missing — {why}"
    );
}

#[test]
fn the_shed_and_the_hold_carry_the_shallow_half_1367() {
    let src = squished();
    let converge = body(
        &src,
        "static bool genlock_should_converge_phase(const obs_source_t *source, uint32_t reserve_ms, uint64_t interval, uint64_t wall_now)",
    );
    assert_in(
        converge,
        "genlock_n1_shallow_shed_due(tick_wall, source->genlock_locked_next_boundary_ns, arrival_floor, reserve_ms, interval, source->genlock_shallow_target_frames, source->genlock_ticks_since_drain));",
        "the N==1 converge route must also shed a shallow source toward its latched D",
    );
    let hold = body(
        &src,
        "static bool genlock_should_hold_n1_phase(const obs_source_t *source, uint32_t reserve_ms, uint64_t interval, uint64_t wall_now)",
    );
    assert_in(
        hold,
        "genlock_n1_shallow_hold_due(tick_wall, head_stamp, arrival_floor, reserve_ms, interval, source->genlock_shallow_target_frames, source->genlock_ticks_since_drain));",
        "the N==1 STEADY hold must also hold a shallow source up to its latched D",
    );
    // the deep halves are still there, unchanged in effect.
    assert_in(
        converge,
        "(genlock_n1_shed_due(tick_wall, source->genlock_locked_next_boundary_ns, arrival_floor, reserve_ms, interval, source->genlock_ticks_since_drain) ||",
        "the deep N==1 shed must stay",
    );
    assert_in(
        hold,
        "(genlock_n1_hold_due(tick_wall, head_stamp, arrival_floor, reserve_ms, interval, 1, source->genlock_ticks_since_drain) ||",
        "the deep N==1 hold must stay",
    );
}

#[test]
fn the_release_tick_locks_measures_and_keeps_the_drain_out_1367() {
    let src = squished();
    let tick = body(
        &src,
        "static bool genlock_release_tick(obs_source_t *source, uint64_t wall_now, uint64_t present_ts, size_t due, uint64_t interval, uint32_t reserve_ms, uint64_t now_ns)",
    );
    assert_in(
        tick,
        "bool genlock_shallow_relock = source->genlock_locked_next_boundary_ns == 0;",
        "an ACQUIRE must be a relock of the shallow depth",
    );
    assert_in(
        tick,
        "if (genlock_n1_shallow_gap_is_relock(source->async_frames.array[0]->timestamp - source->genlock_locked_next_boundary_ns)) genlock_shallow_relock = true;",
        "a sender-restart GAP RESYNC must be a relock",
    );
    let eligible = tick
        .find("drain_eligible = true;")
        .expect("issue 1367: the N==1 STEADY drain mark is gone");
    let out = tick
        .find("if (genlock_n1_shallow_governs_now(source, reserve_ms, interval, wall_now)) drain_eligible = false;")
        .expect("issue 1367: the #859 queue-length drain must stay out while the shallow depth governs");
    assert!(
        out > eligible,
        "issue 1367: the shallow drain suppression must follow the N==1 drain mark"
    );
    let track = tick
        .find("genlock_video_delay_lock_ms(source->genlock_shallow_target_frames, source->genlock_shallow_measuring, interval), genlock_video_delay_sample_ns(genlock_delay_tick_wall, next_frame->timestamp), interval);")
        .expect("issue 1367: the audio tracker no longer follows the latched shallow depth");
    assert_in(
        tick,
        "&source->genlock_video_delay_settle_ticks, &source->genlock_video_delay_locked_ms, genlock_video_delay_lock_ms(",
        "the tracker remembers the lock it applied, so the realized delay can bound the hold \
         (design 5830750134)",
    );
    let latch = tick
        .find("genlock_shallow_latch(source, genlock_delay_tick_wall, wall_now, interval, reserve_ms, genlock_shallow_relock);")
        .expect("issue 1367: the present tail no longer samples the arrival floor and latches D");
    assert!(
        latch > track,
        "issue 1367: the shallow latch must sit after the tracker (it reuses the scheduled tick read)"
    );
    assert_eq!(
        src.matches("genlock_shallow_latch(source, ").count(),
        1,
        "issue 1367: the shallow latch has exactly one call site (the present tail)"
    );
    let helper = body(
        &src,
        "static void genlock_shallow_latch(obs_source_t *source, uint64_t tick_wall, uint64_t wall_now, uint64_t interval, uint32_t reserve_ms, bool relock)",
    );
    assert_in(
        helper,
        "source->genlock_last_known_n < 2, relock, genlock_n1_tick_is_on_grid(tick_wall, interval), genlock_n1_depth_frames(tick_wall, newest_stamp, interval), base_frames, genlock_n1_is_deep_source(",
        "the latch samples the newest frame's floor on an on-grid N==1 tick (an N>=2 tick clears it)",
    );
    assert_in(
        helper,
        "reserve_ms, interval), genlock_min_latency_box(), genlock_n1_depth_frames(tick_wall, source->last_frame_ts, interval), backlog_relock,",
        "the latch passes the deep flag, the min-latency (imag) box guard, and (design 5830750134) the \
         REALIZED depth of the frame this tick presents plus the backlog-relock event",
    );
    assert_in(
        helper,
        "source->genlock_shallow_hist, &source->genlock_shallow_under_ticks, &source->genlock_shallow_churn_relocks, &source->genlock_shallow_churn_quiet_ticks, &source->genlock_shallow_rejects);",
        "the latch keeps the window histogram (p90 + spread) and the downward re-measure state",
    );
    assert_in(
        helper,
        "const bool backlog_relock = source->genlock_relocks != source->genlock_shallow_relocks_seen; source->genlock_shallow_relocks_seen = source->genlock_relocks;",
        "the churn input is the BACKLOG relock counter's delta since the previous call",
    );
    assert_in(
        helper,
        "\"genlock-shallow-remeasure '%s': reason=%s depth_frames=%llu realized_frames=%llu floor_frames=%llu \"",
        "every re-measure without a relock is logged with its reason (design 5830750134)",
    );
    assert_in(
        &src,
        "_Static_assert(GENLOCK_SHALLOW_HIST_FIELD_BINS == GENLOCK_N1_SHALLOW_HIST_BINS,",
        "the obs_source histogram field and the helper's bin count must agree",
    );
    assert_in(
        helper,
        "&source->genlock_shallow_deep_ticks, &source->genlock_shallow_measuring, &source->genlock_shallow_capped, source->genlock_last_known_n < 2, relock,",
        "the latch counts the window's deep ticks (the MAJORITY decides deep, review round 2)",
    );
    // the relock flag is set before the present tail reads it, and never elsewhere.
    assert_eq!(
        src.matches("genlock_shallow_relock = true;").count(),
        1,
        "issue 1367: exactly the sender-restart GAP RESYNC sets the relock flag"
    );
}

#[test]
fn a_pin_change_rearms_and_the_box_marker_log_and_audit_exist_1367() {
    let src = squished();
    let setter = body(
        &src,
        "void obs_source_set_genlock_latency_ms(obs_source_t *source, uint32_t ms)",
    );
    assert_in(
        setter,
        "if (source->genlock_last_known_n < 2) genlock_n1_shallow_rearm(&source->genlock_shallow_floor_max_frames, &source->genlock_shallow_window_ticks, &source->genlock_shallow_over_ticks, &source->genlock_shallow_deep_ticks, &source->genlock_shallow_measuring);",
        "a new pin is a new base: an N==1 source's shallow depth must re-measure",
    );
    assert_in(
        &src,
        "snprintf(path, sizeof(path), \"%s/.camera-box/genlock-min-latency\", home);",
        "the min-latency (imag) box marker must be read",
    );
    assert_in(
        &src,
        "\"genlock-shallow-lock '%s': depth_frames=%llu floor_max_frames=%llu base_frames=%llu \"",
        "each latch must be logged (a capped one as a WARNING)",
    );
    assert_in(
        &src,
        "\"wanted_frames=%llu latency_ms=%u capped=%d (issue 1367)\",",
        "the latch line must name the depth the floor asked for (the capped report)",
    );
    assert_in(
        &src,
        "\"latch_floor_frames=%llu spread_frames=%llu rejects=%u \"",
        "the latch line must name the p90 floor it latched on and the window spread (design 5830750134)",
    );
    assert_in(
        &src,
        "blog(source->genlock_shallow_capped ? LOG_WARNING : LOG_INFO,",
        "a capped latch must be REPORTED loudly (the imag guard)",
    );
    assert_in(
        &src,
        "\"shallow_depth=%llu shallow_capped=%d shallow_latches=%u audio_slew_ms=%lld audio_slews=%u \"",
        "the audit line must carry the shallow depth and the audio slew",
    );
    assert_in(
        &src,
        "\"audio_steps=%u audio_withheld=%llu \"",
        "the audit line must count audio steps (must stay 0) and withheld packets",
    );
    // the imag provisioning writes the very marker libobs reads (one path, two files).
    let setup =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/setup-imag.sh"))
            .expect("read scripts/setup-imag.sh");
    assert!(
        setup.contains(
            "install -o \"$DESKTOP_USER\" -g \"$DESKTOP_USER\" -m 0644 /dev/null \"$USER_HOME/.camera-box/genlock-min-latency\""
        ),
        "issue 1367: setup-imag.sh no longer declares the imag box a genlock MIN-LATENCY box — \
         an imag input could be deepened past base + 1"
    );
    // the new marker must not alias the families the dev1 parsers read.
    for family in [
        "genlock-fifo audit",
        "genlock-relock",
        "genlock-acquire-bracket",
    ] {
        assert!(
            !"genlock-shallow-lock".contains(family) && !family.contains("genlock-shallow-lock"),
            "issue 1367: the latch marker aliases `{family}`"
        );
    }
}
