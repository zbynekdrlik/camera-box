/* camera-box #1355: ONE per-second genlock frame grid.
 *
 * Every genlocked sender stamps its frames on a PER-SECOND grid: slot k of second S is
 * S + floor(k * 1 s / fps) -- the camera sender (camera-box src/ndi.rs floor_boundary_100ns)
 * and the OBS sender (DistroAV ndi-output.cpp genlock_floor_boundary_100ns) restart the slot
 * count every whole second. Until #1355 this receiver floored its ts-align release deadline
 * (obs-source.c genlock_phase_pin_deadline) and its render tick (obs-video.c
 * genlock_next_deadline) on the grid counted from 1970 instead -- (t / interval) * interval.
 * At 30 fps the interval is 33_333_333 ns and 30 of them are 999_999_990 ns, so that grid
 * loses 10 ns per second (0.864 ms per day) against the senders' grid: the deep 'NDI 2ME PGM'
 * FIFO depth walked 31 <-> 32 frames with the calendar date (the stamps sat ~2 ms past the
 * floored deadline on 24.9.2026, due under the 5 ms hysteresis but not for the GAP-RESYNC
 * check). Both call sites now use the helpers below, so stamps, deadlines and ticks coincide
 * on every box on every date.
 *
 * Nanosecond units; the multiply-then-divide order and the at-most-one-slot promotion (#1009)
 * are the sender's, so a sender stamp (100 ns units) is at most 99 ns before the receiver grid
 * point of its own slot and never after it. A non-integer rate (29.97) has no per-second grid
 * and keeps the pre-#1355 arithmetic; interval 0 (unknown video info) returns t unchanged.
 *
 * Pure: stdint only, no libobs types -- tests/genlock_relock_selection_parity.rs compiles this
 * header as-is and requires byte-identical results from the Tier-0 Rust authority
 * src/genlock_grid.rs over vectors spanning a whole day. Keep both in lock-step. */
#pragma once

#include <stdint.h>

#define GENLOCK_GRID_NS_PER_SECOND 1000000000ULL

/* The integer frame rate interval_ns belongs to, or 0 when it is not an integer rate.
 * Mirror of src/genlock_grid.rs integer_fps (None == 0). */
static inline uint64_t genlock_grid_integer_fps(uint64_t interval_ns)
{
	if (interval_ns == 0)
		return 0;
	const uint64_t fps = (GENLOCK_GRID_NS_PER_SECOND + interval_ns / 2) / interval_ns;
	if (fps == 0)
		return 0;
	const uint64_t prod = fps * interval_ns;
	const uint64_t diff = prod > GENLOCK_GRID_NS_PER_SECOND ? prod - GENLOCK_GRID_NS_PER_SECOND
								: GENLOCK_GRID_NS_PER_SECOND - prod;
	return diff < fps ? fps : 0;
}

/* The slot (0..fps-1) an offset into its second falls in, at or before the offset, with the
 * #1009 promotion for an offset exactly ON a boundary. fps > 0 is the caller's guard. */
static inline uint64_t genlock_grid_slot(uint64_t offset_ns, uint64_t fps)
{
	uint64_t slot = offset_ns * fps / GENLOCK_GRID_NS_PER_SECOND;
	if ((slot + 1) * GENLOCK_GRID_NS_PER_SECOND / fps <= offset_ns)
		slot++;
	return slot;
}

/* The grid point AT OR BEFORE t_ns. Mirror of src/genlock_grid.rs grid_floor_ns. */
static inline uint64_t genlock_grid_floor_ns(uint64_t t_ns, uint64_t interval_ns)
{
	if (interval_ns == 0)
		return t_ns;
	const uint64_t fps = genlock_grid_integer_fps(interval_ns);
	if (fps == 0)
		return (t_ns / interval_ns) * interval_ns;
	const uint64_t sec = (t_ns / GENLOCK_GRID_NS_PER_SECOND) * GENLOCK_GRID_NS_PER_SECOND;
	return sec + genlock_grid_slot(t_ns - sec, fps) * GENLOCK_GRID_NS_PER_SECOND / fps;
}

/* The grid point STRICTLY AFTER t_ns. For the last slot of a second (slot + 1 == fps) the
 * expression is exactly the next whole second, so the roll-over needs no branch.
 * Mirror of src/genlock_grid.rs grid_next_boundary_ns. */
static inline uint64_t genlock_grid_next_boundary_ns(uint64_t t_ns, uint64_t interval_ns)
{
	if (interval_ns == 0)
		return t_ns;
	const uint64_t fps = genlock_grid_integer_fps(interval_ns);
	if (fps == 0)
		return t_ns - (t_ns % interval_ns) + interval_ns;
	const uint64_t sec = (t_ns / GENLOCK_GRID_NS_PER_SECOND) * GENLOCK_GRID_NS_PER_SECOND;
	return sec + (genlock_grid_slot(t_ns - sec, fps) + 1) * GENLOCK_GRID_NS_PER_SECOND / fps;
}
