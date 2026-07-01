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
    let dir = std::env::temp_dir().join(format!(
        "rig-restore-watchdog-{}-{}",
        std::process::id(),
        name
    ));
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
    assert_eq!(
        d.get("act").map(String::as_str),
        Some("0"),
        "fresh heartbeat must NEVER act"
    );
    assert_eq!(d.get("alert").map(String::as_str), Some("0"));
    assert_eq!(
        d.get("confirm").map(String::as_str),
        Some("0"),
        "fresh heartbeat resets the counter"
    );
}

#[test]
fn no_stranded_signal_resets_and_does_not_act() {
    let d = decide(
        &[("RIG_HB_ACTIVE", "0"), ("RIG_PREV_CONFIRM", "1")],
        &["cam cam1 down=0 probe=0", "obs strih scene=Cam 5"],
    );
    assert_eq!(d.get("act").map(String::as_str), Some("0"));
    assert_eq!(
        d.get("confirm").map(String::as_str),
        Some("0"),
        "clean rig resets the confirm counter"
    );
}

#[test]
fn first_stranded_sighting_is_observe_only() {
    // 1 stranded read = observe-only; counter increments to 1 but does NOT act yet.
    let d = decide(
        &[("RIG_HB_ACTIVE", "0"), ("RIG_PREV_CONFIRM", "0")],
        &["cam cam1 down=1 probe=0"],
    );
    assert_eq!(d.get("confirm").map(String::as_str), Some("1"));
    assert_eq!(
        d.get("act").map(String::as_str),
        Some("0"),
        "first sighting must be observe-only"
    );
    assert_eq!(d.get("alert").map(String::as_str), Some("0"));
}

#[test]
fn second_consecutive_confirmation_acts_and_alerts() {
    let d = decide(
        &[("RIG_HB_ACTIVE", "0"), ("RIG_PREV_CONFIRM", "1")],
        &["cam cam1 down=1 probe=0"],
    );
    assert_eq!(d.get("confirm").map(String::as_str), Some("2"));
    assert_eq!(
        d.get("act").map(String::as_str),
        Some("1"),
        "2nd consecutive confirmation must act"
    );
    assert_eq!(
        d.get("alert").map(String::as_str),
        Some("1"),
        "acting must ALWAYS alert"
    );
    assert!(
        d.get("actions")
            .map(|s| s.contains("restore_cam:cam1"))
            .unwrap_or(false),
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
    assert!(d
        .get("actions")
        .map(|s| s.contains("restore_cam:cam2"))
        .unwrap_or(false));
}

#[test]
fn obs_on_known_test_scene_is_a_stranded_signal() {
    let d = decide(
        &[("RIG_HB_ACTIVE", "0"), ("RIG_PREV_CONFIRM", "1")],
        &["obs strih scene=PHASE2-PROBE"],
    );
    assert_eq!(d.get("act").map(String::as_str), Some("1"));
    assert!(
        d.get("actions")
            .map(|s| s.contains("restore_obs:strih"))
            .unwrap_or(false),
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
    assert!(d
        .get("actions")
        .map(|s| s.contains("restore_obs:stream"))
        .unwrap_or(false));
}

// ─── #352: scene names in RIG_KNOWN_TEST_SCENES must NOT contain spaces ──────

#[test]
fn spaced_scene_name_in_known_set_does_not_match_full_record() {
    // #352 invariant: the matcher word-splits $known (`for ks in $known`), so a TWO-WORD entry
    // like "NDI 2ME PGM" splits into "NDI","2ME","PGM" — none of which equals the full program
    // scene name "NDI 2ME PGM". This lock test PROVES the documented no-spaces constraint: a
    // spaced entry does NOT match the OBS record carrying that exact full name, so it can never
    // (silently) trigger a restore. A future maintainer who adds a spaced scene name and expects
    // it to match will see THIS test fail, pointing them at the no-spaces requirement.
    let d = decide(
        &[
            ("RIG_HB_ACTIVE", "0"),
            ("RIG_PREV_CONFIRM", "1"),
            ("RIG_KNOWN_TEST_SCENES", "NDI 2ME PGM"),
        ],
        &["obs stream scene=NDI 2ME PGM"],
    );
    assert_eq!(
        d.get("act").map(String::as_str),
        Some("0"),
        "#352: a SPACED scene name in RIG_KNOWN_TEST_SCENES must NOT match (the word-split makes \
         it impossible) — got act={:?} reason={:?}",
        d.get("act"),
        d.get("reason")
    );
    assert_eq!(
        d.get("confirm").map(String::as_str),
        Some("0"),
        "#352: a non-matching (spaced) known-scene entry means no stranded signal → counter resets"
    );
    // Control: the SAME full name, but added to the set as a single hyphen-joined token, DOES
    // match — confirming the non-match above is caused purely by the spaces, not the name itself.
    let hyphenated = decide(
        &[
            ("RIG_HB_ACTIVE", "0"),
            ("RIG_PREV_CONFIRM", "1"),
            ("RIG_KNOWN_TEST_SCENES", "NDI-2ME-PGM"),
        ],
        &["obs stream scene=NDI-2ME-PGM"],
    );
    assert_eq!(
        hyphenated.get("act").map(String::as_str),
        Some("1"),
        "#352 control: a hyphenated (space-free) scene name matches normally — proving the \
         spaced-name non-match is the word-split invariant, not a different bug"
    );
}

// ─── #353: REC-STRIH-TMP is legacy (per #343 the stream box stays on prod PRO); the E2E MARKER,
//      not a scene-name list, now detects a stranded rig ──────────────────────────────────────

#[test]
fn rec_strih_tmp_no_longer_default_scene_marker_detects_instead() {
    // #343 changed recording-e2e.sh to record the stream box's ALREADY-ACTIVE prod scene (PRO)
    // instead of building + switching to the ephemeral REC-STRIH-TMP scene, so the stream box
    // never lands on REC-STRIH-TMP any more. #353 therefore DROPS REC-STRIH-TMP from the default
    // RIG_KNOWN_TEST_SCENES (dead since #343) and replaces scene-name scraping with the E2E
    // MARKER: a harness that entered a test state and died without cleaning up leaves the marker
    // behind, and THAT (not the scraped scene name) is the stranded signal.
    //
    // Without a marker, a box merely sitting on REC-STRIH-TMP is NOT flagged by the default set
    // (the legacy scene is gone). RED with the old default ("PHASE2-PROBE REC-STRIH-TMP") — which
    // would still flag it — GREEN after the default drops to "PHASE2-PROBE".
    let no_marker = decide(
        &[("RIG_HB_ACTIVE", "0"), ("RIG_PREV_CONFIRM", "1")],
        &["obs stream scene=REC-STRIH-TMP"],
    );
    assert_eq!(
        no_marker.get("act").map(String::as_str),
        Some("0"),
        "#353: REC-STRIH-TMP is no longer in the DEFAULT known-test-scenes (legacy since #343) — \
         without an E2E marker a box on it must NOT trigger a restore — got act={:?} reason={:?}",
        no_marker.get("act"),
        no_marker.get("reason")
    );

    // WITH the marker present (a harness left it behind on an unclean death), the SAME box IS
    // flagged — regardless of which scene it is on — and gets a restore_obs.
    let with_marker = decide(
        &[
            ("RIG_HB_ACTIVE", "0"),
            ("RIG_PREV_CONFIRM", "1"),
            ("RIG_E2E_MARKER", "1"),
        ],
        &["obs stream scene=REC-STRIH-TMP"],
    );
    assert_eq!(
        with_marker.get("act").map(String::as_str),
        Some("1"),
        "#353: with the E2E marker present, a stranded stream box is detected regardless of scene \
         — got act={:?} reason={:?}",
        with_marker.get("act"),
        with_marker.get("reason")
    );
    assert!(
        with_marker
            .get("actions")
            .map(|s| s.contains("restore_obs:stream"))
            .unwrap_or(false),
        "actions must include restore_obs:stream — got {:?}",
        with_marker.get("actions")
    );
}

// ─── #353: the E2E MARKER — durable stranded-rig detection (replaces scene scraping) ────────

#[test]
fn marker_present_strands_every_observed_obs_box_regardless_of_scene() {
    // A harness (recording-e2e.sh) writes the marker on entry and removes it ONLY on clean exit.
    // A leftover marker + no fresh heartbeat = a harness entered a test state and died without
    // cleaning up. The watchdog must then restore EVERY observed OBS box, regardless of scene —
    // robust to env-overridden / custom scene names, no scene-list to keep in sync (#353).
    let d = decide(
        &[
            ("RIG_HB_ACTIVE", "0"),
            ("RIG_PREV_CONFIRM", "1"),
            ("RIG_E2E_MARKER", "1"),
        ],
        // both boxes on genuine PROD scenes — scene scraping would flag NEITHER
        &["obs strih scene=Cam 5", "obs stream scene=PRO"],
    );
    assert_eq!(
        d.get("act").map(String::as_str),
        Some("1"),
        "#353: marker present + heartbeat absent must strand the rig even when OBS is on prod \
         scenes — got act={:?} reason={:?}",
        d.get("act"),
        d.get("reason")
    );
    let actions = d.get("actions").cloned().unwrap_or_default();
    assert!(actions.contains("restore_obs:strih"), "got {actions}");
    assert!(actions.contains("restore_obs:stream"), "got {actions}");
}

#[test]
fn marker_present_is_still_gated_by_a_fresh_heartbeat() {
    // The heartbeat gate is a SEPARATE mechanism that must stay intact: while a legit E2E runs it
    // holds BOTH a fresh heartbeat AND the marker. A fresh heartbeat must still win → NEVER act,
    // so the watchdog can never fight a live E2E (the #266 false-positive lesson).
    let d = decide(
        &[
            ("RIG_HB_ACTIVE", "1"),
            ("RIG_PREV_CONFIRM", "5"),
            ("RIG_E2E_MARKER", "1"),
        ],
        &["obs stream scene=PRO"],
    );
    assert_eq!(
        d.get("act").map(String::as_str),
        Some("0"),
        "#353: a fresh heartbeat must override the marker — a live E2E is running, never act"
    );
    assert_eq!(d.get("confirm").map(String::as_str), Some("0"));
}

#[test]
fn marker_absent_falls_back_to_scene_list_no_false_positive() {
    // With NO marker (older harness / manual testing) the RIG_KNOWN_TEST_SCENES fallback still
    // works AND a prod scene is still safe (no false positive): PHASE2-PROBE (default) → act,
    // a prod scene → no act.
    let on_test_scene = decide(
        &[("RIG_HB_ACTIVE", "0"), ("RIG_PREV_CONFIRM", "1")],
        &["obs strih scene=PHASE2-PROBE"],
    );
    assert_eq!(
        on_test_scene.get("act").map(String::as_str),
        Some("1"),
        "#353: with no marker, the scene-list fallback must still flag a known TEST scene"
    );
    let on_prod = decide(
        &[("RIG_HB_ACTIVE", "0"), ("RIG_PREV_CONFIRM", "1")],
        &["obs strih scene=Cam 5", "obs stream scene=PRO"],
    );
    assert_eq!(
        on_prod.get("act").map(String::as_str),
        Some("0"),
        "#353: with no marker, prod scenes must NOT trigger a restore"
    );
}

#[test]
fn marker_present_does_not_restore_a_healthy_cam() {
    // The marker fixes the FRAGILE part (OBS scene detection). Cam restore stays driven by the
    // RELIABLE down/probe signals — a healthy cam (up, no stale probe) must NOT be restored just
    // because a marker is present, even though its OBS boxes are restored (#353).
    let d = decide(
        &[
            ("RIG_HB_ACTIVE", "0"),
            ("RIG_PREV_CONFIRM", "1"),
            ("RIG_E2E_MARKER", "1"),
        ],
        &["cam cam1 down=0 probe=0", "obs strih scene=PRO"],
    );
    let actions = d.get("actions").cloned().unwrap_or_default();
    assert!(
        !actions.contains("restore_cam:cam1"),
        "#353: a healthy cam must NOT be restored on marker-presence alone — got {actions}"
    );
    assert!(
        actions.contains("restore_obs:strih"),
        "#353: but the observed OBS box IS restored on marker-presence — got {actions}"
    );
}

// ─── #353: the marker helpers (rig-heartbeat.sh) ─────────────────────────────

#[test]
fn marker_set_present_clear_round_trips() {
    let dir = scratch("marker-roundtrip");
    let path = dir.join("rig-in-e2e");
    let script = format!(
        r#"set -e
. "$HEARTBEAT_LIB"
export CAMERA_BOX_RIG_E2E_MARKER="{p}"
rig_e2e_marker_present && {{ echo "PRESENT before set"; exit 11; }}
rig_e2e_marker_set "unit-test"
test -f "{p}" || {{ echo "MISSING after set"; exit 12; }}
rig_e2e_marker_present || {{ echo "not PRESENT after set"; exit 13; }}
rig_e2e_marker_clear
test -f "{p}" && {{ echo "STILL EXISTS after clear"; exit 14; }}
rig_e2e_marker_present && {{ echo "PRESENT after clear"; exit 15; }}
echo OK
"#,
        p = path.display()
    );
    let (code, stdout, stderr) = run_bash(&script);
    assert_eq!(
        code, 0,
        "#353: marker set→present→clear must round-trip\nstdout:{stdout}\nstderr:{stderr}"
    );
    assert!(stdout.contains("OK"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn marker_clear_is_idempotent_when_absent() {
    let dir = scratch("marker-idem");
    let path = dir.join("rig-in-e2e");
    let script = format!(
        r#". "$HEARTBEAT_LIB"; export CAMERA_BOX_RIG_E2E_MARKER="{p}"; rig_e2e_marker_clear; echo rc=$?"#,
        p = path.display()
    );
    let (code, stdout, _stderr) = run_bash(&script);
    assert_eq!(
        code, 0,
        "#353: clearing an absent marker must be a no-op success"
    );
    assert!(stdout.contains("rc=0"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn marker_path_resolves_to_the_documented_rig_in_e2e_file() {
    let (code, stdout, stderr) = run_bash(r#". "$HEARTBEAT_LIB"; rig_e2e_marker_path"#);
    assert_eq!(code, 0, "rig_e2e_marker_path must run: {stderr}");
    assert!(
        stdout.contains("rig-in-e2e"),
        "#353: marker path must be the documented rig-in-e2e file — got {stdout}"
    );
}

#[test]
fn marker_set_returns_nonzero_when_the_path_is_unwritable() {
    // #353 review (finding 3): rig_e2e_marker_set must NOT swallow a failed write — it must return
    // the real exit status so recording-e2e.sh's `|| echo WARNING` guard actually fires. A silently-
    // missing marker would make the watchdog blind to a stranded rig. Point the marker at a path
    // whose parent cannot be created (under /dev/null), so both mkdir -p and the write fail.
    let (code, _o, _e) = run_bash(
        r#". "$HEARTBEAT_LIB"; export CAMERA_BOX_RIG_E2E_MARKER=/dev/null/nope/rig-in-e2e; rig_e2e_marker_set "x"; echo rc=$?"#,
    );
    assert_eq!(code, 0, "harness wrapper itself runs");
    let (_c, stdout, _e2) = run_bash(
        r#". "$HEARTBEAT_LIB"; export CAMERA_BOX_RIG_E2E_MARKER=/dev/null/nope/rig-in-e2e; rig_e2e_marker_set "x" && echo SET_OK || echo SET_FAIL"#,
    );
    assert!(
        stdout.contains("SET_FAIL"),
        "#353: rig_e2e_marker_set must return non-zero when the write fails (so the WARNING guard \
         fires) — got {stdout}"
    );
}

// ─── #353 review (finding 1): the watchdog clears the marker ONLY after a positive full restore ──

#[test]
fn marker_should_clear_only_after_a_positive_full_obs_restore() {
    // The masking bug: clearing the marker on ANY act (gated only on marker presence) drops the
    // durable stranded signal even when the OBS box that the marker exists to catch was NEVER
    // restored this pass (OBS unreadable → no restore_obs emitted; or a teardown failed). The next
    // pass then has no marker and — if the box sits on a custom/env scene outside the fallback list
    // — it stays stranded forever. The pure `rig_marker_should_clear <marker> <act> <obs_unreadable>
    // <obs_failed>` gate fixes it: CLEAR (exit 0) ONLY when marker present, we acted, NO OBS box was
    // unreadable, and NO OBS teardown failed; otherwise KEEP (exit 1) so a later pass retries.
    let cases: &[(&str, i32, &str)] = &[
        // marker act unreadable failed
        ("1 1 0 0", 0, "all conditions met → CLEAR"),
        (
            "1 1 1 0",
            1,
            "an OBS box was unreadable → KEEP (might hide a stranded box)",
        ),
        (
            "1 1 0 1",
            1,
            "an OBS teardown failed → KEEP (box not confirmed restored)",
        ),
        ("1 1 2 1", 1, "unreadable AND failed → KEEP"),
        (
            "0 1 0 0",
            1,
            "no marker → KEEP (nothing to clear via this path)",
        ),
        ("1 0 0 0", 1, "did not act → KEEP"),
        ("x 1 0 0", 1, "non-numeric arg → conservatively KEEP"),
    ];
    for (args, want_rc, why) in cases {
        let script = format!(r#". "$DECISION_LIB"; rig_marker_should_clear {args}; echo rc=$?"#);
        let (_c, stdout, stderr) = run_bash(&script);
        let got = if stdout.contains("rc=0") { 0 } else { 1 };
        assert_eq!(
            got, *want_rc,
            "#353 rig_marker_should_clear {args} → expected rc={want_rc} ({why})\nstdout:{stdout}\nstderr:{stderr}"
        );
    }
}

#[test]
fn prod_scene_pro_not_in_default_known_test_scenes() {
    // "PRO" is the real prod scene on the stream box — it must NOT appear in the default
    // known-test-scenes set, so (with no marker) it never triggers a restore (no false positive).
    let d = decide(
        &[("RIG_HB_ACTIVE", "0"), ("RIG_PREV_CONFIRM", "1")],
        &["obs strih scene=Cam 5", "obs stream scene=PRO"],
    );
    assert_eq!(
        d.get("act").map(String::as_str),
        Some("0"),
        "prod scenes Cam 5 + PRO must NOT trigger restore — got act={:?} reason={:?}",
        d.get("act"),
        d.get("reason")
    );
    assert_eq!(
        d.get("confirm").map(String::as_str),
        Some("0"),
        "clean rig (prod scenes only) must reset the counter"
    );
}

// ─── heartbeat helpers ───────────────────────────────────────────────────────

#[test]
fn heartbeat_is_fresh_is_a_pure_age_check() {
    // rig_heartbeat_is_fresh <last_epoch> <now_epoch> <stale_sec> -> exit 0 (fresh) / 1 (stale).
    let (code_fresh, _o, e) =
        run_bash(r#". "$HEARTBEAT_LIB"; rig_heartbeat_is_fresh 1000 1010 60; echo "rc=$?""#);
    assert_eq!(code_fresh, 0, "harness must run: {e}");
    let (_c, out_fresh, _e) = run_bash(
        r#". "$HEARTBEAT_LIB"; rig_heartbeat_is_fresh 1000 1010 60 && echo FRESH || echo STALE"#,
    );
    assert!(
        out_fresh.contains("FRESH"),
        "10s old with 60s threshold is FRESH — got {out_fresh}"
    );
    let (_c2, out_stale, _e2) = run_bash(
        r#". "$HEARTBEAT_LIB"; rig_heartbeat_is_fresh 1000 1200 60 && echo FRESH || echo STALE"#,
    );
    assert!(
        out_stale.contains("STALE"),
        "200s old with 60s threshold is STALE — got {out_stale}"
    );
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
    assert_eq!(
        code, 0,
        "clearing an absent heartbeat must be a no-op success"
    );
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

#[test]
fn refresher_removes_heartbeat_when_its_owner_dies() {
    // A SIGKILL'd harness bypasses its cleanup trap; the disowned refresher must NOT keep the
    // heartbeat "fresh" forever (that would make the watchdog blind to the exact #281 stranded
    // rig). The refresher checks its owner PID each tick and removes the heartbeat once the owner
    // is gone. Here we point the owner at a guaranteed-dead PID so the FIRST tick self-expires it.
    let dir = scratch("hb-owner-death");
    let path = dir.join("rig-active");
    let script = format!(
        r#". "$HEARTBEAT_LIB"
export CAMERA_BOX_RIG_HEARTBEAT="{p}"
export RIG_HEARTBEAT_REFRESH_SEC=1
# A PID that is already dead: spawn `true`, reap it, reuse its PID as the (dead) owner.
( true ) & dead=$!; wait "$dead" 2>/dev/null
export RIG_HEARTBEAT_OWNER_PID="$dead"
rig_heartbeat_start "owner-death-test"
test -f "{p}" || {{ echo "MISSING right after start"; exit 21; }}
# Within a few refresh intervals the refresher sees the dead owner and removes the heartbeat.
for _ in 1 2 3 4 5 6; do
  sleep 1
  [ -f "{p}" ] || {{ echo OK_REMOVED; exit 0; }}
done
echo "STILL FRESH after owner death"; exit 22
"#,
        p = path.display()
    );
    let (code, stdout, stderr) = run_bash(&script);
    assert_eq!(
        code, 0,
        "#281: the refresher must remove the heartbeat once its owner dies\nstdout:{stdout}\nstderr:{stderr}"
    );
    assert!(stdout.contains("OK_REMOVED"));
    let _ = fs::remove_dir_all(&dir);
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

// ─── #353: wiring — harness writes/removes the marker, watchdog reads + clears it ────────────

#[test]
fn recording_e2e_sets_and_clears_the_e2e_marker() {
    let src = fs::read_to_string(manifest_dir().join("scripts/recording-e2e.sh"))
        .expect("read recording-e2e.sh");
    assert!(
        src.contains("rig_e2e_marker_set"),
        "#353: recording-e2e.sh must SET the E2E marker on entry (a harness in a test state)"
    );
    assert!(
        src.contains("rig_e2e_marker_clear"),
        "#353: recording-e2e.sh cleanup() must CLEAR the marker on clean exit (trap-protected)"
    );
}

#[test]
fn watchdog_reads_and_clears_the_e2e_marker() {
    let src = fs::read_to_string(watchdog()).expect("read watchdog");
    assert!(
        src.contains("rig_e2e_marker_present"),
        "#353: the watchdog must read the E2E marker as a stranded-rig signal"
    );
    assert!(
        src.contains("RIG_E2E_MARKER"),
        "#353: the watchdog must feed the marker presence into the decision as RIG_E2E_MARKER"
    );
    assert!(
        src.contains("rig_e2e_marker_clear"),
        "#353: the watchdog must CLEAR the marker after it restores prod, so a stale marker does \
         not re-trigger (and re-alert) on the next pass"
    );
    assert!(
        src.contains("rig_marker_should_clear"),
        "#353 (review): the watchdog must gate the marker-clear behind rig_marker_should_clear so it \
         only clears after a positive full OBS restore (never while an OBS box was unreadable or a \
         teardown failed — that would mask a still-stranded box)"
    );
}

#[test]
fn rec_strih_tmp_dropped_from_default_known_test_scenes_in_both_files() {
    // #353/#343: the now-legacy REC-STRIH-TMP scene must be removed from the DEFAULT
    // RIG_KNOWN_TEST_SCENES in BOTH the decision lib and the watchdog (the two-file-sync the
    // marker mechanism eliminates). PHASE2-PROBE stays (still a live obs_phase2 probe scene).
    for f in [
        "scripts/lib/rig-restore-decision.sh",
        "scripts/rig-restore-watchdog.sh",
    ] {
        let src = fs::read_to_string(manifest_dir().join(f)).expect("read file");
        let default_lines: Vec<&str> = src
            .lines()
            .filter(|l| l.contains("RIG_KNOWN_TEST_SCENES:-"))
            .collect();
        assert!(
            !default_lines.is_empty(),
            "{f} must set a RIG_KNOWN_TEST_SCENES default"
        );
        for l in default_lines {
            assert!(
                !l.contains("REC-STRIH-TMP"),
                "#353: {f} must DROP REC-STRIH-TMP from the default RIG_KNOWN_TEST_SCENES (legacy \
                 since #343) — line: {l}"
            );
            assert!(
                l.contains("PHASE2-PROBE"),
                "#353: {f} default must still include PHASE2-PROBE — line: {l}"
            );
        }
    }
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
    // _conn/_rpc WS approach instead of re-implementing the obs-websocket handshake. Verified
    // STATICALLY (the function + the argparse registration + the dispatch wiring): obs_phase2.py
    // imports websocket-client at module load, which the cargo Test job runner does NOT install
    // (only the separate "Python harness tests" job does), so running it here is environment-fragile.
    let src = fs::read_to_string(manifest_dir().join("scripts/obs_phase2.py"))
        .expect("read obs_phase2.py");
    assert!(
        src.contains("def program_scene("),
        "#281: obs_phase2.py must define a program_scene() reader for the watchdog"
    );
    assert!(
        src.contains("\"program-scene\""),
        "#281: obs_phase2.py must register the `program-scene` subcommand in argparse + dispatch"
    );
    assert!(
        src.contains("GetCurrentProgramScene"),
        "#281: the program-scene reader must read the OBS current program scene over WS"
    );
}

// ─── systemd units shipped DISABLED ──────────────────────────────────────────

#[test]
fn systemd_timer_and_service_units_exist() {
    assert!(
        manifest_dir()
            .join("systemd/rig-restore-watchdog.service")
            .exists(),
        "#281: systemd/rig-restore-watchdog.service must exist"
    );
    assert!(
        manifest_dir()
            .join("systemd/rig-restore-watchdog.timer")
            .exists(),
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

// ─── #370: alert classification + rate-limit (pure seam, Tier-0) ─────────────

/// Invoke `rig_classify_restore` with positional integer args; parse key=val stdout.
fn classify_restore(act: &str, obs_unreadable: &str, obs_failed: &str) -> HashMap<String, String> {
    let script = format!(
        r#"set -u
. "$DECISION_LIB"
rig_classify_restore {act} {obs_unreadable} {obs_failed}
"#
    );
    let (code, stdout, stderr) = run_bash(&script);
    assert_eq!(
        code, 0,
        "#370: rig_classify_restore must exit 0\nstdout:{stdout}\nstderr:{stderr}"
    );
    let mut map = HashMap::new();
    for line in stdout.lines() {
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    map
}

/// Invoke `rig_alert_throttle`; parse key=val stdout.
fn alert_throttle(
    kind: &str,
    current_sig: &str,
    prior_sig: &str,
    prior_passes: &str,
    throttle_n: Option<&str>,
) -> HashMap<String, String> {
    let tn_arg = match throttle_n {
        Some(n) => format!(" {}", shell_quote(n)),
        None => String::new(),
    };
    let script = format!(
        r#"set -u
. "$DECISION_LIB"
rig_alert_throttle {kind} {csig} {psig} {pp}{tn}
"#,
        kind = shell_quote(kind),
        csig = shell_quote(current_sig),
        psig = shell_quote(prior_sig),
        pp = shell_quote(prior_passes),
        tn = tn_arg,
    );
    let (code, stdout, stderr) = run_bash(&script);
    assert_eq!(
        code, 0,
        "#370: rig_alert_throttle must exit 0\nstdout:{stdout}\nstderr:{stderr}"
    );
    let mut map = HashMap::new();
    for line in stdout.lines() {
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    map
}

// ── rig_classify_restore ──────────────────────────────────────────────────────

#[test]
fn classify_restore_positive_when_full_restore() {
    // act=1, obs_unreadable=0, obs_failed=0 → kind=positive (all OBS boxes restored)
    let m = classify_restore("1", "0", "0");
    assert_eq!(
        m.get("kind").map(String::as_str),
        Some("positive"),
        "#370: full restore (0 unreadable, 0 failed) must be classified as positive"
    );
}

#[test]
fn classify_restore_partial_when_obs_unreadable() {
    // An OBS box was unreadable → marker KEPT, prod only partially restored → partial
    let m = classify_restore("1", "1", "0");
    assert_eq!(
        m.get("kind").map(String::as_str),
        Some("partial"),
        "#370: obs_unreadable>0 must classify as partial (marker KEPT for retry)"
    );
}

#[test]
fn classify_restore_partial_when_teardown_failed() {
    // An OBS teardown returned non-zero → box NOT confirmed restored → partial
    let m = classify_restore("1", "0", "1");
    assert_eq!(
        m.get("kind").map(String::as_str),
        Some("partial"),
        "#370: obs_failed>0 must classify as partial (box restore unconfirmed)"
    );
}

#[test]
fn classify_restore_partial_when_both_unreadable_and_failed() {
    let m = classify_restore("1", "1", "1");
    assert_eq!(
        m.get("kind").map(String::as_str),
        Some("partial"),
        "#370: unreadable AND failed must classify as partial"
    );
}

#[test]
fn classify_restore_partial_on_non_numeric_args() {
    // Non-numeric input → conservatively partial (never classify garbage as positive)
    let m = classify_restore("x", "0", "0");
    assert_eq!(
        m.get("kind").map(String::as_str),
        Some("partial"),
        "#370: non-numeric act must default to partial (fail-conservative)"
    );
}

// ── rig_alert_throttle ────────────────────────────────────────────────────────

#[test]
fn alert_throttle_positive_always_alerts() {
    // Positive restore: ALWAYS alert regardless of prior state; counter resets to 0
    let m = alert_throttle("positive", "positive::0", "positive::0", "3", Some("5"));
    assert_eq!(
        m.get("alert_now").map(String::as_str),
        Some("1"),
        "#370: positive kind must always alert (never throttled even with same sig + passes)"
    );
    assert_eq!(
        m.get("new_passes").map(String::as_str),
        Some("0"),
        "#370: positive restore resets throttle pass counter to 0"
    );
}

#[test]
fn alert_throttle_partial_first_occurrence_alerts() {
    // prior_sig="" (no prior) → first occurrence → alert, new_passes=1
    let m = alert_throttle("partial", "partial:strih:0", "", "0", None);
    assert_eq!(
        m.get("alert_now").map(String::as_str),
        Some("1"),
        "#370: first partial occurrence (no prior sig) must alert"
    );
    assert_eq!(
        m.get("new_sig").map(String::as_str),
        Some("partial:strih:0"),
        "#370: new_sig must be set to current_sig after first alert"
    );
    assert_eq!(
        m.get("new_passes").map(String::as_str),
        Some("1"),
        "#370: first alert sets new_passes=1 (1 pass since last alert)"
    );
}

#[test]
fn alert_throttle_partial_same_sig_suppressed() {
    // Same sig, prior_passes=1, throttle_n=5 → 1 < 5 → suppress, increment counter
    let m = alert_throttle("partial", "partial:strih:0", "partial:strih:0", "1", Some("5"));
    assert_eq!(
        m.get("alert_now").map(String::as_str),
        Some("0"),
        "#370: same partial condition (passes=1 < throttle_n=5) must be suppressed"
    );
    assert_eq!(
        m.get("new_passes").map(String::as_str),
        Some("2"),
        "#370: suppressed pass increments counter to 2"
    );
}

#[test]
fn alert_throttle_partial_same_sig_repeat_alerts_after_n_passes() {
    // Same sig, prior_passes=5 ≥ throttle_n=5 → re-alert, reset to 1
    let m = alert_throttle("partial", "partial:strih:0", "partial:strih:0", "5", Some("5"));
    assert_eq!(
        m.get("alert_now").map(String::as_str),
        Some("1"),
        "#370: same partial condition after throttle_n (5) passes must re-alert"
    );
    assert_eq!(
        m.get("new_passes").map(String::as_str),
        Some("1"),
        "#370: re-alert resets pass counter to 1"
    );
}

#[test]
fn alert_throttle_partial_sig_change_always_alerts() {
    // Signature changed (different unreadable box) → alert immediately
    let m = alert_throttle(
        "partial",
        "partial:stream:0",
        "partial:strih:0",
        "2",
        Some("5"),
    );
    assert_eq!(
        m.get("alert_now").map(String::as_str),
        Some("1"),
        "#370: a different unreadable box (sig change) must always alert"
    );
    assert_eq!(
        m.get("new_sig").map(String::as_str),
        Some("partial:stream:0"),
        "#370: new_sig must reflect the new (changed) current_sig"
    );
    assert_eq!(
        m.get("new_passes").map(String::as_str),
        Some("1"),
        "#370: sig-change alert sets new_passes=1"
    );
}

// ── watchdog wiring (static source checks) ────────────────────────────────────

#[test]
fn watchdog_calls_rig_classify_restore() {
    let src = fs::read_to_string(watchdog()).expect("read watchdog");
    assert!(
        src.contains("rig_classify_restore"),
        "#370: the watchdog must call rig_classify_restore to classify positive vs partial"
    );
}

#[test]
fn watchdog_calls_rig_alert_throttle() {
    let src = fs::read_to_string(watchdog()).expect("read watchdog");
    assert!(
        src.contains("rig_alert_throttle"),
        "#370: the watchdog must call rig_alert_throttle to rate-limit repeat KEPT/partial alerts"
    );
}

#[test]
fn watchdog_alert_body_distinguishes_partial_from_positive() {
    // The positive body must keep "AUTO-RECOVERED"; the partial body must be distinct.
    let src = fs::read_to_string(watchdog()).expect("read watchdog");
    assert!(
        src.contains("AUTO-RECOVERED"),
        "#370: the positive alert body must still contain AUTO-RECOVERED (unchanged from #281)"
    );
    assert!(
        src.to_uppercase().contains("PARTIAL") || src.contains("KEPT"),
        "#370: the watchdog must emit a DISTINCT lower-urgency body for the partial/KEPT case"
    );
}

#[test]
fn watchdog_state_persists_alert_sig_and_passes() {
    // The STATE_FILE must be extended to hold alert_sig + alert_passes for throttle dedup.
    let src = fs::read_to_string(watchdog()).expect("read watchdog");
    assert!(
        src.contains("alert_sig"),
        "#370: watchdog state must persist alert_sig across invocations (throttle dedup)"
    );
    assert!(
        src.contains("alert_passes"),
        "#370: watchdog state must persist alert_passes across invocations (throttle pass counting)"
    );
}
