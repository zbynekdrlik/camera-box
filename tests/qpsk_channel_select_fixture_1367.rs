//! Issue 1367 — the measurement-audio QPSK decode must not downmix stereo: a REAL-signal fixture.
//!
//! `tests/fixtures/mbc-stereo-skew-1367/mbc-stereo-2s.wav` is the first 2 s of the stream program
//! recording `2026-09-27 14-29-50.mp4` (release E2E run 36317806422, PR 1376 at 1.7.0-dev.712), the
//! `mbc` track as 48 kHz stereo s16. The SAME cam2 QPSK marker is on both channels, R 488 samples
//! (10.17 ms) behind L, zero-lag L/R correlation 0.005. The CI-built dev-712 `--qpsk-probe` read this
//! window as: stereo (downmixed) cluster 0 POLLUTED, L alone cluster 3 (below the floor of 4), R
//! alone cluster 4 OK. So BOTH the downmix and a fixed "always channel 0" pick fail here, and only
//! the per-channel best pick passes.
//!
//! Default features, no ffmpeg, no rig: the WAV is parsed here and fed to the same crate-root demod
//! and channel pick the probe-gated `recording-verdict` glue calls.

use camera_box::qpsk_channel_select::{
    channel_probe_report, decode_best_channel, deinterleave, ChannelProbeReport,
};
use camera_box::qpsk_marker::{decode_markers_with_stats, AudioParams};
use camera_box::qpsk_probe_decision::{
    consistency_cluster_size, ClusterParams, QpskProbeThresholds, QpskProbeVerdict,
};

/// The `--av-threshold` default of `recording-verdict` (the preflight passes no override).
const PROBE_THRESHOLD: f64 = 0.35;

fn th() -> QpskProbeThresholds {
    QpskProbeThresholds {
        min_clusters: 4,
        silent_db: -60.0,
        loud_db: -20.0,
    }
}

/// Minimal RIFF/WAVE reader for the committed 16-bit PCM fixture: returns (channels, sample_rate,
/// interleaved f32). s16 → f32 is x / 32768, the conversion ffmpeg's `-f f32le` applies.
fn read_pcm16_wav(path: &std::path::Path) -> (usize, u32, Vec<f32>) {
    let b = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    assert_eq!(&b[0..4], b"RIFF", "not a RIFF file");
    assert_eq!(&b[8..12], b"WAVE", "not a WAVE file");
    let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    let mut pos = 12;
    let mut fmt: Option<(usize, u32, u16)> = None;
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let len = u32_at(pos + 4) as usize;
        let body = pos + 8;
        if id == b"fmt " {
            assert_eq!(u16_at(body), 1, "PCM only");
            fmt = Some((
                u16_at(body + 2) as usize,
                u32_at(body + 4),
                u16_at(body + 14),
            ));
        } else if id == b"data" {
            let (channels, rate, bits) = fmt.expect("fmt chunk before data");
            assert_eq!(bits, 16, "16-bit PCM only");
            let data = &b[body..(body + len).min(b.len())];
            let samples = data
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
                .collect();
            return (channels, rate, samples);
        }
        pos = body + len + (len & 1);
    }
    panic!("no data chunk in {}", path.display());
}

fn fixture() -> (Vec<f32>, Vec<f32>) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mbc-stereo-skew-1367/mbc-stereo-2s.wav");
    let (channels, rate, interleaved) = read_pcm16_wav(&path);
    assert_eq!(channels, 2, "the fixture is the stereo mbc track");
    assert_eq!(rate, 48_000);
    let mut ch = deinterleave(&interleaved, channels);
    assert_eq!(ch[0].len(), 96_000, "2 s at 48 kHz");
    let r = ch.pop().expect("R");
    let l = ch.pop().expect("L");
    (l, r)
}

fn probe(channels: &[Vec<f32>]) -> ChannelProbeReport {
    let best = decode_best_channel(
        channels,
        &AudioParams::rig60(),
        PROBE_THRESHOLD,
        ClusterParams::default(),
    )
    .expect("at least one channel");
    channel_probe_report(&best, &th())
}

#[test]
fn real_skewed_stereo_downmix_is_not_decodable() {
    // The pre-1367 path (`-ac 1`, then decode): documents WHY the downmix must go.
    let (l, r) = fixture();
    let mono: Vec<f32> = l.iter().zip(&r).map(|(a, b)| (a + b) / 2.0).collect();
    let (markers, _) = decode_markers_with_stats(&mono, &AudioParams::rig60(), PROBE_THRESHOLD);
    let cluster = consistency_cluster_size(&markers, ClusterParams::default());
    assert!(
        cluster < 4,
        "the 10 ms-skewed mono sum must not reach the decodability floor (cluster {cluster})"
    );
}

#[test]
fn real_skewed_stereo_picks_the_decodable_channel() {
    let (l, r) = fixture();
    let rep = probe(&[l, r]);
    let per = &rep.pick.per_channel;
    assert_eq!(per.len(), 2, "{rep:?}");
    assert!(
        per[0].cluster_samples < 4,
        "L alone stays below the floor in this window, so 'always channel 0' would fail: {per:?}"
    );
    assert!(
        per[1].cluster_samples >= 4,
        "R alone decodes the cadence: {per:?}"
    );
    assert_eq!(rep.pick.chosen_channel, 1, "the best channel is R: {per:?}");
    assert_eq!(rep.report.cluster_samples, per[1].cluster_samples);
    assert_eq!(rep.report.crc_ok, per[1].crc_ok);
    assert_eq!(rep.report.preamble_screens, per[1].preamble_screens);
    assert_eq!(
        rep.report.verdict,
        QpskProbeVerdict::Ok,
        "the gate reads the real marker as decodable: {rep:?}"
    );
}

#[test]
fn real_single_channel_is_the_identity() {
    // A mono track (here: R alone) decodes exactly as the single-buffer path did.
    let (_, r) = fixture();
    let rep = probe(std::slice::from_ref(&r));
    let (markers, stats) = decode_markers_with_stats(&r, &AudioParams::rig60(), PROBE_THRESHOLD);
    assert_eq!(rep.pick.chosen_channel, 0);
    assert_eq!(rep.report.crc_ok, stats.crc_ok);
    assert_eq!(rep.report.preamble_screens, stats.preamble_screens_passed);
    assert_eq!(
        rep.report.cluster_samples,
        consistency_cluster_size(&markers, ClusterParams::default())
    );
}
