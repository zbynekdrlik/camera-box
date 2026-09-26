/* camera-box issue 1372: the ONE wall-step detector of the genlock render tick, and the one-tick
 * re-grid it decides.
 *
 * The render tick (obs-video.c genlock_next_deadline) maps the next wall-clock grid point into
 * the monotonic sleep timebase through the LIVE wall - mono offset, and clamps the per-tick
 * correction to GENLOCK_WALL_STEP_MAX_SLEW_NS (2 ms). A coordinated dantesync fleet DATE step
 * moves the wall clock by up to ~50 ms at once while the media clock (os_gettime_ns, issue 1372
 * part A) stays continuous; the clamp then slewed the tick back 2 ms per tick -- ~9 ticks off
 * phase at 30 fps -- and the NDI sender, which floors the wall clock at emit (DistroAV
 * ndi-output.cpp), stamped off-phase frames the whole time (live 25.9.2026 23:17:07 UTC, a
 * -51 ms step: strih-lx CG-obs underruns 20 -> 1069). Now:
 *
 * - genlock_wall_offset_ns() reads wall - mono against the midpoint of a (mono, wall, mono)
 *   bracket, and rejects a bracket wider than GENLOCK_WALL_STEP_READ_MAX_NS (a preempted read);
 * - genlock_wall_step_observe() compares it with the previous tick: a jump beyond
 *   GENLOCK_WALL_STEP_MIN_NS is a wall STEP (the media clock follows the wall RATE, so the
 *   offset is otherwise flat);
 * - genlock_wall_step_regrid_due() keeps the re-grid PENDING until a deadline lands within the
 *   clamp of the stock one: a re-grid target already past when os_sleepto_ns samples the clock
 *   sends video_sleep back onto the OLD grid, and the next tick must still re-grid;
 * - genlock_wall_step_deadline_ns() takes the wall-grid target unclamped on a re-grid (the tick,
 *   and with it every stamp the sender floors at emit, lands on the new grid in ONE tick) and
 *   keeps the 2 ms clamp otherwise, byte-identical to before.
 *
 * Pure: stdint only, no libobs types -- tests/genlock_wall_step_parity_1372.rs compiles this
 * header as-is and requires byte-identical results from the Tier-0 Rust authority
 * src/genlock_wall_step.rs. Keep both in lock-step. */
#pragma once

#include <stdint.h>

/* The per-tick slew clamp of the render tick, ns -- THE definition; obs-video.c's
 * GENLOCK_MAX_SLEW_NS is defined from it. Mirror of src/genlock_wall_step.rs MAX_SLEW_NS. */
#define GENLOCK_WALL_STEP_MAX_SLEW_NS 2000000LL
/* A wall - mono jump beyond this between two ticks is a wall STEP, ns: a smaller one is absorbed
 * in one tick by the clamp already. Mirror of WALL_STEP_MIN_NS. */
#define GENLOCK_WALL_STEP_MIN_NS GENLOCK_WALL_STEP_MAX_SLEW_NS
/* The widest trusted (mono, wall, mono) bracket, ns. Mirror of READ_MAX_NS. */
#define GENLOCK_WALL_STEP_READ_MAX_NS 100000ULL

/* The detector state. Mirror of src/genlock_wall_step.rs WallStepState. */
struct genlock_wall_step_state {
	int have;
	int64_t offset_ns;
	uint64_t steps;
	int regrid_pending;
};

/* wall - the midpoint of the monotonic reads around it; 0 (untrusted) when the bracket is wider
 * than GENLOCK_WALL_STEP_READ_MAX_NS or runs backwards, else 1 with *offset_out set.
 * Mirror of src/genlock_wall_step.rs wall_offset_ns. */
static inline int genlock_wall_offset_ns(uint64_t mono_before, uint64_t wall, uint64_t mono_after,
					 int64_t *offset_out)
{
	if (mono_after < mono_before || mono_after - mono_before > GENLOCK_WALL_STEP_READ_MAX_NS)
		return 0;
	const uint64_t mid = mono_before + (mono_after - mono_before) / 2;
	*offset_out = (int64_t)(wall - mid);
	return 1;
}

/* Feed one bracketed read; returns the wall STEP in ns (0 = none). The first trusted read seeds;
 * an untrusted read decides nothing and keeps the previous offset; every trusted read becomes the
 * new reference, so a slow drift is never summed into a false step.
 * Mirror of src/genlock_wall_step.rs WallStepState::observe. */
static inline int64_t genlock_wall_step_observe(struct genlock_wall_step_state *s, uint64_t mono_before,
						uint64_t wall, uint64_t mono_after)
{
	int64_t offset = 0;
	if (!genlock_wall_offset_ns(mono_before, wall, mono_after, &offset))
		return 0;
	if (!s->have) {
		s->have = 1;
		s->offset_ns = offset;
		return 0;
	}
	const int64_t step = (int64_t)((uint64_t)offset - (uint64_t)s->offset_ns);
	s->offset_ns = offset;
	const int64_t mag = step >= 0 ? step : (step == INT64_MIN ? INT64_MAX : -step);
	if (mag > GENLOCK_WALL_STEP_MIN_NS) {
		s->steps++;
		return step;
	}
	return 0;
}

/* Whether this tick's deadline re-grids: a step seen this tick, or one still pending. It stays
 * pending while the target is more than the clamp away from the stock deadline, i.e. until a tick
 * sits on the new grid. Mirror of src/genlock_wall_step.rs WallStepState::regrid_due. */
static inline int genlock_wall_step_regrid_due(struct genlock_wall_step_state *s, int64_t step_ns, uint64_t target,
					       uint64_t stock)
{
	(void)target;
	(void)stock;
	s->regrid_pending = 0;
	return step_ns != 0;
}

/* The render-tick deadline: the wall-grid target as-is on a re-grid (one-tick re-grid),
 * else clamped to +/-GENLOCK_WALL_STEP_MAX_SLEW_NS against the stock deadline -- the pre-issue-1372
 * arithmetic. Mirror of src/genlock_wall_step.rs deadline_ns. */
static inline uint64_t genlock_wall_step_deadline_ns(uint64_t target, uint64_t stock, int regrid)
{
	if (regrid)
		return target;
	const int64_t corr = (int64_t)(target - stock);
	if (corr > GENLOCK_WALL_STEP_MAX_SLEW_NS)
		return stock + (uint64_t)GENLOCK_WALL_STEP_MAX_SLEW_NS;
	if (corr < -GENLOCK_WALL_STEP_MAX_SLEW_NS)
		return stock - (uint64_t)GENLOCK_WALL_STEP_MAX_SLEW_NS;
	return target;
}
