//! Issue 1367 — EXECUTABLE C-vs-Rust parity gate for the OBS output frame stamp.
//!
//! Lifts `genlock_frame_stamp_100ns` (with the floor helper, the units constant and the age bound
//! it reads) VERBATIM from `vendor/distroav/src/ndi-output.cpp`, compiles it with `cc`, runs it
//! over a vector set and requires byte-identical stamps from the Tier-0 Rust authority
//! `camera_box::genlock_stamp::obs_frame_stamp_100ns`. Also pins the wiring: both DistroAV
//! outputs stamp the frame's own `frame->timestamp`. `cc` is required — this FAILS LOUDLY rather
//! than skipping when the toolchain is missing.

use camera_box::genlock_stamp::{obs_frame_stamp_100ns, OBS_FRAME_STAMP_MAX_AGE_NS};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const NDI_OUTPUT: &str = "vendor/distroav/src/ndi-output.cpp";
const JXS_OUTPUT: &str = "vendor/distroav/src/jxs-output.cpp";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn read(rel: &str) -> String {
    let p = repo(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// One line starting with `prefix`, or one `static ... name(` function up to its closing brace.
fn lift_line(src: &str, prefix: &str) -> String {
    src.lines()
        .find(|l| l.starts_with(prefix))
        .unwrap_or_else(|| panic!("issue 1367: {NDI_OUTPUT} no longer has a line `{prefix}`"))
        .to_string()
}

fn lift_fn(src: &str, signature: &str) -> String {
    let start = src
        .find(signature)
        .unwrap_or_else(|| panic!("issue 1367: {NDI_OUTPUT} no longer defines `{signature}`"));
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .unwrap_or_else(|| panic!("issue 1367: `{signature}` has no closing brace"));
    src[start..end].to_string()
}

/// `(wall_now_100ns, mono_now_ns, frame_ts_ns, fps)` vectors.
fn vectors() -> Vec<(i64, u64, u64, i64)> {
    const UNITS: i64 = 10_000_000;
    let sec: i64 = 1_793_420_130;
    let offset_ns: i64 = 1_792_470_000_123_456_789;
    let mono = |wall_100ns: i64| (wall_100ns * 100 - offset_ns) as u64;
    let mut v = Vec::new();
    let mut x: u64 = 0x1367_0000_2026_1031;
    for fps in [30i64, 60, 25, 24, 50, 0] {
        for k in 0..fps.max(1) {
            let slot = if fps > 0 {
                sec * UNITS + k * UNITS / fps
            } else {
                sec * UNITS
            };
            for jitter in [-50_000i64, -1, 0, 1, 50_000] {
                for late in [0i64, 1, 20_000, 166_666, 400_000, 2_000_000] {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    let send = slot + late + (x % 97) as i64;
                    let frame_ts = (mono(slot) as i64 + jitter) as u64;
                    v.push((send, mono(send), frame_ts, fps));
                }
            }
        }
        // the fallbacks: none, ahead of the media clock, at and past the age bound
        let send = sec * UNITS + 123_456;
        let m = mono(send);
        for ts in [
            0,
            m + 1,
            m - OBS_FRAME_STAMP_MAX_AGE_NS,
            m - OBS_FRAME_STAMP_MAX_AGE_NS - 1,
            m,
        ] {
            v.push((send, m, ts, fps));
        }
    }
    v
}

#[test]
fn the_shipped_c_frame_stamp_matches_the_rust_authority_1367() {
    let src = read(NDI_OUTPUT);
    let program = format!(
        "#include <stdint.h>\n#include <stdio.h>\n{}\n{}\n{}\n{}\n\
         int main(void) {{\n\
           long long w; unsigned long long m, t; long long f;\n\
           while (scanf(\"%lld %llu %llu %lld\", &w, &m, &t, &f) == 4)\n\
             printf(\"%lld\\n\", (long long)genlock_frame_stamp_100ns(w, m, t, f));\n\
           return 0;\n}}\n",
        lift_line(&src, "static const int64_t GENLOCK_UNITS_PER_SECOND"),
        lift_line(&src, "static const uint64_t GENLOCK_FRAME_STAMP_MAX_AGE_NS"),
        lift_fn(&src, "static int64_t genlock_floor_boundary_100ns("),
        lift_fn(&src, "static int64_t genlock_frame_stamp_100ns("),
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let c = dir.path().join("frame_stamp.c");
    let bin = dir.path().join("frame_stamp");
    fs::write(&c, program).unwrap();
    let out = Command::new("cc")
        .args(["-std=c99", "-O1", "-Wall", "-Werror", "-o"])
        .arg(&bin)
        .arg(&c)
        .output()
        .expect("issue 1367: `cc` is required for the frame-stamp parity gate");
    assert!(
        out.status.success(),
        "cc failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let vecs = vectors();
    let input: String = vecs
        .iter()
        .map(|(w, m, t, f)| format!("{w} {m} {t} {f}\n"))
        .collect();
    // Input from a file, never a pipe written before stdout is read: the output outgrows the
    // pipe buffer and both ends would block.
    let vec_file = dir.path().join("vectors.txt");
    fs::write(&vec_file, input).unwrap();
    let got = Command::new(&bin)
        .stdin(fs::File::open(&vec_file).unwrap())
        .output()
        .unwrap();
    assert!(got.status.success());
    let c_stamps: Vec<i64> = String::from_utf8(got.stdout)
        .unwrap()
        .lines()
        .map(|l| l.parse().unwrap())
        .collect();
    assert_eq!(c_stamps.len(), vecs.len());
    for (i, (w, m, t, f)) in vecs.iter().enumerate() {
        assert_eq!(
            c_stamps[i],
            obs_frame_stamp_100ns(*w, *m, *t, *f),
            "vector {i}: wall {w} mono {m} frame {t} fps {f}"
        );
    }
}

#[test]
fn both_outputs_stamp_the_frames_own_render_tick_1367() {
    let ndi = read(NDI_OUTPUT);
    assert!(ndi.contains(
        "video_frame.timecode = genlock_emit_timecode_100ns(o->video_framerate, frame->timestamp);"
    ));
    assert!(ndi.contains("return genlock_frame_stamp_100ns(wall_now, mono_now, frame_ts_ns, fps);"));
    let jxs = read(JXS_OUTPUT);
    assert!(jxs.contains(
        "pic.timecode_100ns = genlock_emit_timecode_100ns(o->framerate, frame->timestamp);"
    ));
}
