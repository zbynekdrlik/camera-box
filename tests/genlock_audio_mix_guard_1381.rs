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
//!   `convert_time_to_frames`, `ignore_audio` .. `calc_min_ts` and the guard), substituted into
//!   `tests/c/genlock_audio_mix_guard_1381_harness.c`. That stub libobs feeds sources the way
//!   `source_output_audio_place` does (bytes only) and drives scenarios: hidden late sources, an
//!   off-program scene, a mixed late source, cuts into the mix, a launch-late source. The printed
//!   trace is the truth table. `cc` is required — it FAILS LOUDLY rather than skips.

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

/// `(file, squished needle)` — the wiring both this test and the pwsh guard in both
/// `windows-genlock*.yml` workflows require, each exactly once. ONE list, so the two copies cannot
/// drift apart.
const WIRING: [(&str, &str); 6] = [
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
];

const WINDOWS_WORKFLOWS: [&str; 2] = [
    ".github/workflows/windows-genlock.yml",
    ".github/workflows/windows-genlock-fast.yml",
];

fn pwsh_var(file: &str) -> &'static str {
    match file {
        OBS_AUDIO => "$audio1381",
        OBS_INTERNAL => "$internal1381",
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
        2,
        "issue 1381: the mixer tick counter lives in BOTH struct obs_core_audio and struct obs_source"
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
        for (file, var) in [(OBS_AUDIO, "$audio1381"), (OBS_INTERNAL, "$internal1381")] {
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

/// The blocks of `audio_callback`, verbatim, in order.
fn lift_callback_blocks() -> [String; 4] {
    let s = read(OBS_AUDIO);
    let decision_start = "\tconst int genlock_buffering = genlock_audio_buffering_action(";
    let decision_tail =
        "add_audio_buffering(audio, sample_rate, &ts, min_ts, buffering_name);\n\t}\n";
    let a = s
        .find(decision_start)
        .expect("issue 1381: the 1367 tick decision is gone");
    let b = s[a..]
        .find(decision_tail)
        .expect("issue 1381: the 1367 tick decision's end is gone")
        + decision_tail.len();
    [
        // the render-order build after the output mixes: the mark + the catch-all loop
        slice_between(
            &s,
            "\tpthread_mutex_unlock(&obs->video.mixes_mutex);\n",
            "\t/* ------------------------------------------------ */\n\t/* render audio data */",
        ),
        // the render loop + the minimum timestamp
        slice_between(
            &s,
            "\t/* render audio data */\n",
            "\t/* camera-box #800: audio-side telemetry",
        ),
        s[a..a + b].to_string(),
        // mix + discard
        slice_between(
            &s,
            "\t/* mix audio */\n",
            "\t/* ------------------------------------------------ */\n\t/* release audio sources */",
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
/// (the processed window = real time - 85.33 ms at the floor; `waits` counts the ticks
/// `audio_callback` returned false, the launch floor alone is 4; `mic_missed` the ticks it returned
/// true without mixing the always-mixed mic; `stale` the mixes of an output buffer rendered from
/// another front than the one mixed, i.e. a re-anchor that was not re-rendered). The probes call
/// the guard directly at its exact-sample edges and past its locked re-check.
const EXPECTED_TRACE: &[&str] = &[
    "== hidden-ndi-jumps",
    "L300 genlock audio buffering floor (issue 1367): total audio buffering is now 85 milliseconds from the first audio tick, dynamically increasing above",
    "L200 buffering-guard: 'NDI test' is not mixed: its audio ran 224.7 ms behind the mix window; re-anchored (dropped 30.0 ms, timeline restarted) instead of adding 234 ms to the whole mix's 85 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=30.0 ms)",
    "L200 buffering-guard: 'NDI test' is not mixed: its audio ran 324.7 ms behind the mix window; re-anchored (dropped 30.0 ms, timeline restarted) instead of adding 341 ms to the whole mix's 85 ms of audio buffering (issue 1381; events=2, +1 since the last line, dropped_total=60.0 ms)",
    "L200 buffering-guard: 'NDI test' is not mixed: its audio ran 621.3 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 640 ms to the whole mix's 85 ms of audio buffering (issue 1381; events=3, +1 since the last line, dropped_total=80.0 ms)",
    "L200 buffering-guard: 'NDI test' is not mixed: its audio ran 1418.0 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 874 ms to the whole mix's 85 ms of audio buffering (issue 1381; events=4, +1 since the last line, dropped_total=100.0 ms)",
    "hidden-ndi-jumps: waits=4 total_ms=85 mic_missed=0 'NDI test' events=4 dropped_ms=100.0 mixed=0 stale=0 guard_lines=4 above_lines=0 restart_lines=0",
    "== hidden-direct-late",
    "L200 buffering-guard: 'Cam audio' is not mixed: its audio ran 300.0 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 320 ms to the whole mix's 0 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=20.0 ms)",
    "L300 genlock audio buffering floor (issue 1367): total audio buffering is now 85 milliseconds from the first audio tick, dynamically increasing above",
    "L200 buffering-guard: 'Cam audio' is not mixed: its audio ran 220.0 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 234 ms to the whole mix's 85 ms of audio buffering (issue 1381; events=470, +469 since the last line, dropped_total=10020.0 ms)",
    "L200 buffering-guard: 'Cam audio' is not mixed: its audio ran 215.3 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 234 ms to the whole mix's 85 ms of audio buffering (issue 1381; events=939, +469 since the last line, dropped_total=20030.0 ms)",
    "hidden-direct-late: waits=4 total_ms=85 mic_missed=0 'Cam audio' events=1400 dropped_ms=29860.0 mixed=0 stale=0 guard_lines=3 above_lines=0 restart_lines=0",
    "== hidden-scene",
    "L300 genlock audio buffering floor (issue 1367): total audio buffering is now 85 milliseconds from the first audio tick, dynamically increasing above",
    "hidden-scene: waits=4 total_ms=85 mic_missed=0 'Scene NDI' events=0 dropped_ms=0.0 mixed=0 stale=0 guard_lines=0 above_lines=0 restart_lines=0",
    "== mixed-late",
    "L300 genlock audio buffering floor (issue 1367): total audio buffering is now 85 milliseconds from the first audio tick, dynamically increasing above",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 85 milliseconds of audio buffering, total audio buffering is now 170 milliseconds (source: mbc); ASRC level band BROKEN: buffering + 9 ms base lands -79.7 ms off the 100 ms level target (reach +/-35 ms) -- a mixed source on an absolute ASRC level target (#1335/#1355, e.g. the stream mbc) cannot reach it at this buffering",
    "mixed-late: waits=8 total_ms=170 mic_missed=0 'mbc' events=0 dropped_ms=0.0 mixed=292 stale=0 guard_lines=0 above_lines=1 restart_lines=0",
    "== cut-in-backlog",
    "L300 genlock audio buffering floor (issue 1367): total audio buffering is now 85 milliseconds from the first audio tick, dynamically increasing above",
    "L200 buffering-guard: 'NDI cut' entered the mix late: its audio ran 193.3 ms behind the mix window; re-anchored (dropped 193.4 ms) instead of adding 213 ms to the whole mix's 85 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=193.4 ms)",
    "cut-in-backlog: waits=4 total_ms=85 mic_missed=0 'NDI cut' events=1 dropped_ms=193.4 mixed=200 stale=0 guard_lines=1 above_lines=0 restart_lines=0",
    "== cut-in-jump",
    "L300 genlock audio buffering floor (issue 1367): total audio buffering is now 85 milliseconds from the first audio tick, dynamically increasing above",
    "L200 buffering-guard: 'NDI jump' entered the mix late: its audio ran 218.0 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 234 ms to the whole mix's 85 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=20.0 ms)",
    "cut-in-jump: waits=4 total_ms=85 mic_missed=0 'NDI jump' events=1 dropped_ms=20.0 mixed=195 stale=0 guard_lines=1 above_lines=0 restart_lines=0",
    "== cut-in-direct-limit",
    "L200 buffering-guard: 'Media late' is not mixed: its audio ran 300.0 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 320 ms to the whole mix's 0 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=20.0 ms)",
    "L300 genlock audio buffering floor (issue 1367): total audio buffering is now 85 milliseconds from the first audio tick, dynamically increasing above",
    "L200 buffering-guard: 'Media late' entered the mix late: its audio ran 218.0 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 234 ms to the whole mix's 85 ms of audio buffering (issue 1381; events=101, +100 since the last line, dropped_total=2150.0 ms)",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 256 milliseconds of audio buffering, total audio buffering is now 341 milliseconds (source: Media late); ASRC level band BROKEN: buffering + 9 ms base lands -250.3 ms off the 100 ms level target (reach +/-35 ms) -- a mixed source on an absolute ASRC level target (#1335/#1355, e.g. the stream mbc) cannot reach it at this buffering",
    "cut-in-direct-limit: waits=16 total_ms=341 mic_missed=0 'Media late' events=101 dropped_ms=2150.0 mixed=186 stale=0 guard_lines=2 above_lines=1 restart_lines=0",
    "== launch-late",
    "L300 genlock audio buffering floor (issue 1367): total audio buffering is now 85 milliseconds from the first audio tick, dynamically increasing above",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 42 milliseconds of audio buffering, total audio buffering is now 128 milliseconds (source: ASIO); ASRC level band BROKEN: buffering + 9 ms base lands -37.0 ms off the 100 ms level target (reach +/-35 ms) -- a mixed source on an absolute ASRC level target (#1335/#1355, e.g. the stream mbc) cannot reach it at this buffering",
    "launch-late: waits=6 total_ms=128 mic_missed=0 'ASIO' events=0 dropped_ms=0.0 mixed=294 stale=0 guard_lines=0 above_lines=1 restart_lines=0",
    "== probes",
    "probe 1ns-behind-is-rounding: reason=0 in_sync=-1 ts=start-1ns left=960 pending=0 timing_set=1 events=0",
    "L200 buffering-guard: 'probe' is not mixed: its audio ran 0.0 ms behind the mix window; re-anchored (dropped 0.0 ms) instead of adding 0 ms to the whole mix's 0 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=0.0 ms)",
    "probe 2ns-behind: reason=1 in_sync=1 ts=start+20831ns left=959 pending=0 timing_set=1 events=1",
    "L200 buffering-guard: 'probe' is not mixed: its audio ran 10.0 ms behind the mix window; re-anchored (dropped 10.0 ms) instead of adding 21 ms to the whole mix's 0 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=10.0 ms)",
    "probe 10ms-exact: reason=1 in_sync=1 ts=start+0ns left=480 pending=0 timing_set=1 events=1",
    "L200 buffering-guard: 'probe' is not mixed: its audio ran 0.0 ms behind the mix window; re-anchored (dropped 0.0 ms) instead of adding 21 ms to the whole mix's 0 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=0.0 ms)",
    "probe rounding-adjust: reason=1 in_sync=1 ts=start+0ns left=959 pending=0 timing_set=1 events=1",
    "L200 buffering-guard: 'probe' is not mixed: its audio ran 30.0 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 42 ms to the whole mix's 0 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=20.0 ms)",
    "probe exhausted: reason=1 in_sync=0 ts=restarted left=0 pending=1 timing_set=0 events=1",
    "probe moved-on-before-the-lock: reason=0 in_sync=0 ts=start-1ns left=960 pending=0 timing_set=1 events=0",
    "probe reset-before-the-lock: reason=0 in_sync=0 ts=restarted left=960 pending=0 timing_set=1 events=0",
];

#[test]
fn a_source_that_is_not_mixed_never_grows_the_mix_buffering_1381() {
    let trace = c_trace();
    for scenario in ["hidden-ndi-jumps", "hidden-direct-late", "hidden-scene"] {
        let s = summary(trace, scenario);
        assert!(
            s.contains(" waits=4 total_ms=85 mic_missed=0 ") && s.contains(" above_lines=0 "),
            "issue 1381: a source that is not mixed moved the mix window ({scenario}):\n{s}\n\n{}",
            section(trace, scenario).join("\n")
        );
    }
    // A composite (an off-program scene reporting its late child) is never re-anchored itself.
    assert!(summary(trace, "hidden-scene").contains(" events=0 "));
}

#[test]
fn a_mixed_late_source_keeps_the_upstream_increase_1381() {
    let trace = c_trace();
    for scenario in ["mixed-late", "launch-late"] {
        let s = summary(trace, scenario);
        assert!(
            s.contains(" above_lines=1 ") && s.contains(" events=0 "),
            "issue 1381: a MIXED late source must keep OBS's own dynamic increase ({scenario}):\n{s}"
        );
    }
}

#[test]
fn a_cut_into_the_mix_is_re_anchored_without_a_hole_1381() {
    let trace = c_trace();
    for scenario in ["cut-in-backlog", "cut-in-jump"] {
        let s = summary(trace, scenario);
        assert!(
            s.contains(" waits=4 total_ms=85 mic_missed=0 ")
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
                && (l.contains("' is not mixed: ") || l.contains("' entered the mix late: "))
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
    ];
    for w in want {
        assert!(
            probes.iter().any(|l| l.starts_with(w)),
            "issue 1381: no probe line starting `{w}`:\n{}",
            probes.join("\n")
        );
    }
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
