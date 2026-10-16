//! Issue 1425 (E3 slices S4 + S4v, design 6225701358) — the WIRING of the ARRIVAL rebase in
//! `source_output_audio_data` and of the media-clock stamps of the vendored `vlc-video` plugin.
//!
//! The decision is pinned by `tests/genlock_audio_asrc_basis_parity_1425.rs` (C == Rust), the behaviour
//! through the shipped ingest by `tests/genlock_audio_arrival_rebase_1425.rs`, the 8 h two-clock model by
//! `src/asrc_arrival_rebase_bench.rs`, the VLC mapping by `tests/vlc_video_media_clock_1425.rs`. This
//! file pins that the code CALLS them where the design says: the stamp-domain rebase right after the
//! direct-stamp detection and before the smoothing, the system-domain rebase under `audio_buf_mutex`
//! before the push-back check, the count recorded before the lock is released and printed after it, and
//! both VLC stamp sites mapped onto the media clock. The same needle lists are required in both
//! `windows-genlock*.yml` pwsh gates (one list each, so the copies cannot drift).
//!
//! Std-only on purpose: it runs under `cargo test` AND standalone
//! (`CARGO_MANIFEST_DIR=<repo> rustc --test --edition 2021 tests/genlock_audio_arrival_rebase_wiring_1425.rs`).
//! Every anchor is whitespace-squished, the same form the pwsh gates use.

use std::fs;
use std::path::PathBuf;

const OBS_SOURCE: &str = "vendor/obs-studio/libobs/obs-source.c";
const OBS_INTERNAL: &str = "vendor/obs-studio/libobs/obs-internal.h";
const VLC_SOURCE: &str = "vendor/obs-studio/plugins/vlc-video/vlc-video-source.c";
const WINDOWS_WORKFLOWS: [&str; 2] = [
    ".github/workflows/windows-genlock.yml",
    ".github/workflows/windows-genlock-fast.yml",
];

/// The `obs-source.c` wiring (squished). The pwsh gate in both Windows workflows requires each one.
const ARRIVAL_REBASE_WIRING: [&str; 9] = [
    "const bool genlock_rebase_monitor_only = source->monitoring_type == OBS_MONITORING_TYPE_MONITOR_ONLY; const bool genlock_rebase_can_resample = source->asrc_enabled && source->resampler; const int genlock_rebase_basis = genlock_audio_asrc_placement(genlock_hold_mode, using_direct_ts, genlock_rebase_monitor_only, genlock_rebase_can_resample);",
    "const bool genlock_arrival_rebase_ts = source->timing_set && source->next_audio_ts_min != 0 && genlock_audio_arrival_rebase(genlock_rebase_basis, genlock_rebase_monitor_only, genlock_rebase_can_resample, source->asrc.level_captured, uint64_diff(source->next_audio_ts_min, data->timestamp)); if (genlock_arrival_rebase_ts) { genlock_arrival_drift_ns = (int64_t)(data->timestamp - source->next_audio_ts_min); source->next_audio_ts_min = data->timestamp; }",
    "const bool genlock_arrival_rebase_sys = source->next_audio_sys_ts_min != 0 && genlock_audio_arrival_rebase(genlock_rebase_basis, genlock_rebase_monitor_only, genlock_rebase_can_resample, source->asrc.level_captured, uint64_diff(source->next_audio_sys_ts_min, in.timestamp));",
    "if ((genlock_relabel || genlock_relabel_pending || genlock_arrival_rebase_sys) && source->next_audio_sys_ts_min) source->next_audio_sys_ts_min = in.timestamp; if (source->next_audio_sys_ts_min == in.timestamp) { push_back = true;",
    "if ((genlock_arrival_rebase_ts || genlock_arrival_rebase_sys) && push_back && source->audio_ts) { source->genlock_arrival_rebases++; source->genlock_arrival_rebase_due = true; source->genlock_arrival_rebase_sys = !genlock_arrival_rebase_ts; source->genlock_arrival_rebase_drift_ns = genlock_arrival_rebase_ts ? genlock_arrival_drift_ns : genlock_arrival_sys_drift_ns; }",
    "genlock_audio_arrival_rebase_log(source, os_time); source_signal_audio_data(source, data, source_muted(source, os_time));",
    "\"genlock-audio-arrival-rebase '%s': domain=%s drift_ms=%+.1f rebases=%u (issue 1425)\"",
    "_Static_assert(GENLOCK_AUDIO_TS_SMOOTHING_NS == TS_SMOOTHING_THRESHOLD,",
    "#define GENLOCK_AUDIO_ARRIVAL_REBASE_LOG_NS (60ULL * 1000000000ULL)",
];

/// The `vlc-video-source.c` wiring (squished): both stamp sites on the OBS media clock.
const VLC_WIRING: [&str; 4] = [
    "#ifdef _WIN32 #include <util/windows/qpc-timestamp.h> #endif",
    "return os_foreign_clock_ns_to_gettime_ns(stamp_ns, (uint64_t)libvlc_clock_() * 1000ULL);",
    "c->frame.timestamp = vlcs_media_clock_ns(libvlc_clock_());",
    "c->audio.timestamp = vlcs_media_clock_ns(pts);",
];

/// Shapes that must be GONE from `vlc-video-source.c`: the stamps relative to plugin load.
const VLC_ABSENT: [&str; 1] = ["- time_start;"];

/// The log marker and every other OBS-log family a parser keys on.
const MARKER: &str = "genlock-audio-arrival-rebase";
const OTHER_FAMILIES: [&str; 16] = [
    "genlock-audio-step-hold",
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
    "asrc:",
    "audio-telemetry",
    "audio-stall",
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
        .expect("issue 1425: source_output_audio_data is gone");
    let body = &src[at..];
    &body[..body.find("enum convert_type {").unwrap_or(body.len())]
}

fn at(hay: &str, needle: &str) -> usize {
    hay.find(needle)
        .unwrap_or_else(|| panic!("issue 1425: `{needle}` not found"))
}

#[test]
fn the_ingest_rebases_a_locked_arrival_source_in_both_domains_1425() {
    let src = squish(&read(OBS_SOURCE));
    for needle in ARRIVAL_REBASE_WIRING {
        assert_eq!(
            src.matches(needle).count(),
            1,
            "issue 1425: obs-source.c must carry `{needle}` exactly once -- a locked arrival source \
             would be placed 70 ms off by its own stamps again"
        );
    }
    let ingest = audio_ingest(&src);
    let direct = at(
        ingest,
        "if (uint64_diff(in.timestamp, os_time) < MAX_TS_VAR) {",
    );
    let basis = at(ingest, ARRIVAL_REBASE_WIRING[0]);
    let rebase_ts = at(ingest, ARRIVAL_REBASE_WIRING[1]);
    let smooth = at(
        ingest,
        "diff = uint64_diff(source->next_audio_ts_min, in.timestamp);",
    );
    let lock = at(ingest, "pthread_mutex_lock(&source->audio_buf_mutex);");
    let rebase_sys = at(ingest, ARRIVAL_REBASE_WIRING[2]);
    let sys = at(ingest, ARRIVAL_REBASE_WIRING[3]);
    let record = at(ingest, ARRIVAL_REBASE_WIRING[4]);
    let sync = at(ingest, "sync_offset = source->sync_offset;");
    let unlock = at(ingest, "pthread_mutex_unlock(&source->audio_buf_mutex);");
    let log = at(ingest, ARRIVAL_REBASE_WIRING[5]);
    assert!(
        direct < basis
            && basis < rebase_ts
            && rebase_ts < smooth
            && smooth < lock
            && lock < rebase_sys
            && rebase_sys < sys
            && sys < record
            && record < sync
            && sync < unlock
            && unlock < log,
        "issue 1425: the stamp-domain rebase must follow the direct-stamp detection and precede the \
         smoothing, the system-domain rebase must run inside the buffer lock before its check, the \
         count is recorded under the lock and printed after it"
    );
    // the basis the rebase predicts is decided from the same inputs as the ASRC basis
    assert!(
        ingest.contains("genlock_audio_asrc_placement( genlock_hold_mode, genlock_direct_ts, source->monitoring_type == OBS_MONITORING_TYPE_MONITOR_ONLY, source->asrc_enabled && source->resampler);"),
        "issue 1425: the ASRC basis inputs changed: re-check the rebase's predicted basis"
    );
    let internal = squish(&read(OBS_INTERNAL));
    for field in [
        "uint32_t genlock_arrival_rebases;",
        "bool genlock_arrival_rebase_due;",
        "bool genlock_arrival_rebase_sys;",
        "int64_t genlock_arrival_rebase_drift_ns;",
        "uint64_t genlock_arrival_rebase_logged_ns;",
    ] {
        assert!(
            internal.contains(field),
            "issue 1425: obs-internal.h lost `{field}`"
        );
    }
}

#[test]
fn the_rebase_log_is_its_own_family_1425() {
    let src = read(OBS_SOURCE);
    assert_eq!(
        src.matches(MARKER).count(),
        1,
        "issue 1425: `{MARKER}` must be printed by exactly one log call"
    );
    for other in OTHER_FAMILIES {
        assert!(
            !MARKER.contains(other) && !other.contains(MARKER),
            "issue 1425: `{MARKER}` and `{other}` must be mutually non-substring (a parser keyed on \
             one would read the other)"
        );
    }
}

#[test]
fn both_vlc_stamp_sites_are_on_the_media_clock_1425() {
    let vlc = squish(&read(VLC_SOURCE));
    for needle in VLC_WIRING {
        assert_eq!(
            vlc.matches(needle).count(),
            1,
            "issue 1425: vlc-video-source.c must carry `{needle}` exactly once"
        );
    }
    for needle in VLC_ABSENT {
        assert!(
            !vlc.contains(needle),
            "issue 1425: vlc-video-source.c stamps relative to plugin load again (`{needle}`): the \
             source drifts against the disciplined mixer"
        );
    }
}

#[test]
fn windows_workflows_guard_the_same_wiring_1425() {
    for wf in WINDOWS_WORKFLOWS {
        let text = read(wf);
        for (var, needle) in ARRIVAL_REBASE_WIRING
            .iter()
            .map(|n| ("$s4src", n))
            .chain(VLC_WIRING.iter().map(|n| ("$s4vlc", n)))
        {
            let want = format!(
                "{var} -notmatch [regex]::Escape('{}')",
                needle.replace('\'', "''")
            );
            assert!(
                text.contains(&want),
                "issue 1425: {wf} no longer requires `{needle}`"
            );
        }
        for needle in VLC_ABSENT {
            let want = format!(
                "$s4vlc -match [regex]::Escape('{}')",
                needle.replace('\'', "''")
            );
            assert!(
                text.contains(&want),
                "issue 1425: {wf} no longer forbids `{needle}` in vlc-video-source.c"
            );
        }
    }
}
