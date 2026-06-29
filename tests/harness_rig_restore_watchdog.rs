//! #281 Fix#3 — behavioral tests for the auto-restore rig watchdog.
//!
//! Background (#281): dispatched workers die mid-rig-task and leave the rig stranded in a TEST
//! state (prod camera-box down while a manual /tmp probe runs, OBS program on a probe scene, burns
//! left on). Parts A+B (PR #348) added the `with-rig-restore` wrapper + resumable decode. Fix#3 is
//! the safety net: DETECT a stranded rig, AUTO-RECOVER prod, and ALWAYS alert.
//!
//! The prior #266 auto-watchdog was REMOVED for false positives, so Fix#3 is deliberately
//! conservative:
//!   1. A FRESH heartbeat (a legit E2E touches it periodically) means "never act".
//!   2. It acts ONLY on a CLEAR stranded signal (cam-box down, stale probe, OBS on a known TEST
//!      scene) while the heartbeat is absent/stale.
//!   3. It requires 2 CONSECUTIVE confirmations before acting (the #266 "2-live-sample" lesson).
//!   4. When it acts it ALWAYS fires a Discord alert.
//!
//! These are pure-shell / file-system / argparse tests — NO rig, NO ssh, NO OBS. The watchdog
//! SCRIPT contains ssh/WS calls (that is its job) but is never EXECUTED against the live rig here;
//! we test the pure decision function, the heartbeat helpers, and the script's syntax + wiring.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn decision_lib() -> PathBuf {
    manifest_dir().join("scripts/lib/rig-restore-decision.sh")
}

fn heartbeat_lib() -> PathBuf {
    manifest_dir().join("scripts/lib/rig-heartbeat.sh")
}

fn watchdog() -> PathBuf {
    manifest_dir().join("scripts/rig-restore-watchdog.sh")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join(format!("rig-restore-watchdog-{}-{}", std::process::id(), name));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run a bash script with DECISION_LIB / HEARTBEAT_LIB env vars pointing at the libs under test.
fn run_bash(script: &str) -> (i32, String, String) {
    let out = Command::new("bash")
        .arg("-c")
        .arg(script)
        .env("DECISION_LIB", decision_lib())
        .env("HEARTBEAT_LIB", heartbeat_lib())
        .output()
        .expect("run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Invoke `rig_restore_decide` with the given env vars + observation records, parse key=val stdout.
fn decide(env: &[(&str, &str)], obs: &[&str]) -> HashMap<String, String> {
    let mut exports = String::new();
    for (k, v) in env {
        exports.push_str(&format!("export {k}={}\n", shell_quote(v)));
    }
    let obs_joined = obs.join("\n");
    let script = format!(
        r#"set -u
. "$DECISION_LIB"
{exports}export RIG_OBS={obs}
rig_restore_decide
"#,
        exports = exports,
        obs = shell_quote(&obs_joined),
    );
    let (code, stdout, stderr) = run_bash(&script);
    assert_eq!(
        code, 0,
        "rig_restore_decide must exit 0\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let mut map = HashMap::new();
    for line in stdout.lines() {
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    map
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

// ─── existence ───────────────────────────────────────────────────────────────

#[test]
fn decision_lib_exists() {
    assert!(
        decision_lib().exists(),
        "#281 Fix#3: scripts/lib/rig-restore-decision.sh (the pure decision function) must exist"
    );
}

#[test]
fn heartbeat_lib_exists() {
    assert!(
        heartbeat_lib().exists(),
        "#281 Fix#3: scripts/lib/rig-heartbeat.sh must exist"
    );
}

#[test]
fn watchdog_script_exists() {
    assert!(
        watchdog().exists(),
        "#281 Fix#3: scripts/rig-restore-watchdog.sh must exist"
    );
}

#[test]
fn decision_lib_is_sourceable_without_side_effects() {
    let (code, stdout, stderr) = run_bash(r#". "$DECISION_LIB"; echo ok"#);
    assert_eq!(
        code, 0,
        "#281: sourcing rig-restore-decision.sh must be a no-op (no side effects)\nstdout:{stdout}\nstderr:{stderr}"
    );
    assert!(stdout.contains("ok"));
}

#[test]
fn heartbeat_lib_is_sourceable_without_side_effects() {
    let (code, stdout, stderr) = run_bash(r#". "$HEARTBEAT_LIB"; echo ok"#);
    assert_eq!(
        code, 0,
        "#281: sourcing rig-heartbeat.sh must be a no-op\nstdout:{stdout}\nstderr:{stderr}"
    );
    assert!(stdout.contains("ok"));
}

// ─── the heart: pure decision function ───────────────────────────────────────

#[test]
fn fresh_heartbeat_never_acts_even_when_stranded() {
    // The #266-false-positive gate: a legit E2E is running (fresh heartbeat) → NEVER act, no
    // matter how "stranded" the rig looks.
    let d = decide(
        &[("RIG_HB_ACTIVE", "1"), ("RIG_PREV_CONFIRM", "5")],
        &["cam cam1 down=1 probe=1", "obs strih scene=PHASE2-PROBE"],
    );
    assert_eq!(d.get("act").map(String::as_str), Some("0"), "fresh heartbeat must NEVER act");
    assert_eq!(d.get("alert").map(String::as_str), Some("0"));
    assert_eq!(d.get("confirm").map(String::as_str), Some("0"), "fresh heartbeat resets the counter");
}

#[test]
fn no_stranded_signal_resets_and_does_not_act() {
    let d = decide(
        &[("RIG_HB_ACTIVE", "0"), ("RIG_PREV_CONFIRM", "1")],
        &["cam cam1 down=0 probe=0", "obs strih scene=Cam 5"],
    );
    assert_eq!(d.get("act").map(String::as_str), Some("0"));
    assert_eq!(d.get("confirm").map(String::as_str), Some("0"), "clean rig resets the confirm counter");
}

#[test]
fn first_stranded_sighting_is_observe_only() {
    // 1 stranded read = observe-only; counter increments to 1 but does NOT act yet.
    let d = decide(
        &[("RIG_HB_ACTIVE", "0"), ("RIG_PREV_CONFIRM", "0")],
        &["cam cam1 down=1 probe=0"],
    );
    assert_eq!(d.get("confirm").map(String::as_str), Some("1"));
    assert_eq!(d.get("act").map(String::as_str), Some("0"), "first sighting must be observe-only");
    assert_eq!(d.get("alert").map(String::as_str), Some("0"));
}

#[test]
fn second_consecutive_confirmation_acts_and_alerts() {
    let d = decide(
        &[("RIG_HB_ACTIVE", "0"), ("RIG_PREV_CONFIRM", "1")],
        &["cam cam1 down=1 probe=0"],
    );
    assert_eq!(d.get("confirm").map(String::as_str), Some("2"));
    assert_eq!(d.get("act").map(String::as_str), Some("1"), "2nd consecutive confirmation must act");
    assert_eq!(d.get("alert").map(String::as_str), Some("1"), "acting must ALWAYS alert");
    assert!(
        d.get("actions").map(|s| s.contains("restore_cam:cam1")).unwrap_or(false),
        "actions must include restore_cam:cam1 — got {:?}",
        d.get("actions")
    );
}

#[test]
fn stale_probe_alone_is_a_stranded_signal() {
    let d = decide(
        &[("RIG_HB_ACTIVE", "0"), ("RIG_PREV_CONFIRM", "1")],
        &["cam cam2 down=0 probe=1"],
    );
    assert_eq!(d.get("act").map(String::as_str), Some("1"));
    assert!(d.get("actions").map(|s| s.contains("restore_cam:cam2")).unwrap_or(false));
}

#[test]
fn obs_on_known_test_scene_is_a_stranded_signal() {
    let d = decide(
        &[("RIG_HB_ACTIVE", "0"), ("RIG_PREV_CONFIRM", "1")],
        &["obs strih scene=PHASE2-PROBE"],
    );
    assert_eq!(d.get("act").map(String::as_str), Some("1"));
    assert!(
        d.get("actions").map(|s| s.contains("restore_obs:strih")).unwrap_or(false),
        "OBS on a known TEST scene must trigger restore_obs — got {:?}",
        d.get("actions")
    );
}

#[test]
fn obs_on_prod_scene_is_not_a_stranded_signal() {
    // A prod scene name is NOT in the default known-test-scene set → no action (no false positive).
    let d = decide(
        &[("RIG_HB_ACTIVE", "0"), ("RIG_PREV_CONFIRM", "1")],
        &["obs strih scene=Cam 5", "obs stream scene=PRE"],
    );
    assert_eq!(d.get("act").map(String::as_str), Some("0"));
    assert_eq!(d.get("confirm").map(String::as_str), Some("0"));
}

#[test]
fn multiple_stranded_signals_each_get_a_restore_action() {
    let d = decide(
        &[("RIG_HB_ACTIVE", "0"), ("RIG_PREV_CONFIRM", "1")],
        &[
            "cam cam1 down=1 probe=0",
            "cam cam4 down=0 probe=1",
            "obs stream scene=PHASE2-PROBE",
        ],
    );
    assert_eq!(d.get("act").map(String::as_str), Some("1"));
    let actions = d.get("actions").cloned().unwrap_or_default();
    assert!(actions.contains("restore_cam:cam1"), "got {actions}");
    assert!(actions.contains("restore_cam:cam4"), "got {actions}");
    assert!(actions.contains("restore_obs:stream"), "got {actions}");
}

#[test]
fn confirm_threshold_is_configurable() {
    // With threshold=1 the watchdog acts on the FIRST sighting (no second confirmation needed).
    let d = decide(
        &[
            ("RIG_HB_ACTIVE", "0"),
            ("RIG_PREV_CONFIRM", "0"),
            ("RIG_CONFIRM_THRESHOLD", "1"),
        ],
        &["cam cam1 down=1 probe=0"],
    );
    assert_eq!(d.get("confirm").map(String::as_str), Some("1"));
    assert_eq!(d.get("act").map(String::as_str), Some("1"));
}

#[test]
fn known_test_scene_set_is_overridable_via_env() {
    // A custom scene added to RIG_KNOWN_TEST_SCENES becomes actionable; PHASE2-PROBE still counts.
    let d = decide(
        &[
            ("RIG_HB_ACTIVE", "0"),
            ("RIG_PREV_CONFIRM", "1"),
            ("RIG_KNOWN_TEST_SCENES", "PHASE2-PROBE REC-STRIH-TMP"),
        ],
        &["obs stream scene=REC-STRIH-TMP"],
    );
    assert_eq!(d.get("act").map(String::as_str), Some("1"));
    assert!(d.get("actions").map(|s| s.contains("restore_obs:stream")).unwrap_or(false));
}

// ─── heartbeat helpers ───────────────────────────────────────────────────────

#[test]
fn heartbeat_is_fresh_is_a_pure_age_check() {
    // rig_heartbeat_is_fresh <last_epoch> <now_epoch> <stale_sec> -> exit 0 (fresh) / 1 (stale).
    let (code_fresh, _o, e) = run_bash(
        r#". "$HEARTBEAT_LIB"; rig_heartbeat_is_fresh 1000 1010 60; echo "rc=$?""#,
    );
    assert_eq!(code_fresh, 0, "harness must run: {e}");
    let (_c, out_fresh, _e) =
        run_bash(r#". "$HEARTBEAT_LIB"; rig_heartbeat_is_fresh 1000 1010 60 && echo FRESH || echo STALE"#);
    assert!(out_fresh.contains("FRESH"), "10s old with 60s threshold is FRESH — got {out_fresh}");
    let (_c2, out_stale, _e2) =
        run_bash(r#". "$HEARTBEAT_LIB"; rig_heartbeat_is_fresh 1000 1200 60 && echo FRESH || echo STALE"#);
    assert!(out_stale.contains("STALE"), "200s old with 60s threshold is STALE — got {out_stale}");
}

#[test]
fn heartbeat_write_then_clear_round_trips() {
    let dir = scratch("hb-roundtrip");
    let path = dir.join("rig-active");
    let script = format!(
        r#"set -e
. "$HEARTBEAT_LIB"
export CAMERA_BOX_RIG_HEARTBEAT="{p}"
rig_heartbeat_write "unit-test"
test -f "{p}" || {{ echo "MISSING after write"; exit 11; }}
grep -q "unit-test" "{p}" || {{ echo "label not embedded"; exit 12; }}
rig_heartbeat_clear
test -f "{p}" && {{ echo "STILL EXISTS after clear"; exit 13; }}
echo OK
"#,
        p = path.display()
    );
    let (code, stdout, stderr) = run_bash(&script);
    assert_eq!(
        code, 0,
        "#281: heartbeat write→clear must round-trip\nstdout:{stdout}\nstderr:{stderr}"
    );
    assert!(stdout.contains("OK"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn heartbeat_clear_is_idempotent_when_absent() {
    let dir = scratch("hb-idem");
    let path = dir.join("rig-active");
    let script = format!(
        r#". "$HEARTBEAT_LIB"; export CAMERA_BOX_RIG_HEARTBEAT="{p}"; rig_heartbeat_clear; echo rc=$?"#,
        p = path.display()
    );
    let (code, stdout, _stderr) = run_bash(&script);
    assert_eq!(code, 0, "clearing an absent heartbeat must be a no-op success");
    assert!(stdout.contains("rc=0"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn heartbeat_path_resolves_to_a_well_known_location() {
    let (code, stdout, stderr) = run_bash(r#". "$HEARTBEAT_LIB"; rig_heartbeat_path"#);
    assert_eq!(code, 0, "rig_heartbeat_path must run: {stderr}");
    assert!(
        stdout.contains("camera-box-rig-active"),
        "#281: heartbeat path must be the documented camera-box-rig-active file — got {stdout}"
    );
}

// ─── watchdog script: syntax + wiring (static, no rig) ────────────────────────

#[test]
fn watchdog_script_passes_bash_syntax_check() {
    let out = Command::new("bash")
        .arg("-n")
        .arg(watchdog())
        .output()
        .expect("bash -n");
    assert!(
        out.status.success(),
        "#281: scripts/rig-restore-watchdog.sh must pass `bash -n`\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn watchdog_sources_the_pure_decision_lib() {
    let src = fs::read_to_string(watchdog()).expect("read watchdog");
    assert!(
        src.contains("rig-restore-decision.sh"),
        "#281: the watchdog must reuse the pure decision lib, not re-implement the rules"
    );
    assert!(
        src.contains("rig-heartbeat.sh"),
        "#281: the watchdog must read the shared heartbeat helper"
    );
    assert!(
        src.contains("rig_restore_decide"),
        "#281: the watchdog must call rig_restore_decide"
    );
}

#[test]
fn watchdog_executes_the_restore_actions_it_decides() {
    let src = fs::read_to_string(watchdog()).expect("read watchdog");
    // cam restore = restart prod camera-box; obs restore = teardown to the prod scene.
    assert!(
        src.contains("systemctl restart camera-box"),
        "#281: cam restore must restart the prod camera-box service"
    );
    assert!(
        src.contains("obs_phase2.py") && src.contains("teardown"),
        "#281: OBS restore must run obs_phase2.py teardown to restore the prod scene + burns off"
    );
}

#[test]
fn watchdog_always_fires_a_discord_alert_when_it_acts() {
    let src = fs::read_to_string(watchdog()).expect("read watchdog");
    assert!(
        src.contains("airuleset.py notify"),
        "#281: the watchdog must ALWAYS alert via airuleset.py notify so the user knows it acted"
    );
}

#[test]
fn watchdog_persists_the_confirm_counter_across_runs() {
    // The 2-consecutive-confirmation logic only works if the counter survives between the timer's
    // ~2-min invocations — i.e. it is read from / written to a state file, and RIG_PREV_CONFIRM is fed in.
    let src = fs::read_to_string(watchdog()).expect("read watchdog");
    assert!(
        src.contains("RIG_PREV_CONFIRM"),
        "#281: the watchdog must feed the persisted counter into the decision as RIG_PREV_CONFIRM"
    );
    assert!(
        src.to_lowercase().contains("state"),
        "#281: the watchdog must persist the confirm counter in a state file across runs"
    );
}

#[test]
fn watchdog_probes_all_three_cam_boxes() {
    let src = fs::read_to_string(watchdog()).expect("read watchdog");
    for ip in ["10.77.9.61", "10.77.9.62", "10.77.9.64"] {
        assert!(
            src.contains(ip),
            "#281: the watchdog must probe cam box {ip} (cam1/cam2/cam4)"
        );
    }
}

// ─── wiring: heartbeat into the rig-touching harnesses ───────────────────────

#[test]
fn recording_e2e_starts_and_stops_the_heartbeat() {
    let src = fs::read_to_string(manifest_dir().join("scripts/recording-e2e.sh"))
        .expect("read recording-e2e.sh");
    assert!(
        src.contains("rig-heartbeat.sh"),
        "#281: recording-e2e.sh must source the heartbeat lib"
    );
    assert!(
        src.contains("rig_heartbeat_start"),
        "#281: recording-e2e.sh must START a refreshing heartbeat while the rig is in a test state"
    );
    assert!(
        src.contains("rig_heartbeat_stop") || src.contains("rig_heartbeat_clear"),
        "#281: recording-e2e.sh cleanup() must STOP/clear the heartbeat on exit (trap-protected)"
    );
}

#[test]
fn rig_mode_sets_heartbeat_in_test_and_clears_in_event() {
    let src =
        fs::read_to_string(manifest_dir().join("scripts/rig-mode.sh")).expect("read rig-mode.sh");
    assert!(
        src.contains("rig-heartbeat.sh"),
        "#281: rig-mode.sh must source the heartbeat lib"
    );
    assert!(
        src.contains("rig_heartbeat_write") || src.contains("rig_heartbeat_start"),
        "#281: rig-mode.sh TEST mode must SET the heartbeat"
    );
    assert!(
        src.contains("rig_heartbeat_clear") || src.contains("rig_heartbeat_stop"),
        "#281: rig-mode.sh EVENT mode must CLEAR the heartbeat"
    );
}

// ─── obs_phase2.py program-scene reader (used by the watchdog over WS) ────────

#[test]
fn obs_phase2_has_a_program_scene_reader_subcommand() {
    // The watchdog reads the current program scene over WS via obs_phase2.py — reusing its
    // _conn/_rpc WS approach instead of re-implementing the obs-websocket handshake.
    let out = Command::new("python3")
        .arg(manifest_dir().join("scripts/obs_phase2.py"))
        .arg("program-scene")
        .arg("--help")
        .output()
        .expect("run obs_phase2.py program-scene --help");
    assert!(
        out.status.success(),
        "#281: obs_phase2.py must expose a `program-scene` subcommand\nstdout:{}\nstderr:{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

// ─── systemd units shipped DISABLED ──────────────────────────────────────────

#[test]
fn systemd_timer_and_service_units_exist() {
    assert!(
        manifest_dir().join("systemd/rig-restore-watchdog.service").exists(),
        "#281: systemd/rig-restore-watchdog.service must exist"
    );
    assert!(
        manifest_dir().join("systemd/rig-restore-watchdog.timer").exists(),
        "#281: systemd/rig-restore-watchdog.timer must exist"
    );
}

#[test]
fn systemd_install_note_documents_disabled_by_default() {
    let note = manifest_dir().join("systemd/rig-restore-watchdog.README.md");
    assert!(
        note.exists(),
        "#281: systemd/rig-restore-watchdog.README.md install note must exist"
    );
    let body = fs::read_to_string(&note).unwrap().to_lowercase();
    assert!(
        body.contains("disabled"),
        "#281: the install note must state the watchdog ships DISABLED by default"
    );
    assert!(
        body.contains("supervisor"),
        "#281: the install note must state the SUPERVISOR enables + live-verifies before turning it on"
    );
}
