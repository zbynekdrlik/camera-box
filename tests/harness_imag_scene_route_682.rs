//! #682 — recording-e2e.sh never set imag's program scene to the camera under test. Whatever a
//! PRIOR session left on imag's program silently decided which camera imag's leg measured.
//!
//! ## The bug (live incident, 2026-07-11)
//!
//! The CAM=cam1 dedicated cert (RUN_ID 1573931971) FAILed its imag leg purely from test setup:
//! imag's program scene was still 'Cam 4' (leftover from the prior #674 restart experiment), so
//! the recording carried NDI CAM4 -- the 911003 burn configured on 'NDI CAM1' was absent
//! (fail-closed #585) while the cam1->strih->stream chain itself was ZERO loss end-to-end.
//!
//! ## The fix these tests lock (static read of the shell script + the new pure lib -- no rig,
//! no ssh, no live OBS)
//!
//! recording-e2e.sh now sources scripts/lib/imag-scene-route.sh (the SINGLE SOURCE OF TRUTH for
//! camera-name -> imag-scene-name, "cam1" -> "Cam 1" etc — the SAME "Cam {n}" pattern
//! imag_scenes.py itself seeds), saves imag's CURRENT program scene before routing it to the
//! camera-under-test's own scene (failing loud — via obs_phase2.py's existing `switch` non-black
//! self-check — if that scene is missing/dead), and restores the saved scene in cleanup(). Same
//! "no existing anchor line changes" discipline as the sibling `harness_recording_e2e_*` tests
//! (the #675 prevention pattern) — this is a pure ADDITION, not an edit of an existing line.

use std::fs;
use std::process::Command;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

fn lib_path() -> String {
    format!(
        "{}/scripts/lib/imag-scene-route.sh",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// The body of cleanup() -- from `cleanup()` to the `\ntrap ` that installs it (same slice the
/// sibling #675/#684 cleanup tests use).
fn cleanup_body(s: &str) -> String {
    let start = s
        .find("cleanup()")
        .expect("recording-e2e.sh must define cleanup()");
    let end = s[start..]
        .find("\ntrap ")
        .map(|i| start + i)
        .expect("recording-e2e.sh must install the cleanup trap after cleanup()");
    s[start..end].to_string()
}

// --- pure function: imag_scene_for_camera (no ssh, no OBS, no rig) --------------------------

#[test]
fn imag_scene_for_camera_maps_1to1_by_camera_number() {
    for (cam, want) in [
        ("cam1", "Cam 1"),
        ("cam2", "Cam 2"),
        ("cam3", "Cam 3"),
        ("cam4", "Cam 4"),
        ("cam5", "Cam 5"),
        ("cam6", "Cam 6"),
    ] {
        let out = Command::new("bash")
            .arg("-c")
            .arg(format!(
                "set -euo pipefail; . '{}'; imag_scene_for_camera '{cam}'",
                lib_path()
            ))
            .output()
            .expect("run bash");
        assert!(
            out.status.success(),
            "#682: imag_scene_for_camera({cam}) must succeed. stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "#682: imag_scene_for_camera({cam}) must print '{want}' (imag_scenes.py's own \
             f\"Cam {{n}}\" pattern, verified live 2026-07-11)"
        );
    }
}

#[test]
fn imag_scene_for_camera_rejects_an_unknown_name() {
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "set -uo pipefail; . '{}'; imag_scene_for_camera 'cam99'",
            lib_path()
        ))
        .output()
        .expect("run bash");
    assert!(
        !out.status.success(),
        "#682: imag_scene_for_camera must FAIL LOUD on an unknown camera name, never guess a scene"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).is_empty(),
        "#682: an unknown camera name must print no scene name at all"
    );
}

// --- pure function: imag_source_for_camera (#1135 — sibling of imag_scene_for_camera) -------

/// #1135: imag_source_for_camera maps a resolved source camera name to imag-nb's OWN NDI input
/// name ("cam1" -> "NDI CAM1", "cam3" -> "NDI CAM3", ...) — the SAME 1:1 pin imag_scenes.py seeds
/// (f"NDI CAM{n}"). rig-mode.sh derives IMAG_PROG_SOURCE off this so the imag burn target follows
/// the resolved source role, never a hard-pinned 'NDI CAM1'.
#[test]
fn imag_source_for_camera_maps_1to1_by_camera_number_1135() {
    for (cam, want) in [
        ("cam1", "NDI CAM1"),
        ("cam2", "NDI CAM2"),
        ("cam3", "NDI CAM3"),
        ("cam4", "NDI CAM4"),
        ("cam5", "NDI CAM5"),
        ("cam6", "NDI CAM6"),
    ] {
        let out = Command::new("bash")
            .arg("-c")
            .arg(format!(
                "set -euo pipefail; . '{}'; imag_source_for_camera '{cam}'",
                lib_path()
            ))
            .output()
            .expect("run bash");
        assert!(
            out.status.success(),
            "#1135: imag_source_for_camera({cam}) must succeed. stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "#1135: imag_source_for_camera({cam}) must print '{want}' (imag_scenes.py's own \
             f\"NDI CAM{{n}}\" 1:1 pin)"
        );
    }
}

#[test]
fn imag_source_for_camera_rejects_an_unknown_name_1135() {
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "set -uo pipefail; . '{}'; imag_source_for_camera 'cam99'",
            lib_path()
        ))
        .output()
        .expect("run bash");
    assert!(
        !out.status.success(),
        "#1135: imag_source_for_camera must FAIL LOUD on an unknown camera name, never guess an input"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).is_empty(),
        "#1135: an unknown camera name must print no NDI input name at all"
    );
}

// --- recording-e2e.sh must source the lib + route + restore ---------------------------------

#[test]
fn recording_e2e_sources_the_imag_scene_route_lib() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("imag-scene-route.sh"),
        "#682: recording-e2e.sh must source scripts/lib/imag-scene-route.sh"
    );
    assert!(
        s.contains(". \"$HERE/lib/imag-scene-route.sh\""),
        "#682: recording-e2e.sh must actually `source` (not just mention) imag-scene-route.sh"
    );
}

/// The setup step must GET imag's current program scene (to save it) BEFORE it SETs the new one
/// -- ordering matters, mirrors the #675 restart-before-verify ordering test.
#[test]
fn recording_e2e_saves_imags_scene_before_routing_it() {
    let s = read("scripts/recording-e2e.sh");
    let get = s.find("program-scene --host \"$IMAG_IP\"").expect(
        "#682: recording-e2e.sh must GET imag's current program scene (program-scene subcommand)",
    );
    let set = s
        .find("switch --host \"$IMAG_IP\" --program-scene")
        .expect("#682: recording-e2e.sh must SET imag's program scene (switch subcommand)");
    assert!(
        get < set,
        "#682: imag's CURRENT program scene must be read (and saved) BEFORE it is routed to the \
         camera-under-test's scene, so cleanup() can restore it"
    );
}

/// The routing call must use the resolved per-camera scene (via imag_scene_for_camera), and must
/// FAIL LOUD (no `|| true` swallowing it) if the target scene is missing/dead.
#[test]
fn recording_e2e_routes_imag_to_the_camera_under_test_and_fails_loud() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("imag_scene_for_camera \"$CAMERA_NAME\""),
        "#682: recording-e2e.sh must resolve imag's target scene via imag_scene_for_camera \
         \"$CAMERA_NAME\" (never a hardcoded 'Cam 1' — a dedicated cam3/cam4/cam5/cam6 cert must \
         route imag correctly too)"
    );
    let set_idx = s
        .find("switch --host \"$IMAG_IP\" --program-scene")
        .expect("#682: recording-e2e.sh must route imag via the switch subcommand");
    // The routing call's own line must not be swallowed by `|| true` immediately after it (the
    // #195/#257 burn-on-gate fail-loud style, not the cleanup warn-only style) -- scan forward to
    // the end of that statement's line.
    let line_end = s[set_idx..]
        .find('\n')
        .map(|i| set_idx + i)
        .unwrap_or(s.len());
    let stmt = &s[set_idx..line_end];
    assert!(
        !stmt.contains("|| true") && !stmt.contains("2>/dev/null"),
        "#682: routing imag to the camera-under-test must FAIL LOUD (bare set -e propagation) if \
         the scene is missing/dead, mirroring the #195/#257 burn-on gate -- never silently \
         swallowed. Statement: {stmt:?}"
    );
}

/// cleanup() must restore imag's saved program scene -- mirrors the strih/stream scene restore
/// immediately above it, and must be a no-op (never `set -u`-abort) when [4a/8] never ran.
#[test]
fn cleanup_restores_imags_saved_scene() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));
    assert!(
        body.contains("IMAG_PREV_SCENE"),
        "#682: cleanup() must reference IMAG_PREV_SCENE (the scene saved before routing). Body:\n{body}"
    );
    assert!(
        body.contains("switch --host \"$IMAG_IP\""),
        "#682: cleanup() must restore imag's program scene via the switch subcommand. Body:\n{body}"
    );
    // Never an unguarded `set -u` read: either a `${IMAG_PREV_SCENE:-}` default or an explicit
    // `-n "${IMAG_PREV_SCENE` guard so an early abort (before [4a/8] ever ran) can't blow up
    // cleanup() on an unbound variable.
    assert!(
        body.contains("${IMAG_PREV_SCENE:-}") || body.contains("-n \"${IMAG_PREV_SCENE"),
        "#682: cleanup()'s imag-scene restore must guard against IMAG_PREV_SCENE being unset (an \
         early abort before [4a/8] ever ran) -- never a bare `set -u` read. Body:\n{body}"
    );
}

/// IMAG_PREV_SCENE must have a safe pre-trap default (mirrors #246's STRIH_PROG_SCENE /
/// BURN_TARGETS discipline: every var cleanup() reads must be declared, with a safe default,
/// BEFORE the trap installs -- never left to be set only conditionally deep inside the script).
#[test]
fn imag_prev_scene_has_a_safe_pretrap_default() {
    let s = read("scripts/recording-e2e.sh");
    let trap_idx = s
        .find("\ntrap cleanup EXIT HUP INT TERM")
        .expect("recording-e2e.sh must install the cleanup trap");
    let pre_trap = &s[..trap_idx];
    assert!(
        pre_trap.contains("IMAG_PREV_SCENE="),
        "#682: IMAG_PREV_SCENE must be declared (with a safe default) BEFORE the cleanup trap \
         installs, so an early abort never `set -u`-aborts cleanup()'s restore. Pre-trap region \
         does not declare it."
    );
}

// --- issue 1204: the imag BURN TARGET must follow the imag PROGRAM route, not the CAM default ---
//
// Bug (run 32908274448 / verdict 518418121): recording-e2e.sh derived imag's PROGRAM SCENE from the
// camera-under-test (imag_scene_for_camera "$CAMERA_NAME" -> 'Cam 3' -> renders 'NDI CAM3') but the
// imag BURN TARGET (IMAG_PROG_SOURCE) was hard-pinned to 'NDI CAM1'. With cam1 offline-acked and the
// active set = cam3 the two diverged, the burn landed on a NON-program input, and the imag recording
// carried zero 911003 anchors. #1135 added the sibling resolver imag_source_for_camera and its
// docstring already states the invariant ("the imag burn target follows the resolved source role,
// never a hard-pinned 'NDI CAM1'") -- but #1135 only wired it into rig-mode.sh; recording-e2e.sh was
// missed. These tests complete #1135's intent for the E2E harness.

/// The imag burn target must be DERIVED from the camera-under-test via imag_source_for_camera
/// "$CAMERA_NAME" -- exactly mirroring the SCENE derivation (imag_scene_for_camera "$CAMERA_NAME"),
/// so the burn ALWAYS targets the input backing imag's routed program scene.
#[test]
fn recording_e2e_derives_imag_burn_target_via_imag_source_for_camera_1204() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("imag_source_for_camera \"$CAMERA_NAME\""),
        "#1204: recording-e2e.sh must derive the imag burn target (IMAG_PROG_SOURCE) via \
         imag_source_for_camera \"$CAMERA_NAME\" -- the SAME camera-under-test resolution the \
         program SCENE already uses (imag_scene_for_camera \"$CAMERA_NAME\") -- never a hard-pinned \
         'NDI CAM1' (the run 32908274448 zero-anchor failure)."
    );
}

/// The old buggy hard-pinned default must be GONE from recording-e2e.sh (the explanatory 1:1-mapping
/// comment may still MENTION 'NDI CAM1'; only the DEFAULT EXPRESSION must be removed).
#[test]
fn recording_e2e_no_longer_hard_pins_imag_burn_target_to_ndi_cam1_1204() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        !s.contains("IMAG_PROG_SOURCE=\"${IMAG_PROG_SOURCE:-NDI CAM1}\""),
        "#1204: recording-e2e.sh must NOT hard-pin IMAG_PROG_SOURCE to 'NDI CAM1' -- that constant \
         diverges from the program route the moment CAMERA_NAME != cam1 (cam1 offline-acked, active \
         set = cam3)."
    );
}

/// recording-e2e.sh must SOURCE the new fail-closed cross-check helper (the #675 sourced-helper
/// pattern -- new logic lives in a scripts/lib/*.sh file, not inline in the anchor-minefield).
#[test]
fn recording_e2e_sources_imag_burn_verify_lib_1204() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains(". \"$HERE/lib/imag-burn-verify.sh\""),
        "#1204: recording-e2e.sh must actually `source` scripts/lib/imag-burn-verify.sh"
    );
}

/// The [4a/8] imag block must CROSS-CHECK the burn target against the input imag ACTUALLY renders
/// (obs_phase2.py program-rendered-input) and FAIL LOUD (exit 1) on a mismatch -- so the [4b/8]
/// burn-check (which validates IMAG_PROG_SOURCE) is proven to be checking the PROGRAM input, and any
/// future derivation/route divergence fails BEFORE the wasted recording (fail-closed, #901 style).
#[test]
fn recording_e2e_cross_checks_imag_burn_target_against_program_rendered_input_1204() {
    let s = read("scripts/recording-e2e.sh");
    let read_idx = s.find("program-rendered-input --host \"$IMAG_IP\"").expect(
        "#1204: recording-e2e.sh must READ imag's actually-rendered program input via \
             obs_phase2.py program-rendered-input --host \"$IMAG_IP\"",
    );
    let call_idx = s.find("imag_burn_target_matches_program").expect(
        "#1204: recording-e2e.sh must cross-check via imag_burn_target_matches_program (the \
             pure helper), not an inline ad-hoc comparison",
    );
    // The read must precede the comparison (you compare what you just read).
    assert!(
        read_idx < call_idx,
        "#1204: the program-rendered-input read must precede the imag_burn_target_matches_program \
         cross-check"
    );
    // The mismatch path must FAIL LOUD via exit 1 (not a warn-only `|| true`).
    let after = &s[call_idx..(call_idx + 400).min(s.len())];
    assert!(
        after.contains("exit 1"),
        "#1204: an imag burn-target mismatch must FAIL LOUD (exit 1) -- never a silent warn. \
         Region after the cross-check:\n{after}"
    );
    // The cross-check must live in the [4a/8] imag block, AFTER the scene switch (so it verifies the
    // routed program), and stay inside the IMAG_OFFLINE_ACKED-guarded else-branch (issue 1013/1171).
    let switch_idx = s
        .find("switch --host \"$IMAG_IP\" --program-scene")
        .expect("#1204: the imag scene switch must exist");
    assert!(
        switch_idx < read_idx,
        "#1204: the burn-target cross-check must run AFTER the [4a/8] scene switch (it verifies the \
         ROUTED program's rendered input)"
    );
}
