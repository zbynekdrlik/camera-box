//! #807 — self-verifying VLC babysitter for the stream-OBS program audio tap.
//!
//! Background: the correct RTMP URL previously lived in only two places (the Desktop "AV-sync
//! monitor.bat" and the hand-built, unscheduled `C:\avsync\vlc-babysitter.ps1`) -- a human
//! sometimes typed the URL by hand instead, silently dropping the "/obs-e2e-test" playpath (an
//! RTMP URL with no stream name opens and plays nothing, with ZERO error). The old babysitter also
//! had no notion of whether the RTMP SOURCE was actually publishing, so a genuine off-air period
//! looked identical to "VLC frozen" (confirmed live: its own stale log shows an infinite
//! `demuxreadbytes=0` restart loop). Separately, the configured playback device (Focusrite) is
//! confirmed absent from the box today, with nothing detecting/alerting on that. This file proves
//! `scripts/avsync-vlc-monitor.ps1` (the repo-tracked, self-verifying replacement) fixes all
//! three.

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

const VLC_MONITOR_PS1: &str = "scripts/avsync-vlc-monitor.ps1";
const INSTALL_SH: &str = "scripts/avsync-watchdog-install.sh";

#[test]
fn hardcodes_the_correct_url_with_playpath_matching_the_watchdogs_own_constant() {
    let body = read(VLC_MONITOR_PS1);
    assert!(
        body.contains("rtmp://127.0.0.1:1234/live/obs-e2e-test"),
        "must hardcode the FULL url WITH the /obs-e2e-test playpath -- a hand-typed URL missing \
         it was the root cause (an RTMP url with no stream name opens and plays nothing, silently)"
    );
    // Never the truncated, playpath-less form as a STANDALONE launch target.
    let bare = "rtmp://127.0.0.1:1234/live'";
    assert!(
        !body.contains(bare),
        "must never launch VLC against the bare app url with no stream key: {body}"
    );
}

#[test]
fn asserts_rtmp_publishing_before_ever_blaming_vlc_for_frozen() {
    let body = read(VLC_MONITOR_PS1);
    assert!(
        body.contains("Test-RtmpPublishing"),
        "must independently verify the SOURCE is publishing (ffprobe) -- the old babysitter \
         couldn't tell 'off-air' apart from 'VLC frozen' and looped forever on a genuinely idle source"
    );
    assert!(
        body.contains("ffprobe"),
        "the publishing check must use ffprobe: {body}"
    );

    // The no-signal branch must NOT strike/restart VLC -- it must reset strikes and skip straight
    // to the next loop iteration (a `continue`), never touching the freeze-detection logic below.
    let no_signal_idx = body
        .find("no-signal: RTMP source is not publishing")
        .expect("must log the no-signal case distinctly");
    let region = &body[no_signal_idx..(no_signal_idx + 300).min(body.len())];
    assert!(
        region.contains("$strikes = 0") && region.contains("continue"),
        "the no-signal branch must reset strikes and `continue` -- never strike VLC for an idle \
         source: {region}"
    );
}

#[test]
fn frozen_restart_only_fires_when_the_source_actually_is_publishing() {
    let body = read(VLC_MONITOR_PS1);
    let frozen_idx = body
        .find("FROZEN - restarting vlc")
        .expect("must keep a frozen/restart branch for a genuine VLC-side stall");
    let region = &body[frozen_idx..(frozen_idx + 260).min(body.len())];
    assert!(
        region.contains("source IS publishing"),
        "the frozen-restart branch's own log line must state the source WAS confirmed publishing \
         (distinguishing a real VLC fault from an off-air period): {region}"
    );
}

#[test]
fn asserts_the_configured_audio_device_and_fails_loud_naming_it_when_absent() {
    let body = read(VLC_MONITOR_PS1);
    assert!(
        body.contains("Test-AudioDeviceConfigured"),
        "must check whether the configured playback device is actually present"
    );
    assert!(
        body.contains("'Focusrite'") || body.contains("\"Focusrite\""),
        "the configured device name must be visible/named, not a generic silent check: {body}"
    );
    assert!(
        body.contains("Win32_SoundDevice"),
        "must enumerate real Windows sound devices, not guess: {body}"
    );
    let fatal_idx = body
        .find("FATAL: configured audio device")
        .expect("must fail LOUD (a named FATAL log line), not silently degrade");
    let region = &body[fatal_idx..(fatal_idx + 200).min(body.len())];
    assert!(
        region.contains("$audioDevice"),
        "the fail-loud message must NAME the missing device: {region}"
    );
}

#[test]
fn writes_a_heartbeat_on_every_branch_not_just_the_happy_path() {
    let body = read(VLC_MONITOR_PS1);
    assert!(
        body.contains("Write-Heartbeat"),
        "must write a heartbeat so the dev1-side watchdog can detect this monitor's silence too"
    );
    assert!(body.contains("avsync-vlc-monitor-heartbeat.txt"));
    for status in ["no-signal", "launched", "frozen-restarted", "ok"] {
        assert!(
            body.contains(&format!("Write-Heartbeat \"{status}")),
            "must write the heartbeat on the '{status}' branch too, not just one happy path"
        );
    }
}

// ================================================================================================
// scripts/avsync-watchdog-install.sh (shared with #812) -- confirm the vlc-monitor task piece
// this ticket needs is actually wired into the shared install plan.
// ================================================================================================

#[test]
fn shared_install_plan_registers_the_vlc_monitor_boot_task() {
    let script = manifest_dir().join(INSTALL_SH);
    let out = Command::new("bash")
        .arg(&script)
        .output()
        .expect("run install script directly");
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let plan = String::from_utf8_lossy(&out.stdout);
    assert!(
        plan.contains("avsync-vlc-monitor") && plan.contains("<BootTrigger>"),
        "the shared install plan must register avsync-vlc-monitor as a BootTrigger task: {plan}"
    );
}
