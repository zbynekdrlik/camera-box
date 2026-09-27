/*
 * issue 1381 -- the av-sync dock's audio marker demod: its cost per OBS audio push, and that the
 * faster decode reports exactly what the old whole-window decode reported.
 *
 * Live 27.9.2026 on the resolume cg OBS the dock demodulated the whole program mix on libobs's
 * audio thread, re-decoding its whole 3-marker window on every 1024-frame push: with music on
 * program the mixer fell 13-22 s behind real time. The investigation bench (this file's origin,
 * work-products/issue-1381-dock-bench) measured 11.5 ms per push for 2-channel music and 65 ms for
 * a 2-channel 442 Hz tone, on an N100, against a 21.3 ms audio tick.
 *
 * Checks (exit 0 + "ALL PASS"):
 *   1. identity: StreamingMarkerDecoder reports the same (absolute sample, index) markers, push by
 *      push, as the pre-1381 decoder (re-decode the whole window each push, `ReferenceDecoder`
 *      below) on the marker fixtures: every one of the 256 indices, a rig-cadence marker track over
 *      silence and over white noise, and both channels of the real stereo mbc fixture, at several
 *      callback sizes;
 *   2. the worker cost: ChannelMarkerPicker::dock, per 1024-frame push, in THREAD CPU time (the
 *      work itself, not the scheduling noise of a loaded box), for silence / white / pink /
 *      music-like / a 442 Hz tone, mono and stereo. The 2-channel music-like input and the
 *      2-channel tone (every position passes the preamble screen -- the worst case) must stay
 *      <= CB_BENCH_WORKER_BUDGET_MS per push on average.
 *
 * Build (Linux; tests/av_sync_dock_audio_worker_1381.rs does this on every CI run):
 *   g++ -std=c++11 -O2 -Wall -Wextra -Werror -pthread -Ivendor/av-sync-dock/src
 *       -Ivendor/av-sync-dock/test tests/c/av_sync_dock_demod_bench_1381.cpp -o bench
 *   ./bench tests/fixtures/mbc-stereo-skew-1367/mbc-stereo-2s.wav
 */

#include "camera-box-channel-pick.hpp"
#include "cb-marker-emitter.hpp"

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <ctime>
#include <fstream>
#include <iterator>
#include <random>
#include <string>
#include <vector>

using namespace camerabox;

static int g_failures = 0;
#define CHECK(cond, msg)                                                             \
	do {                                                                         \
		if (!(cond)) {                                                       \
			std::printf("FAIL: %s  (%s:%d)\n", msg, __FILE__, __LINE__); \
			g_failures++;                                                \
		}                                                                    \
	} while (0)

/* The design acceptance (issue 1381, comment 5858886583): the audio-worker decode of a 2-channel
 * music-like mix stays within 2 ms per 1024-frame push. */
static const double CB_BENCH_WORKER_BUDGET_MS = 2.0;
static const size_t CB_BENCH_BLOCK = 1024;
static const size_t CB_BENCH_PUSHES = 1406; // ~30 s of 48 kHz audio

static double thread_cpu_ms()
{
	struct timespec ts;
	clock_gettime(CLOCK_THREAD_CPUTIME_ID, &ts);
	return (double)ts.tv_sec * 1e3 + (double)ts.tv_nsec / 1e6;
}

/* The pre-1381 StreamingMarkerDecoder::push, verbatim: append, trim to capacity, re-decode the WHOLE
 * window, report each marker once by absolute sample index. The identity reference. */
struct ReferenceDecoder {
	std::vector<float> buf;
	size_t capacity;
	uint64_t origin = 0;
	bool have_last = false;
	uint64_t last_reported = 0;
	uint64_t min_gap;

	ReferenceDecoder(size_t cap, uint64_t gap) : capacity(cap), min_gap(gap) {}

	std::vector<std::pair<uint64_t, uint8_t>> push(const float *samples, size_t len)
	{
		buf.insert(buf.end(), samples, samples + len);
		if (buf.size() > capacity) {
			size_t drop = buf.size() - capacity;
			buf.erase(buf.begin(), buf.begin() + drop);
			origin += (uint64_t)drop;
		}
		std::vector<std::pair<uint64_t, uint8_t>> out;
		const double sr = (double)CB_AUDIO_SAMPLE_RATE;
		std::vector<std::pair<double, uint8_t>> found =
			cb_decode_markers_with_stats(buf, CB_AUDIO_SAMPLE_RATE, CB_AUDIO_CARRIER_HZ, CB_AUDIO_C,
						     CB_QPSK_THRESHOLD)
				.first;
		for (size_t k = 0; k < found.size(); k++) {
			uint64_t abs = origin + (uint64_t)std::llround(found[k].first * sr);
			if (!have_last || abs > last_reported + min_gap) {
				have_last = true;
				last_reported = abs;
				out.push_back(std::make_pair(abs, found[k].second));
			}
		}
		return out;
	}
};

typedef std::vector<std::pair<uint64_t, uint8_t>> Markers;

/* Push `x` through the new decoder and the reference in `chunk`-frame callbacks; true when every
 * callback returned the same markers. `all` collects the new decoder's markers. */
static bool same_as_reference(const std::vector<float> &x, size_t chunk, Markers *all)
{
	const size_t sig = cb_signal_len(CB_AUDIO_SAMPLE_RATE, CB_AUDIO_CARRIER_HZ, CB_AUDIO_C);
	StreamingMarkerDecoder dec(CB_AUDIO_SAMPLE_RATE, CB_AUDIO_CARRIER_HZ, CB_AUDIO_C, CB_QPSK_THRESHOLD,
				   sig * 3, (uint64_t)sig);
	ReferenceDecoder ref(sig * 3, (uint64_t)sig);
	bool same = true;
	for (size_t at = 0; at < x.size(); at += chunk) {
		const size_t n = std::min(chunk, x.size() - at);
		Markers a = dec.push(x.data() + at, n);
		Markers b = ref.push(x.data() + at, n);
		if (a != b) {
			if (same)
				std::printf("  first difference at sample %zu: new %zu marker(s), reference %zu\n",
					    at, a.size(), b.size());
			same = false;
		}
		if (all)
			all->insert(all->end(), a.begin(), a.end());
	}
	return same;
}

/* The investigation bench's signals, deterministic per seed. */
static std::vector<float> make(const std::string &kind, size_t n, unsigned seed)
{
	std::vector<float> x(n, 0.f);
	std::mt19937 rng(seed);
	std::normal_distribution<float> g(0.f, 1.f);
	const double sr = 48000.0;
	double b0 = 0, b1 = 0, b2 = 0;
	for (size_t i = 0; i < n; i++) {
		double t = (double)i / sr, w = g(rng), v = 0;
		if (kind == "white") {
			v = 0.1 * w;
		} else if (kind == "pink") {
			b0 = 0.99765 * b0 + w * 0.0990460;
			b1 = 0.96300 * b1 + w * 0.2965164;
			b2 = 0.57000 * b2 + w * 1.0526913;
			v = 0.05 * (b0 + b1 + b2 + w * 0.1848);
		} else if (kind == "music") {
			// chord A3/C#4/E4/A4 with harmonics, 2 Hz tremolo, over a pink bed
			double env = 0.6 + 0.4 * std::sin(2 * CB_PI * 2.0 * t);
			const double f[4] = {220.0, 277.18, 329.63, 440.0};
			for (int k = 0; k < 4; k++)
				for (int h = 1; h <= 4; h++)
					v += 0.04 / h * std::sin(2 * CB_PI * f[k] * h * t + k);
			b0 = 0.99765 * b0 + w * 0.0990460;
			b1 = 0.96300 * b1 + w * 0.2965164;
			v = env * v + 0.02 * (b0 + b1);
		} else if (kind == "tone442") {
			v = 0.3 * std::sin(2 * CB_PI * 442.0 * t) + 0.02 * w;
		}
		x[i] = (float)v;
	}
	return x;
}

/* A rig-cadence marker track: a marker every ~3 s (180 frames at 60 fps), index stepping by 180. */
static std::vector<float> marker_track(size_t n, size_t first, float gain)
{
	std::vector<float> x(n, 0.f);
	uint8_t idx = 189;
	for (size_t at = first; at < n; at += 144000 + 37) {
		cbtest::add_marker(x, at, idx, gain);
		idx = (uint8_t)(idx + 180);
	}
	return x;
}

/* The committed 48 kHz stereo s16 fixture as two f32 channels (s16 / 32768, like ffmpeg). */
static bool read_stereo_wav(const char *path, std::vector<float> &l, std::vector<float> &r)
{
	std::ifstream in(path, std::ios::binary);
	if (!in)
		return false;
	std::vector<unsigned char> b((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
	if (b.size() < 12 || std::memcmp(&b[0], "RIFF", 4) != 0 || std::memcmp(&b[8], "WAVE", 4) != 0)
		return false;
	size_t pos = 12;
	while (pos + 8 <= b.size()) {
		const size_t len = (size_t)b[pos + 4] | ((size_t)b[pos + 5] << 8) | ((size_t)b[pos + 6] << 16) |
				   ((size_t)b[pos + 7] << 24);
		if (std::memcmp(&b[pos], "data", 4) == 0) {
			const size_t end = std::min(b.size(), pos + 8 + len);
			for (size_t p = pos + 8; p + 4 <= end; p += 4) {
				const int16_t s0 = (int16_t)(b[p] | (b[p + 1] << 8));
				const int16_t s1 = (int16_t)(b[p + 2] | (b[p + 3] << 8));
				l.push_back((float)s0 / 32768.0f);
				r.push_back((float)s1 / 32768.0f);
			}
			return !l.empty();
		}
		pos += 8 + len + (len & 1);
	}
	return false;
}

static void check_identity(const char *fixture)
{
	std::printf("== identity with the whole-window decode ==\n");
	const size_t chunks[4] = {1024, 480, 441, 256};
	/* every index, as one marker after lead silence, at two callback sizes */
	{
		int same = 0, decoded = 0;
		for (int idx = 0; idx <= 255; idx++) {
			std::vector<float> x(48000 / 2, 0.f);
			cbtest::add_marker(x, 4801 + (size_t)idx * 7, (uint8_t)idx, 1.0f);
			Markers got;
			bool ok = same_as_reference(x, 1024, &got) && same_as_reference(x, 441, nullptr);
			same += ok ? 1 : 0;
			decoded += got.size() == 1 && got[0].second == (uint8_t)idx ? 1 : 0;
		}
		std::printf("  256 indices: %d identical, %d decoded\n", same, decoded);
		CHECK(same == 256, "every index: the new decoder matches the whole-window decode");
		CHECK(decoded == 256, "every index decodes");
	}
	/* a rig-cadence track over silence and over white noise (~-35 dB below the marker) */
	for (int noisy = 0; noisy < 2; noisy++) {
		std::vector<float> x = marker_track(48000 * 20, 12345, 0.8f);
		if (noisy) {
			std::vector<float> w = make("white", x.size(), 7);
			for (size_t i = 0; i < x.size(); i++)
				x[i] += 0.02f * w[i];
		}
		for (size_t c = 0; c < 4; c++) {
			Markers got;
			const bool ok = same_as_reference(x, chunks[c], &got);
			std::printf("  track %s chunk %zu: %s, %zu markers\n", noisy ? "white" : "silence",
				    chunks[c], ok ? "identical" : "DIFFERENT", got.size());
			CHECK(ok, "marker track: the new decoder matches the whole-window decode");
			CHECK(got.size() == 7, "marker track: all 7 markers decode");
		}
	}
	/* the real stereo mbc fixture, per channel (issue 1367) */
	std::vector<float> l, r;
	const bool have = fixture && read_stereo_wav(fixture, l, r);
	CHECK(have, "read the stereo mbc fixture");
	if (have) {
		for (size_t c = 0; c < 4; c++) {
			Markers gl, gr;
			const bool okl = same_as_reference(l, chunks[c], &gl);
			const bool okr = same_as_reference(r, chunks[c], &gr);
			std::printf("  mbc fixture chunk %zu: L %s (%zu), R %s (%zu)\n", chunks[c],
				    okl ? "identical" : "DIFFERENT", gl.size(), okr ? "identical" : "DIFFERENT",
				    gr.size());
			CHECK(okl && okr, "stereo fixture: the new decoder matches the whole-window decode");
			CHECK(!gr.empty(), "stereo fixture: R decodes markers");
		}
	}
}

static void bench_worker()
{
	std::printf("== worker decode cost per %zu-frame push (thread CPU time) ==\n", CB_BENCH_BLOCK);
	const char *kinds[5] = {"silence", "white", "pink", "music", "tone442"};
	for (int k = 0; k < 5; k++) {
		const std::string kind = kinds[k];
		std::vector<float> L = make(kind, CB_BENCH_BLOCK * CB_BENCH_PUSHES, 1);
		std::vector<float> R = make(kind, CB_BENCH_BLOCK * CB_BENCH_PUSHES, 2);
		for (size_t nch = 1; nch <= 2; nch++) {
			ChannelMarkerPicker pk = ChannelMarkerPicker::dock(nch, CB_AUDIO_SAMPLE_RATE,
									   CB_AUDIO_CARRIER_HZ, CB_AUDIO_C);
			std::vector<double> ms;
			ms.reserve(CB_BENCH_PUSHES);
			for (size_t p = 0; p < CB_BENCH_PUSHES; p++) {
				const float *planes[2] = {L.data() + p * CB_BENCH_BLOCK, R.data() + p * CB_BENCH_BLOCK};
				const double t0 = thread_cpu_ms();
				(void)pk.push(planes, CB_BENCH_BLOCK);
				ms.push_back(thread_cpu_ms() - t0);
			}
			std::vector<double> s = ms;
			std::sort(s.begin(), s.end());
			double sum = 0;
			for (size_t i = 0; i < ms.size(); i++)
				sum += ms[i];
			const double mean = sum / (double)ms.size();
			std::printf("  %-8s ch=%zu  mean_ms=%.3f p99_ms=%.3f max_ms=%.3f  screens/push=%.0f\n",
				    kind.c_str(), nch, mean, s[s.size() * 99 / 100], s.back(),
				    (double)pk.stats.preamble_screens_passed / (double)CB_BENCH_PUSHES);
			if (nch == 2 && (kind == "music" || kind == "tone442"))
				CHECK(mean <= CB_BENCH_WORKER_BUDGET_MS,
				      "2-channel music / tone: the worker decode stays within its per-push budget");
		}
	}
}

int main(int argc, char **argv)
{
	check_identity(argc > 1 ? argv[1] : nullptr);
	bench_worker();
	if (g_failures) {
		std::printf("%d FAILURE(S)\n", g_failures);
		return 1;
	}
	std::printf("ALL PASS\n");
	return 0;
}
