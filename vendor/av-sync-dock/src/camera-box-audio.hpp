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
 * `src/av_sync_dock.rs`. Every function here is a byte-for-byte port of that Rust; the Rust Tier-0
 * tests remain the authoritative gate. Cross-checking lives in TWO places, by function age (#926
 * fix-up review finding 15 — this note replaces a stale claim that ALL of this header is
 * cross-checked by the committed `test/camera-box-selftest.cpp`, which is only true for the
 * original #398 functions): the #398-era decode/cluster/top-band/otsu functions ARE cross-checked
 * there; every NEWER addition (`CbLockAuditTracker` #634, `CbDockLockCorrector` #926) instead has
 * its OWN dedicated Rust-driven twin harness (`tests/av_sync_dock_audit_log.rs`,
 * `tests/av_sync_dock_lock_926.rs`) that compiles+runs a tiny real C++ program against this exact
 * header, off-rig, with no vendored-OBS build needed — check the function's own doc comment for
 * which harness actually exercises it before assuming the selftest covers it.
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

/* #690 -- diagnostic counters for one cb_decode_markers_with_stats() call (mirror of
 * qpsk_marker::DecodeStats). Pure counting, zero effect on the returned markers -- lets a live
 * session tell apart "the demod sees nothing" (preamble_screens_passed==0) from "sees candidates
 * but they're garbage" (crc_fail>0, crc_ok==0) from "decodes fine" (crc_ok>0, in which case a
 * still-empty live Audio Index points further downstream, at the ring lookup / cluster lock). */
struct CbDecodeStats {
	uint64_t preamble_screens_passed = 0;
	uint64_t crc_ok = 0;
	uint64_t crc_fail = 0;
};

/* Detect QPSK markers in mono f32 audio -> (audio_ts_s at signal start, index) per marker, PLUS
 * CbDecodeStats. A byte-for-byte port of `crate::qpsk_marker::decode_markers_with_stats`:
 * absolute-phase prefix sums (cos/sin/energy), a normalized 2-symbol preamble screen, a forward
 * refine to the true onset, preamble derotation, per-symbol quadrant bits, then a 0xF-preamble +
 * CRC-4 gate. `c` is cycles-per-symbol (1 at the rig). Keep IN SYNC with the Rust; the self-test
 * cross-checks it. */
inline std::pair<std::vector<std::pair<double, uint8_t>>, CbDecodeStats>
cb_decode_markers_with_stats(const std::vector<float> &samples, uint32_t sample_rate, uint32_t carrier_hz,
                             uint32_t c, double threshold)
{
	typedef std::complex<double> cd;
	std::vector<std::pair<double, uint8_t>> out;
	CbDecodeStats stats;

	double ar = (double)sample_rate;
	double f = (double)carrier_hz;
	double cc = (double)(c < 1 ? 1 : c);
	double sps = ar * cc / f; // samples per symbol (fractional)
	size_t sig_len = cb_signal_len(sample_rate, carrier_hz, (c < 1 ? 1 : c));
	size_t n = samples.size();
	if (sig_len == 0 || n < sig_len || sps < 1.0)
		return std::make_pair(out, stats);

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
			stats.preamble_screens_passed++;
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
				stats.crc_ok++;
				out.push_back(std::make_pair((double)base / ar, (uint8_t)((word >> 4) & 0xFF)));
				i = base + sig_len; // markers are far apart; skip past this one
				continue;
			}
			stats.crc_fail++;
		}
		i += 1;
	}
	return std::make_pair(out, stats);
}

/* Thin wrapper over cb_decode_markers_with_stats() (identical decode, stats discarded) -- kept so
 * every existing caller (StreamingMarkerDecoder below, the self-test) is untouched by the #690
 * diagnostics addition. Mirrors qpsk_marker::decode_markers. */
inline std::vector<std::pair<double, uint8_t>>
cb_decode_markers(const std::vector<float> &samples, uint32_t sample_rate, uint32_t carrier_hz,
                  uint32_t c, double threshold)
{
	return cb_decode_markers_with_stats(samples, sample_rate, carrier_hz, c, threshold).first;
}

/* Streaming QPSK marker detector (mirror av_sync_dock::StreamingMarkerDecoder): a rolling window of
 * the most recent raw mono samples, re-decoded each push(), each marker reported ONCE by absolute
 * stream-sample index (dedup). `stats` accumulates CbDecodeStats across every push() -- see the
 * Rust field doc (av_sync_dock::StreamingMarkerDecoder::stats) for the deliberate over-count
 * caveat (each push() re-decodes the whole rolling window, so a real onset is re-screened/
 * re-counted on every push() until it ages out of `capacity` -- fine for "zero vs nonzero" and
 * rough relative magnitude, not an exact per-marker count). */
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
		std::pair<std::vector<std::pair<double, uint8_t>>, CbDecodeStats> decoded =
			cb_decode_markers_with_stats(buf, sample_rate, carrier_hz, c, threshold);
		std::vector<std::pair<double, uint8_t>> &found = decoded.first;
		stats.preamble_screens_passed += decoded.second.preamble_screens_passed;
		stats.crc_ok += decoded.second.crc_ok;
		stats.crc_fail += decoded.second.crc_fail;
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

	/* #926 fix-up (review finding 1/7) -- mirror of av_sync_dock::RollingOffsetCluster::rebase():
	 * shift every RETAINED sample's offset by -delta_ms the instant a correction of delta_ms
	 * (new_delay - current_delay) actually lands, so the window reflects the post-correction
	 * state immediately instead of lagging for up to window_ns. See the Rust method's own doc
	 * comment for the full closed-form justification. */
	void rebase(double delta_ms)
	{
		for (std::deque<std::pair<uint64_t, double>>::iterator it = samples.begin(); it != samples.end();
		     ++it)
			it->second -= delta_ms;
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

/* small named clamp helpers (#926 fix-up review finding 14 -- replaces a hand-rolled nested-ternary
 * `clampedw` expression that used to live inline in CbDockLockCorrector::decide()). cb_clamp_f64
 * treats a NaN input as "below lo" (NaN comparisons are always false, so `v < lo`/`v > hi` both
 * evaluate false for a NaN `v` -- falling through to `return v` would silently propagate the NaN;
 * the explicit `v == v` guard, false only for NaN, catches that and returns the floor instead). */
inline int64_t cb_clamp_i64(int64_t v, int64_t lo, int64_t hi)
{
	if (v < lo)
		return lo;
	if (v > hi)
		return hi;
	return v;
}

inline double cb_clamp_f64(double v, double lo, double hi)
{
	if (!(v == v))
		return lo; // NaN guard
	if (v < lo)
		return lo;
	if (v > hi)
		return hi;
	return v;
}

/* ---- #926 auto-correction: hold genlock_latency_ms_src so audio is NEVER early (pure, no I/O) ----
 *
 * Mirror of src/av_sync_dock.rs::DockLockCorrector -- see that module's doc comment for the full
 * closed-form proof. Summary: given the dock's own displayed offset (`ts_ms = audio_ts - video_ts`,
 * the SAME sign as `CbAvOffset::offset_ms` / the Latency label) and a noise-scaled safety MARGIN
 * (`mad_ms.clamp(CB_DOCK_LOCK_MIN_MARGIN_MS, CB_CLUSTER_MAX_MAD_MS)` -- #926 fix-up review finding
 * 3: targeting a bare `[0, 1)` claims sub-millisecond precision the ~25ms cluster noise floor
 * cannot back up), issue #942 widens the HOLD BAND to `[margin, 2*margin)` -- as wide as the SAME
 * clamped dispersion the low edge already uses, instead of the fixed 1ms dead zone that
 * limit-cycled the live actuator against the cluster's own 10-25ms measurement noise (never
 * settling: 339-470 actuator writes/session on the live rig). An offset already inside the band is
 * a plain Hold. Otherwise, setting `new_delay = current_delay + round(ts_ms - mid)` (mid =
 * 1.5*margin, the band's own middle -- stepping toward the middle rather than the low edge means a
 * landed correction has headroom on both sides, so it can't immediately re-trip the opposite
 * direction on ordinary jitter the size of the band) changes `ts` by exactly `-round(ts_ms - mid)`,
 * so the resulting ts always lands in `[mid-0.5, mid+0.5] subseteq [margin, 2*margin]` -- margin or
 * more, NEVER negative ("audio early", the forbidden steady state per issue #926's own directive).
 * Only ever acts on a genuine TRUSTED measurement (the caller passes locked=true only when the
 * rolling cluster currently reports est.ok -- #926 fix-up review finding 2: EVERY trusted
 * measurement, not only a CbLockAuditTracker Locked/Updated classifier transition, whose Updated
 * needs a >5ms move of the window-smoothed median and stalls convergence once the window lags a
 * landed correction); locked=false (no test signal: real event, no QR, no marker) always Holds --
 * freezes the actuator, implementing requirement 5 (measure-only, permanent lock) with no separate
 * timeout. */
static const int32_t CB_DOCK_LOCK_MAX_STEP_MS = 5;
static const double CB_DOCK_LOCK_MIN_REAPPLY_S = 30.0;
static const int32_t CB_DOCK_LOCK_LATENCY_MIN_MS = 3;
static const int32_t CB_DOCK_LOCK_LATENCY_MAX_MS = 2000;
/* #926 fix-up review finding 3 -- mirror of av_sync_dock::DOCK_LOCK_MIN_MARGIN_MS. */
static const double CB_DOCK_LOCK_MIN_MARGIN_MS = 1.0;

struct CbDockLockAction {
	bool apply = false; // false == Hold (do not touch the actuator)
	int32_t new_delay_ms = 0; // meaningful only when apply == true
};

class CbDockLockCorrector {
public:
	CbDockLockCorrector(int32_t max_step_ms = CB_DOCK_LOCK_MAX_STEP_MS,
	                    double min_reapply_s = CB_DOCK_LOCK_MIN_REAPPLY_S)
		: max_step_ms_(max_step_ms), min_delay_ms_(CB_DOCK_LOCK_LATENCY_MIN_MS),
		  max_delay_ms_(CB_DOCK_LOCK_LATENCY_MAX_MS), min_reapply_s_(min_reapply_s),
		  have_last_applied_(false), last_applied_ns_(0)
	{
	}

	/* locked: true only when the caller's rolling cluster currently reports a trusted (est.ok)
	 * measurement -- pass true on EVERY such measurement (#926 fix-up finding 2), never only on a
	 * lock-audit classifier transition. offset_ms: the locked cluster offset in dock convention
	 * (audio_ts - video_ts), meaningful only when locked. mad_ms: the SAME cluster's median
	 * absolute deviation (ms), used to size the safety margin -- meaningful only when locked.
	 * current_delay_ms: the actuator's CURRENT genlock_latency_ms_src, read fresh by the caller
	 * (never cached); now_ns: the caller's own monotonic clock (e.g. the OBS pipeline timestamp)
	 * for the cooldown.
	 *
	 * #926 fix-up finding 5: a non-finite offset_ms (NaN/+-inf) always Holds rather than risking
	 * UB on the later float->int conversions. */
	CbDockLockAction decide(bool locked, double offset_ms, double mad_ms, int32_t current_delay_ms,
	                        uint64_t now_ns)
	{
		CbDockLockAction action;
		if (!locked)
			return action;

		if (!std::isfinite(offset_ms))
			return action;

		double margin = std::isfinite(mad_ms) ? cb_clamp_f64(mad_ms, CB_DOCK_LOCK_MIN_MARGIN_MS,
		                                                      CB_CLUSTER_MAX_MAD_MS)
		                                       : CB_DOCK_LOCK_MIN_MARGIN_MS;
		// #942 -- the hold BAND scales with the cluster's own measured noise instead of a fixed 1ms
		// dead zone: [band_lo, band_hi) = [margin, 2*margin), i.e. as wide as the SAME clamped
		// dispersion the low edge already uses. Any offset already inside it is left alone -- no
		// actuator write at all.
		double band_lo = margin;
		double band_hi = margin * 2.0;
		if (offset_ms >= band_lo && offset_ms < band_hi)
			return action; // already inside the noise-scaled hold band

		if (have_last_applied_) {
			uint64_t elapsed_ns = now_ns > last_applied_ns_ ? now_ns - last_applied_ns_ : 0;
			double elapsed_s = (double)elapsed_ns / 1000000000.0;
			if (elapsed_s < min_reapply_s_)
				return action; // cooldown -- let the last correction take effect first
		}

		// Step toward the band's MIDDLE, not its low edge (#942) -- a landed correction then has
		// headroom on both sides of the band instead of sitting right at its boundary. Clamp
		// BEFORE the later int64_t casts (finding 5): offset_ms is finite but could still be
		// astronomically large, which would otherwise risk UB on the cast / an overflowing add.
		double mid = margin * 1.5;
		double g = cb_clamp_f64(std::round(offset_ms - mid), -1000000.0, 1000000.0);

		int64_t raw = (int64_t)current_delay_ms + (int64_t)g;
		int64_t lo = (int64_t)current_delay_ms - (int64_t)max_step_ms_;
		int64_t hi = (int64_t)current_delay_ms + (int64_t)max_step_ms_;
		int64_t stepped = cb_clamp_i64(raw, lo, hi);
		int32_t clamped = (int32_t)cb_clamp_i64(stepped, (int64_t)min_delay_ms_, (int64_t)max_delay_ms_);
		if (clamped == current_delay_ms)
			return action;

		have_last_applied_ = true;
		last_applied_ns_ = now_ns;
		action.apply = true;
		action.new_delay_ms = clamped;
		return action;
	}

private:
	int32_t max_step_ms_;
	int32_t min_delay_ms_;
	int32_t max_delay_ms_;
	double min_reapply_s_;
	bool have_last_applied_;
	uint64_t last_applied_ns_;
};

} // namespace camerabox
