//! Issue 1367 — the live A/V-sync dock decodes the QPSK marker on EVERY channel, never a mix.
//!
//! WHY. The stream box's `mbc` input is stereo and carries the same cam2 marker on L and R, with R
//! 10.17 ms behind L. The dock used to average the channels before its one streaming decoder
//! (`st_raw_audio_camera_box`, now in `vendor/av-sync-dock/src/sync-test-output-audio.cpp`), and that mono sum
//! comb-filters the two copies into an undecodable signal (the offline path measured cluster 2
//! POLLUTED on the downmix, L 7 and R 8 alone, release E2E run 36317806422). So the live
//! monitor-only LOCK-CORRECT suggestion saw the same broken signal the offline gate stopped using.
//!
//! WHAT. [`ChannelMarkerPicker`] runs one [`StreamingMarkerDecoder`] per channel and keeps each
//! channel's decoded markers over the last [`DOCK_CHANNEL_PICK_WINDOW_S`]. Each channel's
//! self-consistency cluster is `qpsk_probe_decision::consistency_cluster_size` over that window,
//! and the chosen channel is the ONE rule `qpsk_channel_select::pick_marker_channel` against
//! `qpsk_probe_decision::DEFAULT_MIN_CLUSTERS`: the lowest channel that clears the floor, else the
//! largest cluster, ties to the lowest. Only the chosen channel's markers go on to the ring pairing,
//! so two arrival times 10 ms apart are never averaged.
//!
//! This is the Rust REFERENCE of `vendor/av-sync-dock/src/camera-box-channel-pick.hpp`
//! (`cb_consistency_cluster_size`, `cb_pick_marker_channel`, `ChannelMarkerPicker`). The dock C++ is
//! compiled only on the Windows genlock build, so `tests/qpsk_channel_pick_parity_1367.rs` runs the
//! C++ mirror through `vendor/av-sync-dock/test/channel-pick-parity.cpp` and compares it with this
//! module, push by push, on the real stereo fixture and on synthetic tracks.
//!
//! Behaviour worth knowing:
//! - A mono input is the identity: one decoder, chosen 0, the same markers the single decoder gave.
//! - A marker a channel decodes while ANOTHER channel is chosen feeds nothing. When the marker rides
//!   only on R, R's first two markers (a chain needs three to count) are therefore not paired.
//! - The pick is re-applied after every push, so a channel that stops decoding hands over once its
//!   markers age out of the window and its cluster drops below the floor.
//! - A pick switch never pairs one physical marker twice: when the newly chosen channel's copy of
//!   the marker the previous channel just returned arrives (same index, within one dedup gap), it is
//!   dropped. Without this, R's copy of the marker that tips the pick to R came back 10.17 ms after
//!   L's copy of it (two of four markers on the committed 2 s fixture).
//! - Each channel keeps at most [`DOCK_CHANNEL_PICK_MAX_MARKERS`] (the newest), so the O(n²)
//!   cluster on the dock's audio decode worker (the OBS audio thread until issue 1381) stays
//!   bounded under a decode flood.

use crate::av_sync_dock::{StreamingMarkerDecoder, DOCK_QPSK_THRESHOLD};
use crate::qpsk_channel_select::pick_marker_channel;
use crate::qpsk_marker::{signal_len, AudioParams, DecodeStats};
use crate::qpsk_probe_decision::{consistency_cluster_size, ClusterParams, DEFAULT_MIN_CLUSTERS};
use std::collections::VecDeque;

/// The window (seconds) over which the dock judges each channel's decodability: the #1324
/// calibration window (the `[4b3/8]` preflight's `marker_decodability_default_probe_secs`, 25 s), in
/// which a healthy chain clusters >= 7 and a drowned one <= 3 against the floor of 4. Mirrored by
/// `CB_CHANNEL_PICK_WINDOW_S`.
pub const DOCK_CHANNEL_PICK_WINDOW_S: u64 = 25;

/// The most markers one channel keeps in its pick window (the newest). The self-consistency
/// cluster is O(n²) and runs on the dock's audio decode worker: a real chain puts ~8 markers in 25 s, but a
/// decode flood can reach one per dedup gap (~1100 in 25 s, ~1.4 ms per recompute). 256 keeps a
/// recompute near 0.1 ms and never touches a real chain. Mirrored by `CB_CHANNEL_PICK_MAX_MARKERS`.
pub const DOCK_CHANNEL_PICK_MAX_MARKERS: usize = 256;

/// The dock's per-channel marker decode + channel pick (module docs). Every channel advances by the
/// same number of samples per [`push`](Self::push), so one absolute sample index serves all of them.
pub struct ChannelMarkerPicker {
    /// One streaming decoder per channel, in channel order.
    decoders: Vec<StreamingMarkerDecoder>,
    /// Per channel: the markers `(absolute sample index, index)` decoded within the last
    /// `window_samples`, oldest first.
    history: Vec<VecDeque<(u64, u8)>>,
    /// Per channel: the self-consistency cluster of `history`, recomputed when `history` changes.
    clusters: Vec<u64>,
    /// The chosen channel (0 until a channel has a cluster, and always 0 on a mono input).
    chosen: usize,
    /// Samples pushed per channel since construction.
    pushed: u64,
    sample_rate: u32,
    window_samples: u64,
    min_clusters: u64,
    /// The decoders' dedup gap: one physical marker's copies on two channels lie within it.
    min_gap: u64,
    /// The last marker `(absolute sample index, index)` this picker returned, from any channel.
    last_returned: Option<(u64, u8)>,
}

impl ChannelMarkerPicker {
    /// `channels` decoders, each a [`StreamingMarkerDecoder::new`]`(params, threshold, capacity,
    /// min_gap)`; the pick window is `window_samples`, the decodability floor `min_clusters`.
    pub fn new(
        channels: usize,
        params: AudioParams,
        threshold: f64,
        capacity: usize,
        min_gap: u64,
        window_samples: u64,
        min_clusters: u64,
    ) -> Self {
        Self {
            decoders: (0..channels)
                .map(|_| StreamingMarkerDecoder::new(params, threshold, capacity, min_gap))
                .collect(),
            history: vec![VecDeque::new(); channels],
            clusters: vec![0; channels],
            chosen: 0,
            pushed: 0,
            sample_rate: params.sample_rate,
            window_samples,
            min_clusters,
            min_gap,
            last_returned: None,
        }
    }

    /// The dock's configuration, the same one the glue builds (`ChannelMarkerPicker::dock` in the
    /// header): a decode window of three marker lengths, a dedup gap of one, the
    /// [`DOCK_QPSK_THRESHOLD`], a [`DOCK_CHANNEL_PICK_WINDOW_S`] pick window and the
    /// `DEFAULT_MIN_CLUSTERS` floor.
    pub fn dock(channels: usize, params: AudioParams) -> Self {
        let sig = signal_len(&params);
        Self::new(
            channels,
            params,
            DOCK_QPSK_THRESHOLD,
            sig * 3,
            sig as u64,
            DOCK_CHANNEL_PICK_WINDOW_S * params.sample_rate as u64,
            DEFAULT_MIN_CLUSTERS,
        )
    }

    /// Decode one callback's planar audio (`planes[c]` is channel c; every plane has the same
    /// length) on every channel, age the per-channel histories, re-apply the pick, and return the
    /// CHOSEN channel's newly decoded markers `(absolute sample index, index)`, minus the other
    /// channel's copy of the marker this picker returned last (a pick switch, module docs).
    ///
    /// Panics when `planes.len() != self.channels()`: the caller hands one plane per channel.
    pub fn push(&mut self, planes: &[&[f32]]) -> Vec<(u64, u8)> {
        assert_eq!(
            planes.len(),
            self.decoders.len(),
            "one audio plane per decoded channel"
        );
        let frames = planes.first().map_or(0, |p| p.len());
        let mut fresh: Vec<Vec<(u64, u8)>> = self
            .decoders
            .iter_mut()
            .zip(planes)
            .map(|(dec, plane)| dec.push(plane))
            .collect();
        self.pushed += frames as u64;
        let cutoff = self.pushed.saturating_sub(self.window_samples);
        let sr = self.sample_rate as f64;
        for (c, new) in fresh.iter().enumerate() {
            let history = &mut self.history[c];
            let mut changed = !new.is_empty();
            history.extend(new.iter().copied());
            while history.front().is_some_and(|&(abs, _)| abs < cutoff) {
                history.pop_front();
                changed = true;
            }
            while history.len() > DOCK_CHANNEL_PICK_MAX_MARKERS {
                history.pop_front();
                changed = true;
            }
            if changed {
                let markers: Vec<(f64, u8)> = history
                    .iter()
                    .map(|&(abs, idx)| (abs as f64 / sr, idx))
                    .collect();
                self.clusters[c] = consistency_cluster_size(&markers, ClusterParams::default());
            }
        }
        self.chosen = pick_marker_channel(&self.clusters, self.min_clusters).unwrap_or(0);
        if fresh.is_empty() {
            return Vec::new();
        }
        let mut out = fresh.swap_remove(self.chosen);
        if let Some((last_abs, last_idx)) = self.last_returned {
            out.retain(|&(abs, idx)| !(idx == last_idx && abs <= last_abs + self.min_gap));
        }
        if let Some(&last) = out.last() {
            self.last_returned = Some(last);
        }
        out
    }

    /// Number of decoded channels.
    pub fn channels(&self) -> usize {
        self.decoders.len()
    }

    /// The chosen channel (0 for a mono or channel-less input).
    pub fn chosen(&self) -> usize {
        self.chosen
    }

    /// Every channel's current self-consistency cluster, in channel order.
    pub fn clusters(&self) -> &[u64] {
        &self.clusters
    }

    /// The decode diagnostics summed over every channel: cumulative, so the dock's diag line, its
    /// staleness detector and its pairing watchdog keep reading monotonic counters. A mono input
    /// reads exactly its one decoder's stats.
    pub fn stats(&self) -> DecodeStats {
        let mut s = DecodeStats::default();
        for d in &self.decoders {
            let x = d.stats();
            s.preamble_screens_passed += x.preamble_screens_passed;
            s.crc_ok += x.crc_ok;
            s.crc_fail += x.crc_fail;
        }
        s
    }

    /// The dead-pairing reset (#1153): every channel's decoder drops its window and dedup anchor,
    /// keeping origin continuity and its cumulative stats. The per-channel histories and the pick
    /// stay: they describe whether a channel decodes, not the pairing state being reset.
    pub fn reset_window(&mut self) {
        for d in &mut self.decoders {
            d.reset_window();
        }
    }
}

/// When a switch of the paired channel goes to the OBS log (issue 1367, review round 3; mirrored
/// by `CbChannelSwitchLog`). A switch moves the dock's measured offset by ~10 ms and the offset
/// cluster is not reset, so every switch is counted and logged: the first at once, then at most
/// one line per interval, naming how many switches it stands for, so an L/R flip-flop near the
/// floor shows without flooding the log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChannelSwitchLog {
    total: u64,
    unlogged: u64,
    last_log_ns: Option<u64>,
}

impl ChannelSwitchLog {
    /// One push observed: the channel chosen before it (`prev`) and after it (`chosen`), at the
    /// callback timestamp `now_ns`. Returns `Some(n)` when a log line goes out now, `n` being the
    /// switches it stands for (this one included); `None` for no switch or a suppressed one. A
    /// timestamp that goes backwards reads as a long gap (the u64 difference wraps, as in the C++),
    /// so it logs rather than hides a switch.
    pub fn observe(
        &mut self,
        prev: usize,
        chosen: usize,
        now_ns: u64,
        interval_ns: u64,
    ) -> Option<u64> {
        if chosen == prev {
            return None;
        }
        self.total += 1;
        self.unlogged += 1;
        if let Some(last) = self.last_log_ns {
            if now_ns.wrapping_sub(last) < interval_ns {
                return None;
            }
        }
        self.last_log_ns = Some(now_ns);
        Some(std::mem::take(&mut self.unlogged))
    }

    /// Every switch seen, logged or not (the diag line's `channel_switches=`).
    pub fn total(&self) -> u64 {
        self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qpsk_marker::marker_signal;

    /// `n` real markers every `cadence_s`, index stepping by 30, delayed by `delay` samples, starting
    /// at `start_s`, in a buffer of `len` samples.
    fn marker_track(n: usize, cadence_s: f64, start_s: f64, delay: usize, len: usize) -> Vec<f32> {
        let p = AudioParams::rig60();
        let sr = p.sample_rate as f64;
        let mut buf = vec![0.0f32; len];
        for k in 0..n {
            let idx = 7u8.wrapping_add((k as u16 * 30) as u8);
            let off = ((start_s + k as f64 * cadence_s) * sr) as usize + delay;
            for (i, &s) in marker_signal(idx, &p).iter().enumerate() {
                if off + i < buf.len() {
                    buf[off + i] += s * 0.25;
                }
            }
        }
        buf
    }

    /// Marker k (index 7 + 30k) at `start + k * spacing` samples plus `delay`, for each k in `ks`.
    fn markers_at(
        ks: &[usize],
        spacing: usize,
        start: usize,
        delay: usize,
        len: usize,
    ) -> Vec<f32> {
        let p = AudioParams::rig60();
        let mut buf = vec![0.0f32; len];
        for &k in ks {
            let idx = 7u8.wrapping_add((k as u32 * 30) as u8);
            let off = start + k * spacing + delay;
            for (i, &s) in marker_signal(idx, &p).iter().enumerate() {
                if off + i < buf.len() {
                    buf[off + i] += s * 0.25;
                }
            }
        }
        buf
    }

    /// Push `channels` through `picker` in `chunk`-frame callbacks; return every returned marker.
    fn run(
        picker: &mut ChannelMarkerPicker,
        channels: &[Vec<f32>],
        chunk: usize,
    ) -> Vec<(u64, u8)> {
        let frames = channels[0].len();
        let mut out = Vec::new();
        let mut at = 0;
        while at < frames {
            let end = (at + chunk).min(frames);
            let planes: Vec<&[f32]> = channels.iter().map(|c| &c[at..end]).collect();
            out.extend(picker.push(&planes));
            at = end;
        }
        out
    }

    /// The single streaming decoder over one channel, the pre-1367 mono path.
    fn single(channel: &[f32], chunk: usize) -> (Vec<(u64, u8)>, DecodeStats) {
        let p = AudioParams::rig60();
        let sig = signal_len(&p);
        let mut dec = StreamingMarkerDecoder::new(p, DOCK_QPSK_THRESHOLD, sig * 3, sig as u64);
        let mut out = Vec::new();
        for c in channel.chunks(chunk) {
            out.extend(dec.push(c));
        }
        (out, dec.stats())
    }

    const SR: usize = 48_000;

    #[test]
    fn mono_is_the_single_decoder_identity() {
        let l = marker_track(8, 0.5, 0.25, 0, SR * 5);
        let mut picker = ChannelMarkerPicker::dock(1, AudioParams::rig60());
        let got = run(&mut picker, std::slice::from_ref(&l), 1024);
        let (want, stats) = single(&l, 1024);
        assert_eq!(got, want);
        assert_eq!(got.len(), 8);
        assert_eq!(picker.chosen(), 0);
        assert_eq!(picker.clusters(), &[8]);
        assert_eq!(picker.stats(), stats);
    }

    #[test]
    fn skewed_stereo_decodes_on_each_channel_never_on_their_mix() {
        // The measured shape: the same marker on L and R, R 488 samples (10.17 ms) late. Their
        // average (the old dock) decodes nothing; each channel alone decodes all eight.
        let l = marker_track(8, 0.5, 0.25, 0, SR * 5);
        let r = marker_track(8, 0.5, 0.25, 488, SR * 5);
        let mix: Vec<f32> = l.iter().zip(&r).map(|(a, b)| (a + b) / 2.0).collect();
        let mixed: Vec<(f64, u8)> = single(&mix, 1024)
            .0
            .iter()
            .map(|&(abs, idx)| (abs as f64 / SR as f64, idx))
            .collect();
        assert!(
            consistency_cluster_size(&mixed, ClusterParams::default()) < DEFAULT_MIN_CLUSTERS,
            "the 10 ms-skewed mix must not decode: {mixed:?}"
        );
        let mut picker = ChannelMarkerPicker::dock(2, AudioParams::rig60());
        let got = run(&mut picker, &[l.clone(), r], 1024);
        assert_eq!(picker.clusters(), &[8, 8]);
        assert_eq!(
            picker.chosen(),
            0,
            "both clear the floor: the lowest channel"
        );
        assert_eq!(
            got,
            single(&l, 1024).0,
            "the chosen channel's own markers, never a mix"
        );
    }

    #[test]
    fn both_clear_the_floor_so_l_wins_although_r_has_the_longer_chain() {
        let l = marker_track(6, 0.5, 0.25, 0, SR * 5);
        let r = marker_track(8, 0.5, 0.25, 488, SR * 5);
        let mut picker = ChannelMarkerPicker::dock(2, AudioParams::rig60());
        let got = run(&mut picker, &[l.clone(), r], 1024);
        assert_eq!(picker.clusters(), &[6, 8]);
        assert_eq!(picker.chosen(), 0);
        assert_eq!(got, single(&l, 1024).0);
    }

    #[test]
    fn a_marker_only_on_r_is_picked_once_its_chain_forms() {
        let silent = vec![0.0f32; SR * 5];
        let r = marker_track(8, 0.5, 0.25, 488, SR * 5);
        let mut picker = ChannelMarkerPicker::dock(2, AudioParams::rig60());
        let got = run(&mut picker, &[silent, r.clone()], 1024);
        assert_eq!(picker.clusters(), &[0, 8]);
        assert_eq!(picker.chosen(), 1);
        // R's first two markers arrived while channel 0 was still chosen (no chain yet), so they
        // fed nothing; every marker from the third on is R's own.
        let all_r = single(&r, 1024).0;
        assert_eq!(got, all_r[2..].to_vec());
    }

    #[test]
    fn a_channel_that_stops_decoding_hands_over_once_its_window_ages_out() {
        // L carries the marker for the first 2 s only; R all along. With a 3 s pick window, L's
        // markers age out and its cluster falls below the floor, so R takes over.
        let p = AudioParams::rig60();
        let sig = signal_len(&p);
        let len = SR * 8;
        let l = marker_track(4, 0.5, 0.25, 0, len);
        let r = marker_track(15, 0.5, 0.25, 488, len);
        let mut picker = ChannelMarkerPicker::new(
            2,
            p,
            DOCK_QPSK_THRESHOLD,
            sig * 3,
            sig as u64,
            3 * SR as u64,
            DEFAULT_MIN_CLUSTERS,
        );
        let mut chosen_at_2s = None;
        let mut at = 0;
        while at < len {
            let end = (at + 1024).min(len);
            picker.push(&[&l[at..end], &r[at..end]]);
            if chosen_at_2s.is_none() && end >= 2 * SR {
                chosen_at_2s = Some(picker.chosen());
            }
            at = end;
        }
        assert_eq!(
            chosen_at_2s,
            Some(0),
            "L decodes and clears the floor first"
        );
        assert_eq!(
            picker.clusters()[0],
            0,
            "L's markers aged out of the window"
        );
        assert!(picker.clusters()[1] >= DEFAULT_MIN_CLUSTERS);
        assert_eq!(picker.chosen(), 1);
    }

    #[test]
    fn stats_are_the_sum_over_channels_and_survive_a_window_reset() {
        let l = marker_track(4, 0.5, 0.25, 0, SR * 3);
        let r = marker_track(4, 0.5, 0.25, 488, SR * 3);
        let mut picker = ChannelMarkerPicker::dock(2, AudioParams::rig60());
        run(&mut picker, &[l.clone(), r.clone()], 1024);
        let (sl, sr) = (single(&l, 1024).1, single(&r, 1024).1);
        let s = picker.stats();
        assert_eq!(s.crc_ok, sl.crc_ok + sr.crc_ok);
        assert_eq!(
            s.preamble_screens_passed,
            sl.preamble_screens_passed + sr.preamble_screens_passed
        );
        assert_eq!(s.crc_fail, sl.crc_fail + sr.crc_fail);
        let before = (picker.clusters().to_vec(), picker.chosen());
        picker.reset_window();
        assert_eq!(picker.stats(), s, "cumulative counters stay monotonic");
        assert_eq!((picker.clusters().to_vec(), picker.chosen()), before);
    }

    #[test]
    fn a_pick_switch_never_returns_the_same_marker_twice() {
        // The committed 2 s fixture's shape: L below the floor, R clearing it, R 488 samples late.
        // L carries markers 1-3, R markers 0-3. In 256-frame callbacks L's copy of marker 2 is
        // returned while L is still chosen, then R's copy tips the pick to R one callback later;
        // that copy (and R's copy of marker 3) is the SAME physical marker and must not be paired
        // a second time.
        let p = AudioParams::rig60();
        let sig = signal_len(&p) as u64;
        let len = SR * 3;
        let l = markers_at(&[1, 2, 3], SR / 2, SR / 4, 0, len);
        let r = markers_at(&[0, 1, 2, 3], SR / 2, SR / 4, 488, len);
        let mut picker = ChannelMarkerPicker::dock(2, p);
        let got = run(&mut picker, &[l.clone(), r], 256);
        for w in got.windows(2) {
            assert!(
                !(w[0].1 == w[1].1 && w[1].0 <= w[0].0 + sig),
                "marker {} returned twice: {got:?}",
                w[1].1
            );
        }
        assert_eq!(picker.clusters(), &[3, 4]);
        assert_eq!(picker.chosen(), 1, "only R clears the floor");
        assert_eq!(got, single(&l, 256).0, "L's own three markers, each once");
    }

    #[test]
    fn a_switch_keeps_a_different_marker_inside_the_dedup_gap() {
        // The switch filter drops only the SAME marker. L's lone marker (index 200) is returned
        // while L is chosen; R's third chain marker (index 67), 300 samples later, tips the pick to
        // R and must still be returned.
        let p = AudioParams::rig60();
        let sig = signal_len(&p) as u64;
        let len = SR * 2;
        let r = markers_at(&[0, 1, 2], SR / 2, SR / 4, 0, len);
        let mut l = vec![0.0f32; len];
        let at = SR / 4 + 2 * (SR / 2) - 300;
        for (i, &s) in marker_signal(200, &p).iter().enumerate() {
            l[at + i] += s * 0.25;
        }
        let mut picker = ChannelMarkerPicker::dock(2, p);
        let got = run(&mut picker, &[l, r], 256);
        assert_eq!(picker.chosen(), 1);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!((got[0].1, got[1].1), (200, 67), "{got:?}");
        assert!(got[1].0 <= got[0].0 + sig, "inside the gap: {got:?}");
    }

    #[test]
    fn a_same_index_copy_exactly_one_gap_later_is_still_the_same_marker() {
        // The filter's boundary is inclusive: R's copy exactly one dedup gap (one marker length)
        // after L's copy of the same marker is still that marker, and is dropped.
        let p = AudioParams::rig60();
        let len = SR * 3;
        let l = markers_at(&[1, 2, 3], SR / 2, SR / 4, 0, len);
        let r = markers_at(&[0, 1, 2, 3], SR / 2, SR / 4, signal_len(&p), len);
        let mut picker = ChannelMarkerPicker::dock(2, p);
        let got = run(&mut picker, &[l.clone(), r], 256);
        assert_eq!(picker.chosen(), 1);
        assert_eq!(got, single(&l, 256).0, "R's copies are the same markers");
    }

    #[test]
    fn a_window_reset_drops_the_marker_in_flight_and_keeps_the_sample_clock() {
        // The dead-pairing recovery calls reset_window(). Marker 2 starts at 60000 samples and is
        // 1085 long; after 59 pushes of 1024 (60416 samples) only its head is in. The reset throws
        // the head away, so marker 2 is never reported, and every later marker decodes at the same
        // absolute sample index as without the reset.
        let track = markers_at(&[0, 1, 2, 3, 4], SR / 2, SR / 4, 0, SR * 3);
        let mut want = single(&track, 1024).0;
        assert_eq!(want.len(), 5);
        want.remove(2);
        let mut picker = ChannelMarkerPicker::dock(1, AudioParams::rig60());
        let mut got = Vec::new();
        for (push, chunk) in track.chunks(1024).enumerate() {
            got.extend(picker.push(&[chunk]));
            if push + 1 == 59 {
                picker.reset_window();
            }
        }
        assert_eq!(got, want);
    }

    #[test]
    fn a_decode_flood_keeps_only_the_newest_markers_per_channel() {
        // One marker every 1200 samples (just over the 1085-sample dedup gap), more than the cap.
        let n = DOCK_CHANNEL_PICK_MAX_MARKERS + 40;
        let ks: Vec<usize> = (0..n).collect();
        let track = markers_at(&ks, 1200, SR / 4, 0, SR / 4 + n * 1200 + SR / 4);
        let mut picker = ChannelMarkerPicker::dock(1, AudioParams::rig60());
        let got = run(&mut picker, std::slice::from_ref(&track), 1024);
        assert_eq!(got.len(), n, "every marker still decodes and is returned");
        assert_eq!(picker.history[0].len(), DOCK_CHANNEL_PICK_MAX_MARKERS);
        assert_eq!(picker.history[0].back(), got.last(), "the newest are kept");
        assert_eq!(
            picker.history[0].front(),
            got.get(40),
            "the oldest are dropped"
        );
        assert_eq!(picker.clusters(), &[DOCK_CHANNEL_PICK_MAX_MARKERS as u64]);
    }

    const S: u64 = 1_000_000_000;

    #[test]
    fn the_first_switch_logs_at_once_and_a_no_switch_push_is_nothing() {
        let mut log = ChannelSwitchLog::default();
        assert_eq!(log.observe(0, 0, 5 * S, 10 * S), None);
        assert_eq!(log.total(), 0);
        assert_eq!(log.observe(0, 1, 6 * S, 10 * S), Some(1));
        assert_eq!(log.total(), 1);
    }

    #[test]
    fn switches_inside_the_interval_are_counted_and_carried_to_the_next_line() {
        let mut log = ChannelSwitchLog::default();
        assert_eq!(log.observe(0, 1, 100 * S, 10 * S), Some(1));
        assert_eq!(
            log.observe(1, 0, 103 * S, 10 * S),
            None,
            "inside the interval"
        );
        assert_eq!(log.observe(0, 1, 109 * S, 10 * S), None, "still inside");
        assert_eq!(
            log.observe(1, 1, 109 * S + 5, 10 * S),
            None,
            "no switch, no line"
        );
        assert_eq!(
            log.observe(1, 0, 110 * S + 1, 10 * S),
            Some(3),
            "the two suppressed switches plus this one"
        );
        assert_eq!(log.total(), 4);
        assert_eq!(
            log.observe(0, 1, 110 * S + 2, 10 * S),
            None,
            "a fresh interval"
        );
        assert_eq!(log.observe(1, 0, 120 * S + 1, 10 * S), Some(2));
        assert_eq!(log.total(), 6);
    }

    #[test]
    fn exactly_one_interval_later_logs() {
        let mut log = ChannelSwitchLog::default();
        assert_eq!(log.observe(0, 1, 50 * S, 10 * S), Some(1));
        assert_eq!(log.observe(1, 0, 60 * S - 1, 10 * S), None);
        assert_eq!(log.observe(0, 1, 60 * S, 10 * S), Some(2));
    }

    #[test]
    fn a_timestamp_going_backwards_logs_rather_than_hides() {
        let mut log = ChannelSwitchLog::default();
        assert_eq!(log.observe(0, 1, 50 * S, 10 * S), Some(1));
        assert_eq!(log.observe(1, 0, 40 * S, 10 * S), Some(1));
        assert_eq!(log.total(), 2);
    }

    #[test]
    fn a_channel_less_input_decodes_nothing() {
        let mut picker = ChannelMarkerPicker::dock(0, AudioParams::rig60());
        assert!(picker.push(&[]).is_empty());
        assert_eq!(picker.channels(), 0);
        assert_eq!(picker.chosen(), 0);
        assert_eq!(picker.stats(), DecodeStats::default());
    }
}
