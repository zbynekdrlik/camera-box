#![cfg(feature = "probe")]
//! Issue 1404 Task 5 part b — the COMPILED `recording-verdict --av-sync --av-run 911016` on the
//! committed measurement-clip fixture (`tests/fixtures/measurement-clip-1404/clip-v1-4s.mp4`, cut
//! from the generated clip), end to end through the probe glue: the rqrr decode of every frame, the
//! clip's own tick (`av_run_pairing`), the per-channel audio decode and the pairing. CI only (the
//! probe feature never compiles under Tier-0); the pure pairing is pinned on the same fixture by
//! `tests/av_run_pairing_clip_1404.rs`.
//! - the clean clip reads 0 within one frame, all 7 markers paired;
//! - a copy whose audio comes 100 ms early (video re-muxed untouched) reads +100 ± 17 ms;
//! - the painter path (no `--av-run`) measures nothing on the clip: 911016 is never the cam2 tick;
//! - a run that is not self-marked, or `--av-run` without `--av-sync`, is refused.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CLIP_RUN: &str = "911016";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/measurement-clip-1404")
        .join(name)
}

fn verdict(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_recording-verdict"))
        .args(args)
        .output()
        .expect("spawn recording-verdict")
}

fn av_sync(clip: &Path, extra: &[&str]) -> Output {
    let log = fixture("clip-v1-4s.mp4.markers.csv");
    let mut args = vec![
        "--av-sync",
        clip.to_str().expect("utf-8 path"),
        "--av-marker-log",
        log.to_str().expect("utf-8 path"),
    ];
    args.extend_from_slice(extra);
    verdict(&args)
}

/// The JSON block `run_av_sync` prints (a line that is exactly `{` through its closing brace).
fn json_of(out: &Output) -> serde_json::Value {
    let text = String::from_utf8_lossy(&out.stdout);
    let at = text
        .lines()
        .scan(0usize, |pos, line| {
            let start = *pos;
            *pos += line.len() + 1;
            Some((start, line))
        })
        .find(|(_, line)| line.trim_end() == "{")
        .map(|(start, _)| start)
        .unwrap_or_else(|| {
            panic!(
                "no JSON block in: {text}{}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
    serde_json::Deserializer::from_str(&text[at..])
        .into_iter::<serde_json::Value>()
        .next()
        .expect("a JSON value")
        .expect("valid JSON")
}

#[test]
fn the_clean_clip_reads_zero_within_one_frame_through_its_own_tick_1404() {
    let out = av_sync(&fixture("clip-v1-4s.mp4"), &["--av-run", CLIP_RUN]);
    assert!(
        out.status.success(),
        "--av-sync --av-run failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let j = json_of(&out);
    let off = j["av_offset_ms"].as_f64().expect("av_offset_ms");
    assert!(off.abs() <= 1000.0 / 30.0, "0 within one frame: {j}");
    assert_eq!(j["av_run"], 911_016, "{j}");
    assert!(j["matched"].as_u64().expect("matched") >= 6, "{j}");
    assert!(
        j["video_ticks"].as_u64().expect("video_ticks") >= 100,
        "the clip's tick on nearly every frame: {j}"
    );
}

#[test]
fn audio_100_ms_early_reads_plus_100_1404() {
    let dir = std::env::temp_dir().join(format!("av-run-1404-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let shifted = dir.join("clip-audio-early.mp4");
    let ff = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(fixture("clip-v1-4s.mp4"))
        .args([
            "-map",
            "0:v:0",
            "-map",
            "0:a:0",
            "-c:v",
            "copy",
            "-af",
            "atrim=start=0.1,asetpts=PTS-STARTPTS",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
        ])
        .arg(&shifted)
        .output()
        .expect("spawn ffmpeg");
    assert!(
        ff.status.success(),
        "ffmpeg: {}",
        String::from_utf8_lossy(&ff.stderr)
    );
    let out = av_sync(&shifted, &["--av-run", CLIP_RUN]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "--av-sync --av-run on the shifted copy failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let j = json_of(&out);
    let off = j["av_offset_ms"].as_f64().expect("av_offset_ms");
    assert!(
        (off - 100.0).abs() <= 17.0,
        "audio 100 ms early = the picture lags by +100 +/- 17 ms: {j}"
    );
}

#[test]
fn the_painter_path_never_reads_the_clips_tick_1404() {
    let out = av_sync(&fixture("clip-v1-4s.mp4"), &[]);
    assert!(
        !out.status.success(),
        "without --av-run the clip has no cam2 tick (911016 is tick-excluded), so nothing may be \
         measured: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn a_run_that_is_not_self_marked_or_av_run_alone_is_refused_1404() {
    let out = av_sync(&fixture("clip-v1-4s.mp4"), &["--av-run", "911015"]);
    assert!(!out.status.success());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        all.contains("not a self-marked run") && all.contains("911016"),
        "{all}"
    );
    let alone = verdict(&["--av-run", CLIP_RUN]);
    assert!(!alone.status.success());
    assert!(
        String::from_utf8_lossy(&alone.stderr).contains("requires --av-sync"),
        "{}",
        String::from_utf8_lossy(&alone.stderr)
    );
}
