//! Regression guard for `scripts/lib/imag-gpu-guard.sh` (#709 prevention) — the imag-nb GPU VRAM
//! headroom preflight, and its wiring into `scripts/recording-e2e.sh` BEFORE `[5/8] StartRecord`.
//!
//! ## The bug (live incident, 2026-07-12)
//!
//! imag-nb's OBS process had run 5 days without a restart; its render pipeline (6 continuous NDI
//! genlock feeds + the built-in Multiview) leaked GPU VRAM up to 6872MiB used out of 8151MiB
//! total (nvidia-smi, live-confirmed), leaving only ~1058MiB free. `StartRecord` over the
//! WebSocket returned `result:true` (obs-websocket only calls the frontend API — it never
//! verifies the encoder actually initialized), but OBS's OWN log showed the real failure:
//! `[obs-nvenc] init_encoder_h264: nv.nvEncInitializeEncoder(...) failed: 10
//! (NV_ENC_ERR_OUT_OF_MEMORY)`, twice, then "Already in non_texture encoder, can't fall back
//! further!" — the recording silently never started. The pre-existing #627 post-start liveness
//! check correctly caught the dead-on-arrival recording and aborted (working as designed), but
//! gave no hint that the real cause was GPU memory exhaustion. A plain `pkill -9 -x obs` +
//! relaunch (matching setup-imag.sh's own hot-swap relaunch pattern) freed the leak back down to
//! 302MiB used (nvidia-smi, live-confirmed) and fully restored recording — two short live test
//! recordings both wrote real, growing bytes after the restart.
//!
//! ## The fix these tests lock (static read of the shell scripts -- no rig, no ssh, no live OBS)
//!
//! `recording-e2e.sh` now sources `scripts/lib/imag-gpu-guard.sh` and runs a new `[4e/8]`
//! preflight step, BEFORE `[5/8] StartRecord`, that reads imag-nb's current free VRAM via
//! nvidia-smi and FAILS FAST with an actionable #709 message when headroom is below
//! `IMAG_GPU_MIN_FREE_MIB` (default 1500MiB — real margin above the observed ~1058MiB failure
//! point, well below the observed ~7849MiB healthy-post-restart free value).

use std::fs;
use std::process::Command;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

fn lib_path() -> String {
    format!(
        "{}/scripts/lib/imag-gpu-guard.sh",
        env!("CARGO_MANIFEST_DIR")
    )
}

fn run_sourced(body: &str) -> std::process::Output {
    Command::new("bash")
        .arg("-c")
        .arg(format!("set -uo pipefail; . '{}'; {body}", lib_path()))
        .output()
        .expect("failed to run bash harness")
}

// --- pure function: imag_gpu_query_cmd ------------------------------------------------------

#[test]
fn imag_gpu_query_cmd_uses_nvidia_smi_free_mib_csv() {
    let out = run_sourced("imag_gpu_query_cmd");
    assert!(out.status.success());
    let cmd = String::from_utf8_lossy(&out.stdout);
    assert!(
        cmd.contains("nvidia-smi") && cmd.contains("memory.free") && cmd.contains("noheader"),
        "#709: imag_gpu_query_cmd must query nvidia-smi's free-VRAM CSV, got: {cmd}"
    );
}

// --- pure function: imag_gpu_free_mib_from_query --------------------------------------------

#[test]
fn imag_gpu_free_mib_from_query_parses_a_clean_integer_line() {
    let out = run_sourced("imag_gpu_free_mib_from_query '7849'");
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "7849");
}

#[test]
fn imag_gpu_free_mib_from_query_strips_whitespace_and_trailing_newline() {
    let out = run_sourced("imag_gpu_free_mib_from_query $'  302  \\n'");
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "302");
}

#[test]
fn imag_gpu_free_mib_from_query_takes_only_the_first_line() {
    // A driver that ignores `-i 0` could print one line per GPU -- the FIRST line is authoritative.
    let out = run_sourced("imag_gpu_free_mib_from_query $'1058\\n4096\\n'");
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "1058");
}

#[test]
fn imag_gpu_free_mib_from_query_fails_loud_on_unparseable_output() {
    // nvidia-smi missing / erroring must NEVER be silently treated as "plenty free".
    for bad in [
        "",
        "N/A",
        "NVIDIA-SMI has failed because it couldn't communicate with the NVIDIA driver.",
        "command not found",
    ] {
        let out = run_sourced(&format!("imag_gpu_free_mib_from_query '{bad}'"));
        assert!(
            !out.status.success(),
            "#709: imag_gpu_free_mib_from_query must FAIL on unparseable input {bad:?}, never guess a value"
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).trim().is_empty(),
            "#709: unparseable input {bad:?} must print nothing"
        );
    }
}

// --- pure function: imag_gpu_headroom_ok ----------------------------------------------------

#[test]
fn imag_gpu_headroom_ok_true_when_free_exceeds_min() {
    let out = run_sourced("imag_gpu_headroom_ok 7849 1500");
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "true");
}

#[test]
fn imag_gpu_headroom_ok_false_on_the_real_709_failure_numbers() {
    // Live #709 numbers: 8151 total - 7093 used = 1058 free, below the 1500 default floor.
    let out = run_sourced("imag_gpu_headroom_ok 1058 1500");
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "false");
}

#[test]
fn imag_gpu_headroom_ok_true_on_the_real_post_restart_healthy_numbers() {
    // Live #709 fix numbers: 8151 total - 302 used = 7849 free, comfortably above the floor.
    let out = run_sourced("imag_gpu_headroom_ok 7849 1500");
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "true");
}

#[test]
fn imag_gpu_headroom_ok_boundary_equal_is_ok() {
    let out = run_sourced("imag_gpu_headroom_ok 1500 1500");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "true");
}

#[test]
fn imag_gpu_headroom_ok_one_below_boundary_fails() {
    let out = run_sourced("imag_gpu_headroom_ok 1499 1500");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "false");
}

// --- pure function: imag_gpu_preflight_message / imag_gpu_unreadable_message ----------------

#[test]
fn imag_gpu_preflight_message_names_709_and_the_fix() {
    let out = run_sourced("imag_gpu_preflight_message 1058 1500");
    assert!(out.status.success());
    let msg = String::from_utf8_lossy(&out.stdout);
    assert!(
        msg.contains("1058"),
        "must include the observed free MiB: {msg}"
    );
    assert!(
        msg.contains("1500"),
        "must include the required floor: {msg}"
    );
    assert!(msg.contains("#709"), "must reference #709: {msg}");
    assert!(
        msg.to_lowercase().contains("restart"),
        "must name the proven fix (restart OBS): {msg}"
    );
}

#[test]
fn imag_gpu_unreadable_message_names_nvidia_smi() {
    let out = run_sourced("imag_gpu_unreadable_message");
    assert!(out.status.success());
    let msg = String::from_utf8_lossy(&out.stdout).to_lowercase();
    assert!(msg.contains("nvidia-smi"), "must name nvidia-smi: {msg}");
}

// --- structural: recording-e2e.sh wiring, ordered BEFORE [5/8] StartRecord ------------------

#[test]
fn recording_e2e_sources_the_imag_gpu_guard_lib() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains(". \"$HERE/lib/imag-gpu-guard.sh\""),
        "#709: recording-e2e.sh must source scripts/lib/imag-gpu-guard.sh"
    );
}

#[test]
fn recording_e2e_runs_the_gpu_preflight_before_startrecord() {
    let s = read("scripts/recording-e2e.sh");
    let preflight_idx = s
        .find("imag_gpu_headroom_ok")
        .expect("#709: recording-e2e.sh must call imag_gpu_headroom_ok somewhere");
    let start_record_idx = s
        .find("[5/8] StartRecord")
        .expect("recording-e2e.sh must still have the [5/8] StartRecord step");
    assert!(
        preflight_idx < start_record_idx,
        "#709: the GPU headroom preflight must run BEFORE [5/8] StartRecord, not after \
         (preflight at byte {preflight_idx}, StartRecord at byte {start_record_idx})"
    );
}

#[test]
fn recording_e2e_gpu_preflight_exits_nonzero_on_low_headroom() {
    let s = read("scripts/recording-e2e.sh");
    let idx = s
        .find("imag_gpu_headroom_ok \"$IMAG_GPU_FREE_MIB\"")
        .expect("#709: recording-e2e.sh must check headroom against the parsed free-MiB value");
    let window = &s[idx..(idx + 400).min(s.len())];
    assert!(
        window.contains("exit 1"),
        "#709: a failed GPU headroom check must exit 1 (fail fast, before StartRecord), got window: {window}"
    );
}

#[test]
fn recording_e2e_gpu_preflight_env_override_is_documented() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("IMAG_GPU_MIN_FREE_MIB"),
        "#709: the free-VRAM floor must be an overridable env var (matches the sibling \
         *_GATE_WINDOW_S / *_THRESHOLD env-override convention elsewhere in this script)"
    );
}
