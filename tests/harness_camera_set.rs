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
// --- #593: cam7 removed -- it was NEVER built (the user only expressed future interest); it
// must not resolve as an active camera anywhere in the fleet map. ------------------------------

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
        assert!(ok, "camera_resolve {name} should succeed (#451 fleet growth 4->6)");
        assert_eq!(got_ip, ip, "camera_resolve {name} resolved the wrong IP");
        assert_eq!(
            got_src, source,
            "camera_resolve {name} resolved the wrong NDI source"
        );
    }
}

#[test]
fn camera_set_rejects_cam7_not_yet_built() {
    // #593: cam7 was NEVER built -- the user only expressed FUTURE interest in a 7th camera, no
    // box was ever connected. It must be rejected as an unknown camera, exactly like any other
    // made-up name, never silently resolved to a phantom IP/source.
    let (ok, _ip, _src) = resolve("cam7");
    assert!(
        !ok,
        "#593: camera_resolve cam7 must FAIL -- cam7 was never built and must not be part of \
         the active fleet"
    );
}

#[test]
fn camera_set_reject_message_lists_six_cameras_not_seven() {
    // The reject message must stay in sync with the real accepted set, or a typo report
    // misleads whoever reads it about which names are actually valid. #593: cam7 is not real.
    let s = read("scripts/camera-set.sh");
    assert!(
        s.contains("expected one of: cam1 cam2 cam3 cam4 cam5 cam6"),
        "#593: the unknown-camera reject message must list the six real cameras (cam1..cam6)."
    );
    assert!(
        !s.contains("expected one of: cam1 cam2 cam3 cam4 cam5 cam6 cam7"),
        "#593: the unknown-camera reject message must NOT list cam7 -- it was never built."
    );
}

#[test]
fn camera_set_default_includes_six_cameras_not_cam7() {
    // CAMERA_SET is the "drive the whole set" default the fleet-wide orchestrators
    // (deploy-fleet.sh, upgrade-fleet-ndi.sh) fall back to when the operator doesn't override
    // it. #593: cam7 was never built, so it must not appear in the default active set.
    let s = read("scripts/camera-set.sh");
    assert!(
        s.contains("CAMERA_SET=\"${CAMERA_SET:-cam1 cam2 cam3 cam4 cam5 cam6}\""),
        "#593: CAMERA_SET default must list exactly the six real cameras (cam1..cam6), no cam7."
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

// --- #528: per-cam HDMI cameraman preview source table -----------------------------------------

/// Source `camera-set.sh`, run `camera_resolve <name>`, and return the resolved
/// `CAMERA_DISPLAY_SOURCE` value (empty string for a box with no configured preview, or on
/// reject). Uses `set -u` deliberately — an unset (not merely empty) var is a real bug, and
/// referencing it under `-u` fails loud rather than silently printing nothing.
fn resolve_display_source(cam: &str) -> (bool, String) {
    let script = manifest_dir().join("scripts/camera-set.sh");
    let harness = r#"
set -uo pipefail
. "$SCRIPT"
if camera_resolve "$CAM" 2>/dev/null; then
  printf 'OK\n'
  printf 'DISPLAY\t%s\n' "$CAMERA_DISPLAY_SOURCE"
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
    let ok = out.status.success() && stdout.lines().next() == Some("OK");
    let display = stdout
        .lines()
        .find_map(|l| l.strip_prefix("DISPLAY\t"))
        .unwrap_or_default()
        .to_string();
    (ok, display)
}

#[test]
fn camera_resolve_wires_cam1_to_the_interkom_return_monitor() {
    // #528: cam1 had NO functional HDMI cameraman preview because setup-device.sh never wired a
    // `--display` source at all. The fix is a per-cam table entry (not a free-text SSH edit) so a
    // re-provision keeps the preview. cam1 gets the same interkom/return-monitor class cam2's
    // live (manual, ExecStart-baked) preview already uses.
    let (ok, display) = resolve_display_source("cam1");
    assert!(ok, "camera_resolve cam1 should succeed");
    assert_eq!(
        display, "STRIH-SNV (interkom)",
        "camera_resolve cam1 must resolve CAMERA_DISPLAY_SOURCE to the interkom/return monitor \
         (#528); got '{display}'"
    );
}

#[test]
fn camera_resolve_leaves_cam2_through_cam6_with_no_display_source() {
    // A box with no CAMERA_DISPLAY_SOURCE table entry must resolve to an EMPTY (never unset —
    // `set -u` would trip on unset) value — empty means "no provisioner-persistent preview for
    // this box today"; callers never need to distinguish "no entry" from "entry that is empty".
    //
    // cam2 is DELIBERATELY here, not with cam1 above: its live box already has the SAME interkom
    // preview, but only as a manual `--display` flag baked into ExecStart — table-driving it via
    // config.toml would make a future re-provision silently break scripts/rig-mode.sh's
    // ExecStart-flag-based TEST/EVENT display toggle (used by the QR-painter E2E harness). See the
    // comment on cam2's case arm in scripts/camera-set.sh for the full mechanism.
    //
    // #593: cam7 was never built and is intentionally NOT in this fleet sweep -- it must be
    // REJECTED (see camera_set_rejects_cam7_not_yet_built), not resolve to an empty display.
    for cam in ["cam2", "cam3", "cam4", "cam5", "cam6"] {
        let (ok, display) = resolve_display_source(cam);
        assert!(ok, "camera_resolve {cam} should succeed");
        assert_eq!(
            display, "",
            "camera_resolve {cam} must resolve CAMERA_DISPLAY_SOURCE to empty (no table entry, \
             #528); got '{display}'"
        );
    }
}

// --- #562: per-cam ExecStart-mechanism HDMI preview source table --------------------------------
//
// cam2's interkom preview lives in a manual `--display "STRIH-SNV (interkom)"` edit baked into
// ExecStart (never config.toml -- see camera_resolve_leaves_cam2_through_cam6_with_no_display_source
// above for why). CAMERA_DISPLAY_SOURCE deliberately stays empty for cam2 forever; a NEW, SEPARATE
// table entry -- CAMERA_DISPLAY_EXECSTART_SOURCE -- makes cam2's ExecStart mechanism provisioner-
// persistent (scripts/setup-device.sh STEP 7) without touching config.toml or rig-mode.sh's
// ExecStart-flag-based TEST/EVENT toggle at all (mechanism (a), issuecomment-4898996033).

/// Source `camera-set.sh`, run `camera_resolve <name>`, and return the resolved
/// `CAMERA_DISPLAY_EXECSTART_SOURCE` value (empty string for a box with no ExecStart-mechanism
/// entry, or on reject). Uses `set -u` deliberately, same as resolve_display_source above.
fn resolve_execstart_display_source(cam: &str) -> (bool, String) {
    let script = manifest_dir().join("scripts/camera-set.sh");
    let harness = r#"
set -uo pipefail
. "$SCRIPT"
if camera_resolve "$CAM" 2>/dev/null; then
  printf 'OK\n'
  printf 'DISPLAY\t%s\n' "$CAMERA_DISPLAY_EXECSTART_SOURCE"
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
    let ok = out.status.success() && stdout.lines().next() == Some("OK");
    let display = stdout
        .lines()
        .find_map(|l| l.strip_prefix("DISPLAY\t"))
        .unwrap_or_default()
        .to_string();
    (ok, display)
}

#[test]
fn camera_resolve_wires_cam2_to_the_execstart_interkom_preview() {
    // #562: cam2's live box already runs with `--display "STRIH-SNV (interkom)"` baked into
    // ExecStart as a manual edit. This table entry is what lets setup-device.sh STEP 7
    // re-provision cam2 without silently dropping it (the #379 recurrence risk).
    let (ok, display) = resolve_execstart_display_source("cam2");
    assert!(ok, "camera_resolve cam2 should succeed");
    assert_eq!(
        display, "STRIH-SNV (interkom)",
        "camera_resolve cam2 must resolve CAMERA_DISPLAY_EXECSTART_SOURCE to the interkom/return \
         monitor (#562); got '{display}'"
    );
}

#[test]
fn camera_resolve_leaves_cam1_and_cam3_through_cam6_with_no_execstart_display_source() {
    // cam1 already gets its preview through the config.toml [display] mechanism
    // (CAMERA_DISPLAY_SOURCE) -- it must NOT also get an ExecStart-baked flag (that would mean two
    // independent, driftable sources of the same preview). cam3-6 have no preview at all today.
    // #593: cam7 was never built and is intentionally excluded from this fleet sweep.
    for cam in ["cam1", "cam3", "cam4", "cam5", "cam6"] {
        let (ok, display) = resolve_execstart_display_source(cam);
        assert!(ok, "camera_resolve {cam} should succeed");
        assert_eq!(
            display, "",
            "camera_resolve {cam} must resolve CAMERA_DISPLAY_EXECSTART_SOURCE to empty (#562); \
             got '{display}'"
        );
    }
}

/// #562-review: nothing in `camera_resolve()`'s `case` statement mechanically PREVENTS a future
/// table edit from filling in BOTH `CAMERA_DISPLAY_SOURCE` and `CAMERA_DISPLAY_EXECSTART_SOURCE`
/// for the same camera -- it's comment-only discipline today. That would be a real, silent bug:
/// `src/main.rs`'s CLI-overrides-config precedence means the ExecStart flag would win, config.toml's
/// `[display]` section would sit inert, and `verify-device.sh`'s two INDEPENDENT (p) checks would
/// both report "ok" even though only one mechanism is actually active. This sweep pins the
/// invariant across the whole real fleet so a future accidental double-entry fails a test instead
/// of shipping silently.
#[test]
fn camera_resolve_never_configures_both_display_mechanisms_for_the_same_camera() {
    // #593: cam7 excluded -- it was never built and must reject, not resolve either mechanism.
    for cam in ["cam1", "cam2", "cam3", "cam4", "cam5", "cam6"] {
        let (ok1, config_toml_source) = resolve_display_source(cam);
        let (ok2, execstart_source) = resolve_execstart_display_source(cam);
        assert!(ok1 && ok2, "camera_resolve {cam} should succeed");
        assert!(
            config_toml_source.is_empty() || execstart_source.is_empty(),
            "camera_resolve {cam} has BOTH CAMERA_DISPLAY_SOURCE ('{config_toml_source}') AND \
             CAMERA_DISPLAY_EXECSTART_SOURCE ('{execstart_source}') non-empty -- exactly ONE HDMI-\
             preview mechanism must be configured per camera (#562), never both"
        );
    }
}
