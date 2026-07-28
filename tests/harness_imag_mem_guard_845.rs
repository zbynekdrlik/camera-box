//! #845 -- the [4e/8] imag-nb headroom preflight (#709) demanded `nvidia-smi`, which does not
//! exist on the REPLACEMENT imag notebook (10.77.9.187, #816): Intel UHD Raptor Lake-P / i915
//! only, no discrete GPU. Run 30358343543 aborted every gate run with "GPU free-VRAM query
//! (nvidia-smi) returned an unreadable value", wrongly pointing at a driver that was never
//! installed by design -- and blocked #791/#840/#841 from ever merging behind it.
//!
//! The fix makes the preflight hardware-aware, detecting dGPU presence the SAME way
//! setup-imag.sh/verify-imag.sh already do (`imag_has_discrete_nvidia`, an lspci display-class
//! match, #816) -- never a second detector:
//!   - discrete NVIDIA present -> the ORIGINAL nvidia-smi free-VRAM check, byte-for-byte
//!     unchanged (see tests/harness_imag_gpu_guard.rs, untouched by this ticket).
//!   - no discrete GPU -> the genuinely meaningful equivalent on an integrated GPU: there is no
//!     separate VRAM pool (UMA -- the iGPU draws render/encode buffers from SYSTEM memory), so
//!     `/proc/meminfo`'s MemAvailable is the real headroom figure. Live-confirmed on .187
//!     (2026-07-28): no per-GPU memory accounting exists under /sys/class/drm/card1/ at all --
//!     only clock-scaling `gt_*_freq_mhz` files -- so this is NOT an analogy-invented metric.
//!
//! Also covers the #833 tool-preflight rule: a missing `lspci` on the remote box must fail loud
//! BY NAME, never be silently misread as "no discrete GPU" (the exact measured-zero bug class).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    let p = manifest_dir().join("scripts/lib/imag-gpu-guard.sh");
    assert!(p.exists(), "{} not found", p.display());
    p
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const RECORDING_E2E: &str = "scripts/recording-e2e.sh";

fn run_sourced(body: &str) -> std::process::Output {
    Command::new("bash")
        .arg("-c")
        .arg(format!(
            "set -uo pipefail; . '{}'; {body}",
            lib_script().display()
        ))
        .output()
        .expect("failed to run bash harness")
}

// ---------------------------------------------------------------------------------------------
// 1. imag_mem_query_cmd -- the remote MemAvailable query text
// ---------------------------------------------------------------------------------------------

#[test]
fn imag_mem_query_cmd_reads_proc_meminfo_memavailable() {
    let out = run_sourced("imag_mem_query_cmd");
    assert!(out.status.success());
    let cmd = String::from_utf8_lossy(&out.stdout);
    assert!(
        cmd.contains("/proc/meminfo") && cmd.contains("MemAvailable"),
        "#845: imag_mem_query_cmd must read MemAvailable from /proc/meminfo (the UMA-correct \
         headroom figure on an integrated GPU), got: {cmd}"
    );
    assert!(
        !cmd.contains("nvidia-smi"),
        "#845: the no-dGPU query must never shell out to nvidia-smi: {cmd}"
    );
}

#[test]
fn imag_mem_query_cmd_actually_runs_and_prints_a_bare_integer() {
    // Prove the generated command is real, executable bash -- not just text that looks right.
    let cmd_out = run_sourced("imag_mem_query_cmd");
    let cmd = String::from_utf8_lossy(&cmd_out.stdout).trim().to_string();
    let out = Command::new("bash")
        .arg("-c")
        .arg(&cmd)
        .output()
        .unwrap_or_else(|e| panic!("failed to execute generated command {cmd:?}: {e}"));
    assert!(out.status.success(), "generated command must exit 0: {cmd}");
    let printed = String::from_utf8_lossy(&out.stdout);
    let trimmed = printed.trim();
    assert!(
        !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_digit()),
        "generated command must print a bare integer MiB value on THIS dev box's own \
         /proc/meminfo, got: {printed:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// 2. imag_mem_available_mib_from_query -- pure parser, mirrors imag_gpu_free_mib_from_query
// ---------------------------------------------------------------------------------------------

#[test]
fn imag_mem_available_mib_from_query_parses_a_clean_integer_line() {
    let out = run_sourced("imag_mem_available_mib_from_query '5537'");
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "5537");
}

#[test]
fn imag_mem_available_mib_from_query_strips_whitespace_and_trailing_newline() {
    let out = run_sourced("imag_mem_available_mib_from_query $'  1411  \\n'");
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "1411");
}

#[test]
fn imag_mem_available_mib_from_query_fails_loud_on_unparseable_output() {
    for bad in ["", "N/A", "awk: cannot open /proc/meminfo", "command not found"] {
        let out = run_sourced(&format!("imag_mem_available_mib_from_query '{bad}'"));
        assert!(
            !out.status.success(),
            "#845: imag_mem_available_mib_from_query must FAIL on unparseable input {bad:?}, \
             never guess a value"
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).trim().is_empty(),
            "#845: unparseable input {bad:?} must print nothing"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// 3. imag_mem_headroom_ok -- pure integer compare, its own function (not a GPU-named reuse)
// ---------------------------------------------------------------------------------------------

#[test]
fn imag_mem_headroom_ok_true_when_available_exceeds_min() {
    let out = run_sourced("imag_mem_headroom_ok 5537 1500");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "true");
}

#[test]
fn imag_mem_headroom_ok_false_when_available_is_below_min() {
    let out = run_sourced("imag_mem_headroom_ok 1200 1500");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "false");
}

#[test]
fn imag_mem_headroom_ok_boundary_equal_is_ok() {
    let out = run_sourced("imag_mem_headroom_ok 1500 1500");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "true");
}

#[test]
fn imag_mem_headroom_ok_one_below_boundary_fails() {
    let out = run_sourced("imag_mem_headroom_ok 1499 1500");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "false");
}

// ---------------------------------------------------------------------------------------------
// 4. imag_mem_preflight_message / imag_mem_unreadable_message -- name the REAL condition
// ---------------------------------------------------------------------------------------------

#[test]
fn imag_mem_preflight_message_names_no_dgpu_and_the_figures_never_a_driver() {
    let out = run_sourced("imag_mem_preflight_message 1200 1500");
    assert!(out.status.success());
    let msg = String::from_utf8_lossy(&out.stdout);
    assert!(msg.contains("1200"), "must include the observed available MiB: {msg}");
    assert!(msg.contains("1500"), "must include the required floor: {msg}");
    assert!(
        msg.to_lowercase().contains("no discrete gpu") || msg.to_lowercase().contains("igpu"),
        "#845: must name the ACTUAL condition (no discrete GPU / iGPU), not blame a driver: {msg}"
    );
    assert!(
        !msg.to_lowercase().contains("nvidia driver"),
        "#845: must never send the reader to check an NVIDIA driver that does not exist on this \
         box: {msg}"
    );
}

#[test]
fn imag_mem_unreadable_message_names_meminfo_never_nvidia_driver() {
    let out = run_sourced("imag_mem_unreadable_message");
    assert!(out.status.success());
    let msg = String::from_utf8_lossy(&out.stdout).to_lowercase();
    assert!(
        msg.contains("/proc/meminfo") || msg.contains("meminfo"),
        "must name /proc/meminfo: {msg}"
    );
    assert!(
        !msg.contains("nvidia driver"),
        "#845: the original message says 'check nvidia-smi / the NVIDIA driver' -- the no-dGPU \
         variant must NEVER repeat that misdirection (there is no NVIDIA driver on this box): \
         {msg}"
    );
}

// ---------------------------------------------------------------------------------------------
// 5. recording-e2e.sh wiring -- hardware branch, reusing imag_has_discrete_nvidia, before
//    [5/8] StartRecord, with a preflighted lspci tool check (#833 class)
// ---------------------------------------------------------------------------------------------

#[test]
fn recording_e2e_sources_setup_imag_for_the_shared_dgpu_detector() {
    let s = read(RECORDING_E2E);
    assert!(
        s.contains(". \"$HERE/setup-imag.sh\""),
        "#845: recording-e2e.sh must source scripts/setup-imag.sh to reuse \
         imag_has_discrete_nvidia -- the SAME detector setup-imag.sh/verify-imag.sh already use, \
         never a second one"
    );
}

#[test]
fn recording_e2e_branches_on_imag_has_discrete_nvidia() {
    let s = read(RECORDING_E2E);
    assert!(
        s.contains("imag_has_discrete_nvidia"),
        "#845: the [4e/8] preflight must call imag_has_discrete_nvidia to pick its variant"
    );
}

#[test]
fn recording_e2e_preflights_lspci_before_trusting_its_absence_as_no_dgpu() {
    let s = read(RECORDING_E2E);
    let tool_check_idx = s
        .find("imag_require_remote_tool_cmd lspci")
        .expect("#833/#845: lspci must be preflighted via imag_require_remote_tool_cmd -- a \
                 missing lspci must never be silently read as 'no discrete GPU'");
    let dgpu_branch_idx = s
        .find("imag_has_discrete_nvidia")
        .expect("the dGPU branch decision must exist");
    assert!(
        tool_check_idx < dgpu_branch_idx,
        "#833: the lspci tool-presence check must run BEFORE the code that trusts lspci's \
         output to decide dGPU presence"
    );
    let block = &s[tool_check_idx..(tool_check_idx + 600).min(s.len())];
    assert!(
        block.contains("exit 1"),
        "#833: a missing lspci must hard-fail the preflight, never silently proceed as if no \
         dGPU were present: {block}"
    );
    assert!(
        block.contains("apt-get install") && block.contains("pciutils"),
        "#833: the failure message must name the missing tool and how to install it: {block}"
    );
}

#[test]
fn recording_e2e_both_headroom_branches_run_before_startrecord() {
    let s = read(RECORDING_E2E);
    let start_record_idx = s
        .find("[5/8] StartRecord")
        .expect("recording-e2e.sh must still have the [5/8] StartRecord step");

    let gpu_idx = s
        .find("imag_gpu_headroom_ok")
        .expect("#709: the dGPU branch must still call imag_gpu_headroom_ok");
    assert!(
        gpu_idx < start_record_idx,
        "#709: the dGPU headroom check must run BEFORE [5/8] StartRecord"
    );

    let mem_idx = s
        .find("imag_mem_headroom_ok")
        .expect("#845: the no-dGPU branch must call imag_mem_headroom_ok");
    assert!(
        mem_idx < start_record_idx,
        "#845: the no-dGPU headroom check must run BEFORE [5/8] StartRecord"
    );
}

#[test]
fn recording_e2e_mem_preflight_exits_nonzero_on_low_headroom() {
    let s = read(RECORDING_E2E);
    let idx = s
        .find("imag_mem_headroom_ok \"$IMAG_MEM_AVAILABLE_MIB\"")
        .expect("#845: recording-e2e.sh must check headroom against the parsed available-MiB value");
    let window = &s[idx..(idx + 400).min(s.len())];
    assert!(
        window.contains("exit 1"),
        "#845: a failed mem headroom check must exit 1 (fail fast, before StartRecord), got \
         window: {window}"
    );
}

#[test]
fn recording_e2e_mem_min_available_env_override_is_documented() {
    let s = read(RECORDING_E2E);
    assert!(
        s.contains("IMAG_MEM_MIN_AVAILABLE_MIB"),
        "#845: the system-RAM headroom floor must be an overridable env var (matches the \
         sibling IMAG_GPU_MIN_FREE_MIB / *_GATE_WINDOW_S convention elsewhere in this script)"
    );
}

#[test]
fn recording_e2e_gpu_branch_block_is_byte_for_byte_unchanged() {
    // The original #709 nvidia-smi block's exact wording must survive inside the dGPU branch --
    // proves requirement 1 (a box that still has a dGPU must behave exactly as today).
    let s = read(RECORDING_E2E);
    assert!(
        s.contains("6872MiB") && s.contains("1058MiB") && s.contains("NV_ENC_ERR_OUT_OF_MEMORY"),
        "#845: the dGPU branch's own #709 explanatory comment (with its original live-diagnosed \
         figures) must survive unedited inside the new hardware branch"
    );
    assert!(
        s.contains("imag_gpu_preflight_message") && s.contains("imag_gpu_unreadable_message"),
        "the dGPU branch must still route through the untouched imag_gpu_preflight_message / \
         imag_gpu_unreadable_message functions"
    );
}
