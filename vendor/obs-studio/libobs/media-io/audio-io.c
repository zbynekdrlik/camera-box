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

#include <math.h>
#include <inttypes.h>

#include "../util/threading.h"
#include "../util/darray.h"
#include "../util/deque.h"
#include "../util/platform.h"
#include "../util/profiler.h"
#include "../util/util_uint64.h"

#include "audio-io.h"
#include "audio-resampler.h"

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <Windows.h>
#include <avrt.h>
#endif

extern profiler_name_store_t *obs_get_profiler_name_store(void);

/* #define DEBUG_AUDIO */

#define nop()                    \
	do {                     \
		int invalid = 0; \
	} while (0)

struct audio_input {
	struct audio_convert_info conversion;
	audio_resampler_t *resampler;

	audio_output_callback_t callback;
	void *param;
};

static inline void audio_input_free(struct audio_input *input)
{
	audio_resampler_destroy(input->resampler);
}

struct audio_mix {
	DARRAY(struct audio_input) inputs;
	float buffer[MAX_AUDIO_CHANNELS][AUDIO_OUTPUT_FRAMES];
	float buffer_unclamped[MAX_AUDIO_CHANNELS][AUDIO_OUTPUT_FRAMES];
};

/* camera-box issue 1381: one audio-thread iteration, wake to the next sleep */
struct audio_stall_iter {
	uint64_t wake_ns;       /* when it woke (0 = no iteration yet) */
	uint64_t busy_ns;       /* wake -> the next sleep */
	uint64_t mix_ns;        /* input_cb (obs-audio.c audio_callback) */
	uint64_t pass_ns;       /* the output pass */
	uint64_t lock_ns;       /* its longest input-mutex wait */
	uint64_t tail_ns;       /* end of input_and_output -> the next sleep */
	uint64_t tail_start_ns; /* when input_and_output returned (0 = not yet) */
};

struct audio_output {
	struct audio_output_info info;
	size_t block_size;
	size_t channels;
	size_t planes;

	pthread_t thread;
	os_event_t *stop_event;

	bool initialized;

	audio_input_callback_t input_cb;
	void *input_param;
	pthread_mutex_t input_mutex;
	struct audio_mix mixes[MAX_AUDIO_MIXES];

	/* camera-box issue 1381: the audio thread's stall attribution (audio-io.h): the window, and
	 * the current iteration for the worst-wake breakdown */
	struct audio_output_stall_stats stall;
	struct audio_stall_iter stall_cur;
};

/* camera-box issue 1381 BEGIN -- what the audio thread spends its time on between two mixer ticks
 * (obs-audio.c's `audio-stall #1367` line measured only the gap between ticks and the mix itself).
 * Window maxima in `audio->stall`: the audio thread is their only writer, and the 60 s dump that
 * takes them (audio_output_take_stall_stats) runs on it too, so plain fields. No allocation, no
 * log and no lock of its own: one clock read per measured edge; the one lock here is the input
 * mutex upstream already takes at both call sites. */
static inline void audio_stall_note_max(uint64_t *max_ns, uint64_t start_ns, uint64_t end_ns)
{
	if (end_ns > start_ns && end_ns - start_ns > *max_ns)
		*max_ns = end_ns - start_ns;
}

static inline uint64_t audio_stall_span(uint64_t start_ns, uint64_t end_ns)
{
	return end_ns > start_ns ? end_ns - start_ns : 0;
}

/* the start of the next sleep ends the current iteration: close its tail and busy time (it is
 * read as "the iteration just before" by the wake that follows, then reset) */
static inline void audio_stall_iter_end(struct audio_output *audio, uint64_t now_ns)
{
	struct audio_stall_iter *cur = &audio->stall_cur;
	if (!cur->wake_ns)
		return;
	if (cur->tail_start_ns) {
		cur->tail_ns = audio_stall_span(cur->tail_start_ns, now_ns);
		audio_stall_note_max(&audio->stall.tail_max_ns, cur->tail_start_ns, now_ns);
	}
	cur->busy_ns = audio_stall_span(cur->wake_ns, now_ns);
}

/* how late the thread woke for the tick due at target_ns; `slept` = it really slept (the OS woke
 * it late), else the tick started behind because the previous one ran long. A new worst wake
 * keeps the previous iteration's breakdown with it. Then a new iteration starts. */
static inline void audio_stall_note_wake(struct audio_output *audio, uint64_t target_ns, bool slept, uint64_t woke_ns)
{
	if (woke_ns > target_ns && woke_ns - target_ns > audio->stall.wake_late_max_ns) {
		const struct audio_stall_iter *prev = &audio->stall_cur; /* the iteration that just ended */
		audio->stall.wake_late_max_ns = woke_ns - target_ns;
		audio->stall.late_slept = slept;
		audio->stall.late_prev_busy_ns = prev->busy_ns;
		audio->stall.late_prev_mix_ns = prev->mix_ns;
		audio->stall.late_prev_pass_ns = prev->pass_ns;
		audio->stall.late_prev_lock_ns = prev->lock_ns;
		audio->stall.late_prev_tail_ns = prev->tail_ns;
	}
	if (slept)
		audio_stall_note_max(&audio->stall.sleep_over_max_ns, target_ns, woke_ns);
	memset(&audio->stall_cur, 0, sizeof(audio->stall_cur));
	audio->stall_cur.wake_ns = woke_ns;
}

/* the input mutex, with the wait for it measured (audio_output_connect / _disconnect hold it on
 * other threads) */
static inline void audio_stall_lock_input(struct audio_output *audio)
{
	const uint64_t wait_start_ns = os_gettime_ns();
	pthread_mutex_lock(&audio->input_mutex);
	const uint64_t locked_ns = os_gettime_ns();
	audio_stall_note_max(&audio->stall.lock_wait_max_ns, wait_start_ns, locked_ns);
	audio_stall_note_max(&audio->stall_cur.lock_ns, wait_start_ns, locked_ns);
}

/* the slowest output callback of the window and who it was */
static inline void audio_stall_note_callback(struct audio_output *audio, audio_output_callback_t callback, void *param,
					     size_t mix_idx, uint64_t start_ns, uint64_t end_ns)
{
	if (end_ns > start_ns && end_ns - start_ns > audio->stall.cb_max_ns) {
		audio->stall.cb_max_ns = end_ns - start_ns;
		audio->stall.cb_max_callback = callback;
		audio->stall.cb_max_param = param;
		audio->stall.cb_max_mix = mix_idx;
	}
}
/* camera-box issue 1381 END */

/* ------------------------------------------------------------------------- */

static bool resample_audio_output(struct audio_input *input, struct audio_data *data)
{
	bool success = true;

	if (input->resampler) {
		uint8_t *output[MAX_AV_PLANES];
		uint32_t frames;
		uint64_t offset;

		memset(output, 0, sizeof(output));

		success = audio_resampler_resample(input->resampler, output, &frames, &offset,
						   (const uint8_t *const *)data->data, data->frames);

		for (size_t i = 0; i < MAX_AV_PLANES; i++)
			data->data[i] = output[i];
		data->frames = frames;
		data->timestamp -= offset;
	}

	return success;
}

static inline void do_audio_output(struct audio_output *audio, size_t mix_idx, uint64_t timestamp, uint32_t frames)
{
	struct audio_mix *mix = &audio->mixes[mix_idx];
	struct audio_data data;

	audio_stall_lock_input(audio);

	for (size_t i = mix->inputs.num; i > 0; i--) {
		struct audio_input *input = mix->inputs.array + (i - 1);

		float (*buf)[AUDIO_OUTPUT_FRAMES] = input->conversion.allow_clipping ? mix->buffer_unclamped
										     : mix->buffer;
		for (size_t i = 0; i < audio->planes; i++)
			data.data[i] = (uint8_t *)buf[i];

		data.frames = frames;
		data.timestamp = timestamp;

		if (resample_audio_output(input, &data)) {
			/* camera-box issue 1381: time each output callback; its identity is read before
			 * the call (a callback may disconnect itself under the recursive mutex) */
			const audio_output_callback_t callback = input->callback;
			void *const callback_param = input->param;
			const uint64_t callback_start_ns = os_gettime_ns();
			callback(callback_param, mix_idx, &data);
			audio_stall_note_callback(audio, callback, callback_param, mix_idx, callback_start_ns,
						  os_gettime_ns());
		}
	}

	pthread_mutex_unlock(&audio->input_mutex);
}

static inline void clamp_audio_output(struct audio_output *audio, size_t bytes)
{
	size_t float_size = bytes / sizeof(float);

	for (size_t mix_idx = 0; mix_idx < MAX_AUDIO_MIXES; mix_idx++) {
		struct audio_mix *mix = &audio->mixes[mix_idx];

		/* do not process mixing if a specific mix is inactive */
		if (!mix->inputs.num)
			continue;

		for (size_t plane = 0; plane < audio->planes; plane++) {
			float *mix_data = mix->buffer[plane];
			float *mix_end = &mix_data[float_size];
			/* Unclamped mix is copied directly. */
			memcpy(mix->buffer_unclamped[plane], mix_data, bytes);

			while (mix_data < mix_end) {
				float val = *mix_data;
				val = (val == val) ? val : 0.0f;
				val = (val > 1.0f) ? 1.0f : val;
				val = (val < -1.0f) ? -1.0f : val;
				*(mix_data++) = val;
			}
		}
	}
}

static void input_and_output(struct audio_output *audio, uint64_t audio_time, uint64_t prev_time)
{
	size_t bytes = AUDIO_OUTPUT_FRAMES * audio->block_size;
	struct audio_output_data data[MAX_AUDIO_MIXES];
	uint32_t active_mixes = 0;
	uint64_t new_ts = 0;
	bool success;

	memset(data, 0, sizeof(data));

#ifdef DEBUG_AUDIO
	blog(LOG_DEBUG, "audio_time: %llu, prev_time: %llu, bytes: %lu", audio_time, prev_time, bytes);
#endif

	/* get mixers */
	audio_stall_lock_input(audio);
	for (size_t i = 0; i < MAX_AUDIO_MIXES; i++) {
		if (audio->mixes[i].inputs.num)
			active_mixes |= (1 << i);
	}
	pthread_mutex_unlock(&audio->input_mutex);

	/* clear mix buffers */
	for (size_t mix_idx = 0; mix_idx < MAX_AUDIO_MIXES; mix_idx++) {
		struct audio_mix *mix = &audio->mixes[mix_idx];

		memset(mix->buffer, 0, sizeof(mix->buffer));

		for (size_t i = 0; i < audio->planes; i++)
			data[mix_idx].data[i] = mix->buffer[i];
	}

	/* get new audio data */
	/* camera-box issue 1381: the mix (input_cb) and then the whole output pass of this tick (clamp
	 * + every mix's outputs) are timed for the worst-wake breakdown */
	const uint64_t mix_start_ns = os_gettime_ns();
	success = audio->input_cb(audio->input_param, prev_time, audio_time, &new_ts, active_mixes, data);
	const uint64_t out_pass_start_ns = os_gettime_ns();
	audio->stall_cur.mix_ns = audio_stall_span(mix_start_ns, out_pass_start_ns);
	if (!success)
		return;

	/* clamps audio data to -1.0..1.0 */
	clamp_audio_output(audio, bytes);

	/* output */
	for (size_t i = 0; i < MAX_AUDIO_MIXES; i++)
		do_audio_output(audio, i, new_ts, AUDIO_OUTPUT_FRAMES);

	const uint64_t out_pass_end_ns = os_gettime_ns();
	audio_stall_note_max(&audio->stall.out_pass_max_ns, out_pass_start_ns, out_pass_end_ns);
	audio->stall_cur.pass_ns = audio_stall_span(out_pass_start_ns, out_pass_end_ns);
}

static void *audio_thread(void *param)
{
#ifdef _WIN32
	DWORD unused = 0;
	const HANDLE handle = AvSetMmThreadCharacteristics(L"Audio", &unused);
#endif

	struct audio_output *audio = param;
	size_t rate = audio->info.samples_per_sec;
	uint64_t samples = 0;
	uint64_t start_time = os_gettime_ns();
	uint64_t prev_time = start_time;

	os_set_thread_name("audio-io: audio thread");

	const char *audio_thread_name =
		profile_store_name(obs_get_profiler_name_store(), "audio_thread(%s)", audio->info.name);

	while (os_event_try(audio->stop_event) == EAGAIN) {
		samples += AUDIO_OUTPUT_FRAMES;
		uint64_t audio_time = start_time + audio_frames_to_ns(rate, samples);

		/* camera-box issue 1381: the previous iteration ends here; how late this one woke */
		audio_stall_iter_end(audio, os_gettime_ns());
		const bool slept = os_sleepto_ns_fast(audio_time);
		audio_stall_note_wake(audio, audio_time, slept, os_gettime_ns());

		profile_start(audio_thread_name);

		input_and_output(audio, audio_time, prev_time);
		audio->stall_cur.tail_start_ns = os_gettime_ns(); /* camera-box issue 1381 */
		prev_time = audio_time;

		profile_end(audio_thread_name);

		profile_reenable_thread();
	}

#ifdef _WIN32
	if (handle)
		AvRevertMmThreadCharacteristics(handle);
#endif

	return NULL;
}

/* ------------------------------------------------------------------------- */

static size_t audio_get_input_idx(const audio_t *audio, size_t mix_idx, audio_output_callback_t callback, void *param)
{
	const struct audio_mix *mix = &audio->mixes[mix_idx];

	for (size_t i = 0; i < mix->inputs.num; i++) {
		struct audio_input *input = mix->inputs.array + i;

		if (input->callback == callback && input->param == param)
			return i;
	}

	return DARRAY_INVALID;
}

static inline bool audio_input_init(struct audio_input *input, struct audio_output *audio)
{
	if (input->conversion.format != audio->info.format ||
	    input->conversion.samples_per_sec != audio->info.samples_per_sec ||
	    input->conversion.speakers != audio->info.speakers) {
		struct resample_info from = {.format = audio->info.format,
					     .samples_per_sec = audio->info.samples_per_sec,
					     .speakers = audio->info.speakers};

		struct resample_info to = {.format = input->conversion.format,
					   .samples_per_sec = input->conversion.samples_per_sec,
					   .speakers = input->conversion.speakers};

		input->resampler = audio_resampler_create(&to, &from);
		if (!input->resampler) {
			blog(LOG_ERROR, "audio_input_init: Failed to "
					"create resampler");
			return false;
		}
	} else {
		input->resampler = NULL;
	}

	return true;
}

bool audio_output_connect(audio_t *audio, size_t mi, const struct audio_convert_info *conversion,
			  audio_output_callback_t callback, void *param)
{
	bool success = false;

	if (!audio || mi >= MAX_AUDIO_MIXES)
		return false;

	pthread_mutex_lock(&audio->input_mutex);

	if (audio_get_input_idx(audio, mi, callback, param) == DARRAY_INVALID) {
		struct audio_mix *mix = &audio->mixes[mi];
		struct audio_input input = {
			.callback = callback,
			.param = param,
		};

		if (conversion) {
			input.conversion = *conversion;
		} else {
			input.conversion.format = audio->info.format;
			input.conversion.speakers = audio->info.speakers;
			input.conversion.samples_per_sec = audio->info.samples_per_sec;
		}

		if (input.conversion.format == AUDIO_FORMAT_UNKNOWN)
			input.conversion.format = audio->info.format;
		if (input.conversion.speakers == SPEAKERS_UNKNOWN)
			input.conversion.speakers = audio->info.speakers;
		if (input.conversion.samples_per_sec == 0)
			input.conversion.samples_per_sec = audio->info.samples_per_sec;

		success = audio_input_init(&input, audio);
		if (success)
			da_push_back(mix->inputs, &input);
	}

	pthread_mutex_unlock(&audio->input_mutex);

	return success;
}

void audio_output_disconnect(audio_t *audio, size_t mix_idx, audio_output_callback_t callback, void *param)
{
	if (!audio || mix_idx >= MAX_AUDIO_MIXES)
		return;

	pthread_mutex_lock(&audio->input_mutex);

	size_t idx = audio_get_input_idx(audio, mix_idx, callback, param);
	if (idx != DARRAY_INVALID) {
		struct audio_mix *mix = &audio->mixes[mix_idx];
		audio_input_free(mix->inputs.array + idx);
		da_erase(mix->inputs, idx);
	}

	pthread_mutex_unlock(&audio->input_mutex);
}

static inline bool valid_audio_params(const struct audio_output_info *info)
{
	return info->format && info->name && info->samples_per_sec > 0 && info->speakers > 0;
}

int audio_output_open(audio_t **audio, struct audio_output_info *info)
{
	struct audio_output *out;
	bool planar = is_audio_planar(info->format);

	if (!valid_audio_params(info))
		return AUDIO_OUTPUT_INVALIDPARAM;

	out = bzalloc(sizeof(struct audio_output));
	if (!out)
		goto fail0;

	memcpy(&out->info, info, sizeof(struct audio_output_info));
	out->channels = get_audio_channels(info->speakers);
	out->planes = planar ? out->channels : 1;
	out->input_cb = info->input_callback;
	out->input_param = info->input_param;
	out->block_size = (planar ? 1 : out->channels) * get_audio_bytes_per_channel(info->format);

	if (pthread_mutex_init_recursive(&out->input_mutex) != 0)
		goto fail0;
	if (os_event_init(&out->stop_event, OS_EVENT_TYPE_MANUAL) != 0)
		goto fail1;
	if (pthread_create(&out->thread, NULL, audio_thread, out) != 0)
		goto fail2;

	out->initialized = true;
	*audio = out;
	return AUDIO_OUTPUT_SUCCESS;

fail2:
	os_event_destroy(out->stop_event);
fail1:
	pthread_mutex_destroy(&out->input_mutex);
fail0:
	audio_output_close(out);
	return AUDIO_OUTPUT_FAIL;
}

void audio_output_close(audio_t *audio)
{
	void *thread_ret;

	if (!audio)
		return;

	if (audio->initialized) {
		os_event_signal(audio->stop_event);
		pthread_join(audio->thread, &thread_ret);
		os_event_destroy(audio->stop_event);
		pthread_mutex_destroy(&audio->input_mutex);
	}

	for (size_t mix_idx = 0; mix_idx < MAX_AUDIO_MIXES; mix_idx++) {
		struct audio_mix *mix = &audio->mixes[mix_idx];

		for (size_t i = 0; i < mix->inputs.num; i++)
			audio_input_free(mix->inputs.array + i);

		da_free(mix->inputs);
	}
	bfree(audio);
}

const struct audio_output_info *audio_output_get_info(const audio_t *audio)
{
	return audio ? &audio->info : NULL;
}

bool audio_output_active(const audio_t *audio)
{
	if (!audio)
		return false;

	for (size_t mix_idx = 0; mix_idx < MAX_AUDIO_MIXES; mix_idx++) {
		const struct audio_mix *mix = &audio->mixes[mix_idx];

		if (mix->inputs.num != 0)
			return true;
	}

	return false;
}

size_t audio_output_get_block_size(const audio_t *audio)
{
	return audio->block_size;
}

size_t audio_output_get_planes(const audio_t *audio)
{
	return audio->planes;
}

size_t audio_output_get_channels(const audio_t *audio)
{
	return audio->channels;
}

uint32_t audio_output_get_sample_rate(const audio_t *audio)
{
	return audio->info.samples_per_sec;
}

/* camera-box issue 1381: copy the stall window and start a new one -- the audio thread only (the
 * 60 s dump in obs-audio.c), see audio-io.h */
void audio_output_take_stall_stats(audio_t *audio, struct audio_output_stall_stats *stats)
{
	if (!stats)
		return;
	if (!audio) {
		memset(stats, 0, sizeof(*stats));
		return;
	}
	*stats = audio->stall;
	memset(&audio->stall, 0, sizeof(audio->stall));
}
