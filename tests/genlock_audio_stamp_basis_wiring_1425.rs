//! Issue 1425 (E2 slice 1, designs 6205974120 + 6206822792) — the WIRING of the STAMP basis of the
//! per-source ASRC.
//!
//! The decisions are pinned elsewhere: `src/genlock_audio_pairing.rs` `audio_asrc_placement` /
//! `audio_place_measured` / `audio_asrc_rate_point` (their C port by
//! `tests/genlock_audio_asrc_basis_parity_1425.rs`), the behaviour by the two-clock bench
//! `src/asrc_stamp_bench.rs` (+ its `asrc_stamp_bench_stall.rs` rows), the compensator's confirmed
//! booking by `tests/asrc_compensator_stamp_parity_1425.rs`. This file pins that `obs-source.c`
//! CALLS them where the designs say:
//!
//! - round 2: whether THIS packet's stamps are direct is a local of the audio path
//!   (`genlock_direct_ts` = OBS's direct verdict), cleared only where the audio path itself makes
//!   `timing_adjust` non-zero (the decoupled smoothing branch, the second `reset_audio_timing`);
//!   `source->timing_adjust`, which the render thread writes for async video, is never read for it;
//! - the ingest decides the basis from the hold mode and that flag, and hands the BASIS to the
//!   compensator (`asrc_compensator_set_basis`; its placement-judged flag is derived from it);
//! - the placement error is measured for a stamp-judged source too, while only a genlock hold writes
//!   the pairing audit facet (`genlock_audio_place_err_ns` / `_seeded`);
//! - round 2: the timecode ingest builds each rate point through `genlock_audio_asrc_rate_point`
//!   (the stamp basis's waiting point, which a packet with no stamp advance still adds to);
//! - the `asrc:` line names the basis after its byte-identical issue-1367 suffix.
//!
//! The same needle lists are required in both `windows-genlock*.yml` pwsh gates (one list, so the
//! copies cannot drift). Std-only on purpose: it runs under `cargo test` AND standalone
//! (`CARGO_MANIFEST_DIR=<repo> rustc --test --edition 2021 tests/genlock_audio_stamp_basis_wiring_1425.rs`).
//! Every anchor is whitespace-squished, the same form the pwsh gates use.

use std::fs;
use std::path::PathBuf;

const OBS_SOURCE: &str = "vendor/obs-studio/libobs/obs-source.c";
const OBS_INTERNAL: &str = "vendor/obs-studio/libobs/obs-internal.h";
const WINDOWS_WORKFLOWS: [&str; 2] = [
    ".github/workflows/windows-genlock.yml",
    ".github/workflows/windows-genlock-fast.yml",
];

/// `source_output_audio_data` (squished): the basis, its direct-stamp flag and the measure gate.
const INGEST_WIRING: [&str; 8] = [
    "bool genlock_direct_ts = using_direct_ts;",
    "if (source->async_unbuffered && source->async_decoupled) { source->timing_adjust = os_time - in.timestamp; genlock_direct_ts = false; }",
    "reset_audio_timing(source, data->timestamp, os_time); genlock_direct_ts = false;",
    "const int genlock_asrc_basis = genlock_audio_asrc_placement( genlock_hold_mode, genlock_direct_ts, source->monitoring_type == OBS_MONITORING_TYPE_MONITOR_ONLY, source->asrc_enabled && source->resampler);",
    "const bool genlock_asrc_tc = genlock_asrc_basis != GENLOCK_AUDIO_ASRC_BASIS_ARRIVAL; source->genlock_asrc_basis = genlock_asrc_basis; if (source->asrc.timecode != genlock_asrc_tc) source->asrc_tc_have_prev = false;",
    "asrc_compensator_set_basis(&source->asrc, genlock_asrc_basis);",
    "if (genlock_audio_place_measured(genlock_hold_mode, genlock_asrc_basis)) { const uint64_t genlock_actual_ns = genlock_audio_actual_place_ns(",
    "const int64_t genlock_place_err_ns = genlock_audio_place_error_ns(genlock_actual_ns, genlock_intended_ns); if (genlock_audio_mode_active(genlock_hold_mode)) { source->genlock_audio_place_err_ns = genlock_audio_place_error_smooth_ns( source->genlock_audio_place_err_ns, genlock_place_err_ns, source->genlock_audio_place_err_seeded); source->genlock_audio_place_err_seeded = true; } genlock_asrc_err_ms =",
];

/// `asrc_timecode_ingest` (squished): every rate point through the rate-point decision.
const RATE_POINT_WIRING: [&str; 3] = [
    "const struct genlock_audio_rate_point point = genlock_audio_asrc_rate_point( source->genlock_asrc_basis, contiguous, master_s, source->asrc_tc_prev_raw_s, &source->asrc_tc_pend_raw_s, &source->asrc_tc_pend_master_s, &source->asrc_tc_pend_due);",
    "if (point.due) asrc_compensator_compensate(&source->asrc, point.raw_s, point.master_s, err_ms, &applied_ppm);",
    "if (!point.keep_prev_stamp) source->asrc_tc_prev_stamp_ns = stamp_mono_ns;",
];

/// `asrc_process_audio` (squished): the `asrc:` line's `basis=`.
const ASRC_LINE_WIRING: [&str; 2] = [
    "\" timecode=%d place_jumps=%u last_jump_ms=%.1f (issue 1367)\" \" basis=%s (issue 1425)\",",
    "source->asrc.place_jump_count, source->asrc.last_place_jump_ms, genlock_audio_asrc_basis_token(source->genlock_asrc_basis));",
];

/// Shapes that must be GONE: the timecode-only predicate (every directly stamped source would keep
/// the arrival servo), the genlock-only measure gate (a stamp-judged source would feed its servo
/// nothing), the basis read from the render-thread-written `timing_adjust` (round 2), the
/// compensator told a bool instead of the basis (round 2: the stamp basis would book at once), and
/// the rate point that dropped a packet with no stamp advance on every basis (round 2).
const STAMP_BASIS_ABSENT: [&str; 5] = [
    "genlock_audio_asrc_timecode(",
    "if (genlock_audio_mode_active(genlock_hold_mode)) { const uint64_t genlock_actual_ns",
    "using_direct_ts && source->timing_adjust == 0",
    "asrc_compensator_set_timecode(&source->asrc,",
    "if (master_s > 0.0) asrc_compensator_compensate(",
];

fn read(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A function's body (squished), from its definition up to `end`.
fn body<'a>(src: &'a str, sig: &str, end: &str) -> &'a str {
    let at = src
        .find(sig)
        .unwrap_or_else(|| panic!("issue 1425: obs-source.c no longer defines `{sig}`"));
    let body = &src[at..];
    let rest = &body[1..];
    &body[..1 + rest.find(end).unwrap_or(rest.len())]
}

fn audio_ingest(src: &str) -> &str {
    body(
        src,
        "static void source_output_audio_data(obs_source_t *source, const struct audio_data *data) {",
        "enum convert_type {",
    )
}

fn asrc_process(src: &str) -> &str {
    body(
        src,
        "static inline void asrc_process_audio(obs_source_t *source, uint32_t frames, uint32_t samples_per_sec) {",
        " static ",
    )
}

fn timecode_ingest(src: &str) -> &str {
    body(
        src,
        "static void asrc_timecode_ingest(obs_source_t *source, uint64_t stamp_mono_ns, double err_ms, bool appended) {",
        " static ",
    )
}

fn at(hay: &str, needle: &str) -> usize {
    hay.find(needle)
        .unwrap_or_else(|| panic!("issue 1425: obs-source.c no longer contains `{needle}`"))
}

fn exactly_once(hay: &str, needles: &[&str], what: &str) {
    for needle in needles {
        assert_eq!(
            hay.matches(needle).count(),
            1,
            "issue 1425: {what} must contain `{needle}` exactly once"
        );
    }
}

#[test]
fn the_ingest_judges_a_directly_stamped_source_by_its_stamp_1425() {
    let src = squish(&read(OBS_SOURCE));
    let ingest = audio_ingest(&src);
    exactly_once(ingest, &INGEST_WIRING, "source_output_audio_data");
    for needle in STAMP_BASIS_ABSENT {
        assert!(
            !src.contains(needle),
            "issue 1425: obs-source.c must no longer contain `{needle}`"
        );
    }
    // the flag starts from OBS's direct verdict for THIS packet, both clears follow, and the basis
    // reads the flag after them ...
    let direct = at(ingest, "using_direct_ts = true;");
    let flag = at(ingest, INGEST_WIRING[0]);
    let smoothing_clear = at(ingest, INGEST_WIRING[1]);
    let reset_clear = at(ingest, INGEST_WIRING[2]);
    let basis = at(ingest, INGEST_WIRING[3]);
    assert!(
        direct < flag
            && flag < smoothing_clear
            && smoothing_clear < reset_clear
            && reset_clear < basis,
        "issue 1425: the order must be direct verdict -> flag -> its two clears -> basis"
    );
    // ... the compensator learns the basis before the placement shifts of this packet (a no-op in
    // its placement-judged mode), the measure gate reads it, and the servo is fed after the unlock.
    let mode = at(ingest, INGEST_WIRING[5]);
    let first_shift = at(
        ingest,
        "push_back = false; asrc_compensator_shift_level_target(",
    );
    let measured = at(ingest, INGEST_WIRING[6]);
    let facet = at(ingest, INGEST_WIRING[7]);
    let fed = at(
        ingest,
        "if (genlock_asrc_tc && genlock_asrc_measured && !source->genlock_audio_step_active) asrc_timecode_ingest(",
    );
    assert!(
        basis < at(ingest, INGEST_WIRING[4])
            && at(ingest, INGEST_WIRING[4]) < mode
            && mode < first_shift
            && first_shift < measured
            && measured < facet
            && facet < fed,
        "issue 1425: the order must be basis -> compensator basis -> shifts -> measure gate -> \
         audit facet -> servo feed"
    );
}

#[test]
fn the_timecode_ingest_builds_each_rate_point_through_the_decision_1425() {
    let src = squish(&read(OBS_SOURCE));
    let tc = timecode_ingest(&src);
    exactly_once(tc, &RATE_POINT_WIRING, "asrc_timecode_ingest");
    let point = at(tc, RATE_POINT_WIRING[0]);
    let observe = at(
        tc,
        "asrc_compensator_observe_placement(&source->asrc, err_ms,",
    );
    let fed = at(tc, RATE_POINT_WIRING[1]);
    let stamp = at(tc, RATE_POINT_WIRING[2]);
    assert!(
        point < observe && observe < fed && fed < stamp,
        "issue 1425: the rate point is decided first, the placement observed before the servo is fed, \
         and the previous stamp moves only after"
    );
    let internal = squish(&read(OBS_INTERNAL));
    assert!(
        internal.contains(
            "double asrc_tc_pend_raw_s; double asrc_tc_pend_master_s; bool asrc_tc_pend_due;"
        ),
        "issue 1425: struct obs_source must carry the stamp basis's waiting rate point"
    );
}

#[test]
fn the_asrc_line_names_the_basis_1425() {
    let src = squish(&read(OBS_SOURCE));
    exactly_once(asrc_process(&src), &ASRC_LINE_WIRING, "asrc_process_audio");
    let internal = squish(&read(OBS_INTERNAL));
    assert!(
        internal.contains("int genlock_asrc_basis;"),
        "issue 1425: struct obs_source must carry the ingest's basis for the asrc: line"
    );
}

#[test]
fn windows_workflows_guard_the_same_wiring_1425() {
    for wf in WINDOWS_WORKFLOWS {
        let text = read(wf);
        for needle in INGEST_WIRING
            .iter()
            .chain(&RATE_POINT_WIRING)
            .chain(&ASRC_LINE_WIRING)
        {
            let want = format!(
                "$src -notmatch [regex]::Escape('{}')",
                needle.replace('\'', "''")
            );
            assert!(
                text.contains(&want),
                "issue 1425: {wf} no longer requires `{needle}` in obs-source.c"
            );
        }
        for needle in STAMP_BASIS_ABSENT {
            let want = format!(
                "$src -match [regex]::Escape('{}')",
                needle.replace('\'', "''")
            );
            assert!(
                text.contains(&want),
                "issue 1425: {wf} no longer forbids `{needle}` in obs-source.c"
            );
        }
    }
}
