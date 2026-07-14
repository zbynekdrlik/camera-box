//! Regression guard for #24 — the frame-loss harness must be parameterized over the
//! camera SET (cam1-4), not hard-coded to cam2.
//!
//! Before #24 the orchestrators baked cam2 in:
//!   * `scripts/multitap-e2e.sh`: `CAM2=10.77.9.62` and `CAM_SOURCE="CAM2 (usb)"`
//!     (plain vars, NO env override) — the full-path gate could ONLY certify cam2.
//!   * `scripts/loopback-e2e.sh`: `CAM_IP`/`SOURCE` were env-overridable but only the
//!     cam2 default was wired; nothing resolved a camera NAME → its IP + NDI source, so
//!     driving cam1/cam3/cam4 meant hand-passing two correlated values every time.
//!
//! The fix introduces ONE source of truth — `scripts/camera-set.sh` — that maps a camera
//! name (`cam1`..`cam4`) to its IP and NDI source (`"CAMn (usb)"`), and both orchestrators
//! resolve through it (defaulting to cam2 for back-compat). These tests pin that:
//!   1. the resolver returns the RIGHT IP + source per camera (cam1-4), and rejects
//!      unknown names (so a typo fails loudly, never silently certifies the wrong box);
//!   2. resolution is injection-safe — a hostile `CAM` value cannot run a command when the
//!      resolved env prefix is applied by a remote shell (same threat model as #39);
//!   3. the orchestrators no longer hard-code cam2's IP and actually route through the set.
//!
//! RED before #24 (no `camera-set.sh`; multitap hard-codes `CAM2=10.77.9.62`); GREEN after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source `camera-set.sh`, run `camera_resolve <name>`, and return its
/// `IP\t<ip>\nSOURCE\t<source>` resolution (or the empty string + nonzero exit on reject).
fn resolve(cam: &str) -> (bool, String, String) {
    let script = manifest_dir().join("scripts/camera-set.sh");
    assert!(script.exists(), "{} not found", script.display());

    // The resolver must expose IP + SOURCE via two well-known shell vars after calling
    // `camera_resolve <name>`. We read them back through a child bash exactly as the
    // orchestrators do, so the test exercises the REAL contract, not a re-spelling of it.
    let harness = r#"
set -uo pipefail
. "$SCRIPT"
if camera_resolve "$CAM" 2>/dev/null; then
  printf 'OK\nIP\t%s\nSOURCE\t%s\n' "$CAMERA_IP" "$CAMERA_SOURCE"
else
  printf 'REJECT\n'
fi
"#;

    let out = Command::new("bash")
        .arg("-c")
        .arg(harness)
        .env("SCRIPT", &script)
        .env("CAM", cam)
        .output()
        .expect("failed to run bash resolver harness");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let ok = stdout.lines().next() == Some("OK");
    let mut ip = String::new();
    let mut src = String::new();
    for line in stdout.lines() {
        if let Some(v) = line.strip_prefix("IP\t") {
            ip = v.to_string();
        } else if let Some(v) = line.strip_prefix("SOURCE\t") {
            src = v.to_string();
        }
    }
    (ok, ip, src)
}

#[test]
fn camera_set_resolves_all_four_cameras() {
    // The authoritative cam1-4 map (CLAUDE.md / targets.md). The resolver must return
    // exactly these — a wrong IP would deploy the probe to (and certify) the WRONG box.
    let expected = [
        ("cam1", "10.77.9.61", "CAM1 (usb)"),
        ("cam2", "10.77.9.62", "CAM2 (usb)"),
        ("cam3", "10.77.9.63", "CAM3 (usb)"),
        ("cam4", "10.77.9.64", "CAM4 (usb)"),
    ];

    for (name, ip, source) in expected {
        let (ok, got_ip, got_src) = resolve(name);
        assert!(ok, "camera_resolve {name} should succeed");
        assert_eq!(got_ip, ip, "camera_resolve {name} resolved the wrong IP");
        assert_eq!(
            got_src, source,
            "camera_resolve {name} resolved the wrong NDI source"
        );
    }
}

#[test]
fn camera_set_rejects_unknown_camera() {
    // A typo must FAIL loudly, not silently fall through to cam2 (the exact way the harness
    // would otherwise certify the wrong camera while reporting success).
    let (ok, _, _) = resolve("cam9");
    assert!(!ok, "camera_resolve cam9 must reject an unknown camera");

    let (ok, _, _) = resolve("");
    assert!(!ok, "camera_resolve '' must reject an empty camera name");
}

#[test]
fn camera_set_resolution_is_injection_safe() {
    // #39 threat model, re-applied to the new selector: a hostile CAM value (e.g. from a
    // workflow_dispatch input) must NOT be able to run a command when the resolver is
    // sourced and the name is looked up. A safe resolver rejects unknown names with no
    // eval/word-splitting of the value, so nothing executes.
    let script = manifest_dir().join("scripts/camera-set.sh");
    let marker = std::env::temp_dir().join(format!("camset_inject_marker_{}", std::process::id()));
    let _ = std::fs::remove_file(&marker);

    let evil = format!("cam2; touch {}", marker.display());

    let harness = r#"
set -uo pipefail
. "$SCRIPT"
camera_resolve "$CAM" >/dev/null 2>&1 || true
"#;

    let _ = Command::new("bash")
        .arg("-c")
        .arg(harness)
        .env("SCRIPT", &script)
        .env("CAM", &evil)
        .output()
        .expect("failed to run bash injection harness");

    let injected = marker.exists();
    let _ = std::fs::remove_file(&marker);
    assert!(
        !injected,
        "injection: a hostile CAM value escaped camera_resolve and ran `touch {}`. \
         The resolver must look the name up without eval/word-splitting the value.",
        marker.display()
    );
}

#[test]
fn loopback_e2e_routes_through_camera_set() {
    // loopback-e2e.sh already had CAM_IP/SOURCE overrides; #24 adds NAME-based selection
    // (CAM=cam3) so an operator drives any camera by name, resolved through the shared set.
    let s = read("scripts/loopback-e2e.sh");
    assert!(
        s.contains("camera-set.sh"),
        "#24: loopback-e2e.sh must source scripts/camera-set.sh so a CAM name (cam1-4) \
         resolves its IP + NDI source from the single source of truth."
    );
}

// --- #451: fleet growing 4 -> 6 (cam5/cam6) + per-camera CAMERA_GENLOCK_FPS -------------------
// --- #593 (superseded by #753, see below): cam7 was excluded because it had NEVER been built.
// -------------------------------------------------------------------------------------------

#[test]
fn camera_set_resolves_cam5_and_cam6() {
    // The fleet grew 4->6 (#451). A wrong/missing IP would deploy the probe to (and certify)
    // the WRONG box, exactly like the original cam1-4 guard above.
    let expected = [
        ("cam5", "10.77.9.65", "CAM5 (usb)"),
        ("cam6", "10.77.9.66", "CAM6 (usb)"),
    ];

    for (name, ip, source) in expected {
        let (ok, got_ip, got_src) = resolve(name);
        assert!(
            ok,
            "camera_resolve {name} should succeed (#451 fleet growth 4->6)"
        );
        assert_eq!(got_ip, ip, "camera_resolve {name} resolved the wrong IP");
        assert_eq!(
            got_src, source,
            "camera_resolve {name} resolved the wrong NDI source"
        );
    }
}

// --- #753 (2026-07-14): cam7 physical box EXISTS and is fully provisioned --------------------
// The #593 "cam7 was never built" premise the three tests above used to pin is now STALE --
// verify-device.sh CAM7 reported ALL CLEAR (21/21), static IP 10.77.9.67, grabber Elgato 4K S.
// Genuinely-wrong-test rewrite per tdd-workflow.md: the world these tests encoded has changed,
// not the code's correctness.

#[test]
fn camera_set_resolves_cam7() {
    // #753: cam7 is a REAL, fully-provisioned box now (10.77.9.67, Elgato 4K S,
    // verify-device.sh ALL CLEAR 21/21) -- camera_resolve must resolve it exactly like every
    // other real camera, mirroring camera_set_resolves_cam5_and_cam6 above.
    let (ok, ip, source) = resolve("cam7");
    assert!(
        ok,
        "#753: camera_resolve cam7 should succeed -- the box is real now"
    );
    assert_eq!(
        ip, "10.77.9.67",
        "camera_resolve cam7 resolved the wrong IP"
    );
    assert_eq!(
        source, "CAM7 (usb)",
        "camera_resolve cam7 resolved the wrong NDI source"
    );
}

#[test]
fn camera_set_reject_message_lists_seven_cameras() {
    // #753: cam7 is real now -- the reject message must list all seven, or a typo report
    // misleads whoever reads it about which names are actually valid.
    let s = read("scripts/camera-set.sh");
    assert!(
        s.contains("expected one of: cam1 cam2 cam3 cam4 cam5 cam6 cam7"),
        "#753: the unknown-camera reject message must list all seven real cameras (cam1..cam7)."
    );
}

#[test]
fn camera_set_default_includes_all_seven_cameras() {
    // CAMERA_SET is the "drive the whole set" default the fleet-wide orchestrators
    // (deploy-fleet.sh, verify-fleet.sh) fall back to when the operator doesn't override it.
    // #753: cam7 is a real, fully-provisioned box now -- the default must include it.
    let s = read("scripts/camera-set.sh");
    assert!(
        s.contains("CAMERA_SET=\"${CAMERA_SET:-cam1 cam2 cam3 cam4 cam5 cam6 cam7}\""),
        "#753: CAMERA_SET default must list all seven real cameras (cam1..cam7)."
    );
}

/// Source `camera-set.sh`, run `camera_resolve <name>`, and return the resolved
/// `CAMERA_GENLOCK_FPS` value (or empty string on reject).
fn resolve_genlock_fps(cam: &str) -> String {
    let script = manifest_dir().join("scripts/camera-set.sh");
    let harness = r#"
set -uo pipefail
. "$SCRIPT"
if camera_resolve "$CAM" 2>/dev/null; then
  printf 'FPS\t%s\n' "$CAMERA_GENLOCK_FPS"
fi
"#;
    let out = Command::new("bash")
        .arg("-c")
        .arg(harness)
        .env("SCRIPT", &script)
        .env("CAM", cam)
        .output()
        .expect("failed to run bash resolver harness");
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("FPS\t"))
        .unwrap_or_default()
        .to_string()
}

#[test]
fn camera_resolve_emits_per_camera_genlock_fps() {
    // #451: camera_resolve() must ALSO set an authoritative per-camera CAMERA_GENLOCK_FPS
    // (today uniformly 60 for the whole program-feeding fleet) — this is the table #450's
    // provisioning drop-in generation is meant to read, distinct from the existing GLOBAL
    // GENLOCK_FPS the harness uses for its own manually-launched cam1 sender.
    for cam in ["cam1", "cam2", "cam3", "cam4", "cam5", "cam6"] {
        let fps = resolve_genlock_fps(cam);
        assert_eq!(
            fps, "60",
            "camera_resolve {cam} must set CAMERA_GENLOCK_FPS=60 (#451); got '{fps}'"
        );
    }
}

// --- #24 item 1 / #312 item 1: camera_strih_route() -- which strih OBS scene shows a given
// physical camera, so scripts/recording-e2e.sh can drive cam1, cam3, cam4, cam5, OR cam6 as the
// dedicated SOURCE camera (the box filming cam2's monitor + carrying the #174 capture burn)
// instead of being hard-coded to cam1. The scene/source pins mirror
// scripts/set-ndi-mapping.py's fixed, Claude-owned genlock mapping exactly: NDI cam5->CAM1,
// NDI cam1->CAM3, NDI cam3->CAM4, NDI cam4->CAM5, NDI cam6->CAM6. -------

/// Source `camera-set.sh`, run `camera_strih_route <name>`, and return its
/// `SCENE\t<scene>\nSOURCE\t<source>` resolution (or REJECT on an unsupported name).
fn resolve_strih_route(cam: &str) -> (bool, String, String) {
    let script = manifest_dir().join("scripts/camera-set.sh");
    let harness = r#"
set -uo pipefail
. "$SCRIPT"
if camera_strih_route "$CAM" 2>/dev/null; then
  printf 'OK\nSCENE\t%s\nSOURCE\t%s\n' "$CAMERA_STRIH_SCENE" "$CAMERA_STRIH_SOURCE"
else
  printf 'REJECT\n'
fi
"#;
    let out = Command::new("bash")
        .arg("-c")
        .arg(harness)
        .env("SCRIPT", &script)
        .env("CAM", cam)
        .output()
        .expect("failed to run bash camera_strih_route harness");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let ok = stdout.lines().next() == Some("OK");
    let mut scene = String::new();
    let mut source = String::new();
    for line in stdout.lines() {
        if let Some(v) = line.strip_prefix("SCENE\t") {
            scene = v.to_string();
        } else if let Some(v) = line.strip_prefix("SOURCE\t") {
            source = v.to_string();
        }
    }
    (ok, scene, source)
}

#[test]
fn camera_strih_route_resolves_the_five_source_eligible_cameras() {
    // #24/#312: the exact pins scripts/set-ndi-mapping.py programs onto strih (NDI cam5->CAM1,
    // NDI cam1->CAM3, NDI cam3->CAM4, NDI cam4->CAM5, NDI cam6->CAM6). A wrong scene/source
    // would route strih's PROGRAM to the WRONG box's NDI feed and silently certify nothing (or
    // the wrong camera).
    let expected = [
        ("cam1", "Cam 5", "NDI cam5"),
        ("cam3", "Cam 1", "NDI cam1"),
        ("cam4", "Cam 3", "NDI cam3"),
        ("cam5", "Cam 4", "NDI cam4"),
        ("cam6", "Cam 6", "NDI cam6"),
    ];
    for (name, scene, source) in expected {
        let (ok, got_scene, got_source) = resolve_strih_route(name);
        assert!(ok, "camera_strih_route {name} should succeed");
        assert_eq!(
            got_scene, scene,
            "camera_strih_route {name} resolved the wrong strih scene"
        );
        assert_eq!(
            got_source, source,
            "camera_strih_route {name} resolved the wrong strih NDI-input source"
        );
    }
}

#[test]
fn camera_strih_route_resolves_cam7() {
    // #753: cam7's strih route is a NEW, DIRECT (non-inverted) pin -- scene 'Cam 7' shows
    // input 'NDI cam7' 1:1, unlike the historical six's set-ndi-mapping.py DEFAULT_MAP
    // inversion. Wired live over OBS WebSocket as part of this ticket's strih integration.
    let (ok, scene, source) = resolve_strih_route("cam7");
    assert!(ok, "camera_strih_route cam7 should succeed (#753)");
    assert_eq!(
        scene, "Cam 7",
        "camera_strih_route cam7 resolved the wrong strih scene"
    );
    assert_eq!(
        source, "NDI cam7",
        "camera_strih_route cam7 resolved the wrong strih NDI-input source"
    );
}

#[test]
fn camera_strih_route_rejects_cam2_and_unknown_cameras() {
    // cam2 is the FIXED painter -- NEVER the SOURCE camera-under-test here, deliberately, even
    // though #312 makes it a measurable "camera under test" for the ALL-CAMBOX sweep's digital
    // contiguity check by a SEPARATE mechanism (recording-e2e.sh's CAMBOX_SWEEP default +
    // [2b/8] deploy loop, keyed off $PAINTER_IP directly). Accepting "cam2" HERE would let an
    // un-overridden recording-e2e.sh run (whose $CAMERA_NAME default IS "cam2") try to deploy
    // the SOURCE-camera burn binary to the SAME physical box $PAINTER_IP already targets --
    // a real /dev/video0 + /dev/fb0 device conflict. A typo or an out-of-scope camera must also
    // fail loudly, never silently route the wrong scene.
    for name in ["cam2", "cam9", ""] {
        let (ok, _scene, _source) = resolve_strih_route(name);
        assert!(
            !ok,
            "camera_strih_route '{name}' must reject -- not a SOURCE-eligible camera (#24/#312)"
        );
    }
}
