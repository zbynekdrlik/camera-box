//! #833 -- the [0/8] projector-count preflight shells out to `wmctrl` on imag-nb over SSH; when
//! wmctrl is ABSENT (a freshly (re)provisioned box, #791), the remote command emits nothing, the
//! local `grep -c` reads 0, and recording-e2e.sh reported "Multiview=0 Program=0 -- stray
//! projector windows are accumulating" while BOTH projectors were in fact open and correct --
//! three wasted hardware-gate re-runs before the missing tool was suspected. Same class as #822
//! (a missing `readelf`/`nm` made a hot-swap report a false ABI mismatch), applied here to the
//! SSH-remote case. The [1/8] MV-divisor capability check (`nm -D -u ... | grep -c ...`) has the
//! identical shape with a different tool and is fixed the same way.
//!
//! These tests (a) source the REAL scripts/lib/imag-require-remote-tool.sh (never re-implement
//! the parser) and drive it with a stubbed PATH -- no ssh, no rig -- and (b) pin the structural
//! wiring into scripts/recording-e2e.sh and the provisioned package set in scripts/setup-imag.sh.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    let p = manifest_dir().join("scripts/lib/imag-require-remote-tool.sh");
    assert!(p.exists(), "{} not found", p.display());
    p
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const RECORDING_E2E: &str = "scripts/recording-e2e.sh";
const SETUP: &str = "scripts/setup-imag.sh";

/// Build a fake-tool bin dir on disk containing one executable stub per name in `present`.
/// Returns the dir's path (kept alive via the returned TempDir).
fn fake_bin_dir(present: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for name in present {
        let p = dir.path().join(name);
        fs::write(&p, "#!/bin/sh\nexit 0\n").expect("write stub");
        let mut perm = fs::metadata(&p).expect("stat stub").permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
        fs::set_permissions(&p, perm).expect("chmod stub");
    }
    dir
}

/// Source the lib, run `imag_require_remote_tool_cmd $ARGS` as a REMOTE snippet (exactly the
/// way the real code embeds it: printed then executed by a fresh bash, simulating "the ssh
/// target ran this"). The OUTER shell (which sources the lib and needs `bash`/`cat`/`printf` to
/// even generate the snippet text) keeps the test process's normal PATH; only the INNER
/// "simulated remote" bash -- which is where `command -v <tool>` actually gets evaluated -- gets
/// its PATH restricted to `bin_dir` alone. Restricting the outer PATH too would risk picking up
/// this dev box's OWN real `nm`/`wmctrl` (both genuinely installed here) and silently proving
/// nothing. Returns the probe's stdout.
fn run_probe(tools: &[&str], present: &[&str]) -> String {
    let bin_dir = fake_bin_dir(present);
    let harness = format!(
        "set -uo pipefail\n. \"$SCRIPT\"\nPATH=\"$FAKEPATH\" /usr/bin/bash -c \"$(imag_require_remote_tool_cmd {})\"",
        tools.join(" ")
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib_script())
        .env("FAKEPATH", bin_dir.path())
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "probe harness must exit 0 regardless of missing tools (the CALLER decides).\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Source the lib and call `imag_remote_tool_probe_missing "$PROBE"` with the fixture passed via
/// env var (never interpolated into the bash -c script text).
fn missing_for(probe: &str) -> String {
    let harness =
        "set -uo pipefail\n. \"$SCRIPT\"\nimag_remote_tool_probe_missing \"$PROBE\"".to_string();
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib_script())
        .env("PROBE", probe)
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "imag_remote_tool_probe_missing exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

// ---------------------------------------------------------------------------------------------
// 1. imag_require_remote_tool_cmd -- the remote probe snippet itself
// ---------------------------------------------------------------------------------------------

#[test]
fn probe_reports_nothing_when_the_tool_is_present() {
    let out = run_probe(&["wmctrl"], &["wmctrl"]);
    assert_eq!(
        out.trim(),
        "",
        "a present tool must produce NO output -- a real 0/0 count must still be reachable when \
         wmctrl genuinely IS installed: got {out:?}"
    );
}

#[test]
fn probe_reports_the_missing_tool_by_name() {
    let out = run_probe(&["wmctrl"], &[]);
    assert_eq!(
        out.trim(),
        "TOOL_MISSING:wmctrl",
        "an absent tool must be reported BY NAME via a TOOL_MISSING: marker, never silently \
         empty (which is indistinguishable from 'present' -- the exact #833 bug): got {out:?}"
    );
}

#[test]
fn probe_checks_each_tool_independently_never_stopping_at_the_first() {
    // wmctrl present, nm absent -- only nm must be reported missing.
    let out = run_probe(&["wmctrl", "nm"], &["wmctrl"]);
    assert_eq!(out.trim(), "TOOL_MISSING:nm");

    // Both absent -- both must be named, not just the first one checked.
    let out_both = run_probe(&["wmctrl", "nm"], &[]);
    assert!(
        out_both.contains("TOOL_MISSING:wmctrl") && out_both.contains("TOOL_MISSING:nm"),
        "both missing tools must be named independently, not short-circuited at the first: \
         {out_both:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// 2. imag_remote_tool_probe_missing -- pure local parser of the probe's captured stdout
// ---------------------------------------------------------------------------------------------

#[test]
fn missing_parses_a_single_missing_tool() {
    assert_eq!(missing_for("TOOL_MISSING:wmctrl\n"), "wmctrl");
}

#[test]
fn missing_parses_multiple_missing_tools_space_joined() {
    assert_eq!(
        missing_for("TOOL_MISSING:wmctrl\nTOOL_MISSING:nm\n"),
        "wmctrl nm"
    );
}

#[test]
fn missing_is_empty_when_probe_output_is_empty() {
    // Also covers an ssh/connectivity failure upstream (`|| true` around the ssh call yields an
    // empty string) -- the caller must not read that as "everything present" either, but THIS
    // function's contract is "empty in -> empty out"; the caller layers its own PARSE_ERROR
    // distinction if it needs one. Never panics/errors on empty input.
    assert_eq!(missing_for(""), "");
}

#[test]
fn missing_ignores_unrelated_probe_noise() {
    // A defensive parse: only TOOL_MISSING: lines are ever extracted, everything else ignored.
    assert_eq!(
        missing_for("some unrelated line\nTOOL_MISSING:wmctrl\nok: fine\n"),
        "wmctrl"
    );
}

// ---------------------------------------------------------------------------------------------
// 3. recording-e2e.sh wiring -- the preflight tool checks must run BEFORE the wmctrl/nm calls
//    they guard, and the failure message must NAME the tool, never claim a stray-window count.
// ---------------------------------------------------------------------------------------------

#[test]
fn recording_e2e_sources_the_require_remote_tool_lib() {
    let s = read(RECORDING_E2E);
    assert!(
        s.contains("lib/imag-require-remote-tool.sh") && s.contains("imag_require_remote_tool_cmd"),
        "{RECORDING_E2E} must source lib/imag-require-remote-tool.sh and call \
         imag_require_remote_tool_cmd -- #833"
    );
}

#[test]
fn wmctrl_tool_check_runs_before_the_heal_and_the_count_check() {
    let s = read(RECORDING_E2E);
    let open_idx = s
        .find("imag-nb Multiview + Program projectors must be OPEN")
        .expect("the #758 open-projectors preflight must exist");
    let heal_idx = s
        .find("imag_projector_heal_cmds")
        .expect("the #769 heal call must exist");
    let count_idx = s
        .find("projector count must be EXACTLY 1 Multiview + 1 Program")
        .expect("the #756 projector-count preflight must exist");

    // Find the wmctrl require-tool check: the FIRST imag_require_remote_tool_cmd invocation
    // naming "wmctrl" (there is a second one further down guarding the nm-based divisor check).
    let tool_check_idx = s
        .find("imag_require_remote_tool_cmd wmctrl")
        .expect("#833: a wmctrl tool preflight (imag_require_remote_tool_cmd wmctrl) must exist");

    assert!(
        open_idx < tool_check_idx,
        "#833: the wmctrl tool check must run AFTER open-projectors (no point checking tooling \
         before we even know imag is reachable/usable)"
    );
    assert!(
        tool_check_idx < heal_idx,
        "#833: the wmctrl tool check must run BEFORE the #769 heal (which also shells wmctrl) -- \
         a missing tool must be caught before either downstream wmctrl call can misreport it"
    );
    assert!(
        heal_idx < count_idx,
        "#756 ordering must be preserved: heal before the count check"
    );
}

#[test]
fn wmctrl_tool_check_fails_loud_naming_the_tool_never_a_stray_windows_message() {
    let s = read(RECORDING_E2E);
    let idx = s
        .find("imag_require_remote_tool_cmd wmctrl")
        .expect("#833: the wmctrl tool preflight must exist");
    let block = &s[idx..(idx + 1200).min(s.len())];
    assert!(
        block.contains("exit 1"),
        "#833: a missing wmctrl must hard-fail the preflight (exit 1), never silently continue \
         into the count check that cannot measure anything: {block}"
    );
    assert!(
        block.contains("apt-get install") && block.contains("wmctrl"),
        "#833: the failure message must name the missing tool and how to install it, not \
         describe a stray-window/config problem that was never real: {block}"
    );
    assert!(
        !block.contains("stray projector windows are accumulating"),
        "#833: the tool-missing failure must be its OWN distinct message -- it must never reuse \
         the #756 'stray projector windows are accumulating' wording, which is a false diagnosis \
         when the real cause is a missing tool: {block}"
    );
}

#[test]
fn nm_tool_check_runs_before_the_divisor_capability_probe() {
    let s = read(RECORDING_E2E);
    let checks: Vec<_> = s.match_indices("imag_require_remote_tool_cmd").collect();
    assert!(
        checks.len() >= 2,
        "#833: expected at least 2 imag_require_remote_tool_cmd call sites (wmctrl + nm), \
         got {}",
        checks.len()
    );
    let nm_check_idx = s
        .find("imag_require_remote_tool_cmd nm")
        .expect("#833: an nm tool preflight (imag_require_remote_tool_cmd nm) must exist");
    let divisor_probe_idx = s
        .find("nm -D -u /usr/bin/obs")
        .expect("the #756 divisor-capability probe must exist");
    assert!(
        nm_check_idx < divisor_probe_idx,
        "#833: the nm tool check must run BEFORE the divisor-capability probe that shells nm -- \
         a missing nm must be caught before it can be misread as 'MV divisor capability MISSING \
         (#756 regression)'"
    );
}

#[test]
fn nm_tool_check_fails_loud_naming_the_tool_never_a_false_regression_claim() {
    let s = read(RECORDING_E2E);
    let idx = s
        .find("imag_require_remote_tool_cmd nm")
        .expect("#833: the nm tool preflight must exist");
    let block = &s[idx..(idx + 1200).min(s.len())];
    assert!(
        block.contains("exit 1"),
        "#833: a missing nm must hard-fail the preflight (exit 1): {block}"
    );
    assert!(
        block.contains("apt-get install") && block.contains("binutils"),
        "#833: the failure message must name the missing tool (nm, from binutils) and how to \
         install it: {block}"
    );
    assert!(
        !block.contains("#756 regression"),
        "#833: the tool-missing failure must never claim a '#756 regression' -- that message is \
         reserved for a GENUINE symbol-absence measurement, not a tool that could not even run: \
         {block}"
    );
}

// ---------------------------------------------------------------------------------------------
// 4. setup-imag.sh -- wmctrl joins the provisioned package set (a fresh imag box must have it)
// ---------------------------------------------------------------------------------------------

#[test]
fn setup_imag_installs_wmctrl_833() {
    let body = read(SETUP);
    assert!(
        body.lines()
            .any(|l| { l.contains("apt-get install") && l.contains("wmctrl") }),
        "{SETUP} (#833) must `apt-get install` wmctrl -- the replacement imag notebook lacked it \
         precisely because it was newly provisioned, and the #756 projector-count preflight \
         depends on it being present"
    );
}
