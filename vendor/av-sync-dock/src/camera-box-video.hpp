#pragma once

/*
 * camera-box LIVE video-QR decode helpers for the A/V-sync dock (#398).
 *
 * WHY this exists: norihiro's `st_start` downscales the WHOLE frame by `qr_step` (÷8 at a 4K program
 * output) with NEAREST sampling, shrinking each ~700 px dual-QR half to ~87 px -> quirc misses ~98 %
 * of frames, so the video ring is almost never populated and no Latency can pair. This header gives
 * the dock a better look: decode only the TOP band (where the top-anchored dual-QR lives),
 * AREA-averaged (not nearest) to a scale that keeps each QR large, with an Otsu-binarized retry — the
 * same techniques `src/probe/qr.rs` proved on the real soft optical stream frames (#202/#363).
 *
 * The GEOMETRY (`cb_top_band_decode_plan`) and the Otsu threshold mirror `src/av_sync_dock.rs`
 * (Tier-0 tested); the box-downscale is a plain area filter. Dependency-free (STL + <cstdint>), so
 * the committed self-test cross-checks it standalone. The quirc driving stays in sync-test-output.cpp.
 */

#include <cstdint>
#include <cstddef>
#include <vector>

namespace camerabox {

/* Mirror TOP_BAND_* in src/av_sync_dock.rs. */
static const uint32_t CB_TOP_BAND_FRAC_NUM = 72;
static const uint32_t CB_TOP_BAND_FRAC_DEN = 100;
static const uint32_t CB_TOP_BAND_DECODE_CAP = 760;

struct CbTopBandPlan {
	uint32_t band_h; // rows of the top crop taken from the source frame
	uint32_t dst_w;  // downscaled width fed to quirc
	uint32_t dst_h;  // downscaled height fed to quirc
};

/* Mirror av_sync_dock::top_band_decode_plan: top 72 % crop, area-downscaled preserving aspect so the
 * long side <= CB_TOP_BAND_DECODE_CAP (never UPscaled), every dim clamped >= 1. */
inline CbTopBandPlan cb_top_band_decode_plan(uint32_t frame_w, uint32_t frame_h)
{
	CbTopBandPlan p;
	uint32_t fh = frame_h < 1 ? 1 : frame_h;
	uint32_t band = fh * CB_TOP_BAND_FRAC_NUM / CB_TOP_BAND_FRAC_DEN;
	if (band < 1)
		band = 1;
	if (band > fh)
		band = fh;
	p.band_h = band;
	uint32_t src_w = frame_w < 1 ? 1 : frame_w;
	uint32_t lng = src_w > band ? src_w : band;
	if (lng > CB_TOP_BAND_DECODE_CAP) {
		uint32_t dw = src_w * CB_TOP_BAND_DECODE_CAP / lng;
		uint32_t dh = band * CB_TOP_BAND_DECODE_CAP / lng;
		p.dst_w = dw < 1 ? 1 : dw;
		p.dst_h = dh < 1 ? 1 : dh;
	} else {
		p.dst_w = src_w;
		p.dst_h = band;
	}
	return p;
}

/* Otsu's global threshold (0..=255) with the plateau-midpoint refinement — a byte-for-byte port of
 * av_sync_dock::otsu_threshold (itself the proven src/probe/qr.rs threshold). Empty/flat -> 128. */
inline uint8_t cb_otsu_threshold(const uint64_t hist[256])
{
	uint64_t total = 0;
	for (int i = 0; i < 256; i++)
		total += hist[i];
	if (total == 0)
		return 128;
	double sum_all = 0.0;
	for (int i = 0; i < 256; i++)
		sum_all += (double)i * (double)hist[i];
	uint64_t w_bg = 0;
	double sum_bg = 0.0;
	double best_var = -1.0;
	int plateau_lo = 128, plateau_hi = 128;
	const double EPS = 2.220446049250313e-16; // f64::EPSILON
	for (int t = 0; t < 256; t++) {
		w_bg += hist[t];
		if (w_bg == 0)
			continue;
		uint64_t w_fg = total - w_bg;
		if (w_fg == 0)
			break;
		sum_bg += (double)t * (double)hist[t];
		double m_bg = sum_bg / (double)w_bg;
		double m_fg = (sum_all - sum_bg) / (double)w_fg;
		double between = (double)w_bg * (double)w_fg * (m_bg - m_fg) * (m_bg - m_fg);
		if (between > best_var + EPS) {
			best_var = between;
			plateau_lo = t;
			plateau_hi = t;
		} else if ((between - best_var < 0 ? best_var - between : between - best_var) <= EPS) {
			plateau_hi = t;
		}
	}
	return (uint8_t)((plateau_lo + plateau_hi) / 2);
}

/* Area-average (box) downscale a tight row-major luma buffer `src` (src_w x src_h) into `dst`
 * (dst_w x dst_h). Each destination pixel is the mean of its covering source block — far better than
 * norihiro's nearest ÷qr_step sampling for a soft QR. dst must hold dst_w*dst_h bytes. No-op-safe for
 * degenerate sizes. */
inline void cb_box_downscale_luma(const uint8_t *src, uint32_t src_w, uint32_t src_h, uint8_t *dst,
                                  uint32_t dst_w, uint32_t dst_h)
{
	if (src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0)
		return;
	for (uint32_t dy = 0; dy < dst_h; dy++) {
		uint32_t sy0 = (uint32_t)((uint64_t)dy * src_h / dst_h);
		uint32_t sy1 = (uint32_t)((uint64_t)(dy + 1) * src_h / dst_h);
		if (sy1 <= sy0)
			sy1 = sy0 + 1;
		if (sy1 > src_h)
			sy1 = src_h;
		for (uint32_t dx = 0; dx < dst_w; dx++) {
			uint32_t sx0 = (uint32_t)((uint64_t)dx * src_w / dst_w);
			uint32_t sx1 = (uint32_t)((uint64_t)(dx + 1) * src_w / dst_w);
			if (sx1 <= sx0)
				sx1 = sx0 + 1;
			if (sx1 > src_w)
				sx1 = src_w;
			uint32_t sum = 0, cnt = 0;
			for (uint32_t sy = sy0; sy < sy1; sy++) {
				const uint8_t *row = src + (size_t)sy * src_w;
				for (uint32_t sx = sx0; sx < sx1; sx++) {
					sum += row[sx];
					cnt++;
				}
			}
			dst[(size_t)dy * dst_w + dx] = cnt ? (uint8_t)(sum / cnt) : 0;
		}
	}
}

/* Otsu-binarize a luma buffer in place (>= threshold -> 255, else 0) — the hard black/white cut that
 * locks quirc's finder on a soft optical capture (the #363 recovery). */
inline void cb_binarize_otsu(uint8_t *buf, size_t len)
{
	uint64_t hist[256];
	for (int i = 0; i < 256; i++)
		hist[i] = 0;
	for (size_t i = 0; i < len; i++)
		hist[buf[i]]++;
	uint8_t t = cb_otsu_threshold(hist);
	for (size_t i = 0; i < len; i++)
		buf[i] = buf[i] >= t ? 255 : 0;
}

} // namespace camerabox
