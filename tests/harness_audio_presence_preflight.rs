//! #748 — the fused gate must FAIL LOUD pre-record when the measurement audio (mbc chain) is
//! silent, never burn a full ~300s cycle reported only as a quiet per-camera av_sync
//! "unknown, candidates: 0" (run 29272685333 / RUN_ID 237189640 ran fully silent, -91.0 dB, and
//! the previous occurrence of the same class went UNNOTICED FOR A WEEK).
//!
//! Two layers locked here (all Tier-0 — no rig, no ssh):
//!  1. the pure decision lib scripts/lib/audio-presence-preflight.sh — parse the ffmpeg
//!     volumedetect max_volume, classify silent vs audible at a threshold, compose the
//!     operator-actionable messages, and build the remote ffmpeg/delete commands;
//!  2. recording-e2e.sh actually WIRES a pre-record audio-presence preflight step that sources the
//!     lib and exits non-zero on silence BEFORE [5/8] StartRecord (a static read of the shell
//!     script — the same model as tests/harness_recording_e2e_painter_freshness.rs).

use std::process::Command;

fn lib() -> String {
    format!(
        "{}/scripts/lib/audio-presence-preflight.sh",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// Source the lib and run `snippet`, returning (exit_ok, stdout_trimmed).
fn run(snippet: &str) -> (bool, String) {
    let script = format!(". \"{}\"\n{}", lib(), snippet);
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run bash");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

#[test]
fn parses_max_volume_from_ffmpeg_volumedetect_output() {
    let ff = "[Parsed_volumedetect_0 @ 0x55] n_samples: 1234\n\
              [Parsed_volumedetect_0 @ 0x55] mean_volume: -18.7 dB\n\
              [Parsed_volumedetect_0 @ 0x55] max_volume: -5.4 dB\n";
    let (ok, db) = run(&format!("audio_preflight_parse_max_db '{ff}'"));
    assert!(ok, "parse must succeed on a real volumedetect block");
    assert_eq!(db, "-5.4");
}

#[test]
fn parses_digital_silence_level() {
    let ff = "[Parsed_volumedetect_0 @ 0x55] max_volume: -91.0 dB\n";
    let (_ok, db) = run(&format!("audio_preflight_parse_max_db '{ff}'"));
    assert_eq!(db, "-91.0");
}

#[test]
fn parse_fails_non_zero_when_no_max_volume() {
    let ff = "ffmpeg version 6.0\nSome error: no audio stream found\n";
    let (ok, db) = run(&format!(
        "audio_preflight_parse_max_db '{ff}' && echo GOTDB"
    ));
    assert!(
        !ok,
        "parse of output with no max_volume must return non-zero"
    );
    assert!(db.is_empty(), "no db should be printed, got {db:?}");
}

#[test]
fn silent_track_classified_silent() {
    let (_ok, v) = run("audio_preflight_is_silent -91.0");
    assert_eq!(v, "true");
    let (_ok, v) = run("audio_preflight_is_silent -70 -60");
    assert_eq!(v, "true", "-70 dB is below the -60 threshold -> silent");
}

#[test]
fn audible_track_classified_not_silent() {
    let (_ok, v) = run("audio_preflight_is_silent -5.4");
    assert_eq!(v, "false");
    // exactly at the threshold is audible, not silent (strict <)
    let (_ok, v) = run("audio_preflight_is_silent -60 -60");
    assert_eq!(v, "false", "exactly -60 dB is NOT below -60 -> audible");
}

#[test]
fn silent_message_names_the_mbc_chain_to_check() {
    let (_ok, m) = run("audio_preflight_silent_message -91.0");
    for needle in ["mbc", "Ableton", "Dante", "#748"] {
        assert!(
            m.contains(needle),
            "silent message must mention {needle:?}: {m}"
        );
    }
}

#[test]
fn volumedetect_command_targets_the_probe_file_and_null_sink() {
    let (_ok, c) = run("audio_preflight_volumedetect_ps 'C:\\rec\\probe.mkv'");
    assert!(
        c.contains("volumedetect"),
        "must run the volumedetect filter: {c}"
    );
    assert!(
        c.contains("C:\\rec\\probe.mkv"),
        "must target the probe path: {c}"
    );
    assert!(
        c.contains("NUL"),
        "must write to the Windows null sink: {c}"
    );
}

/// The pre-record step must actually be WIRED into recording-e2e.sh: source the lib and, on a
/// silent measurement track, exit non-zero (a hard gate) BEFORE the [5/8] StartRecord.
#[test]
fn recording_e2e_wires_a_pre_record_audio_presence_gate() {
    let s = std::fs::read_to_string(format!(
        "{}/scripts/recording-e2e.sh",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("read recording-e2e.sh");
    assert!(
        s.contains("lib/audio-presence-preflight.sh"),
        "#748: recording-e2e.sh must source scripts/lib/audio-presence-preflight.sh"
    );
    let step = s
        .find("audio-presence preflight")
        .expect("#748: recording-e2e.sh must have a pre-record audio-presence preflight step");
    let step_region = &s[step..];
    assert!(
        step_region.contains("audio_preflight_is_silent"),
        "#748: the preflight step must classify silence via audio_preflight_is_silent"
    );
    let start = s
        .find("[5/8] StartRecord")
        .expect("recording-e2e.sh has the [5/8] StartRecord step");
    assert!(
        step < start,
        "#748: the audio-presence preflight must run BEFORE [5/8] StartRecord"
    );
}
