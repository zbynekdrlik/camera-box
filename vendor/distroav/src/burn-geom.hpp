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

// Which corner this node burns into. strih = bottom-left, stream = bottom-right (the
// decided per-node assignment); the env OBS_BURN_CORNER selects it (see resolve_corner).
enum class Corner { BottomLeft, BottomRight };

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
	p.band_x = (corner == Corner::BottomLeft) ? margin : (frame_w - margin - side);
	// Vertical center so the bottom edge sits at frame_h - margin.
	p.band_cy = frame_h - margin - side / 2;
	return p;
}

} // namespace burn_geom
