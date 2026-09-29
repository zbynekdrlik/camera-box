//! Issue 1381 (design 5900385541) — the RELABEL through the shipped ingest: a lift-and-compile of the
//! place-vs-append branch of `source_output_audio_data` (`vendor/obs-studio/libobs/obs-source.c`).
//!
//! A sender that follows the genlock sender contract relabels its stamps by N whole slots at a wall
//! step, within one interval, while its samples stay continuous. Stock OBS turns that into a loss:
//! a stamp jump of 70 ms or more fails `TS_SMOOTHING_THRESHOLD` and the packet is PLACED r early
//! (r ms of queued audio overwritten), and a jump over 2 s runs `handle_ts_jump`, which drops the
//! whole queued buffer. The relabel decision (`src/genlock_audio_pairing.rs` `audio_relabel`, its C
//! port parity-gated by `tests/genlock_audio_step_hold_parity_1381.rs`) continues the timeline
//! instead, so the packet APPENDS.
//!
//! This gate lifts the shipped bytes VERBATIM (the relabel decision, the raw-domain TS smoothing with
//! its reset, the system-domain push-back check with its second reset, `reset_audio_timing` /
//! `reset_audio_data` / `handle_ts_jump`, `uint64_diff`, `conv_frames_to_time`, the skew-hold and
//! relabel wrappers and the whole pure audio-pairing block) into `tests/c/`'s stub harness, compiles
//! it under `-Wall -Wextra -Wformat=2 -Werror`, drives relabels (joint and split, +260 ms, +682 ms,
//! -1.5 s, +2.5 s), a catch-up sender, a stamp leap and a sender restart, and compares the trace with
//! its truth table. The `genlock-audio-step-hold` line each relabel prints is lifted too and checked
//! value for value (review round 1: the live acceptance reads its step, residual and counters). It
//! FAILS LOUDLY when no C compiler is present.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

mod genlock_audio_pairing_lift;
use genlock_audio_pairing_lift::lift_block;

const OBS_SOURCE: &str = "vendor/obs-studio/libobs/obs-source.c";
const OBS_INTERNAL: &str = "vendor/obs-studio/libobs/obs-internal.h";
const HARNESS: &str = "tests/c/genlock_audio_relabel_ingest_1381_harness.c";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn read(rel: &str) -> String {
    let path = repo(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The text of `src` from `start` up to (not including) `end`; `start` must occur exactly once.
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

/// One function of `src`, from its signature through its closing `\n}\n`.
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

/// The harness with the shipped code substituted in.
fn c_harness() -> String {
    let src = read(OBS_SOURCE);
    let internal = read(OBS_INTERNAL);
    let max_ts_var = internal
        .lines()
        .find(|l| l.starts_with("#define MAX_TS_VAR "))
        .expect("issue 1381: obs-internal.h no longer defines MAX_TS_VAR")
        .to_string();
    let functions = [
        lift_fn(&src, "static inline uint64_t conv_frames_to_time("),
        slice_between(
            &src,
            "/* time threshold in nanoseconds to ensure audio timing is as seamless as",
            "static void source_signal_audio_data(",
        ),
        lift_fn(&src, "static inline uint64_t uint64_diff("),
        lift_fn(&src, "static int genlock_audio_step_hold_source("),
        lift_fn(&src, "static bool genlock_audio_relabel_source("),
        lift_fn(&src, "static void genlock_audio_step_log("),
    ]
    .join("\n");
    let branch = slice_between(
        &src,
        "\t/* camera-box issue 1381 (design 5900385541): a RELABEL",
        "\tsync_offset = source->sync_offset;",
    );
    // the lifted branch must be the whole place-vs-append decision
    for needle in [
        "handle_ts_jump(source, source->next_audio_ts_min, in.timestamp, diff, os_time);",
        "} else if (diff < TS_SMOOTHING_THRESHOLD) {",
        "if (source->next_audio_sys_ts_min == in.timestamp) {",
        "reset_audio_timing(source, data->timestamp, os_time);",
    ] {
        assert!(
            branch.contains(needle),
            "issue 1381: the lifted ingest branch no longer contains `{needle}`"
        );
    }
    let template = read(HARNESS);
    for marker in [
        "@LIFTED_MAX_TS_VAR@",
        "@LIFTED_BLOCK@",
        "@LIFTED_FUNCTIONS@",
        "@INGEST_BRANCH@",
    ] {
        assert_eq!(
            template.matches(marker).count(),
            1,
            "issue 1381: {HARNESS} must carry {marker} exactly once"
        );
    }
    template
        .replace("@LIFTED_MAX_TS_VAR@", &max_ts_var)
        .replace("@LIFTED_BLOCK@", &lift_block())
        .replace("@LIFTED_FUNCTIONS@", &functions)
        .replace("@INGEST_BRANCH@", &branch)
}

fn c_trace() -> Vec<String> {
    let base = option_env!("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!(
        "genlock_audio_relabel_ingest_1381_{}",
        std::process::id()
    ));
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
                 shipped audio ingest C; it must FAIL rather than skip. Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "issue 1381: the lifted obs-source.c ingest branch does NOT COMPILE standalone under -Wall \
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

/// One traced packet: relabel, push-back after the branch, a timeline reset, a dropped buffer, the
/// raw-domain timeline continuing from this packet's own stamp, the skew hold's release and whether
/// it holds, and the cumulative relabel count.
fn line(what: &str, f: [u8; 8]) -> String {
    let [relabel, push, reset, dropped, on_raw, release, active, relabels] = f;
    format!(
        "{what} relabel={relabel} push={push} reset={reset} dropped={dropped} on_raw={on_raw} \
         release={release} active={active} relabels={relabels}"
    )
}

/// The `genlock-audio-step-hold` line a packet printed.
fn log(src: &str, fields: &str) -> String {
    format!("log genlock-audio-step-hold '{src}': {fields} (issue 1381)")
}

/// The decided behaviour, packet by packet.
fn truth_table() -> Vec<String> {
    let mut t = Vec::new();
    // a relabel on the receiver's step packet: appended, the timeline continued from the relabelled
    // stamp (also under the 70 ms smoothing: joint_40ms, one slot), no reset, no hold. Its line
    // carries the wall step and the landing move -r = N slots - S (released=none, nothing held).
    for (name, step, residual) in [
        ("joint_40ms", "+40.000", "-6.7"),
        ("joint_260ms", "+260.000", "-26.7"),
        ("joint_682ms", "+682.474", "-15.8"),
        ("joint_back_1500ms", "-1500.000", "+0.0"),
        ("joint_2500ms", "+2500.000", "+0.0"),
    ] {
        t.push(format!("== {name}"));
        t.push(line("relabel", [1, 1, 0, 0, 1, 0, 0, 1]));
        t.push(log(
            name,
            &format!(
                "step_ms={step} held_ms=0.0 released=none residual_ms={residual} holds=0 relabels=1"
            ),
        ));
        t.push(line("after", [0, 1, 0, 0, 1, 0, 0, 1]));
    }
    // the split shape: the hold starts on the step packet, the relabel appends and releases it
    // FOLLOWED (1) after one packet, with the landing move -r as its residual
    t.push("== split_682ms".to_string());
    t.push(line("step", [0, 1, 0, 0, 1, 0, 1, 0]));
    t.push(line("relabel", [1, 1, 0, 0, 1, 1, 0, 1]));
    t.push(log(
        "split_682ms",
        "step_ms=+682.474 held_ms=33.3 released=followed residual_ms=-15.8 holds=1 relabels=1",
    ));
    t.push(line("after", [0, 1, 0, 0, 1, 0, 0, 1]));
    // a catch-up sender: today's path (continuous stamps snap and append, the hold runs)
    t.push("== catchup_682ms".to_string());
    t.push(line("step", [0, 1, 0, 0, 1, 0, 1, 0]));
    for _ in 0..3 {
        t.push(line("burst", [0, 1, 0, 0, 1, 0, 1, 0]));
    }
    // a stamp leap without a wall step: placed at its stamp (push 0), as today
    t.push("== leap_80ms".to_string());
    t.push(line("leap", [0, 0, 0, 0, 1, 0, 0, 0]));
    // a sender restart without a wall step: handle_ts_jump resets and drops the buffer, as today
    t.push("== restart_3000ms".to_string());
    t.push(line("restart", [0, 1, 1, 1, 1, 0, 0, 0]));
    // the stock "exceeded TS_SMOOTHING_THRESHOLD" and "jumped" debug lines: the leap and the restart
    // only -- no relabel reached the stock >= 70 ms or > 2 s path
    t.push("debug_lines=2".to_string());
    t
}

#[test]
fn the_shipped_ingest_appends_a_relabel_and_keeps_every_other_path_1381() {
    let trace = c_trace();
    assert!(
        !trace.iter().any(|l| l.starts_with("STEADY-BROKEN")),
        "issue 1381: a steady packet no longer appends:\n{}",
        trace.join("\n")
    );
    let want = truth_table();
    for (i, (got, exp)) in trace.iter().zip(&want).enumerate() {
        assert_eq!(
            got,
            exp,
            "issue 1381: the lifted ingest diverges from the decided behaviour at trace line {i}:\n{}",
            trace.join("\n")
        );
    }
    assert_eq!(
        trace.len(),
        want.len(),
        "issue 1381: the lifted ingest printed {} lines, the truth table has {}:\n{}",
        trace.len(),
        want.len(),
        trace.join("\n")
    );
}
