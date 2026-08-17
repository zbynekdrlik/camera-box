//! #772 (provisioning bake-in half) -- `scripts/lib/camera-box-free-device.sh` installs a helper +
//! a `camera-box.service.d/free-capture-device.conf` drop-in whose ExecStartPre frees /dev/video
//! before EVERY camera-box start (the dead-man, cleanup(), the next-run preflight, OR a manual
//! operator restart), so a killed E2E run's stray `camera-box-burn-*.service` can never crash-loop
//! production on "Device or resource busy" (os error 16).
//!
//! These tests (a) source the REAL lib for its pure generator/parser functions, (b) re-exec the
//! GENERATED helper under fake `systemctl`/`pkill` to prove ACTUAL runtime behaviour (stops the
//! stray burn UNIT, pkills the burn, never touches the painter), and (c) assert setup-device.sh
//! writes both files and verify-device.sh's (y) check enforces them.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const LIB: &str = "scripts/lib/camera-box-free-device.sh";

/// Source the lib, call a 0/1-returning predicate with its argument passed via env (never
/// interpolated into the bash text), return whether it exited 0.
fn predicate(func: &str, arg: &str) -> bool {
    let body = format!("set -uo pipefail\n. \"$SCRIPT\"\n{func} \"$ARG\"");
    Command::new("bash")
        .arg("-c")
        .arg(&body)
        .env("SCRIPT", manifest_dir().join(LIB))
        .env("ARG", arg)
        .status()
        .expect("failed to run bash")
        .success()
}

/// Source the lib and echo the output of a generator function.
fn generate(func: &str) -> String {
    let body = format!("set -uo pipefail\n. \"$SCRIPT\"\n{func}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&body)
        .env("SCRIPT", manifest_dir().join(LIB))
        .output()
        .expect("failed to run bash");
    assert!(out.status.success(), "generator {func} failed: {:?}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// ---------------------------------------------------------------------------------------------- //
// Generators + parsers
// ---------------------------------------------------------------------------------------------- //

#[test]
fn generated_dropin_is_recognised_as_wired_772() {
    let dropin = generate("camera_box_free_capture_device_dropin_content");
    assert!(
        dropin.contains("ExecStartPre="),
        "#772: the drop-in must set an ExecStartPre. Got:\n{dropin}"
    );
    assert!(
        predicate("camera_box_free_device_dropin_wired", &dropin),
        "#772: the real generated drop-in must be recognised as wired to the helper"
    );
}

#[test]
fn generated_helper_is_recognised_as_burn_scoped_772() {
    let helper = generate("camera_box_free_capture_device_script_content");
    assert!(
        predicate("camera_box_free_device_script_is_burn_scoped", &helper),
        "#772: the real generated helper must be recognised as burn-scoped. Got:\n{helper}"
    );
}

#[test]
fn dropin_wired_rejects_an_unwired_dropin_772() {
    assert!(
        !predicate("camera_box_free_device_dropin_wired", "[Service]\nExecStart=/usr/local/bin/camera-box\n"),
        "#772: a drop-in with no ExecStartPre to the helper must NOT be accepted as wired"
    );
}

#[test]
fn burn_scoped_rejects_a_helper_that_touches_frame_probe_772() {
    assert!(
        !predicate(
            "camera_box_free_device_script_is_burn_scoped",
            "systemctl stop camera-box-burn-*\npkill -9 -x camera-box-burn\npkill -9 -x frame-probe\n",
        ),
        "#772: a helper that kills frame-probe (the cam2 painter) must be REJECTED, never accepted"
    );
}

#[test]
fn burn_scoped_rejects_a_pkill_only_helper_772() {
    assert!(
        !predicate(
            "camera_box_free_device_script_is_burn_scoped",
            "pkill -9 -x camera-box-burn\n",
        ),
        "#772: a helper that only pkills (never STOPS the Restart=on-failure burn unit) must be \
         rejected -- a pkilled burn just respawns and re-steals the device"
    );
}

// ---------------------------------------------------------------------------------------------- //
// Functional -- re-exec the generated helper under fake systemctl/pkill
// ---------------------------------------------------------------------------------------------- //

#[test]
fn generated_helper_stops_the_stray_burn_unit_and_never_the_painter_772() {
    let script = r#"
set -uo pipefail
. "$SCRIPT"
FAKE="$(mktemp -d)"
LOG="$FAKE/calls.log"
cat > "$FAKE/systemctl" <<FAKESC
#!/usr/bin/env bash
echo "systemctl \$*" >> "$LOG"
case "\$1 \$2" in
  "list-units --all") echo "camera-box-burn-91002.service" ;;
esac
exit 0
FAKESC
chmod +x "$FAKE/systemctl"
cat > "$FAKE/pkill" <<FAKEPK
#!/usr/bin/env bash
echo "pkill \$*" >> "$LOG"
exit 0
FAKEPK
chmod +x "$FAKE/pkill"
camera_box_free_capture_device_script_content > "$FAKE/helper.sh"
chmod +x "$FAKE/helper.sh"
PATH="$FAKE:/usr/bin:/bin" bash "$FAKE/helper.sh"
cat "$LOG"
rm -rf "$FAKE"
"#;
    let out = Command::new("bash")
        .arg("-c")
        .arg(script)
        .env("SCRIPT", manifest_dir().join(LIB))
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "helper harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let log = String::from_utf8_lossy(&out.stdout);
    assert!(
        log.contains("systemctl stop camera-box-burn-91002.service"),
        "#772: the helper must STOP the stray burn unit. Got:\n{log}"
    );
    assert!(
        log.contains("pkill -9 -x camera-box-burn"),
        "#772: the helper must pkill the stray burn. Got:\n{log}"
    );
    assert!(
        !log.contains("frame-probe"),
        "#772: the helper must NEVER touch frame-probe (the cam2 painter). Got:\n{log}"
    );
}

// ---------------------------------------------------------------------------------------------- //
// Provisioning wiring: setup-device.sh writes both files; verify-device.sh (y) enforces them
// ---------------------------------------------------------------------------------------------- //

#[test]
fn setup_device_sources_the_lib_and_writes_both_files_772() {
    let s = read("scripts/setup-device.sh");
    assert!(
        s.contains("lib/camera-box-free-device.sh"),
        "#772: setup-device.sh must source scripts/lib/camera-box-free-device.sh"
    );
    assert!(
        s.contains("camera_box_free_capture_device_script_content > /usr/local/bin/camera-box-free-capture-device.sh"),
        "#772: setup-device.sh must install the ExecStartPre helper to /usr/local/bin"
    );
    assert!(
        s.contains("camera_box_free_capture_device_dropin_content > /etc/systemd/system/camera-box.service.d/free-capture-device.conf"),
        "#772: setup-device.sh must install the free-capture-device.conf drop-in"
    );
}

#[test]
fn verify_device_has_the_y_check_before_q_772() {
    let s = read("scripts/verify-device.sh");
    assert!(
        s.contains("lib/camera-box-free-device.sh"),
        "#772: verify-device.sh must source the lib for the (y) check's parsers"
    );
    let y = s
        .find("camera_box_free_device_dropin_wired")
        .expect("#772: verify-device.sh must gate on the drop-in being wired (y check)");
    let y2 = s
        .find("camera_box_free_device_script_is_burn_scoped")
        .expect("#772: verify-device.sh must gate on the helper being burn-scoped (y check)");
    // (q) is the intentionally-LAST check (per .claude/rules/provisioning-scripts.md); the new (y)
    // check must sit BEFORE it.
    let q = s
        .rfind("(q) .bak cruft drift")
        .expect("#772: verify-device.sh must still have the (q) last check");
    assert!(
        y < q && y2 < q,
        "#772: the (y) ExecStartPre check must be inserted BEFORE the intentionally-last (q) check \
         (dropin-check {y}, helper-check {y2}, q {q})"
    );
}
