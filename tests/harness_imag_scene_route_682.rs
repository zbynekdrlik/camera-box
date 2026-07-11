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
