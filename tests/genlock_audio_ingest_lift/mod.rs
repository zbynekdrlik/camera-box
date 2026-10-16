//! Shared by the ingest lift gates (`tests/genlock_audio_relabel_ingest_1381.rs`,
//! `tests/genlock_audio_arrival_rebase_1425.rs`): the C harness
//! `tests/c/genlock_audio_relabel_ingest_1381_harness.c` with the shipped `obs-source.c` place-vs-append
//! branch of `source_output_audio_data` substituted in VERBATIM (the relabel decision, the raw-domain TS
//! smoothing with its reset, the system-domain push-back check with its second reset, the issue-1425
//! arrival rebase), the helpers it calls, the pure audio-pairing block, and the relabel remainder
//! booking; plus a compile-and-run helper that FAILS LOUDLY (never skips) when no C compiler is present.
//! A directory module, so cargo does not build it as a test target of its own; the including test also
//! declares `mod genlock_audio_pairing_lift;` (this module lifts the pairing block through it).
#![allow(dead_code)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::genlock_audio_pairing_lift::lift_block;

pub const OBS_SOURCE: &str = "vendor/obs-studio/libobs/obs-source.c";
pub const OBS_INTERNAL: &str = "vendor/obs-studio/libobs/obs-internal.h";
pub const HARNESS: &str = "tests/c/genlock_audio_relabel_ingest_1381_harness.c";

pub fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

pub fn read(rel: &str) -> String {
    let path = repo(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The text of `src` from `start` up to (not including) `end`; `start` must occur exactly once.
pub fn slice_between(src: &str, start: &str, end: &str) -> String {
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

/// One function of `src`, from its signature through its closing `\n}\n`, or None when `src` does not
/// define it; a signature that occurs more than once fails.
pub fn find_fn(src: &str, signature: &str) -> Option<String> {
    let n = src.matches(signature).count();
    assert!(
        n <= 1,
        "issue 1381: `{signature}` must occur at most once ({n} found)"
    );
    let a = src.find(signature)?;
    let b = src[a..]
        .find("\n}\n")
        .unwrap_or_else(|| panic!("issue 1381: `{signature}` does not end"));
    Some(src[a..a + b + 3].to_string())
}

/// One function of `src`, from its signature through its closing `\n}\n`; it must occur exactly once.
pub fn lift_fn(src: &str, signature: &str) -> String {
    find_fn(src, signature).unwrap_or_else(|| panic!("issue 1381: `{signature}` not found"))
}

/// The issue-1381 harness with the shipped code substituted in.
pub fn ingest_harness() -> String {
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
        lift_fn(&src, "static bool genlock_audio_relabel_pending_source("),
        lift_fn(&src, "static void genlock_audio_step_log("),
    ]
    .join("\n");
    // design 5901213031: the relabel remainder booking, verbatim (it runs after the hold in the tail)
    let book = slice_between(
        &src,
        "\t/* camera-box issue 1381 (design 5901213031, ROZHODNUTE on finding 5900705310): a relabel's remainder",
        "\t/* camera-box issue 1381: a hold release beyond one packet",
    );
    assert!(
        book.contains("genlock_audio_relabel_book_ns("),
        "issue 1381: the lifted booking slice no longer books the relabel remainder"
    );
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
        "genlock_audio_relabel_pending_source(",
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
        "@BOOK_SLICE@",
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
        .replace("@BOOK_SLICE@", &book)
}

static RUNS: AtomicUsize = AtomicUsize::new(0);

/// Compile `c_src` under `-Wall -Wextra -Wformat=2 -Werror` (libobs on the include path), run it and
/// return its stdout lines. Each call gets its own scratch dir (pid + a counter, so parallel test
/// threads and nextest's per-test processes never share a binary), removed after the run.
pub fn compile_and_run(c_src: &str, tag: &str) -> Vec<String> {
    let base = option_env!("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!(
        "{tag}_{}_{}",
        std::process::id(),
        RUNS.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&dir).expect("create the lift scratch dir");
    let harness = dir.join("harness.c");
    let bin = dir.join("harness.bin");
    fs::write(&harness, c_src).expect("write the lift harness");
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
                "{tag}: could not run the C compiler `{cc}` ({e}). This gate compiles the \
                 shipped audio ingest C; it must FAIL rather than skip. Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "{tag}: the lifted obs-source.c ingest branch does NOT COMPILE standalone under -Wall \
         -Wextra -Wformat=2 -Werror:\n--- cc stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin)
        .output()
        .unwrap_or_else(|e| panic!("{tag}: the compiled lift harness failed to execute: {e}"));
    assert!(
        run.status.success(),
        "{tag}: the lift harness exited non-zero:\n{}",
        String::from_utf8_lossy(&run.stdout)
    );
    let _ = fs::remove_dir_all(&dir);
    String::from_utf8(run.stdout)
        .expect("harness stdout is utf-8")
        .lines()
        .map(str::to_string)
        .collect()
}
