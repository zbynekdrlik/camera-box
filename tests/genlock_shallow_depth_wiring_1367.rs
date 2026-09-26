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
//! 5. the min-latency (imag) box marker, the latch log line and the audit tokens exist;
//! 6. a GAP RESYNC of a governed shallow source holds until the post-gap head is D frames old
//!    (`genlock_should_hold_n1_gap`, design 5833339163);
//! 7. the latch histogram reads the newest frame's RECEIVE-time arrival lag plus the arrival-jitter
//!    budget (`genlock_rx_arrival_lag_ns`, ROZHODNUTÉ 5842640404), while the rise / fell watch keeps
//!    the raw tick floor.
//!
//! Std-only on purpose: it runs under `cargo test` AND standalone
//! (`CARGO_MANIFEST_DIR=<repo> rustc --test --edition 2021 tests/genlock_shallow_depth_wiring_1367.rs`).
//! Every anchor is whitespace-squished, the same form the pwsh gates in both `windows-genlock*.yml`
//! use.

use std::fs;
use std::path::PathBuf;

const OBS_SOURCE: &str = "vendor/obs-studio/libobs/obs-source.c";
const OBS_INTERNAL: &str = "vendor/obs-studio/libobs/obs-internal.h";

fn squished_file(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    let raw = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn squished() -> String {
    squished_file(OBS_SOURCE)
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
        "source->genlock_last_known_n < 2, relock, genlock_n1_tick_is_on_grid(tick_wall, interval), genlock_n1_depth_frames(tick_wall, newest_stamp, interval), genlock_n1_shallow_latch_floor_frames(source->genlock_rx_arrival_lag_ns, interval), base_frames, genlock_n1_is_deep_source(",
        "the latch samples the newest frame's raw tick floor (the rise / fell watch) and its budgeted \
         receive-lag latch floor (the histogram) on an on-grid N==1 tick (an N>=2 tick clears it)",
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

/// design 5833339163 (the song change): a GAP RESYNC of a governed shallow source HOLDS while the
/// post-gap head is younger than the latched D, so a skipped stamp never puts the conveyor on air
/// under D (live resolume 25.9.2026 15:29: `video_delay_ms=67 audio_delay_ms=100`).
#[test]
fn a_gap_resync_holds_a_shallow_source_on_its_latched_depth_1367() {
    let src = squished();
    let wrapper = body(
        &src,
        "static bool genlock_should_hold_n1_gap(const obs_source_t *source, uint32_t reserve_ms, uint64_t interval, uint64_t wall_now)",
    );
    assert_in(
        wrapper,
        "if (interval == 0 || source->async_frames.num == 0 || source->genlock_last_known_n >= 2) return false;",
        "the GAP hold is N==1 only (an N>=2 source has no latched D)",
    );
    assert_in(
        wrapper,
        "const uint64_t next_stamp = source->async_frames.num > 1 ? source->async_frames.array[1]->timestamp : 0;",
        "the frame queued behind the head tells a late-labelled (duplicated) head from a real skip",
    );
    assert_in(
        wrapper,
        "const uint64_t tick_wall = genlock_n1_tick_wall_now(wall_now);",
        "the GAP hold reads the head's age at the tick's SCHEDULED instant",
    );
    assert_in(
        wrapper,
        "return genlock_n1_tick_is_on_grid(tick_wall, interval) && genlock_n1_shallow_gap_hold_due(tick_wall, head_stamp, next_stamp, source->genlock_locked_next_boundary_ns, arrival_floor, reserve_ms, interval, source->genlock_shallow_target_frames);",
        "the GAP hold defers off the grid and delegates to the pure decision with the latched D",
    );
    let tick = body(
        &src,
        "static bool genlock_release_tick(obs_source_t *source, uint64_t wall_now, uint64_t present_ts, size_t due, uint64_t interval, uint32_t reserve_ms, uint64_t now_ns)",
    );
    let branch = tick
        .find("} else if (present_ts >= source->async_frames.array[0]->timestamp) {")
        .expect("issue 1367: the GAP RESYNC branch is gone");
    let call = "if (genlock_should_hold_n1_gap(source, reserve_ms, interval, wall_now)) { source->genlock_n1_grows++; genlock_audit_log(source, now_ns); return false; }";
    let hold = tick
        .find(call)
        .expect("issue 1367: the GAP RESYNC no longer asks the shallow GAP hold first");
    let sticky = tick[branch..]
        .find("source->genlock_last_known_n = 0;")
        .map(|i| branch + i)
        .expect("issue 1367: the GAP RESYNC STICKY-N clear is gone");
    assert!(
        branch < hold && hold < sticky,
        "issue 1367: the GAP hold must decide at the HEAD of the GAP RESYNC branch, before it \
         clears STICKY-N and presents"
    );
    assert_eq!(
        src.matches("genlock_should_hold_n1_gap(").count(),
        2,
        "issue 1367: the GAP hold has exactly one definition and one call site"
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

/// ROZHODNUTÉ 5842640404: since issue 1355 the tick-read age is whole frames, so the latch budgets
/// the RECEIVE-time arrival lag instead. It is measured at the producer push site (under
/// `async_mutex`, after the stamp tracker), so it always describes the newest queued frame; it is
/// reset with the stamp timeline at the explicit flush.
#[test]
fn the_latch_reads_the_receive_time_arrival_lag_1367() {
    let internal = squished_file(OBS_INTERNAL);
    assert_in(
        &internal,
        "uint64_t genlock_rx_arrival_lag_ns;",
        "the per-source receive-time arrival lag field",
    );
    let src = squished();
    let track = src
        .find("&source->genlock_stamp_gaps, output->timestamp);")
        .expect("issue 1367: the #1355 arrival-side stamp tracking call is gone");
    let set = "const uint64_t rx_wall = genlock_wall_now_ns(); source->genlock_rx_arrival_lag_ns = rx_wall > output->timestamp ? rx_wall - output->timestamp : 0;";
    assert_eq!(
        src.matches(set).count(),
        1,
        "issue 1367: the receive-time arrival lag must be recorded exactly once, saturating at 0"
    );
    let at = src.find(set).expect("counted above");
    let unlock = src[track..]
        .find("pthread_mutex_unlock(&source->async_mutex);")
        .map(|i| track + i)
        .expect("the push path's async_mutex unlock is gone");
    assert!(
        track < at && at < unlock,
        "issue 1367: the arrival lag must be recorded at the producer push site, after the stamp \
         tracker and under async_mutex (the same lock the release tick reads it under)"
    );
    assert_in(
        &src,
        "source->genlock_rx_last_ts = 0; source->genlock_rx_min_delta_ns = 0; source->genlock_rx_arrival_lag_ns = 0;",
        "the explicit flush resets the arrival lag with the stamp timeline",
    );
    assert_eq!(
        src.matches("genlock_rx_arrival_lag_ns = ").count(),
        2,
        "issue 1367: the arrival lag has exactly two writers (the push site and the flush)"
    );
    assert_in(
        &src,
        "static inline uint64_t genlock_n1_shallow_latch_floor_frames(uint64_t arrival_lag_ns, uint64_t interval_ns)",
        "the pure budgeted latch floor (parity-lifted with the N==1 block)",
    );
}
