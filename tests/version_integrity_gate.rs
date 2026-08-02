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
//! dantesync-gate.sh: the Windows boxes (this gate has no headless ssh gather of its own — #701
//! proved plain scp/ssh reaches strih/stream, not migrated here) have their live observed stack
//! state pre-fetched to a JSON FILE by the win-* MCP holder (or fetched over the standing http.server);
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
///
/// #826: the gate's own top-of-file `set -euo pipefail` LEAKS into the current shell when the
/// file is merely SOURCED (a sourced script's `set` options are not scoped away on return) — so
/// any `body` that calls a verdict function returning non-zero (a DRIFT/UNKNOWN scenario, exactly
/// what most #826 verdict tests need to assert) would abort THIS harness before its own trailing
/// `echo RC=$?` ever ran, well before `body`'s own logic had a say. `set +e` immediately after the
/// source neutralizes the leaked `-e` for every caller — behavior-preserving for existing callers
/// (which only ever exercised return-0 scenarios and so never tripped over this).
fn run_sourced(body: &str, extra_env: &[(&str, &str)]) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\nset +e\n{body}");
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
/// Topology v2 (#459, EPIC #466, was #11 mixed 60/30): strih is now cut-to-stream-only at 30fps
/// (the 60fps IMAG role moved to imag-nb, #458/#463) -- its observed output_fps must match the
/// re-pinned `output_fps_strih=30`.
const STRIH_PINNED: &str = "{\
\"obs_version\":\"32.1.2\",\
\"distroav_version\":\"6.2.1\",\
\"ndi_runtime\":\"6.3.2.0\",\
\"output_fps\":\"30\",\
\"genlock_wall_clock\":\"1\",\
\"ndi_input_latency\":\"NDI cam5=0,NDI cam1=0,NDI cam3=0\",\
\"distroav_dll_paths\":\"C:\\\\ProgramData\\\\obs-studio\\\\plugins\\\\distroav\\\\bin\\\\64bit\\\\distroav.dll\"\
}";

/// A stream state JSON that MATCHES the pinned set (the stream box's broadcast input is NDI 2ME PGM).
/// #459 (was #11 mixed 60/30): stream now receives an ALREADY-30fps feed from strih (plain
/// pass-through, no further decimation), so its observed output_fps is 30 (matches the
/// host-keyed `output_fps_stream` pin, unchanged by this topology move).
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
    // may proceed and trust its result. #758: the cross-box genlock parity facet is now ENFORCED
    // (no longer opt-in) -- a genuinely clean/healthy fixture must ALSO carry a matching
    // genlock_build_sha on every box (strih/stream via the state fixture, imag via --genlock-sha),
    // exactly like the real fleet does as of 2026-07-14 (~21:40, all three unified).
    const SHA: &str = "26de1c3c23980488a110dbf02e5e472f15cb001d";
    let s = write_state("strih_pin", &with_sha(STRIH_PINNED, SHA));
    let t = write_state("stream_pin", &with_sha(STREAM_PINNED, SHA));
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        "--genlock-sha",
        &format!("imag={SHA}"),
    ]);
    assert_eq!(
        code, 0,
        "both boxes pinned + fleet genlock parity must PASS. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout: {stdout}");
    assert!(
        stdout.contains("genlock_parity") && stdout.contains("ONE genlock build"),
        "the ENFORCED parity facet must have engaged + reported OK: {stdout}"
    );
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
}

#[test]
fn gate_is_incomplete_when_no_box_reports_a_genlock_build_sha_758() {
    // #758 — the OTHER half of "ENFORCED": a fixture that does NOT carry any genlock_build_sha at
    // all (the #756 opt-in-rollout scenario this test used to represent as a dormant-facet PASS)
    // must now INCOMPLETE (11), not silently pass -- an un-upgraded/unread bundle-state-server is
    // itself a real gap once the facet is enforced, never a reason to skip the whole check.
    let s = write_state("strih_pin_no_sha_758", STRIH_PINNED);
    let t = write_state("stream_pin_no_sha_758", STREAM_PINNED);
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
    ]);
    assert_eq!(
        code, 11,
        "zero boxes reporting a genlock_build_sha must now be INCOMPLETE, not a silent PASS. \
         stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("genlock_parity") && stdout.contains("INCOMPLETE"),
        "must report the ENFORCED parity facet as incomplete: {stdout}"
    );
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
}

/// Inject a #756 `genlock_build_sha` into a pinned state fixture (insert before the closing brace).
fn with_sha(base: &str, sha: &str) -> String {
    format!(
        "{},\"genlock_build_sha\":\"{sha}\"}}",
        &base[..base.len() - 1]
    )
}

#[test]
fn gate_passes_when_fleet_genlock_builds_are_in_parity_756() {
    // strih + stream states carry a matching genlock_build_sha AND imag's SHA (supplied via
    // --genlock-sha) matches -> the whole fleet is on ONE build -> cross-box parity OK, GATE PASS.
    const SHA: &str = "26de1c3c23980488a110dbf02e5e472f15cb001d";
    let s = write_state("strih_parity_ok", &with_sha(STRIH_PINNED, SHA));
    let t = write_state("stream_parity_ok", &with_sha(STREAM_PINNED, SHA));
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        "--genlock-sha",
        &format!("imag={SHA}"),
    ]);
    assert_eq!(
        code, 0,
        "unified fleet must PASS. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout: {stdout}");
    assert!(
        stdout.contains("genlock_parity") && stdout.contains("ONE genlock build"),
        "the parity facet must have engaged + reported OK: {stdout}"
    );
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
}

#[test]
fn gate_refuses_when_imag_genlock_build_skews_from_the_windows_boxes_756() {
    // The #756 scenario: strih/stream on the current build, imag on a STALE lineage. Each box's own
    // drift-guard --compare passes (versions/settings identical across builds — the exact false OK
    // the ref-compare gives), but the CROSS-BOX parity assert catches the skew -> GATE REFUSED (20).
    const CUR: &str = "26de1c3c23980488a110dbf02e5e472f15cb001d";
    const STALE: &str = "8e2817e5aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let s = write_state("strih_parity_skew", &with_sha(STRIH_PINNED, CUR));
    let t = write_state("stream_parity_skew", &with_sha(STREAM_PINNED, CUR));
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        "--genlock-sha",
        &format!("imag={STALE}"),
    ]);
    assert_eq!(
        code, 20,
        "a stale imag vs current strih/stream must REFUSE (20). stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("genlock_parity") && stdout.contains("SKEW"),
        "must report the cross-box genlock SKEW: {stdout}"
    );
    assert!(stderr.contains("GATE FAILED"), "stderr: {stderr}");
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
}

#[test]
fn gate_refuses_when_only_windows_boxes_report_a_build_and_imag_is_unread_756() {
    // strih/stream report a SHA (2 read peers -> the facet ENGAGES) but imag's SHA is empty (ssh
    // hiccup): the parity picture is INCOMPLETE for the box we most need to check -> UNKNOWN (11),
    // never a false OK. Proves the facet is fail-closed once populated, not just on an outright skew.
    const SHA: &str = "26de1c3c23980488a110dbf02e5e472f15cb001d";
    let s = write_state("strih_parity_unread", &with_sha(STRIH_PINNED, SHA));
    let t = write_state("stream_parity_unread", &with_sha(STREAM_PINNED, SHA));
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        "--genlock-sha",
        "imag=",
    ]);
    assert_eq!(
        code, 11,
        "an unread imag among a populated set must be INCOMPLETE (11). stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("genlock_parity") && stdout.contains("INCOMPLETE"),
        "must report the incomplete parity naming imag: {stdout}"
    );
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
}

// ---- #949 — a Windows-only vendor change must not block the fleet on a cosmetic LABEL skew ----
// THE regression test for the actual bug (live 2026-08-02, run 30768287281 / PR #948):
// linux-genlock.yml's push trigger deliberately excludes vendor/av-sync-dock/** (a Windows-only
// OBS dock DLL), so a Windows-only vendor change advances strih/stream's deployed
// GENLOCK_BUILD_SHA.txt to a SHA imag's own build can never reach — even though imag's ACTUAL
// built bytes never changed. These are the REAL SHAs from that incident: the only vendor commit
// between them (a48b56380) touches vendor/av-sync-dock/** only.
const WIN_INCIDENT_SHA_949: &str = "d77426c758074686b7bc8716962f0042fa8687bf";
const IMAG_INCIDENT_SHA_949: &str = "2a12a6a9991eeeae5580a6fbe047d60275d0c8b2";

#[test]
fn gate_passes_when_a_windows_only_vendor_change_advances_strih_stream_past_imags_reachable_sha_949(
) {
    // Before the #949 fix this call reproduces the live incident verbatim: strih/stream share ONE
    // real SHA, imag reports a DIFFERENT real SHA, and the only vendor commit between them touches
    // vendor/av-sync-dock only — imag's binaries are byte-identical, so this must be GATE PASS (0),
    // never DRIFT. Uses `run_gate`, whose subprocess `current_dir` is this repo's own checkout (real
    // git history for both SHAs), so the fix's real `git diff` path is exercised end-to-end, not
    // just the pure decision layer (see tests/drift_guard.rs for that half).
    let s = write_state(
        "strih_949_incident",
        &with_sha(STRIH_PINNED, WIN_INCIDENT_SHA_949),
    );
    let t = write_state(
        "stream_949_incident",
        &with_sha(STREAM_PINNED, WIN_INCIDENT_SHA_949),
    );
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        "--genlock-sha",
        &format!("imag={IMAG_INCIDENT_SHA_949}"),
    ]);
    assert_eq!(
        code, 0,
        "a Windows-only vendor change (imag's actual bytes unchanged) must PASS, not DRIFT the \
         whole gate. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout: {stdout}");
    assert!(
        stdout.contains("genlock_parity") && stdout.contains("OK") && stdout.contains("PARITY"),
        "the parity facet must report OK via the #949 content-equivalence path: {stdout}"
    );
    assert!(
        !stdout.contains("SKEW"),
        "must never report a genlock_parity SKEW for a cosmetic label-only mismatch: {stdout}"
    );
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
}

#[test]
fn gate_still_refuses_a_genuine_vendor_obs_studio_skew_after_the_949_fix() {
    // The strictness #949 must NOT weaken: a REAL vendor/obs-studio content difference (not just a
    // label mismatch) must still DRIFT the gate. Real commit pair — only the newer one touches
    // vendor/obs-studio/libobs/obs.h + obs-source.c + asrc-compensator.* + the obs-websocket
    // requesthandler; genuinely different built bytes on both the Windows AND Linux consumed sets.
    const OLDER: &str = "cb92f28a6a90a89b2877f7d00dde93561ae9a70c";
    const NEWER: &str = "f6477a4fe6a7b7a36e6351d13ed106e10d673356";
    let s = write_state("strih_949_real_skew", &with_sha(STRIH_PINNED, NEWER));
    let t = write_state("stream_949_real_skew", &with_sha(STREAM_PINNED, NEWER));
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        "--genlock-sha",
        &format!("imag={OLDER}"),
    ]);
    assert_eq!(
        code, 20,
        "a genuine vendor/obs-studio content skew must still REFUSE (20) after #949. \
         stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("genlock_parity") && stdout.contains("DRIFT") && stdout.contains("SKEW"),
        "must still report the real cross-box genlock SKEW: {stdout}"
    );
    assert!(stderr.contains("GATE FAILED"), "stderr: {stderr}");
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
| `output_fps_strih` | `30` | log |
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

// ── #826: strih OBS-identity machine-check facet ────────────────────────────────────────────
//
// The 2026-07-27 incident: a hand-launched stale `1ME` OBS 31.1.2 install squatted TCP :4455
// while the version-integrity gate's own parity marker still described the pinned genlock
// 32.1.2 build -- the harness silently drove/measured the WRONG renderer for a whole gate cycle.
// These tests pin the four new pure verdict functions (sourced directly, mirroring how
// `compare_args_from_state` is tested above) plus the end-to-end opt-in rollout behavior: a box
// that reports NONE of the new keys must gate EXACTLY as before (the live fleet + every existing
// fixture here predates the redeployed bundle-state-server.py that will report them).

const PINNED_OBS_EXE: &str = r"C:\Program Files\obs-studio\bin\64bit\obs64.exe";
const PINNED_OBS_WORKDIR: &str = r"C:\Program Files\obs-studio\bin\64bit";
const PINNED_SHORTCUT: &str =
    r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\OBS Studio.lnk";

#[test]
fn state_json_value_is_the_generic_single_key_parser() {
    let p = write_state(
        "generic_kv",
        "{\"foo\":\"bar baz\",\"genlock_build_sha\":\"abc123\"}",
    );
    let out = run_sourced("state_json_value \"$F\" foo", &[("F", p.to_str().unwrap())]);
    assert_eq!(out.trim_end_matches('\n'), "bar baz");
    let _ = std::fs::remove_file(&p);
}

#[test]
fn genlock_build_sha_from_state_still_works_after_the_generic_refactor() {
    // Behavior-preserving refactor check: genlock_build_sha_from_state must keep returning
    // exactly what it did before, now implemented via state_json_value.
    let p = write_state("sha_refactor", "{\"genlock_build_sha\":\"deadbeef\"}");
    let out = run_sourced(
        "genlock_build_sha_from_state \"$F\"",
        &[("F", p.to_str().unwrap())],
    );
    assert_eq!(out, "deadbeef");
    let _ = std::fs::remove_file(&p);
}

#[test]
fn obs_installs_verdict_ok_when_only_the_pinned_install_exists() {
    let out = run_sourced(
        &format!("obs_installs_verdict '{PINNED_OBS_EXE}' '{PINNED_OBS_EXE}'; echo RC=$?"),
        &[],
    );
    assert!(out.contains("OK"), "expected OK: {out}");
    assert!(out.contains("RC=0"), "{out}");
}

#[test]
fn obs_installs_verdict_drifts_on_a_retired_folder_still_present() {
    // Renaming a folder aside (_RETIRED_*) is NOT the same as removing the install -- its exe is
    // still launchable, and must still be reported as a DRIFT-worthy extra.
    let extra = r"D:\_APPS\_RETIRED_1ME-obs_2026-07-27\bin\64bit\obs64.exe";
    let csv = format!("{PINNED_OBS_EXE},{extra}");
    let out = run_sourced(
        &format!("obs_installs_verdict '{PINNED_OBS_EXE}' '{csv}'; echo RC=$?"),
        &[],
    );
    assert!(out.contains("DRIFT"), "expected DRIFT: {out}");
    assert!(
        out.contains("_RETIRED_1ME-obs_2026-07-27"),
        "must name the extra install: {out}"
    );
    assert!(out.contains("RC=20"), "{out}");
}

#[test]
fn obs_installs_verdict_unknown_when_scan_unread() {
    let out = run_sourced(
        &format!("obs_installs_verdict '{PINNED_OBS_EXE}' ''; echo RC=$?"),
        &[],
    );
    assert!(out.contains("UNKNOWN"), "expected UNKNOWN: {out}");
    assert!(out.contains("RC=11"), "{out}");
}

#[test]
fn port_identity_verdict_ok_when_owner_matches_pinned() {
    let out = run_sourced(
        &format!(
            "port_identity_verdict '{PINNED_OBS_EXE}' '32.1.2' '{PINNED_OBS_EXE}' '32.1.2'; echo RC=$?"
        ),
        &[],
    );
    assert!(out.contains("OK"), "{out}");
    assert!(out.contains("RC=0"), "{out}");
}

#[test]
fn port_identity_verdict_drifts_when_the_owner_is_a_different_install() {
    // The exact 2026-07-27 incident: :4455 owned by the stale 1ME install, by PATH, not name.
    let stale = r"D:\_APPS\_RETIRED_1ME-obs_2026-07-27\bin\64bit\obs64.exe";
    let out = run_sourced(
        &format!(
            "port_identity_verdict '{PINNED_OBS_EXE}' '32.1.2' '{stale}' '31.1.2'; echo RC=$?"
        ),
        &[],
    );
    assert!(out.contains("DRIFT"), "{out}");
    assert!(out.contains(stale), "must name the wrong owner path: {out}");
    assert!(out.contains("RC=20"), "{out}");
}

#[test]
fn port_identity_verdict_drifts_on_version_mismatch_at_the_right_path() {
    let out = run_sourced(
        &format!(
            "port_identity_verdict '{PINNED_OBS_EXE}' '32.1.2' '{PINNED_OBS_EXE}' '31.1.2'; echo RC=$?"
        ),
        &[],
    );
    assert!(out.contains("DRIFT"), "{out}");
    assert!(out.contains("RC=20"), "{out}");
}

#[test]
fn port_identity_verdict_unknown_when_owner_unread() {
    let out = run_sourced(
        &format!("port_identity_verdict '{PINNED_OBS_EXE}' '32.1.2' '' ''; echo RC=$?"),
        &[],
    );
    assert!(out.contains("UNKNOWN"), "{out}");
    assert!(out.contains("RC=11"), "{out}");
}

#[test]
fn obs_process_count_verdict_ok_for_exactly_one() {
    let out = run_sourced("obs_process_count_verdict '1'; echo RC=$?", &[]);
    assert!(out.contains("OK"), "{out}");
    assert!(out.contains("RC=0"), "{out}");
}

#[test]
fn obs_process_count_verdict_drifts_on_a_second_process() {
    let out = run_sourced("obs_process_count_verdict '2'; echo RC=$?", &[]);
    assert!(out.contains("DRIFT"), "{out}");
    assert!(out.contains("RC=20"), "{out}");
}

#[test]
fn obs_process_count_verdict_drifts_on_zero_running() {
    let out = run_sourced("obs_process_count_verdict '0'; echo RC=$?", &[]);
    assert!(out.contains("DRIFT"), "{out}");
    assert!(out.contains("RC=20"), "{out}");
}

#[test]
fn obs_process_count_verdict_unknown_when_unread() {
    let out = run_sourced("obs_process_count_verdict ''; echo RC=$?", &[]);
    assert!(out.contains("UNKNOWN"), "{out}");
    assert!(out.contains("RC=11"), "{out}");
}

#[test]
fn startup_chain_verdict_ok_when_everything_resolves_to_the_pinned_install() {
    let out = run_sourced(
        &format!(
            "startup_chain_verdict '{PINNED_OBS_EXE}' '{PINNED_OBS_WORKDIR}' '{PINNED_SHORTCUT}' \
             '{PINNED_SHORTCUT}' '1' '0' '{PINNED_OBS_EXE}' '{PINNED_OBS_WORKDIR}'; echo RC=$?"
        ),
        &[],
    );
    assert!(out.contains("OK"), "{out}");
    assert!(out.contains("RC=0"), "{out}");
}

#[test]
fn startup_chain_verdict_drifts_when_dead_leftover_config_present() {
    // #826's "config states one truth" requirement: NL_STARTUP.ahk still carrying the dead
    // app1_binarypath / enabled app2_* leftover is itself a DRIFT, even when app1 resolves fine.
    let out = run_sourced(
        &format!(
            "startup_chain_verdict '{PINNED_OBS_EXE}' '{PINNED_OBS_WORKDIR}' '{PINNED_SHORTCUT}' \
             '{PINNED_SHORTCUT}' '1' '1' '{PINNED_OBS_EXE}' '{PINNED_OBS_WORKDIR}'; echo RC=$?"
        ),
        &[],
    );
    assert!(out.contains("DRIFT"), "{out}");
    assert!(
        out.contains("app1_binarypath") || out.contains("app2"),
        "{out}"
    );
    assert!(out.contains("RC=20"), "{out}");
}

#[test]
fn startup_chain_verdict_drifts_when_shortcut_resolves_elsewhere() {
    // The exact incident shape: app1_path still names the Start Menu shortcut, but that shortcut
    // (or a hand launch) resolves to the stale install instead of the pinned genlock build.
    let stale = r"D:\_APPS\_RETIRED_1ME-obs_2026-07-27\bin\64bit\obs64.exe";
    let out = run_sourced(
        &format!(
            "startup_chain_verdict '{PINNED_OBS_EXE}' '{PINNED_OBS_WORKDIR}' '{PINNED_SHORTCUT}' \
             '{PINNED_SHORTCUT}' '1' '0' '{stale}' '{PINNED_OBS_WORKDIR}'; echo RC=$?"
        ),
        &[],
    );
    assert!(out.contains("DRIFT"), "{out}");
    assert!(out.contains("RC=20"), "{out}");
}

#[test]
fn startup_chain_verdict_unknown_when_unread() {
    let out = run_sourced(
        &format!(
            "startup_chain_verdict '{PINNED_OBS_EXE}' '{PINNED_OBS_WORKDIR}' '{PINNED_SHORTCUT}' \
             '' '' '' '' ''; echo RC=$?"
        ),
        &[],
    );
    assert!(out.contains("UNKNOWN"), "{out}");
    assert!(out.contains("RC=11"), "{out}");
}

/// Inject arbitrary extra `"key":"value"` pairs into a pinned state fixture (before the closing
/// brace) — mirrors `with_sha` above, generalized to any #826 obs-identity key set.
fn with_obs_identity(base: &str, extra_pairs: &[(&str, &str)]) -> String {
    let mut out = base[..base.len() - 1].to_string();
    for (k, v) in extra_pairs {
        out.push_str(&format!(",\"{k}\":\"{}\"", v.replace('\\', "\\\\")));
    }
    out.push('}');
    out
}

#[test]
fn gate_still_passes_when_a_box_reports_none_of_the_826_obs_identity_keys() {
    // Rollout is opt-in (mirrors #756's original landing): a box whose bundle-state-server has
    // not yet been redeployed with the #826 facet must gate EXACTLY as before -- this is the
    // backward-compatibility proof for the entire existing fleet/fixture set.
    const SHA: &str = "26de1c3c23980488a110dbf02e5e472f15cb001d";
    let s = write_state("strih_no826", &with_sha(STRIH_PINNED, SHA));
    let t = write_state("stream_no826", &with_sha(STREAM_PINNED, SHA));
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        "--genlock-sha",
        &format!("imag={SHA}"),
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert!(
        !stdout.contains("obs_installs"),
        "must not engage when unreported: {stdout}"
    );
    assert!(
        !stdout.contains("port4455_identity"),
        "must not engage when unreported: {stdout}"
    );
    assert!(
        !stdout.contains("obs_process_count"),
        "must not engage when unreported: {stdout}"
    );
    assert!(
        !stdout.contains("startup_chain"),
        "must not engage when unreported: {stdout}"
    );
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
}

#[test]
fn gate_refuses_when_a_reporting_box_has_an_extra_obs_install_826() {
    const SHA: &str = "26de1c3c23980488a110dbf02e5e472f15cb001d";
    let extra = r"D:\_APPS\_RETIRED_1ME-obs_2026-07-27\bin\64bit\obs64.exe";
    let s = write_state(
        "strih_extra_install_826",
        &with_obs_identity(
            &with_sha(STRIH_PINNED, SHA),
            &[("obs_installs", &format!("{PINNED_OBS_EXE},{extra}"))],
        ),
    );
    let t = write_state("stream_extra_install_826", &with_sha(STREAM_PINNED, SHA));
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        "--genlock-sha",
        &format!("imag={SHA}"),
    ]);
    assert_eq!(code, 20, "stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains("obs_installs") && stdout.contains("DRIFT"),
        "{stdout}"
    );
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
}

#[test]
fn gate_refuses_when_port_4455_is_owned_by_the_wrong_install_826() {
    // The exact 2026-07-27 incident, reproduced end-to-end through the gate.
    const SHA: &str = "26de1c3c23980488a110dbf02e5e472f15cb001d";
    let stale = r"D:\_APPS\_RETIRED_1ME-obs_2026-07-27\bin\64bit\obs64.exe";
    let s = write_state(
        "strih_port_squat_826",
        &with_obs_identity(
            &with_sha(STRIH_PINNED, SHA),
            &[
                ("port4455_owner_path", stale),
                ("port4455_owner_version", "31.1.2"),
            ],
        ),
    );
    let t = write_state("stream_port_squat_826", &with_sha(STREAM_PINNED, SHA));
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        "--genlock-sha",
        &format!("imag={SHA}"),
    ]);
    assert_eq!(code, 20, "stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains("port4455_identity") && stdout.contains("DRIFT"),
        "{stdout}"
    );
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
}

#[test]
fn gate_refuses_when_a_second_obs_class_process_is_running_826() {
    const SHA: &str = "26de1c3c23980488a110dbf02e5e472f15cb001d";
    let s = write_state(
        "strih_second_proc_826",
        &with_obs_identity(&with_sha(STRIH_PINNED, SHA), &[("obs_process_count", "2")]),
    );
    let t = write_state("stream_second_proc_826", &with_sha(STREAM_PINNED, SHA));
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        "--genlock-sha",
        &format!("imag={SHA}"),
    ]);
    assert_eq!(code, 20, "stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains("obs_process_count") && stdout.contains("DRIFT"),
        "{stdout}"
    );
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
}

#[test]
fn gate_refuses_when_strih_startup_chain_still_carries_the_dead_leftover_826() {
    const SHA: &str = "26de1c3c23980488a110dbf02e5e472f15cb001d";
    let s = write_state(
        "strih_startup_dead_826",
        &with_obs_identity(
            &with_sha(STRIH_PINNED, SHA),
            &[
                ("ahk_app1_shortcut_path", PINNED_SHORTCUT),
                ("ahk_app1_run", "1"),
                ("ahk_dead_config_present", "1"),
                ("shortcut_target_path", PINNED_OBS_EXE),
                ("shortcut_workdir", PINNED_OBS_WORKDIR),
            ],
        ),
    );
    let t = write_state("stream_startup_dead_826", &with_sha(STREAM_PINNED, SHA));
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        "--genlock-sha",
        &format!("imag={SHA}"),
    ]);
    assert_eq!(code, 20, "stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains("startup_chain") && stdout.contains("DRIFT"),
        "{stdout}"
    );
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
}

#[test]
fn gate_never_engages_startup_chain_for_a_box_with_no_ahk_826() {
    // stream has NO NL_STARTUP.ahk at all -- reporting obs_installs/port4455/process_count
    // (the generic per-box facets) must NOT also require the ahk-specific startup_chain facet.
    const SHA: &str = "26de1c3c23980488a110dbf02e5e472f15cb001d";
    let s = write_state("strih_ok_826", &with_sha(STRIH_PINNED, SHA));
    let t = write_state(
        "stream_no_ahk_826",
        &with_obs_identity(
            &with_sha(STREAM_PINNED, SHA),
            &[
                ("obs_installs", PINNED_OBS_EXE),
                ("port4455_owner_path", PINNED_OBS_EXE),
                ("port4455_owner_version", "32.1.2"),
                ("obs_process_count", "1"),
            ],
        ),
    );
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        "--genlock-sha",
        &format!("imag={SHA}"),
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert!(!stdout.contains("startup_chain"), "{stdout}");
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
}
