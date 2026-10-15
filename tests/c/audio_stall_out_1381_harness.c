/* Issue 1381 -- the audio-thread stall ATTRIBUTION harness (tests/audio_stall_out_1381.rs).
 *
 * The 60 s `audio-stall #1367:` line read 100-209 ms between two mixer ticks on stream-snv while
 * the mix itself never took more than ~1.8 ms. The rest of one audio-thread iteration lives in
 * media-io/audio-io.c: the sleep to the tick, the input mutex, and every output's callback. This
 * harness compiles the REAL audio-io.c (included below, so its static functions are callable) on a
 * fake clock and drives `audio_thread` tick by tick:
 *   - os_sleepto_ns_fast / os_gettime_ns are the fake clock (an overshoot per tick, or "already
 *     late" when the target has passed);
 *   - pthread_mutex_lock is macro-substituted so a scripted wait lands INSIDE the lock call;
 *   - the mix (input_cb) and the two output callbacks advance the clock by scripted amounts.
 * It also runs two pieces of obs-audio.c, lifted verbatim by the Rust test: the dump's name
 * resolver (substituted at the identity marker) on a stub output / encoder list with a
 * scriptable trylock, and the dump's log statement (substituted at the log marker) through a blog
 * stub that captures the line.
 *
 * Output lines (the Rust test holds the truth table):
 *   W<n> wake=.. sleep=.. lock=.. pass=.. cb=.. fn=A|B|- param=PA|PB|- mix=..
 *   ID <case> kind=.. name='..' held=..
 *   LINE <the captured LOG_INFO line>
 */
#include <errno.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "util/threading.h"
#include "util/platform.h"
#include "util/bmem.h"
#include "util/base.h"
#include "util/profiler.h"
#include "media-io/audio-resampler.h"

/* ---------------------------------------------------------------- the fake clock + script */
#define MAX_TICKS 8
#define MAX_LOCKS 8

static uint64_t fake_now = 1000000000ULL;
static int script_ticks;   /* audio_thread iterations left in this run */
static int tick;           /* 1-based tick number of the running iteration */
static bool running;       /* the scripted lock waits apply only while audio_thread runs */
static int lock_in_tick;   /* index of the input-mutex lock inside the current tick */
static uint64_t overshoot_ns[MAX_TICKS + 1];
static uint64_t mix_ns[MAX_TICKS + 1];
static uint64_t a_ns[MAX_TICKS + 1];
static uint64_t b_ns[MAX_TICKS + 1];
static uint64_t lock_wait_ns[MAX_TICKS + 1][MAX_LOCKS];

static void script_clear(void)
{
	memset(overshoot_ns, 0, sizeof(overshoot_ns));
	memset(mix_ns, 0, sizeof(mix_ns));
	memset(a_ns, 0, sizeof(a_ns));
	memset(b_ns, 0, sizeof(b_ns));
	memset(lock_wait_ns, 0, sizeof(lock_wait_ns));
	tick = 0;
}

static int h_mutex_lock(pthread_mutex_t *m)
{
	if (running && tick >= 1 && tick <= MAX_TICKS && lock_in_tick < MAX_LOCKS)
		fake_now += lock_wait_ns[tick][lock_in_tick];
	lock_in_tick++;
	return pthread_mutex_lock(m);
}

#define pthread_mutex_lock(m) h_mutex_lock(m)
#include AUDIO_IO_C
#undef pthread_mutex_lock

/* ---------------------------------------------------------------- libobs stubs */
uint64_t os_gettime_ns(void)
{
	return fake_now;
}

/* the real contract: false (no sleep) when the target has passed, else sleep until at least it */
bool os_sleepto_ns_fast(uint64_t time_target)
{
	tick++;
	lock_in_tick = 0;
	if (time_target < fake_now)
		return false;
	fake_now = time_target + overshoot_ns[tick];
	return true;
}

int os_event_try(os_event_t *event)
{
	(void)event;
	if (script_ticks > 0) {
		script_ticks--;
		return EAGAIN;
	}
	return 0;
}

int os_event_init(os_event_t **event, enum os_event_type type)
{
	(void)type;
	*event = NULL;
	return 0;
}

void os_event_destroy(os_event_t *event)
{
	(void)event;
}

int os_event_signal(os_event_t *event)
{
	(void)event;
	return 0;
}

void os_set_thread_name(const char *name)
{
	(void)name;
}

profiler_name_store_t *obs_get_profiler_name_store(void)
{
	return NULL;
}

const char *profile_store_name(profiler_name_store_t *store, const char *format, ...)
{
	(void)store;
	return format;
}

void profile_start(const char *name)
{
	(void)name;
}

void profile_end(const char *name)
{
	(void)name;
}

void profile_reenable_thread(void) {}

audio_resampler_t *audio_resampler_create(const struct resample_info *dst, const struct resample_info *src)
{
	(void)dst;
	(void)src;
	return NULL;
}

void audio_resampler_destroy(audio_resampler_t *resampler)
{
	(void)resampler;
}

bool audio_resampler_resample(audio_resampler_t *resampler, uint8_t *output[], uint32_t *out_frames,
			      uint64_t *ts_offset, const uint8_t *const input[], uint32_t in_frames)
{
	(void)resampler;
	(void)output;
	(void)out_frames;
	(void)ts_offset;
	(void)input;
	(void)in_frames;
	return true;
}

void *bmalloc(size_t size)
{
	return malloc(size ? size : 1);
}

void *brealloc(void *ptr, size_t size)
{
	return realloc(ptr, size ? size : 1);
}

void bfree(void *ptr)
{
	free(ptr);
}

void *bmemdup(const void *ptr, size_t size)
{
	void *out = bmalloc(size);
	if (size)
		memcpy(out, ptr, size);
	return out;
}

static char last_line[1024];

void blog(int log_level, const char *format, ...)
{
	if (log_level != LOG_INFO)
		return;
	va_list args;
	va_start(args, format);
	vsnprintf(last_line, sizeof(last_line), format, args);
	va_end(args);
}

/* ---------------------------------------------------------------- the scripted mix + outputs */
static int tag_a, tag_b;
#define PARAM_A ((void *)&tag_a)
#define PARAM_B ((void *)&tag_b)

static bool fake_mix(void *param, uint64_t start_ts, uint64_t end_ts, uint64_t *new_ts, uint32_t active_mixers,
		     struct audio_output_data *mixes)
{
	(void)param;
	(void)end_ts;
	(void)active_mixers;
	(void)mixes;
	fake_now += mix_ns[tick];
	*new_ts = start_ts;
	return true;
}

static void cb_a(void *param, size_t mix_idx, struct audio_data *data)
{
	(void)param;
	(void)mix_idx;
	(void)data;
	fake_now += a_ns[tick];
}

static void cb_b(void *param, size_t mix_idx, struct audio_data *data)
{
	(void)param;
	(void)mix_idx;
	(void)data;
	fake_now += b_ns[tick];
}

static const char *fn_name(audio_output_callback_t fn)
{
	if (fn == cb_a)
		return "A";
	if (fn == cb_b)
		return "B";
	return fn ? "?" : "-";
}

static const char *param_name(const void *param)
{
	if (param == PARAM_A)
		return "PA";
	if (param == PARAM_B)
		return "PB";
	return param ? "?" : "-";
}

static void print_window(const char *label, struct audio_output *out)
{
	struct audio_output_stall_stats s;
	memset(&s, 0xA5, sizeof(s));
	audio_output_take_stall_stats(out, &s);
	printf("%s wake=%llu sleep=%llu lock=%llu pass=%llu cb=%llu fn=%s param=%s mix=%zu\n", label,
	       (unsigned long long)s.wake_late_max_ns, (unsigned long long)s.sleep_over_max_ns,
	       (unsigned long long)s.lock_wait_max_ns, (unsigned long long)s.out_pass_max_ns,
	       (unsigned long long)s.cb_max_ns, fn_name(s.cb_max_callback), param_name(s.cb_max_param),
	       s.cb_max_mix);
}

static void run_ticks(struct audio_output *out, int n)
{
	script_ticks = n;
	running = true;
	audio_thread(out);
	running = false;
}

static void run_audio_io(void)
{
	struct audio_output *out = bzalloc(sizeof(struct audio_output));
	out->info.name = "harness";
	out->info.format = AUDIO_FORMAT_FLOAT_PLANAR;
	out->info.samples_per_sec = 48000;
	out->info.speakers = SPEAKERS_STEREO;
	out->channels = 2;
	out->planes = 2;
	out->block_size = sizeof(float);
	out->input_cb = fake_mix;
	if (pthread_mutex_init_recursive(&out->input_mutex) != 0) {
		printf("ERR mutex\n");
		exit(2);
	}
	if (!audio_output_connect(out, 0, NULL, cb_a, PARAM_A) || !audio_output_connect(out, 1, NULL, cb_b, PARAM_B)) {
		printf("ERR connect\n");
		exit(2);
	}

	/* window 1, four ticks from S = the clock at audio_thread entry (tick k is due at
	 * S + floor(k * 1024 * 1e9 / 48000)): tick 1 sleeps and the OS wakes it 30 ms late; tick 2
	 * starts behind and output A takes 80 ms; tick 3 starts behind and waits 12 ms for the input
	 * mutex of mix 0; tick 4 ends with output B, a short callback after the slow one. */
	script_clear();
	overshoot_ns[1] = 30000000;
	for (int k = 1; k <= 4; k++) {
		mix_ns[k] = 1000000;
		a_ns[k] = 1000000;
		b_ns[k] = 1000000;
	}
	a_ns[1] = 2000000;
	b_ns[1] = 5000000;
	a_ns[2] = 80000000;
	lock_wait_ns[3][1] = 12000000;
	a_ns[4] = 3000000;
	run_ticks(out, 4);
	print_window("W1", out);
	print_window("W1R", out); /* the take reset the window */

	/* window 2, one tick that sleeps and wakes on time: the get-mixers lock waits 5 ms, output B
	 * (mix 1) is the slowest this time. */
	script_clear();
	mix_ns[1] = 1000000;
	a_ns[1] = 2000000;
	b_ns[1] = 4000000;
	lock_wait_ns[1][0] = 5000000;
	run_ticks(out, 1);
	print_window("W2", out);

	audio_output_take_stall_stats(NULL, NULL);
}

/* ---------------------------------------------------------------- the dump's name resolver */
struct obs_context_data {
	const char *name;
	struct obs_context_data *next;
};
struct obs_output {
	struct obs_context_data context;
};
struct obs_encoder {
	struct obs_context_data context;
};
struct obs_core_data {
	pthread_mutex_t outputs_mutex;
	pthread_mutex_t encoders_mutex;
	struct obs_output *first_output;
	struct obs_encoder *first_encoder;
};
struct obs_core {
	struct obs_core_data data;
};
static struct obs_core core_stub;
static struct obs_core *obs = &core_stub;

static const char *obs_output_get_name(const struct obs_output *output)
{
	return output ? output->context.name : NULL;
}

static const char *obs_encoder_get_name(const struct obs_encoder *encoder)
{
	return encoder ? encoder->context.name : NULL;
}

static bool outputs_busy, encoders_busy;
static int held;

static int h_mutex_trylock(pthread_mutex_t *m)
{
	if ((m == &obs->data.outputs_mutex && outputs_busy) || (m == &obs->data.encoders_mutex && encoders_busy))
		return EBUSY;
	held++;
	return 0;
}

static int h_mutex_unlock(pthread_mutex_t *m)
{
	(void)m;
	held--;
	return 0;
}

#define pthread_mutex_trylock(m) h_mutex_trylock(m)
#define pthread_mutex_unlock(m) h_mutex_unlock(m)
@IDENTITY_FN@
#undef pthread_mutex_trylock
#undef pthread_mutex_unlock

static struct obs_output out1 = {{"NDI PGM", NULL}};
static struct obs_output out2 = {{"Rec", NULL}};
static struct obs_output out3 = {{NULL, NULL}};
static struct obs_encoder enc1 = {{"aac'enc", NULL}};
static int raw_tag;

static void id_case(const char *label, const void *param, uint64_t cb_max_ns, size_t name_size, bool ob, bool eb)
{
	char name[64];
	outputs_busy = ob;
	encoders_busy = eb;
	held = 0;
	memset(name, 'X', sizeof(name));
	const char *kind = audio_stall_out_cb_identity(param, cb_max_ns, name, name_size);
	printf("ID %s kind=%s name='%s' held=%d\n", label, kind, name, held);
}

static void run_identity(void)
{
	out1.context.next = &out2.context;
	out2.context.next = &out3.context;
	obs->data.first_output = &out1;
	obs->data.first_encoder = &enc1;

	id_case("none", &out1, 0, 64, false, false);
	id_case("out1", &out1, 5, 64, false, false);
	id_case("out2", &out2, 5, 64, false, false);
	id_case("enc", &enc1, 5, 64, false, false);
	id_case("raw", &raw_tag, 5, 64, false, false);
	id_case("outbusy-enc", &enc1, 5, 64, true, false);
	id_case("outbusy-raw", &raw_tag, 5, 64, true, false);
	id_case("encbusy-raw", &raw_tag, 5, 64, false, true);
	id_case("encbusy-out", &out1, 5, 64, false, true);
	id_case("trunc", &out1, 5, 4, false, false);
	id_case("noname", &out3, 5, 64, false, false);
}

/* ---------------------------------------------------------------- the dump's log statement */
static uint64_t audio_stall_gap_max_ns = 101400000;
static uint64_t audio_stall_busy_max_ns = 1700000;
static uint64_t audio_stall_ticks = 2816;
static uint64_t audio_stall_ticks_over = 441;

static void run_log_line(void)
{
	size_t sample_rate = 48000;
	struct audio_output_stall_stats out_stall;
	memset(&out_stall, 0, sizeof(out_stall));
	out_stall.wake_late_max_ns = 95260000;
	out_stall.sleep_over_max_ns = 3120000;
	out_stall.lock_wait_max_ns = 12340000;
	out_stall.out_pass_max_ns = 88880000;
	out_stall.cb_max_ns = 80060000;
	out_stall.cb_max_callback = (audio_output_callback_t)(uintptr_t)0x7ff612345678ULL;
	out_stall.cb_max_param = (void *)(uintptr_t)0xdeadbeef00ULL;
	out_stall.cb_max_mix = 1;
	const char *out_cb_kind = "output";
	char out_cb_name[128] = "NDI PGM";
	last_line[0] = '\0';
@STALL_LOG@
	printf("LINE %s\n", last_line);
}

int main(void)
{
	run_audio_io();
	run_identity();
	run_log_line();
	return 0;
}
