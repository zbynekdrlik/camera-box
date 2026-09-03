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
    run_gate_env(args, &[])
}

/// Run the gate as a subprocess WITH extra env (the #1137 vendor-pin fixture seams);
/// return (exit_code, stdout, stderr).
fn run_gate_env(args: &[&str], extra_env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(script());
    cmd.args(args).current_dir(manifest_dir());
    // #1137 hermeticity: the report-only vendor-pin section runs a live `git fetch origin` +
    // `git log origin/main` whenever VERSION_INTEGRITY_GATE_VENDOR_NEWEST is unset. Seed both seams
    // by default so the pre-existing subprocess tests stay OFFLINE (no fetch, no side-effect on the
    // shared checkout's refs) — a test that exercises the vendor pin overrides them via extra_env.
    // Defaults: a fixed newest sha + an empty pending list => the section reports OK, zero git.
    let has = |k: &str| extra_env.iter().any(|(ek, _)| *ek == k);
    if !has("VERSION_INTEGRITY_GATE_VENDOR_NEWEST") {
        cmd.env(
            "VERSION_INTEGRITY_GATE_VENDOR_NEWEST",
            "0000000hermetictest",
        );
    }
    if !has("VERSION_INTEGRITY_GATE_VENDOR_PENDING") {
        cmd.env("VERSION_INTEGRITY_GATE_VENDOR_PENDING", "");
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run version-integrity-gate.sh");
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
\"obs_version\":\"32.2.0\",\
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
\"obs_version\":\"32.2.0\",\
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
        lines.iter().any(|l| l == "obs_version=32.2.0"),
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
    // #829: the obs-identity facet is ENFORCED now, so a genuinely healthy fixture must also carry
    // the enforced 826 keys on every box (like it carries genlock_build_sha since #758).
    let s = write_state(
        "strih_pin",
        &with_obs_identity_ok(&with_sha(STRIH_PINNED, SHA), true),
    );
    let t = write_state(
        "stream_pin",
        &with_obs_identity_ok(&with_sha(STREAM_PINNED, SHA), false),
    );
    let (imag_m, imag_b) = clean_imag_bytes_1100("both");
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        "--genlock-sha",
        &format!("imag={SHA}"),
        "--imag-manifest",
        imag_m.to_str().unwrap(),
        "--imag-bytes",
        &imag_b,
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
    let _ = std::fs::remove_file(&imag_m);
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
    let s = write_state(
        "strih_parity_ok",
        &with_obs_identity_ok(&with_sha(STRIH_PINNED, SHA), true),
    );
    let t = write_state(
        "stream_parity_ok",
        &with_obs_identity_ok(&with_sha(STREAM_PINNED, SHA), false),
    );
    let (imag_m, imag_b) = clean_imag_bytes_1100("parity756");
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        "--genlock-sha",
        &format!("imag={SHA}"),
        "--imag-manifest",
        imag_m.to_str().unwrap(),
        "--imag-bytes",
        &imag_b,
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
    let _ = std::fs::remove_file(&imag_m);
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
        &with_obs_identity_ok(&with_sha(STRIH_PINNED, WIN_INCIDENT_SHA_949), true),
    );
    let t = write_state(
        "stream_949_incident",
        &with_obs_identity_ok(&with_sha(STREAM_PINNED, WIN_INCIDENT_SHA_949), false),
    );
    let (imag_m, imag_b) = clean_imag_bytes_1100("vendor949");
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        "--genlock-sha",
        &format!("imag={IMAG_INCIDENT_SHA_949}"),
        "--imag-manifest",
        imag_m.to_str().unwrap(),
        "--imag-bytes",
        &imag_b,
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
    let _ = std::fs::remove_file(&imag_m);
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
    // #949: the gate's own flow must compute the real offending paths end-to-end (not just at the
    // pure-decision layer, tested separately in tests/drift_guard.rs) — the issue explicitly asked
    // for a message naming the offending paths, "not just the SHAs".
    assert!(
        stdout.contains("vendor/obs-studio/libobs/obs.h"),
        "the gate must name a real offending path end-to-end, not just opaque SHAs: {stdout}"
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
    let drifted = STRIH_PINNED.replace("\"obs_version\":\"32.2.0\"", "\"obs_version\":\"31.0.0\"");
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
// #1067 — the pinned OBS version vendor/README.md carries for `vendor/obs-studio` (32.1.2). Used as
// the healthy `port4455_owner_version` now that port4455_identity is ENFORCED (a matching owner
// version passes port_identity_verdict; pinned_obs_version(readme) reads the same value).
const PINNED_OBS_VERSION: &str = "32.2.0";

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

/// The enforced #826/#1067 obs-identity keys for a healthy box: the generic per-box facets
/// (`obs_installs` = only the pinned exe, `obs_process_count` = 1) on every box, plus — on strih
/// only — the NL_STARTUP.ahk startup-chain resolving to the pinned install (`startup_chain` is
/// strih-scoped + unconditional after #829). When `port4455` is true (the default healthy set), a
/// pinned `port4455_owner_path`/`_version` is included too: #1067 fixed the bundle-state-server
/// gather context (WMI Win32_Process.ExecutablePath) and flipped `port4455_identity` from opt-in to
/// ENFORCED, so a fully-healthy fixture must now carry it (before #1067 it was deliberately omitted
/// — the deployed non-elevated task could not read the :4455 owner path).
fn obs_identity_ok_pairs(strih: bool, port4455: bool) -> Vec<(&'static str, &'static str)> {
    let mut pairs: Vec<(&str, &str)> =
        vec![("obs_installs", PINNED_OBS_EXE), ("obs_process_count", "1")];
    if port4455 {
        pairs.push(("port4455_owner_path", PINNED_OBS_EXE));
        pairs.push(("port4455_owner_version", PINNED_OBS_VERSION));
    }
    if strih {
        pairs.extend_from_slice(&[
            ("ahk_app1_shortcut_path", PINNED_SHORTCUT),
            ("ahk_app1_run", "1"),
            ("ahk_dead_config_present", "0"),
            ("shortcut_target_path", PINNED_OBS_EXE),
            ("shortcut_workdir", PINNED_OBS_WORKDIR),
        ]);
    }
    pairs
}

/// A healthy fixture that PASSES the ENFORCED gate — including the now-enforced pinned port4455
/// owner (#1067).
fn with_obs_identity_ok(base: &str, strih: bool) -> String {
    with_obs_identity(base, &obs_identity_ok_pairs(strih, true))
}

/// Like `with_obs_identity_ok` but OMITTING port4455 — used only by the #1067 enforcement test,
/// which proves a box healthy on every OTHER facet but not reporting the :4455 owner is now a
/// gate-blocking UNKNOWN (before #1067 this was the healthy shape; after, it must UNKNOWN-block).
fn with_obs_identity_ok_no_port4455(base: &str, strih: bool) -> String {
    with_obs_identity(base, &obs_identity_ok_pairs(strih, false))
}

#[test]
fn gate_refuses_when_a_reporting_box_omits_the_enforced_826_keys() {
    // #829 ENFORCED flip (the backward-INCOMPATIBLE half of the 756->758 two-step): `obs_installs`
    // + `obs_process_count` are now unconditional on EVERY box, so a box that omits them is a real
    // gate-blocking UNKNOWN (11), never the old silent skip. This replaces the former
    // opt-in-rollout backward-compatibility test (a box reporting NONE of the keys used to pass).
    const SHA: &str = "26de1c3c23980488a110dbf02e5e472f15cb001d";
    let s = write_state("strih_omit826", &with_sha(STRIH_PINNED, SHA));
    let t = write_state("stream_omit826", &with_sha(STREAM_PINNED, SHA));
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        "--genlock-sha",
        &format!("imag={SHA}"),
    ]);
    assert_eq!(
        code, 11,
        "a box omitting the enforced obs-identity keys must be UNKNOWN (11), not a silent pass. \
         stdout={stdout} stderr={stderr}"
    );
    assert!(!stdout.contains("GATE PASS"), "must not pass: {stdout}");
    assert!(
        stdout.contains("obs_installs") && stdout.contains("UNKNOWN"),
        "obs_installs must engage unconditionally and report UNKNOWN: {stdout}"
    );
    assert!(
        stdout.contains("obs_process_count") && stdout.contains("UNKNOWN"),
        "obs_process_count must engage unconditionally and report UNKNOWN: {stdout}"
    );
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
}

#[test]
fn gate_enforces_startup_chain_on_strih_even_without_ahk_826() {
    // #829: startup_chain is now strih-scoped + UNCONDITIONAL (re-keyed off the box identity, not
    // ahk-presence). A strih box healthy on every OTHER facet but omitting the NL_STARTUP.ahk
    // startup-chain keys is a gate-blocking UNKNOWN -- strih MUST run NL_STARTUP.ahk, so an
    // unreported chain is "unread", never "not applicable". (A healthy port4455 is included here
    // so the ONLY signal is the missing startup chain: clean 0-under-old -> 11-under-#829.)
    const SHA: &str = "26de1c3c23980488a110dbf02e5e472f15cb001d";
    let s = write_state(
        "strih_no_ahk_enforced_829",
        &with_obs_identity(
            &with_sha(STRIH_PINNED, SHA),
            &[
                ("obs_installs", PINNED_OBS_EXE),
                ("port4455_owner_path", PINNED_OBS_EXE),
                ("port4455_owner_version", "32.2.0"),
                ("obs_process_count", "1"),
            ],
        ),
    );
    let t = write_state(
        "stream_ok_for_strih_ahk_829",
        &with_obs_identity_ok(&with_sha(STREAM_PINNED, SHA), false),
    );
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        "--genlock-sha",
        &format!("imag={SHA}"),
    ]);
    assert_eq!(
        code, 11,
        "strih omitting the startup-chain keys must be UNKNOWN (11) under strih-scoped \
         enforcement. stdout={stdout} stderr={stderr}"
    );
    assert!(
        stderr.contains("strih:startup_chain") || stdout.contains("startup_chain"),
        "strih's startup_chain must engage + report UNKNOWN: stdout={stdout} stderr={stderr}"
    );
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
}

#[test]
fn gate_enforces_port4455_identity_when_unreported_1067() {
    // #1067: port4455_identity is now ENFORCED (its opt-in guard removed) because the bundle-state-
    // server gather context was fixed (WMI Win32_Process.ExecutablePath is readable from the
    // non-elevated task where Get-Process.Path was access-denied). A box healthy on every OTHER
    // facet but NOT reporting the :4455 owner path is now a gate-blocking UNKNOWN (11), never the
    // old silent opt-in skip. This is the exact flip of the former
    // gate_keeps_port4455_identity_opt_in_when_unreported_826 (the #829 opt-in landing) — the same
    // 756->758 second step, applied to the last obs-identity facet.
    const SHA: &str = "26de1c3c23980488a110dbf02e5e472f15cb001d";
    // strih healthy on obs_installs / obs_process_count / startup_chain but OMITTING port4455 -> the
    // ONLY missing signal is the now-enforced :4455 owner.
    let s = write_state(
        "strih_no_port_1067",
        &with_obs_identity_ok_no_port4455(&with_sha(STRIH_PINNED, SHA), true),
    );
    // stream fully healthy (incl. the enforced port4455) so it contributes no UNKNOWN of its own.
    let t = write_state(
        "stream_ok_1067",
        &with_obs_identity_ok(&with_sha(STREAM_PINNED, SHA), false),
    );
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        "--genlock-sha",
        &format!("imag={SHA}"),
    ]);
    assert_eq!(
        code, 11,
        "a box omitting the now-enforced port4455 owner must be UNKNOWN (11), not a silent pass. \
         stdout={stdout} stderr={stderr}"
    );
    assert!(!stdout.contains("GATE PASS"), "must not pass: {stdout}");
    assert!(
        stdout.contains("port4455_identity") && stdout.contains("UNKNOWN"),
        "port4455_identity must engage unconditionally + report UNKNOWN: {stdout}"
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
    // stream runs NO NL_STARTUP.ahk. After #829's strih-scoping, startup_chain is keyed off the box
    // IDENTITY, so stream never engages it and its absent ahk keys stay OK, never UNKNOWN. (strih,
    // which does run it, is healthy here so the whole gate still passes.)
    const SHA: &str = "26de1c3c23980488a110dbf02e5e472f15cb001d";
    let s = write_state(
        "strih_ok_826",
        &with_obs_identity_ok(&with_sha(STRIH_PINNED, SHA), true),
    );
    let t = write_state(
        "stream_no_ahk_826",
        &with_obs_identity(
            &with_sha(STREAM_PINNED, SHA),
            &[
                ("obs_installs", PINNED_OBS_EXE),
                ("port4455_owner_path", PINNED_OBS_EXE),
                ("port4455_owner_version", "32.2.0"),
                ("obs_process_count", "1"),
            ],
        ),
    );
    let (imag_m, imag_b) = clean_imag_bytes_1100("noahk826");
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        "--genlock-sha",
        &format!("imag={SHA}"),
        "--imag-manifest",
        imag_m.to_str().unwrap(),
        "--imag-bytes",
        &imag_b,
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert!(
        !stderr.contains("stream:startup_chain"),
        "stream (no NL_STARTUP.ahk) must never engage startup_chain: stderr={stderr}"
    );
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
    let _ = std::fs::remove_file(&imag_m);
}

// ── #770: byte-derived DistroAV/libobs parity — the [0/8] gate compares the DEPLOYED plugin/core
// BYTES against the #120 BUNDLE_MANIFEST, not just the hand-written GENLOCK_BUILD_SHA marker ─────
//
// The wrong-direction #119/#767 hole: a box whose marker advanced to build X while its DLL bytes
// are an OLDER build passes the marker-only cross-box parity (all markers agree on X) — the byte
// compare that would catch it never ran because the box state carried no byte sha256. Now that
// bundle_state_gather emits obs_dll_sha256 / distroav_dll_sha256 (via component_sha256), the gate
// threads them to drift-guard --compare, which compares them against the authoritative manifest for
// build X. These fixtures prove the whole state->gate->engine path end to end (the same --win-state
// pattern as the #756 parity fixtures), with NO live rig.

/// The genlock build-unique capability marker a real OBS log emits — makes drift_check_capability
/// read OK (a manifest= supplied ALWAYS activates that check alongside the byte facet).
const GENLOCK_CAP_770: &str = "genlock: wall-clock-slaved render tick ENABLED";

/// Inject the #770 byte-derived facet keys (obs_dll_sha256, distroav_dll_sha256, genlock_capability)
/// into a pinned state fixture — same insert-before-closing-brace shape as `with_sha`, so it chains
/// with `with_sha` / `with_obs_identity_ok`.
fn with_manifest_facet(base: &str, obs_sha: &str, distroav_sha: &str, capability: &str) -> String {
    format!(
        "{},\"obs_dll_sha256\":\"{obs_sha}\",\"distroav_dll_sha256\":\"{distroav_sha}\",\"genlock_capability\":\"{capability}\"}}",
        &base[..base.len() - 1]
    )
}

/// Write a minimal #120 BUNDLE_MANIFEST.json listing obs.dll + distroav.dll (the two genlock-bearing
/// DLLs drift-guard's #122 by-basename component check reads). One-line files[] entries, exactly the
/// shape genlock-manifest.sh emits + drift-guard's manifest_sha_for_component parses.
fn write_manifest(name: &str, obs_sha: &str, distroav_sha: &str) -> PathBuf {
    let json = format!(
        "{{\n  \"schema\": \"camera-box/genlock-bundle-manifest@1\",\n  \"build_sha\": \"deadbeefdeadbeefdeadbeefdeadbeefdeadbeef\",\n  \"files\": [\n    {{ \"path\": \"bin/64bit/obs.dll\", \"sha256\": \"{obs_sha}\", \"size\": 100 }},\n    {{ \"path\": \"obs-plugins/64bit/distroav.dll\", \"sha256\": \"{distroav_sha}\", \"size\": 200 }}\n  ]\n}}\n"
    );
    write_state(name, &json)
}

#[test]
fn gate_refuses_when_deployed_obs_bytes_mismatch_the_manifest_770() {
    // The anti-#119 core: EVERYTHING else agrees — the marketing versions, the fps, the
    // genlock_build_sha MARKER (parity OK across the fleet), the obs-identity, even the capability
    // marker — but strih's DEPLOYED obs.dll BYTES do not match the authoritative manifest for that
    // build. The marker-only parity facet cannot see this; the byte facet MUST, and REFUSE (exit 20)
    // naming the drifted component (obs_dll_sha256) under strih's box section.
    const SHA: &str = "26de1c3c23980488a110dbf02e5e472f15cb001d";
    const OBS_MANIFEST_SHA: &str =
        "1111111111111111111111111111111111111111111111111111111111111111";
    const DISTROAV_MANIFEST_SHA: &str =
        "2222222222222222222222222222222222222222222222222222222222222222";
    const OBS_STALE_SHA: &str = "9999999999999999999999999999999999999999999999999999999999999999";
    let manifest = write_manifest("bundle_770_drift", OBS_MANIFEST_SHA, DISTROAV_MANIFEST_SHA);
    // strih: stale obs.dll bytes (the wrong-direction hole), distroav bytes correct.
    let s = write_state(
        "strih_bytes_drift_770",
        &with_obs_identity_ok(
            &with_manifest_facet(
                &with_sha(STRIH_PINNED, SHA),
                OBS_STALE_SHA,
                DISTROAV_MANIFEST_SHA,
                GENLOCK_CAP_770,
            ),
            true,
        ),
    );
    // stream: bytes correct — isolates the DRIFT to strih's obs.dll.
    let t = write_state(
        "stream_bytes_ok_770",
        &with_obs_identity_ok(
            &with_manifest_facet(
                &with_sha(STREAM_PINNED, SHA),
                OBS_MANIFEST_SHA,
                DISTROAV_MANIFEST_SHA,
                GENLOCK_CAP_770,
            ),
            false,
        ),
    );
    let (code, stdout, stderr) = run_gate(&[
        "--manifest",
        manifest.to_str().unwrap(),
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        "--genlock-sha",
        &format!("imag={SHA}"),
    ]);
    assert_eq!(
        code, 20,
        "stale deployed obs.dll bytes (marker/version all agree) must REFUSE with DRIFT (20). \
         stdout={stdout} stderr={stderr}"
    );
    let all = format!("{stdout}{stderr}");
    assert!(
        all.contains("obs_dll_sha256") && all.contains("DRIFT"),
        "must name the drifted component (obs_dll_sha256) as DRIFT: {all}"
    );
    assert!(
        all.contains("strih"),
        "must attribute the byte drift to the strih box: {all}"
    );
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
    let _ = std::fs::remove_file(&manifest);
}

#[test]
fn gate_passes_when_deployed_bytes_match_the_manifest_770() {
    // Both boxes' deployed obs.dll + distroav.dll bytes match the authoritative manifest for the
    // fleet's build -> the byte facet is OK on every box, and (with versions/fps/identity/parity all
    // pinned) the whole gate PASSES. This is the marker-as-pointer end state: the truth is the bytes.
    const SHA: &str = "26de1c3c23980488a110dbf02e5e472f15cb001d";
    const OBS_MANIFEST_SHA: &str =
        "1111111111111111111111111111111111111111111111111111111111111111";
    const DISTROAV_MANIFEST_SHA: &str =
        "2222222222222222222222222222222222222222222222222222222222222222";
    let manifest = write_manifest("bundle_770_ok", OBS_MANIFEST_SHA, DISTROAV_MANIFEST_SHA);
    let s = write_state(
        "strih_bytes_ok_770",
        &with_obs_identity_ok(
            &with_manifest_facet(
                &with_sha(STRIH_PINNED, SHA),
                OBS_MANIFEST_SHA,
                DISTROAV_MANIFEST_SHA,
                GENLOCK_CAP_770,
            ),
            true,
        ),
    );
    let t = write_state(
        "stream_bytes_ok_770_pass",
        &with_obs_identity_ok(
            &with_manifest_facet(
                &with_sha(STREAM_PINNED, SHA),
                OBS_MANIFEST_SHA,
                DISTROAV_MANIFEST_SHA,
                GENLOCK_CAP_770,
            ),
            false,
        ),
    );
    let (imag_m, imag_b) = clean_imag_bytes_1100("bytes770");
    let (code, stdout, stderr) = run_gate(&[
        "--manifest",
        manifest.to_str().unwrap(),
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        "--genlock-sha",
        &format!("imag={SHA}"),
        "--imag-manifest",
        imag_m.to_str().unwrap(),
        "--imag-bytes",
        &imag_b,
    ]);
    assert_eq!(
        code, 0,
        "matching deployed bytes + pinned set must PASS. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout: {stdout}");
    assert!(
        stdout.contains("obs_dll_sha256") && stdout.contains("OK"),
        "the byte facet must have engaged + reported OK: {stdout}"
    );
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
    let _ = std::fs::remove_file(&imag_m);
    let _ = std::fs::remove_file(&manifest);
}

// ── #1082 byte-parity follow-up to #770: the imag (Linux) box's DEPLOYED .so BYTES are compared
// against its CI-authoritative linux BUNDLE_MANIFEST, closing the gap #770 left. #770 wired the
// WINDOWS obs.dll/distroav.dll byte compare (via drift-guard's by-basename manifest_sha_for_component);
// imag is threaded into the gate only by its MARKER (--genlock-sha), never its bytes, and the engine's
// component resolver knows only the Windows DLL basenames. The gate now takes a TARGETED per-.so
// facet (--imag-manifest + --imag-bytes) that resolves each gathered .so path via manifest_sha_for_path
// (the linux resolver) — a per-path compare, NOT the whole-bundle walk, so a partial 3-file ssh gather
// never flips the gate UNKNOWN. ENFORCED (#758-shape, #1100): an absent gather/manifest is a
// gate-blocking UNKNOWN, so every box must report its .so bytes (the #1082-part-3 flip, landed in #1100).

const LIBOBS_SO_PATH_1082: &str = "lib/x86_64-linux-gnu/libobs.so.30";
const DISTROAV_SO_PATH_1082: &str = "lib/x86_64-linux-gnu/obs-plugins/distroav.so";

/// Write a minimal #120 linux BUNDLE_MANIFEST.json listing the two genlock-bearing .so files, one
/// files[] entry per LINE (drift-guard's manifest parsers are line-based — a single-line JSON makes
/// the greedy `.*` grab the wrong entry, see the version-integrity-gate playbook).
fn write_linux_manifest(name: &str, libobs_sha: &str, distroav_sha: &str) -> PathBuf {
    let json = format!(
        "{{\n  \"schema\": \"camera-box/genlock-bundle-manifest@1\",\n  \"build_sha\": \"deadbeefdeadbeefdeadbeefdeadbeefdeadbeef\",\n  \"files\": [\n    {{ \"path\": \"{LIBOBS_SO_PATH_1082}\", \"sha256\": \"{libobs_sha}\", \"size\": 100 }},\n    {{ \"path\": \"{DISTROAV_SO_PATH_1082}\", \"sha256\": \"{distroav_sha}\", \"size\": 200 }}\n  ]\n}}\n"
    );
    write_state(name, &json)
}

/// Build a clean, PASSING fleet (strih/stream pinned + obs-identity + a matching genlock_build_sha,
/// imag's marker matching via --genlock-sha) so the ONLY signal a byte test can move is the imag .so
/// facet. Returns (strih_state, stream_state) paths.
fn clean_fleet_states_1082(sha: &str, tag: &str) -> (PathBuf, PathBuf) {
    // `tag` = caller-unique suffix: the _1082 tests all share one SHA const, run in PARALLEL
    // threads of ONE process (same pid dir), and each removes its state files at the end -- a
    // shared name lets test A's cleanup delete the file test B's gate subprocess is about to
    // read (observed as a strih UNKNOWN flake on the CI coverage job, 2026-08-18).
    let s = write_state(
        &format!("strih_1082_{}_{}", &sha[..8], tag),
        &with_obs_identity_ok(&with_sha(STRIH_PINNED, sha), true),
    );
    let t = write_state(
        &format!("stream_1082_{}_{}", &sha[..8], tag),
        &with_obs_identity_ok(&with_sha(STREAM_PINNED, sha), false),
    );
    (s, t)
}

/// #1100 — the imag .so byte facet is now ENFORCED, so a GATE-PASS fixture must ALSO carry a matching
/// imag manifest + gathered bytes (the imag analogue of with_sha / with_obs_identity_ok's enforced-key
/// injection). Writes a clean linux manifest and returns (manifest_path, imag_bytes_csv) whose bytes
/// match it -> imag_bytes_verdict OK. `tag` = caller-unique suffix (parallel tests share one pid dir).
fn clean_imag_bytes_1100(tag: &str) -> (PathBuf, String) {
    const LIBOBS: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const DISTROAV: &str = "2222222222222222222222222222222222222222222222222222222222222222";
    let manifest = write_linux_manifest(&format!("imag_clean_1100_{tag}"), LIBOBS, DISTROAV);
    let csv = format!("imag={LIBOBS_SO_PATH_1082}={LIBOBS},{DISTROAV_SO_PATH_1082}={DISTROAV}");
    (manifest, csv)
}

#[test]
fn gate_refuses_when_imag_so_bytes_mismatch_the_manifest_1082() {
    // imag's DEPLOYED libobs.so.30 bytes do NOT match the authoritative linux manifest for its build
    // (the wrong-direction #119 hole for imag: its marker agrees with the fleet, but the bytes are an
    // older lineage). The marker-only parity facet cannot see this; the new imag byte facet MUST, and
    // REFUSE (exit 20) naming the drifted .so + the imag box.
    const SHA: &str = "26de1c3c23980488a110dbf02e5e472f15cb001d";
    const LIBOBS_MANIFEST: &str =
        "1111111111111111111111111111111111111111111111111111111111111111";
    const DISTROAV_MANIFEST: &str =
        "2222222222222222222222222222222222222222222222222222222222222222";
    const LIBOBS_STALE: &str = "9999999999999999999999999999999999999999999999999999999999999999";
    let manifest =
        write_linux_manifest("imag_bundle_1082_drift", LIBOBS_MANIFEST, DISTROAV_MANIFEST);
    let (s, t) = clean_fleet_states_1082(SHA, "refuses_when_imag_so_bytes_mismatch_the_");
    // imag: stale libobs.so.30 bytes, distroav.so bytes correct — isolates the DRIFT to libobs.so.30.
    let imag_bytes = format!(
        "imag={LIBOBS_SO_PATH_1082}={LIBOBS_STALE},{DISTROAV_SO_PATH_1082}={DISTROAV_MANIFEST}"
    );
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        "--genlock-sha",
        &format!("imag={SHA}"),
        "--imag-manifest",
        manifest.to_str().unwrap(),
        "--imag-bytes",
        &imag_bytes,
    ]);
    assert_eq!(
        code, 20,
        "stale deployed imag libobs.so.30 bytes (marker agrees) must REFUSE with DRIFT (20). \
         stdout={stdout} stderr={stderr}"
    );
    let all = format!("{stdout}{stderr}");
    assert!(
        all.contains("imag_so_bytes") && all.contains("DRIFT"),
        "must name the imag byte facet as DRIFT: {all}"
    );
    assert!(
        all.contains("libobs.so.30") && all.contains("imag"),
        "must name the drifted .so + attribute it to imag: {all}"
    );
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
    let _ = std::fs::remove_file(&manifest);
}

#[test]
fn gate_passes_when_imag_so_bytes_match_the_manifest_1082() {
    // imag's deployed libobs.so.30 + distroav.so bytes match the authoritative linux manifest -> the
    // imag byte facet is OK, and (with the fleet otherwise pinned) the whole gate PASSES. The
    // marker-as-pointer end state for imag: the truth is the bytes.
    const SHA: &str = "26de1c3c23980488a110dbf02e5e472f15cb001d";
    const LIBOBS_MANIFEST: &str =
        "1111111111111111111111111111111111111111111111111111111111111111";
    const DISTROAV_MANIFEST: &str =
        "2222222222222222222222222222222222222222222222222222222222222222";
    let manifest = write_linux_manifest("imag_bundle_1082_ok", LIBOBS_MANIFEST, DISTROAV_MANIFEST);
    let (s, t) = clean_fleet_states_1082(SHA, "passes_when_imag_so_bytes_match_the_mani");
    let imag_bytes = format!(
        "imag={LIBOBS_SO_PATH_1082}={LIBOBS_MANIFEST},{DISTROAV_SO_PATH_1082}={DISTROAV_MANIFEST}"
    );
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        "--genlock-sha",
        &format!("imag={SHA}"),
        "--imag-manifest",
        manifest.to_str().unwrap(),
        "--imag-bytes",
        &imag_bytes,
    ]);
    assert_eq!(
        code, 0,
        "matching imag .so bytes + pinned fleet must PASS. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout: {stdout}");
    assert!(
        stdout.contains("imag_so_bytes") && stdout.contains("OK"),
        "the imag byte facet must have engaged + reported OK: {stdout}"
    );
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
    let _ = std::fs::remove_file(&manifest);
}

#[test]
fn gate_refuses_when_imag_so_bytes_not_gathered_1100() {
    // #1100 ENFORCE (was gate_stays_dormant_..._1082): the imag .so byte facet is now enforced
    // (#758-shape) — its live gather is deployed + verified on the rig. A manifest auto-sourced but
    // an EMPTY imag .so gather (--imag-bytes empty) is no longer a silent DORMANT skip; it is a
    // gate-blocking UNKNOWN (11), so a live ssh gather failure REFUSES the run rather than passing a
    // run whose imag bytes were never verified. The exact flip of the former opt-in behavior, the
    // same 756->758 second step #1067 applied to port4455_identity.
    const SHA: &str = "26de1c3c23980488a110dbf02e5e472f15cb001d";
    let manifest = write_linux_manifest(
        "imag_bundle_1100_empty",
        "1111111111111111111111111111111111111111111111111111111111111111",
        "2222222222222222222222222222222222222222222222222222222222222222",
    );
    let (s, t) = clean_fleet_states_1082(SHA, "refuses_when_imag_so_bytes_not_gathered_1");
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        "--genlock-sha",
        &format!("imag={SHA}"),
        "--imag-manifest",
        manifest.to_str().unwrap(),
        "--imag-bytes",
        "imag=",
    ]);
    assert_eq!(
        code, 11,
        "an empty imag byte gather must now REFUSE as UNKNOWN (11), not silently pass. \
         stdout={stdout} stderr={stderr}"
    );
    assert!(!stdout.contains("GATE PASS"), "must not pass: {stdout}");
    let all = format!("{stdout}{stderr}");
    assert!(
        all.contains("imag_so_bytes") && all.contains("UNKNOWN"),
        "the facet must report UNKNOWN (enforced), not DORMANT: {all}"
    );
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
    let _ = std::fs::remove_file(&manifest);
}

#[test]
fn gate_unknown_when_imag_so_path_not_in_manifest_1082() {
    // A gathered .so path the manifest does not list is UNKNOWN (never a false clean) -> the gate is
    // INCOMPLETE (11), exactly the never-false-clean discipline every other facet enforces.
    const SHA: &str = "26de1c3c23980488a110dbf02e5e472f15cb001d";
    let manifest = write_linux_manifest(
        "imag_bundle_1082_unknown",
        "1111111111111111111111111111111111111111111111111111111111111111",
        "2222222222222222222222222222222222222222222222222222222222222222",
    );
    let (s, t) = clean_fleet_states_1082(SHA, "unknown_when_imag_so_path_not_in_manifes");
    // A path NOT in the manifest -> UNKNOWN.
    let imag_bytes = "imag=lib/x86_64-linux-gnu/libobs-opengl.so.30=abcabcabcabcabcabcabcabcabcabcabcabcabcabcabcabcabcabcabcabcabca";
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        "--genlock-sha",
        &format!("imag={SHA}"),
        "--imag-manifest",
        manifest.to_str().unwrap(),
        "--imag-bytes",
        imag_bytes,
    ]);
    assert_eq!(
        code, 11,
        "a gathered .so not listed in the manifest must be UNKNOWN/INCOMPLETE (11). \
         stdout={stdout} stderr={stderr}"
    );
    let all = format!("{stdout}{stderr}");
    assert!(
        all.contains("imag_so_bytes") && all.contains("UNKNOWN"),
        "must report the imag byte facet UNKNOWN: {all}"
    );
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
    let _ = std::fs::remove_file(&manifest);
}

#[test]
fn gate_enforces_imag_bytes_when_unreported_1100() {
    // #1100: the imag .so byte facet is ENFORCED (its opt-in main() guard removed) now that the live
    // gather is deployed + verified on the rig. A fleet healthy on every OTHER facet but supplying NO
    // imag bytes/manifest at all is a gate-blocking UNKNOWN (11), never the old silent opt-in skip.
    // The exact flip of the former DORMANT behavior, mirroring gate_enforces_port4455_identity_..._1067.
    const SHA: &str = "26de1c3c23980488a110dbf02e5e472f15cb001d";
    let (s, t) = clean_fleet_states_1082(SHA, "enforces_imag_bytes_when_unreported_1100_");
    // No --imag-manifest / --imag-bytes at all: the facet must engage unconditionally and UNKNOWN.
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        "--genlock-sha",
        &format!("imag={SHA}"),
    ]);
    assert_eq!(
        code, 11,
        "a fleet reporting no imag bytes must be UNKNOWN (11), not a silent pass. \
         stdout={stdout} stderr={stderr}"
    );
    assert!(!stdout.contains("GATE PASS"), "must not pass: {stdout}");
    assert!(
        stdout.contains("imag_so_bytes") && stdout.contains("UNKNOWN"),
        "imag byte facet must engage unconditionally + report UNKNOWN: {stdout}"
    );
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
}

// ---------------------------------------------------------------------------
// #1137 — genlock_vendor_pin_verdict: the report-only vendor-pin ALARM layer.
// The existing genlock check is CROSS-BOX PARITY only (drift-guard.sh #756/#949) — it passes a
// UNIFORMLY-stale fleet (every box agrees on an OLD build). This layer PINS the deployed
// genlock_build_sha to the newest origin/main commit touching vendor/**, and SCREAMS (report-only,
// never flips the gate exit) when the deployed bundle lags — the #1136 early-gate-pin-doctrine
// orphan class. Fail-closed-LOUD on UNKNOWN. Pure function, unit-tested by sourcing.
// ---------------------------------------------------------------------------

#[test]
fn vendor_pin_ok_when_deployed_at_newest_vendor_head() {
    let out = run_sourced(
        r#"o="$(genlock_vendor_pin_verdict "46d868a29a7e" "46d868a29a7e" "")"; rc=$?; printf '%s\n' "$o"; echo "RC=$rc""#,
        &[],
    );
    assert!(
        out.contains("RC=0"),
        "current bundle must be OK (rc 0): {out}"
    );
    assert!(out.contains("OK"), "must report OK: {out}");
    assert!(
        !out.to_uppercase().contains("ALARM") && !out.contains("UNKNOWN"),
        "a current bundle must not alarm: {out}"
    );
}

#[test]
fn vendor_pin_alarm_names_pending_commits() {
    // The exact #1137 live scenario: deployed 03cd9c073 with 2 undeployed #1097 vendor commits.
    let out = run_sourced(
        r#"pend="$(printf 'f70317e81 fix(#1097): [green] framesync_create failure retries in place\n2386b60d9 docs(#1097): [review] correct the retry-cleanup comment')"; o="$(genlock_vendor_pin_verdict "03cd9c073" "2386b60d9" "$pend")"; rc=$?; printf '%s\n' "$o"; echo "RC=$rc""#,
        &[],
    );
    assert!(
        out.contains("RC=30"),
        "a lagging bundle must return the ALARM code 30: {out}"
    );
    assert!(
        out.to_uppercase().contains("ALARM"),
        "must SCREAM ALARM: {out}"
    );
    assert!(
        out.contains("f70317e81") && out.contains("2386b60d9"),
        "the alarm MUST name the pending vendor commits: {out}"
    );
    assert!(
        out.contains("2 undeployed"),
        "must state the count (2 undeployed vendor commits): {out}"
    );
}

#[test]
fn vendor_pin_unknown_when_deployed_sha_unread() {
    let out = run_sourced(
        r#"o="$(genlock_vendor_pin_verdict "" "2386b60d9" "")"; rc=$?; printf '%s\n' "$o"; echo "RC=$rc""#,
        &[],
    );
    assert!(
        out.contains("RC=31"),
        "an unread deployed SHA must fail-closed to UNKNOWN (31): {out}"
    );
    assert!(
        out.contains("UNKNOWN"),
        "must report UNKNOWN (never a silent OK): {out}"
    );
}

#[test]
fn vendor_pin_unknown_when_newest_vendor_unresolved() {
    let out = run_sourced(
        r#"o="$(genlock_vendor_pin_verdict "46d868a29a7e" "" "")"; rc=$?; printf '%s\n' "$o"; echo "RC=$rc""#,
        &[],
    );
    assert!(
        out.contains("RC=31"),
        "an unresolved newest-vendor HEAD must fail-closed to UNKNOWN (31): {out}"
    );
    assert!(out.contains("UNKNOWN"), "must report UNKNOWN: {out}");
}

#[test]
fn vendor_pin_alarm_is_report_only_does_not_block_an_otherwise_clean_gate() {
    // #1137 — the report-only property: even when the deployed bundle LAGS origin/main's vendor
    // HEAD, an otherwise-clean gate must still PASS (exit 0). The vendor-pin layer SCREAMS but never
    // flips the gate exit (the coordinated-restart bundle deploy makes a hard block too blunt), so a
    // lagging bundle is loudly surfaced without halting every E2E. Fixture seams override the git
    // read so the flow is deterministic.
    const SHA: &str = "26de1c3c23980488a110dbf02e5e472f15cb001d";
    let s = write_state(
        "strih_vendorpin",
        &with_obs_identity_ok(&with_sha(STRIH_PINNED, SHA), true),
    );
    let t = write_state(
        "stream_vendorpin",
        &with_obs_identity_ok(&with_sha(STREAM_PINNED, SHA), false),
    );
    let (imag_m, imag_b) = clean_imag_bytes_1100("vendorpin");
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--win-state",
            &format!("strih={}", s.display()),
            "--win-state",
            &format!("stream={}", t.display()),
            "--genlock-sha",
            &format!("imag={SHA}"),
            "--imag-manifest",
            imag_m.to_str().unwrap(),
            "--imag-bytes",
            &imag_b,
        ],
        &[
            ("VERSION_INTEGRITY_GATE_VENDOR_NEWEST", "beefface1234"),
            (
                "VERSION_INTEGRITY_GATE_VENDOR_PENDING",
                "f70317e81 fix: framesync retries in place\n2386b60d9 docs: correct comment",
            ),
        ],
    );
    // Report-only: the lag does NOT block a green gate.
    assert_eq!(
        code, 0,
        "the vendor-pin ALARM must be REPORT-ONLY (must not flip the gate exit). stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("GATE PASS"),
        "gate must still pass: {stdout}"
    );
    // But it SCREAMS and names the pending commits.
    assert!(
        stdout.contains("vendor-pin alarm (#1137"),
        "the vendor-pin section must run: {stdout}"
    );
    assert!(
        stdout.contains("ALARM") && stdout.contains("f70317e81") && stdout.contains("2386b60d9"),
        "the ALARM must name the pending vendor commits: {stdout}"
    );
    assert!(
        stderr.contains("VENDOR-PIN ALARM"),
        "a loud stderr SCREAM banner must fire: {stderr}"
    );
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
    let _ = std::fs::remove_file(&imag_m);
}

#[test]
fn vendor_pin_ok_when_deployed_at_newest_vendor_head_flow() {
    // The clean case through the flow: deployed SHA == newest vendor HEAD (no pending) -> the
    // vendor-pin section reports OK and the gate passes.
    const SHA: &str = "26de1c3c23980488a110dbf02e5e472f15cb001d";
    let s = write_state(
        "strih_vendorpin_ok",
        &with_obs_identity_ok(&with_sha(STRIH_PINNED, SHA), true),
    );
    let t = write_state(
        "stream_vendorpin_ok",
        &with_obs_identity_ok(&with_sha(STREAM_PINNED, SHA), false),
    );
    let (imag_m, imag_b) = clean_imag_bytes_1100("vendorpin_ok");
    let (code, stdout, _stderr) = run_gate_env(
        &[
            "--win-state",
            &format!("strih={}", s.display()),
            "--win-state",
            &format!("stream={}", t.display()),
            "--genlock-sha",
            &format!("imag={SHA}"),
            "--imag-manifest",
            imag_m.to_str().unwrap(),
            "--imag-bytes",
            &imag_b,
        ],
        &[
            ("VERSION_INTEGRITY_GATE_VENDOR_NEWEST", SHA),
            ("VERSION_INTEGRITY_GATE_VENDOR_PENDING", ""),
        ],
    );
    assert_eq!(code, 0, "clean vendor pin must pass: {stdout}");
    assert!(
        stdout.contains("vendor_pin") && stdout.contains("OK"),
        "vendor_pin must report OK: {stdout}"
    );
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
    let _ = std::fs::remove_file(&imag_m);
}

// ---------------------------------------------------------------------------
// #1292 review follow-up — the SAME false-LAGS polarity trap the merged drift-guard.sh fix removed
// (imag_genlock_range_log's merge-base scoping) also existed HERE: the vendor-pin ALARM's own
// PENDING_LIST used to be computed via a PLAIN ancestry range (`vp_sha..origin/main`), which reads
// LAGGING for a deployed bundle that is genuinely AHEAD of origin/main on the dev candidate line (a
// release-candidate build). vendor_pin_range_log/vendor_pin_ahead_log/vendor_pin_on_dev mirror
// drift-guard.sh's imag_genlock_range_log/imag_genlock_ahead_log/imag_genlock_on_dev exactly, scoped
// to the WHOLE vendor/ tree; genlock_vendor_pin_verdict's extended AHEAD_LIST/ON_DEV args classify a
// recognized release-candidate build (OK) apart from an unrecognized orphan one (ALARM, rc 30 —
// report-only semantics unchanged).
// ---------------------------------------------------------------------------

#[test]
fn vendor_pin_ok_when_ahead_of_main_on_the_dev_line_1292() {
    // The exact false-ALARM this ticket's caller-side merge-base fix removes: a deployed bundle
    // that is genuinely AHEAD of origin/main on the dev candidate line (a recognized
    // release-candidate build) must be OK, never an ALARM.
    let out = run_sourced(
        r#"
            ahead="$(printf 'abc1234 fix(#1292): vendor change')"
            o="$(genlock_vendor_pin_verdict "46d868a29a7e" "beefface1234" "" "$ahead" "1")"
            rc=$?
            printf '%s\n' "$o"
            echo "RC=$rc"
        "#,
        &[],
    );
    assert!(
        out.contains("RC=0"),
        "an ahead-and-recognized bundle must be OK (rc 0): {out}"
    );
    assert!(out.contains("OK"), "must report OK: {out}");
    assert!(
        !out.to_uppercase().contains("ALARM"),
        "an ahead-and-recognized bundle must NOT alarm: {out}"
    );
}

#[test]
fn vendor_pin_orphan_alarm_when_ahead_but_unrecognized_1292() {
    // The AHEAD-but-UNRECOGNIZED case: vendor commits reachable from neither origin/main nor
    // origin/dev must still SCREAM (the early-gate-pin doctrine's "an orphan release must SCREAM"),
    // never a silent pass just because the bundle happens to be a content superset of main.
    let out = run_sourced(
        r#"
            ahead="$(printf 'abc1234 fix(#1292): vendor change\ndef5678 orphan change')"
            o="$(genlock_vendor_pin_verdict "46d868a29a7e" "beefface1234" "" "$ahead" "0")"
            rc=$?
            printf '%s\n' "$o"
            echo "RC=$rc"
        "#,
        &[],
    );
    assert!(
        out.contains("RC=30"),
        "an orphan bundle must return the report-only ALARM code 30 (unchanged semantics): {out}"
    );
    assert!(
        out.to_uppercase().contains("ALARM"),
        "must SCREAM ALARM: {out}"
    );
    assert!(out.contains("ORPHAN"), "must name the ORPHAN reason: {out}");
    assert!(
        out.contains("abc1234") && out.contains("def5678"),
        "the alarm MUST name the ahead vendor commits: {out}"
    );
}

fn run_git_vig(cwd: &std::path::Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("failed to run git {args:?} in {cwd:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} in {cwd:?} failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_git_out_vig(cwd: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("failed to run git {args:?} in {cwd:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} in {cwd:?} failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// #1292 review follow-up: a throwaway two-branch (main/dev) synthetic repo — the SAME DAG shape as
/// tests/drift_guard.rs's `build_two_branch_ahead_repo` (duplicated here rather than shared; each
/// `tests/*.rs` file in this repo is its own compilation unit with no shared test-support module —
/// see the top-level CLAUDE.md/`.claude/rules/ci-testing-gotchas.md`'s own "never pin against this
/// repo's live history" rule for WHY a synthetic repo is used at all: origin/main only ever GROWS
/// under this repo's two-branch workflow, so a test pinned to a real SHA's relationship with today's
/// live tip would silently stop holding the moment a new vendor-touching PR merges). `main` gains a
/// MERGE commit (M1, second parent = dev's earlier tip C3) whose vendor/ content is already a subset
/// of dev's LATER tip (C4) via C2->C3 — the case a deployed SHA=C4 must read as `OK ... AHEAD ...`,
/// never LAGGING. `C1` (main's own pre-merge base) is genuinely missing M1's content — the case that
/// must stay LAGGING. `C5` is a further dev-only commit built on C4 but never pushed to either
/// branch — the orphan-build case. Returns (origin tempdir guard, repo tempdir guard, repo path, C1,
/// C4, C5) — keep BOTH tempdir guards alive for the whole test body.
fn build_two_branch_ahead_repo_vig() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    PathBuf,
    String,
    String,
    String,
) {
    let origin_holder = tempfile::tempdir().expect("origin tempdir");
    let repo_holder = tempfile::tempdir().expect("repo tempdir");
    let origin = origin_holder.path().join("origin.git");
    let repo = repo_holder.path().to_path_buf();

    run_git_vig(
        origin_holder.path(),
        &["init", "--quiet", "--bare", origin.to_str().unwrap()],
    );
    run_git_vig(
        repo_holder.path(),
        &["init", "--quiet", repo.to_str().unwrap()],
    );
    run_git_vig(&repo, &["config", "user.email", "t@example.com"]);
    run_git_vig(&repo, &["config", "user.name", "T"]);
    run_git_vig(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );

    std::fs::create_dir_all(repo.join("vendor")).expect("mkdir vendor");
    std::fs::write(repo.join("vendor/a.txt"), "1\n").expect("write a.txt");
    run_git_vig(&repo, &["add", "-A"]);
    run_git_vig(&repo, &["commit", "-q", "-m", "C1: base vendor content"]);
    run_git_vig(&repo, &["branch", "-M", "main"]);
    run_git_vig(&repo, &["push", "-q", "origin", "main:main"]);
    let c1 = run_git_out_vig(&repo, &["rev-parse", "HEAD"]);

    run_git_vig(&repo, &["checkout", "-q", "-b", "dev"]);
    std::fs::write(repo.join("vendor/a.txt"), "2\n").expect("write a.txt");
    run_git_vig(&repo, &["add", "-A"]);
    run_git_vig(&repo, &["commit", "-q", "-m", "C2: dev-only vendor change"]);
    std::fs::write(repo.join("vendor/b.txt"), "1\n").expect("write b.txt");
    run_git_vig(&repo, &["add", "-A"]);
    run_git_vig(
        &repo,
        &["commit", "-q", "-m", "C3: dev-only vendor change 2"],
    );

    run_git_vig(&repo, &["checkout", "-q", "main"]);
    run_git_vig(
        &repo,
        &[
            "merge",
            "-q",
            "--no-ff",
            "dev",
            "-m",
            "M1: Merge pull request (dev->main)",
        ],
    );
    run_git_vig(&repo, &["push", "-q", "origin", "main:main"]);

    run_git_vig(&repo, &["checkout", "-q", "dev"]);
    std::fs::write(repo.join("vendor/a.txt"), "3\n").expect("write a.txt");
    run_git_vig(&repo, &["add", "-A"]);
    run_git_vig(
        &repo,
        &[
            "commit",
            "-q",
            "-m",
            "C4: dev-only NEW vendor change (post-merge)",
        ],
    );
    let c4 = run_git_out_vig(&repo, &["rev-parse", "HEAD"]);
    run_git_vig(&repo, &["push", "-q", "origin", "dev:dev"]);

    run_git_vig(&repo, &["checkout", "-q", "--detach", &c4]);
    std::fs::write(repo.join("vendor/a.txt"), "orphanchange\n").expect("write a.txt");
    run_git_vig(&repo, &["add", "-A"]);
    run_git_vig(
        &repo,
        &[
            "commit",
            "-q",
            "-m",
            "C5: orphan build, never pushed to either branch",
        ],
    );
    let c5 = run_git_out_vig(&repo, &["rev-parse", "HEAD"]);

    run_git_vig(&repo, &["fetch", "-q", "origin"]);

    (origin_holder, repo_holder, repo, c1, c4, c5)
}

#[test]
fn vendor_pin_range_log_merge_base_never_false_lags_for_a_bundle_ahead_of_main_1292() {
    let (_origin, _repo_holder, repo, _c1, c4, _c5) = build_two_branch_ahead_repo_vig();
    let out = run_sourced(
        r#"
            out="$(vendor_pin_range_log "$SYN_REPO" "$SYN_SHA")"
            rc=$?
            echo "RC=$rc"
            echo "OUT=[$out]"
        "#,
        &[
            ("SYN_REPO", repo.to_str().unwrap()),
            ("SYN_SHA", c4.as_str()),
        ],
    );
    assert!(
        out.contains("RC=0"),
        "merge-base range must resolve cleanly: {out:?}"
    );
    assert!(
        out.contains("OUT=[]"),
        "a bundle that is a content SUPERSET of main via independent dev lineage must read an EMPTY \
         (never-lagging) range — the exact #1292 false-LAGS fix for version-integrity-gate.sh's own \
         vendor-pin alarm: {out:?}"
    );
}

#[test]
fn vendor_pin_range_log_still_reports_a_genuinely_lagging_bundle_1292() {
    // The merge-base fix must never mask a REAL lag; it only removes the false positive on the
    // AHEAD direction. C1 (main's own pre-merge base) is genuinely missing M1's vendor content.
    let (_origin, _repo_holder, repo, c1, _c4, _c5) = build_two_branch_ahead_repo_vig();
    let out = run_sourced(
        r#"
            out="$(vendor_pin_range_log "$SYN_REPO" "$SYN_SHA")"
            rc=$?
            echo "RC=$rc"
            n="$(printf '%s\n' "$out" | grep -c . || true)"
            echo "N=$n"
        "#,
        &[
            ("SYN_REPO", repo.to_str().unwrap()),
            ("SYN_SHA", c1.as_str()),
        ],
    );
    assert!(
        out.contains("RC=0"),
        "range command must resolve cleanly: {out:?}"
    );
    assert!(
        out.contains("N=2"),
        "a genuinely-behind bundle must still report the real missing vendor commits, never an \
         empty range: {out:?}"
    );
}

#[test]
fn vendor_pin_ahead_log_and_on_dev_classify_a_release_candidate_build_1292() {
    let (_origin, _repo_holder, repo, _c1, c4, _c5) = build_two_branch_ahead_repo_vig();
    let out = run_sourced(
        r#"
            ahead="$(vendor_pin_ahead_log "$SYN_REPO" "$SYN_SHA")"
            echo "AHEAD_N=$(printf '%s\n' "$ahead" | grep -c . || true)"
            vendor_pin_on_dev "$SYN_REPO" "$SYN_SHA"
            echo "ON_DEV_RC=$?"
        "#,
        &[
            ("SYN_REPO", repo.to_str().unwrap()),
            ("SYN_SHA", c4.as_str()),
        ],
    );
    assert!(
        out.contains("AHEAD_N=1"),
        "C4 carries exactly one vendor commit ahead of origin/main (its own post-merge change): \
         {out:?}"
    );
    assert!(
        out.contains("ON_DEV_RC=0"),
        "C4 is reachable from origin/dev — a recognized release-candidate build: {out:?}"
    );
}

#[test]
fn vendor_pin_ahead_log_and_on_dev_flag_an_orphan_build_1292() {
    let (_origin, _repo_holder, repo, _c1, _c4, c5) = build_two_branch_ahead_repo_vig();
    let out = run_sourced(
        r#"
            ahead="$(vendor_pin_ahead_log "$SYN_REPO" "$SYN_SHA")"
            echo "AHEAD_N=$(printf '%s\n' "$ahead" | grep -c . || true)"
            vendor_pin_on_dev "$SYN_REPO" "$SYN_SHA"
            echo "ON_DEV_RC=$?"
        "#,
        &[
            ("SYN_REPO", repo.to_str().unwrap()),
            ("SYN_SHA", c5.as_str()),
        ],
    );
    assert!(
        out.contains("AHEAD_N=2"),
        "C5 carries C4's own vendor change plus its own orphan change ahead of origin/main: {out:?}"
    );
    assert!(
        out.contains("ON_DEV_RC=1"),
        "C5 was never pushed to either branch — must NOT be reachable from origin/dev: {out:?}"
    );
}

#[test]
fn vendor_pin_ahead_seam_flows_through_the_gate_to_ok_never_a_false_alarm_1292() {
    // Flow-level wiring proof (env fixture seams, no live git — the #1137 hermeticity model): a box
    // deployed at a bundle the CALLER computed as AHEAD-and-on-dev must reach the gate as OK, never
    // the report-only ALARM the pre-#1292 caller's plain ancestry range would have produced.
    const SHA: &str = "26de1c3c23980488a110dbf02e5e472f15cb001d";
    let s = write_state(
        "strih_vendorpin_ahead",
        &with_obs_identity_ok(&with_sha(STRIH_PINNED, SHA), true),
    );
    let t = write_state(
        "stream_vendorpin_ahead",
        &with_obs_identity_ok(&with_sha(STREAM_PINNED, SHA), false),
    );
    let (imag_m, imag_b) = clean_imag_bytes_1100("vendorpin_ahead");
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--win-state",
            &format!("strih={}", s.display()),
            "--win-state",
            &format!("stream={}", t.display()),
            "--genlock-sha",
            &format!("imag={SHA}"),
            "--imag-manifest",
            imag_m.to_str().unwrap(),
            "--imag-bytes",
            &imag_b,
        ],
        &[
            ("VERSION_INTEGRITY_GATE_VENDOR_NEWEST", "beefface1234"),
            ("VERSION_INTEGRITY_GATE_VENDOR_PENDING", ""),
            (
                "VERSION_INTEGRITY_GATE_VENDOR_AHEAD",
                "abc1234 fix(#1292): vendor change",
            ),
            ("VERSION_INTEGRITY_GATE_VENDOR_ON_DEV", "1"),
        ],
    );
    assert_eq!(
        code, 0,
        "an ahead-and-recognized bundle must still pass: {stdout}"
    );
    assert!(stdout.contains("GATE PASS"), "gate must pass: {stdout}");
    assert!(
        stdout.contains("vendor_pin")
            && stdout.contains("OK")
            && stdout.contains(
                "vendored vendor/** commit(s) AHEAD of origin/main on the dev candidate line"
            ),
        "vendor_pin must report the exact OK/AHEAD phrase, never ALARM: {stdout}"
    );
    assert!(
        !stdout.to_uppercase().contains("ALARM") && !stderr.contains("VENDOR-PIN ALARM"),
        "an ahead-and-recognized bundle must NEVER fire the ALARM stderr banner (or print ALARM \
         anywhere in stdout): {stdout} / {stderr}"
    );
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
    let _ = std::fs::remove_file(&imag_m);
}

// ── #1164 — an operator-acked, physically-absent imag must NOT UNKNOWN-refuse the whole gate.
// After the #1100 ENFORCED flip, an acked-offline imag (rig-fleet.txt `imag:…`, issue 1013) fed no
// SHA + no .so bytes made BOTH the cross-box genlock parity AND the imag .so byte facet UNKNOWN(11),
// refusing the whole E2E (run 32480962068: "2 box(es) UNKNOWN: genlock_parity imag:so_bytes") even
// though every OTHER imag site legally skips. The `--imag-acked-offline REASON` flag closes that gap
// WITHOUT weakening the fail-closed default — an absent imag still refuses unless explicitly acked.

#[test]
fn gate_skips_imag_facets_when_acked_offline_1164() {
    // The exact buggy call shape from run 32480962068 (an EMPTY imag SHA passed because imag was
    // unreachable) but WITH --imag-acked-offline: the gate must SKIP the imag .so byte facet (a LOUD
    // greppable line, counted ok, never UNKNOWN) AND drop the `imag` genlock-sha entry from parity
    // (which then certifies the remaining fleet strih+stream) -> GATE PASS (0). Proves BOTH halves.
    const SHA: &str = "26de1c3c23980488a110dbf02e5e472f15cb001d";
    let s = write_state(
        "strih_acked_1164",
        &with_obs_identity_ok(&with_sha(STRIH_PINNED, SHA), true),
    );
    let t = write_state(
        "stream_acked_1164",
        &with_obs_identity_ok(&with_sha(STREAM_PINNED, SHA), false),
    );
    let (code, stdout, stderr) = run_gate(&[
        "--win-state",
        &format!("strih={}", s.display()),
        "--win-state",
        &format!("stream={}", t.display()),
        // the current call site passes an EMPTY imag SHA when imag is unreachable:
        "--genlock-sha",
        "imag=",
        "--imag-acked-offline",
        "notebook-replacement-issue-1162-new-unit-2026-08-22",
    ]);
    assert_eq!(
        code, 0,
        "an acked-offline imag must not UNKNOWN-refuse the gate. stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("GATE PASS"),
        "the gate must PASS with imag acked offline: {stdout}"
    );
    assert!(
        stdout.contains("SKIPPED") && stdout.contains("imag acked offline"),
        "the imag .so byte facet must emit a LOUD SKIPPED line naming the acked reason: {stdout}"
    );
    assert!(
        stdout.contains("ONE genlock build"),
        "cross-box parity must certify strih+stream after dropping the acked imag entry: {stdout}"
    );
    let all = format!("{stdout}{stderr}");
    assert!(
        !all.contains("imag:so_bytes"),
        "imag:so_bytes must NOT be counted UNKNOWN when acked offline: {all}"
    );
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
}

#[test]
fn gate_still_refuses_absent_imag_without_the_ack_flag_1164() {
    // The fail-closed default guard: the SAME absent-imag inputs as the acked test above but WITHOUT
    // --imag-acked-offline must STILL refuse (11) -- the #1100 ENFORCED contract is unweakened; only
    // an explicit operator ack changes the verdict. This pins that the new flag is the ONLY escape.
    const SHA: &str = "26de1c3c23980488a110dbf02e5e472f15cb001d";
    let s = write_state(
        "strih_noack_1164",
        &with_obs_identity_ok(&with_sha(STRIH_PINNED, SHA), true),
    );
    let t = write_state(
        "stream_noack_1164",
        &with_obs_identity_ok(&with_sha(STREAM_PINNED, SHA), false),
    );
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
        "an absent imag WITHOUT the ack flag must still refuse (11), not pass. \
         stdout={stdout} stderr={stderr}"
    );
    assert!(!stdout.contains("GATE PASS"), "must not pass: {stdout}");
    let all = format!("{stdout}{stderr}");
    assert!(
        all.contains("imag_so_bytes") && all.contains("UNKNOWN"),
        "the byte facet must still enforce UNKNOWN when not acked: {all}"
    );
    // Independently pin the parity-drop's default-OFF behavior: without the flag, imag is STILL
    // counted in the cross-box parity (UNREAD), so parity stays UNKNOWN -- not silently dropped.
    assert!(
        stdout.contains("UNREAD: imag"),
        "without --imag-acked-offline, the parity-drop must be OFF -- imag must still be counted \
         UNREAD in the cross-box parity: {stdout}"
    );
    let _ = std::fs::remove_file(&s);
    let _ = std::fs::remove_file(&t);
}
