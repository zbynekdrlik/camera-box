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
#include <cstring>
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

	// camera-box #275: BULK row/run fills instead of per-pixel put_bgra. The old
	// nested per-pixel loops with a per-pixel bounds check made the burn render too
	// CPU-heavy to hold a 60fps render (strih genlock_burn measured 18.9ms > the
	// 16.6ms 60fps budget). Identical OUTPUT BYTES — white = FF FF FF FF, black =
	// 00 00 00 FF — so the rqrr recorded-file decoder reads it UNCHANGED (the
	// burn_payload_parity render→decode test is the behavioral proof). Clamp the
	// square to the frame ONCE (no per-pixel checks), white = one memset/row, black
	// = a tight 32-bit run-fill per module-run scanline.
	const int fw = (int)frame_w;
	const int fh = (int)frame_h;
	const int x0 = ox < 0 ? 0 : ox;
	const int y0 = oy < 0 ? 0 : oy;
	const int x1 = (ox + sq) > fw ? fw : (ox + sq);
	const int y1 = (oy + sq) > fh ? fh : (oy + sq);
	if (x1 <= x0 || y1 <= y0)
		return sq; // square fully off-frame — nothing to draw

	// Black BGRA pixel built portably from the byte order (endianness-independent):
	// B=0, G=0, R=0, A=255. memcpy into a uint32 so the run-fill is a single store.
	uint32_t black;
	{
		const uint8_t black_px[4] = {0, 0, 0, 255};
		std::memcpy(&black, black_px, 4);
	}

	// 1) White backing square (quiet zone + module background): one memset per row
	//    over the clamped x-span. BGRA white = 0xFF for all four bytes, so memset works.
	for (int yy = y0; yy < y1; ++yy) {
		uint8_t *row = buf + (size_t)yy * stride + (size_t)x0 * 4;
		std::memset(row, 0xFF, (size_t)(x1 - x0) * 4);
	}

	// 2) Black modules: per module-row, fill each contiguous run of set modules as a
	//    rectangle (tight 32-bit stores, no per-pixel bounds check). buf+y*stride+x*4
	//    is 4-byte aligned for any BGRA frame (stride and x*4 are multiples of 4).
	for (int my = 0; my < data_modules; ++my) {
		const int py0 = oy + (my + border) * mp;
		const int ry0 = py0 < y0 ? y0 : py0;
		const int ry1 = (py0 + mp) > y1 ? y1 : (py0 + mp);
		if (ry1 <= ry0)
			continue; // this module-row is fully off-frame
		int mx = 0;
		while (mx < data_modules) {
			if (!qr.getModule(mx, my)) {
				++mx;
				continue;
			}
			int run = 1;
			while (mx + run < data_modules && qr.getModule(mx + run, my))
				++run;
			int rx0 = ox + (mx + border) * mp;
			int rx1 = rx0 + run * mp;
			if (rx0 < x0)
				rx0 = x0;
			if (rx1 > x1)
				rx1 = x1;
			if (rx1 > rx0) {
				const int n = rx1 - rx0;
				for (int yy = ry0; yy < ry1; ++yy) {
					uint32_t *px = (uint32_t *)(buf + (size_t)yy * stride +
								    (size_t)rx0 * 4);
					for (int i = 0; i < n; ++i)
						px[i] = black;
				}
			}
			mx += run;
		}
	}
	return sq;
}

} // namespace burn_qr
