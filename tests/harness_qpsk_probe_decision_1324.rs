//! #1324 — the AUDIO-ONLY QPSK decodability probe's PURE decision (crate-root
//! `qpsk_probe_decision`, default features, Tier-0 — no probe feature, no ffmpeg, no rig).
//!
//! Locks the four things the `[4b3/8]` preflight relies on. A healthy emit-cadence chain clusters
//! fully; scattered false decodes do NOT (the real-data discriminator — a drowned chain decodes
//! MORE markers, but they don't self-cluster). A single missed marker does not break the chain.
//! DECODABILITY-PRIMARY classification: decodable is OK at ANY level (loud + decodable = OK, the
//! supervisor's pinned case), and only when undecodable do the level bars name SILENT / POLLUTED /
//! UNDECODED. The JSON line is single-line and carries every field with disjoint verdict words.

use camera_box::qpsk_marker::{decode_markers_with_stats, marker_signal, AudioParams, DecodeStats};
use camera_box::qpsk_probe_decision::{
    build_report, classify, consistency_cluster_size, peak_dbfs, report_json, ClusterParams,
    QpskProbeThresholds, QpskProbeVerdict,
};

/// A synthetic REAL-marker chain: `n` markers every `gap` s, index stepping by `step` (mod 256) —
/// mirroring the rig's ~3 s / ~180-step emitter cadence.
fn cadence_chain(n: usize, gap: f64, step: u8, start_idx: u8, t0: f64) -> Vec<(f64, u8)> {
    (0..n)
        .map(|k| {
            (
                t0 + k as f64 * gap,
                start_idx.wrapping_add((k as u16 * step as u16) as u8),
            )
        })
        .collect()
}

fn th() -> QpskProbeThresholds {
    QpskProbeThresholds {
        min_clusters: 4,
        silent_db: -60.0,
        loud_db: -20.0,
    }
}

#[test]
fn healthy_cadence_chain_clusters_fully() {
    let m = cadence_chain(7, 3.0, 180, 189, 0.5);
    assert_eq!(consistency_cluster_size(&m, ClusterParams::default()), 7);
}

#[test]
fn scattered_false_decodes_do_not_cluster() {
    // Like a drowned chain's false CRC passes — no shared cadence on either axis.
    let m = vec![
        (0.3f64, 17u8),
        (0.9, 200),
        (1.1, 44),
        (3.7, 91),
        (4.0, 250),
        (5.2, 7),
        (9.9, 130),
        (11.0, 3),
    ];
    assert!(
        consistency_cluster_size(&m, ClusterParams::default()) < 4,
        "scattered false decodes must not form a decodable cluster"
    );
}

#[test]
fn tolerates_a_single_missed_marker() {
    let mut m = cadence_chain(8, 3.0, 180, 10, 0.0);
    m.remove(3);
    assert!(
        consistency_cluster_size(&m, ClusterParams::default()) >= 6,
        "a single dropped marker must not break the chain"
    );
}

#[test]
fn fewer_than_three_markers_is_zero() {
    assert_eq!(consistency_cluster_size(&[], ClusterParams::default()), 0);
    assert_eq!(
        consistency_cluster_size(&[(0.0, 1), (3.0, 181)], ClusterParams::default()),
        0
    );
}

#[test]
fn peak_dbfs_silence_is_floor_sentinel_and_full_scale_is_zero() {
    assert_eq!(peak_dbfs(&[0.0, 0.0, 0.0]), -120.0);
    assert_eq!(peak_dbfs(&[]), -120.0);
    assert!((peak_dbfs(&[0.0, -1.0, 0.5]) - 0.0).abs() < 1e-9);
    assert!((peak_dbfs(&[0.5, -0.25]) - (-6.02)).abs() < 0.05);
}

#[test]
fn decodable_is_ok_regardless_of_level() {
    // The supervisor's pinned case: LOUD + DECODABLE = OK (healthy marker reads ≈ −19 dB, ABOVE the
    // −20 covariate; decodability must win).
    assert_eq!(classify(7, -19.0, &th()), QpskProbeVerdict::Ok);
    assert_eq!(classify(5, -47.0, &th()), QpskProbeVerdict::Ok);
}

#[test]
fn undecodable_at_plausible_level_is_undecoded() {
    assert_eq!(classify(1, -47.0, &th()), QpskProbeVerdict::Undecoded);
    assert_eq!(classify(3, -47.0, &th()), QpskProbeVerdict::Undecoded); // 3 < 4
}

#[test]
fn undecodable_below_floor_is_silent() {
    assert_eq!(classify(0, -91.0, &th()), QpskProbeVerdict::Silent);
}

#[test]
fn undecodable_and_loud_is_polluted() {
    assert_eq!(classify(2, -5.0, &th()), QpskProbeVerdict::Polluted);
}

#[test]
fn verdict_words_are_disjoint() {
    let ws = [
        QpskProbeVerdict::Ok.as_str(),
        QpskProbeVerdict::Undecoded.as_str(),
        QpskProbeVerdict::Silent.as_str(),
        QpskProbeVerdict::Polluted.as_str(),
    ];
    for (i, a) in ws.iter().enumerate() {
        for (j, b) in ws.iter().enumerate() {
            if i != j {
                assert!(!a.contains(b) && !b.contains(a), "{a} vs {b} not disjoint");
            }
        }
    }
}

/// Synthesize a cadence of REAL QPSK marker waveforms at the rig timing (~`cadence_s` apart, index
/// stepping by `step` mod 256) scaled to `amp`, into an f32 buffer padded with silence — a
/// stand-in for a healthy mbc capture (48 kHz, 442 Hz, c=1).
fn synth_cadence_audio(n: usize, cadence_s: f64, step: u8, start_idx: u8, amp: f32) -> Vec<f32> {
    let p = AudioParams::rig60();
    let cad = (cadence_s * p.sample_rate as f64) as usize;
    let lead = p.sample_rate as usize / 2; // half-second lead before the first marker
    let mut buf = vec![0.0f32; cad * n + p.sample_rate as usize];
    for k in 0..n {
        let idx = start_idx.wrapping_add((k as u16 * step as u16) as u8);
        let sig = marker_signal(idx, &p);
        let off = k * cad + lead;
        for (i, &s) in sig.iter().enumerate() {
            if off + i < buf.len() {
                buf[off + i] = s * amp;
            }
        }
    }
    buf
}

#[test]
fn synthesized_marker_audio_decodes_and_loud_is_still_ok() {
    // The FULL audio → demod → decision path (all default-feature / Tier-0). The supervisor's pinned
    // case: a LOUD capture (amp 0.5 ≈ −6 dBFS, above the −20 loud covariate) that is DECODABLE must
    // be OK, never POLLUTED — decodability wins over level.
    let p = AudioParams::rig60();
    let audio = synth_cadence_audio(8, 3.0, 180, 189, 0.5);
    let (markers, stats) = decode_markers_with_stats(&audio, &p, 0.35);
    let r = build_report(&markers, &stats, &audio, ClusterParams::default(), &th());
    assert!(
        r.cluster_samples >= 4,
        "a synthetic healthy cadence must cluster (got {r:?})"
    );
    assert!(r.peak_dbfs > -20.0, "amp 0.5 is loud (> −20): {}", r.peak_dbfs);
    assert_eq!(
        r.verdict,
        QpskProbeVerdict::Ok,
        "loud + decodable must be OK, never POLLUTED ({r:?})"
    );
}

#[test]
fn build_report_and_json_line_carry_every_field() {
    // A healthy synthetic window: a real cadence chain + a matching DecodeStats + loud-ish samples.
    let markers = cadence_chain(7, 3.0, 180, 189, 0.5);
    let stats = DecodeStats {
        preamble_screens_passed: 190,
        crc_ok: 107,
        crc_fail: 83,
    };
    let samples = vec![0.11f32, -0.12, 0.10]; // ≈ −19 dBFS
    let r = build_report(&markers, &stats, &samples, ClusterParams::default(), &th());
    assert_eq!(r.cluster_samples, 7);
    assert_eq!(r.crc_ok, 107);
    assert_eq!(r.candidates, 190);
    assert_eq!(r.verdict, QpskProbeVerdict::Ok);
    let j = report_json(&r);
    assert!(!j.contains('\n'));
    assert!(j.contains("\"cluster_samples\":7"));
    assert!(j.contains("\"preamble_screens\":190"));
    assert!(j.contains("\"verdict\":\"OK\""));
}
