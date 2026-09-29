//! Issue 1381 (design 5882391108) — the WIRING of the per-source audio SKEW HOLD across a wall step,
//! the timecode ASRC's placement re-seed and the beyond-cap backstop.
//!
//! The decisions are pinned elsewhere: `src/genlock_audio_pairing.rs` `audio_step_hold` (its C port by
//! `tests/genlock_audio_step_hold_parity_1381.rs`), the compensator's re-seed and backstop by
//! `tests/asrc_compensator_parity_1367.rs`, the behaviour by the two-clock bench
//! `src/asrc_timecode_step_bench.rs`. This file pins that `obs-source.c` CALLS them where the design
//! says: the ingest maps the term, the previous term and the timecode ASRC's stamp through the HELD
//! offset, a release that moves the placement by more than one packet places, the backstop places a
//! jump the owed cap cannot hold, the ASRC is not fed while a hold runs, and the render thread leaves
//! the shallow latch and both video-delay tracker calls alone meanwhile. The same needle list is
//! required in both `windows-genlock*.yml` pwsh gates (one list, so the copies cannot drift).
//!
//! Std-only on purpose: it runs under `cargo test` AND standalone
//! (`CARGO_MANIFEST_DIR=<repo> rustc --test --edition 2021 tests/genlock_audio_step_hold_wiring_1381.rs`).
//! Every anchor is whitespace-squished, the same form the pwsh gates use.

use std::fs;
use std::path::PathBuf;

const OBS_SOURCE: &str = "vendor/obs-studio/libobs/obs-source.c";
const OBS_INTERNAL: &str = "vendor/obs-studio/libobs/obs-internal.h";
const ASRC_C: &str = "vendor/obs-studio/libobs/media-io/asrc-compensator.c";
const ASRC_H: &str = "vendor/obs-studio/libobs/media-io/asrc-compensator.h";
const WINDOWS_WORKFLOWS: [&str; 2] = [
    ".github/workflows/windows-genlock.yml",
    ".github/workflows/windows-genlock-fast.yml",
];

/// The `obs-source.c` wiring (squished). The pwsh gate in both Windows workflows requires each one.
const STEP_HOLD_WIRING: [&str; 17] = [
    "#include \"obs-genlock-wall-step.h\"",
    "int64_t genlock_off_ns = genlock_off_live_ns; const int genlock_step_release = genlock_audio_step_hold_source( source, genlock_hold_mode == GENLOCK_AUDIO_HOLD_TIMECODE, genlock_off_live_ns, data->timestamp, genlock_step_packet_ns, os_time, genlock_timeline_reset, &genlock_off_ns);",
    "&source->genlock_audio_step_step_ns, timecode, off_live_ns, raw_ts_ns, packet_ns, now_ns, timeline_reset, GENLOCK_WALL_STEP_MIN_NS, off_out);",
    "genlock_audio_place_term_ns(genlock_hold_mode, genlock_hold_ms, genlock_off_ns, genlock_timing_adjust);",
    "genlock_audio_place_term_ns( prev_genlock_audio_hold_mode, prev_genlock_audio_delay_ms, genlock_off_ns, genlock_timing_adjust);",
    "if (genlock_audio_step_places(source, genlock_step_release, genlock_off_live_ns, genlock_step_packet_ns, genlock_asrc_tc, push_back, sample_rate, in.timestamp, genlock_intended_ns)) push_back = false;",
    "if (genlock_audio_step_release_places( release, genlock_audio_step_residual_ns(source->genlock_audio_step_held_off_ns, off_live_ns), packet_ns)) return true; if (!asrc_tc || source->genlock_audio_step_active || !push_back || !source->audio_ts) return false;",
    "return asrc_compensator_place_beyond_cap( &source->asrc, genlock_audio_asrc_error_ms(genlock_audio_place_error_ns(append_ns, intended_ns), source->genlock_audio_slew_remaining_ns), source->asrc_tc_raw_s * 1000.0);",
    "if (genlock_asrc_tc && genlock_asrc_measured && !source->genlock_audio_step_active) asrc_timecode_ingest(source, genlock_audio_stamp_mono_ns(data->timestamp, genlock_off_ns), genlock_asrc_err_ms, genlock_asrc_appended); else if (source->genlock_audio_step_active) source->asrc_tc_have_prev = false;",
    "genlock_audio_step_log(source, genlock_step_release, genlock_off_live_ns, os_time);",
    "step_ms=%+.3f held_ms=%.1f released=%s residual_ms=%+.1f",
    "if (genlock_audio_step_video_frozen(source)) { if (relock) source->genlock_audio_step_relock_pending = true; return; }",
    "if (n2_on_grid && !genlock_audio_step_video_frozen(source)) genlock_video_delay_track(",
    "const uint64_t genlock_delay_tick_wall = genlock_n1_tick_wall_now(wall_now); if (genlock_n1_tick_is_on_grid(genlock_delay_tick_wall, interval) && !genlock_audio_step_video_frozen(source)) genlock_video_delay_track(&source->genlock_video_delay_smoothed_ns,",
    "if (source->genlock_audio_step_relock_pending) relock = true; source->genlock_audio_step_relock_pending = false;",
    "static bool genlock_audio_step_video_frozen(const obs_source_t *source) { return genlock_audio_step_freezes_video(source->genlock_audio_step_active, source->genlock_audio_step_start_ns, os_gettime_ns()); }",
    "source->context.name ? source->context.name : \"?\", (double)source->genlock_audio_step_step_ns / 1e6,",
];

/// Shapes that must be GONE: the live offset feeding the timecode ASRC's stamp or the placement term
/// directly (the step would reach both before the sender follows).
const STEP_HOLD_ABSENT: [&str; 2] = [
    "genlock_audio_stamp_mono_ns(data->timestamp, genlock_off_live_ns)",
    "genlock_audio_place_term_ns(genlock_hold_mode, genlock_hold_ms, genlock_off_live_ns,",
];

/// The step-hold log marker and every other `genlock-*` OBS-log family a parser keys on.
const MARKER: &str = "genlock-audio-step-hold";
const OTHER_FAMILIES: [&str; 12] = [
    "genlock-fifo audit",
    "genlock-relock",
    "genlock-shallow-lock",
    "genlock-shallow-remeasure",
    "genlock-regrid",
    "genlock-reap",
    "genlock-ndi-output",
    "genlock-ndi-filter",
    "genlock-min-latency",
    "genlock-wall-step",
    "genlock-lock",
    "genlock-lock-json",
];

fn read(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `source_output_audio_data`'s body (squished), up to the next function.
fn audio_ingest(src: &str) -> &str {
    let at = src
        .find("static void source_output_audio_data(")
        .expect("issue 1381: source_output_audio_data is gone");
    let body = &src[at..];
    &body[..body.find("enum convert_type {").unwrap_or(body.len())]
}

fn at(hay: &str, needle: &str) -> usize {
    hay.find(needle)
        .unwrap_or_else(|| panic!("issue 1381: `{needle}` not found"))
}

#[test]
fn the_ingest_holds_the_pre_step_offset_1381() {
    let src = squish(&read(OBS_SOURCE));
    for needle in STEP_HOLD_WIRING {
        assert_eq!(
            src.matches(needle).count(),
            1,
            "issue 1381: obs-source.c must carry `{needle}` exactly once -- the audio skew hold across \
             a wall step is no longer wired as designed"
        );
    }
    for needle in STEP_HOLD_ABSENT {
        assert!(
            !src.contains(needle),
            "issue 1381: obs-source.c maps `{needle}` through the LIVE wall offset again -- a wall \
             step would reach the placement / the timecode ASRC before the sender follows"
        );
    }
    let ingest = audio_ingest(&src);
    // the hold is decided on the live offset, before any term is computed from the held one
    let live = at(ingest, "const int64_t genlock_off_live_ns =");
    let hold = at(ingest, STEP_HOLD_WIRING[1]);
    let term = at(ingest, STEP_HOLD_WIRING[3]);
    let prev = at(ingest, STEP_HOLD_WIRING[4]);
    assert!(live < hold && hold < term && term < prev);
    // the release placement / backstop decide push_back after the sync-offset branch and BEFORE the
    // slew fold, which reads the placement
    let sync = at(ingest, "if (source->last_sync_offset != sync_offset) {");
    let places = at(ingest, STEP_HOLD_WIRING[5]);
    let fold = at(
        ingest,
        "const int64_t genlock_fold_ns = genlock_audio_placed_slew_fold_ns(",
    );
    assert!(
        sync < places && places < fold,
        "issue 1381: the release placement and the backstop must decide the placement after the \
         sync-offset branch and before the slew fold (which reads the placement)"
    );
    // the ASRC feed and the log run after the buffer lock is released
    let unlock = at(
        ingest,
        "pthread_mutex_unlock(&source->audio_buf_mutex); /* camera-box issue 1367",
    );
    let feed = at(ingest, STEP_HOLD_WIRING[8]);
    let log = at(ingest, STEP_HOLD_WIRING[9]);
    let signal = at(ingest, "source_signal_audio_data(source, data,");
    assert!(unlock < feed && feed < log && log < signal);
    // the helpers sit before the ingest; the log helper counts each released hold
    let ingest_at = at(&src, "static void source_output_audio_data(");
    for helper in [
        "static int genlock_audio_step_hold_source(",
        "static bool genlock_audio_step_places(",
        "static void genlock_audio_step_log(",
    ] {
        assert!(
            at(&src, helper) < ingest_at,
            "issue 1381: `{helper}` must precede the ingest"
        );
    }
    assert!(
        src.contains(
            "if (release == GENLOCK_AUDIO_STEP_NONE) return; source->genlock_audio_step_holds++;"
        ),
        "issue 1381: one log line per released hold, counted"
    );
}

#[test]
fn the_render_thread_freezes_the_latch_and_both_trackers_1381() {
    let src = squish(&read(OBS_SOURCE));
    let latch = at(&src, "static void genlock_shallow_latch(");
    let body = &src[latch..];
    let body = &body[..body.find(" static ").unwrap_or(body.len())];
    let seen = at(
        body,
        "source->genlock_shallow_relocks_seen = source->genlock_relocks;",
    );
    let frozen = at(body, STEP_HOLD_WIRING[11]);
    let sticky = at(body, "genlock_n1_shallow_sticky_track(");
    assert!(
        seen < frozen && frozen < sticky,
        "issue 1381: the shallow latch must return while the audio holds across a wall step -- after \
         the backlog relock count is taken (a relock in the hold is absorbed), before the sticky \
         floor and the latch sample the step as frame age"
    );
    assert_eq!(
        src.matches("genlock_video_delay_track(").count(),
        3,
        "issue 1381: the definition and the two present tails only"
    );
    assert_eq!(
        src.matches("!genlock_audio_step_video_frozen(source)) genlock_video_delay_track(")
            .count(),
        2,
        "issue 1381: BOTH present-tail video-delay tracker calls (N==1 conveyor, N>=2 grid release) \
         must be frozen while the audio holds across a wall step"
    );
}

#[test]
fn the_source_carries_the_hold_state_1381() {
    let h = squish(&read(OBS_INTERNAL));
    for field in [
        "bool genlock_audio_step_active;",
        "int64_t genlock_audio_step_prev_off_ns;",
        "uint64_t genlock_audio_step_prev_raw_ns;",
        "uint64_t genlock_audio_step_prev_packet_ns;",
        "int64_t genlock_audio_step_nominal_age_ns;",
        "uint64_t genlock_audio_step_nominal_dev_since_ns;",
        "int64_t genlock_audio_step_held_off_ns;",
        "uint64_t genlock_audio_step_start_ns;",
        "int64_t genlock_audio_step_step_ns;",
        "uint32_t genlock_audio_step_holds;",
    ] {
        assert!(
            h.contains(field),
            "issue 1381: obs-internal.h lost the skew-hold field `{field}`"
        );
    }
}

#[test]
fn the_compensator_reseeds_at_a_placement_and_backstops_the_cap_1381() {
    let c = squish(&read(ASRC_C));
    for needle in [
        "c->level_err_ema_ms = place_err_ms - c->level_target_ms;",
        "bool asrc_compensator_place_beyond_cap(struct asrc_compensator *c, double place_err_ms, double packet_ms) { if (!c->timecode || !c->level_captured) return false;",
    ] {
        assert!(
            c.contains(needle),
            "issue 1381: asrc-compensator.c lost `{needle}` -- a placement would leave the step's \
             smoothed error (and the restore arm) behind, or a jump beyond the owed cap would be \
             partly booked again"
        );
    }
    let h = squish(&read(ASRC_H));
    assert!(
        h.contains("EXPORT bool asrc_compensator_place_beyond_cap(struct asrc_compensator *c, double place_err_ms, double packet_ms);"),
        "issue 1381: the backstop query is no longer exported"
    );
}

#[test]
fn the_log_marker_is_its_own_family_1381() {
    let src = read(OBS_SOURCE);
    assert_eq!(
        src.matches(MARKER).count(),
        1,
        "issue 1381: `{MARKER}` must be printed by exactly one log call"
    );
    for other in OTHER_FAMILIES {
        assert!(
            !MARKER.contains(other) && !other.contains(MARKER),
            "issue 1381: `{MARKER}` and `{other}` must be mutually non-substring (a parser keyed on \
             one would read the other)"
        );
    }
}

#[test]
fn windows_workflows_guard_the_same_wiring_1381() {
    for wf in WINDOWS_WORKFLOWS {
        let text = read(wf);
        for needle in STEP_HOLD_WIRING {
            let want = format!(
                "$src -notmatch [regex]::Escape('{}')",
                needle.replace('\'', "''")
            );
            assert!(
                text.contains(&want),
                "issue 1381: {wf} no longer requires `{needle}` in obs-source.c"
            );
        }
        for needle in STEP_HOLD_ABSENT {
            let want = format!(
                "$src -match [regex]::Escape('{}')",
                needle.replace('\'', "''")
            );
            assert!(
                text.contains(&want),
                "issue 1381: {wf} no longer forbids `{needle}` in obs-source.c"
            );
        }
    }
}

#[test]
fn the_render_thread_freeze_is_bounded_and_replays_a_relock_1381() {
    // review round 1: the render thread's view of the hold is bounded by the same 10 s on its own
    // clock (a source whose audio stops inside a hold never freezes its video side for longer), and
    // a latch relock that lands inside the hold is replayed on the first tick after it, never lost.
    let src = squish(&read(OBS_SOURCE));
    assert!(
        src.contains("static bool genlock_audio_step_video_frozen(const obs_source_t *source) { return genlock_audio_step_freezes_video(source->genlock_audio_step_active, source->genlock_audio_step_start_ns, os_gettime_ns()); }"),
        "issue 1381: the render thread must read the hold through the bounded helper"
    );
    assert_eq!(
        src.matches("genlock_audio_step_video_frozen(source)")
            .count(),
        3,
        "issue 1381: the shallow latch and both video-delay tracker calls read the bounded helper"
    );
    assert!(
        !src.contains("if (source->genlock_audio_step_active) return;")
            && !src.contains("!source->genlock_audio_step_active) genlock_video_delay_track("),
        "issue 1381: an unbounded render-thread read of the hold flag is back"
    );
    let latch = at(&src, "static void genlock_shallow_latch(");
    let body = &src[latch..];
    let body = &body[..body.find(" static ").unwrap_or(body.len())];
    for needle in [
        "if (genlock_audio_step_video_frozen(source)) { if (relock) source->genlock_audio_step_relock_pending = true; return; }",
        "if (source->genlock_audio_step_relock_pending) relock = true; source->genlock_audio_step_relock_pending = false;",
    ] {
        assert!(
            body.contains(needle),
            "issue 1381: the shallow latch lost `{needle}` -- a relock inside the hold is dropped"
        );
    }
    assert!(
        squish(&read(OBS_INTERNAL)).contains("bool genlock_audio_step_relock_pending;"),
        "issue 1381: obs-internal.h lost the pending relock"
    );
}

#[test]
fn the_step_hold_log_line_is_null_safe_1381() {
    // review round 1: obs_source_get_name() can return NULL into %s
    let src = squish(&read(OBS_SOURCE));
    let log = at(&src, "static void genlock_audio_step_log(");
    let body = &src[log..];
    let body = &body[..body.find(" static ").unwrap_or(body.len())];
    assert!(
        body.contains("source->context.name ? source->context.name : \"?\"")
            && !body.contains("obs_source_get_name(source)"),
        "issue 1381: the step-hold log line must not pass a NULL name to %s"
    );
}
