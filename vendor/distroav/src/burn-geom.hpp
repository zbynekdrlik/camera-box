/******************************************************************************
	#111 — burn QR corner-placement geometry (the 4-corner no-overlap layout).

	The decided layout (one stream recording carries all four QRs, none overlapping):
	  - camera dual-QR (cam2 painter): TOP band — left + right.
	  - strih burn:  BOTTOM-LEFT corner.
	  - stream burn: BOTTOM-RIGHT corner.
	  - each burn ~300px (smaller than the camera QR), fully clear of the camera QRs
	    and of each other.

	Root cause this replaces: the burn was drawn CENTER-BOTTOM at ~700px, so it (a)
	overlapped the camera dual-QR (covered ~220px of each half) and (b) strih and stream
	drew their burn in the SAME spot → they overlapped each other → strih→stream 0 paired
	frames (a frame can't carry two readable QRs in the same pixels).

	`burn_qr::render` places a QR centered within a horizontal band [band_x, band_x+band_w)
	and vertically centered on band_cy. This header computes (band_x, band_w, band_cy) for a
	given corner + frame size + qr_px + margin, so the QR's square lands fully inside the
	corner with `margin` px of clearance from the frame edges. Splitting the geometry out of
	the filter keeps it unit-testable (tests/burn_payload_parity.rs renders all four and
	asserts the rectangles don't overlap), independent of OBS.

	Header-only, freestanding (no OBS, no chrono) — the parity test compiles it directly.
******************************************************************************/

#pragma once

#include <cstdint>

namespace burn_geom {

// #186 / #172: fraction of the canvas HEIGHT for the auto (canvas-relative) burn QR size.
// Both the QR size AND the edge margin are canvas-relative so the clearance vs the
// top-anchored camera dual-QR holds on EVERY canvas (the #172 gap: a fixed 40px margin ate
// the clearance on a SHORT canvas — 720p overlapped). The dual-QR is ~0.67*h top-anchored
// (700+24 on 1080, scaled by h/1080). With margin = BURN_MARGIN_FRACTION*h and burn =
// BURN_QR_HEIGHT_FRACTION*h, the bottom-anchored burn top sits at
//   h - 0.037*h - 0.28*h = 0.683*h  >=  the dual-QR bottom 0.6704*h  (a ~0.013*h gap, all h).
//   1080: 296px burn (≈ old 300), 4K: 605px (2× the old fixed 300 — crisp through the upscale).
inline constexpr double BURN_QR_HEIGHT_FRACTION = 0.28;

// #172: edge margin as a fraction of the canvas height (≈ the 40px-on-1080 production margin,
// scaled so the clearance is canvas-independent — a fixed 40px was too big a slice of a 720p
// frame and pushed the burn up into the dual-QR).
inline constexpr double BURN_MARGIN_FRACTION = 40.0 / 1080.0;

// Effective burn QR square px for a canvas of height `frame_h`: the absolute `configured`
// value when non-zero (the OBS_BURN_QR_PX override), else BURN_QR_HEIGHT_FRACTION * frame_h
// floored at 64 (a tiny canvas still gets a readable burn). Pure so the filter and the
// parity test share ONE tested size; corner_placement clamps further if a misconfig exceeds
// the frame. Canvas-relative so the burn is the same RELATIVE size on 1080 and 4K and never
// goes soft on the 4K stream upscale (#186 — the old fixed 300px was half-size on 4K).
inline uint32_t burn_qr_px_for_canvas(uint32_t configured, uint32_t frame_h)
{
	if (configured != 0)
		return configured;
	uint32_t px = (uint32_t)(BURN_QR_HEIGHT_FRACTION * (double)frame_h);
	return px < 64u ? 64u : px;
}

// #172: canvas-relative edge margin in px (floored at 8 so a tiny canvas still has a quiet
// border). Used for BOTH the burn placement and the no-overlap budget.
inline uint32_t burn_margin_for_canvas(uint32_t frame_h)
{
	uint32_t m = (uint32_t)(BURN_MARGIN_FRACTION * (double)frame_h);
	return m < 8u ? 8u : m;
}

// Which corner this node burns into. strih = bottom-left, stream = bottom-right (the
// decided per-node assignment); the env OBS_BURN_CORNER selects it (see resolve_corner).
enum class Corner { BottomLeft, BottomRight };

// Case-insensitive substring search (ASCII only, no <cctype> locale dependence).
inline bool ci_contains(const char *hay, const char *needle)
{
	if (!hay || !needle)
		return false;
	auto lc = [](char c) -> char { return (c >= 'A' && c <= 'Z') ? char(c + 32) : c; };
	for (const char *h = hay; *h; ++h) {
		const char *a = h;
		const char *b = needle;
		while (*a && *b && lc(*a) == lc(*b)) {
			++a;
			++b;
		}
		if (!*b)
			return true;
	}
	return false;
}

// Parse an OBS_BURN_CORNER value to a Corner, falling back to `dflt` when the string is
// null/empty/unrecognized. Matched by SUBSTRING so EVERY documented form works:
//   "right"/"br"/"bottom-right"/"bottom_right"/"r"  → BottomRight
//   "left"/"bl"/"bottom-left"/"l"                   → BottomLeft
// Split out (pure, no getenv) so it is unit-testable — the long-form parse was a real
// bug (the old code disambiguated on the 2nd char, which is 'o' for "bottom-…").
inline Corner corner_from_string(const char *s, Corner dflt)
{
	if (!s || !*s)
		return dflt;
	if (ci_contains(s, "right") || ci_contains(s, "br"))
		return Corner::BottomRight;
	if (ci_contains(s, "left") || ci_contains(s, "bl"))
		return Corner::BottomLeft;
	if (s[0] == 'r' || s[0] == 'R')
		return Corner::BottomRight;
	if (s[0] == 'l' || s[0] == 'L')
		return Corner::BottomLeft;
	return dflt;
}

// A QR placement: the band burn_qr::render draws into. The QR square (side `square_px`,
// == the actual rendered size) is centered in [band_x, band_x+band_w) and vertically
// centered on band_cy, so its bounding box is:
//   x in [band_x + (band_w - square_px)/2, ... + square_px)
//   y in [band_cy - square_px/2,           ... + square_px)
struct Placement {
	uint32_t band_x;
	uint32_t band_w;
	uint32_t band_cy;
	uint32_t square_px; // the QR square side actually used (== qr_px here; the band is
	                    // exactly one QR wide so the QR fills it, no horizontal slack)
};

// Compute the bottom-corner placement for a `qr_px`-sized burn QR with `margin` px of
// clearance from the frame edges. The band is exactly `qr_px` wide and sits hard against
// the chosen corner: left edge `margin` (bottom-left) or right edge `frame_w - margin`
// (bottom-right); the QR's vertical center is `frame_h - margin - qr_px/2` (bottom edge at
// `frame_h - margin`). Defensive clamps keep everything in-frame for any frame size.
inline Placement corner_placement(uint32_t frame_w, uint32_t frame_h, Corner corner,
				  uint32_t qr_px, uint32_t margin)
{
	// Clamp qr_px so a QR + 2*margin always fits the frame (a misconfigured huge qr_px
	// must never push the band off-frame). Leave at least 1px.
	uint32_t max_w = (frame_w > 2 * margin) ? (frame_w - 2 * margin) : 1;
	uint32_t max_h = (frame_h > 2 * margin) ? (frame_h - 2 * margin) : 1;
	uint32_t side = qr_px;
	if (side > max_w)
		side = max_w;
	if (side > max_h)
		side = max_h;
	if (side < 1)
		side = 1;

	Placement p{};
	p.square_px = side;
	p.band_w = side; // band exactly one QR wide → render centers it == flush in the band
	// band_x / band_cy with uint32 subtraction guarded against underflow on a degenerate
	// tiny frame (frame_w/frame_h < margin + side) — clamp to 0 rather than wrap to a huge
	// off-frame coordinate. Production is 1920×1080 so this never bites live; the clamp just
	// makes the "in-frame for any frame size" contract genuinely true.
	if (corner == Corner::BottomLeft) {
		p.band_x = margin;
	} else {
		p.band_x = (frame_w > margin + side) ? (frame_w - margin - side) : 0;
	}
	// Vertical center so the bottom edge sits at frame_h - margin.
	const uint32_t half = side / 2;
	p.band_cy = (frame_h > margin + half) ? (frame_h - margin - half) : half;
	return p;
}

} // namespace burn_geom
