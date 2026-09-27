//! Issue 1367 — ONE measurement-audio channel rule, the same in the offline gate and the live dock.
//!
//! The rule (design 5856569255): the LOWEST channel whose self-consistency cluster clears the #1324
//! decodability floor; when none clears it, the largest cluster, ties to the lowest. The offline
//! decode applies it in Rust (`qpsk_channel_select::pick_marker_channel`); the live dock applies
//! it in C++ (`vendor/av-sync-dock/src/camera-box-channel-pick.hpp`), which compiles only on the
//! Windows genlock build. So this file compiles the C++ side here with g++
//! (`vendor/av-sync-dock/test/channel-pick-parity.cpp`) and compares it with the Rust:
//! - the pick over the shared table `tests/fixtures/qpsk_channel_pick_parity.tsv`;
//! - the self-consistency cluster over generated marker sequences and the real fixture's decodes;
//! - the whole per-channel streaming decode + pick, callback by callback, on the real stereo
//!   fixture and on synthetic tracks (both channels decode, marker only on R, a channel that stops);
//! - every mirrored constant, and the `[4b3/8]` shell defaults it is single-sourced with.
//!
//! Default features, no rig, no ffmpeg: g++ and bash are the only tools.

use camera_box::av_sync_dock::DOCK_QPSK_THRESHOLD;
use camera_box::av_sync_dock_channels::{
    ChannelMarkerPicker, DOCK_CHANNEL_PICK_MAX_MARKERS, DOCK_CHANNEL_PICK_WINDOW_S,
};
use camera_box::qpsk_channel_select::{f32le_to_channels, pick_marker_channel};
use camera_box::qpsk_marker::{decode_markers_with_stats, marker_signal, signal_len, AudioParams};
use camera_box::qpsk_probe_decision::{
    consistency_cluster_size, ClusterParams, DEFAULT_MIN_CLUSTERS,
};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const TABLE: &str = "tests/fixtures/qpsk_channel_pick_parity.tsv";
const TOOL: &str = "vendor/av-sync-dock/test/channel-pick-parity.cpp";
const PREFLIGHT_LIB: &str = "scripts/lib/marker-decodability-preflight.sh";
const FIXTURE: &str = "tests/fixtures/mbc-stereo-skew-1367/mbc-stereo-2s.wav";

fn repo(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// A per-process scratch directory for the compiled tool and its input files.
fn scratch() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let d = std::env::temp_dir().join(format!(
            "qpsk-channel-pick-parity-{}-{}",
            std::process::id(),
            env!("CARGO_PKG_VERSION")
        ));
        std::fs::create_dir_all(&d).expect("create the scratch dir");
        d
    })
}

/// The C++ side, compiled once per test process (`-std=c++11 -Wall -Wextra -Werror`, like the
/// dock's other mirror gates).
fn tool() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let bin = scratch().join("channel-pick-parity");
        let out = Command::new("g++")
            .args(["-std=c++11", "-O2", "-Wall", "-Wextra", "-Werror"])
            .arg(repo(TOOL))
            .arg("-o")
            .arg(&bin)
            .output()
            .expect("spawn g++ (install build-essential) for the dock channel-pick mirror");
        assert!(
            out.status.success(),
            "the dock channel-pick mirror must compile clean:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        bin
    })
}

fn run_tool(args: &[&str]) -> String {
    let out = Command::new(tool())
        .args(args)
        .output()
        .expect("run the channel-pick parity tool");
    assert!(
        out.status.success(),
        "channel-pick-parity {args:?} failed:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf-8 output")
}

struct Row {
    clusters: Vec<u64>,
    min_clusters: u64,
    expected: Option<usize>,
}

fn table() -> Vec<Row> {
    let text = std::fs::read_to_string(repo(TABLE)).expect("read the parity table");
    let rows: Vec<Row> = text
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let cols: Vec<&str> = l.split('\t').collect();
            assert_eq!(cols.len(), 3, "bad table row {l:?}");
            let clusters = if cols[0] == "<empty>" {
                Vec::new()
            } else {
                cols[0]
                    .split(',')
                    .map(|c| c.parse().expect("cluster"))
                    .collect()
            };
            Row {
                clusters,
                min_clusters: cols[1].parse().expect("min_clusters"),
                expected: match cols[2] {
                    "none" => None,
                    n => Some(n.parse().expect("expected position")),
                },
            }
        })
        .collect();
    assert!(rows.len() >= 20, "the table must stay substantial");
    rows
}

#[test]
fn the_table_is_the_rust_rule() {
    for r in table() {
        assert_eq!(
            pick_marker_channel(&r.clusters, r.min_clusters),
            r.expected,
            "clusters {:?} floor {}",
            r.clusters,
            r.min_clusters
        );
    }
    // the live 4 s clip, named: both channels clear the floor, so L wins although R is larger
    assert_eq!(pick_marker_channel(&[7, 8], DEFAULT_MIN_CLUSTERS), Some(0));
}

#[test]
fn the_cpp_pick_follows_the_table() {
    let path = repo(TABLE);
    let got = run_tool(&["pick", path.to_str().expect("utf-8 path")]);
    let want: Vec<String> = table()
        .iter()
        .map(|r| r.expected.map_or("none".to_string(), |p| p.to_string()))
        .collect();
    assert_eq!(got.lines().collect::<Vec<_>>(), want);
}

/// Deterministic marker sequences covering the cluster's branches: clean cadence chains, missed
/// markers, interspersed and pure false decodes, equal timestamps, wrap-around steps, short inputs.
fn generated_sequences() -> Vec<Vec<(f64, u8)>> {
    let mut seed: u64 = 0x1367_5856_5692_55aa;
    let mut next = move || {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) as u32
    };
    let mut out = Vec::new();
    for case in 0..400u32 {
        let n = (next() % 14) as usize;
        let gap = 0.5 + (next() % 3000) as f64 / 1000.0;
        let step = (next() % 256) as u8;
        let mut idx = (next() % 256) as u8;
        let mut t = (next() % 5000) as f64 / 1000.0;
        let mut m = Vec::new();
        for _ in 0..n {
            match case % 5 {
                // a clean chain with a little timing jitter
                0 => m.push((t + (next() % 40) as f64 / 1000.0, idx)),
                // a chain that misses a marker now and then (the next pair spans 2S / 2G)
                1 => {
                    if next() % 5 != 0 {
                        m.push((t, idx));
                    }
                }
                // a chain with a false decode in between now and then
                2 => {
                    if next() % 4 == 0 {
                        m.push((t - gap / 3.0, (next() % 256) as u8));
                    }
                    m.push((t, idx));
                }
                // pure noise
                3 => m.push(((next() % 20_000) as f64 / 1000.0, (next() % 256) as u8)),
                // equal timestamps: the stable time sort must keep their input order
                _ => {
                    if next() % 3 == 0 {
                        m.push((t, (next() % 256) as u8));
                    }
                    m.push((t, idx));
                }
            }
            idx = idx.wrapping_add(step);
            t += gap;
        }
        out.push(m);
    }
    // The upper median of an even gap count decides the chain: gaps 1.5, 1, 1, 1, 1.5, 1.5 give
    // G = 1.5 (the upper median) and a chain of 3; the lower median would give 1.0 and 4.
    let times = [0.0, 1.5, 2.5, 3.5, 4.5, 6.0, 7.5];
    out.push(
        times
            .iter()
            .enumerate()
            .map(|(k, &t)| (t, 189u8.wrapping_add((k as u32 * 180) as u8)))
            .collect(),
    );
    // Many equal timestamps in a long input (past the small-array insertion sort): the time sort
    // must be stable, so each group keeps its input order.
    for _ in 0..12 {
        let mut m = Vec::new();
        let mut idx = (next() % 256) as u8;
        for k in 0..30 {
            let t = 3.0 * k as f64;
            let real_at = next() % 3;
            for j in 0..3 {
                let i = if j == real_at {
                    idx
                } else {
                    (next() % 256) as u8
                };
                m.push((t, i));
            }
            idx = idx.wrapping_add(180);
        }
        out.push(m);
    }
    out
}

/// The committed fixture (48 kHz stereo s16) as two f32 channels, s16 / 32768 like ffmpeg.
fn fixture_channels() -> Vec<Vec<f32>> {
    let b = std::fs::read(repo(FIXTURE)).expect("read the stereo fixture");
    assert_eq!(&b[0..4], b"RIFF");
    assert_eq!(&b[8..12], b"WAVE");
    // walk the chunks (the file carries a LIST chunk before its data)
    let mut pos = 12;
    loop {
        assert!(pos + 8 <= b.len(), "no data chunk");
        let len = u32::from_le_bytes([b[pos + 4], b[pos + 5], b[pos + 6], b[pos + 7]]) as usize;
        if &b[pos..pos + 4] == b"fmt " {
            let fmt = &b[pos + 8..pos + 8 + len];
            assert_eq!(&fmt[0..4], &[1, 0, 2, 0], "PCM stereo");
            assert_eq!(u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]), 48_000);
            assert_eq!(u16::from_le_bytes([fmt[14], fmt[15]]), 16, "s16");
        }
        if &b[pos..pos + 4] == b"data" {
            let data = &b[pos + 8..(pos + 8 + len).min(b.len())];
            let f32le: Vec<u8> = data
                .as_chunks::<2>()
                .0
                .iter()
                .flat_map(|c| (i16::from_le_bytes(*c) as f32 / 32768.0).to_le_bytes())
                .collect();
            return f32le_to_channels(&f32le, 2).expect("whole stereo frames");
        }
        pos += 8 + len + (len & 1);
    }
}

#[test]
fn the_cpp_cluster_size_matches_rust_on_generated_and_real_decodes() {
    let p = AudioParams::rig60();
    let mut seqs = generated_sequences();
    let ch = fixture_channels();
    let mix: Vec<f32> = ch[0]
        .iter()
        .zip(&ch[1])
        .map(|(a, b)| (a + b) / 2.0)
        .collect();
    for samples in [&ch[0], &ch[1], &mix] {
        seqs.push(decode_markers_with_stats(samples, &p, DOCK_QPSK_THRESHOLD).0);
    }
    let mut text = String::new();
    for s in &seqs {
        write!(text, "{}", s.len()).expect("write");
        for &(ts, idx) in s {
            write!(text, " {ts} {idx}").expect("write");
        }
        text.push('\n');
    }
    let path = scratch().join("cluster-input.txt");
    std::fs::write(&path, text).expect("write the cluster input");
    let got = run_tool(&["cluster", path.to_str().expect("utf-8 path")]);
    let got: Vec<u64> = got.lines().map(|l| l.parse().expect("u64")).collect();
    let want: Vec<u64> = seqs
        .iter()
        .map(|s| consistency_cluster_size(s, ClusterParams::default()))
        .collect();
    assert_eq!(got.len(), want.len());
    for (i, (g, w)) in got.iter().zip(&want).enumerate() {
        assert_eq!(g, w, "sequence {i}: {:?}", seqs[i]);
    }
    // the sequences exercise both sides of the floor, not only zeros
    assert!(want.iter().any(|&c| c >= DEFAULT_MIN_CLUSTERS));
    assert!(want.iter().any(|&c| c > 0 && c < DEFAULT_MIN_CLUSTERS));
    // the real fixture: L below the floor, R at it (the committed dev-712 reading)
    let n = want.len();
    assert!(want[n - 3] < DEFAULT_MIN_CLUSTERS, "L: {}", want[n - 3]);
    assert!(want[n - 2] >= DEFAULT_MIN_CLUSTERS, "R: {}", want[n - 2]);
}

/// The Rust reference's transcript, in the C++ tool's exact line format.
/// `window` = `None` is the dock configuration; `Some(w)` the same with a pick window of `w` samples.
fn rust_transcript(channels: &[Vec<f32>], chunk: usize, window: Option<u64>) -> String {
    let p = AudioParams::rig60();
    let sig = signal_len(&p);
    let mut picker = match window {
        None => ChannelMarkerPicker::dock(channels.len(), p),
        Some(w) => ChannelMarkerPicker::new(
            channels.len(),
            p,
            DOCK_QPSK_THRESHOLD,
            sig * 3,
            sig as u64,
            w,
            DEFAULT_MIN_CLUSTERS,
        ),
    };
    let frames = channels[0].len();
    let mut out = String::new();
    let (mut at, mut push) = (0usize, 0usize);
    while at < frames {
        let end = (at + chunk).min(frames);
        let planes: Vec<&[f32]> = channels.iter().map(|c| &c[at..end]).collect();
        let markers = picker.push(&planes);
        let clusters: Vec<String> = picker.clusters().iter().map(|c| c.to_string()).collect();
        let markers: Vec<String> = markers.iter().map(|(a, i)| format!("{a}:{i}")).collect();
        let s = picker.stats();
        writeln!(
            out,
            "{push} chosen={} clusters={} markers={} stats={}/{}/{}",
            picker.chosen(),
            clusters.join(","),
            markers.join(","),
            s.preamble_screens_passed,
            s.crc_ok,
            s.crc_fail
        )
        .expect("write");
        at = end;
        push += 1;
    }
    out
}

fn cpp_transcript(name: &str, channels: &[Vec<f32>], chunk: usize, window: Option<u64>) -> String {
    let path = scratch().join(format!("{name}.f32"));
    let bytes: Vec<u8> = channels
        .iter()
        .flat_map(|c| c.iter().flat_map(|x| x.to_le_bytes()))
        .collect();
    std::fs::write(&path, bytes).expect("write the stream input");
    let mut args = vec![
        "stream".to_string(),
        path.to_str().expect("utf-8 path").to_string(),
        channels.len().to_string(),
        chunk.to_string(),
    ];
    args.extend(window.map(|w| w.to_string()));
    run_tool(&args.iter().map(String::as_str).collect::<Vec<_>>())
}

/// `n` real markers every `cadence_s` from `start_s`, index stepping by 180 (the rig's ~3 s /
/// 180-frame emitter), delayed by `delay` samples, in `len` samples.
fn marker_track(n: usize, cadence_s: f64, start_s: f64, delay: usize, len: usize) -> Vec<f32> {
    let p = AudioParams::rig60();
    let sr = p.sample_rate as f64;
    let mut buf = vec![0.0f32; len];
    for k in 0..n {
        let idx = 189u8.wrapping_add((k as u32 * 180) as u8);
        let off = ((start_s + k as f64 * cadence_s) * sr) as usize + delay;
        for (i, &s) in marker_signal(idx, &p).iter().enumerate() {
            if off + i < buf.len() {
                buf[off + i] += s * 0.25;
            }
        }
    }
    buf
}

/// Marker k (index 189 + 180k) at `start + k * spacing` samples plus `delay`, for each k in `ks`.
fn markers_at(ks: &[usize], spacing: usize, start: usize, delay: usize, len: usize) -> Vec<f32> {
    let p = AudioParams::rig60();
    let mut buf = vec![0.0f32; len];
    for &k in ks {
        let idx = 189u8.wrapping_add((k as u32 * 180) as u8);
        let off = start + k * spacing + delay;
        for (i, &s) in marker_signal(idx, &p).iter().enumerate() {
            if off + i < buf.len() {
                buf[off + i] += s * 0.25;
            }
        }
    }
    buf
}

/// Every returned marker `(abs, idx)` of a transcript, in order.
fn returned(t: &str) -> Vec<(u64, u8)> {
    t.lines()
        .filter_map(|l| l.split(" markers=").nth(1)?.split(' ').next())
        .flat_map(|m| m.split(',').filter(|x| !x.is_empty()))
        .map(|x| {
            let (a, i) = x.split_once(':').expect("abs:idx");
            (a.parse().expect("abs"), i.parse().expect("idx"))
        })
        .collect()
}

/// No marker index is returned twice within one marker length (one physical marker, one pairing).
fn assert_no_double_return(name: &str, t: &str) {
    let sig = signal_len(&AudioParams::rig60()) as u64;
    let got = returned(t);
    for w in got.windows(2) {
        assert!(
            !(w[0].1 == w[1].1 && w[1].0 <= w[0].0 + sig),
            "{name}: marker {} returned twice: {got:?}",
            w[1].1
        );
    }
}

fn assert_same_transcript(
    name: &str,
    channels: &[Vec<f32>],
    chunk: usize,
    window: Option<u64>,
) -> String {
    let rust = rust_transcript(channels, chunk, window);
    let cpp = cpp_transcript(name, channels, chunk, window);
    for (i, (r, c)) in rust.lines().zip(cpp.lines()).enumerate() {
        assert_eq!(c, r, "{name}: push {i} differs (left C++, right Rust)");
    }
    assert_eq!(
        cpp.lines().count(),
        rust.lines().count(),
        "{name}: push count"
    );
    rust
}

fn last_line(t: &str) -> &str {
    t.lines().last().expect("at least one push")
}

#[test]
fn the_cpp_streaming_picker_matches_rust_callback_by_callback() {
    const SR: usize = 48_000;
    // The real 2 s stereo fixture: L below the floor, R clears it, so R is chosen.
    let real = fixture_channels();
    for chunk in [1024, 441] {
        let t = assert_same_transcript(&format!("real-{chunk}"), &real, chunk, None);
        assert!(last_line(&t).contains(" chosen=1 "), "{}", last_line(&t));
        // the switch to R happens mid-clip: R's copy of a marker L already returned is not paired
        assert_no_double_return(&format!("real-{chunk}"), &t);
    }
    // The same switch, synthetic: L carries markers 1-3, R markers 0-3 488 samples later; in
    // 256-frame callbacks R's copies of markers 2 and 3 tip the pick after L returned them.
    let l = markers_at(&[1, 2, 3], SR / 2, SR / 4, 0, SR * 3);
    let r = markers_at(&[0, 1, 2, 3], SR / 2, SR / 4, 488, SR * 3);
    let t = assert_same_transcript("switch", &[l, r], 256, None);
    assert_no_double_return("switch", &t);
    assert!(
        last_line(&t).contains(" chosen=1 clusters=3,4 "),
        "{}",
        last_line(&t)
    );
    assert_eq!(returned(&t).len(), 3, "L's three markers, each once");
    // The filter drops only the SAME marker: a different index inside the dedup gap on the newly
    // chosen channel (L's lone index-200 marker, then R's third chain marker 300 samples later)
    // is still returned.
    let r = markers_at(&[0, 1, 2], SR / 2, SR / 4, 0, SR * 2);
    let mut l = vec![0.0f32; SR * 2];
    let at = SR / 4 + 2 * (SR / 2) - 300;
    for (i, &s) in marker_signal(200, &AudioParams::rig60()).iter().enumerate() {
        l[at + i] += s * 0.25;
    }
    let t = assert_same_transcript("switch-other", &[l, r], 256, None);
    let got = returned(&t);
    assert_eq!(got.len(), 2, "{got:?}");
    assert_eq!(got[0].1, 200, "{got:?}");
    assert!(last_line(&t).contains(" chosen=1 "), "{}", last_line(&t));
    // A decode flood (one marker every 1200 samples, more than the cap): both mirrors keep only
    // the newest markers, so the cluster stops at the cap.
    let n = DOCK_CHANNEL_PICK_MAX_MARKERS + 40;
    let ks: Vec<usize> = (0..n).collect();
    let flood = vec![markers_at(&ks, 1200, SR / 4, 0, SR / 2 + n * 1200)];
    let t = assert_same_transcript("flood", &flood, 1024, None);
    assert_eq!(returned(&t).len(), n);
    assert!(
        last_line(&t).contains(&format!(" clusters={DOCK_CHANNEL_PICK_MAX_MARKERS} ")),
        "{}",
        last_line(&t)
    );
    // Both channels decode, R later and with the longer chain: L stays chosen.
    let l = marker_track(6, 1.0, 0.25, 0, SR * 9);
    let r = marker_track(8, 1.0, 0.25, 488, SR * 9);
    let t = assert_same_transcript("both-clear", &[l, r], 1024, None);
    assert!(
        last_line(&t).contains(" chosen=0 clusters=6,8 "),
        "{}",
        last_line(&t)
    );
    // The marker only on R.
    let silent = vec![0.0f32; SR * 9];
    let r = marker_track(8, 1.0, 0.25, 488, SR * 9);
    let t = assert_same_transcript("r-only", &[silent, r], 1024, None);
    assert!(
        last_line(&t).contains(" chosen=1 clusters=0,8 "),
        "{}",
        last_line(&t)
    );
    // L stops after 8 s, R carries on: once L ages out of the 25 s window, R takes over.
    let len = SR * 40;
    let l = marker_track(8, 1.0, 0.25, 0, len);
    let r = marker_track(40, 1.0, 0.25, 488, len);
    let t = assert_same_transcript("hand-over", &[l, r], 1024, None);
    let at_8s = t.lines().nth(8 * SR / 1024).expect("a push at 8 s");
    assert!(at_8s.contains(" chosen=0 "), "{at_8s}");
    assert!(last_line(&t).contains(" chosen=1 "), "{}", last_line(&t));
    // Three channels, the marker on the third only; and a mono track.
    let three = vec![
        vec![0.0f32; SR * 6],
        vec![0.0f32; SR * 6],
        marker_track(5, 1.0, 0.25, 0, SR * 6),
    ];
    let t = assert_same_transcript("three", &three, 1024, None);
    assert!(last_line(&t).contains(" chosen=2 "), "{}", last_line(&t));
    let mono = vec![marker_track(5, 1.0, 0.25, 0, SR * 6)];
    let t = assert_same_transcript("mono", &mono, 1024, None);
    assert!(
        last_line(&t).contains(" chosen=0 clusters=5 "),
        "{}",
        last_line(&t)
    );

    // The window boundary: a marker exactly `window` samples older than the samples pushed so far
    // is still in the window; one push later it is out. Three markers (cluster 3) on one channel,
    // the window set so the first one sits exactly on the boundary after push k - 1.
    let track = vec![marker_track(3, 1.0, 0.25, 0, SR * 4)];
    let first = rust_transcript(&track, 1024, None)
        .lines()
        .find_map(|l| {
            let m = l.split(" markers=").nth(1)?.split(' ').next()?;
            m.split(':').next()?.parse::<u64>().ok()
        })
        .expect("the first marker's absolute sample index");
    let k = 107u64; // push k - 1 ends at 107 x 1024 samples, after the third marker (2.25 s)
    let window = k * 1024 - first;
    let t = assert_same_transcript("boundary", &track, 1024, Some(window));
    let line = |i: u64| t.lines().nth(i as usize).expect("push").to_string();
    assert!(line(k - 1).contains(" clusters=3 "), "{}", line(k - 1));
    assert!(line(k).contains(" clusters=0 "), "{}", line(k));
}

#[test]
fn the_constants_are_single_sourced() {
    let consts = run_tool(&["consts"]);
    let get = |name: &str| -> String {
        consts
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{name} ")))
            .unwrap_or_else(|| panic!("the tool prints no {name}: {consts}"))
            .to_string()
    };
    let p = AudioParams::rig60();
    let cl = ClusterParams::default();
    assert_eq!(get("min_clusters"), DEFAULT_MIN_CLUSTERS.to_string());
    assert_eq!(get("pick_window_s"), DOCK_CHANNEL_PICK_WINDOW_S.to_string());
    assert_eq!(
        get("max_markers"),
        DOCK_CHANNEL_PICK_MAX_MARKERS.to_string()
    );
    assert_eq!(get("step_tol"), cl.step_tol.to_string());
    assert_eq!(get("gap_ratio").parse::<f64>().ok(), Some(cl.gap_ratio));
    assert_eq!(
        get("qpsk_threshold").parse::<f64>().ok(),
        Some(DOCK_QPSK_THRESHOLD)
    );
    assert_eq!(get("sample_rate"), p.sample_rate.to_string());
    assert_eq!(get("carrier_hz"), p.carrier_hz.to_string());
    assert_eq!(get("c"), p.c.to_string());

    // The [4b3/8] preflight's shell defaults are the same floor and the same window.
    let out = Command::new("bash")
        .arg("-c")
        .arg(
            "set -euo pipefail; source \"$1\"; marker_decodability_default_min_clusters; \
             marker_decodability_default_probe_secs",
        )
        .arg("bash")
        .arg(repo(PREFLIGHT_LIB))
        .output()
        .expect("run bash");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let shell = String::from_utf8(out.stdout).expect("utf-8");
    let shell: Vec<&str> = shell.lines().collect();
    assert_eq!(
        shell,
        [
            DEFAULT_MIN_CLUSTERS.to_string(),
            DOCK_CHANNEL_PICK_WINDOW_S.to_string()
        ]
    );
}
