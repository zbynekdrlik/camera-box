/* camera-box issue 1367: the genlock audio-buffering FLOOR, and since issue 1425 the FREEZE.
 *
 * Stock libobs grows the mix-window buffering DYNAMICALLY, only when a source is behind the window
 * (obs-audio.c add_audio_buffering). Issue 1367 raised every launch to an 85 ms floor so the stream
 * `mbc` could reach its #1355 absolute ASRC level target (100 ms + the sync offset) inside OBS's 70 ms
 * TS-smoothing reach. Issue 1425 retired that: since E2 slice 1 a directly stamped mixed source is
 * judged by its own stamp, so its A/V no longer depends on the buffering; since slice 2 the buffering
 * cannot grow while an audio encoder runs; and slice 3 (design 6220191978) deleted the absolute target
 * and set the floor to 0. A launch now starts at 0 like stock OBS, its own startup race grows it
 * (stock OBS drew 0, 64 or 85 ms per launch), and nothing moves it once an encoder runs after the
 * launch settle window (ROZHODNUTE 6221252305).
 *
 * - genlock_audio_buffering_make_plan() (obs_reset_audio2): the floor is GENLOCK_AUDIO_BUFFERING_FLOOR_MS
 *   rounded up to whole ticks the way OBS rounds its own maximum (0 ticks today). Fixed buffering is
 *   never used, so the frontend low-latency toggle is overridden. The maximum stays the caller's, or
 *   45 ticks.
 * - genlock_audio_buffering_action() (every mixer tick): below the floor raise to the floor (never,
 *   at 0); above it OBS's dynamic increase for a late source stays active, since the resolume cg OBS
 *   legitimately grows 128-362 ms (ROZHODNUTE 5857354949: a floor, never a cap).
 * - genlock_audio_buffering_frozen() (issue 1425, E2 slice 2, design 6216736061 + ROZHODNUTE
 *   6217557472): the FREEZE. A buffering increase while an encoder runs makes that encoder's audio
 *   permanently later (obs-encoder.c counts audio pts in samples after the start), so the dynamic
 *   increase is OFF while an output with an audio encoder is active -- and only then: with no
 *   encoder a growth moves no live output's A/V, so such a box (the resolume cg OBS) keeps stock
 *   growth. A mixed source that falls behind a frozen window is re-anchored instead
 *   (obs-genlock-mix-guard.h MIXED_LATE). Nothing ever steps down mid-run: the buffering starts over
 *   at the next launch. Since issue 1425 slice 3 (ROZHODNUTE 6221252305) the freeze also waits out
 *   the launch's startup race: GENLOCK_AUDIO_BUFFERING_LAUNCH_SETTLE_NS of mix time after the mix's
 *   first audio tick (genlock_audio_buffering_launch_settled), so an output started within seconds
 *   of a launch (--startstreaming, Companion, an operator) never mutes a member that is a few ms
 *   late; it gets that launch's growth instead, logged once.
 *
 * Pure: stdint/stdbool only, no libobs types -- tests/genlock_audio_buffering_parity_1367.rs
 * compiles this header as-is and requires byte-identical results from the Tier-0 Rust authority
 * src/genlock_audio_buffering.rs. Keep both in lock-step. */
#pragma once

#include <stdbool.h>
#include <stdint.h>

/* The floor, ms, before tick rounding: 0 since issue 1425 E2 slice 3 (it was 85 = 4 ticks for the
 * retired absolute ASRC level target). Mirror of src/genlock_audio_buffering.rs FLOOR_MS. */
#define GENLOCK_AUDIO_BUFFERING_FLOOR_MS 0u
/* OBS's own maximum when the caller asks for none (obs_reset_audio2), and the maximum of an
 * overridden request. Mirror of DEFAULT_MAX_TICKS. */
#define GENLOCK_AUDIO_BUFFERING_DEFAULT_MAX_TICKS 45u
/* issue 1425 slice 3 (ROZHODNUTE 6221252305): the launch settle window, ns of mix time after the mix's
 * first audio tick, during which a running encoder does not freeze the buffering (the startup race
 * draws it). Mirror of src/genlock_audio_buffering.rs LAUNCH_SETTLE_NS. */
#define GENLOCK_AUDIO_BUFFERING_LAUNCH_SETTLE_NS 30000000000ULL

/* genlock_audio_buffering_action() results. Mirror of src/genlock_audio_buffering.rs BufferingAction. */
#define GENLOCK_AUDIO_BUFFERING_ACTION_NONE 0
#define GENLOCK_AUDIO_BUFFERING_ACTION_FLOOR 1
#define GENLOCK_AUDIO_BUFFERING_ACTION_DYNAMIC 2

/* The reset decision. Mirror of src/genlock_audio_buffering.rs BufferingPlan. */
struct genlock_audio_buffering_plan {
	uint32_t floor_ticks;
	uint32_t max_ticks;
	/* Always false: the genlock build never uses OBS's fixed mode (the floor is the fixed part).
	 * obs-audio.c keeps the upstream fixed branch byte-identical for rebases; it is unreachable. */
	bool fixed;
	bool overridden;
};

/* Ticks for `ms` of buffering, rounded up: obs_reset_audio2's own uint32_t formula. 0 for
 * frames == 0. Mirror of ticks_for_ms. */
static inline uint32_t genlock_audio_buffering_ticks(uint32_t ms, uint32_t rate, uint32_t frames)
{
	if (frames == 0)
		return 0;
	uint32_t max_frames = ms * rate / 1000u;
	max_frames += frames - 1u;
	return max_frames / frames;
}

/* The duration of `ticks` mixer ticks, ns (truncated). 0 for rate == 0. Mirror of ticks_ns. */
static inline uint64_t genlock_audio_buffering_ticks_ns(uint32_t ticks, uint32_t frames, uint32_t rate)
{
	if (rate == 0)
		return 0;
	return (uint64_t)ticks * (uint64_t)frames * 1000000000ULL / (uint64_t)rate;
}

/* The plan for a given floor (ms): the floor, the dynamic maximum, and whether the request was
 * overridden. req_max_ms == 0 means OBS's default maximum. Mirror of plan_with_floor; the shipped
 * floor 0 never reaches the override or the max lift, this keeps both testable. */
static inline struct genlock_audio_buffering_plan
genlock_audio_buffering_make_plan_floor(uint32_t floor_ms, uint32_t req_max_ms, bool req_fixed, uint32_t rate,
					uint32_t frames)
{
	struct genlock_audio_buffering_plan p;
	p.floor_ticks = genlock_audio_buffering_ticks(floor_ms, rate, frames);
	const uint32_t req_ticks = req_max_ms != 0 ? genlock_audio_buffering_ticks(req_max_ms, rate, frames)
						   : GENLOCK_AUDIO_BUFFERING_DEFAULT_MAX_TICKS;
	p.overridden = req_fixed || req_ticks < p.floor_ticks;
	p.max_ticks = p.overridden ? GENLOCK_AUDIO_BUFFERING_DEFAULT_MAX_TICKS : req_ticks;
	if (p.max_ticks < p.floor_ticks)
		p.max_ticks = p.floor_ticks;
	p.fixed = false;
	return p;
}

/* The plan at the shipped floor (obs_reset_audio2). Mirror of plan. */
static inline struct genlock_audio_buffering_plan genlock_audio_buffering_make_plan(uint32_t req_max_ms, bool req_fixed,
									       uint32_t rate, uint32_t frames)
{
	return genlock_audio_buffering_make_plan_floor(GENLOCK_AUDIO_BUFFERING_FLOOR_MS, req_max_ms, req_fixed, rate,
						       frames);
}

/* The floor-then-dynamic rule for one mixer tick; source_behind = obs-audio.c `min_ts < ts.start`,
 * frozen = genlock_audio_buffering_frozen() for this tick (issue 1425: no dynamic increase while an
 * audio encoder runs; the floor is never frozen). Mirror of action. */
static inline int genlock_audio_buffering_action(int total_ticks, int floor_ticks, int max_ticks, bool source_behind,
						 bool frozen)
{
	if (total_ticks >= max_ticks)
		return GENLOCK_AUDIO_BUFFERING_ACTION_NONE;
	if (total_ticks < floor_ticks)
		return GENLOCK_AUDIO_BUFFERING_ACTION_FLOOR;
	return source_behind && !frozen ? GENLOCK_AUDIO_BUFFERING_ACTION_DYNAMIC : GENLOCK_AUDIO_BUFFERING_ACTION_NONE;
}

/* issue 1425 slice 3 (ROZHODNUTE 6221252305): whether the launch's startup race is over -- at least
 * GENLOCK_AUDIO_BUFFERING_LAUNCH_SETTLE_NS of mix time since the mix's first audio tick (first_tick_ns,
 * 0 = no tick yet). A mix time before the first tick (never on the monotonic audio timeline) reads
 * settled, so the freeze never lifts mid-run. Mirror of launch_settled. */
static inline bool genlock_audio_buffering_launch_settled(uint64_t first_tick_ns, uint64_t now_ns)
{
	if (first_tick_ns == 0)
		return false;
	return now_ns < first_tick_ns || now_ns - first_tick_ns >= GENLOCK_AUDIO_BUFFERING_LAUNCH_SETTLE_NS;
}

/* issue 1425 (ROZHODNUTE 6217557472 + 6221252305): whether the mix's buffering is frozen on this tick --
 * an output with an audio encoder is active AND the launch has settled. Nothing else freezes it: a box
 * with no audio encoder keeps stock growth. Mirror of frozen. */
static inline bool genlock_audio_buffering_frozen(bool encoder_active, bool settled)
{
	return encoder_active && settled;
}
