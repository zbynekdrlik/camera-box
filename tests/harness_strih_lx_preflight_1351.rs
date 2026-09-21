//! issue 1351 — the `[0/8]` Linux-strih preflight TIMEOUT + named-banner hardening.
//!
//! After the M4 cut-over (strih = the Linux notebook strih-lx, 10.77.9.202), the full-path E2E
//! `[0/8]` preflight band could HANG ~23 min silently on a strih-touching call that had no
//! harness-level `timeout`, aborting BEFORE `RUN_ID` is exported (recording-e2e.sh) so the #703
//! fail-closed guard reported "no verdict" with NO named stage. This pins the fix (Approach 1):
//!
//!   - a new sourced lib `scripts/lib/strih-lx-preflight.sh` owns the timeout+named-banner
//!     mechanism as ONE source of truth: `strih_lx_gate_prefix <secs> <host>` (the `timeout <secs>`
//!     command prefix, emitted ONLY on the linux-strih path — empty on Windows so that path stays
//!     behaviorally byte-identical) and `strih_lx_preflight_timeout_banner <rc> <label> <secs>`
//!     (a named `[0/8] strih-lx <label>` banner emitted ONLY on a timeout kill: rc 124/137);
//!   - the genuine unbounded call — the `obs_phase2.py record --action status` WS round-trip inside
//!     `strih_linux_visibility_check` (scripts/lib/strih-platform.sh) — is wrapped in `timeout`;
//!   - `scripts/recording-e2e.sh` computes the prefix once and threads `${STRIH_LX_GATE_PREFIX:-}`
//!     into the two heaviest strih-touching `[0/8]` gate invocations (DanteSync NTP+PTP +
//!     dantesync version-parity), with a linux-only named-banner `|| { … }` on a timeout kill.
//!
//! The Windows path stays behaviorally byte-identical (the prefix is an empty `${VAR:-}` expansion
//! there) and NO existing anchor string is duplicated (no `.find()`/`.split()` collision) — the
//! anchor-count sweep proves it. Design-by main (issue 1351, comment 5760764625), Approach 1.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/strih-lx-preflight.sh");
    assert!(s.exists(), "issue 1351: {} not found", s.display());
    s
}

fn platform_lib() -> PathBuf {
    manifest_dir().join("scripts/lib/strih-platform.sh")
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source strih-platform.sh (for `strih_platform`) THEN strih-lx-preflight.sh, run `body`, return
/// the full Output (so a test can inspect stdout AND stderr — the banner writes to stderr).
fn run_sourced(body: &str) -> Output {
    let harness = format!("set -uo pipefail\n. \"$PLATFORM\"\n. \"$SCRIPT\"\n{body}");
    Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("PLATFORM", platform_lib())
        .env("SCRIPT", lib_script())
        .output()
        .expect("failed to run bash harness")
}

// ================================================================================================
// strih_lx_gate_prefix — the `timeout <secs>` command prefix, linux-strih ONLY.
// ================================================================================================

#[test]
fn gate_prefix_is_timeout_secs_for_the_linux_strih() {
    let out = run_sourced("strih_lx_gate_prefix 180 10.77.9.202");
    assert!(
        out.status.success(),
        "stderr={:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "timeout 180",
        "a linux strih must get a `timeout <secs>` command prefix"
    );
}

#[test]
fn gate_prefix_is_empty_for_a_windows_strih() {
    let out = run_sourced("strih_lx_gate_prefix 180 10.77.9.99");
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "",
        "a non-linux (Windows) strih must get an EMPTY prefix — that path stays byte-identical"
    );
}

#[test]
fn gate_prefix_honours_the_requested_seconds() {
    let out = run_sourced("strih_lx_gate_prefix 90 10.77.9.202");
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "timeout 90");
}

#[test]
fn gate_prefix_respects_the_strih_platform_env_override() {
    // STRIH_PLATFORM=windows must force the empty prefix even for the strih-lx IP (rollback hatch).
    let harness =
        "set -uo pipefail\n. \"$PLATFORM\"\n. \"$SCRIPT\"\nstrih_lx_gate_prefix 180 10.77.9.202";
    let out = Command::new("bash")
        .arg("-c")
        .arg(harness)
        .env("PLATFORM", platform_lib())
        .env("SCRIPT", lib_script())
        .env("STRIH_PLATFORM", "windows")
        .output()
        .expect("run");
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "",
        "STRIH_PLATFORM=windows must force the empty prefix even for the lx IP"
    );
}

// ================================================================================================
// strih_lx_preflight_timeout_banner — named `[0/8] strih-lx <label>` banner, ONLY on a timeout kill.
// ================================================================================================

#[test]
fn banner_names_the_call_and_issue_on_a_timeout_kill() {
    let out = run_sourced("strih_lx_preflight_timeout_banner 124 \"DanteSync NTP+PTP gate\" 180");
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("[0/8] strih-lx") && err.contains("DanteSync NTP+PTP gate") && err.contains("1351"),
        "a timeout kill (rc 124) must emit a named `[0/8] strih-lx <label>` banner citing issue 1351. err={err:?}"
    );
}

#[test]
fn banner_also_fires_on_a_sigkill_137() {
    let out =
        run_sourced("strih_lx_preflight_timeout_banner 137 \"dantesync version-parity gate\" 300");
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("[0/8] strih-lx") && err.contains("dantesync version-parity gate"),
        "a SIGKILL (rc 137) must ALSO emit the named banner (timeout --kill-after path). err={err:?}"
    );
}

#[test]
fn banner_is_silent_for_a_normal_gate_failure() {
    // A genuine gate FAILURE (e.g. clock not locked -> exit 20) is NOT a timeout — the gate printed
    // its own diagnosis, so the banner must stay silent and never misattribute it as a hang.
    let out = run_sourced("strih_lx_preflight_timeout_banner 20 \"DanteSync NTP+PTP gate\" 180");
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "",
        "a normal non-timeout failure (rc 20) must NOT emit the timeout banner"
    );
}

// ================================================================================================
// scripts/lib/strih-platform.sh — the genuine unbounded WS call is now timeout-bounded.
// ================================================================================================

#[test]
fn visibility_ws_roundtrip_is_timeout_bounded_1351() {
    let lib = read("scripts/lib/strih-platform.sh");
    // Byte-match the ACTUAL bounded call, never `.find("obs_phase2.py")` — that literal also occurs
    // in this lib's own doc comments (the #832 self-collision class), so an anchor on the bare name
    // would latch onto a comment line, not the real invocation. The WS status read (the ssh probe
    // above was already `timeout`-bounded; this WS call was the un-bounded strih-touching call the
    // whole issue is about) must be wrapped in `timeout`.
    assert!(
        lib.contains("timeout \"${STRIH_LX_WS_TIMEOUT:-20}\" python3 \"$here/obs_phase2.py\""),
        "the obs_phase2 WS status round-trip in strih_linux_visibility_check must be `timeout`-bounded \
         (issue 1351 — this was the un-timeout-bounded strih call that hung [0/8] ~23 min)."
    );
}

// ================================================================================================
// scripts/recording-e2e.sh wiring — new lines only; the Windows path stays behaviorally identical.
// ================================================================================================

#[test]
fn recording_e2e_sources_the_new_preflight_lib_1351() {
    let body = read("scripts/recording-e2e.sh");
    assert!(
        body.contains("lib/strih-lx-preflight.sh"),
        "recording-e2e.sh must source scripts/lib/strih-lx-preflight.sh"
    );
}

#[test]
fn recording_e2e_computes_the_gate_prefix_via_the_lib_1351() {
    let body = read("scripts/recording-e2e.sh");
    assert!(
        body.contains("STRIH_LX_GATE_PREFIX=\"$(strih_lx_gate_prefix"),
        "recording-e2e.sh must compute STRIH_LX_GATE_PREFIX once via strih_lx_gate_prefix (the lib SSoT)"
    );
}

#[test]
fn dantesync_ntp_gate_is_bounded_on_the_linux_strih_path_1351() {
    let body = read("scripts/recording-e2e.sh");
    let banner = body
        .find("[0/8] DanteSync NTP+PTP gate")
        .expect("the DanteSync NTP+PTP banner must still exist");
    let end = body[banner..]
        .find("# Version-integrity precondition gate")
        .map(|i| banner + i)
        .expect("the version-integrity banner must follow");
    let window = &body[banner..end];
    assert!(
        window.contains("${STRIH_LX_GATE_PREFIX:-}"),
        "the DanteSync NTP+PTP gate invocation must be prefixed with ${{STRIH_LX_GATE_PREFIX:-}} \
         (bounded on the linux strih, empty on Windows). Window:\n{window}"
    );
    assert!(
        window.contains("strih_lx_preflight_timeout_banner"),
        "the DanteSync NTP+PTP gate must emit the named strih-lx banner on a timeout kill. Window:\n{window}"
    );
    // The gate is still invoked with its enforce env + win-http nodes (unchanged argv).
    assert!(
        window.contains("DANTESYNC_GATE_GM_ENFORCE=1")
            && window.contains("dantesync-gate.sh")
            && window.contains("--win-http \"strih=$STRIH\""),
        "the DanteSync gate's env + argv must be unchanged. Window:\n{window}"
    );
}

#[test]
fn dantesync_version_parity_gate_is_bounded_on_the_linux_strih_path_1351() {
    let body = read("scripts/recording-e2e.sh");
    let banner = body
        .find("[0/8] dantesync version-parity gate")
        .expect("the dantesync version-parity banner must still exist");
    let end = body[banner..]
        .find("camera-box binary CROSS-BOX version-parity gate")
        .map(|i| banner + i)
        .expect("the camera-box version-parity comment must follow");
    let window = &body[banner..end];
    assert!(
        window.contains("${STRIH_LX_GATE_PREFIX:-}"),
        "the dantesync version-parity gate invocation must be prefixed with ${{STRIH_LX_GATE_PREFIX:-}}. \
         Window:\n{window}"
    );
    assert!(
        window.contains("strih_lx_preflight_timeout_banner"),
        "the dantesync version-parity gate must emit the named strih-lx banner on a timeout kill. \
         Window:\n{window}"
    );
    assert!(
        window.contains("dantesync-version-gate.sh") && window.contains("--win \"strih="),
        "the dantesync version-parity gate's argv must be unchanged. Window:\n{window}"
    );
}

/// NEGATIVE ANCHOR: the fix must NOT duplicate any existing gate-invocation anchor string — the
/// bound is a `${VAR:-}` prefix, never a copied `if/else` invocation. A 1->2 count here would break
/// the many `.find()`/`.split()` anchor tests on these gates (14/9/8 test files).
#[test]
fn the_bound_never_duplicates_a_gate_invocation_anchor_1351() {
    let body = read("scripts/recording-e2e.sh");
    // These three literals live ONLY in the two gate invocations this fix wraps (the MAIN DanteSync
    // NTP+PTP gate's own stream leg, and the dantesync version-parity gate) — each is count-1 today.
    // An `if [platform]; then WRAP; else ORIGINAL; fi` duplication (the anchor-hostile shape this fix
    // deliberately AVOIDS) would flip any of them to 2 and break the many `.find()`/`.split()` gate
    // anchors (14/9/8 test files). The `${STRIH_LX_GATE_PREFIX:-}` prefix + a `|| { ... }` tail
    // duplicate nothing, so they stay 1.
    for anchor in [
        "--win-http \"stream=$STREAM\"", // only in the MAIN DanteSync gate this fix prefixed
        "\"$HERE/dantesync-version-gate.sh\"", // the version-parity gate this fix prefixed
        "--win \"strih=",                // only in the version-parity gate's --win arg
    ] {
        assert_eq!(
            body.matches(anchor).count(),
            1,
            "issue 1351: the timeout bound must not duplicate the gate anchor {anchor:?} \
             (it is a ${{VAR:-}} prefix + a `|| {{...}}` tail, never a copied if/else invocation)"
        );
    }
}
