#pragma once

/*
 * issue 1367 -- the live dock decodes the QPSK marker on EVERY channel and keeps one, never a mix.
 *
 * The stream box's `mbc` input is stereo and carries the same cam2 marker on L and R, R 10.17 ms
 * behind L. Their average (what `st_raw_audio_camera_box` used to decode) comb-filters the two
 * copies into an undecodable signal. This header decodes each channel with its own
 * StreamingMarkerDecoder (camera-box-audio.hpp), keeps each channel's decoded markers over the last
 * CB_CHANNEL_PICK_WINDOW_S, measures each channel's self-consistency cluster, and applies the ONE
 * channel rule the offline gate uses: the lowest channel that clears the decodability floor, else
 * the largest cluster, ties to the lowest. Only the chosen channel's markers reach the ring pairing.
 *
 * Every piece is a port of Rust: `cb_consistency_cluster_size` = qpsk_probe_decision::
 * consistency_cluster_size, `cb_pick_marker_channel` = qpsk_channel_select::pick_marker_channel,
 * `ChannelMarkerPicker` = av_sync_dock_channels::ChannelMarkerPicker. The dock itself compiles only
 * on the Windows genlock build, so tests/qpsk_channel_pick_parity_1367.rs compiles
 * test/channel-pick-parity.cpp against this header with g++ and compares it with the Rust (the
 * shared table tests/fixtures/qpsk_channel_pick_parity.tsv, generated marker sequences, and the
 * real stereo fixture pushed callback by callback). Keep the two in lock-step.
 *
 * Dependency-free (STL only, C++11) like camera-box-audio.hpp.
 */

#include "camera-box-audio.hpp"

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <deque>
#include <string>
#include <utility>
#include <vector>

namespace camerabox {

/* Mirror of qpsk_probe_decision::DEFAULT_MIN_CLUSTERS -- the #1324 decodability floor. */
static const uint64_t CB_MARKER_MIN_CLUSTERS = 4;
/* Mirror of av_sync_dock_channels::DOCK_CHANNEL_PICK_WINDOW_S -- the #1324 calibration window. */
static const uint64_t CB_CHANNEL_PICK_WINDOW_S = 25;
/* Mirror of av_sync_dock_channels::DOCK_CHANNEL_PICK_MAX_MARKERS -- the most markers one channel
 * keeps in its pick window (the newest), so the O(n^2) cluster on the audio thread stays bounded
 * under a decode flood. */
static const size_t CB_CHANNEL_PICK_MAX_MARKERS = 256;
/* Mirror of qpsk_probe_decision::ClusterParams::default(). */
static const uint32_t CB_CLUSTER_STEP_TOL = 3;
static const double CB_CLUSTER_GAP_RATIO = 0.25;

/* Circular distance on the 0..256 index ring (mirror of qpsk_probe_decision::circ_dist). */
inline uint32_t cb_circ_dist(uint32_t a, uint32_t b)
{
	const uint32_t d = (a + 256u - b) % 256u;
	return d < 256u - d ? d : 256u - d;
}

/* The #1324 self-consistency cluster (mirror of qpsk_probe_decision::consistency_cluster_size):
 * the length of the longest run of consecutive-in-time decodes sharing the modal index step S
 * (+-step_tol) and the modal gap G (+-gap_ratio), a single missed marker (2S / 2G) allowed. Fewer
 * than 3 markers, or no step shared by 2 pairs, is 0. `markers` are (audio_ts_s, index) in any
 * order; the time sort is STABLE like the Rust `sort_by`, so equal timestamps keep their order. */
inline uint64_t cb_consistency_cluster_size(const std::vector<std::pair<double, uint8_t>> &markers,
					    uint32_t step_tol, double gap_ratio)
{
	if (markers.size() < 3)
		return 0;
	std::vector<std::pair<double, uint8_t>> m(markers);
	std::stable_sort(m.begin(), m.end(),
			 [](const std::pair<double, uint8_t> &a, const std::pair<double, uint8_t> &b) {
				 return a.first < b.first;
			 });
	const size_t n = m.size();
	std::vector<std::pair<double, uint32_t>> pairs; // (gap, step)
	pairs.reserve(n - 1);
	for (size_t i = 0; i + 1 < n; i++) {
		const double gap = m[i + 1].first - m[i].first;
		const int d = ((int)m[i + 1].second - (int)m[i].second) % 256;
		pairs.push_back(std::make_pair(gap, (uint32_t)(d < 0 ? d + 256 : d)));
	}
	/* Modal step S: the step matched (+-step_tol) by the most pairs; the FIRST such step wins. */
	bool have_s = false;
	uint32_t s = 0;
	size_t best_c = 0;
	for (size_t i = 0; i < pairs.size(); i++) {
		size_t c = 0;
		for (size_t j = 0; j < pairs.size(); j++)
			if (cb_circ_dist(pairs[j].second, pairs[i].second) <= step_tol)
				c++;
		if (c > best_c) {
			best_c = c;
			s = pairs[i].second;
			have_s = true;
		}
	}
	if (!have_s || best_c < 2)
		return 0;
	/* Modal gap G: the upper median of the positive gaps among the step-matching pairs. */
	std::vector<double> gaps;
	for (size_t i = 0; i < pairs.size(); i++)
		if (cb_circ_dist(pairs[i].second, s) <= step_tol && pairs[i].first > 0.0)
			gaps.push_back(pairs[i].first);
	if (gaps.empty())
		return 0;
	std::sort(gaps.begin(), gaps.end());
	const double g_ref = gaps[gaps.size() / 2];
	const uint32_t s2 = (2u * s) % 256u;
	uint64_t best = 1, cur = 1;
	for (size_t i = 0; i < pairs.size(); i++) {
		const double gap = pairs[i].first;
		const uint32_t step = pairs[i].second;
		const bool single = cb_circ_dist(step, s) <= step_tol && std::fabs(gap - g_ref) <= gap_ratio * g_ref;
		const bool missed = cb_circ_dist(step, s2) <= step_tol &&
				    std::fabs(gap - 2.0 * g_ref) <= gap_ratio * 2.0 * g_ref;
		if (single || missed) {
			cur++;
			if (cur > best)
				best = cur;
		} else {
			cur = 1;
		}
	}
	return best;
}

/* The ONE channel rule (mirror of qpsk_channel_select::pick_marker_channel): the LOWEST channel whose
 * cluster clears `min_clusters`; when none does, the largest cluster, ties to the lowest. Returns
 * the chosen position, or clusters.size() when there are no channels (the Rust `None`). */
inline size_t cb_pick_marker_channel(const std::vector<uint64_t> &clusters, uint64_t min_clusters)
{
	for (size_t i = 0; i < clusters.size(); i++)
		if (clusters[i] >= min_clusters)
			return i;
	size_t best = clusters.size();
	for (size_t i = 0; i < clusters.size(); i++)
		if (best == clusters.size() || clusters[i] > clusters[best])
			best = i;
	return best;
}

/* The per-channel cluster list for the dock diag line: "7,8" (one value per channel, in order). */
inline std::string cb_channel_clusters_text(const std::vector<uint64_t> &clusters)
{
	std::string out;
	for (size_t i = 0; i < clusters.size(); i++) {
		if (i)
			out += ',';
		out += std::to_string((unsigned long long)clusters[i]);
	}
	return out;
}

/* The dock's per-channel marker decode + channel pick (mirror of
 * av_sync_dock_channels::ChannelMarkerPicker). Every channel advances by the same number of samples
 * per push(), so one absolute sample index serves all of them and the caller's pushed-sample count
 * maps it to an OBS timestamp exactly as it did for the single decoder. */
struct ChannelMarkerPicker {
	std::vector<StreamingMarkerDecoder> decoders;
	/* Per channel: the markers (absolute sample index, index) decoded within the last
	 * window_samples, oldest first. */
	std::vector<std::deque<std::pair<uint64_t, uint8_t>>> history;
	/* Per channel: the self-consistency cluster of `history`, recomputed when it changes. */
	std::vector<uint64_t> clusters;
	size_t chosen;
	uint64_t pushed;
	uint32_t sample_rate;
	uint64_t window_samples;
	uint64_t min_clusters;
	/* The decoders' dedup gap: one physical marker's copies on two channels lie within it. */
	uint64_t min_gap;
	/* The last marker (absolute sample index, index) this picker returned, from any channel. */
	bool have_last_returned;
	uint64_t last_returned_abs;
	uint8_t last_returned_idx;
	/* The decode diagnostics summed over every channel (cumulative, monotonic across a
	 * reset_window()); a mono input reads its one decoder's stats. */
	CbDecodeStats stats;

	ChannelMarkerPicker(size_t channels, uint32_t sr, uint32_t f, uint32_t c, double thr, size_t cap,
			    uint64_t gap, uint64_t window, uint64_t min_cl)
		: history(channels), clusters(channels, 0), chosen(0), pushed(0), sample_rate(sr),
		  window_samples(window), min_clusters(min_cl), min_gap(gap), have_last_returned(false),
		  last_returned_abs(0), last_returned_idx(0)
	{
		decoders.reserve(channels);
		for (size_t i = 0; i < channels; i++)
			decoders.push_back(StreamingMarkerDecoder(sr, f, c, thr, cap, gap));
	}

	/* The dock's configuration (mirror of ChannelMarkerPicker::dock): a decode window of three
	 * marker lengths, a dedup gap of one, CB_QPSK_THRESHOLD, a CB_CHANNEL_PICK_WINDOW_S pick window
	 * and the CB_MARKER_MIN_CLUSTERS floor. */
	static ChannelMarkerPicker dock(size_t channels, uint32_t sr, uint32_t f, uint32_t c)
	{
		const size_t sig = cb_signal_len(sr, f, c);
		return ChannelMarkerPicker(channels, sr, f, c, CB_QPSK_THRESHOLD, sig * 3, (uint64_t)sig,
					   CB_CHANNEL_PICK_WINDOW_S * (uint64_t)sr, CB_MARKER_MIN_CLUSTERS);
	}

	size_t channels() const { return decoders.size(); }

	/* Decode one callback's planar audio (planes[i] is channel i, `frames` samples each) on every
	 * channel, age the histories, re-apply the pick, and return the CHOSEN channel's newly decoded
	 * markers (absolute sample index, index). `planes` must hold channels() pointers. A pick switch
	 * never returns one physical marker twice: the newly chosen channel's copy of the marker this
	 * picker returned last (same index, within one dedup gap) is dropped. Each channel keeps at most
	 * CB_CHANNEL_PICK_MAX_MARKERS (the newest). */
	std::vector<std::pair<uint64_t, uint8_t>> push(const float *const *planes, size_t frames)
	{
		std::vector<std::vector<std::pair<uint64_t, uint8_t>>> fresh;
		fresh.reserve(decoders.size());
		for (size_t i = 0; i < decoders.size(); i++)
			fresh.push_back(decoders[i].push(planes[i], frames));
		pushed += (uint64_t)frames;
		const uint64_t cutoff = pushed > window_samples ? pushed - window_samples : 0;
		const double sr = (double)sample_rate;
		for (size_t i = 0; i < decoders.size(); i++) {
			std::deque<std::pair<uint64_t, uint8_t>> &h = history[i];
			bool changed = !fresh[i].empty();
			h.insert(h.end(), fresh[i].begin(), fresh[i].end());
			while (!h.empty() && h.front().first < cutoff) {
				h.pop_front();
				changed = true;
			}
			while (h.size() > CB_CHANNEL_PICK_MAX_MARKERS) {
				h.pop_front();
				changed = true;
			}
			if (changed) {
				std::vector<std::pair<double, uint8_t>> m;
				m.reserve(h.size());
				for (size_t k = 0; k < h.size(); k++)
					m.push_back(std::make_pair((double)h[k].first / sr, h[k].second));
				clusters[i] = cb_consistency_cluster_size(m, CB_CLUSTER_STEP_TOL, CB_CLUSTER_GAP_RATIO);
			}
		}
		const size_t pick = cb_pick_marker_channel(clusters, min_clusters);
		chosen = pick < clusters.size() ? pick : 0;
		CbDecodeStats sum;
		for (size_t i = 0; i < decoders.size(); i++) {
			sum.preamble_screens_passed += decoders[i].stats.preamble_screens_passed;
			sum.crc_ok += decoders[i].stats.crc_ok;
			sum.crc_fail += decoders[i].stats.crc_fail;
		}
		stats = sum;
		std::vector<std::pair<uint64_t, uint8_t>> out;
		if (fresh.empty())
			return out;
		for (size_t k = 0; k < fresh[chosen].size(); k++) {
			const std::pair<uint64_t, uint8_t> &m = fresh[chosen][k];
			if (have_last_returned && m.second == last_returned_idx &&
			    m.first <= last_returned_abs + min_gap)
				continue;
			out.push_back(m);
		}
		if (!out.empty()) {
			have_last_returned = true;
			last_returned_abs = out.back().first;
			last_returned_idx = out.back().second;
		}
		return out;
	}

	/* The #1153 dead-pairing reset: every decoder drops its window and dedup anchor (origin
	 * continuity and cumulative stats kept). The histories and the pick stay -- they describe
	 * whether a channel decodes, not the pairing state being reset. */
	void reset_window()
	{
		for (size_t i = 0; i < decoders.size(); i++)
			decoders[i].reset_window();
	}
};

/* When a switch of the paired channel goes to the OBS log (mirror of
 * av_sync_dock_channels::ChannelSwitchLog). A switch moves the dock's measured offset by ~10 ms and
 * the offset cluster is not reset, so every switch is counted and logged: the first at once, then
 * at most one line per interval, naming how many switches it stands for. */
struct CbChannelSwitchLog {
	uint64_t total;
	uint64_t unlogged;
	bool have_logged;
	uint64_t last_log_ns;

	CbChannelSwitchLog() : total(0), unlogged(0), have_logged(false), last_log_ns(0) {}

	/* One push observed: the channel chosen before it (prev) and after it (chosen), at the callback
	 * timestamp now_ns. Returns true when a log line goes out now, with *count = the switches it
	 * stands for (this one included). A timestamp that goes backwards wraps the unsigned
	 * difference, so it logs rather than hides a switch. */
	bool observe(size_t prev, size_t chosen, uint64_t now_ns, uint64_t interval_ns, uint64_t *count)
	{
		if (chosen == prev)
			return false;
		total++;
		unlogged++;
		if (have_logged && now_ns - last_log_ns < interval_ns)
			return false;
		have_logged = true;
		last_log_ns = now_ns;
		*count = unlogged;
		unlogged = 0;
		return true;
	}
};

} // namespace camerabox
