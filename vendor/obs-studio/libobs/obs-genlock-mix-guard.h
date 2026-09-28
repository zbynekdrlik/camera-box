/* camera-box issue 1381 (design 5862336131): the mix buffering GUARD -- only a source that is MIXED can
 * move the mix window.
 *
 * Stock libobs walked every audio source in find_min_ts / mark_invalid_sources (obs-audio.c), so a
 * hidden input nobody hears grew the buffering of the WHOLE mix: 27.9.2026 on resolume the hidden
 * "NDI test", whose timestamps ran later and later, took the cg mix +85/+42/+106/+128/+106/+490 ms to
 * the 960 ms maximum, and every step was a hole in every output (audio_callback returns false while
 * the added ticks drain).
 *
 * A source is IN THE MIX on a mixer tick when an output mix's active tree reached it: audio_callback
 * marks the render order it has built from the mixes (genlock_mix_mark_members) before its catch-all
 * loop adds every other audio source (rendered for meters and monitoring, never mixed). A source that
 * is not mixed and runs behind the window, or one that JOINED the mix on this tick behind it (a cut to
 * it, or a program source joining after the scene collection loads), is re-anchored to the window
 * instead (genlock_mix_guard_reanchor, obs-audio.c). A source that stays mixed keeps upstream's
 * behaviour: the dynamic increase above the 1367 floor, ignore_audio at the maximum. That includes a
 * source whose audio STARTS only after it joined the mix (a media start on program).
 *
 * Known limit: the restart of an exhausted re-anchor moves a source only when its placement follows
 * timing_adjust (a stamp that is not direct, outside a genlock TIMECODE hold). A source that KEEPS its
 * stamp -- a direct one (within MAX_TS_VAR of the OBS clock: ASIO/WASAPI capture, media) or a genlock
 * TIMECODE hold (its placement term cancels timing_adjust, obs-source.c genlock_audio_place_term_ns)
 * -- and stays late after the entry re-anchor is placed late again; a tick or two later it is a late
 * MIXED source and upstream grows the mix. A hidden source of either kind never does.
 *
 * Pure: stdint/stdbool only, no libobs types. The stateful half (the mark, the re-anchor, the log line)
 * stays in obs-audio.c. tests/genlock_audio_mix_guard_1381.rs compiles both into a lift of the shipped
 * audio_callback path. */
#pragma once

#include <stdbool.h>
#include <stdint.h>

/* The guard's reasons for one rendered source. */
#define GENLOCK_MIX_GUARD_NONE 0
#define GENLOCK_MIX_GUARD_NOT_MIXED 1
#define GENLOCK_MIX_GUARD_ENTERED 2

/* A re-anchored source logs every entry (a cut) and its first event at once, and later not-mixed
 * events at most once a minute (the #800 telemetry cadence): a hidden source that keeps a late stamp
 * is re-anchored on nearly every mixer tick. */
#define GENLOCK_MIX_GUARD_LOG_INTERVAL_NS (60ULL * 1000000000ULL)
/* What it would have added is computed up to this far behind; farther reads as the maximum (the frame
 * product stays inside 64 bits for any timeline). */
#define GENLOCK_MIX_GUARD_WOULD_ADD_SPAN_NS (60ULL * 1000000000ULL)

/* Whether a source last marked on mixer tick `source_tick` is in the mix on tick `cur_tick` (>= 1 once
 * audio_callback has marked the render order). */
static inline bool genlock_mix_is_member(uint64_t source_tick, uint64_t cur_tick)
{
	return source_tick == cur_tick;
}

/* On marking a member for tick `cur_tick`: did it JOIN the mix on this tick? A source never marked
 * (tick 0) joins on its first membership; a marked one when it was not in the mix on the previous
 * tick. The `prev_source_tick == 0` term only changes the answer on the process's first mixer tick
 * (cur_tick 1, where `0 + 1 == 1` would otherwise read a never-marked source as a member of tick 0);
 * on every later tick the second term already says "joined". */
static inline bool genlock_mix_joined(uint64_t prev_source_tick, uint64_t cur_tick)
{
	return prev_source_tick == 0 || prev_source_tick + 1 != cur_tick;
}

/* The guard's decision for one rendered source: a timeline behind the window (by more than the 1 ns
 * rounding discard_audio tolerates) of a source that is not mixed, or of one that joined the mix on
 * this tick. A composite (scene, transition) takes its timestamp from its children and is never
 * re-anchored itself; a source with no timeline has nothing to re-anchor. */
static inline int genlock_mix_guard_reason(bool member, bool entered, bool composite, uint64_t audio_ts,
					   uint64_t window_start)
{
	if (composite || !audio_ts || audio_ts + 1 >= window_start)
		return GENLOCK_MIX_GUARD_NONE;
	if (!member)
		return GENLOCK_MIX_GUARD_NOT_MIXED;
	return entered ? GENLOCK_MIX_GUARD_ENTERED : GENLOCK_MIX_GUARD_NONE;
}

/* What stock OBS would have added for a source `behind_ns` behind the window, ms: add_audio_buffering's
 * own rounding (whole ticks of `frames_per_tick`, up) and its clamp at the maximum. */
static inline uint32_t genlock_mix_guard_would_add_ms(uint64_t behind_ns, uint32_t sample_rate,
						      uint32_t frames_per_tick, int total_ticks, int max_ticks)
{
	int ticks = max_ticks - total_ticks;

	if (behind_ns < GENLOCK_MIX_GUARD_WOULD_ADD_SPAN_NS) {
		const uint64_t frames = behind_ns * sample_rate / 1000000000ULL;
		const uint64_t need = (frames + frames_per_tick - 1) / frames_per_tick;
		if (need < (uint64_t)ticks)
			ticks = (int)need;
	}
	return (uint32_t)((uint64_t)ticks * frames_per_tick * 1000 / sample_rate);
}

/* Whether the guard logs this re-anchor: an entry (at most one per cut), the source's first one, or one
 * GENLOCK_MIX_GUARD_LOG_INTERVAL_NS after its last line. */
static inline bool genlock_mix_guard_log_due(int reason, uint64_t logged_events, uint64_t last_log_ns,
					     uint64_t now_ns)
{
	return reason == GENLOCK_MIX_GUARD_ENTERED || logged_events == 0 ||
	       now_ns - last_log_ns >= GENLOCK_MIX_GUARD_LOG_INTERVAL_NS;
}
