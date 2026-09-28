/******************************************************************************
    Copyright (C) 2023 by Lain Bailey <lain@obsproject.com>

    This program is free software: you can redistribute it and/or modify
    it under the terms of the GNU General Public License as published by
    the Free Software Foundation, either version 2 of the License, or
    (at your option) any later version.

    This program is distributed in the hope that it will be useful,
    but WITHOUT ANY WARRANTY; without even the implied warranty of
    MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
    GNU General Public License for more details.

    You should have received a copy of the GNU General Public License
    along with this program.  If not, see <http://www.gnu.org/licenses/>.
******************************************************************************/

#include <inttypes.h>
#include "obs-internal.h"
#include "util/util_uint64.h"
#include "obs-genlock-audio-buffering.h" /* camera-box issue 1367: the audio-buffering floor */

struct ts_info {
	uint64_t start;
	uint64_t end;
};

#define DEBUG_AUDIO 0
#define DEBUG_LAGGED_AUDIO 0

static void push_audio_tree(obs_source_t *parent, obs_source_t *source, void *p)
{
	struct obs_core_audio *audio = p;

	if (da_find(audio->render_order, &source, 0) == DARRAY_INVALID) {
		obs_source_t *s = obs_source_get_ref(source);
		if (s) {
			da_push_back(audio->render_order, &s);
			s->audio_is_duplicated = false;
		}
	}

	UNUSED_PARAMETER(parent);
}

static inline bool is_individual_audio_source(obs_source_t *source)
{
	return source->info.type == OBS_SOURCE_TYPE_INPUT && (source->info.output_flags & OBS_SOURCE_AUDIO) &&
	       !(source->info.output_flags & OBS_SOURCE_COMPOSITE);
}

/*
 * This version of push_audio_tree checks whether any source is an Audio Output Capture source ('Desktop Audio',
 * 'wasapi_output_capture' on Windows, 'pulse_output_capture' on Linux, 'coreaudio_output_capture' on macOS), & if the
 * corresponding device is the monitoring device. It then sets the core audio bool 'prevent_monitoring_duplication' to
 * true, which will silence all monitored sources (unless the Audio Output Capture source is muted).
 * Moreover, it has the purpose of detecting sources which appear several times in the audio tree. They are then tagged
 * as such to avoid their mixing in scenes and transitions and mixed directly as root_nodes.
 */
static void push_audio_tree2(obs_source_t *parent, obs_source_t *source, void *p)
{
	if (obs_source_removed(source))
		return;

	struct obs_core_audio *audio = p;
	size_t idx = da_find(audio->render_order, &source, 0);

	if (idx == DARRAY_INVALID) {
		/* First time we see this source → add to render order */
		obs_source_t *s = obs_source_get_ref(source);
		if (s) {
			da_push_back(audio->render_order, &s);
			s->audio_is_duplicated = false;
		}
	} else {
		/* Source already present in tree → mark as duplicated if applicable */
		obs_source_t *s = audio->render_order.array[idx];
		if (is_individual_audio_source(s) && !s->audio_is_duplicated) {
			da_push_back(audio->root_nodes, &source);
			s->audio_is_duplicated = true;
		}
	}
	UNUSED_PARAMETER(parent);
}

static inline size_t convert_time_to_frames(size_t sample_rate, uint64_t t)
{
	return (size_t)util_mul_div64(t, sample_rate, 1000000000ULL);
}

static inline void mix_audio(struct audio_output_data *mixes, obs_source_t *source, size_t channels, size_t sample_rate,
			     struct ts_info *ts)
{
	size_t total_floats = AUDIO_OUTPUT_FRAMES;
	size_t start_point = 0;

	if (source->audio_ts < ts->start || ts->end <= source->audio_ts)
		return;

	if (source->audio_ts != ts->start) {
		start_point = convert_time_to_frames(sample_rate, source->audio_ts - ts->start);
		if (start_point == AUDIO_OUTPUT_FRAMES)
			return;

		total_floats -= start_point;
	}

	for (size_t mix_idx = 0; mix_idx < MAX_AUDIO_MIXES; mix_idx++) {
		for (size_t ch = 0; ch < channels; ch++) {
			register float *mix = mixes[mix_idx].data[ch];
			register float *aud = source->audio_output_buf[mix_idx][ch];
			register float *end;

			mix += start_point;
			end = aud + total_floats;

			while (aud < end)
				*(mix++) += *(aud++);
		}
	}
}

static bool ignore_audio(obs_source_t *source, size_t channels, size_t sample_rate, uint64_t start_ts)
{
	size_t num_floats = source->audio_input_buf[0].size / sizeof(float);
	const char *name = obs_source_get_name(source);

	if (!source->audio_ts && num_floats) {
#if DEBUG_LAGGED_AUDIO == 1
		blog(LOG_DEBUG, "[src: %s] no timestamp, but audio available?", name);
#endif
		for (size_t ch = 0; ch < channels; ch++)
			deque_pop_front(&source->audio_input_buf[ch], NULL, source->audio_input_buf[0].size);
		source->last_audio_input_buf_size = 0;
		return false;
	}

	if (num_floats) {
		/* round up the number of samples to drop */
		size_t drop = (size_t)util_mul_div64(start_ts - source->audio_ts - 1, sample_rate, 1000000000ULL) + 1;
		if (drop > num_floats)
			drop = num_floats;

#if DEBUG_LAGGED_AUDIO == 1
		blog(LOG_DEBUG, "[src: %s] ignored %" PRIu64 "/%" PRIu64 " samples", name, (uint64_t)drop,
		     (uint64_t)num_floats);
#endif
		for (size_t ch = 0; ch < channels; ch++)
			deque_pop_front(&source->audio_input_buf[ch], NULL, drop * sizeof(float));

		source->last_audio_input_buf_size = 0;
		source->audio_ts += util_mul_div64(drop, 1000000000ULL, sample_rate);
		blog(LOG_DEBUG, "[src: %s] ts lag after ignoring: %" PRIu64, name, start_ts - source->audio_ts);

		/* rounding error, adjust */
		if (source->audio_ts == (start_ts - 1))
			source->audio_ts = start_ts;

		/* source is back in sync */
		if (source->audio_ts >= start_ts)
			return true;
	} else {
#if DEBUG_LAGGED_AUDIO == 1
		blog(LOG_DEBUG, "[src: %s] no samples to ignore! ts = %" PRIu64, name, source->audio_ts);
#endif
	}

	if (!source->audio_pending || num_floats) {
		blog(LOG_WARNING,
		     "Source %s audio is lagging (over by %.02f ms) "
		     "at max audio buffering. Restarting source audio.",
		     name, (start_ts - source->audio_ts) / 1000000.);
	}

	source->audio_pending = true;
	source->audio_ts = 0;
	/* tell the timestamp adjustment code in source_output_audio_data to
	 * reset everything, and hopefully fix the timestamps */
	source->timing_set = false;
	return false;
}

static bool discard_if_stopped(obs_source_t *source, size_t channels)
{
	size_t last_size;
	size_t size;

	last_size = source->last_audio_input_buf_size;
	size = source->audio_input_buf[0].size;

	if (!size)
		return false;

	/* if perpetually pending data, it means the audio has stopped,
	 * so clear the audio data */
	if (last_size == size) {
		if (!source->pending_stop) {
			source->pending_stop = true;
#if DEBUG_AUDIO == 1
			blog(LOG_DEBUG, "doing pending stop trick: '%s'", source->context.name);
#endif
			return false;
		}

		for (size_t ch = 0; ch < channels; ch++)
			deque_pop_front(&source->audio_input_buf[ch], NULL, source->audio_input_buf[ch].size);

		source->pending_stop = false;
		source->audio_ts = 0;
		source->last_audio_input_buf_size = 0;
#if DEBUG_AUDIO == 1
		blog(LOG_DEBUG, "source audio data appears to have "
				"stopped, clearing");
#endif
		return true;
	} else {
		source->last_audio_input_buf_size = size;
		return false;
	}
}

#define MAX_AUDIO_SIZE (AUDIO_OUTPUT_FRAMES * sizeof(float))

static inline void discard_audio(struct obs_core_audio *audio, obs_source_t *source, size_t channels,
				 size_t sample_rate, struct ts_info *ts)
{
	size_t total_floats = AUDIO_OUTPUT_FRAMES;
	size_t size;
	/* debug assert only */
	UNUSED_PARAMETER(audio);

#if DEBUG_AUDIO == 1
	bool is_audio_source = source->info.output_flags & OBS_SOURCE_AUDIO;
#endif

	if (source->info.audio_render) {
		source->audio_ts = 0;
		return;
	}

	if (ts->end <= source->audio_ts) {
#if DEBUG_AUDIO == 1
		blog(LOG_DEBUG,
		     "can't discard, source "
		     "timestamp (%" PRIu64 ") >= "
		     "end timestamp (%" PRIu64 ")",
		     source->audio_ts, ts->end);
#endif
		return;
	}

	if (source->audio_ts < (ts->start - 1)) {
		if (source->audio_pending && source->audio_input_buf[0].size < MAX_AUDIO_SIZE &&
		    discard_if_stopped(source, channels))
			return;

#if DEBUG_AUDIO == 1
		if (is_audio_source) {
			blog(LOG_DEBUG,
			     "can't discard, source "
			     "timestamp (%" PRIu64 ") < "
			     "start timestamp (%" PRIu64 ")",
			     source->audio_ts, ts->start);
		}

		/* ignore_audio should have already run and marked this source
		 * pending, unless we *just* added buffering */
		assert(audio->total_buffering_ticks < audio->max_buffering_ticks || source->audio_pending ||
		       !source->audio_ts || audio->buffering_wait_ticks);
#endif
		return;
	}

	if (source->audio_ts != ts->start && source->audio_ts != (ts->start - 1)) {
		size_t start_point = convert_time_to_frames(sample_rate, source->audio_ts - ts->start);
		if (start_point == AUDIO_OUTPUT_FRAMES) {
#if DEBUG_AUDIO == 1
			if (is_audio_source)
				blog(LOG_DEBUG, "can't discard, start point is "
						"at audio frame count");
#endif
			return;
		}

		total_floats -= start_point;
	}

	size = total_floats * sizeof(float);

	if (source->audio_input_buf[0].size < size) {
		if (discard_if_stopped(source, channels))
			return;

#if DEBUG_AUDIO == 1
		if (is_audio_source)
			blog(LOG_DEBUG, "can't discard, data still pending");
#endif
		source->audio_ts = ts->end;
		return;
	}

	for (size_t ch = 0; ch < channels; ch++)
		deque_pop_front(&source->audio_input_buf[ch], NULL, size);

	source->last_audio_input_buf_size = 0;

#if DEBUG_AUDIO == 1
	if (is_audio_source)
		blog(LOG_DEBUG, "audio discarded, new ts: %" PRIu64, ts->end);
#endif

	source->pending_stop = false;
	source->audio_ts = ts->end;
}

static inline bool audio_buffering_maxed(struct obs_core_audio *audio)
{
	return audio->total_buffering_ticks == audio->max_buffering_ticks;
}

/* camera-box issue 1367: raise the total buffering to target_ticks in ONE tick. This is the body of
 * upstream's fixed mode (set_fixed_audio_buffering, target = max_buffering_ticks), shared with the
 * genlock floor (set_floor_audio_buffering, target = floor_buffering_ticks). A target at or below the
 * current total is a no-op (the window and the timestamp queue are left alone; without the guard a
 * negative count would loop ~2^31 times on the audio thread). Returns the new total in ms. */
static size_t raise_audio_buffering(struct obs_core_audio *audio, size_t sample_rate, struct ts_info *ts,
				    int target_ticks)
{
	struct ts_info new_ts;
	int ticks;

	if (target_ticks <= audio->total_buffering_ticks)
		return (size_t)audio->total_buffering_ticks * AUDIO_OUTPUT_FRAMES * 1000 / sample_rate;

	if (!audio->buffering_wait_ticks)
		audio->buffered_ts = ts->start;

	ticks = target_ticks - audio->total_buffering_ticks;
	audio->total_buffering_ticks += ticks;

	new_ts.start =
		audio->buffered_ts - audio_frames_to_ns(sample_rate, audio->buffering_wait_ticks * AUDIO_OUTPUT_FRAMES);

	while (ticks--) {
		const uint64_t cur_ticks = ++audio->buffering_wait_ticks;

		new_ts.end = new_ts.start;
		new_ts.start = audio->buffered_ts - audio_frames_to_ns(sample_rate, cur_ticks * AUDIO_OUTPUT_FRAMES);

#if DEBUG_AUDIO == 1
		blog(LOG_DEBUG, "add buffered ts: %" PRIu64 "-%" PRIu64, new_ts.start, new_ts.end);
#endif

		deque_push_front(&audio->buffered_timestamps, &new_ts, sizeof(new_ts));
	}

	*ts = new_ts;
	return (size_t)audio->total_buffering_ticks * AUDIO_OUTPUT_FRAMES * 1000 / sample_rate;
}

static void set_fixed_audio_buffering(struct obs_core_audio *audio, size_t sample_rate, struct ts_info *ts)
{
	size_t total_ms;

	if (audio_buffering_maxed(audio))
		return;

	total_ms = raise_audio_buffering(audio, sample_rate, ts, audio->max_buffering_ticks);

	blog(LOG_INFO,
	     "Enabling fixed audio buffering, total "
	     "audio buffering is now %d milliseconds",
	     (int)total_ms);
}

/* camera-box issue 1367 (ROZHODNUTE 5857354949): the genlock FLOOR, raised on the first mixer tick
 * of every launch, so every launch starts with the same buffering (stock OBS started at 0 and grew
 * only when a source happened to arrive late). The dynamic increase stays active above it
 * (add_audio_buffering). The line keeps the "total audio buffering is now %d milliseconds" text the
 * #786 launch gates parse; the floor (85 ms at 48 kHz, 92 ms at 44.1 kHz) is under their 100 ms
 * bound. */
static void set_floor_audio_buffering(struct obs_core_audio *audio, size_t sample_rate, struct ts_info *ts)
{
	const size_t total_ms = raise_audio_buffering(audio, sample_rate, ts, audio->floor_buffering_ticks);

	blog(LOG_INFO,
	     "genlock audio buffering floor (issue 1367): total audio buffering is now %d milliseconds from the "
	     "first audio tick, dynamically increasing above",
	     (int)total_ms);
}

static void add_audio_buffering(struct obs_core_audio *audio, size_t sample_rate, struct ts_info *ts, uint64_t min_ts,
				const char *buffering_name)
{
	struct ts_info new_ts;
	uint64_t offset;
	uint64_t frames;
	size_t total_ms;
	size_t ms;
	int ticks;

	if (audio_buffering_maxed(audio))
		return;

	if (!audio->buffering_wait_ticks)
		audio->buffered_ts = ts->start;

	offset = ts->start - min_ts;
	frames = ns_to_audio_frames(sample_rate, offset);
	ticks = (int)((frames + AUDIO_OUTPUT_FRAMES - 1) / AUDIO_OUTPUT_FRAMES);

	audio->total_buffering_ticks += ticks;

	if (audio->total_buffering_ticks >= audio->max_buffering_ticks) {
		ticks -= audio->total_buffering_ticks - audio->max_buffering_ticks;
		audio->total_buffering_ticks = audio->max_buffering_ticks;
		blog(LOG_WARNING, "Max audio buffering reached!");
	}

	ms = ticks * AUDIO_OUTPUT_FRAMES * 1000 / sample_rate;
	total_ms = audio->total_buffering_ticks * AUDIO_OUTPUT_FRAMES * 1000 / sample_rate;

	/* camera-box issue 1367: an increase ABOVE the genlock floor is one LOUD named line: the late
	 * source and the new total (keeping the "total audio buffering is now %d milliseconds" text the
	 * #786 launch gates parse), and where the new total leaves the #1335/#1355 ASRC level band -- a
	 * mixed source on an absolute level target (the stream `mbc`) then sits more than
	 * GENLOCK_AUDIO_LEVEL_REACH_NS from its natural depth (buffering + the ~9 ms nominal base) and
	 * the servo cannot reach it. libobs has no box identity: on the resolume cg OBS a media / NDI
	 * start legitimately grows the buffering, and the note then only matters for a mixed source
	 * on an absolute target there. */
	const int64_t genlock_band_err_ns = genlock_audio_buffering_band_error_ns(
		genlock_audio_buffering_ticks_ns((uint32_t)audio->total_buffering_ticks, AUDIO_OUTPUT_FRAMES,
						 (uint32_t)sample_rate),
		GENLOCK_AUDIO_LEVEL_BASE_NOMINAL_NS, (int64_t)(ASRC_LEVEL_TARGET_MS * 1e6));
	const bool genlock_band_ok = genlock_audio_buffering_band_ok(genlock_band_err_ns);
	blog(LOG_WARNING,
	     "genlock audio buffering ABOVE the floor (issue 1367): adding %d milliseconds of audio buffering, total "
	     "audio buffering is now %d milliseconds (source: %s); ASRC level band %s: buffering + %d ms base lands "
	     "%.1f ms off the %.0f ms level target (reach +/-%d ms)%s",
	     (int)ms, (int)total_ms, buffering_name, genlock_band_ok ? "ok" : "BROKEN",
	     (int)(GENLOCK_AUDIO_LEVEL_BASE_NOMINAL_NS / 1000000ULL), (double)genlock_band_err_ns / 1e6,
	     ASRC_LEVEL_TARGET_MS, (int)(GENLOCK_AUDIO_LEVEL_REACH_NS / 1000000LL),
	     genlock_band_ok ? ""
			     : " -- a mixed source on an absolute ASRC level target (#1335/#1355, e.g. the stream mbc) "
			       "cannot reach it at this buffering");
#if DEBUG_AUDIO == 1
	blog(LOG_DEBUG,
	     "min_ts (%" PRIu64 ") < start timestamp "
	     "(%" PRIu64 ")",
	     min_ts, ts->start);
	blog(LOG_DEBUG, "old buffered ts: %" PRIu64 "-%" PRIu64, ts->start, ts->end);
#endif

	new_ts.start =
		audio->buffered_ts - audio_frames_to_ns(sample_rate, audio->buffering_wait_ticks * AUDIO_OUTPUT_FRAMES);

	while (ticks--) {
		const uint64_t cur_ticks = ++audio->buffering_wait_ticks;

		new_ts.end = new_ts.start;
		new_ts.start = audio->buffered_ts - audio_frames_to_ns(sample_rate, cur_ticks * AUDIO_OUTPUT_FRAMES);

#if DEBUG_AUDIO == 1
		blog(LOG_DEBUG, "add buffered ts: %" PRIu64 "-%" PRIu64, new_ts.start, new_ts.end);
#endif

		deque_push_front(&audio->buffered_timestamps, &new_ts, sizeof(new_ts));
	}

	*ts = new_ts;
}

static bool audio_buffer_insufficient(struct obs_source *source, size_t sample_rate, uint64_t min_ts)
{
	size_t total_floats = AUDIO_OUTPUT_FRAMES;
	size_t size;

	if (source->info.audio_render || source->audio_pending || !source->audio_ts) {
		return false;
	}

	if (source->audio_ts != min_ts && source->audio_ts != (min_ts - 1)) {
		size_t start_point = convert_time_to_frames(sample_rate, source->audio_ts - min_ts);
		if (start_point >= AUDIO_OUTPUT_FRAMES)
			return false;

		total_floats -= start_point;
	}

	size = total_floats * sizeof(float);

	if (source->audio_input_buf[0].size < size) {
		source->audio_pending = true;
		return true;
	}

	return false;
}

/* camera-box issue 1381 (design 5862336131): the mix buffering GUARD -- only a source that is MIXED can
 * move the mix window. Stock OBS walked every audio source in find_min_ts / mark_invalid_sources, so a
 * hidden input nobody hears grew the buffering of the WHOLE mix: 27.9.2026 on resolume the hidden
 * "NDI test", whose timestamps ran later and later, took the cg mix +85/+42/+106/+128/+106/+490 ms to
 * the 960 ms maximum, and every step was a hole in every output (audio_callback returns false while
 * the added ticks drain).
 *
 * A source is IN THE MIX on a mixer tick when an output mix's active tree reached it: audio_callback
 * marks the render order it has built from the mixes (genlock_mix_mark_members) before its catch-all
 * loop adds every other audio source (rendered for meters and monitoring, never mixed). A source that
 * is not mixed and runs behind the window, or one that JOINED the mix on this tick behind it (a cut to
 * it), is re-anchored to the window instead (genlock_mix_guard_reanchor). A source that stays mixed
 * keeps upstream's behaviour: the dynamic increase above the 1367 floor, ignore_audio at the maximum.
 * That includes a source whose audio STARTS only after it joined the mix (a media start on program).
 * Known limit: a source with DIRECT stamps (within MAX_TS_VAR of the OBS clock: ASIO/WASAPI capture,
 * media) that stays late after the entry re-anchor is placed late again by its next packet, and one
 * tick later it is a late mixed source (upstream growth). NDI timecode stamps are never direct, so
 * the restart re-anchors them to their arrival. */

/* The guard's reasons for one rendered source. */
#define GENLOCK_MIX_GUARD_NONE 0
#define GENLOCK_MIX_GUARD_NOT_MIXED 1
#define GENLOCK_MIX_GUARD_ENTERED 2

/* A re-anchored source logs its first event and every entry (a cut) at once, and later not-mixed
 * events at most once per interval: a hidden direct-stamp source that stays late is re-anchored on
 * nearly every mixer tick. */
#define GENLOCK_MIX_GUARD_LOG_INTERVAL_NS 10000000000ULL

/* Whether a source last marked on mixer tick `source_tick` is in the mix on tick `cur_tick` (>= 1 once
 * audio_callback has marked the render order). */
static inline bool genlock_mix_is_member(uint64_t source_tick, uint64_t cur_tick)
{
	return source_tick == cur_tick;
}

/* On marking a member for tick `cur_tick`: did it JOIN the mix on this tick (not in it on the previous
 * one)? A source never marked has tick 0, so on the mixer's first tick (1) it reads as a member of tick
 * 0: the sources present at launch ARE the mix, not an entry, and a source late at launch keeps
 * upstream's behaviour (the 1367 floor, then the dynamic increase). */
static inline bool genlock_mix_joined(uint64_t prev_source_tick, uint64_t cur_tick)
{
	return prev_source_tick + 1 != cur_tick;
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
 * own rounding (whole ticks, up) and its clamp at the maximum. 60 s or more behind reads as the
 * maximum (the frame product stays inside 64 bits for any timeline). */
static inline uint32_t genlock_mix_guard_would_add_ms(uint64_t behind_ns, uint32_t sample_rate, int total_ticks,
						      int max_ticks)
{
	int ticks = max_ticks - total_ticks;

	if (behind_ns < 60000000000ULL) {
		const uint64_t frames = behind_ns * sample_rate / 1000000000ULL;
		const uint64_t need = (frames + AUDIO_OUTPUT_FRAMES - 1) / AUDIO_OUTPUT_FRAMES;
		if (need < (uint64_t)ticks)
			ticks = (int)need;
	}
	return (uint32_t)((uint64_t)ticks * AUDIO_OUTPUT_FRAMES * 1000 / sample_rate);
}

/* Whether the guard logs this re-anchor: an entry (at most one per cut), the source's first one, or one
 * GENLOCK_MIX_GUARD_LOG_INTERVAL_NS after its last line. */
static inline bool genlock_mix_guard_log_due(int reason, uint64_t logged_events, uint64_t last_log_ns,
					     uint64_t now_ns)
{
	return reason == GENLOCK_MIX_GUARD_ENTERED || logged_events == 0 ||
	       now_ns - last_log_ns >= GENLOCK_MIX_GUARD_LOG_INTERVAL_NS;
}

static inline bool genlock_mix_source_is_member(const struct obs_source *source)
{
	return genlock_mix_is_member(source->genlock_mix_tick, obs->audio.genlock_mix_tick);
}

/* audio_callback, right after the output mixes' active trees are in the render order and before the
 * catch-all loop adds every other audio source: advance the mixer tick and mark that render order as
 * the mix. */
static void genlock_mix_mark_members(struct obs_core_audio *audio)
{
	const uint64_t tick = ++audio->genlock_mix_tick;

	for (size_t i = 0; i < audio->render_order.num; i++) {
		obs_source_t *source = audio->render_order.array[i];
		source->genlock_mix_entered = genlock_mix_joined(source->genlock_mix_tick, tick);
		source->genlock_mix_tick = tick;
	}
}

/* Re-anchor a source whose audio runs behind the mix window (reason NOT_MIXED or ENTERED) instead of
 * letting it grow the whole mix's buffering: drop its samples behind the window (ignore_audio's
 * rounding); if none are left, restart its timeline exactly like ignore_audio (audio_pending,
 * audio_ts = 0, timing_set = false), so its next packet is placed fresh -- at its arrival for a stamp
 * that is not direct (an NDI timecode, obs-source.c reset_audio_timing). Counted per source, logged as
 * one `buffering-guard:` WARNING per source per GENLOCK_MIX_GUARD_LOG_INTERVAL_NS. The caller holds
 * audio_buf_mutex. Returns true when the source is back in sync (re-render it). */
static bool genlock_mix_guard_reanchor(struct obs_core_audio *audio, obs_source_t *source, size_t channels,
				       size_t sample_rate, uint64_t start_ts, int reason)
{
	/* The ingest thread may have moved the timeline since the unlocked check. */
	if (!source->audio_ts || source->audio_ts + 1 >= start_ts)
		return false;

	const size_t num_floats = source->audio_input_buf[0].size / sizeof(float);
	const uint64_t behind_ns = start_ts - source->audio_ts;
	size_t drop = (size_t)util_mul_div64(behind_ns - 1, sample_rate, 1000000000ULL) + 1;

	if (drop > num_floats)
		drop = num_floats;
	for (size_t ch = 0; ch < channels; ch++)
		deque_pop_front(&source->audio_input_buf[ch], NULL, drop * sizeof(float));
	source->last_audio_input_buf_size = 0;

	const uint64_t dropped_ns = util_mul_div64(drop, 1000000000ULL, sample_rate);
	source->audio_ts += dropped_ns;
	/* rounding error, adjust (ignore_audio) */
	if (source->audio_ts == start_ts - 1)
		source->audio_ts = start_ts;

	const bool in_sync = source->audio_ts >= start_ts;
	if (!in_sync) {
		source->audio_pending = true;
		source->audio_ts = 0;
		source->timing_set = false;
	}

	source->genlock_mix_guard_events++;
	source->genlock_mix_guard_dropped_ns += dropped_ns;

	const uint64_t now = os_gettime_ns();
	if (genlock_mix_guard_log_due(reason, source->genlock_mix_guard_logged_events,
				      source->genlock_mix_guard_last_log_ns, now)) {
		blog(LOG_WARNING,
		     "buffering-guard: '%s' %s: its audio ran %.1f ms behind the mix window; re-anchored (dropped %.1f ms%s) "
		     "instead of adding %u ms to the whole mix's %d ms of audio buffering (issue 1381; events=%" PRIu64
		     ", +%" PRIu64 " since the last line, dropped_total=%.1f ms)",
		     obs_source_get_name(source),
		     reason == GENLOCK_MIX_GUARD_ENTERED ? "entered the mix late" : "is not mixed",
		     (double)behind_ns / 1e6, (double)dropped_ns / 1e6, in_sync ? "" : ", timeline restarted",
		     genlock_mix_guard_would_add_ms(behind_ns, (uint32_t)sample_rate, audio->total_buffering_ticks,
						    audio->max_buffering_ticks),
		     (int)((size_t)audio->total_buffering_ticks * AUDIO_OUTPUT_FRAMES * 1000 / sample_rate),
		     source->genlock_mix_guard_events,
		     source->genlock_mix_guard_events - source->genlock_mix_guard_logged_events,
		     (double)source->genlock_mix_guard_dropped_ns / 1e6);
		source->genlock_mix_guard_logged_events = source->genlock_mix_guard_events;
		source->genlock_mix_guard_last_log_ns = now;
	}
	return in_sync;
}

static inline const char *find_min_ts(struct obs_core_data *data, uint64_t *min_ts)
{
	obs_source_t *buffering_source = NULL;
	struct obs_source *source = data->first_audio_source;
	while (source) {
		/* camera-box issue 1381: only a MIXED source moves the mix window. */
		if (genlock_mix_source_is_member(source) && !source->audio_pending && source->audio_ts &&
		    source->audio_ts < *min_ts) {
			*min_ts = source->audio_ts;
			buffering_source = source;
		}

		source = (struct obs_source *)source->next_audio_source;
	}
	return buffering_source ? obs_source_get_name(buffering_source) : NULL;
}

static inline bool mark_invalid_sources(struct obs_core_data *data, size_t sample_rate, uint64_t min_ts)
{
	bool recalculate = false;

	struct obs_source *source = data->first_audio_source;
	while (source) {
		/* camera-box issue 1381: a source that is not mixed never forces a recalculation. */
		if (genlock_mix_source_is_member(source))
			recalculate |= audio_buffer_insufficient(source, sample_rate, min_ts);
		source = (struct obs_source *)source->next_audio_source;
	}

	return recalculate;
}

static inline const char *calc_min_ts(struct obs_core_data *data, size_t sample_rate, uint64_t *min_ts)
{
	const char *buffering_name = find_min_ts(data, min_ts);
	if (mark_invalid_sources(data, sample_rate, *min_ts))
		buffering_name = find_min_ts(data, min_ts);
	return buffering_name;
}

static inline void release_audio_sources(struct obs_core_audio *audio)
{
	for (size_t i = 0; i < audio->render_order.num; i++)
		obs_source_release(audio->render_order.array[i]);
}

static inline void execute_audio_tasks(void)
{
	struct obs_core_audio *audio = &obs->audio;
	bool tasks_remaining = true;

	while (tasks_remaining) {
		pthread_mutex_lock(&audio->task_mutex);
		if (audio->tasks.size) {
			struct obs_task_info info;
			deque_pop_front(&audio->tasks, &info, sizeof(info));
			info.task(info.param);
		}
		tasks_remaining = !!audio->tasks.size;
		pthread_mutex_unlock(&audio->task_mutex);
	}
}

/* In case monitoring and an 'Audio Output Capture' source have the same device, one silences all the monitored
 * sources unless the 'Audio Output Capture' is muted.
 */
static inline bool should_silence_monitored_source(obs_source_t *source, struct obs_core_audio *audio)
{
	obs_source_t *dup_src = audio->monitoring_duplicating_source;

	if (!dup_src || !obs_source_active(dup_src))
		return false;

	if (dup_src->monitoring_type == OBS_MONITORING_TYPE_MONITOR_ONLY)
		return false;

	bool fader_muted = close_float(audio->monitoring_duplicating_source->volume, 0.0f, 0.0001f);
	bool output_capture_unmuted = !audio->monitoring_duplicating_source->muted && !fader_muted;

	if (output_capture_unmuted) {
		if (source->monitoring_type == OBS_MONITORING_TYPE_MONITOR_AND_OUTPUT &&
		    source != audio->monitoring_duplicating_source) {
			return true;
		}
	}
	return false;
}

static inline void clear_audio_output_buf(obs_source_t *source, struct obs_core_audio *audio)
{
	if (!audio->monitoring_duplicating_source)
		return;

	uint32_t aoc_mixers = audio->monitoring_duplicating_source->audio_mixers;
	uint32_t source_mixers = source->audio_mixers;

	for (size_t mix = 0; mix < MAX_AUDIO_MIXES; mix++) {
		uint32_t mix_and_val = (1 << mix);
		if ((aoc_mixers & mix_and_val) && (source_mixers & mix_and_val)) {
			for (size_t ch = 0; ch < MAX_AUDIO_CHANNELS; ch++) {
				float *buf = source->audio_output_buf[mix][ch];
				if (buf)
					memset(buf, 0, AUDIO_OUTPUT_FRAMES * sizeof(float));
			}
		}
	}
}

/* camera-box issue 1367 (the FOH-click report, 25.9.2026: the obs-vban raw-audio output on resolume
 * sent with 308-378 ms gaps while the recording was clean) -- an audio-thread STALL probe. Every mixer
 * tick records the gap since the previous tick's entry and its own duration; the 60 s #800 dump logs
 * the window maxima on their own `audio-stall #1367:` line (a marker no other audio/genlock line
 * contains) and resets them. A healthy thread shows tick_gap_max_ms near one tick (21.3 ms at 48 kHz)
 * and ticks_over=0 (gaps over 1.5 ticks). Audio thread only (audio_callback), so plain statics. */
static uint64_t audio_stall_prev_entry_ns = 0;
static uint64_t audio_stall_gap_max_ns = 0;
static uint64_t audio_stall_busy_max_ns = 0;
static uint64_t audio_stall_ticks = 0;
static uint64_t audio_stall_ticks_over = 0;

static void audio_stall_probe_entry(uint64_t entry_ns, uint64_t tick_ns)
{
	if (audio_stall_prev_entry_ns != 0 && entry_ns > audio_stall_prev_entry_ns) {
		const uint64_t gap = entry_ns - audio_stall_prev_entry_ns;
		if (gap > audio_stall_gap_max_ns)
			audio_stall_gap_max_ns = gap;
		if (gap > tick_ns + tick_ns / 2)
			audio_stall_ticks_over++;
	}
	audio_stall_prev_entry_ns = entry_ns;
	audio_stall_ticks++;
}

static void audio_stall_probe_exit(uint64_t entry_ns)
{
	const uint64_t now = os_gettime_ns();
	if (now > entry_ns && now - entry_ns > audio_stall_busy_max_ns)
		audio_stall_busy_max_ns = now - entry_ns;
}

bool audio_callback(void *param, uint64_t start_ts_in, uint64_t end_ts_in, uint64_t *out_ts, uint32_t mixers,
		    struct audio_output_data *mixes)
{
	const uint64_t stall_entry_ns = os_gettime_ns();
	struct obs_core_data *data = &obs->data;
	struct obs_core_audio *audio = &obs->audio;
	struct obs_source *source;
	size_t sample_rate = audio_output_get_sample_rate(audio->audio);
	size_t channels = audio_output_get_channels(audio->audio);
	struct ts_info ts = {start_ts_in, end_ts_in};
	size_t audio_size;
	uint64_t min_ts;

	audio_stall_probe_entry(stall_entry_ns, audio_frames_to_ns(sample_rate, AUDIO_OUTPUT_FRAMES));

	da_resize(audio->render_order, 0);
	da_resize(audio->root_nodes, 0);

	deque_push_back(&audio->buffered_timestamps, &ts, sizeof(ts));
	deque_peek_front(&audio->buffered_timestamps, &ts, sizeof(ts));
	min_ts = ts.start;

	audio_size = AUDIO_OUTPUT_FRAMES * sizeof(float);

#if DEBUG_AUDIO == 1
	blog(LOG_DEBUG, "ts %llu-%llu", ts.start, ts.end);
#endif

	/* ------------------------------------------------ */
	/* build audio render order */

	pthread_mutex_lock(&obs->video.mixes_mutex);
	for (size_t j = 0; j < obs->video.mixes.num; j++) {
		struct obs_view *view = obs->video.mixes.array[j]->view;
		if (!view)
			continue;

		pthread_mutex_lock(&view->channels_mutex);

		/* NOTE: these are source channels, not audio channels */
		for (uint32_t i = 0; i < MAX_CHANNELS; i++) {
			obs_source_t *source = view->channels[i];
			if (!source)
				continue;
			if (!obs_source_active(source))
				continue;
			if (obs_source_removed(source))
				continue;

			/* first, add top - level sources as root_nodes */
			if (obs->video.mixes.array[j]->mix_audio)
				da_push_back(audio->root_nodes, &source);

			/* Build audio tree, tag duplicate individual sources */
			obs_source_enum_active_tree(source, push_audio_tree2, audio);

			/* add top - level sources to audio tree */
			push_audio_tree(NULL, source, audio);
		}
		pthread_mutex_unlock(&view->channels_mutex);
	}
	pthread_mutex_unlock(&obs->video.mixes_mutex);

	/* camera-box issue 1381: the render order so far is the MIX -- every source an output mix's active
	 * tree reached. Mark it before the loop below adds every other audio source (rendered for meters
	 * and monitoring, never mixed). */
	genlock_mix_mark_members(audio);

	pthread_mutex_lock(&data->audio_sources_mutex);

	source = data->first_audio_source;
	while (source) {
		if (!obs_source_removed(source)) {
			push_audio_tree(NULL, source, audio);
		}
		source = (struct obs_source *)source->next_audio_source;
	}

	pthread_mutex_unlock(&data->audio_sources_mutex);

	/* ------------------------------------------------ */
	/* render audio data */
	for (size_t i = 0; i < audio->render_order.num; i++) {
		obs_source_t *source = audio->render_order.array[i];
		obs_source_audio_render(source, mixers, channels, sample_rate, audio_size);
		if (should_silence_monitored_source(source, audio))
			clear_audio_output_buf(source, audio);

		/* camera-box issue 1381: a source that is not mixed, or that joined the mix on this tick, and
		 * runs behind the window is re-anchored to it instead of growing the whole mix's buffering
		 * (genlock_mix_guard_reanchor); a source that stays mixed keeps upstream's path below. */
		const int genlock_guard =
			genlock_mix_guard_reason(genlock_mix_source_is_member(source), source->genlock_mix_entered,
						 source->info.audio_render != NULL, source->audio_ts, ts.start);
		if (genlock_guard != GENLOCK_MIX_GUARD_NONE) {
			pthread_mutex_lock(&source->audio_buf_mutex);
			const bool genlock_rerender = genlock_mix_guard_reanchor(audio, source, channels, sample_rate,
										 ts.start, genlock_guard);
			pthread_mutex_unlock(&source->audio_buf_mutex);
			if (genlock_rerender)
				obs_source_audio_render(source, mixers, channels, sample_rate, audio_size);
			continue;
		}

		/* if a source has gone backward in time and we can no
		 * longer buffer, drop some or all of its audio */
		if (audio_buffering_maxed(audio) && source->audio_ts != 0 && source->audio_ts < ts.start) {
			if (source->info.audio_render) {
				blog(LOG_DEBUG,
				     "render audio source %s timestamp has "
				     "gone backwards",
				     obs_source_get_name(source));

				/* just avoid further damage */
				source->audio_pending = true;
#if DEBUG_AUDIO == 1
				/* this should really be fixed */
				assert(false);
#endif
			} else {
				pthread_mutex_lock(&source->audio_buf_mutex);
				bool rerender = ignore_audio(source, channels, sample_rate, ts.start);
				pthread_mutex_unlock(&source->audio_buf_mutex);

				/* if we (potentially) recovered, re-render */
				if (rerender)
					obs_source_audio_render(source, mixers, channels, sample_rate, audio_size);
			}
		}
	}

	/* ------------------------------------------------ */
	/* get minimum audio timestamp */
	pthread_mutex_lock(&data->audio_sources_mutex);
	const char *buffering_name = calc_min_ts(data, sample_rate, &min_ts);
	pthread_mutex_unlock(&data->audio_sources_mutex);

	/* camera-box #800: audio-side telemetry — the audio twin of the genlock-fifo audit.
	 * The recurring live A/V-desync hunt died on a LOG BLIND SPOT: the video chain is
	 * instrumented per hop (sender timecodes + FIFO audits) while the audio path logged
	 * NOTHING between "adding audio buffering" events, so an in-OBS audio-timeline shift
	 * could never be told apart from an external (console/mastering) chain change. Every
	 * 60 s dump, per audio source: how far its audio timeline sits behind the OS clock
	 * (ts_lag_ms — a smooth ppm-scale GROWTH here = the source's sample clock drifting vs
	 * the wall/render clock, e.g. a Dante Virtual Soundcard slaved to a different clock
	 * domain), buffered depth, and timing_adjust. Runs on the audio thread; audio_ts and
	 * timing_adjust are audio-thread-owned, the input-buf size read is approximate
	 * (unlocked, telemetry-only). */
	{
		static uint64_t t800_last_log_ns = 0;
		const uint64_t t800_now = os_gettime_ns();
		if (t800_now - t800_last_log_ns >= 60000000000ULL) {
			t800_last_log_ns = t800_now;
			blog(LOG_INFO,
			     "audio-telemetry #800: total_buffering=%d ms (ticks=%d/%d) buffering_source=%s",
			     (int)(audio->total_buffering_ticks * AUDIO_OUTPUT_FRAMES * 1000 / sample_rate),
			     (int)audio->total_buffering_ticks, (int)audio->max_buffering_ticks,
			     buffering_name ? buffering_name : "-");
			/* camera-box issue 1367: the audio-thread stall probe's window (see above), then reset. */
			blog(LOG_INFO,
			     "audio-stall #1367: tick_gap_max_ms=%.1f callback_max_ms=%.1f ticks=%llu ticks_over=%llu "
			     "tick_ms=%.1f",
			     (double)audio_stall_gap_max_ns / 1e6, (double)audio_stall_busy_max_ns / 1e6,
			     (unsigned long long)audio_stall_ticks, (unsigned long long)audio_stall_ticks_over,
			     (double)audio_frames_to_ns(sample_rate, AUDIO_OUTPUT_FRAMES) / 1e6);
			audio_stall_gap_max_ns = 0;
			audio_stall_busy_max_ns = 0;
			audio_stall_ticks = 0;
			audio_stall_ticks_over = 0;
			pthread_mutex_lock(&data->audio_sources_mutex);
			struct obs_source *tsrc = data->first_audio_source;
			while (tsrc) {
				if (tsrc->audio_ts || tsrc->audio_input_buf[0].size) {
					const int64_t lag_ms =
						tsrc->audio_ts ? (int64_t)(t800_now - tsrc->audio_ts) / 1000000 : -1;
					/* camera-box #1335: shared bytes->ms helper (obs-internal.h) -- same
					 * value as the pre-#1335 inline computation, now shared with the ASRC
					 * level integral in obs-source.c so the two never drift. */
					const int buf_ms = (int)obs_source_input_buf_ms(tsrc->audio_input_buf[0].size, (uint32_t)sample_rate);
					blog(LOG_INFO,
					     "audio-telemetry #800 '%s': ts_lag_ms=%" PRId64
					     " buffered_ms=%d pending=%d timing_adjust_ms=%" PRId64,
					     obs_source_get_name(tsrc), lag_ms, buf_ms, (int)tsrc->audio_pending,
					     (int64_t)tsrc->timing_adjust / 1000000);
				}
				tsrc = (struct obs_source *)tsrc->next_audio_source;
			}
			pthread_mutex_unlock(&data->audio_sources_mutex);
		}
	}

	/* ------------------------------------------------ */
	/* if a source has gone backward in time, buffer    */
	/* camera-box issue 1367: the genlock floor first (the first tick of every launch), then OBS's own
	 * dynamic increase above it -- genlock_audio_buffering_action() (obs-genlock-audio-buffering.h).
	 * The upstream fixed branch stays byte-identical for rebases; the genlock plan never sets
	 * fixed_buffer, so it is unreachable. */
	const int genlock_buffering = genlock_audio_buffering_action(
		audio->total_buffering_ticks, audio->floor_buffering_ticks, audio->max_buffering_ticks, min_ts < ts.start);
	if (audio->fixed_buffer) {
		if (!audio_buffering_maxed(audio)) {
			set_fixed_audio_buffering(audio, sample_rate, &ts);
		}
	} else if (genlock_buffering == GENLOCK_AUDIO_BUFFERING_ACTION_FLOOR) {
		set_floor_audio_buffering(audio, sample_rate, &ts);
	} else if (genlock_buffering == GENLOCK_AUDIO_BUFFERING_ACTION_DYNAMIC) {
		add_audio_buffering(audio, sample_rate, &ts, min_ts, buffering_name);
	}

	/* ------------------------------------------------ */
	/* mix audio */
	if (!audio->buffering_wait_ticks) {
		for (size_t i = 0; i < audio->root_nodes.num; i++) {
			obs_source_t *source = audio->root_nodes.array[i];

			if (source->audio_pending)
				continue;

			pthread_mutex_lock(&source->audio_buf_mutex);

			if (source->audio_output_buf[0][0] && source->audio_ts)
				mix_audio(mixes, source, channels, sample_rate, &ts);

			pthread_mutex_unlock(&source->audio_buf_mutex);
		}
	}

	/* ------------------------------------------------ */
	/* discard audio */
	pthread_mutex_lock(&data->audio_sources_mutex);

	source = data->first_audio_source;
	while (source) {
		pthread_mutex_lock(&source->audio_buf_mutex);
		discard_audio(audio, source, channels, sample_rate, &ts);
		pthread_mutex_unlock(&source->audio_buf_mutex);

		source = (struct obs_source *)source->next_audio_source;
	}

	pthread_mutex_unlock(&data->audio_sources_mutex);

	/* ------------------------------------------------ */
	/* release audio sources */
	release_audio_sources(audio);

	deque_pop_front(&audio->buffered_timestamps, NULL, sizeof(ts));

	*out_ts = ts.start;

	if (audio->buffering_wait_ticks) {
		audio->buffering_wait_ticks--;
		audio_stall_probe_exit(stall_entry_ns);
		return false;
	}

	execute_audio_tasks();

	UNUSED_PARAMETER(param);
	audio_stall_probe_exit(stall_entry_ns);
	return true;
}
