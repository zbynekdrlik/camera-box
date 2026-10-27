//! Issue 1425 (design 6443440513, ROZHODNUTÉ 6436368009) -- the per-box FIXED audio buffering on the
//! libobs side. std-only: runs offline and under `cargo test`.
//!
//! - The marker READER (`genlock_audio_buffering_read_box_ms` in obs.c) is lifted VERBATIM and
//!   compiled with `cc -Werror -Wconversion -Wformat=2` against the real pure header, then run with
//!   `HOME` pointing at a `tempfile::tempdir()`: absent, valid (with and without the trailing
//!   newline), garbage, out of range, too long, a directory, HOME unset. The file read lives in the
//!   .c, never in the pure header (the header only parses bytes, parity-gated against the Rust).
//! - The wiring: `obs_reset_audio2` reads the marker BEFORE the plan, builds the plan through
//!   `genlock_audio_buffering_make_plan_box`, stores `box_fixed` on the core audio, logs the ONE
//!   FIXED line (or the ONE not-deterministic WARNING); the per-tick freeze takes the plan's
//!   `box_fixed`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const OBS_C: &str = "vendor/obs-studio/libobs/obs.c";
const OBS_AUDIO: &str = "vendor/obs-studio/libobs/obs-audio.c";
const OBS_INTERNAL: &str = "vendor/obs-studio/libobs/obs-internal.h";
const HEADER_DIR: &str = "vendor/obs-studio/libobs";

const READER_SIG: &str = "static int genlock_audio_buffering_read_box_ms(uint32_t *ms_out)";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn read(rel: &str) -> String {
    fs::read_to_string(repo(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The reader, verbatim: its signature up to the first `\n}\n` after it.
fn lift_reader() -> String {
    let s = read(OBS_C);
    assert_eq!(
        s.matches(READER_SIG).count(),
        1,
        "issue 1425: `{READER_SIG}` must be defined exactly once in {OBS_C}"
    );
    let a = s.find(READER_SIG).unwrap();
    let b = s[a..]
        .find("\n}\n")
        .expect("issue 1425: the reader's closing brace is gone")
        + 3;
    s[a..a + b].to_string()
}

fn harness_text() -> String {
    format!(
        r#"#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include "obs-genlock-audio-buffering.h"

/* libobs util/platform.h os_fopen on Linux is fopen. */
#define os_fopen fopen

/* ---- lifted verbatim from obs.c ---- */
{reader}
/* ---- end lift ---- */

int main(void)
{{
	uint32_t ms = 4242u;
	const int state = genlock_audio_buffering_read_box_ms(&ms);
	printf("%d %" PRIu32 "\n", state, ms);
	return 0;
}}
"#,
        reader = lift_reader()
    )
}

/// Compile the lifted reader once into `dir` and return the binary.
fn build(dir: &Path) -> PathBuf {
    let src = dir.join("reader.c");
    let bin = dir.join("reader.bin");
    fs::write(&src, harness_text()).expect("write the reader harness");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=gnu11",
            "-Wall",
            "-Wextra",
            "-Wconversion",
            "-Wformat=2",
            "-Werror",
            "-O1",
        ])
        .arg("-I")
        .arg(repo(HEADER_DIR))
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "issue 1425: could not run the C compiler `{cc}` ({e}); this gate compiles the \
                 shipped marker reader and must FAIL rather than skip"
            )
        });
    assert!(
        out.status.success(),
        "issue 1425: the lifted obs.c marker reader does NOT COMPILE standalone:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    bin
}

/// Run the reader with HOME = `home` (None = HOME unset) and return `(state, ms)`.
fn run(bin: &Path, home: Option<&Path>) -> (i32, u32) {
    let mut cmd = Command::new(bin);
    cmd.env_remove("HOME");
    if let Some(h) = home {
        cmd.env("HOME", h);
    }
    let out = cmd.output().expect("run the reader harness");
    assert!(out.status.success(), "the reader harness exited non-zero");
    let text = String::from_utf8(out.stdout).unwrap();
    let mut it = text.split_whitespace();
    let state = it.next().unwrap().parse().unwrap();
    let ms = it.next().unwrap().parse().unwrap();
    (state, ms)
}

/// A HOME holding `~/.camera-box/genlock-audio-buffering-ms` = `bytes`.
fn home_with(bytes: &[u8]) -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("scratch HOME");
    let d = home.path().join(".camera-box");
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("genlock-audio-buffering-ms"), bytes).unwrap();
    home
}

const ABSENT: i32 = 0;
const INVALID: i32 = 1;
const FIXED: i32 = 2;

#[test]
fn the_marker_reader_decides_absent_invalid_and_fixed_1425() {
    let scratch = tempfile::tempdir().expect("scratch dir");
    let bin = build(scratch.path());

    // Absent: no HOME, an empty HOME, no ~/.camera-box, no file. *ms_out untouched (4242).
    assert_eq!(run(&bin, None), (ABSENT, 4242));
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(run(&bin, Some(empty.path())), (ABSENT, 4242));
    let only_dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(only_dir.path().join(".camera-box")).unwrap();
    assert_eq!(run(&bin, Some(only_dir.path())), (ABSENT, 4242));

    // Valid: what setup-strih writes ("<ms>\n"), and without the newline.
    for (bytes, ms) in [
        (&b"0\n"[..], 0),
        (b"0", 0),
        (b"85\n", 85),
        (b"960\n", 960),
        (b"960", 960),
    ] {
        let h = home_with(bytes);
        assert_eq!(run(&bin, Some(h.path())), (FIXED, ms), "{bytes:?}");
    }

    // Invalid: present but not an integer 0..960 with at most one trailing newline.
    for bytes in [
        &b""[..],
        b"\n",
        b"961\n",
        b"1000",
        b"85\r\n",
        b"85\n\n",
        b" 85",
        b"-1",
        b"abc",
        b"85 ms",
        b"12345678901234567890",
    ] {
        let h = home_with(bytes);
        assert_eq!(run(&bin, Some(h.path())), (INVALID, 4242), "{bytes:?}");
    }

    // A directory where the file belongs reads nothing: invalid, never fixed.
    let dir_home = tempfile::tempdir().unwrap();
    fs::create_dir_all(
        dir_home
            .path()
            .join(".camera-box")
            .join("genlock-audio-buffering-ms"),
    )
    .unwrap();
    assert_eq!(run(&bin, Some(dir_home.path())).0, INVALID);
}

#[test]
fn obs_reset_audio2_reads_the_marker_and_fixes_the_plan_1425() {
    // Squished, with the C string-literal joins removed (a clang-format reflow cannot break it).
    let c = squish(&read(OBS_C)).replace("\" \"", "");
    for needle in [
        "uint32_t genlock_box_ms = 0;",
        "const int genlock_box = genlock_audio_buffering_read_box_ms(&genlock_box_ms);",
        "const struct genlock_audio_buffering_plan genlock_buf = genlock_audio_buffering_make_plan_box( genlock_box == GENLOCK_AUDIO_BUFFERING_BOX_FIXED, genlock_box_ms, oai->max_buffering_ms, oai->fixed_buffering, oai->samples_per_sec, AUDIO_OUTPUT_FRAMES);",
        "audio->genlock_buffering_box_fixed = genlock_buf.box_fixed;",
        "\"genlock-audio-buffering: FIXED %\" PRIu32 \" ms (%\" PRIu32 \" ticks) from ~/.camera-box/genlock-audio-buffering-ms (issue 1425)\"",
        "\"genlock-audio-buffering: NOT deterministic on this box -- %s ~/.camera-box/genlock-audio-buffering-ms (an integer 0..960 ms), so the launch's startup race draws the audio buffering (issue 1425)\"",
    ] {
        assert_eq!(
            c.matches(needle).count(),
            1,
            "issue 1425: {OBS_C} must carry `{needle}` exactly once"
        );
    }
    // The read comes before the plan; the box flag is stored after the plan.
    let read_at = c.find("genlock_audio_buffering_read_box_ms(&genlock_box_ms)").unwrap();
    let plan_at = c.find("genlock_audio_buffering_make_plan_box(").unwrap();
    let store_at = c.find("audio->genlock_buffering_box_fixed = ").unwrap();
    assert!(read_at < plan_at && plan_at < store_at);
    // The FIXED line is INFO, the not-deterministic line a WARNING.
    assert!(c.contains("blog(LOG_INFO, \"genlock-audio-buffering: FIXED"));
    assert!(c.contains("blog(LOG_WARNING, \"genlock-audio-buffering: NOT deterministic"));
    // The shipped-floor plan call is gone from the reset (the box plan wraps it).
    assert!(!c.contains("genlock_buf = genlock_audio_buffering_make_plan("));
}

#[test]
fn the_tick_freeze_takes_the_plans_box_flag_1425() {
    let a = squish(&read(OBS_AUDIO));
    for needle in [
        "audio->genlock_buffering_frozen = genlock_buffering_frozen_now( start_ts_in, obs_output_genlock_audio_encoder_outputs() > 0, audio->genlock_buffering_box_fixed);",
        "static inline bool genlock_buffering_frozen_now(uint64_t tick_start_ns, bool encoder_active, bool box_fixed)",
        "genlock_launch_encoder_unsettled = !box_fixed && encoder_active && !settled;",
        "return genlock_audio_buffering_frozen_plan(box_fixed, encoder_active, settled);",
    ] {
        assert_eq!(
            a.matches(needle).count(),
            1,
            "issue 1425: {OBS_AUDIO} must carry `{needle}` exactly once"
        );
    }
    let i = squish(&read(OBS_INTERNAL));
    assert!(
        i.contains("bool genlock_buffering_box_fixed;"),
        "issue 1425: struct obs_core_audio lost genlock_buffering_box_fixed"
    );
}
