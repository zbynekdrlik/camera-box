//! issue 1346 -- strih-lx provisioning + acceptance for the DRM-lease HDMI output (owner
//! ROZHODNUTE 24.9.2026, design comment 5810067589): the HDMI output is the issue-1152 in-OBS
//! DRM-lease output with a Program / Multiview view, NEVER an OBS projector window and NEVER the
//! desktop.
//!
//! Functional tests of the pure helpers in `scripts/lib/strih-drm-output.sh` (sourced in a bash
//! harness -- no root, no X, no rig) plus static wiring anchors in setup-strih.sh /
//! verify-strih.sh / strih-obs-start.sh. std-only, runs offline:
//! `CARGO_MANIFEST_DIR=<abs> rustc --test --edition 2021 tests/strih_drm_output_provision_1346.rs -o /tmp/t && /tmp/t`.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("issue 1346: read {}: {e}", p.display()))
}

/// Source the lib and run `body` (optional `stdin`). Returns (exit code, stdout, stderr).
fn run(body: &str, stdin: Option<&str>) -> (i32, String, String) {
    use std::io::Write;
    let lib = root().join("scripts/lib/strih-drm-output.sh");
    assert!(lib.exists(), "issue 1346: {} must exist", lib.display());
    let harness = format!("set -euo pipefail\n. \"$LIB\"\n{body}");
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", &lib)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn bash");
    {
        let mut si = child.stdin.take().unwrap();
        si.write_all(stdin.unwrap_or("").as_bytes()).unwrap();
    }
    let out = child.wait_with_output().expect("bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("strih_drm_1346_{tag}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn lib_is_source_only() {
    let (c, out, err) = run("type strih_drm_output_verdict >/dev/null", None);
    assert_eq!(
        c, 0,
        "sourcing must succeed with no side effects; stderr={err}"
    );
    assert!(out.is_empty(), "sourcing must print nothing: {out}");
}

#[test]
fn hdmi_connected_reads_the_kernel_connector_status() {
    let d = scratch("sysfs");
    for (name, st) in [
        ("card1-eDP-1", "connected"),
        ("card1-HDMI-A-1", "disconnected"),
    ] {
        fs::create_dir_all(d.join(name)).unwrap();
        fs::write(d.join(name).join("status"), format!("{st}\n")).unwrap();
    }
    let dir = d.to_str().unwrap();
    let (c, _o, _e) = run(&format!("strih_drm_hdmi_connected '{dir}'"), None);
    assert_ne!(
        c, 0,
        "eDP connected + HDMI disconnected -> NOT connected (today's strih-lx)"
    );

    fs::write(d.join("card1-HDMI-A-1").join("status"), "connected\n").unwrap();
    let (c2, _o, _e) = run(&format!("strih_drm_hdmi_connected '{dir}'"), None);
    assert_eq!(c2, 0, "a connected HDMI-A kernel connector -> connected");

    let empty = scratch("sysfs_empty");
    let (c3, _o, _e) = run(
        &format!("strih_drm_hdmi_connected '{}'", empty.display()),
        None,
    );
    assert_ne!(
        c3, 0,
        "no connectors at all -> not connected (never a false positive)"
    );
    let _ = fs::remove_dir_all(&d);
    let _ = fs::remove_dir_all(&empty);
}

#[test]
fn hdmi_output_name_comes_from_xrandr_not_the_kernel_name() {
    // NVIDIA-primary strih-lx names its HDMI port "HDMI-0"; modesetting names HDMI-A-1 "HDMI-1".
    let q = "Screen 0: minimum 8 x 8, current 1920 x 1080\n\
             eDP-1-1 connected primary 1920x1080+0+0 (normal left inverted right x axis y axis) 344mm x 194mm\n\
             DP-0 disconnected (normal left inverted right x axis y axis)\n\
             HDMI-0 connected 1920x1080+1920+0 (normal left inverted right x axis y axis) 520mm x 290mm\n\
             HDMI-1 connected (normal left inverted right x axis y axis)\n";
    let (c, out, _e) = run("strih_drm_hdmi_output_from_xrandr", Some(q));
    assert_eq!(c, 0);
    assert_eq!(
        out.trim(),
        "HDMI-0",
        "the FIRST connected HDMI output, by its X RandR name"
    );

    let none = "eDP-1 connected primary 1920x1080+0+0\nHDMI-1 disconnected (normal)\n";
    let (c2, out2, _e) = run("strih_drm_hdmi_output_from_xrandr", Some(none));
    assert_eq!(
        c2, 0,
        "no match is not an error (the caller SKIPs on an empty name)"
    );
    assert_eq!(out2.trim(), "");

    // `HDMI-0 disconnected` must never be read as connected by a substring match.
    let tricky = "HDMI-0 disconnected (normal)\nDP-1 connected 1920x1080+0+0\n";
    let (_c3, out3, _e) = run("strih_drm_hdmi_output_from_xrandr", Some(tricky));
    assert_eq!(out3.trim(), "", "a disconnected HDMI is not a candidate");
}

#[test]
fn config_json_is_the_one_line_c_contract() {
    let (c, out, _e) = run("strih_drm_output_config_json HDMI-0 multiview", None);
    assert_eq!(c, 0);
    assert_eq!(
        out,
        "{\"enabled\":true,\"connector\":\"HDMI-0\",\"argb\":2105376,\"view\":\"multiview\"}\n",
        "ONE line, explicit non-empty connector (obs-drm-output.md: a CONTRACT, not a style)"
    );
    let (c2, out2, _e) = run("strih_drm_output_config_json HDMI-1 program", None);
    assert_eq!(c2, 0);
    assert!(out2.contains("\"view\":\"program\""), "{out2}");
    let (c3, _o, _e) = run("strih_drm_output_config_json HDMI-0", None);
    assert_eq!(c3, 0, "the default view is multiview (owner ROZHODNUTE)");
    for bad in [
        "strih_drm_output_config_json '' multiview",
        "strih_drm_output_config_json 'HDMI 0' multiview",
        "strih_drm_output_config_json 'HDMI-0\"' multiview",
        "strih_drm_output_config_json HDMI-0 preview",
    ] {
        let (cb, ob, _e) = run(bad, None);
        assert_ne!(
            cb, 0,
            "`{bad}` must refuse, never write a config the C would ignore"
        );
        assert!(ob.is_empty(), "`{bad}` must print nothing: {ob}");
    }
}

#[test]
fn legacy_projector_type_carries_over_as_the_initial_view() {
    for (text, want) in [
        ("{\"type\":\"program\"}", "program"),
        ("{\"type\": \"program\"}\n", "program"),
        ("{\"type\":\"multiview\"}", "multiview"),
        ("", "multiview"),
        ("garbage", "multiview"),
    ] {
        let (c, out, _e) = run(&format!("strih_drm_legacy_view '{text}'"), None);
        assert_eq!(c, 0);
        assert_eq!(out, want, "legacy `{text}` -> {want}");
    }
}

/// (hdmi_connected, armed_connector, view, live_scanout, live_multiview) -> (token, rc).
/// rc 0 = PASS, 2 = NOTE (skip / report), 1 = FAIL.
#[test]
fn verdict_grades_the_config_and_the_lease_live_line() {
    let cases: &[(&str, &str, i32)] = &[
        ("0 - program 0 0", "skip-no-hdmi", 2),
        ("0 - multiview 1 1", "skip-no-hdmi", 2),
        ("0 HDMI-0 multiview 0 0", "hdmi-unplugged", 2),
        ("1 - program 0 0", "config-missing", 1),
        ("1 HDMI-0 unknown 1 0", "view-invalid", 1),
        ("1 HDMI-0 program 0 0", "lease-not-live", 1),
        ("1 HDMI-0 multiview 0 1", "lease-not-live", 1),
        ("1 HDMI-0 multiview 1 0", "multiview-not-live", 1),
        ("1 HDMI-0 multiview 1 1", "ok", 0),
        ("1 HDMI-0 program 1 0", "ok", 0),
        ("1 HDMI-0 program 1 1", "ok", 0),
    ];
    for (args, token, rc) in cases {
        let (c, out, _e) = run(&format!("strih_drm_output_verdict {args} || exit $?"), None);
        assert_eq!(out, *token, "args `{args}`");
        assert_eq!(c, *rc, "args `{args}` -> rc");
    }
    let (c, out, _e) = run("strih_drm_output_verdict || exit $?", None);
    assert_eq!(
        (c, out.as_str()),
        (2, "skip-no-hdmi"),
        "missing args = nothing connected"
    );
}

// ----------------------------------------------------------------------------------------------
// Wiring anchors
// ----------------------------------------------------------------------------------------------

#[test]
fn setup_strih_provisions_drm_output_only_with_hdmi_and_retires_the_projector_json() {
    let s = read("scripts/setup-strih.sh");
    assert!(
        s.contains(". \"${HERE}/lib/strih-drm-output.sh\""),
        "setup-strih must source the drm-output lib"
    );
    assert!(
        !s.contains("> /opt/camera-box/strih-lx-projector.json"),
        "the retired HDMI projector config must no longer be written"
    );
    assert!(
        s.contains("rm -f /opt/camera-box/strih-lx-projector.json"),
        "a leftover strih-lx-projector.json is removed (its type carried over as the initial view)"
    );
    for token in [
        "strih_drm_hdmi_connected",
        "strih_drm_hdmi_output_from_xrandr",
        "strih_drm_output_config_json",
        "SKIP issue 1346",
        ".camera-box/drm-output.json",
    ] {
        assert!(s.contains(token), "setup-strih must contain `{token}`");
    }
}

#[test]
fn verify_strih_grades_drm_output_and_skips_without_hdmi() {
    let v = read("scripts/verify-strih.sh");
    assert!(
        v.contains(". \"${HERE}/lib/strih-drm-output.sh\""),
        "verify must source the lib"
    );
    assert!(
        !v.contains("strih_projector_verdict"),
        "the projector-window verdict is retired"
    );
    assert!(
        v.contains("strih_drm_output_verdict"),
        "verify grades the drm-output state"
    );
    assert!(
        v.contains("drm-output: program scanout LIVE")
            && v.contains("drm-output: multiview bind LIVE"),
        "verify reads the lease-live log lines"
    );
    assert!(
        v.contains("skip-no-hdmi)"),
        "a SKIP branch when no HDMI connector is connected"
    );
    let lib = read("scripts/lib/strih-provision.sh");
    assert!(
        !lib.contains("strih_projector_verdict()"),
        "strih-provision.sh must drop the retired projector verdict"
    );
}

#[test]
fn strih_obs_start_takes_the_armed_connector_out_of_x_before_the_launch() {
    let w = read("scripts/strih-obs-start.sh");
    let classify = w
        .find("strih_scenes.drm_output_lease_connector(")
        .expect("the wrapper must classify the config with the ONE Python grammar");
    let off = w
        .find("xrandr --output \"$DRM_CONNECTOR\" --off")
        .expect("the wrapper must take the armed connector out of the X layout");
    let launch = w.find("OBS_PID=$!").expect("launch line");
    assert!(
        classify < off && off < launch,
        "classify -> xrandr --off -> launch (the idle-connector lease precondition)"
    );
    assert!(
        !w.contains("NO DRM lease"),
        "the stale 'NO DRM lease' header claim must go"
    );
}

#[test]
fn kiosk_autostart_never_extends_the_desktop_onto_hdmi() {
    let lib = read("scripts/lib/strih-provision.sh");
    let start = lib
        .find("strih_openbox_autostart_text() {")
        .expect("autostart generator");
    let body = &lib[start..];
    let end = body.find("\n}\n").expect("generator end");
    let body = &body[..end];
    assert!(
        body.contains("xrandr --output \"$PROJ\" --off"),
        "the kiosk must turn HDMI OFF in X: the HDMI output is the DRM lease, never the desktop"
    );
    assert!(
        !body.contains("--right-of"),
        "the desktop must never extend onto HDMI"
    );
}
