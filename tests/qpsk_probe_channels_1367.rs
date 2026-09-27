#![cfg(feature = "probe")]
//! Issue 1367 — the COMPILED `recording-verdict --qpsk-probe` over the real stereo mbc fixture.
//!
//! End-to-end through the probe-gated ffmpeg/ffprobe glue (CI installs ffmpeg for the probe suite):
//! the extract must keep both channels (never `-ac 1`), decode each, pick the decodable one, and
//! print the per-channel pick on the ONE JSON line the `[4b3/8]` preflight parses. The fixture is the
//! first 2 s of the stream recording `2026-09-27 14-29-50.mp4`: L alone clusters 3 (below the floor
//! of 4), R alone clusters 4, the downmix clusters 0 (see `tests/qpsk_channel_select_fixture_1367.rs`).

use std::path::Path;
use std::process::Command;

fn probe_json(extra: &[&str]) -> serde_json::Value {
    let bin = env!("CARGO_BIN_EXE_recording-verdict");
    let wav = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mbc-stereo-skew-1367/mbc-stereo-2s.wav");
    let out = Command::new(bin)
        .arg("--qpsk-probe")
        .arg(&wav)
        .args([
            "--av-audio-track",
            "0",
            "--qpsk-min-clusters",
            "4",
            "--qpsk-silent-db=-60",
            "--qpsk-loud-db=-20",
        ])
        .args(extra)
        .output()
        .expect("spawn recording-verdict --qpsk-probe");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "--qpsk-probe failed: {stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let line = stdout
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON line in: {stdout}"));
    serde_json::from_str(line).unwrap_or_else(|e| panic!("invalid JSON {line:?}: {e}"))
}

#[test]
fn qpsk_probe_decodes_every_channel_and_picks_the_decodable_one_1367() {
    let j = probe_json(&[]);
    assert_eq!(j["channels"], 2, "both channels kept: {j}");
    assert_eq!(j["chosen_channel"], 1, "R is the decodable channel: {j}");
    assert_eq!(j["verdict"], "OK", "{j}");
    let per = j["per_channel"].as_array().expect("per_channel array");
    assert_eq!(per.len(), 2, "{j}");
    assert!(
        per[0]["ch_cluster_samples"].as_u64().expect("ch0 cluster") < 4,
        "L alone stays below the floor here: {j}"
    );
    assert_eq!(j["cluster_samples"], per[1]["ch_cluster_samples"], "{j}");
    assert_eq!(j["crc_ok"], per[1]["ch_crc_ok"], "{j}");
    assert_eq!(j["preamble_screens"], per[1]["ch_preamble_screens"], "{j}");
}

#[test]
fn qpsk_probe_seconds_bound_applies_to_every_channel_1367() {
    // 1 s holds at most two 0.5 s-cadence markers per channel: nothing clusters, both channels are
    // still reported, and the whole-track peak names the class.
    let j = probe_json(&["--qpsk-probe-seconds", "1"]);
    assert_eq!(j["channels"], 2, "{j}");
    for c in j["per_channel"].as_array().expect("per_channel array") {
        assert!(c["ch_crc_ok"].as_u64().expect("crc_ok") <= 3, "{j}");
    }
    assert_ne!(j["verdict"], "OK", "{j}");
}
