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
        "output_fps=30",
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
        "output_fps=60", // drifted
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
        "output_fps=30",
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
        "output_fps=30",
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
        "output_fps=30",
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
        "output_fps=30",
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
        "output_fps=30",
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
