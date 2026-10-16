//! Issue 1381 (design 5862336131) — only sources that are MIXED can move the mix's audio
//! buffering.
//!
//! Stock libobs let every audio source move the mix window: `find_min_ts` / `mark_invalid_sources`
//! walk `data->first_audio_source`, and `audio_callback` renders every audio source, mixed or not.
//! 27.9.2026 on resolume a hidden "NDI test" whose timestamps ran later and later grew the whole
//! cg mix +85/+42/+106/+128/+106/+490 ms to the 960 ms maximum — one hole in every output per step.
//!
//! Std-only (no `camera_box::`), so it runs standalone with plain rustc:
//! `CARGO_MANIFEST_DIR=<wt> rustc --edition 2021 --test tests/genlock_audio_mix_guard_1381.rs`.
//!
//! - The wiring anchors (the membership mark, the two min_ts filters, the render-loop re-anchor, the
//!   loud `buffering-guard:` line). The same list is required by the pwsh guard in both
//!   `windows-genlock*.yml` workflows.
//! - A C LIFT of the shipped `audio_callback` path, verbatim: the tail of the render-order build (the
//!   membership mark + the catch-all loop), the render loop + `calc_min_ts`, the 1367 tick decision,
//!   the mix + discard blocks, and every function they call (`push_audio_tree`,
//!   `convert_time_to_frames`, `ignore_audio` .. `calc_min_ts` and the guard; the pure helpers come
//!   from `obs-genlock-mix-guard.h` by `#include`), substituted into
//!   `tests/c/genlock_audio_mix_guard_1381_harness.c`. That stub libobs feeds sources the way
//!   `source_output_audio_place` does (bytes only) and drives scenarios: hidden late sources, an
//!   ingest-thread race, an audio reset, a mixed late source, cuts into the mix (a source, a program
//!   scene), a launch in the frontend's order, plus direct probes of the guard's edges and its
//!   `ignore_audio` mirror. The printed trace is the truth table. `cc` is required — it FAILS LOUDLY
//!   rather than skips.
//! - Issue 1425 (E2 slice 2, design 6216736061 + ROZHODNUTÉ 6217557472): the buffering FREEZE through
//!   the same lift. The three E1 replays (a 128 ms and a 192 ms ASIO outage on `mbc`, a media source
//!   late after a 432 ms mixer stall, all while stream-snv streamed) grew the mix +128 / +192 / +85 ms
//!   for good; frozen by the running audio encoder, the mix stays where the launch drew it (issue 1425
//!   slice 3: no floor) and the late member is re-anchored once (`MIXED_LATE`). With no audio encoder
//!   nothing freezes: the outage replay and a clip started late in place grow the mix as stock libobs
//!   does. The encoder switching on and off, the frozen ingest race, an output started inside the
//!   launch settle window (ROZHODNUTÉ 6221252305: it grows with the startup race and plays, logged
//!   once) and one started after it (frozen), and the reason's truth table with the freeze input.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

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
const OBS_INTERNAL: &str = "vendor/obs-studio/libobs/obs-internal.h";
const AUDIO_IO_H: &str = "vendor/obs-studio/libobs/media-io/audio-io.h";
const MIX_GUARD_H: &str = "vendor/obs-studio/libobs/obs-genlock-mix-guard.h";

/// `(file, squished needle)` — the wiring both this test and the pwsh guard in both
/// `windows-genlock*.yml` workflows require, each exactly once. ONE list, so the two copies cannot
/// drift apart.
const WIRING: [(&str, &str); 8] = [
    (OBS_AUDIO, "#include \"obs-genlock-mix-guard.h\""),
    (OBS_AUDIO, "genlock_mix_mark_members(audio);"),
    (
        OBS_AUDIO,
        "if (genlock_mix_source_is_member(source) && !source->audio_pending && source->audio_ts && source->audio_ts < *min_ts) {",
    ),
    (
        OBS_AUDIO,
        "if (genlock_mix_source_is_member(source)) recalculate |= audio_buffer_insufficient(source, sample_rate, min_ts);",
    ),
    (
        OBS_AUDIO,
        "const bool genlock_rerender = genlock_mix_guard_reanchor(audio, source, channels, sample_rate, ts.start, genlock_guard);",
    ),
    (
        OBS_AUDIO,
        "\"buffering-guard: '%s' %s: its audio ran %.1f ms behind the mix window; re-anchored (dropped %.1f ms%s) \"",
    ),
    (OBS_INTERNAL, "bool genlock_mix_entered;"),
    (
        MIX_GUARD_H,
        "#define GENLOCK_MIX_GUARD_LOG_INTERVAL_NS (60ULL * 1000000000ULL)",
    ),
];

const WINDOWS_WORKFLOWS: [&str; 2] = [
    ".github/workflows/windows-genlock.yml",
    ".github/workflows/windows-genlock-fast.yml",
];

fn pwsh_var(file: &str) -> &'static str {
    match file {
        OBS_AUDIO => "$audio1381",
        OBS_INTERNAL => "$internal1381",
        MIX_GUARD_H => "$hdr1381",
        other => panic!("no pwsh variable for {other}"),
    }
}

#[test]
fn the_mixer_carries_the_mix_buffering_guard_1381() {
    for (file, needle) in WIRING {
        let s = squish(&read(file));
        assert_eq!(
            s.matches(needle).count(),
            1,
            "issue 1381: {file} must carry `{needle}` exactly once -- the mix buffering guard is not \
             wired (a hidden source would grow the whole mix's buffering again)"
        );
    }
    let internal = squish(&read(OBS_INTERNAL));
    for field in [
        "uint64_t genlock_mix_tick;",
        "uint64_t genlock_mix_guard_events;",
        "uint64_t genlock_mix_guard_dropped_ns;",
        "uint64_t genlock_mix_guard_logged_events;",
        "uint64_t genlock_mix_guard_last_log_ns;",
    ] {
        assert!(
            internal.contains(field),
            "issue 1381: obs-internal.h lost the guard field `{field}`"
        );
    }
    assert_eq!(
        internal.matches("uint64_t genlock_mix_tick;").count(),
        1,
        "issue 1381: only struct obs_source carries the mix tick -- the mixer's own counter is a \
         file-scope static in obs-audio.c, so an audio reset (obs_free_audio zeroes struct \
         obs_core_audio) can never bring a source's stale tick back as a member"
    );
    // The #800 telemetry test pins its 60 s rate limit by the literal `60000000000ULL`; a second
    // occurrence in obs-audio.c would make that anchor blind.
    assert_eq!(
        read(OBS_AUDIO).matches("60000000000ULL").count(),
        1,
        "issue 1381: obs-audio.c must carry the literal 60000000000ULL once (the #800 rate limit) -- \
         spell any other 60 s bound as a product"
    );
}

#[test]
fn the_guard_line_never_reads_as_a_786_buffering_draw_1381() {
    // The #786 launch gates (obs-guarded-launch.ps1, launch-obs-genlock.sh, rig-health-audit.py)
    // parse `total audio buffering is now (\d+) milliseconds`: a guard line must never match it,
    // or a hidden source's re-anchor would read as a bad launch draw.
    let joined = squish(&read(OBS_AUDIO)).replace("\" \"", "");
    let start = joined
        .find("\"buffering-guard: ")
        .expect("issue 1381: the buffering-guard line is gone");
    let end = start
        + joined[start..]
            .find("\",")
            .expect("issue 1381: the buffering-guard format does not end");
    let fmt = &joined[start..end];
    assert!(
        !fmt.contains("is now"),
        "issue 1381: the buffering-guard line must not carry the #786 `is now` text: {fmt}"
    );
    assert!(
        joined.contains("blog(LOG_WARNING, \"buffering-guard: "),
        "issue 1381: a re-anchor must be LOUD (LOG_WARNING)"
    );
    // A marker no other audio line contains (the audio-mixer pager and the #800 / 1367 parsers stay
    // independent).
    for other in [
        "audio-stall #1367:",
        "audio-telemetry #800",
        "genlock audio buffering",
    ] {
        assert!(!"buffering-guard: ".contains(other) && !other.contains("buffering-guard"));
    }
}

#[test]
fn windows_workflows_guard_the_same_wiring_1381() {
    for wf in WINDOWS_WORKFLOWS {
        let text = read(wf);
        for (file, needle) in WIRING {
            let line = format!(
                "{} -notmatch [regex]::Escape('{}')",
                pwsh_var(file),
                needle.replace('\'', "''")
            );
            assert!(
                text.contains(&line),
                "issue 1381: {wf} no longer guards `{needle}` ({file}) -- its pwsh copy drifted \
                 from WIRING"
            );
        }
        for (file, var) in [
            (OBS_AUDIO, "$audio1381"),
            (OBS_INTERNAL, "$internal1381"),
            (MIX_GUARD_H, "$hdr1381"),
        ] {
            assert!(
                text.contains(&format!(
                    "{var} = (Get-Content \"{file}\" -Raw) -replace '\\s+', ' '"
                )),
                "issue 1381: {wf} does not load {file} into {var}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The C lift: the shipped bytes, driven.

/// The text of `src` from `start` up to (not including) `end`.
fn slice_between(src: &str, start: &str, end: &str) -> String {
    assert_eq!(
        src.matches(start).count(),
        1,
        "issue 1381: lift anchor `{start}` must occur exactly once"
    );
    let a = src
        .find(start)
        .unwrap_or_else(|| panic!("issue 1381: lift anchor `{start}` not found"));
    let b = src[a..]
        .find(end)
        .unwrap_or_else(|| panic!("issue 1381: lift end `{end}` not found after `{start}`"));
    src[a..a + b].to_string()
}

/// One `static inline` function of `src`, from its signature through its closing `\n}\n`.
fn lift_fn(src: &str, signature: &str) -> String {
    assert_eq!(
        src.matches(signature).count(),
        1,
        "issue 1381: `{signature}` must occur exactly once"
    );
    let a = src
        .find(signature)
        .unwrap_or_else(|| panic!("issue 1381: `{signature}` not found"));
    let b = src[a..]
        .find("\n}\n")
        .unwrap_or_else(|| panic!("issue 1381: `{signature}` does not end"));
    src[a..a + b + 3].to_string()
}

/// The functions `audio_callback` calls, verbatim: `push_audio_tree`, `convert_time_to_frames`,
/// and `ignore_audio` through `calc_min_ts` (the guard's helpers included).
fn lift_functions() -> String {
    let s = read(OBS_AUDIO);
    let io = read(AUDIO_IO_H);
    [
        lift_fn(&io, "static inline uint64_t audio_frames_to_ns("),
        lift_fn(&io, "static inline uint64_t ns_to_audio_frames("),
        slice_between(
            &s,
            "static void push_audio_tree(",
            "static inline bool is_individual_audio_source(",
        ),
        slice_between(
            &s,
            "static inline size_t convert_time_to_frames(",
            "static inline void mix_audio(",
        ),
        slice_between(
            &s,
            "static bool ignore_audio(",
            "static inline void release_audio_sources(",
        ),
    ]
    .join("\n")
}

/// The blocks of `audio_callback`, verbatim, in order. The render loop and the minimum-timestamp
/// step are apart, so the harness can land an ingest-thread packet between them.
fn lift_callback_blocks() -> [String; 5] {
    let s = read(OBS_AUDIO);
    let sep = "\t/* ------------------------------------------------ */\n";
    [
        // the render-order build after the output mixes: the mark + the catch-all loop
        slice_between(
            &s,
            "\tpthread_mutex_unlock(&obs->video.mixes_mutex);\n",
            &format!("{sep}\t/* render audio data */"),
        ),
        // the render loop
        slice_between(
            &s,
            "\t/* render audio data */\n",
            &format!("{sep}\t/* get minimum audio timestamp */"),
        ),
        // the minimum timestamp
        slice_between(
            &s,
            "\t/* get minimum audio timestamp */\n",
            "\t/* camera-box #800: audio-side telemetry",
        ),
        // the 1367 tick decision, up to the mix
        slice_between(
            &s,
            "\tconst int genlock_buffering = genlock_audio_buffering_action(",
            &format!("{sep}\t/* mix audio */"),
        ),
        // mix + discard
        slice_between(
            &s,
            "\t/* mix audio */\n",
            &format!("{sep}\t/* release audio sources */"),
        ),
    ]
}

/// The C harness template, with the lifted code substituted in.
const HARNESS: &str = "tests/c/genlock_audio_mix_guard_1381_harness.c";

fn c_harness() -> String {
    let blocks = lift_callback_blocks();
    let template = read(HARNESS);
    for marker in [
        "@LIFTED_FUNCTIONS@",
        "@BLOCK0@",
        "@BLOCK1@",
        "@BLOCK2@",
        "@BLOCK3@",
        "@BLOCK4@",
    ] {
        assert_eq!(
            template.matches(marker).count(),
            1,
            "issue 1381: {HARNESS} must carry {marker} exactly once"
        );
    }
    template
        .replace("@LIFTED_FUNCTIONS@", &lift_functions())
        .replace("@BLOCK0@", &blocks[0])
        .replace("@BLOCK1@", &blocks[1])
        .replace("@BLOCK2@", &blocks[2])
        .replace("@BLOCK3@", &blocks[3])
        .replace("@BLOCK4@", &blocks[4])
}

fn scratch_dir() -> PathBuf {
    let base = option_env!("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join(format!(
        "genlock_audio_mix_guard_1381_{}",
        std::process::id()
    ))
}

/// The lift's printed trace, compiled and run ONCE per test binary (parallel tests compiling into
/// one scratch dir race each other).
fn c_trace() -> &'static [String] {
    static TRACE: OnceLock<Vec<String>> = OnceLock::new();
    TRACE.get_or_init(build_and_run_lift)
}

fn build_and_run_lift() -> Vec<String> {
    let dir = scratch_dir();
    fs::create_dir_all(&dir).expect("create the lift scratch dir");
    let harness = dir.join("harness.c");
    let bin = dir.join("harness.bin");
    fs::write(&harness, c_harness()).expect("write the lift harness");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=gnu11",
            "-Wall",
            "-Wextra",
            "-Wformat=2",
            "-Werror",
            "-O1",
        ])
        .arg("-I")
        .arg(repo("vendor/obs-studio/libobs"))
        .arg(&harness)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "issue 1381: could not run the C compiler `{cc}` ({e}). This gate compiles the \
                 shipped audio mixer C; it must FAIL rather than skip. Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "issue 1381: the lifted obs-audio.c mixer code does NOT COMPILE standalone under -Wall \
         -Wextra -Wformat=2 -Werror:\n--- cc stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin)
        .output()
        .expect("issue 1381: the compiled lift harness failed to execute");
    assert!(
        run.status.success(),
        "issue 1381: the lift harness exited non-zero:\n{}",
        String::from_utf8_lossy(&run.stdout)
    );
    let _ = fs::remove_dir_all(&dir);
    String::from_utf8(run.stdout)
        .expect("harness stdout is utf-8")
        .lines()
        .map(str::to_string)
        .collect()
}

/// The launch's own draw since issue 1425 slice 3 (no floor): the mic, whose packets reach the mixer a
/// little after their stamps, grows the mix by two ticks (42 ms) on its first ticks -- two hole ticks
/// and two ticks without the mic, all before any output runs -- and nothing else moves it.
const LAUNCH: &str = " waits=2 total_ms=42 mic_missed=2 ";

/// Every `ABOVE the floor` line of `scenario` is the launch's own draw by the mic.
fn only_the_launch_grew(trace: &[String], scenario: &str) -> bool {
    section(trace, scenario)
        .iter()
        .filter(|l| l.contains("ABOVE the floor"))
        .all(|l| l.contains("now 42 milliseconds (source: Mic/Aux)"))
}

/// The summary line of `scenario`.
fn summary<'a>(trace: &'a [String], scenario: &str) -> &'a str {
    let head = format!("{scenario}: ");
    trace
        .iter()
        .find(|l| l.starts_with(&head))
        .unwrap_or_else(|| {
            panic!(
                "issue 1381: no summary for {scenario}:\n{}",
                trace.join("\n")
            )
        })
}

/// The lines of one scenario (from its `== name` header to the next).
fn section<'a>(trace: &'a [String], scenario: &str) -> Vec<&'a str> {
    let head = format!("== {scenario}");
    let a = trace
        .iter()
        .position(|l| *l == head)
        .unwrap_or_else(|| panic!("issue 1381: no section {scenario}"));
    trace[a + 1..]
        .iter()
        .take_while(|l| !l.starts_with("== "))
        .map(String::as_str)
        .collect()
}

/// The truth table: the whole printed trace, verified line by line against the scenario physics
/// (the processed window = real time - the buffering; `waits` counts the ticks `audio_callback`
/// returned false; `mic_missed` the ticks it returned true without mixing the mic while it was
/// mixed; `stale` the mixes of an output buffer rendered from another front than the one mixed,
/// i.e. a re-anchor that was not re-rendered). Issue 1425 slice 3: no floor, so every launch draws
/// its own buffering from its startup race, here [`LAUNCH`]. The probes
/// call the guard directly at its exact-sample edges and past its locked re-check, and compare it
/// with `ignore_audio`.
const EXPECTED_TRACE: &[&str] = &[
    "== hidden-ndi-jumps",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 42 milliseconds of audio buffering, total audio buffering is now 42 milliseconds (source: Mic/Aux)",
    "L200 buffering-guard: 'NDI test' is not mixed: its audio ran 267.3 ms behind the mix window; re-anchored (dropped 30.0 ms, timeline restarted) instead of adding 277 ms to the whole mix's 42 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=30.0 ms)",
    "L200 buffering-guard: 'NDI test' is not mixed: its audio ran 358.0 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 362 ms to the whole mix's 42 ms of audio buffering (issue 1381; events=2, +1 since the last line, dropped_total=50.0 ms)",
    "L200 buffering-guard: 'NDI test' is not mixed: its audio ran 658.7 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 661 ms to the whole mix's 42 ms of audio buffering (issue 1381; events=3, +1 since the last line, dropped_total=70.0 ms)",
    "L200 buffering-guard: 'NDI test' is not mixed: its audio ran 1459.3 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 917 ms to the whole mix's 42 ms of audio buffering (issue 1381; events=4, +1 since the last line, dropped_total=90.0 ms)",
    "hidden-ndi-jumps: waits=2 total_ms=42 mic_missed=2 'NDI test' events=4 dropped_ms=90.0 mixed=0 stale=0 guard_lines=4 above_lines=1 restart_lines=0",
    "== hidden-kept-stamp-late",
    "L200 buffering-guard: 'Cam audio' is not mixed: its audio ran 300.0 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 320 ms to the whole mix's 0 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=20.0 ms)",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 42 milliseconds of audio buffering, total audio buffering is now 42 milliseconds (source: Mic/Aux)",
    "L200 buffering-guard: 'Cam audio' is not mixed: its audio ran 258.0 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 277 ms to the whole mix's 42 ms of audio buffering (issue 1381; events=2814, +2813 since the last line, dropped_total=60030.0 ms)",
    "L200 buffering-guard: 'Cam audio' is not mixed: its audio ran 258.7 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 277 ms to the whole mix's 42 ms of audio buffering (issue 1381; events=5627, +2813 since the last line, dropped_total=120040.0 ms)",
    "hidden-kept-stamp-late: waits=2 total_ms=42 mic_missed=2 'Cam audio' events=5726 dropped_ms=122150.0 mixed=0 stale=0 guard_lines=3 above_lines=1 restart_lines=0",
    "== hidden-ingest-race",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 42 milliseconds of audio buffering, total audio buffering is now 42 milliseconds (source: Mic/Aux)",
    "L200 buffering-guard: 'NDI race' is not mixed: its audio ran 257.3 ms behind the mix window; re-anchored (dropped 257.3 ms) instead of adding 277 ms to the whole mix's 42 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=257.3 ms)",
    "hidden-ingest-race: waits=2 total_ms=42 mic_missed=2 'NDI race' events=1 dropped_ms=257.3 mixed=0 stale=0 guard_lines=1 above_lines=1 restart_lines=0",
    "== audio-reset",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 42 milliseconds of audio buffering, total audio buffering is now 42 milliseconds (source: Mic/Aux)",
    "L200 buffering-guard: 'NDI old' is not mixed: its audio ran 42.7 ms behind the mix window; re-anchored (dropped 42.7 ms) instead of adding 42 ms to the whole mix's 0 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=42.7 ms)",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 42 milliseconds of audio buffering, total audio buffering is now 42 milliseconds (source: Mic/Aux)",
    "audio-reset: waits=4 total_ms=42 mic_missed=2 'NDI old' events=2 dropped_ms=278.7 mixed=47 stale=0 guard_lines=1 above_lines=2 restart_lines=0",
    "== mixed-late",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 42 milliseconds of audio buffering, total audio buffering is now 42 milliseconds (source: Mic/Aux)",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 128 milliseconds of audio buffering, total audio buffering is now 170 milliseconds (source: mbc)",
    "mixed-late: waits=8 total_ms=170 mic_missed=2 'mbc' events=0 dropped_ms=0.0 mixed=290 stale=0 guard_lines=0 above_lines=2 restart_lines=0",
    "== cut-in-backlog",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 42 milliseconds of audio buffering, total audio buffering is now 42 milliseconds (source: Mic/Aux)",
    "L200 buffering-guard: 'NDI cut' entered the mix late: its audio ran 236.0 ms behind the mix window; re-anchored (dropped 236.0 ms) instead of adding 256 ms to the whole mix's 42 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=236.0 ms)",
    "cut-in-backlog: waits=2 total_ms=42 mic_missed=2 'NDI cut' events=1 dropped_ms=236.0 mixed=200 stale=0 guard_lines=1 above_lines=1 restart_lines=0",
    "== cut-in-jump",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 42 milliseconds of audio buffering, total audio buffering is now 42 milliseconds (source: Mic/Aux)",
    "L200 buffering-guard: 'NDI jump' entered the mix late: its audio ran 260.7 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 277 ms to the whole mix's 42 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=20.0 ms)",
    "cut-in-jump: waits=2 total_ms=42 mic_missed=2 'NDI jump' events=1 dropped_ms=20.0 mixed=197 stale=0 guard_lines=1 above_lines=1 restart_lines=0",
    "== cut-in-scene",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 42 milliseconds of audio buffering, total audio buffering is now 42 milliseconds (source: Mic/Aux)",
    "cut-in-scene: waits=2 total_ms=42 mic_missed=2 'Scene' events=0 dropped_ms=0.0 mixed=0 stale=0 guard_lines=0 above_lines=1 restart_lines=0",
    "== cut-in-kept-stamp-limit",
    "L200 buffering-guard: 'NDI genlock tc' is not mixed: its audio ran 300.0 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 320 ms to the whole mix's 0 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=20.0 ms)",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 42 milliseconds of audio buffering, total audio buffering is now 42 milliseconds (source: Mic/Aux)",
    "L200 buffering-guard: 'NDI genlock tc' entered the mix late: its audio ran 260.7 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 277 ms to the whole mix's 42 ms of audio buffering (issue 1381; events=101, +100 since the last line, dropped_total=2150.0 ms)",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 298 milliseconds of audio buffering, total audio buffering is now 341 milliseconds (source: NDI genlock tc)",
    "cut-in-kept-stamp-limit: waits=16 total_ms=341 mic_missed=2 'NDI genlock tc' events=101 dropped_ms=2150.0 mixed=184 stale=0 guard_lines=2 above_lines=2 restart_lines=0",
    "== launch-late",
    "L200 buffering-guard: 'ASIO' is not mixed: its audio ran 100.0 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 106 ms to the whole mix's 0 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=20.0 ms)",
    "L200 buffering-guard: 'Mic/Aux' is not mixed: its audio ran 1.3 ms behind the mix window; re-anchored (dropped 1.3 ms) instead of adding 21 ms to the whole mix's 0 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=1.3 ms)",
    "L200 buffering-guard: 'Mic/Aux' entered the mix late: its audio ran 3.3 ms behind the mix window; re-anchored (dropped 3.3 ms) instead of adding 21 ms to the whole mix's 0 ms of audio buffering (issue 1381; events=10, +9 since the last line, dropped_total=43.3 ms)",
    "L200 buffering-guard: 'ASIO' entered the mix late: its audio ran 103.3 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 106 ms to the whole mix's 0 ms of audio buffering (issue 1381; events=11, +10 since the last line, dropped_total=230.0 ms)",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 128 milliseconds of audio buffering, total audio buffering is now 128 milliseconds (source: ASIO)",
    "launch-late: waits=6 total_ms=128 mic_missed=2 'ASIO' events=11 dropped_ms=230.0 mixed=282 stale=0 guard_lines=4 above_lines=1 restart_lines=0",
    "== e1-asio-outage-128",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 42 milliseconds of audio buffering, total audio buffering is now 42 milliseconds (source: Mic/Aux)",
    "L200 buffering-guard: 'mbc' fell behind the mix (frozen at 42 ms): its audio ran 166.0 ms behind the mix window; re-anchored (dropped 166.0 ms) instead of adding 170 ms to the whole mix's 42 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=166.0 ms)",
    "e1-asio-outage-128: waits=2 total_ms=42 mic_missed=2 'mbc' events=1 dropped_ms=166.0 mixed=1986 stale=0 guard_lines=1 above_lines=1 restart_lines=0",
    "e1-asio-outage-128 freeze: frozen=1 encoders=1 'mbc' mixed_late=1 mixed_late_dropped_ms=166.0 mix_mixed_late=1",
    "== e1-asio-outage-192",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 42 milliseconds of audio buffering, total audio buffering is now 42 milliseconds (source: Mic/Aux)",
    "L200 buffering-guard: 'mbc' fell behind the mix (frozen at 42 ms): its audio ran 226.0 ms behind the mix window; re-anchored (dropped 226.0 ms) instead of adding 234 ms to the whole mix's 42 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=226.0 ms)",
    "e1-asio-outage-192: waits=2 total_ms=42 mic_missed=2 'mbc' events=1 dropped_ms=226.0 mixed=1983 stale=0 guard_lines=1 above_lines=1 restart_lines=0",
    "e1-asio-outage-192 freeze: frozen=1 encoders=1 'mbc' mixed_late=1 mixed_late_dropped_ms=226.0 mix_mixed_late=1",
    "== e1-media-stall-85",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 42 milliseconds of audio buffering, total audio buffering is now 42 milliseconds (source: Mic/Aux)",
    "L200 buffering-guard: 'pre-fotky video' fell behind the mix (frozen at 42 ms): its audio ran 116.0 ms behind the mix window; re-anchored (dropped 116.0 ms) instead of adding 128 ms to the whole mix's 42 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=116.0 ms)",
    "e1-media-stall-85: waits=2 total_ms=42 mic_missed=2 'pre-fotky video' events=1 dropped_ms=116.0 mixed=1988 stale=0 guard_lines=1 above_lines=1 restart_lines=0",
    "e1-media-stall-85 freeze: frozen=1 encoders=1 'pre-fotky video' mixed_late=1 mixed_late_dropped_ms=116.0 mix_mixed_late=1",
    "== e1-asio-outage-128-no-encoder",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 42 milliseconds of audio buffering, total audio buffering is now 42 milliseconds (source: Mic/Aux)",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 170 milliseconds of audio buffering, total audio buffering is now 213 milliseconds (source: mbc)",
    "e1-asio-outage-128-no-encoder: waits=10 total_ms=213 mic_missed=2 'mbc' events=0 dropped_ms=0.0 mixed=1978 stale=0 guard_lines=0 above_lines=2 restart_lines=0",
    "e1-asio-outage-128-no-encoder freeze: frozen=0 encoders=0 'mbc' mixed_late=0 mixed_late_dropped_ms=0.0 mix_mixed_late=0",
    "== encoder-on-off",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 42 milliseconds of audio buffering, total audio buffering is now 42 milliseconds (source: Mic/Aux)",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 128 milliseconds of audio buffering, total audio buffering is now 170 milliseconds (source: Media)",
    "L200 buffering-guard: 'Media' fell behind the mix (frozen at 170 ms): its audio ran 132.7 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 149 ms to the whole mix's 170 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=20.0 ms)",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 170 milliseconds of audio buffering, total audio buffering is now 341 milliseconds (source: Media)",
    "encoder-on-off: waits=16 total_ms=341 mic_missed=2 'Media' events=700 dropped_ms=14930.0 mixed=1881 stale=0 guard_lines=1 above_lines=3 restart_lines=0",
    "encoder-on-off freeze: frozen=0 encoders=0 'Media' mixed_late=700 mixed_late_dropped_ms=14930.0 mix_mixed_late=700",
    "== late-start-kept-stamp",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 42 milliseconds of audio buffering, total audio buffering is now 42 milliseconds (source: Mic/Aux)",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 106 milliseconds of audio buffering, total audio buffering is now 149 milliseconds (source: media clip)",
    "late-start-kept-stamp: waits=7 total_ms=149 mic_missed=2 'media clip' events=0 dropped_ms=0.0 mixed=294 stale=0 guard_lines=0 above_lines=2 restart_lines=0",
    "late-start-kept-stamp freeze: frozen=0 encoders=0 'media clip' mixed_late=0 mixed_late_dropped_ms=0.0 mix_mixed_late=0",
    "== late-start-kept-stamp-encoder",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 42 milliseconds of audio buffering, total audio buffering is now 42 milliseconds (source: Mic/Aux)",
    "L200 buffering-guard: 'media clip' fell behind the mix (frozen at 42 ms): its audio ran 85.3 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 85 ms to the whole mix's 42 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=20.0 ms)",
    "late-start-kept-stamp-encoder: waits=2 total_ms=42 mic_missed=2 'media clip' events=300 dropped_ms=6390.0 mixed=0 stale=0 guard_lines=1 above_lines=1 restart_lines=0",
    "late-start-kept-stamp-encoder freeze: frozen=1 encoders=1 'media clip' mixed_late=300 mixed_late_dropped_ms=6390.0 mix_mixed_late=300",
    "== frozen-ingest-race",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 42 milliseconds of audio buffering, total audio buffering is now 42 milliseconds (source: Mic/Aux)",
    "L200 buffering-guard: 'NDI race' fell behind the mix (frozen at 42 ms): its audio ran 257.3 ms behind the mix window; re-anchored (dropped 257.3 ms) instead of adding 277 ms to the whole mix's 42 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=257.3 ms)",
    "frozen-ingest-race: waits=2 total_ms=42 mic_missed=2 'NDI race' events=1 dropped_ms=257.3 mixed=1696 stale=0 guard_lines=1 above_lines=1 restart_lines=0",
    "frozen-ingest-race freeze: frozen=1 encoders=1 'NDI race' mixed_late=1 mixed_late_dropped_ms=257.3 mix_mixed_late=1",
    "== encoder-at-launch",
    "L200 buffering-guard: 'mbc' entered the mix late: its audio ran 2.7 ms behind the mix window; re-anchored (dropped 2.7 ms) instead of adding 21 ms to the whole mix's 0 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=2.7 ms)",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 42 milliseconds of audio buffering, total audio buffering is now 42 milliseconds (source: mbc)",
    "L200 genlock audio buffering: grew 42 ms while an encoder ran inside the launch settle window",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 42 milliseconds of audio buffering, total audio buffering is now 85 milliseconds (source: mbc)",
    "encoder-at-launch: waits=4 total_ms=85 mic_missed=2 'mbc' events=1 dropped_ms=2.7 mixed=293 stale=0 guard_lines=1 above_lines=2 restart_lines=0",
    "encoder-at-launch freeze: frozen=0 encoders=1 'mbc' mixed_late=0 mixed_late_dropped_ms=0.0 mix_mixed_late=0",
    "== encoder-after-settle",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 42 milliseconds of audio buffering, total audio buffering is now 42 milliseconds (source: Mic/Aux)",
    "L200 buffering-guard: 'mbc' fell behind the mix (frozen at 42 ms): its audio ran 6.0 ms behind the mix window; re-anchored (dropped 6.0 ms) instead of adding 21 ms to the whole mix's 42 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=6.0 ms)",
    "encoder-after-settle: waits=2 total_ms=42 mic_missed=2 'mbc' events=200 dropped_ms=1611.7 mixed=1596 stale=0 guard_lines=1 above_lines=1 restart_lines=0",
    "encoder-after-settle freeze: frozen=1 encoders=1 'mbc' mixed_late=200 mixed_late_dropped_ms=1611.7 mix_mixed_late=200",
    "== encoder-at-launch-deep-race",
    "L200 buffering-guard: 'ASIO' entered the mix late: its audio ran 100.0 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 106 ms to the whole mix's 0 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=20.0 ms)",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 128 milliseconds of audio buffering, total audio buffering is now 128 milliseconds (source: ASIO)",
    "encoder-at-launch-deep-race frozen_ticks=0",
    "encoder-at-launch-deep-race: waits=6 total_ms=128 mic_missed=2 'ASIO' events=1 dropped_ms=20.0 mixed=292 stale=0 guard_lines=1 above_lines=1 restart_lines=0",
    "encoder-at-launch-deep-race freeze: frozen=0 encoders=1 'ASIO' mixed_late=0 mixed_late_dropped_ms=0.0 mix_mixed_late=0",
    "== probes",
    "probe 1ns-behind-is-rounding: reason=0 in_sync=-1 ts=start-1ns left=960 pending=0 timing_set=1 events=0 ignore_audio=n/a",
    "L200 buffering-guard: 'probe' is not mixed: its audio ran 0.0 ms behind the mix window; re-anchored (dropped 0.0 ms) instead of adding 0 ms to the whole mix's 0 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=0.0 ms)",
    "probe 2ns-behind: reason=1 in_sync=1 ts=start+20831ns left=959 pending=0 timing_set=1 events=1 ignore_audio=same",
    "L200 buffering-guard: 'probe' is not mixed: its audio ran 10.0 ms behind the mix window; re-anchored (dropped 10.0 ms) instead of adding 21 ms to the whole mix's 0 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=10.0 ms)",
    "probe 10ms-exact: reason=1 in_sync=1 ts=start+0ns left=480 pending=0 timing_set=1 events=1 ignore_audio=same",
    "L200 buffering-guard: 'probe' is not mixed: its audio ran 0.0 ms behind the mix window; re-anchored (dropped 0.0 ms) instead of adding 21 ms to the whole mix's 0 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=0.0 ms)",
    "probe rounding-adjust: reason=1 in_sync=1 ts=start+0ns left=959 pending=0 timing_set=1 events=1 ignore_audio=same",
    "L200 buffering-guard: 'probe' is not mixed: its audio ran 30.0 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 42 ms to the whole mix's 0 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=20.0 ms)",
    "L200 Source probe audio is lagging (over by 10.00 ms) at max audio buffering. Restarting source audio.",
    "probe exhausted: reason=1 in_sync=0 ts=restarted left=0 pending=1 timing_set=0 events=1 ignore_audio=same",
    "probe moved-on-before-the-lock: reason=0 in_sync=0 ts=start-1ns left=960 pending=0 timing_set=1 events=0 ignore_audio=n/a",
    "probe reset-before-the-lock: reason=0 in_sync=0 ts=restarted left=960 pending=0 timing_set=1 events=0 ignore_audio=n/a",
    "probe joined: first-tick=1 next-tick=0 skipped-a-tick=1 later-first=1",
    "probe reason-1425 mecbf=00000..11111: 00110000001100000003000000220000",
    "L200 buffering-guard: 'probe late' fell behind the mix (frozen at 85 ms): its audio ran 10.0 ms behind the mix window; re-anchored (dropped 10.0 ms) instead of adding 21 ms to the whole mix's 85 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=10.0 ms)",
    "probe mixed-late: reason=3 in_sync=1 events=1 mixed_late=1 mixed_late_dropped_ms=10.0 mix_mixed_late=1",
];

#[test]
fn a_source_that_is_not_mixed_never_grows_the_mix_buffering_1381() {
    let trace = c_trace();
    for scenario in [
        "hidden-ndi-jumps",
        "hidden-kept-stamp-late",
        "hidden-ingest-race",
    ] {
        let s = summary(trace, scenario);
        assert!(
            s.contains(LAUNCH) && only_the_launch_grew(trace, scenario),
            "issue 1381: a source that is not mixed moved the mix window ({scenario}):\n{s}\n\n{}",
            section(trace, scenario).join("\n")
        );
    }
    // An audio reset of a running OBS starts the mixer's state over; a source mixed before it and
    // hidden + late after it must still not count (only the post-reset launch draw, 2 more waits).
    let reset = summary(trace, "audio-reset");
    assert!(
        reset.contains(" waits=4 total_ms=42 ") && only_the_launch_grew(trace, "audio-reset"),
        "issue 1381: after an audio reset a source's stale mix tick read as a member:\n{reset}\n\n{}",
        section(trace, "audio-reset").join("\n")
    );
}

#[test]
fn a_mixed_late_source_keeps_the_upstream_increase_1381() {
    let trace = c_trace();
    let s = summary(trace, "mixed-late");
    assert!(
        s.contains(" total_ms=170 ") && s.contains(" above_lines=2 ") && s.contains(" events=0 "),
        "issue 1381: a MIXED late source must keep OBS's own dynamic increase:\n{s}"
    );
    // A direct-stamp source 100 ms late when it joins at launch (the frontend loads the scene
    // collection after the audio thread starts): one entry re-anchor, then it is a late MIXED
    // source and upstream reaches the same 128 ms it reached before the guard (the 1367 stream
    // ASIO startup race).
    let s = summary(trace, "launch-late");
    assert!(
        s.contains(" total_ms=128 ") && s.contains(" above_lines=1 "),
        "issue 1381: a source late at launch must still reach upstream's increase:\n{s}"
    );
}

#[test]
fn a_cut_into_the_mix_is_re_anchored_without_a_hole_1381() {
    let trace = c_trace();
    for scenario in ["cut-in-backlog", "cut-in-jump"] {
        let s = summary(trace, scenario);
        assert!(
            s.contains(LAUNCH)
                && only_the_launch_grew(trace, scenario)
                && s.contains(" events=1 ")
                && s.contains(" stale=0 "),
            "issue 1381: a late source cut into the mix cut a hole into the others ({scenario}):\n{s}\n\n{}",
            section(trace, scenario).join("\n")
        );
        assert!(
            section(trace, scenario)
                .iter()
                .any(|l| l.contains("entered the mix late")),
            "issue 1381: the entry re-anchor of {scenario} must be logged"
        );
    }
    // A program scene (a composite, fed by its children) joins the mix and is never re-anchored.
    let scene = summary(trace, "cut-in-scene");
    assert!(
        scene.contains(LAUNCH)
            && only_the_launch_grew(trace, "cut-in-scene")
            && scene.contains(" events=0 "),
        "issue 1381: a composite cut into the mix was re-anchored:\n{scene}"
    );
}

#[test]
fn the_guard_lines_are_loud_and_name_what_they_saved_1381() {
    let trace = c_trace();
    let lines: Vec<&String> = trace
        .iter()
        .filter(|l| l.contains("buffering-guard: "))
        .collect();
    assert!(
        !lines.is_empty(),
        "issue 1381: no buffering-guard line at all"
    );
    for l in lines {
        assert!(
            l.starts_with("L200 buffering-guard: '")
                && (l.contains("' is not mixed: ")
                    || l.contains("' entered the mix late: ")
                    || l.contains("' fell behind the mix (frozen at "))
                && l.contains(" ms behind the mix window; re-anchored (dropped ")
                && l.contains(" instead of adding ")
                && l.contains(" (issue 1381; events=")
                && !l.contains("is now"),
            "issue 1381: a malformed buffering-guard line: {l}"
        );
    }
}

#[test]
fn the_guard_holds_its_exact_sample_edges_1381() {
    let trace = c_trace();
    let probes: Vec<&str> = section(trace, "probes")
        .into_iter()
        .filter(|l| l.starts_with("probe "))
        .collect();
    let want = [
        // 1 ns behind is discard_audio's rounding, never a re-anchor
        "probe 1ns-behind-is-rounding: reason=0 in_sync=-1 ts=start-1ns",
        // a whole number of samples behind lands exactly on the window start
        "probe 10ms-exact: reason=1 in_sync=1 ts=start+0ns left=480 ",
        // one truncated sample short of the start is ignore_audio's rounding adjust
        "probe rounding-adjust: reason=1 in_sync=1 ts=start+0ns left=959 ",
        // nothing left: the timeline restarts like ignore_audio (pending, timing reset)
        "probe exhausted: reason=1 in_sync=0 ts=restarted left=0 pending=1 timing_set=0 ",
        // the ingest thread moved the timeline before the lock: nothing dropped, nothing counted
        "probe moved-on-before-the-lock: reason=0 in_sync=0 ts=start-1ns left=960 pending=0 timing_set=1 events=0",
        "probe reset-before-the-lock: reason=0 in_sync=0 ts=restarted left=960 pending=0 timing_set=1 events=0",
        // entry = not in the mix on the previous tick; a source's first membership is an entry
        "probe joined: first-tick=1 next-tick=0 skipped-a-tick=1 later-first=1",
    ];
    // The re-anchor mirrors ignore_audio (drop, rounding adjust, restart): same state on every probe
    // that re-anchors.
    for l in &probes {
        if l.contains(" reason=1 ") {
            assert!(
                l.ends_with(" ignore_audio=same"),
                "issue 1381: the re-anchor drifted from ignore_audio: {l}"
            );
        }
    }
    for w in want {
        assert!(
            probes.iter().any(|l| l.starts_with(w)),
            "issue 1381: no probe line starting `{w}`:\n{}",
            probes.join("\n")
        );
    }
}

/// The freeze line of `scenario`.
fn freeze<'a>(trace: &'a [String], scenario: &str) -> &'a str {
    let head = format!("{scenario} freeze: ");
    trace
        .iter()
        .find(|l| l.starts_with(&head))
        .unwrap_or_else(|| panic!("issue 1425: no freeze line for {scenario}"))
}

#[test]
fn the_e1_events_no_longer_grow_a_frozen_mix_1425() {
    let trace = c_trace();
    // Stock libobs grew the stream-snv mix +128 / +192 / +85 ms for good on these three events. Frozen
    // (the stream's audio encoder runs, from 2 s after launch), the mix stays at the launch's own draw,
    // no tick after the launch is a hole and the mic is heard on every one, and the late member is
    // re-anchored ONCE: one counted dropout of its late span (issue 1425 slice 3: measured against a
    // 42 ms launch draw, no 85 ms floor), then it is mixed again.
    for (scenario, src, late_ms) in [
        ("e1-asio-outage-128", "mbc", "166.0"),
        ("e1-asio-outage-192", "mbc", "226.0"),
        ("e1-media-stall-85", "pre-fotky video", "116.0"),
    ] {
        let s = summary(trace, scenario);
        assert!(
            s.contains(LAUNCH)
                && only_the_launch_grew(trace, scenario)
                && s.contains(&format!("'{src}' events=1 dropped_ms={late_ms} "))
                && s.contains(" stale=0 guard_lines=1 above_lines=1 "),
            "issue 1425: {scenario} grew the frozen mix or cut a hole:
{s}

{}",
            section(trace, scenario).join("\n")
        );
        let f = freeze(trace, scenario);
        assert!(
            f.contains(" frozen=1 ")
                && f.contains(&format!(
                    " mixed_late=1 mixed_late_dropped_ms={late_ms} mix_mixed_late=1"
                )),
            "issue 1425: {scenario} was not one counted MIXED_LATE dropout:
{f}"
        );
        assert!(
            section(trace, scenario).iter().any(|l| l
                .contains(&format!("'{src}' fell behind the mix (frozen at 42 ms)"))
                && l.contains(" instead of adding ")),
            "issue 1425: {scenario}'s re-anchor must name the freeze and what it saved"
        );
    }
    // The late member is mixed again after its one dropout (output continuous).
    let s = summary(trace, "e1-asio-outage-128");
    let mixed: u64 = s
        .split(" mixed=")
        .nth(1)
        .and_then(|t| t.split_whitespace().next())
        .and_then(|t| t.parse().ok())
        .expect("mixed= field");
    assert!(mixed > 500, "issue 1425: mbc was not mixed again: {s}");
}

#[test]
fn only_a_running_audio_encoder_freezes_the_mix_1425() {
    // ROZHODNUTÉ 6217557472: with no audio encoder (the resolume cg OBS shape) the 128 ms outage, 34 s
    // after launch, grows the mix exactly as stock libobs: to 213 ms (+170 ms over the 42 ms launch
    // draw), one hole of 8 ticks, no re-anchor.
    let trace = c_trace();
    let s = summary(trace, "e1-asio-outage-128-no-encoder");
    assert!(
        s.contains(" waits=10 total_ms=213 mic_missed=2 ")
            && s.contains("'mbc' events=0 ")
            && s.contains(" guard_lines=0 above_lines=2 "),
        "issue 1425: a box with no audio encoder must keep stock growth:\n{s}\n\n{}",
        section(trace, "e1-asio-outage-128-no-encoder").join("\n")
    );
    assert!(
        freeze(trace, "e1-asio-outage-128-no-encoder")
            .contains(" frozen=0 encoders=0 'mbc' mixed_late=0 "),
        "issue 1425: nothing but an audio encoder may freeze the mix"
    );
    // The encoder is read on every tick: no encoder, a late member grows the mix (170 ms); a stream
    // starts after the launch settle window and the member goes 300 ms late for good: re-anchored on
    // every tick (700 events, one line), never a growth; the stream stops 49 s after launch, and the
    // still-late member grows the mix again (341 ms) and plays.
    let s = summary(trace, "encoder-on-off");
    assert!(
        s.contains(" waits=16 total_ms=341 mic_missed=2 ")
            && s.contains("'Media' events=700 ")
            && s.contains(" guard_lines=1 above_lines=3 "),
        "issue 1425: the freeze must follow the encoder on and off:\n{s}\n\n{}",
        section(trace, "encoder-on-off").join("\n")
    );
    assert!(
        freeze(trace, "encoder-on-off").contains(" frozen=0 encoders=0 'Media' mixed_late=700 "),
        "issue 1425: encoder-on-off must end unfrozen with the 700 re-anchors of the frozen span"
    );
}

#[test]
fn a_clip_started_late_in_place_grows_the_mix_unless_an_encoder_runs_1425() {
    // The resolume shape: a media clip in the program mix starts in place 34 s after launch (no cut,
    // so no join) with its stamps 128 ms late for good. No audio encoder: stock growth, the clip plays.
    let trace = c_trace();
    let s = summary(trace, "late-start-kept-stamp");
    assert!(
        s.contains(" total_ms=149 mic_missed=2 ")
            && s.contains("'media clip' events=0 ")
            && s.contains(" mixed=294 ")
            && s.contains(" above_lines=2 "),
        "issue 1425: with no audio encoder a late clip must grow the mix and play:\n{s}"
    );
    // Its twin while a recording runs: frozen, every packet is re-anchored, so the clip is muted while
    // it stays late (the slice-2b limit, the owner's call) and the mix never grows.
    let s = summary(trace, "late-start-kept-stamp-encoder");
    assert!(
        s.contains(LAUNCH)
            && s.contains("'media clip' events=300 ")
            && s.contains(" mixed=0 ")
            && s.contains(" guard_lines=1 above_lines=1 "),
        "issue 1425: a member late for good while an encoder runs must be re-anchored on every \
         tick, never grow the mix:\n{s}"
    );
}

#[test]
fn a_frozen_mix_never_grows_for_an_ingest_race_1425() {
    // The ingest thread places a mixed source late between the render loop and calc_min_ts: min_ts
    // falls behind the window, but the frozen action adds nothing; the next tick re-anchors it.
    let trace = c_trace();
    let s = summary(trace, "frozen-ingest-race");
    assert!(
        s.contains(LAUNCH) && only_the_launch_grew(trace, "frozen-ingest-race"),
        "issue 1425: the frozen action grew the mix for an ingest race:
{s}"
    );
    assert!(
        freeze(trace, "frozen-ingest-race").contains(" mixed_late=1 "),
        "issue 1425: the raced source was not re-anchored as MIXED_LATE"
    );
}

#[test]
fn an_output_started_inside_the_launch_settle_window_grows_once_and_plays_1425() {
    // ROZHODNUTÉ 6221252305: an audio encoder running from the first tick (--startstreaming, Companion,
    // an operator right after a launch) with `mbc` 2.7 ms late. Before the window the mix froze at
    // 0 ms and `mbc` + the mic were re-anchored on every tick, muted for the whole output. Inside the
    // 30 s launch settle window the startup race grows the mix as without an encoder: `mbc` is
    // re-anchored once as it joins, then grows the mix 42 ms and plays; the mic only misses the
    // launch's two hole ticks. `mbc` goes 50 ms later at 4 s, still inside the window, and grows it
    // again to 85 ms (two more hole ticks). The growth while the encoder runs is logged ONCE, for
    // the first growth: two growths, one line (the log-once latch).
    let trace = c_trace();
    let s = summary(trace, "encoder-at-launch");
    assert!(
        s.contains(" waits=4 total_ms=85 mic_missed=2 ")
            && s.contains("'mbc' events=1 dropped_ms=2.7 mixed=293 ")
            && s.contains(" above_lines=2 "),
        "issue 1425: an output started inside the launch settle window must not mute the late \
         members:\n{s}\n\n{}",
        section(trace, "encoder-at-launch").join("\n")
    );
    assert!(
        freeze(trace, "encoder-at-launch").contains(" frozen=0 encoders=1 'mbc' mixed_late=0 "),
        "issue 1425: the mix must stay unfrozen inside the launch settle window"
    );
    let sec = section(trace, "encoder-at-launch");
    assert_eq!(
        sec.iter()
            .filter(|l| l.contains("while an encoder ran inside the launch settle window"))
            .count(),
        1,
        "issue 1425: a growth inside the launch settle window while an encoder runs is logged once"
    );
    assert!(
        sec.contains(
            &"L200 genlock audio buffering: grew 42 ms while an encoder ran inside the launch settle window"
        ),
        "issue 1425: the settle-window growth line names the growth"
    );
}

#[test]
fn the_launch_settle_window_reads_the_input_tick_never_the_processed_window_1425() {
    // A member 100 ms late on the first tick grows the mix several ticks at once, so the processed
    // windows that follow start BEFORE the first tick. The window counts on audio_callback's input
    // tick start, which only advances: no tick inside the window is frozen, and nothing is
    // re-anchored as MIXED_LATE.
    let trace = c_trace();
    assert!(
        trace
            .iter()
            .any(|l| l == "encoder-at-launch-deep-race frozen_ticks=0"),
        "issue 1425: a tick inside the launch settle window was frozen:\n{}",
        section(trace, "encoder-at-launch-deep-race").join("\n")
    );
    assert!(
        freeze(trace, "encoder-at-launch-deep-race")
            .contains(" frozen=0 encoders=1 'ASIO' mixed_late=0 "),
        "issue 1425: the deep startup race must not be re-anchored as MIXED_LATE"
    );
}

#[test]
fn an_output_started_after_the_launch_settle_window_freezes_the_mix_1425() {
    // The slice-2 freeze is unchanged once the launch has settled: the stream starts 32 s after
    // launch, and `mbc` going later than the launch's draw is re-anchored (MIXED_LATE) on every
    // tick, the mix never grows, and no settle-window line is logged.
    let trace = c_trace();
    let s = summary(trace, "encoder-after-settle");
    assert!(
        s.contains(LAUNCH)
            && s.contains("'mbc' events=200 ")
            && only_the_launch_grew(trace, "encoder-after-settle"),
        "issue 1425: an output started after the launch settle window must freeze the mix:\n{s}"
    );
    assert!(
        freeze(trace, "encoder-after-settle")
            .contains(" frozen=1 encoders=1 'mbc' mixed_late=200 "),
        "issue 1425: the late member must be re-anchored as MIXED_LATE after the window"
    );
    assert!(
        trace
            .iter()
            .filter(|l| l.contains("inside the launch settle window"))
            .count()
            == 1,
        "issue 1425: only the encoder-at-launch scenario grows inside the settle window"
    );
}

#[test]
fn mixed_late_is_only_a_frozen_member_behind_the_window_1425() {
    // Bits m e c b f (member, entered, composite, behind, frozen), 00000 .. 11111: a composite or a
    // source on time is never re-anchored; NOT_MIXED (1) and ENTERED (2) win over the freeze; only a
    // member, not entering, behind, while frozen is MIXED_LATE (3).
    let trace = c_trace();
    assert!(
        trace
            .iter()
            .any(|l| l == "probe reason-1425 mecbf=00000..11111: 00110000001100000003000000220000"),
        "issue 1425: the guard's reason table changed:
{}",
        section(trace, "probes").join("\n")
    );
    assert!(
        trace.iter().any(|l| l
            == "probe mixed-late: reason=3 in_sync=1 events=1 mixed_late=1 mixed_late_dropped_ms=10.0 mix_mixed_late=1"),
        "issue 1425: a MIXED_LATE re-anchor must count on the source and the mix"
    );
}

#[test]
fn the_shipped_c_matches_the_truth_table_1381() {
    let trace = c_trace();
    for (i, (got, want)) in trace.iter().zip(EXPECTED_TRACE).enumerate() {
        assert_eq!(
            got,
            want,
            "issue 1381: the lifted C diverges from the decided behaviour at trace line {i}:\n{}",
            trace.join("\n")
        );
    }
    assert_eq!(
        trace.len(),
        EXPECTED_TRACE.len(),
        "issue 1381: the lifted C printed {} lines, the truth table has {}:\n{}",
        trace.len(),
        EXPECTED_TRACE.len(),
        trace.join("\n")
    );
}
