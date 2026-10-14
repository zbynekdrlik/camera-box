//! #1134 — the E2E chain's SOURCE node (the "cam1 role": films cam2's monitor, carries the
//! #174 capture burn, routed onto strih program) must be VYRADITEĽNÝ: derivable off
//! `CAMERA_ACTIVE_SET` instead of hard-pinned to cam1, so retiring cam1 (its USB grabber
//! hardware-faulted — #1110 `-EPROTO`, live grayscale capture; owner order #1130) moves the
//! source role to the next healthy box (cam3) with a one-line set edit and nothing else.
//!
//! Doctrine (camera-set.sh header, #827/#898/#939): retirement is expressed ONLY as
//! `CAMERA_ACTIVE_SET` membership. #1134 extends that doctrine from the SECONDARY cameras to the
//! PRIMARY/source role via a new `camera_source_box()` derivation.
//!
//! RED before #1134: `camera_source_box` does not exist; the default set includes cam1;
//! recording-e2e.sh hard-pins the source to the literal `cam1` at three sites. GREEN after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source `camera-set.sh` with an optional `CAMERA_ACTIVE_SET` / `CAMERA_SOURCE_BOX` override and
/// run `camera_source_box`, returning `(ok, stdout_trimmed)`.
fn source_box(active_set: Option<&str>, source_box_env: Option<&str>) -> (bool, String) {
    let script = manifest_dir().join("scripts/camera-set.sh");
    let harness = r#"
set -uo pipefail
. "$SCRIPT"
if out="$(camera_source_box 2>/dev/null)"; then printf 'OK\t%s' "$out"; else printf 'REJECT'; fi
"#;
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(harness).env("SCRIPT", &script);
    if let Some(v) = active_set {
        cmd.env("CAMERA_ACTIVE_SET", v);
    }
    if let Some(v) = source_box_env {
        cmd.env("CAMERA_SOURCE_BOX", v);
    }
    let out = cmd
        .output()
        .expect("failed to run camera_source_box harness");
    let stdout = String::from_utf8_lossy(&out.stdout);
    if let Some(v) = stdout.strip_prefix("OK\t") {
        (true, v.to_string())
    } else {
        (false, String::new())
    }
}

#[test]
fn camera_source_box_defaults_to_cam5_the_test_camera_usb_box_1413() {
    // issue 1413 (owner 12.10.2026): cam1/cam4/cam6/cam7 moved to Poprad, the SNV default is
    // "cam2 cam3 cam5". 14.10.2026: the test camera's USB-C is on cam5 (its bkshading relay runs
    // there), so the default source is cam5 (CAMERA_TEST_CAMERA_BOX), not the first routable cam3;
    // the release E2E 38520662080 aborted at issue 1371 with the camera found on no candidate box.
    // The history below is kept.
    // issue 1198 (2026-08-27, owner ruling): both cam1's #1110 "hardware-defective" diagnosis and
    // cam2's #1170 "camera-under-test retired" diagnosis were built from EPISODES, not a permanent
    // card state -- the owner refused the physical card swap outright and a live journal check
    // confirmed both cards are healthy today. The default active set is back to
    // "cam1 cam2 cam3 cam4 cam5 cam6 cam7" -- the FULL seven-camera fleet, as of issue 1216's
    // completion (2026-08-30, owner directive "kamery od 1-7 bezia" after a physical cable
    // reseat; cam5 briefly dropped out again same-day as its 2026-08-28 restoration on issue
    // 1217, a DEAD_PORT splitter leg, before rejoining once its leg read colour again):
    // the source role is the FIRST strih-routable member = cam1 (cam2 is the painter and
    // camera_strih_route rejects it, so it is skipped) -- byte-identical to the pre-#1110
    // behaviour. The derivation (not a literal) is the point -- re-retiring either camera again
    // (if a fresh episode reproduces) just changes the set.
    let (ok, got) = source_box(None, None);
    assert!(
        ok,
        "#1134: camera_source_box must resolve a source for the default active set"
    );
    assert_eq!(
        got, "cam5",
        "issue 1413: the default source box must be cam5, the SNV box whose USB-C carries the test \
         camera (CAMERA_TEST_CAMERA_BOX), for the default CAMERA_ACTIVE_SET='cam2 cam3 cam5'"
    );
}

#[test]
fn camera_source_box_is_cam1_for_any_legacy_cam1_first_set_backcompat() {
    // Back-compat: every legacy set that still lists cam1 first resolves the source to cam1
    // EXACTLY as before #1134 — so re-adding cam1 to CAMERA_ACTIVE_SET (once its grabber is
    // fixed) restores the cam1 source role with a one-line edit, no other change.
    let (ok, got) = source_box(Some("cam1 cam2 cam3"), None);
    assert!(
        ok,
        "#1134: camera_source_box must resolve for a cam1-first set"
    );
    assert_eq!(
        got, "cam1",
        "#1134: a cam1-first active set must still select cam1 as the source (back-compat)"
    );
}

#[test]
fn camera_source_box_skips_the_painter_cam2_and_picks_the_first_strih_routable() {
    // cam2 is the fixed painter — camera_strih_route rejects it — so even when cam2 comes first
    // in the set, camera_source_box skips it and picks the first strih-routable member.
    let (ok, got) = source_box(Some("cam2 cam4"), None);
    assert!(
        ok,
        "#1134: camera_source_box must resolve past the painter cam2"
    );
    assert_eq!(
        got, "cam4",
        "#1134: cam2 (painter, not strih-routable) must be skipped; cam4 is the source here"
    );
}

#[test]
fn camera_source_box_honors_the_explicit_env_override() {
    // CAMERA_SOURCE_BOX is the operator's explicit override (same trust model as CAM=), wins over
    // the derivation — for a one-off run pinning a specific source box.
    let (ok, got) = source_box(Some("cam2 cam3"), Some("cam4"));
    assert!(ok, "#1134: an explicit CAMERA_SOURCE_BOX must resolve");
    assert_eq!(
        got, "cam4",
        "#1134: CAMERA_SOURCE_BOX must override the derived-from-active-set source box"
    );
}

#[test]
fn camera_source_box_fails_loudly_when_the_active_set_has_no_strih_routable_source() {
    // A set with only the painter cam2 has no strih-routable source — a misconfiguration that
    // must fail loudly (nonzero), never silently certify with no source.
    let (ok, _got) = source_box(Some("cam2"), None);
    assert!(
        !ok,
        "#1134: camera_source_box must REJECT (nonzero) a painter-only active set — no source"
    );
}

#[test]
fn camera_source_box_does_not_clobber_the_strih_route_globals() {
    // camera_source_box probes camera_strih_route to test routability; it must do so WITHOUT
    // leaking CAMERA_STRIH_SCENE/CAMERA_STRIH_SOURCE into the caller (it runs the probe in a
    // subshell) — otherwise a caller that resolved cam3 could read a stale/wrong strih scene.
    let script = manifest_dir().join("scripts/camera-set.sh");
    let harness = r#"
set -uo pipefail
. "$SCRIPT"
CAMERA_STRIH_SCENE=SENTINEL_SCENE
CAMERA_STRIH_SOURCE=SENTINEL_SOURCE
camera_source_box >/dev/null
printf 'SCENE=%s\nSOURCE=%s\n' "${CAMERA_STRIH_SCENE:-}" "${CAMERA_STRIH_SOURCE:-}"
"#;
    let out = Command::new("bash")
        .arg("-c")
        .arg(harness)
        .env("SCRIPT", &script)
        .env("CAMERA_ACTIVE_SET", "cam2 cam3")
        .output()
        .expect("failed to run clobber-check harness");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("SCENE=SENTINEL_SCENE") && stdout.contains("SOURCE=SENTINEL_SOURCE"),
        "#1134: camera_source_box must not clobber CAMERA_STRIH_SCENE/CAMERA_STRIH_SOURCE. Got:\n{stdout}"
    );
}

// --- recording-e2e.sh must READ the source role, never the literal cam1 --------------------------

#[test]
fn recording_e2e_source_default_derives_from_camera_source_box() {
    // L305: the SOURCE-camera default must be $(camera_source_box), not the literal cam1 — so an
    // un-overridden gate run selects the derived source (cam3) instead of the retired cam1.
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("camera_resolve \"${CAM:-$(camera_source_box)}\"")
            || s.contains("camera_resolve \"${CAM:-$E2E_SOURCE_BOX}\""),
        "#1134: recording-e2e.sh must default the SOURCE camera to $(camera_source_box) \
         (camera-set.sh), never the literal cam1."
    );
    assert!(
        !s.contains("camera_resolve \"${CAM:-cam1}\""),
        "#1134: recording-e2e.sh must no longer hard-pin the SOURCE default to the literal cam1."
    );
}

#[test]
fn recording_e2e_all_cambox_guard_reads_the_source_role_not_literal_cam1() {
    // L326: the ALL_CAMBOX-with-non-default-source guard must compare $CAMERA_NAME against the
    // DERIVED source box, not the literal cam1 (else it would wrongly reject the new default
    // source cam3, or wrongly accept a stale cam1).
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains(r#"[ "$CAMERA_NAME" != "$(camera_source_box)" ]"#)
            || s.contains(r#"[ "$CAMERA_NAME" != "$E2E_SOURCE_BOX" ]"#),
        "#1134: the ALL_CAMBOX guard must reject a source != the derived source box, not != cam1."
    );
    assert!(
        !s.contains(r#"[ "$CAMERA_NAME" != "cam1" ]"#),
        "#1134: the ALL_CAMBOX guard must no longer be pinned to the literal cam1."
    );
}

#[test]
fn recording_e2e_fleet_preflight_labels_the_source_with_camera_name() {
    // L961-962: the ALL_CAMBOX fleet preflight target list must label the source node with
    // $CAMERA_NAME (the resolved source), never the literal "cam1" — otherwise, with cam1 acked
    // in rig-fleet.txt, the preflight would treat the resolved source IP under the name "cam1"
    // and the stale-ack guard would fire on a healthy cam3.
    let s = read("scripts/recording-e2e.sh");
    let idx = s
        .find("PREFLIGHT_TARGETS=(")
        .expect("#1134: recording-e2e.sh must define PREFLIGHT_TARGETS");
    let window = &s[idx..(idx + 200).min(s.len())];
    assert!(
        window.contains("\"$CAMERA_NAME=$CAM1_IP\""),
        "#1134: PREFLIGHT_TARGETS must label the source node with $CAMERA_NAME, not the literal \
         cam1. Window:\n{window}"
    );
    assert!(
        !window.contains("\"cam1=$CAM1_IP\""),
        "#1134: PREFLIGHT_TARGETS must no longer hard-label the source node as cam1."
    );
}

// --- restoration (issue 1198, 2026-08-27): default set regains cam1+cam2, rig-fleet.txt's cam1
// ack is REMOVED (owner ruling: both cards are healthy, the swap is off the table) ---------------

#[test]
fn camera_active_set_default_keeps_cam2_and_moves_cam1_to_poprad_1413() {
    // issue 1198 restored cam1 + cam2; issue 1413 (12.10.2026) moved cam1 (with cam4/cam6/cam7)
    // to Poprad, so the SNV default keeps cam2 and drops cam1.
    let s = read("scripts/camera-set.sh");
    assert!(
        s.contains("CAMERA_ACTIVE_SET=\"${CAMERA_ACTIVE_SET:-cam2 cam3 cam5}\""),
        "issue 1413: the camera-set.sh default must be the SNV rig's \"cam2 cam3 cam5\" -- \
         cam1/cam4/cam6/cam7 moved to Poprad on 11.10.2026."
    );
}

#[test]
fn rig_fleet_no_longer_acks_cam1_1198() {
    // The #1110 offline ack is the FIRST knob of the reversible-retirement procedure (the second
    // being CAMERA_ACTIVE_SET membership above) -- issue 1198 removes it now that cam1 is back in
    // the active set (a healthy, in-set box acked here would trip the stale-ack guard, #758/#827).
    let s = read("rig-fleet.txt");
    assert!(
        !s.contains("cam1:grabber-hw-defect-swap-pending-issue-1110"),
        "issue 1198: rig-fleet.txt must NOT ack cam1 any more -- it is back in CAMERA_ACTIVE_SET \
         and a healthy in-set box would trip the stale-ack guard if still acked."
    );
}

#[test]
fn restoration_procedure_reverses_the_original_retirement_membership_plus_ack_1198() {
    // The documented RE-ENABLE procedure this doctrine always promised (camera-active-set.md):
    // cam1 back == add it to CAMERA_ACTIVE_SET AND delete its rig-fleet.txt ack line — nothing
    // else. Prove the round trip actually completed: cam1 is present in the default active set
    // AND its rig-fleet.txt ack line is gone, while its facts (case arm) stay fully resolvable.
    let cs = read("scripts/camera-set.sh");
    let rf = read("rig-fleet.txt");
    // issue 1413: cam1 has since moved to Poprad -- out of the SNV default set, its facts still
    // resolvable, and (a box outside the active set) no ack line is needed for it.
    assert!(
        cs.contains("CAMERA_ACTIVE_SET=\"${CAMERA_ACTIVE_SET:-cam2 cam3 cam5}\"")
            && cs.contains("cam1) CAMERA_IP=10.77.9.61"),
        "issue 1413: cam1 must be out of the SNV default active set AND still fully resolvable \
         (its case arm intact) -- membership-only, never a deleted fact."
    );
    assert!(
        !rf.contains("cam1:grabber-hw-defect-swap-pending-issue-1110"),
        "issue 1198: cam1's ack line must be gone -- the second (and last) knob of the reversal."
    );
}

#[test]
fn every_python_camera_active_set_default_mirror_matches_camera_set_sh_1134() {
    // #1134 doctrine: the CAMERA_ACTIVE_SET default is ONE declared list. camera-set.sh is
    // authoritative; every standalone Python subprocess that self-defaults CAMERA_ACTIVE_SET (its
    // env-fallback, hit when the caller passes no --active-set and the var is not exported) MUST
    // mirror the SAME literal, or a future retirement that edits camera-set.sh but misses one
    // silently re-selects a retired camera in a standalone run (the camera-active-set drift risk).
    // set-ndi-mapping.py is ALSO locked against camera-set.sh by harness_rig_ndi_mapping.rs; this
    // extends the lock to the other four mirrors so none can silently diverge. issue 1198
    // (2026-08-27): cam1+cam2 restored. issue 1216 (2026-08-28): cam5/cam6/cam7 also restored;
    // issue 1217 (same day): cam5 dropped back out (DEAD_PORT splitter leg). issue 1216
    // completion (2026-08-30, owner directive "kamery od 1-7 bezia"): cam4 (#947) and cam5
    // (DEAD_PORT) both rejoin. issue 1413 (2026-10-12): cam1/cam4/cam6/cam7 moved to Poprad --
    // the shared literal is now the SNV rig's "cam2 cam3 cam5".
    let sh = read("scripts/camera-set.sh");
    assert!(
        sh.contains("CAMERA_ACTIVE_SET=\"${CAMERA_ACTIVE_SET:-cam2 cam3 cam5}\""),
        "camera-set.sh must default CAMERA_ACTIVE_SET to \"cam2 cam3 cam5\""
    );
    let fallback = r#"os.environ.get("CAMERA_ACTIVE_SET", "cam2 cam3 cam5")"#;
    for py in [
        "scripts/set-ndi-mapping.py",
        "scripts/phase_sync_calibrate.py",
        "scripts/phase_sync_active_floor_check.py",
        "scripts/phase_sync_reanchor.py",
        "scripts/latency_pins_snapshot.py",
    ] {
        assert!(
            read(py).contains(fallback),
            "#1134/issue 1198/#1216/#1217: {py} must mirror camera-set.sh's CAMERA_ACTIVE_SET \
             default via the identical env-fallback os.environ.get(\"CAMERA_ACTIVE_SET\", \
             \"cam2 cam3 cam5\") -- a diverged fallback silently re-selects a camera that is not \
             at the SNV rig in a standalone run"
        );
    }
}

#[test]
fn an_overridden_active_set_ignores_the_snv_test_camera_box_1413() {
    // CAMERA_TEST_CAMERA_BOX belongs to the SNV DEFAULT set only: an explicit CAMERA_ACTIVE_SET
    // (a legacy or test set) keeps the first-strih-routable derivation, even when it lists cam5.
    let (ok, got) = source_box(Some("cam1 cam2 cam3 cam4 cam5"), None);
    assert!(ok, "issue 1413: an explicit set must resolve a source");
    assert_eq!(
        got, "cam1",
        "issue 1413: an explicit cam1-first set keeps cam1 even though it lists cam5"
    );
}
