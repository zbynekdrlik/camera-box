//! Shared by the receiver-side audio-pairing parity gates (`tests/genlock_audio_pairing_parity.rs`,
//! `tests/genlock_audio_step_hold_parity_1381.rs`): lift the contiguous `genlock_audio_*` /
//! `genlock_video_delay_*` block VERBATIM out of the vendored `obs-source.c`, compile it standalone
//! under `-Werror` with a test `main`, and return the printed lines. A directory module
//! (`tests/genlock_audio_pairing_lift/mod.rs`), so cargo does not build it as a test target of its
//! own. Each including test file uses a different subset, hence the module-level `dead_code` allow.
#![allow(dead_code)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;

pub const SRC: &str = "vendor/obs-studio/libobs/obs-source.c";

pub fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// Lift the contiguous block VERBATIM out of obs-source.c: from the `genlock_audio_present_delay_ns`
/// signature through `genlock_audio_decide_health`'s closing brace.
pub fn lift_block() -> String {
    let path = repo(SRC);
    let src = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let start = src
        .find("static inline uint64_t genlock_audio_present_delay_ns(")
        .unwrap_or_else(|| {
            panic!("#1303: {SRC} no longer defines `genlock_audio_present_delay_ns` — the pure audio-pairing block is gone, nothing to check parity against.")
        });
    let last_fn = src
        .find("static inline int genlock_audio_decide_health(")
        .unwrap_or_else(|| panic!("#1303: {SRC} no longer defines `genlock_audio_decide_health`"));
    assert!(
        last_fn > start,
        "#1303: the genlock_audio_* helpers are no longer contiguous in {SRC} — keep the block together or the lift splices unrelated code."
    );
    let end = src[last_fn..]
        .find("\n}\n")
        .map(|i| last_fn + i + 3)
        .expect("#1303: genlock_audio_decide_health has no closing brace");
    let block = src[start..end].to_string();
    for helper in [
        "genlock_video_delay_sample_ns(",
        "genlock_video_delay_smooth_ns(",
        "genlock_video_delay_round_ms(",
        "genlock_video_delay_moved(",
        "genlock_video_delay_track(",
        "genlock_audio_hold_mode(",
        "genlock_audio_hold_ms(",
        "genlock_audio_hold_token(",
        "genlock_audio_needs_live_offset(",
        "genlock_audio_wall_to_mono_ns(",
        "genlock_audio_place_term_ns(",
        "genlock_audio_video_delay_ref_ns(",
        "genlock_audio_pairing_offset_ms(",
        "genlock_video_delay_lock_ms(",
        "genlock_audio_withhold_expired(",
        "genlock_audio_mode_active(",
        "genlock_audio_hold_action(",
        "genlock_audio_level_shift_ns(",
        "genlock_audio_slew_step_ns(",
        "genlock_audio_slew_ppm(",
        "genlock_audio_slew_book_ts_ns(",
        "genlock_audio_placed_slew_fold_ns(",
        "genlock_audio_applied_delay_ns(",
        "genlock_audio_push_back_allowed(",
        "genlock_audio_actual_place_ns(",
        "genlock_audio_place_error_ns(",
        "genlock_audio_place_error_smooth_ns(",
        "genlock_audio_realized_delay_ns(",
        "genlock_audio_intended_raw_ns(",
        "genlock_audio_asrc_timecode(",
        "genlock_audio_asrc_error_ms(",
        "genlock_audio_stamp_mono_ns(",
        "genlock_audio_stamp_interval_s(",
        "genlock_audio_step_release_token(",
        "genlock_audio_stamp_age_ns(",
        "genlock_audio_step_mag_ns(",
        "genlock_audio_step_track_nominal(",
        "genlock_audio_step_unmatched_follow(",
        "genlock_audio_step_remember_unmatched(",
        "genlock_audio_step_hold(",
        "genlock_audio_step_freezes_video(",
        "genlock_audio_step_residual_ns(",
        "genlock_audio_step_release_places(",
    ] {
        assert!(
            block.contains(helper),
            "issue 1367: `{helper}` is no longer inside the contiguous audio-pairing block of {SRC}"
        );
    }
    block
}

pub fn compile(block: &str, main_body: &str, tag: &str) -> PathBuf {
    let mut c = String::from("#include <stdint.h>\n#include <stdbool.h>\n#include <stdio.h>\n");
    c.push_str(block);
    c.push_str("int main(void){\n");
    c.push_str(main_body);
    c.push_str("    return 0;\n}\n");

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("genlock_audio_pairing_parity_1303");
    fs::create_dir_all(&dir).expect("create the parity scratch dir");
    let cfile = dir.join(format!("{tag}.c"));
    let bin = dir.join(format!("{tag}.bin"));
    fs::write(&cfile, &c).expect("write the parity harness");

    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=gnu99",
            "-Wall",
            "-Wextra",
            "-Wconversion",
            "-Wformat=2",
            "-Werror",
            "-O1",
        ])
        .arg(&cfile)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "#1303: could not run the C compiler `{cc}` ({e}). This gate compiles the vendored \
                 audio-pairing helpers to prove the C and the Rust authority agree numerically; it \
                 must FAIL rather than skip when the toolchain is absent. Install a C compiler or \
                 set CC."
            )
        });
    assert!(
        out.status.success(),
        "#1303: the audio-pairing helpers lifted from {SRC} do NOT COMPILE standalone under \
         -Wall -Wextra -Wconversion -Wformat=2 -Werror. libobs is otherwise built only by the \
         genlock workflows, so this is very likely a real compile error heading for CI:\n--- cc \
         stderr ---\n{}\n--- harness ---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );
    bin
}

pub fn run_lines(bin: &PathBuf) -> Vec<String> {
    let run = Command::new(bin)
        .output()
        .expect("#1303: the compiled parity harness failed to execute");
    assert!(
        run.status.success(),
        "#1303: the parity harness exited non-zero: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    String::from_utf8(run.stdout)
        .expect("harness stdout is utf-8")
        .lines()
        .map(|s| s.to_string())
        .collect()
}

pub fn run_c(body: &str, tag: &str) -> Vec<String> {
    run_lines(&compile(&lift_block(), body, tag))
}

/// A C literal for an `int64_t` (`INT64_MIN` has no literal form).
pub fn i64_lit(v: i64) -> String {
    if v == i64::MIN {
        "INT64_MIN".to_string()
    } else {
        format!("{v}ll")
    }
}
