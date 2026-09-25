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
    // issue 1317 (M4 cut-over, 20.9.2026): 10.77.9.202 is now the Linux strih-lx notebook; the
    // Windows strih PC is RETIRED and its row is gone from the fleet list.
    assert_eq!(fleet_stdout("obs_fleet_host strih-lx"), "10.77.9.202");
    assert_eq!(fleet_stdout("obs_fleet_host stream"), "10.77.9.204");
    assert_eq!(fleet_stdout("obs_fleet_host imag"), "10.77.9.182");
    // resolume is a TRAVELING box — its host is the HOSTNAME, not a pinned IP (DHCP drift + the
    // 10.77.9.201/`bridge` collision, see the lib header + targets.md).
    assert_eq!(fleet_stdout("obs_fleet_host resolume"), "resolume.lan");
}

#[test]
fn fleet_class_distinguishes_windows_from_linux_genlock_1296() {
    assert_eq!(fleet_stdout("obs_fleet_class stream"), "windows-genlock");
    assert_eq!(fleet_stdout("obs_fleet_class resolume"), "windows-genlock");
    assert_eq!(fleet_stdout("obs_fleet_class strih-lx"), "linux-genlock");
    assert_eq!(fleet_stdout("obs_fleet_class imag"), "linux-genlock");
}

#[test]
fn fleet_home_check_is_always_for_fixed_boxes_traveling_for_resolume_1296() {
    assert_eq!(fleet_stdout("obs_fleet_home_check strih-lx"), "always");
    assert_eq!(fleet_stdout("obs_fleet_home_check stream"), "always");
    // issue 1316: imag-nb was RETURNED to the owner (16.9.2026); its home-check is now `retired`
    // (was `always`). The row + host lookup are KEPT (the role returns on a new notebook next year).
    assert_eq!(fleet_stdout("obs_fleet_home_check imag"), "retired");
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
    // issue 1317: audio-lag watches the PRODUCTION strih (now the Linux strih-lx at .202 -- its
    // vendored OBS emits the same `audio-telemetry #800` lines) + stream. vb-matrix is WINDOWS-only
    // (PipeWire replaced VB-Matrix on strih-lx), so it is stream alone.
    assert_eq!(
        boxes("audio-lag"),
        "strih-lx|10.77.9.202 stream|10.77.9.204"
    );
    assert_eq!(boxes("vb-matrix"), "stream|10.77.9.204");
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
fn fleet_boxes_genlock_lock_excludes_retired_imag_1316() {
    // #1299 made the genlock LOCKED/DEGRADED/UNLOCKED facet fleet-wide incl. imag; issue 1316
    // RETIRED imag-nb (returned to the owner), so obs_fleet_boxes now EXCLUDES it from every facet
    // roster centrally — genlock-lock drops to the two Windows genlock boxes + the traveling
    // resolume cg box. `obs_fleet_facet_members genlock-lock` still LISTS imag (the policy is
    // unchanged); the exclusion is the `retired` filter in obs_fleet_boxes, so a one-word flip back
    // to `always` on re-provision restores it automatically.
    // issue 1317: strih-lx (the Linux strih notebook) also joins genlock-lock (a linux-genlock box
    // that locks every input to the fleet clock), appended after resolume.
    // issue 1317 (M4): the Windows strih row is retired; strih-lx takes the strih slot at .202.
    assert_eq!(
        boxes("genlock-lock"),
        "strih-lx|10.77.9.202 stream|10.77.9.204 resolume|resolume.lan"
    );
    // A retired imag must not appear in ANY facet roster.
    for facet in [
        "genlock-lock",
        "audio-lag",
        "av-step",
        "vb-matrix",
        "bundle-state",
    ] {
        assert!(
            !boxes(facet).contains("imag"),
            "facet {facet} must NOT carry retired imag: {}",
            boxes(facet)
        );
    }
}

#[test]
fn fleet_strih_lx_is_the_always_home_production_strih_at_202_1317() {
    // issue 1317 (M4 cut-over 20.9.2026): strih-lx IS the production strih. Its row dials the real
    // address (strih-lx.lan has no DNS entry on dev1 -- the old traveling row read it as AWAY, so
    // every dev1 watchdog was blind to the production strih) and it is permanently home.
    assert_eq!(fleet_stdout("obs_fleet_host strih-lx"), "10.77.9.202");
    assert_eq!(fleet_stdout("obs_fleet_class strih-lx"), "linux-genlock");
    assert_eq!(fleet_stdout("obs_fleet_home_check strih-lx"), "always");
    assert!(
        is_home("strih-lx", &[]),
        "strih-lx must be home with no probe"
    );
    // No facet may still dial the unresolvable hostname.
    for facet in ALL_FACETS {
        assert!(
            !boxes(facet).contains("strih-lx.lan"),
            "facet {facet} still dials the unresolvable strih-lx.lan: {}",
            boxes(facet)
        );
    }
}

#[test]
fn fleet_windows_strih_row_is_retired_from_the_list_1317() {
    // The Windows strih PC is gone (M4, 20.9.2026). Its row is REMOVED (not `retired`): its address
    // .202 now belongs to strih-lx, so keeping `strih|10.77.9.202|windows-genlock` would make
    // `obs_fleet_host strih` hand a Linux box to a Windows-class caller. An explicit `strih` lookup
    // fails closed like any unknown name.
    let (rc, out, _err) = run_fleet("obs_fleet_host strih", &[]);
    assert_ne!(rc, 0, "the retired Windows strih must not resolve: {out:?}");
    assert!(
        out.trim().is_empty(),
        "no host for the retired strih: {out:?}"
    );
    assert!(
        !is_home("strih", &[]),
        "the retired Windows strih is never home"
    );
    for facet in ALL_FACETS {
        let members = fleet_stdout(&format!("obs_fleet_facet_members {facet}"));
        assert!(
            !members.split_whitespace().any(|m| m == "strih"),
            "facet {facet} still lists the retired Windows strih: {members}"
        );
        let r = boxes(facet);
        assert!(
            !r.split_whitespace().any(|p| p.starts_with("strih|")),
            "facet {facet} roster still carries the Windows strih: {r}"
        );
    }
}

#[test]
fn fleet_strih_lx_facet_membership_by_premise_1317() {
    // strih-lx joins every facet whose premise is a platform-neutral read of the production strih
    // (ping / OBS-WS :4455 / its own :8899 bundle-state server / its dantesync :8898).
    for facet in [
        "audio-lag",
        "bundle-state",
        "network-reach",
        "obs-liveness",
        "genlock-lock",
        "render-freeze",
    ] {
        assert!(
            boxes(facet).contains("strih-lx|10.77.9.202"),
            "facet {facet} must watch the production strih-lx: {}",
            boxes(facet)
        );
    }
    assert_eq!(
        fleet_stdout("obs_fleet_facet_members ndi-portmap"),
        "strih-lx"
    );
    // vb-matrix is a WINDOWS VB-Audio Matrix process check (PipeWire replaced it on strih-lx);
    // av-step is the stream box's av-sync dock only.
    for facet in ["vb-matrix", "av-step"] {
        assert!(
            !boxes(facet).contains("strih-lx"),
            "facet {facet} must NOT carry strih-lx: {}",
            boxes(facet)
        );
    }
}

#[test]
fn dantesync_clock_default_obs_nodes_name_the_production_strih_1317() {
    // The dante-clock watchdog's OBS-node default is a NAME list resolved through obs_fleet_host; a
    // retired `strih` name would resolve to nothing and silently drop the production strih (the
    // NTP master since M4) from clock paging.
    assert_eq!(
        watchdog_var(
            "dantesync-clock-alert-watchdog.sh",
            "DANTE_CLOCK_OBS_NODES",
            &[]
        ),
        "strih-lx stream resolume"
    );
}

const ALL_FACETS: [&str; 13] = [
    "audio-lag",
    "av-step",
    "vb-matrix",
    "bundle-state",
    "network-reach",
    "obs-liveness",
    "genlock-lock",
    "render-freeze",
    "ndi-portmap",
    "obs-session",
    "burn-reconcile",
    "rig-restore",
    "ndi-sender",
];

// ---------------------------------------------------------------------------------------------
// issue 1317 part 2 -- the dev1 watchdogs that used to dial a LITERAL `strih` at .202 now derive
// their rosters from the fleet list too, each facet by its own premise.
// ---------------------------------------------------------------------------------------------
#[test]
fn fleet_obs_session_facet_is_windows_genlock_only_1317() {
    // obs-session = the Windows session-0 / AHK visibility probe (a PowerShell probe over
    // win_ssh_run). It has no meaning on a Linux box, so the facet carries ONLY windows-genlock
    // boxes: stream + resolume (the latter gated on is_home by the consumer).
    assert_eq!(
        boxes("obs-session"),
        "stream|10.77.9.204 resolume|resolume.lan"
    );
    for m in fleet_stdout("obs_fleet_facet_members obs-session").split_whitespace() {
        assert_eq!(
            fleet_stdout(&format!("obs_fleet_class {m}")),
            "windows-genlock",
            "obs-session member {m} is not a Windows box -- it would be probed with PowerShell"
        );
    }
}

#[test]
fn fleet_burn_reconcile_and_rig_restore_facets_watch_strih_lx_1317() {
    // Both are OBS-WebSocket-only (GetStats renderTotalFrames + obs_burn_filter sweeps; the
    // obs_phase2 program-scene read + teardown) -- platform-neutral, so the production strih-lx
    // keeps the coverage the Windows strih had.
    for facet in ["burn-reconcile", "rig-restore"] {
        assert_eq!(
            boxes(facet),
            "strih-lx|10.77.9.202 stream|10.77.9.204",
            "facet {facet}"
        );
    }
}

/// `obs_fleet_poll_now <name>` -> 0 when a consumer should poll NAME this pass: an `always` box, or
/// a `traveling` box that is home. A traveling box that is away and a `retired` box return 1. A name
/// with NO row (an ops override naming a box the table does not know yet) returns 0, because the
/// override is authoritative.
fn poll_now(name: &str, env: &[(&str, &str)]) -> bool {
    let (rc, _out, _err) = run_fleet(&format!("obs_fleet_poll_now {name}"), env);
    rc == 0
}

#[test]
fn fleet_poll_now_gates_traveling_and_retired_boxes_only_1317() {
    let force = [("OBS_FLEET_HOME", "resolume")];
    assert!(
        poll_now("strih-lx", &force),
        "an always box is polled even when not in the force-list"
    );
    assert!(poll_now("stream", &force));
    assert!(poll_now("resolume", &force), "traveling + home -> poll");
    let away = [("OBS_FLEET_HOME", "nobody")];
    assert!(
        !poll_now("resolume", &away),
        "traveling + away -> never poll"
    );
    assert!(!poll_now("imag", &[]), "a retired box is never polled");
    assert!(
        poll_now("strih-pp", &away),
        "an unknown override name is polled as given"
    );
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
    assert!(is_home("strih-lx", &[]));
    assert!(is_home("stream", &[]));
}

#[test]
fn fleet_retired_box_is_never_home_1316() {
    // issue 1316: imag-nb was returned to the owner; a `retired` box is NEVER home, so no watchdog
    // gating on obs_fleet_is_home ever probes or pages the dead box. The row/host lookup stay.
    assert!(!is_home("imag", &[]));
    assert_eq!(fleet_stdout("obs_fleet_host imag"), "10.77.9.182");
}

#[test]
fn fleet_is_home_traveling_box_both_branches_via_force_list_1296() {
    // HOME branch: the force-list names resolume -> home.
    assert!(is_home("resolume", &[("OBS_FLEET_HOME", "resolume")]));
    // AWAY branch: the force-list names a DIFFERENT box -> resolume is away (deterministic, no I/O).
    assert!(!is_home("resolume", &[("OBS_FLEET_HOME", "stream")]));
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
        "strih-lx|10.77.9.202 stream|10.77.9.204"
    );
    assert_eq!(
        watchdog_var("av-step-alert-watchdog.sh", "BOXES", &[]),
        "stream|10.77.9.204"
    );
    assert_eq!(
        watchdog_var("vb-matrix-alert-watchdog.sh", "BOXES", &[]),
        "stream|10.77.9.204"
    );
}

#[test]
fn bundle_state_default_carries_resolume_1296() {
    // issue 1317 (M4): the production strih is the Linux strih-lx at .202 (the Windows strih row
    // is retired) -- a fully-unreachable traveling box (resolume) is deferred to the reachability
    // watchdog by bundle-state itself, so it is traveling-safe without a gate.
    assert_eq!(
        watchdog_var("bundle-state-alert-watchdog.sh", "BOXES", &[]),
        "strih-lx|10.77.9.202 stream|10.77.9.204 resolume|resolume.lan"
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
            // OBS_FLEET_HOME forces is_home deterministically so sourcing network-reach never fires a
            // live getent/:4455 probe — keeps this Tier-0 case offline + fast (#1296 review 🔵2).
            &[
                ("NETWORK_REACH_BOXES", "strih|127.0.0.1 resolume|127.0.0.2"),
                ("OBS_FLEET_HOME", "strih"),
            ]
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
            &[("OBS_FLEET_HOME", "stream")]
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
    // exactly the polled boxes. AWAY -> strih-lx+stream only; HOME -> + resolume.
    let stub = manifest_dir().join("tests/fixtures/obs_liveness_echo_probe_1296.py");
    let stub = stub.to_string_lossy().to_string();
    let away = watchdog_measure(&[("OBS_FLEET_HOME", "stream"), ("OBS_LIVENESS_PROBE", &stub)]);
    assert!(
        away.contains("strih-lx") && away.contains("stream"),
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

#[test]
fn obs_liveness_strih_lx_arm_keeps_the_strih_knobs_1317() {
    // issue 1317: strih-lx is polled through its OWN arm, which keeps the strih knobs
    // (STRIH_HOST + STRIH_TARGET_FPS) -- not the generic default-fps fallback. The echo stub prints
    // the full `--box name=host:fps` value, so the chosen host/fps are observable.
    let stub = manifest_dir().join("tests/fixtures/obs_liveness_echo_probe_1296.py");
    let stub = stub.to_string_lossy().to_string();
    let out = watchdog_measure(&[
        ("OBS_FLEET_HOME", "stream"),
        ("OBS_LIVENESS_PROBE", &stub),
        ("STRIH_HOST", "1.2.3.4"),
        ("STRIH_TARGET_FPS", "29"),
        ("OBS_LIVENESS_DEFAULT_FPS", "17"),
    ]);
    assert!(
        out.contains("box=strih-lx=1.2.3.4:29"),
        "strih-lx must use STRIH_HOST + STRIH_TARGET_FPS: {out}"
    );
    // Default: the knob-free STRIH_HOST default is strih-lx's fleet address .202.
    let dflt = watchdog_measure(&[("OBS_FLEET_HOME", "stream"), ("OBS_LIVENESS_PROBE", &stub)]);
    assert!(
        dflt.contains("box=strih-lx=10.77.9.202:30"),
        "strih-lx default must be .202 at 30 fps: {dflt}"
    );
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

#[test]
fn fleet_has_ahk_is_the_one_ahk_watcher_fact_1317() {
    // The NL_STARTUP.ahk auto-respawn watcher runs on resolume; stream and the Linux boxes have none.
    // issue 1317 part 3: the retired Windows strih's planner arms are gone, so the name `strih` no
    // longer carries the AHK fact either (it used to, for those legacy arms).
    for (name, want) in [
        ("resolume", "1"),
        ("strih", "0"),
        ("stream", "0"),
        ("strih-lx", "0"),
        ("imag", "0"),
    ] {
        assert_eq!(
            fleet_stdout(&format!("obs_fleet_has_ahk {name}")),
            want,
            "obs_fleet_has_ahk {name}"
        );
    }
}

/// issue 1317 part 3: the ONE class gate a Windows-only dev1 tool calls before touching an address.
/// 10.77.9.202 (and the name strih-lx) resolve to the linux-genlock class, so a PowerShell/schtasks/
/// C:\ tool aimed at the production strih refuses; a Windows fleet box passes; an address the list
/// does not know passes (an explicit ops target stays authoritative).
#[test]
fn fleet_class_for_host_and_the_linux_refusal_gate_1317() {
    for (host, want) in [
        ("10.77.9.202", "linux-genlock"),
        ("strih-lx", "linux-genlock"),
        ("10.77.9.204", "windows-genlock"),
        ("resolume.lan", "windows-genlock"),
        ("resolume", "windows-genlock"),
    ] {
        assert_eq!(
            fleet_stdout(&format!("obs_fleet_class_for_host {host}")),
            want,
            "obs_fleet_class_for_host {host}"
        );
    }
    let (rc, out, _e) = run_fleet("obs_fleet_class_for_host 10.1.2.3", &[]);
    assert_eq!(rc, 1, "an unknown address has no class: {out:?}");
    // the refusal gate: rc 1 + a named error for a Linux fleet box, rc 0 + silent otherwise.
    let (rc, _o, err) = run_fleet(
        "obs_fleet_refuse_linux_target 10.77.9.202 some-tool.sh",
        &[],
    );
    assert_eq!(
        rc, 1,
        "a Windows action at the Linux strih-lx must be refused"
    );
    assert!(
        err.contains("some-tool.sh")
            && err.contains("linux-genlock")
            && err.contains("10.77.9.202"),
        "the refusal names the tool, the class and the address: {err:?}"
    );
    for ok_host in ["10.77.9.204", "resolume.lan", "10.1.2.3"] {
        let (rc, _o, err) = run_fleet(&format!("obs_fleet_refuse_linux_target {ok_host} t"), &[]);
        assert_eq!(rc, 0, "{ok_host} is not a Linux fleet box: {err:?}");
        assert!(err.is_empty(), "silent pass for {ok_host}: {err:?}");
    }
}
