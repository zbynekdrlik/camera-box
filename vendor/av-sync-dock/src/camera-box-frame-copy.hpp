#pragma once

/*
 * camera-box A/V-sync dock — the bounded frame copies the video-output thread makes (issue 1367).
 *
 * `st_raw_video` runs on libobs's ONE video-output thread, so it only copies what the decoders read
 * and hands the copy to the decode worker (camera-box-decode-mailbox.hpp). These are those copies,
 * as pure functions over a plane view, so the g++ self-test
 * (`vendor/av-sync-dock/test/decode-mailbox-selftest.cpp`) proves them against a plain reference
 * read of the frame:
 *
 *   - `cb_copy_top_band`: the camera-box top band, full width, rows 0..rows (a row memcpy for
 *     8-bit planar luma, which gives the same bytes as the per-pixel read);
 *   - `cb_copy_step_grid`: norihiro's whole-frame QR grid, every step-th pixel of every step-th
 *     row starting at step/2;
 *   - `cb_marker_patch_rect` / `cb_copy_patch`: the bounding box of one marker circle and its copy;
 *   - `cb_marker_circle_row`: the x-span of that circle on one row — the SAME span the marker
 *     search sums, so every pixel it reads lies inside the copied box.
 *
 * Dependency-free (STL + <cstdint>/<cstring>).
 */

#include <cstddef>
#include <cstdint>
#include <cstring>

namespace camerabox {

typedef uint8_t (*CbIntensityFn)(const uint8_t *data);

/* One video plane as OBS hands it to a raw output: `data` + `linesize`, `pixelsize` bytes per
 * pixel, the intensity byte at `pixeloffset` — or `intensity` to extract it (10-bit formats). */
struct CbPlaneView {
	const uint8_t *data;
	uint32_t linesize;
	uint32_t pixelsize;
	uint32_t pixeloffset;
	CbIntensityFn intensity;
};

inline uint8_t cb_plane_sample(const CbPlaneView &v, uint32_t x, uint32_t y)
{
	const uint8_t *p = v.data + (size_t)v.linesize * y + v.pixeloffset + (size_t)v.pixelsize * x;
	return v.intensity ? v.intensity(p) : *p;
}

/* Rows 0..rows, columns 0..width into `dst` (width x rows, row-major). */
inline void cb_copy_top_band(const CbPlaneView &v, uint32_t width, uint32_t rows, uint8_t *dst)
{
	for (uint32_t y = 0; y < rows; y++) {
		const uint8_t *line = v.data + (size_t)v.linesize * y + v.pixeloffset;
		uint8_t *dstrow = dst + (size_t)y * width;
		if (!v.intensity && v.pixelsize == 1) {
			memcpy(dstrow, line, width);
		} else if (!v.intensity) {
			const uint8_t *d = line;
			for (uint32_t x = 0; x < width; x++) {
				dstrow[x] = *d;
				d += v.pixelsize;
			}
		} else {
			const uint8_t *d = line;
			for (uint32_t x = 0; x < width; x++) {
				dstrow[x] = v.intensity(d);
				d += v.pixelsize;
			}
		}
	}
}

/* norihiro's grid: sample (step/2 + gx*step, step/2 + gy*step) for gx < grid_w, gy < grid_h into
 * `dst` (grid_w x grid_h). The caller sizes the grid so every sample lies inside the frame. */
inline void cb_copy_step_grid(const CbPlaneView &v, uint32_t step, uint32_t grid_w, uint32_t grid_h, uint8_t *dst)
{
	const size_t stride = (size_t)v.pixelsize * step;
	const uint8_t *linedata = v.data + (size_t)v.linesize * (step / 2);
	uint8_t *ptr = dst;
	for (uint32_t y = 0; y < grid_h; y++) {
		const uint8_t *d = linedata + v.pixeloffset + (size_t)v.pixelsize * (step / 2);
		if (!v.intensity) {
			for (uint32_t x = 0; x < grid_w; x++) {
				*ptr++ = *d;
				d += stride;
			}
		} else {
			for (uint32_t x = 0; x < grid_w; x++) {
				*ptr++ = v.intensity(d);
				d += stride;
			}
		}
		linedata += (size_t)v.linesize * step;
	}
}

/* Integer square root, floor(sqrt(x)) — norihiro's sqrt_u32, bit by bit. */
inline uint32_t cb_isqrt_u32(uint32_t x)
{
	uint32_t r = 0;
	for (uint32_t b = 1u << 15; b; b >>= 1) {
		const uint32_t t = r | b;
		if (t * t <= x)
			r = t;
	}
	return r;
}

struct CbSpan {
	uint32_t x0;
	uint32_t x1; // exclusive; empty when x0 >= x1
};

/* The x-span of the marker circle (centre cx, cy, radius r) on row y, clamped to the frame width —
 * exactly the span norihiro's marker search sums. y must lie in [cy - r, cy + r). */
inline CbSpan cb_marker_circle_row(uint32_t cx, uint32_t cy, uint32_t r, uint32_t y, uint32_t frame_w)
{
	const uint32_t d = y > cy ? y - cy : cy - y;
	const uint32_t dx = cb_isqrt_u32(r * r - d * d);
	CbSpan s;
	s.x0 = cx > dx ? cx - dx : 0;
	s.x1 = cx + dx < frame_w ? cx + dx : frame_w;
	return s;
}

struct CbPatchRect {
	uint32_t x0 = 0, y0 = 0, w = 0, h = 0;
};

/* The bounding box of the marker circle clamped to the frame: rows [cy - r, cy + r), columns
 * [cx - r, cx + r). Every span cb_marker_circle_row returns for those rows lies inside it. Empty
 * (w = h = 0) for r == 0 or a box that falls outside the frame. */
inline CbPatchRect cb_marker_patch_rect(uint32_t cx, uint32_t cy, uint32_t r, uint32_t frame_w, uint32_t frame_h)
{
	CbPatchRect p;
	if (r == 0)
		return p;
	const uint32_t y0 = cy > r ? cy - r : 0;
	const uint32_t y1 = cy + r < frame_h ? cy + r : frame_h;
	const uint32_t x0 = cx > r ? cx - r : 0;
	const uint32_t x1 = cx + r < frame_w ? cx + r : frame_w;
	if (y1 <= y0 || x1 <= x0)
		return p;
	p.x0 = x0;
	p.y0 = y0;
	p.w = x1 - x0;
	p.h = y1 - y0;
	return p;
}

/* Copy the rect's intensity into `dst` (rect.w x rect.h, row-major). */
inline void cb_copy_patch(const CbPlaneView &v, const CbPatchRect &rect, uint8_t *dst)
{
	for (uint32_t y = 0; y < rect.h; y++) {
		for (uint32_t x = 0; x < rect.w; x++)
			*dst++ = cb_plane_sample(v, rect.x0 + x, rect.y0 + y);
	}
}

} // namespace camerabox
