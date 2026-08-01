/*
 * camera-box A/V-sync dock — pure-logic self-test (#398).
 *
 * Cross-checks the dependency-free C++ mirror headers (`camera-box-audio.hpp`,
 * `camera-box-video.hpp`) against the SAME results the Tier-0 Rust proves (`src/av_sync_dock.rs`,
 * `src/qpsk_marker.rs`). Because those headers pull in NO libobs/quirc/Qt, this compiles + runs with
 * a plain `g++ -std=c++11` on any machine (and in the fast CI gate) — the honest, rig-free proof that
 * the C++ decode the dock ships is the proven camera-box decode, not norihiro's broken-at-c=1 path.
 *
 * Exit 0 = all checks pass; non-zero = a mismatch (prints which). Add a case whenever you touch a
 * mirrored function. The Rust tests remain the authoritative gate; this pins the C++ port to them.
 */

#include "../src/camera-box-audio.hpp"
#include "../src/camera-box-video.hpp"

#include <cstdio>
#include <cmath>
#include <vector>
#include <cstdint>

using namespace camerabox;

static int g_failures = 0;
#define CHECK(cond, msg)                                                     \
	do {                                                                \
		if (!(cond)) {                                               \
			std::printf("FAIL: %s  (%s:%d)\n", msg, __FILE__, __LINE__); \
			g_failures++;                                       \
		}                                                           \
	} while (0)

/* ----- reference emitter (port of qpsk_marker.rs::marker_signal at c=1) -----
 * Kept in the TEST only (never compiled into the plugin): the dock never emits — cam2 does. This
 * lets the self-test round-trip a marker the SAME way the Rust `round_trip_all_256_indices` does. */
static uint32_t enc_crc4(uint32_t data, uint32_t size)
{
	data <<= 4;
	uint32_t p = 0x13u << (size - 1);
	long s = (long)size;
	while (s > 0) {
		if (data & (0x8u << s))
			data ^= p;
		s -= 1;
		p >>= 1;
	}
	return data;
}
static uint32_t enc_payload_word(uint8_t index)
{
	uint32_t data16 = 0xF000u | (uint32_t)index;
	return (data16 << 4) | enc_crc4(data16, 16);
}
static double sym_wave(uint32_t sym, double phase)
{
	switch (sym) {
	case 0:
		return std::sin(phase);
	case 1:
		return std::cos(phase);
	case 2:
		return -std::cos(phase);
	case 3:
		return -std::sin(phase);
	}
	return 0.0;
}
static std::vector<float> marker_signal(uint8_t index)
{
	const uint32_t sr = CB_AUDIO_SAMPLE_RATE, f = CB_AUDIO_CARRIER_HZ, c = CB_AUDIO_C;
	uint32_t word = enc_payload_word(index);
	uint32_t sym[10];
	for (uint32_t i = 0; i < 10; i++)
		sym[i] = (word >> (20 - 2 - 2 * i)) & 0x3u;
	size_t n = cb_signal_len(sr, f, c);
	std::vector<float> out;
	out.reserve(n);
	const double CONT = 0.25;
	for (uint32_t i = 0; i < n; i++) {
		double phase = (double)i * 2.0 * CB_PI * (double)f / (double)sr;
		uint32_t k = (i * f) / (sr * c);
		if (k > 9)
			k = 9;
		double v = sym_wave(sym[k], phase);
		double f_sym = (double)((i * f) % (sr * c)) / (double)sr;
		int prev = k > 0 ? (int)sym[k - 1] : -1;
		int next = k + 1 < 10 ? (int)sym[k + 1] : -1;
		if (f_sym < CONT && (int)sym[k] != prev)
			v *= 0.5 - std::cos(f_sym / CONT * CB_PI) * 0.5;
		else if (((double)c - f_sym) < CONT && (int)sym[k] != next)
			v *= 0.5 - std::cos(((double)c - f_sym) / CONT * CB_PI) * 0.5;
		out.push_back((float)v);
	}
	return out;
}

int main()
{
	/* 1. The reference emitter matches the Rust marker_signal golden values (pins the C++ encoder,
	 * hence the round-trip below, to the REAL cam2 emitter output). */
	CHECK(cb_signal_len(48000, 442, 1) == 1085, "signal_len == 1085");
	{
		std::vector<float> s0 = marker_signal(0);
		// FIRST8 (preamble ramp — identical for every index) from Rust.
		const double g0[8] = {-0.000000, -0.000193, -0.001539, -0.005151,
		                      -0.012067, -0.023215, -0.039379, -0.061173};
		for (int k = 0; k < 8; k++)
			CHECK(std::fabs((double)s0[k] - g0[k]) < 1e-4, "golden first8 idx0");
		// MID3 differs by index — idx 255 from Rust.
		std::vector<float> s255 = marker_signal(255);
		size_t m = s255.size() / 2;
		const double g255[3] = {0.057041, -0.000785, -0.058609};
		for (int k = 0; k < 3; k++)
			CHECK(std::fabs((double)s255[m + k] - g255[k]) < 1e-4, "golden mid3 idx255");
	}

	/* 2. Round-trip EVERY index through the C++ streaming decode path (mirrors the Rust
	 * round_trip_all_256_indices): emit, place after lead silence, decode, must recover the index. */
	{
		int ok = 0;
		for (int idx = 0; idx <= 255; idx++) {
			std::vector<float> buf(48000 / 2, 0.0f); // 0.5 s
			std::vector<float> sig = marker_signal((uint8_t)idx);
			size_t start = 48000 / 10; // 0.1 s lead silence
			for (size_t j = 0; j < sig.size(); j++)
				buf[start + j] = sig[j];
			std::vector<std::pair<double, uint8_t>> found =
				cb_decode_markers(buf, 48000, 442, 1, 0.4);
			if (found.size() == 1 && found[0].second == (uint8_t)idx)
				ok++;
			else
				std::printf("FAIL: round-trip idx %d -> %zu markers%s\n", idx, found.size(),
				            found.empty() ? "" : "");
		}
		CHECK(ok == 256, "all 256 indices round-trip through cb_decode_markers");
	}

	/* 2b. #690: CbDecodeStats mirrors qpsk_marker::DecodeStats (mirrors the Rust
	 * decode_stats_count_* tests). A clean marker -> crc_ok==1, preamble_screens_passed>=crc_ok,
	 * crc_fail==0. Silence -> all-zero. A marker with one whole symbol window negated (corrupts an
	 * index-field bit while leaving the preamble energy intact) -> screened but never CRC-valid. */
	{
		std::vector<float> buf(48000 / 2, 0.0f);
		std::vector<float> sig = marker_signal(77);
		size_t start = 48000 / 10;
		for (size_t j = 0; j < sig.size(); j++)
			buf[start + j] = sig[j];
		std::pair<std::vector<std::pair<double, uint8_t>>, CbDecodeStats> r =
			cb_decode_markers_with_stats(buf, 48000, 442, 1, 0.4);
		CHECK(r.first.size() == 1 && r.first[0].second == 77, "clean marker still decodes idx 77");
		CHECK(r.second.crc_ok == 1, "clean marker: crc_ok == 1");
		CHECK(r.second.preamble_screens_passed >= r.second.crc_ok, "screens >= crc_ok");
		// cb_decode_markers (thin wrapper) returns the identical markers.
		std::vector<std::pair<double, uint8_t>> plain = cb_decode_markers(buf, 48000, 442, 1, 0.4);
		CHECK(plain.size() == r.first.size() && plain[0].second == r.first[0].second,
		      "cb_decode_markers wrapper matches cb_decode_markers_with_stats");
	}
	{
		std::vector<float> silence(48000, 0.0f);
		std::pair<std::vector<std::pair<double, uint8_t>>, CbDecodeStats> r =
			cb_decode_markers_with_stats(silence, 48000, 442, 1, 0.4);
		CHECK(r.first.empty(), "silence decodes nothing");
		CHECK(r.second.preamble_screens_passed == 0, "silence: no preamble screens");
		CHECK(r.second.crc_ok == 0 && r.second.crc_fail == 0, "silence: crc counters stay zero");
	}
	{
		std::vector<float> buf(48000 / 2, 0.0f);
		std::vector<float> sig = marker_signal(200);
		size_t sym5_start = sig.size() * 5 / 10, sym5_end = sig.size() * 6 / 10;
		for (size_t j = sym5_start; j < sym5_end; j++)
			sig[j] = -sig[j];
		size_t start = 48000 / 10;
		for (size_t j = 0; j < sig.size(); j++)
			buf[start + j] = sig[j];
		std::pair<std::vector<std::pair<double, uint8_t>>, CbDecodeStats> r =
			cb_decode_markers_with_stats(buf, 48000, 442, 1, 0.4);
		CHECK(r.first.empty(), "corrupted marker must not decode a valid index");
		CHECK(r.second.preamble_screens_passed > 0, "corrupted marker: still screened");
		CHECK(r.second.crc_ok == 0, "corrupted marker: never crc_ok");
		CHECK(r.second.crc_fail > 0, "corrupted marker: counted as crc_fail");
	}

	/* 3. Streaming dedup: two markers fed in small chunks, each reported exactly once with the right
	 * index (mirrors av_sync_dock::streaming_decoder_reports_each_marker_once...). */
	{
		size_t sr = 48000, sig = cb_signal_len(48000, 442, 1);
		std::vector<float> stream(sr * 8, 0.0f);
		uint8_t i0 = (uint8_t)(2000u & 0xFF), i1 = (uint8_t)(2431u & 0xFF);
		std::vector<float> s0 = marker_signal(i0), s1 = marker_signal(i1);
		for (size_t j = 0; j < s0.size(); j++)
			stream[sr * 1 + j] = s0[j];
		for (size_t j = 0; j < s1.size(); j++)
			stream[sr * 5 + j] = s1[j];
		StreamingMarkerDecoder dec(48000, 442, 1, CB_QPSK_THRESHOLD, sig * 3, (uint64_t)sig);
		std::vector<std::pair<uint64_t, uint8_t>> got;
		for (size_t off = 0; off < stream.size(); off += 480) {
			size_t len = std::min((size_t)480, stream.size() - off);
			std::vector<std::pair<uint64_t, uint8_t>> r = dec.push(&stream[off], len);
			got.insert(got.end(), r.begin(), r.end());
		}
		CHECK(got.size() == 2, "streaming: exactly 2 markers");
		if (got.size() == 2) {
			CHECK(got[0].second == i0 && got[1].second == i1, "streaming: indices");
			CHECK(std::llabs((long long)got[0].first - (long long)sr) < 8, "streaming: pos0");
			CHECK(std::llabs((long long)got[1].first - (long long)(sr * 5)) < 8, "streaming: pos1");
		}
		// #690: the two decoded markers must have driven StreamingMarkerDecoder::stats.crc_ok >= 2.
		CHECK(dec.stats.crc_ok >= 2, "streaming: stats.crc_ok reflects the decoded markers");
		CHECK(dec.stats.preamble_screens_passed >= dec.stats.crc_ok, "streaming: screens >= crc_ok");
	}
	{
		// #690: a fresh decoder over pure silence must report all-zero stats.
		StreamingMarkerDecoder dec(48000, 442, 1, CB_QPSK_THRESHOLD, cb_signal_len(48000, 442, 1) * 3,
		                           (uint64_t)cb_signal_len(48000, 442, 1));
		CHECK(dec.stats.preamble_screens_passed == 0 && dec.stats.crc_ok == 0 && dec.stats.crc_fail == 0,
		      "streaming: fresh decoder stats start at zero");
		std::vector<float> silence(4800, 0.0f);
		dec.push(silence.data(), silence.size());
		CHECK(dec.stats.preamble_screens_passed == 0 && dec.stats.crc_ok == 0,
		      "streaming: silence push leaves stats at zero");
	}

	/* 4. Rolling cluster locks the real offset among a heavy false-decode scatter, and rejects a
	 * wide band (mirrors the two av_sync_dock rolling-cluster tests). */
	{
		RollingOffsetCluster c = RollingOffsetCluster::dock();
		double cycle_ms = (double)(256ull * 1000000000ull / 60ull) / 1e6; // ring cycle ~4266 ms
		bool locked = false;
		double locked_off = 0.0, locked_mad = 0.0;
		uint64_t t = 0;
		for (uint64_t k = 0; k < 600; k++) {
			t += 100000000ull;
			double r = (double)((k * 2654435761ull >> 8) % 100000) / 100000.0;
			double false_off = (r - 0.5) * cycle_ms;
			CbAvOffset e = c.push(t, false_off);
			if (e.ok) {
				locked = true;
				locked_off = e.offset_ms;
				locked_mad = e.mad_ms;
			}
			if (k % 30 == 0) {
				double jit = ((double)(k % 7) - 3.0) * 2.0;
				CbAvOffset e2 = c.push(t, 40.0 + jit);
				if (e2.ok) {
					locked = true;
					locked_off = e2.offset_ms;
					locked_mad = e2.mad_ms;
				}
			}
		}
		CHECK(locked, "rolling cluster locks");
		CHECK(std::fabs(locked_off - 40.0) < CB_CLUSTER_TOL_MS, "rolling cluster locks the real +40ms");
		CHECK(locked_mad <= CB_CLUSTER_MAX_MAD_MS, "rolling cluster lock is tight");
	}
	{
		// wide band, exactly min_matched points across the full window -> MAD gate rejects.
		RollingOffsetCluster c = RollingOffsetCluster::dock();
		bool any = false;
		size_t nn = CB_CLUSTER_MIN_MATCHED;
		for (size_t k = 0; k < nn; k++) {
			double off = -CB_CLUSTER_TOL_MS + (double)k * (2.0 * CB_CLUSTER_TOL_MS / (double)(nn - 1));
			if (c.push((uint64_t)k * 100000000ull, off).ok)
				any = true;
		}
		CHECK(!any, "MAD gate rejects a wide band");
	}

	/* 5. Video geometry mirrors av_sync_dock::top_band_decode_plan. */
	{
		CbTopBandPlan p4 = cb_top_band_decode_plan(3840, 2160);
		CHECK(p4.band_h == 2160u * 72 / 100, "4k band_h");
		CHECK(p4.dst_w == 760, "4k dst_w capped");
		CHECK(p4.dst_h == (2160u * 72 / 100) * 760 / 3840, "4k dst_h aspect");
		CbTopBandPlan p1 = cb_top_band_decode_plan(1920, 1080);
		CHECK(p1.band_h == 1080u * 72 / 100, "1080 band_h");
		CHECK(p1.dst_w == 760, "1080 dst_w");
		CbTopBandPlan ps = cb_top_band_decode_plan(320, 200);
		CHECK(ps.dst_w == 320 && ps.dst_h == 200u * 72 / 100, "small frame not upscaled");
		CbTopBandPlan pd = cb_top_band_decode_plan(1, 1);
		CHECK(pd.band_h >= 1 && pd.dst_w >= 1 && pd.dst_h >= 1, "degenerate frame never zero");
	}

	/* 6. Otsu mirrors av_sync_dock::otsu_threshold. */
	{
		uint64_t hist[256];
		for (int i = 0; i < 256; i++)
			hist[i] = 0;
		hist[20] = 1000;
		hist[230] = 1000;
		uint8_t t = cb_otsu_threshold(hist);
		CHECK(t > 20 && t < 230, "otsu cuts between peaks");
		uint64_t empty[256];
		for (int i = 0; i < 256; i++)
			empty[i] = 0;
		CHECK(cb_otsu_threshold(empty) == 128, "otsu empty -> 128");
	}

	/* 7. Box downscale averages (a 4x4 checker -> 2x2 of the block means). */
	{
		uint8_t src[16] = {0,   0,   255, 255, 0,   0,   255, 255,
		                   255, 255, 0,   0,   255, 255, 0,   0};
		uint8_t dst[4] = {0, 0, 0, 0};
		cb_box_downscale_luma(src, 4, 4, dst, 2, 2);
		// each 2x2 block: top-left all 0 -> 0; top-right all 255 -> 255; bottom-left 255; bottom-right 0
		CHECK(dst[0] == 0 && dst[1] == 255 && dst[2] == 255 && dst[3] == 0, "box downscale block means");
	}

	/* 8. #921: CbQrResizeCache mirrors av_sync_dock::QrResizeCache -- needed only when the size
	 * actually changes; a reset forces a fresh resize even at the SAME size as before the reset. */
	{
		CbQrResizeCache c;
		CHECK(cb_qr_resize_needed(c, 760, 307), "resize cache: first call always needed");
		CHECK(!cb_qr_resize_needed(c, 760, 307), "resize cache: repeated identical size not needed");
		CHECK(!cb_qr_resize_needed(c, 760, 307), "resize cache: still not needed");
		CHECK(cb_qr_resize_needed(c, 760, 300), "resize cache: a real size change is needed again");
		CHECK(!cb_qr_resize_needed(c, 760, 300), "resize cache: settles at the new size");
		CHECK(cb_qr_resize_needed(c, 700, 300), "resize cache: width-only change is needed");
		CHECK(cb_qr_resize_needed(c, 700, 250), "resize cache: height-only change is needed");

		CbQrResizeCache c2;
		CHECK(cb_qr_resize_needed(c2, 760, 307), "resize cache: warm up");
		CHECK(!cb_qr_resize_needed(c2, 760, 307), "resize cache: warmed up, not needed");
		c2 = CbQrResizeCache();
		CHECK(cb_qr_resize_needed(c2, 760, 307),
		      "resize cache: a reset must force a fresh resize, even at the SAME size as before");
	}

	if (g_failures == 0) {
		std::printf("camera-box-selftest: ALL PASS\n");
		return 0;
	}
	std::printf("camera-box-selftest: %d FAILURE(S)\n", g_failures);
	return 1;
}
