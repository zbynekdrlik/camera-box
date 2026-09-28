//! Issue 1381 (design 5862336131) — only sources that are MIXED can move the mix's audio
//! buffering.
//!
//! Stock libobs let every audio source move the mix window: `find_min_ts` / `mark_invalid_sources`
//! walk `data->first_audio_source`, and `audio_callback` renders every audio source, mixed or not.
//! 27.9.2026 on resolume a hidden "NDI test" whose timestamps ran later and later grew the whole
//! cg mix +85/+42/+106/+128/+106/+490 ms to the 960 ms maximum — one hole in every output per step.
//!
//! Std-only (no `camera_box::`), so it runs standalone with plain rustc:
//! `CARGO_MANIFEST_DIR=<wt> rustc --edition 2021 --test tests/genlock_audio_mix_guard_1381.rs`.
//!
//! - The wiring anchors (the membership mark, the two min_ts filters, the render-loop re-anchor, the
//!   loud `buffering-guard:` line). The same list is required by the pwsh guard in both
//!   `windows-genlock*.yml` workflows.
//! - A C LIFT of the shipped `audio_callback` path, verbatim: the tail of the render-order build (the
//!   membership mark + the catch-all loop), the render loop + `calc_min_ts`, the 1367 tick decision,
//!   the mix + discard blocks, and every function they call (`push_audio_tree`,
//!   `convert_time_to_frames`, `ignore_audio` .. `calc_min_ts` and the guard). A stub libobs feeds
//!   sources the way `source_output_audio_place` does (bytes only) and drives scenarios: a hidden
//!   late source, a mixed late source, cuts into the mix, a launch-late source. The printed trace is
//!   the truth table. `cc` is required — it FAILS LOUDLY rather than skips.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn read(rel: &str) -> String {
    fs::read_to_string(repo(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

const OBS_AUDIO: &str = "vendor/obs-studio/libobs/obs-audio.c";
const OBS_INTERNAL: &str = "vendor/obs-studio/libobs/obs-internal.h";
const AUDIO_IO_H: &str = "vendor/obs-studio/libobs/media-io/audio-io.h";

/// `(file, squished needle)` — the wiring both this test and the pwsh guard in both
/// `windows-genlock*.yml` workflows require, each exactly once. ONE list, so the two copies cannot
/// drift apart.
const WIRING: [(&str, &str); 6] = [
    (OBS_AUDIO, "genlock_mix_mark_members(audio);"),
    (
        OBS_AUDIO,
        "if (genlock_mix_source_is_member(source) && !source->audio_pending && source->audio_ts && source->audio_ts < *min_ts) {",
    ),
    (
        OBS_AUDIO,
        "if (genlock_mix_source_is_member(source)) recalculate |= audio_buffer_insufficient(source, sample_rate, min_ts);",
    ),
    (
        OBS_AUDIO,
        "const bool genlock_rerender = genlock_mix_guard_reanchor(audio, source, channels, sample_rate, ts.start, genlock_guard);",
    ),
    (
        OBS_AUDIO,
        "\"buffering-guard: '%s' %s: its audio ran %.1f ms behind the mix window; re-anchored (dropped %.1f ms%s) \"",
    ),
    (OBS_INTERNAL, "bool genlock_mix_entered;"),
];

const WINDOWS_WORKFLOWS: [&str; 2] = [
    ".github/workflows/windows-genlock.yml",
    ".github/workflows/windows-genlock-fast.yml",
];

fn pwsh_var(file: &str) -> &'static str {
    match file {
        OBS_AUDIO => "$audio1381",
        OBS_INTERNAL => "$internal1381",
        other => panic!("no pwsh variable for {other}"),
    }
}

#[test]
fn the_mixer_carries_the_mix_buffering_guard_1381() {
    for (file, needle) in WIRING {
        let s = squish(&read(file));
        assert_eq!(
            s.matches(needle).count(),
            1,
            "issue 1381: {file} must carry `{needle}` exactly once -- the mix buffering guard is not \
             wired (a hidden source would grow the whole mix's buffering again)"
        );
    }
    let internal = squish(&read(OBS_INTERNAL));
    for field in [
        "uint64_t genlock_mix_tick;",
        "uint64_t genlock_mix_guard_events;",
        "uint64_t genlock_mix_guard_dropped_ns;",
        "uint64_t genlock_mix_guard_logged_events;",
        "uint64_t genlock_mix_guard_last_log_ns;",
    ] {
        assert!(
            internal.contains(field),
            "issue 1381: obs-internal.h lost the guard field `{field}`"
        );
    }
    assert_eq!(
        internal.matches("uint64_t genlock_mix_tick;").count(),
        2,
        "issue 1381: the mixer tick counter lives in BOTH struct obs_core_audio and struct obs_source"
    );
}

#[test]
fn the_guard_line_never_reads_as_a_786_buffering_draw_1381() {
    // The #786 launch gates (obs-guarded-launch.ps1, launch-obs-genlock.sh, rig-health-audit.py)
    // parse `total audio buffering is now (\d+) milliseconds`: a guard line must never match it,
    // or a hidden source's re-anchor would read as a bad launch draw.
    let joined = squish(&read(OBS_AUDIO)).replace("\" \"", "");
    let start = joined
        .find("\"buffering-guard: ")
        .expect("issue 1381: the buffering-guard line is gone");
    let end = start
        + joined[start..]
            .find("\",")
            .expect("issue 1381: the buffering-guard format does not end");
    let fmt = &joined[start..end];
    assert!(
        !fmt.contains("is now"),
        "issue 1381: the buffering-guard line must not carry the #786 `is now` text: {fmt}"
    );
    assert!(
        joined.contains("blog(LOG_WARNING, \"buffering-guard: "),
        "issue 1381: a re-anchor must be LOUD (LOG_WARNING)"
    );
    // A marker no other audio line contains (the audio-mixer pager and the #800 / 1367 parsers stay
    // independent).
    for other in [
        "audio-stall #1367:",
        "audio-telemetry #800",
        "genlock audio buffering",
    ] {
        assert!(!"buffering-guard: ".contains(other) && !other.contains("buffering-guard"));
    }
}

#[test]
fn windows_workflows_guard_the_same_wiring_1381() {
    for wf in WINDOWS_WORKFLOWS {
        let text = read(wf);
        for (file, needle) in WIRING {
            let line = format!(
                "{} -notmatch [regex]::Escape('{}')",
                pwsh_var(file),
                needle.replace('\'', "''")
            );
            assert!(
                text.contains(&line),
                "issue 1381: {wf} no longer guards `{needle}` ({file}) -- its pwsh copy drifted \
                 from WIRING"
            );
        }
        for (file, var) in [(OBS_AUDIO, "$audio1381"), (OBS_INTERNAL, "$internal1381")] {
            assert!(
                text.contains(&format!(
                    "{var} = (Get-Content \"{file}\" -Raw) -replace '\\s+', ' '"
                )),
                "issue 1381: {wf} does not load {file} into {var}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The C lift: the shipped bytes, driven.

/// The text of `src` from `start` up to (not including) `end`.
fn slice_between(src: &str, start: &str, end: &str) -> String {
    let a = src
        .find(start)
        .unwrap_or_else(|| panic!("issue 1381: lift anchor `{start}` not found"));
    let b = src[a..]
        .find(end)
        .unwrap_or_else(|| panic!("issue 1381: lift end `{end}` not found after `{start}`"));
    src[a..a + b].to_string()
}

/// One `static inline` function of `src`, from its signature through its closing `\n}\n`.
fn lift_fn(src: &str, signature: &str) -> String {
    let a = src
        .find(signature)
        .unwrap_or_else(|| panic!("issue 1381: `{signature}` not found"));
    let b = src[a..]
        .find("\n}\n")
        .unwrap_or_else(|| panic!("issue 1381: `{signature}` does not end"));
    src[a..a + b + 3].to_string()
}

/// The functions `audio_callback` calls, verbatim: `push_audio_tree`, `convert_time_to_frames`,
/// and `ignore_audio` through `calc_min_ts` (the guard's helpers included).
fn lift_functions() -> String {
    let s = read(OBS_AUDIO);
    let io = read(AUDIO_IO_H);
    [
        lift_fn(&io, "static inline uint64_t audio_frames_to_ns("),
        lift_fn(&io, "static inline uint64_t ns_to_audio_frames("),
        slice_between(
            &s,
            "static void push_audio_tree(",
            "static inline bool is_individual_audio_source(",
        ),
        slice_between(
            &s,
            "static inline size_t convert_time_to_frames(",
            "static inline void mix_audio(",
        ),
        slice_between(
            &s,
            "static bool ignore_audio(",
            "static inline void release_audio_sources(",
        ),
    ]
    .join("\n")
}

/// The blocks of `audio_callback`, verbatim, in order.
fn lift_callback_blocks() -> [String; 4] {
    let s = read(OBS_AUDIO);
    let decision_start = "\tconst int genlock_buffering = genlock_audio_buffering_action(";
    let decision_tail =
        "add_audio_buffering(audio, sample_rate, &ts, min_ts, buffering_name);\n\t}\n";
    let a = s
        .find(decision_start)
        .expect("issue 1381: the 1367 tick decision is gone");
    let b = s[a..]
        .find(decision_tail)
        .expect("issue 1381: the 1367 tick decision's end is gone")
        + decision_tail.len();
    [
        // the render-order build after the output mixes: the mark + the catch-all loop
        slice_between(
            &s,
            "\tpthread_mutex_unlock(&obs->video.mixes_mutex);\n",
            "\t/* ------------------------------------------------ */\n\t/* render audio data */",
        ),
        // the render loop + the minimum timestamp
        slice_between(
            &s,
            "\t/* render audio data */\n",
            "\t/* camera-box #800: audio-side telemetry",
        ),
        s[a..a + b].to_string(),
        // mix + discard
        slice_between(
            &s,
            "\t/* mix audio */\n",
            "\t/* ------------------------------------------------ */\n\t/* release audio sources */",
        ),
    ]
}

const HARNESS_HEAD: &str = r#"#include <inttypes.h>
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
"#;

const HARNESS_DRIVER: &str = r#"
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
"#;

fn c_harness() -> String {
    let blocks = lift_callback_blocks();
    let driver = HARNESS_DRIVER
        .replace("@BLOCK0@", &blocks[0])
        .replace("@BLOCK1@", &blocks[1])
        .replace("@BLOCK2@", &blocks[2])
        .replace("@BLOCK3@", &blocks[3]);
    format!(
        "{HARNESS_HEAD}\n/* ---- lifted verbatim from obs-audio.c / audio-io.h ---- */\n{}\n/* ---- end lift ---- */\n{driver}",
        lift_functions()
    )
}

fn scratch_dir() -> PathBuf {
    let base = option_env!("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join(format!(
        "genlock_audio_mix_guard_1381_{}",
        std::process::id()
    ))
}

/// The lift's printed trace, compiled and run ONCE per test binary (parallel tests compiling into
/// one scratch dir race each other).
fn c_trace() -> &'static [String] {
    static TRACE: OnceLock<Vec<String>> = OnceLock::new();
    TRACE.get_or_init(build_and_run_lift)
}

fn build_and_run_lift() -> Vec<String> {
    let dir = scratch_dir();
    fs::create_dir_all(&dir).expect("create the lift scratch dir");
    let harness = dir.join("harness.c");
    let bin = dir.join("harness.bin");
    fs::write(&harness, c_harness()).expect("write the lift harness");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=gnu11",
            "-Wall",
            "-Wextra",
            "-Wformat=2",
            "-Werror",
            "-O1",
        ])
        .arg("-I")
        .arg(repo("vendor/obs-studio/libobs"))
        .arg(&harness)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "issue 1381: could not run the C compiler `{cc}` ({e}). This gate compiles the \
                 shipped audio mixer C; it must FAIL rather than skip. Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "issue 1381: the lifted obs-audio.c mixer code does NOT COMPILE standalone under -Wall \
         -Wextra -Wformat=2 -Werror:\n--- cc stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin)
        .output()
        .expect("issue 1381: the compiled lift harness failed to execute");
    assert!(
        run.status.success(),
        "issue 1381: the lift harness exited non-zero:\n{}",
        String::from_utf8_lossy(&run.stdout)
    );
    let _ = fs::remove_dir_all(&dir);
    String::from_utf8(run.stdout)
        .expect("harness stdout is utf-8")
        .lines()
        .map(str::to_string)
        .collect()
}

/// The summary line of `scenario`.
fn summary<'a>(trace: &'a [String], scenario: &str) -> &'a str {
    let head = format!("{scenario}: ");
    trace
        .iter()
        .find(|l| l.starts_with(&head))
        .unwrap_or_else(|| {
            panic!(
                "issue 1381: no summary for {scenario}:\n{}",
                trace.join("\n")
            )
        })
}

/// The lines of one scenario (from its `== name` header to the next).
fn section<'a>(trace: &'a [String], scenario: &str) -> Vec<&'a str> {
    let head = format!("== {scenario}");
    let a = trace
        .iter()
        .position(|l| *l == head)
        .unwrap_or_else(|| panic!("issue 1381: no section {scenario}"));
    trace[a + 1..]
        .iter()
        .take_while(|l| !l.starts_with("== "))
        .map(String::as_str)
        .collect()
}

/// The truth table: the whole printed trace, verified line by line against the scenario physics
/// (the processed window = real time - 85.33 ms at the floor; `waits` counts the ticks
/// `audio_callback` returned false, the launch floor alone is 4; `mic_missed` the ticks it returned
/// true without mixing the always-mixed mic).
const EXPECTED_TRACE: &[&str] = &[
    "== hidden-ndi-jumps",
    "L300 genlock audio buffering floor (issue 1367): total audio buffering is now 85 milliseconds from the first audio tick, dynamically increasing above",
    "L200 buffering-guard: 'NDI test' is not mixed: its audio ran 224.7 ms behind the mix window; re-anchored (dropped 30.0 ms, timeline restarted) instead of adding 234 ms to the whole mix's 85 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=30.0 ms)",
    "L200 buffering-guard: 'NDI test' is not mixed: its audio ran 324.7 ms behind the mix window; re-anchored (dropped 30.0 ms, timeline restarted) instead of adding 341 ms to the whole mix's 85 ms of audio buffering (issue 1381; events=2, +1 since the last line, dropped_total=60.0 ms)",
    "L200 buffering-guard: 'NDI test' is not mixed: its audio ran 621.3 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 640 ms to the whole mix's 85 ms of audio buffering (issue 1381; events=3, +1 since the last line, dropped_total=80.0 ms)",
    "L200 buffering-guard: 'NDI test' is not mixed: its audio ran 1418.0 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 874 ms to the whole mix's 85 ms of audio buffering (issue 1381; events=4, +1 since the last line, dropped_total=100.0 ms)",
    "hidden-ndi-jumps: waits=4 total_ms=85 mic_missed=0 'NDI test' events=4 dropped_ms=100.0 mixed=0 guard_lines=4 above_lines=0 restart_lines=0",
    "== hidden-direct-late",
    "L200 buffering-guard: 'Cam audio' is not mixed: its audio ran 300.0 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 320 ms to the whole mix's 0 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=20.0 ms)",
    "L300 genlock audio buffering floor (issue 1367): total audio buffering is now 85 milliseconds from the first audio tick, dynamically increasing above",
    "L200 buffering-guard: 'Cam audio' is not mixed: its audio ran 220.0 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 234 ms to the whole mix's 85 ms of audio buffering (issue 1381; events=470, +469 since the last line, dropped_total=10020.0 ms)",
    "L200 buffering-guard: 'Cam audio' is not mixed: its audio ran 215.3 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 234 ms to the whole mix's 85 ms of audio buffering (issue 1381; events=939, +469 since the last line, dropped_total=20030.0 ms)",
    "hidden-direct-late: waits=4 total_ms=85 mic_missed=0 'Cam audio' events=1400 dropped_ms=29860.0 mixed=0 guard_lines=3 above_lines=0 restart_lines=0",
    "== hidden-scene",
    "L300 genlock audio buffering floor (issue 1367): total audio buffering is now 85 milliseconds from the first audio tick, dynamically increasing above",
    "hidden-scene: waits=4 total_ms=85 mic_missed=0 'Scene NDI' events=0 dropped_ms=0.0 mixed=0 guard_lines=0 above_lines=0 restart_lines=0",
    "== mixed-late",
    "L300 genlock audio buffering floor (issue 1367): total audio buffering is now 85 milliseconds from the first audio tick, dynamically increasing above",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 85 milliseconds of audio buffering, total audio buffering is now 170 milliseconds (source: mbc); ASRC level band BROKEN: buffering + 9 ms base lands -79.7 ms off the 100 ms level target (reach +/-35 ms) -- a mixed source on an absolute ASRC level target (#1335/#1355, e.g. the stream mbc) cannot reach it at this buffering",
    "mixed-late: waits=8 total_ms=170 mic_missed=0 'mbc' events=0 dropped_ms=0.0 mixed=292 guard_lines=0 above_lines=1 restart_lines=0",
    "== cut-in-backlog",
    "L300 genlock audio buffering floor (issue 1367): total audio buffering is now 85 milliseconds from the first audio tick, dynamically increasing above",
    "L200 buffering-guard: 'NDI cut' entered the mix late: its audio ran 193.3 ms behind the mix window; re-anchored (dropped 193.4 ms) instead of adding 213 ms to the whole mix's 85 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=193.4 ms)",
    "cut-in-backlog: waits=4 total_ms=85 mic_missed=0 'NDI cut' events=1 dropped_ms=193.4 mixed=200 guard_lines=1 above_lines=0 restart_lines=0",
    "== cut-in-jump",
    "L300 genlock audio buffering floor (issue 1367): total audio buffering is now 85 milliseconds from the first audio tick, dynamically increasing above",
    "L200 buffering-guard: 'NDI jump' entered the mix late: its audio ran 218.0 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 234 ms to the whole mix's 85 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=20.0 ms)",
    "cut-in-jump: waits=4 total_ms=85 mic_missed=0 'NDI jump' events=1 dropped_ms=20.0 mixed=195 guard_lines=1 above_lines=0 restart_lines=0",
    "== cut-in-direct-limit",
    "L200 buffering-guard: 'Media late' is not mixed: its audio ran 300.0 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 320 ms to the whole mix's 0 ms of audio buffering (issue 1381; events=1, +1 since the last line, dropped_total=20.0 ms)",
    "L300 genlock audio buffering floor (issue 1367): total audio buffering is now 85 milliseconds from the first audio tick, dynamically increasing above",
    "L200 buffering-guard: 'Media late' entered the mix late: its audio ran 218.0 ms behind the mix window; re-anchored (dropped 20.0 ms, timeline restarted) instead of adding 234 ms to the whole mix's 85 ms of audio buffering (issue 1381; events=101, +100 since the last line, dropped_total=2150.0 ms)",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 256 milliseconds of audio buffering, total audio buffering is now 341 milliseconds (source: Media late); ASRC level band BROKEN: buffering + 9 ms base lands -250.3 ms off the 100 ms level target (reach +/-35 ms) -- a mixed source on an absolute ASRC level target (#1335/#1355, e.g. the stream mbc) cannot reach it at this buffering",
    "cut-in-direct-limit: waits=16 total_ms=341 mic_missed=0 'Media late' events=101 dropped_ms=2150.0 mixed=186 guard_lines=2 above_lines=1 restart_lines=0",
    "== launch-late",
    "L300 genlock audio buffering floor (issue 1367): total audio buffering is now 85 milliseconds from the first audio tick, dynamically increasing above",
    "L200 genlock audio buffering ABOVE the floor (issue 1367): adding 42 milliseconds of audio buffering, total audio buffering is now 128 milliseconds (source: ASIO); ASRC level band BROKEN: buffering + 9 ms base lands -37.0 ms off the 100 ms level target (reach +/-35 ms) -- a mixed source on an absolute ASRC level target (#1335/#1355, e.g. the stream mbc) cannot reach it at this buffering",
    "launch-late: waits=6 total_ms=128 mic_missed=0 'ASIO' events=0 dropped_ms=0.0 mixed=294 guard_lines=0 above_lines=1 restart_lines=0",
];

#[test]
fn a_source_that_is_not_mixed_never_grows_the_mix_buffering_1381() {
    let trace = c_trace();
    for scenario in ["hidden-ndi-jumps", "hidden-direct-late", "hidden-scene"] {
        let s = summary(trace, scenario);
        assert!(
            s.contains(" waits=4 total_ms=85 mic_missed=0 ") && s.contains(" above_lines=0 "),
            "issue 1381: a source that is not mixed moved the mix window ({scenario}):\n{s}\n\n{}",
            section(trace, scenario).join("\n")
        );
    }
    // A composite (an off-program scene reporting its late child) is never re-anchored itself.
    assert!(summary(trace, "hidden-scene").contains(" events=0 "));
}

#[test]
fn a_mixed_late_source_keeps_the_upstream_increase_1381() {
    let trace = c_trace();
    for scenario in ["mixed-late", "launch-late"] {
        let s = summary(trace, scenario);
        assert!(
            s.contains(" above_lines=1 ") && s.contains(" events=0 "),
            "issue 1381: a MIXED late source must keep OBS's own dynamic increase ({scenario}):\n{s}"
        );
    }
}

#[test]
fn a_cut_into_the_mix_is_re_anchored_without_a_hole_1381() {
    let trace = c_trace();
    for scenario in ["cut-in-backlog", "cut-in-jump"] {
        let s = summary(trace, scenario);
        assert!(
            s.contains(" waits=4 total_ms=85 mic_missed=0 ") && s.contains(" events=1 "),
            "issue 1381: a late source cut into the mix cut a hole into the others ({scenario}):\n{s}\n\n{}",
            section(trace, scenario).join("\n")
        );
        assert!(
            section(trace, scenario)
                .iter()
                .any(|l| l.contains("entered the mix late")),
            "issue 1381: the entry re-anchor of {scenario} must be logged"
        );
    }
}

#[test]
fn the_guard_lines_are_loud_and_name_what_they_saved_1381() {
    let trace = c_trace();
    let lines: Vec<&String> = trace
        .iter()
        .filter(|l| l.contains("buffering-guard: "))
        .collect();
    assert!(
        !lines.is_empty(),
        "issue 1381: no buffering-guard line at all"
    );
    for l in lines {
        assert!(
            l.starts_with("L200 buffering-guard: '")
                && (l.contains("' is not mixed: ") || l.contains("' entered the mix late: "))
                && l.contains(" ms behind the mix window; re-anchored (dropped ")
                && l.contains(" instead of adding ")
                && l.contains(" (issue 1381; events=")
                && !l.contains("is now"),
            "issue 1381: a malformed buffering-guard line: {l}"
        );
    }
}

#[test]
fn the_shipped_c_matches_the_truth_table_1381() {
    let trace = c_trace();
    for (i, (got, want)) in trace.iter().zip(EXPECTED_TRACE).enumerate() {
        assert_eq!(
            got,
            want,
            "issue 1381: the lifted C diverges from the decided behaviour at trace line {i}:\n{}",
            trace.join("\n")
        );
    }
    assert_eq!(
        trace.len(),
        EXPECTED_TRACE.len(),
        "issue 1381: the lifted C printed {} lines, the truth table has {}:\n{}",
        trace.len(),
        EXPECTED_TRACE.len(),
        trace.join("\n")
    );
}
