/******************************************************************************
	#111 — QR module-to-BGRA renderer for the render-time burn.

	Mirrors the Rust `blit_qr_bgra` (src/probe/qr.rs): a QR Code (EC level HIGH, so it
	survives the DistroAV NDI re-compression at the OBS outputs — the same level the
	camera-box probe uses) drawn as black modules on a WHITE quiet-zone background,
	scaled up to a target pixel size with integer module pixels, centered in a
	horizontal band of the BGRA frame. EC-High + a white quiet zone is what lets the
	existing `rqrr` recorded-file decoder (src/probe/recording.rs / decode_qr_luma_all)
	read the burned QR unchanged.

	Header-only; depends only on the vendored qrcodegen (Nayuki, MIT) and <cstdint>.
	The pure geometry (module_px, origin) is split into free functions so the burn
	filter and tests share one tested implementation, and so the parity test can render
	a frame and prove rqrr decodes it back to the burned payload.
******************************************************************************/

#pragma once

#include "qrcodegen/qrcodegen.hpp"

#include <cstdint>
#include <string>

namespace burn_qr {

// Largest integer pixels-per-module that fits a `modules`-wide QR (already including
// its quiet zone) into `target_px` pixels. >=1 so a too-small target still renders a
// (tiny) readable QR rather than collapsing to 0.
inline int module_px(int modules, int target_px)
{
	if (modules <= 0)
		return 1;
	int px = target_px / modules;
	return px < 1 ? 1 : px;
}

// Draw one BGRA pixel (B,G,R,A) at (x,y) into `buf` of stride `stride` bytes/row.
// Bounds-checked: an out-of-frame coordinate is a no-op (defensive — the caller clamps,
// but a malformed width/height must never write past the buffer).
inline void put_bgra(uint8_t *buf, uint32_t stride, uint32_t frame_w, uint32_t frame_h, uint32_t x,
		     uint32_t y, uint8_t b, uint8_t g, uint8_t r, uint8_t a)
{
	if (x >= frame_w || y >= frame_h)
		return;
	uint8_t *p = buf + (size_t)y * stride + (size_t)x * 4;
	p[0] = b;
	p[1] = g;
	p[2] = r;
	p[3] = a;
}

// Render `text` as a QR (EC level HIGH) into the BGRA `buf` (stride bytes/row, frame
// `frame_w` x `frame_h`), centered within the horizontal band [band_x, band_x+band_w),
// vertically centered on `band_cy`. The QR (incl. quiet zone) is first painted onto a
// WHITE backing square so the quiet zone is guaranteed (rqrr needs it), then black
// modules over it. `target_px` is the desired QR square size in pixels; the actual size
// is module_px * (modules incl. quiet zone) <= target_px. Returns the actual rendered
// square size in pixels (0 if it could not be placed).
inline int render(uint8_t *buf, uint32_t stride, uint32_t frame_w, uint32_t frame_h,
		  const std::string &text, uint32_t band_x, uint32_t band_w, uint32_t band_cy,
		  uint32_t target_px)
{
	const qrcodegen::QrCode qr = qrcodegen::QrCode::encodeText(text.c_str(), qrcodegen::QrCode::Ecc::HIGH);
	const int data_modules = qr.getSize();
	const int border = 4; // quiet zone, in modules (>=4 per the QR spec / rqrr needs)
	const int modules = data_modules + 2 * border;

	const int mp = module_px(modules, (int)target_px);
	const int sq = mp * modules; // actual square size in px
	if (sq <= 0)
		return 0;

	// Top-left origin: center the square in the band, vertically center on band_cy.
	int ox = (int)band_x + ((int)band_w - sq) / 2;
	int oy = (int)band_cy - sq / 2;
	if (ox < 0)
		ox = 0;
	if (oy < 0)
		oy = 0;

	// 1) White backing square (the quiet zone + module background).
	for (int yy = 0; yy < sq; ++yy)
		for (int xx = 0; xx < sq; ++xx)
			put_bgra(buf, stride, frame_w, frame_h, (uint32_t)(ox + xx),
				 (uint32_t)(oy + yy), 255, 255, 255, 255);

	// 2) Black modules (offset by the border quiet zone), each as an mp x mp block.
	for (int my = 0; my < data_modules; ++my) {
		for (int mx = 0; mx < data_modules; ++mx) {
			if (!qr.getModule(mx, my))
				continue;
			const int px0 = ox + (mx + border) * mp;
			const int py0 = oy + (my + border) * mp;
			for (int dy = 0; dy < mp; ++dy)
				for (int dx = 0; dx < mp; ++dx)
					put_bgra(buf, stride, frame_w, frame_h,
						 (uint32_t)(px0 + dx), (uint32_t)(py0 + dy), 0, 0,
						 0, 255);
		}
	}
	return sq;
}

} // namespace burn_qr
