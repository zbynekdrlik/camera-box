//! Issue 1425 (E2 slice 2, design 6216736061 + ROZHODNUTÉ 6217557472) — the audio-buffering FREEZE is
//! wired.
//!
//! Stock libobs (and the issue-1367 floor above it) grew the WHOLE mix's buffering whenever a mixed
//! source fell behind the window; a growth while an encoder runs makes that encoder's audio
//! permanently later. On stream-snv the E1 events grew it +85 / +128 / +192 ms for good. Since slice 2
//! the mix freezes while an output with an audio encoder runs (and only then), and a mixed source that
//! falls behind it is re-anchored (`MIXED_LATE`). Since slice 3 (ROZHODNUTÉ 6221252305) the freeze
//! waits out the launch settle window (30 s of mix time after the mix's first audio tick, a
//! never-reset file static), and a growth inside it while an encoder runs is logged once.
//!
//! The decisions are pinned elsewhere: the freeze + action by
//! `tests/genlock_audio_buffering_parity_1367.rs` (C vs the Rust authority), the behaviour by the
//! issue-1381 lift (`tests/genlock_audio_mix_guard_1381.rs`, the E1 replays). This file pins the
//! WIRING: ONE needle list, required here and by the pwsh step in both `windows-genlock*.yml`
//! workflows, plus the order of the three per-tick steps and the encoder count's two sites.
//!
//! Std-only: `CARGO_MANIFEST_DIR=<wt> rustc --edition 2021 --test
//! tests/genlock_audio_buffering_freeze_wiring_1425.rs`.

use std::fs;
use std::path::PathBuf;

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn read(rel: &str) -> String {
    fs::read_to_string(repo(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

const OBS_AUDIO: &str = "vendor/obs-studio/libobs/obs-audio.c";
const OBS_OUTPUT: &str = "vendor/obs-studio/libobs/obs-output.c";
const OBS_INTERNAL: &str = "vendor/obs-studio/libobs/obs-internal.h";
const BUFFERING_H: &str = "vendor/obs-studio/libobs/obs-genlock-audio-buffering.h";
const MIX_GUARD_H: &str = "vendor/obs-studio/libobs/obs-genlock-mix-guard.h";

/// `(file, squished needle)`, each required exactly once.
const FREEZE_WIRING: [(&str, &str); 24] = [
    (OBS_AUDIO, "audio->genlock_buffering_frozen = genlock_buffering_frozen_now(start_ts_in, obs_output_genlock_audio_encoder_outputs() > 0);"),
    (OBS_AUDIO, "static uint64_t genlock_launch_first_tick_ns = 0;"),
    (OBS_AUDIO, "if (!genlock_launch_first_tick_ns) genlock_launch_first_tick_ns = tick_start_ns;"),
    (OBS_AUDIO, "blog(LOG_WARNING, \"genlock audio buffering: grew %d ms while an encoder ran inside the launch settle window\", (int)ms);"),
    (OBS_AUDIO, "genlock_launch_growth_log(ms);"),
    (OBS_AUDIO, "const int genlock_guard = genlock_mix_guard_reason( genlock_mix_source_is_member(source), source->genlock_mix_entered, source->info.audio_render != NULL, source->audio_ts, ts.start, audio->genlock_buffering_frozen);"),
    (OBS_AUDIO, "const int genlock_buffering = genlock_audio_buffering_action(audio->total_buffering_ticks, audio->floor_buffering_ticks, audio->max_buffering_ticks, min_ts < ts.start, audio->genlock_buffering_frozen);"),
    (OBS_AUDIO, "if (reason == GENLOCK_MIX_GUARD_MIXED_LATE) { source->genlock_mixed_late_events++; source->genlock_mixed_late_dropped_ns += dropped_ns; audio->genlock_mixed_late_events++; }"),
    (OBS_AUDIO, "snprintf(why, sizeof(why), \"fell behind the mix (frozen at %d ms)\", total_ms);"),
    (OBS_AUDIO, "\"audio-telemetry #800 mix: frozen=%d mixed_late=%\" PRIu64,"),
    (OBS_AUDIO, "\" buffered_ms=%d pending=%d timing_adjust_ms=%\" PRId64 \" mixed_late=%\" PRIu64,"),
    (OBS_OUTPUT, "static volatile long genlock_audio_encoder_outputs = 0;"),
    (OBS_OUTPUT, "long obs_output_genlock_audio_encoder_outputs(void) { return os_atomic_load_long(&genlock_audio_encoder_outputs); }"),
    (OBS_OUTPUT, "if (has_audio) os_atomic_inc_long(&genlock_audio_encoder_outputs);"),
    (OBS_OUTPUT, "if (has_audio) os_atomic_dec_long(&genlock_audio_encoder_outputs);"),
    (OBS_INTERNAL, "bool genlock_buffering_frozen; uint64_t genlock_mixed_late_events;"),
    (OBS_INTERNAL, "uint64_t genlock_mixed_late_events; uint64_t genlock_mixed_late_dropped_ns;"),
    (OBS_INTERNAL, "extern long obs_output_genlock_audio_encoder_outputs(void);"),
    (BUFFERING_H, "static inline bool genlock_audio_buffering_frozen(bool encoder_active, bool settled) { return encoder_active && settled; }"),
    (BUFFERING_H, "#define GENLOCK_AUDIO_BUFFERING_LAUNCH_SETTLE_NS 30000000000ULL"),
    (BUFFERING_H, "return now_ns >= first_tick_ns && now_ns - first_tick_ns >= GENLOCK_AUDIO_BUFFERING_LAUNCH_SETTLE_NS;"),
    (BUFFERING_H, "return source_behind && !frozen ? GENLOCK_AUDIO_BUFFERING_ACTION_DYNAMIC : GENLOCK_AUDIO_BUFFERING_ACTION_NONE;"),
    (MIX_GUARD_H, "#define GENLOCK_MIX_GUARD_MIXED_LATE 3"),
    (MIX_GUARD_H, "return frozen ? GENLOCK_MIX_GUARD_MIXED_LATE : GENLOCK_MIX_GUARD_NONE;"),
];

/// `(file, squished needle)`, each must be GONE: the pre-freeze action and reason calls, and the
/// settle arm ROZHODNUTÉ 6217557472 dropped (nothing but a running audio encoder freezes the mix).
const FREEZE_ABSENT: [(&str, &str); 6] = [
    (OBS_AUDIO, "audio->max_buffering_ticks, min_ts < ts.start);"),
    (
        OBS_AUDIO,
        "source->info.audio_render != NULL, source->audio_ts, ts.start);",
    ),
    (OBS_AUDIO, "genlock_settle_tick"),
    (OBS_INTERNAL, "genlock_settle_tick"),
    (BUFFERING_H, "GENLOCK_AUDIO_BUFFERING_SETTLE_NS"),
    (BUFFERING_H, "genlock_audio_buffering_ns_since"),
];

const WINDOWS_WORKFLOWS: [&str; 2] = [
    ".github/workflows/windows-genlock.yml",
    ".github/workflows/windows-genlock-fast.yml",
];

fn pwsh_var(file: &str) -> &'static str {
    match file {
        OBS_AUDIO => "$audio1425",
        OBS_OUTPUT => "$output1425",
        OBS_INTERNAL => "$internal1425",
        BUFFERING_H => "$buf1425",
        MIX_GUARD_H => "$guard1425",
        other => panic!("no pwsh variable for {other}"),
    }
}

#[test]
fn the_mixer_and_the_outputs_carry_the_freeze_1425() {
    for (file, needle) in FREEZE_WIRING {
        assert_eq!(
            squish(&read(file)).matches(needle).count(),
            1,
            "issue 1425: {file} must carry `{needle}` exactly once -- the audio-buffering freeze is \
             not wired (a mixed source that falls behind would grow the whole mix again)"
        );
    }
    for (file, needle) in FREEZE_ABSENT {
        assert!(
            !squish(&read(file)).contains(needle),
            "issue 1425: {file} still carries the pre-freeze `{needle}`"
        );
    }
}

/// The squished text of `src` from `start` up to the next top-level `static ` (a function body).
fn function_body(src: &str, start: &str) -> String {
    let s = squish(src);
    assert_eq!(
        s.matches(start).count(),
        1,
        "issue 1425: `{start}` must occur once"
    );
    let a = s.find(start).unwrap();
    let rest = &s[a + start.len()..];
    let b = rest.find(" static ").unwrap_or(rest.len());
    rest[..b].to_string()
}

#[test]
fn the_freeze_is_decided_after_the_mark_before_the_render_loop_1425() {
    let s = squish(&read(OBS_AUDIO));
    let pos = |needle: &str| {
        s.find(needle)
            .unwrap_or_else(|| panic!("issue 1425: `{needle}` not in {OBS_AUDIO}"))
    };
    let mark = pos("genlock_mix_mark_members(audio);");
    let decide = pos("audio->genlock_buffering_frozen = genlock_buffering_frozen_now(");
    let render = pos("/* render audio data */");
    let reason = pos("const int genlock_guard = genlock_mix_guard_reason(");
    let action = pos("const int genlock_buffering = genlock_audio_buffering_action(");
    let mix = pos("/* mix audio */");
    assert!(
        mark < decide && decide < render && render < reason && reason < action && action < mix,
        "issue 1425: the per-tick order must be the mark -> the freeze (read once) -> the render loop \
         (MIXED_LATE) -> the tick decision -> the mix"
    );
    // ROZHODNUTÉ 6217557472: no settle -- the mark records no join time.
    assert!(
        !function_body(
            &read(OBS_AUDIO),
            "static void genlock_mix_mark_members(struct obs_core_audio *audio)"
        )
        .contains("audio->genlock_"),
        "issue 1425: genlock_mix_mark_members must not feed the freeze (only an audio encoder does)"
    );
}

#[test]
fn the_encoder_count_rises_at_the_capture_start_and_falls_at_its_stop_1425() {
    let src = read(OBS_OUTPUT);
    let hook = function_body(
        &src,
        "static void hook_data_capture(struct obs_output *output)",
    );
    let end = function_body(&src, "static void *end_data_capture_thread(void *data)");
    assert!(
        hook.contains("if (has_audio) start_audio_encoders(output, encoded_callback);")
            && hook.contains("if (has_audio) os_atomic_inc_long(&genlock_audio_encoder_outputs);")
            && !hook.contains("os_atomic_dec_long(&genlock_audio_encoder_outputs)"),
        "issue 1425: hook_data_capture must count an encoded output's audio encoders up, once"
    );
    assert!(
        end.contains("if (has_audio) stop_audio_encoders(output, encoded_callback);")
            && end.contains("if (has_audio) os_atomic_dec_long(&genlock_audio_encoder_outputs);")
            && !end.contains("os_atomic_inc_long(&genlock_audio_encoder_outputs)"),
        "issue 1425: end_data_capture_thread must count them down, once"
    );
    // Both sit in the ENCODED branch: the raw-output branch (start_raw_audio / stop_raw_audio) comes
    // after the `} else {`.
    for (body, needle) in [
        (
            &hook,
            "if (has_audio) os_atomic_inc_long(&genlock_audio_encoder_outputs);",
        ),
        (
            &end,
            "if (has_audio) os_atomic_dec_long(&genlock_audio_encoder_outputs);",
        ),
    ] {
        let at = body.find(needle).unwrap();
        let raw = body.find("} else {").expect("the raw-output branch");
        assert!(at < raw, "issue 1425: `{needle}` left the encoded branch");
    }
}

#[test]
fn windows_workflows_guard_the_same_freeze_wiring_1425() {
    for wf in WINDOWS_WORKFLOWS {
        let text = read(wf);
        for (file, needle) in FREEZE_WIRING {
            let line = format!(
                "{} -notmatch [regex]::Escape('{}')",
                pwsh_var(file),
                needle.replace('\'', "''")
            );
            assert!(
                text.contains(&line),
                "issue 1425: {wf} no longer guards `{needle}` ({file})"
            );
        }
        for (file, needle) in FREEZE_ABSENT {
            let line = format!(
                "{} -match [regex]::Escape('{}')",
                pwsh_var(file),
                needle.replace('\'', "''")
            );
            assert!(
                text.contains(&line),
                "issue 1425: {wf} no longer refuses the pre-freeze `{needle}` ({file})"
            );
        }
        for file in [
            OBS_AUDIO,
            OBS_OUTPUT,
            OBS_INTERNAL,
            BUFFERING_H,
            MIX_GUARD_H,
        ] {
            assert!(
                text.contains(&format!(
                    "{} = (Get-Content \"{file}\" -Raw) -replace '\\s+', ' '",
                    pwsh_var(file)
                )),
                "issue 1425: {wf} does not load {file} into {}",
                pwsh_var(file)
            );
        }
    }
}
