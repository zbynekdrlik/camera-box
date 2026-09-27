#pragma once

/*
 * camera-box QPSK marker scan + the dock's streaming marker decoder.
 *
 * Split out of camera-box-audio.hpp (which includes this header) when issue 1381 reworked both: that
 * header had grown past its size budget, and these pieces are one unit -- the demod kernel and the
 * decoder that feeds it the live audio. Everything here is a byte-for-byte port of Rust:
 * `cb_scan_markers` = qpsk_marker::scan_markers, `cb_decode_markers_with_stats` =
 * qpsk_marker::decode_markers_with_stats, `StreamingMarkerDecoder` =
 * av_sync_dock::StreamingMarkerDecoder. Keep the two in lock-step; test/camera-box-selftest.cpp and
 * tests/qpsk_channel_pick_parity_1367.rs cross-check them.
 *
 * issue 1381 -- the cost. The dock used to re-decode its whole 3-marker window on every OBS audio
 * push, re-computing ~223 preamble magnitudes for every position that passed the screen: 6.8 ms of
 * CPU per stereo push for music, 35 ms for a 442 Hz tone (N100), against a 21.3 ms audio tick. Two
 * changes, neither of which changes a single decoded marker:
 *   - the refine is a sliding-window maximum (`CbRefineWindow`): each position's |preamble| is
 *     computed once per scan and the window maximum comes from a monotonic queue, O(1) amortized,
 *     landing on the same position the old linear refine did; the carrier cos/sin over the window
 *     is a table (`CbScanWorkspace`), bit-identical to the inline calls;
 *   - the streaming decoder screens only positions that are not final yet (`next_scan`): the new
 *     positions, plus the tail whose refine range the previous window end cut. Re-screening that
 *     tail is what keeps the reported markers identical to the whole-window decode, whose first
 *     sight of a marker can be such a cut refine.
 *
 * Dependency-free (STL + <cmath> only), C++11.
 */

#include <algorithm>
#include <cmath>
#include <complex>
#include <cstdint>
#include <utility>
#include <vector>

namespace camerabox {

/* ---- constants (mirror src/qpsk_marker.rs) ---- */
static const double CB_PI = 3.14159265358979323846;
static const uint32_t CB_N_SYMBOLS = 10;      // 20 payload bits / 2 bits per QPSK symbol
static const uint32_t CB_N_PAYLOAD_BITS = 20; // 4 preamble + 8 index + 4 zero + 4 CRC
static const uint32_t CB_PREAMBLE_NIBBLE = 0xF;

/* signal_len: samples in a marker's 10-symbol signal = N_SYMBOLS * c * sr / f (mirror
 * qpsk_marker::signal_len; integer floor, u64 to avoid overflow). */
inline size_t cb_signal_len(uint32_t sample_rate, uint32_t carrier_hz, uint32_t c)
{
	if (carrier_hz == 0)
		return 0;
	uint64_t n = (uint64_t)CB_N_SYMBOLS * (uint64_t)c * (uint64_t)sample_rate / (uint64_t)carrier_hz;
	return (size_t)n;
}

/* CRC-4/ITU residual check (mirror qpsk_marker::crc4_check). Returns 0 for a valid word. */
inline uint32_t cb_crc4_check(uint32_t data, uint32_t size)
{
	uint32_t p = 0x13u << (size - 5);
	while (size > 4) {
		if (data & (1u << (size - 1)))
			data ^= p;
		size--;
		p >>= 1;
	}
	return data;
}

/* #690 -- diagnostic counters for one scan (mirror of qpsk_marker::DecodeStats). Pure counting, zero
 * effect on the returned markers -- lets a live session tell apart "the demod sees nothing"
 * (preamble_screens_passed==0) from "sees candidates but they're garbage" (crc_fail>0, crc_ok==0)
 * from "decodes fine" (crc_ok>0, in which case a still-empty live Audio Index points further
 * downstream, at the ring lookup / cluster lock). */
struct CbDecodeStats {
	uint64_t preamble_screens_passed = 0;
	uint64_t crc_ok = 0;
	uint64_t crc_fail = 0;
};

/* The carrier angle per sample, w = 2 pi f / sr: ONE expression for the scan and its table, so a
 * table entry is bit-identical to the inline std::cos((double)m * w). */
inline double cb_carrier_w(uint32_t sample_rate, uint32_t carrier_hz)
{
	double ar = (double)sample_rate;
	double f = (double)carrier_hz;
	return 2.0 * CB_PI * f / ar;
}

/* issue 1381 -- the refine's sliding window over |preamble(p)| (mirror qpsk_marker::RefineWindow).
 * The scan asks for windows [from, to] whose ends never move back, so each position's magnitude is
 * computed ONCE per scan (kept in a ring) and the window's maximum comes from a monotonic queue: the
 * positions whose magnitude is not exceeded by a later one, oldest first, magnitudes non-increasing.
 * Its front is the LEFTMOST position holding the window maximum -- the position the old linear
 * refine landed on (it only moved on a strictly larger magnitude). O(1) amortized per position. */
struct CbRefineWindow {
	std::vector<double> mag; // mag[p % size] = |preamble(p)| for p in [lo, hi)
	size_t lo = 0, hi = 0;
	std::vector<size_t> q; // the monotonic queue, a ring of positions q[head .. tail)
	size_t head = 0, tail = 0;

	/* `ring` must be at least the widest window asked for. */
	void reset(size_t ring)
	{
		mag.resize(ring);
		q.resize(ring);
		lo = hi = 0;
		head = tail = 0;
	}
	/* Slide to [from, to] (inclusive, from <= to); `magnitude(p)` computes a position not held yet. */
	template<typename F> void slide(size_t from, size_t to, F magnitude)
	{
		if (from >= hi || from < lo) { // no overlap with what is held: start over at `from`
			lo = hi = from;
			head = tail = 0;
		} else {
			lo = from;
		}
		while (head != tail && q[head % q.size()] < from)
			head++;
		while (hi <= to) {
			const double v = magnitude(hi);
			mag[hi % mag.size()] = v;
			while (head != tail && at(q[(tail - 1) % q.size()]) < v)
				tail--;
			q[tail % q.size()] = hi;
			tail++;
			hi++;
		}
	}
	double at(size_t p) const { return mag[p % mag.size()]; }
	/* The leftmost position holding the maximum of [lo, hi). */
	size_t argmax() const { return q[head % q.size()]; }
	/* The refine for the screen position `i`: slide to [from, to] (which holds `i`) and return the
	 * position of the maximum magnitude, `i` itself on a tie, else the leftmost maximum. */
	template<typename F> size_t refine(size_t i, size_t from, size_t to, F magnitude)
	{
		slide(from, to, magnitude);
		const size_t best = argmax();
		return at(best) > at(i) ? best : i;
	}
};

/* issue 1381 -- the buffers one decoder reuses across scans, so a push allocates nothing once they
 * have grown: the carrier table over the decoder's window, the prefix sums, the refine window. */
struct CbScanWorkspace {
	double w = 0.0; // the carrier angle the table holds
	std::vector<double> cosv, sinv;
	std::vector<double> pc, ps, pe;
	CbRefineWindow window;

	void build_carrier(size_t n, uint32_t sample_rate, uint32_t carrier_hz)
	{
		w = cb_carrier_w(sample_rate, carrier_hz);
		cosv.resize(n);
		sinv.resize(n);
		for (size_t m = 0; m < n; m++) {
			double ph = (double)m * w;
			cosv[m] = std::cos(ph);
			sinv[m] = std::sin(ph);
		}
	}
};

/* One scan of the demod (mirror qpsk_marker::MarkerScan). `markers` are (start sample within the
 * scanned samples, index); `resume` is where a scan of the same audio extended by more samples must
 * start to report the same markers (see cb_scan_markers). */
struct CbMarkerScan {
	std::vector<std::pair<size_t, uint8_t>> markers;
	CbDecodeStats stats;
	size_t resume = 0;
};

/* Detect QPSK markers in `samples[0..n)`, screening positions from `start` on (mirror
 * qpsk_marker::scan_markers): absolute-phase prefix sums (cos/sin/energy), a normalized 2-symbol
 * preamble screen, a forward refine to the true onset, preamble derotation, per-symbol quadrant bits,
 * then the 0xF-preamble + zero-nibble + CRC-4 gate. `c` is cycles-per-symbol (1 at the rig).
 *
 * `resume` = the first position visited whose screen passed while its refine range was cut by the end
 * of `samples` (a longer window may refine it differently), else where the scan stopped. Every
 * position before it is final. The work is capped by the window whatever the audio: at most
 * n - signal_len + 1 positions, each one magnitude, one screen and O(1) amortized window upkeep,
 * plus one 10-symbol word for a position that passes the screen. `ws` (optional) supplies reusable
 * buffers and a carrier table; without it, or when its table does not cover `n` samples at this
 * carrier, the trig is computed inline -- the same values either way. */
inline void cb_scan_markers(const float *samples, size_t n, uint32_t sample_rate, uint32_t carrier_hz,
			    uint32_t c, double threshold, size_t start, CbScanWorkspace *ws, CbMarkerScan &out)
{
	typedef std::complex<double> cd;
	out.markers.clear();
	out.stats = CbDecodeStats();
	out.resume = start;

	double ar = (double)sample_rate;
	double f = (double)carrier_hz;
	double cc = (double)(c < 1 ? 1 : c);
	double sps = ar * cc / f; // samples per symbol (fractional)
	size_t sig_len = cb_signal_len(sample_rate, carrier_hz, (c < 1 ? 1 : c));
	if (sig_len == 0 || n < sig_len || sps < 1.0)
		return;

	CbScanWorkspace local;
	CbScanWorkspace &W = ws ? *ws : local;
	double w = cb_carrier_w(sample_rate, carrier_hz);
	const bool table = W.w == w && W.cosv.size() >= n && W.sinv.size() >= n;
	W.pc.resize(n + 1);
	W.ps.resize(n + 1);
	W.pe.resize(n + 1);
	std::vector<double> &pc = W.pc, &ps = W.ps, &pe = W.pe;
	pc[0] = ps[0] = pe[0] = 0.0;
	for (size_t m = 0; m < n; m++) {
		double x = (double)samples[m];
		/* #1153: a non-finite input sample would otherwise contaminate every prefix sum after
		 * it, silently killing decode for the REST of the window; treat it as silence instead.
		 * Mirrors the identical sanitize in qpsk_marker::scan_markers. */
		if (!std::isfinite(x))
			x = 0.0;
		double cs, sn;
		if (table) {
			cs = W.cosv[m];
			sn = W.sinv[m];
		} else {
			double ph = (double)m * w;
			cs = std::cos(ph);
			sn = std::sin(ph);
		}
		pc[m + 1] = pc[m] + x * cs;
		ps[m + 1] = ps[m] + x * sn;
		pe[m + 1] = pe[m] + x * x;
	}

	// Z over [a,b): (re = sum signal*cos, im = -sum signal*sin), a/b clamped to n.
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
	/* |preamble| as the Rust cmag computes it, sqrt(re^2 + im^2) (issue 1381: was std::abs, i.e.
	 * hypot, which can differ from the Rust in the last bit). */
	auto magnitude = [&](size_t base) -> double {
		cd p = preamble(base);
		return std::sqrt(p.real() * p.real() + p.imag() * p.imag());
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

	/* The screen crosses threshold on the RISING edge, up to ~one symbol before the true onset; the
	 * refine searches the whole preamble span (+ a few back) for the max preamble magnitude. */
	const size_t span = (size_t)std::ceil(2.0 * sps);
	const size_t last = n - sig_len; // the last position a marker can start at
	W.window.reset(span + 8);
	bool resume_set = false;
	size_t i = start;
	while (i + sig_len <= n) {
		const size_t lo = i >= 4 ? i - 4 : 0;
		W.window.slide(lo, i, magnitude);
		const double mag_i = W.window.at(i);
		if (mag_i / norm_at(i) >= threshold) {
			out.stats.preamble_screens_passed++;
			const size_t hi = i + span < last ? i + span : last;
			if (!resume_set && i + span > last) {
				out.resume = i;
				resume_set = true;
			}
			/* The refine: the max magnitude over [i-4, i+span] (candidates past `last` cannot
			 * hold a whole marker); i itself wins a tie, else the leftmost maximum. */
			const size_t base = W.window.refine(i, lo, hi, magnitude);
			cd refp = preamble(base) * cd(1.0, -1.0);
			uint32_t word = 0;
			for (uint32_t k = 0; k < CB_N_SYMBOLS; k++) {
				std::pair<size_t, size_t> ab = sym_win(base, k);
				cd zz = z(ab.first, ab.second);
				// complex divide with the same 1e-12 denominator guard as the Rust cdiv.
				double d = refp.real() * refp.real() + refp.imag() * refp.imag() + 1e-12;
				double re = (zz.real() * refp.real() + zz.imag() * refp.imag()) / d;
				double im = (zz.imag() * refp.real() - zz.real() * refp.imag()) / d;
				uint32_t sym = (uint32_t)(im > 0.0 ? 2 : 0) | (uint32_t)(re > 0.0 ? 1 : 0);
				word |= sym << (CB_N_PAYLOAD_BITS - 2 - 2 * k);
			}
			// #1153: mirror qpsk_marker's zero-nibble gate — the emitter always sends bits[15:12]==0;
			// checking it reclaims 4 bits of redundancy and cuts the false-decode flood ~16x.
			if (((word >> 16) & 0xF) == CB_PREAMBLE_NIBBLE && ((word >> 12) & 0xF) == 0 &&
			    cb_crc4_check(word, CB_N_PAYLOAD_BITS) == 0) {
				out.stats.crc_ok++;
				out.markers.push_back(std::make_pair(base, (uint8_t)((word >> 4) & 0xFF)));
				i = base + sig_len; // markers are far apart; skip past this one
				continue;
			}
			out.stats.crc_fail++;
		}
		i += 1;
	}
	if (!resume_set)
		out.resume = i;
}

/* Detect QPSK markers in mono f32 audio -> (audio_ts_s at signal start, index) per marker, PLUS
 * CbDecodeStats: the whole-buffer scan (mirror qpsk_marker::decode_markers_with_stats). */
inline std::pair<std::vector<std::pair<double, uint8_t>>, CbDecodeStats>
cb_decode_markers_with_stats(const std::vector<float> &samples, uint32_t sample_rate, uint32_t carrier_hz,
			     uint32_t c, double threshold)
{
	CbMarkerScan scan;
	cb_scan_markers(samples.data(), samples.size(), sample_rate, carrier_hz, c, threshold, 0, nullptr,
			scan);
	std::vector<std::pair<double, uint8_t>> out;
	out.reserve(scan.markers.size());
	const double ar = (double)sample_rate;
	for (size_t k = 0; k < scan.markers.size(); k++)
		out.push_back(std::make_pair((double)scan.markers[k].first / ar, scan.markers[k].second));
	return std::make_pair(out, scan.stats);
}

/* Thin wrapper over cb_decode_markers_with_stats() (identical decode, stats discarded) -- kept so
 * every existing caller (the self-test) is untouched by the #690 diagnostics addition. Mirrors
 * qpsk_marker::decode_markers. */
inline std::vector<std::pair<double, uint8_t>>
cb_decode_markers(const std::vector<float> &samples, uint32_t sample_rate, uint32_t carrier_hz,
                  uint32_t c, double threshold)
{
	return cb_decode_markers_with_stats(samples, sample_rate, carrier_hz, c, threshold).first;
}

/* Streaming QPSK marker detector (mirror av_sync_dock::StreamingMarkerDecoder): a rolling window of
 * the most recent raw mono samples, each marker reported ONCE by absolute stream-sample index
 * (dedup). issue 1381: a push() scans only the positions that are not final yet -- from `next_scan`,
 * the new positions plus the tail whose refine the previous window end cut -- so it reports the same
 * markers the old whole-window re-decode reported, at a fraction of the work. `stats` accumulates
 * CbDecodeStats across every push(): each screened position counts once, and a cut-tail position
 * again when it is re-screened (at most one refine span per push). */
struct StreamingMarkerDecoder {
	uint32_t sample_rate;
	uint32_t carrier_hz;
	uint32_t c;
	double threshold;
	std::vector<float> buf;
	size_t capacity;
	uint64_t origin;         // absolute index of buf[0]
	bool have_last;          // last_reported present?
	uint64_t last_reported;  // absolute index of the last reported marker start
	uint64_t min_gap;
	CbDecodeStats stats;
	uint64_t next_scan;      // absolute index of the first position the next push() screens
	CbScanWorkspace ws;
	CbMarkerScan scan;

	StreamingMarkerDecoder(uint32_t sr, uint32_t f, uint32_t cc, double thr, size_t cap, uint64_t gap)
		: sample_rate(sr), carrier_hz(f), c(cc), threshold(thr), capacity(cap < 1 ? 1 : cap),
		  origin(0), have_last(false), last_reported(0), min_gap(gap < 1 ? 1 : gap), next_scan(0)
	{
		buf.reserve(capacity);
		ws.build_carrier(capacity, sr, f);
	}

	// Append `len` mono samples; return (absolute_start_index, index) of each NEW marker.
	std::vector<std::pair<uint64_t, uint8_t>> push(const float *samples, size_t len)
	{
		buf.insert(buf.end(), samples, samples + len);
		if (buf.size() > capacity) {
			size_t drop = buf.size() - capacity;
			buf.erase(buf.begin(), buf.begin() + drop);
			origin += (uint64_t)drop;
		}
		const size_t start = next_scan > origin ? (size_t)(next_scan - origin) : 0;
		cb_scan_markers(buf.data(), buf.size(), sample_rate, carrier_hz, c, threshold, start, &ws, scan);
		stats.preamble_screens_passed += scan.stats.preamble_screens_passed;
		stats.crc_ok += scan.stats.crc_ok;
		stats.crc_fail += scan.stats.crc_fail;
		next_scan = origin + (uint64_t)scan.resume;
		std::vector<std::pair<uint64_t, uint8_t>> out;
		for (size_t k = 0; k < scan.markers.size(); k++) {
			uint64_t abs = origin + (uint64_t)scan.markers[k].first;
			bool is_new = !have_last || abs > last_reported + min_gap;
			if (is_new) {
				have_last = true;
				last_reported = abs;
				out.push_back(std::make_pair(abs, scan.markers[k].second));
			}
		}
		return out;
	}

	/* #1153 -- drop the rolling window + dedup anchor while PRESERVING origin continuity (the
	 * absolute-sample coordinate the caller's own pushed-sample count mirrors) and the cumulative
	 * `stats` (the live diag counters must stay monotonic across a pairing recovery). Part of the
	 * dead-pairing reset: the decoder re-acquires from a clean window without disturbing the
	 * caller's timestamp mapping; the next scan starts at the new window. Mirror of
	 * av_sync_dock::StreamingMarkerDecoder::reset_window. */
	void reset_window()
	{
		origin += (uint64_t)buf.size();
		buf.clear();
		have_last = false;
		last_reported = 0;
		next_scan = origin;
	}
};

} // namespace camerabox
