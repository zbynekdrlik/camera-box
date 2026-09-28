/* camera-box issue 1381 -- the C harness of tests/genlock_audio_mix_guard_1381.rs.
 *
 * NOT compiled on its own: the test substitutes the shipped obs-audio.c / audio-io.h code, lifted
 * verbatim, for the six at-sign markers below, compiles the result with cc and compares
 * the printed trace with its truth table. Everything else here is a stub libobs (the types, the
 * deques, the render / mix stubs) and a source model of obs-source.c's ingest. */
#include <inttypes.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "util/util_uint64.h"
#include "media-io/asrc-compensator.h"
#include "obs-genlock-audio-buffering.h"
#include "obs-genlock-mix-guard.h"

#define AUDIO_OUTPUT_FRAMES 1024
#define MAX_AUDIO_MIXES 6
#define MAX_AUDIO_CHANNELS 8
#define LOG_WARNING 200
#define LOG_INFO 300
#define LOG_DEBUG 400
#define DEBUG_AUDIO 0
#define DEBUG_LAGGED_AUDIO 0
#define DARRAY_INVALID ((size_t)-1)

/* pthread_mutex_t comes from <sys/types.h>; the mixer tick is single-threaded here. */
#define pthread_mutex_lock(m) ((void)(m))
#define pthread_mutex_unlock(m) ((void)(m))

struct ts_info {
	uint64_t start;
	uint64_t end;
};

/* libobs util/deque.h, bytes only for the audio input buffers (virt), real for the timestamp queue. */
#define H_DQ_CAP 2048
struct deque {
	bool virt;
	size_t size;
	uint8_t data[H_DQ_CAP];
};
static inline void deque_push_back(struct deque *dq, const void *d, size_t n)
{
	if (dq->size + n > H_DQ_CAP) {
		printf("DEQUE OVERFLOW\n");
		exit(3);
	}
	memcpy(dq->data + dq->size, d, n);
	dq->size += n;
}
static inline void deque_push_front(struct deque *dq, const void *d, size_t n)
{
	if (dq->size + n > H_DQ_CAP) {
		printf("DEQUE OVERFLOW\n");
		exit(3);
	}
	memmove(dq->data + n, dq->data, dq->size);
	memcpy(dq->data, d, n);
	dq->size += n;
}
static inline void deque_peek_front(struct deque *dq, void *out, size_t n)
{
	memcpy(out, dq->data, n);
}
static inline void deque_pop_front(struct deque *dq, void *out, size_t n)
{
	if (n > dq->size) {
		printf("DEQUE UNDERFLOW\n");
		exit(3);
	}
	if (!dq->virt) {
		if (out)
			memcpy(out, dq->data, n);
		memmove(dq->data, dq->data + n, dq->size - n);
	}
	dq->size -= n;
}

typedef struct obs_source obs_source_t;
struct h_darray {
	obs_source_t *array[16];
	size_t num;
};
static inline void h_da_push(struct h_darray *v, obs_source_t *s)
{
	v->array[v->num++] = s;
}
static inline size_t h_da_find(struct h_darray *v, obs_source_t *s, size_t idx)
{
	for (size_t i = idx; i < v->num; i++)
		if (v->array[i] == s)
			return i;
	return DARRAY_INVALID;
}
#define da_resize(v, n) ((v).num = (n))
#define da_push_back(v, pitem) h_da_push(&(v), *(pitem))
#define da_find(v, pitem, idx) h_da_find(&(v), *(pitem), (idx))

struct obs_source_info {
	void *audio_render;
};
struct obs_source {
	struct obs_source_info info;
	bool audio_pending;
	bool pending_stop;
	volatile bool timing_set;
	uint64_t audio_ts;
	struct deque audio_input_buf[MAX_AUDIO_CHANNELS];
	size_t last_audio_input_buf_size;
	float *audio_output_buf[MAX_AUDIO_MIXES][MAX_AUDIO_CHANNELS];
	struct obs_source *next_audio_source;
	pthread_mutex_t audio_buf_mutex;
	bool audio_is_duplicated;
	/* issue 1381 */
	uint64_t genlock_mix_tick;
	bool genlock_mix_entered;
	uint64_t genlock_mix_guard_events;
	uint64_t genlock_mix_guard_dropped_ns;
	uint64_t genlock_mix_guard_logged_events;
	uint64_t genlock_mix_guard_last_log_ns;
	/* the harness's source model (obs-source.c ingest), never read by the lift */
	const char *h_name;
	bool h_keeps_stamp;
	bool h_running;
	int64_t h_lag_ns;
	uint64_t h_fed_until_ns;
	uint64_t h_mixed;
	/* the audio_ts its output buffer was last rendered from; a mix from any other front is stale */
	uint64_t h_rendered_ts;
	uint64_t h_stale;
};
struct obs_core_audio {
	struct h_darray render_order;
	struct h_darray root_nodes;
	uint64_t buffered_ts;
	struct deque buffered_timestamps;
	uint64_t buffering_wait_ticks;
	int total_buffering_ticks;
	int max_buffering_ticks;
	int floor_buffering_ticks;
	bool fixed_buffer;
};
struct obs_core_data {
	pthread_mutex_t audio_sources_mutex;
	struct obs_source *first_audio_source;
};
struct obs_core_video {
	pthread_mutex_t mixes_mutex;
};
struct obs_core {
	struct obs_core_audio audio;
	struct obs_core_data data;
	struct obs_core_video video;
};
static struct obs_core h_obs;
static struct obs_core *obs = &h_obs;
struct audio_output_data {
	float *data[MAX_AUDIO_CHANNELS];
};

static uint64_t h_now_ns;
/* the start of the window the current tick processes (the front of buffered_timestamps) */
static uint64_t h_window_start;
static inline uint64_t os_gettime_ns(void)
{
	return h_now_ns;
}
static inline const char *obs_source_get_name(const obs_source_t *s)
{
	return s->h_name;
}
static inline obs_source_t *obs_source_get_ref(obs_source_t *s)
{
	return s;
}
static inline bool obs_source_removed(const obs_source_t *s)
{
	(void)s;
	return false;
}

static int h_guard_lines, h_above_lines, h_restart_lines;
static void blog(int level, const char *fmt, ...) __attribute__((format(printf, 2, 3)));
static void blog(int level, const char *fmt, ...)
{
	char line[1024];
	va_list a;
	va_start(a, fmt);
	vsnprintf(line, sizeof(line), fmt, a);
	va_end(a);
	if (strstr(line, "buffering-guard: "))
		h_guard_lines++;
	if (strstr(line, "ABOVE the floor"))
		h_above_lines++;
	if (strstr(line, "Restarting source audio"))
		h_restart_lines++;
	if (level != LOG_DEBUG)
		printf("L%d %s\n", level, line);
}

/* obs-source.c obs_source_audio_render: an individual source keeps process_audio_source_tick's pending
 * rule; a composite (a scene) reports its earliest visible child (scene_audio_render), here a child
 * 50 ms behind the window. Upstream mix_audio's window test counts a mixed source as heard. */
static void obs_source_audio_render(obs_source_t *source, uint32_t mixers, size_t channels, size_t sample_rate,
				    size_t size)
{
	(void)mixers;
	(void)channels;
	(void)sample_rate;
	if (source->info.audio_render) {
		source->audio_ts = h_window_start - 50000000ULL;
		source->audio_pending = false;
		return;
	}
	if (!source->audio_ts || source->audio_input_buf[0].size < size) {
		source->audio_pending = true;
		return;
	}
	source->audio_pending = false;
	source->h_rendered_ts = source->audio_ts;
}
static inline void mix_audio(struct audio_output_data *mixes, obs_source_t *source, size_t channels,
			     size_t sample_rate, struct ts_info *ts)
{
	(void)mixes;
	(void)channels;
	(void)sample_rate;
	if (source->audio_ts < ts->start || ts->end <= source->audio_ts)
		return;
	source->h_mixed++;
	if (source->h_rendered_ts != source->audio_ts)
		source->h_stale++;
}
static inline bool should_silence_monitored_source(obs_source_t *source, struct obs_core_audio *audio)
{
	(void)source;
	(void)audio;
	return false;
}
static inline void clear_audio_output_buf(obs_source_t *source, struct obs_core_audio *audio)
{
	(void)source;
	(void)audio;
}

/* ---- lifted verbatim from obs-audio.c / audio-io.h ---- */
@LIFTED_FUNCTIONS@
/* ---- end lift ---- */

/* ---- the harness: a source model and scenarios ---- */
#define H_RATE 48000u
#define H_CH 2u
#define H_PKT_FRAMES 480u
#define H_PKT_NS 10000000ULL
#define H_T0 1000000000ULL /* 1 s after "boot": the guard's first line must not wait out its interval */
#define H_MAX_BUF_SIZE (1000 * AUDIO_OUTPUT_FRAMES * sizeof(float)) /* obs-source.c MAX_BUF_SIZE */
#define H_MINUTE_TICKS 2813 /* one minute of mixer ticks at 48 kHz (60 s / 21.33 ms, rounded up) */

static float h_out_buf[AUDIO_OUTPUT_FRAMES];
static uint64_t h_t;
static uint64_t h_tick_ns;

/* obs-source.c source_output_audio_place, bytes only: a packet older than the buffer start resets
 * it; otherwise it lands at its offset and the buffer ends right after it. */
static void h_place(obs_source_t *s, uint64_t ts_in)
{
	if (!s->audio_ts || ts_in < s->audio_ts) {
		for (size_t ch = 0; ch < H_CH; ch++)
			s->audio_input_buf[ch].size = 0;
		s->last_audio_input_buf_size = 0;
		s->audio_ts = ts_in;
	}
	const size_t placement = (size_t)util_mul_div64(ts_in - s->audio_ts, H_RATE, 1000000000ULL) * sizeof(float);
	const size_t size = H_PKT_FRAMES * sizeof(float);
	if (placement + size > H_MAX_BUF_SIZE)
		return;
	for (size_t ch = 0; ch < H_CH; ch++)
		s->audio_input_buf[ch].size = placement + size;
	s->last_audio_input_buf_size = 0;
}

/* The 10 ms packets that arrived up to `now`. A packet's placed start is its arrival - 10 ms - lag.
 * source_output_audio_data: a timeline restart (timing_set false) re-anchors a source whose placement
 * follows timing_adjust (a stamp that is not direct and not in a genlock TIMECODE hold: an NDI source
 * with genlock off or on the latency hold) to its arrival (reset_audio_timing). A source that keeps its
 * stamp (h_keeps_stamp: a direct stamp -- ASIO/WASAPI/media -- or a genlock TIMECODE hold, whose term
 * cancels timing_adjust) keeps its lag. */
static void h_feed(obs_source_t *s, uint64_t now)
{
	while (s->h_running && s->h_fed_until_ns + H_PKT_NS <= now) {
		const uint64_t arrival = s->h_fed_until_ns + H_PKT_NS;
		s->h_fed_until_ns = arrival;
		if (!s->timing_set) {
			if (!s->h_keeps_stamp)
				s->h_lag_ns = -(int64_t)H_PKT_NS;
			s->timing_set = true;
		}
		h_place(s, (uint64_t)((int64_t)arrival - (int64_t)H_PKT_NS - s->h_lag_ns));
	}
}

/* A receiver that stalled and now delivers `backlog_ns` of queued packets at once. */
static void h_burst(obs_source_t *s, uint64_t now, uint64_t backlog_ns)
{
	for (uint64_t t = now - backlog_ns; t + H_PKT_NS <= now; t += H_PKT_NS)
		h_place(s, t);
	s->h_fed_until_ns = now;
	s->h_running = true;
}

/* obs_free_audio + obs_init_audio + obs_reset_audio2: the core audio state starts over (a fresh
 * launch, or an audio reset of a running OBS -- the sources and the time go on). */
static void h_audio_reset(void)
{
	const struct genlock_audio_buffering_plan p = genlock_audio_buffering_make_plan(0, false, H_RATE, AUDIO_OUTPUT_FRAMES);
	memset(&h_obs.audio, 0, sizeof(h_obs.audio));
	h_obs.audio.max_buffering_ticks = (int)p.max_ticks;
	h_obs.audio.floor_buffering_ticks = (int)p.floor_ticks;
	h_obs.audio.fixed_buffer = p.fixed;
}

static void h_launch(void)
{
	memset(&h_obs, 0, sizeof(h_obs));
	h_audio_reset();
	h_t = H_T0;
	h_now_ns = H_T0;
	h_tick_ns = audio_frames_to_ns(H_RATE, AUDIO_OUTPUT_FRAMES);
	h_guard_lines = h_above_lines = h_restart_lines = 0;
}

static void h_add(obs_source_t *s, const char *name, bool keeps_stamp, int64_t lag_ns, bool timing_set)
{
	memset(s, 0, sizeof(*s));
	s->h_name = name;
	s->h_keeps_stamp = keeps_stamp;
	s->h_lag_ns = lag_ns;
	s->timing_set = timing_set;
	s->h_running = true;
	s->h_fed_until_ns = H_T0;
	for (size_t ch = 0; ch < MAX_AUDIO_CHANNELS; ch++)
		s->audio_input_buf[ch].virt = true;
	s->audio_output_buf[0][0] = h_out_buf;
	obs_source_t **link = &h_obs.data.first_audio_source;
	while (*link)
		link = &(*link)->next_audio_source;
	*link = s;
}

static obs_source_t h_mic, h_other;
static int h_waits, h_mic_missed;
/* The source the ingest thread re-places between the render loop and calc_min_ts, and when. */
static int h_tick_no, h_race_tick = -1;
static uint64_t h_race_behind_ns;

/* One audio_callback, the shipped blocks pasted in order. `mix` = the sources the output mixes'
 * active trees reach this tick (channel sources, so root nodes). Returns audio_callback's result:
 * false = a tick the outputs never receive (a hole). */
static bool h_tick(obs_source_t *const *mix, size_t n_mix)
{
	struct obs_core_data *data = &obs->data;
	struct obs_core_audio *audio = &obs->audio;
	struct obs_source *source;
	struct audio_output_data *mixes = NULL;
	const uint32_t mixers = 1;
	const size_t sample_rate = H_RATE;
	const size_t channels = H_CH;
	struct ts_info ts = {h_t, h_t + h_tick_ns};
	size_t audio_size;
	uint64_t min_ts;

	h_now_ns = ts.end;
	for (source = data->first_audio_source; source; source = source->next_audio_source)
		h_feed(source, h_now_ns);

	da_resize(audio->render_order, 0);
	da_resize(audio->root_nodes, 0);
	deque_push_back(&audio->buffered_timestamps, &ts, sizeof(ts));
	deque_peek_front(&audio->buffered_timestamps, &ts, sizeof(ts));
	min_ts = ts.start;
	h_window_start = ts.start;
	audio_size = AUDIO_OUTPUT_FRAMES * sizeof(float);

	/* the output mixes' active trees (upstream: root nodes + push_audio_tree2 / push_audio_tree) */
	for (size_t i = 0; i < n_mix; i++) {
		obs_source_t *m = mix[i];
		da_push_back(audio->root_nodes, &m);
		push_audio_tree(NULL, m, audio);
	}

/* ---- lifted verbatim from obs-audio.c audio_callback ---- */
@BLOCK0@
@BLOCK1@
	/* the ingest thread races the mixer: source_output_audio_data holds only the source's
	 * audio_buf_mutex, so a late packet can land between the render loop and calc_min_ts */
	if (h_tick_no == h_race_tick)
		h_place(&h_other, h_now_ns - h_race_behind_ns);
@BLOCK2@
@BLOCK3@
@BLOCK4@
/* ---- end lift ---- */

	deque_pop_front(&audio->buffered_timestamps, NULL, sizeof(ts));
	h_t += h_tick_ns;
	h_tick_no++;
	if (audio->buffering_wait_ticks) {
		audio->buffering_wait_ticks--;
		return false;
	}
	return true;
}

/* One tick: the mic in the mix when `mic_mixed`, h_other when `other_mixed`. */
static void h_step2(bool mic_mixed, bool other_mixed)
{
	obs_source_t *mix[2];
	size_t n = 0;
	if (mic_mixed)
		mix[n++] = &h_mic;
	if (other_mixed)
		mix[n++] = &h_other;
	const uint64_t before = h_mic.h_mixed;
	if (!h_tick(mix, n))
		h_waits++;
	else if (mic_mixed && h_mic.h_mixed == before)
		h_mic_missed++;
}

/* One tick with the mic always in the mix. */
static void h_step(bool other_mixed)
{
	h_step2(true, other_mixed);
}

static void h_summary(const char *scenario)
{
	printf("%s: waits=%d total_ms=%d mic_missed=%d '%s' events=%" PRIu64 " dropped_ms=%.1f mixed=%" PRIu64
	       " stale=%" PRIu64 " guard_lines=%d above_lines=%d restart_lines=%d\n",
	       scenario, h_waits, (int)(h_obs.audio.total_buffering_ticks * AUDIO_OUTPUT_FRAMES * 1000 / H_RATE),
	       h_mic_missed, h_other.h_name, h_other.genlock_mix_guard_events,
	       (double)h_other.genlock_mix_guard_dropped_ns / 1e6, h_other.h_mixed, h_mic.h_stale + h_other.h_stale,
	       h_guard_lines, h_above_lines, h_restart_lines);
}

static void h_begin(const char *scenario, const char *other, bool keeps_stamp, int64_t lag_ns, bool timing_set)
{
	printf("== %s\n", scenario);
	h_launch();
	h_waits = h_mic_missed = 0;
	h_tick_no = 0;
	h_race_tick = -1;
	h_add(&h_mic, "Mic/Aux", true, 0, true);
	h_add(&h_other, other, keeps_stamp, lag_ns, timing_set);
}

/* One direct call of the guard on a hand-built source whose timeline starts `behind_ns` before a window
 * at 5 s with `floats` samples buffered: its decision (not mixed, no entry) and, when it re-anchors,
 * the result, plus ignore_audio's result on a copy (the re-anchor mirrors it). `direct` calls the
 * re-anchor even when the decision is none (the ingest moved the timeline between the unlocked check
 * and the locked re-anchor). */
static void h_probe(const char *what, uint64_t behind_ns, size_t floats, bool direct)
{
	static obs_source_t s, t;
	const uint64_t start = 5000000000ULL;
	memset(&s, 0, sizeof(s));
	s.h_name = "probe";
	for (size_t ch = 0; ch < MAX_AUDIO_CHANNELS; ch++)
		s.audio_input_buf[ch].virt = true;
	for (size_t ch = 0; ch < H_CH; ch++)
		s.audio_input_buf[ch].size = floats * sizeof(float);
	s.timing_set = true;
	s.audio_ts = behind_ns ? start - behind_ns : 0;
	t = s;
	const int reason = genlock_mix_guard_reason(false, false, false, s.audio_ts, start);
	int sync = -1;
	const char *parity = "n/a";
	if (reason != GENLOCK_MIX_GUARD_NONE || direct)
		sync = genlock_mix_guard_reanchor(&h_obs.audio, &s, H_CH, H_RATE, start,
						  reason != GENLOCK_MIX_GUARD_NONE ? reason : GENLOCK_MIX_GUARD_NOT_MIXED)
			       ? 1
			       : 0;
	if (reason != GENLOCK_MIX_GUARD_NONE) {
		const bool t_sync = ignore_audio(&t, H_CH, H_RATE, start);
		parity = (t_sync == (sync == 1) && t.audio_ts == s.audio_ts &&
			  t.audio_input_buf[0].size == s.audio_input_buf[0].size && t.audio_pending == s.audio_pending &&
			  t.timing_set == s.timing_set)
				 ? "same"
				 : "DIFF";
	}
	char ts[48];
	if (s.audio_ts)
		snprintf(ts, sizeof(ts), "start%+" PRId64 "ns", (int64_t)(s.audio_ts - start));
	else
		snprintf(ts, sizeof(ts), "restarted");
	printf("probe %s: reason=%d in_sync=%d ts=%s left=%zu pending=%d timing_set=%d events=%" PRIu64
	       " ignore_audio=%s\n",
	       what, reason, sync, ts, s.audio_input_buf[0].size / sizeof(float), s.audio_pending ? 1 : 0,
	       s.timing_set ? 1 : 0, s.genlock_mix_guard_events, parity);
}

int main(void)
{
	/* (a) RC4: a HIDDEN NDI source whose placement follows timing_adjust (genlock off, or the latency
	 * hold) and whose timeline jumps later four times, a minute apart, the last past the maximum (what
	 * it would have added is clamped there). Stock OBS grew the whole mix at every jump; the guard
	 * re-anchors the hidden source instead, and its timing restart puts it back at its arrival. */
	h_begin("hidden-ndi-jumps", "NDI test", false, 0, false);
	for (int i = 0; i < 4 * H_MINUTE_TICKS; i++) {
		if (i == 60)
			h_other.h_lag_ns = 300000000;
		if (i == 60 + H_MINUTE_TICKS)
			h_other.h_lag_ns = 400000000;
		if (i == 60 + 2 * H_MINUTE_TICKS)
			h_other.h_lag_ns = 700000000;
		if (i == 60 + 3 * H_MINUTE_TICKS)
			h_other.h_lag_ns = 1500000000;
		h_step(false);
	}
	h_summary("hidden-ndi-jumps");

	/* (a') a HIDDEN source that keeps its stamp (direct, or a genlock TIMECODE hold) 300 ms late for
	 * good: re-anchored on every tick, one line a minute, the mix never grows. */
	h_begin("hidden-kept-stamp-late", "Cam audio", true, 300000000, true);
	for (int i = 0; i < 2 * H_MINUTE_TICKS + 100; i++)
		h_step(false);
	h_summary("hidden-kept-stamp-late");

	/* (a'') the ingest thread re-places a hidden on-time source 300 ms late between the render loop
	 * and calc_min_ts: it is not mixed, so it never reaches min_ts; the next tick re-anchors it. */
	h_begin("hidden-ingest-race", "NDI race", false, 0, false);
	h_race_tick = 100;
	h_race_behind_ns = 300000000;
	for (int i = 0; i < 300; i++)
		h_step(false);
	h_summary("hidden-ingest-race");

	/* (a''') an audio reset of a running OBS (the core audio state starts over, the sources go on): a
	 * source mixed on the 50 ticks before it keeps tick 50. Hidden after it, it delivers a 300 ms
	 * backlog on the 50th tick after the reset -- where a counter reset with the core state would
	 * read its stale tick as a member again. It is still not mixed. */
	h_begin("audio-reset", "NDI old", false, 0, false);
	for (int i = 0; i < 50; i++)
		h_step(true);
	h_audio_reset();
	for (int i = 0; i < 150; i++) {
		if (i == 49)
			h_burst(&h_other, h_t + h_tick_ns, 300000000);
		h_step(false);
	}
	h_summary("audio-reset");

	/* (b) a MIXED source whose timeline goes 150 ms late: upstream, the dynamic increase above the
	 * floor (one hole of the added ticks), no guard. */
	h_begin("mixed-late", "mbc", true, 0, true);
	for (int i = 0; i < 300; i++) {
		if (i == 60)
			h_other.h_lag_ns = 150000000;
		h_step(true);
	}
	h_summary("mixed-late");

	/* (c) a cut to a hidden NDI source whose receiver stalled and delivers a 300 ms backlog as it is
	 * cut in: re-anchored on the entry tick, no hole in the mic. */
	h_begin("cut-in-backlog", "NDI cut", false, 0, false);
	for (int i = 0; i < 300; i++) {
		if (i == 80)
			h_other.h_running = false;
		if (i == 100)
			h_burst(&h_other, h_t + h_tick_ns, 300000000);
		h_step(i >= 100);
	}
	h_summary("cut-in-backlog");

	/* (c') a cut to a hidden NDI source (placement follows timing_adjust) whose timeline jumps 300 ms
	 * late on the same tick: its timeline restarts and its next packet is on time; no hole. */
	h_begin("cut-in-jump", "NDI jump", false, 0, false);
	for (int i = 0; i < 300; i++) {
		if (i == 100)
			h_other.h_lag_ns = 300000000;
		h_step(i >= 100);
	}
	h_summary("cut-in-jump");

	/* (c'') a cut to a program SCENE (a composite, never an audio-list source) that reports a child
	 * 50 ms behind the window: the scene joins the mix and is never re-anchored itself. */
	h_begin("cut-in-scene", "Scene", false, 0, true);
	h_obs.data.first_audio_source = &h_mic;
	h_mic.next_audio_source = NULL;
	h_other.info.audio_render = &h_other;
	h_other.h_running = false;
	for (int i = 0; i < 300; i++)
		h_step(i >= 100);
	h_summary("cut-in-scene");

	/* (d) the known limit: a cut to a hidden source that keeps its stamp (direct, or a genlock
	 * TIMECODE hold) 300 ms late for good. The entry re-anchor cannot move such a stamp, so a tick or
	 * two later (the restarted source is pending while it refills) it is a late MIXED source and
	 * upstream grows the mix (one hole). */
	h_begin("cut-in-kept-stamp-limit", "NDI genlock tc", true, 300000000, true);
	for (int i = 0; i < 300; i++)
		h_step(i >= 100);
	h_summary("cut-in-kept-stamp-limit");

	/* (e) a launch in the frontend's order: the audio thread runs (the floor is raised) before the
	 * scene collection loads, so the program sources JOIN the mix later. A direct-stamp source 100 ms
	 * late when it joins is an entry (re-anchored), then a late MIXED source: upstream's increase (the
	 * 1367 stream ASIO startup race). */
	h_begin("launch-late", "ASIO", true, 100000000, true);
	for (int i = 0; i < 300; i++)
		h_step2(i >= 10, i >= 10);
	h_summary("launch-late");

	/* The guard's exact-sample edges and its locked re-check, called directly. */
	printf("== probes\n");
	h_launch();
	h_now_ns = H_T0 + 60000000000ULL;
	h_probe("1ns-behind-is-rounding", 1, 960, false);
	h_probe("2ns-behind", 2, 960, false);
	h_probe("10ms-exact", 10000000, 960, false);
	h_probe("rounding-adjust", 20834, 960, false);
	h_probe("exhausted", 30000000, 960, false);
	h_probe("moved-on-before-the-lock", 1, 960, true);
	h_probe("reset-before-the-lock", 0, 960, true);
	/* entry = not in the mix on the previous tick; a source's first membership is an entry */
	printf("probe joined: first-tick=%d next-tick=%d skipped-a-tick=%d later-first=%d\n", genlock_mix_joined(0, 1),
	       genlock_mix_joined(5, 6), genlock_mix_joined(4, 6), genlock_mix_joined(0, 7));
	return 0;
}
