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
	/* RED stub (issue 1367): today's dock measures no per-channel cluster. */
	(void)markers;
	(void)step_tol;
	(void)gap_ratio;
	return 0;
}

/* The ONE channel rule (mirror of qpsk_channel_select::pick_marker_channel): the LOWEST channel whose
 * cluster clears `min_clusters`; when none does, the largest cluster, ties to the lowest. Returns
 * the chosen position, or clusters.size() when there are no channels (the Rust `None`). */
inline size_t cb_pick_marker_channel(const std::vector<uint64_t> &clusters, uint64_t min_clusters)
{
	/* RED stub (issue 1367): today's rule, the largest cluster with ties to the lowest. */
	(void)min_clusters;
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
	/* The decode diagnostics summed over every channel (cumulative, monotonic across a
	 * reset_window()); a mono input reads its one decoder's stats. */
	CbDecodeStats stats;

	ChannelMarkerPicker(size_t channels, uint32_t sr, uint32_t f, uint32_t c, double thr, size_t cap,
			    uint64_t gap, uint64_t window, uint64_t min_cl)
		: history(channels), clusters(channels, 0), chosen(0), pushed(0), sample_rate(sr),
		  window_samples(window), min_clusters(min_cl)
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
	 * markers (absolute sample index, index). `planes` must hold channels() pointers. */
	std::vector<std::pair<uint64_t, uint8_t>> push(const float *const *planes, size_t frames)
	{
		/* RED stub (issue 1367): today's dock -- average the channels (a non-finite sample reads
		 * as silence), decode that sum on one decoder, always channel 0, no per-channel cluster. */
		pushed += (uint64_t)frames;
		if (decoders.empty())
			return std::vector<std::pair<uint64_t, uint8_t>>();
		std::vector<float> mono(frames, 0.0f);
		for (size_t k = 0; k < frames; k++) {
			float acc = 0.0f;
			for (size_t i = 0; i < decoders.size(); i++)
				if (std::isfinite(planes[i][k]))
					acc += planes[i][k];
			mono[k] = acc / (float)decoders.size();
		}
		std::vector<std::pair<uint64_t, uint8_t>> out = decoders[0].push(mono.data(), frames);
		stats = decoders[0].stats;
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

} // namespace camerabox
