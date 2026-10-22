/*
 * camera-box issue 1439 -- the concealment of a short run of lost VBAN packets (ROZHODNUTE 6352253147
 * item (b)). RED stub: the sample math is not written yet (no fade, the previous packet as it is).
 */

#pragma once

#include <stdbool.h>
#include <stdint.h>

#define VBAN_CONCEAL_FADE_MS 1u
#define VBAN_CONCEAL_FADE_DIV 4u

static inline uint32_t vban_conceal_fade_frames(uint32_t frames, uint32_t rate)
{
	(void)frames;
	(void)rate;
	return 1u;
}

static inline double vban_conceal_cospi(double u)
{
	(void)u;
	return 1.0;
}

static inline double vban_conceal_weight(uint32_t i, uint32_t n)
{
	(void)i;
	(void)n;
	return 0.0;
}

static inline double vban_conceal_sample(double rep, double last, double next, bool toward_next, uint32_t i,
					 uint32_t frames, uint32_t fade)
{
	(void)last;
	(void)next;
	(void)toward_next;
	(void)i;
	(void)frames;
	(void)fade;
	return rep;
}
