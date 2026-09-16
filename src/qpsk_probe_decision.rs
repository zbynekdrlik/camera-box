//! Pure Tier-0 decision for the AUDIO-ONLY QPSK decodability probe (#1324).
//!
//! WHY crate root, not `src/probe/`: like `src/qpsk_marker.rs` / `src/colour_scale.rs`, this is
//! pure decision math with NO I/O and NO probe deps, so it compiles + unit-tests on DEFAULT
//! features (Tier-0). The probe-gated ffmpeg/WAV glue (`src/bin/recording-verdict.rs`
//! `--qpsk-probe`) reads the audio, runs `qpsk_marker::decode_markers_with_stats`, and hands the
//! decoded markers + stats + samples INTO this module for the verdict + JSON line.
//!
//! WHAT it decides (#1324 + supervisor correction 16.9.2026). The mbc measurement chain can sit at
//! a plausible LEVEL yet be UNDECODABLE — a marker moved off-axis, a foreign signal drowning it, a
//! format/routing fault. The #1323 level ceiling cannot catch that (the chain read −47.8 dB, inside
//! the band). This probe reuses the demod on a short audio-only capture and reports whether the
//! REAL cadence markers are decodable — the honest sibling of the #1323 level ceiling.
//!
//! WHY a SELF-CONSISTENCY cluster, not the raw decode count: on a drowned chain the demod fires
//! MORE CRC-valid decodes than a healthy one (real-data 16.9: failed runs 480/551 false decodes vs
//! the green run's 107 real), so a raw `crc_ok` gate PASSES the broken chain. What separates them
//! is SELF-CONSISTENCY: real markers arrive on ONE emit cadence (a steady inter-arrival gap G and a
//! fixed index-step S = the emitter's per-marker `frame_id`-increment mod 256), so their decoded
//! `(ts,index)` form a long chain sharing (S,G); false CRC decodes from noise scatter on BOTH axes
//! and yield a short chain. Measured over 20/25 s windows of the real recordings: GREEN chain
//! ∈ [5,9], FAILED ∈ [1,3] — a clean split at N=4. (S,G) are SELF-CALIBRATED from the window's own
//! decodes, so this bakes in NO fixed painter cadence (the rig runs ~180-step/~3 s today; the code
//! default is 300 → step 44) — the modal derivation follows whatever the emitter actually uses.
//!
//! DECODABILITY IS PRIMARY: `cluster_samples >= min_clusters ⇒ OK regardless of level` (a
//! loud-but-decodable capture is the healthy marker — issue 1323's −20 dB bar was miscalibrated on
//! the broken chain, so it is a COVARIATE here, never a standalone POLLUTED gate). Only when
//! UNDECODABLE do the level bars name WHY: below the #748 −60 floor ⇒ SILENT, above the −20 loud
//! covariate ⇒ POLLUTED (loud AND undecodable), else ⇒ UNDECODED.

use crate::qpsk_marker::DecodeStats;

/// The audio-only decodability verdict. The four words are pairwise DISJOINT substrings so the
/// `[4b3/8]` preflight abort (and any log grep) can name the exact class unambiguously.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QpskProbeVerdict {
    /// The real cadence markers are decodable (`cluster_samples >= min_clusters`), at ANY level.
    Ok,
    /// A plausible level, but the marker is not decodable (the target #1324 class).
    Undecoded,
    /// Below the #748 silence floor — the chain is (near-)silent, marker not even present.
    Silent,
    /// Above the loud covariate AND undecodable — a foreign signal is drowning the marker.
    Polluted,
}

impl QpskProbeVerdict {
    /// The uppercase word emitted in the JSON `verdict` field and matched by the shell abort.
    pub fn as_str(&self) -> &'static str {
        match self {
            QpskProbeVerdict::Ok => "OK",
            QpskProbeVerdict::Undecoded => "UNDECODED",
            QpskProbeVerdict::Silent => "SILENT",
            QpskProbeVerdict::Polluted => "POLLUTED",
        }
    }
}

/// The level bars + cluster floor the verdict keys on. `silent_db` / `loud_db` are READ from
/// `scripts/lib/audio-presence-preflight.sh` (`audio_preflight_default_threshold_db` /
/// `_default_ceiling_db`) and passed in — NEVER retyped here (the #748/#1323 single-source rule).
#[derive(Debug, Clone, Copy)]
pub struct QpskProbeThresholds {
    /// Minimum self-consistent cluster size to call the chain decodable (#1324 default 4).
    pub min_clusters: u64,
    /// The #748 silence floor (dBFS, default −60): below it ⇒ SILENT.
    pub silent_db: f64,
    /// The loud covariate (dBFS, default −20, issue 1323): above it AND undecodable ⇒ POLLUTED.
    pub loud_db: f64,
}

/// Tolerances for the self-consistency cluster. Defaults calibrated on the real 16.9 recordings.
#[derive(Debug, Clone, Copy)]
pub struct ClusterParams {
    /// Circular index-step match tolerance (mod 256). Real steps wobble ±1 (the ~180-frame cadence
    /// varies a frame per marker); ±3 absorbs that with margin while staying far below a random
    /// step's expected spread.
    pub step_tol: u32,
    /// Fractional inter-arrival gap tolerance around the modal gap G (and around 2·G for a single
    /// missed marker). 0.25 = ±25 %.
    pub gap_ratio: f64,
}

impl Default for ClusterParams {
    fn default() -> Self {
        ClusterParams {
            step_tol: 3,
            gap_ratio: 0.25,
        }
    }
}

/// Circular distance between two index values on the 0..256 ring.
fn circ_dist(a: u32, b: u32) -> u32 {
    let d = (a + 256 - b) % 256;
    d.min(256 - d)
}

/// The self-consistency cluster size (see the module docs). `markers` are the demod's
/// `(audio_ts_s, index)` decodes (any order). Returns the number of markers in the LONGEST chain of
/// consecutive-in-time markers that share the modal emit cadence (index-step S ± `step_tol` and
/// inter-arrival gap ≈ modal G, tolerating a single missed marker via 2S/2G). Fewer than 3 markers,
/// or no dominant step, ⇒ 0 (nothing decodable to speak of).
pub fn consistency_cluster_size(markers: &[(f64, u8)], p: ClusterParams) -> u64 {
    if markers.len() < 3 {
        return 0;
    }
    let mut m: Vec<(f64, u8)> = markers.to_vec();
    m.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let n = m.len();
    // Consecutive (gap, step) pairs. step = (idx_{k+1} − idx_k) mod 256.
    let pairs: Vec<(f64, u32)> = (0..n - 1)
        .map(|i| {
            let gap = m[i + 1].0 - m[i].0;
            let step = ((m[i + 1].1 as i32 - m[i].1 as i32).rem_euclid(256)) as u32;
            (gap, step)
        })
        .collect();
    // Modal step S: the step value maximizing the count of pairs within step_tol of it.
    let mut best_s: Option<u32> = None;
    let mut best_c = 0usize;
    for &(_, s) in &pairs {
        let c = pairs
            .iter()
            .filter(|&&(_, s2)| circ_dist(s2, s) <= p.step_tol)
            .count();
        if c > best_c {
            best_c = c;
            best_s = Some(s);
        }
    }
    let s = match best_s {
        Some(s) if best_c >= 2 => s,
        _ => return 0,
    };
    // Modal gap G = median of the gaps among step-matching pairs.
    let mut gaps: Vec<f64> = pairs
        .iter()
        .filter(|&&(g, s2)| circ_dist(s2, s) <= p.step_tol && g > 0.0)
        .map(|&(g, _)| g)
        .collect();
    if gaps.is_empty() {
        return 0;
    }
    gaps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let g_ref = gaps[gaps.len() / 2];
    let s2 = (2 * s) % 256;
    let ok = |gap: f64, step: u32| -> bool {
        let single = circ_dist(step, s) <= p.step_tol && (gap - g_ref).abs() <= p.gap_ratio * g_ref;
        let missed = circ_dist(step, s2) <= p.step_tol
            && (gap - 2.0 * g_ref).abs() <= p.gap_ratio * 2.0 * g_ref;
        single || missed
    };
    // Longest run of consecutive matching pairs → markers = run of pairs + 1.
    let mut best = 1u64;
    let mut cur = 1u64;
    for &(gap, step) in &pairs {
        if ok(gap, step) {
            cur += 1;
            best = best.max(cur);
        } else {
            cur = 1;
        }
    }
    best
}

/// Peak level in dBFS = 20·log10(max|sample|). Digital silence (all zeros) and any non-finite input
/// return the `-120.0` sentinel (a valid JSON number, well below the #748 −60 floor ⇒ SILENT).
pub fn peak_dbfs(samples: &[f32]) -> f64 {
    let mut peak = 0.0f32;
    for &x in samples {
        if x.is_finite() {
            let a = x.abs();
            if a > peak {
                peak = a;
            }
        }
    }
    if peak <= 0.0 {
        -120.0
    } else {
        (20.0 * (peak as f64).log10()).max(-120.0)
    }
}

/// The full audio-only decodability report (one JSON line per [`report_json`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QpskProbeReport {
    /// Onsets whose preamble screen crossed threshold — the demod's `preamble_screens_passed`. Huge
    /// (10^5+ per 20 s) on a noise-flooded chain, ~10^2 on a healthy one. Informative, not the gate.
    pub preamble_screens: u64,
    /// Decode ATTEMPTS = crc_ok + crc_fail (== preamble_screens by construction — every screen-pass
    /// is one CRC attempt; the recording verdict's per-cam `candidates` is a DIFFERENT number that
    /// comes from video pairing, absent here). Kept for schema continuity with the full verdict.
    pub candidates: u64,
    /// The self-consistency cluster size — the audio-only decodability count the verdict keys on.
    pub cluster_samples: u64,
    /// CRC-valid decoded markers (== `DecodeStats::crc_ok`).
    pub crc_ok: u64,
    /// Preamble/CRC failures (== `DecodeStats::crc_fail`).
    pub crc_fail: u64,
    /// Peak level (dBFS) of the analyzed window.
    pub peak_dbfs: f64,
    /// The decision.
    pub verdict: QpskProbeVerdict,
}

/// Decodability-primary classification (supervisor correction 16.9): decodable ⇒ OK at ANY level;
/// only when undecodable do the level covariates name the class.
pub fn classify(
    cluster_samples: u64,
    peak_dbfs: f64,
    th: &QpskProbeThresholds,
) -> QpskProbeVerdict {
    if cluster_samples >= th.min_clusters {
        QpskProbeVerdict::Ok
    } else if peak_dbfs < th.silent_db {
        QpskProbeVerdict::Silent
    } else if peak_dbfs > th.loud_db {
        QpskProbeVerdict::Polluted
    } else {
        QpskProbeVerdict::Undecoded
    }
}

/// Assemble the report from the demod outputs + the analyzed samples.
pub fn build_report(
    markers: &[(f64, u8)],
    stats: &DecodeStats,
    samples: &[f32],
    cluster: ClusterParams,
    th: &QpskProbeThresholds,
) -> QpskProbeReport {
    let cluster_samples = consistency_cluster_size(markers, cluster);
    let peak = peak_dbfs(samples);
    let verdict = classify(cluster_samples, peak, th);
    QpskProbeReport {
        preamble_screens: stats.preamble_screens_passed,
        candidates: stats.crc_ok + stats.crc_fail,
        cluster_samples,
        crc_ok: stats.crc_ok,
        crc_fail: stats.crc_fail,
        peak_dbfs: peak,
        verdict,
    }
}

/// The single JSON line the probe prints on stdout (parsed by `marker-decodability-preflight.sh`).
pub fn report_json(r: &QpskProbeReport) -> String {
    format!(
        "{{\"preamble_screens\":{},\"candidates\":{},\"cluster_samples\":{},\"crc_ok\":{},\"crc_fail\":{},\"peak_dbfs\":{:.1},\"verdict\":\"{}\"}}",
        r.preamble_screens,
        r.candidates,
        r.cluster_samples,
        r.crc_ok,
        r.crc_fail,
        r.peak_dbfs,
        r.verdict.as_str()
    )
}
