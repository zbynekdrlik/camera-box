/*
 * camera-box issue 1439 -- the concealment of a short run of lost VBAN packets (ROZHODNUTE 6352253147
 * item (b), the run limit raised to 3 slots on 23.10.2026).
 *
 * The receive timeline (vban-receive-timeline.h) decides WHICH missing slot is concealed: one of a run
 * of at most VBAN_RECV_CONCEAL_MAX_SLOTS missing slots that sits between two received packets, decided
 * at the slot's deadline from what is stored then, never waiting for a later packet. This header is the
 * sample math of a concealed slot: the PREVIOUS packet's samples played again in the missing slot, with
 * two short raised-cosine crossfades so neither boundary steps:
 *
 *  - IN: over the first FADE frames the slot fades from the last sample handed to libobs on that
 *    channel (held) into the repeated samples. The previous output already went out and cannot change;
 *    holding its last value keeps the join continuous.
 *  - OUT (only when the next slot is the stored packet that ends the run, TOWARD_NEXT): over the last
 *    FADE frames the repeated samples fade toward that packet's first sample, so the packet starts where
 *    the concealment ends. A slot whose next slot is missing too ends on the repeat; the next concealed
 *    slot fades in from it.
 *
 * FADE is 1 ms of audio (VBAN_CONCEAL_FADE_MS), at most a quarter of the slot, at least one frame. The
 * weight of fade frame I of N is 0.5 - 0.5 cos(pi (I + 1) / (N + 1)): rising, strictly inside (0, 1),
 * and the largest step between two weights is under pi / (2 (N + 1)) -- so a concealed slot never
 * steps more than the signal's own step plus that slope times the gap a fade bridges.
 *
 * Timing is not touched: a concealed slot has exactly the slot's frames and the slot's stamp; only its
 * CONTENT differs from silence.
 *
 * Pure: no OBS dependency and no libm (the cosine is a Taylor series of cos on [0, pi/4] and of sin on
 * [pi/4, pi/2], cos x = sin(pi/2 - x), each to 8 terms: error under 1e-14, exactly 0 at pi/2), every
 * expression in double. The Tier-0 authority is src/vban_conceal.rs in camera-box;
 * tests/vban_conceal_parity_1439.rs compiles THIS header and requires identical results bit for bit.
 */

#pragma once

#include <stdbool.h>
#include <stdint.h>

/* Each crossfade lasts this much audio ... */
#define VBAN_CONCEAL_FADE_MS 1u
/* ... and at most this fraction of the slot. */
#define VBAN_CONCEAL_FADE_DIV 4u
#define VBAN_CONCEAL_PI 3.14159265358979323846

/* The crossfade length in frames for a slot of FRAMES frames at RATE: 1 ms of audio, at most a quarter of
 * the slot, at least one frame. */
static inline uint32_t vban_conceal_fade_frames(uint32_t frames, uint32_t rate)
{
	uint32_t n = rate / 1000u * VBAN_CONCEAL_FADE_MS;
	const uint32_t cap = frames / VBAN_CONCEAL_FADE_DIV;
	if (n > cap)
		n = cap;
	return n ? n : 1u;
}

/* cos(pi U) for U in [0, 1] (no libm): mirrored above 1/2 (cos(pi u) = -cos(pi (1 - u))), then a Taylor
 * series in Horner form of cos on [0, pi/4] and of sin(pi/2 - x) on (pi/4, pi/2], 8 terms each. */
static inline double vban_conceal_cospi(double u)
{
	const bool upper = u > 0.5;
	const double v = upper ? 1.0 - u : u;
	double c;
	if (v <= 0.25) {
		const double x = v * VBAN_CONCEAL_PI;
		const double x2 = x * x;
		c = 1.0 - x2 / 2.0 *
				  (1.0 - x2 / 12.0 *
						 (1.0 - x2 / 30.0 *
								(1.0 - x2 / 56.0 *
									       (1.0 - x2 / 90.0 *
											      (1.0 - x2 / 132.0 *
													     (1.0 - x2 / 182.0))))));
	} else {
		const double x = (0.5 - v) * VBAN_CONCEAL_PI;
		const double x2 = x * x;
		c = x * (1.0 - x2 / 6.0 *
				       (1.0 - x2 / 20.0 *
						      (1.0 - x2 / 42.0 *
								     (1.0 - x2 / 72.0 *
										    (1.0 - x2 / 110.0 *
											    (1.0 - x2 / 156.0 *
												   (1.0 - x2 / 210.0)))))));
	}
	return upper ? -c : c;
}

/* The raised-cosine weight of fade frame I (0 .. N - 1) of N: 0.5 - 0.5 cos(pi (I + 1) / (N + 1)), rising,
 * strictly inside (0, 1). */
static inline double vban_conceal_weight(uint32_t i, uint32_t n)
{
	return 0.5 - 0.5 * vban_conceal_cospi((double)(i + 1u) / (double)(n + 1u));
}

/* One concealed sample: frame I of a slot of FRAMES frames, REP the previous packet's sample at I on this
 * channel. The first FADE frames cross-fade in from LAST (the last sample handed to libobs on this
 * channel); with TOWARD_NEXT the last FADE frames cross-fade out to NEXT (the first sample of the packet
 * that follows the slot). */
static inline double vban_conceal_sample(double rep, double last, double next, bool toward_next, uint32_t i,
					 uint32_t frames, uint32_t fade)
{
	double x = rep;
	if (i < fade)
		x = last + vban_conceal_weight(i, fade) * (x - last);
	if (toward_next && frames >= fade && i >= frames - fade)
		x = x + vban_conceal_weight(i - (frames - fade), fade) * (next - x);
	return x;
}
