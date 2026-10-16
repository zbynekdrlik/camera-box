//! Issue 1425 (E2 slice 1, design 6205974120) — the WIRING of the STAMP basis of the per-source ASRC.
//!
//! The decision is pinned elsewhere: `src/genlock_audio_pairing.rs` `audio_asrc_placement` /
//! `audio_place_measured` (their C port by `tests/genlock_audio_asrc_basis_parity_1425.rs`), the behaviour by
//! the two-clock bench `src/asrc_stamp_bench.rs`, the compensator (unchanged) by
//! `tests/asrc_compensator_parity_1367.rs`. This file pins that `obs-source.c` CALLS them where the
//! design says:
//!
//! - the ingest decides the basis from the hold mode AND whether this packet's stamps are direct
//!   (`using_direct_ts && source->timing_adjust == 0`), after OBS's direct-stamp detection;
//! - the compensator's timecode ("placement-judged") flag is set for every basis but arrival;
//! - the placement error is measured for a stamp-judged source too, while only a genlock hold writes
//!   the pairing audit facet (`genlock_audio_place_err_ns` / `_seeded`);
//! - the `asrc:` line names the basis after its byte-identical issue-1367 suffix.
//!
//! The same needle list is required in both `windows-genlock*.yml` pwsh gates (one list, so the copies
//! cannot drift). Std-only on purpose: it runs under `cargo test` AND standalone
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

/// The `obs-source.c` wiring (squished). The pwsh gate in both Windows workflows requires each one.
const STAMP_BASIS_WIRING: [&str; 6] = [
    "const int genlock_asrc_basis = genlock_audio_asrc_placement( genlock_hold_mode, using_direct_ts && source->timing_adjust == 0, source->monitoring_type == OBS_MONITORING_TYPE_MONITOR_ONLY, source->asrc_enabled && source->resampler);",
    "const bool genlock_asrc_tc = genlock_asrc_basis != GENLOCK_AUDIO_ASRC_BASIS_ARRIVAL; source->genlock_asrc_basis = genlock_asrc_basis; if (source->asrc.timecode != genlock_asrc_tc) source->asrc_tc_have_prev = false; asrc_compensator_set_timecode(&source->asrc, genlock_asrc_tc);",
    "if (genlock_audio_place_measured(genlock_hold_mode, genlock_asrc_basis)) { const uint64_t genlock_actual_ns = genlock_audio_actual_place_ns(",
    "const int64_t genlock_place_err_ns = genlock_audio_place_error_ns(genlock_actual_ns, genlock_intended_ns); if (genlock_audio_mode_active(genlock_hold_mode)) { source->genlock_audio_place_err_ns = genlock_audio_place_error_smooth_ns( source->genlock_audio_place_err_ns, genlock_place_err_ns, source->genlock_audio_place_err_seeded); source->genlock_audio_place_err_seeded = true; } genlock_asrc_err_ms =",
    "\" timecode=%d place_jumps=%u last_jump_ms=%.1f (issue 1367)\" \" basis=%s (issue 1425)\",",
    "source->asrc.place_jump_count, source->asrc.last_place_jump_ms, genlock_audio_asrc_basis_token(source->genlock_asrc_basis));",
];

/// Shapes that must be GONE: the timecode-only predicate (every directly stamped source would keep the
/// arrival servo), and the genlock-only measure gate (a stamp-judged source would feed its servo
/// nothing).
const STAMP_BASIS_ABSENT: [&str; 2] = [
    "genlock_audio_asrc_timecode(",
    "if (genlock_audio_mode_active(genlock_hold_mode)) { const uint64_t genlock_actual_ns",
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
        .find("static void source_output_audio_data(obs_source_t *source, const struct audio_data *data) {")
        .expect("issue 1425: source_output_audio_data is gone");
    let body = &src[at..];
    &body[..body.find("enum convert_type {").unwrap_or(body.len())]
}

/// `asrc_process_audio`'s body (squished), up to the next function.
fn asrc_process(src: &str) -> &str {
    let at = src
        .find("static inline void asrc_process_audio(obs_source_t *source, uint32_t frames, uint32_t samples_per_sec) {")
        .expect("issue 1425: asrc_process_audio is gone");
    let body = &src[at..];
    let rest = &body[1..];
    &body[..1 + rest.find(" static ").unwrap_or(rest.len())]
}

fn at(hay: &str, needle: &str) -> usize {
    hay.find(needle)
        .unwrap_or_else(|| panic!("issue 1425: obs-source.c no longer contains `{needle}`"))
}

#[test]
fn the_ingest_judges_a_directly_stamped_source_by_its_stamp_1425() {
    let src = squish(&read(OBS_SOURCE));
    let ingest = audio_ingest(&src);
    for needle in &STAMP_BASIS_WIRING[..4] {
        assert_eq!(
            ingest.matches(needle).count(),
            1,
            "issue 1425: source_output_audio_data must contain `{needle}` exactly once"
        );
    }
    for needle in STAMP_BASIS_ABSENT {
        assert!(
            !src.contains(needle),
            "issue 1425: obs-source.c must no longer contain `{needle}`"
        );
    }
    // the basis is decided from THIS packet's direct-stamp verdict, after OBS detected it ...
    let direct = at(ingest, "using_direct_ts = true;");
    let basis = at(ingest, STAMP_BASIS_WIRING[0]);
    assert!(
        direct < basis,
        "issue 1425: the basis must read the direct-stamp verdict of this packet"
    );
    // ... the compensator learns it before the placement shifts of this packet (a no-op in its
    // placement-judged mode), the measure gate reads it, and the servo is fed after the unlock.
    let mode = at(ingest, STAMP_BASIS_WIRING[1]);
    let first_shift = at(
        ingest,
        "push_back = false; asrc_compensator_shift_level_target(",
    );
    let measured = at(ingest, STAMP_BASIS_WIRING[2]);
    let facet = at(ingest, STAMP_BASIS_WIRING[3]);
    let fed = at(ingest, "if (genlock_asrc_tc && genlock_asrc_measured && !source->genlock_audio_step_active) asrc_timecode_ingest(");
    assert!(
        basis < mode && mode < first_shift && first_shift < measured && measured < facet && facet < fed,
        "issue 1425: the order must be basis -> compensator mode -> shifts -> measure gate -> audit \
         facet -> servo feed"
    );
}

#[test]
fn the_asrc_line_names_the_basis_1425() {
    let src = squish(&read(OBS_SOURCE));
    let asrc = asrc_process(&src);
    for needle in &STAMP_BASIS_WIRING[4..] {
        assert_eq!(
            asrc.matches(needle).count(),
            1,
            "issue 1425: asrc_process_audio must contain `{needle}` exactly once"
        );
    }
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
        for needle in STAMP_BASIS_WIRING {
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
