//! Issue 1470 — the dock's measurement-source share against the SHIPPED libobs mix.
//!
//! The dock takes a tick's per-source mix buffer only when `camera-box-measure-share.hpp` judges it
//! the whole tick at offset 0, from what libobs left after the tick: the source's timeline (which
//! `discard_audio` has already moved to the tick end), `audio_pending`, `obs_source_active`. This
//! test lifts `convert_time_to_frames` + `mix_audio` and `discard_if_stopped` + `discard_audio`
//! VERBATIM from `vendor/obs-studio/libobs/obs-audio.c`, compiles them with the real tracker
//! (`tests/c/measure_share_lift_1470_harness.cpp`, a stub libobs with a render and a placement
//! model) and asserts, over a steady source, a mid-tick start, a producer stall, a source out of
//! the program, a lagging timeline and a tick without an output callback, that every tick the dock
//! takes is byte-identical to what the mix carried and a tick it leaves silent never was.
//!
//! The one residual is pinned: a producer-side timeline reset landing between the dock's read and
//! the next mix costs one wrong tick per reset (`reset-race-*`).

use std::path::PathBuf;
use std::process::Command;

const OBS_AUDIO: &str = "vendor/obs-studio/libobs/obs-audio.c";
const HARNESS: &str = "tests/c/measure_share_lift_1470_harness.cpp";

fn manifest(rel: &str) -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), rel].iter().collect()
}

fn read(rel: &str) -> String {
    let p = manifest(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// The text of `src` from `start` up to (not including) `end`, `start` unique.
fn slice_between(src: &str, start: &str, end: &str) -> String {
    assert_eq!(
        src.matches(start).count(),
        1,
        "issue 1470: lift anchor `{start}` must occur exactly once"
    );
    let a = src.find(start).expect("lift anchor");
    let b = src[a..]
        .find(end)
        .unwrap_or_else(|| panic!("issue 1470: lift end `{end}` not found after `{start}`"));
    src[a..a + b].to_string()
}

fn lifted() -> String {
    let s = read(OBS_AUDIO);
    [
        slice_between(
            &s,
            "static inline size_t convert_time_to_frames(",
            "static bool ignore_audio(",
        ),
        slice_between(
            &s,
            "static bool discard_if_stopped(",
            "static inline bool audio_buffering_maxed(",
        ),
    ]
    .join("\n")
}

fn trace() -> String {
    let template = read(HARNESS);
    assert_eq!(
        template.matches("@LIFTED@").count(),
        1,
        "{HARNESS} must carry the lift marker once"
    );
    let lift = lifted();
    for need in [
        "static inline void mix_audio(",
        "static inline void discard_audio(",
    ] {
        assert!(lift.contains(need), "issue 1470: the lift lost `{need}`");
    }
    let code = template.replace("@LIFTED@", &lift);
    let dir = tempfile::tempdir().expect("scratch dir");
    let src = dir.path().join("harness.cpp");
    let bin = dir.path().join("harness");
    std::fs::write(&src, code).expect("write the harness");
    let compile = Command::new("g++")
        .args([
            "-std=c++11",
            "-O1",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-Wno-register",
        ])
        .arg(format!(
            "-I{}",
            manifest("vendor/obs-studio/libobs").display()
        ))
        .arg(format!(
            "-I{}",
            manifest("vendor/av-sync-dock/src").display()
        ))
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn g++ (install build-essential) for the measure-share lift");
    assert!(
        compile.status.success(),
        "the lifted mix_audio / discard_audio + the dock tracker must compile clean:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&bin).output().expect("run the lift harness");
    let out = String::from_utf8_lossy(&run.stdout).to_string();
    assert!(
        run.status.success(),
        "the lift harness failed:\n{out}{}",
        String::from_utf8_lossy(&run.stderr)
    );
    out
}

/// The truth table: scenario -> (ticks, taken, mixed, mismatched, missed, absent_long).
const TABLE: &[(&str, &str)] = &[
    // the first tick has no previous timeline; every later one is taken and equals the mix
    (
        "steady",
        "ticks=40 taken=39 mixed=40 mismatched=0 missed=1 absent_long=0",
    ),
    // the start tick is mixed at an offset and left silent; whole after it
    (
        "mid-tick-start",
        "ticks=20 taken=18 mixed=19 mismatched=0 missed=0 absent_long=0",
    ),
    // two pending ticks are neither mixed nor taken; the catch-up tick is whole again
    (
        "pending",
        "ticks=20 taken=17 mixed=18 mismatched=0 missed=1 absent_long=0",
    ),
    // out of the program: not mixed, not taken; the first tick back is taken
    (
        "inactive",
        "ticks=20 taken=14 mixed=15 mismatched=0 missed=1 absent_long=0",
    ),
    (
        "never-in-program",
        "ticks=60 taken=0 mixed=0 mismatched=0 missed=0 absent_long=1",
    ),
    // a timeline behind the window is neither mixed nor taken
    (
        "lagging",
        "ticks=4 taken=0 mixed=0 mismatched=0 missed=0 absent_long=0",
    ),
    // the tick after a missing callback is silent (its start offset is unknown)
    (
        "skipped-callback",
        "ticks=11 taken=9 mixed=11 mismatched=0 missed=2 absent_long=0",
    ),
    // KNOWN LIMIT: one wrong tick per producer reset between the dock's read and the next mix
    (
        "reset-race-inside",
        "ticks=12 taken=11 mixed=12 mismatched=1 missed=1 absent_long=0",
    ),
    (
        "reset-race-1ns-early",
        "ticks=12 taken=11 mixed=11 mismatched=1 missed=1 absent_long=0",
    ),
    // a restart past the tick end: that tick is not mixed and the end check leaves it silent; the
    // next one is mixed at an offset and left silent (its start was not the tick start)
    (
        "reset-race-past-the-tick",
        "ticks=12 taken=9 mixed=11 mismatched=0 missed=1 absent_long=0",
    ),
];

#[test]
fn every_taken_tick_is_what_the_shipped_mix_carried() {
    let out = trace();
    for (name, want) in TABLE {
        let line = out
            .lines()
            .find(|l| l.split_whitespace().next() == Some(*name))
            .unwrap_or_else(|| panic!("issue 1470: no `{name}` line in the trace:\n{out}"));
        assert_eq!(
            line,
            format!("{name} {want}"),
            "issue 1470: scenario `{name}` moved off its truth table (trace:\n{out})"
        );
    }
    for line in out.lines() {
        let name = line.split_whitespace().next().unwrap_or("");
        if !matches!(name, "reset-race-inside" | "reset-race-1ns-early") {
            assert!(
                line.contains(" mismatched=0 "),
                "issue 1470: the dock took a tick the mix did not carry as it is: {line}"
            );
        }
    }
}
