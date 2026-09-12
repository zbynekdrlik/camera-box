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
