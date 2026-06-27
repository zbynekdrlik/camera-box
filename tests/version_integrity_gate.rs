//! Behavioral guard for `scripts/version-integrity-gate.sh` — the pre-rig-test VERSION-INTEGRITY
//! precondition gate (#123, EPIC #125). No rig test (recording-e2e, loopback, the obs
//! phase scripts) may bring up the rig and trust its result unless the LIVE strih+stream stack
//! matches the pinned SHA set: a test run on a randomly-deployed / drifted / stock OBS build is
//! worthless (that is exactly #119 — a wrong-bytes-right-version build that silently shipped). So
//! this gate runs FIRST, ALONGSIDE the DanteSync gate (#7), and must FAIL FAST (refuse, exit
//! non-zero) on DRIFT (20) or UNKNOWN (11) so a meaningless run never reaches the recording step.
//!
//! The gate is a WIRING layer over the unit-tested `scripts/drift-guard.sh --compare` engine
//! (tested in tests/drift_guard.rs) — it does NOT reinvent any comparison. It mirrors
//! dantesync-gate.sh: the Windows boxes (ssh denied) have their live observed stack state
//! pre-fetched to a JSON FILE by the win-* MCP holder (or fetched over the standing http.server);
//! this gate parses each box's state into drift-guard `--compare` key=val args, runs the engine
//! per box, and rolls the verdicts up. A box with no state file is UNKNOWN -> the gate refuses
//! (never a silent pass). These tests pin the gate's own FLOW: the state->args parse, the verdict
//! roll-up, and the end-to-end exit-code contract over state FILES (the path that needs no live
//! rig).

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/version-integrity-gate.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the gate (its BASH_SOURCE!=$0 guard skips main) and run `body`, returning stdout.
fn run_sourced(body: &str, extra_env: &[(&str, &str)]) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&harness).env("SCRIPT", script());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run bash harness");
    assert!(
        out.status.success(),
        "sourced harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Run the gate as a subprocess; return (exit_code, stdout, stderr).
fn run_gate(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(script())
        .args(args)
        .current_dir(manifest_dir())
        .output()
        .expect("run version-integrity-gate.sh");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn write_state(name: &str, json: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vig-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.json"));
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(json.as_bytes()).unwrap();
    path
}

/// A strih state JSON that MATCHES the pinned set in vendor/README.md (the known-good zero-loss
/// state verified live). The flat object carries the drift-guard `--compare` observed keys.
const STRIH_PINNED: &str = "{\
\"obs_version\":\"32.1.2\",\
\"distroav_version\":\"6.2.1\",\
\"ndi_runtime\":\"6.3.2.0\",\
\"output_fps\":\"60\",\
\"genlock_wall_clock\":\"1\",\
\"ndi_input_latency\":\"NDI cam5=0,NDI cam1=0,NDI cam3=0\",\
\"distroav_dll_paths\":\"C:\\\\ProgramData\\\\obs-studio\\\\plugins\\\\distroav\\\\bin\\\\64bit\\\\distroav.dll\"\
}";

/// A stream state JSON that MATCHES the pinned set (the stream box's broadcast input is NDI 2ME PGM).
/// #11 mixed 60/30: the stream box DECIMATES the 60fps strih feed to 30fps output, so its observed
/// output_fps is 30 (matches the host-keyed `output_fps_stream` pin).
const STREAM_PINNED: &str = "{\
\"obs_version\":\"32.1.2\",\
\"distroav_version\":\"6.2.1\",\
\"ndi_runtime\":\"6.3.2.0\",\
\"output_fps\":\"30\",\
\"genlock_wall_clock\":\"1\",\
\"ndi_input_latency\":\"NDI 2ME PGM=0\",\
\"distroav_dll_paths\":\"C:\\\\ProgramData\\\\obs-studio\\\\plugins\\\\distroav\\\\bin\\\\64bit\\\\distroav.dll\"\
}";

#[test]
fn compare_args_from_state_emits_drift_guard_key_vals() {
    // The pure parse turns a box's flat state JSON into the `key=val` args drift-guard --compare
    // expects, one per line, preserving values with spaces (ndi_input_latency) and backslashes
    // (the Windows plugin path). This is the only bespoke parsing the gate does — everything else
    // is delegated to the unit-tested engine.
    let p = write_state("strih_args", STRIH_PINNED);
    let out = run_sourced(
        "compare_args_from_state \"$F\"",
        &[("F", p.to_str().unwrap())],
    );
    let lines: Vec<String> = out.lines().map(|l| l.to_string()).collect();
    assert!(
        lines.iter().any(|l| l == "obs_version=32.1.2"),
        "must emit obs_version: {out:?}"
    );
    assert!(
        lines.iter().any(|l| l == "ndi_runtime=6.3.2.0"),
        "must emit ndi_runtime: {out:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l == "ndi_input_latency=NDI cam5=0,NDI cam1=0,NDI cam3=0"),
        "must preserve a value containing spaces AND '=' (one key=val per line): {out:?}"
    );
    assert!(
        lines.iter().any(
            |l| l == r"distroav_dll_paths=C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll"
        ),
        "must preserve the Windows backslash path: {out:?}"
    );
    let _ = std::fs::remove_file(&p);
}

#[test]
fn gate_passes_when_both_boxes_match_the_pinned_set() {
    // The whole point: the live stack == pinned set on BOTH boxes -> GATE PASS (0). The rig test
    // may proceed and trust its result.
    let s = write_state("strih_pin", STRIH_PINNED);
    let t = write_state("stream_pin", STREAM_PINNED);
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
    ]);
    assert_eq!(
        code, 0,
        "both boxes pinned must PASS. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout: {stdout}");
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
}

#[test]
fn gate_refuses_when_a_box_has_drifted() {
    // strih is on a DIFFERENT obs version than the pinned 32.1.2 (a randomly-deployed / stale build).
    // The gate must REFUSE (exit 20) — running the rig test on a drifted stack would produce a
    // worthless result (#123/#119). It names the box + the engine's DRIFT line.
    let drifted = STRIH_PINNED.replace("\"obs_version\":\"32.1.2\"", "\"obs_version\":\"31.0.0\"");
    let s = write_state("strih_drift", &drifted);
    let t = write_state("stream_pin2", STREAM_PINNED);
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
    ]);
    assert_eq!(
        code, 20,
        "a drifted box must REFUSE (20). stdout={stdout} stderr={stderr}"
    );
    assert!(
        stderr.contains("REFUSED") || stderr.contains("GATE FAILED"),
        "must refuse loudly. stderr: {stderr}"
    );
    assert!(
        stdout.contains("strih") && stdout.contains("DRIFT"),
        "must name the drifted box + the drift. stdout: {stdout}"
    );
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
}

#[test]
fn gate_incomplete_when_a_box_state_was_unread() {
    // strih state present + pinned, but stream's state file is MISSING (the win-* MCP fetch did not
    // run) -> stream UNKNOWN -> gate REFUSES (exit 11), never a silent pass on an unread box.
    let s = write_state("strih_pin3", STRIH_PINNED);
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        "stream=/tmp/definitely-not-a-real-version-state.json",
    ]);
    assert_eq!(
        code, 11,
        "an unread box state must be INCOMPLETE (11), not 0. stdout={stdout} stderr={stderr}"
    );
    assert!(
        !stdout.contains("GATE PASS"),
        "must NOT pass when a box is unread. stdout: {stdout}"
    );
    assert!(
        stderr.contains("INCOMPLETE") && stderr.contains("stream"),
        "must report the unread box. stderr: {stderr}"
    );
    let _ = std::fs::remove_file(&s);
}

#[test]
fn gate_with_no_boxes_refuses_to_pass() {
    // Zero boxes to check must be a usage error (1), never "all clear" — the same fail-closed
    // discipline as dantesync-gate.
    let (code, _o, stderr) = run_gate(&[]);
    assert_eq!(code, 1, "zero boxes -> usage error (1). stderr: {stderr}");
    assert!(
        stderr.contains("no box") || stderr.contains("zero box") || stderr.contains("no nodes"),
        "stderr: {stderr}"
    );
}

#[test]
fn gate_uses_a_custom_readme_for_the_pinned_set() {
    // The gate reads the pinned set from vendor/README.md by default (same source-of-truth as the
    // engine), but --readme overrides it. A README pinning a DIFFERENT obs version makes a
    // box-matching-the-real-README state DRIFT against the custom pin -> exit 20. This proves the
    // gate threads the pin source through to the engine rather than hard-coding versions.
    let dir = std::env::temp_dir().join(format!("vig-readme-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("distroav")).unwrap();
    let readme = dir.join("README.md");
    std::fs::write(
        &readme,
        "\
| `vendor/obs-studio` | x | **99.9.9** (commit `a`) | git subtree --squash |
| `vendor/distroav` | x | **6.2.1** (commit `b`) | git subtree --squash |
| NDI | x | requires **NDI ≥ 6.3.0** | tree |
| `output_fps_strih` | `60` | log |
| `output_fps_stream` | `30` | log |
| `genlock_wall_clock` | `1` | env |
| `ndi_input_latency` | `0` | obs-websocket |
| `canonical_plugin_path` | `C:\\ProgramData\\obs-studio\\plugins\\distroav\\bin\\64bit` | scan |
",
    )
    .unwrap();
    std::fs::write(
        dir.join("distroav/buildspec.json"),
        "{\n    \"version\": \"6.2.1\"\n}\n",
    )
    .unwrap();

    // The box reports the REAL 32.1.2, which DRIFTS from the custom README's 99.9.9 pin.
    let s = write_state("strih_custompin", STRIH_PINNED);
    let (code, stdout, _stderr) = run_gate(&[
        "--readme",
        readme.to_str().unwrap(),
        "--win-state",
        &format!("strih={}", s.display()),
    ]);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&s);
    assert_eq!(
        code, 20,
        "a box drifting from the custom README pin must REFUSE (20). stdout={stdout}"
    );
    assert!(
        stdout.contains("99.9.9") && stdout.contains("DRIFT"),
        "must compare against the custom README's pin. stdout: {stdout}"
    );
}

#[test]
fn help_describes_the_version_integrity_requirement() {
    let (code, stdout, _e) = run_gate(&["--help"]);
    assert_eq!(code, 0, "--help must exit 0");
    let low = stdout.to_lowercase();
    assert!(
        low.contains("pinned") && (low.contains("drift") || low.contains("version")),
        "help must describe the pinned-set / drift requirement: {stdout}"
    );
}
