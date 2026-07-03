//! #458 — imag-nb one-shot provisioner guards.
//!
//! imag-nb (10.77.9.182) is the NEW 60fps low-latency IMAG OBS box (spec
//! docs/superpowers/specs/2026-07-03-imag-nb-topology-design.md). The user's standing directive
//! from the cam6 provisioning saga: a new box is provisioned by ONE unambiguous script, never a
//! pile of ad-hoc manual steps. These tests pin the load-bearing contract of that script pair
//! (`scripts/setup-imag.sh` on-box + `scripts/imag_scenes.py` WS seeding from dev1) so a later
//! edit cannot silently drop a step that made the box production-ready.
//!
//! Style follows the repo's other script guards (`appliance_boot_hardening.rs`,
//! `cluster_clock_setup.rs`): read the REAL scripts and assert on the REAL contract.

use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const SETUP: &str = "scripts/setup-imag.sh";
const SCENES: &str = "scripts/imag_scenes.py";

/// The script must fail loud (script-failure policy) and never half-provision silently.
#[test]
fn setup_imag_fails_loud() {
    let body = read(SETUP);
    assert!(
        body.contains("set -euo pipefail"),
        "{SETUP} must run under `set -euo pipefail` — a half-provisioned imag box that LOOKS done \
         is the exact cam6-saga failure this one-shot script exists to kill"
    );
}

/// curl must be ENSURED before any download step — the cam5/#450 lesson: base images ship
/// without curl and every fetch silently fails mid-provision (hit AGAIN live on imag-nb).
#[test]
fn setup_imag_ensures_curl_up_front() {
    let body = read(SETUP);
    let curl_ensure = body
        .find("apt-get install -y curl ca-certificates")
        .expect("setup-imag.sh must install curl+ca-certificates up-front (cam5/#450 lesson)");
    let first_curl_use = body
        .find("curl -fsSL")
        .expect("setup-imag.sh downloads via curl -fsSL");
    assert!(
        curl_ensure < first_curl_use,
        "{SETUP}: the curl-ensure preflight must come BEFORE the first curl use"
    );
}

/// USB autosuspend must be forced OFF: the box's ONLY rig link is a USB-ethernet dongle
/// (enx…), and a suspended USB NIC silently stalls all 6 NDI streams.
#[test]
fn setup_imag_disables_usb_autosuspend() {
    let body = read(SETUP);
    assert!(
        body.contains("/sys/bus/usb/devices/*/power/control"),
        "{SETUP} must write `on` to /sys/bus/usb/devices/*/power/control — the rig NIC is USB; \
         autosuspend on it = all camera feeds stall"
    );
}

/// The IMAG box must never sleep or blank mid-service: lid ignore + sleep targets masked.
#[test]
fn setup_imag_masks_sleep_and_lid() {
    let body = read(SETUP);
    assert!(
        body.contains("HandleLidSwitch=ignore"),
        "{SETUP} must set HandleLidSwitch=ignore (it is a NOTEBOOK — closing the lid must not \
         kill the LED-wall program)"
    );
    assert!(
        body.contains("systemctl mask sleep.target suspend.target hibernate.target"),
        "{SETUP} must mask the sleep/suspend/hibernate targets (fleet parity)"
    );
}

/// DistroAV's Linux NDI loader scans ONLY /usr/lib, /usr/lib64 and /usr/local/lib
/// (non-recursive; NOT the multiarch dir, NOT the ld.so cache — vendor/distroav
/// src/plugin-main.cpp load_ndilib). Live-proven on imag-nb: without a libndi.so.<N>
/// symlink in a scanned dir the plugin loads UI-only with ERR-404.
#[test]
fn setup_imag_symlinks_ndi_into_distroav_scan_path() {
    let body = read(SETUP);
    assert!(
        body.contains("/usr/local/lib/libndi.so.6"),
        "{SETUP} must symlink libndi.so.6 into /usr/local/lib — the only fleet-convention dir \
         DistroAV's own Linux loader actually scans (ERR-404 otherwise, hit live on imag-nb)"
    );
}

/// The OBS WebSocket must come up on the fleet-convention port with no auth (stream-box
/// convention) — every rig WS tool (imag_scenes.py, render-budget gate, burn tooling) keys on it.
#[test]
fn setup_imag_seeds_websocket_4455() {
    let body = read(SETUP);
    for needle in [
        "ServerEnabled=true",
        "ServerPort=4455",
        "AuthRequired=false",
    ] {
        assert!(
            body.contains(needle),
            "{SETUP} must pre-seed the OBS WebSocket config with `{needle}` — without it no rig \
             tooling (scene seeding, render-budget gate, E2E) can reach the box"
        );
    }
}

/// Projector persistence: SaveProjectors=true is what makes the fullscreen PROGRAM projector
/// survive an OBS restart — without it every reboot loses the LED-wall output.
#[test]
fn setup_imag_persists_projectors() {
    let body = read(SETUP);
    assert!(
        body.contains("SaveProjectors=true"),
        "{SETUP} must seed SaveProjectors=true so the program projector survives OBS restarts"
    );
}

/// The scene seeder must create all six cameras, bind them 1:1 to the fleet NDI names, and
/// run the canvas at 1080p60 — the whole point of the box.
#[test]
fn imag_scenes_seeds_six_cams_at_1080p60() {
    let body = read(SCENES);
    assert!(
        body.contains("range(1, 7)"),
        "{SCENES} must iterate cameras 1..=6 (all six NDI cams)"
    );
    for needle in ["CANVAS_W, CANVAS_H, FPS = 1920, 1080, 60", "\"ndi_source\""] {
        assert!(
            body.contains(needle),
            "{SCENES} must pin `{needle}` — imag-nb is the 1080p60 NDI IMAG cutter"
        );
    }
    assert!(
        body.contains("(usb)"),
        "{SCENES} must bind inputs to the fleet `CAM<n> (usb)` NDI source names"
    );
}

/// Low-latency mode on every NDI source is the box's reason to exist.
#[test]
fn imag_scenes_pins_low_latency() {
    let body = read(SCENES);
    assert!(
        body.contains("\"latency\": 1"),
        "{SCENES} must set the DistroAV low-latency mode (latency=1) on every NDI input"
    );
}

/// The projector subcommand must refuse to open on the built-in panel — the program projector
/// belongs on the EXTERNAL (HDMI) monitor, and silently projecting onto eDP would look 'done'
/// while the LED wall stays black.
#[test]
fn imag_scenes_projector_targets_external_monitor_only() {
    let body = read(SCENES);
    assert!(
        body.contains("eDP") && body.contains("OBS_WEBSOCKET_VIDEO_MIX_TYPE_PROGRAM"),
        "{SCENES} --projector must exclude the eDP built-in panel and open the PROGRAM mix \
         projector on the external monitor"
    );
}
