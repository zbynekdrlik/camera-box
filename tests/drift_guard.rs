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
| `output_fps_strih` | `60` | OBS log |
| `output_fps_stream` | `30` | OBS log |
| `genlock_wall_clock` | `1` | OBS log |
| `ndi_input_latency` | `0` | obs-websocket GetInputSettings |
| `canonical_plugin_path` | `C:\\ProgramData\\obs-studio\\plugins\\distroav\\bin\\64bit` | Get-ChildItem the OBS scan paths |
| `genlock_source_latency_strih` | `NDI cam5=3,NDI cam1=3,NDI cam3=3` | OBS log genlock-fifo audit |
| `genlock_source_latency_stream` | `NDI 2ME PGM=450` | OBS log genlock-fifo audit |
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
    assert_eq!(setting("output_fps_strih"), "60");
    assert_eq!(setting("output_fps_stream"), "30");
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
    assert_eq!(
        setting("genlock_source_latency_strih"),
        "NDI cam5=3,NDI cam1=3,NDI cam3=3",
        "must read strih per-source genlock held-latency pin (#357)"
    );
    assert_eq!(
        setting("genlock_source_latency_stream"),
        "NDI 2ME PGM=450",
        "must read stream per-source genlock held-latency pin (NDI 2ME PGM A/V-align=450ms, #357)"
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
fn real_manifest_pins_output_fps_per_box_strih_30_stream_30_459() {
    // Topology v2 (#459, EPIC #466, SUPERSEDES the #11 mixed 60/30 pin this test used to lock):
    // strih dropped from the 60fps LED-wall IMAG role to a 30fps cut-to-stream-only box (the
    // 60fps IMAG role moved to the new imag-nb box, #458/#463). The 60→30 camera-feed decimation
    // now happens ON strih's OWN ingest (cam boxes still emit 60fps NDI into strih's 30fps
    // canvas); strih→stream is now a plain 30→30 pass-through. A drift of EITHER box away from
    // 30 is the regression this locks. Reads the REAL manifest via pinned_setting (the same
    // parser drift-guard --compare uses per host).
    let readme = manifest_dir().join("vendor/README.md");
    let env = [("README", readme.to_str().unwrap())];
    let strih = run_sourced("pinned_setting \"$README\" output_fps_strih", &env)
        .trim()
        .to_string();
    let stream = run_sourced("pinned_setting \"$README\" output_fps_stream", &env)
        .trim()
        .to_string();
    assert_eq!(
        strih, "30",
        "#459: the real vendor/README.md must pin output_fps_strih=30 (strih is now cut-to-stream only); a drift to 60 is drift"
    );
    assert_eq!(
        stream, "30",
        "#459: the real vendor/README.md must pin output_fps_stream=30 (strih->stream is now a plain 30->30 pass-through); a drift to 60 is drift"
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
| `output_fps_strih` | `60` | log |
| `output_fps_stream` | `30` | log |
| `genlock_wall_clock` | `0` | env |
| `ndi_input_latency` | `0` | obs-websocket |
| `canonical_plugin_path` | `C:\\ProgramData\\obs-studio\\plugins\\distroav\\bin\\64bit` | scan paths |
| `genlock_source_latency_strih` | `NDI cam5=3,NDI cam1=3,NDI cam3=3` | OBS log |
| `genlock_source_latency_stream` | `NDI 2ME PGM=450` | OBS log |
| `output_fps_imag` | `60` | log |
| `genlock_latency_ms_imag` | `3` | log |
| `dantesync_locked_imag` | `locked` | journalctl |
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
fn check_pins_flags_source_latency_range_pin_that_disagrees_with_the_code_clamp() {
    // #390: the `range:MIN-MAX` bounds embedded in the manifest text and the
    // GENLOCK_LATENCY_MS_MIN/_MAX constants in drift-guard.sh are two independent copies (markdown
    // vs bash) — a manifest typo (here `range:3-200`, missing a zero) must NOT silently narrow the
    // sane backstop. check-pins must catch the disagreement loudly, exit 1, and name both values.
    let dir = std::env::temp_dir().join(format!("dg_range_mismatch_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("distroav")).unwrap();
    let readme = dir.join("README.md");
    std::fs::write(
        &readme,
        "\
| `vendor/obs-studio` | x | **32.1.2** (commit `a`) | git subtree --squash |
| `vendor/distroav` | x | **6.2.1** (commit `b`) | git subtree --squash |
| NDI | x | requires **NDI ≥ 6.3.0** | tree |
| `output_fps_strih` | `60` | log |
| `output_fps_stream` | `30` | log |
| `genlock_wall_clock` | `1` | env |
| `ndi_input_latency` | `0` | obs-websocket |
| `canonical_plugin_path` | `C:\\ProgramData\\obs-studio\\plugins\\distroav\\bin\\64bit` | scan paths |
| `genlock_source_latency_strih` | `NDI cam5=3,NDI cam1=3,NDI cam3=3` | OBS log |
| `genlock_source_latency_stream` | `NDI 2ME PGM=range:3-200` | OBS log |
| `output_fps_imag` | `60` | log |
| `genlock_latency_ms_imag` | `3` | log |
| `dantesync_locked_imag` | `locked` | journalctl |
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
        code, 1,
        "a range pin disagreeing with the code's clamp must exit 1. stdout={stdout:?} stderr={stderr:?}"
    );
    let all = format!("{stdout}{stderr}");
    assert!(
        all.contains("3-200") && all.contains("3-2000") && all.to_uppercase().contains("MALFORMED"),
        "must name both the pinned range and the code's clamp loudly. stdout={stdout:?} stderr={stderr:?}"
    );
}

#[test]
fn check_pins_fails_loudly_on_a_missing_imag_pin_463() {
    // #463 review: output_fps_imag / genlock_latency_ms_imag are real, always-pinned values
    // (unlike genlock_build_sha_imag/distroav_so_sha256_imag, deliberately left unpinned until
    // the first post-#463 live deploy) — before this fix, an otherwise-COMPLETE manifest missing
    // one of these two imag pins passed --check-pins silently (they were never validated
    // offline), so a malformed/blank imag pin would only ever surface via a LIVE --check-imag
    // SSH run against imag-nb, never caught by CI. Manifest here has every OTHER required pin
    // but omits `genlock_latency_ms_imag` entirely.
    let dir = std::env::temp_dir().join(format!("dg_missing_imag_pin_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("distroav")).unwrap();
    let readme = dir.join("README.md");
    std::fs::write(
        &readme,
        "\
| `vendor/obs-studio` | x | **32.1.2** (commit `a`) | git subtree --squash |
| `vendor/distroav` | x | **6.2.1** (commit `b`) | git subtree --squash |
| NDI | x | requires **NDI ≥ 6.3.0** | tree |
| `output_fps_strih` | `60` | log |
| `output_fps_stream` | `30` | log |
| `genlock_wall_clock` | `1` | env |
| `ndi_input_latency` | `0` | obs-websocket |
| `canonical_plugin_path` | `C:\\ProgramData\\obs-studio\\plugins\\distroav\\bin\\64bit` | scan paths |
| `genlock_source_latency_strih` | `NDI cam5=3,NDI cam1=3,NDI cam3=3` | OBS log |
| `genlock_source_latency_stream` | `NDI 2ME PGM=450` | OBS log |
| `output_fps_imag` | `60` | log |
",
        // genlock_latency_ms_imag row intentionally absent.
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
        code, 1,
        "a missing imag pin must exit 1, not a silent pass. stdout={stdout:?} stderr={stderr:?}"
    );
    let all = format!("{stdout}{stderr}");
    assert!(
        all.contains("genlock_latency_ms_imag") && all.to_lowercase().contains("missing"),
        "must name the missing imag pin loudly. stdout={stdout:?} stderr={stderr:?}"
    );
}

#[test]
fn check_pins_fails_loudly_on_a_missing_dantesync_locked_pin_489() {
    // #489 review: dantesync_locked_imag is, like output_fps_imag/genlock_latency_ms_imag above,
    // an always-pinned steady-state value (not a build-artifact SHA deferred until first deploy)
    // -- deliberately wired into this offline --check-pins gate, mirroring #463's own precedent
    // for those two sibling pins. Manifest here has every OTHER required pin but omits
    // `dantesync_locked_imag` entirely.
    let dir = std::env::temp_dir().join(format!("dg_missing_dantesync_pin_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("distroav")).unwrap();
    let readme = dir.join("README.md");
    std::fs::write(
        &readme,
        "\
| `vendor/obs-studio` | x | **32.1.2** (commit `a`) | git subtree --squash |
| `vendor/distroav` | x | **6.2.1** (commit `b`) | git subtree --squash |
| NDI | x | requires **NDI ≥ 6.3.0** | tree |
| `output_fps_strih` | `60` | log |
| `output_fps_stream` | `30` | log |
| `genlock_wall_clock` | `1` | env |
| `ndi_input_latency` | `0` | obs-websocket |
| `canonical_plugin_path` | `C:\\ProgramData\\obs-studio\\plugins\\distroav\\bin\\64bit` | scan paths |
| `genlock_source_latency_strih` | `NDI cam5=3,NDI cam1=3,NDI cam3=3` | OBS log |
| `genlock_source_latency_stream` | `NDI 2ME PGM=450` | OBS log |
| `output_fps_imag` | `60` | log |
| `genlock_latency_ms_imag` | `3` | log |
",
        // dantesync_locked_imag row intentionally absent.
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
        code, 1,
        "a missing dantesync_locked_imag pin must exit 1, not a silent pass. stdout={stdout:?} stderr={stderr:?}"
    );
    let all = format!("{stdout}{stderr}");
    assert!(
        all.contains("dantesync_locked_imag") && all.to_lowercase().contains("missing"),
        "must name the missing dantesync_locked_imag pin loudly. stdout={stdout:?} stderr={stderr:?}"
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
        "obs_version=32.2.0",
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
        "output_fps=30", // matches the stream pin (#11 60→30); obs_version is the drift here
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
        "obs_version=32.2.0",
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
        "obs_version=32.2.0",
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
        "obs_version=32.2.0",
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
        "obs_version=32.2.0",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=30",
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

/// #1184: the drift-guard OBS-log matchers grep LOCALLY (drift-guard runs on dev1 over ssh-fetched
/// REMOTE log text, and the grep that DECIDES runs locally in dev1's UTF-8 locale). OBS logs carry
/// raw invalid-UTF-8 bytes (DistroAV mojibake); in a UTF-8 locale GNU grep then MISSES an ASCII
/// marker that IS present, so a healthy box under-reports genlock capability/state/latency/rt-pin.
/// `LC_ALL=C grep -a` (byte-literal, single-byte locale) is the sanctioned fix (same as issue 1183
/// in verify-imag.sh). RED against the current non-`-a` greps in this UTF-8 harness locale, GREEN
/// after `LC_ALL=C grep -a` on the whole `genlock_*_from_log` family.
#[test]
fn genlock_log_matchers_survive_invalid_utf8_bytes_1184() {
    // Each marker line carries an invalid-UTF-8 sequence: `\xe2\x82` in the `.*` gap for the
    // capability/state/latency regexes, and at the LINE START for the rt-pin regex (which is a
    // fixed literal with no `.*` to absorb an in-line byte). All markers are otherwise present.
    let out = run_sourced(
        r#"LOG="$(printf '07:42:29.658: genlock: wall-clock-slaved \xe2\x82render tick ENABLED (OBS_GENLOCK_WALL_CLOCK, slew cap 2000000 ns/tick)\n07:42:38.746: genlock: \xe2\x82latency = 3 ms (0 frames @ 60.000fps) (OBS_GENLOCK_LATENCY_MS)\n\xe2\x8214:27:54.427: genlock: render-tick thread set SCHED_FIFO prio 10 on the isolated core (#484)\n')"
printf 'cap=%s\n' "$(genlock_capability_from_log "$LOG")"
printf 'state=%s\n' "$(genlock_from_log "$LOG")"
printf 'lat=%s\n' "$(genlock_latency_ms_from_log "$LOG")"
printf 'rtpin=%s\n' "$(genlock_rt_pin_from_log "$LOG")""#,
        &[("LC_ALL", "C.UTF-8")],
    );
    let got: Vec<&str> = out.lines().collect();
    assert_eq!(
        got,
        vec!["cap=1", "state=1", "lat=3", "rtpin=ok"],
        "invalid-UTF-8 bytes in the OBS log must not suppress any genlock_*_from_log marker \
         (#1184, drift-guard the sibling of #1183): {out:?}"
    );
}

/// The imag-nb (#484) OBS log lines for the genlock render-tick SCHED_FIFO pin — SUCCESS shape
/// (`vendor/obs-studio/libobs/obs-video.c genlock_pin_render_tick_thread`, Linux-only).
const GENLOCK_RT_PIN_OK_LINE: &str =
    "14:27:54.427: genlock: render-tick thread set SCHED_FIFO prio 10 on the isolated core (#484)\n";

/// The #484 WARN-and-continue FAILURE shape — the EXACT #572 root cause: the render-tick thread
/// could not get SCHED_FIFO (missing rtprio ulimit grant) and fell back to SCHED_OTHER.
const GENLOCK_RT_PIN_FAILED_LINE: &str = "14:27:54.392: genlock: could NOT set render-tick thread \
     SCHED_FIFO prio 10 (errno 1 — missing rtprio ulimit grant?) — continuing SCHED_OTHER (#484)\n";

#[test]
fn genlock_rt_pin_parser_detects_the_484_success_line() {
    let out = run_sourced(
        "genlock_rt_pin_from_log \"$LOG\"",
        &[("LOG", GENLOCK_RT_PIN_OK_LINE)],
    );
    assert_eq!(
        out.trim(),
        "ok",
        "the #484 SCHED_FIFO success line must parse as ok: {out:?}"
    );
}

#[test]
fn genlock_rt_pin_parser_detects_the_572_failure_line() {
    let out = run_sourced(
        "genlock_rt_pin_from_log \"$LOG\"",
        &[("LOG", GENLOCK_RT_PIN_FAILED_LINE)],
    );
    assert_eq!(
        out.trim(),
        "failed",
        "the #572 'could NOT set ... SCHED_FIFO' line must parse as failed: {out:?}"
    );
}

#[test]
fn genlock_rt_pin_parser_absent_when_neither_line_present() {
    // A real, non-empty imag-nb log (header + fps + the #235 latency line) that carries NEITHER
    // the #484 success nor failure line -- e.g. a build that predates #484. Must read as ""
    // (UNKNOWN upstream), never guessed as ok or failed.
    let pre_484_log = "11:40:39.376: OBS 32.1.2 (64-bit, linux)\n11:40:39.714: video settings reset:\n\
        11:40:39.714: \tfps:               60/1\n07:42:38.746: genlock: latency = 3 ms (\u{2248} 0 frames @ 60.000fps)\n";
    let out = run_sourced("genlock_rt_pin_from_log \"$LOG\"", &[("LOG", pre_484_log)]);
    assert_eq!(
        out.trim(),
        "",
        "a log with neither RT-pin marker must read empty (UNKNOWN), never a guess: {out:?}"
    );
}

#[test]
fn genlock_rt_pin_parser_empty_on_empty_text() {
    let out = run_sourced("genlock_rt_pin_from_log \"$LOG\"", &[("LOG", "")]);
    assert_eq!(
        out.trim(),
        "",
        "empty log text must read empty (UNKNOWN, never read): {out:?}"
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
        "obs_version=32.2.0",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=30",
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
        "output_fps=30",
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
        "output_fps=30",
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
        "obs_version=32.2.0",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=30",
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
        "obs_version=32.2.0",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=30",
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
// gathered off the Windows box (Get-FileHash over each deployed bundle file, via the win-* MCP
// Shell -- #701 proved plain scp/ssh reaches strih/stream and #703's win-ssh-exec.sh proves a
// remote PowerShell command CAN run over ssh too, but this facet has not been migrated) and fed as
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
        "obs_version=32.2.0",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=30",
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
        "output_fps=30",
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
        "obs_version=32.2.0",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=30",
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
        "obs_version=32.2.0",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=30",
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
        "output_fps=30",
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
        "obs_version=32.2.0",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=30",
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
        "obs_version=32.2.0",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=30",
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

// --- #357: pin + check per-source genlock FIFO held-latency (genlock_source_latency) -----------
//
// The drift-guard had ZERO coverage of the per-source genlock held-latency (`latency_ms=N` from
// the `genlock-fifo audit` OBS log line). Only `ndi_input_latency` (the DistroAV INPUT buffer
// mode 0/1/2) was pinned — a completely different setting. So the deliberate A/V-align per-source
// override (`NDI 2ME PGM`=450ms on the stream box, slowing video to sync with the ~1s-late
// mastered audio) went UNCHECKED: a drift from 450ms to 900ms would be silently passed. This adds:
//   • `genlock_source_latency_from_log TEXT` — parses the per-source `latency_ms=N` from the OBS
//     `genlock-fifo audit 'SOURCE': … latency_ms=N …` lines into a `source=N,source=N,…` CSV.
//   • `drift_check_source_latency EXPECTED_CSV OBSERVED_CSV` — per-source check; for each pinned
//     source the observed `latency_ms` must match exactly (UNKNOWN if not observed, DRIFT if
//     different).
//   • `genlock_source_latency_strih` + `genlock_source_latency_stream` rows in vendor/README.md.
//   • `check_pins` validation of both new pins.
//   • `--compare genlock_source_latency=` opt-in facet: dormant when the key is absent (backward
//     compat), fails loudly on drift when supplied.

/// A single `genlock-fifo audit` log line in the exact format obs-source.c emits (#357).
/// `NDI 2ME PGM` carries a deliberate 450ms per-source override; `NDI cam5` follows the 3ms global.
const GENLOCK_FIFO_AUDIT_FIXTURE: &str = "\
07:42:38.746: genlock-fifo audit 'NDI 2ME PGM': received=1200 consumed=1200 underruns=0 holds=1200 \
overruns=0 backward_steps=0 depth=27 peak=27 latency_ms=450 (\u{2248}27 frames @ 59.940fps) \
src_latency_ms=450 global_latency_ms=3 preload=0 (=0 ms) reserve_ms=450 cap=30 empty_run=0 (re-arm@0)\n\
07:42:38.747: genlock-fifo audit 'NDI cam5': received=3600 consumed=3600 underruns=0 holds=3600 \
overruns=0 backward_steps=0 depth=1 peak=1 latency_ms=3 (\u{2248}0 frames @ 59.940fps) \
src_latency_ms=0 global_latency_ms=3 preload=0 (=0 ms) reserve_ms=3 cap=2 empty_run=0 (re-arm@0)\n\
";

#[test]
fn genlock_source_latency_parser_extracts_per_source_effective_latency() {
    // RED: `genlock_source_latency_from_log` does not exist yet → sourced harness exits non-zero.
    // GREEN: parses the `latency_ms=N` field per source from `genlock-fifo audit 'SOURCE':` lines.
    // NDI 2ME PGM has a deliberate per-source override (src_latency_ms=450); NDI cam5 follows
    // the global 3ms (src_latency_ms=0 -> effective latency_ms=3). We pin latency_ms (the
    // EFFECTIVE held latency), not src_latency_ms, because that is what the FIFO actually holds.
    let out = run_sourced(
        r#"genlock_source_latency_from_log "$LOG""#,
        &[("LOG", GENLOCK_FIFO_AUDIT_FIXTURE)],
    );
    assert!(
        out.trim().contains("NDI 2ME PGM=450"),
        "must extract NDI 2ME PGM latency_ms=450 (per-source A/V-align override): {out:?}"
    );
    assert!(
        out.trim().contains("NDI cam5=3"),
        "must extract NDI cam5 effective latency_ms=3 (follows global): {out:?}"
    );
    // must NOT bleed in src_latency_ms / global_latency_ms (different fields)
    assert!(
        !out.contains("=0"),
        "src_latency_ms=0 (follows-global sentinel) must not appear in the result: {out:?}"
    );
}

#[test]
fn drift_check_source_latency_catches_drift_and_passes_on_match() {
    // RED: `drift_check_source_latency` does not exist yet → sourced harness exits non-zero.
    // GREEN: per-source check — for each pinned source the observed latency_ms must match exactly.
    //   • 450ms observed vs 450ms pinned → OK (rc 0)
    //   • 900ms observed vs 450ms pinned → DRIFT (rc 2) — the failure mode #357 exists to catch
    //   • empty observed               → UNKNOWN (rc 3)
    let case = |observed: &str| -> String {
        run_sourced(
            r#"rc=0; drift_check_source_latency "$EXP" "$OBS" || rc=$?; echo "RC=$rc""#,
            &[("EXP", "NDI 2ME PGM=450"), ("OBS", observed)],
        )
    };

    // match → OK
    let ok = case("NDI 2ME PGM=450");
    assert!(ok.contains("RC=0"), "450ms matches the pin → OK: {ok:?}");
    assert!(ok.contains("OK"), "must print OK status line: {ok:?}");

    // drift: 900ms vs pinned 450ms → DRIFT (rc 2); must name the source + both values
    let drift = case("NDI 2ME PGM=900");
    assert!(
        drift.contains("RC=2"),
        "900ms vs pinned 450ms must return rc 2 (DRIFT): {drift:?}"
    );
    assert!(
        drift.contains("DRIFT"),
        "must print DRIFT status line: {drift:?}"
    );
    assert!(
        drift.contains("NDI 2ME PGM"),
        "must name the drifted source: {drift:?}"
    );

    // unread → UNKNOWN (rc 3)
    let unknown = case("");
    assert!(
        unknown.contains("RC=3"),
        "empty observed must return rc 3 (UNKNOWN): {unknown:?}"
    );
    assert!(
        unknown.contains("UNKNOWN"),
        "must print UNKNOWN status line: {unknown:?}"
    );

    // mixed: one source drifted + one source absent → DRIFT must win (rc 2, not rc 3)
    // This validates the return-order fix (DRIFT before UNKNOWN) — without it the
    // unobserved source would mask the drift and exit 3 instead of 2.
    let mixed = run_sourced(
        r#"rc=0; drift_check_source_latency "$EXP" "$OBS" || rc=$?; echo "RC=$rc""#,
        &[
            ("EXP", "NDI 2ME PGM=450,NDI cam5=3"),
            ("OBS", "NDI 2ME PGM=900"), // cam5 absent, PGM drifted
        ],
    );
    assert!(
        mixed.contains("RC=2"),
        "drift must take priority over unknown in the mixed case (rc 2, not 3): {mixed:?}"
    );
    assert!(
        mixed.contains("DRIFT"),
        "must print DRIFT for the drifted source: {mixed:?}"
    );
}

#[test]
fn real_manifest_pins_genlock_source_latency_per_box() {
    // RED: vendor/README.md has no `genlock_source_latency_strih` / `genlock_source_latency_stream`
    // rows yet → `pinned_setting` returns empty → assertion fails.
    // GREEN: both pins present with correct values.
    let readme = manifest_dir().join("vendor/README.md");
    let env = [("README", readme.to_str().unwrap())];
    let setting = |k: &str| -> String {
        run_sourced(&format!("pinned_setting \"$README\" {k}"), &env)
            .trim()
            .to_string()
    };

    let strih = setting("genlock_source_latency_strih");
    assert!(
        !strih.is_empty(),
        "real manifest must pin genlock_source_latency_strih (post-#753 1:1 active-set cameras); got empty"
    );
    // #757: after #753's 1:1 mapping pivot + the issue-1061 per-source A/V-align calibration, the
    // strih camera pins are the operator's re-tunable A/V-align domain (live-verified REPORT-ONLY
    // against scripts/latency-pins-baseline.json), so the drift-guard row is a clamp-range backstop
    // under the post-pivot 1:1 names (NDI cam1/cam2/cam3) — NOT the dead pre-pivot
    // `NDI cam5=3,...` all-at-3ms-floor model, and NOT re-hardcoded fixed ms values that re-go-stale
    // on the next recalibration (the same #390 lesson already applied to the stream pin below).
    assert!(
        strih.contains("NDI cam1=range:")
            && strih.contains("NDI cam2=range:")
            && strih.contains("NDI cam3=range:"),
        "strih pin must use the post-pivot 1:1 active-set names as range backstops \
         (NDI cam1/cam2/cam3=range:...), not re-hardcoded ms values (#390): {strih:?}"
    );
    assert!(
        !strih.contains("NDI cam5="),
        "strih pin must not carry the retired pre-pivot slot name NDI cam5 (#753 mapping pivot): {strih:?}"
    );

    let stream = setting("genlock_source_latency_stream");
    // #390: the stream A/V-align source is NOT a fixed constant any more — it is calibration-
    // tracked (a `range:MIN-MAX` sane backstop, not a hand-guessed ms value that goes stale the
    // next time the operator re-calibrates #188). The exact bound values are asserted below.
    assert!(
        stream.contains("NDI 2ME PGM=range:"),
        "real manifest must pin NDI 2ME PGM as calibration-tracked (range:MIN-MAX, #390), not a \
         hardcoded ms constant: {stream:?}"
    );
    assert!(
        !stream.contains("NDI 2ME PGM=450") && !stream.contains("NDI 2ME PGM=1000"),
        "must NOT be re-hardcoded to another fixed constant (450 or 1000) — that just re-goes-stale \
         the next time the A/V-align is re-calibrated (#390 root cause): {stream:?}"
    );
}

#[test]
fn compare_fails_when_per_source_genlock_latency_egregiously_out_of_range() {
    // #390: the stream `NDI 2ME PGM` pin is now calibration-tracked (a `range:3-2000` sane
    // backstop — the DistroAV per-source genlock-latency clamp — instead of a hardcoded ms
    // constant), so a plausible calibrated value (e.g. 900ms or 1000ms) is no longer drift by
    // itself (see `compare_does_not_false_alarm_...` below — that IS the #390 bug this PR fixes).
    // The range check must still catch a value the DistroAV UI/WS could never legitimately hold:
    // here 5000ms, which exceeds the clamp max (2000ms).
    let (code, stdout, stderr) = run_script(&[
        "--compare",
        "host=stream",
        "obs_version=32.1.2",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=30",
        "genlock_wall_clock=1",
        "ndi_input_latency=NDI 2ME PGM=0",
        r"distroav_dll_paths=C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll",
        "genlock_source_latency=NDI 2ME PGM=5000", // egregiously out of range (clamp max=2000)
    ]);
    assert_eq!(
        code, 20,
        "egregiously out-of-range per-source latency must still exit 20. stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("NDI 2ME PGM") && stdout.contains("DRIFT"),
        "must name the drifted source in stdout. stdout={stdout:?}"
    );
    assert!(
        stderr.contains("DRIFT DETECTED"),
        "must fail loudly on stderr. stderr={stderr:?}"
    );
}

#[test]
fn compare_does_not_false_alarm_when_live_av_align_diverges_from_stale_pin() {
    // #390 REGRESSION: this is the exact false-alarm the issue reports. The stream A/V-align
    // latency is whatever the #188 calibration last measured (NOT a fixed constant) — the pin was
    // hand-guessed at 450ms, but the genuinely-delivered, correctly-working live value was
    // verified at 1000ms (2026-07-01, via OBS WS + genlock-fifo audit: src_latency_ms=1000
    // latency_ms=1000 reserve_ms=1000, head_skew ~1s, underruns=0). A `--compare` against that
    // live value must NOT drift merely because it differs from any single previously-pinned
    // constant — it only fails when it is out of the sane DistroAV clamp range, or (separately)
    // when it has drifted from the last #427-persisted calibration. Neither applies here: no
    // `av_sync_calibrated_ms=` was supplied (best-effort facet dormant) and 1000ms is well inside
    // the [3, 2000] clamp range.
    let (code, stdout, stderr) = run_script(&[
        "--compare",
        "host=stream",
        "obs_version=32.2.0",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=30",
        "genlock_wall_clock=1",
        "ndi_input_latency=NDI 2ME PGM=0",
        r"distroav_dll_paths=C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll",
        "genlock_source_latency=NDI 2ME PGM=1000", // live, genuinely-delivered A/V-align value
    ]);
    assert_eq!(
        code, 0,
        "the live calibrated 1000ms value must NOT false-alarm as drift (#390). \
         stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("NDI 2ME PGM") && stdout.contains("OK"),
        "must report the source as OK (range-checked, within the sane clamp). stdout={stdout:?}"
    );
    assert!(
        stdout.to_lowercase().contains("calibrat"),
        "must surface that the calibrated value was not supplied (range-checked only), per #390's \
         graceful-degradation contract: stdout={stdout:?}"
    );
    assert!(
        stderr.is_empty(),
        "clean compare must not write to stderr. stderr={stderr:?}"
    );
}

#[test]
fn compare_catches_genuine_drift_from_last_calibrated_value_when_supplied() {
    // #390 best-effort tracking: when the operator/agent supplies `av_sync_calibrated_ms=` (the
    // #427-persisted `applied_latency_ms` read from av-sync-last.json on the OBS box), the guard
    // cross-checks the LIVE value against that calibration — catching a genuine drift (e.g. a
    // hand-nudge in the OBS UI since the last calibration run) that the sane-range backstop alone
    // would miss (900ms is well inside [3, 2000], so range-only would report it clean).
    let (code, stdout, stderr) = run_script(&[
        "--compare",
        "host=stream",
        "obs_version=32.1.2",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=30",
        "genlock_wall_clock=1",
        "ndi_input_latency=NDI 2ME PGM=0",
        r"distroav_dll_paths=C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll",
        "genlock_source_latency=NDI 2ME PGM=900", // live value
        "av_sync_calibrated_ms=1000",             // last-calibrated value (#427 persisted)
    ]);
    assert_eq!(
        code, 20,
        "a live value that drifted 100ms from the last calibration must exit 20. \
         stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.to_lowercase().contains("calibrat") && stdout.contains("DRIFT"),
        "must name the calibration-drift check in stdout. stdout={stdout:?}"
    );
    assert!(
        stderr.contains("DRIFT DETECTED"),
        "must fail loudly on stderr. stderr={stderr:?}"
    );
}

#[test]
fn compare_passes_when_live_value_matches_last_calibrated_value() {
    // The counterpart to the drift case above: when the live value matches the last-calibrated
    // value (within the small rounding tolerance), the calibration cross-check must report clean.
    let (code, stdout, stderr) = run_script(&[
        "--compare",
        "host=stream",
        "obs_version=32.2.0",
        "distroav_version=6.2.1",
        "ndi_runtime=6.3.2.0",
        "output_fps=30",
        "genlock_wall_clock=1",
        "ndi_input_latency=NDI 2ME PGM=0",
        r"distroav_dll_paths=C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll",
        "genlock_source_latency=NDI 2ME PGM=1000",
        "av_sync_calibrated_ms=1000",
    ]);
    assert_eq!(
        code, 0,
        "a live value matching the last calibration must pass. stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.to_lowercase().contains("calibrat") && stdout.contains("OK"),
        "must report the calibration check as OK. stdout={stdout:?}"
    );
}

// --- #390: `drift_check_source_latency` range mode (calibration-tracked pins) -------------------

#[test]
fn drift_check_source_latency_range_mode_accepts_any_value_in_the_sane_clamp() {
    // RED: `drift_check_source_latency` has no `range:MIN-MAX` mode yet — it would try an exact
    // string compare of "1000" against "range:3-2000" and always report DRIFT.
    // GREEN: an expected pin shaped `NAME=range:MIN-MAX` checks the observed value falls within
    // [MIN, MAX] inclusive, rather than requiring an exact match.
    let case = |observed: &str| -> String {
        run_sourced(
            r#"rc=0; drift_check_source_latency "$EXP" "$OBS" || rc=$?; echo "RC=$rc""#,
            &[("EXP", "NDI 2ME PGM=range:3-2000"), ("OBS", observed)],
        )
    };

    // any value inside the range → OK, regardless of the exact number (this is the whole point —
    // no single hardcoded constant to go stale).
    for v in ["3", "450", "1000", "2000"] {
        let out = case(&format!("NDI 2ME PGM={v}"));
        assert!(
            out.contains("RC=0") && out.contains("OK"),
            "{v}ms is inside [3,2000] -> must be OK: {out:?}"
        );
    }

    // below the DistroAV clamp minimum → DRIFT
    let below = case("NDI 2ME PGM=2");
    assert!(
        below.contains("RC=2") && below.contains("DRIFT"),
        "2ms is below the sane clamp minimum (3) -> must be DRIFT: {below:?}"
    );

    // above the DistroAV clamp maximum → DRIFT
    let above = case("NDI 2ME PGM=2001");
    assert!(
        above.contains("RC=2") && above.contains("DRIFT"),
        "2001ms is above the sane clamp maximum (2000) -> must be DRIFT: {above:?}"
    );

    // unread → still UNKNOWN, same as the exact-match mode
    let unknown = case("");
    assert!(
        unknown.contains("RC=3") && unknown.contains("UNKNOWN"),
        "empty observed must still return rc 3 (UNKNOWN): {unknown:?}"
    );
}

#[test]
fn drift_check_source_latency_exact_mode_unaffected_by_range_mode() {
    // The strih camera-floor pins (`NDI cam5=3,NDI cam1=3,NDI cam3=3`) are STRUCTURAL, not
    // calibration-tracked — they must keep the strict exact-match behavior #357 already proved
    // (see `drift_check_source_latency_catches_drift_and_passes_on_match` above). This asserts a
    // mixed expected CSV (one exact pin, one range pin) evaluates each entry by its OWN mode.
    let out = run_sourced(
        r#"rc=0; drift_check_source_latency "$EXP" "$OBS" || rc=$?; echo "RC=$rc""#,
        &[
            ("EXP", "NDI cam5=3,NDI 2ME PGM=range:3-2000"),
            ("OBS", "NDI cam5=9,NDI 2ME PGM=1000"), // cam5 drifted off the exact floor; PGM in-range
        ],
    );
    assert!(
        out.contains("RC=2"),
        "the exact-mode cam5 entry must still drift on a mismatch: {out:?}"
    );
    assert!(
        out.contains("NDI cam5") && out.contains("DRIFT"),
        "must name the drifted exact-mode source: {out:?}"
    );
    assert!(
        out.contains("NDI 2ME PGM") && out.contains("OK"),
        "the in-range range-mode source must still report OK: {out:?}"
    );
}

// --- #390: best-effort cross-check against the #427-persisted calibrated value ------------------

#[test]
fn drift_check_calibrated_source_latency_skips_gracefully_when_not_supplied() {
    // RED: `drift_check_calibrated_source_latency` does not exist yet → sourced harness exits
    // non-zero.
    // GREEN: an empty calibrated value (the operator/agent could not read av-sync-last.json off
    // the OBS box — drift-guard itself runs on dev1, not on the box) is a graceful SKIP, never a
    // failure — #390's explicit "do NOT fail the whole drift-guard on its absence" contract.
    let out = run_sourced(
        r#"rc=0; drift_check_calibrated_source_latency "NDI 2ME PGM" "NDI 2ME PGM=1000" "" 10 || rc=$?; echo "RC=$rc""#,
        &[],
    );
    assert!(
        out.contains("RC=0"),
        "no calibrated value supplied must be a graceful pass, not a failure: {out:?}"
    );
    assert!(
        out.to_lowercase().contains("skip") || out.to_lowercase().contains("not available"),
        "must say the calibrated value was not available (range-checked only): {out:?}"
    );
}

#[test]
fn drift_check_calibrated_source_latency_catches_drift_beyond_tolerance() {
    // GREEN: a live value 100ms off the last-calibrated value (tolerance 10ms) is genuine drift.
    let out = run_sourced(
        r#"rc=0; drift_check_calibrated_source_latency "NDI 2ME PGM" "NDI 2ME PGM=900" "1000" 10 || rc=$?; echo "RC=$rc""#,
        &[],
    );
    assert!(
        out.contains("RC=2"),
        "100ms off calibration -> DRIFT: {out:?}"
    );
    assert!(
        out.contains("DRIFT"),
        "must print DRIFT status line: {out:?}"
    );
}

#[test]
fn drift_check_calibrated_source_latency_passes_within_tolerance() {
    // GREEN: a live value within the rounding tolerance of the last-calibrated value is clean.
    let exact = run_sourced(
        r#"rc=0; drift_check_calibrated_source_latency "NDI 2ME PGM" "NDI 2ME PGM=1000" "1000" 10 || rc=$?; echo "RC=$rc""#,
        &[],
    );
    assert!(
        exact.contains("RC=0") && exact.contains("OK"),
        "exact match -> OK: {exact:?}"
    );

    let within = run_sourced(
        r#"rc=0; drift_check_calibrated_source_latency "NDI 2ME PGM" "NDI 2ME PGM=1005" "1000" 10 || rc=$?; echo "RC=$rc""#,
        &[],
    );
    assert!(
        within.contains("RC=0") && within.contains("OK"),
        "5ms off, within +/-10ms tolerance -> OK: {within:?}"
    );
}

#[test]
fn drift_check_calibrated_source_latency_unknown_when_source_unobserved() {
    // A calibrated value WAS supplied but the live per-source latency for that source was not
    // read — this is a genuine UNKNOWN (we meant to check but couldn't), never a silent pass.
    let out = run_sourced(
        r#"rc=0; drift_check_calibrated_source_latency "NDI 2ME PGM" "NDI cam5=3" "1000" 10 || rc=$?; echo "RC=$rc""#,
        &[],
    );
    assert!(
        out.contains("RC=3"),
        "unobserved source -> UNKNOWN: {out:?}"
    );
    assert!(
        out.contains("UNKNOWN"),
        "must print UNKNOWN status line: {out:?}"
    );
}

#[test]
fn range_tracked_source_name_finds_the_calibration_tracked_pin() {
    // RED: `range_tracked_source_name` does not exist yet → sourced harness exits non-zero.
    // GREEN: picks out the (first) source pinned as `range:MIN-MAX` from a mixed expected CSV;
    // "" when none of the pinned sources use range mode (the strih camera-floor pins).
    let mixed = run_sourced(
        r#"range_tracked_source_name "$EXP""#,
        &[("EXP", "NDI cam5=3,NDI cam1=3,NDI 2ME PGM=range:3-2000")],
    );
    assert_eq!(mixed.trim(), "NDI 2ME PGM");

    let none = run_sourced(
        r#"range_tracked_source_name "$EXP""#,
        &[("EXP", "NDI cam5=3,NDI cam1=3,NDI cam3=3")],
    );
    assert_eq!(none.trim(), "");
}

// ---- #463 — imag-nb (Topology v2, EPIC #466) drift-guard host case, gathered over SSH ----

#[test]
fn genlock_latency_ms_from_log_extracts_the_235_single_knob_value() {
    // The real #235 startup log line (also used by genlock_capability_parser_detects_the_235_
    // single_knob_latency_line above) — this parser extracts the NUMBER, not just presence.
    let line = "07:42:38.746: genlock: latency = 3 ms (\u{2248} 0 frames @ 29.970fps) (OBS_GENLOCK_LATENCY_MS) \u{2014} single user-facing latency knob, ts-align implied ON (#235)\n";
    let out = run_sourced("genlock_latency_ms_from_log \"$LOG\"", &[("LOG", line)]);
    assert_eq!(out.trim(), "3", "must extract the ms value: {out:?}");

    // A different value (imag's own pin might differ from strih/stream's 3ms floor).
    let line60 = "07:42:38.746: genlock: latency = 16 ms (\u{2248} 1 frames @ 60.000fps)\n";
    let out60 = run_sourced("genlock_latency_ms_from_log \"$LOG\"", &[("LOG", line60)]);
    assert_eq!(out60.trim(), "16");

    // No genlock latency line at all -> "" (UNKNOWN/absent), never a wrong guess.
    let stock = "11:40:39.376: OBS 32.1.2 (64-bit, linux)\n";
    let out_stock = run_sourced("genlock_latency_ms_from_log \"$LOG\"", &[("LOG", stock)]);
    assert_eq!(
        out_stock.trim(),
        "",
        "no genlock line -> empty: {out_stock:?}"
    );
}

#[test]
fn or_list_exit_code_capture_survives_errexit_the_463_gather_and_check_imag_fix() {
    // #463 review (2nd pass): drift-guard.sh runs under `set -euo pipefail` (line 47) — applied
    // even when the script is SOURCED (the BASH_SOURCE guard only wraps the bottom-of-file
    // dispatch call, not the top-of-file `set`), so `run_sourced`'s harness inherits real
    // errexit the instant it sources the script, exactly like production. `gather_and_check_
    // imag`'s SSH glue captures ssh/timeout's exit code (255 = connection failure, 124 =
    // timeout) to tell "imag-nb is unreachable" apart from "the file is genuinely gone" (the
    // check_imag_report UNKNOWN-vs-DRIFT tests below exercise that decision once the code is
    // read). The FIRST shape of that capture — `var="$(cmd)"` on its own line, followed on the
    // NEXT line by `local rc=$?` — CRASHES THE WHOLE SCRIPT under errexit the instant `cmd`
    // returns nonzero: errexit fires on the failing assignment before the `rc=$?` line ever
    // runs, so a transient network blip would abort drift-guard.sh entirely (exit 255) instead
    // of reporting a graceful UNKNOWN. This test locks in the fix actually shipped:
    // `rc=0; var="$(cmd)" || rc=$?` — an OR-list, the one shape `set -e` exempts (only the LAST
    // command of an AND/OR list is errexit-checked) — survives AND captures the real code.
    let out = run_sourced(
        r#"
        fails_255() { return 255; }
        rc=0
        captured="$(fails_255)" || rc=$?
        echo "SURVIVED rc=$rc captured=[$captured]"
        "#,
        &[],
    );
    assert_eq!(
        out.trim(),
        "SURVIVED rc=255 captured=[]",
        "#463: the OR-list exit-code capture must survive drift-guard.sh's inherited `set -e` \
         and correctly report the failing command's exit code, output={out:?}"
    );
}

// ---- #531 — imag_build_drift_report: DYNAMIC "is imag-nb's genlock build behind origin/main?" ----
// The pre-#531 static empty-pin build check could ONLY ever report UNKNOWN (the genlock_build_sha_imag
// README pin is deliberately empty) — it could NEVER fail, so a merged-but-never-deployed genlock
// change silently reached a live event (#530, imag-nb ran a stale build -> 45fps). imag_build_drift_
// report replaces it: the impure caller runs `git log <box_sha>..origin/main -- vendor/obs-studio
// vendor/distroav` and this PURE function decides OK / DRIFT / UNKNOWN from (box_sha, git_rc,
// range_log) — no live box, no real git repo, fully mockable. The check can now ACTUALLY FAIL.

#[test]
fn imag_build_drift_report_ok_when_box_is_current_with_main_531() {
    // range_log EMPTY (no genlock-touching commit between the box and origin/main) + git ran OK ->
    // the box already carries every genlock change on main -> OK, exit 0, never a DRIFT.
    let body = r#"
        rc=0
        imag_build_drift_report "80dac432" "0" "" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[]);
    assert!(out.contains("RC=0"), "current box -> clean exit: {out:?}");
    assert!(
        out.contains("genlock_build") && out.contains("OK"),
        "must report genlock_build OK: {out:?}"
    );
    assert!(
        !out.contains("DRIFT"),
        "no drift for a current box: {out:?}"
    );
}

#[test]
fn imag_build_drift_report_drift_and_fails_loud_when_box_behind_main_531() {
    // range_log NON-EMPTY (genlock commits on main the box is missing) -> DRIFT, exit 20, with the
    // STALE message + the count + the operator's exact next action. This is the whole point of #531:
    // the guard can now ACTUALLY FAIL when imag-nb's deployed genlock build is behind main (the #530
    // 45fps disaster) — a static empty-pin compare could only ever say UNKNOWN.
    let body = r#"
        rc=0
        RANGE="cb64631fd feat:[green] #501 genlock_monitor low-bandwidth NDI exception
af02f9bc3 feat:[green] #505 orphan the Linux GL PBO"
        imag_build_drift_report "80dac432" "0" "$RANGE" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[]);
    assert!(
        out.contains("RC=20"),
        "box behind main -> DRIFT exit 20: {out:?}"
    );
    assert!(
        out.contains("genlock STALE") && out.contains("behind origin/main"),
        "must FAIL LOUD with the exact 'genlock STALE' / behind-main message (rig-mode.sh's \
         pre-event banner keys on the 'genlock STALE' phrase — cross-script contract): {out:?}"
    );
    assert!(
        out.contains("2 genlock-commit"),
        "must report the count of missing genlock commits: {out:?}"
    );
    assert!(
        out.contains("cb64631fd") && out.contains("af02f9bc3"),
        "must list the stale commit SHAs so the operator sees WHAT is missing: {out:?}"
    );
    assert!(
        out.contains("setup-imag.sh"),
        "must name the operator's deploy action (setup-imag.sh step-12): {out:?}"
    );
}

#[test]
fn imag_build_drift_report_unknown_when_box_sha_unread_531() {
    // box_sha EMPTY (GENLOCK_BUILD_SHA.txt could not be read — SSH failure / marker absent) ->
    // UNKNOWN (exit 11), NEVER a false OK. A box we could not read is not proof it is current.
    let body = r#"
        rc=0
        imag_build_drift_report "" "0" "" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[]);
    assert!(
        out.contains("RC=11"),
        "unread box sha -> UNKNOWN exit 11: {out:?}"
    );
    assert!(
        out.contains("UNKNOWN") && out.contains("not read"),
        "must say UNKNOWN / not read, never OK: {out:?}"
    );
}

#[test]
fn imag_build_drift_report_unknown_when_git_failed_never_a_false_ok_531() {
    // git_rc NON-ZERO (box_sha is not a commit in this checkout, or the fetch is unreachable) -> we
    // could not COMPUTE the drift -> UNKNOWN (exit 11), never a false OK that would hide a real stale
    // build behind a transient git error. Distinct from an empty-range OK.
    let body = r#"
        rc=0
        imag_build_drift_report "80dac432" "128" "" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[]);
    assert!(
        out.contains("RC=11"),
        "git error -> UNKNOWN exit 11, never a false OK: {out:?}"
    );
    assert!(
        out.contains("UNKNOWN") && out.to_lowercase().contains("git"),
        "must say UNKNOWN and name the git failure: {out:?}"
    );
}

// ---- #548 — genlock_build_drift_report is now BOX-AGNOSTIC (the same dynamic staleness verdict for
// strih/stream that imag-nb got in #531). The OBS/DistroAV/NDI version strings drift-guard --compare
// checks are byte-identical across a stock vs genlock build AND across an OLD vs NEW genlock build, so
// only this deployed-SHA-vs-origin/main compare catches a Windows box left on a stale build — the exact
// blind spot that hid the 843-commit deploy-drift. These tests prove the DRIFT/UNKNOWN messages carry
// the caller's box label + deploy action (NOT the hardcoded "imag-nb"/"setup-imag.sh" of the old fn).

#[test]
fn genlock_build_drift_report_carries_windows_box_label_and_action_when_behind_548() {
    // A stale STREAM box (behind origin/main) must FAIL LOUD naming THIS box + THIS box's deploy
    // action — never the imag-nb wording. The old imag-only fn could not produce a stream-labeled
    // verdict; this is the whole point of the #548 generalization.
    let body = r#"
        rc=0
        RANGE="cb64631fd feat:[green] #501 genlock_monitor low-bandwidth NDI exception
af02f9bc3 feat:[green] #505 orphan the Linux GL PBO"
        genlock_build_drift_report "stream" "redeploy the current genlock bundle to this box at a safe off-event time" "e81f8bab" "0" "$RANGE" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[]);
    assert!(
        out.contains("RC=20"),
        "stale stream box -> DRIFT exit 20: {out:?}"
    );
    assert!(
        out.contains("stream genlock STALE") && out.contains("behind origin/main"),
        "must FAIL LOUD naming the stream box (rig-mode.sh keys on 'genlock STALE'): {out:?}"
    );
    assert!(
        out.contains("2 genlock-commit") && out.contains("cb64631fd") && out.contains("af02f9bc3"),
        "must report the count + the stale commit SHAs: {out:?}"
    );
    assert!(
        out.contains("redeploy the current genlock bundle"),
        "must name THIS box's deploy action, not setup-imag.sh: {out:?}"
    );
    assert!(
        !out.contains("imag-nb") && !out.contains("setup-imag.sh"),
        "a stream verdict must NOT carry imag-nb wording: {out:?}"
    );
}

#[test]
fn genlock_build_drift_report_ok_for_non_imag_box_548() {
    // Empty range + git OK -> the box already carries every genlock commit on main -> OK exit 0, for
    // ANY box label (strih here), never a DRIFT.
    let body = r#"
        rc=0
        genlock_build_drift_report "strih" "redeploy via the #548 Windows deploy" "3253b94cd" "0" "" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[]);
    assert!(out.contains("RC=0"), "current box -> clean exit 0: {out:?}");
    assert!(
        out.contains("genlock_build") && out.contains("OK") && !out.contains("DRIFT"),
        "must report genlock_build OK, no drift: {out:?}"
    );
}

#[test]
fn genlock_build_drift_report_unknown_names_the_box_when_sha_unread_548() {
    // box_sha EMPTY (BUNDLE_MANIFEST.json .build_sha not read off the box) -> UNKNOWN exit 11 naming
    // THIS box, never a false OK.
    let body = r#"
        rc=0
        genlock_build_drift_report "stream" "redeploy via the #548 Windows deploy" "" "0" "" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[]);
    assert!(
        out.contains("RC=11"),
        "unread sha -> UNKNOWN exit 11: {out:?}"
    );
    assert!(
        out.contains("UNKNOWN") && out.contains("not read") && out.contains("stream"),
        "must say UNKNOWN / not read and name the stream box, never OK: {out:?}"
    );
}

// ---- #756 — genlock_build_parity_report: CROSS-BOX peer parity (not a ref-compare) ------------
// The ref-compare facets above (imag_/genlock_build_drift_report) read "OK: current with
// origin/main" for a box that is generations behind its PEERS during a long-lived dev train (the
// live boxes run unmerged PR builds AHEAD of origin/main). #756: the imag segfault + GPU wedge
// happened while imag ran a STALE lineage that the ref-compare could never flag. This peer-parity
// facet asserts every fleet box's DEPLOYED build SHA is IDENTICAL — any skew FAILS, no git ref
// involved. Pure: the caller gathers each box's live GENLOCK_BUILD_SHA.txt; this decides.

#[test]
fn genlock_build_parity_report_ok_when_whole_fleet_on_one_build_756() {
    // Every box read + all identical -> OK, exit 0, the fleet is on ONE lineage.
    let body = r#"
        rc=0
        genlock_build_parity_report "imag=26de1c3c2" "strih=26de1c3c2" "stream=26de1c3c2" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[]);
    assert!(out.contains("RC=0"), "unified fleet -> clean exit: {out:?}");
    assert!(
        out.contains("genlock_parity")
            && out.contains("OK")
            && out.contains("ONE genlock build")
            && out.contains("26de1c3c2"),
        "must report genlock_parity OK with the shared build: {out:?}"
    );
    assert!(
        !out.contains("DRIFT"),
        "no drift for a unified fleet: {out:?}"
    );
}

#[test]
fn genlock_build_parity_report_drift_and_fails_loud_when_one_box_skews_756() {
    // imag on an OLDER build than strih/stream (the #756 scenario) -> a proven skew -> DRIFT exit
    // 20, naming every box's SHA + the #460 hot-swap remediation. This is the false OK the
    // ref-compare could not catch during the PR #704 train.
    let body = r#"
        rc=0
        genlock_build_parity_report "imag=STALE111" "strih=26de1c3c2" "stream=26de1c3c2" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[]);
    assert!(
        out.contains("RC=20"),
        "a fleet skew -> DRIFT exit 20: {out:?}"
    );
    assert!(
        out.contains("genlock_parity")
            && out.contains("DRIFT")
            && out.contains("SKEW")
            && out.contains("STALE111")
            && out.contains("26de1c3c2"),
        "DRIFT must name the skew + every box's SHA: {out:?}"
    );
    assert!(
        out.contains("#460") || out.contains("hot-swap"),
        "must give the concrete remediation (hot-swap the lagging box): {out:?}"
    );
}

#[test]
fn genlock_build_parity_report_drift_wins_even_when_a_third_box_is_unread_756() {
    // A DEFINITE skew between the two boxes we COULD read must FAIL LOUD even though a third box is
    // unread — a proven split is a proven split, never downgraded to UNKNOWN by an unreadable peer.
    let body = r#"
        rc=0
        genlock_build_parity_report "imag=STALE111" "strih=26de1c3c2" "stream=" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[]);
    assert!(
        out.contains("RC=20"),
        "a proven skew beats an unread third box -> DRIFT 20, not UNKNOWN: {out:?}"
    );
    assert!(
        out.contains("DRIFT") && out.contains("SKEW"),
        "must report the skew: {out:?}"
    );
}

#[test]
fn genlock_build_parity_report_unknown_when_a_box_is_unread_never_a_false_ok_756() {
    // strih/stream agree but imag's SHA is unread (ssh hiccup) -> the parity picture is INCOMPLETE
    // -> UNKNOWN exit 11, NOT a false OK. imag being stale is the exact thing we must not miss, so
    // an unreadable imag can never pass.
    let body = r#"
        rc=0
        genlock_build_parity_report "imag=" "strih=26de1c3c2" "stream=26de1c3c2" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[]);
    assert!(
        out.contains("RC=11"),
        "an unread box -> UNKNOWN exit 11, never OK: {out:?}"
    );
    assert!(
        out.contains("genlock_parity")
            && out.contains("UNKNOWN")
            && out.contains("INCOMPLETE")
            && out.contains("imag"),
        "must say UNKNOWN/INCOMPLETE and name the unread box: {out:?}"
    );
    assert!(
        !out.contains("OK       "),
        "must not report a false OK: {out:?}"
    );
}

#[test]
fn genlock_build_parity_report_unknown_when_fewer_than_two_peers_756() {
    // Only one box read -> nothing to compare against -> UNKNOWN exit 11 (a parity check needs >=2
    // peers; a lone box proves nothing about fleet agreement).
    let body = r#"
        rc=0
        genlock_build_parity_report "imag=26de1c3c2" "strih=" "stream=" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[]);
    assert!(
        out.contains("RC=11"),
        "fewer than two read peers -> UNKNOWN exit 11: {out:?}"
    );
    assert!(
        out.contains("UNKNOWN") && out.contains("INCOMPLETE"),
        "must report the incomplete/unknown parity: {out:?}"
    );
}

// ---- #949 — genlock_build_parity_report: EQUIV=label_a:label_b overrides a LABEL-only skew ----
// A Windows-only vendor/av-sync-dock/** change advances strih/stream's GENLOCK_BUILD_SHA.txt past
// a SHA imag's build (linux-genlock.yml, which excludes vendor/av-sync-dock/**) can never reach —
// even though imag's actual built bytes never changed. The caller (version-integrity-gate.sh)
// resolves this via real `git diff` (genlock_parity_equivalent, tested further below) and hands the
// verdict in as an EQUIV marker; these tests exercise the PURE decision layer with pre-computed
// markers, no git involved — the real incident SHAs are used purely as realistic literal values.

const IMAG_INCIDENT_SHA: &str = "2a12a6a9991eeeae5580a6fbe047d60275d0c8b2";
const WIN_INCIDENT_SHA: &str = "d77426c758074686b7bc8716962f0042fa8687bf";

#[test]
fn genlock_build_parity_report_ok_when_equiv_markers_cover_every_mismatched_pair_949() {
    // The REAL #949 incident shape: strih/stream share one SHA, imag reports a DIFFERENT (but,
    // per the caller's git-diff check, content-equivalent) SHA. Both mismatched pairs carry an
    // EQUIV marker -> the label-only skew is explained away -> OK, exit 0, never DRIFT.
    let body = format!(
        r#"
        rc=0
        genlock_build_parity_report "imag={IMAG_INCIDENT_SHA}" "strih={WIN_INCIDENT_SHA}" "stream={WIN_INCIDENT_SHA}" \
          "EQUIV=imag:strih" "EQUIV=imag:stream" || rc=$?
        echo "RC=$rc"
        "#
    );
    let out = run_sourced(&body, &[]);
    assert!(
        out.contains("RC=0"),
        "every mismatched pair EQUIV-covered -> clean exit: {out:?}"
    );
    assert!(
        out.contains("genlock_parity") && out.contains("OK") && out.contains("PARITY"),
        "must report genlock_parity OK via the #949 content-equivalence path: {out:?}"
    );
    assert!(
        !out.contains("DRIFT"),
        "an EQUIV-explained label mismatch must never read as DRIFT: {out:?}"
    );
}

#[test]
fn genlock_build_parity_report_still_drifts_on_a_pair_the_equiv_markers_do_not_cover_949() {
    // EQUIV covers imag~strih only; stream is a THIRD, genuinely different, unexplained SHA ->
    // that pair is a real unexplained skew and must still DRIFT even though ANOTHER pair in the
    // same call is EQUIV-covered. EQUIV markers are pair-scoped, never a blanket "trust everything".
    let body = format!(
        r#"
        rc=0
        genlock_build_parity_report "imag={IMAG_INCIDENT_SHA}" "strih={WIN_INCIDENT_SHA}" "stream=UNRELATED999" \
          "EQUIV=imag:strih" || rc=$?
        echo "RC=$rc"
        "#
    );
    let out = run_sourced(&body, &[]);
    assert!(
        out.contains("RC=20"),
        "an unexplained pair must still DRIFT even when a sibling pair is EQUIV-covered: {out:?}"
    );
    assert!(
        out.contains("DRIFT") && out.contains("SKEW"),
        "must report the unexplained skew: {out:?}"
    );
}

#[test]
fn genlock_parity_consumed_paths_matches_the_ci_workflow_path_filters_949() {
    // Lock-step assertion (per this repo's convention, e.g. tests/vendored_cpp_compile_gate.rs):
    // the pure lookup table must never silently drift out of sync with the REAL CI trigger path
    // filters it is meant to mirror. imag's own build trigger is linux-genlock.yml's `on.push.paths`
    // (deliberately WITHOUT vendor/av-sync-dock/**, a Windows-only OBS dock DLL); every other
    // (Windows) box's is windows-genlock-fast.yml's (WITH vendor/av-sync-dock/**).
    let linux_wf =
        std::fs::read_to_string(manifest_dir().join(".github/workflows/linux-genlock.yml"))
            .expect("read linux-genlock.yml");
    let windows_wf =
        std::fs::read_to_string(manifest_dir().join(".github/workflows/windows-genlock-fast.yml"))
            .expect("read windows-genlock-fast.yml");

    assert!(
        linux_wf.contains("vendor/obs-studio/**") && linux_wf.contains("vendor/distroav/**"),
        "linux-genlock.yml must still trigger on vendor/obs-studio/** + vendor/distroav/** — \
         genlock_parity_consumed_paths(imag) mirrors exactly this set"
    );
    assert!(
        !linux_wf.contains("vendor/av-sync-dock/**"),
        "linux-genlock.yml must still NOT trigger on vendor/av-sync-dock/** (a Windows-only OBS \
         dock DLL) — this is the #949 root cause; if this ever starts triggering on it, \
         genlock_parity_consumed_paths(imag) must gain it too"
    );
    assert!(
        windows_wf.contains("vendor/obs-studio/**")
            && windows_wf.contains("vendor/distroav/**")
            && windows_wf.contains("vendor/av-sync-dock/**"),
        "windows-genlock-fast.yml must still trigger on all three vendor dirs — \
         genlock_parity_consumed_paths(<a Windows box>) mirrors exactly this set"
    );

    let body = r#"
        genlock_parity_consumed_paths imag
        echo "---"
        genlock_parity_consumed_paths strih
        echo "---"
        genlock_parity_consumed_paths stream
    "#;
    let out = run_sourced(body, &[]);
    let sections: Vec<&str> = out.split("---\n").collect();
    assert_eq!(sections.len(), 3, "expected 3 sections: {out:?}");
    assert_eq!(
        sections[0].trim(),
        "vendor/obs-studio\nvendor/distroav",
        "imag's consumed set must be exactly the linux-genlock.yml pair, no av-sync-dock: {out:?}"
    );
    for win_section in [sections[1], sections[2]] {
        assert_eq!(
            win_section.trim(),
            "vendor/obs-studio\nvendor/distroav\nvendor/av-sync-dock",
            "a Windows box's consumed set must be exactly windows-genlock-fast.yml's three dirs: {out:?}"
        );
    }
}

// ---- #949 — genlock_parity_equivalent: the IMPURE real-git content check ----------------------
// No live SSH/box needed — runs against THIS repo's own checkout (tests process cwd = crate root,
// per imag_genlock_range_log's established convention above), which has the real incident commits.

#[test]
fn genlock_parity_equivalent_true_for_the_949_incident_pair_over_imags_own_consumed_paths() {
    // THE regression proof at the impure-git layer: strih/stream's real deployed SHA vs imag's real
    // deployed SHA from the live 2026-08-02 incident (run 30768287281 / PR #948) — the ONLY vendor
    // commit between them (a48b56380) touches vendor/av-sync-dock only, which is NOT in imag's own
    // consumed set. Restricted to imag's set (vendor/obs-studio, vendor/distroav), the diff is
    // empty -> content-equivalent -> the fleet's label mismatch is cosmetic, not a real skew.
    let body = format!(
        r#"
        rc=0
        genlock_parity_equivalent "$(pwd)" "{WIN_INCIDENT_SHA}" "{IMAG_INCIDENT_SHA}" \
          $(genlock_parity_consumed_paths imag) || rc=$?
        echo "RC=$rc"
        "#
    );
    let out = run_sourced(&body, &[]);
    assert!(
        out.contains("RC=0"),
        "the #949 incident pair must be content-equivalent over imag's own consumed paths: {out:?}"
    );
}

#[test]
fn genlock_parity_equivalent_false_for_the_949_incident_pair_over_the_windows_consumed_set() {
    // The SAME pair, but checked over a WINDOWS box's own (larger) consumed set — which DOES
    // include vendor/av-sync-dock. This must NOT be equivalent: two Windows boxes genuinely
    // differing in av-sync-dock content IS a real functional skew for them (never laundered away).
    let body = format!(
        r#"
        rc=0
        genlock_parity_equivalent "$(pwd)" "{WIN_INCIDENT_SHA}" "{IMAG_INCIDENT_SHA}" \
          $(genlock_parity_consumed_paths strih) || rc=$?
        echo "RC=$rc"
        "#
    );
    let out = run_sourced(&body, &[]);
    assert_eq!(
        out.trim(),
        "RC=1",
        "over the Windows (av-sync-dock-inclusive) set the same pair must NOT read equivalent \
         (RC must be EXACTLY 1, not e.g. a 127 command-not-found masquerading as a substring \
         match): {out:?}"
    );
}

#[test]
fn genlock_parity_equivalent_false_for_a_real_vendor_obs_studio_skew_949() {
    // Criterion #3: a GENUINE vendor/obs-studio (or vendor/distroav) difference must still DRIFT —
    // the strictness this whole facet exists for must survive #949's fix. Real commit pair: only
    // the newer commit touches vendor/obs-studio/libobs/obs.h + obs-source.c + asrc-compensator.*
    // + the obs-websocket requesthandler — genuinely different built bytes, not a label mismatch.
    const OLDER: &str = "cb92f28a6a90a89b2877f7d00dde93561ae9a70c";
    const NEWER: &str = "f6477a4fe6a7b7a36e6351d13ed106e10d673356";
    let body = format!(
        r#"
        rc=0
        genlock_parity_equivalent "$(pwd)" "{OLDER}" "{NEWER}" \
          $(genlock_parity_consumed_paths imag) || rc=$?
        echo "RC=$rc"
        "#
    );
    let out = run_sourced(&body, &[]);
    assert_eq!(
        out.trim(),
        "RC=1",
        "a genuine vendor/obs-studio content difference must NEVER read as equivalent (RC must \
         be EXACTLY 1): {out:?}"
    );
}

#[test]
fn genlock_parity_equivalent_false_for_an_unresolvable_sha_never_a_false_pass_949() {
    // Fail-closed requirement: a SHA the local repo cannot resolve (never fetched, force-pushed
    // away, a corrupted/garbage marker) must never be silently treated as equivalent.
    let body = format!(
        r#"
        rc=0
        genlock_parity_equivalent "$(pwd)" "0000000000000000000000000000000000000000" "{WIN_INCIDENT_SHA}" \
          $(genlock_parity_consumed_paths strih) || rc=$?
        echo "RC=$rc"
        "#
    );
    let out = run_sourced(&body, &[]);
    assert_eq!(
        out.trim(),
        "RC=1",
        "an unresolvable SHA must fail closed (NOT equivalent, RC must be EXACTLY 1), never a \
         silent pass: {out:?}"
    );
}

// ---- #949 — genlock_parity_diff_paths + the DIFF= marker: naming the offending paths ----------
// The issue body explicitly asked for this: "a message naming the offending paths (not just the
// SHAs — the current message is hard to act on)".

#[test]
fn genlock_parity_diff_paths_names_the_real_files_that_changed_949() {
    const OLDER: &str = "cb92f28a6a90a89b2877f7d00dde93561ae9a70c";
    const NEWER: &str = "f6477a4fe6a7b7a36e6351d13ed106e10d673356";
    let body = format!(
        r#"
        genlock_parity_diff_paths "$(pwd)" "{OLDER}" "{NEWER}" \
          $(genlock_parity_consumed_paths imag)
        "#
    );
    let out = run_sourced(&body, &[]);
    assert!(
        out.contains("vendor/obs-studio/libobs/obs.h"),
        "must name a real changed file: {out:?}"
    );
    assert!(
        out.contains("vendor/obs-studio/libobs/obs-source.c"),
        "must name every real changed file, not just the first: {out:?}"
    );
}

#[test]
fn genlock_parity_diff_paths_empty_for_an_unresolvable_sha_never_a_fabricated_list_949() {
    let body = r#"
        genlock_parity_diff_paths "$(pwd)" "0000000000000000000000000000000000000000" \
          "d77426c758074686b7bc8716962f0042fa8687bf" vendor/obs-studio
    "#;
    let out = run_sourced(body, &[]);
    assert_eq!(
        out.trim(),
        "",
        "an unresolvable sha must yield NO paths — never a fabricated/misleading list: {out:?}"
    );
}

#[test]
fn genlock_build_parity_report_drift_message_names_the_offending_paths_when_diff_marker_given_949()
{
    // A DIFF=label_a:label_b:paths marker (the caller's pre-computed real-git-diff paths) must
    // make it into the DRIFT message for that exact pair — the actionability the issue asked for.
    let body = r#"
        rc=0
        genlock_build_parity_report "imag=AAA" "strih=BBB" "stream=BBB" \
          "DIFF=imag:strih:vendor/obs-studio/libobs/obs.h,vendor/obs-studio/libobs/obs-source.c" \
          "DIFF=imag:stream:vendor/obs-studio/libobs/obs.h,vendor/obs-studio/libobs/obs-source.c" \
          || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[]);
    assert!(out.contains("RC=20"), "still a real DRIFT: {out:?}");
    assert!(
        out.contains("vendor/obs-studio/libobs/obs.h")
            && out.contains("vendor/obs-studio/libobs/obs-source.c"),
        "the DRIFT message must name the actual offending paths from the DIFF= marker: {out:?}"
    );
}

#[test]
fn genlock_build_parity_report_drift_message_has_no_path_detail_without_a_diff_marker_949() {
    // No DIFF= marker supplied (e.g. the caller's git work failed to resolve a path list) -> the
    // message falls back to the pre-#949 wording, never fabricates path detail from nothing.
    let body = r#"
        rc=0
        genlock_build_parity_report "imag=AAA" "strih=BBB" "stream=BBB" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[]);
    assert!(out.contains("RC=20"), "still a real DRIFT: {out:?}");
    assert!(
        !out.contains("changed:"),
        "must never print a '[changed: ...]' annotation with no DIFF= marker to back it: {out:?}"
    );
}

#[test]
fn imag_genlock_range_log_rejects_option_shaped_box_sha_never_a_false_ok_531() {
    // #531 review: `imag_genlock_range_log` feeds an UNVALIDATED box_sha (read over SSH from
    // imag-nb's GENLOCK_BUILD_SHA.txt) straight into `git log "<box_sha>..origin/main" ...`. A
    // corrupted/truncated marker file could be shaped like a git long-option (e.g. "--grep=x").
    // Empirically confirmed (this repo, git 2.43): WITHOUT `--end-of-options`, git log SILENTLY
    // consumes such a value as a real flag and exits 0 with EMPTY output — exactly the "range is
    // empty, box is current" OK verdict `imag_build_drift_report` would report, i.e. a FALSE OK,
    // the precise failure mode #531 exists to eliminate. `--end-of-options` must turn this into a
    // LOUD failure (non-zero exit) instead, so the caller reports UNKNOWN, never a false OK. This
    // function needs no live SSH/box — it runs `git log` against THIS repo's own checkout (the test
    // process's cwd is the crate root), which always has an `origin/main` to compare against.
    let body = r#"
        rc=0
        out="$(imag_genlock_range_log "$(pwd)" "--grep=x")" || rc=$?
        echo "RC=$rc"
        echo "OUT=[$out]"
    "#;
    let out = run_sourced(body, &[]);
    assert!(
        !(out.contains("RC=0") && out.contains("OUT=[]")),
        "an option-shaped box_sha must NEVER silently succeed with an empty range (that reads as \
         a false 'box is current' OK to the caller) — it must fail loud instead: {out:?}"
    );
}

/// A realistic imag-nb OBS log snippet: header + fps + the #235 genlock latency line + the #484
/// RT-pin success line (60fps, imag's low-latency IMAG role, EPIC #466 Topology v2).
const IMAG_LOG_60FPS_3MS: &str = "11:40:39.376: OBS 32.1.2 (64-bit, linux)\n\
11:40:39.714: video settings reset:\n\
11:40:39.714: \tfps:               60/1\n\
07:42:38.746: genlock: latency = 3 ms (\u{2248} 0 frames @ 60.000fps) (OBS_GENLOCK_LATENCY_MS) \u{2014} single user-facing latency knob, ts-align implied ON (#235)\n\
14:27:54.427: genlock: render-tick thread set SCHED_FIFO prio 10 on the isolated core (#484)\n";

/// A clean per-daemon `NAME|DPKG|ACTIVE|ENABLED` block (#596): every competing timesync daemon
/// purged (empty dpkg) + inactive + masked — the real post-provisioning steady state
/// `setup-imag.sh` produces (mirrors `timesync_authority_verdict_ok_on_the_real_post_
/// provisioning_steady_state` in tests/verify_device_pure_functions.rs).
const TIMESYNC_STATES_CLEAN_FIXTURE: &str = "\
systemd-timesyncd||inactive|masked
chrony||inactive|masked
ntp||inactive|masked
ntpsec||inactive|masked
openntpd||inactive|masked";

/// The exact cam5/cam6 #591 failure signature, reused here for imag-nb (#596): systemd-timesyncd
/// installed + active + enabled ALONGSIDE dantesync.
const TIMESYNC_STATES_CAM5_STYLE_FIXTURE: &str = "\
systemd-timesyncd|install ok installed|active|enabled
chrony||inactive|
ntp||inactive|
ntpsec||inactive|
openntpd||inactive|";

#[test]
fn timesync_gather_remote_snippet_output_matches_what_timesync_authority_verdict_expects_596() {
    // Code-review finding (#596): sharing timesync_gather_remote_snippet() between
    // verify-device.sh and drift-guard.sh guarantees the two callers can't diverge FROM EACH
    // OTHER, but nothing proved the snippet's own printf shape actually matches the
    // `NAME|DPKG|ACTIVE|ENABLED` block timesync_authority_verdict parses -- a future edit to the
    // daemon list or the printf format could silently break that seam with no test catching it.
    //
    // This EXECUTES the real snippet locally via a nested `bash -c` -- the exact same read-only
    // `dpkg -s` / `systemctl is-active` / `is-enabled` calls the live SSH flow runs remotely
    // (safe on any Linux CI runner: ubuntu-latest ships both dpkg and systemctl, regardless of
    // which of these 5 packages happen to be installed) -- and feeds the REAL output straight
    // into timesync_authority_verdict, proving the gather<->verdict seam end-to-end rather than
    // only against hand-written Rust fixtures.
    let body = r#"
        snippet="$(timesync_gather_remote_snippet)"
        gathered="$(bash -c "$snippet")"
        printf '%s' "$gathered"
        printf '\n---VERDICT---\n'
        timesync_authority_verdict "$gathered"
    "#;
    let out = run_sourced(body, &[]);
    let (gathered, verdict) = out
        .split_once("---VERDICT---\n")
        .unwrap_or_else(|| panic!("missing ---VERDICT--- marker: {out:?}"));
    let verdict = verdict.trim();

    let lines: Vec<&str> = gathered.lines().filter(|l| !l.is_empty()).collect();
    // #597: linuxptp (ptp4l/phc2sys) widened the competing-daemon set from the 5 stock NTP
    // daemons to 7 -- the 2 linuxptp UNITS ride alongside the 5 NTP daemons in the same block.
    assert_eq!(
        lines.len(),
        7,
        "must gather the 5 stock NTP daemons + linuxptp's 2 units (ptp4l, phc2sys) (#597): \
         {gathered:?}"
    );
    for (line, expected_name) in lines.iter().zip([
        "systemd-timesyncd",
        "chrony",
        "ntp",
        "ntpsec",
        "openntpd",
        "ptp4l",
        "phc2sys",
    ]) {
        let fields: Vec<&str> = line.splitn(4, '|').collect();
        assert_eq!(
            fields.len(),
            4,
            "each line must be NAME|DPKG|ACTIVE|ENABLED (4 pipe-delimited fields): {line:?}"
        );
        assert_eq!(
            fields[0], expected_name,
            "daemon name/order must match what timesync_authority_verdict iterates: {line:?}"
        );
    }
    // Whatever this runner's actual daemon state happens to be, the verdict must be a real
    // decision -- "ok" or "FAIL: <daemon> ...\n"* -- never empty, never a parse error. That
    // proves timesync_authority_verdict can consume the snippet's REAL output end-to-end.
    assert!(
        verdict == "ok" || verdict.starts_with("FAIL: "),
        "verdict must be a real ok/FAIL decision, never empty or malformed: {verdict:?}"
    );
}

#[test]
fn timesync_gather_remote_snippet_pairs_linuxptp_units_with_the_linuxptp_package_status() {
    // #597: ptp4l/phc2sys are systemd UNIT names but ship inside the "linuxptp" dpkg PACKAGE --
    // there is no "ptp4l" or "phc2sys" apt package to query, so `dpkg -s ptp4l` would always read
    // empty even when linuxptp IS installed. Both unit rows must instead carry the dpkg status of
    // the shared "linuxptp" package.
    let body = r#"
        snippet="$(timesync_gather_remote_snippet)"
        gathered="$(bash -c "$snippet")"
        printf '%s' "$gathered"
        printf '\n---REAL_LINUXPTP_DPKG---\n'
        dpkg -s linuxptp 2>/dev/null | sed -n "s/^Status: //p" || true
    "#;
    let out = run_sourced(body, &[]);
    let (gathered, real_dpkg) = out
        .split_once("---REAL_LINUXPTP_DPKG---\n")
        .unwrap_or_else(|| panic!("missing ---REAL_LINUXPTP_DPKG--- marker: {out:?}"));
    let real_dpkg = real_dpkg.trim();
    for name in ["ptp4l", "phc2sys"] {
        let line = gathered
            .lines()
            .find(|l| l.starts_with(&format!("{name}|")))
            .unwrap_or_else(|| panic!("no {name} row in gathered output (#597): {gathered:?}"));
        let fields: Vec<&str> = line.splitn(4, '|').collect();
        assert_eq!(
            fields[1], real_dpkg,
            "{name}'s dpkg field must come from `dpkg -s linuxptp` (the real package), not a \
             per-unit dpkg query that would always read empty: {line:?}"
        );
    }
}

#[test]
fn check_imag_report_clean_when_every_value_matches_the_pinned_set_463() {
    // Full live facet, all EIGHT live-state checks match the pin -> exit 0, every line OK, no
    // DRIFT/UNKNOWN. (#531: the build-identity check moved OUT to imag_build_drift_report.)
    // #489: the 10th/11th args are the dantesync lock pin + a realistic locked journal line —
    // must ALSO read clean, or this "everything matches" case would spuriously report DRIFT/UNKNOWN.
    // #572: the log text now ALSO carries the #484 RT-pin success line, so the 7th genlock_rt_pin
    // check reads OK too — otherwise this "everything matches" case would regress to UNKNOWN.
    // #596: the 12th arg is a clean per-daemon timesync-authority block (imag-nb's own #591
    // extension) — must ALSO read clean, or this "everything matches" case regresses to UNKNOWN
    // (an unsupplied/empty 12th arg defaults to check #8's UNKNOWN branch, never a false DRIFT).
    // #1151: the log ALSO carries the issue-1146 `projector-vsync: present-vsync ARMED` marker so the
    // new REPORT-ONLY projector_vsync facet (check #12) reads OK — otherwise this "everything matches"
    // case would print an UNKNOWN row and trip the `!contains("UNKNOWN")` assertion below. The facet
    // touches no counter, so this addition changes no exit code; it only feeds the OK-row read.
    let log = format!(
        "genlock: latency = 3 ms\n{GENLOCK_RT_PIN_OK_LINE}\n\
         15:52:14.820: projector-vsync: present-vsync ARMED (GL/EGL swap interval 1; no-op on D3D11)"
    );
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "$LOG" "/usr/lib/x86_64-linux-gnu/obs-plugins/distroav.so" "1" "locked" "Jul 05 10:15:22 imag-nb dantesync[1234]: [PTP] LOCK Drift 12 ns/s offset -340ns" "$TS_STATES" "$POWER" "29" "$DP" "$CL" || rc=$?
        echo "RC=$rc"
    "#;
    // #1040: the 13th/14th args are a clean power-envelope block + the pinned 29 W — must ALSO read
    // clean, or this "everything matches" case regresses to UNKNOWN on the new power_envelope check.
    // #780: the 15th arg is a clean display-path block — same reason, or this case regresses to
    // UNKNOWN on the new display_path check #10.
    // #784: the 16th arg is a clean /proc/cmdline block — same reason, or this case regresses to
    // UNKNOWN on the new cmdline_isolation check #11.
    let out = run_sourced(
        body,
        &[
            ("LOG", &log),
            ("TS_STATES", TIMESYNC_STATES_CLEAN_FIXTURE),
            ("POWER", POWER_GATHER_CLEAN_29W),
            ("DP", DISPLAY_PATH_GATHER_CLEAN),
            ("CL", CMDLINE_GATHER_CLEAN),
        ],
    );
    assert!(
        out.contains("RC=0"),
        "every value matches -> clean: {out:?}"
    );
    assert!(!out.contains("DRIFT"), "no drift line expected: {out:?}");
    assert!(
        !out.contains("UNKNOWN"),
        "no unknown line expected: {out:?}"
    );
    // Every one of the 8 live-state checks must print its own OK line (comprehensive-logging:
    // values, not just a bare pass/fail).
    // #531: `genlock_build_sha` (the retired static build check) is NO LONGER one of check_imag_
    // report's lines — the DYNAMIC build-staleness now lives in imag_build_drift_report, run
    // separately by gather_and_check_imag. So this function prints these 8 live-state lines.
    for label in [
        "distroav_so_sha256",
        "genlock_capability",
        "output_fps_imag",
        "genlock_latency_ms_imag",
        "distroav_so_path",
        "dantesync_locked",
        "genlock_rt_pin",
        "timesync_authority",
    ] {
        assert!(out.contains(label), "must report {label}: {out:?}");
    }
}

#[test]
fn check_imag_report_timesync_authority_ok_when_no_competing_daemon_596() {
    // #596: extend #591's sole-timesync-authority gate to imag-nb via drift-guard's --check-imag.
    // A clean per-daemon block -> the timesync_authority line reads OK.
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "genlock: latency = 3 ms" "/plugin/path" "1" "" "" "$TS_STATES" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[("TS_STATES", TIMESYNC_STATES_CLEAN_FIXTURE)]);
    let line = out
        .lines()
        .find(|l| l.contains("timesync_authority"))
        .unwrap_or_else(|| panic!("no timesync_authority line printed: {out:?}"));
    assert!(line.contains("OK"), "must report OK: {line:?}");
}

#[test]
fn check_imag_report_timesync_authority_drift_when_competing_daemon_installed_596() {
    // #596: the EXACT #591/cam5-cam6 signature (systemd-timesyncd installed+active+enabled
    // alongside dantesync) reaching imag-nb via drift-guard's --check-imag MUST now FAIL loud —
    // before this fix, drift-guard silently ignored this signal (the gate PASSED regardless).
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "genlock: latency = 3 ms" "/plugin/path" "1" "" "" "$TS_STATES" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[("TS_STATES", TIMESYNC_STATES_CAM5_STYLE_FIXTURE)]);
    assert!(
        out.contains("RC=20"),
        "a competing timesync daemon installed+active+enabled must DRIFT (exit 20): {out:?}"
    );
    let line = out
        .lines()
        .find(|l| l.contains("timesync_authority"))
        .unwrap_or_else(|| panic!("no timesync_authority line printed: {out:?}"));
    assert!(
        line.contains("DRIFT") && line.contains("systemd-timesyncd"),
        "must flag the offending daemon by name as DRIFT: {line:?}"
    );
}

#[test]
fn check_imag_report_timesync_authority_drift_reason_has_no_double_space_596() {
    // Code-review finding (#596): the INSTALLED reason's own text already contains a semicolon
    // ("...runs only dantesync; masking is not enough)"). The reason-joining pipeline must not
    // blanket-replace THAT semicolon too (which previously produced "dantesync;  masking" with a
    // double space) -- only the delimiter BETWEEN multiple daemons' reasons gets the "; " spacing.
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "genlock: latency = 3 ms" "/plugin/path" "1" "" "" "$TS_STATES" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[("TS_STATES", TIMESYNC_STATES_CAM5_STYLE_FIXTURE)]);
    let line = out
        .lines()
        .find(|l| l.contains("timesync_authority"))
        .unwrap_or_else(|| panic!("no timesync_authority line printed: {out:?}"));
    // The report line itself uses printf column-padding spaces (e.g. "DRIFT    (") which are NOT
    // the bug -- only a semicolon immediately followed by TWO spaces (";  ") is the double-space
    // signature the blanket `sed 's/;/; /g'` produced when it also matched the semicolon that was
    // already part of the INSTALLED reason's own text ("...only dantesync; masking...").
    assert!(
        !line.contains(";  "),
        "a semicolon inside the reason text must not gain a double space: {line:?}"
    );
    assert!(
        line.contains("dantesync; masking"),
        "the reason text must keep its ORIGINAL single-space semicolon intact: {line:?}"
    );
}

#[test]
fn check_imag_report_timesync_authority_unknown_when_not_read_596() {
    // Empty gathered block (SSH failure, or the remote per-daemon loop produced no output at
    // all) -> UNKNOWN, never a false OK for a mere connectivity hiccup — mirrors every other
    // two-tier check in this function (genlock_capability / genlock_rt_pin / dantesync_locked).
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "genlock: latency = 3 ms" "/plugin/path" "1" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[]);
    let line = out
        .lines()
        .find(|l| l.contains("timesync_authority"))
        .unwrap_or_else(|| panic!("no timesync_authority line printed: {out:?}"));
    assert!(
        line.contains("UNKNOWN"),
        "an unread timesync-daemon block must report UNKNOWN, never a false OK/DRIFT: {line:?}"
    );
}

// #1040: power/thermal-envelope facet — the 13th (power gather block) + 14th (pinned watts)
// optional args of check_imag_report. A clean gather at the pinned 29 W is OK; a 25 W clamp is
// DRIFT (the whole regression signature); an unread block is UNKNOWN, never a false DRIFT.
const POWER_GATHER_CLEAN_29W: &str = "\
ZONE|package-0
CONSTRAINT|package-0|1|long_term|29000000
ENABLED|package-0|1
SLPC|1
THERMALD||inactive|not-found
UNIT|imag-power-envelope.service|enabled|active
UNIT|imag-power-envelope-guard.timer|enabled|active
TCPU|84
";

#[test]
fn check_imag_report_power_envelope_ok_when_pl1_matches_the_pin_1040() {
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "genlock: latency = 3 ms" "/plugin/path" "1" "" "" "" "$POWER" "29" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[("POWER", POWER_GATHER_CLEAN_29W)]);
    // Scope this to the POWER facet: every power_envelope row must be OK, and the clean envelope
    // must contribute NO DRIFT (the sibling dantesync/timesync rows read UNKNOWN here only because
    // this test deliberately leaves their args empty — that is unrelated to the power facet).
    let power_rows: Vec<&str> = out
        .lines()
        .filter(|l| l.contains("power_envelope"))
        .collect();
    assert!(
        power_rows.len() >= 4,
        "expected pl1/slpc/thermald/units power rows: {out:?}"
    );
    for l in &power_rows {
        assert!(
            l.contains("OK"),
            "every power_envelope row must read OK on a clean 29 W gather: {l:?}"
        );
    }
}

#[test]
fn check_imag_report_power_envelope_drift_when_pl1_clamped_to_25w_1040() {
    // The exact regression signature: MMIO PL1 clamped to 25 W while the pin is 29 W.
    let clamped = POWER_GATHER_CLEAN_29W.replace("long_term|29000000", "long_term|25000000");
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "genlock: latency = 3 ms" "/plugin/path" "1" "" "" "" "$POWER" "29" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[("POWER", &clamped)]);
    assert!(
        out.contains("RC=20"),
        "a 25 W clamp vs pinned 29 W must DRIFT (exit 20): {out:?}"
    );
    let line = out
        .lines()
        .find(|l| l.contains("power_envelope") && l.contains("pl1"))
        .unwrap_or_else(|| panic!("no power_envelope pl1 DRIFT row printed: {out:?}"));
    assert!(
        line.contains("DRIFT"),
        "the pl1 row must read DRIFT: {line:?}"
    );
}

#[test]
fn check_imag_report_power_envelope_unknown_when_not_gathered_backward_compat_1040() {
    // Old 12-arg call sites (no power gather block) must still get a graceful UNKNOWN power row,
    // never an `unbound variable` crash under set -u nor a false DRIFT.
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "genlock: latency = 3 ms" "/plugin/path" "1" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[]);
    let line = out
        .lines()
        .find(|l| l.contains("power_envelope"))
        .unwrap_or_else(|| panic!("no power_envelope row printed on a 9-arg call: {out:?}"));
    assert!(
        line.contains("UNKNOWN"),
        "an unread power-envelope block must report UNKNOWN, never a false DRIFT: {line:?}"
    );
}

// #780/issue 1146 REVERT: check_imag_report's check #10 — the display-path facet (picom NOT
// running + unit NOT enabled — the compositor cost 21.57% render skips on the 25W envelope, live
// 2026-08-20, so the #841 picom-off doctrine stands; HDMI the xrandr primary, the #841 iGPU
// max-freq pin, the #779 tap conf) — passed as the 15th (optional) arg. A clean gather is OK;
// picom RUNNING / a non-HDMI primary / a lost tap conf is DRIFT (exit 20); an unread block is
// UNKNOWN, never a false DRIFT (backward-compat with 9..14-arg call sites). Full doctrine
// history: scripts/lib/imag-display-path.sh's header (reversal + same-day revert).
const DISPLAY_PATH_GATHER_CLEAN: &str = "\
PICOM_PGREP|ok
PICOM_PROC|
PICOM_SERVICE|disabled
XRANDR|ok
PRIMARY_OUTPUT|HDMI-1
MAXPERF_APPLICABLE|1
MAXPERF_MIN|1400
MAXPERF_RP0|1400
MAXPERF_ENABLED|enabled
MAXPERF_ACTIVE|active
TAPCONF|present
TAPCONF_TAPPING|on
";

#[test]
fn check_imag_report_display_path_ok_when_every_facet_clean_780() {
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "genlock: latency = 3 ms" "/plugin/path" "1" "" "" "" "" "" "$DP" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[("DP", DISPLAY_PATH_GATHER_CLEAN)]);
    let dp_rows: Vec<&str> = out
        .lines()
        .filter(|l| l.contains("display_path/"))
        .collect();
    assert!(
        dp_rows.len() == 5,
        "expected picom_process/picom_service/hdmi_primary/igpu_maxperf/tap_conf rows: {out:?}"
    );
    for l in &dp_rows {
        assert!(
            l.contains("OK"),
            "every display_path row must read OK on a clean gather: {l:?}"
        );
    }
}

#[test]
fn check_imag_report_display_path_drift_when_picom_running_1146_revert() {
    // issue 1146 REVERT: picom RUNNING starves the OBS render (21.57% skips measured on the 25W
    // envelope) -> DRIFT (exit 20). The compositor's PRESENCE is the drift again (#841 stands).
    let stopped = DISPLAY_PATH_GATHER_CLEAN.replace("PICOM_PROC|", "PICOM_PROC|2038724");
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "genlock: latency = 3 ms" "/plugin/path" "1" "" "" "" "" "" "$DP" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[("DP", &stopped)]);
    assert!(
        out.contains("RC=20"),
        "picom running must DRIFT (exit 20, issue 1146 revert): {out:?}"
    );
    let line = out
        .lines()
        .find(|l| l.contains("display_path/picom_process"))
        .unwrap_or_else(|| panic!("no picom_process row printed: {out:?}"));
    assert!(
        line.contains("DRIFT"),
        "picom_process must DRIFT when picom is running: {line:?}"
    );
}

#[test]
fn check_imag_report_display_path_drift_when_panel_is_primary_1146() {
    // issue 1146: the panel as xrandr primary makes IT the vsync anchor -> the projector tears.
    let panel = DISPLAY_PATH_GATHER_CLEAN.replace("PRIMARY_OUTPUT|HDMI-1", "PRIMARY_OUTPUT|eDP-1");
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "genlock: latency = 3 ms" "/plugin/path" "1" "" "" "" "" "" "$DP" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[("DP", &panel)]);
    assert!(
        out.contains("RC=20"),
        "a non-HDMI primary must DRIFT (exit 20): {out:?}"
    );
    let line = out
        .lines()
        .find(|l| l.contains("display_path/hdmi_primary"))
        .unwrap_or_else(|| panic!("no hdmi_primary row printed: {out:?}"));
    assert!(
        line.contains("DRIFT") && line.contains("eDP-1"),
        "hdmi_primary must DRIFT naming the wrong primary: {line:?}"
    );
}

#[test]
fn check_imag_report_display_path_drift_when_tap_conf_gone_780() {
    // #779 tap conf removed — a gathered-but-absent conf is a real DRIFT, not an SSH-hiccup UNKNOWN.
    // Baseline is the issue-1146 clean gather (picom running, service enabled, HDMI primary) with
    // ONLY the tap conf gone, so the drift isolates to tap_conf.
    let gone = DISPLAY_PATH_GATHER_CLEAN
        .replace("TAPCONF|present\nTAPCONF_TAPPING|on\n", "TAPCONF|absent\n");
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "genlock: latency = 3 ms" "/plugin/path" "1" "" "" "" "" "" "$DP" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[("DP", &gone)]);
    assert!(
        out.contains("RC=20"),
        "a lost tap conf must DRIFT (exit 20): {out:?}"
    );
    let line = out
        .lines()
        .find(|l| l.contains("display_path/tap_conf"))
        .unwrap_or_else(|| panic!("no tap_conf row printed: {out:?}"));
    assert!(
        line.contains("DRIFT"),
        "tap_conf must DRIFT when the conf is gone: {line:?}"
    );
}

#[test]
fn check_imag_report_display_path_unknown_when_not_gathered_backward_compat_780() {
    // Old 9..14-arg call sites (no display-path gather block) must still get a graceful UNKNOWN
    // display_path row — never an `unbound variable` crash under set -u nor a false DRIFT.
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "genlock: latency = 3 ms" "/plugin/path" "1" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[]);
    let line = out
        .lines()
        .find(|l| l.contains("display_path"))
        .unwrap_or_else(|| panic!("no display_path row printed on a 9-arg call: {out:?}"));
    assert!(
        line.contains("UNKNOWN"),
        "an unread display-path block must report UNKNOWN, never a false DRIFT: {line:?}"
    );
}

// #531: the old `check_imag_report_flags_a_wrong_deployed_build_sha_463` test lived here — it
// exercised check_imag_report's static build-SHA compare (box GENLOCK_BUILD_SHA.txt == an empty
// genlock_build_sha_imag README pin), which was inert (always UNKNOWN, never DRIFT). That static
// check is RETIRED; the DYNAMIC build-staleness (box vs origin/main's vendored-genlock HEAD) is now
// covered by the `imag_build_drift_report_*_531` tests above, which CAN actually fail.

#[test]
fn check_imag_report_flags_an_fps_drift_down_from_60_463() {
    // #463/#459: imag holds the 60fps low-latency IMAG role (Topology v2) — a drift DOWN to 30
    // (strih's rate) is exactly the kind of silent regression this pin exists to catch.
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "30" "3" "3" "genlock: latency = 3 ms" "/plugin/path" "1" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[]);
    assert!(out.contains("RC=20"), "fps drift -> DRIFT exit: {out:?}");
    assert!(
        out.contains("output_fps_imag") && out.contains("DRIFT"),
        "must flag the fps line as DRIFT: {out:?}"
    );
}

#[test]
fn check_imag_report_flags_missing_genlock_capability_as_stock_build_drift_463() {
    // A NON-EMPTY log (real header lines) that carries NO genlock marker at all -> the #119
    // wrong-build case: DRIFT, never silently passed just because the build-SHA marker file
    // happens to still be present. #463 review: the 9th param is the RAW log text (checked
    // internally via genlock_capability_from_log), so this must be a log that WAS actually
    // read -- an empty string means something different now (see the UNKNOWN test below).
    let stock_log = "11:40:39.376: OBS 32.1.2 (64-bit, linux)\n11:40:39.714: video settings reset:\n11:40:39.714: \tfps:               60/1\n";
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "$LOG" "/plugin/path" "1" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[("LOG", stock_log)]);
    assert!(
        out.contains("RC=20"),
        "no capability marker in a non-empty log -> DRIFT exit: {out:?}"
    );
    assert!(
        out.contains("genlock_capability") && out.contains("DRIFT"),
        "must flag the capability line as DRIFT: {out:?}"
    );
}

#[test]
fn check_imag_report_capability_unknown_when_the_log_was_never_read_463() {
    // #463 review: an EMPTY log text (SSH failed to reach imag-nb, or OBS has never launched
    // there) must read as UNKNOWN, never the same DRIFT a genuine stock/wrong build gets --
    // otherwise a mere connectivity hiccup prints a false "#119 wrong-build" alarm. Mirrors the
    // sibling strih/stream `drift_check_capability`'s own empty-text guard.
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "" "/plugin/path" "1" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[]);
    assert!(
        out.contains("RC=11"),
        "empty log text -> UNKNOWN exit (never the DRIFT a stock build gets): {out:?}"
    );
    let capability_line = out
        .lines()
        .find(|l| l.contains("genlock_capability"))
        .unwrap_or_else(|| panic!("no genlock_capability line printed: {out:?}"));
    assert!(
        capability_line.contains("UNKNOWN"),
        "the capability line must say UNKNOWN, never a false #119 wrong-build DRIFT for an \
         unread log: {capability_line:?}"
    );
}

#[test]
fn check_imag_report_flags_the_plugin_path_missing_463() {
    // distroav.so is not found at the canonical Linux plugin path -> DRIFT (mirrors the
    // strih/stream canonical_plugin_path shadow-copy invariant, #124/#125).
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "genlock: latency = 3 ms" "/usr/lib/x86_64-linux-gnu/obs-plugins/distroav.so" "0" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[]);
    assert!(
        out.contains("RC=20"),
        "missing plugin path -> DRIFT exit: {out:?}"
    );
    assert!(
        out.contains("distroav_so_path") && out.contains("DRIFT"),
        "must flag the plugin path line as DRIFT: {out:?}"
    );
}

#[test]
fn check_imag_report_unknown_when_values_were_not_read_never_a_silent_pass_463() {
    // Every observed value is empty (SSH failed / paths unreachable) -> UNKNOWN (exit 11), NEVER
    // reported clean — the "we meant to check but couldn't" case must never read as a pass.
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "" "60" "" "3" "" "" "/plugin/path" "" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[]);
    assert!(
        out.contains("RC=11") || out.contains("RC=20"),
        "unread values must never report clean (RC=0): {out:?}"
    );
    assert!(out.contains("UNKNOWN"), "must print UNKNOWN lines: {out:?}");
}

#[test]
fn check_imag_report_flags_genlock_rt_pin_failure_as_drift_572() {
    // The EXACT #572 signature: the OBS log shows the render-tick thread stuck SCHED_OTHER
    // (missing rtprio ulimit grant) -> DRIFT, exit 20 — even though every other value matches.
    let log = format!("genlock: latency = 3 ms\n{GENLOCK_RT_PIN_FAILED_LINE}");
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "$LOG" "/plugin/path" "1" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[("LOG", log.as_str())]);
    assert!(
        out.contains("RC=20"),
        "the #572 SCHED_OTHER fallback line must DRIFT: {out:?}"
    );
    assert!(
        out.contains("genlock_rt_pin") && out.contains("DRIFT"),
        "must flag the genlock_rt_pin line as DRIFT: {out:?}"
    );
}

#[test]
fn check_imag_report_genlock_rt_pin_ok_when_pin_achieved_572() {
    // Only 9 positional args supplied (no dantesync pair) -> that row defaults to UNKNOWN
    // (mirrors check_imag_report_flags_the_plugin_path_missing_463's own 9-arg convention), so
    // this asserts the genlock_rt_pin LINE specifically rather than the overall exit code.
    let log = format!("genlock: latency = 3 ms\n{GENLOCK_RT_PIN_OK_LINE}");
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "$LOG" "/plugin/path" "1" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[("LOG", log.as_str())]);
    let line = out
        .lines()
        .find(|l| l.contains("genlock_rt_pin"))
        .unwrap_or_else(|| panic!("no genlock_rt_pin line printed: {out:?}"));
    assert!(line.contains("OK"), "must report OK: {line:?}");
}

#[test]
fn check_imag_report_genlock_rt_pin_unknown_when_log_never_read_572() {
    // Empty log text (SSH failure, or OBS never launched) -> UNKNOWN, never a false DRIFT.
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "" "/plugin/path" "1" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[]);
    let line = out
        .lines()
        .find(|l| l.contains("genlock_rt_pin"))
        .unwrap_or_else(|| panic!("no genlock_rt_pin line printed: {out:?}"));
    assert!(
        line.contains("UNKNOWN"),
        "an unread log must report UNKNOWN, never a false DRIFT: {line:?}"
    );
}

#[test]
fn check_imag_report_genlock_rt_pin_unknown_when_marker_absent_from_a_read_log_572() {
    // A NON-EMPTY log (real content, genlock capability marker present) that predates #484 —
    // carries NEITHER the success nor the failure RT-pin line -> UNKNOWN (the separate dynamic
    // build-staleness facet, imag_build_drift_report, already flags a stale build; this facet
    // must not guess at a pin outcome the build never attempted).
    let pre_484_log = "11:40:39.376: OBS 32.1.2 (64-bit, linux)\ngenlock: latency = 3 ms\n";
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "$LOG" "/plugin/path" "1" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[("LOG", pre_484_log)]);
    let line = out
        .lines()
        .find(|l| l.contains("genlock_rt_pin"))
        .unwrap_or_else(|| panic!("no genlock_rt_pin line printed: {out:?}"));
    assert!(
        line.contains("UNKNOWN"),
        "a read log with no RT-pin marker at all must report UNKNOWN, never a guess: {line:?}"
    );
    assert!(
        out.contains("RC=11"),
        "no other drift in this fixture -> overall UNKNOWN exit: {out:?}"
    );
}

#[test]
fn check_imag_report_end_to_end_from_a_realistic_imag_log_463() {
    // Parse a realistic imag-nb OBS log (IMAG_LOG_60FPS_3MS) through the real fps/latency
    // *_from_log parsers, THEN feed the RAW log text (not a pre-extracted capability flag,
    // #463 review) into check_imag_report — the same wiring `gather_and_check_imag` does,
    // minus the actual `ssh` calls. check_imag_report derives the capability marker itself.
    // #489: also feed a realistic locked dantesync journal snippet through the real
    // dantesync_locked_from_log parser, mirroring the same end-to-end wiring for the new pin.
    // #596: also feed a clean per-daemon timesync-authority block — the imag-nb extension of
    // #591's sole-clock-authority gate — through the shared timesync_authority_verdict.
    let body = r#"
        fps="$(fps_from_log "$LOG")"
        latency="$(genlock_latency_ms_from_log "$LOG")"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "$fps" "3" "$latency" "$LOG" "/plugin/path" "1" "locked" "$DANTESYNC_LOG" "$TS_STATES" "$POWER" "29" "$DP" "$CL" || rc=$?
        echo "RC=$rc"
    "#;
    // #1040: a clean power-envelope block + pinned 29 W keeps this end-to-end case fully clean.
    // #780: a clean display-path block (15th) keeps it clean on the new display_path check #10.
    // #784: a clean /proc/cmdline block (16th) keeps it clean on the new cmdline_isolation check #11.
    let out = run_sourced(
        body,
        &[
            ("LOG", IMAG_LOG_60FPS_3MS),
            ("DANTESYNC_LOG", DANTESYNC_LOG_LOCKED_FIXTURE),
            ("TS_STATES", TIMESYNC_STATES_CLEAN_FIXTURE),
            ("POWER", POWER_GATHER_CLEAN_29W),
            ("DP", DISPLAY_PATH_GATHER_CLEAN),
            ("CL", CMDLINE_GATHER_CLEAN),
        ],
    );
    assert!(
        out.contains("RC=0"),
        "a real 60fps/3ms imag log + a locked dantesync journal + a clean timesync-authority \
         block parsed end-to-end must match cleanly: {out:?}"
    );
}

/// A realistic imag-nb `journalctl -u dantesync` snippet with the DanteSync PTP LOCK markers
/// (the SAME markers `scripts/setup-imag.sh`'s own provisioning-time restart check keys on,
/// setup-imag.sh:230). #489.
const DANTESYNC_LOG_LOCKED_FIXTURE: &str = "\
Jul 05 10:15:20 imag-nb dantesync[1234]: [PTP] LOCK Drift 14 ns/s offset -412ns\n\
Jul 05 10:15:22 imag-nb dantesync[1234]: [PTP] LOCK Drift 12 ns/s offset -340ns\n";

/// A realistic imag-nb `journalctl -u dantesync` snippet running but WITHOUT ever reporting a
/// PTP/NTP lock (e.g. grandmaster unreachable, clock never disciplined) — the genuine #489 DRIFT
/// case: the service IS up (non-empty journal) but the clock basis genlock depends on never
/// locked.
const DANTESYNC_LOG_UNLOCKED_FIXTURE: &str = "\
Jul 05 10:15:20 imag-nb dantesync[1234]: starting DanteSync -i eth0 --ntp-server strih.lan\n\
Jul 05 10:15:21 imag-nb dantesync[1234]: [PTP] searching for grandmaster...\n";

#[test]
fn dantesync_locked_from_log_reports_locked_on_a_ptp_lock_line_489() {
    let out = run_sourced(
        "dantesync_locked_from_log \"$LOG\"",
        &[("LOG", DANTESYNC_LOG_LOCKED_FIXTURE)],
    );
    assert_eq!(
        out.trim(),
        "locked",
        "a PTP LOCK line must report locked: {out:?}"
    );
}

#[test]
fn dantesync_locked_from_log_reports_locked_on_a_ptp_nano_line_489() {
    let nano_log = "Jul 05 10:15:20 imag-nb dantesync[1234]: [PTP] NANO Drift 2 ns/s offset -8ns\n";
    let out = run_sourced("dantesync_locked_from_log \"$LOG\"", &[("LOG", nano_log)]);
    assert_eq!(
        out.trim(),
        "locked",
        "a PTP NANO line must ALSO report locked (the ops-skill NANO variant): {out:?}"
    );
}

#[test]
fn dantesync_locked_from_log_reports_locked_on_an_ntp_offset_line_489() {
    let ntp_log = "Jul 05 10:15:20 imag-nb dantesync[1234]: [NTP] offset: 213us\n";
    let out = run_sourced("dantesync_locked_from_log \"$LOG\"", &[("LOG", ntp_log)]);
    assert_eq!(
        out.trim(),
        "locked",
        "an NTP offset line (grandmaster-absent fallback) must ALSO report locked: {out:?}"
    );
}

#[test]
fn dantesync_locked_from_log_reports_unlocked_when_running_but_never_locked_489() {
    // Non-empty journal (dantesync IS running / was read successfully) but NO lock marker at
    // all -> "unlocked", the genuine drift case -- never confused with an unread/empty journal.
    let out = run_sourced(
        "dantesync_locked_from_log \"$LOG\"",
        &[("LOG", DANTESYNC_LOG_UNLOCKED_FIXTURE)],
    );
    assert_eq!(
        out.trim(),
        "unlocked",
        "a non-empty journal with no lock marker must report unlocked: {out:?}"
    );
}

#[test]
fn dantesync_locked_from_log_empty_when_journal_never_read_489() {
    // Empty log text (SSH failed, or dantesync was never read) -> "" (UNKNOWN upstream), never a
    // false "unlocked" for a mere connectivity hiccup — mirrors genlock_capability_from_log's own
    // empty-text handling.
    let out = run_sourced("dantesync_locked_from_log \"$LOG\"", &[("LOG", "")]);
    assert_eq!(
        out.trim(),
        "",
        "an unread journal must report empty (UNKNOWN), not unlocked: {out:?}"
    );
}

#[test]
fn check_imag_report_dantesync_lock_ok_when_locked_and_pinned_489() {
    // Every OTHER value also clean, so the dantesync row is the only one exercised in isolation.
    // #572: the log now also carries the #484 RT-pin success line so this "everything else is
    // clean" case doesn't regress to UNKNOWN on the new genlock_rt_pin check.
    // #596: also pass a clean timesync-authority block so this case doesn't regress to UNKNOWN on
    // the new check #8.
    let log = format!("genlock: latency = 3 ms\n{GENLOCK_RT_PIN_OK_LINE}");
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "$LOG" "/plugin/path" "1" "locked" "$DANTESYNC_LOG" "$TS_STATES" "$POWER" "29" "$DP" "$CL" || rc=$?
        echo "RC=$rc"
    "#;
    // #1040: a clean power-envelope block + pinned 29 W keeps this "everything else clean" case clean.
    // #780: a clean display-path block (15th) keeps it clean on the new display_path check #10.
    // #784: a clean /proc/cmdline block (16th) keeps it clean on the new cmdline_isolation check #11.
    let out = run_sourced(
        body,
        &[
            ("LOG", log.as_str()),
            ("DANTESYNC_LOG", DANTESYNC_LOG_LOCKED_FIXTURE),
            ("TS_STATES", TIMESYNC_STATES_CLEAN_FIXTURE),
            ("POWER", POWER_GATHER_CLEAN_29W),
            ("DP", DISPLAY_PATH_GATHER_CLEAN),
            ("CL", CMDLINE_GATHER_CLEAN),
        ],
    );
    assert!(out.contains("RC=0"), "locked matches pin -> clean: {out:?}");
    let line = out
        .lines()
        .find(|l| l.contains("dantesync_locked"))
        .unwrap_or_else(|| panic!("no dantesync_locked line printed: {out:?}"));
    assert!(line.contains("OK"), "must report OK: {line:?}");
}

#[test]
fn check_imag_report_dantesync_lock_drift_when_running_but_not_locked_489() {
    // dantesync IS reachable (non-empty journal) but never reports a lock -> DRIFT, exit 20 —
    // genlock's wall-clock basis is compromised even if every other pin still looks clean.
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "genlock: latency = 3 ms" "/plugin/path" "1" "locked" "$DANTESYNC_LOG" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[("DANTESYNC_LOG", DANTESYNC_LOG_UNLOCKED_FIXTURE)]);
    assert!(
        out.contains("RC=20"),
        "dantesync running but never locked -> DRIFT exit: {out:?}"
    );
    assert!(
        out.contains("dantesync_locked") && out.contains("DRIFT"),
        "must flag the dantesync_locked line as DRIFT: {out:?}"
    );
}

#[test]
fn check_imag_report_dantesync_lock_unknown_when_journal_not_read_489() {
    // Empty journal text (SSH failed to read journalctl on imag-nb) -> UNKNOWN, never a false
    // "unlocked" DRIFT for a mere connectivity hiccup.
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "genlock: latency = 3 ms" "/plugin/path" "1" "locked" "" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[]);
    assert!(
        out.contains("RC=11"),
        "unread journal -> UNKNOWN exit (never a false DRIFT): {out:?}"
    );
    let line = out
        .lines()
        .find(|l| l.contains("dantesync_locked"))
        .unwrap_or_else(|| panic!("no dantesync_locked line printed: {out:?}"));
    assert!(line.contains("UNKNOWN"), "must report UNKNOWN: {line:?}");
}

#[test]
fn check_imag_report_dantesync_lock_unknown_when_no_pin_in_readme_489() {
    // Journal WAS read and reports locked, but no `dantesync_locked_imag` pin exists in
    // README yet -> UNKNOWN (nothing pinned to compare against), never a silent pass.
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "genlock: latency = 3 ms" "/plugin/path" "1" "" "$DANTESYNC_LOG" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[("DANTESYNC_LOG", DANTESYNC_LOG_LOCKED_FIXTURE)]);
    assert!(
        out.contains("RC=11"),
        "no pinned dantesync_locked_imag -> UNKNOWN exit: {out:?}"
    );
    let line = out
        .lines()
        .find(|l| l.contains("dantesync_locked"))
        .unwrap_or_else(|| panic!("no dantesync_locked line printed: {out:?}"));
    assert!(line.contains("UNKNOWN"), "must report UNKNOWN: {line:?}");
}

#[test]
fn dantesync_locked_from_log_finds_the_marker_amid_a_realistic_noisy_journal_489() {
    // A moderate, realistic-shaped multi-line journal: the lock marker interleaved among ~40
    // unrelated dantesync lines that genuinely do NOT match the marker regex (startup banner,
    // grandmaster-search retries, heartbeat ticks) -- mirroring how `OBS_LOG_FIXTURE` above is a
    // realistic multi-line snippet, not a toy. This is a coverage test for realistic shape, not
    // the SIGPIPE regression guard (see the >64KB test below for that).
    let mut log = String::from(
        "Jul 05 09:58:01 imag-nb dantesync[1234]: starting DanteSync -i eth0 --ntp-server strih.lan\n",
    );
    for i in 0..20 {
        log.push_str(&format!(
            "Jul 05 09:58:{:02} imag-nb dantesync[1234]: [PTP] searching for grandmaster on eth0, attempt {}\n",
            i, i
        ));
    }
    log.push_str(
        "Jul 05 09:58:25 imag-nb dantesync[1234]: [PTP] LOCK Drift 12 ns/s offset -340ns\n",
    );
    for i in 0..20 {
        log.push_str(&format!(
            "Jul 05 09:58:{:02} imag-nb dantesync[1234]: heartbeat tick {} (no state change)\n",
            26 + i,
            i
        ));
    }
    let out = run_sourced("dantesync_locked_from_log \"$LOG\"", &[("LOG", &log)]);
    assert_eq!(
        out.trim(),
        "locked",
        "must find the lock marker amid ~40 realistic interleaved journal lines: {out:?}"
    );
}

#[test]
fn dantesync_locked_from_log_survives_a_large_journal_without_sigpipe_489() {
    // #489 review: the ORIGINAL shape of this function used `grep -qE` directly inside an `if`
    // pipe -- the exact anti-pattern `genlock_from_log`'s own large-log regression test above
    // (`genlock_parser_reads_running_state_from_log`, ~line 200) already guards against for that
    // sibling parser: `grep -q` exits on the FIRST match and closes its read end, so a `printf`
    // still writing a large blob AFTER that match can raise SIGPIPE on its next write. Because the
    // pipeline is the CONDITION of an `if`, `set -e` doesn't abort the script -- the pipeline's
    // non-zero exit status just flips the `if` to its else branch, so the SILENT failure is a
    // WRONG ANSWER ("unlocked" for a genuinely-locked box), not a crash (empirically re-verified:
    // a naive `yes | head`-generated test blob has its OWN unrelated SIGPIPE hazard between `yes`
    // and `head` that can crash a script regardless of any downstream consumer -- this test
    // mirrors the sibling test's PROVEN-correct construction instead: build the blob as a plain
    // Rust string, write it to a temp FILE, and `cat` it inside the bash body, exactly like
    // `genlock_parser_reads_running_state_from_log` does for `genlock_from_log`).
    let mut big = String::from(
        "Jul 05 09:58:25 imag-nb dantesync[1234]: [PTP] LOCK Drift 12 ns/s offset -340ns\n",
    );
    for i in 0..5000 {
        big.push_str(&format!(
            "Jul 05 09:58:{i:04}: imag-nb dantesync[1234]: filler heartbeat line {i}\n"
        ));
    }
    let logfile =
        std::env::temp_dir().join(format!("dg_dantesync_biglog_{}.txt", std::process::id()));
    std::fs::write(&logfile, &big).expect("write big log");
    let out = run_sourced(
        "dantesync_locked_from_log \"$(cat \"$LOGFILE\")\"",
        &[("LOGFILE", logfile.to_str().unwrap())],
    );
    let _ = std::fs::remove_file(&logfile);
    assert_eq!(
        out.trim(),
        "locked",
        "must read locked from a >64KB journal without SIGPIPE/wrong-answer: {out:?}"
    );
}

#[test]
fn real_manifest_pins_dantesync_locked_imag_489() {
    // RED: vendor/README.md has no `dantesync_locked_imag` row yet -> pinned_setting returns
    // empty -> assertion fails. GREEN: the row is present and pins "locked" (#489, spun out of
    // #479's setup-imag.sh provisioning-time dantesync check).
    let readme = manifest_dir().join("vendor/README.md");
    let env = [("README", readme.to_str().unwrap())];
    let pin = run_sourced("pinned_setting \"$README\" dantesync_locked_imag", &env)
        .trim()
        .to_string();
    assert_eq!(
        pin, "locked",
        "real manifest must pin dantesync_locked_imag=locked (imag-nb's DanteSync clock must \
         stay PTP/NTP-locked -- genlock's wall-clock basis depends on it); got {pin:?}"
    );
}

// #784: check_imag_report's check #11 — the kernel-cmdline ISOLATION facet, via a NEW shared
// source-only lib scripts/lib/imag-cmdline-isolation.sh sourced by drift-guard.sh, passed as the
// 16th (optional) arg. Root cause: the večerný kolaps + the issue-842 recurrence were both caused by
// kernel isolcpus=/nohz_full= tokens on /proc/cmdline piling OBS's ~119 threads onto ONE core
// (60fps -> ~53fps NDI receive). The current design (issue 842) is AFFINITY-ONLY (taskset), so the
// cmdline must carry NO isolcpus=/nohz_full=; a SCOPED rcu_nocbs=<cpu-list> is the same footgun
// family and is DRIFT — but rcu_nocbs=all is the LEGITIMATE issue-482 low-latency (preempt=full)
// token and must NEVER be flagged (live-verified healthy cmdline on 10.77.9.182 carries it). A clean
// gather is OK; isolcpus/nohz_full/scoped-rcu_nocbs is DRIFT (exit 20); an unread block is UNKNOWN,
// never a false DRIFT (backward-compat with 9..15-arg call sites).
const CMDLINE_GATHER_CLEAN: &str = "\
CMDLINE|BOOT_IMAGE=/boot/vmlinuz-7.0.0-28-generic root=UUID=abc123 ro quiet splash preempt=full rcu_nocbs=all vt.handoff=7
";

#[test]
fn imag_cmdline_isolation_verdict_ok_on_a_clean_live_cmdline_784() {
    // The exact healthy cmdline shape read live from imag (rcu_nocbs=all present, no isolcpus/nohz).
    let out = run_sourced(
        "imag_cmdline_isolation_verdict \"$G\"",
        &[("G", CMDLINE_GATHER_CLEAN)],
    );
    let line = out
        .lines()
        .find(|l| l.starts_with("cmdline_isolation|"))
        .unwrap_or_else(|| panic!("no cmdline_isolation verdict line: {out:?}"));
    assert!(
        line.contains("|OK|"),
        "a clean cmdline (rcu_nocbs=all is the legit issue-482 token, no isolcpus/nohz_full) must be OK: {line:?}"
    );
}

#[test]
fn imag_cmdline_isolation_verdict_accepts_rcu_nocbs_all_784() {
    // The false-positive this facet is DESIGNED to avoid: rcu_nocbs=all is written by the issue-482
    // low-latency-kernel config (preempt=full) on EVERY healthy box — a blanket "flag any rcu_nocbs"
    // would false-fail the whole fleet. It must read OK.
    let g = "CMDLINE|root=UUID=x ro preempt=full rcu_nocbs=all\n";
    let out = run_sourced("imag_cmdline_isolation_verdict \"$G\"", &[("G", g)]);
    let line = out
        .lines()
        .find(|l| l.starts_with("cmdline_isolation|"))
        .unwrap_or_else(|| panic!("no cmdline_isolation line: {out:?}"));
    assert!(
        line.contains("|OK|"),
        "rcu_nocbs=all is the legitimate issue-482 lowlatency token and must never be flagged: {line:?}"
    );
}

#[test]
fn imag_cmdline_isolation_verdict_drift_on_isolcpus_784() {
    let g = "CMDLINE|root=UUID=x ro preempt=full isolcpus=2-11 nohz_full=10,11 rcu_nocbs=all\n";
    let out = run_sourced("imag_cmdline_isolation_verdict \"$G\"", &[("G", g)]);
    let line = out
        .lines()
        .find(|l| l.starts_with("cmdline_isolation|"))
        .unwrap_or_else(|| panic!("no cmdline_isolation line: {out:?}"));
    assert!(
        line.contains("|DRIFT|") && line.contains("isolcpus"),
        "isolcpus= must DRIFT naming the token (the #784/#842 footgun): {line:?}"
    );
}

#[test]
fn imag_cmdline_isolation_verdict_drift_on_nohz_full_beside_a_legit_rcu_nocbs_all_784() {
    // nohz_full= alone must DRIFT even though rcu_nocbs=all sits right next to it — the legit token
    // must not mask a genuine isolation-family drift.
    let g = "CMDLINE|root=UUID=x ro preempt=full nohz_full=10,11 rcu_nocbs=all\n";
    let out = run_sourced("imag_cmdline_isolation_verdict \"$G\"", &[("G", g)]);
    let line = out
        .lines()
        .find(|l| l.starts_with("cmdline_isolation|"))
        .unwrap_or_else(|| panic!("no cmdline_isolation line: {out:?}"));
    assert!(
        line.contains("|DRIFT|") && line.contains("nohz_full"),
        "nohz_full= must DRIFT even beside a legit rcu_nocbs=all: {line:?}"
    );
}

#[test]
fn imag_cmdline_isolation_verdict_drift_on_scoped_rcu_nocbs_784() {
    // A SCOPED per-core rcu_nocbs list (NOT =all) is the isolation family, not the issue-482 token.
    let g = "CMDLINE|root=UUID=x ro preempt=full rcu_nocbs=2-11\n";
    let out = run_sourced("imag_cmdline_isolation_verdict \"$G\"", &[("G", g)]);
    let line = out
        .lines()
        .find(|l| l.starts_with("cmdline_isolation|"))
        .unwrap_or_else(|| panic!("no cmdline_isolation line: {out:?}"));
    assert!(
        line.contains("|DRIFT|") && line.contains("rcu_nocbs=2-11"),
        "a scoped rcu_nocbs=<cpu-list> must DRIFT (distinct from the =all lowlatency token): {line:?}"
    );
}

#[test]
fn imag_cmdline_isolation_verdict_unknown_when_not_gathered_784() {
    // Empty gather (SSH hiccup / not read) → UNKNOWN, never a false OK/DRIFT.
    let out = run_sourced("imag_cmdline_isolation_verdict \"$G\"", &[("G", "")]);
    let line = out
        .lines()
        .find(|l| l.starts_with("cmdline_isolation|"))
        .unwrap_or_else(|| panic!("no cmdline_isolation line: {out:?}"));
    assert!(
        line.contains("|UNKNOWN|"),
        "an ungathered cmdline must be UNKNOWN (never a false OK/DRIFT on an SSH hiccup): {line:?}"
    );
}

#[test]
fn check_imag_report_cmdline_isolation_ok_when_clean_784() {
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "genlock: latency = 3 ms" "/plugin/path" "1" "" "" "" "" "" "" "$CL" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[("CL", CMDLINE_GATHER_CLEAN)]);
    let line = out
        .lines()
        .find(|l| l.contains("cmdline_isolation"))
        .unwrap_or_else(|| panic!("no cmdline_isolation row printed: {out:?}"));
    assert!(
        line.contains("OK"),
        "a clean cmdline must read OK on the check #11 row: {line:?}"
    );
}

#[test]
fn check_imag_report_cmdline_isolation_drift_when_isolcpus_784() {
    let g = "CMDLINE|root=UUID=x ro preempt=full isolcpus=2-11 nohz_full=10,11\n";
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "genlock: latency = 3 ms" "/plugin/path" "1" "" "" "" "" "" "" "$CL" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[("CL", g)]);
    assert!(
        out.contains("RC=20"),
        "kernel isolation on the cmdline must DRIFT (exit 20): {out:?}"
    );
    let line = out
        .lines()
        .find(|l| l.contains("cmdline_isolation"))
        .unwrap_or_else(|| panic!("no cmdline_isolation row printed: {out:?}"));
    assert!(
        line.contains("DRIFT") && line.contains("isolcpus"),
        "must DRIFT naming isolcpus: {line:?}"
    );
}

#[test]
fn check_imag_report_cmdline_isolation_unknown_when_not_gathered_backward_compat_784() {
    // Old 9..15-arg call sites (no cmdline gather block) must still get a graceful UNKNOWN
    // cmdline_isolation row — never an `unbound variable` crash under set -u nor a false DRIFT.
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "genlock: latency = 3 ms" "/plugin/path" "1" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[]);
    let line = out
        .lines()
        .find(|l| l.contains("cmdline_isolation"))
        .unwrap_or_else(|| panic!("no cmdline_isolation row printed on a 9-arg call: {out:?}"));
    assert!(
        line.contains("UNKNOWN"),
        "an unread cmdline block must report UNKNOWN, never a false DRIFT: {line:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// #1151 — the REPORT-ONLY projector_vsync facet (check #12) + the OBS-log-glob fix. The facet runs
// the SHARED projector_vsync_verdict (scripts/lib/obs-projector-vsync.sh) over the already-gathered
// $obs_log_text and must NEVER change check_imag_report's 20/11/0 exit contract (report-only).
// ---------------------------------------------------------------------------------------------

/// The issue-1146 marker exactly as it lands on imag (STEP-0 live-confirmed on 10.77.9.182).
const PROJECTOR_VSYNC_ARMED_LINE: &str =
    "15:52:14.820: projector-vsync: present-vsync ARMED (GL/EGL swap interval 1; no-op on D3D11)";

#[test]
fn check_imag_report_projector_vsync_ok_when_marker_present_1151() {
    // A log carrying the issue-1146 ARMED marker -> the check #12 row reads OK, naming the mechanism.
    let log = format!("genlock: latency = 3 ms\n{PROJECTOR_VSYNC_ARMED_LINE}");
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "$LOG" "/plugin/path" "1" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(body, &[("LOG", &log)]);
    let row = out
        .lines()
        .find(|l| l.contains("projector_vsync"))
        .unwrap_or_else(|| panic!("no projector_vsync row printed: {out:?}"));
    assert!(
        row.contains("OK") && !row.contains("UNKNOWN"),
        "marker present -> projector_vsync OK: {row:?}"
    );
    assert!(
        row.contains("present-vsync armed"),
        "the OK row must name the armed mechanism (comprehensive-logging): {row:?}"
    );
}

#[test]
fn check_imag_report_projector_vsync_unknown_when_marker_absent_or_log_empty_1151() {
    // No ARMED marker in a NON-empty log -> UNKNOWN (projector not (re)opened / build predates #1146),
    // NEVER a DRIFT. An EMPTY log -> UNKNOWN (log not read, #833). Both are report-only.
    let no_marker = "genlock: latency = 3 ms\nsome other OBS line";
    let body = r#"
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "$LOG" "/plugin/path" "1" || true
    "#;
    for (label, log) in [("no-marker", no_marker), ("empty", "")] {
        let out = run_sourced(body, &[("LOG", log)]);
        let row = out
            .lines()
            .find(|l| l.contains("projector_vsync"))
            .unwrap_or_else(|| panic!("[{label}] no projector_vsync row: {out:?}"));
        assert!(
            row.contains("UNKNOWN"),
            "[{label}] absent/unreadable marker -> UNKNOWN (report-only, fail-closed #833): {row:?}"
        );
        assert!(
            !row.contains("DRIFT"),
            "[{label}] projector_vsync must NEVER DRIFT (report-only): {row:?}"
        );
    }
}

#[test]
fn check_imag_report_projector_vsync_is_report_only_exit_code_unchanged_1151() {
    // The whole point: the facet's OK vs UNKNOWN state must NOT change check_imag_report's exit code.
    // Same identical call twice, differing ONLY in whether the OBS log carries the ARMED marker; the
    // RC line must be byte-identical (the facet touches neither $drift nor $unknown).
    let with_marker = format!("genlock: latency = 3 ms\n{PROJECTOR_VSYNC_ARMED_LINE}");
    let without_marker = "genlock: latency = 3 ms".to_string();
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "$LOG" "/plugin/path" "1" || rc=$?
        echo "RC=$rc"
    "#;
    let out_with = run_sourced(body, &[("LOG", &with_marker)]);
    let out_without = run_sourced(body, &[("LOG", &without_marker)]);
    let rc_with = out_with
        .lines()
        .find(|l| l.starts_with("RC="))
        .expect("RC line (with marker)");
    let rc_without = out_without
        .lines()
        .find(|l| l.starts_with("RC="))
        .expect("RC line (without marker)");
    assert_eq!(
        rc_with, rc_without,
        "projector_vsync is report-only: the exit code must be identical whether or not the ARMED \
         marker is present (with={rc_with:?} without={rc_without:?})"
    );
    // And the two runs genuinely differed in the facet row (guards against the test asserting a tie
    // because the facet never ran at all).
    assert!(
        out_with
            .lines()
            .any(|l| l.contains("projector_vsync") && l.contains("OK"))
            && out_without
                .lines()
                .any(|l| l.contains("projector_vsync") && l.contains("UNKNOWN")),
        "the with/without runs must actually differ in the projector_vsync row"
    );
}

#[test]
fn gather_and_check_imag_reads_txt_not_log_obs_glob_1151() {
    // #1151 bug: OBS names its logs `YYYY-MM-DD HH-MM-SS.txt`, but gather_and_check_imag uniquely
    // globbed `logs/*.log` (matching NOTHING on imag) -> its OBS-log facets read EMPTY -> chronic
    // UNKNOWN on the real box. The fix globs `*.txt` like every other imag OBS-log reader.
    let src = std::fs::read_to_string(script()).expect("read drift-guard.sh");
    assert!(
        src.contains(r#"obs-studio/logs/"*.txt"#),
        "gather_and_check_imag must glob the OBS log as *.txt (OBS's real extension), #1151"
    );
    assert!(
        !src.contains(r#"obs-studio/logs/"*.log"#),
        "the OBS-log gather must NOT still glob *.log — it matches nothing on imag (#1151)"
    );
}

#[test]
fn drift_guard_sources_the_shared_projector_vsync_lib_1151() {
    // The facet must run the SHARED verdict, not an inline copy (single marker-string source).
    let src = std::fs::read_to_string(script()).expect("read drift-guard.sh");
    assert!(
        src.contains(r#". "$HERE/lib/obs-projector-vsync.sh""#),
        "drift-guard.sh must source scripts/lib/obs-projector-vsync.sh (#1151)"
    );
    assert!(
        src.contains("projector_vsync_verdict \"$obs_log_text\""),
        "the facet must run projector_vsync_verdict over the already-gathered log (#1151)"
    );
}

#[test]
fn check_imag_report_projector_vsync_unknown_row_does_not_bump_the_unknown_counter_1151() {
    // The report-only exit_code_unchanged test above uses a 9-arg call, so facets 6-11 already
    // saturate `unknown` at exit 11 — it proves a spurious `drift++` (11->20) cannot happen but NOT
    // a spurious `unknown++` (already 11). This locks the `$unknown`-neutrality from a CLEAN BASELINE:
    // the SAME full-16-arg clean call the sibling clean tests use (all facets match -> exit 0), but
    // with the projector-vsync marker ABSENT so facet #12 genuinely prints UNKNOWN. If the facet
    // wrongly did `unknown++`, the exit would flip 0->11; report-only -> it stays 0.
    let log = format!("genlock: latency = 3 ms\n{GENLOCK_RT_PIN_OK_LINE}"); // NO projector-vsync marker
    let body = r#"
        rc=0
        check_imag_report "DSHA_A" "DSHA_A" "60" "60" "3" "3" "$LOG" "/plugin/path" "1" "locked" "$DANTESYNC_LOG" "$TS_STATES" "$POWER" "29" "$DP" "$CL" || rc=$?
        echo "RC=$rc"
    "#;
    let out = run_sourced(
        body,
        &[
            ("LOG", log.as_str()),
            ("DANTESYNC_LOG", DANTESYNC_LOG_LOCKED_FIXTURE),
            ("TS_STATES", TIMESYNC_STATES_CLEAN_FIXTURE),
            ("POWER", POWER_GATHER_CLEAN_29W),
            ("DP", DISPLAY_PATH_GATHER_CLEAN),
            ("CL", CMDLINE_GATHER_CLEAN),
        ],
    );
    assert!(
        out.contains("RC=0"),
        "projector_vsync's UNKNOWN row must NOT bump the unknown counter — a clean baseline stays \
         exit 0 with the marker absent (report-only): {out:?}"
    );
    let row = out
        .lines()
        .find(|l| l.contains("projector_vsync"))
        .unwrap_or_else(|| panic!("no projector_vsync row printed: {out:?}"));
    assert!(
        row.contains("UNKNOWN"),
        "the marker is absent, so the facet must genuinely print UNKNOWN (else the exit-0 assertion \
         above proves nothing): {row:?}"
    );
}
