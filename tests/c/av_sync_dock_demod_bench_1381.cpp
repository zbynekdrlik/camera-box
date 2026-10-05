/*
 * issue 1381 -- the av-sync dock's audio marker demod: its cost per OBS audio push, and that the
 * faster decode reports exactly what the old whole-window decode reported.
 *
 * Live 27.9.2026 on the resolume cg OBS the dock demodulated the whole program mix on libobs's
 * audio thread, re-decoding its whole 3-marker window on every 1024-frame push: with music on
 * program the mixer fell 13-22 s behind real time. The investigation bench (this file's origin,
 * work-products/issue-1381-dock-bench) measured 11.5 ms of WALL time per push for 2-channel music
 * and 65 ms for a 2-channel 442 Hz tone, on an N100 at load ~20, against a 21.3 ms audio tick. This
 * bench measures THREAD CPU instead: the pre-1381 decoder (its RED run) took 6.8 ms (music) and
 * 34.8 ms (tone) per stereo push, the incremental decoder 0.18-0.24 ms and 0.56-0.7 ms.
 *
 * Checks (exit 0 + "ALL PASS"):
 *   1. identity: StreamingMarkerDecoder reports the same (absolute sample, index) markers, push by
 *      push, as the pre-1381 decoder (`ReferenceDecoder` below: the whole window re-decoded each push
 *      by a FROZEN copy of the pre-1381 kernel) on the marker fixtures: every one of the 256 indices,
 *      a rig-cadence marker track over silence and over white noise, markers over music-like and
 *      pink program audio (including the music's false decodes), and both channels of the real
 *      stereo mbc fixture, at several callback sizes;
 *   2. the worker cost: ChannelMarkerPicker::dock, per 1024-frame push, in THREAD CPU time (the
 *      work itself, not the scheduling noise of a loaded box), for silence / white / pink /
 *      music-like / a 442 Hz tone, mono and stereo. The 2-channel music-like input and the
 *      2-channel tone (every position passes the preamble screen -- the worst case) must stay
 *      <= CB_BENCH_WORKER_BUDGET_MS per push on average;
 *   3. the audio-thread cost: the dock's share of libobs's audio thread is the gate and a
 *      CbAudioBlockFifo::publish of the stereo block, while the worker decodes the blocks with the
 *      same picker. For every signal the publish must stay <= CB_BENCH_AUDIO_THREAD_BUDGET_MS of
 *      thread CPU time in the mean AND the p99 push. The worst single push is REPORTED against
 *      CB_BENCH_AUDIO_THREAD_MAX_MS, never gated: thread CPU time still carries what a shared CI
 *      runner charges to the thread. One push read 2.42 ms (cpu ~= wall) on a 0.009 ms mean, run
 *      37345820890, on a FIFO whose slots reused already-resident memory, so not a first touch of
 *      its slots; whether it was another fault (a reclaimed page) or time charged to the thread is
 *      unattributed -- that run had no fault counter. The producer's page faults are now reported
 *      beside it (first pass over the slots / every push), so the next outlier is attributed; the
 *      deterministic no-fault check lives in the audio-worker self-test, whose window is the 64
 *      publishes right after start() -- here a memory-pressured runner may legitimately reclaim and
 *      refault a page during the 2 s run. Wall-clock p99 / max are reported.
 *
 * Build (Linux; tests/av_sync_dock_demod_bench_1381.rs does this on every CI run):
 *   g++ -std=c++11 -O2 -Wall -Wextra -Werror -pthread -Ivendor/av-sync-dock/src
 *       -Ivendor/av-sync-dock/test tests/c/av_sync_dock_demod_bench_1381.cpp -o bench
 *   ./bench tests/fixtures/mbc-stereo-skew-1367/mbc-stereo-2s.wav
 */

#include "camera-box-audio-worker.hpp"
#include "camera-box-channel-pick.hpp"
#include "cb-marker-emitter.hpp"
#include "cb-thread-faults.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <complex>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <ctime>
#include <fstream>
#include <iterator>
#include <random>
#include <string>
#include <thread>
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
/* The design acceptance: the audio thread's share stays within 0.2 ms per push for every signal. */
static const double CB_BENCH_AUDIO_THREAD_BUDGET_MS = 0.2;
/* The worst single push, REPORTED (issue 1381: no longer gated -- see check 3 above): ~10x below the
 * 21.3 ms audio period. */
static const double CB_BENCH_AUDIO_THREAD_MAX_MS = 2.0;
static const size_t CB_BENCH_BLOCK = 1024;
static const size_t CB_BENCH_PUSHES = 1406; // ~30 s of 48 kHz audio

static double thread_cpu_ms()
{
	struct timespec ts;
	clock_gettime(CLOCK_THREAD_CPUTIME_ID, &ts);
	return (double)ts.tv_sec * 1e3 + (double)ts.tv_nsec / 1e6;
}

/* The pre-1381 cb_decode_markers_with_stats, frozen verbatim (camera-box-audio.hpp at dev.716): the
 * whole-window decode with the linear refine and std::abs magnitudes. The identity reference. */
static std::vector<std::pair<double, uint8_t>> reference_decode_markers(const std::vector<float> &samples)
{
	typedef std::complex<double> cd;
	std::vector<std::pair<double, uint8_t>> out;
	const uint32_t sample_rate = CB_AUDIO_SAMPLE_RATE, carrier_hz = CB_AUDIO_CARRIER_HZ, c = CB_AUDIO_C;
	const double threshold = CB_QPSK_THRESHOLD;

	double ar = (double)sample_rate;
	double f = (double)carrier_hz;
	double cc = (double)(c < 1 ? 1 : c);
	double sps = ar * cc / f;
	size_t sig_len = cb_signal_len(sample_rate, carrier_hz, (c < 1 ? 1 : c));
	size_t n = samples.size();
	if (sig_len == 0 || n < sig_len || sps < 1.0)
		return out;

	double w = 2.0 * CB_PI * f / ar;
	std::vector<double> pc(n + 1, 0.0), ps(n + 1, 0.0), pe(n + 1, 0.0);
	for (size_t m = 0; m < n; m++) {
		double ph = (double)m * w;
		double x = (double)samples[m];
		if (!std::isfinite(x))
			x = 0.0;
		pc[m + 1] = pc[m] + x * std::cos(ph);
		ps[m + 1] = ps[m] + x * std::sin(ph);
		pe[m + 1] = pe[m] + x * x;
	}
	auto z = [&](size_t a, size_t b) -> cd {
		if (a > n)
			a = n;
		if (b > n)
			b = n;
		return cd(pc[b] - pc[a], -(ps[b] - ps[a]));
	};
	auto sym_win = [&](size_t base, size_t k) -> std::pair<size_t, size_t> {
		size_t a = base + (size_t)std::llround((double)k * sps);
		size_t b = base + (size_t)std::llround((double)(k + 1) * sps);
		return std::make_pair(a, b);
	};
	auto preamble = [&](size_t base) -> cd {
		std::pair<size_t, size_t> s0 = sym_win(base, 0);
		std::pair<size_t, size_t> s1 = sym_win(base, 1);
		return z(s0.first, s0.second) + z(s1.first, s1.second);
	};
	size_t two_sym = (size_t)std::llround(2.0 * sps);
	auto norm_at = [&](size_t base) -> double {
		size_t hi = base + two_sym;
		if (hi > n)
			hi = n;
		size_t lo = base > n ? n : base;
		double e = pe[hi] - pe[lo];
		if (e < 0.0)
			e = 0.0;
		return std::sqrt(e) * std::sqrt((double)two_sym) + 1e-12;
	};

	size_t i = 0;
	while (i + sig_len <= n) {
		cd refph = preamble(i);
		if (std::abs(refph) / norm_at(i) >= threshold) {
			size_t span = (size_t)std::ceil(2.0 * sps);
			size_t lo = i >= 4 ? i - 4 : 0;
			size_t base = i;
			double bestm = std::abs(refph);
			for (size_t cand = lo; cand <= i + span; cand++) {
				if (cand + sig_len <= n) {
					double m = std::abs(preamble(cand));
					if (m > bestm) {
						bestm = m;
						base = cand;
					}
				}
			}
			cd refp = preamble(base) * cd(1.0, -1.0);
			uint32_t word = 0;
			for (uint32_t k = 0; k < CB_N_SYMBOLS; k++) {
				std::pair<size_t, size_t> ab = sym_win(base, k);
				cd zz = z(ab.first, ab.second);
				double d = refp.real() * refp.real() + refp.imag() * refp.imag() + 1e-12;
				double re = (zz.real() * refp.real() + zz.imag() * refp.imag()) / d;
				double im = (zz.imag() * refp.real() - zz.real() * refp.imag()) / d;
				uint32_t sym = (uint32_t)(im > 0.0 ? 2 : 0) | (uint32_t)(re > 0.0 ? 1 : 0);
				word |= sym << (CB_N_PAYLOAD_BITS - 2 - 2 * k);
			}
			if (((word >> 16) & 0xF) == CB_PREAMBLE_NIBBLE && ((word >> 12) & 0xF) == 0 &&
			    cb_crc4_check(word, CB_N_PAYLOAD_BITS) == 0) {
				out.push_back(std::make_pair((double)base / ar, (uint8_t)((word >> 4) & 0xFF)));
				i = base + sig_len;
				continue;
			}
		}
		i += 1;
	}
	return out;
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
		std::vector<std::pair<double, uint8_t>> found = reference_decode_markers(buf);
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
	/* program audio: markers over the music-like mix (which also decodes false markers) and over
	 * pink noise -- the scan-path cases where the incremental screen could part from the reference */
	const char *mixes[2] = {"music", "pink"};
	for (int k = 0; k < 2; k++) {
		std::vector<float> x = make(mixes[k], 48000 * 8, 11 + k);
		std::vector<float> m = marker_track(x.size(), 23456, 0.2f);
		for (size_t i = 0; i < x.size(); i++)
			x[i] += m[i];
		for (size_t c = 0; c < 4; c += 3) {
			Markers got;
			const bool ok = same_as_reference(x, chunks[c], &got);
			std::printf("  %s + markers chunk %zu: %s, %zu markers\n", mixes[k], chunks[c],
				    ok ? "identical" : "DIFFERENT", got.size());
			CHECK(ok, "markers over program audio: the new decoder matches the whole-window decode");
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
			if (kind == "tone442") {
				/* The tone passes the screen at every position, so this counts the positions a push
				 * screens: the new ones plus at most one refine span of re-screened tail, per
				 * channel -- never the whole window again. */
				const double span = std::ceil(2.0 * CB_AUDIO_SAMPLE_RATE / CB_AUDIO_CARRIER_HZ);
				const double per_push = (double)pk.stats.preamble_screens_passed / (double)CB_BENCH_PUSHES;
				CHECK(per_push >= 0.9 * (double)(nch * CB_BENCH_BLOCK),
				      "tone: nearly every new position passes the screen");
				CHECK(per_push <= (double)nch * ((double)CB_BENCH_BLOCK + span + 1.0),
				      "tone: a push screens only the positions not final yet");
			}
		}
	}
}

static double wall_ms()
{
	return std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now().time_since_epoch()).count();
}

static void bench_audio_thread()
{
	std::printf("== audio-thread cost per %zu-frame push (gate + FIFO copy, worker decoding) ==\n",
		    CB_BENCH_BLOCK);
	const char *kinds[5] = {"silence", "white", "pink", "music", "tone442"};
	const size_t pushes = 1000;
	for (int k = 0; k < 5; k++) {
		const std::string kind = kinds[k];
		std::vector<float> L = make(kind, CB_BENCH_BLOCK * pushes, 1);
		std::vector<float> R = make(kind, CB_BENCH_BLOCK * pushes, 2);
		ChannelMarkerPicker pk = ChannelMarkerPicker::dock(2, CB_AUDIO_SAMPLE_RATE, CB_AUDIO_CARRIER_HZ, CB_AUDIO_C);
		CbAudioBlockFifo fifo;
		CbAudioBlockFifo::Handlers h;
		h.process = [&pk](const CbAudioBlock &b) {
			const float *planes[2] = {b.planes[0].data(), b.planes[1].data()};
			(void)pk.push(planes, b.frames);
		};
		h.on_gap = [&pk](const CbAudioBlock &) { pk.reset_window(); };
		fifo.start(h, 2, CB_BENCH_BLOCK);
		std::vector<double> cpu, wall;
		cpu.reserve(pushes);
		wall.reserve(pushes);
		uint64_t accepted = 0, faults_first = 0, faults_all = 0;
		/* 2 ms per push: ten times real time, still slower than the worker's worst case */
		const double period_ms = 2.0;
		double next = wall_ms();
		for (size_t p = 0; p < pushes; p++) {
			const float *planes[2] = {L.data() + p * CB_BENCH_BLOCK, R.data() + p * CB_BENCH_BLOCK};
			/* The fault reads bracket the timed window from outside, so they cost it nothing. */
			const uint64_t f0 = cb_thread_page_faults();
			const double w0 = wall_ms(), c0 = thread_cpu_ms();
			const CbAudioGate gate = cb_audio_decode_gate(true, true, (uint64_t)p * 21333333u,
								      (uint64_t)p * 21333333u, 20000000000ull);
			if (gate == CbAudioGate::Open && fifo.publish(planes, 2, CB_BENCH_BLOCK, (uint64_t)p))
				accepted++;
			const double c1 = thread_cpu_ms(), w1 = wall_ms();
			const uint64_t df = cb_thread_page_faults() - f0;
			faults_all += df;
			if (p < CB_AUDIO_FIFO_SLOTS)
				faults_first += df;
			cpu.push_back(c1 - c0);
			wall.push_back(w1 - w0);
			next += period_ms;
			const double wait = next - wall_ms();
			if (wait > 0)
				std::this_thread::sleep_for(std::chrono::microseconds((long long)(wait * 1000.0)));
		}
		for (int spin = 0; fifo.taken() < accepted && spin < 5000; spin++)
			std::this_thread::sleep_for(std::chrono::milliseconds(1));
		const uint64_t worker_sum_ns = fifo.take_process_sum_ns();
		const uint64_t worker_max_ns = fifo.take_process_max_ns();
		fifo.stop();
		std::vector<double> sc = cpu, sw = wall;
		std::sort(sc.begin(), sc.end());
		std::sort(sw.begin(), sw.end());
		double sum = 0;
		for (size_t i = 0; i < cpu.size(); i++)
			sum += cpu[i];
		const double mean = sum / (double)cpu.size();
		const double p99 = sc[sc.size() * 99 / 100];
		std::printf("  %-8s ch=2  cpu mean_ms=%.4f p99_ms=%.4f max_ms=%.4f  faults=%llu/%llu  wall p99_ms=%.4f "
			    "max_ms=%.4f  dropped=%llu  worker mean_ms=%.3f max_ms=%.3f\n",
			    kind.c_str(), mean, p99, sc.back(), (unsigned long long)faults_first,
			    (unsigned long long)faults_all, sw[sw.size() * 99 / 100], sw.back(),
			    (unsigned long long)fifo.dropped(),
			    accepted ? (double)worker_sum_ns / 1e6 / (double)accepted : 0.0, (double)worker_max_ns / 1e6);
		if (sc.back() >= CB_BENCH_AUDIO_THREAD_MAX_MS)
			std::printf("  REPORT (not gated): %s: the worst push read %.4f ms of thread CPU time, over the %.1f ms "
				    "report bound; producer page faults this run: %llu\n",
				    kind.c_str(), sc.back(), CB_BENCH_AUDIO_THREAD_MAX_MS, (unsigned long long)faults_all);
		CHECK(mean <= CB_BENCH_AUDIO_THREAD_BUDGET_MS && p99 <= CB_BENCH_AUDIO_THREAD_BUDGET_MS,
		      "the audio thread's share (gate + FIFO copy) stays within its per-push budget (mean and p99)");
		CHECK(fifo.taken() == accepted, "the worker handled every accepted block");
	}
}

int main(int argc, char **argv)
{
	check_identity(argc > 1 ? argv[1] : nullptr);
	bench_worker();
	bench_audio_thread();
	if (g_failures) {
		std::printf("%d FAILURE(S)\n", g_failures);
		return 1;
	}
	std::printf("ALL PASS\n");
	return 0;
}
