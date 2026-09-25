//! Issue 1367 (Option 3) — a std-only static guard that the genlock AUDIO placement is WIRED, not
//! just defined: the pure helpers (held to `src/genlock_audio_pairing.rs` by
//! `tests/genlock_audio_pairing_parity.rs`) must actually be called at the three seams.
//!
//! 1. The render thread tracks the source's REAL stamp->present delay at the tick's SCHEDULED
//!    instant (`genlock_video_delay_track` fed by `genlock_n1_tick_wall_now(wall_now)` and the head).
//! 2. The audio ingest maps the NDI timecode through the LIVE wall->mono offset read on every packet,
//!    adds the placement term, and on a hold change RE-PLACES at once and shifts the ASRC level
//!    target by the placement delta.
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
fn render_thread_tracks_the_scheduled_tick_video_delay_1367() {
    let src = squished();
    assert_has(
        &src,
        "genlock_video_delay_track(&source->genlock_video_delay_smoothed_ns, &source->genlock_video_delay_applied_ms, &source->genlock_video_delay_settle_ticks,",
        "the render thread no longer tracks the source's stamp->present delay",
    );
    assert_has(
        &src,
        "genlock_video_delay_sample_ns( genlock_n1_tick_wall_now(wall_now), source->async_frames.array[0]->timestamp), interval);",
        "the delay sample must be the head's age at the tick's SCHEDULED instant, not the processing wall",
    );
}

#[test]
fn audio_ingest_places_on_the_live_offset_and_replaces_on_a_change_1367() {
    let src = squished();
    let ingest = audio_ingest(&src);
    for (needle, why) in [
        (
            "const int64_t genlock_off_live_ns = genlock_audio_wall_to_mono_ns(os_gettime_ns(), genlock_wall_now_ns());",
            "the wall->mono offset must be read LIVE on every packet (never latched)",
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
            "if (genlock_hold_mode != prev_genlock_audio_hold_mode || genlock_hold_ms != prev_genlock_audio_delay_ms) {",
            "a hold change is no longer detected",
        ),
        (
            "push_back = false; asrc_compensator_shift_level_target(&source->asrc, genlock_audio_place_shift_ms(genlock_term_ns, genlock_prev_term_ns));",
            "a hold change must RE-PLACE at once and shift the ASRC level target by the placement delta",
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
