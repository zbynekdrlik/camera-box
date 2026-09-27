//! Issue 1367 — NEVER downmix the measurement audio before the QPSK demod: decode the marker on
//! EVERY channel of the track and keep the one that decodes best.
//!
//! WHY (measured 27.9.2026, release E2E run 36317806422 / PR 1376 at 1.7.0-dev.712). The stream
//! program recording's `mbc` audio is stereo (DVS ch1 → L, ch2 → R) and carries the SAME cam2 QPSK
//! marker on both channels, but R lags L by 10.17 ms (488 samples). One symbol is one 442 Hz carrier
//! cycle (2.26 ms), so the two copies sit ~4.5 symbols apart at ~178° carrier phase, nearly
//! anti-phase, and the mono sum smears every symbol into another: on the real 4 s clip of
//! `2026-09-27 14-29-50.mp4` the downmix (`-ac 1`)
//! read `preamble_screens 9212, cluster_samples 2, crc_ok 3` → POLLUTED, while L alone read
//! `cluster 7, crc_ok 7` and R alone `cluster 8, crc_ok 8`, both OK. The owner ruled the skew is not
//! ours to police ("je tam len nejaky sum ten by ti predsa nemal vadit") — the gate must be robust.
//!
//! WHAT. [`f32le_to_channels`] splits the channel-preserving ffmpeg extract; [`decode_channel`] runs the
//! UNCHANGED demod (`qpsk_marker::decode_markers_with_stats`) and the #1324 self-consistency cluster
//! (`qpsk_probe_decision::consistency_cluster_size`) on one channel; [`best_marker_channel`] picks
//! the channel with the LARGEST cluster, ties to the LOWEST index (deterministic); [`decode_best_channel`]
//! composes them. Every consumer — the `[4b3/8]` preflight (`--qpsk-probe`), the standalone
//! `--av-sync`, and the fused all-cambox A/V gate — decodes through this ONE pick, and the A/V
//! offset is paired from the CHOSEN channel's markers only, so a channel skew can never average two
//! arrival times. A mono track is the identity: one channel, chosen 0, the same decode as before.
//!
//! Level and silence are judged across the WHOLE track ([`ChannelPick::peak_dbfs`] /
//! [`ChannelPick::max_preamble_screens`] take the max over channels), decodability on the chosen
//! channel: a silent channel 0 must never read "the chain is silent" while channel 1 carries audio.
//!
//! Pure, no I/O, default features (Tier-0 testable). The ffmpeg/ffprobe glue is the probe-gated
//! `probe::av_sync_recording::extract_audio_channels_f32`.

use crate::qpsk_marker::{decode_markers_with_stats, AudioParams, DecodeStats};
use crate::qpsk_probe_decision::{
    classify, consistency_cluster_size, peak_dbfs, report_json_body, ClusterParams,
    QpskProbeReport, QpskProbeThresholds,
};
use serde::{Deserialize, Serialize};

/// What the demod read on ONE channel of the measurement track.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ChannelMarkerStats {
    /// 0-based channel index within the audio track (0 = L on a stereo track).
    pub channel: u32,
    /// Onsets whose preamble screen crossed the threshold (`DecodeStats::preamble_screens_passed`).
    pub preamble_screens: u64,
    /// CRC-valid decoded markers (`DecodeStats::crc_ok`).
    pub crc_ok: u64,
    /// Preamble/CRC failures (`DecodeStats::crc_fail`).
    pub crc_fail: u64,
    /// The #1324 self-consistency cluster size of this channel's decodes — the pick key.
    pub cluster_samples: u64,
    /// Peak level (dBFS) of this channel (`qpsk_probe_decision::peak_dbfs`).
    pub peak_dbfs: f64,
}

/// Which channel the marker decode used, and what every channel read. Carried in the qpsk-probe
/// JSON line, the `--av-sync` summary and the fused partial (`AvMarkerInputs`). `Default` (chosen 0,
/// no channels) is what an older partial without this field deserializes to — "not recorded".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChannelPick {
    /// The chosen channel's index (== `per_channel[i].channel` of the chosen entry).
    pub chosen_channel: u32,
    /// One entry per channel of the track, in channel order.
    pub per_channel: Vec<ChannelMarkerStats>,
}

impl ChannelPick {
    /// The chosen channel's stats (`None` for an empty/default pick).
    pub fn chosen(&self) -> Option<&ChannelMarkerStats> {
        self.per_channel
            .iter()
            .find(|c| c.channel == self.chosen_channel)
    }

    /// Peak level of the WHOLE track: the max over every channel. `-120.0` (the `peak_dbfs`
    /// silence sentinel) when there are no channels.
    pub fn peak_dbfs(&self) -> f64 {
        self.per_channel
            .iter()
            .map(|c| c.peak_dbfs)
            .fold(-120.0, f64::max)
    }

    /// The most preamble screens any channel saw — zero iff EVERY channel was silent, the #748
    /// silent-vs-undecoded discriminator's meaning on a multi-channel track.
    pub fn max_preamble_screens(&self) -> u64 {
        self.per_channel
            .iter()
            .map(|c| c.preamble_screens)
            .max()
            .unwrap_or(0)
    }

    /// One log line: `marker channel 1 of 2 (cluster ch0=3 ch1=4)`, or `marker channel pick not
    /// recorded` for an empty (older-partial) pick, which must never read like a real channel 0.
    pub fn summary_line(&self) -> String {
        if self.per_channel.is_empty() {
            return "marker channel pick not recorded".to_string();
        }
        let clusters: Vec<String> = self
            .per_channel
            .iter()
            .map(|c| format!("ch{}={}", c.channel, c.cluster_samples))
            .collect();
        format!(
            "marker channel {} of {} (cluster {})",
            self.chosen_channel,
            self.per_channel.len(),
            clusters.join(" ")
        )
    }
}

/// Parse the first line of `ffprobe -show_entries stream=channels -of default=nw=1:nk=1` into a
/// channel count. `None` for a missing, `N/A`, non-numeric or zero value (the glue fails loud).
pub fn parse_ffprobe_channels(s: &str) -> Option<usize> {
    match s.lines().next()?.trim().parse::<usize>() {
        Ok(n) if n > 0 => Some(n),
        _ => None,
    }
}

/// The `ffprobe` arguments (the input path goes LAST) that print the channel count of audio stream
/// `track` as one bare number, parsed by [`parse_ffprobe_channels`].
pub fn ffprobe_channels_args(track: u32) -> Vec<String> {
    vec![
        "-v".to_string(),
        "error".to_string(),
        "-select_streams".to_string(),
        format!("a:{track}"),
        "-show_entries".to_string(),
        "stream=channels".to_string(),
        "-of".to_string(),
        "default=nw=1:nk=1".to_string(),
    ]
}

/// The `ffmpeg` arguments that follow `-i <path>`: audio stream `track` as raw interleaved f32le at
/// `sample_rate`, with its OWN `channels` kept. `-ac` is pinned to the stream's probed channel count
/// so the raw stride is known — never a downmix (a mono stream is 1 because it IS mono).
pub fn ffmpeg_extract_args(track: u32, sample_rate: u32, channels: usize) -> Vec<String> {
    vec![
        "-map".to_string(),
        format!("0:a:{track}"),
        "-ac".to_string(),
        channels.to_string(),
        "-ar".to_string(),
        sample_rate.to_string(),
        "-f".to_string(),
        "f32le".to_string(),
        "-".to_string(),
    ]
}

/// Split ffmpeg's raw interleaved f32le output into `channels` per-channel buffers. Errors (never a
/// silent truncation) when there is no whole frame or the byte count is not a whole number of
/// `channels`-wide frames — the extract and the stride disagree.
pub fn f32le_to_channels(bytes: &[u8], channels: usize) -> Result<Vec<Vec<f32>>, String> {
    let frame_bytes = 4 * channels;
    if channels == 0 || bytes.len() < frame_bytes || !bytes.len().is_multiple_of(frame_bytes) {
        return Err(format!(
            "{} bytes is not a whole number of {channels}-channel f32 frames",
            bytes.len()
        ));
    }
    // Straight from the bytes into the per-channel buffers: a long recording's audio is hundreds of
    // MB, so no intermediate interleaved copy.
    let frames = bytes.len() / frame_bytes;
    let mut out: Vec<Vec<f32>> = (0..channels).map(|_| Vec::with_capacity(frames)).collect();
    for frame in bytes.chunks_exact(frame_bytes) {
        for (ch, s) in frame.chunks_exact(4).enumerate() {
            out[ch].push(f32::from_le_bytes([s[0], s[1], s[2], s[3]]));
        }
    }
    Ok(out)
}

/// The pick: the channel with the LARGEST self-consistency cluster; ties go to the LOWEST index so
/// the choice is deterministic. Returns a position into `per_channel`; `None` when it is empty. A
/// single (mono) channel is always position 0.
pub fn best_marker_channel(per_channel: &[ChannelMarkerStats]) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (i, c) in per_channel.iter().enumerate() {
        match best {
            Some(b) if per_channel[b].cluster_samples >= c.cluster_samples => {}
            _ => best = Some(i),
        }
    }
    best
}

/// One channel's full decode: its summary stats, its decoded markers and the raw demod counters.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelDecode {
    pub stats: ChannelMarkerStats,
    pub markers: Vec<(f64, u8)>,
    pub decode: DecodeStats,
}

/// Decode channel `channel` (its samples in `samples`) with the unchanged demod and measure its
/// self-consistency cluster.
pub fn decode_channel(
    channel: u32,
    samples: &[f32],
    p: &AudioParams,
    threshold: f64,
    cluster: ClusterParams,
) -> ChannelDecode {
    let (markers, decode) = decode_markers_with_stats(samples, p, threshold);
    let stats = ChannelMarkerStats {
        channel,
        preamble_screens: decode.preamble_screens_passed,
        crc_ok: decode.crc_ok,
        crc_fail: decode.crc_fail,
        cluster_samples: consistency_cluster_size(&markers, cluster),
        peak_dbfs: peak_dbfs(samples),
    };
    ChannelDecode {
        stats,
        markers,
        decode,
    }
}

/// The result of decoding every channel and keeping the best one.
#[derive(Debug, Clone, PartialEq)]
pub struct BestChannelDecode {
    /// The chosen channel + every channel's stats.
    pub pick: ChannelPick,
    /// The CHOSEN channel's decoded markers `(audio_ts_s, index)` — the only markers any A/V offset
    /// or decodability verdict may use.
    pub markers: Vec<(f64, u8)>,
    /// The CHOSEN channel's raw demod counters.
    pub stats: DecodeStats,
}

/// Decode every channel and keep the best ([`best_marker_channel`]). `None` when `channels` is
/// empty. A mono track decodes exactly as the pre-1367 single-buffer path did.
pub fn decode_best_channel(
    channels: &[Vec<f32>],
    p: &AudioParams,
    threshold: f64,
    cluster: ClusterParams,
) -> Option<BestChannelDecode> {
    let mut decoded: Vec<ChannelDecode> = channels
        .iter()
        .enumerate()
        .map(|(i, s)| decode_channel(i as u32, s, p, threshold, cluster))
        .collect();
    let per_channel: Vec<ChannelMarkerStats> = decoded.iter().map(|d| d.stats).collect();
    let chosen = best_marker_channel(&per_channel)?;
    let best = decoded.swap_remove(chosen);
    Some(BestChannelDecode {
        pick: ChannelPick {
            chosen_channel: best.stats.channel,
            per_channel,
        },
        markers: best.markers,
        stats: best.decode,
    })
}

/// The qpsk-probe report over a channel pick: the `QpskProbeReport` of the CHOSEN channel (its
/// counters + cluster), with the level covariate read over the whole track ([`ChannelPick::peak_dbfs`]).
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelProbeReport {
    pub report: QpskProbeReport,
    pub pick: ChannelPick,
}

/// Build the probe report from a [`BestChannelDecode`]. On a mono track the `report` is identical
/// to `qpsk_probe_decision::build_report` over that one channel.
pub fn channel_probe_report(
    best: &BestChannelDecode,
    th: &QpskProbeThresholds,
) -> ChannelProbeReport {
    let cluster_samples = best.pick.chosen().map_or(0, |c| c.cluster_samples);
    let peak = best.pick.peak_dbfs();
    ChannelProbeReport {
        report: QpskProbeReport {
            preamble_screens: best.stats.preamble_screens_passed,
            candidates: best.stats.crc_ok + best.stats.crc_fail,
            cluster_samples,
            crc_ok: best.stats.crc_ok,
            crc_fail: best.stats.crc_fail,
            peak_dbfs: peak,
            verdict: classify(cluster_samples, peak, th),
        },
        pick: best.pick.clone(),
    }
}

/// The ONE JSON line `--qpsk-probe` prints. Every pre-1367 key comes FIRST with its old meaning (the
/// chosen channel's counters), then the new keys: `channels`, `chosen_channel`, and `per_channel`
/// LAST. The per-channel keys carry a `ch_` prefix so the preflight's first-match grep on
/// `"cluster_samples":` / `"preamble_screens":` / `"peak_dbfs":` / `"verdict":` can never read a
/// per-channel value, whatever the key order.
pub fn channel_report_json(r: &ChannelProbeReport) -> String {
    let per: Vec<String> = r
        .pick
        .per_channel
        .iter()
        .map(|c| {
            format!(
                "{{\"channel\":{},\"ch_preamble_screens\":{},\"ch_cluster_samples\":{},\"ch_crc_ok\":{},\"ch_crc_fail\":{},\"ch_peak_dbfs\":{:.1}}}",
                c.channel, c.preamble_screens, c.cluster_samples, c.crc_ok, c.crc_fail, c.peak_dbfs
            )
        })
        .collect();
    format!(
        "{{{},\"channels\":{},\"chosen_channel\":{},\"per_channel\":[{}]}}",
        report_json_body(&r.report),
        r.pick.per_channel.len(),
        r.pick.chosen_channel,
        per.join(",")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qpsk_marker::marker_signal;
    use crate::qpsk_probe_decision::{build_report, report_json, QpskProbeVerdict};

    fn stat(channel: u32, cluster_samples: u64) -> ChannelMarkerStats {
        ChannelMarkerStats {
            channel,
            preamble_screens: 10,
            crc_ok: cluster_samples,
            crc_fail: 1,
            cluster_samples,
            peak_dbfs: -18.0,
        }
    }

    fn th() -> QpskProbeThresholds {
        QpskProbeThresholds {
            min_clusters: 4,
            silent_db: -60.0,
            loud_db: -20.0,
        }
    }

    /// `n` real QPSK markers from the crate's own emitter, every `cadence_s`, index stepping by
    /// `step`, scaled to `amp`, delayed by `delay` samples, in a silence-padded buffer of `len`.
    fn marker_track(
        n: usize,
        cadence_s: f64,
        step: u8,
        amp: f32,
        delay: usize,
        len: usize,
    ) -> Vec<f32> {
        let p = AudioParams::rig60();
        let cad = (cadence_s * p.sample_rate as f64) as usize;
        let lead = p.sample_rate as usize / 4;
        let mut buf = vec![0.0f32; len];
        for k in 0..n {
            let idx = 7u8.wrapping_add((k as u16 * step as u16) as u8);
            let off = lead + k * cad + delay;
            for (i, &s) in marker_signal(idx, &p).iter().enumerate() {
                if off + i < buf.len() {
                    buf[off + i] += s * amp;
                }
            }
        }
        buf
    }

    /// The measured 27.9.2026 shape: the same marker on L and R, R 10.17 ms (488 samples) late.
    fn skewed_stereo() -> (Vec<f32>, Vec<f32>) {
        let len = 48_000 * 5;
        (
            marker_track(8, 0.5, 30, 0.25, 0, len),
            marker_track(8, 0.5, 30, 0.25, 488, len),
        )
    }

    #[test]
    fn best_channel_is_the_largest_cluster() {
        assert_eq!(best_marker_channel(&[stat(0, 7), stat(1, 8)]), Some(1));
        assert_eq!(best_marker_channel(&[stat(0, 9), stat(1, 2)]), Some(0));
        assert_eq!(
            best_marker_channel(&[stat(0, 0), stat(1, 0), stat(2, 5)]),
            Some(2)
        );
    }

    #[test]
    fn best_channel_tie_goes_to_the_lowest_index() {
        assert_eq!(best_marker_channel(&[stat(0, 5), stat(1, 5)]), Some(0));
        assert_eq!(
            best_marker_channel(&[stat(0, 3), stat(1, 6), stat(2, 6)]),
            Some(1)
        );
        assert_eq!(best_marker_channel(&[stat(0, 0), stat(1, 0)]), Some(0));
    }

    #[test]
    fn best_channel_mono_is_the_identity_and_empty_is_none() {
        assert_eq!(best_marker_channel(&[stat(0, 0)]), Some(0));
        assert_eq!(best_marker_channel(&[stat(0, 11)]), Some(0));
        assert_eq!(best_marker_channel(&[]), None);
    }

    #[test]
    fn f32le_split_keeps_channel_order() {
        let bytes: Vec<u8> = [1.0f32, -1.0, 2.0, -2.0, 3.0, -3.0]
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .collect();
        assert_eq!(
            f32le_to_channels(&bytes, 3),
            Ok(vec![vec![1.0, -2.0], vec![-1.0, 3.0], vec![2.0, -3.0]])
        );
        assert_eq!(
            f32le_to_channels(&bytes, 2),
            Ok(vec![vec![1.0, 2.0, 3.0], vec![-1.0, -2.0, -3.0]])
        );
    }

    #[test]
    fn ffprobe_channel_count_parses_and_rejects_junk() {
        assert_eq!(parse_ffprobe_channels("2\n"), Some(2));
        assert_eq!(parse_ffprobe_channels(" 1 \r\n"), Some(1));
        assert_eq!(parse_ffprobe_channels("6\n2\n"), Some(6));
        assert_eq!(parse_ffprobe_channels("N/A"), None);
        assert_eq!(parse_ffprobe_channels("0"), None);
        assert_eq!(parse_ffprobe_channels(""), None);
    }

    #[test]
    fn extract_args_keep_the_stream_own_channel_count() {
        let a = ffmpeg_extract_args(0, 48_000, 2);
        assert_eq!(
            a,
            ["-map", "0:a:0", "-ac", "2", "-ar", "48000", "-f", "f32le", "-"]
        );
        // a mono stream stays mono; a stereo stream is never pinned to 1
        assert!(ffmpeg_extract_args(3, 48_000, 1)
            .windows(2)
            .any(|w| w == ["-ac", "1"]));
        for ch in [2usize, 6] {
            let a = ffmpeg_extract_args(0, 48_000, ch);
            assert!(!a.windows(2).any(|w| w == ["-ac", "1"]), "{a:?}");
            assert!(
                !a.iter().any(|s| s.contains("pan=") || s.contains("amix")),
                "{a:?}"
            );
        }
        assert_eq!(
            ffprobe_channels_args(1),
            [
                "-v",
                "error",
                "-select_streams",
                "a:1",
                "-show_entries",
                "stream=channels",
                "-of",
                "default=nw=1:nk=1"
            ]
        );
    }

    #[test]
    fn f32le_bytes_split_into_channels_or_fail_loud() {
        let frames: Vec<u8> = [0.5f32, -0.25, 1.0, -1.0]
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .collect();
        assert_eq!(
            f32le_to_channels(&frames, 2),
            Ok(vec![vec![0.5, 1.0], vec![-0.25, -1.0]])
        );
        assert_eq!(
            f32le_to_channels(&frames, 1),
            Ok(vec![vec![0.5, -0.25, 1.0, -1.0]])
        );
        assert!(
            f32le_to_channels(&frames, 3).is_err(),
            "16 bytes are not whole 3-ch frames"
        );
        assert!(
            f32le_to_channels(&frames[..6], 1).is_err(),
            "not whole f32 samples"
        );
        assert!(f32le_to_channels(&[], 2).is_err(), "no frame at all");
        assert!(f32le_to_channels(&frames, 0).is_err());
    }

    #[test]
    fn skewed_stereo_mono_sum_fails_but_the_per_channel_pick_decodes() {
        let p = AudioParams::rig60();
        let cl = ClusterParams::default();
        let (l, r) = skewed_stereo();
        // The pre-1367 path: downmix, then decode.
        let mono: Vec<f32> = l.iter().zip(&r).map(|(a, b)| (a + b) / 2.0).collect();
        let (m_markers, _) = decode_markers_with_stats(&mono, &p, 0.35);
        let mono_cluster = consistency_cluster_size(&m_markers, cl);
        assert!(
            mono_cluster < 4,
            "the 10 ms-skewed mono sum must NOT decode (cluster {mono_cluster})"
        );
        // Each channel alone decodes the full cadence.
        for (i, ch) in [&l, &r].into_iter().enumerate() {
            let d = decode_channel(i as u32, ch, &p, 0.35, cl);
            assert_eq!(
                d.stats.cluster_samples, 8,
                "channel {i} alone must decode: {:?}",
                d.stats
            );
        }
        // The pick decodes, and the chosen markers are the chosen channel's own (never a mix).
        let best =
            decode_best_channel(&[l.clone(), r.clone()], &p, 0.35, cl).expect("two channels");
        assert_eq!(best.pick.per_channel.len(), 2);
        assert_eq!(
            best.pick.chosen_channel, 0,
            "an exact tie goes to the lowest index"
        );
        assert_eq!(best.markers, decode_markers_with_stats(&l, &p, 0.35).0);
        let rep = channel_probe_report(&best, &th());
        assert_eq!(rep.report.verdict, QpskProbeVerdict::Ok, "{rep:?}");
        assert_eq!(rep.report.cluster_samples, 8);
    }

    #[test]
    fn marker_only_on_the_second_channel_is_picked() {
        // Approach 2 of the design (always channel 0) breaks the day the marker rides only on R.
        let p = AudioParams::rig60();
        let (_, r) = skewed_stereo();
        let silent = vec![0.0f32; r.len()];
        let best = decode_best_channel(&[silent, r.clone()], &p, 0.35, ClusterParams::default())
            .expect("two channels");
        assert_eq!(best.pick.chosen_channel, 1);
        assert_eq!(best.markers, decode_markers_with_stats(&r, &p, 0.35).0);
        assert_eq!(best.pick.per_channel[0].cluster_samples, 0);
        assert_eq!(best.pick.per_channel[0].peak_dbfs, -120.0);
        // the A/V offset is paired from R's arrival times: the first marker sits 10 ms late
        let first = best.markers.first().expect("markers").0;
        assert!(
            (first - (0.25 + 488.0 / 48_000.0)).abs() < 0.002,
            "first marker at {first}"
        );
    }

    #[test]
    fn mono_track_report_is_identical_to_the_single_buffer_report() {
        let p = AudioParams::rig60();
        let (l, _) = skewed_stereo();
        let (markers, stats) = decode_markers_with_stats(&l, &p, 0.35);
        let old = build_report(&markers, &stats, &l, ClusterParams::default(), &th());
        let best =
            decode_best_channel(std::slice::from_ref(&l), &p, 0.35, ClusterParams::default())
                .expect("one channel");
        let new = channel_probe_report(&best, &th());
        assert_eq!(
            new.report, old,
            "a mono track must behave exactly as before"
        );
        assert_eq!(best.pick.chosen_channel, 0);
        let j = channel_report_json(&new);
        let old_j = report_json(&old);
        assert!(
            j.starts_with(&old_j[..old_j.len() - 1]),
            "every pre-1367 key keeps its place and value:\n{j}\n{old_j}"
        );
        assert!(j.ends_with(",\"channels\":1,\"chosen_channel\":0,\"per_channel\":[{\"channel\":0,\"ch_preamble_screens\":8,\"ch_cluster_samples\":8,\"ch_crc_ok\":8,\"ch_crc_fail\":0,\"ch_peak_dbfs\":-12.0}]}"), "{j}");
    }

    #[test]
    fn empty_track_has_no_pick() {
        let p = AudioParams::rig60();
        assert!(decode_best_channel(&[], &p, 0.35, ClusterParams::default()).is_none());
    }

    #[test]
    fn stereo_json_keeps_top_level_keys_first_and_prefixes_the_per_channel_ones() {
        let pick = ChannelPick {
            chosen_channel: 1,
            per_channel: vec![
                ChannelMarkerStats {
                    channel: 0,
                    preamble_screens: 649,
                    crc_ok: 3,
                    crc_fail: 646,
                    cluster_samples: 3,
                    peak_dbfs: -17.8,
                },
                ChannelMarkerStats {
                    channel: 1,
                    preamble_screens: 4,
                    crc_ok: 4,
                    crc_fail: 0,
                    cluster_samples: 4,
                    peak_dbfs: -17.9,
                },
            ],
        };
        let best = BestChannelDecode {
            pick,
            markers: vec![],
            stats: DecodeStats {
                preamble_screens_passed: 4,
                crc_ok: 4,
                crc_fail: 0,
            },
        };
        let r = channel_probe_report(&best, &th());
        assert_eq!(r.report.cluster_samples, 4);
        assert_eq!(r.report.verdict, QpskProbeVerdict::Ok);
        assert_eq!(
            r.report.peak_dbfs, -17.8,
            "the level covariate is the whole track's peak"
        );
        let j = channel_report_json(&r);
        assert!(!j.contains('\n'));
        assert_eq!(
            j,
            "{\"preamble_screens\":4,\"candidates\":4,\"cluster_samples\":4,\"crc_ok\":4,\"crc_fail\":0,\"peak_dbfs\":-17.8,\"verdict\":\"OK\",\"channels\":2,\"chosen_channel\":1,\"per_channel\":[{\"channel\":0,\"ch_preamble_screens\":649,\"ch_cluster_samples\":3,\"ch_crc_ok\":3,\"ch_crc_fail\":646,\"ch_peak_dbfs\":-17.8},{\"channel\":1,\"ch_preamble_screens\":4,\"ch_cluster_samples\":4,\"ch_crc_ok\":4,\"ch_crc_fail\":0,\"ch_peak_dbfs\":-17.9}]}"
        );
        // the shell's first-match grep keys occur exactly once, all before per_channel
        let cut = j.find("\"per_channel\"").expect("per_channel key");
        for key in [
            "\"preamble_screens\":",
            "\"cluster_samples\":",
            "\"peak_dbfs\":",
            "\"verdict\":",
        ] {
            assert_eq!(j.matches(key).count(), 1, "{key} must occur once in {j}");
            assert!(
                j.find(key).expect("key") < cut,
                "{key} must precede per_channel"
            );
        }
    }

    #[test]
    fn pick_helpers_read_the_whole_track() {
        let pick = ChannelPick {
            chosen_channel: 1,
            per_channel: vec![
                ChannelMarkerStats {
                    peak_dbfs: -61.0,
                    preamble_screens: 0,
                    ..stat(0, 0)
                },
                ChannelMarkerStats {
                    peak_dbfs: -30.0,
                    preamble_screens: 12,
                    ..stat(1, 2)
                },
            ],
        };
        assert_eq!(pick.chosen().map(|c| c.channel), Some(1));
        assert_eq!(pick.peak_dbfs(), -30.0);
        assert_eq!(pick.max_preamble_screens(), 12);
        assert_eq!(
            pick.summary_line(),
            "marker channel 1 of 2 (cluster ch0=0 ch1=2)"
        );
        let none = ChannelPick::default();
        assert!(none.chosen().is_none());
        assert_eq!(none.peak_dbfs(), -120.0);
        assert_eq!(none.max_preamble_screens(), 0);
    }

    #[test]
    fn an_empty_pick_reads_not_recorded_never_channel_0() {
        // An older partial carries no pick; its log line must not read like a real channel-0 pick.
        assert_eq!(
            ChannelPick::default().summary_line(),
            "marker channel pick not recorded"
        );
    }

    #[test]
    fn probe_report_candidates_are_the_chosen_channel_attempts() {
        // candidates = crc_ok + crc_fail of the CHOSEN channel (a non-zero crc_fail pins the sum).
        let pick = ChannelPick {
            chosen_channel: 0,
            per_channel: vec![ChannelMarkerStats {
                preamble_screens: 12,
                crc_ok: 5,
                crc_fail: 7,
                ..stat(0, 5)
            }],
        };
        let best = BestChannelDecode {
            pick,
            markers: vec![],
            stats: DecodeStats {
                preamble_screens_passed: 12,
                crc_ok: 5,
                crc_fail: 7,
            },
        };
        let r = channel_probe_report(&best, &th());
        assert_eq!(r.report.candidates, 12);
        assert_eq!(r.report.crc_fail, 7);
        assert!(channel_report_json(&r).starts_with(
            "{\"preamble_screens\":12,\"candidates\":12,\"cluster_samples\":5,\"crc_ok\":5,\"crc_fail\":7,"
        ));
    }
}
