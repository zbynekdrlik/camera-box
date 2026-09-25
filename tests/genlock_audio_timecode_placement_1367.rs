//! Issue 1367 (Option 3) — a std-only static guard that the genlock AUDIO placement is WIRED, not
//! just defined: the pure helpers (held to `src/genlock_audio_pairing.rs` by
//! `tests/genlock_audio_pairing_parity.rs`) must actually be called at the three seams.
//!
//! 1. The render thread tracks the source's REAL stamp->present delay at the tick's SCHEDULED
//!    instant (`genlock_video_delay_track` fed by `genlock_n1_tick_wall_now(wall_now)` and the head).
//! 2. The audio ingest maps the NDI timecode through the LIVE wall->mono offset read on every packet
//!    and adds the placement term. Since ROZHODNUTÉ 5827497952 a hold change while audio PLAYS is
//!    SLEWED through the ASRC resampler (`asrc_process_audio` stretches at the slew rate, the ingest
//!    books each step out of the smoothing timeline and into the level setpoint); a packet with no
//!    video delay known yet is WITHHELD; only a first placement / a discontinuity PLACES (and a
//!    source with no resampler keeps the legacy step).
//! 3. The pairing offset is measured against the measured delay, and the audit line carries the
//!    basis (`audio_hold=` / `video_delay_ms=` / `audio_health=`).
//!
//! The #1303 fixed-pin arrival hold (`in.timestamp += (int64_t)genlock_audio_present_delay_ns(
//! source->genlock_latency_ms);`) must be GONE — it is the defect (audio ~94 ms ahead of a shallow
//! cg feed's video on win-resolume).
//!
//! Std-only on purpose: it runs under `cargo test` AND standalone
//! (`CARGO_MANIFEST_DIR=<repo> rustc --test --edition 2021 tests/genlock_audio_timecode_placement_1367.rs`).
//! Every anchor is whitespace-squished (the same `split_whitespace` form the pwsh gates in both
//! `windows-genlock*.yml` use), so a reflow of the C does not break it.

use std::fs;
use std::path::PathBuf;

const OBS_SOURCE: &str = "vendor/obs-studio/libobs/obs-source.c";
const TRACK: &str = "const uint64_t genlock_delay_tick_wall = genlock_n1_tick_wall_now(wall_now); if (genlock_n1_tick_is_on_grid(genlock_delay_tick_wall, interval)) genlock_video_delay_track(&source->genlock_video_delay_smoothed_ns,";
const SAMPLE: &str =
    "genlock_video_delay_sample_ns(genlock_delay_tick_wall, next_frame->timestamp), interval);";

fn squished() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(OBS_SOURCE);
    let raw = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn assert_has(src: &str, needle: &str, why: &str) {
    assert!(
        src.contains(needle),
        "issue 1367: {OBS_SOURCE} no longer contains `{needle}` — {why}"
    );
}

/// The body of `source_output_audio_data` (the definition, not a forward declaration).
fn audio_ingest(src: &str) -> &str {
    let sig = "static void source_output_audio_data(obs_source_t *source, const struct audio_data *data) {";
    let start = src.find(sig).unwrap_or_else(|| {
        panic!("issue 1367: {OBS_SOURCE} no longer defines source_output_audio_data")
    });
    let rest = &src[start + sig.len()..];
    let end = rest.find(" static ").unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn render_thread_tracks_the_presented_frame_at_the_scheduled_tick_1367() {
    let src = squished();
    // The sample is taken at the PRESENT TAIL of genlock_release_tick, on the frame this tick
    // actually presents (after every erase / drain / converge shed): on an N >= 2 source the queue
    // head is (N - 1) source intervals older than the presented frame.
    let tick = src
        .find("static bool genlock_release_tick(")
        .expect("issue 1367: genlock_release_tick is gone");
    let body = &src[tick..];
    let body = &body[..body.find(" static ").unwrap_or(body.len())];
    let presented = body
        .find("struct obs_source_frame *next_frame = source->async_frames.array[0];")
        .expect("issue 1367: genlock_release_tick no longer picks the presented frame");
    let track = body.find(TRACK).unwrap_or_else(|| {
        panic!(
            "issue 1367: genlock_release_tick no longer tracks the presented frame's delay on an \
             on-grid scheduled tick (anchor `{TRACK}`)"
        )
    });
    assert!(
        track > presented,
        "issue 1367: the delay must be sampled AFTER the presented frame is chosen"
    );
    assert!(
        body.contains(SAMPLE),
        "issue 1367: the sample must be the PRESENTED frame's age at the scheduled tick (`{SAMPLE}`)"
    );
    // the head-skew site (the processing wall, array[0]) must not feed the tracker.
    assert_eq!(
        src.matches("genlock_video_delay_track(").count(),
        2,
        "issue 1367: genlock_video_delay_track must have exactly its definition and ONE call site"
    );
}

#[test]
fn audio_ingest_places_on_the_live_offset_and_replaces_on_a_change_1367() {
    let src = squished();
    let ingest = audio_ingest(&src);
    for (needle, why) in [
        (
            "const int64_t genlock_off_live_ns = genlock_audio_needs_live_offset(genlock_hold_mode, prev_genlock_audio_hold_mode) ? genlock_audio_wall_to_mono_ns(os_gettime_ns(), genlock_wall_now_ns()) : 0;",
            "the wall->mono offset must be read LIVE on every packet that needs it (never latched)",
        ),
        (
            "genlock_is_wallclock_ts(data->timestamp),",
            "the timecode mapping must be gated on a wall-clock audio timestamp",
        ),
        (
            "genlock_video_delay_ms = source->genlock_video_delay_applied_ms;",
            "the ingest must follow the render thread's applied video delay",
        ),
        (
            "genlock_audio_place_term_ns(genlock_hold_mode, genlock_hold_ms, genlock_off_live_ns, genlock_timing_adjust);",
            "the placement term is no longer computed",
        ),
        ("in.timestamp += (uint64_t)genlock_term_ns;", "the placement term is no longer applied"),
        (
            "genlock_audio_withhold_expired(source->genlock_audio_first_packet_ns, os_time));",
            "the hold must withhold a timecode source until its video delay is known (bounded)",
        ),
        (
            "const int genlock_action = genlock_audio_hold_action(",
            "the per-packet action (withhold / place / continue / slew / step) is no longer decided",
        ),
        (
            "if (genlock_action == GENLOCK_AUDIO_ACT_SLEW) {",
            "a hold change while playing must SLEW (ROZHODNUTÉ 5827497952), never step",
        ),
        (
            "source->genlock_audio_slew_remaining_ns += (int64_t)((uint64_t)genlock_term_ns - (uint64_t)genlock_prev_term_ns);",
            "the slew must owe exactly the term delta of this packet",
        ),
        (
            "push_back = false; asrc_compensator_shift_level_target( &source->asrc, (double)genlock_audio_level_shift_ns(",
            "a (re)placement must place at once and shift the level target by the true buffer jump",
        ),
        (
            "if (genlock_action != GENLOCK_AUDIO_ACT_WITHHOLD && source->monitoring_type != OBS_MONITORING_TYPE_MONITOR_ONLY) {",
            "a withheld packet must never enter the mix",
        ),
        (
            "source->next_audio_ts_min -= (uint64_t)genlock_slew_step_ns;",
            "a slew step must be kept out of the smoothing timeline (else the 70 ms guard snaps it back)",
        ),
        (
            "asrc_compensator_shift_level_target(&source->asrc, (double)genlock_slew_step_ns / 1e6);",
            "a slew step must move the ASRC level setpoint with the buffer",
        ),
    ] {
        assert!(
            ingest.contains(needle),
            "issue 1367: source_output_audio_data no longer contains `{needle}` — {why}"
        );
    }
    assert!(
        !src.contains(
            "in.timestamp += (int64_t)genlock_audio_present_delay_ns(source->genlock_latency_ms);"
        ),
        "issue 1367: the #1303 fixed-pin ARRIVAL hold is back — genlock audio would again lead a \
         shallow feed's video by its FIFO depth"
    );
    // the legacy "re-place on every hold change" is gone: that step IS the audible 33 ms dropout.
    assert!(
        !src.contains(
            "push_back = false; asrc_compensator_shift_level_target(&source->asrc, genlock_audio_place_shift_ms(genlock_term_ns, genlock_prev_term_ns));"
        ),
        "issue 1367: the unconditional re-placement on a hold change is back — every relock would \
         drop out the audio again"
    );
    // forward declarations: the helpers are defined with the genlock FIFO far below the ingest.
    assert_has(
        &src,
        "static inline bool genlock_is_wallclock_ts(uint64_t ts_ns); static inline uint64_t genlock_wall_now_ns(void); static void source_output_audio_data(",
        "the ingest's forward declarations are gone (MSVC C4013 / gcc implicit-declaration)",
    );
}

#[test]
fn pairing_offset_and_audit_basis_use_the_measured_delay_1367() {
    let src = squished();
    assert_has(
        &src,
        "genlock_audio_video_delay_ref_ns(source->genlock_video_delay_smoothed_ns, effective_latency_ms));",
        "audio_pairing_offset_ms must be measured against the MEASURED video delay, not the pin",
    );
    assert_has(
        &src,
        "\"audio_hold=%s video_delay_ms=%lld audio_health=%d \"",
        "the audit line no longer carries the audio pairing basis",
    );
    assert_has(
        &src,
        "genlock_audio_hold_token(source->genlock_audio_hold_mode),",
        "the audit line no longer prints the hold mode",
    );
    assert_has(
        &src,
        "const int audio_health = have_vi ? genlock_audio_decide_health(",
        "the audit line no longer carries the half-frame pairing verdict",
    );
}

/// The body of `asrc_process_audio`.
fn asrc_process(src: &str) -> &str {
    let sig = "static inline void asrc_process_audio(obs_source_t *source, uint32_t frames, uint32_t samples_per_sec) {";
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("issue 1367: {OBS_SOURCE} no longer defines asrc_process_audio"));
    let rest = &src[start + sig.len()..];
    let end = rest.find(" static ").unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn the_asrc_resampler_carries_the_slew_1367() {
    let src = squished();
    let asrc = asrc_process(&src);
    for (needle, why) in [
        (
            "const int64_t genlock_slew_step = genlock_audio_slew_step_ns(source->genlock_audio_slew_remaining_ns, genlock_slew_dt_ns);",
            "each callback must consume a slew step at the slew rate",
        ),
        (
            "source->genlock_audio_slew_step_ns += genlock_slew_step;",
            "the consumed step must be handed to the ingest for booking",
        ),
        (
            "audio_resampler_set_compensation_ppm(source->resampler, genlock_slew_ppm - applied_ppm, ASRC_COMPENSATION_DISTANCE_MS);",
            "the slew must ride on the resampler on top of the servo's own (negated) ppm",
        ),
        (
            "audio_resampler_set_compensation_ppm(source->resampler, -applied_ppm, ASRC_COMPENSATION_DISTANCE_MS);",
            "with no slew the #1325 negated servo compensation must stay byte-identical",
        ),
    ] {
        assert!(
            asrc.contains(needle),
            "issue 1367: asrc_process_audio no longer contains `{needle}` — {why}"
        );
    }
}
