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

// --- #827 (2026-07-27): cam5/cam6/cam7 RETIRED from the ACTIVE fleet -- but REVERSIBLY --------
// Binding owner directive (posted on #827): retiring cam5/cam6/cam7 must be a ONE-LINE reversal
// when the boxes come back (grabber cards returned to their owner today). So camera_resolve() /
// camera_strih_route() KEEP resolving all seven cameras fully (facts never deleted); retirement
// is expressed ONLY via `CAMERA_ACTIVE_SET` -- the ONE declared list every fleet-wide consumer
// derives its working set from. These tests pin BOTH directions: the default is exactly cam1-4,
// AND overriding CAMERA_ACTIVE_SET to include a retired camera flows through to the derived
// consumer (camera_active_secondary_set) -- the proof the reversal actually works, not just a
// comment claiming it does.

#[test]
fn camera_set_still_resolves_cam5_cam6_cam7_facts() {
    // #827: retiring a camera from the ACTIVE fleet must NEVER delete its resolvable facts (IP,
    // NDI source, genlock fps) -- camera_resolve is a FACT lookup, not a policy decision. This is
    // what makes reactivation a one-line CAMERA_ACTIVE_SET edit instead of archaeology through a
    // deleted diff.
    let expected = [
        ("cam5", "10.77.9.65", "CAM5 (usb)"),
        ("cam6", "10.77.9.66", "CAM6 (usb)"),
        ("cam7", "10.77.9.67", "CAM7 (usb)"),
    ];
    for (name, ip, source) in expected {
        let (ok, got_ip, got_src) = resolve(name);
        assert!(
            ok,
            "#827: camera_resolve {name} must still succeed -- retirement is an ACTIVE-SET \
             membership question, never a deleted fact"
        );
        assert_eq!(got_ip, ip, "camera_resolve {name} resolved the wrong IP");
        assert_eq!(
            got_src, source,
            "camera_resolve {name} resolved the wrong NDI source"
        );
    }
}

#[test]
fn camera_set_reject_message_still_lists_all_seven_cameras() {
    // The reject message reflects what camera_resolve can RESOLVE (a fact lookup), not what is
    // currently ACTIVE -- #827 kept all seven names resolvable, so the message must too.
    let s = read("scripts/camera-set.sh");
    assert!(
        s.contains("expected one of: cam1 cam2 cam3 cam4 cam5 cam6 cam7"),
        "#827: the unknown-camera reject message must still list all seven resolvable cameras."
    );
}

#[test]
fn camera_active_set_default_is_exactly_cam3_1170() {
    // CAMERA_ACTIVE_SET is the ONE declared list of cameras physically installed + MEASURED TODAY.
    // issue 1170 (2026-08-24): cam2's camera-under-test participation is RETIRED -- its ShadowCast
    // grabber (capturing imag-nb's HDMI, issue 781) has a cure-decay collapsed to ~7min (issue 1193),
    // so its capture leg cannot survive a 40-min run. cam2 stays the fixed PAINTER (keyed off
    // PAINTER_IP, not this set) but is no longer a measured camera; the source moves to cam3
    // automatically (camera_source_box, #1134). cam1 re-retired (grabber hw defect, #1110);
    // cam4/cam5/cam6/cam7 out as before. Membership-retired: every retired cam's facts stay
    // resolvable below. RE-ENABLE cam2: add it back to CAMERA_ACTIVE_SET (issue 1198 card swap).
    let s = read("scripts/camera-set.sh");
    assert!(
        s.contains("CAMERA_ACTIVE_SET=\"${CAMERA_ACTIVE_SET:-cam3}\""),
        "issue 1170: CAMERA_ACTIVE_SET default must be exactly cam3 (the sole measured camera) -- \
         cam2 camera-under-test retired (grabber cure-decay), cam1 re-retired, cam4/5/6/7 out."
    );
}

#[test]
fn camera_set_cam3_reactivated_939_resolves_and_is_active_by_default() {
    // #898 retired cam3 (grabber destroyed 2026-07-31); #939 re-activated it (Cam Link 4K
    // fitted, 2026-08-13). The facts (IP, NDI source, strih route) were never deleted -- this
    // test now pins the completed round trip: cam3 resolves fully AND is active by default.
    let (ok, ip, src) = resolve("cam3");
    assert!(
        ok,
        "#898: camera_resolve cam3 must still succeed -- retirement is active-set membership \
         only, never a deleted fact"
    );
    assert_eq!(
        ip, "10.77.9.63",
        "camera_resolve cam3 resolved the wrong IP"
    );
    assert_eq!(
        src, "CAM3 (usb)",
        "camera_resolve cam3 resolved the wrong NDI source"
    );

    let (ok, scene, source) = resolve_strih_route("cam3");
    assert!(ok, "#898: camera_strih_route cam3 must still succeed");
    assert_eq!(
        scene, "Cam 3",
        "camera_strih_route cam3 resolved the wrong strih scene"
    );
    assert_eq!(
        source, "NDI cam3",
        "camera_strih_route cam3 resolved the wrong strih NDI-input source"
    );

    assert!(
        is_active("cam3", None),
        "#939: cam3 IS active by default again (Cam Link 4K card fitted, re-activated \
         2026-08-13) -- the reversible retirement design completing its round trip"
    );
    assert!(
        is_active("cam3", Some("cam1 cam2 cam3 cam4")),
        "#898: cam3 must become active again once re-added to CAMERA_ACTIVE_SET (replacement \
         card fitted) -- the whole point of the reversible active-set design"
    );
}

#[test]
fn camera_set_default_derives_from_camera_active_set_not_a_second_list() {
    // #827: CAMERA_SET (the deploy-fleet.sh/verify-fleet.sh/upgrade-fleet-ndi.sh fallback) must
    // DERIVE from CAMERA_ACTIVE_SET -- never independently enumerate the fleet a second time.
    let s = read("scripts/camera-set.sh");
    assert!(
        s.contains("CAMERA_SET=\"${CAMERA_SET:-$CAMERA_ACTIVE_SET}\""),
        "#827: CAMERA_SET must default to $CAMERA_ACTIVE_SET, not a second hardcoded cam list."
    );
}

/// Source `camera-set.sh` with an optional `CAMERA_ACTIVE_SET` override and run
/// `camera_active_secondary_set`, returning its stdout (space-separated cam names).
fn active_secondary_set(active_set_override: Option<&str>) -> String {
    let script = manifest_dir().join("scripts/camera-set.sh");
    let harness = r#"
set -uo pipefail
. "$SCRIPT"
camera_active_secondary_set
"#;
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(harness).env("SCRIPT", &script);
    if let Some(v) = active_set_override {
        cmd.env("CAMERA_ACTIVE_SET", v);
    }
    let out = cmd.output().expect("failed to run bash harness");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn camera_active_secondary_set_is_empty_for_the_default_source_plus_painter_1134() {
    // #1110: the default active set is "cam2 cam3" (cam1 re-retired 2026-08-22, grabber hw defect)
    // -- cam3 is the DERIVED source (camera_source_box), cam2 the painter, so NO secondary camera
    // is left for the ALL_CAMBOX sweep to cut in. The secondary set now excludes the DERIVED
    // source (cam3), not the literal cam1, and yields an empty string rather than wrongly
    // including cam3 (the source) or falling back to a literal.
    assert_eq!(
        active_secondary_set(None),
        "",
        "#1110: with source=cam3 (derived) and cam2=painter, the default active set has no \
         secondary camera -- the derived set must be empty"
    );
}

#[test]
fn camera_active_set_env_override_reactivates_a_retired_camera() {
    // THE PROOF the reversal actually works (owner directive on #827: a comment saying "just add
    // it back" is not proof) -- overriding CAMERA_ACTIVE_SET to include a RETIRED camera (cam5)
    // must flow through to the derived secondary set, with ZERO code changes beyond the env var.
    assert_eq!(
        active_secondary_set(Some("cam1 cam2 cam3 cam4 cam5")),
        "cam3 cam4 cam5",
        "#827: adding a retired camera back to CAMERA_ACTIVE_SET must make it appear in the \
         derived secondary set -- this is the whole point of the active-set design"
    );
    // And removing one back out un-reactivates it, just as easily.
    assert_eq!(
        active_secondary_set(Some("cam1 cam2 cam3")),
        "cam3",
        "#827: shrinking CAMERA_ACTIVE_SET must shrink the derived secondary set the same way"
    );
}

/// Source `camera-set.sh` with an optional `CAMERA_ACTIVE_SET` override and run
/// `camera_is_active <name>`, returning true/false.
fn is_active(name: &str, active_set_override: Option<&str>) -> bool {
    let script = manifest_dir().join("scripts/camera-set.sh");
    let harness = r#"
set -uo pipefail
. "$SCRIPT"
camera_is_active "$NAME" && echo YES || echo NO
"#;
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(harness)
        .env("SCRIPT", &script)
        .env("NAME", name);
    if let Some(v) = active_set_override {
        cmd.env("CAMERA_ACTIVE_SET", v);
    }
    let out = cmd.output().expect("failed to run bash harness");
    String::from_utf8_lossy(&out.stdout).trim() == "YES"
}

#[test]
fn camera_is_active_matches_whole_words_only() {
    // #827: a substring match would wrongly treat "cam1" as active just because "cam10" (a
    // hypothetical future name) appears in the set -- must be an exact word match.
    assert!(
        is_active("cam3", None),
        "cam3 must be active by default (the source, #1134)"
    );
    assert!(
        !is_active("cam1", None),
        "cam1 must NOT be active by default (re-retired 2026-08-22, issue 1110 -- grabber hw defect)"
    );
    assert!(
        !is_active("cam5", None),
        "cam5 must NOT be active by default (retired)"
    );
    assert!(
        is_active("cam5", Some("cam1 cam2 cam3 cam4 cam5")),
        "cam5 must become active once added to CAMERA_ACTIVE_SET"
    );
    assert!(
        !is_active("cam1", Some("cam10 cam2")),
        "camera_is_active must not substring-match 'cam1' inside 'cam10'"
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
    for cam in ["cam1", "cam2", "cam3", "cam4", "cam5", "cam6", "cam7"] {
        let fps = resolve_genlock_fps(cam);
        assert_eq!(
            fps, "60",
            "camera_resolve {cam} must set CAMERA_GENLOCK_FPS=60 (#451); got '{fps}'"
        );
    }
}

// --- #24 item 1 / #312: camera_strih_route() -- which strih OBS scene shows a given
// physical camera, so scripts/recording-e2e.sh can drive cam1, cam3, cam4, cam5, cam6, OR cam7
// as the dedicated SOURCE camera (the box filming cam2's monitor + carrying the #174 capture
// burn) instead of being hard-coded to cam1. #827 (2026-07-27): cam5/cam6/cam7 are RETIRED from
// CAMERA_ACTIVE_SET but their strih routes stay fully resolvable here (facts, not policy). -------

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
fn camera_strih_route_resolves_the_six_source_eligible_cameras() {
    // #753 PIVOT (2026-07-14, binding user directive): the mapping scripts/set-ndi-mapping.py
    // programs onto strih is now 1:1 (NDI cam<N> -> CAM<N> (usb) for every N) -- the pre-pivot
    // offset table (NDI cam5->CAM1, NDI cam1->CAM3, NDI cam3->CAM4, NDI cam4->CAM5) is HISTORY.
    // #827 (2026-07-27): cam5/cam6/cam7 are RETIRED from CAMERA_ACTIVE_SET, but camera_strih_route
    // stays a pure FACT lookup -- it must still resolve them exactly, since recording-e2e.sh's
    // single-node CAM= selection is orthogonal to the active-set gate (a one-off manual
    // reactivation test needs the route to still exist). A wrong scene/source would route strih's
    // PROGRAM to the WRONG box's NDI feed and silently certify nothing (or the wrong camera).
    let expected = [
        ("cam1", "Cam 1", "NDI cam1"),
        ("cam3", "Cam 3", "NDI cam3"),
        ("cam4", "Cam 4", "NDI cam4"),
        ("cam5", "Cam 5", "NDI cam5"),
        ("cam6", "Cam 6", "NDI cam6"),
        ("cam7", "Cam 7", "NDI cam7"),
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

// --- #827 follow-up (2026-07-28): the `[0/8]`/`[1/8]`/`[5/8 pre]` recording-e2e.sh preflight
// loops still enumerated the fleet via a LITERAL `for _n in 1 2 3 4 5 6 7` range and only
// subtracted `PREFLIGHT_EXCLUDED_CAMS` (the TEMPORARY acked-offline list) -- never intersecting
// with `CAMERA_ACTIVE_SET` (the PERMANENT retired-fleet list). A live hardware gate run
// (30310110884) proved this: the frozen-camera-gate preflight still sampled "NDI cam5"/"NDI cam6"/
// "NDI cam7" and failed FROZEN on all three, even though they are retired and correctly excluded
// everywhere else. These two new helpers are the single derivation point all three call sites now
// use -- fixture-driven, no rig needed. -------------------------------------------------------

/// Source `camera-set.sh` with an optional `CAMERA_ACTIVE_SET` override, run
/// `camera_active_excluding <excluded>`, and return its stdout (space-separated cam names).
fn active_excluding(active_set_override: Option<&str>, excluded: &str) -> String {
    let script = manifest_dir().join("scripts/camera-set.sh");
    let harness = r#"
set -uo pipefail
. "$SCRIPT"
camera_active_excluding "$EXCLUDED"
"#;
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(harness)
        .env("SCRIPT", &script)
        .env("EXCLUDED", excluded);
    if let Some(v) = active_set_override {
        cmd.env("CAMERA_ACTIVE_SET", v);
    }
    let out = cmd.output().expect("failed to run bash harness");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn camera_active_excluding_never_includes_a_retired_camera_even_with_empty_exclusion() {
    // #827/#898 follow-up, #939 update: cam5/cam6/cam7 stay retired and cam4 stays out (issue
    // 947), while cam3 is back in the default set since 2026-08-13 -- the derived list must
    // reflect exactly that, regardless of what (if anything) is passed as excluded.
    assert_eq!(
        active_excluding(None, ""),
        "cam2 cam3",
        "issue 1110: camera_active_excluding with no exclusion must return exactly the active set \
         (cam2 painter + cam3 source; cam1 re-retired 2026-08-22 grabber hw defect, cam4/cam5/cam6/cam7 out)"
    );
}

#[test]
fn camera_active_excluding_subtracts_the_acked_offline_list_within_the_active_set() {
    // cam4 acked-offline (e.g. grabber card removed) must drop out, but only cam4 -- the other
    // active cameras stay, and no retired camera ever appears.
    assert_eq!(
        active_excluding(None, "cam3"),
        "cam2",
        "camera_active_excluding must drop an acked-offline camera that IS in the active set \
         (excluding cam3 from the cam2 cam3 default leaves only cam2) -- honest to the test name"
    );
}

#[test]
fn camera_active_excluding_reactivation_flows_through_env_override() {
    // THE reversibility proof for this call path specifically (mirrors
    // camera_active_set_env_override_reactivates_a_retired_camera above): re-adding cam5 to
    // CAMERA_ACTIVE_SET must make it appear here too, with zero code changes.
    assert_eq!(
        active_excluding(Some("cam1 cam2 cam3 cam4 cam5"), ""),
        "cam1 cam2 cam3 cam4 cam5",
        "#827: a reactivated camera must flow through camera_active_excluding"
    );
}

/// Source `camera-set.sh` with an optional `CAMERA_ACTIVE_SET` override, run
/// `camera_active_ndi_sources_excluding_csv <excluded>`, and return its stdout.
fn active_ndi_sources_excluding_csv(active_set_override: Option<&str>, excluded: &str) -> String {
    let script = manifest_dir().join("scripts/camera-set.sh");
    let harness = r#"
set -uo pipefail
. "$SCRIPT"
camera_active_ndi_sources_excluding_csv "$EXCLUDED"
"#;
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(harness)
        .env("SCRIPT", &script)
        .env("EXCLUDED", excluded);
    if let Some(v) = active_set_override {
        cmd.env("CAMERA_ACTIVE_SET", v);
    }
    let out = cmd.output().expect("failed to run bash harness");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn camera_active_ndi_sources_excluding_csv_never_includes_a_retired_camera() {
    // This is the EXACT property that failed live on run 30310110884: a retired camera whose
    // strih OBS input is STILL PRESENT (NDI cam5/cam6/cam7 scene-collection entries were never
    // deleted, #827) must not appear in the sampled/checked source list -- with NO exclusion
    // passed at all, since retirement (not acking) is what keeps them out. #898 (2026-07-31):
    // cam3 joins the retired set (grabber card destroyed). issue 947 (2026-08-02): cam4 joins it
    // too (grabber wedges the capture leg within minutes of every start) -- and cam4 is exactly
    // the case this property protects, because its strih input "NDI cam4" is still present and
    // still sampled by the [1/8] frozen-camera preflight if the derivation leaks it.
    let csv = active_ndi_sources_excluding_csv(None, "");
    assert_eq!(
        csv, "NDI cam2,NDI cam3",
        "issue 1110: cam1 re-retired -- the derived NDI source CSV carries exactly the active set \
         (cam2, cam3); a retired camera (cam1/cam4/cam5/cam6/cam7) must never appear regardless \
         of whether its strih OBS input still exists"
    );
    for retired in ["NDI cam1", "NDI cam4", "NDI cam5", "NDI cam6", "NDI cam7"] {
        assert!(
            !csv.contains(retired),
            "{retired} must not appear in the derived source list -- it is retired from \
             CAMERA_ACTIVE_SET (#827/#898)"
        );
    }
}

#[test]
fn camera_active_ndi_sources_excluding_csv_also_drops_acked_offline_within_active_set() {
    // cam4 acked-offline (e.g. grabber card removed) subtracts from the CSV too -- the SAME
    // PREFLIGHT_EXCLUDED_CAMS mechanism the three recording-e2e.sh call sites already pass
    // through unchanged.
    assert_eq!(
        active_ndi_sources_excluding_csv(None, "cam3"),
        "NDI cam2",
        "an acked-offline camera within the active set must be excluded from the CSV (excluding \
         cam3 from the cam2 cam3 default leaves only cam2)"
    );
}

#[test]
fn camera_active_ndi_sources_excluding_csv_reactivation_flows_through() {
    // Re-enabling a retired camera via CAMERA_ACTIVE_SET must make it appear back in the derived
    // NDI-source CSV too -- the whole point of deriving from the active set instead of a literal
    // range.
    assert_eq!(
        active_ndi_sources_excluding_csv(Some("cam1 cam2 cam3 cam4 cam5"), ""),
        "NDI cam1,NDI cam2,NDI cam3,NDI cam4,NDI cam5",
        "#827: a reactivated camera must flow through camera_active_ndi_sources_excluding_csv"
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
