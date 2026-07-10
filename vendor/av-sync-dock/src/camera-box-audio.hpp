#pragma once

/*
 * camera-box LIVE audio-marker decode for the A/V-sync dock (#398).
 *
 * WHY this exists: norihiro's own `st_raw_audio_*` demod does NOT decode camera-box's marker at the
 * rig's audio params. It computes a half-symbol resolution as `c1 = c/2`, which is 0 at our
 * c = auto_c(q=2, f=442, vr=60/1) = 1 (60 fps), collapsing the preamble finder so NO marker is ever
 * detected; and its bit extraction reads only 6 symbols and cannot recover the full 8-bit index. So
 * the deployed dock shows an empty Audio Index and empty Latency. This header instead MIRRORS
 * camera-box's OWN QPSK demod (`crate::qpsk_marker::decode_markers`, round-trip tested for all 256
 * indices AT c=1) plus the streaming wrapper + rolling densest-cluster estimator from
 * `src/av_sync_dock.rs`. Every function here is a byte-for-byte port of that Rust; the committed
 * `test/camera-box-selftest.cpp` cross-checks this port against the Rust results, and the Rust
 * Tier-0 tests are the authoritative gate.
 *
 * Dependency-free (STL + <cmath> only) so it compiles standalone (C++11) and the self-test runs
 * without libobs/quirc/Qt. Keep in sync with `src/av_sync_dock.rs` and `src/qpsk_marker.rs`.
 */

#include <cstdint>
#include <cmath>
#include <complex>
#include <vector>
#include <deque>
#include <utility>
#include <algorithm>

namespace camerabox {

/* ---- constants (mirror src/qpsk_marker.rs + src/av_sync_dock.rs) ---- */
static const double CB_PI = 3.14159265358979323846;
static const uint32_t CB_N_SYMBOLS = 10;      // 20 payload bits / 2 bits per QPSK symbol
static const uint32_t CB_N_PAYLOAD_BITS = 20; // 4 preamble + 8 index + 4 zero + 4 CRC
static const uint32_t CB_PREAMBLE_NIBBLE = 0xF;

/* Fixed rig audio params (AudioParams::rig60()): 48 kHz, 442 Hz carrier, c = 1. */
static const uint32_t CB_AUDIO_SAMPLE_RATE = 48000u;
static const uint32_t CB_AUDIO_CARRIER_HZ = 442u;
static const uint32_t CB_AUDIO_C = 1u;

/* Dock tuning (mirror the DOCK_* consts in src/av_sync_dock.rs — themselves the proven offline
 * recording-verdict --av-sync defaults, widened for a live display). */
static const double CB_QPSK_THRESHOLD = 0.35;
static const double CB_CLUSTER_TOL_MS = 60.0;
static const size_t CB_CLUSTER_MIN_MATCHED = 8;
static const double CB_CLUSTER_MAX_MAD_MS = 25.0;
static const uint64_t CB_CLUSTER_WINDOW_NS = 180ull * 1000000000ull;

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

/* Detect QPSK markers in mono f32 audio -> (audio_ts_s at signal start, index) per marker. A
 * byte-for-byte port of `crate::qpsk_marker::decode_markers`: absolute-phase prefix sums (cos/sin/
 * energy), a normalized 2-symbol preamble screen, a forward refine to the true onset, preamble
 * derotation, per-symbol quadrant bits, then a 0xF-preamble + CRC-4 gate. `c` is cycles-per-symbol
 * (1 at the rig). Keep IN SYNC with the Rust; the self-test cross-checks it. */
inline std::vector<std::pair<double, uint8_t>>
cb_decode_markers(const std::vector<float> &samples, uint32_t sample_rate, uint32_t carrier_hz,
                  uint32_t c, double threshold)
{
	typedef std::complex<double> cd;
	std::vector<std::pair<double, uint8_t>> out;

	double ar = (double)sample_rate;
	double f = (double)carrier_hz;
	double cc = (double)(c < 1 ? 1 : c);
	double sps = ar * cc / f; // samples per symbol (fractional)
	size_t sig_len = cb_signal_len(sample_rate, carrier_hz, (c < 1 ? 1 : c));
	size_t n = samples.size();
	if (sig_len == 0 || n < sig_len || sps < 1.0)
		return out;

	double w = 2.0 * CB_PI * f / ar;
	std::vector<double> pc(n + 1, 0.0), ps(n + 1, 0.0), pe(n + 1, 0.0);
	for (size_t m = 0; m < n; m++) {
		double ph = (double)m * w;
		double x = (double)samples[m];
		pc[m + 1] = pc[m] + x * std::cos(ph);
		ps[m + 1] = ps[m] + x * std::sin(ph);
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
				// complex divide with the same 1e-12 denominator guard as the Rust cdiv.
				double d = refp.real() * refp.real() + refp.imag() * refp.imag() + 1e-12;
				double re = (zz.real() * refp.real() + zz.imag() * refp.imag()) / d;
				double im = (zz.imag() * refp.real() - zz.real() * refp.imag()) / d;
				uint32_t sym = (uint32_t)(im > 0.0 ? 2 : 0) | (uint32_t)(re > 0.0 ? 1 : 0);
				word |= sym << (CB_N_PAYLOAD_BITS - 2 - 2 * k);
			}
			if (((word >> 16) & 0xF) == CB_PREAMBLE_NIBBLE && cb_crc4_check(word, CB_N_PAYLOAD_BITS) == 0) {
				out.push_back(std::make_pair((double)base / ar, (uint8_t)((word >> 4) & 0xFF)));
				i = base + sig_len; // markers are far apart; skip past this one
				continue;
			}
		}
		i += 1;
	}
	return out;
}

/* Streaming QPSK marker detector (mirror av_sync_dock::StreamingMarkerDecoder): a rolling window of
 * the most recent raw mono samples, re-decoded each push(), each marker reported ONCE by absolute
 * stream-sample index (dedup). */
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

	StreamingMarkerDecoder(uint32_t sr, uint32_t f, uint32_t cc, double thr, size_t cap, uint64_t gap)
		: sample_rate(sr), carrier_hz(f), c(cc), threshold(thr), capacity(cap < 1 ? 1 : cap),
		  origin(0), have_last(false), last_reported(0), min_gap(gap < 1 ? 1 : gap)
	{
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
		std::vector<std::pair<uint64_t, uint8_t>> out;
		double sr = (double)sample_rate;
		std::vector<std::pair<double, uint8_t>> found =
			cb_decode_markers(buf, sample_rate, carrier_hz, c, threshold);
		for (size_t k = 0; k < found.size(); k++) {
			uint64_t abs = origin + (uint64_t)std::llround(found[k].first * sr);
			bool is_new = !have_last || abs > last_reported + min_gap;
			if (is_new) {
				have_last = true;
				last_reported = abs;
				out.push_back(std::make_pair(abs, found[k].second));
			}
		}
		return out;
	}
};

/* median of a vector<double> (mirror qpsk_marker::median): sort in place, midpoint (avg of two
 * middle for an even count). Empty -> 0. */
inline double cb_median(std::vector<double> &v)
{
	std::sort(v.begin(), v.end());
	size_t n = v.size();
	if (n == 0)
		return 0.0;
	if (n % 2 == 1)
		return v[n / 2];
	return (v[n / 2 - 1] + v[n / 2]) / 2.0;
}

/* Result of cluster_offset_ms. */
struct CbAvOffset {
	bool ok;
	double offset_ms;
	size_t matched;
	double mad_ms;
};

/* Densest-cluster offset (mirror qpsk_marker::cluster_offset_ms): the median of the densest window
 * of width 2*tol among the candidate offsets, + MAD + count. ok=false if fewer than min_matched fall
 * in that window. */
inline CbAvOffset cb_cluster_offset_ms(const std::vector<double> &candidates, size_t min_matched,
                                       double cluster_tol_ms)
{
	CbAvOffset r;
	r.ok = false;
	r.offset_ms = 0.0;
	r.matched = 0;
	r.mad_ms = 0.0;
	size_t need = min_matched < 1 ? 1 : min_matched;
	if (candidates.size() < need)
		return r;
	std::vector<double> s = candidates;
	std::sort(s.begin(), s.end());
	double w = 2.0 * cluster_tol_ms;
	size_t best_lo = 0, best_cnt = 0;
	size_t j = 0;
	for (size_t i = 0; i < s.size(); i++) {
		if (j < i)
			j = i;
		while (j + 1 < s.size() && s[j + 1] - s[i] <= w)
			j++;
		size_t cnt = j - i + 1;
		if (cnt > best_cnt) {
			best_cnt = cnt;
			best_lo = i;
		}
	}
	double lo = s[best_lo];
	double hi = lo + w;
	std::vector<double> keep;
	for (size_t i = 0; i < s.size(); i++)
		if (s[i] >= lo && s[i] <= hi)
			keep.push_back(s[i]);
	if (keep.size() < need)
		return r;
	std::vector<double> tmp = keep;
	double offset = cb_median(tmp);
	std::vector<double> dev;
	dev.reserve(keep.size());
	for (size_t i = 0; i < keep.size(); i++)
		dev.push_back(std::fabs(keep[i] - offset));
	double mad = cb_median(dev);
	r.ok = true;
	r.offset_ms = offset;
	r.matched = keep.size();
	r.mad_ms = mad;
	return r;
}

/* Rolling densest-cluster estimator (mirror av_sync_dock::RollingOffsetCluster): keeps the last
 * window_ns of (ts, offset) samples and returns the TRUSTED cluster only when it is both big enough
 * (min_matched) AND tight enough (max_mad_ms) — else ok=false ("still measuring"). */
struct RollingOffsetCluster {
	uint64_t window_ns;
	double tol_ms;
	size_t min_matched;
	double max_mad_ms;
	std::deque<std::pair<uint64_t, double>> samples;

	RollingOffsetCluster(uint64_t win, double tol, size_t minm, double maxmad)
		: window_ns(win), tol_ms(tol), min_matched(minm), max_mad_ms(maxmad)
	{
	}

	static RollingOffsetCluster dock()
	{
		return RollingOffsetCluster(CB_CLUSTER_WINDOW_NS, CB_CLUSTER_TOL_MS, CB_CLUSTER_MIN_MATCHED,
		                            CB_CLUSTER_MAX_MAD_MS);
	}

	CbAvOffset push(uint64_t sample_ts_ns, double offset_ms)
	{
		samples.push_back(std::make_pair(sample_ts_ns, offset_ms));
		while (!samples.empty()) {
			uint64_t ts = samples.front().first;
			uint64_t age = sample_ts_ns > ts ? sample_ts_ns - ts : 0;
			if (age > window_ns)
				samples.pop_front();
			else
				break;
		}
		std::vector<double> offsets;
		offsets.reserve(samples.size());
		for (std::deque<std::pair<uint64_t, double>>::iterator it = samples.begin(); it != samples.end();
		     ++it)
			offsets.push_back(it->second);
		CbAvOffset est = cb_cluster_offset_ms(offsets, min_matched, tol_ms);
		if (est.ok && est.matched >= min_matched && est.mad_ms <= max_mad_ms)
			return est;
		CbAvOffset none;
		none.ok = false;
		none.offset_ms = 0.0;
		none.matched = 0;
		none.mad_ms = 0.0;
		return none;
	}
};

/* ---- #634 audit-logging: lock-state transition classification (pure, no I/O) ----
 *
 * The dock silently applies whatever `RollingOffsetCluster::push()` returns: a new
 * `sync_found`/`audio_marker_found` every time `est.ok` is true, and nothing at all when it is
 * false. That leaves zero log trail for diagnosing a live desync after the fact (the ask behind
 * #634, following the #529 incident). `CbLockAuditTracker` wraps that same `CbAvOffset` stream
 * and decides WHAT (if anything) is worth a log line: a lock acquired, a lock lost, or the
 * locked offset moving enough to matter — never a re-log of an unchanged, already-locked value
 * (that would spam a line per marker, ~once every few seconds while locked, for no new
 * information). The dock-side glue (sync-test-output.cpp) just `push()`es this every estimate
 * and `blog()`s the returned event — kept deliberately trivial so it doesn't need the 150-min
 * windows-genlock.yml frontend build to verify; this class is unit-tested off-rig via the same
 * twin-harness pattern as tests/obs_titlebar_newlevel_parse.rs (see
 * tests/av_sync_dock_audit_log.rs). */
enum class CbLockEventKind {
	None,     // nothing changed this push — do not log
	Locked,   // transitioned from unlocked (or startup) to a trusted cluster estimate
	Updated,  // still locked, but the offset moved by more than the stable tolerance
	Unlocked, // transitioned from locked to untrusted (est.ok went false)
};

struct CbLockAuditEvent {
	CbLockEventKind kind = CbLockEventKind::None;
	double offset_ms = 0.0; // the (new, or last-known-before-unlock) offset
	size_t matched = 0;     // cluster size backing the value ("source": the densest cluster)
	double mad_ms = 0.0;    // cluster dispersion backing the value
};

class CbLockAuditTracker {
public:
	// stable_tol_ms: while already locked, only classify as Updated when the new offset differs
	// from the last-logged one by more than this many ms (default mirrors CB_CLUSTER_TOL_MS/12,
	// well under the cluster's own ~60ms grouping window so a genuine re-alignment is caught).
	explicit CbLockAuditTracker(double stable_tol_ms = 5.0)
		: locked_(false), last_offset_ms_(0.0), stable_tol_ms_(stable_tol_ms)
	{
	}

	CbLockAuditEvent push(const CbAvOffset &est)
	{
		CbLockAuditEvent ev;
		if (!est.ok) {
			if (locked_) {
				ev.kind = CbLockEventKind::Unlocked;
				ev.offset_ms = last_offset_ms_;
			}
			locked_ = false;
			return ev;
		}

		if (!locked_) {
			ev.kind = CbLockEventKind::Locked;
		} else if (std::fabs(est.offset_ms - last_offset_ms_) > stable_tol_ms_) {
			ev.kind = CbLockEventKind::Updated;
		}
		ev.offset_ms = est.offset_ms;
		ev.matched = est.matched;
		ev.mad_ms = est.mad_ms;
		locked_ = true;
		last_offset_ms_ = est.offset_ms;
		return ev;
	}

private:
	bool locked_;
	double last_offset_ms_;
	double stable_tol_ms_;
};

} // namespace camerabox
