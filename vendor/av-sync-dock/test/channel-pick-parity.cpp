/*
 * issue 1367 -- the C++ side of tests/qpsk_channel_pick_parity_1367.rs.
 *
 * Runs the dock's per-channel marker decode (`camera-box-channel-pick.hpp`) on inputs the Rust test
 * writes, and prints what the C++ decided; the Rust test compares every line with the Rust
 * reference (qpsk_channel_select / qpsk_probe_decision / av_sync_dock_channels). Dependency-free:
 * `g++ -std=c++11 -O2 -Wall -Wextra -Werror channel-pick-parity.cpp`.
 *
 * Commands:
 *   consts
 *       one "<name> <value>" line per mirrored constant.
 *   pick <table.tsv>
 *       for every data row "<clusters>\t<min_clusters>\t<expected>" (clusters "a,b,..." or
 *       "<empty>"; '#' lines are comments): the chosen position, or "none".
 *   cluster <file>
 *       for every line "<n> <ts> <idx> <ts> <idx> ...": the self-consistency cluster size.
 *   stream <file> <channels> <chunk> [<window_samples>]
 *       <file> holds <channels> planes of f32 LE samples back to back, all the same length. They
 *       are pushed through ChannelMarkerPicker::dock at the rig's audio params (or, with
 *       <window_samples>, the same configuration with that pick window) in <chunk>-frame
 *       callbacks, one line per push:
 *       "<push> chosen=<c> clusters=<a,b> markers=<abs:idx,...> stats=<preambles>/<crc_ok>/<crc_fail>".
 *
 * Exit 0 on success, 2 on a usage or I/O error.
 */

#include "../src/camera-box-channel-pick.hpp"

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <iterator>
#include <sstream>
#include <string>
#include <vector>

using namespace camerabox;

static int usage()
{
	std::fprintf(stderr, "usage: channel-pick-parity consts | pick <tsv> | cluster <file> | "
			     "stream <file> <channels> <chunk> [<window_samples>]\n");
	return 2;
}

static int cmd_consts()
{
	std::printf("min_clusters %llu\n", (unsigned long long)CB_MARKER_MIN_CLUSTERS);
	std::printf("pick_window_s %llu\n", (unsigned long long)CB_CHANNEL_PICK_WINDOW_S);
	std::printf("step_tol %u\n", (unsigned)CB_CLUSTER_STEP_TOL);
	std::printf("gap_ratio %.17g\n", CB_CLUSTER_GAP_RATIO);
	std::printf("qpsk_threshold %.17g\n", CB_QPSK_THRESHOLD);
	std::printf("sample_rate %u\n", (unsigned)CB_AUDIO_SAMPLE_RATE);
	std::printf("carrier_hz %u\n", (unsigned)CB_AUDIO_CARRIER_HZ);
	std::printf("c %u\n", (unsigned)CB_AUDIO_C);
	return 0;
}

static std::vector<std::string> split(const std::string &s, char sep)
{
	std::vector<std::string> out;
	std::string cur;
	for (size_t i = 0; i < s.size(); i++) {
		if (s[i] == sep) {
			out.push_back(cur);
			cur.clear();
		} else {
			cur += s[i];
		}
	}
	out.push_back(cur);
	return out;
}

static int cmd_pick(const char *path)
{
	std::ifstream in(path);
	if (!in) {
		std::fprintf(stderr, "cannot read %s\n", path);
		return 2;
	}
	std::string line;
	while (std::getline(in, line)) {
		if (line.empty() || line[0] == '#')
			continue;
		std::vector<std::string> cols = split(line, '\t');
		if (cols.size() != 3) {
			std::fprintf(stderr, "bad row: %s\n", line.c_str());
			return 2;
		}
		std::vector<uint64_t> clusters;
		if (cols[0] != "<empty>") {
			std::vector<std::string> items = split(cols[0], ',');
			for (size_t i = 0; i < items.size(); i++)
				clusters.push_back(std::strtoull(items[i].c_str(), nullptr, 10));
		}
		const uint64_t min_clusters = std::strtoull(cols[1].c_str(), nullptr, 10);
		const size_t pick = cb_pick_marker_channel(clusters, min_clusters);
		if (pick < clusters.size())
			std::printf("%zu\n", pick);
		else
			std::printf("none\n");
	}
	return 0;
}

static int cmd_cluster(const char *path)
{
	std::ifstream in(path);
	if (!in) {
		std::fprintf(stderr, "cannot read %s\n", path);
		return 2;
	}
	std::string line;
	while (std::getline(in, line)) {
		std::istringstream fields(line);
		size_t n = 0;
		if (!(fields >> n)) {
			std::fprintf(stderr, "bad line: %s\n", line.c_str());
			return 2;
		}
		std::vector<std::pair<double, uint8_t>> markers;
		for (size_t k = 0; k < n; k++) {
			std::string ts;
			unsigned idx = 0;
			if (!(fields >> ts >> idx) || idx > 255) {
				std::fprintf(stderr, "bad marker in: %s\n", line.c_str());
				return 2;
			}
			/* strtod reads the Rust shortest round-trip float text back to the same double. */
			markers.push_back(std::make_pair(std::strtod(ts.c_str(), nullptr), (uint8_t)idx));
		}
		std::printf("%llu\n", (unsigned long long)cb_consistency_cluster_size(
					      markers, CB_CLUSTER_STEP_TOL, CB_CLUSTER_GAP_RATIO));
	}
	return 0;
}

static int cmd_stream(const char *path, size_t channels, size_t chunk, uint64_t window)
{
	std::ifstream in(path, std::ios::binary);
	if (!in || channels == 0 || chunk == 0) {
		std::fprintf(stderr, "cannot read %s (channels %zu, chunk %zu)\n", path, channels, chunk);
		return 2;
	}
	std::vector<char> bytes((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
	if (bytes.size() % (4 * channels) != 0) {
		std::fprintf(stderr, "%zu bytes are not %zu whole f32 planes\n", bytes.size(), channels);
		return 2;
	}
	const size_t frames = bytes.size() / (4 * channels);
	std::vector<float> all(bytes.size() / 4);
	std::memcpy(all.data(), bytes.data(), bytes.size());

	ChannelMarkerPicker picker =
		ChannelMarkerPicker::dock(channels, CB_AUDIO_SAMPLE_RATE, CB_AUDIO_CARRIER_HZ, CB_AUDIO_C);
	if (window != 0) {
		const size_t sig = cb_signal_len(CB_AUDIO_SAMPLE_RATE, CB_AUDIO_CARRIER_HZ, CB_AUDIO_C);
		picker = ChannelMarkerPicker(channels, CB_AUDIO_SAMPLE_RATE, CB_AUDIO_CARRIER_HZ, CB_AUDIO_C,
					     CB_QPSK_THRESHOLD, sig * 3, (uint64_t)sig, window, CB_MARKER_MIN_CLUSTERS);
	}
	std::vector<const float *> planes(channels);
	size_t push = 0;
	for (size_t at = 0; at < frames; at += chunk, push++) {
		const size_t n = frames - at < chunk ? frames - at : chunk;
		for (size_t c = 0; c < channels; c++)
			planes[c] = all.data() + c * frames + at;
		std::vector<std::pair<uint64_t, uint8_t>> markers = picker.push(planes.data(), n);
		std::printf("%zu chosen=%zu clusters=%s markers=", push, picker.chosen,
			    cb_channel_clusters_text(picker.clusters).c_str());
		for (size_t k = 0; k < markers.size(); k++)
			std::printf("%s%llu:%u", k ? "," : "", (unsigned long long)markers[k].first,
				    (unsigned)markers[k].second);
		std::printf(" stats=%llu/%llu/%llu\n", (unsigned long long)picker.stats.preamble_screens_passed,
			    (unsigned long long)picker.stats.crc_ok, (unsigned long long)picker.stats.crc_fail);
	}
	return 0;
}

int main(int argc, char **argv)
{
	if (argc < 2)
		return usage();
	const std::string cmd = argv[1];
	if (cmd == "consts" && argc == 2)
		return cmd_consts();
	if (cmd == "pick" && argc == 3)
		return cmd_pick(argv[2]);
	if (cmd == "cluster" && argc == 3)
		return cmd_cluster(argv[2]);
	if (cmd == "stream" && (argc == 5 || argc == 6))
		return cmd_stream(argv[2], (size_t)std::strtoull(argv[3], nullptr, 10),
				  (size_t)std::strtoull(argv[4], nullptr, 10),
				  argc == 6 ? (uint64_t)std::strtoull(argv[5], nullptr, 10) : 0);
	return usage();
}
