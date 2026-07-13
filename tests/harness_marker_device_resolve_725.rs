//! #725 — the QPSK audio-marker device must be resolved DYNAMICALLY from the live `aplay -l`
//! output (matching the connected monitor's EDID name), never trusted to a hardcoded
//! `hw:CARD=PCH,DEV=3` — a reboot (or any HDMI renegotiation) can move which ALSA `DEV=N` the
//! physical monitor actually lands on, and a hardcoded device silently plays into a DEAD pin
//! while every existing check (ALSA `state: RUNNING`, the #431 marker-log-growth check) still
//! reports PASS, because both only prove the CONTINUOUS-FEED writer is alive, never that a real
//! sink is attached.
//!
//! Root cause (2026-07-12 evening, second speaker-silent incident that day): TEST mode claimed
//! "audio marker RUNNING+VERIFIED" while no sound left the speaker.
//!
//! Owner's PREMISE CORRECTION (issue #725 comment, live-resolved at the rig): the negotiated
//! monitor's EDID product name is *already* printed by `aplay -l` next to the HDMI device that
//! carries it — e.g. `device 3: HDMI 0 [BenQ GL2480]` — a device with NO connected monitor (or
//! one whose EDID hasn't negotiated) instead shows the GENERIC placeholder identical to its own
//! slot label, e.g. `device 7: HDMI 1 [HDMI 1]`. Resolving by matching a REAL (non-generic)
//! monitor name in `aplay -l` is simple and needs no `/proc/asound/.../eld#N.M` file-numbering
//! guesswork (the exact inference that went wrong and briefly made things worse on 2026-07-12).
//!
//! These tests source the REAL `scripts/lib/marker-device-resolve.sh` (never re-implement the
//! parser) against a fixture `aplay -l` transcript shaped like the ticket's own quoted excerpt
//! (a single genuine monitor name on device 3 amid several generic HDMI placeholder devices) —
//! captured from the ticket body since cam2's SSH is down for an UNRELATED reason (#737, a
//! failing-storage incident) at the time this fixture was authored, so a byte-exact fresh
//! `aplay -l` could not be pulled from the live box for this specific commit.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    manifest_dir().join("scripts/lib/marker-device-resolve.sh")
}

/// Fixture aplay -l transcript, shaped after the ticket's own quoted line
/// ("device 3: HDMI 0 [BenQ GL2480]") plus several generic (no-monitor) HDMI slots — the classic
/// Intel HDA "PCH" card layout on the cam2 appliance.
const APLAY_ONE_MONITOR_ON_DEV3: &str = "\
**** List of PLAYBACK Hardware Devices ****
card 0: PCH [HDA Intel PCH], device 0: ALC3234 Analog [ALC3234 Analog]
  Subdevices: 1/1
  Subdevice #0: subdevice #0
card 0: PCH [HDA Intel PCH], device 3: HDMI 0 [BenQ GL2480]
  Subdevices: 1/1
  Subdevice #0: subdevice #0
card 0: PCH [HDA Intel PCH], device 7: HDMI 1 [HDMI 1]
  Subdevices: 1/1
  Subdevice #0: subdevice #0
card 0: PCH [HDA Intel PCH], device 8: HDMI 2 [HDMI 2]
  Subdevices: 1/1
  Subdevice #0: subdevice #0
card 0: PCH [HDA Intel PCH], device 9: HDMI 3 [HDMI 3]
  Subdevices: 1/1
  Subdevice #0: subdevice #0
";

/// The SAME rig after a reboot re-negotiated HDMI pins: the monitor now lands on device 7
/// instead of 3 — this is the exact failure class #725 exists to catch (a hardcoded DEV=3 would
/// now silently target a dead pin while this fixture proves the LIVE monitor moved to DEV=7).
const APLAY_MONITOR_MOVED_TO_DEV7: &str = "\
**** List of PLAYBACK Hardware Devices ****
card 0: PCH [HDA Intel PCH], device 0: ALC3234 Analog [ALC3234 Analog]
  Subdevices: 1/1
  Subdevice #0: subdevice #0
card 0: PCH [HDA Intel PCH], device 3: HDMI 0 [HDMI 0]
  Subdevices: 1/1
  Subdevice #0: subdevice #0
card 0: PCH [HDA Intel PCH], device 7: HDMI 1 [BenQ GL2480]
  Subdevices: 1/1
  Subdevice #0: subdevice #0
";

/// No monitor connected anywhere — every HDMI device still shows its generic placeholder name.
const APLAY_NO_MONITOR: &str = "\
**** List of PLAYBACK Hardware Devices ****
card 0: PCH [HDA Intel PCH], device 0: ALC3234 Analog [ALC3234 Analog]
  Subdevices: 1/1
  Subdevice #0: subdevice #0
card 0: PCH [HDA Intel PCH], device 3: HDMI 0 [HDMI 0]
  Subdevices: 1/1
  Subdevice #0: subdevice #0
card 0: PCH [HDA Intel PCH], device 7: HDMI 1 [HDMI 1]
  Subdevices: 1/1
  Subdevice #0: subdevice #0
";

struct Run {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

fn run_sourced(aplay_text: &str, body: &str) -> Run {
    // The fixture is passed via an ENV VAR (not interpolated into the bash -c script text) —
    // Rust's `{:?}` Debug-escapes embedded newlines as literal `\n` two-character sequences,
    // which a plain double-quoted bash string does NOT expand back into real newlines (that
    // needs $'...' ANSI-C quoting). An env var carries the bytes verbatim, no escaping needed.
    let harness = format!("set -uo pipefail\n. {:?}\nAPLAY_TEXT=\"$APLAY_TEXT_FIXTURE\"\n{body}", script());
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("APLAY_TEXT_FIXTURE", aplay_text)
        .output()
        .expect("failed to run bash harness");
    Run {
        exit_code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

#[test]
fn resolves_the_genuine_monitor_device_amid_generic_placeholders() {
    let r = run_sourced(
        APLAY_ONE_MONITOR_ON_DEV3,
        "marker_device_resolve_from_aplay \"$APLAY_TEXT\"",
    );
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert_eq!(r.stdout, "hw:CARD=PCH,DEV=3");
}

#[test]
fn resolves_the_moved_monitor_after_a_pin_renegotiation() {
    // The exact bug class: a hardcoded DEV=3 would now be WRONG (dead pin); dynamic resolution
    // must follow the monitor to its new device.
    let r = run_sourced(
        APLAY_MONITOR_MOVED_TO_DEV7,
        "marker_device_resolve_from_aplay \"$APLAY_TEXT\"",
    );
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert_eq!(r.stdout, "hw:CARD=PCH,DEV=7");
}

#[test]
fn no_monitor_anywhere_fails_loud_instead_of_silently_resolving() {
    let r = run_sourced(
        APLAY_NO_MONITOR,
        "marker_device_resolve_from_aplay \"$APLAY_TEXT\"",
    );
    assert_ne!(
        r.exit_code, 0,
        "resolving with NO device carrying a monitor name must fail (never silently pick a dead pin)"
    );
    assert_eq!(r.stdout, "", "must print nothing on failure, stdout={}", r.stdout);
}

#[test]
fn carries_monitor_true_for_the_resolved_device() {
    let r = run_sourced(
        APLAY_ONE_MONITOR_ON_DEV3,
        "marker_device_carries_monitor \"$APLAY_TEXT\" 'hw:CARD=PCH,DEV=3'",
    );
    assert_eq!(
        r.exit_code, 0,
        "device 3 genuinely carries the BenQ monitor name -- must report present, stderr={}",
        r.stderr
    );
}

#[test]
fn carries_monitor_false_for_a_generic_no_monitor_device() {
    let r = run_sourced(
        APLAY_ONE_MONITOR_ON_DEV3,
        "marker_device_carries_monitor \"$APLAY_TEXT\" 'hw:CARD=PCH,DEV=7'",
    );
    assert_ne!(
        r.exit_code, 0,
        "device 7 shows only the generic placeholder -- must report NOT present"
    );
}

#[test]
fn carries_monitor_false_for_a_device_absent_from_aplay_output_entirely() {
    // The pin vanished entirely (e.g. cable unplugged mid-run, card renumbered) -- must fail
    // loud, never silently treat "not listed" as "fine".
    let r = run_sourced(
        APLAY_ONE_MONITOR_ON_DEV3,
        "marker_device_carries_monitor \"$APLAY_TEXT\" 'hw:CARD=PCH,DEV=99'",
    );
    assert_ne!(r.exit_code, 0);
}

#[test]
fn resolve_prefers_first_genuine_monitor_when_multiple_are_present() {
    // Two genuine monitor names present (an unusual but possible rig state) -- resolution must
    // be deterministic (first match in aplay -l's own listing order), never ambiguous/random.
    let text = "\
**** List of PLAYBACK Hardware Devices ****
card 0: PCH [HDA Intel PCH], device 3: HDMI 0 [BenQ GL2480]
  Subdevices: 1/1
  Subdevice #0: subdevice #0
card 0: PCH [HDA Intel PCH], device 7: HDMI 1 [Dell U2720Q]
  Subdevices: 1/1
  Subdevice #0: subdevice #0
";
    let r = run_sourced(text, "marker_device_resolve_from_aplay \"$APLAY_TEXT\"");
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert_eq!(r.stdout, "hw:CARD=PCH,DEV=3");
}

// ---------------------------------------------------------------------------
// Static wiring: rig-mode.sh's TEST-mode entry point must actually call the resolver before
// launching the painter -- never fall straight to the hardcoded AUDIO_MARKER_DEVICE default
// without at least attempting live resolution first (the exact regression #725 exists to close).
// ---------------------------------------------------------------------------

#[test]
fn rig_mode_do_test_calls_the_live_resolver() {
    let rig_mode = manifest_dir().join("scripts/rig-mode.sh");
    let text = std::fs::read_to_string(&rig_mode).expect("read rig-mode.sh");
    let do_test_start = text
        .find("do_test() {")
        .expect("rig-mode.sh must define do_test()");
    let do_test_end = text[do_test_start..]
        .find("\ndo_event() {")
        .map(|off| do_test_start + off)
        .unwrap_or(text.len());
    let do_test_body = &text[do_test_start..do_test_end];
    assert!(
        do_test_body.contains("resolve_marker_device") && do_test_body.contains("painter_launch_remote"),
        "do_test() must call the #725 live-resolver wrapper (resolve_marker_device) BEFORE \
         launching the painter, not rely solely on the hardcoded AUDIO_MARKER_DEVICE default"
    );
    assert!(
        do_test_body.contains("verify_marker_device_monitor"),
        "do_test() must re-verify the chosen marker device still carries a live monitor AFTER \
         launch (#725's post-launch re-check)"
    );
    assert!(
        text.contains("marker_device_resolve_from_aplay"),
        "the #725 pure resolver (marker_device_resolve_from_aplay) must actually be called \
         somewhere in rig-mode.sh, not just referenced in comments"
    );
    assert!(
        text.contains("lib/marker-device-resolve.sh"),
        "rig-mode.sh must source scripts/lib/marker-device-resolve.sh"
    );
}
