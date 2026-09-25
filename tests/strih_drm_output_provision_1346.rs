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
    run_full(body, stdin, &[])
}

/// `run` with extra environment variables (fixture text is passed by env, never inlined into bash).
fn run_env(body: &str, envs: &[(&str, &str)]) -> (i32, String, String) {
    run_full(body, None, envs)
}

fn run_full(body: &str, stdin: Option<&str>, envs: &[(&str, &str)]) -> (i32, String, String) {
    use std::io::Write;
    let lib = root().join("scripts/lib/strih-drm-output.sh");
    assert!(lib.exists(), "issue 1346: {} must exist", lib.display());
    let harness = format!("set -euo pipefail\n. \"$LIB\"\n{body}");
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", &lib)
        .envs(envs.iter().copied())
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

/// main design 5840508308: on strih-lx the NVIDIA connector reads `card1-HDMI-A-1 disconnected`
/// in sysfs while X reports `HDMI-0 connected` (the NVIDIA X driver does not drive the KMS
/// connector status). With the box fact `vk-direct` the detector reads the X RandR view instead --
/// connected with a mode, and connected with NO mode/CRTC (how xrandr lists HDMI-0 while vk-direct
/// holds it). The lease backend keeps the sysfs read byte-identical and ignores the xrandr text.
#[test]
fn hdmi_connected_vk_direct_reads_the_x_randr_view() {
    let d = scratch("sysfs_vk");
    for (name, st) in [
        ("card1-eDP-1", "connected"),
        ("card1-HDMI-A-1", "disconnected"),
    ] {
        fs::create_dir_all(d.join(name)).unwrap();
        fs::write(d.join(name).join("status"), format!("{st}\n")).unwrap();
    }
    let dir = d.to_str().unwrap();
    let with_mode = "Screen 0: minimum 8 x 8, current 1920 x 1200\n\
                     eDP-1-1 connected primary 1920x1200+0+0 (normal left inverted right x axis y axis) 344mm x 215mm\n\
                     HDMI-0 connected 1920x1080+1920+0 (normal left inverted right x axis y axis) 520mm x 290mm\n";
    let no_crtc = "Screen 0: minimum 8 x 8, current 1920 x 1200\n\
                   eDP-1-1 connected primary 1920x1200+0+0 (normal left inverted right x axis y axis) 344mm x 215mm\n\
                   HDMI-0 connected (normal left inverted right x axis y axis)\n";
    let unplugged =
        "eDP-1-1 connected primary 1920x1200+0+0 (normal left inverted right x axis y axis)\n\
                     HDMI-0 disconnected (normal left inverted right x axis y axis)\n";
    for (backend, xr, want, why) in [
        (
            "vk-direct",
            with_mode,
            true,
            "X shows HDMI-0 connected with a mode",
        ),
        (
            "vk-direct",
            no_crtc,
            true,
            "vk-direct holds HDMI-0: connected, no mode/CRTC",
        ),
        ("vk-direct", unplugged, false, "X shows HDMI-0 disconnected"),
        (
            "vk-direct",
            "",
            false,
            "no X answer (Xorg :0 not up) is never a false positive",
        ),
        (
            "lease",
            with_mode,
            false,
            "lease keeps the kernel status (disconnected here)",
        ),
    ] {
        let (c, _o, _e) = run_env(
            &format!("strih_drm_hdmi_connected '{dir}' {backend} \"$XR\" || exit $?"),
            &[("XR", xr)],
        );
        assert_eq!(c == 0, want, "{backend}: {why}");
    }

    // The lease backend ignores the xrandr text entirely: kernel connected -> connected, whatever X says.
    fs::write(d.join("card1-HDMI-A-1").join("status"), "connected\n").unwrap();
    let (c, _o, _e) = run_env(
        &format!("strih_drm_hdmi_connected '{dir}' lease \"$XR\" || exit $?"),
        &[("XR", unplugged)],
    );
    assert_eq!(c, 0, "lease: the kernel status wins over the X view");
    let (c2, _o, _e) = run(&format!("strih_drm_hdmi_connected '{dir}'"), None);
    assert_eq!(c2, 0, "no backend argument = the lease read, unchanged");
    let _ = fs::remove_dir_all(&d);
}

/// The X RandR gatherer (`strih_drm_xrandr_query`): queries display :0 with the desktop user's
/// Xauthority and prints nothing (rc 0, never aborting a set -e caller) when xrandr fails.
#[test]
fn xrandr_query_reads_display_zero_and_never_aborts() {
    let d = scratch("fake_xrandr");
    let ok = d.join("ok");
    let bad = d.join("bad");
    for (dir, body) in [
        (
            &ok,
            "#!/bin/sh\necho \"DISPLAY=$DISPLAY XAUTHORITY=$XAUTHORITY args=$*\"\necho 'HDMI-0 connected (normal)'\n",
        ),
        (&bad, "#!/bin/sh\necho 'cannot open display :0' >&2\nexit 1\n"),
    ] {
        fs::create_dir_all(dir).unwrap();
        // a non-root `id` keeps the test on the inline branch even on a root CI runner
        for (name, text) in [("xrandr", body), ("id", "#!/bin/sh\necho 1000\n")] {
            let p = dir.join(name);
            fs::write(&p, text).unwrap();
            Command::new("chmod").arg("+x").arg(&p).status().unwrap();
        }
    }
    let (c, out, _e) = run_env(
        "PATH=\"$FAKE:$PATH\"; strih_drm_xrandr_query /home/strihuser strihuser",
        &[("FAKE", ok.to_str().unwrap())],
    );
    assert_eq!(c, 0);
    assert!(
        out.contains("DISPLAY=:0 XAUTHORITY=/home/strihuser/.Xauthority args=--query"),
        "queries :0 with the desktop user's Xauthority: {out}"
    );
    assert!(out.contains("HDMI-0 connected"), "{out}");
    let (c2, out2, _e) = run_env(
        "PATH=\"$FAKE:$PATH\"; x=\"$(strih_drm_xrandr_query /home/strihuser strihuser)\"; printf '[%s]' \"$x\"",
        &[("FAKE", bad.to_str().unwrap())],
    );
    assert_eq!(
        (c2, out2.as_str()),
        (0, "[]"),
        "a failing xrandr prints nothing and never aborts the set -e caller"
    );
    let _ = fs::remove_dir_all(&d);
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
        // review: the classifier itself could not run (strih_scenes import failed) -- never read
        // that as a missing config; SKIP still wins with no HDMI.
        ("1 ? program 0 0", "classify-failed", 1),
        ("0 ? program 0 0", "skip-no-hdmi", 2),
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

/// The step-6 block lives in the lib (`strih_drm_output_provision`, issue 1346 review: setup-strih.sh
/// stays under the 1000-line budget); setup-strih calls it with the box's backend fact.
fn provision_body() -> String {
    let lib = read("scripts/lib/strih-drm-output.sh");
    let start = lib
        .find("strih_drm_output_provision() {")
        .expect("the step-6 provisioning function");
    let end = lib[start..].find("\n}\n").expect("its end") + start;
    lib[start..end].to_string()
}

#[test]
fn setup_strih_provisions_drm_output_only_with_hdmi_and_retires_the_projector_json() {
    let setup = read("scripts/setup-strih.sh");
    assert!(
        setup.contains(". \"${HERE}/lib/strih-drm-output.sh\""),
        "setup-strih must source the drm-output lib"
    );
    assert!(
        setup.contains(
            "strih_drm_output_provision \"$USER_HOME\" \"$DESKTOP_USER\" \"$HERE\" \"$(strih_lx_hdmi_output_backend)\""
        ),
        "setup-strih step 6 runs the lib's provisioning with the backend fact"
    );
    assert!(
        !setup.contains("> /opt/camera-box/strih-lx-projector.json"),
        "the retired HDMI projector config must no longer be written"
    );
    let s = provision_body();
    assert!(
        !s.contains("> /opt/camera-box/strih-lx-projector.json"),
        "the retired HDMI projector config must no longer be written"
    );
    assert!(
        s.contains("LEGACY_PROJ=/opt/camera-box/strih-lx-projector.json")
            && s.contains("rm -f \"$LEGACY_PROJ\""),
        "a leftover strih-lx-projector.json is removed (its type carried over as the initial view)"
    );
    assert!(
        s.contains("[ -L \"$DRM_CONF_DIR\" ]"),
        "review round 2: a symlinked ~/.camera-box directory is refused too"
    );
    assert!(
        s.contains("[ -L \"$DRM_CONF\" ]"),
        "review: the root-run step must refuse a symlinked config path"
    );
    assert!(
        s.contains("install -m 0644 -o \"$desktop_user\" -g \"$desktop_user\" /dev/stdin \"$DRM_CONF\""),
        "review: the config is written by install (owned by the desktop user), never a root redirect"
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
    assert!(
        v.contains("DRM_SUMMARY=\"? program\""),
        "review round 2: with no classifier at all (strih_scenes / python3 missing) the default is \
         the unclassified token, never a dormant `-`"
    );
    assert!(
        v.contains("LC_ALL=C grep -aqF 'drm-output: program scanout LIVE'"),
        "review round 2: OBS logs carry invalid UTF-8 -- grep them byte-safe"
    );
    assert!(
        v.contains("echo \"? program\"") && v.contains("classify-failed)"),
        "review: a failed classifier run is its own token + verdict, never `config-missing`"
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

// ----------------------------------------------------------------------------------------------
// issue 1346 -- the NVIDIA Vulkan direct-display backend, selected by the box fact
// STRIH_HDMI_OUTPUT_BACKEND (main design 5838663570; STEP 0 + the GPU interop proven live on
// strih-lx, 5838745730 + 5839007372)
// ----------------------------------------------------------------------------------------------

#[test]
fn config_json_carries_the_backend_and_keeps_the_lease_shape_byte_identical() {
    let (c, out, _e) = run(
        "strih_drm_output_config_json HDMI-0 multiview vk-direct",
        None,
    );
    assert_eq!(c, 0);
    assert_eq!(
        out,
        "{\"enabled\":true,\"connector\":\"HDMI-0\",\"argb\":2105376,\"view\":\"multiview\",\"backend\":\"vk-direct\"}\n",
        "vk-direct is written explicitly, on the same one machine-written line"
    );
    let (c2, lease, _e) = run("strih_drm_output_config_json HDMI-1 program lease", None);
    let (c3, legacy, _e) = run("strih_drm_output_config_json HDMI-1 program", None);
    assert_eq!((c2, c3), (0, 0));
    assert_eq!(
        lease, legacy,
        "the lease backend is the ABSENT key: a lease config stays byte-identical to the pre-backend one"
    );
    assert!(!lease.contains("backend"), "{lease}");
    for bad in [
        "strih_drm_output_config_json HDMI-0 multiview vulkan",
        "strih_drm_output_config_json HDMI-0 multiview VK-DIRECT",
        "strih_drm_output_config_json HDMI-0 multiview 'vk-direct\"'",
    ] {
        let (cb, ob, _e) = run(bad, None);
        assert_ne!(
            cb, 0,
            "`{bad}` must refuse, never write a config the C would keep dormant"
        );
        assert!(ob.is_empty(), "`{bad}` must print nothing: {ob}");
    }
}

/// The two backend args (config token, box fact) are optional: an old 5-arg call grades exactly as
/// before (the existing verdict test), a drift between the config and the fact is its own FAIL.
#[test]
fn verdict_grades_the_backend_against_the_box_fact() {
    let cases: &[(&str, &str, i32)] = &[
        ("1 HDMI-0 multiview 1 1 vk-direct vk-direct", "ok", 0),
        ("1 HDMI-0 program 1 0 lease lease", "ok", 0),
        ("1 HDMI-0 multiview 1 1 lease vk-direct", "backend-drift", 1),
        ("1 HDMI-0 multiview 0 0 lease vk-direct", "backend-drift", 1),
        (
            "1 HDMI-0 multiview 1 1 unknown vk-direct",
            "backend-invalid",
            1,
        ),
        ("1 HDMI-0 multiview 1 1 vk-direct ?", "ok", 0),
        ("1 HDMI-0 multiview 1 1 ? vk-direct", "ok", 0),
        ("0 - program 0 0 lease vk-direct", "skip-no-hdmi", 2),
        ("1 - program 0 0 lease vk-direct", "config-missing", 1),
        (
            "1 HDMI-0 multiview 1 1 vk-direct vk-direct 1",
            "present-dead",
            1,
        ),
        ("1 HDMI-0 multiview 1 1 vk-direct vk-direct 0", "ok", 0),
        (
            "0 HDMI-0 multiview 1 1 vk-direct vk-direct 1",
            "hdmi-unplugged",
            2,
        ),
    ];
    for (args, token, rc) in cases {
        let (c, out, _e) = run(&format!("strih_drm_output_verdict {args} || exit $?"), None);
        assert_eq!(out, *token, "args `{args}`");
        assert_eq!(c, *rc, "args `{args}` -> rc");
    }
}

#[test]
fn setup_and_verify_take_the_backend_from_the_box_fact() {
    let facts = read("scripts/lib/strih-box-facts.sh");
    assert!(
        facts.contains(
            "strih_lx_hdmi_output_backend() { strih_box_fact STRIH_HDMI_OUTPUT_BACKEND; }"
        ),
        "the fact has ONE accessor"
    );
    let env = read("scripts/strih-boxes/strih-lx.env");
    assert!(
        env.lines()
            .any(|l| l == "STRIH_HDMI_OUTPUT_BACKEND=vk-direct"),
        "strih-lx's built-in HDMI is NVIDIA-driven: vk-direct (owner ruling 5838662632)"
    );
    let pp = read("scripts/strih-boxes/strih-pp.env");
    assert!(
        pp.lines()
            .any(|l| l == "STRIH_HDMI_OUTPUT_BACKEND=TODO_OWNER"),
        "the strih PP template carries the new fact undecided"
    );
    let s = provision_body();
    for token in [
        "strih_drm_output_config_json \"$DRM_CONN\" \"$DRM_VIEW0\" \"$backend\"",
        "write_drm_backend",
        "fail \"issue 1346: apt-get install libvulkan1 failed",
        "cannot import strih_scenes",
        "lease | vk-direct) ;;",
    ] {
        assert!(
            s.contains(token),
            "the step-6 provisioning must contain `{token}`"
        );
    }
    let v = read("scripts/verify-strih.sh");
    for token in [
        "drm_output_backend_token",
        "strih_lx_hdmi_output_backend",
        "backend-drift)",
        "backend-invalid)",
        "present-dead)",
        "strih_drm_vk_present_dead < \"$DRM_LOG\"",
        "\"$DRM_BACKEND_V\" \"$DRM_BACKEND_FACT\" \"$DRM_VK_DEAD\"",
    ] {
        assert!(v.contains(token), "verify-strih must contain `{token}`");
    }
}

/// main design 5840508308: setup-strih step 6 and verify-strih item 4c both detect the HDMI monitor
/// with the box's backend, so a vk-direct box reads the X RandR view (the sysfs status of the NVIDIA
/// connector stays `disconnected`) and a lease box keeps the kernel read.
#[test]
fn setup_and_verify_detect_the_hdmi_monitor_by_backend() {
    let s = provision_body();
    for token in [
        "DRM_XRANDR=\"$(strih_drm_xrandr_query \"$user_home\" \"$desktop_user\")\"",
        "elif strih_drm_hdmi_connected /sys/class/drm \"$backend\" \"$DRM_XRANDR\"; then",
        "DRM_CONN=\"$(printf '%s\\n' \"$DRM_XRANDR\" | strih_drm_hdmi_output_from_xrandr)\"",
    ] {
        assert!(
            s.contains(token),
            "the step-6 provisioning must contain `{token}`"
        );
    }
    let gather = s
        .find("DRM_XRANDR=\"$(strih_drm_xrandr_query")
        .expect("the X RandR gather");
    let detect = s
        .find("elif strih_drm_hdmi_connected /sys/class/drm \"$backend\"")
        .expect("the backend-aware detector");
    assert!(
        gather < detect,
        "the X RandR view is read before the detector uses it"
    );
    assert!(
        !s.contains("elif strih_drm_hdmi_connected /sys/class/drm; then"),
        "the backend-blind sysfs-only detection is gone from step 6"
    );

    let v = read("scripts/verify-strih.sh");
    let fact = v
        .find("DRM_BACKEND_FACT=\"$(strih_lx_hdmi_output_backend")
        .expect("verify reads the backend fact");
    let xr = v
        .find("DRM_XRANDR_V=\"$(strih_drm_xrandr_query \"$USER_HOME\"")
        .expect("verify reads the X RandR view for vk-direct");
    let det = v
        .find("strih_drm_hdmi_connected /sys/class/drm \"$DRM_BACKEND_FACT\" \"$DRM_XRANDR_V\" && DRM_HDMI=1")
        .expect("verify's detector takes the backend fact + the X view");
    assert!(
        fact < xr && xr < det,
        "verify: backend fact -> X RandR view -> detector"
    );
    assert_eq!(
        v.matches("strih_drm_hdmi_connected").count(),
        1,
        "verify runs the detector exactly once"
    );
}

/// A dead vk-direct present loop is visible although `program scanout LIVE` stays in the log (review
/// round 1): the loop's exit line with no stop after it = dead; a clean stop and an old log are alive.
#[test]
fn vk_present_dead_reads_the_exit_without_a_stop() {
    let live = "10:00:00.000: drm-output: program scanout LIVE (vk-direct: a published frame reached 'HDMI-0')\n";
    let exit = "10:05:00.000: drm-output: vk-direct present loop exited after 18000 presents (3 overwritten frames consumed)\n";
    let stop = "10:05:00.100: drm-output: stopped (vk-direct, 'HDMI-0')\n";
    for (log, dead) in [
        (format!("{live}{exit}"), true),
        (format!("{live}{exit}{stop}"), false),
        (live.to_string(), false),
        (String::new(), false),
        (format!("{live}{exit}{stop}{live}{exit}"), true),
    ] {
        let (c, _o, _e) = run("strih_drm_vk_present_dead", Some(&log));
        assert_eq!(c == 0, dead, "log:\n{log}");
    }
}
