//! #1296 — guard for `scripts/lib/obs-fleet.sh`, the ONE declared list of managed broadcast-OBS
//! boxes the dev1 fleet mechanisms watch + version-check, and the single place each watchdog's
//! box-roster default derives from.
//!
//! Root cause (issue 1296): fleet membership was duplicated as SIX independent literals — the five
//! `BOXES="${X_BOXES:-strih|… stream|…}"` defaults in
//! audio-lag/av-step/bundle-state/network-reach/vb-matrix-alert-watchdog.sh, plus obs-liveness's
//! hardcoded STRIH_HOST/STREAM_HOST `--box` pair — so registering RESOLUME-SNV meant editing six
//! files with no source of truth. This file pins the new single-source-of-truth lib: the pure
//! `obs_fleet_boxes <facet>` policy (which must reproduce each facet's current byte-exact default)
//! and the traveling-box `obs_fleet_is_home` gate (both branches via the OBS_FLEET_HOME seam).
//!
//! Same convention as `tests/harness_network_reach_health_1001.rs`: source the REAL lib (source-only,
//! no side effects) and exercise the pure functions directly. RED before the lib exists (sourcing
//! fails, every test fails); GREEN after. Tier-0: pure bash, no rig, no cargo-compiled probe.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fleet_lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/obs-fleet.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the REAL obs-fleet lib and run `body` against its functions. Returns (exit, stdout, stderr).
/// `env` is extra KEY=VALUE pairs (e.g. the OBS_FLEET_HOME force-list seam).
fn run_fleet(body: &str, env: &[(&str, &str)]) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$FLEET_LIB\"\n{body}", body = body);
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(&harness)
        .env("FLEET_LIB", fleet_lib())
        .current_dir(manifest_dir());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn fleet_stdout(body: &str) -> String {
    let (rc, out, err) = run_fleet(body, &[]);
    assert_eq!(rc, 0, "body failed (rc={rc}): {body}\nstderr={err}");
    out.trim().to_string()
}

// ---------------------------------------------------------------------------------------------
// lib shape — the public functions must be defined
// ---------------------------------------------------------------------------------------------
#[test]
fn fleet_lib_defines_the_public_functions_1296() {
    for f in [
        "obs_fleet_host",
        "obs_fleet_class",
        "obs_fleet_home_check",
        "obs_fleet_facet_members",
        "obs_fleet_boxes",
        "obs_fleet_is_home",
    ] {
        let out = fleet_stdout(&format!("type {f} >/dev/null 2>&1 && echo DEFINED"));
        assert_eq!(out, "DEFINED", "{f} is not defined by obs-fleet.sh");
    }
}

// ---------------------------------------------------------------------------------------------
// FACT lookups — name -> host / class / home-check
// ---------------------------------------------------------------------------------------------
#[test]
fn fleet_host_resolves_each_declared_box_1296() {
    assert_eq!(fleet_stdout("obs_fleet_host strih"), "10.77.9.202");
    assert_eq!(fleet_stdout("obs_fleet_host stream"), "10.77.9.204");
    assert_eq!(fleet_stdout("obs_fleet_host imag"), "10.77.9.182");
    // resolume is a TRAVELING box — its host is the HOSTNAME, not a pinned IP (DHCP drift + the
    // 10.77.9.201/`bridge` collision, see the lib header + targets.md).
    assert_eq!(fleet_stdout("obs_fleet_host resolume"), "resolume.lan");
}

#[test]
fn fleet_class_distinguishes_windows_from_linux_genlock_1296() {
    assert_eq!(fleet_stdout("obs_fleet_class strih"), "windows-genlock");
    assert_eq!(fleet_stdout("obs_fleet_class resolume"), "windows-genlock");
    assert_eq!(fleet_stdout("obs_fleet_class imag"), "linux-genlock");
}

#[test]
fn fleet_home_check_is_always_for_fixed_boxes_traveling_for_resolume_1296() {
    assert_eq!(fleet_stdout("obs_fleet_home_check strih"), "always");
    assert_eq!(fleet_stdout("obs_fleet_home_check stream"), "always");
    assert_eq!(fleet_stdout("obs_fleet_home_check imag"), "always");
    assert_eq!(fleet_stdout("obs_fleet_home_check resolume"), "traveling");
}

#[test]
fn fleet_unknown_name_fails_closed_1296() {
    let (rc, out, _err) = run_fleet("obs_fleet_host nosuchbox", &[]);
    assert_ne!(
        rc, 0,
        "an unknown box name must fail (nonzero), not emit a host"
    );
    assert!(
        out.trim().is_empty(),
        "an unknown box name must emit no host: {out:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// obs_fleet_boxes <facet> — the `name|host …` rosters each watchdog's BOXES= derives from.
// The three pre-existing facets MUST reproduce the exact byte-for-byte legacy default so the
// env-override stays byte-compatible; the three OBS-box facets carry resolume.
// ---------------------------------------------------------------------------------------------
fn boxes(facet: &str) -> String {
    fleet_stdout(&format!("obs_fleet_boxes {facet}"))
}

#[test]
fn fleet_boxes_reproduces_the_legacy_byte_exact_defaults_1296() {
    // audio-lag / vb-matrix: strih + stream (the two literals those watchdogs shipped).
    assert_eq!(boxes("audio-lag"), "strih|10.77.9.202 stream|10.77.9.204");
    assert_eq!(boxes("vb-matrix"), "strih|10.77.9.202 stream|10.77.9.204");
    // av-step: stream only (the av-sync dock box, #1267).
    assert_eq!(boxes("av-step"), "stream|10.77.9.204");
}

#[test]
fn fleet_boxes_carries_resolume_only_where_the_facet_applies_1296() {
    for facet in ["bundle-state", "network-reach", "obs-liveness"] {
        assert!(
            boxes(facet).contains("resolume|resolume.lan"),
            "facet {facet} must carry resolume: {}",
            boxes(facet)
        );
    }
    // resolume has no mbc audio (audio-lag/av-step) and no VB-Matrix — it must NOT be in those.
    for facet in ["audio-lag", "av-step", "vb-matrix"] {
        assert!(
            !boxes(facet).contains("resolume"),
            "facet {facet} must NOT carry resolume: {}",
            boxes(facet)
        );
    }
}

#[test]
fn fleet_boxes_unknown_facet_fails_closed_1296() {
    let (rc, out, _err) = run_fleet("obs_fleet_boxes bogus-facet", &[]);
    assert_ne!(
        rc, 0,
        "an unknown facet must fail, never emit an empty roster"
    );
    assert!(
        out.trim().is_empty(),
        "an unknown facet must emit no roster: {out:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// obs_fleet_is_home — the traveling-box gate, both branches via the OBS_FLEET_HOME force-list seam
// (so the test never depends on live getent/:4455 I/O).
// ---------------------------------------------------------------------------------------------
fn is_home(name: &str, env: &[(&str, &str)]) -> bool {
    let (rc, _out, _err) = run_fleet(&format!("obs_fleet_is_home {name}"), env);
    rc == 0
}

#[test]
fn fleet_is_home_always_box_is_unconditionally_home_1296() {
    // a `home-check=always` box needs no probe and no force-list.
    assert!(is_home("strih", &[]));
    assert!(is_home("imag", &[]));
}

#[test]
fn fleet_is_home_traveling_box_both_branches_via_force_list_1296() {
    // HOME branch: the force-list names resolume -> home.
    assert!(is_home("resolume", &[("OBS_FLEET_HOME", "resolume")]));
    // AWAY branch: the force-list names a DIFFERENT box -> resolume is away (deterministic, no I/O).
    assert!(!is_home("resolume", &[("OBS_FLEET_HOME", "strih")]));
}

#[test]
fn fleet_is_home_unknown_name_is_away_1296() {
    // fail-closed: an untracked box is never treated as home.
    assert!(!is_home("nosuchbox", &[]));
}

// ---------------------------------------------------------------------------------------------
// WATCHDOG WIRING — each watchdog derives its BOXES default from obs_fleet_boxes <facet>, the env
// override still wins byte-compatibly, and resolume lands only where the facet applies. These
// SOURCE the real watchdog scripts (each guards `main` behind a BASH_SOURCE==$0 check, so sourcing
// only defines functions + runs the config block) and read the resulting config var.
// ---------------------------------------------------------------------------------------------
fn scripts_dir() -> PathBuf {
    manifest_dir().join("scripts")
}

/// Source a watchdog script (it must guard `main`, so sourcing is side-effect-free beyond config)
/// under `env` and echo one config `var`. Returns its trimmed stdout.
fn watchdog_var(script: &str, var: &str, env: &[(&str, &str)]) -> String {
    let wd = scripts_dir().join(script);
    assert!(wd.exists(), "{} not found", wd.display());
    let harness = format!(
        "set -uo pipefail\n. \"$WD\"\nprintf '%s' \"${{{var}}}\"",
        var = var
    );
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(&harness)
        .env("WD", &wd)
        .current_dir(manifest_dir());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("failed to source watchdog");
    assert!(
        out.status.success(),
        "sourcing {script} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn watchdog_src(script: &str) -> String {
    std::fs::read_to_string(scripts_dir().join(script)).unwrap()
}

#[test]
fn watchdogs_source_the_obs_fleet_lib_1296() {
    for script in [
        "audio-lag-alert-watchdog.sh",
        "av-step-alert-watchdog.sh",
        "bundle-state-alert-watchdog.sh",
        "network-reach-alert-watchdog.sh",
        "vb-matrix-alert-watchdog.sh",
        "obs-liveness-watchdog.sh",
    ] {
        assert!(
            watchdog_src(script).contains("lib/obs-fleet.sh"),
            "{script} must source the obs-fleet lib"
        );
    }
}

#[test]
fn each_boxes_watchdog_derives_its_default_from_obs_fleet_boxes_1296() {
    // The five BOXES= watchdogs each derive their default via obs_fleet_boxes <facet>.
    for (script, facet) in [
        ("audio-lag-alert-watchdog.sh", "audio-lag"),
        ("av-step-alert-watchdog.sh", "av-step"),
        ("bundle-state-alert-watchdog.sh", "bundle-state"),
        ("network-reach-alert-watchdog.sh", "network-reach"),
        ("vb-matrix-alert-watchdog.sh", "vb-matrix"),
    ] {
        assert!(
            watchdog_src(script).contains(&format!("obs_fleet_boxes {facet}")),
            "{script} must derive its BOXES default from obs_fleet_boxes {facet}"
        );
    }
}

#[test]
fn sourced_boxes_default_is_byte_exact_for_the_legacy_facets_1296() {
    // The three pre-#1296 facets reproduce their exact legacy literal when sourced with no override.
    assert_eq!(
        watchdog_var("audio-lag-alert-watchdog.sh", "BOXES", &[]),
        "strih|10.77.9.202 stream|10.77.9.204"
    );
    assert_eq!(
        watchdog_var("av-step-alert-watchdog.sh", "BOXES", &[]),
        "stream|10.77.9.204"
    );
    assert_eq!(
        watchdog_var("vb-matrix-alert-watchdog.sh", "BOXES", &[]),
        "strih|10.77.9.202 stream|10.77.9.204"
    );
}

#[test]
fn bundle_state_default_carries_resolume_1296() {
    assert_eq!(
        watchdog_var("bundle-state-alert-watchdog.sh", "BOXES", &[]),
        "strih|10.77.9.202 stream|10.77.9.204 resolume|resolume.lan"
    );
}

#[test]
fn boxes_env_override_still_wins_byte_compatibly_1296() {
    // The X_BOXES env override must bypass the derived default entirely.
    assert_eq!(
        watchdog_var(
            "bundle-state-alert-watchdog.sh",
            "BOXES",
            &[("BUNDLE_STATE_BOXES", "fakebox|127.0.0.1")]
        ),
        "fakebox|127.0.0.1"
    );
    assert_eq!(
        watchdog_var(
            "vb-matrix-alert-watchdog.sh",
            "BOXES",
            &[("VB_MATRIX_BOXES", "stream|1.2.3.4")]
        ),
        "stream|1.2.3.4"
    );
    assert_eq!(
        watchdog_var(
            "network-reach-alert-watchdog.sh",
            "BOXES",
            &[("NETWORK_REACH_BOXES", "strih|127.0.0.1 resolume|127.0.0.2")]
        ),
        "strih|127.0.0.1 resolume|127.0.0.2"
    );
}

// ---------------------------------------------------------------------------------------------
// network-reach: resolume is report-only unless obs_fleet_is_home holds (BOTH branches), and the
// explicit REPORT_ONLY env override still wins (the #811 offline test relies on that, and an
// explicit override must NOT trigger a live is_home probe).
// ---------------------------------------------------------------------------------------------
#[test]
fn network_reach_resolume_report_only_when_away_paging_when_home_1296() {
    // AWAY (force-list omits resolume) -> resolume STAYS report-only (never pages).
    assert_eq!(
        watchdog_var(
            "network-reach-alert-watchdog.sh",
            "REPORT_ONLY_BOXES",
            &[("OBS_FLEET_HOME", "strih")]
        ),
        "resolume"
    );
    // HOME (force-list names resolume) -> resolume PROMOTED to a paging node (report-only empty).
    assert_eq!(
        watchdog_var(
            "network-reach-alert-watchdog.sh",
            "REPORT_ONLY_BOXES",
            &[("OBS_FLEET_HOME", "resolume")]
        ),
        ""
    );
}

#[test]
fn network_reach_report_only_env_override_still_wins_1296() {
    // An explicit override wins regardless of is_home (the #811 offline-determinism contract).
    assert_eq!(
        watchdog_var(
            "network-reach-alert-watchdog.sh",
            "REPORT_ONLY_BOXES",
            &[
                ("NETWORK_REACH_REPORT_ONLY_BOXES", "resolume"),
                ("OBS_FLEET_HOME", "resolume"),
            ]
        ),
        "resolume"
    );
}

// ---------------------------------------------------------------------------------------------
// obs-liveness: keeps its strih/stream IP literals (the #391 test anchors on them), derives the
// box SET from obs_fleet_boxes, and polls resolume ONLY while obs_fleet_is_home holds.
// ---------------------------------------------------------------------------------------------
#[test]
fn obs_liveness_keeps_strih_stream_ip_literals_1296() {
    let src = watchdog_src("obs-liveness-watchdog.sh");
    assert!(src.contains("10.77.9.202"), "strih IP literal must remain");
    assert!(src.contains("10.77.9.204"), "stream IP literal must remain");
    assert!(
        src.contains("obs_fleet_boxes obs-liveness"),
        "obs-liveness must derive its poll set from obs_fleet_boxes"
    );
}

#[test]
fn obs_liveness_polls_resolume_only_when_home_1296() {
    // Stub the python probe to echo one verdict line per --box it receives, so VERDICT_LINES names
    // exactly the polled boxes. AWAY -> strih+stream only; HOME -> + resolume.
    let stub = manifest_dir().join("tests/fixtures/obs_liveness_echo_probe_1296.py");
    let stub = stub.to_string_lossy().to_string();
    let away = watchdog_measure(&[("OBS_FLEET_HOME", "strih"), ("OBS_LIVENESS_PROBE", &stub)]);
    assert!(
        away.contains("strih") && away.contains("stream"),
        "away: {away}"
    );
    assert!(
        !away.contains("resolume"),
        "away must not poll resolume: {away}"
    );
    let home = watchdog_measure(&[
        ("OBS_FLEET_HOME", "resolume"),
        ("OBS_LIVENESS_PROBE", &stub),
    ]);
    assert!(home.contains("resolume"), "home must poll resolume: {home}");
}

/// Source obs-liveness-watchdog.sh under `env`, run measure_boxes, echo VERDICT_LINES.
fn watchdog_measure(env: &[(&str, &str)]) -> String {
    let wd = scripts_dir().join("obs-liveness-watchdog.sh");
    let harness = "set -uo pipefail\n. \"$WD\"\nmeasure_boxes\nprintf '%s' \"$VERDICT_LINES\"";
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(harness)
        .env("WD", &wd)
        .current_dir(manifest_dir());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("failed to source obs-liveness");
    assert!(
        out.status.success(),
        "measure_boxes failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}
