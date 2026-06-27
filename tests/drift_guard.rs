//! Behavioral guard for the drift-guard engine `scripts/drift-guard.sh` (#45).
//!
//! The user directive is that strih + stream must stay on the EXACT pinned versions + critical
//! settings that guarantee zero-loss functionality. `scripts/drift-guard.sh` is the deterministic
//! engine that decides "does this box match the pinned set?" — its pure core (parse the pinned
//! manifest, parse the live OBS-log/setting values, compare versions, flag drift vs UNKNOWN) must
//! be correct regardless of network/MCP state, because that is what decides whether a silently
//! drifted production box is caught or shipped. These tests source the REAL script (its
//! `BASH_SOURCE != $0` guard skips the executed flow) and exercise the pure functions directly,
//! plus run the script end-to-end for the `--check-pins` / `--compare` exit-code contracts — the
//! same convention as tests/av_stack_update.rs.
//!
//! The OBS-log fixtures below are the ACTUAL lines captured read-only from strih + stream on
//! 2026-06-14, so the parsers are proven against the real production log format, not a guess.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/drift-guard.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the script and run `body` (which may call the pure functions). Returns stdout.
fn run_sourced(body: &str, extra_env: &[(&str, &str)]) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}", body = body);
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&harness).env("SCRIPT", script());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "sourced harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Run the script as a subprocess; return (exit_code, stdout, stderr).
fn run_script(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(script())
        .args(args)
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run drift-guard.sh");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A representative manifest carrying the version table + the pinned-settings table.
const MANIFEST_FIXTURE: &str = "\
# vendor

| dir | upstream | version | imported as |
|---|---|---|---|
| `vendor/obs-studio` | github.com/obsproject/obs-studio | **32.1.2** (commit `fb4d98bf8`) | git subtree --squash |
| `vendor/distroav` | github.com/DistroAV/DistroAV | **6.2.1** (commit `038d9d6`) | git subtree --squash |
| NDI SDK headers | shipped inside DistroAV (`vendor/distroav/lib/ndi/`) | SDK v6 (plugin requires **NDI ≥ 6.3.0**) | part of the DistroAV tree |

| setting | pinned value | live source |
|---|---|---|
| `output_fps` | `30` | OBS log |
| `genlock_wall_clock` | `1` | OBS log |
| `ndi_input_latency` | `0` | obs-websocket GetInputSettings |
| `canonical_plugin_path` | `C:\\ProgramData\\obs-studio\\plugins\\distroav\\bin\\64bit` | Get-ChildItem the OBS scan paths |
";

/// The ACTUAL OBS log lines captured from strih/stream 2026-06-14. Note the graphics-adapter
/// `fps:` line (60/1 here) precedes the `video settings reset:` block whose `fps:` (30/1) is the
/// real OUTPUT fps — the parser must pick 30, not 60.
const OBS_LOG_FIXTURE: &str = "\
11:40:39.048: Qt Version: 6.8.3 (runtime), 6.8.3 (compiled)
11:40:39.376: OBS 32.1.2 (64-bit, windows)
11:40:39.512: \t  Driver Version: 32.0.15.9144
11:40:39.714: \tfps:               60/1
11:40:39.714: video settings reset:
11:40:39.714: \tbase resolution:   1920x1080
11:40:39.714: \tfps:               30/1
11:40:39.718: genlock: wall-clock-slaved render tick ENABLED (OBS_GENLOCK_WALL_CLOCK, slew cap 2000000 ns/tick)
11:40:40.092: [distroav] obs_module_load: you can haz DistroAV (Version 6.2.1)
11:37:09.025: [distroav] NDI Library Version detected: 6.3.2.0
11:37:09.027: [distroav] plugin loaded (full NDI features) (version 6.2.1)
";

fn write_temp(name: &str, body: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("{name}_{}.md", std::process::id()));
    std::fs::write(&p, body).expect("write temp file");
    p
}

#[test]
fn parses_pinned_versions_and_settings_from_manifest() {
    let readme = write_temp("dg_manifest", MANIFEST_FIXTURE);
    let env = [("README", readme.to_str().unwrap())];
    let got = |f: &str| {
        run_sourced(&format!("{f} \"$README\""), &env)
            .trim()
            .to_string()
    };

    assert_eq!(got("pinned_obs_version"), "32.1.2");
    assert_eq!(got("pinned_distroav_version"), "6.2.1");
    assert_eq!(
        got("pinned_ndi_min"),
        "6.3.0",
        "must read the NDI >= minimum"
    );

    let setting = |k: &str| {
        run_sourced(&format!("pinned_setting \"$README\" {k}"), &env)
            .trim()
            .to_string()
    };
    assert_eq!(setting("output_fps"), "30");
    assert_eq!(setting("genlock_wall_clock"), "1");
    assert_eq!(
        setting("ndi_input_latency"),
        "0",
        "must read the pinned NDI input latency mode (0=Normal, certified low-latency, #84)"
    );
    assert_eq!(
        setting("canonical_plugin_path"),
        r"C:\ProgramData\obs-studio\plugins\distroav\bin\64bit",
        "must read the pinned single canonical OBS plugin-load path (#124)"
    );

    let _ = std::fs::remove_file(&readme);
}

#[test]
fn parses_versions_from_real_obs_log() {
    let env = [("LOG", OBS_LOG_FIXTURE)];
    let got = |f: &str| {
        run_sourced(&format!("{f} \"$LOG\""), &env)
            .trim()
            .to_string()
    };

    assert_eq!(got("obs_version_from_log"), "32.1.2");
    assert_eq!(got("distroav_version_from_log"), "6.2.1");
    assert_eq!(got("ndi_runtime_from_log"), "6.3.2.0");
}

#[test]
fn fps_parser_picks_output_fps_not_adapter_fps() {
    // The fixture has an adapter `fps: 60/1` BEFORE the "video settings reset:" block and the
    // real output `fps: 30/1` inside it. A naive "first fps:" parse would wrongly return 60.
    let out = run_sourced("fps_from_log \"$LOG\"", &[("LOG", OBS_LOG_FIXTURE)]);
    assert_eq!(
        out.trim(),
        "30",
        "must pick the OUTPUT fps inside 'video settings reset:', not the adapter fps"
    );
}

#[test]
fn genlock_parser_reads_running_state_from_log() {
    // The genlock master gate's TRUE running state is the OBS log line (the gate is read at
    // launch), not a later env read. ENABLED -> 1, DISABLED -> 0, no line -> UNKNOWN ("").
    let enabled = run_sourced("genlock_from_log \"$LOG\"", &[("LOG", OBS_LOG_FIXTURE)]);
    assert_eq!(enabled.trim(), "1", "ENABLED log line -> 1: {enabled:?}");

    let dis = "12:00:00.000: genlock: wall-clock-slaved render tick DISABLED (stock tick)\n";
    let out = run_sourced("genlock_from_log \"$LOG\"", &[("LOG", dis)]);
    assert_eq!(out.trim(), "0", "DISABLED log line -> 0: {out:?}");

    let none = "12:00:00.000: OBS 32.1.2 (64-bit, windows)\n";
    let out = run_sourced("genlock_from_log \"$LOG\"", &[("LOG", none)]);
    assert_eq!(
        out.trim(),
        "",
        "no genlock line -> UNKNOWN (empty): {out:?}"
    );

    // Real OBS logs are thousands of lines with the genlock line near the top. A `grep -q`-style
    // parser SIGPIPEs printf under `set -euo pipefail` (printf blocks on a full pipe after grep -q
    // already exited) and wrongly returns UNKNOWN — but only once the log exceeds the pipe buffer
    // (~64 KB), so the small fixtures above don't trigger it. This >64 KB log is the regression
    // guard. It is passed via a temp FILE (cat'd into a bash-internal arg) rather than an env var,
    // which would blow ARG_MAX at spawn.
    let mut big = String::from(
        "11:40:39.718: genlock: wall-clock-slaved render tick ENABLED (OBS_GENLOCK_WALL_CLOCK)\n",
    );
    for i in 0..5000 {
        big.push_str(&format!("11:40:40.{i:04}: filler log line {i}\n"));
    }
    let logfile = std::env::temp_dir().join(format!("dg_biglog_{}.txt", std::process::id()));
    std::fs::write(&logfile, &big).expect("write big log");
    let out = run_sourced(
        "genlock_from_log \"$(cat \"$LOGFILE\")\"",
        &[("LOGFILE", logfile.to_str().unwrap())],
    );
    let _ = std::fs::remove_file(&logfile);
    assert_eq!(
        out.trim(),
        "1",
        "must read ENABLED from a large log without SIGPIPE/UNKNOWN: {out:?}"
    );
}

#[test]
fn drift_check_exact_flags_match_mismatch_and_missing() {
    // (mode, expected, observed, want_status_substr, want_rc)
    let cases = [
        ("exact", "32.1.2", "32.1.2", "OK", 0),
        ("exact", "32.1.2", "31.0.0", "DRIFT", 2),
        ("exact", "32.1.2", "", "UNKNOWN", 3), // unread value is never OK
        ("exact", "30", "30", "OK", 0),
        ("exact", "0", "1", "DRIFT", 2),
    ];
    for (mode, exp, obs, want_sub, want_rc) in cases {
        let body =
            format!("rc=0; drift_check label {mode} \"$EXP\" \"$OBS\" || rc=$?; echo \"RC=$rc\"");
        let out = run_sourced(&body, &[("EXP", exp), ("OBS", obs)]);
        assert!(
            out.contains(want_sub),
            "drift_check({mode},{exp},{obs}) should print {want_sub}: {out:?}"
        );
        assert!(
            out.contains(&format!("RC={want_rc}")),
            "drift_check({mode},{exp},{obs}) should return {want_rc}: {out:?}"
        );
    }
}

#[test]
fn drift_check_min_compares_semver_numerically() {
    // NDI runtime is a 4-part version (6.3.2.0); the >= minimum is 6.3.0.
    let cases = [
        ("6.3.0", "6.3.2.0", "OK", 0),  // above min
        ("6.3.0", "6.3.0", "OK", 0),    // exactly min
        ("6.3.0", "6.2.1", "DRIFT", 2), // below min
        ("6.3.0", "6.10.0", "OK", 0),   // numeric, not lexical (10 > 3)
        ("6.3.0", "", "UNKNOWN", 3),    // unread -> never OK
    ];
    for (min, obs, want_sub, want_rc) in cases {
        let body = "rc=0; drift_check ndi min \"$MIN\" \"$OBS\" || rc=$?; echo \"RC=$rc\"";
        let out = run_sourced(body, &[("MIN", min), ("OBS", obs)]);
        assert!(
            out.contains(want_sub),
            "drift_check(min,{min},{obs}) should print {want_sub}: {out:?}"
        );
        assert!(
            out.contains(&format!("RC={want_rc}")),
            "drift_check(min,{min},{obs}) should return {want_rc}: {out:?}"
        );
    }
}

#[test]
fn check_pins_passes_on_the_real_repo_manifest() {
    // The committed vendor/README.md must always declare a complete, well-formed pin set AND
    // agree with the vendored DistroAV source. This is the exact invocation CI runs.
    let (code, stdout, stderr) = run_script(&["--check-pins"]);
    assert_eq!(
        code, 0,
        "real manifest must pass --check-pins. stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("vendored DistroAV source matches the manifest pin"),
        "CI check must cross-check the vendored source. stdout={stdout:?}"
    );
}

#[test]
fn real_manifest_pins_output_fps_60_after_the_11_rollout() {
    // #11 60fps rollout: the committed vendor/README.md must pin output_fps = 60 (the chain runs
    // at 60fps end-to-end — both OBS canvas/output at 60, cam1 emits 60fps NDI, full-chain
    // zero-loss proven at 60). A silent revert to 30 (or any other value) is the regression this
    // locks. Reads the REAL manifest via pinned_setting (the same parser drift-guard --compare
    // uses to flag a live box drifted off 60).
    let readme = manifest_dir().join("vendor/README.md");
    let env = [("README", readme.to_str().unwrap())];
    let fps = run_sourced("pinned_setting \"$README\" output_fps", &env)
        .trim()
        .to_string();
    assert_eq!(
        fps, "60",
        "#11: the real vendor/README.md must pin output_fps=60 (the 60fps rollout); a drop to 30 is drift"
    );
}

#[test]
fn check_pins_flags_manifest_vs_vendored_source_drift() {
    // A manifest that pins a DistroAV version the vendored source does not carry is drift the
    // guard must catch in CI (subtree pulled but table left stale, or vice versa).
    let dir = std::env::temp_dir().join(format!("dg_xcheck_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("distroav")).unwrap();
    let readme = dir.join("README.md");
    std::fs::write(
        &readme,
        "\
| `vendor/obs-studio` | x | **32.1.2** (commit `a`) | git subtree --squash |
| `vendor/distroav` | x | **9.9.9** (commit `b`) | git subtree --squash |
| NDI | x | requires **NDI ≥ 6.3.0** | tree |
| `output_fps` | `30` | log |
| `genlock_wall_clock` | `0` | env |
| `ndi_input_latency` | `0` | obs-websocket |
| `canonical_plugin_path` | `C:\\ProgramData\\obs-studio\\plugins\\distroav\\bin\\64bit` | scan paths |
",
    )
    .unwrap();
    std::fs::write(
        dir.join("distroav/buildspec.json"),
        "{\n    \"version\": \"6.2.1\"\n}\n",
    )
    .unwrap();

    let (code, stdout, stderr) =
        run_script(&["--check-pins", "--readme", readme.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
        code, 20,
        "manifest/vendored DistroAV drift must exit 20. stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stderr.contains("DRIFT") && stderr.contains("9.9.9") && stderr.contains("6.2.1"),
        "must name both the manifest pin and the vendored version. stderr={stderr:?}"
    );
}

#[test]
fn check_pins_fails_loudly_on_incomplete_manifest() {
    // A manifest missing a required pin (here the `output_fps` row) must fail LOUDLY — a MISSING
    // diagnostic naming the absent pin + the "cannot enforce an incomplete pin set" banner + exit
    // 1 — never a silent opaque abort. This guards the `|| true` in the parsers: without it, the
    // no-match `grep|sed|head` trips `set -e` in main()'s command substitution and the script dies
    // before check_pins can report which pin is missing.
    let dir = std::env::temp_dir().join(format!("dg_incomplete_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let readme = dir.join("README.md");
    std::fs::write(
        &readme,
        // output_fps row intentionally absent.
        "\
| `vendor/obs-studio` | x | **32.1.2** (commit `a`) | git subtree --squash |
| `vendor/distroav` | x | **6.2.1** (commit `b`) | git subtree --squash |
| NDI | x | requires **NDI ≥ 6.3.0** | tree |
| `genlock_wall_clock` | `0` | env |
| `ndi_input_latency` | `0` | obs-websocket |
",
    )
    .unwrap();

    let (code, stdout, stderr) =
        run_script(&["--check-pins", "--readme", readme.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
        code, 1,
        "incomplete manifest must exit 1, not a silent abort. stdout={stdout:?} stderr={stderr:?}"
    );
    let all = format!("{stdout}{stderr}");
    assert!(
        all.contains("output_fps") && all.to_lowercase().contains("missing"),
        "must name the missing pin loudly. stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stderr.contains("cannot enforce an incomplete pin set"),
        "must emit the incomplete-pin-set banner. stderr={stderr:?}"
    );
}

#[test]
fn compare_clean_when_observed_matches_the_pinned_set() {
    // The real known-good values verified live on strih 2026-06-14 + the broadcast-path NDI
    // inputs all at the pinned Normal(0) latency (the #84 certified low-latency re-pin verified
    // live 2026-06-16: Normal is ~33 ms lower abs_emit than Lowest and zero-loss over 30 min).
    let (code, stdout, _stderr) = run_script(&[
        "--compare",
        "host=strih",
        "obs_version=32.1.2",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=60",
        "genlock_wall_clock=1",
        "ndi_input_latency=NDI cam5=0,NDI cam1=0,NDI cam3=0",
        // #124: distroav.dll lives in exactly ONE OBS scan path, the canonical ProgramData one.
        r"distroav_dll_paths=C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll",
    ]);
    assert_eq!(
        code, 0,
        "matching observed set must be clean. stdout={stdout:?}"
    );
    assert!(stdout.contains("NO DRIFT"), "stdout={stdout:?}");
}

#[test]
fn compare_fails_loudly_on_drift() {
    let (code, stdout, stderr) = run_script(&[
        "--compare",
        "host=stream",
        "obs_version=31.0.0", // drifted
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=30", // drifted
        "genlock_wall_clock=1",
    ]);
    assert_eq!(
        code, 20,
        "drift must exit 20. stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("obs_version") && stdout.contains("DRIFT"),
        "must show the drifted setting. stdout={stdout:?}"
    );
    assert!(
        stderr.contains("DRIFT DETECTED"),
        "must fail loudly. stderr={stderr:?}"
    );
}

#[test]
fn compare_never_silently_passes_when_a_value_is_unread() {
    // Omit ndi_runtime: the box may be drifted on a value we failed to read. Reporting that as
    // clean (exit 0) is the exact false-negative the UNKNOWN signal exists to prevent.
    let (code, stdout, stderr) = run_script(&[
        "--compare",
        "host=strih",
        "obs_version=32.1.2",
        "distroav_version=6.2.1",
        "output_fps=60",
        "genlock_wall_clock=1",
        // ndi_runtime intentionally missing
    ]);
    assert_eq!(
        code, 11,
        "a missing observed value must exit 11 (UNKNOWN), not 0. stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !stdout.contains("NO DRIFT"),
        "must NOT claim clean when a value is unread. stdout={stdout:?}"
    );
    assert!(
        stdout.contains("UNKNOWN") && stderr.contains("INCOMPLETE"),
        "must report the unread value. stdout={stdout:?} stderr={stderr:?}"
    );
}

// --- #84: per-input NDI ingest latency (pinned latency=Normal=0) drift guard ---------------

#[test]
fn drift_check_inputs_pure_flags_per_input_latency_drift() {
    // The pure helper: every broadcast-path input must run the pinned latency mode. One input
    // off the pin is DRIFT (rc 2); all on-pin is OK (rc 0); an empty observed set is UNKNOWN
    // (rc 3) — never silently OK. Input names contain spaces ("NDI cam5") but never commas,
    // so the CSV split on commas is unambiguous.
    let case = |csv: &str| {
        let body = "rc=0; drift_check_inputs \"$EXP\" \"$CSV\" || rc=$?; echo \"RC=$rc\"";
        run_sourced(body, &[("EXP", "0"), ("CSV", csv)])
    };

    let ok = case("NDI cam5=0,NDI cam1=0,NDI cam3=0");
    assert!(ok.contains("RC=0"), "all-on-pin must be OK: {ok:?}");
    assert!(
        ok.contains("NDI cam5") && ok.contains("latency=0"),
        "must echo each input's observed latency: {ok:?}"
    );

    // The exact #84 regression: an ingest input drifted OFF the certified Normal(0) to
    // latency=2 (extra ingest buffer, ~33 ms HIGHER abs_emit — the slower state).
    let drift = case("NDI cam5=2,NDI cam1=0,NDI cam3=0");
    assert!(
        drift.contains("RC=2"),
        "a drifted input must be DRIFT: {drift:?}"
    );
    assert!(
        drift.contains("NDI cam5")
            && drift.contains("DRIFT")
            && drift.contains("expected latency=0"),
        "must name the drifted input + expected vs observed: {drift:?}"
    );

    let unknown = case("");
    assert!(
        unknown.contains("RC=3") && unknown.contains("UNKNOWN"),
        "an empty observed set must be UNKNOWN, never OK: {unknown:?}"
    );
}

#[test]
fn compare_fails_loudly_when_an_input_latency_drifted() {
    // End-to-end: the strih box with `NDI cam5` (=CAM1) drifted OFF the certified Normal(0) latency
    // to 2 (extra ingest buffer). The guard must exit 20 and name the input vs the pinned 0 (#84).
    let (code, stdout, stderr) = run_script(&[
        "--compare",
        "host=strih",
        "obs_version=32.1.2",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=60",
        "genlock_wall_clock=1",
        "ndi_input_latency=NDI cam5=2,NDI cam1=0,NDI cam3=0",
    ]);
    assert_eq!(
        code, 20,
        "an input drifted off the pinned latency must exit 20. stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("NDI cam5") && stdout.contains("DRIFT") && stdout.contains("latency=0"),
        "must name the drifted input + the pinned latency. stdout={stdout:?}"
    );
    assert!(
        stderr.contains("DRIFT DETECTED"),
        "must fail loudly. stderr={stderr:?}"
    );
}

#[test]
fn compare_input_latency_unknown_when_not_read() {
    // Supply every other observed value but omit ndi_input_latency: the per-input ingest mode is
    // a real drift vector (#84), so an unread set must be UNKNOWN (exit 11), never a silent clean.
    let (code, stdout, stderr) = run_script(&[
        "--compare",
        "host=stream",
        "obs_version=32.1.2",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=60",
        "genlock_wall_clock=1",
        // ndi_input_latency intentionally missing
    ]);
    assert_eq!(
        code, 11,
        "omitting ndi_input_latency must exit 11 (UNKNOWN), not 0. stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !stdout.contains("NO DRIFT"),
        "must NOT claim clean when the input latency set is unread. stdout={stdout:?}"
    );
    assert!(
        stdout.contains("ndi_input_latency") && stdout.contains("UNKNOWN"),
        "must report the unread input-latency set. stdout={stdout:?} stderr={stderr:?}"
    );
}

// --- #124: single canonical OBS plugin-load path (no ProgramData-vs-Program Files shadow) ----

#[test]
fn drift_check_plugin_paths_pure_flags_shadow_and_wrong_path() {
    // The pure helper: distroav.dll must exist in EXACTLY ONE OBS scan path, and that path must be
    // the pinned canonical one. The exact #124 failure is a SECOND copy in another scan path
    // (ProgramData AND Program Files\obs-plugins) that can silently shadow the intended build.
    // Observed = comma-separated list of every distroav.dll location found across the box's OBS
    // scan paths. The pinned canonical value is the directory; an observed entry may be the dir or
    // the full .dll path — both count as "at the canonical path".
    let canon = r"C:\ProgramData\obs-studio\plugins\distroav\bin\64bit";
    let case = |observed: &str| {
        let body = "rc=0; drift_check_plugin_paths \"$EXP\" \"$OBS\" || rc=$?; echo \"RC=$rc\"";
        run_sourced(body, &[("EXP", canon), ("OBS", observed)])
    };

    // Exactly one copy, at the canonical path -> OK.
    let ok = case(r"C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll");
    assert!(
        ok.contains("RC=0"),
        "a single distroav.dll at the canonical path must be OK: {ok:?}"
    );

    // The #124 shadow: distroav.dll in TWO scan paths -> DRIFT. A stale copy in the other path
    // can shadow the intended build (the mixed-version incident #119 that burned the user).
    let shadow = case(concat!(
        r"C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll",
        ",",
        r"C:\Program Files\obs-studio\obs-plugins\64bit\distroav.dll"
    ));
    assert!(
        shadow.contains("RC=2"),
        "distroav.dll in TWO scan paths is a shadow -> DRIFT: {shadow:?}"
    );
    assert!(
        shadow.contains("DRIFT") && shadow.contains("Program Files"),
        "must name the shadow path: {shadow:?}"
    );

    // Exactly one copy but in the WRONG (non-canonical) path -> DRIFT too. The pinned path is the
    // single source of truth; a lone copy anywhere else is still off the canonical path.
    let wrong = case(r"C:\Program Files\obs-studio\obs-plugins\64bit\distroav.dll");
    assert!(
        wrong.contains("RC=2"),
        "a lone copy off the canonical path must be DRIFT: {wrong:?}"
    );
    assert!(
        wrong.contains("DRIFT") && wrong.contains("canonical"),
        "must say it is not on the canonical path: {wrong:?}"
    );

    // No copy read -> UNKNOWN (never silently OK — a scan we could not run must not look clean).
    let unknown = case("");
    assert!(
        unknown.contains("RC=3") && unknown.contains("UNKNOWN"),
        "an empty observed set must be UNKNOWN, never OK: {unknown:?}"
    );
}

#[test]
fn compare_fails_loudly_on_plugin_shadow() {
    // End-to-end: the box has distroav.dll in BOTH ProgramData (canonical) AND Program Files\
    // obs-plugins (a stale shadow left by a deploy). The guard must exit 20 and name the shadow.
    let (code, stdout, stderr) = run_script(&[
        "--compare",
        "host=stream",
        "obs_version=32.1.2",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=60",
        "genlock_wall_clock=1",
        "ndi_input_latency=NDI 2ME PGM=0",
        concat!(
            r"distroav_dll_paths=C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll",
            ",",
            r"C:\Program Files\obs-studio\obs-plugins\64bit\distroav.dll"
        ),
    ]);
    assert_eq!(
        code, 20,
        "a shadowing duplicate plugin must exit 20. stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("DRIFT") && stdout.contains("Program Files"),
        "must name the shadow path. stdout={stdout:?}"
    );
    assert!(
        stderr.contains("DRIFT DETECTED"),
        "must fail loudly. stderr={stderr:?}"
    );
}

#[test]
fn compare_plugin_paths_unknown_when_not_read() {
    // Omit distroav_dll_paths: the duplicate-plugin shadow is a real version-integrity drift vector
    // (#124), so an unread plugin-path scan must be UNKNOWN (exit 11), never a silent clean.
    let (code, stdout, stderr) = run_script(&[
        "--compare",
        "host=strih",
        "obs_version=32.1.2",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=60",
        "genlock_wall_clock=1",
        "ndi_input_latency=NDI cam5=0,NDI cam1=0,NDI cam3=0",
        // distroav_dll_paths intentionally missing
    ]);
    assert_eq!(
        code, 11,
        "omitting distroav_dll_paths must exit 11 (UNKNOWN), not 0. stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !stdout.contains("NO DRIFT"),
        "must NOT claim clean when the plugin-path scan is unread. stdout={stdout:?}"
    );
    assert!(
        stdout.contains("distroav_dll_paths") && stdout.contains("UNKNOWN"),
        "must report the unread plugin-path scan. stdout={stdout:?} stderr={stderr:?}"
    );
}

// --- #122: per-component BUILD SHA + genlock capability drift guard -------------------------
//
// The marketing-version + settings facets above pass a STOCK OBS 32.1.2 — it is byte-for-byte a
// different build from our genlock 32.1.2 but reports the identical version (#119/#120: a wrong
// build with the right version silently shipped). #122 closes that: drift-guard compares the LIVE
// rig's per-component BUILD SHA (obs.dll / distroav.dll Get-FileHash) against the #120 bundle
// manifest, AND asserts the genlock CAPABILITY markers only OUR build emits are present.
//
// The fixtures below are the ACTUAL #184 build: the BUNDLE_MANIFEST.json shape genlock-manifest.sh
// emits (build_sha 19472506e), the obs.dll sha256 24e22357… recorded in it AND read live off both
// strih+stream rig boxes 2026-06-25, and the real genlock marker lines from the running OBS log.

/// The #120 manifest for the deployed #184 build (the `obs-genlock-fast-dll` layout: obs.dll at the
/// stage root). The full windows-genlock bundle nests it at `bin/64bit/obs.dll` +
/// `obs-plugins/64bit/distroav.dll`; the SHA lookup matches by BASENAME so both layouts work.
const MANIFEST_184_FAST: &str = "\
{
  \"schema\": \"camera-box/genlock-bundle-manifest@1\",
  \"build_sha\": \"19472506ec156696c6fcb097899ba745e17b8953\",
  \"components\": [
    { \"name\": \"obs-studio\", \"rebuilt_from_source\": true, \"pinned_version\": \"32.1.2\", \"pinned_commit\": \"fb4d98bf8\", \"source\": \"vendor/obs-studio\" }
  ],
  \"files\": [
    { \"path\": \"GENLOCK_BUILD_SHA.txt\", \"sha256\": \"4b1881f9fb31a852f8c6be0010ce296639538be163296885e5e5e23d10763aae\", \"size\": 42 },
    { \"path\": \"obs.dll\", \"sha256\": \"24e2235788988e6ab8da033a129af172ba634ec4b0120815989002d594c1ef33\", \"size\": 1316352 }
  ]
}
";

/// The FULL bundle layout (nested paths) — both obs.dll and distroav.dll, the real deployed SHAs.
const MANIFEST_184_FULL: &str = "\
{
  \"schema\": \"camera-box/genlock-bundle-manifest@1\",
  \"build_sha\": \"19472506ec156696c6fcb097899ba745e17b8953\",
  \"files\": [
    { \"path\": \"bin/64bit/obs64.exe\", \"sha256\": \"aaaa000000000000000000000000000000000000000000000000000000000000\", \"size\": 100 },
    { \"path\": \"bin/64bit/obs.dll\", \"sha256\": \"24e2235788988e6ab8da033a129af172ba634ec4b0120815989002d594c1ef33\", \"size\": 1316352 },
    { \"path\": \"obs-plugins/64bit/distroav.dll\", \"sha256\": \"66cea7039aa0547823f60935bfd1fb36f38cfdfc76ba5911609c33cbfd022880\", \"size\": 663040 }
  ]
}
";

/// #237: a decoy manifest whose first obs/distroav entries have NO literal dot (`obsXdll` /
/// `distroavYdll`) sitting BEFORE the real `obs.dll` / `distroav.dll`. With the dot matched as a
/// regex wildcard the decoy over-matches and `head -1` returns its (wrong) sha; with the dot
/// escaped the decoy is skipped and the real dll's sha is returned.
const MANIFEST_DOT_DECOY: &str = "\
{
  \"schema\": \"camera-box/genlock-bundle-manifest@1\",
  \"files\": [
    { \"path\": \"obsXdll\", \"sha256\": \"1111111111111111111111111111111111111111111111111111111111111111\", \"size\": 1 },
    { \"path\": \"distroavYdll\", \"sha256\": \"3333333333333333333333333333333333333333333333333333333333333333\", \"size\": 1 },
    { \"path\": \"bin/64bit/obs.dll\", \"sha256\": \"2222222222222222222222222222222222222222222222222222222222222222\", \"size\": 2 },
    { \"path\": \"obs-plugins/64bit/distroav.dll\", \"sha256\": \"4444444444444444444444444444444444444444444444444444444444444444\", \"size\": 2 }
  ]
}
";

/// The genlock CAPABILITY marker text as it appears in the running OBS log (the build-unique lines
/// captured live off stream 2026-06-25). A STOCK OBS emits NONE of these.
const GENLOCK_CAP_OURS: &str = "07:42:29.658: genlock: wall-clock-slaved render tick ENABLED (OBS_GENLOCK_WALL_CLOCK, slew cap 2000000 ns/tick)
07:42:38.746: genlock: sub-frame jitter reserve = 3 ms (OBS_GENLOCK_RESERVE_MS) — ms-granular latency, replaces the whole-frame preload on the ts-align path (#184)";

#[test]
fn manifest_sha_for_component_matches_by_basename_in_both_layouts() {
    // The pure lookup pulls the recorded sha256 for a logical component (obs / distroav) by matching
    // the dll BASENAME in files[], so the flat fast-dll layout AND the nested full-bundle layout both
    // resolve. This is the manifest side of the BUILD-SHA compare.
    let fast = write_temp("dg_manifest_fast", MANIFEST_184_FAST);
    let full = write_temp("dg_manifest_full", MANIFEST_184_FULL);

    let obs_fast = run_sourced(
        "manifest_sha_for_component \"$M\" obs",
        &[("M", fast.to_str().unwrap())],
    );
    assert_eq!(
        obs_fast.trim(),
        "24e2235788988e6ab8da033a129af172ba634ec4b0120815989002d594c1ef33",
        "flat obs.dll sha not resolved: {obs_fast:?}"
    );

    let obs_full = run_sourced(
        "manifest_sha_for_component \"$M\" obs",
        &[("M", full.to_str().unwrap())],
    );
    assert_eq!(
        obs_full.trim(),
        "24e2235788988e6ab8da033a129af172ba634ec4b0120815989002d594c1ef33",
        "nested bin/64bit/obs.dll sha not resolved: {obs_full:?}"
    );

    let da_full = run_sourced(
        "manifest_sha_for_component \"$M\" distroav",
        &[("M", full.to_str().unwrap())],
    );
    assert_eq!(
        da_full.trim(),
        "66cea7039aa0547823f60935bfd1fb36f38cfdfc76ba5911609c33cbfd022880",
        "nested obs-plugins/64bit/distroav.dll sha not resolved: {da_full:?}"
    );

    // A component absent from the manifest resolves to empty (the engine then reports UNKNOWN, never
    // a false clean) — the flat fast manifest carries no distroav.dll.
    let da_fast = run_sourced(
        "manifest_sha_for_component \"$M\" distroav",
        &[("M", fast.to_str().unwrap())],
    );
    assert_eq!(
        da_fast.trim(),
        "",
        "absent distroav must resolve empty: {da_fast:?}"
    );
}

#[test]
fn manifest_sha_for_component_dot_is_literal_not_a_wildcard() {
    // #237: the dll BASENAME is fed to grep as an EXTENDED REGEX, so the literal dot in obs.dll /
    // distroav.dll must be ESCAPED — otherwise `.` is an any-char wildcard and a (hypothetical)
    // path like `obsXdll` (any char where the dot belongs, NO real dot) OVER-MATCHES, returning
    // the WRONG file's sha. The decoy manifest lists `obsXdll` / `distroavYdll` BEFORE the real
    // dll, so a wildcard match (pre-fix) returns the decoy's sha via `head -1`; an escaped match
    // (post-fix) skips the decoy and returns the real dll's sha. No real OBS file is named that —
    // this is a latent-robustness tightening, not a live bug.
    let m = write_temp("dg_dot_decoy", MANIFEST_DOT_DECOY);

    let obs = run_sourced(
        "manifest_sha_for_component \"$M\" obs",
        &[("M", m.to_str().unwrap())],
    );
    assert_eq!(
        obs.trim(),
        "2222222222222222222222222222222222222222222222222222222222222222",
        "obs.dll lookup must match the LITERAL dot (the real obs.dll), not over-match the \
         dot-less `obsXdll` decoy: {obs:?}"
    );

    let da = run_sourced(
        "manifest_sha_for_component \"$M\" distroav",
        &[("M", m.to_str().unwrap())],
    );
    assert_eq!(
        da.trim(),
        "4444444444444444444444444444444444444444444444444444444444444444",
        "distroav.dll lookup must match the LITERAL dot (the real distroav.dll), not over-match \
         the dot-less `distroavYdll` decoy: {da:?}"
    );
}

#[test]
fn compare_labels_unverified_distroav_sha_as_skipped_not_ok() {
    // #237: when the supplied manifest is an obs.dll-only (fast-dll) bundle, a supplied
    // distroav_dll_sha256 is NOT compared against anything — labeling that UNCHECKED value "OK" is
    // misleading (an operator could believe distroav was verified when it wasn't). It must read
    // SKIPPED. The verdict STAYS NO DRIFT (an obs.dll-only manifest legitimately checks only
    // obs.dll; SKIPPED != DRIFT/UNKNOWN), so the exit code is unchanged (0).
    let manifest = write_temp("dg_237_skipped", MANIFEST_184_FAST);
    let (code, stdout, stderr) = run_script(&[
        "--compare",
        "host=strih",
        "obs_version=32.1.2",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=60",
        "genlock_wall_clock=1",
        "ndi_input_latency=NDI cam5=0,NDI cam1=0,NDI cam3=0",
        r"distroav_dll_paths=C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll",
        &format!("manifest={}", manifest.to_str().unwrap()),
        "obs_dll_sha256=24e2235788988e6ab8da033a129af172ba634ec4b0120815989002d594c1ef33",
        "distroav_dll_sha256=66cea7039aa0547823f60935bfd1fb36f38cfdfc76ba5911609c33cbfd022880",
        "genlock_capability=07:42:29.658: genlock: wall-clock-slaved render tick ENABLED (OBS_GENLOCK_WALL_CLOCK)",
    ]);
    assert_eq!(
        code, 0,
        "an obs.dll-only manifest legitimately skips distroav — verdict stays NO DRIFT (exit 0). \
         stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(stdout.contains("NO DRIFT"), "stdout={stdout:?}");
    let line = stdout
        .lines()
        .find(|l| l.contains("distroav_dll_sha256"))
        .expect("must print a distroav_dll_sha256 status line");
    assert!(
        line.contains("SKIPPED"),
        "#237: the unverified distroav SHA must be labeled SKIPPED (not compared in an \
         obs.dll-only manifest): {line:?}"
    );
    assert!(
        !line.contains("OK"),
        "#237: labeling an UNCHECKED distroav SHA 'OK' is misleading — it must read SKIPPED: \
         {line:?}"
    );
}

#[test]
fn genlock_capability_parser_detects_our_build_vs_stock() {
    // The build-unique marker (the wall-clock render-tick line) is present ONLY in our genlock build.
    // The parser returns "1" when present (our build), "" (UNKNOWN) when the text carries no genlock
    // marker at all — a STOCK OBS log, which is the #119 wrong-build case this facet catches.
    let ours = run_sourced(
        "genlock_capability_from_log \"$LOG\"",
        &[("LOG", GENLOCK_CAP_OURS)],
    );
    assert_eq!(
        ours.trim(),
        "1",
        "our build's capability marker must read 1: {ours:?}"
    );

    // A stock OBS log: real OBS header lines but ZERO genlock markers.
    let stock = "11:40:39.376: OBS 32.1.2 (64-bit, windows)\n11:40:39.714: video settings reset:\n11:40:39.714: \tfps:               30/1\n";
    let out = run_sourced("genlock_capability_from_log \"$LOG\"", &[("LOG", stock)]);
    assert_eq!(
        out.trim(),
        "",
        "a stock build emits no genlock marker -> UNKNOWN/absent: {out:?}"
    );
}

#[test]
fn genlock_capability_parser_detects_the_235_single_knob_latency_line() {
    // #235: the single-knob build's startup log emits 'genlock: latency = N ms (≈ M frames @ Ffps)'
    // instead of the #184 'sub-frame jitter reserve = N ms' line. The capability parser must
    // recognize the NEW line as a build-unique marker too — otherwise a #235 build (which no longer
    // emits the old reserve line) would read as UNKNOWN/stock and false-trip the #119 facet.
    let lat_only = "07:42:38.746: genlock: latency = 3 ms (≈ 0 frames @ 29.970fps) (OBS_GENLOCK_LATENCY_MS) — single user-facing latency knob, ts-align implied ON (#235)\n";
    let out = run_sourced("genlock_capability_from_log \"$LOG\"", &[("LOG", lat_only)]);
    assert_eq!(
        out.trim(),
        "1",
        "the #235 'genlock: latency = N ms' line must read as a build-unique capability marker: {out:?}"
    );
    // The alias-sourced wording (RESERVE_MS alias) must also match — it carries the same line shape.
    let alias = "07:42:38.746: genlock: latency = 3 ms (≈ 0 frames @ 29.970fps) (OBS_GENLOCK_RESERVE_MS alias) — single user-facing latency knob, ts-align implied ON (#235)\n";
    let out2 = run_sourced("genlock_capability_from_log \"$LOG\"", &[("LOG", alias)]);
    assert_eq!(
        out2.trim(),
        "1",
        "the alias-sourced #235 latency line must also match: {out2:?}"
    );
}

#[test]
fn compare_clean_when_build_sha_and_capability_match_the_manifest() {
    // Full live facet: the deployed obs.dll/distroav.dll SHAs match the #184 manifest AND the
    // genlock capability marker is present -> NO DRIFT. This is the live-rig PASS #122 proves.
    let manifest = write_temp("dg_184_full", MANIFEST_184_FULL);
    let (code, stdout, stderr) = run_script(&[
        "--compare",
        "host=strih",
        "obs_version=32.1.2",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=60",
        "genlock_wall_clock=1",
        "ndi_input_latency=NDI cam5=0,NDI cam1=0,NDI cam3=0",
        r"distroav_dll_paths=C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll",
        &format!("manifest={}", manifest.to_str().unwrap()),
        "obs_dll_sha256=24e2235788988e6ab8da033a129af172ba634ec4b0120815989002d594c1ef33",
        "distroav_dll_sha256=66cea7039aa0547823f60935bfd1fb36f38cfdfc76ba5911609c33cbfd022880",
        "genlock_capability=07:42:29.658: genlock: wall-clock-slaved render tick ENABLED (OBS_GENLOCK_WALL_CLOCK)",
    ]);
    assert_eq!(
        code, 0,
        "matching build SHA + capability must be clean. stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(stdout.contains("NO DRIFT"), "stdout={stdout:?}");
    assert!(
        stdout.contains("obs_dll_sha256") && stdout.contains("OK"),
        "must show the obs.dll build-SHA OK line. stdout={stdout:?}"
    );
    assert!(
        stdout.contains("genlock_capability") && stdout.contains("OK"),
        "must show the capability OK line. stdout={stdout:?}"
    );
}

#[test]
fn compare_fails_loudly_when_obs_dll_sha_is_a_stock_or_wrong_build() {
    // THE #122 failure this guard exists to catch: marketing version 32.1.2 matches, but the live
    // obs.dll bytes are a DIFFERENT build (a stock OBS, or a stale/wrong genlock build). The SHA
    // differs from the manifest -> DRIFT (exit 20), even though every version/setting check passes.
    let manifest = write_temp("dg_184_sha_drift", MANIFEST_184_FAST);
    let (code, stdout, stderr) = run_script(&[
        "--compare",
        "host=strih",
        "obs_version=32.1.2",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=60",
        "genlock_wall_clock=1",
        "ndi_input_latency=NDI cam5=0,NDI cam1=0,NDI cam3=0",
        r"distroav_dll_paths=C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll",
        &format!("manifest={}", manifest.to_str().unwrap()),
        // a STOCK / wrong-build obs.dll: same version, different bytes
        "obs_dll_sha256=deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
        "genlock_capability=07:42:29.658: genlock: wall-clock-slaved render tick ENABLED",
    ]);
    assert_eq!(
        code, 20,
        "a wrong obs.dll build SHA must exit 20 even when the version matches. stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("obs_dll_sha256") && stdout.contains("DRIFT"),
        "must show the obs.dll SHA as DRIFT. stdout={stdout:?}"
    );
    assert!(
        stderr.contains("DRIFT DETECTED"),
        "must fail loudly. stderr={stderr:?}"
    );
}

#[test]
fn compare_fails_loudly_on_stock_build_with_no_genlock_capability() {
    // A stock OBS 32.1.2 passes every version check but emits NO genlock marker. With the manifest
    // supplied (live facet active), an ABSENT capability marker is DRIFT — the stock-build tell.
    let manifest = write_temp("dg_184_cap_drift", MANIFEST_184_FAST);
    let stock_log =
        "11:40:39.376: OBS 32.1.2 (64-bit, windows)\n11:40:39.714: video settings reset:\n";
    let (code, stdout, stderr) = run_script(&[
        "--compare",
        "host=stream",
        "obs_version=32.1.2",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=60",
        "genlock_wall_clock=1",
        "ndi_input_latency=NDI 2ME PGM=0",
        r"distroav_dll_paths=C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll",
        &format!("manifest={}", manifest.to_str().unwrap()),
        "obs_dll_sha256=24e2235788988e6ab8da033a129af172ba634ec4b0120815989002d594c1ef33",
        &format!("genlock_capability={stock_log}"),
    ]);
    assert_eq!(
        code, 20,
        "a stock build with no genlock capability must exit 20. stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("genlock_capability") && stdout.contains("DRIFT"),
        "must flag the absent capability marker. stdout={stdout:?}"
    );
    assert!(
        stderr.contains("DRIFT DETECTED"),
        "must fail loudly. stderr={stderr:?}"
    );
}

#[test]
fn compare_build_sha_unknown_when_manifest_supplied_but_dll_sha_unread() {
    // When the operator supplies a manifest (activating the build-SHA facet) but a live dll SHA was
    // NOT read, that component is UNKNOWN (exit 11) — never a silent clean. A wrong build we failed
    // to hash is exactly the false-negative the UNKNOWN signal exists to prevent.
    let manifest = write_temp("dg_184_sha_unknown", MANIFEST_184_FULL);
    let (code, stdout, stderr) = run_script(&[
        "--compare",
        "host=strih",
        "obs_version=32.1.2",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=60",
        "genlock_wall_clock=1",
        "ndi_input_latency=NDI cam5=0,NDI cam1=0,NDI cam3=0",
        r"distroav_dll_paths=C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll",
        &format!("manifest={}", manifest.to_str().unwrap()),
        "obs_dll_sha256=24e2235788988e6ab8da033a129af172ba634ec4b0120815989002d594c1ef33",
        // distroav_dll_sha256 + genlock_capability intentionally missing
    ]);
    assert_eq!(
        code, 11,
        "an unread dll SHA with manifest supplied must exit 11 (UNKNOWN). stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !stdout.contains("NO DRIFT"),
        "must NOT claim clean when a build SHA is unread. stdout={stdout:?}"
    );
    assert!(
        stdout.contains("distroav_dll_sha256") && stdout.contains("UNKNOWN"),
        "must report the unread distroav build SHA. stdout={stdout:?}"
    );
}

#[test]
fn compare_build_sha_facet_dormant_without_a_manifest() {
    // The build-SHA + capability facet is OPT-IN on a supplied manifest. Without `manifest=`, the
    // engine runs the marketing-version + settings facets exactly as before (the historic contract):
    // a clean version/settings set is NO DRIFT and the new keys are not demanded. This is what keeps
    // the pre-#122 live checks valid while the SHA facet is the stronger superset when data is given.
    let (code, stdout, _stderr) = run_script(&[
        "--compare",
        "host=strih",
        "obs_version=32.1.2",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=60",
        "genlock_wall_clock=1",
        "ndi_input_latency=NDI cam5=0,NDI cam1=0,NDI cam3=0",
        r"distroav_dll_paths=C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll",
        // no manifest=, no obs_dll_sha256, no genlock_capability
    ]);
    assert_eq!(
        code, 0,
        "no manifest -> marketing facet only -> clean. stdout={stdout:?}"
    );
    assert!(stdout.contains("NO DRIFT"), "stdout={stdout:?}");
    assert!(
        !stdout.contains("obs_dll_sha256"),
        "the build-SHA facet must stay dormant without a manifest. stdout={stdout:?}"
    );
}

// --- #121: post-deploy WHOLE-BUNDLE byte/SHA verify (deploy FAILS on ANY file mismatch) -------
//
// The #122 facet above checks only obs.dll + distroav.dll against the manifest — the two
// genlock-bearing components. #121 raises the bar to deploy-from-clean-tree's contract: after a
// deploy, EVERY file the bundle shipped must match the manifest byte-for-byte on the live box, and
// the deploy FAILS on ANY mismatch (missing, extra-via-unread, or sha-drifted file) — so a partial
// or corrupted deploy (one file silently stale) can never pass. The live per-file hashes are
// gathered off the Windows box (Get-FileHash over each deployed bundle file, ssh denied) and fed as
// a comma-separated `relpath=sha256` list via the new `bundle_hashes=` observed key; the engine
// walks the manifest's files[] and compares each. The manifest path uses forward slashes (the
// genlock-manifest.sh layout), so the observed relpaths must match that convention.

/// A multi-file bundle manifest (the FULL windows-genlock layout) with FOUR files — beyond the two
/// DLLs the #122 facet covers — so the all-files check is proven to verify the WHOLE bundle, not
/// just the genlock components.
const MANIFEST_BUNDLE_4: &str = "\
{
  \"schema\": \"camera-box/genlock-bundle-manifest@1\",
  \"build_sha\": \"19472506ec156696c6fcb097899ba745e17b8953\",
  \"files\": [
    { \"path\": \"GENLOCK_BUILD_SHA.txt\", \"sha256\": \"4b1881f9fb31a852f8c6be0010ce296639538be163296885e5e5e23d10763aae\", \"size\": 42 },
    { \"path\": \"bin/64bit/obs64.exe\", \"sha256\": \"aaaa000000000000000000000000000000000000000000000000000000000000\", \"size\": 100 },
    { \"path\": \"bin/64bit/obs.dll\", \"sha256\": \"24e2235788988e6ab8da033a129af172ba634ec4b0120815989002d594c1ef33\", \"size\": 1316352 },
    { \"path\": \"obs-plugins/64bit/distroav.dll\", \"sha256\": \"66cea7039aa0547823f60935bfd1fb36f38cfdfc76ba5911609c33cbfd022880\", \"size\": 663040 }
  ]
}
";

/// Every file in MANIFEST_BUNDLE_4 with its CORRECT recorded sha — the live read of a clean deploy.
const OBSERVED_ALL_MATCH: &str =
    "GENLOCK_BUILD_SHA.txt=4b1881f9fb31a852f8c6be0010ce296639538be163296885e5e5e23d10763aae,\
bin/64bit/obs64.exe=aaaa000000000000000000000000000000000000000000000000000000000000,\
bin/64bit/obs.dll=24e2235788988e6ab8da033a129af172ba634ec4b0120815989002d594c1ef33,\
obs-plugins/64bit/distroav.dll=66cea7039aa0547823f60935bfd1fb36f38cfdfc76ba5911609c33cbfd022880";

#[test]
fn manifest_all_paths_lists_every_bundle_file() {
    // The pure lister returns EVERY files[] path (one per line), in manifest order — the input the
    // all-files compare iterates. (drift-guard owns its own parser; it must not depend on
    // genlock-manifest.sh at --compare time.)
    let m = write_temp("dg_bundle4_paths", MANIFEST_BUNDLE_4);
    let out = run_sourced("manifest_all_paths \"$M\"", &[("M", m.to_str().unwrap())]);
    let lines: Vec<&str> = out
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(
        lines,
        vec![
            "GENLOCK_BUILD_SHA.txt",
            "bin/64bit/obs64.exe",
            "bin/64bit/obs.dll",
            "obs-plugins/64bit/distroav.dll",
        ],
        "must list every bundle file path: {out:?}"
    );
    let _ = std::fs::remove_file(&m);
}

#[test]
fn drift_check_all_files_pure_ok_drift_unknown() {
    // The pure whole-bundle helper: every manifest file must have a matching observed sha.
    //   * all files match            -> OK    (rc 0)
    //   * any file sha differs       -> DRIFT (rc 2), names the drifted file
    //   * a manifest file unobserved -> UNKNOWN(rc 3) — a file we failed to hash is never a clean pass
    //   * empty observed set         -> UNKNOWN(rc 3) — a scan we could not run must not look clean
    let m = write_temp("dg_bundle4_pure", MANIFEST_BUNDLE_4);
    let case = |observed: &str| {
        let body = "rc=0; drift_check_all_files \"$M\" \"$OBS\" || rc=$?; echo \"RC=$rc\"";
        run_sourced(body, &[("M", m.to_str().unwrap()), ("OBS", observed)])
    };

    // Clean deploy — every file matches.
    let ok = case(OBSERVED_ALL_MATCH);
    assert!(ok.contains("RC=0"), "all-files match must be OK: {ok:?}");
    assert!(
        ok.contains("bin/64bit/obs.dll") && ok.contains("OK"),
        "must report each verified file: {ok:?}"
    );

    // ONE file's bytes drifted (obs64.exe stale) — the partial-deploy class #121 catches.
    let drifted = OBSERVED_ALL_MATCH.replace(
        "bin/64bit/obs64.exe=aaaa000000000000000000000000000000000000000000000000000000000000",
        "bin/64bit/obs64.exe=deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
    );
    let d = case(&drifted);
    assert!(d.contains("RC=2"), "a drifted file must be DRIFT: {d:?}");
    assert!(
        d.contains("bin/64bit/obs64.exe") && d.contains("DRIFT"),
        "must name the drifted file: {d:?}"
    );

    // A manifest file the live scan did NOT report (the distroav.dll line dropped) — UNKNOWN, never
    // a silent OK on a file we could not hash.
    let missing = OBSERVED_ALL_MATCH.replace(
        ",obs-plugins/64bit/distroav.dll=66cea7039aa0547823f60935bfd1fb36f38cfdfc76ba5911609c33cbfd022880",
        "",
    );
    let u = case(&missing);
    assert!(
        u.contains("RC=3") && u.contains("UNKNOWN") && u.contains("obs-plugins/64bit/distroav.dll"),
        "an unobserved bundle file must be UNKNOWN, naming it: {u:?}"
    );

    // Empty observed set -> UNKNOWN (a scan that did not run).
    let empty = case("");
    assert!(
        empty.contains("RC=3") && empty.contains("UNKNOWN"),
        "an empty observed set must be UNKNOWN, never OK: {empty:?}"
    );

    let _ = std::fs::remove_file(&m);
}

#[test]
fn compare_clean_when_every_bundle_file_matches_the_manifest() {
    // End-to-end #121: a clean deploy — every bundle file's live Get-FileHash matches the manifest.
    // With `bundle_hashes=` supplied (whole-bundle facet active), the engine verifies EVERY file,
    // not just the two DLLs, and reports NO DRIFT.
    let m = write_temp("dg_121_clean", MANIFEST_BUNDLE_4);
    let (code, stdout, stderr) = run_script(&[
        "--compare",
        "host=strih",
        "obs_version=32.1.2",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=60",
        "genlock_wall_clock=1",
        "ndi_input_latency=NDI cam5=0,NDI cam1=0,NDI cam3=0",
        r"distroav_dll_paths=C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll",
        "genlock_capability=07:42:29.658: genlock: wall-clock-slaved render tick ENABLED",
        &format!("manifest={}", m.to_str().unwrap()),
        &format!("bundle_hashes={OBSERVED_ALL_MATCH}"),
    ]);
    assert_eq!(
        code, 0,
        "every-file match must be clean. stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(stdout.contains("NO DRIFT"), "stdout={stdout:?}");
    assert!(
        stdout.contains("bundle_files") && stdout.contains("4/4"),
        "must report the whole-bundle verify count. stdout={stdout:?}"
    );
    let _ = std::fs::remove_file(&m);
}

#[test]
fn compare_fails_loudly_when_any_bundle_file_drifted() {
    // THE #121 failure this guard exists to catch: the two DLLs are fine, but a NON-DLL bundle file
    // (obs64.exe — outside the #122 2-DLL facet) is byte-stale on the box (a partial/corrupted
    // deploy). The whole-bundle facet flags it -> DRIFT (exit 20), even though obs.dll/distroav.dll
    // both match. This is exactly the file #122 alone would have missed.
    let m = write_temp("dg_121_drift", MANIFEST_BUNDLE_4);
    let drifted = OBSERVED_ALL_MATCH.replace(
        "bin/64bit/obs64.exe=aaaa000000000000000000000000000000000000000000000000000000000000",
        "bin/64bit/obs64.exe=deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
    );
    let (code, stdout, stderr) = run_script(&[
        "--compare",
        "host=stream",
        "obs_version=32.1.2",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=60",
        "genlock_wall_clock=1",
        "ndi_input_latency=NDI 2ME PGM=0",
        r"distroav_dll_paths=C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll",
        "genlock_capability=07:42:29.658: genlock: wall-clock-slaved render tick ENABLED",
        &format!("manifest={}", m.to_str().unwrap()),
        &format!("bundle_hashes={drifted}"),
    ]);
    assert_eq!(
        code, 20,
        "a drifted bundle file must exit 20 even when the DLLs match. stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("bin/64bit/obs64.exe") && stdout.contains("DRIFT"),
        "must name the drifted bundle file. stdout={stdout:?}"
    );
    assert!(
        stderr.contains("DRIFT DETECTED"),
        "must fail loudly. stderr={stderr:?}"
    );
    let _ = std::fs::remove_file(&m);
}

#[test]
fn compare_bundle_files_unknown_when_a_listed_file_was_unread() {
    // A manifest supplied with `bundle_hashes=` but a listed file NOT in the observed set is UNKNOWN
    // (exit 11) — a deployed file we failed to hash must never be a silent clean (the same
    // never-false-clean discipline as every other facet).
    let m = write_temp("dg_121_unknown", MANIFEST_BUNDLE_4);
    let missing = OBSERVED_ALL_MATCH.replace(
        ",obs-plugins/64bit/distroav.dll=66cea7039aa0547823f60935bfd1fb36f38cfdfc76ba5911609c33cbfd022880",
        "",
    );
    let (code, stdout, stderr) = run_script(&[
        "--compare",
        "host=strih",
        "obs_version=32.1.2",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=60",
        "genlock_wall_clock=1",
        "ndi_input_latency=NDI cam5=0,NDI cam1=0,NDI cam3=0",
        r"distroav_dll_paths=C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll",
        "genlock_capability=07:42:29.658: genlock: wall-clock-slaved render tick ENABLED",
        &format!("manifest={}", m.to_str().unwrap()),
        &format!("bundle_hashes={missing}"),
    ]);
    assert_eq!(
        code, 11,
        "an unread bundle file must exit 11 (UNKNOWN). stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !stdout.contains("NO DRIFT"),
        "must NOT claim clean when a bundle file is unread. stdout={stdout:?}"
    );
    assert!(
        stdout.contains("obs-plugins/64bit/distroav.dll") && stdout.contains("UNKNOWN"),
        "must report the unread bundle file. stdout={stdout:?}"
    );
    let _ = std::fs::remove_file(&m);
}

#[test]
fn compare_whole_bundle_facet_dormant_without_bundle_hashes() {
    // The whole-bundle facet is OPT-IN on `bundle_hashes=`. A manifest supplied WITHOUT bundle_hashes
    // keeps the #122 two-DLL contract exactly (so the hot-swap obs.dll-only verify path is unchanged):
    // the engine checks obs.dll/distroav.dll build SHA + capability, and does NOT demand a full file
    // set. This proves #121 is an additive superset, not a regression of #122.
    let m = write_temp("dg_121_dormant", MANIFEST_184_FULL);
    let (code, stdout, _stderr) = run_script(&[
        "--compare",
        "host=strih",
        "obs_version=32.1.2",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=60",
        "genlock_wall_clock=1",
        "ndi_input_latency=NDI cam5=0,NDI cam1=0,NDI cam3=0",
        r"distroav_dll_paths=C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll",
        &format!("manifest={}", m.to_str().unwrap()),
        "obs_dll_sha256=24e2235788988e6ab8da033a129af172ba634ec4b0120815989002d594c1ef33",
        "distroav_dll_sha256=66cea7039aa0547823f60935bfd1fb36f38cfdfc76ba5911609c33cbfd022880",
        "genlock_capability=07:42:29.658: genlock: wall-clock-slaved render tick ENABLED",
        // no bundle_hashes= -> whole-bundle facet dormant
    ]);
    assert_eq!(
        code, 0,
        "no bundle_hashes -> two-DLL facet only -> clean. stdout={stdout:?}"
    );
    assert!(stdout.contains("NO DRIFT"), "stdout={stdout:?}");
    assert!(
        !stdout.contains("bundle_files"),
        "the whole-bundle facet must stay dormant without bundle_hashes. stdout={stdout:?}"
    );
    let _ = std::fs::remove_file(&m);
}

// --- #246: prod burn-env guard (OBS_BURN_* must NEVER be set in the prod Machine env) ----------
//
// RUN 235001 (#235 re-validate) set OBS_BURN_QR / OBS_BURN_QR_PX / OBS_BURN_RUN_ID into Machine
// scope on BOTH stream and strih and never cleaned up — so QR test-burns drew on the LIVE
// broadcast (Machine scope survives reboot). Burns are test-mode ONLY. drift-guard had ZERO burn
// check; this adds a pure guard + a --compare facet that FAILS LOUDLY if the prod Machine env
// carries ANY OBS_BURN_*. Opt-in (keyed on burn_env=), mirroring the manifest facet, so the
// historic --compare calls are unchanged; the /drift-guard command always feeds it.

#[test]
fn drift_check_burn_env_flags_a_set_burn_var_as_drift() {
    // The #246 failure: a burn var present in the prod Machine env. Any set burn var -> DRIFT (rc 2).
    let body = "rc=0; drift_check_burn_env \"$B\" || rc=$?; echo \"RC=$rc\"";
    let out = run_sourced(
        body,
        &[(
            "B",
            "OBS_BURN_QR=1,OBS_BURN_QR_PX=300,OBS_BURN_RUN_ID=911004",
        )],
    );
    assert!(
        out.contains("DRIFT"),
        "a set burn var must be flagged DRIFT: {out:?}"
    );
    assert!(
        out.contains("RC=2"),
        "a set burn var must return rc 2: {out:?}"
    );
    assert!(
        out.contains("OBS_BURN_QR"),
        "the drift line must name the offending burn var: {out:?}"
    );
}

#[test]
fn drift_check_burn_env_clean_when_none() {
    // The expected prod state: no burn var set. The operator passes the literal "none" -> OK (rc 0).
    let body = "rc=0; drift_check_burn_env \"$B\" || rc=$?; echo \"RC=$rc\"";
    let out = run_sourced(body, &[("B", "none")]);
    assert!(out.contains("OK"), "none -> OK: {out:?}");
    assert!(out.contains("RC=0"), "none -> rc 0: {out:?}");
    // A list whose entries all have EMPTY values (var exists but unset/blank) is also clean.
    let out2 = run_sourced(
        body,
        &[("B", "OBS_BURN_QR=,OBS_BURN_QR_PX=,OBS_BURN_RUN_ID=")],
    );
    assert!(
        out2.contains("OK") && out2.contains("RC=0"),
        "all-empty -> OK: {out2:?}"
    );
}

#[test]
fn drift_check_burn_env_unknown_when_not_read() {
    // An empty observed value (not read) is UNKNOWN (rc 3), never a silent clean.
    let body = "rc=0; drift_check_burn_env \"$B\" || rc=$?; echo \"RC=$rc\"";
    let out = run_sourced(body, &[("B", "")]);
    assert!(
        out.contains("UNKNOWN"),
        "unread burn_env -> UNKNOWN: {out:?}"
    );
    assert!(out.contains("RC=3"), "unread burn_env -> rc 3: {out:?}");
}

#[test]
fn compare_fails_loudly_when_prod_has_a_burn_env_set() {
    // End-to-end: every version/setting matches, but the prod Machine env carries OBS_BURN_QR — the
    // #246 incident. The burn facet (opt-in via burn_env=) must DRIFT the box (exit 20) even though
    // every other check is clean.
    let (code, stdout, stderr) = run_script(&[
        "--compare",
        "host=stream",
        "obs_version=32.1.2",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=60",
        "genlock_wall_clock=1",
        "ndi_input_latency=NDI 2ME PGM=0",
        r"distroav_dll_paths=C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll",
        "burn_env=OBS_BURN_QR=1,OBS_BURN_QR_PX=300,OBS_BURN_RUN_ID=911004",
    ]);
    assert_eq!(
        code, 20,
        "a prod Machine burn var must exit 20 (DRIFT) even when all else is clean. stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("burn") && stdout.contains("DRIFT"),
        "must show the burn var as DRIFT. stdout={stdout:?}"
    );
    assert!(
        stderr.contains("DRIFT DETECTED"),
        "must fail loudly. stderr={stderr:?}"
    );
}

#[test]
fn compare_clean_when_prod_burn_env_is_none() {
    // The certified prod state: burn_env=none. The facet runs (key supplied) and reports OK; the
    // verdict stays NO DRIFT (exit 0).
    let (code, stdout, _stderr) = run_script(&[
        "--compare",
        "host=strih",
        "obs_version=32.1.2",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=60",
        "genlock_wall_clock=1",
        "ndi_input_latency=NDI cam5=0,NDI cam1=0,NDI cam3=0",
        r"distroav_dll_paths=C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll",
        "burn_env=none",
    ]);
    assert_eq!(code, 0, "burn_env=none must be clean. stdout={stdout:?}");
    assert!(stdout.contains("NO DRIFT"), "stdout={stdout:?}");
    assert!(
        stdout.contains("burn_env") && stdout.contains("OK"),
        "must show the burn_env OK line. stdout={stdout:?}"
    );
}

#[test]
fn compare_burn_env_facet_dormant_without_the_key() {
    // Back-compat: omitting burn_env keeps the historic --compare contract — the burn facet stays
    // dormant (no UNKNOWN, no exit-11), exactly like the manifest facet.
    let (code, stdout, _stderr) = run_script(&[
        "--compare",
        "host=strih",
        "obs_version=32.1.2",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=60",
        "genlock_wall_clock=1",
        "ndi_input_latency=NDI cam5=0,NDI cam1=0,NDI cam3=0",
        r"distroav_dll_paths=C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll",
        // no burn_env=
    ]);
    assert_eq!(
        code, 0,
        "no burn_env -> dormant -> clean. stdout={stdout:?}"
    );
    assert!(stdout.contains("NO DRIFT"), "stdout={stdout:?}");
    assert!(
        !stdout.contains("burn"),
        "the burn facet must stay dormant without burn_env=. stdout={stdout:?}"
    );
}

#[test]
fn status_surface_reports_genlock_and_burn_in_one_place() {
    // #246 item 2: a read-only --status facet that prints genlock + burn state in ONE place from
    // the same observed inputs (so an operator never needs ad-hoc PEB/env reads). It is
    // informational (exit 0) — --compare is the fail-loud gate; the rich live OBS dock is #188.
    let (code, stdout, stderr) = run_script(&[
        "--status",
        "host=stream",
        "genlock_wall_clock=1",
        "genlock_capability=07:42:29.658: genlock: wall-clock-slaved render tick ENABLED (OBS_GENLOCK_WALL_CLOCK)",
        "burn_env=OBS_BURN_QR=1",
    ]);
    assert_eq!(
        code, 0,
        "--status is informational (exit 0). stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("genlock") && stdout.contains("burn"),
        "--status must print BOTH genlock and burn state in one place. stdout={stdout:?}"
    );
    // a set burn must be visibly surfaced (not hidden)
    assert!(
        stdout.contains("OBS_BURN_QR"),
        "--status must surface the set burn var. stdout={stdout:?}"
    );
}

#[test]
fn status_surface_does_not_require_the_pinned_manifest() {
    // #246: --status is a read-only live-state dump that uses NONE of the pinned set, so it must
    // work even when vendor/README.md is absent (a checkout shipping only the script). RED before
    // the fix: the manifest-required check + pin load ran for every mode and exited 1 here.
    let (code, stdout, stderr) = run_script(&[
        "--status",
        "host=stream",
        "genlock_wall_clock=1",
        "burn_env=none",
        "--readme",
        "/nonexistent/path/vendor/README.md",
    ]);
    assert_eq!(
        code, 0,
        "--status must not require the pinned manifest (read-only live-state dump). \
         stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !stderr.contains("manifest not found"),
        "--status must skip the manifest-required check. stderr={stderr:?}"
    );
}
