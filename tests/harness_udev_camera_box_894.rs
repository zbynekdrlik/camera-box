//! #894 -- `/etc/udev/rules.d/99-camera-box.rules` used to unconditionally restart production
//! `camera-box.service` on every video4linux "add" event (a benign USB re-enumeration). During an
//! E2E run `scripts/recording-e2e.sh` deliberately stops production and runs its own probe-featured
//! `camera-box-burn-<RUN_ID>.service` instead -- any hotplug during the run restarted production,
//! which stole `/dev/videoN` back from the burn unit (`77/NOPERM`, then a restart-loop into
//! `1/FAILURE`). `recording-verdict.rs` then reported this as `frozen_leg` on the camera, cost a
//! session two full gate runs chasing the wrong hypothesis (gate run 30554124753).
//!
//! Second defect found while root-causing the first: USB autosuspend is disabled ONLY by a
//! one-shot `/etc/rc.local` loop at boot -- a device that re-enumerates later comes back at the
//! kernel default `auto` (measured fleet-wide: the box that stayed `on` had zero re-enumerations
//! that day; the two that drifted to `auto` had 5 and 1, an amplifying feedback loop).
//!
//! These tests (a) source the REAL `scripts/lib/udev-camera-box.sh` for its pure decision/parser
//! functions, and (b) re-exec the GENERATED helper-script content under a nested bash with a fake
//! `systemctl` stub + a throwaway fake sysfs tree (the `imag-ssh-remote-tool-preflight.md`
//! "fake the remote, not the ssh" pattern) to prove the generated script's ACTUAL runtime behavior
//! -- not just that it contains the right substrings.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    let p = manifest_dir().join("scripts/lib/udev-camera-box.sh");
    assert!(p.exists(), "{} not found", p.display());
    p
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source the real lib and run `body`, returning stdout. Asserts the harness itself exited 0.
fn run_sourced(body: &str) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib_script())
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "sourced harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Source the lib, call a 0/1-returning predicate with an argument passed via env var (never
/// interpolated into the bash -c script text), return its exit code.
fn predicate(func: &str, arg: &str) -> bool {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{func} \"$ARG\"");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib_script())
        .env("ARG", arg)
        .output()
        .expect("failed to run bash harness");
    out.status.success()
}

// -------------------------------------------------------------------------------------------
// (A)/(B) content generators -- well-formed + wired to the guarded helper, never the bare
// unconditional restart the fleet's retired scripts/setup.sh used to ship.
// -------------------------------------------------------------------------------------------

#[test]
fn rules_content_points_at_the_guarded_helper_script() {
    let rules = run_sourced("udev_camera_box_rules_content");
    assert!(
        rules.contains(r#"SUBSYSTEM=="video4linux""#),
        "must still trigger on the video4linux add event: {rules}"
    );
    assert!(
        rules.contains("/usr/local/bin/camera-box-udev-video-add.sh"),
        "must RUN+= the guarded helper script, not a bare systemctl call: {rules}"
    );
    assert!(
        !rules.contains(r#"RUN+="/bin/systemctl restart camera-box.service""#),
        "must NOT reintroduce the fleet's old UNCONDITIONAL restart rule: {rules}"
    );
}

#[test]
fn helper_script_content_is_executable_bash_with_the_burn_guard() {
    let script = run_sourced("udev_camera_box_helper_script_content");
    assert!(
        script.starts_with("#!/bin/bash"),
        "helper script must start with a shebang: {script}"
    );
    assert!(
        script.contains("camera-box-burn-"),
        "must check for an active camera-box-burn-*.service: {script}"
    );
    assert!(
        script.contains("restart camera-box.service"),
        "must still restart production when no burn unit is active: {script}"
    );
}

#[test]
fn rule_is_burn_gated_rejects_the_old_unconditional_literal() {
    assert!(!predicate(
        "udev_camera_box_rule_is_burn_gated",
        r#"ACTION=="add", SUBSYSTEM=="video4linux", RUN+="/bin/systemctl restart camera-box.service""#
    ), "the OLD unconditional rule must NOT pass the burn-gated check -- this is the exact #894 regression");
}

#[test]
fn rule_is_burn_gated_accepts_the_generated_content() {
    let rules = run_sourced("udev_camera_box_rules_content");
    assert!(predicate("udev_camera_box_rule_is_burn_gated", &rules));
}

#[test]
fn helper_has_burn_guard_accepts_the_generated_content() {
    let script = run_sourced("udev_camera_box_helper_script_content");
    assert!(predicate("udev_camera_box_helper_has_burn_guard", &script));
}

#[test]
fn helper_has_burn_guard_rejects_a_helper_with_no_burn_check() {
    assert!(!predicate(
        "udev_camera_box_helper_has_burn_guard",
        "#!/bin/bash\nexec /bin/systemctl restart camera-box.service\n"
    ));
}

// -------------------------------------------------------------------------------------------
// (C) functional test -- run the GENERATED helper script for real, under a fake systemctl +
// fake sysfs tree. Proves the ACTUAL runtime decision, not just string content.
// -------------------------------------------------------------------------------------------

struct FakeRig {
    dir: tempfile::TempDir,
    systemctl_log: PathBuf,
}

impl FakeRig {
    /// Lay out a fake sysfs tree: <root>/devices/pci0000:00/usb2/2-2 (idVendor + power/control,
    /// starting at "auto") with a video4linux child at .../2-2/2-2:1.0/video4linux/video0 -- the
    /// SAME shape confirmed live on cam1 (readlink -f /sys/class/video4linux/video0).
    /// Also drops a fake `systemctl` stub on disk: `list-units ...camera-box-burn-*` echoes ONE
    /// line iff `$FAKE_BURN_ACTIVE=1`; `restart camera-box.service` appends a marker to a log file
    /// the test reads back afterward.
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let usb_dev = dir.path().join("devices/pci0000:00/usb2/2-2");
        let v4l_dir = usb_dev.join("2-2:1.0/video4linux/video0");
        fs::create_dir_all(&v4l_dir).expect("mkdir v4l dir");
        fs::write(usb_dev.join("idVendor"), "0100\n").expect("write idVendor");
        fs::create_dir_all(usb_dev.join("power")).expect("mkdir power");
        fs::write(usb_dev.join("power/control"), "auto\n").expect("write power/control");

        let systemctl_log = dir.path().join("systemctl.log");
        let systemctl_bin = dir.path().join("systemctl");
        fs::write(
            &systemctl_bin,
            "#!/bin/sh\n\
             if [ \"$1\" = \"list-units\" ]; then\n\
             \x20\x20if [ \"${FAKE_BURN_ACTIVE:-0}\" = \"1\" ]; then\n\
             \x20\x20\x20\x20echo 'camera-box-burn-123.service loaded active running fake'\n\
             \x20\x20fi\n\
             \x20\x20exit 0\n\
             fi\n\
             if [ \"$1\" = \"restart\" ]; then\n\
             \x20\x20echo \"RESTART_CALLED:$2\" >> \"$FAKE_SYSTEMCTL_LOG\"\n\
             \x20\x20exit 0\n\
             fi\n\
             exit 0\n",
        )
        .expect("write fake systemctl");
        let mut perm = fs::metadata(&systemctl_bin)
            .expect("stat fake systemctl")
            .permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
        fs::set_permissions(&systemctl_bin, perm).expect("chmod fake systemctl");

        FakeRig { dir, systemctl_log }
    }

    fn power_control(&self) -> String {
        fs::read_to_string(
            self.dir
                .path()
                .join("devices/pci0000:00/usb2/2-2/power/control"),
        )
        .unwrap_or_default()
        .trim()
        .to_string()
    }

    fn reset_power_control(&self) {
        fs::write(
            self.dir
                .path()
                .join("devices/pci0000:00/usb2/2-2/power/control"),
            "auto\n",
        )
        .expect("reset power/control");
    }

    fn restart_was_called(&self) -> bool {
        fs::read_to_string(&self.systemctl_log)
            .unwrap_or_default()
            .contains("RESTART_CALLED:camera-box.service")
    }

    /// Run the GENERATED helper script content against this fake rig. `devpath` mirrors the real
    /// udev-provided $DEVPATH shape (leading slash, rooted the same as the fake tree's own
    /// "/devices/..." layout).
    fn run_helper(&self, burn_active: bool) {
        let script = generated_helper_script();
        let devpath = "/devices/pci0000:00/usb2/2-2/2-2:1.0/video4linux/video0";
        let out = Command::new("bash")
            .arg("-c")
            .arg(&script)
            .env("DEVPATH", devpath)
            .env("_CBX_SYS_ROOT", self.dir.path())
            .env("_CBX_SYSTEMCTL", self.dir.path().join("systemctl"))
            .env("FAKE_SYSTEMCTL_LOG", &self.systemctl_log)
            .env("FAKE_BURN_ACTIVE", if burn_active { "1" } else { "0" })
            .output()
            .expect("run generated helper script");
        assert!(
            out.status.success(),
            "helper script must exit 0.\nstdout={:?}\nstderr={:?}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn generated_helper_script() -> String {
    let harness = "set -uo pipefail\n. \"$SCRIPT\"\nudev_camera_box_helper_script_content";
    let out = Command::new("bash")
        .arg("-c")
        .arg(harness)
        .env("SCRIPT", lib_script())
        .output()
        .expect("generate helper script content");
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn helper_skips_the_restart_while_a_burn_unit_is_active() {
    let rig = FakeRig::new();
    rig.run_helper(true);
    assert!(
        !rig.restart_was_called(),
        "production must NOT be restarted while a camera-box-burn-*.service is active -- this is \
         the exact #894 device-steal bug"
    );
}

#[test]
fn helper_restarts_production_when_no_burn_unit_is_active() {
    let rig = FakeRig::new();
    rig.run_helper(false);
    assert!(
        rig.restart_was_called(),
        "production restart-on-hotplug must still happen when no E2E run owns the device -- \
         normal operation must be UNCHANGED"
    );
}

#[test]
fn helper_reapplies_autosuspend_off_regardless_of_burn_state() {
    for burn_active in [true, false] {
        let rig = FakeRig::new();
        rig.reset_power_control(); // starts "auto" -- the exact drift #894 measured
        rig.run_helper(burn_active);
        assert_eq!(
            rig.power_control(),
            "on",
            "USB autosuspend must be re-asserted on every video4linux add event \
             (burn_active={burn_active}), independent of the restart decision"
        );
    }
}

// -------------------------------------------------------------------------------------------
// (D) power/control drift-read parser (verify-device.sh's new (w) check)
// -------------------------------------------------------------------------------------------

fn power_control_from(output: &str) -> String {
    let harness =
        "set -uo pipefail\n. \"$SCRIPT\"\nudev_camera_box_grabber_power_control_from_output \"$OUT\""
            .to_string();
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib_script())
        .env("OUT", output)
        .output()
        .expect("run parser");
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn power_control_parser_extracts_the_value() {
    assert_eq!(
        power_control_from("some ssh preamble\nCAMERA_BOX_GRABBER_POWER_CONTROL=on\n"),
        "on"
    );
}

#[test]
fn power_control_parser_returns_empty_when_no_grabber_found() {
    assert_eq!(
        power_control_from("CAMERA_BOX_GRABBER_POWER_CONTROL=\n"),
        ""
    );
}

#[test]
fn power_control_is_on_accepts_only_exactly_on() {
    assert!(predicate("udev_camera_box_power_control_is_on", "on"));
    assert!(!predicate("udev_camera_box_power_control_is_on", "auto"));
    assert!(!predicate("udev_camera_box_power_control_is_on", ""));
}

// -------------------------------------------------------------------------------------------
// (E) burn-unit health -- recording-e2e.sh's post-StopRecord run-integrity assertion
// -------------------------------------------------------------------------------------------

fn burn_unit_state_from(output: &str) -> String {
    let harness =
        "set -uo pipefail\n. \"$SCRIPT\"\nudev_camera_box_burn_unit_state_from_output \"$OUT\""
            .to_string();
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib_script())
        .env("OUT", output)
        .output()
        .expect("run parser");
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn burn_unit_state_parser_extracts_the_state() {
    assert_eq!(burn_unit_state_from("BURN_UNIT_STATE=failed\n"), "failed");
    assert_eq!(burn_unit_state_from("BURN_UNIT_STATE=active\n"), "active");
}

#[test]
fn burn_unit_is_healthy_accepts_only_active() {
    assert!(predicate("udev_camera_box_burn_unit_is_healthy", "active"));
    assert!(!predicate("udev_camera_box_burn_unit_is_healthy", "failed"));
    assert!(!predicate(
        "udev_camera_box_burn_unit_is_healthy",
        "inactive"
    ));
    assert!(!predicate("udev_camera_box_burn_unit_is_healthy", ""));
}

#[test]
fn burn_unit_integrity_message_is_loud_and_never_confused_with_a_frozen_camera() {
    let harness = "set -uo pipefail\n. \"$SCRIPT\"\nudev_camera_box_burn_unit_integrity_message \
                   CAM1 camera-box-burn-123.service failed"
        .to_string();
    let out = run_sourced(&harness.replace("set -uo pipefail\n. \"$SCRIPT\"\n", ""));
    assert!(out.contains("RUN-INTEGRITY FAILURE"), "{out}");
    assert!(out.contains("NOT a frozen camera"), "{out}");
    assert!(out.contains("camera-box-burn-123.service"), "{out}");
    assert!(out.contains("failed"), "{out}");
}

// -------------------------------------------------------------------------------------------
// Wiring -- both provisioning writers + verify-device.sh + recording-e2e.sh actually use this lib.
// -------------------------------------------------------------------------------------------

#[test]
fn setup_device_sources_the_lib_and_writes_both_files() {
    let s = read("scripts/setup-device.sh");
    assert!(s.contains("lib/udev-camera-box.sh"), "must source the lib");
    assert!(
        s.contains("udev_camera_box_rules_content > /etc/udev/rules.d/99-camera-box.rules"),
        "must write the rules file via the shared lib"
    );
    assert!(
        s.contains(
            "udev_camera_box_helper_script_content > /usr/local/bin/camera-box-udev-video-add.sh"
        ),
        "must write the helper script via the shared lib"
    );
    assert!(
        s.contains("chmod +x /usr/local/bin/camera-box-udev-video-add.sh"),
        "the helper script must be executable"
    );
}

#[test]
fn create_usb_linux_sources_the_lib_and_writes_both_files_into_the_image() {
    let s = read("scripts/create-usb-linux.sh");
    assert!(s.contains("lib/udev-camera-box.sh"), "must source the lib");
    assert!(
        s.contains(
            r#"udev_camera_box_rules_content > "$MOUNT_ROOT/etc/udev/rules.d/99-camera-box.rules""#
        ),
        "must bake the rules file into the base image"
    );
    assert!(
        s.contains(r#"udev_camera_box_helper_script_content > "$MOUNT_ROOT/usr/local/bin/camera-box-udev-video-add.sh""#),
        "must bake the helper script into the base image"
    );
}

#[test]
fn verify_device_has_a_w_check_for_the_gated_rule_and_live_power_control() {
    let s = read("scripts/verify-device.sh");
    assert!(
        s.contains("(w)"),
        "verify-device.sh must document a (w) check for #894"
    );
    assert!(
        s.contains("udev_camera_box_rule_is_burn_gated"),
        "(w) must assert the installed rule is burn-gated"
    );
    assert!(
        s.contains("udev_camera_box_power_control_is_on"),
        "(w) must assert the LIVE grabber's power/control has not drifted back to auto"
    );
    // per .claude/rules/provisioning-scripts.md: new checks must be inserted BEFORE (q), which
    // must remain the true last check.
    let q_pos = s
        .find("# (q) .bak cruft drift")
        .expect("(q) check must still exist");
    let w_pos = s
        .find("udev_camera_box_rule_is_burn_gated")
        .expect("(w) check must exist");
    assert!(
        w_pos < q_pos,
        "(w) must be inserted BEFORE (q), never after (provisioning-scripts.md)"
    );
}

#[test]
fn recording_e2e_gives_burn_units_unlimited_restart_and_asserts_their_health() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("--property=StartLimitIntervalSec=0"),
        "burn-mode systemd-run units must disable the start-limit burst so a transient device-\
         steal race can be retried indefinitely instead of permanently failing (#894)"
    );
    assert!(
        s.contains("udev_camera_box_burn_unit_integrity_message")
            || s.contains("udev_camera_box_burn_unit_is_healthy"),
        "the harness must assert burn-unit health itself, per #894's own \
         'the harness knows the unit's state and can assert it'"
    );
}
