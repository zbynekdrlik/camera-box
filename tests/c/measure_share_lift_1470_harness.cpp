/* camera-box issue 1470 -- the C++ harness of tests/av_sync_dock_measure_share_lift_1470.rs.
 *
 * NOT compiled on its own: the test substitutes the shipped obs-audio.c code, lifted verbatim, for the
 * at-sign marker below (convert_time_to_frames + mix_audio, discard_if_stopped + discard_audio), then
 * compiles it with g++ next to the REAL dock tracker (camera-box-measure-share.hpp) and compares the
 * printed trace with its truth table. Everything else here is a stub libobs: the types, a byte deque,
 * a render model of obs_source_audio_render + process_audio_source_tick (every audio source is rendered,
 * as audio_callback's catch-all loop does, active or not; no timeline or too little data = pending; else
 * the input is peeked into the per-source mix buffer), a mix loop that mixes only an ACTIVE source (the
 * root nodes) and a placement model of source_output_audio_place (samples appended to the input from
 * the source's timeline). The mix and the discard are the shipped code. Scope: the source is a ROOT
 * node here; on stream-snv `mbc` is nested (Development > PRO) and reaches the mix through
 * scene_audio_render, whose aligned case is the same offset 0 (its divergences are the header's limit 2).
 *
 * What it proves: every tick the dock TAKES, its block is byte-identical to what the mix carried from
 * the measurement source (the only source in this mix); a tick it does not take is silence. The one
 * residual (a producer-side timeline reset between the dock's read and the next mix) is pinned. */
#include <cinttypes>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include "util/util_uint64.h"
#include "camera-box-measure-share.hpp"

#define AUDIO_OUTPUT_FRAMES 1024
#define MAX_AUDIO_MIXES 6
#define MAX_AUDIO_CHANNELS 8
#define DEBUG_AUDIO 0
#define UNUSED_PARAMETER(param) (void)param

struct ts_info {
	uint64_t start;
	uint64_t end;
};
struct audio_output_data {
	float *data[MAX_AUDIO_CHANNELS];
};
struct obs_core_audio {
	int unused;
};

#define H_DQ_CAP (64 * 1024)
struct deque {
	size_t size;
	uint8_t data[H_DQ_CAP];
};
static inline void deque_push_back(struct deque *dq, const void *d, size_t n)
{
	if (dq->size + n > H_DQ_CAP) {
		std::printf("DEQUE OVERFLOW\n");
		std::exit(3);
	}
	std::memcpy(dq->data + dq->size, d, n);
	dq->size += n;
}
static inline void deque_peek_front(struct deque *dq, void *out, size_t n)
{
	std::memcpy(out, dq->data, n);
}
static inline void deque_pop_front(struct deque *dq, void *out, size_t n)
{
	if (n > dq->size) {
		std::printf("DEQUE UNDERFLOW\n");
		std::exit(3);
	}
	if (out)
		std::memcpy(out, dq->data, n);
	std::memmove(dq->data, dq->data + n, dq->size - n);
	dq->size -= n;
}

struct obs_source_info {
	void *audio_render;
};
typedef struct obs_source {
	struct obs_source_info info;
	bool audio_pending;
	bool pending_stop;
	uint64_t audio_ts;
	struct deque audio_input_buf[MAX_AUDIO_CHANNELS];
	size_t last_audio_input_buf_size;
	float *audio_output_buf[MAX_AUDIO_MIXES][MAX_AUDIO_CHANNELS];
	/* the harness's model, never read by the lift */
	bool h_active;
	uint64_t h_next_sample;
} obs_source_t;

/* ---- the shipped obs-audio.c code, lifted verbatim by the Rust test ---- */
@LIFTED@
/* ---- end of the lift ---- */

static const size_t RATE = 48000;
static const size_t CH = 2;
static float h_out[MAX_AUDIO_MIXES][MAX_AUDIO_CHANNELS][AUDIO_OUTPUT_FRAMES];
static float h_mix[MAX_AUDIO_CHANNELS][AUDIO_OUTPUT_FRAMES];
static float h_dock[MAX_AUDIO_CHANNELS][AUDIO_OUTPUT_FRAMES];

/* libobs's tick boundaries: the start of tick n counted in frames from t0 (audio-io.c). */
static uint64_t h_tick_ns(uint64_t t0, uint64_t n)
{
	return t0 + util_mul_div64(n * AUDIO_OUTPUT_FRAMES, 1000000000ULL, RATE);
}

/* A sample's value: never 0, unique within any window the test compares. */
static float h_sample(uint64_t k)
{
	return 1.0f + (float)(k % 4093u) / 4096.0f;
}

static void h_reset(obs_source_t *s)
{
	std::memset(s, 0, sizeof(*s));
	for (size_t m = 0; m < MAX_AUDIO_MIXES; m++)
		for (size_t c = 0; c < MAX_AUDIO_CHANNELS; c++)
			s->audio_output_buf[m][c] = h_out[m][c];
	s->h_active = true;
}

/* source_output_audio_place, modelled: the first packet sets the timeline, later ones append. */
static void h_place(obs_source_t *s, uint64_t stamp, size_t frames)
{
	if (!s->audio_ts)
		s->audio_ts = stamp;
	for (size_t i = 0; i < frames; i++) {
		const float v = h_sample(s->h_next_sample++);
		for (size_t c = 0; c < CH; c++)
			deque_push_back(&s->audio_input_buf[c], &v, sizeof(float));
	}
}

/* A producer-side timeline reset (reset_audio_timing + reset_audio_data): the input is cleared and the
 * timeline restarts at `stamp`. */
static void h_reset_timeline(obs_source_t *s, uint64_t stamp)
{
	for (size_t c = 0; c < CH; c++)
		s->audio_input_buf[c].size = 0;
	s->last_audio_input_buf_size = 0;
	s->audio_ts = stamp;
}

/* obs_source_audio_render + process_audio_source_tick, modelled (one track, volume 1). */
static void h_render(obs_source_t *s)
{
	const size_t size = AUDIO_OUTPUT_FRAMES * sizeof(float);
	if (!s->audio_ts || s->audio_input_buf[0].size < size) {
		s->audio_pending = true;
		return;
	}
	for (size_t c = 0; c < CH; c++)
		deque_peek_front(&s->audio_input_buf[c], s->audio_output_buf[0][c], size);
	s->audio_pending = false;
}

struct h_counts {
	unsigned ticks, taken, mixed, mismatched, missed;
	bool absent_long;
};

/* One tick: render, the shipped mix + discard, then (when `dock`) the dock's read and its block. A
 * buffering WAIT tick (`mix` false: audio_callback returns false while buffering_wait_ticks > 0) renders
 * and discards but neither mixes nor calls any output callback. */
static void h_tick(obs_source_t *s, camerabox::CbMeasureShareTracker &tr, uint64_t start, uint64_t end, bool dock,
		   struct h_counts *n, bool mix = true)
{
	struct ts_info ts = {start, end};
	std::memset(h_mix, 0, sizeof(h_mix));
	h_render(s);
	/* audio_callback's mix loop: the source is its own root node when active */
	if (mix && s->h_active && !s->audio_pending && s->audio_output_buf[0][0] && s->audio_ts) {
		struct audio_output_data one[MAX_AUDIO_MIXES];
		std::memset(one, 0, sizeof(one));
		for (size_t c = 0; c < CH; c++)
			one[0].data[c] = h_mix[c];
		for (size_t m = 1; m < MAX_AUDIO_MIXES; m++)
			for (size_t c = 0; c < CH; c++)
				one[m].data[c] = h_out[m][c]; /* other tracks: scratch, never compared */
		mix_audio(one, s, CH, RATE, &ts);
	}
	struct obs_core_audio core = {0};
	discard_audio(&core, s, CH, RATE, &ts);
	if (!dock)
		return;

	camerabox::CbShareTick t;
	t.tick_start_ns = start;
	t.tick_end_ns = start + util_mul_div64(AUDIO_OUTPUT_FRAMES, 1000000000ULL, RATE);
	t.source_found = true;
	t.source_active = s->h_active;
	t.source_pending = s->audio_pending;
	t.source_ts_after_ns = s->audio_ts;
	const camerabox::CbShareDecision d = tr.observe(t);
	const float *share[MAX_AUDIO_CHANNELS] = {};
	float *out[MAX_AUDIO_CHANNELS] = {};
	for (size_t c = 0; c < CH; c++) {
		share[c] = s->audio_output_buf[0][c];
		out[c] = h_dock[c];
	}
	camerabox::cb_measure_share_fill(d.take, share, CH, AUDIO_OUTPUT_FRAMES, out);

	bool mixed = false;
	for (size_t i = 0; i < AUDIO_OUTPUT_FRAMES && !mixed; i++)
		mixed = h_mix[0][i] != 0.0f;
	const bool equal = std::memcmp(h_dock[0], h_mix[0], sizeof(h_mix[0])) == 0 &&
			   std::memcmp(h_dock[1], h_mix[1], sizeof(h_mix[1])) == 0;
	n->ticks++;
	n->mixed += mixed;
	n->taken += d.take;
	n->mismatched += d.take && !equal;
	/* conservative silence: the mix carried a whole tick at offset 0 that the dock left silent */
	n->missed += !d.take && h_mix[0][0] != 0.0f && h_mix[0][AUDIO_OUTPUT_FRAMES - 1] != 0.0f;
	n->absent_long = d.absent_long;
}

static void h_print(const char *name, const struct h_counts &n)
{
	std::printf("%s ticks=%u taken=%u mixed=%u mismatched=%u missed=%u absent_long=%d\n", name, n.ticks, n.taken,
		    n.mixed, n.mismatched, n.missed, (int)n.absent_long);
}

static const uint64_t T0 = 7000000000ULL;
static obs_source_t g_src;

/* steady: the timeline starts exactly on a tick; the producer keeps 3 ticks ahead. */
static void scenario_steady()
{
	obs_source_t *s = &g_src;
	h_reset(s);
	camerabox::CbMeasureShareTracker tr;
	struct h_counts n = {};
	h_place(s, h_tick_ns(T0, 0), 3 * AUDIO_OUTPUT_FRAMES);
	for (uint64_t k = 0; k < 40; k++) {
		h_place(s, 0, AUDIO_OUTPUT_FRAMES);
		h_tick(s, tr, h_tick_ns(T0, k), h_tick_ns(T0, k + 1), true, &n);
	}
	h_print("steady", n);
}

/* a source that starts 7 ms into a tick: mixed at an offset the first tick, whole after. */
static void scenario_mid_tick_start()
{
	obs_source_t *s = &g_src;
	h_reset(s);
	camerabox::CbMeasureShareTracker tr;
	struct h_counts n = {};
	h_tick(s, tr, h_tick_ns(T0, 0), h_tick_ns(T0, 1), true, &n); /* nothing yet */
	h_place(s, h_tick_ns(T0, 1) + 7000000ULL, 3 * AUDIO_OUTPUT_FRAMES);
	for (uint64_t k = 1; k < 20; k++) {
		h_place(s, 0, AUDIO_OUTPUT_FRAMES);
		h_tick(s, tr, h_tick_ns(T0, k), h_tick_ns(T0, k + 1), true, &n);
	}
	h_print("mid-tick-start", n);
}

/* the producer stalls for 3 ticks: two ticks run out of data (pending), then it catches up. */
static void scenario_pending()
{
	obs_source_t *s = &g_src;
	h_reset(s);
	camerabox::CbMeasureShareTracker tr;
	struct h_counts n = {};
	h_place(s, h_tick_ns(T0, 0), AUDIO_OUTPUT_FRAMES);
	for (uint64_t k = 0; k < 20; k++) {
		if (k < 5 || k >= 8)
			h_place(s, 0, k == 8 ? 4 * AUDIO_OUTPUT_FRAMES : AUDIO_OUTPUT_FRAMES);
		h_tick(s, tr, h_tick_ns(T0, k), h_tick_ns(T0, k + 1), true, &n);
	}
	h_print("pending", n);
}

/* the source leaves the program for 5 ticks (not in the audio tree; still discarded) and comes back. */
static void scenario_inactive()
{
	obs_source_t *s = &g_src;
	h_reset(s);
	camerabox::CbMeasureShareTracker tr;
	struct h_counts n = {};
	h_place(s, h_tick_ns(T0, 0), 3 * AUDIO_OUTPUT_FRAMES);
	for (uint64_t k = 0; k < 20; k++) {
		s->h_active = !(k >= 6 && k < 11);
		h_place(s, 0, AUDIO_OUTPUT_FRAMES);
		h_tick(s, tr, h_tick_ns(T0, k), h_tick_ns(T0, k + 1), true, &n);
	}
	h_print("inactive", n);
}

/* never in the program for 60 ticks (1.28 s): absent_long. */
static void scenario_never_in_program()
{
	obs_source_t *s = &g_src;
	h_reset(s);
	camerabox::CbMeasureShareTracker tr;
	struct h_counts n = {};
	s->h_active = false;
	h_place(s, h_tick_ns(T0, 0), 3 * AUDIO_OUTPUT_FRAMES);
	for (uint64_t k = 0; k < 60; k++) {
		h_place(s, 0, AUDIO_OUTPUT_FRAMES);
		h_tick(s, tr, h_tick_ns(T0, k), h_tick_ns(T0, k + 1), true, &n);
	}
	h_print("never-in-program", n);
}

/* a timeline behind the window: mix_audio skips it and discard_audio leaves it. */
static void scenario_lagging()
{
	obs_source_t *s = &g_src;
	h_reset(s);
	camerabox::CbMeasureShareTracker tr;
	struct h_counts n = {};
	h_place(s, h_tick_ns(T0, 0) - 5000000ULL, 6 * AUDIO_OUTPUT_FRAMES);
	for (uint64_t k = 0; k < 4; k++)
		h_tick(s, tr, h_tick_ns(T0, k), h_tick_ns(T0, k + 1), true, &n);
	h_print("lagging", n);
}

/* the mix's buffering grows by `g` ticks before tick 10 (add_audio_buffering): libobs inserts g
 * earlier windows that are rendered and discarded with no mix and no output callback (buffering wait
 * ticks), then goes on with the window it would have processed. */
static void scenario_buffering_growth(const char *name, uint64_t g)
{
	obs_source_t *s = &g_src;
	h_reset(s);
	camerabox::CbMeasureShareTracker tr;
	struct h_counts n = {};
	h_place(s, h_tick_ns(T0, 0), 3 * AUDIO_OUTPUT_FRAMES);
	for (uint64_t k = 0; k < 20; k++) {
		if (k == 10) {
			for (uint64_t w = 0; w < g; w++) {
				h_place(s, 0, AUDIO_OUTPUT_FRAMES);
				h_tick(s, tr, h_tick_ns(T0, k - g + w), h_tick_ns(T0, k - g + w + 1), false, &n, false);
			}
		}
		h_place(s, 0, AUDIO_OUTPUT_FRAMES);
		h_tick(s, tr, h_tick_ns(T0, k), h_tick_ns(T0, k + 1), true, &n);
	}
	h_print(name, n);
}

/* KNOWN LIMIT: the producer resets its timeline between the dock's read and the next mix. A restart
 * inside the next tick is mixed at an offset the dock takes as 0; one 1 ns before it is not mixed at
 * all (mix_audio) but popped as aligned (discard_audio). Either way: one wrong tick per reset. */
static void scenario_reset_race(const char *name, int64_t offset_ns)
{
	obs_source_t *s = &g_src;
	h_reset(s);
	camerabox::CbMeasureShareTracker tr;
	struct h_counts n = {};
	h_place(s, h_tick_ns(T0, 0), 3 * AUDIO_OUTPUT_FRAMES);
	for (uint64_t k = 0; k < 12; k++) {
		if (k == 6) {
			h_reset_timeline(s, (uint64_t)((int64_t)h_tick_ns(T0, 6) + offset_ns));
			h_place(s, 0, 4 * AUDIO_OUTPUT_FRAMES);
		} else {
			h_place(s, 0, AUDIO_OUTPUT_FRAMES);
		}
		h_tick(s, tr, h_tick_ns(T0, k), h_tick_ns(T0, k + 1), true, &n);
	}
	h_print(name, n);
}

int main()
{
	setvbuf(stdout, nullptr, _IOLBF, 0);
	scenario_steady();
	scenario_mid_tick_start();
	scenario_pending();
	scenario_inactive();
	scenario_never_in_program();
	scenario_lagging();
	scenario_buffering_growth("buffering-growth-1", 1);
	scenario_buffering_growth("buffering-growth-4", 4);
	scenario_reset_race("reset-race-inside", 7000000);
	scenario_reset_race("reset-race-1ns-early", -1);
	/* a restart past the tick end is not mixed this tick: the end check leaves it silent */
	scenario_reset_race("reset-race-past-the-tick", 25000000);
	return 0;
}
