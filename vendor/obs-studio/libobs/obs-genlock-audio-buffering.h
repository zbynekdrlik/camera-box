/* camera-box issue 1367: the genlock audio-buffering FLOOR. Every OBS launch starts with the SAME
 * global audio buffering, so the stream `mbc` ASRC level (and the audio position against the video)
 * no longer depends on a startup race.
 *
 * Stock libobs grows the mix-window buffering DYNAMICALLY, only when a source is behind the window
 * at startup (obs-audio.c add_audio_buffering). The stream box drew both outcomes on 27.9.2026:
 * 85 ms (`mbc` held its #1355 absolute target all session) and 0 ms (`mbc` sat 91 ms below it,
 * the #1355 fallback re-latched at a random depth, the release A/V gate read -28 ms). A
 * direct-timestamp source's natural depth is buffering + base (~9 ms) + sync offset, and its
 * absolute target is 100 ms + sync offset, so the offset cancels. The level servo can bridge the
 * gap only while the source timeline stays inside TS_SMOOTHING_THRESHOLD (70 ms, obs-source.c),
 * because past it the packet is re-placed at its raw stamp. The band held here is half of that,
 * with a lower AND an upper edge.
 *
 * - genlock_audio_buffering_make_plan() (obs_reset_audio2): the floor is GENLOCK_AUDIO_BUFFERING_FLOOR_MS
 *   rounded up to whole ticks the way OBS rounds its own maximum. Fixed buffering is never used, so
 *   the frontend low-latency toggle is overridden. The maximum stays the caller's, or 45 ticks.
 * - genlock_audio_buffering_action() (every mixer tick): below the floor raise to the floor (the
 *   first tick of a launch); above it OBS's dynamic increase for a late source stays active, since
 *   the resolume cg OBS legitimately grows 128-362 ms (ROZHODNUTE 5857354949: a floor, never a cap).
 * - genlock_audio_buffering_band_error_ns() / _band_ok(): the gap the servo must bridge and whether
 *   it fits; obs-audio.c names a dynamic increase that leaves the band.
 * - genlock_audio_buffering_frozen() (issue 1425, E2 slice 2, design 6216736061): the FREEZE. A
 *   buffering increase while an encoder runs makes that encoder's audio permanently later
 *   (obs-encoder.c counts audio pts in samples after the start), so once the mix has settled the
 *   dynamic increase is OFF: while an output with an audio encoder is active, or after
 *   GENLOCK_AUDIO_BUFFERING_SETTLE_NS with no growth and no member joining the mix. A mixed source that
 *   then falls behind the window is re-anchored instead (obs-genlock-mix-guard.h MIXED_LATE). The
 *   floor is never frozen, and nothing ever steps down mid-run: the buffering starts over at the
 *   next launch.
 *
 * Pure: stdint/stdbool only, no libobs types -- tests/genlock_audio_buffering_parity_1367.rs
 * compiles this header as-is and requires byte-identical results from the Tier-0 Rust authority
 * src/genlock_audio_buffering.rs. Keep both in lock-step. */
#pragma once

#include <stdbool.h>
#include <stdint.h>

/* The floor, ms, before tick rounding: 4 ticks = 85.33 ms at 48 kHz, 92.88 ms at 44.1 kHz (the
 * live-proven 01-33-25 configuration). Mirror of src/genlock_audio_buffering.rs FLOOR_MS. */
#define GENLOCK_AUDIO_BUFFERING_FLOOR_MS 85u
/* OBS's own maximum when the caller asks for none (obs_reset_audio2), and the maximum of an
 * overridden request. Mirror of DEFAULT_MAX_TICKS. */
#define GENLOCK_AUDIO_BUFFERING_DEFAULT_MAX_TICKS 45u
/* Half of obs-source.c TS_SMOOTHING_THRESHOLD (70 ms): how far the level target may sit from the
 * natural depth, ns. Mirror of LEVEL_REACH_NS. */
#define GENLOCK_AUDIO_LEVEL_REACH_NS 35000000LL
/* The measured `mbc` base (its offset-free depth at zero buffering: 8.4 / 8.9 ms), ns, for the log
 * note. Mirror of LEVEL_BASE_NOMINAL_NS. */
#define GENLOCK_AUDIO_LEVEL_BASE_NOMINAL_NS 9000000ULL

/* issue 1425: how long the mix must go without a growth and without a member joining before its
 * buffering freezes, ns. Spelled as a product (tests/audio_telemetry_800.rs pins the #800 60 s literal
 * once). Mirror of src/genlock_audio_buffering.rs SETTLE_NS. */
#define GENLOCK_AUDIO_BUFFERING_SETTLE_NS (30ULL * 1000000000ULL)

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

/* The floor, the dynamic maximum, and whether the request was overridden. req_max_ms == 0 means
 * OBS's default maximum. Mirror of plan. */
static inline struct genlock_audio_buffering_plan genlock_audio_buffering_make_plan(uint32_t req_max_ms, bool req_fixed,
									       uint32_t rate, uint32_t frames)
{
	struct genlock_audio_buffering_plan p;
	p.floor_ticks = genlock_audio_buffering_ticks(GENLOCK_AUDIO_BUFFERING_FLOOR_MS, rate, frames);
	const uint32_t req_ticks = req_max_ms != 0 ? genlock_audio_buffering_ticks(req_max_ms, rate, frames)
						   : GENLOCK_AUDIO_BUFFERING_DEFAULT_MAX_TICKS;
	p.overridden = req_fixed || req_ticks < p.floor_ticks;
	p.max_ticks = p.overridden ? GENLOCK_AUDIO_BUFFERING_DEFAULT_MAX_TICKS : req_ticks;
	if (p.max_ticks < p.floor_ticks)
		p.max_ticks = p.floor_ticks;
	p.fixed = false;
	return p;
}

/* The floor-then-dynamic rule for one mixer tick; source_behind = obs-audio.c `min_ts < ts.start`,
 * frozen = genlock_audio_buffering_frozen() for this tick (issue 1425: no dynamic increase once the mix
 * is settled; the floor is never frozen). Mirror of action. */
static inline int genlock_audio_buffering_action(int total_ticks, int floor_ticks, int max_ticks, bool source_behind,
						 bool frozen)
{
	if (total_ticks >= max_ticks)
		return GENLOCK_AUDIO_BUFFERING_ACTION_NONE;
	if (total_ticks < floor_ticks)
		return GENLOCK_AUDIO_BUFFERING_ACTION_FLOOR;
	return source_behind && !frozen ? GENLOCK_AUDIO_BUFFERING_ACTION_DYNAMIC : GENLOCK_AUDIO_BUFFERING_ACTION_NONE;
}

/* issue 1425: the time since the mix last grew or a member joined it, ns: the mixer ticks from
 * anchor_tick to cur_tick times the tick length, saturating at UINT64_MAX. 0 while there is no anchor
 * yet (anchor_tick 0: an audio reset zeroed it, the floor raise of the first tick sets it) or the
 * anchor is this tick. Mirror of ns_since. */
static inline uint64_t genlock_audio_buffering_ns_since(uint64_t anchor_tick, uint64_t cur_tick, uint64_t tick_ns)
{
	if (anchor_tick == 0 || cur_tick <= anchor_tick)
		return 0;
	const uint64_t ticks = cur_tick - anchor_tick;
	if (tick_ns != 0 && ticks > UINT64_MAX / tick_ns)
		return UINT64_MAX;
	return ticks * tick_ns;
}

/* issue 1425: whether the mix's buffering is frozen on this tick -- an output with an audio encoder is
 * active, or the mix has gone settle_ns without a growth and without a member joining. Mirror of
 * frozen. */
static inline bool genlock_audio_buffering_frozen(uint64_t ns_since_last_growth_or_join, bool encoder_active,
						  uint64_t settle_ns)
{
	return encoder_active || ns_since_last_growth_or_join >= settle_ns;
}

/* The gap the level servo must bridge, ns: target - (buffering + base). The sync offset moves both
 * sides and is not an input. Wrapping two's complement. Mirror of band_error_ns. */
static inline int64_t genlock_audio_buffering_band_error_ns(uint64_t buffering_ns, uint64_t base_ns, int64_t target_ns)
{
	return (int64_t)((uint64_t)target_ns - (buffering_ns + base_ns));
}

/* Whether the gap is inside +/-GENLOCK_AUDIO_LEVEL_REACH_NS. Mirror of band_ok. */
static inline bool genlock_audio_buffering_band_ok(int64_t error_ns)
{
	return error_ns >= -GENLOCK_AUDIO_LEVEL_REACH_NS && error_ns <= GENLOCK_AUDIO_LEVEL_REACH_NS;
}
