/* camera-box issue 1381 -- the C harness of tests/genlock_audio_mix_guard_1381.rs.
 *
 * NOT compiled on its own: the test substitutes the shipped obs-audio.c / audio-io.h code, lifted
 * verbatim, for the five at-sign markers below, compiles the result with cc and compares
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
	bool h_direct;
	bool h_running;
	int64_t h_lag_ns;
	uint64_t h_fed_until_ns;
	uint64_t h_mixed;
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
	uint64_t genlock_mix_tick;
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
 * source_output_audio_data: a timeline restart (timing_set false) re-anchors a stamp that is NOT
 * direct to its arrival (reset_audio_timing); a direct stamp keeps its own lag. */
static void h_feed(obs_source_t *s, uint64_t now)
{
	while (s->h_running && s->h_fed_until_ns + H_PKT_NS <= now) {
		const uint64_t arrival = s->h_fed_until_ns + H_PKT_NS;
		s->h_fed_until_ns = arrival;
		if (!s->timing_set) {
			if (!s->h_direct)
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

static void h_launch(void)
{
	const struct genlock_audio_buffering_plan p = genlock_audio_buffering_make_plan(0, false, H_RATE, AUDIO_OUTPUT_FRAMES);
	memset(&h_obs, 0, sizeof(h_obs));
	h_obs.audio.max_buffering_ticks = (int)p.max_ticks;
	h_obs.audio.floor_buffering_ticks = (int)p.floor_ticks;
	h_obs.audio.fixed_buffer = p.fixed;
	h_t = H_T0;
	h_now_ns = H_T0;
	h_tick_ns = audio_frames_to_ns(H_RATE, AUDIO_OUTPUT_FRAMES);
	h_guard_lines = h_above_lines = h_restart_lines = 0;
}

static void h_add(obs_source_t *s, const char *name, bool direct, int64_t lag_ns, bool timing_set)
{
	memset(s, 0, sizeof(*s));
	s->h_name = name;
	s->h_direct = direct;
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
@BLOCK2@
@BLOCK3@
/* ---- end lift ---- */

	deque_pop_front(&audio->buffered_timestamps, NULL, sizeof(ts));
	h_t += h_tick_ns;
	if (audio->buffering_wait_ticks) {
		audio->buffering_wait_ticks--;
		return false;
	}
	return true;
}

static int h_waits, h_mic_missed;
static obs_source_t h_mic, h_other;

/* One tick with the mic always in the mix and h_other in it when `other_mixed`. */
static void h_step(bool other_mixed)
{
	obs_source_t *mix[2] = {&h_mic, &h_other};
	const uint64_t before = h_mic.h_mixed;
	if (!h_tick(mix, other_mixed ? 2 : 1))
		h_waits++;
	else if (h_mic.h_mixed == before)
		h_mic_missed++;
}

static void h_summary(const char *scenario)
{
	printf("%s: waits=%d total_ms=%d mic_missed=%d '%s' events=%" PRIu64 " dropped_ms=%.1f mixed=%" PRIu64
	       " guard_lines=%d above_lines=%d restart_lines=%d\n",
	       scenario, h_waits, (int)(h_obs.audio.total_buffering_ticks * AUDIO_OUTPUT_FRAMES * 1000 / H_RATE),
	       h_mic_missed, h_other.h_name, h_other.genlock_mix_guard_events,
	       (double)h_other.genlock_mix_guard_dropped_ns / 1e6, h_other.h_mixed, h_guard_lines, h_above_lines,
	       h_restart_lines);
}

static void h_begin(const char *scenario, const char *other, bool direct, int64_t lag_ns, bool timing_set)
{
	printf("== %s\n", scenario);
	h_launch();
	h_waits = h_mic_missed = 0;
	h_add(&h_mic, "Mic/Aux", true, 0, true);
	h_add(&h_other, other, direct, lag_ns, timing_set);
}

int main(void)
{
	/* (a) RC4: a HIDDEN NDI source (stamps not direct) whose timeline jumps later four times, the last
	 * past the maximum (what it would have added is clamped there). Stock OBS grew the whole mix at
	 * every jump; the guard re-anchors the hidden source instead. */
	h_begin("hidden-ndi-jumps", "NDI test", false, 0, false);
	for (int i = 0; i < 1700; i++) {
		if (i == 60)
			h_other.h_lag_ns = 300000000;
		if (i == 600)
			h_other.h_lag_ns = 400000000;
		if (i == 1100)
			h_other.h_lag_ns = 700000000;
		if (i == 1600)
			h_other.h_lag_ns = 1500000000;
		h_step(false);
	}
	h_summary("hidden-ndi-jumps");

	/* (a') a HIDDEN source with DIRECT stamps 300 ms late for good: re-anchored on every tick, one
	 * line per 10 s, the mix never grows. */
	h_begin("hidden-direct-late", "Cam audio", true, 300000000, true);
	for (int i = 0; i < 1400; i++)
		h_step(false);
	h_summary("hidden-direct-late");

	/* (a'') an OFF-PROGRAM scene whose visible child runs late reports that child's timestamp: stock
	 * OBS let it move the mix window too. It is not mixed, so it never does; and as a composite it is
	 * never re-anchored itself (its children are). */
	h_begin("hidden-scene", "Scene NDI", false, 0, true);
	h_other.info.audio_render = &h_other;
	h_other.h_running = false;
	for (int i = 0; i < 300; i++)
		h_step(false);
	h_summary("hidden-scene");

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

	/* (c') a cut to a hidden NDI source whose timeline jumps 300 ms late on the same tick: its
	 * timeline restarts and its next packet is on time; no hole. */
	h_begin("cut-in-jump", "NDI jump", false, 0, false);
	for (int i = 0; i < 300; i++) {
		if (i == 100)
			h_other.h_lag_ns = 300000000;
		h_step(i >= 100);
	}
	h_summary("cut-in-jump");

	/* (d) the known limit: a cut to a hidden source with DIRECT stamps 300 ms late for good. The
	 * entry re-anchor cannot move a direct stamp, so one tick later it is a late MIXED source and
	 * upstream grows the mix (one hole). */
	h_begin("cut-in-direct-limit", "Media late", true, 300000000, true);
	for (int i = 0; i < 300; i++)
		h_step(i >= 100);
	h_summary("cut-in-direct-limit");

	/* (e) a source already 100 ms late at LAUNCH is part of the mix, not an entry: the floor, then
	 * upstream's one-tick increase (the 1367 stream ASIO startup race). */
	h_begin("launch-late", "ASIO", true, 100000000, true);
	for (int i = 0; i < 300; i++)
		h_step(true);
	h_summary("launch-late");
	return 0;
}
