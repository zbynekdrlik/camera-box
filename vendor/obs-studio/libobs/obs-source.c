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
#include <math.h>
#include <stdlib.h> /* camera-box #70: getenv/strtol for OBS_GENLOCK_PRELOAD_FRAMES */
#include <time.h>   /* camera-box #136: clock_gettime/timespec for genlock_wall_now_ns */

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <windows.h> /* camera-box #136: GetSystemTimePreciseAsFileTime for genlock_wall_now_ns */
#endif

#include "media-io/format-conversion.h"
#include "media-io/video-frame.h"
#include "media-io/audio-io.h"
#include "util/threading.h"
#include "util/platform.h"
#include "util/util_uint64.h"
#include "callback/calldata.h"
#include "graphics/matrix3.h"
#include "graphics/vec3.h"

#include "obs.h"
#include "obs-internal.h"

#define get_weak(source) ((obs_weak_source_t *)source->context.control)

static bool filter_compatible(obs_source_t *source, obs_source_t *filter);

static inline bool data_valid(const struct obs_source *source, const char *f)
{
	return obs_source_valid(source, f) && source->context.data;
}

static inline bool deinterlacing_enabled(const struct obs_source *source)
{
	return source->deinterlace_mode != OBS_DEINTERLACE_MODE_DISABLE;
}

static inline bool destroying(const struct obs_source *source)
{
	return os_atomic_load_long(&source->destroying);
}

struct obs_source_info *get_source_info(const char *id)
{
	for (size_t i = 0; i < obs->source_types.num; i++) {
		struct obs_source_info *info = &obs->source_types.array[i];
		if (strcmp(info->id, id) == 0)
			return info;
	}

	return NULL;
}

struct obs_source_info *get_source_info2(const char *unversioned_id, uint32_t ver)
{
	for (size_t i = 0; i < obs->source_types.num; i++) {
		struct obs_source_info *info = &obs->source_types.array[i];
		if (strcmp(info->unversioned_id, unversioned_id) == 0 && info->version == ver)
			return info;
	}

	return NULL;
}

static const char *source_signals[] = {
	"void destroy(ptr source)",
	"void remove(ptr source)",
	"void update(ptr source)",
	"void save(ptr source)",
	"void load(ptr source)",
	"void activate(ptr source)",
	"void deactivate(ptr source)",
	"void show(ptr source)",
	"void hide(ptr source)",
	"void mute(ptr source, bool muted)",
	"void push_to_mute_changed(ptr source, bool enabled)",
	"void push_to_mute_delay(ptr source, int delay)",
	"void push_to_talk_changed(ptr source, bool enabled)",
	"void push_to_talk_delay(ptr source, int delay)",
	"void enable(ptr source, bool enabled)",
	"void rename(ptr source, string new_name, string prev_name)",
	"void volume(ptr source, in out float volume)",
	"void update_properties(ptr source)",
	"void update_flags(ptr source, int flags)",
	"void audio_sync(ptr source, int out int offset)",
	"void audio_balance(ptr source, in out float balance)",
	"void audio_mixers(ptr source, in out int mixers)",
	"void audio_monitoring(ptr source, int type)",
	"void audio_activate(ptr source)",
	"void audio_deactivate(ptr source)",
	"void filter_add(ptr source, ptr filter)",
	"void filter_remove(ptr source, ptr filter)",
	"void reorder_filters(ptr source)",
	"void transition_start(ptr source)",
	"void transition_video_stop(ptr source)",
	"void transition_stop(ptr source)",
	"void media_play(ptr source)",
	"void media_pause(ptr source)",
	"void media_restart(ptr source)",
	"void media_stopped(ptr source)",
	"void media_next(ptr source)",
	"void media_previous(ptr source)",
	"void media_started(ptr source)",
	"void media_ended(ptr source)",
	NULL,
};

bool obs_source_init_context(struct obs_source *source, obs_data_t *settings, const char *name, const char *uuid,
			     obs_data_t *hotkey_data, bool private)
{
	if (!obs_context_data_init(&source->context, OBS_OBJ_TYPE_SOURCE, settings, name, uuid, hotkey_data, private))
		return false;

	return signal_handler_add_array(source->context.signals, source_signals);
}

const char *obs_source_get_display_name(const char *id)
{
	const struct obs_source_info *info = get_source_info(id);
	return (info != NULL) ? info->get_name(info->type_data) : NULL;
}

obs_module_t *obs_source_get_module(const char *id)
{
	obs_module_t *module = obs->first_module;
	while (module) {
		for (size_t i = 0; i < module->sources.num; i++) {
			if (strcmp(module->sources.array[i], id) == 0) {
				return module;
			}
		}
		module = module->next;
	}

	module = obs->first_disabled_module;
	while (module) {
		for (size_t i = 0; i < module->sources.num; i++) {
			if (strcmp(module->sources.array[i], id) == 0) {
				return module;
			}
		}
		module = module->next;
	}

	return NULL;
}

enum obs_module_load_state obs_source_load_state(const char *id)
{
	if (!id)
		return OBS_MODULE_INVALID;

	if (obs_source_type_is_scene(id) || obs_source_type_is_group(id))
		return OBS_MODULE_ENABLED;

	obs_module_t *module = obs_source_get_module(id);
	if (!module) {
		return OBS_MODULE_MISSING;
	}
	return module->load_state;
}

static void allocate_audio_output_buffer(struct obs_source *source)
{
	size_t size = sizeof(float) * AUDIO_OUTPUT_FRAMES * MAX_AUDIO_CHANNELS * MAX_AUDIO_MIXES;
	float *ptr = bzalloc(size);

	for (size_t mix = 0; mix < MAX_AUDIO_MIXES; mix++) {
		size_t mix_pos = mix * AUDIO_OUTPUT_FRAMES * MAX_AUDIO_CHANNELS;

		for (size_t i = 0; i < MAX_AUDIO_CHANNELS; i++) {
			source->audio_output_buf[mix][i] = ptr + mix_pos + AUDIO_OUTPUT_FRAMES * i;
		}
	}
}

static void allocate_audio_mix_buffer(struct obs_source *source)
{
	size_t size = sizeof(float) * AUDIO_OUTPUT_FRAMES * MAX_AUDIO_CHANNELS;
	float *ptr = bzalloc(size);

	for (size_t i = 0; i < MAX_AUDIO_CHANNELS; i++) {
		source->audio_mix_buf[i] = ptr + AUDIO_OUTPUT_FRAMES * i;
	}
}

static inline bool is_audio_source(const struct obs_source *source)
{
	return source->info.output_flags & OBS_SOURCE_AUDIO;
}

static inline bool is_composite_source(const struct obs_source *source)
{
	return source->info.output_flags & OBS_SOURCE_COMPOSITE;
}

static inline bool requires_canvas(const struct obs_source *source)
{
	return source->info.output_flags & OBS_SOURCE_REQUIRES_CANVAS;
}

extern char *find_libobs_data_file(const char *file);

/* internal initialization */
/* camera-box #97: forward decl — the genlock preload helpers are defined further
 * down (next to the FIFO consume logic), but obs_source_init seeds the per-source
 * preload from the env default. */
static uint32_t genlock_preload_default(void);
/* camera-box #257: the per-source genlock latency FLOOR (ms). Defined here (early) so
 * obs_source_init can seed source->genlock_latency_ms to it; the canonical #define
 * GENLOCK_LATENCY_MS_MIN further down (in the latency block) carries the same value and
 * is what the setter clamp + the Rust mirror lock-step guard read. Keep them equal. */
#define GENLOCK_LATENCY_MS_MIN_INIT 3

static bool obs_source_init(struct obs_source *source)
{
	source->user_volume = 1.0f;
	source->volume = 1.0f;
	source->sync_offset = 0;
	source->balance = 0.5f;
	source->audio_active = true;
	pthread_mutex_init_value(&source->filter_mutex);
	pthread_mutex_init_value(&source->async_mutex);
	pthread_mutex_init_value(&source->audio_mutex);
	pthread_mutex_init_value(&source->audio_buf_mutex);
	pthread_mutex_init_value(&source->audio_cb_mutex);
	pthread_mutex_init_value(&source->caption_cb_mutex);
	pthread_mutex_init_value(&source->media_actions_mutex);

	if (pthread_mutex_init_recursive(&source->filter_mutex) != 0)
		return false;
	if (pthread_mutex_init(&source->audio_buf_mutex, NULL) != 0)
		return false;
	if (pthread_mutex_init(&source->audio_actions_mutex, NULL) != 0)
		return false;
	if (pthread_mutex_init(&source->audio_cb_mutex, NULL) != 0)
		return false;
	if (pthread_mutex_init(&source->audio_mutex, NULL) != 0)
		return false;
	if (pthread_mutex_init_recursive(&source->async_mutex) != 0)
		return false;
	if (pthread_mutex_init(&source->caption_cb_mutex, NULL) != 0)
		return false;
	if (pthread_mutex_init(&source->media_actions_mutex, NULL) != 0)
		return false;

	if (is_audio_source(source) || is_composite_source(source))
		allocate_audio_output_buffer(source);
	if (source->info.audio_mix)
		allocate_audio_mix_buffer(source);

	if (source->info.type == OBS_SOURCE_TYPE_TRANSITION) {
		if (!obs_transition_init(source))
			return false;
	}

	obs_context_init_control(&source->context, source, (obs_destroy_cb)obs_source_destroy);

	source->deinterlace_top_first = true;
	source->audio_mixers = 0xFF;

	/* camera-box #97/#257: seed the per-source genlock preload (internal FIFO depth) from
	 * the auto-derived default (GENLOCK_AUTO_PRELOAD_MIN) — preload is now fully internal
	 * (the per-source ms latency knob holds the delay), no OBS_GENLOCK_PRELOAD_FRAMES env. */
	source->genlock_preload = genlock_preload_default();
	/* camera-box #102: start UNfilled — the FIFO builds the preload delay line
	 * before emitting (bzalloc already zeroes this; explicit for intent). */
	source->genlock_filled = false;
	/* camera-box #126: start with no recorded empty run (bzalloc zeroes it; explicit
	 * for intent — the reconnect re-arm counts consecutive steady-state true-empties). */
	source->genlock_empty_run = 0;
	/* camera-box #245/#257: seed the per-source latency at the hard FLOOR (3 ms) — the
	 * #257 build default. No env any more; the DistroAV per-source ms field (default 3,
	 * floor 3) sets the operator value at runtime via obs_source_set_genlock_latency_ms(). */
	source->genlock_latency_ms = GENLOCK_LATENCY_MS_MIN_INIT;
	/* camera-box #257: per-source measurement-burn flag OFF at create (bzalloc zeroes it;
	 * explicit for intent). Toggled live (no restart) via obs_source_set_genlock_burn()
	 * from the DistroAV PROP_BURN field; read by the QR burn filter each render. */
	source->genlock_burn = false;

	/* camera-box #803: per-source ASRC servo OFF at create (bzalloc already zeroes
	 * asrc_enabled/asrc_last_wall_ns/asrc_has_last_wall; explicit for intent). Init the servo
	 * struct itself so its first real compensate() call starts from a clean 0ppm/no-lock
	 * state regardless of what bzalloc happened to leave (defensive — bzalloc already gives
	 * all-zero fields, which IS the correct init state here, but asrc_compensator_init() is
	 * the single source of truth other call sites — a future reset — should also use). */
	asrc_compensator_init(&source->asrc);

	source->private_settings = obs_data_create();
	return true;
}

static void obs_source_init_finalize(struct obs_source *source, obs_canvas_t *canvas)
{
	if (is_audio_source(source)) {
		pthread_mutex_lock(&obs->data.audio_sources_mutex);

		source->next_audio_source = obs->data.first_audio_source;
		source->prev_next_audio_source = &obs->data.first_audio_source;
		if (obs->data.first_audio_source)
			obs->data.first_audio_source->prev_next_audio_source = &source->next_audio_source;
		obs->data.first_audio_source = source;

		pthread_mutex_unlock(&obs->data.audio_sources_mutex);
	}

	if (!source->context.private) {
		if (requires_canvas(source)) {
			obs_canvas_insert_source(canvas, source);
		} else {
			obs_context_data_insert_name(&source->context, &obs->data.sources_mutex,
						     &obs->data.public_sources);
		}
	}
	obs_context_data_insert_uuid(&source->context, &obs->data.sources_mutex, &obs->data.sources);
}

static bool obs_source_hotkey_mute(void *data, obs_hotkey_pair_id id, obs_hotkey_t *key, bool pressed)
{
	UNUSED_PARAMETER(id);
	UNUSED_PARAMETER(key);

	struct obs_source *source = data;

	if (!pressed || obs_source_muted(source))
		return false;

	obs_source_set_muted(source, true);
	return true;
}

static bool obs_source_hotkey_unmute(void *data, obs_hotkey_pair_id id, obs_hotkey_t *key, bool pressed)
{
	UNUSED_PARAMETER(id);
	UNUSED_PARAMETER(key);

	struct obs_source *source = data;

	if (!pressed || !obs_source_muted(source))
		return false;

	obs_source_set_muted(source, false);
	return true;
}

static void obs_source_hotkey_push_to_mute(void *data, obs_hotkey_id id, obs_hotkey_t *key, bool pressed)
{
	struct audio_action action = {.timestamp = os_gettime_ns(), .type = AUDIO_ACTION_PTM, .set = pressed};

	UNUSED_PARAMETER(id);
	UNUSED_PARAMETER(key);

	struct obs_source *source = data;

	pthread_mutex_lock(&source->audio_actions_mutex);
	da_push_back(source->audio_actions, &action);
	pthread_mutex_unlock(&source->audio_actions_mutex);

	source->user_push_to_mute_pressed = pressed;
}

static void obs_source_hotkey_push_to_talk(void *data, obs_hotkey_id id, obs_hotkey_t *key, bool pressed)
{
	struct audio_action action = {.timestamp = os_gettime_ns(), .type = AUDIO_ACTION_PTT, .set = pressed};

	UNUSED_PARAMETER(id);
	UNUSED_PARAMETER(key);

	struct obs_source *source = data;

	pthread_mutex_lock(&source->audio_actions_mutex);
	da_push_back(source->audio_actions, &action);
	pthread_mutex_unlock(&source->audio_actions_mutex);

	source->user_push_to_talk_pressed = pressed;
}

static void obs_source_init_audio_hotkeys(struct obs_source *source)
{
	if (!(source->info.output_flags & OBS_SOURCE_AUDIO) || source->info.type != OBS_SOURCE_TYPE_INPUT) {
		source->mute_unmute_key = OBS_INVALID_HOTKEY_ID;
		source->push_to_talk_key = OBS_INVALID_HOTKEY_ID;
		return;
	}

	source->mute_unmute_key = obs_hotkey_pair_register_source(source, "libobs.mute", obs->hotkeys.mute,
								  "libobs.unmute", obs->hotkeys.unmute,
								  obs_source_hotkey_mute, obs_source_hotkey_unmute,
								  source, source);

	source->push_to_mute_key = obs_hotkey_register_source(source, "libobs.push-to-mute", obs->hotkeys.push_to_mute,
							      obs_source_hotkey_push_to_mute, source);

	source->push_to_talk_key = obs_hotkey_register_source(source, "libobs.push-to-talk", obs->hotkeys.push_to_talk,
							      obs_source_hotkey_push_to_talk, source);
}

void obs_source_audio_output_capture_device_activated(void *vptr, calldata_t *cd)
{
	UNUSED_PARAMETER(vptr);
	obs_source_t *src = calldata_ptr(cd, "source");
	if (!src)
		return;

	obs_data_t *settings = obs_source_get_settings(src);
	const char *device_id = obs_data_get_string(settings, "device_id");
	obs_source_audio_output_capture_device_changed(src, device_id);
	obs_data_release(settings);
}

extern bool devices_match(const char *id1, const char *id2);
void obs_source_audio_output_capture_device_changed(obs_source_t *src, const char *device_id)
{
	struct obs_core_audio *audio = &obs->audio;

	if (!audio->monitoring_device_name)
		return;

	if (!(src->info.output_flags & OBS_SOURCE_DO_NOT_SELF_MONITOR))
		return;

	const char *mon_id = audio->monitoring_device_id;
	bool id_match = false;

#ifdef __APPLE__
	extern void get_desktop_default_id(char **p_id);
	if (device_id && strcmp(device_id, "default") == 0) {
		char *def_id = NULL;
		get_desktop_default_id(&def_id);
		id_match = devices_match(def_id, mon_id);
		if (def_id)
			bfree(def_id);
	} else {
		id_match = devices_match(device_id, mon_id);
	}
#else
	id_match = devices_match(device_id, mon_id);
#endif
	struct calldata cd;
	uint8_t stack[128];
	calldata_init_fixed(&cd, stack, sizeof(stack));

	if (id_match) {
		calldata_set_ptr(&cd, "source", src);
		signal_handler_signal(obs->signals, "deduplication_changed", &cd);
		signal_handler_connect(src->context.signals, "activate",
				       obs_source_audio_output_capture_device_activated, NULL);
		blog(LOG_INFO,
		     "Device for 'Audio Output Capture' source %s is also used for audio monitoring."
		     "\nDeduplication logic is being applied to all monitored sources.",
		     src->context.name);
	} else {
		if (src == audio->monitoring_duplicating_source) {
			calldata_set_ptr(&cd, "source", NULL);
			signal_handler_disconnect(src->context.signals, "activate",
						  obs_source_audio_output_capture_device_activated, NULL);
			signal_handler_signal(obs->signals, "deduplication_changed", &cd);
			blog(LOG_INFO, "Deduplication logic stopped.");
		}
	}
}

static obs_source_t *obs_source_create_internal(const char *id, const char *name, const char *uuid,
						obs_data_t *settings, obs_data_t *hotkey_data, bool private,
						uint32_t last_obs_ver, obs_canvas_t *canvas)
{
	struct obs_source *source = bzalloc(sizeof(struct obs_source));

	const struct obs_source_info *info = get_source_info(id);
	if (!info) {
		blog(LOG_ERROR, "Source ID '%s' not found", id);

		source->info.id = bstrdup(id);
		source->owns_info_id = true;
		source->info.unversioned_id = bstrdup(source->info.id);
	} else {
		source->info = *info;

		/* Always mark filters as private so they aren't found by
		 * source enum/search functions.
		 *
		 * XXX: Fix design flaws with filters */
		if (info->type == OBS_SOURCE_TYPE_FILTER)
			private = true;
	}

	source->mute_unmute_key = OBS_INVALID_HOTKEY_PAIR_ID;
	source->push_to_mute_key = OBS_INVALID_HOTKEY_ID;
	source->push_to_talk_key = OBS_INVALID_HOTKEY_ID;
	source->last_obs_ver = last_obs_ver;

	if (!obs_source_init_context(source, settings, name, uuid, hotkey_data, private))
		goto fail;

	if (info) {
		if (info->get_defaults) {
			info->get_defaults(source->context.settings);
		}
		if (info->get_defaults2) {
			info->get_defaults2(info->type_data, source->context.settings);
		}
	}

	if (!obs_source_init(source))
		goto fail;

	/* Scenes need canvases, fall back to using default canvas if none provided here. */
	if (requires_canvas(source) && !canvas) {
		blog(LOG_WARNING, "Attempted to add Scene without specifying a canvas! Using default canvas instead.");
		canvas = obs->data.main_canvas;
	}

	if (!private)
		obs_source_init_audio_hotkeys(source);

	/* allow the source to be created even if creation fails so that the
	 * user's data doesn't become lost */
	if (info && info->create)
		source->context.data = info->create(source->context.settings, source);
	if ((!info || info->create) && !source->context.data)
		blog(LOG_ERROR, "Failed to create source '%s'!", name);

	blog(LOG_DEBUG, "%ssource '%s' (%s) created", private ? "private " : "", name, id);

	source->flags = source->default_flags;
	source->enabled = true;

	/* camera-box #912: ASRC (per-source clock-drift compensation, issue 803) is a BUILD
	 * DEFAULT, mirroring issue 257's render-tick/ts-align hard-lock -- no env, no per-source
	 * opt-in required, so the servo can never ship silently inert again (nothing in the
	 * vendored tree ever called obs_source_set_asrc_enabled(), which is exactly the
	 * "forgettable command-line tweak" failure mode issue 912 exists to kill). The setter
	 * stays live as an optional override path (parity with obs_source_set_genlock_burn under
	 * the issue-257 FIFO default) -- it is just never the way ASRC gets turned ON. */
	source->asrc_enabled = true;

	/* audio deduplication initialization */
	source->audio_is_duplicated = false;

	obs_source_init_finalize(source, canvas);
	if (!private) {
		if (canvas)
			obs_source_dosignal_canvas(source, canvas, "source_create_canvas", NULL);
		if (!canvas || canvas == obs->data.main_canvas)
			obs_source_dosignal(source, "source_create", NULL);
	}

	return source;

fail:
	blog(LOG_ERROR, "obs_source_create failed");
	obs_source_destroy(source);
	return NULL;
}

obs_source_t *obs_source_create(const char *id, const char *name, obs_data_t *settings, obs_data_t *hotkey_data)
{
	return obs_source_create_internal(id, name, NULL, settings, hotkey_data, false, LIBOBS_API_VER, NULL);
}

obs_source_t *obs_source_create_private(const char *id, const char *name, obs_data_t *settings)
{
	return obs_source_create_internal(id, name, NULL, settings, NULL, true, LIBOBS_API_VER, NULL);
}

obs_source_t *obs_source_create_canvas(obs_canvas_t *canvas, const char *id, const char *name, obs_data_t *settings,
				       obs_data_t *hotkey_data)
{
	return obs_source_create_internal(id, name, NULL, settings, hotkey_data, false, LIBOBS_API_VER, canvas);
}

obs_source_t *obs_source_create_set_last_ver(obs_canvas_t *canvas, const char *id, const char *name, const char *uuid,
					     obs_data_t *settings, obs_data_t *hotkey_data, uint32_t last_obs_ver,
					     bool is_private)
{
	return obs_source_create_internal(id, name, uuid, settings, hotkey_data, is_private, last_obs_ver, canvas);
}

static char *get_new_filter_name(obs_source_t *dst, const char *name)
{
	struct dstr new_name = {0};
	int inc = 0;

	dstr_copy(&new_name, name);

	for (;;) {
		obs_source_t *existing_filter = obs_source_get_filter_by_name(dst, new_name.array);
		if (!existing_filter)
			break;

		obs_source_release(existing_filter);

		dstr_printf(&new_name, "%s %d", name, ++inc + 1);
	}

	return new_name.array;
}

static void duplicate_filters(obs_source_t *dst, obs_source_t *src, bool private)
{
	DARRAY(obs_source_t *) filters;

	da_init(filters);

	pthread_mutex_lock(&src->filter_mutex);
	da_reserve(filters, src->filters.num);
	for (size_t i = 0; i < src->filters.num; i++) {
		obs_source_t *s = obs_source_get_ref(src->filters.array[i]);
		if (s)
			da_push_back(filters, &s);
	}
	pthread_mutex_unlock(&src->filter_mutex);

	for (size_t i = filters.num; i > 0; i--) {
		obs_source_t *src_filter = filters.array[i - 1];
		char *new_name = get_new_filter_name(dst, src_filter->context.name);
		bool enabled = obs_source_enabled(src_filter);

		obs_source_t *dst_filter = obs_source_duplicate(src_filter, new_name, private);
		obs_source_set_enabled(dst_filter, enabled);

		bfree(new_name);
		obs_source_filter_add(dst, dst_filter);
		obs_source_release(dst_filter);
		obs_source_release(src_filter);
	}

	da_free(filters);
}

void obs_source_copy_filters(obs_source_t *dst, obs_source_t *src)
{
	if (!obs_source_valid(dst, "obs_source_copy_filters"))
		return;
	if (!obs_source_valid(src, "obs_source_copy_filters"))
		return;

	duplicate_filters(dst, src, dst->context.private);
}

static void duplicate_filter(obs_source_t *dst, obs_source_t *filter)
{
	if (!filter_compatible(dst, filter))
		return;

	char *new_name = get_new_filter_name(dst, filter->context.name);
	bool enabled = obs_source_enabled(filter);

	obs_source_t *dst_filter = obs_source_duplicate(filter, new_name, true);
	obs_source_set_enabled(dst_filter, enabled);

	bfree(new_name);
	obs_source_filter_add(dst, dst_filter);
	obs_source_release(dst_filter);
}

void obs_source_copy_single_filter(obs_source_t *dst, obs_source_t *filter)
{
	if (!obs_source_valid(dst, "obs_source_copy_single_filter"))
		return;
	if (!obs_source_valid(filter, "obs_source_copy_single_filter"))
		return;

	duplicate_filter(dst, filter);
}

obs_source_t *obs_source_duplicate(obs_source_t *source, const char *new_name, bool create_private)
{
	obs_source_t *new_source;
	obs_data_t *settings;

	if (!obs_source_valid(source, "obs_source_duplicate"))
		return NULL;

	if (source->info.type == OBS_SOURCE_TYPE_SCENE) {
		obs_scene_t *scene = obs_scene_from_source(source);
		if (scene && !create_private) {
			return obs_source_get_ref(source);
		}
		if (!scene)
			scene = obs_group_from_source(source);
		if (!scene)
			return NULL;

		obs_scene_t *new_scene = obs_scene_duplicate(
			scene, new_name, create_private ? OBS_SCENE_DUP_PRIVATE_COPY : OBS_SCENE_DUP_COPY);
		obs_source_t *new_source = obs_scene_get_source(new_scene);
		return new_source;
	}

	if ((source->info.output_flags & OBS_SOURCE_DO_NOT_DUPLICATE) != 0) {
		return obs_source_get_ref(source);
	}

	settings = obs_data_create();
	obs_data_apply(settings, source->context.settings);

	new_source = create_private ? obs_source_create_private(source->info.id, new_name, settings)
				    : obs_source_create(source->info.id, new_name, settings, NULL);

	new_source->audio_mixers = source->audio_mixers;
	new_source->sync_offset = source->sync_offset;
	new_source->user_volume = source->user_volume;
	new_source->user_muted = source->user_muted;
	new_source->volume = source->volume;
	new_source->muted = source->muted;
	new_source->flags = source->flags;

	obs_data_apply(new_source->private_settings, source->private_settings);

	if (source->info.type != OBS_SOURCE_TYPE_FILTER)
		duplicate_filters(new_source, source, create_private);

	obs_data_release(settings);
	return new_source;
}

void obs_source_frame_init(struct obs_source_frame *frame, enum video_format format, uint32_t width, uint32_t height)
{
	struct video_frame vid_frame;

	if (!obs_ptr_valid(frame, "obs_source_frame_init"))
		return;

	video_frame_init(&vid_frame, format, width, height);
	frame->format = format;
	frame->width = width;
	frame->height = height;

	for (size_t i = 0; i < MAX_AV_PLANES; i++) {
		frame->data[i] = vid_frame.data[i];
		frame->linesize[i] = vid_frame.linesize[i];
	}
}

static inline void obs_source_frame_decref(struct obs_source_frame *frame)
{
	if (os_atomic_dec_long(&frame->refs) == 0)
		obs_source_frame_destroy(frame);
}

static bool obs_source_filter_remove_refless(obs_source_t *source, obs_source_t *filter);
static void obs_source_destroy_defer(struct obs_source *source);

void obs_source_destroy(struct obs_source *source)
{
	if (!obs_source_valid(source, "obs_source_destroy"))
		return;

	if (os_atomic_set_long(&source->destroying, true) == true) {
		blog(LOG_ERROR, "Double destroy just occurred. "
				"Something called addref on a source "
				"after it was already fully released, "
				"I guess.");
		return;
	}

	if (is_audio_source(source)) {
		pthread_mutex_lock(&source->audio_cb_mutex);
		da_free(source->audio_cb_list);
		pthread_mutex_unlock(&source->audio_cb_mutex);
	}

	pthread_mutex_lock(&source->caption_cb_mutex);
	da_free(source->caption_cb_list);
	pthread_mutex_unlock(&source->caption_cb_mutex);

	if (source->info.type == OBS_SOURCE_TYPE_TRANSITION)
		obs_transition_clear(source);

	pthread_mutex_lock(&obs->data.audio_sources_mutex);
	if (source->prev_next_audio_source) {
		*source->prev_next_audio_source = source->next_audio_source;
		if (source->next_audio_source)
			source->next_audio_source->prev_next_audio_source = source->prev_next_audio_source;
	}
	pthread_mutex_unlock(&obs->data.audio_sources_mutex);

	if (source->filter_parent)
		obs_source_filter_remove_refless(source->filter_parent, source);

	while (source->filters.num)
		obs_source_filter_remove(source, source->filters.array[0]);

	obs_context_data_remove_uuid(&source->context, &obs->data.sources_mutex, &obs->data.sources);
	if (!source->context.private) {
		if (requires_canvas(source)) {
			obs_canvas_remove_source(source);
		} else {
			obs_context_data_remove_name(&source->context, &obs->data.sources_mutex,
						     &obs->data.public_sources);
		}
	}

	source_profiler_remove_source(source);

	/* defer source destroy */
	os_task_queue_queue_task(obs->destruction_task_thread, (os_task_t)obs_source_destroy_defer, source);
}

static void obs_source_destroy_defer(struct obs_source *source)
{
	size_t i;

	/* prevents the destruction of sources if destroy triggered inside of
	 * a video tick call */
	obs_context_wait(&source->context);

	obs_source_dosignal(source, "source_destroy", "destroy");

	if (source->context.data) {
		source->info.destroy(source->context.data);
		source->context.data = NULL;
	}

	blog(LOG_DEBUG, "%ssource '%s' destroyed", source->context.private ? "private " : "", source->context.name);

	audio_monitor_destroy(source->monitor);

	obs_hotkey_unregister(source->push_to_talk_key);
	obs_hotkey_unregister(source->push_to_mute_key);
	obs_hotkey_pair_unregister(source->mute_unmute_key);

	for (i = 0; i < source->async_cache.num; i++)
		obs_source_frame_decref(source->async_cache.array[i].frame);

	gs_enter_context(obs->video.graphics);
	if (source->async_texrender)
		gs_texrender_destroy(source->async_texrender);
	if (source->async_prev_texrender)
		gs_texrender_destroy(source->async_prev_texrender);
	for (size_t c = 0; c < MAX_AV_PLANES; c++) {
		gs_texture_destroy(source->async_textures[c]);
		gs_texture_destroy(source->async_prev_textures[c]);
	}
	if (source->filter_texrender)
		gs_texrender_destroy(source->filter_texrender);
	if (source->color_space_texrender)
		gs_texrender_destroy(source->color_space_texrender);
	gs_leave_context();

	for (i = 0; i < MAX_AV_PLANES; i++)
		bfree(source->audio_data.data[i]);
	for (i = 0; i < MAX_AUDIO_CHANNELS; i++)
		deque_free(&source->audio_input_buf[i]);
	audio_resampler_destroy(source->resampler);
	bfree(source->audio_output_buf[0][0]);
	bfree(source->audio_mix_buf[0]);

	obs_source_frame_destroy(source->async_preload_frame);

	if (source->info.type == OBS_SOURCE_TYPE_TRANSITION)
		obs_transition_free(source);

	da_free(source->audio_actions);
	da_free(source->audio_cb_list);
	da_free(source->caption_cb_list);
	da_free(source->async_cache);
	da_free(source->async_frames);
	da_free(source->filters);
	da_free(source->media_actions);
	pthread_mutex_destroy(&source->filter_mutex);
	pthread_mutex_destroy(&source->audio_actions_mutex);
	pthread_mutex_destroy(&source->audio_buf_mutex);
	pthread_mutex_destroy(&source->audio_cb_mutex);
	pthread_mutex_destroy(&source->audio_mutex);
	pthread_mutex_destroy(&source->caption_cb_mutex);
	pthread_mutex_destroy(&source->async_mutex);
	pthread_mutex_destroy(&source->media_actions_mutex);
	obs_data_release(source->private_settings);
	obs_context_data_free(&source->context);

	if (source->owns_info_id) {
		bfree((void *)source->info.id);
		bfree((void *)source->info.unversioned_id);
	}

	bfree(source);
}

void obs_source_addref(obs_source_t *source)
{
	if (!source)
		return;

	obs_ref_addref(&source->context.control->ref);
}

void obs_source_release(obs_source_t *source)
{
	if (!obs && source) {
		blog(LOG_WARNING, "Tried to release a source when the OBS "
				  "core is shut down!");
		return;
	}

	if (!source)
		return;

	obs_weak_source_t *control = get_weak(source);
	if (obs_ref_release(&control->ref)) {
		obs_source_destroy(source);
		obs_weak_source_release(control);
	}
}

void obs_weak_source_addref(obs_weak_source_t *weak)
{
	if (!weak)
		return;

	obs_weak_ref_addref(&weak->ref);
}

void obs_weak_source_release(obs_weak_source_t *weak)
{
	if (!weak)
		return;

	if (obs_weak_ref_release(&weak->ref))
		bfree(weak);
}

obs_source_t *obs_source_get_ref(obs_source_t *source)
{
	if (!source)
		return NULL;

	return obs_weak_source_get_source(get_weak(source));
}

obs_weak_source_t *obs_source_get_weak_source(obs_source_t *source)
{
	if (!source)
		return NULL;

	obs_weak_source_t *weak = get_weak(source);
	obs_weak_source_addref(weak);
	return weak;
}

obs_source_t *obs_weak_source_get_source(obs_weak_source_t *weak)
{
	if (!weak)
		return NULL;

	if (obs_weak_ref_get_ref(&weak->ref))
		return weak->source;

	return NULL;
}

bool obs_weak_source_expired(obs_weak_source_t *weak)
{
	return weak ? obs_weak_ref_expired(&weak->ref) : true;
}

bool obs_weak_source_references_source(obs_weak_source_t *weak, obs_source_t *source)
{
	return weak && source && weak->source == source;
}

void obs_source_remove(obs_source_t *source)
{
	if (!obs_source_valid(source, "obs_source_remove"))
		return;

	if (!source->removed) {
		obs_source_t *s = obs_source_get_ref(source);
		if (s) {
			s->removed = true;
			obs_source_dosignal(s, "source_remove", "remove");
			/* Remove from canvas if there is one. */
			if (source->canvas)
				obs_canvas_remove_source(s);

			obs_source_release(s);
		}
	}
}

bool obs_source_removed(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_removed") ? source->removed : true;
}

static inline obs_data_t *get_defaults(const struct obs_source_info *info)
{
	obs_data_t *settings = obs_data_create();
	if (info->get_defaults2)
		info->get_defaults2(info->type_data, settings);
	else if (info->get_defaults)
		info->get_defaults(settings);
	return settings;
}

obs_data_t *obs_source_settings(const char *id)
{
	const struct obs_source_info *info = get_source_info(id);
	return (info) ? get_defaults(info) : NULL;
}

obs_data_t *obs_get_source_defaults(const char *id)
{
	const struct obs_source_info *info = get_source_info(id);
	return info ? get_defaults(info) : NULL;
}

obs_properties_t *obs_get_source_properties(const char *id)
{
	const struct obs_source_info *info = get_source_info(id);
	if (info && (info->get_properties || info->get_properties2)) {
		obs_data_t *defaults = get_defaults(info);
		obs_properties_t *props;

		if (info->get_properties2)
			props = info->get_properties2(NULL, info->type_data);
		else
			props = info->get_properties(NULL);

		obs_properties_apply_settings(props, defaults);
		obs_data_release(defaults);
		return props;
	}
	return NULL;
}

obs_missing_files_t *obs_source_get_missing_files(const obs_source_t *source)
{
	if (!data_valid(source, "obs_source_get_missing_files"))
		return obs_missing_files_create();

	if (source->info.missing_files) {
		return source->info.missing_files(source->context.data);
	}

	return obs_missing_files_create();
}

void obs_source_replace_missing_file(obs_missing_file_cb cb, obs_source_t *source, const char *new_path, void *data)
{
	if (!data_valid(source, "obs_source_replace_missing_file"))
		return;

	cb(source->context.data, new_path, data);
}

bool obs_is_source_configurable(const char *id)
{
	const struct obs_source_info *info = get_source_info(id);
	return info && (info->get_properties || info->get_properties2);
}

bool obs_source_configurable(const obs_source_t *source)
{
	return data_valid(source, "obs_source_configurable") &&
	       (source->info.get_properties || source->info.get_properties2);
}

obs_properties_t *obs_source_properties(const obs_source_t *source)
{
	if (!data_valid(source, "obs_source_properties"))
		return NULL;

	if (source->info.get_properties2) {
		obs_properties_t *props;
		props = source->info.get_properties2(source->context.data, source->info.type_data);
		obs_properties_apply_settings(props, source->context.settings);
		return props;

	} else if (source->info.get_properties) {
		obs_properties_t *props;
		props = source->info.get_properties(source->context.data);
		obs_properties_apply_settings(props, source->context.settings);
		return props;
	}

	return NULL;
}

uint32_t obs_source_get_output_flags(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_get_output_flags") ? source->info.output_flags : 0;
}

uint32_t obs_get_source_output_flags(const char *id)
{
	const struct obs_source_info *info = get_source_info(id);
	return info ? info->output_flags : 0;
}

static void obs_source_deferred_update(obs_source_t *source)
{
	if (source->context.data && source->info.update) {
		long count = os_atomic_load_long(&source->defer_update_count);
		source->info.update(source->context.data, source->context.settings);
		os_atomic_compare_swap_long(&source->defer_update_count, count, 0);
		obs_source_dosignal(source, "source_update", "update");
	}
}

void obs_source_update(obs_source_t *source, obs_data_t *settings)
{
	if (!obs_source_valid(source, "obs_source_update"))
		return;

	if (settings) {
		obs_data_apply(source->context.settings, settings);
	}

	if (source->info.output_flags & OBS_SOURCE_VIDEO) {
		os_atomic_inc_long(&source->defer_update_count);
	} else if (source->context.data && source->info.update) {
		source->info.update(source->context.data, source->context.settings);
		obs_source_dosignal(source, "source_update", "update");
	}
}

void obs_source_reset_settings(obs_source_t *source, obs_data_t *settings)
{
	if (!obs_source_valid(source, "obs_source_reset_settings"))
		return;

	obs_data_clear(source->context.settings);
	obs_source_update(source, settings);
}

void obs_source_update_properties(obs_source_t *source)
{
	if (!obs_source_valid(source, "obs_source_update_properties"))
		return;

	obs_source_dosignal(source, NULL, "update_properties");
}

void obs_source_send_mouse_click(obs_source_t *source, const struct obs_mouse_event *event, int32_t type, bool mouse_up,
				 uint32_t click_count)
{
	if (!obs_source_valid(source, "obs_source_send_mouse_click"))
		return;

	if (source->info.output_flags & OBS_SOURCE_INTERACTION) {
		if (source->info.mouse_click) {
			source->info.mouse_click(source->context.data, event, type, mouse_up, click_count);
		}
	}
}

void obs_source_send_mouse_move(obs_source_t *source, const struct obs_mouse_event *event, bool mouse_leave)
{
	if (!obs_source_valid(source, "obs_source_send_mouse_move"))
		return;

	if (source->info.output_flags & OBS_SOURCE_INTERACTION) {
		if (source->info.mouse_move) {
			source->info.mouse_move(source->context.data, event, mouse_leave);
		}
	}
}

void obs_source_send_mouse_wheel(obs_source_t *source, const struct obs_mouse_event *event, int x_delta, int y_delta)
{
	if (!obs_source_valid(source, "obs_source_send_mouse_wheel"))
		return;

	if (source->info.output_flags & OBS_SOURCE_INTERACTION) {
		if (source->info.mouse_wheel) {
			source->info.mouse_wheel(source->context.data, event, x_delta, y_delta);
		}
	}
}

void obs_source_send_focus(obs_source_t *source, bool focus)
{
	if (!obs_source_valid(source, "obs_source_send_focus"))
		return;

	if (source->info.output_flags & OBS_SOURCE_INTERACTION) {
		if (source->info.focus) {
			source->info.focus(source->context.data, focus);
		}
	}
}

void obs_source_send_key_click(obs_source_t *source, const struct obs_key_event *event, bool key_up)
{
	if (!obs_source_valid(source, "obs_source_send_key_click"))
		return;

	if (source->info.output_flags & OBS_SOURCE_INTERACTION) {
		if (source->info.key_click) {
			source->info.key_click(source->context.data, event, key_up);
		}
	}
}

bool obs_source_get_texcoords_centered(obs_source_t *source)
{
	return source->texcoords_centered;
}

void obs_source_set_texcoords_centered(obs_source_t *source, bool centered)
{
	source->texcoords_centered = centered;
}

static void activate_source(obs_source_t *source)
{
	if (source->context.data && source->info.activate)
		source->info.activate(source->context.data);
	obs_source_dosignal(source, "source_activate", "activate");
}

static void deactivate_source(obs_source_t *source)
{
	if (source->context.data && source->info.deactivate)
		source->info.deactivate(source->context.data);
	obs_source_dosignal(source, "source_deactivate", "deactivate");
}

static void show_source(obs_source_t *source)
{
	if (source->context.data && source->info.show)
		source->info.show(source->context.data);
	obs_source_dosignal(source, "source_show", "show");
}

static void hide_source(obs_source_t *source)
{
	if (source->context.data && source->info.hide)
		source->info.hide(source->context.data);
	obs_source_dosignal(source, "source_hide", "hide");
}

static void activate_tree(obs_source_t *parent, obs_source_t *child, void *param)
{
	os_atomic_inc_long(&child->activate_refs);

	UNUSED_PARAMETER(parent);
	UNUSED_PARAMETER(param);
}

static void deactivate_tree(obs_source_t *parent, obs_source_t *child, void *param)
{
	os_atomic_dec_long(&child->activate_refs);

	UNUSED_PARAMETER(parent);
	UNUSED_PARAMETER(param);
}

static void show_tree(obs_source_t *parent, obs_source_t *child, void *param)
{
	os_atomic_inc_long(&child->show_refs);

	UNUSED_PARAMETER(parent);
	UNUSED_PARAMETER(param);
}

static void hide_tree(obs_source_t *parent, obs_source_t *child, void *param)
{
	os_atomic_dec_long(&child->show_refs);

	UNUSED_PARAMETER(parent);
	UNUSED_PARAMETER(param);
}

void obs_source_activate(obs_source_t *source, enum view_type type)
{
	if (!obs_source_valid(source, "obs_source_activate"))
		return;

	os_atomic_inc_long(&source->show_refs);
	obs_source_enum_active_tree(source, show_tree, NULL);

	if (type == MAIN_VIEW) {
		os_atomic_inc_long(&source->activate_refs);
		obs_source_enum_active_tree(source, activate_tree, NULL);
	}
}

void obs_source_deactivate(obs_source_t *source, enum view_type type)
{
	if (!obs_source_valid(source, "obs_source_deactivate"))
		return;

	if (os_atomic_load_long(&source->show_refs) > 0) {
		os_atomic_dec_long(&source->show_refs);
		obs_source_enum_active_tree(source, hide_tree, NULL);
	}

	if (type == MAIN_VIEW) {
		if (os_atomic_load_long(&source->activate_refs) > 0) {
			os_atomic_dec_long(&source->activate_refs);
			obs_source_enum_active_tree(source, deactivate_tree, NULL);
		}
	}
}

static inline struct obs_source_frame *get_closest_frame(obs_source_t *source, uint64_t sys_time);

static void filter_frame(obs_source_t *source, struct obs_source_frame **ref_frame)
{
	struct obs_source_frame *frame = *ref_frame;
	if (frame) {
		os_atomic_inc_long(&frame->refs);
		frame = filter_async_video(source, frame);
		if (frame)
			os_atomic_dec_long(&frame->refs);
	}

	*ref_frame = frame;
}

void process_media_actions(obs_source_t *source)
{
	struct media_action action = {0};

	for (;;) {
		pthread_mutex_lock(&source->media_actions_mutex);
		if (source->media_actions.num) {
			action = source->media_actions.array[0];
			da_pop_front(source->media_actions);
		} else {
			action.type = MEDIA_ACTION_NONE;
		}
		pthread_mutex_unlock(&source->media_actions_mutex);

		switch (action.type) {
		case MEDIA_ACTION_NONE:
			return;
		case MEDIA_ACTION_PLAY_PAUSE:
			source->info.media_play_pause(source->context.data, action.pause);

			if (action.pause)
				obs_source_dosignal(source, NULL, "media_pause");
			else
				obs_source_dosignal(source, NULL, "media_play");
			break;

		case MEDIA_ACTION_RESTART:
			source->info.media_restart(source->context.data);
			obs_source_dosignal(source, NULL, "media_restart");
			break;

		case MEDIA_ACTION_STOP:
			source->info.media_stop(source->context.data);
			obs_source_dosignal(source, NULL, "media_stopped");
			break;
		case MEDIA_ACTION_NEXT:
			source->info.media_next(source->context.data);
			obs_source_dosignal(source, NULL, "media_next");
			break;
		case MEDIA_ACTION_PREVIOUS:
			source->info.media_previous(source->context.data);
			obs_source_dosignal(source, NULL, "media_previous");
			break;
		case MEDIA_ACTION_SET_TIME:
			source->info.media_set_time(source->context.data, action.ms);
			break;
		}
	}
}

static void async_tick(obs_source_t *source)
{
	uint64_t sys_time = obs->video.video_time;

	pthread_mutex_lock(&source->async_mutex);

	if (deinterlacing_enabled(source)) {
		/* camera-box #70: deinterlaced sources take this path and never reach
		 * get_closest_frame, so they get neither the genlock preload gate nor
		 * the audit counters. The genlock NDI camera sources are progressive
		 * (deinterlacing off), so this is a non-issue in the production rig; if
		 * a genlock source ever needs deinterlacing the gate must be added here
		 * too. */
		deinterlace_process_last_frame(source, sys_time);
	} else {
		if (source->cur_async_frame) {
			remove_async_frame(source, source->cur_async_frame);
			source->cur_async_frame = NULL;
		}

		source->cur_async_frame = get_closest_frame(source, sys_time);
	}

	source->last_sys_timestamp = sys_time;

	if (deinterlacing_enabled(source))
		filter_frame(source, &source->prev_async_frame);
	filter_frame(source, &source->cur_async_frame);

	if (source->cur_async_frame)
		source->async_update_texture = set_async_texture_size(source, source->cur_async_frame);

	pthread_mutex_unlock(&source->async_mutex);
}

void obs_source_video_tick(obs_source_t *source, float seconds)
{
	bool now_showing, now_active;

	if (!obs_source_valid(source, "obs_source_video_tick"))
		return;

	if (source->info.type == OBS_SOURCE_TYPE_TRANSITION)
		obs_transition_tick(source, seconds);

	if ((source->info.output_flags & OBS_SOURCE_ASYNC) != 0)
		async_tick(source);

	if ((source->info.output_flags & OBS_SOURCE_CONTROLLABLE_MEDIA) != 0)
		process_media_actions(source);

	if (os_atomic_load_long(&source->defer_update_count) > 0)
		obs_source_deferred_update(source);

	/* reset the filter render texture information once every frame */
	if (source->filter_texrender)
		gs_texrender_reset(source->filter_texrender);

	/* call show/hide if the reference changed */
	now_showing = !!source->show_refs;
	if (now_showing != source->showing) {
		if (now_showing) {
			show_source(source);
		} else {
			hide_source(source);
		}

		if (source->filters.num) {
			for (size_t i = source->filters.num; i > 0; i--) {
				obs_source_t *filter = source->filters.array[i - 1];
				if (now_showing) {
					show_source(filter);
				} else {
					hide_source(filter);
				}
			}
		}

		source->showing = now_showing;
	}

	/* call activate/deactivate if the reference changed */
	now_active = !!source->activate_refs;
	if (now_active != source->active) {
		if (now_active) {
			activate_source(source);
		} else {
			deactivate_source(source);
		}

		if (source->filters.num) {
			for (size_t i = source->filters.num; i > 0; i--) {
				obs_source_t *filter = source->filters.array[i - 1];
				if (now_active) {
					activate_source(filter);
				} else {
					deactivate_source(filter);
				}
			}
		}

		source->active = now_active;
	}

	if (source->context.data && source->info.video_tick)
		source->info.video_tick(source->context.data, seconds);

	source->async_rendered = false;
	source->deinterlace_rendered = false;
}

/* unless the value is 3+ hours worth of frames, this won't overflow */
static inline uint64_t conv_frames_to_time(const size_t sample_rate, const size_t frames)
{
	if (!sample_rate)
		return 0;

	return util_mul_div64(frames, 1000000000ULL, sample_rate);
}

static inline size_t conv_time_to_frames(const size_t sample_rate, const uint64_t duration)
{
	return (size_t)util_mul_div64(duration, sample_rate, 1000000000ULL);
}

/* maximum buffer size */
#define MAX_BUF_SIZE (1000 * AUDIO_OUTPUT_FRAMES * sizeof(float))

/* time threshold in nanoseconds to ensure audio timing is as seamless as
 * possible */
#define TS_SMOOTHING_THRESHOLD 70000000ULL

static inline void reset_audio_timing(obs_source_t *source, uint64_t timestamp, uint64_t os_time)
{
	source->timing_set = true;
	source->timing_adjust = os_time - timestamp;
}

static void reset_audio_data(obs_source_t *source, uint64_t os_time)
{
	for (size_t i = 0; i < MAX_AUDIO_CHANNELS; i++) {
		if (source->audio_input_buf[i].size)
			deque_pop_front(&source->audio_input_buf[i], NULL, source->audio_input_buf[i].size);
	}

	source->last_audio_input_buf_size = 0;
	source->audio_ts = os_time;
	source->next_audio_sys_ts_min = os_time;
}

static void handle_ts_jump(obs_source_t *source, uint64_t expected, uint64_t ts, uint64_t diff, uint64_t os_time)
{
	blog(LOG_DEBUG,
	     "Timestamp for source '%s' jumped by '%" PRIu64 "', "
	     "expected value %" PRIu64 ", input value %" PRIu64,
	     source->context.name, diff, expected, ts);

	pthread_mutex_lock(&source->audio_buf_mutex);
	reset_audio_timing(source, ts, os_time);
	reset_audio_data(source, os_time);
	pthread_mutex_unlock(&source->audio_buf_mutex);
}

static void source_signal_audio_data(obs_source_t *source, const struct audio_data *in, bool muted)
{
	pthread_mutex_lock(&source->audio_cb_mutex);

	for (size_t i = source->audio_cb_list.num; i > 0; i--) {
		struct audio_cb_info info = source->audio_cb_list.array[i - 1];
		info.callback(info.param, source, in, muted);
	}

	pthread_mutex_unlock(&source->audio_cb_mutex);
}

static inline uint64_t uint64_diff(uint64_t ts1, uint64_t ts2)
{
	return (ts1 < ts2) ? (ts2 - ts1) : (ts1 - ts2);
}

static inline size_t get_buf_placement(audio_t *audio, uint64_t offset)
{
	uint32_t sample_rate = audio_output_get_sample_rate(audio);
	return (size_t)util_mul_div64(offset, sample_rate, 1000000000ULL);
}

static void source_output_audio_place(obs_source_t *source, const struct audio_data *in)
{
	audio_t *audio = obs->audio.audio;
	size_t buf_placement;
	size_t channels = audio_output_get_channels(audio);
	size_t size = in->frames * sizeof(float);

	if (!source->audio_ts || in->timestamp < source->audio_ts)
		reset_audio_data(source, in->timestamp);

	buf_placement = get_buf_placement(audio, in->timestamp - source->audio_ts) * sizeof(float);

#if DEBUG_AUDIO == 1
	blog(LOG_DEBUG, "frames: %lu, size: %lu, placement: %lu, base_ts: %llu, ts: %llu", (unsigned long)in->frames,
	     (unsigned long)source->audio_input_buf[0].size, (unsigned long)buf_placement, source->audio_ts,
	     in->timestamp);
#endif

	/* do not allow the circular buffers to become too big */
	if ((buf_placement + size) > MAX_BUF_SIZE)
		return;

	for (size_t i = 0; i < channels; i++) {
		deque_place(&source->audio_input_buf[i], buf_placement, in->data[i], size);
		deque_pop_back(&source->audio_input_buf[i], NULL,
			       source->audio_input_buf[i].size - (buf_placement + size));
	}

	source->last_audio_input_buf_size = 0;
}

static inline void source_output_audio_push_back(obs_source_t *source, const struct audio_data *in)
{
	audio_t *audio = obs->audio.audio;
	size_t channels = audio_output_get_channels(audio);
	size_t size = in->frames * sizeof(float);

	/* do not allow the circular buffers to become too big */
	if ((source->audio_input_buf[0].size + size) > MAX_BUF_SIZE)
		return;

	for (size_t i = 0; i < channels; i++)
		deque_push_back(&source->audio_input_buf[i], in->data[i], size);

	/* reset audio input buffer size to ensure that audio doesn't get
	 * perpetually cut */
	source->last_audio_input_buf_size = 0;
}

static inline bool source_muted(obs_source_t *source, uint64_t os_time)
{
	if (source->push_to_mute_enabled && source->user_push_to_mute_pressed)
		source->push_to_mute_stop_time = os_time + source->push_to_mute_delay * 1000000;

	if (source->push_to_talk_enabled && source->user_push_to_talk_pressed)
		source->push_to_talk_stop_time = os_time + source->push_to_talk_delay * 1000000;

	bool push_to_mute_active = source->user_push_to_mute_pressed || os_time < source->push_to_mute_stop_time;
	bool push_to_talk_active = source->user_push_to_talk_pressed || os_time < source->push_to_talk_stop_time;

	return !source->enabled || source->user_muted || (source->push_to_mute_enabled && push_to_mute_active) ||
	       (source->push_to_talk_enabled && !push_to_talk_active);
}

static void source_output_audio_data(obs_source_t *source, const struct audio_data *data)
{
	size_t sample_rate = audio_output_get_sample_rate(obs->audio.audio);
	struct audio_data in = *data;
	uint64_t diff;
	uint64_t os_time = os_gettime_ns();
	int64_t sync_offset;
	bool using_direct_ts = false;
	bool push_back = false;

	/* detects 'directly' set timestamps as long as they're within
	 * a certain threshold */
	if (uint64_diff(in.timestamp, os_time) < MAX_TS_VAR) {
		source->timing_adjust = 0;
		source->timing_set = true;
		using_direct_ts = true;
	}

	if (!source->timing_set) {
		reset_audio_timing(source, in.timestamp, os_time);

	} else if (source->next_audio_ts_min != 0) {
		diff = uint64_diff(source->next_audio_ts_min, in.timestamp);

		/* smooth audio if within threshold */
		if (diff > MAX_TS_VAR && !using_direct_ts)
			handle_ts_jump(source, source->next_audio_ts_min, in.timestamp, diff, os_time);
		else if (diff < TS_SMOOTHING_THRESHOLD) {
			if (source->async_unbuffered && source->async_decoupled)
				source->timing_adjust = os_time - in.timestamp;
			in.timestamp = source->next_audio_ts_min;
		} else {
			blog(LOG_DEBUG,
			     "Audio timestamp for '%s' exceeded TS_SMOOTHING_THRESHOLD, diff=%" PRIu64
			     " ns, expected %" PRIu64 ", input %" PRIu64,
			     source->context.name, diff, source->next_audio_ts_min, in.timestamp);
		}
	}

	source->next_audio_ts_min = in.timestamp + conv_frames_to_time(sample_rate, in.frames);

	in.timestamp += source->timing_adjust;

	pthread_mutex_lock(&source->audio_buf_mutex);

	if (source->next_audio_sys_ts_min == in.timestamp) {
		push_back = true;

	} else if (source->next_audio_sys_ts_min) {
		diff = uint64_diff(source->next_audio_sys_ts_min, in.timestamp);

		if (diff < TS_SMOOTHING_THRESHOLD) {
			push_back = true;

		} else if (diff > MAX_TS_VAR) {
			/* This typically only happens if used with async video when
			 * audio/video start transitioning in to a timestamp jump.
			 * Audio will typically have a timestamp jump, and then video
			 * will have a timestamp jump.  If that case is encountered,
			 * just clear the audio data in that small window and force a
			 * resync.  This handles all cases rather than just looping. */
			reset_audio_timing(source, data->timestamp, os_time);
			in.timestamp = data->timestamp + source->timing_adjust;
		}
	}

	sync_offset = source->sync_offset;
	in.timestamp += sync_offset;
	in.timestamp -= source->resample_offset;

	source->next_audio_sys_ts_min = source->next_audio_ts_min + source->timing_adjust;

	if (source->last_sync_offset != sync_offset) {
		if (source->last_sync_offset)
			push_back = false;
		source->last_sync_offset = sync_offset;
	}

	if (source->monitoring_type != OBS_MONITORING_TYPE_MONITOR_ONLY) {
		if (push_back && source->audio_ts)
			source_output_audio_push_back(source, &in);
		else
			source_output_audio_place(source, &in);
	}

	pthread_mutex_unlock(&source->audio_buf_mutex);

	source_signal_audio_data(source, data, source_muted(source, os_time));
}

enum convert_type {
	CONVERT_NONE,
	CONVERT_NV12,
	CONVERT_420,
	CONVERT_420_PQ,
	CONVERT_420_A,
	CONVERT_422,
	CONVERT_422P10LE,
	CONVERT_422_A,
	CONVERT_422_PACK,
	CONVERT_444,
	CONVERT_444P12LE,
	CONVERT_444_A,
	CONVERT_444P12LE_A,
	CONVERT_444_A_PACK,
	CONVERT_800,
	CONVERT_RGB_LIMITED,
	CONVERT_BGR3,
	CONVERT_I010,
	CONVERT_P010,
	CONVERT_V210,
	CONVERT_R10L,
};

static inline enum convert_type get_convert_type(enum video_format format, bool full_range, uint8_t trc)
{
	switch (format) {
	case VIDEO_FORMAT_I420:
		return (trc == VIDEO_TRC_PQ) ? CONVERT_420_PQ : CONVERT_420;
	case VIDEO_FORMAT_NV12:
		return CONVERT_NV12;
	case VIDEO_FORMAT_I444:
		return CONVERT_444;
	case VIDEO_FORMAT_I412:
		return CONVERT_444P12LE;
	case VIDEO_FORMAT_I422:
		return CONVERT_422;
	case VIDEO_FORMAT_I210:
		return CONVERT_422P10LE;

	case VIDEO_FORMAT_YVYU:
	case VIDEO_FORMAT_YUY2:
	case VIDEO_FORMAT_UYVY:
		return CONVERT_422_PACK;

	case VIDEO_FORMAT_Y800:
		return CONVERT_800;

	case VIDEO_FORMAT_NONE:
	case VIDEO_FORMAT_RGBA:
	case VIDEO_FORMAT_BGRA:
	case VIDEO_FORMAT_BGRX:
		return full_range ? CONVERT_NONE : CONVERT_RGB_LIMITED;

	case VIDEO_FORMAT_BGR3:
		return CONVERT_BGR3;

	case VIDEO_FORMAT_I40A:
		return CONVERT_420_A;

	case VIDEO_FORMAT_I42A:
		return CONVERT_422_A;

	case VIDEO_FORMAT_YUVA:
		return CONVERT_444_A;

	case VIDEO_FORMAT_YA2L:
		return CONVERT_444P12LE_A;

	case VIDEO_FORMAT_AYUV:
		return CONVERT_444_A_PACK;

	case VIDEO_FORMAT_I010:
		return CONVERT_I010;

	case VIDEO_FORMAT_P010:
		return CONVERT_P010;

	case VIDEO_FORMAT_V210:
		return CONVERT_V210;

	case VIDEO_FORMAT_R10L:
		return CONVERT_R10L;

	case VIDEO_FORMAT_P216:
	case VIDEO_FORMAT_P416:
		/* Unimplemented */
		break;
	}

	return CONVERT_NONE;
}

static inline bool set_packed422_sizes(struct obs_source *source, const struct obs_source_frame *frame)
{
	const uint32_t width = frame->width;
	const uint32_t height = frame->height;
	const uint32_t half_width = (width + 1) / 2;
	source->async_convert_width[0] = half_width;
	source->async_convert_height[0] = height;
	source->async_texture_formats[0] = GS_BGRA;
	source->async_channel_count = 1;
	return true;
}

static inline bool set_packed444_alpha_sizes(struct obs_source *source, const struct obs_source_frame *frame)
{
	source->async_convert_width[0] = frame->width;
	source->async_convert_height[0] = frame->height;
	source->async_texture_formats[0] = GS_BGRA;
	source->async_channel_count = 1;
	return true;
}

static inline bool set_planar444_sizes(struct obs_source *source, const struct obs_source_frame *frame)
{
	source->async_convert_width[0] = frame->width;
	source->async_convert_width[1] = frame->width;
	source->async_convert_width[2] = frame->width;
	source->async_convert_height[0] = frame->height;
	source->async_convert_height[1] = frame->height;
	source->async_convert_height[2] = frame->height;
	source->async_texture_formats[0] = GS_R8;
	source->async_texture_formats[1] = GS_R8;
	source->async_texture_formats[2] = GS_R8;
	source->async_channel_count = 3;
	return true;
}

static inline bool set_planar444_16_sizes(struct obs_source *source, const struct obs_source_frame *frame)
{
	source->async_convert_width[0] = frame->width;
	source->async_convert_width[1] = frame->width;
	source->async_convert_width[2] = frame->width;
	source->async_convert_height[0] = frame->height;
	source->async_convert_height[1] = frame->height;
	source->async_convert_height[2] = frame->height;
	source->async_texture_formats[0] = GS_R16;
	source->async_texture_formats[1] = GS_R16;
	source->async_texture_formats[2] = GS_R16;
	source->async_channel_count = 3;
	return true;
}

static inline bool set_planar444_alpha_sizes(struct obs_source *source, const struct obs_source_frame *frame)
{
	source->async_convert_width[0] = frame->width;
	source->async_convert_width[1] = frame->width;
	source->async_convert_width[2] = frame->width;
	source->async_convert_width[3] = frame->width;
	source->async_convert_height[0] = frame->height;
	source->async_convert_height[1] = frame->height;
	source->async_convert_height[2] = frame->height;
	source->async_convert_height[3] = frame->height;
	source->async_texture_formats[0] = GS_R8;
	source->async_texture_formats[1] = GS_R8;
	source->async_texture_formats[2] = GS_R8;
	source->async_texture_formats[3] = GS_R8;
	source->async_channel_count = 4;
	return true;
}

static inline bool set_planar444_16_alpha_sizes(struct obs_source *source, const struct obs_source_frame *frame)
{
	source->async_convert_width[0] = frame->width;
	source->async_convert_width[1] = frame->width;
	source->async_convert_width[2] = frame->width;
	source->async_convert_width[3] = frame->width;
	source->async_convert_height[0] = frame->height;
	source->async_convert_height[1] = frame->height;
	source->async_convert_height[2] = frame->height;
	source->async_convert_height[3] = frame->height;
	source->async_texture_formats[0] = GS_R16;
	source->async_texture_formats[1] = GS_R16;
	source->async_texture_formats[2] = GS_R16;
	source->async_texture_formats[3] = GS_R16;
	source->async_channel_count = 4;
	return true;
}

static inline bool set_planar420_sizes(struct obs_source *source, const struct obs_source_frame *frame)
{
	const uint32_t width = frame->width;
	const uint32_t height = frame->height;
	const uint32_t half_width = (width + 1) / 2;
	const uint32_t half_height = (height + 1) / 2;
	source->async_convert_width[0] = width;
	source->async_convert_width[1] = half_width;
	source->async_convert_width[2] = half_width;
	source->async_convert_height[0] = height;
	source->async_convert_height[1] = half_height;
	source->async_convert_height[2] = half_height;
	source->async_texture_formats[0] = GS_R8;
	source->async_texture_formats[1] = GS_R8;
	source->async_texture_formats[2] = GS_R8;
	source->async_channel_count = 3;
	return true;
}

static inline bool set_planar420_alpha_sizes(struct obs_source *source, const struct obs_source_frame *frame)
{
	const uint32_t width = frame->width;
	const uint32_t height = frame->height;
	const uint32_t half_width = (width + 1) / 2;
	const uint32_t half_height = (height + 1) / 2;
	source->async_convert_width[0] = width;
	source->async_convert_width[1] = half_width;
	source->async_convert_width[2] = half_width;
	source->async_convert_width[3] = width;
	source->async_convert_height[0] = height;
	source->async_convert_height[1] = half_height;
	source->async_convert_height[2] = half_height;
	source->async_convert_height[3] = height;
	source->async_texture_formats[0] = GS_R8;
	source->async_texture_formats[1] = GS_R8;
	source->async_texture_formats[2] = GS_R8;
	source->async_texture_formats[3] = GS_R8;
	source->async_channel_count = 4;
	return true;
}

static inline bool set_planar422_sizes(struct obs_source *source, const struct obs_source_frame *frame)
{
	const uint32_t width = frame->width;
	const uint32_t height = frame->height;
	const uint32_t half_width = (width + 1) / 2;
	source->async_convert_width[0] = width;
	source->async_convert_width[1] = half_width;
	source->async_convert_width[2] = half_width;
	source->async_convert_height[0] = height;
	source->async_convert_height[1] = height;
	source->async_convert_height[2] = height;
	source->async_texture_formats[0] = GS_R8;
	source->async_texture_formats[1] = GS_R8;
	source->async_texture_formats[2] = GS_R8;
	source->async_channel_count = 3;
	return true;
}
static inline bool set_planar422_16_sizes(struct obs_source *source, const struct obs_source_frame *frame)
{
	const uint32_t width = frame->width;
	const uint32_t height = frame->height;
	const uint32_t half_width = (width + 1) / 2;
	source->async_convert_width[0] = width;
	source->async_convert_width[1] = half_width;
	source->async_convert_width[2] = half_width;
	source->async_convert_height[0] = height;
	source->async_convert_height[1] = height;
	source->async_convert_height[2] = height;
	source->async_texture_formats[0] = GS_R16;
	source->async_texture_formats[1] = GS_R16;
	source->async_texture_formats[2] = GS_R16;
	source->async_channel_count = 3;
	return true;
}

static inline bool set_planar422_alpha_sizes(struct obs_source *source, const struct obs_source_frame *frame)
{
	const uint32_t width = frame->width;
	const uint32_t height = frame->height;
	const uint32_t half_width = (width + 1) / 2;
	source->async_convert_width[0] = width;
	source->async_convert_width[1] = half_width;
	source->async_convert_width[2] = half_width;
	source->async_convert_width[3] = width;
	source->async_convert_height[0] = height;
	source->async_convert_height[1] = height;
	source->async_convert_height[2] = height;
	source->async_convert_height[3] = height;
	source->async_texture_formats[0] = GS_R8;
	source->async_texture_formats[1] = GS_R8;
	source->async_texture_formats[2] = GS_R8;
	source->async_texture_formats[3] = GS_R8;
	source->async_channel_count = 4;
	return true;
}

static inline bool set_nv12_sizes(struct obs_source *source, const struct obs_source_frame *frame)
{
	const uint32_t width = frame->width;
	const uint32_t height = frame->height;
	const uint32_t half_width = (width + 1) / 2;
	const uint32_t half_height = (height + 1) / 2;
	source->async_convert_width[0] = width;
	source->async_convert_width[1] = half_width;
	source->async_convert_height[0] = height;
	source->async_convert_height[1] = half_height;
	source->async_texture_formats[0] = GS_R8;
	source->async_texture_formats[1] = GS_R8G8;
	source->async_channel_count = 2;
	return true;
}

static inline bool set_y800_sizes(struct obs_source *source, const struct obs_source_frame *frame)
{
	source->async_convert_width[0] = frame->width;
	source->async_convert_height[0] = frame->height;
	source->async_texture_formats[0] = GS_R8;
	source->async_channel_count = 1;
	return true;
}

static inline bool set_rgb_limited_sizes(struct obs_source *source, const struct obs_source_frame *frame)
{
	source->async_convert_width[0] = frame->width;
	source->async_convert_height[0] = frame->height;
	source->async_texture_formats[0] = convert_video_format(frame->format, frame->trc);
	source->async_channel_count = 1;
	return true;
}

static inline bool set_bgr3_sizes(struct obs_source *source, const struct obs_source_frame *frame)
{
	source->async_convert_width[0] = frame->width * 3;
	source->async_convert_height[0] = frame->height;
	source->async_texture_formats[0] = GS_R8;
	source->async_channel_count = 1;
	return true;
}

static inline bool set_i010_sizes(struct obs_source *source, const struct obs_source_frame *frame)
{
	const uint32_t width = frame->width;
	const uint32_t height = frame->height;
	const uint32_t half_width = (width + 1) / 2;
	const uint32_t half_height = (height + 1) / 2;
	source->async_convert_width[0] = width;
	source->async_convert_width[1] = half_width;
	source->async_convert_width[2] = half_width;
	source->async_convert_height[0] = height;
	source->async_convert_height[1] = half_height;
	source->async_convert_height[2] = half_height;
	source->async_texture_formats[0] = GS_R16;
	source->async_texture_formats[1] = GS_R16;
	source->async_texture_formats[2] = GS_R16;
	source->async_channel_count = 3;
	return true;
}

static inline bool set_p010_sizes(struct obs_source *source, const struct obs_source_frame *frame)
{
	const uint32_t width = frame->width;
	const uint32_t height = frame->height;
	const uint32_t half_width = (width + 1) / 2;
	const uint32_t half_height = (height + 1) / 2;
	source->async_convert_width[0] = width;
	source->async_convert_width[1] = half_width;
	source->async_convert_height[0] = height;
	source->async_convert_height[1] = half_height;
	source->async_texture_formats[0] = GS_R16;
	source->async_texture_formats[1] = GS_RG16;
	source->async_channel_count = 2;
	return true;
}

static inline bool set_v210_sizes(struct obs_source *source, const struct obs_source_frame *frame)
{
	const uint32_t width = frame->width;
	const uint32_t height = frame->height;
	const uint32_t adjusted_width = ((width + 5) / 6) * 4;
	source->async_convert_width[0] = adjusted_width;
	source->async_convert_height[0] = height;
	source->async_texture_formats[0] = GS_R10G10B10A2;
	source->async_channel_count = 1;
	return true;
}

static inline bool set_r10l_sizes(struct obs_source *source, const struct obs_source_frame *frame)
{
	source->async_convert_width[0] = frame->width;
	source->async_convert_height[0] = frame->height;
	source->async_texture_formats[0] = GS_BGRA_UNORM;
	source->async_channel_count = 1;
	return true;
}

static inline bool init_gpu_conversion(struct obs_source *source, const struct obs_source_frame *frame)
{
	switch (get_convert_type(frame->format, frame->full_range, frame->trc)) {
	case CONVERT_422_PACK:
		return set_packed422_sizes(source, frame);

	case CONVERT_420:
	case CONVERT_420_PQ:
		return set_planar420_sizes(source, frame);

	case CONVERT_422:
		return set_planar422_sizes(source, frame);

	case CONVERT_422P10LE:
		return set_planar422_16_sizes(source, frame);

	case CONVERT_NV12:
		return set_nv12_sizes(source, frame);

	case CONVERT_444:
		return set_planar444_sizes(source, frame);

	case CONVERT_444P12LE:
		return set_planar444_16_sizes(source, frame);

	case CONVERT_800:
		return set_y800_sizes(source, frame);

	case CONVERT_RGB_LIMITED:
		return set_rgb_limited_sizes(source, frame);

	case CONVERT_BGR3:
		return set_bgr3_sizes(source, frame);

	case CONVERT_420_A:
		return set_planar420_alpha_sizes(source, frame);

	case CONVERT_422_A:
		return set_planar422_alpha_sizes(source, frame);

	case CONVERT_444_A:
		return set_planar444_alpha_sizes(source, frame);

	case CONVERT_444P12LE_A:
		return set_planar444_16_alpha_sizes(source, frame);

	case CONVERT_444_A_PACK:
		return set_packed444_alpha_sizes(source, frame);

	case CONVERT_I010:
		return set_i010_sizes(source, frame);

	case CONVERT_P010:
		return set_p010_sizes(source, frame);

	case CONVERT_V210:
		return set_v210_sizes(source, frame);

	case CONVERT_R10L:
		return set_r10l_sizes(source, frame);

	case CONVERT_NONE:
		assert(false && "No conversion requested");
		break;
	}
	return false;
}

bool set_async_texture_size(struct obs_source *source, const struct obs_source_frame *frame)
{
	enum convert_type cur = get_convert_type(frame->format, frame->full_range, frame->trc);

	if (source->async_width == frame->width && source->async_height == frame->height &&
	    source->async_format == frame->format && source->async_full_range == frame->full_range &&
	    source->async_trc == frame->trc)
		return true;

	source->async_width = frame->width;
	source->async_height = frame->height;
	source->async_format = frame->format;
	source->async_full_range = frame->full_range;
	source->async_trc = frame->trc;

	gs_enter_context(obs->video.graphics);

	for (size_t c = 0; c < MAX_AV_PLANES; c++) {
		gs_texture_destroy(source->async_textures[c]);
		source->async_textures[c] = NULL;
		gs_texture_destroy(source->async_prev_textures[c]);
		source->async_prev_textures[c] = NULL;
	}

	gs_texrender_destroy(source->async_texrender);
	gs_texrender_destroy(source->async_prev_texrender);
	source->async_texrender = NULL;
	source->async_prev_texrender = NULL;

	const enum gs_color_format format = convert_video_format(frame->format, frame->trc);
	const bool async_gpu_conversion = (cur != CONVERT_NONE) && init_gpu_conversion(source, frame);
	source->async_gpu_conversion = async_gpu_conversion;
	if (async_gpu_conversion) {
		source->async_texrender = gs_texrender_create(format, GS_ZS_NONE);

		for (int c = 0; c < source->async_channel_count; ++c)
			source->async_textures[c] =
				gs_texture_create(source->async_convert_width[c], source->async_convert_height[c],
						  source->async_texture_formats[c], 1, NULL, GS_DYNAMIC);
	} else {
		source->async_textures[0] = gs_texture_create(frame->width, frame->height, format, 1, NULL, GS_DYNAMIC);
	}

	if (deinterlacing_enabled(source))
		set_deinterlace_texture_size(source);

	gs_leave_context();

	return source->async_textures[0] != NULL;
}

static void upload_raw_frame(gs_texture_t *tex[MAX_AV_PLANES], const struct obs_source_frame *frame)
{
	switch (get_convert_type(frame->format, frame->full_range, frame->trc)) {
	case CONVERT_422_PACK:
	case CONVERT_800:
	case CONVERT_RGB_LIMITED:
	case CONVERT_BGR3:
	case CONVERT_420:
	case CONVERT_420_PQ:
	case CONVERT_422:
	case CONVERT_422P10LE:
	case CONVERT_NV12:
	case CONVERT_444:
	case CONVERT_444P12LE:
	case CONVERT_420_A:
	case CONVERT_422_A:
	case CONVERT_444_A:
	case CONVERT_444P12LE_A:
	case CONVERT_444_A_PACK:
	case CONVERT_I010:
	case CONVERT_P010:
	case CONVERT_V210:
	case CONVERT_R10L:
		for (size_t c = 0; c < MAX_AV_PLANES; c++) {
			if (tex[c])
				gs_texture_set_image(tex[c], frame->data[c], frame->linesize[c], false);
		}
		break;

	case CONVERT_NONE:
		assert(false && "No conversion requested");
		break;
	}
}

static const char *select_conversion_technique(enum video_format format, bool full_range, uint8_t trc)
{
	switch (format) {
	case VIDEO_FORMAT_UYVY:
		return "UYVY_Reverse";

	case VIDEO_FORMAT_YUY2:
		switch (trc) {
		case VIDEO_TRC_PQ:
			return "YUY2_PQ_Reverse";
		case VIDEO_TRC_HLG:
			return "YUY2_HLG_Reverse";
		default:
			return "YUY2_Reverse";
		}

	case VIDEO_FORMAT_YVYU:
		return "YVYU_Reverse";

	case VIDEO_FORMAT_I420:
		switch (trc) {
		case VIDEO_TRC_PQ:
			return "I420_PQ_Reverse";
		case VIDEO_TRC_HLG:
			return "I420_HLG_Reverse";
		default:
			return "I420_Reverse";
		}

	case VIDEO_FORMAT_NV12:
		switch (trc) {
		case VIDEO_TRC_PQ:
			return "NV12_PQ_Reverse";
		case VIDEO_TRC_HLG:
			return "NV12_HLG_Reverse";
		default:
			return "NV12_Reverse";
		}

	case VIDEO_FORMAT_I444:
		return "I444_Reverse";

	case VIDEO_FORMAT_I412:
		switch (trc) {
		case VIDEO_TRC_PQ:
			return "I412_PQ_Reverse";
		case VIDEO_TRC_HLG:
			return "I412_HLG_Reverse";
		default:
			return "I412_Reverse";
		}

	case VIDEO_FORMAT_Y800:
		return full_range ? "Y800_Full" : "Y800_Limited";

	case VIDEO_FORMAT_BGR3:
		return full_range ? "BGR3_Full" : "BGR3_Limited";

	case VIDEO_FORMAT_I422:
		return "I422_Reverse";

	case VIDEO_FORMAT_I210:
		switch (trc) {
		case VIDEO_TRC_PQ:
			return "I210_PQ_Reverse";
		case VIDEO_TRC_HLG:
			return "I210_HLG_Reverse";
		default:
			return "I210_Reverse";
		}

	case VIDEO_FORMAT_I40A:
		return "I40A_Reverse";

	case VIDEO_FORMAT_I42A:
		return "I42A_Reverse";

	case VIDEO_FORMAT_YUVA:
		return "YUVA_Reverse";

	case VIDEO_FORMAT_YA2L:
		return "YA2L_Reverse";

	case VIDEO_FORMAT_AYUV:
		return "AYUV_Reverse";

	case VIDEO_FORMAT_I010: {
		switch (trc) {
		case VIDEO_TRC_PQ:
			return "I010_PQ_2020_709_Reverse";
		case VIDEO_TRC_HLG:
			return "I010_HLG_2020_709_Reverse";
		default:
			return "I010_SRGB_Reverse";
		}
	}

	case VIDEO_FORMAT_P010: {
		switch (trc) {
		case VIDEO_TRC_PQ:
			return "P010_PQ_2020_709_Reverse";
		case VIDEO_TRC_HLG:
			return "P010_HLG_2020_709_Reverse";
		default:
			return "P010_SRGB_Reverse";
		}
	}

	case VIDEO_FORMAT_V210: {
		switch (trc) {
		case VIDEO_TRC_PQ:
			return "V210_PQ_2020_709_Reverse";
		case VIDEO_TRC_HLG:
			return "V210_HLG_2020_709_Reverse";
		default:
			return "V210_SRGB_Reverse";
		}
	}

	case VIDEO_FORMAT_R10L: {
		switch (trc) {
		case VIDEO_TRC_PQ:
			return full_range ? "R10L_PQ_2020_709_Full_Reverse" : "R10L_PQ_2020_709_Limited_Reverse";
		case VIDEO_TRC_HLG:
			return full_range ? "R10L_HLG_2020_709_Full_Reverse" : "R10L_HLG_2020_709_Limited_Reverse";
		default:
			return full_range ? "R10L_SRGB_Full_Reverse" : "R10L_SRGB_Limited_Reverse";
		}
	}

	case VIDEO_FORMAT_BGRA:
	case VIDEO_FORMAT_BGRX:
	case VIDEO_FORMAT_RGBA:
	case VIDEO_FORMAT_NONE:
		if (full_range)
			assert(false && "No conversion requested");
		else
			return "RGB_Limited";
		break;

	case VIDEO_FORMAT_P216:
	case VIDEO_FORMAT_P416:
		/* Unimplemented */
		break;
	}
	return NULL;
}

static bool need_linear_output(enum video_format format)
{
	return (format == VIDEO_FORMAT_I010) || (format == VIDEO_FORMAT_P010) || (format == VIDEO_FORMAT_I210) ||
	       (format == VIDEO_FORMAT_I412) || (format == VIDEO_FORMAT_YA2L);
}

static inline void set_eparam(gs_effect_t *effect, const char *name, float val)
{
	gs_eparam_t *param = gs_effect_get_param_by_name(effect, name);
	gs_effect_set_float(param, val);
}

static bool update_async_texrender(struct obs_source *source, const struct obs_source_frame *frame,
				   gs_texture_t *tex[MAX_AV_PLANES], gs_texrender_t *texrender)
{
	GS_DEBUG_MARKER_BEGIN(GS_DEBUG_COLOR_CONVERT_FORMAT, "Convert Format");

	gs_texrender_reset(texrender);

	upload_raw_frame(tex, frame);

	uint32_t cx = source->async_width;
	uint32_t cy = source->async_height;

	const char *tech_name = select_conversion_technique(frame->format, frame->full_range, frame->trc);
	gs_effect_t *conv = obs->video.conversion_effect;
	gs_technique_t *tech = gs_effect_get_technique(conv, tech_name);
	const bool linear = need_linear_output(frame->format);

	const bool success = gs_texrender_begin(texrender, cx, cy);

	if (success) {
		const bool previous = gs_framebuffer_srgb_enabled();
		gs_enable_framebuffer_srgb(linear);

		gs_enable_blending(false);

		gs_technique_begin(tech);
		gs_technique_begin_pass(tech, 0);

		if (tex[0])
			gs_effect_set_texture(gs_effect_get_param_by_name(conv, "image"), tex[0]);
		if (tex[1])
			gs_effect_set_texture(gs_effect_get_param_by_name(conv, "image1"), tex[1]);
		if (tex[2])
			gs_effect_set_texture(gs_effect_get_param_by_name(conv, "image2"), tex[2]);
		if (tex[3])
			gs_effect_set_texture(gs_effect_get_param_by_name(conv, "image3"), tex[3]);
		set_eparam(conv, "width", (float)cx);
		set_eparam(conv, "height", (float)cy);
		set_eparam(conv, "width_d2", (float)cx * 0.5f);
		set_eparam(conv, "height_d2", (float)cy * 0.5f);
		set_eparam(conv, "width_x2_i", 0.5f / (float)cx);
		set_eparam(conv, "height_x2_i", 0.5f / (float)cy);

		/* BT.2408 says higher than 1000 isn't comfortable */
		float hlg_peak_level = obs->video.hdr_nominal_peak_level;
		if (hlg_peak_level > 1000.f)
			hlg_peak_level = 1000.f;

		const float maximum_nits = (frame->trc == VIDEO_TRC_HLG) ? hlg_peak_level : 10000.f;
		set_eparam(conv, "maximum_over_sdr_white_nits", maximum_nits / obs_get_video_sdr_white_level());
		const float hlg_exponent = 0.2f + (0.42f * log10f(hlg_peak_level / 1000.f));
		set_eparam(conv, "hlg_exponent", hlg_exponent);
		set_eparam(conv, "hdr_lw", (float)frame->max_luminance);
		set_eparam(conv, "hdr_lmax", obs_get_video_hdr_nominal_peak_level());

		struct vec4 vec0, vec1, vec2;
		vec4_set(&vec0, frame->color_matrix[0], frame->color_matrix[1], frame->color_matrix[2],
			 frame->color_matrix[3]);
		vec4_set(&vec1, frame->color_matrix[4], frame->color_matrix[5], frame->color_matrix[6],
			 frame->color_matrix[7]);
		vec4_set(&vec2, frame->color_matrix[8], frame->color_matrix[9], frame->color_matrix[10],
			 frame->color_matrix[11]);
		gs_effect_set_vec4(gs_effect_get_param_by_name(conv, "color_vec0"), &vec0);
		gs_effect_set_vec4(gs_effect_get_param_by_name(conv, "color_vec1"), &vec1);
		gs_effect_set_vec4(gs_effect_get_param_by_name(conv, "color_vec2"), &vec2);
		if (!frame->full_range) {
			gs_eparam_t *min_param = gs_effect_get_param_by_name(conv, "color_range_min");
			gs_effect_set_val(min_param, frame->color_range_min, sizeof(float) * 3);
			gs_eparam_t *max_param = gs_effect_get_param_by_name(conv, "color_range_max");
			gs_effect_set_val(max_param, frame->color_range_max, sizeof(float) * 3);
		}

		gs_draw(GS_TRIS, 0, 3);

		gs_technique_end_pass(tech);
		gs_technique_end(tech);

		gs_enable_blending(true);

		gs_enable_framebuffer_srgb(previous);

		gs_texrender_end(texrender);
	}

	GS_DEBUG_MARKER_END();
	return success;
}

bool update_async_texture(struct obs_source *source, const struct obs_source_frame *frame, gs_texture_t *tex,
			  gs_texrender_t *texrender)
{
	gs_texture_t *tex3[MAX_AV_PLANES] = {tex, NULL, NULL, NULL, NULL, NULL, NULL, NULL};
	return update_async_textures(source, frame, tex3, texrender);
}

bool update_async_textures(struct obs_source *source, const struct obs_source_frame *frame,
			   gs_texture_t *tex[MAX_AV_PLANES], gs_texrender_t *texrender)
{
	enum convert_type type;

	source->async_flip = frame->flip;
	source->async_linear_alpha = (frame->flags & OBS_SOURCE_FRAME_LINEAR_ALPHA) != 0;

	if (source->async_gpu_conversion && texrender)
		return update_async_texrender(source, frame, tex, texrender);

	type = get_convert_type(frame->format, frame->full_range, frame->trc);
	if (type == CONVERT_NONE) {
		gs_texture_set_image(tex[0], frame->data[0], frame->linesize[0], false);
		return true;
	}

	return false;
}

static inline void obs_source_draw_texture(struct obs_source *source, gs_effect_t *effect)
{
	gs_texture_t *tex = source->async_textures[0];
	gs_eparam_t *param;

	if (source->async_texrender)
		tex = gs_texrender_get_texture(source->async_texrender);

	if (!tex)
		return;

	param = gs_effect_get_param_by_name(effect, "image");

	const bool linear_srgb = gs_get_linear_srgb();

	const bool previous = gs_framebuffer_srgb_enabled();
	gs_enable_framebuffer_srgb(linear_srgb);

	if (linear_srgb) {
		gs_effect_set_texture_srgb(param, tex);
	} else {
		gs_effect_set_texture(param, tex);
	}

	gs_draw_sprite(tex, source->async_flip ? GS_FLIP_V : 0, 0, 0);

	gs_enable_framebuffer_srgb(previous);
}

static void recreate_async_texture(obs_source_t *source, enum gs_color_format format)
{
	uint32_t cx = gs_texture_get_width(source->async_textures[0]);
	uint32_t cy = gs_texture_get_height(source->async_textures[0]);
	gs_texture_destroy(source->async_textures[0]);
	source->async_textures[0] = gs_texture_create(cx, cy, format, 1, NULL, GS_DYNAMIC);
}

static inline void check_to_swap_bgrx_bgra(obs_source_t *source, struct obs_source_frame *frame)
{
	enum gs_color_format format = gs_texture_get_color_format(source->async_textures[0]);
	if (format == GS_BGRX && frame->format == VIDEO_FORMAT_BGRA) {
		recreate_async_texture(source, GS_BGRA);
	} else if (format == GS_BGRA && frame->format == VIDEO_FORMAT_BGRX) {
		recreate_async_texture(source, GS_BGRX);
	}
}

static void obs_source_update_async_video(obs_source_t *source)
{
	if (!source->async_rendered) {
		source->async_rendered = true;

		struct obs_source_frame *frame = obs_source_get_frame(source);
		if (frame) {
			check_to_swap_bgrx_bgra(source, frame);

			if (!source->async_decoupled || !source->async_unbuffered) {
				source->timing_adjust = obs->video.video_time - frame->timestamp;
				source->timing_set = true;
			}

			if (source->async_update_texture) {
				update_async_textures(source, frame, source->async_textures, source->async_texrender);
				source->async_update_texture = false;
			}

			source->async_last_rendered_ts = frame->timestamp;
			obs_source_release_frame(source, frame);
		}
	}
}

static void rotate_async_video(obs_source_t *source, long rotation)
{
	float x = 0;
	float y = 0;

	switch (rotation) {
	case 90:
		y = (float)source->async_width;
		break;
	case 270:
	case -90:
		x = (float)source->async_height;
		break;
	case 180:
		x = (float)source->async_width;
		y = (float)source->async_height;
	}

	gs_matrix_translate3f(x, y, 0);
	gs_matrix_rotaa4f(0.0f, 0.0f, -1.0f, RAD((float)rotation));
}

static inline void obs_source_render_async_video(obs_source_t *source)
{
	if (source->async_textures[0] && source->async_active) {
		gs_timer_t *timer = NULL;
		const uint64_t start = source_profiler_source_render_begin(&timer);

		const enum gs_color_space source_space = convert_video_space(source->async_format, source->async_trc);

		gs_effect_t *const effect = obs_get_base_effect(OBS_EFFECT_DEFAULT);
		const char *tech_name = "Draw";
		float multiplier = 1.0;
		const enum gs_color_space current_space = gs_get_color_space();
		bool linear_srgb = gs_get_linear_srgb();
		bool nonlinear_alpha = false;
		switch (source_space) {
		case GS_CS_SRGB:
			linear_srgb = linear_srgb || (current_space != GS_CS_SRGB);
			nonlinear_alpha = linear_srgb && !source->async_linear_alpha;
			switch (current_space) {
			case GS_CS_SRGB:
			case GS_CS_SRGB_16F:
			case GS_CS_709_EXTENDED:
				if (nonlinear_alpha)
					tech_name = "DrawNonlinearAlpha";
				break;
			case GS_CS_709_SCRGB:
				tech_name = nonlinear_alpha ? "DrawNonlinearAlphaMultiply" : "DrawMultiply";
				multiplier = obs_get_video_sdr_white_level() / 80.0f;
			}
			break;
		case GS_CS_SRGB_16F:
			if (current_space == GS_CS_709_SCRGB) {
				tech_name = "DrawMultiply";
				multiplier = obs_get_video_sdr_white_level() / 80.0f;
			}
			break;
		case GS_CS_709_EXTENDED:
			switch (current_space) {
			case GS_CS_SRGB:
			case GS_CS_SRGB_16F:
				tech_name = "DrawTonemap";
				linear_srgb = true;
				break;
			case GS_CS_709_SCRGB:
				tech_name = "DrawMultiply";
				multiplier = obs_get_video_sdr_white_level() / 80.0f;
				break;
			case GS_CS_709_EXTENDED:
				break;
			}
			break;
		case GS_CS_709_SCRGB:
			switch (current_space) {
			case GS_CS_SRGB:
			case GS_CS_SRGB_16F:
				tech_name = "DrawMultiplyTonemap";
				multiplier = 80.0f / obs_get_video_sdr_white_level();
				linear_srgb = true;
				break;
			case GS_CS_709_EXTENDED:
				tech_name = "DrawMultiply";
				multiplier = 80.0f / obs_get_video_sdr_white_level();
				break;
			case GS_CS_709_SCRGB:
				break;
			}
		}

		const bool previous = gs_set_linear_srgb(linear_srgb);

		gs_technique_t *const tech = gs_effect_get_technique(effect, tech_name);
		gs_effect_set_float(gs_effect_get_param_by_name(effect, "multiplier"), multiplier);
		gs_technique_begin(tech);
		gs_technique_begin_pass(tech, 0);

		long rotation = source->async_rotation;
		if (rotation) {
			gs_matrix_push();
			rotate_async_video(source, rotation);
		}

		if (nonlinear_alpha) {
			gs_blend_state_push();
			gs_blend_function(GS_BLEND_ONE, GS_BLEND_INVSRCALPHA);
		}

		obs_source_draw_texture(source, effect);

		if (nonlinear_alpha) {
			gs_blend_state_pop();
		}

		if (rotation) {
			gs_matrix_pop();
		}

		gs_technique_end_pass(tech);
		gs_technique_end(tech);

		gs_set_linear_srgb(previous);

		source_profiler_source_render_end(source, start, timer);
	}
}

static inline void obs_source_render_filters(obs_source_t *source)
{
	obs_source_t *first_filter;

	pthread_mutex_lock(&source->filter_mutex);
	first_filter = obs_source_get_ref(source->filters.array[0]);
	pthread_mutex_unlock(&source->filter_mutex);

	source->rendering_filter = true;
	obs_source_video_render(first_filter);
	source->rendering_filter = false;

	obs_source_release(first_filter);
}

static inline uint32_t get_async_width(const obs_source_t *source)
{
	return ((source->async_rotation % 180) == 0) ? source->async_width : source->async_height;
}

static inline uint32_t get_async_height(const obs_source_t *source)
{
	return ((source->async_rotation % 180) == 0) ? source->async_height : source->async_width;
}

static uint32_t get_base_width(const obs_source_t *source)
{
	bool is_filter = !!source->filter_parent;
	bool func_valid = source->context.data && source->info.get_width;

	if (source->info.type == OBS_SOURCE_TYPE_TRANSITION) {
		return source->enabled ? source->transition_actual_cx : 0;

	} else if (func_valid && (!is_filter || source->enabled)) {
		return source->info.get_width(source->context.data);

	} else if (is_filter) {
		return get_base_width(source->filter_target);
	}

	return source->async_active ? get_async_width(source) : 0;
}

static uint32_t get_base_height(const obs_source_t *source)
{
	bool is_filter = !!source->filter_parent;
	bool func_valid = source->context.data && source->info.get_height;

	if (source->info.type == OBS_SOURCE_TYPE_TRANSITION) {
		return source->enabled ? source->transition_actual_cy : 0;

	} else if (func_valid && (!is_filter || source->enabled)) {
		return source->info.get_height(source->context.data);

	} else if (is_filter) {
		return get_base_height(source->filter_target);
	}

	return source->async_active ? get_async_height(source) : 0;
}

static void source_render(obs_source_t *source, gs_effect_t *effect)
{
	gs_timer_t *timer = NULL;
	const uint64_t start = source_profiler_source_render_begin(&timer);

	void *const data = source->context.data;
	const enum gs_color_space current_space = gs_get_color_space();
	const enum gs_color_space source_space = obs_source_get_color_space(source, 1, &current_space);

	const char *convert_tech = NULL;
	float multiplier = 1.0;
	enum gs_color_format format = gs_get_format_from_space(source_space);
	switch (source_space) {
	case GS_CS_SRGB:
	case GS_CS_SRGB_16F:
		switch (current_space) {
		case GS_CS_709_EXTENDED:
			convert_tech = "Draw";
			break;
		case GS_CS_709_SCRGB:
			convert_tech = "DrawMultiply";
			multiplier = obs_get_video_sdr_white_level() / 80.0f;
			break;
		case GS_CS_SRGB:
			break;
		case GS_CS_SRGB_16F:
			break;
		}
		break;
	case GS_CS_709_EXTENDED:
		switch (current_space) {
		case GS_CS_SRGB:
		case GS_CS_SRGB_16F:
			convert_tech = "DrawTonemap";
			break;
		case GS_CS_709_SCRGB:
			convert_tech = "DrawMultiply";
			multiplier = obs_get_video_sdr_white_level() / 80.0f;
			break;
		case GS_CS_709_EXTENDED:
			break;
		}
		break;
	case GS_CS_709_SCRGB:
		switch (current_space) {
		case GS_CS_SRGB:
		case GS_CS_SRGB_16F:
			convert_tech = "DrawMultiplyTonemap";
			multiplier = 80.0f / obs_get_video_sdr_white_level();
			break;
		case GS_CS_709_EXTENDED:
			convert_tech = "DrawMultiply";
			multiplier = 80.0f / obs_get_video_sdr_white_level();
			break;
		case GS_CS_709_SCRGB:
			break;
		}
	}

	if (convert_tech) {
		if (source->color_space_texrender) {
			if (gs_texrender_get_format(source->color_space_texrender) != format) {
				gs_texrender_destroy(source->color_space_texrender);
				source->color_space_texrender = NULL;
			}
		}

		if (!source->color_space_texrender) {
			source->color_space_texrender = gs_texrender_create(format, GS_ZS_NONE);
		}

		gs_texrender_reset(source->color_space_texrender);
		const int cx = get_base_width(source);
		const int cy = get_base_height(source);
		if (gs_texrender_begin_with_color_space(source->color_space_texrender, cx, cy, source_space)) {
			gs_enable_blending(false);

			struct vec4 clear_color;
			vec4_zero(&clear_color);
			gs_clear(GS_CLEAR_COLOR, &clear_color, 0.0f, 0);
			gs_ortho(0.0f, (float)cx, 0.0f, (float)cy, -100.0f, 100.0f);

			source->info.video_render(data, effect);

			gs_enable_blending(true);

			gs_texrender_end(source->color_space_texrender);

			gs_effect_t *default_effect = obs->video.default_effect;
			gs_technique_t *tech = gs_effect_get_technique(default_effect, convert_tech);

			const bool previous = gs_framebuffer_srgb_enabled();
			gs_enable_framebuffer_srgb(true);

			gs_texture_t *const tex = gs_texrender_get_texture(source->color_space_texrender);
			gs_effect_set_texture_srgb(gs_effect_get_param_by_name(default_effect, "image"), tex);
			gs_effect_set_float(gs_effect_get_param_by_name(default_effect, "multiplier"), multiplier);

			gs_blend_state_push();
			gs_blend_function(GS_BLEND_ONE, GS_BLEND_INVSRCALPHA);

			const size_t passes = gs_technique_begin(tech);
			for (size_t i = 0; i < passes; i++) {
				gs_technique_begin_pass(tech, i);
				gs_draw_sprite(tex, 0, 0, 0);
				gs_technique_end_pass(tech);
			}
			gs_technique_end(tech);

			gs_blend_state_pop();

			gs_enable_framebuffer_srgb(previous);
		}
	} else {
		source->info.video_render(data, effect);
	}
	source_profiler_source_render_end(source, start, timer);
}

void obs_source_default_render(obs_source_t *source)
{
	if (source->context.data) {
		gs_effect_t *effect = obs->video.default_effect;
		gs_technique_t *tech = gs_effect_get_technique(effect, "Draw");
		size_t passes, i;

		passes = gs_technique_begin(tech);
		for (i = 0; i < passes; i++) {
			gs_technique_begin_pass(tech, i);
			source_render(source, effect);
			gs_technique_end_pass(tech);
		}
		gs_technique_end(tech);
	}
}

static inline void obs_source_main_render(obs_source_t *source)
{
	uint32_t flags = source->info.output_flags;
	bool custom_draw = (flags & OBS_SOURCE_CUSTOM_DRAW) != 0;
	bool srgb_aware = (flags & OBS_SOURCE_SRGB) != 0;
	bool default_effect = !source->filter_parent && source->filters.num == 0 && !custom_draw;
	bool previous_srgb = false;

	if (!srgb_aware) {
		previous_srgb = gs_get_linear_srgb();
		gs_set_linear_srgb(false);
	}

	if (default_effect) {
		obs_source_default_render(source);
	} else if (source->context.data) {
		source_render(source, custom_draw ? NULL : gs_get_effect());
	}

	if (!srgb_aware)
		gs_set_linear_srgb(previous_srgb);
}

static bool ready_async_frame(obs_source_t *source, uint64_t sys_time);

#if GS_USE_DEBUG_MARKERS
static const char *get_type_format(enum obs_source_type type)
{
	switch (type) {
	case OBS_SOURCE_TYPE_INPUT:
		return "Input: %s";
	case OBS_SOURCE_TYPE_FILTER:
		return "Filter: %s";
	case OBS_SOURCE_TYPE_TRANSITION:
		return "Transition: %s";
	case OBS_SOURCE_TYPE_SCENE:
		return "Scene: %s";
	default:
		return "[Unknown]: %s";
	}
}
#endif

static inline void render_video(obs_source_t *source)
{
	if (source->info.type != OBS_SOURCE_TYPE_FILTER && (source->info.output_flags & OBS_SOURCE_VIDEO) == 0) {
		if (source->filter_parent)
			obs_source_skip_video_filter(source);
		return;
	}

	if (source->info.type == OBS_SOURCE_TYPE_INPUT && (source->info.output_flags & OBS_SOURCE_ASYNC) != 0 &&
	    !source->rendering_filter) {
		if (deinterlacing_enabled(source))
			deinterlace_update_async_video(source);
		obs_source_update_async_video(source);
	}

	if (!source->context.data || !source->enabled) {
		if (source->filter_parent)
			obs_source_skip_video_filter(source);
		return;
	}

	GS_DEBUG_MARKER_BEGIN_FORMAT(GS_DEBUG_COLOR_SOURCE, get_type_format(source->info.type),
				     obs_source_get_name(source));

	if (source->filters.num && !source->rendering_filter)
		obs_source_render_filters(source);

	else if (source->info.video_render)
		obs_source_main_render(source);

	else if (source->filter_target)
		obs_source_video_render(source->filter_target);

	else if (deinterlacing_enabled(source))
		deinterlace_render(source);

	else
		obs_source_render_async_video(source);

	GS_DEBUG_MARKER_END();
}

void obs_source_video_render(obs_source_t *source)
{
	if (!obs_source_valid(source, "obs_source_video_render"))
		return;

	source = obs_source_get_ref(source);
	if (source) {
		render_video(source);
		obs_source_release(source);
	}
}

static uint32_t get_recurse_width(obs_source_t *source)
{
	uint32_t width;

	pthread_mutex_lock(&source->filter_mutex);

	width = (source->filters.num) ? get_base_width(source->filters.array[0]) : get_base_width(source);

	pthread_mutex_unlock(&source->filter_mutex);

	return width;
}

static uint32_t get_recurse_height(obs_source_t *source)
{
	uint32_t height;

	pthread_mutex_lock(&source->filter_mutex);

	height = (source->filters.num) ? get_base_height(source->filters.array[0]) : get_base_height(source);

	pthread_mutex_unlock(&source->filter_mutex);

	return height;
}

uint32_t obs_source_get_width(obs_source_t *source)
{
	if (!data_valid(source, "obs_source_get_width"))
		return 0;

	return (source->info.type != OBS_SOURCE_TYPE_FILTER) ? get_recurse_width(source) : get_base_width(source);
}

uint32_t obs_source_get_height(obs_source_t *source)
{
	if (!data_valid(source, "obs_source_get_height"))
		return 0;

	return (source->info.type != OBS_SOURCE_TYPE_FILTER) ? get_recurse_height(source) : get_base_height(source);
}

enum gs_color_space obs_source_get_color_space(obs_source_t *source, size_t count,
					       const enum gs_color_space *preferred_spaces)
{
	if (!data_valid(source, "obs_source_get_color_space"))
		return GS_CS_SRGB;

	if (source->info.type != OBS_SOURCE_TYPE_FILTER && (source->info.output_flags & OBS_SOURCE_VIDEO) == 0) {
		if (source->filter_parent)
			return obs_source_get_color_space(source->filter_parent, count, preferred_spaces);
	}

	if (!source->context.data || !source->enabled) {
		if (source->filter_target)
			return obs_source_get_color_space(source->filter_target, count, preferred_spaces);
	}

	if (source->info.output_flags & OBS_SOURCE_ASYNC) {
		const enum gs_color_space video_space = convert_video_space(source->async_format, source->async_trc);

		enum gs_color_space space = video_space;
		for (size_t i = 0; i < count; ++i) {
			space = preferred_spaces[i];
			if (space == video_space)
				break;
		}

		return space;
	}

	assert(source->context.data);
	return source->info.video_get_color_space
		       ? source->info.video_get_color_space(source->context.data, count, preferred_spaces)
		       : GS_CS_SRGB;
}

uint32_t obs_source_get_base_width(obs_source_t *source)
{
	if (!data_valid(source, "obs_source_get_base_width"))
		return 0;

	return get_base_width(source);
}

uint32_t obs_source_get_base_height(obs_source_t *source)
{
	if (!data_valid(source, "obs_source_get_base_height"))
		return 0;

	return get_base_height(source);
}

obs_source_t *obs_filter_get_parent(const obs_source_t *filter)
{
	return obs_ptr_valid(filter, "obs_filter_get_parent") ? filter->filter_parent : NULL;
}

obs_source_t *obs_filter_get_target(const obs_source_t *filter)
{
	return obs_ptr_valid(filter, "obs_filter_get_target") ? filter->filter_target : NULL;
}

#define OBS_SOURCE_AV (OBS_SOURCE_ASYNC_VIDEO | OBS_SOURCE_AUDIO)

static bool filter_compatible(obs_source_t *source, obs_source_t *filter)
{
	uint32_t s_caps = source->info.output_flags & OBS_SOURCE_AV;
	uint32_t f_caps = filter->info.output_flags & OBS_SOURCE_AV;

	if ((f_caps & OBS_SOURCE_AUDIO) != 0 && (f_caps & OBS_SOURCE_VIDEO) == 0)
		f_caps &= ~OBS_SOURCE_ASYNC;

	return (s_caps & f_caps) == f_caps;
}

void obs_source_filter_add(obs_source_t *source, obs_source_t *filter)
{
	struct calldata cd;
	uint8_t stack[128];

	if (!obs_source_valid(source, "obs_source_filter_add"))
		return;
	if (!obs_ptr_valid(filter, "obs_source_filter_add"))
		return;

	pthread_mutex_lock(&source->filter_mutex);

	if (da_find(source->filters, &filter, 0) != DARRAY_INVALID) {
		blog(LOG_WARNING, "Tried to add a filter that was already "
				  "present on the source");
		pthread_mutex_unlock(&source->filter_mutex);
		return;
	}

	if (!source->owns_info_id && !filter_compatible(source, filter)) {
		pthread_mutex_unlock(&source->filter_mutex);
		return;
	}

	filter = obs_source_get_ref(filter);
	if (!obs_ptr_valid(filter, "obs_source_filter_add"))
		return;

	filter->filter_parent = source;
	filter->filter_target = !source->filters.num ? source : source->filters.array[0];

	da_insert(source->filters, 0, &filter);

	pthread_mutex_unlock(&source->filter_mutex);

	calldata_init_fixed(&cd, stack, sizeof(stack));
	calldata_set_ptr(&cd, "source", source);
	calldata_set_ptr(&cd, "filter", filter);

	signal_handler_signal(obs->signals, "source_filter_add", &cd);
	signal_handler_signal(source->context.signals, "filter_add", &cd);

	blog(LOG_DEBUG, "- filter '%s' (%s) added to source '%s'", filter->context.name, filter->info.id,
	     source->context.name);

	if (filter->info.filter_add)
		filter->info.filter_add(filter->context.data, filter->filter_parent);
}

static bool obs_source_filter_remove_refless(obs_source_t *source, obs_source_t *filter)
{
	struct calldata cd;
	uint8_t stack[128];
	size_t idx;

	pthread_mutex_lock(&source->filter_mutex);

	idx = da_find(source->filters, &filter, 0);
	if (idx == DARRAY_INVALID) {
		pthread_mutex_unlock(&source->filter_mutex);
		return false;
	}

	if (idx > 0) {
		obs_source_t *prev = source->filters.array[idx - 1];
		prev->filter_target = filter->filter_target;
	}

	da_erase(source->filters, idx);

	pthread_mutex_unlock(&source->filter_mutex);

	calldata_init_fixed(&cd, stack, sizeof(stack));
	calldata_set_ptr(&cd, "source", source);
	calldata_set_ptr(&cd, "filter", filter);

	signal_handler_signal(obs->signals, "source_filter_remove", &cd);
	signal_handler_signal(source->context.signals, "filter_remove", &cd);

	blog(LOG_DEBUG, "- filter '%s' (%s) removed from source '%s'", filter->context.name, filter->info.id,
	     source->context.name);

	if (filter->info.filter_remove)
		filter->info.filter_remove(filter->context.data, filter->filter_parent);

	filter->filter_parent = NULL;
	filter->filter_target = NULL;
	return true;
}

void obs_source_filter_remove(obs_source_t *source, obs_source_t *filter)
{
	if (!obs_source_valid(source, "obs_source_filter_remove"))
		return;
	if (!obs_ptr_valid(filter, "obs_source_filter_remove"))
		return;

	if (obs_source_filter_remove_refless(source, filter))
		obs_source_release(filter);
}

static size_t find_next_filter(obs_source_t *source, obs_source_t *filter, size_t cur_idx)
{
	bool curAsync = (filter->info.output_flags & OBS_SOURCE_ASYNC) != 0;
	bool nextAsync;
	obs_source_t *next;

	if (cur_idx == source->filters.num - 1)
		return DARRAY_INVALID;

	next = source->filters.array[cur_idx + 1];
	nextAsync = (next->info.output_flags & OBS_SOURCE_ASYNC);

	if (nextAsync == curAsync)
		return cur_idx + 1;
	else
		return find_next_filter(source, filter, cur_idx + 1);
}

static size_t find_prev_filter(obs_source_t *source, obs_source_t *filter, size_t cur_idx)
{
	bool curAsync = (filter->info.output_flags & OBS_SOURCE_ASYNC) != 0;
	bool prevAsync;
	obs_source_t *prev;

	if (cur_idx == 0)
		return DARRAY_INVALID;

	prev = source->filters.array[cur_idx - 1];
	prevAsync = (prev->info.output_flags & OBS_SOURCE_ASYNC);

	if (prevAsync == curAsync)
		return cur_idx - 1;
	else
		return find_prev_filter(source, filter, cur_idx - 1);
}

static void reorder_filter_targets(obs_source_t *source)
{
	/* reorder filter targets, not the nicest way of dealing with things */
	for (size_t i = 0; i < source->filters.num; i++) {
		obs_source_t *next_filter = (i == source->filters.num - 1) ? source : source->filters.array[i + 1];

		source->filters.array[i]->filter_target = next_filter;
	}
}

/* moves filters above/below matching filter types */
static bool move_filter_dir(obs_source_t *source, obs_source_t *filter, enum obs_order_movement movement)
{
	size_t idx;

	idx = da_find(source->filters, &filter, 0);
	if (idx == DARRAY_INVALID)
		return false;

	if (movement == OBS_ORDER_MOVE_UP) {
		size_t next_id = find_next_filter(source, filter, idx);
		if (next_id == DARRAY_INVALID)
			return false;
		da_move_item(source->filters, idx, next_id);

	} else if (movement == OBS_ORDER_MOVE_DOWN) {
		size_t prev_id = find_prev_filter(source, filter, idx);
		if (prev_id == DARRAY_INVALID)
			return false;
		da_move_item(source->filters, idx, prev_id);

	} else if (movement == OBS_ORDER_MOVE_TOP) {
		if (idx == source->filters.num - 1)
			return false;
		da_move_item(source->filters, idx, source->filters.num - 1);

	} else if (movement == OBS_ORDER_MOVE_BOTTOM) {
		if (idx == 0)
			return false;
		da_move_item(source->filters, idx, 0);
	}

	reorder_filter_targets(source);

	return true;
}

void obs_source_filter_set_order(obs_source_t *source, obs_source_t *filter, enum obs_order_movement movement)
{
	bool success;

	if (!obs_source_valid(source, "obs_source_filter_set_order"))
		return;
	if (!obs_ptr_valid(filter, "obs_source_filter_set_order"))
		return;

	pthread_mutex_lock(&source->filter_mutex);
	success = move_filter_dir(source, filter, movement);
	pthread_mutex_unlock(&source->filter_mutex);

	if (success)
		obs_source_dosignal(source, NULL, "reorder_filters");
}

int obs_source_filter_get_index(obs_source_t *source, obs_source_t *filter)
{
	if (!obs_source_valid(source, "obs_source_filter_get_index"))
		return -1;
	if (!obs_ptr_valid(filter, "obs_source_filter_get_index"))
		return -1;

	size_t idx;

	pthread_mutex_lock(&source->filter_mutex);
	idx = da_find(source->filters, &filter, 0);
	pthread_mutex_unlock(&source->filter_mutex);

	return idx != DARRAY_INVALID ? (int)idx : -1;
}

static bool set_filter_index(obs_source_t *source, obs_source_t *filter, size_t index)
{
	size_t idx = da_find(source->filters, &filter, 0);
	if (idx == DARRAY_INVALID)
		return false;

	da_move_item(source->filters, idx, index);
	reorder_filter_targets(source);

	return true;
}

void obs_source_filter_set_index(obs_source_t *source, obs_source_t *filter, size_t index)
{
	bool success;

	if (!obs_source_valid(source, "obs_source_filter_set_index"))
		return;
	if (!obs_ptr_valid(filter, "obs_source_filter_set_index"))
		return;

	pthread_mutex_lock(&source->filter_mutex);
	success = set_filter_index(source, filter, index);
	pthread_mutex_unlock(&source->filter_mutex);

	if (success)
		obs_source_dosignal(source, NULL, "reorder_filters");
}

obs_data_t *obs_source_get_settings(const obs_source_t *source)
{
	if (!obs_source_valid(source, "obs_source_get_settings"))
		return NULL;

	obs_data_addref(source->context.settings);
	return source->context.settings;
}

struct obs_source_frame *filter_async_video(obs_source_t *source, struct obs_source_frame *in)
{
	size_t i;

	pthread_mutex_lock(&source->filter_mutex);

	for (i = source->filters.num; i > 0; i--) {
		struct obs_source *filter = source->filters.array[i - 1];

		if (!filter->enabled)
			continue;

		if (filter->context.data && filter->info.filter_video) {
			in = filter->info.filter_video(filter->context.data, in);
			if (!in)
				break;
		}
	}

	pthread_mutex_unlock(&source->filter_mutex);

	return in;
}

static inline void copy_frame_data_line(struct obs_source_frame *dst, const struct obs_source_frame *src,
					uint32_t plane, uint32_t y)
{
	uint32_t pos_src = y * src->linesize[plane];
	uint32_t pos_dst = y * dst->linesize[plane];
	uint32_t bytes = dst->linesize[plane] < src->linesize[plane] ? dst->linesize[plane] : src->linesize[plane];

	memcpy(dst->data[plane] + pos_dst, src->data[plane] + pos_src, bytes);
}

static inline void copy_frame_data_plane(struct obs_source_frame *dst, const struct obs_source_frame *src,
					 uint32_t plane, uint32_t lines)
{
	if (dst->linesize[plane] != src->linesize[plane]) {
		for (uint32_t y = 0; y < lines; y++)
			copy_frame_data_line(dst, src, plane, y);
	} else {
		memcpy(dst->data[plane], src->data[plane], (size_t)dst->linesize[plane] * (size_t)lines);
	}
}

static void copy_frame_data(struct obs_source_frame *dst, const struct obs_source_frame *src)
{
	dst->flip = src->flip;
	dst->flags = src->flags;
	dst->trc = src->trc;
	dst->full_range = src->full_range;
	dst->max_luminance = src->max_luminance;
	dst->timestamp = src->timestamp;
	memcpy(dst->color_matrix, src->color_matrix, sizeof(float) * 16);
	if (!dst->full_range) {
		size_t const size = sizeof(float) * 3;
		memcpy(dst->color_range_min, src->color_range_min, size);
		memcpy(dst->color_range_max, src->color_range_max, size);
	}

	switch (src->format) {
	case VIDEO_FORMAT_I420:
	case VIDEO_FORMAT_I010: {
		const uint32_t height = dst->height;
		const uint32_t half_height = (height + 1) / 2;
		copy_frame_data_plane(dst, src, 0, height);
		copy_frame_data_plane(dst, src, 1, half_height);
		copy_frame_data_plane(dst, src, 2, half_height);
		break;
	}

	case VIDEO_FORMAT_NV12:
	case VIDEO_FORMAT_P010: {
		const uint32_t height = dst->height;
		const uint32_t half_height = (height + 1) / 2;
		copy_frame_data_plane(dst, src, 0, height);
		copy_frame_data_plane(dst, src, 1, half_height);
		break;
	}

	case VIDEO_FORMAT_I444:
	case VIDEO_FORMAT_I422:
	case VIDEO_FORMAT_I210:
	case VIDEO_FORMAT_I412:
		copy_frame_data_plane(dst, src, 0, dst->height);
		copy_frame_data_plane(dst, src, 1, dst->height);
		copy_frame_data_plane(dst, src, 2, dst->height);
		break;

	case VIDEO_FORMAT_YVYU:
	case VIDEO_FORMAT_YUY2:
	case VIDEO_FORMAT_UYVY:
	case VIDEO_FORMAT_NONE:
	case VIDEO_FORMAT_RGBA:
	case VIDEO_FORMAT_BGRA:
	case VIDEO_FORMAT_BGRX:
	case VIDEO_FORMAT_Y800:
	case VIDEO_FORMAT_BGR3:
	case VIDEO_FORMAT_AYUV:
	case VIDEO_FORMAT_V210:
	case VIDEO_FORMAT_R10L:
		copy_frame_data_plane(dst, src, 0, dst->height);
		break;

	case VIDEO_FORMAT_I40A: {
		const uint32_t height = dst->height;
		const uint32_t half_height = (height + 1) / 2;
		copy_frame_data_plane(dst, src, 0, height);
		copy_frame_data_plane(dst, src, 1, half_height);
		copy_frame_data_plane(dst, src, 2, half_height);
		copy_frame_data_plane(dst, src, 3, height);
		break;
	}

	case VIDEO_FORMAT_I42A:
	case VIDEO_FORMAT_YUVA:
	case VIDEO_FORMAT_YA2L:
		copy_frame_data_plane(dst, src, 0, dst->height);
		copy_frame_data_plane(dst, src, 1, dst->height);
		copy_frame_data_plane(dst, src, 2, dst->height);
		copy_frame_data_plane(dst, src, 3, dst->height);
		break;

	case VIDEO_FORMAT_P216:
	case VIDEO_FORMAT_P416:
		/* Unimplemented */
		break;
	}
}

void obs_source_frame_copy(struct obs_source_frame *dst, const struct obs_source_frame *src)
{
	copy_frame_data(dst, src);
}

static inline bool async_texture_changed(struct obs_source *source, const struct obs_source_frame *frame)
{
	enum convert_type prev, cur;
	prev = get_convert_type(source->async_cache_format, source->async_cache_full_range, source->async_cache_trc);
	cur = get_convert_type(frame->format, frame->full_range, frame->trc);

	return source->async_cache_width != frame->width || source->async_cache_height != frame->height || prev != cur;
}

static inline void free_async_cache(struct obs_source *source)
{
	for (size_t i = 0; i < source->async_cache.num; i++)
		obs_source_frame_decref(source->async_cache.array[i].frame);

	da_resize(source->async_cache, 0);
	da_resize(source->async_frames, 0);
	source->cur_async_frame = NULL;
	source->prev_async_frame = NULL;
}

#define MAX_UNUSED_FRAME_DURATION 5

/* frees frame allocations if they haven't been used for a specific period
 * of time */
static void clean_cache(obs_source_t *source)
{
	for (size_t i = source->async_cache.num; i > 0; i--) {
		struct async_frame *af = &source->async_cache.array[i - 1];
		if (!af->used) {
			if (++af->unused_count == MAX_UNUSED_FRAME_DURATION) {
				obs_source_frame_destroy(af->frame);
				da_erase(source->async_cache, i - 1);
			}
		}
	}
}

#define MAX_ASYNC_FRAMES 30

/* forward decl (#97): per-source async-FIFO drop-cap. Defined below alongside the
 * genlock preload helpers, but used here by cache_video(); without this prototype
 * MSVC assumes implicit `extern int genlock_source_drop_cap()` (C4013 -> C2220
 * warning-as-error) and the real `static size_t(const obs_source_t*)` definition
 * then clashes as a redefinition (C2371). */
static size_t genlock_source_drop_cap(const obs_source_t *source);

//if return value is not null then do (os_atomic_dec_long(&output->refs) == 0) && obs_source_frame_destroy(output)
static inline struct obs_source_frame *cache_video(struct obs_source *source, const struct obs_source_frame *frame)
{
	struct obs_source_frame *new_frame = NULL;

	pthread_mutex_lock(&source->async_mutex);

	/* camera-box #97: the drop-cap is PER-SOURCE. A non-genlock source keeps the
	 * fixed MAX_ASYNC_FRAMES; a genlock source the operator deliberately delays is
	 * allowed to hold preload+RESERVE frames so its full delay buffer parks without
	 * force-draining (only delayed sources hold a big buffer -- memory-safe on the
	 * RAM-tight stream.lan box, #89). Read under async_mutex (already held here),
	 * same lock as obs_source_set_genlock_preload(). */
	if (source->async_frames.num >= genlock_source_drop_cap(source)) {
		/* camera-box #70/#97: the FIFO hit the per-source drop-cap and is
		 * force-drained. For a genlock source this is an overrun - the
		 * consumer fell behind the producer (or the preload is deeper than
		 * the cap allows). Count it; the audit log surfaces it so the preload
		 * depth can be tuned. */
		if (source->genlock_fifo)
			source->genlock_overruns++;
		/* camera-box #1003: the force-drain destroys the whole delay line, so the
		 * remembered on-air age describes nothing -- a relock firing before the next
		 * STEADY/GAP present would target a pre-drain phase. Same seam as the flush
		 * clear below. */
		source->genlock_phase_anchor_ns = 0;
		free_async_cache(source);
		source->genlock_acquire_bracket_ticks = 0; /* #1161: the delay line is gone -> a new ACQUIRE episode; the bracket-hold counter must not carry a stale count into it (fail-open cap seam). */
		source->last_frame_ts = 0;
		/* camera-box #102: an overrun force-drained the FIFO to empty, so the
		 * delay line is gone — re-enter the BUILD phase to rebuild the preload
		 * delay before emitting again (otherwise the next tick would emit a
		 * frame with no delay, a phase jump). */
		source->genlock_filled = false;
		/* camera-box #126: the overrun drain already re-arms the build latch above;
		 * clear the consecutive-empty run so a stale count can't carry into the rebuild
		 * (the post-drain re-bootstrap empties are excluded by last_frame_ts=0 anyway). */
		source->genlock_empty_run = 0;
		pthread_mutex_unlock(&source->async_mutex);
		return NULL;
	}

	if (async_texture_changed(source, frame)) {
		/* camera-box #1003: a format/size change re-allocs the cache and drops the
		 * delay line with it -- the remembered age no longer describes anything. */
		source->genlock_phase_anchor_ns = 0;
		free_async_cache(source);
		source->genlock_acquire_bracket_ticks = 0; /* #1161: the delay line is gone -> a new ACQUIRE episode; the bracket-hold counter must not carry a stale count into it (fail-open cap seam). */
		source->async_cache_width = frame->width;
		source->async_cache_height = frame->height;
	}

	const enum video_format format = frame->format;
	source->async_cache_format = format;
	source->async_cache_full_range = frame->full_range;
	source->async_cache_trc = frame->trc;

	for (size_t i = 0; i < source->async_cache.num; i++) {
		struct async_frame *af = &source->async_cache.array[i];
		if (!af->used) {
			new_frame = af->frame;
			new_frame->format = format;
			af->used = true;
			af->unused_count = 0;
			break;
		}
	}

	clean_cache(source);

	if (!new_frame) {
		struct async_frame new_af;

		new_frame = obs_source_frame_create(format, frame->width, frame->height);
		new_af.frame = new_frame;
		new_af.used = true;
		new_af.unused_count = 0;
		new_frame->refs = 1;

		da_push_back(source->async_cache, &new_af);
	}

	os_atomic_inc_long(&new_frame->refs);

	pthread_mutex_unlock(&source->async_mutex);

	copy_frame_data(new_frame, frame);

	return new_frame;
}

static void obs_source_output_video_internal(obs_source_t *source, const struct obs_source_frame *frame)
{
	if (!obs_source_valid(source, "obs_source_output_video"))
		return;

	if (!frame) {
		pthread_mutex_lock(&source->async_mutex);
		source->async_active = false;
		source->last_frame_ts = 0;
		/* camera-box #102: the source went inactive (flushed) — the delay line is
		 * gone, so re-arm the startup-fill latch. On resume the FIFO rebuilds the
		 * preload delay before emitting again instead of leaking one undelayed frame
		 * (the stale filled=true + the bootstrap path). Written under async_mutex. */
		source->genlock_filled = false;
		/* camera-box #126: an explicit flush re-arms the latch here; clear the
		 * consecutive-empty run too so it can't carry a stale count into the rebuild. */
		source->genlock_empty_run = 0;
		/* camera-box #741/#707 B2: the source went inactive (flushed) — the whole delay
		 * line is gone, so the last CONFIRMED source-rate multiple is genuinely stale.
		 * Clear it HERE (the reset site the #726 clears missed) so a resumed source
		 * re-confirms N from scratch instead of trusting a pre-flush latch. Mirror: a
		 * fresh src/probe/genlock.rs ReleaseCadence starts with last_known_n = 0. */
		source->genlock_last_known_n = 0;
		/* camera-box #1003: the delay line is gone, so the remembered on-air age
		 * describes nothing. Clear it here (the same reset site the #741/#707 B2
		 * sticky-N clear covers) so a resumed source re-establishes the phase from
		 * its configured latency instead of inheriting a pre-flush one. */
		source->genlock_phase_anchor_ns = 0;
		free_async_cache(source);
		source->genlock_acquire_bracket_ticks = 0; /* #1161: the delay line is gone -> a new ACQUIRE episode; the bracket-hold counter must not carry a stale count into it (fail-open cap seam). */
		pthread_mutex_unlock(&source->async_mutex);
		return;
	}

	source_profiler_async_frame_received(source);

	/* camera-box #797 instrumentation: time the two candidate stall stages of the
	 * receive-thread submit path — cache_video (async_mutex #1 wait + possible alloc +
	 * 4MB copy_frame_data) and the async_mutex #2 wait below. Rate-limited slow-call
	 * log (>5ms total, max ~1 line/s) with a cumulative slow counter, so a systematic
	 * ~3-4ms/frame stall (the 50-of-60fps pull-loop quantizer) is unmissable in the
	 * imag OBS log within seconds. Pure diagnosis — remove after #797 closes. */
	uint64_t iv797_t0 = os_gettime_ns();

	struct obs_source_frame *output = cache_video(source, frame);

	uint64_t iv797_t1 = os_gettime_ns();
	/* ------------------------------------------- */
	pthread_mutex_lock(&source->async_mutex);
	uint64_t iv797_t2 = os_gettime_ns();
	if (output) {
		if (os_atomic_dec_long(&output->refs) == 0) {
			obs_source_frame_destroy(output);
			output = NULL;
		} else {
			da_push_back(source->async_frames, &output);
			source->async_active = true;
			if (source->genlock_fifo) {
				source->genlock_frames_received++; /* #70 audit */
				/* camera-box #99: fold the PRODUCER-side queue depth into the
				 * peak high-water mark. The #70 peak was updated ONLY on the
				 * consumer side (ready_async_frame, at the render-tick consume),
				 * so a producer burst that pushed the FIFO high BETWEEN two render
				 * ticks and drained before the next tick was never observed — the
				 * peak under-reported how close the queue got to the drop-cap
				 * (the audit log's whole purpose: tuning preload). num here is the
				 * depth right AFTER this push = the producer's high-water mark.
				 * Written under async_mutex (held here), same lock as the consumer
				 * update. The "peak = max(peak, observed)" rule is the pure reference
				 * genlock_peak_update() in src/probe/genlock.rs; this inline C update
				 * (and the consumer-side one) are pinned to exist by the
				 * tests/genlock_preload.rs vendored-source guard. */
				const uint32_t depth = (uint32_t)source->async_frames.num;
				if (depth > source->genlock_peak_depth)
					source->genlock_peak_depth = depth;
			}
		}
	}
	pthread_mutex_unlock(&source->async_mutex);

	{
		/* #797: see the comment above iv797_t0. */
		uint64_t iv797_t3 = os_gettime_ns();
		if (iv797_t3 - iv797_t0 > 5000000ULL) {
			static volatile long iv797_slow_count = 0;
			static uint64_t iv797_last_log_ns = 0;
			os_atomic_inc_long((long *)&iv797_slow_count);
			if (iv797_t3 - iv797_last_log_ns > 1000000000ULL) {
				iv797_last_log_ns = iv797_t3;
				blog(LOG_INFO,
				     "genlock #797 slow output_video '%s': total=%.2fms cache_video=%.2fms lock2_wait=%.2fms (slow_total=%ld)",
				     obs_source_get_name(source), (double)(iv797_t3 - iv797_t0) / 1e6,
				     (double)(iv797_t1 - iv797_t0) / 1e6, (double)(iv797_t2 - iv797_t1) / 1e6,
				     iv797_slow_count);
			}
		}
	}
}

void obs_source_output_video(obs_source_t *source, const struct obs_source_frame *frame)
{
	if (destroying(source))
		return;
	if (!frame) {
		obs_source_output_video_internal(source, NULL);
		return;
	}

	struct obs_source_frame new_frame = *frame;
	new_frame.full_range = format_is_yuv(frame->format) ? new_frame.full_range : true;

	obs_source_output_video_internal(source, &new_frame);
}

void obs_source_output_video2(obs_source_t *source, const struct obs_source_frame2 *frame)
{
	if (destroying(source))
		return;
	if (!frame) {
		obs_source_output_video_internal(source, NULL);
		return;
	}

	struct obs_source_frame new_frame = {0};
	enum video_range_type range = resolve_video_range(frame->format, frame->range);

	for (size_t i = 0; i < MAX_AV_PLANES; i++) {
		new_frame.data[i] = frame->data[i];
		new_frame.linesize[i] = frame->linesize[i];
	}

	new_frame.width = frame->width;
	new_frame.height = frame->height;
	new_frame.timestamp = frame->timestamp;
	new_frame.format = frame->format;
	new_frame.full_range = range == VIDEO_RANGE_FULL;
	new_frame.max_luminance = 0;
	new_frame.flip = frame->flip;
	new_frame.flags = frame->flags;
	new_frame.trc = frame->trc;

	memcpy(&new_frame.color_matrix, &frame->color_matrix, sizeof(frame->color_matrix));
	memcpy(&new_frame.color_range_min, &frame->color_range_min, sizeof(frame->color_range_min));
	memcpy(&new_frame.color_range_max, &frame->color_range_max, sizeof(frame->color_range_max));

	obs_source_output_video_internal(source, &new_frame);
}

void obs_source_set_async_rotation(obs_source_t *source, long rotation)
{
	if (source)
		source->async_rotation = rotation;
}

void obs_source_output_cea708(obs_source_t *source, const struct obs_source_cea_708 *captions)
{
	if (destroying(source))
		return;
	if (!captions) {
		return;
	}

	pthread_mutex_lock(&source->caption_cb_mutex);

	for (size_t i = source->caption_cb_list.num; i > 0; i--) {
		struct caption_cb_info info = source->caption_cb_list.array[i - 1];
		info.callback(info.param, source, captions);
	}

	pthread_mutex_unlock(&source->caption_cb_mutex);
}

void obs_source_add_caption_callback(obs_source_t *source, obs_source_caption_t callback, void *param)
{
	struct caption_cb_info info = {callback, param};

	if (!obs_source_valid(source, "obs_source_add_caption_callback"))
		return;

	pthread_mutex_lock(&source->caption_cb_mutex);
	da_push_back(source->caption_cb_list, &info);
	pthread_mutex_unlock(&source->caption_cb_mutex);
}

void obs_source_remove_caption_callback(obs_source_t *source, obs_source_caption_t callback, void *param)
{
	struct caption_cb_info info = {callback, param};

	if (!obs_source_valid(source, "obs_source_remove_caption_callback"))
		return;

	pthread_mutex_lock(&source->caption_cb_mutex);
	da_erase_item(source->caption_cb_list, &info);
	pthread_mutex_unlock(&source->caption_cb_mutex);
}

static inline bool preload_frame_changed(obs_source_t *source, const struct obs_source_frame *in)
{
	if (!source->async_preload_frame)
		return true;

	return in->width != source->async_preload_frame->width || in->height != source->async_preload_frame->height ||
	       in->format != source->async_preload_frame->format;
}

static void obs_source_preload_video_internal(obs_source_t *source, const struct obs_source_frame *frame)
{
	if (!obs_source_valid(source, "obs_source_preload_video"))
		return;
	if (destroying(source))
		return;
	if (!frame)
		return;

	if (preload_frame_changed(source, frame)) {
		obs_source_frame_destroy(source->async_preload_frame);
		source->async_preload_frame = obs_source_frame_create(frame->format, frame->width, frame->height);
	}

	copy_frame_data(source->async_preload_frame, frame);

	source->last_frame_ts = frame->timestamp;
}

void obs_source_preload_video(obs_source_t *source, const struct obs_source_frame *frame)
{
	if (destroying(source))
		return;
	if (!frame) {
		obs_source_preload_video_internal(source, NULL);
		return;
	}

	struct obs_source_frame new_frame = *frame;
	new_frame.full_range = format_is_yuv(frame->format) ? new_frame.full_range : true;

	obs_source_preload_video_internal(source, &new_frame);
}

void obs_source_preload_video2(obs_source_t *source, const struct obs_source_frame2 *frame)
{
	if (destroying(source))
		return;
	if (!frame) {
		obs_source_preload_video_internal(source, NULL);
		return;
	}

	struct obs_source_frame new_frame = {0};
	enum video_range_type range = resolve_video_range(frame->format, frame->range);

	for (size_t i = 0; i < MAX_AV_PLANES; i++) {
		new_frame.data[i] = frame->data[i];
		new_frame.linesize[i] = frame->linesize[i];
	}

	new_frame.width = frame->width;
	new_frame.height = frame->height;
	new_frame.timestamp = frame->timestamp;
	new_frame.format = frame->format;
	new_frame.full_range = range == VIDEO_RANGE_FULL;
	new_frame.max_luminance = 0;
	new_frame.flip = frame->flip;
	new_frame.flags = frame->flags;
	new_frame.trc = frame->trc;

	memcpy(&new_frame.color_matrix, &frame->color_matrix, sizeof(frame->color_matrix));
	memcpy(&new_frame.color_range_min, &frame->color_range_min, sizeof(frame->color_range_min));
	memcpy(&new_frame.color_range_max, &frame->color_range_max, sizeof(frame->color_range_max));

	obs_source_preload_video_internal(source, &new_frame);
}

void obs_source_show_preloaded_video(obs_source_t *source)
{
	uint64_t sys_ts;

	if (!obs_source_valid(source, "obs_source_show_preloaded_video"))
		return;
	if (destroying(source))
		return;
	if (!source->async_preload_frame)
		return;

	obs_enter_graphics();

	set_async_texture_size(source, source->async_preload_frame);
	update_async_textures(source, source->async_preload_frame, source->async_textures, source->async_texrender);
	source->async_active = true;

	obs_leave_graphics();

	pthread_mutex_lock(&source->audio_buf_mutex);
	sys_ts = (source->monitoring_type != OBS_MONITORING_TYPE_MONITOR_ONLY) ? os_gettime_ns() : 0;
	reset_audio_timing(source, source->last_frame_ts, sys_ts);
	reset_audio_data(source, sys_ts);
	pthread_mutex_unlock(&source->audio_buf_mutex);
}

static void obs_source_set_video_frame_internal(obs_source_t *source, const struct obs_source_frame *frame)
{
	if (!obs_source_valid(source, "obs_source_set_video_frame"))
		return;
	if (!frame)
		return;

	obs_enter_graphics();

	if (preload_frame_changed(source, frame)) {
		obs_source_frame_destroy(source->async_preload_frame);
		source->async_preload_frame = obs_source_frame_create(frame->format, frame->width, frame->height);
	}

	copy_frame_data(source->async_preload_frame, frame);
	set_async_texture_size(source, source->async_preload_frame);
	update_async_textures(source, source->async_preload_frame, source->async_textures, source->async_texrender);

	source->last_frame_ts = frame->timestamp;

	obs_leave_graphics();
}

void obs_source_set_video_frame(obs_source_t *source, const struct obs_source_frame *frame)
{
	if (destroying(source))
		return;
	if (!frame) {
		obs_source_preload_video_internal(source, NULL);
		return;
	}

	struct obs_source_frame new_frame = *frame;
	new_frame.full_range = format_is_yuv(frame->format) ? new_frame.full_range : true;

	obs_source_set_video_frame_internal(source, &new_frame);
}

void obs_source_set_video_frame2(obs_source_t *source, const struct obs_source_frame2 *frame)
{
	if (destroying(source))
		return;
	if (!frame) {
		obs_source_preload_video_internal(source, NULL);
		return;
	}

	struct obs_source_frame new_frame = {0};
	enum video_range_type range = resolve_video_range(frame->format, frame->range);

	for (size_t i = 0; i < MAX_AV_PLANES; i++) {
		new_frame.data[i] = frame->data[i];
		new_frame.linesize[i] = frame->linesize[i];
	}

	new_frame.width = frame->width;
	new_frame.height = frame->height;
	new_frame.timestamp = frame->timestamp;
	new_frame.format = frame->format;
	new_frame.full_range = range == VIDEO_RANGE_FULL;
	new_frame.max_luminance = 0;
	new_frame.flip = frame->flip;
	new_frame.flags = frame->flags;
	new_frame.trc = frame->trc;

	memcpy(&new_frame.color_matrix, &frame->color_matrix, sizeof(frame->color_matrix));
	memcpy(&new_frame.color_range_min, &frame->color_range_min, sizeof(frame->color_range_min));
	memcpy(&new_frame.color_range_max, &frame->color_range_max, sizeof(frame->color_range_max));

	obs_source_set_video_frame_internal(source, &new_frame);
}

static inline struct obs_audio_data *filter_async_audio(obs_source_t *source, struct obs_audio_data *in)
{
	size_t i;
	for (i = source->filters.num; i > 0; i--) {
		struct obs_source *filter = source->filters.array[i - 1];

		if (!filter->enabled)
			continue;

		if (filter->context.data && filter->info.filter_audio) {
			in = filter->info.filter_audio(filter->context.data, in);
			if (!in)
				return NULL;
		}
	}

	return in;
}

static inline void reset_resampler(obs_source_t *source, const struct obs_source_audio *audio)
{
	const struct audio_output_info *obs_info;
	struct resample_info output_info;

	obs_info = audio_output_get_info(obs->audio.audio);

	output_info.format = obs_info->format;
	output_info.samples_per_sec = obs_info->samples_per_sec;
	output_info.speakers = obs_info->speakers;

	source->sample_info.format = audio->format;
	source->sample_info.samples_per_sec = audio->samples_per_sec;
	source->sample_info.speakers = audio->speakers;

	audio_resampler_destroy(source->resampler);
	source->resampler = NULL;
	source->resample_offset = 0;
	/* camera-box #803: a format change re-arms the servo's lock delay -- the swresample
	 * context that carries the compensation state was just torn down, so any prior applied
	 * ppm no longer means anything to the new one. Default-safe: start at 0ppm/no-lock
	 * exactly like source creation. */
	asrc_compensator_init(&source->asrc);
	source->asrc_has_last_wall = false;

	const bool formats_match = source->sample_info.samples_per_sec == obs_info->samples_per_sec &&
				    source->sample_info.format == obs_info->format &&
				    source->sample_info.speakers == obs_info->speakers;

	/* camera-box #803: an ASRC-enabled source ALWAYS gets a resampler, even when its declared
	 * sample rate already matches the mix -- the servo needs a real swresample context to
	 * apply audio_resampler_set_compensation_ppm() through (the fast-path "just copy the
	 * samples" branch below has no such context). A non-ASRC source keeps the original
	 * fast-path behavior unchanged. */
	if (formats_match && !source->asrc_enabled) {
		source->audio_failed = false;
		return;
	}

	source->resampler = audio_resampler_create(&output_info, &source->sample_info);

	source->audio_failed = source->resampler == NULL;
	if (source->resampler == NULL)
		blog(LOG_ERROR, "creation of resampler failed");
}

static void copy_audio_data(obs_source_t *source, const uint8_t *const data[], uint32_t frames, uint64_t ts)
{
	size_t planes = audio_output_get_planes(obs->audio.audio);
	size_t blocksize = audio_output_get_block_size(obs->audio.audio);
	size_t size = (size_t)frames * blocksize;
	bool resize = source->audio_storage_size < size;

	source->audio_data.frames = frames;
	source->audio_data.timestamp = ts;

	for (size_t i = 0; i < planes; i++) {
		/* ensure audio storage capacity */
		if (resize) {
			bfree(source->audio_data.data[i]);
			source->audio_data.data[i] = bmalloc(size);
		}

		memcpy(source->audio_data.data[i], data[i], size);
	}

	if (resize)
		source->audio_storage_size = size;
}

/* TODO: SSE optimization */
static void downmix_to_mono_planar(struct obs_source *source, uint32_t frames)
{
	size_t channels = audio_output_get_channels(obs->audio.audio);
	const float channels_i = 1.0f / (float)channels;
	float **data = (float **)source->audio_data.data;

	for (size_t channel = 1; channel < channels; channel++) {
		for (uint32_t frame = 0; frame < frames; frame++)
			data[0][frame] += data[channel][frame];
	}

	for (uint32_t frame = 0; frame < frames; frame++)
		data[0][frame] *= channels_i;

	for (size_t channel = 1; channel < channels; channel++) {
		for (uint32_t frame = 0; frame < frames; frame++)
			data[channel][frame] = data[0][frame];
	}
}

static void process_audio_balancing(struct obs_source *source, uint32_t frames, float balance,
				    enum obs_balance_type type)
{
	float **data = (float **)source->audio_data.data;

	switch (type) {
	case OBS_BALANCE_TYPE_SINE_LAW:
		for (uint32_t frame = 0; frame < frames; frame++) {
			data[0][frame] = data[0][frame] * sinf((1.0f - balance) * (M_PI / 2.0f));
			data[1][frame] = data[1][frame] * sinf(balance * (M_PI / 2.0f));
		}
		break;
	case OBS_BALANCE_TYPE_SQUARE_LAW:
		for (uint32_t frame = 0; frame < frames; frame++) {
			data[0][frame] = data[0][frame] * sqrtf(1.0f - balance);
			data[1][frame] = data[1][frame] * sqrtf(balance);
		}
		break;
	case OBS_BALANCE_TYPE_LINEAR:
		for (uint32_t frame = 0; frame < frames; frame++) {
			data[0][frame] = data[0][frame] * (1.0f - balance);
			data[1][frame] = data[1][frame] * balance;
		}
		break;
	default:
		break;
	}
}

/* resamples/remixes new audio to the designated main audio output format */
/* forward decl (#803): genlock_wall_now_ns() is defined further down (the video-FIFO wall-clock
 * helper) but process_audio() needs it as the ASRC servo's master-clock basis -- same forward-decl
 * convention as genlock_source_drop_cap() above (without it MSVC assumes an implicit int-returning
 * extern, C4013 -> C2220 as an error). */
static inline uint64_t genlock_wall_now_ns(void);

/* camera-box #1016: the ASRC servo's compensation-application window, in OUTPUT milliseconds --
 * passed to audio_resampler_set_compensation_ppm() below as its distance_ms argument. This sets
 * the achievable resolution ("quantum") of the whole mechanism: swr_set_compensation() only takes
 * an INTEGER sample count, so distance_samples = output_freq*distance_ms/1000, and any |ppm|
 * under HALF a quantum (1e6/distance_samples) rounds to a complete no-op. At the ORIGINAL 1000ms
 * (1s) window and the fleet's 48kHz mix rate, that floor was ~10.42ppm -- squarely inside issue
 * 929's own characterization of real observed drift as "typically single-digit ppm" (measured
 * live: requested 5ppm -> achieved 0.0000ppm, i.e. the servo was doing nothing for the common
 * case). Widening to 10000ms (10s) lowers the floor to ~1.04ppm, covering essentially all of that
 * range -- a purely stateless change (no new per-resampler state; the achieved rate depends only
 * on distance_samples, verified empirically at several distance_ms values against real
 * libswresample, see scripts/asrc-quality-bench/RESULTS-1016.md). Does NOT touch the re-issue
 * cadence below (still every audio callback, unchanged) -- the re-trigger-cadence THD+N cost
 * (reissuing swr_set_compensation on ANY cadence measurably distorts the resampled audio, even
 * when the value never changes call to call) is a SEPARATE, harder problem, tracked as issue
 * 1019 (split from issue 1016's own "Scope-gate: cross-cutting" framing). See issue 1016's design
 * comment for the rejected alternatives and the disclosed trade-off: this fix makes small-ppm
 * compensation newly audible (previously silent because it was simply never engaging) at a
 * milder distortion level (~-38dB) than the pre-existing high-ppm case (~-18dB, unchanged either
 * way) -- accepted so the servo's stated purpose (#803/#912: always-on, correcting exactly this
 * drift range) is not silently defeated by an integer-rounding accident. */
#define ASRC_COMPENSATION_DISTANCE_MS 10000

/* camera-box #803: run this source's ASRC servo for one audio callback (only when
 * obs_source_set_asrc_enabled() has turned it on for this source) and push the resulting ppm
 * into the swresample context via the soft compensation wrapper. `frames`/`samples_per_sec` are
 * the RAW (pre-resample) values from the callback -- the servo measures the source's OWN upstream
 * clock, before any resampling changes the sample count. No-op (and safely skipped) on the very
 * first callback after a reset, since there is no previous wall-clock sample to measure a block
 * duration against yet. */
static inline void asrc_process_audio(obs_source_t *source, uint32_t frames, uint32_t samples_per_sec)
{
	if (!source->asrc_enabled || !source->resampler || samples_per_sec == 0)
		return;

	const uint64_t wall_now_ns = genlock_wall_now_ns();

	if (!source->asrc_has_last_wall) {
		source->asrc_last_wall_ns = wall_now_ns;
		source->asrc_has_last_wall = true;
		return;
	}

	const double master_block_s = (double)(wall_now_ns - source->asrc_last_wall_ns) / 1000000000.0;
	source->asrc_last_wall_ns = wall_now_ns;

	const double raw_advance_s = (double)frames / (double)samples_per_sec;
	double applied_ppm = 0.0;
	asrc_compensator_compensate(&source->asrc, raw_advance_s, master_block_s, &applied_ppm);

	/* Refresh the compensation over an ASRC_COMPENSATION_DISTANCE_MS window every callback --
	 * swr_set_compensation() replaces any still-pending ramp, so re-issuing it each callback
	 * keeps the correction continuously tracking the servo's current estimate
	 * (audio-resampler-ffmpeg.c's own doc comment on this wrapper). camera-box #1016: widened
	 * from the original 1000ms to lower the integer-rounding no-op floor for typical
	 * single-digit-ppm drift -- see ASRC_COMPENSATION_DISTANCE_MS's own doc comment above. */
	audio_resampler_set_compensation_ppm(source->resampler, applied_ppm, ASRC_COMPENSATION_DISTANCE_MS);

	double cumulative_correction_ms = 0.0;
	uint32_t starved_block_count = 0;
	if (asrc_compensator_should_log(&source->asrc, &cumulative_correction_ms, &starved_block_count)) {
		/* camera-box #806: outer_bias_ppm appended -- "plna telemetria" for the outer-loop
		 * guard's own correction, on the SAME pre-existing ~60s cadence (never a second log
		 * line). Zero when no watchdog has ever called obs_source_set_asrc_outer_bias_ppm().
		 * camera-box #960: starved_blocks appended -- makes a starved/invalid-block state
		 * explicit instead of only ever showing an estimated/applied pair with no indication
		 * anything was rejected. Zero on a healthy source. */
		blog(LOG_INFO,
		     "asrc: source '%s' estimated=%.2fppm applied=%.2fppm outer_bias=%.2fppm "
		     "cumulative_correction=%.3fms/%.0fs starved_blocks=%u (#803/#806/#960)",
		     obs_source_get_name(source), source->asrc.estimated_ppm, applied_ppm,
		     source->asrc.outer_bias_ppm, cumulative_correction_ms, ASRC_LOG_INTERVAL_S,
		     starved_block_count);
	}
}

static void process_audio(obs_source_t *source, const struct obs_source_audio *audio)
{
	uint32_t frames = audio->frames;
	bool mono_output;

	if (source->sample_info.samples_per_sec != audio->samples_per_sec ||
	    source->sample_info.format != audio->format || source->sample_info.speakers != audio->speakers ||
	    (source->asrc_enabled && !source->resampler))
		reset_resampler(source, audio);

	if (source->audio_failed)
		return;

	asrc_process_audio(source, audio->frames, audio->samples_per_sec);

	if (source->resampler) {
		uint8_t *output[MAX_AV_PLANES];

		memset(output, 0, sizeof(output));

		audio_resampler_resample(source->resampler, output, &frames, &source->resample_offset, audio->data,
					 audio->frames);

		copy_audio_data(source, (const uint8_t *const *)output, frames, audio->timestamp);
	} else {
		copy_audio_data(source, audio->data, audio->frames, audio->timestamp);
	}

	mono_output = audio_output_get_channels(obs->audio.audio) == 1;

	if (!mono_output && source->sample_info.speakers == SPEAKERS_STEREO &&
	    (source->balance > 0.51f || source->balance < 0.49f)) {
		process_audio_balancing(source, frames, source->balance, OBS_BALANCE_TYPE_SINE_LAW);
	}

	if (!mono_output && (source->flags & OBS_SOURCE_FLAG_FORCE_MONO) != 0)
		downmix_to_mono_planar(source, frames);
}

void obs_source_output_audio(obs_source_t *source, const struct obs_source_audio *audio_in)
{
	struct obs_audio_data *output;

	if (!obs_source_valid(source, "obs_source_output_audio"))
		return;
	if (destroying(source))
		return;
	if (!obs_ptr_valid(audio_in, "obs_source_output_audio"))
		return;

	/* sets unused data pointers to NULL automatically because apparently
	 * some filter plugins aren't checking the actual channel count, and
	 * instead are checking to see whether the pointer is non-zero. */
	struct obs_source_audio audio = *audio_in;
	size_t channels = get_audio_planes(audio.format, audio.speakers);
	for (size_t i = channels; i < MAX_AUDIO_CHANNELS; i++)
		audio.data[i] = NULL;

	process_audio(source, &audio);

	pthread_mutex_lock(&source->filter_mutex);
	output = filter_async_audio(source, &source->audio_data);

	if (output) {
		struct audio_data data;

		for (int i = 0; i < MAX_AV_PLANES; i++)
			data.data[i] = output->data[i];

		data.frames = output->frames;
		data.timestamp = output->timestamp;

		pthread_mutex_lock(&source->audio_mutex);
		source_output_audio_data(source, &data);
		pthread_mutex_unlock(&source->audio_mutex);
	}

	pthread_mutex_unlock(&source->filter_mutex);
}

void remove_async_frame(obs_source_t *source, struct obs_source_frame *frame)
{
	if (frame)
		frame->prev_frame = false;

	for (size_t i = 0; i < source->async_cache.num; i++) {
		struct async_frame *f = &source->async_cache.array[i];

		if (f->frame == frame) {
			f->used = false;
			break;
		}
	}
}

/* #define DEBUG_ASYNC_FRAMES 1 */

/* ---- genlock FIFO preload + audit (camera-box #70) -----------------------
 * The #42 genlock FIFO consumed exactly one queued frame per wall-clock render
 * tick with ZERO slack: any NDI arrival jitter left the queue empty at the next
 * tick (underrun) -> a dropped/repeated frame. Measured ~0.38%/frame loss on
 * each OBS hop on the production rig (camera-box #68/#69 QR instrument). The fix
 * keeps a small jitter buffer: hold consumption until the queue exceeds
 * `genlock_preload` frames, then consume one per tick. preload=1 -> one frame of
 * reserve per genlock source = one frame of latency per hop, in exchange for
 * absorbing one tick of jitter. Tunable at OBS launch via the env var (no
 * rebuild to change depth), exactly like OBS_GENLOCK_WALL_CLOCK.
 */
#define GENLOCK_PRELOAD_DEFAULT 1
/* camera-box #97: the preload is now a per-source, runtime-settable VIDEO DELAY
 * (one preload frame = one frame of genlock-disciplined delay), used to push the
 * program video back ~1 s to match late audio on stream.lan. ~1 s @ 30 fps = 30
 * frames, already above the old #70 cap of 28, so the ceiling is raised to 128
 * (~4.3 s @ 30 fps). The old "preload+1 must stay below MAX_ASYNC_FRAMES (30)"
 * invariant is GONE: a genlock source's async FIFO drop-cap now scales with its
 * preload (genlock_source_drop_cap() = preload + RESERVE), so a deep preload no
 * longer force-drains every refill. Non-genlock sources keep the fixed cap. */
#define GENLOCK_PRELOAD_MAX 128
/* Headroom above a genlock source's preload for its per-source FIFO drop-cap. The
 * cap must sit above the steady-state depth (preload+1) so normal jitter never
 * trips an overrun drain; +4 leaves 3 frames of slack above steady state. */
#define GENLOCK_DROP_CAP_RESERVE 4
#define GENLOCK_AUDIT_LOG_INTERVAL_NS (5ULL * 1000 * 1000 * 1000) /* ~5 s */
/* camera-box #126: consecutive true-empty (underrun) ticks before a resume re-arms the
 * build latch. 30 ticks ≈ 1 s @ 30 fps — deliberately FAR above any arrival-jitter dip
 * (the #102 reserve makes even one true empty take preload+1 consecutive misses, so only
 * a real upstream disconnect sustains empties this long). A lower value would risk a
 * spurious re-arm at the shallow cam preload=1, where a 1-2 tick transient empty MUST NOT
 * re-arm (a spurious re-arm forces a ~preload-frame rebuild hold on every blip). Mirrored
 * & unit-tested in camera-box src/probe/genlock.rs (GENLOCK_REARM_EMPTY_TICKS). */
#define GENLOCK_REARM_EMPTY_TICKS 30

/* Clamp an UNSIGNED preload to [0, GENLOCK_PRELOAD_MAX]. The setter takes a
 * uint32_t, so it must clamp the unsigned value directly — NOT round-trip through
 * `long`: on Windows (LLP64, 32-bit long) a uint32_t above LONG_MAX would cast to a
 * negative long and the upper clamp would silently invert to 0 (review finding).
 * camera-box #257: the strtol `long` clamp + the OBS_GENLOCK_PRELOAD_FRAMES env
 * parser are GONE — preload is now fully internal/auto-derived (no env). */
static uint32_t genlock_clamp_preload_u32(uint32_t v)
{
	return v > GENLOCK_PRELOAD_MAX ? (uint32_t)GENLOCK_PRELOAD_MAX : v;
}

/* camera-box #235: the minimum auto-derived INTERNAL FIFO depth a genlock source holds
 * for jitter/dropout resilience now that preload is no longer a user latency knob. >= 1
 * frame so a single-tick arrival dip never empties the queue (the #110 sweep showed
 * depth >= 1 holds 0-loss). LATENCY-FREE under the ms deadline: the held delay is
 * latency_ms, NOT the FIFO depth. Equals the historical default preload (1) so the
 * validated prod behavior is preserved. Mirror of src/probe/genlock.rs
 * GENLOCK_AUTO_PRELOAD_MIN. */
#define GENLOCK_AUTO_PRELOAD_MIN 1
/* camera-box #292: the maximum frame rate any genlock SOURCE feeds at on this rig — the
 * cameras and strih render 60 fps. The genlock ts-align deadline holds every queued frame
 * younger than latency_ms, so the FIFO fills at the SOURCE ARRIVAL rate, which can EXCEED
 * the canvas OUTPUT rate (the stream box receives a 60 fps NDI feed from strih into a 30
 * fps canvas — the 60->30 strih->stream topology). Budgeting the drop-cap at the canvas fps
 * undercounted the held depth ~2x → a deep per-source latency force-drained at ~450 ms on
 * the 30 fps stream box, so the operator could not delay the stream the ~1 s needed to
 * A/V-align to the late mastered audio. genlock_source_drop_cap budgets at this worst-case
 * arrival rate so the configured latency is DELIVERED regardless of canvas fps. Mirror of
 * src/probe/genlock.rs GENLOCK_MAX_SOURCE_FPS. */
#define GENLOCK_MAX_SOURCE_FPS 60
/* camera-box #401 v2: queue depth above which the ts-align release cadence re-locks to
 * the live edge (backlog storm). Steady-state depth is ~1-2 at ANY stamp->arrival skew
 * (the locked boundary paces arrivals), so a depth above 6 is unambiguous backlog while
 * tolerating burst jitter; a stall's burst catches up within one tick with every jumped
 * frame counted (genlock_dropped_due). v1 guarded on wall-boundary drift instead
 * (present_ts > boundary + 2.25*interval), which EMBEDS the constant stamp->arrival
 * skew — the 2026-07-02 live canary (skew 59 ms at the 3 ms reserve) relock-stormed:
 * dropped_due 2918 of 4202 received, relocks 1076. Depth is immune to any constant
 * skew. Mirror of src/probe/genlock.rs ReleaseCadence::QDEPTH_RELOCK_MARGIN.
 *
 * camera-box #859: this is now the MARGIN above the depth a source's own configured latency
 * implies, NOT the whole threshold — see genlock_backlog_relock_qdepth() below. The old bare
 * constant encoded the assumption stated a few lines up ("steady depth is ~1-2 at any skew"),
 * which is true only for a SHALLOW source. A source pinned deep (the stream box's 'NDI 2ME PGM'
 * runs 923 ms to A/V-align against the mbc's 1 s mastering) has a steady depth of ~28, so
 * `28 > 6` was permanently true and the backlog branch fired EVERY tick: relocks incremented
 * once per frame, and every arrival-jitter excursion to due==2 erased a frame (dropped_due) that
 * the next tick repeated (holds). Measured live as +59 duplicate / +57 skipped frames injected
 * into the strih->stream hop, against 2 duplicates in 9626 frames on the cam->strih leg whose
 * sources all sit below the bare 6. Mirror of src/probe/genlock.rs QDEPTH_RELOCK_MARGIN and
 * src/genlock_backlog.rs QDEPTH_RELOCK_MARGIN (the Tier-0 unit-tested decision). */
#define GENLOCK_QDEPTH_RELOCK_MARGIN 6
/* camera-box #741/#707 B2: how many of the front queued frames genlock_measure_source_multiple
 * scans for a strictly-increasing consecutive pair. Reading only array[0..1] read INCONCLUSIVE on
 * a DUPLICATE front stamp / arrival-non-monotonic seam, so a jittery 60-into-30 input crawled;
 * scanning the first few entries recovers one real source interval past a leading degenerate pair.
 * Mirror: src/probe/genlock.rs ReleaseCadence::MEASURE_SCAN_DEPTH. */
#define GENLOCK_MEASURE_SCAN_DEPTH 6
/* camera-box #859 follow-up: hysteresis, in frames, a queue must exceed its OWN steady target
 * depth (genlock_backlog_relock_qdepth() minus GENLOCK_QDEPTH_RELOCK_MARGIN) before the
 * slew-limited settle-back DRAIN engages. Without this, ordinary arrival jitter around the
 * target would trigger a drain with nothing genuinely excess to shed. Mirror:
 * src/genlock_backlog.rs DRAIN_HYSTERESIS_FRAMES (Tier-0 unit-tested). */
#define GENLOCK_DRAIN_HYSTERESIS_FRAMES 2
/* camera-box #859 follow-up: minimum render ticks between two DRAIN events — bounds the drain
 * to at most one frame per this many ticks, which is what makes it structurally incapable of
 * reproducing the every-tick paired duplicate/skip storm the (correctly) disabled per-tick
 * backlog-relock branch used to cause as a side effect of trimming the queue on every tick.
 * Mirror: src/genlock_backlog.rs DRAIN_MIN_TICK_INTERVAL. */
#define GENLOCK_DRAIN_MIN_TICK_INTERVAL 30
/* genlock_latency_ms() is declared further down (after the #184 ms-knob block); forward
 * declare it so genlock_preload_default() can branch on whether the ms knob is set. */
static uint32_t genlock_latency_ms(void);

/* The launch-time default a source's per-source FIFO depth is initialized from at create.
 *
 * #235/#257: preload is fully INTERNAL/auto-derived — the per-source ms latency knob
 * (floor GENLOCK_LATENCY_MS_MIN = 3 ms, always > 0) holds the delay, so the FIFO depth is
 * just the auto resilience minimum (GENLOCK_AUTO_PRELOAD_MIN), never a user value. The
 * OBS_GENLOCK_PRELOAD_FRAMES env was removed in #257. Read once and cached. */
static uint32_t genlock_preload_default(void)
{
	static int preload = -1;
	if (preload == -1) {
		preload = (int)GENLOCK_AUTO_PRELOAD_MIN;
		blog(LOG_INFO,
		     "genlock: FIFO depth auto-derived = %d frame(s) (internal resilience "
		     "buffer; the genlock latency knob holds the delay, not preload) (#235/#257)",
		     preload);
	}
	return (uint32_t)preload;
}

/* camera-box #200 follow-up (#269 review): the LAST-GOOD output-fps cache for
 * genlock_video_fps(). The output fps is the GLOBAL canvas ovi — one value shared by
 * EVERY genlock source — so a single file-scope cached pair is correct. Readers are
 * lock-free via a value-seqlock (genlock_fps_cache_seq: even=stable, odd=write in flight,
 * 0=no good pair ever cached); the RARE writer (only when the freshly-agreed pair DIFFERS
 * from the cache, so steady state never writes) serialises under a private mutex that is
 * NEVER nested with any other lock — it therefore cannot deadlock vs obs_reset_video
 * (unlike the OBS video graphics lock the original #200 note rejected). */
static pthread_mutex_t genlock_fps_cache_lock = PTHREAD_MUTEX_INITIALIZER;
static volatile long genlock_fps_cache_seq = 0; /* even=stable, odd=writing, 0=never set */
static uint32_t genlock_fps_cache_num = 0;
static uint32_t genlock_fps_cache_den = 0;

/* Lock-free seqlock read of the last-good fps cache. Returns false (leaving *num/*den
 * untouched) when no good pair was ever published. */
static bool genlock_fps_cache_load(uint32_t *num, uint32_t *den)
{
	for (int attempt = 0; attempt < 4; attempt++) {
		const long s1 = os_atomic_load_long(&genlock_fps_cache_seq);
		if (s1 == 0)
			return false; /* never initialized */
		if (s1 & 1)
			continue; /* writer in flight — retry */
		const uint32_t n = genlock_fps_cache_num;
		const uint32_t d = genlock_fps_cache_den;
		if (os_atomic_load_long(&genlock_fps_cache_seq) == s1) {
			*num = n;
			*den = d;
			return true;
		}
	}
	return false;
}

/* camera-box #200: read the output frame-rate pair (fps_num, fps_den) WITHOUT a torn
 * read. obs_get_video_info() (obs.c) copies obs->...->mix->ovi UNLOCKED, so a
 * concurrent obs_reset_video() (a resolution/fps change) can interleave between the
 * fps_num and fps_den field copies and return a MISMATCHED pair. We deliberately do NOT
 * take the OBS video graphics lock on this render/audit path: that risks a lock-ordering
 * deadlock vs obs_reset_video (which holds the video lock while it can touch source
 * state). Instead snapshot the pair twice and accept it only when two back-to-back reads
 * AGREE — a bounded value-seqlock; obs_reset_video is rare, so in steady state the first
 * two reads already match (one extra struct copy).
 *
 * camera-box #269 review: on AGREEMENT we also PUBLISH the good pair to a file-scope
 * last-good cache (only when it CHANGED — the steady-state hot path stays lock-free), and
 * on a PERSISTENT tear we return the CACHED last-good pair instead of false/0. The bare
 * false return broke two callers the old always-true read never did: (1)
 * genlock_source_drop_cap skipped its latency_frames bump → the per-source FIFO drop-cap
 * collapsed to the 30-frame floor → a deep-latency override momentarily force-drained the
 * FIFO (an A/V phase jump); (2) genlock_frame_interval_ns returned 0 → the ts-align block
 * was skipped for that tick → the source briefly presented off the shared wall-clock
 * deadline (a one-tick break of the #136 multi-source in-sync invariant). Returning the
 * cached pair eliminates both while still never logging a TORN pair (#200's goal). false
 * is now returned ONLY on a tear before ANY good pair was ever read (first-ever call
 * mid-tear) — every caller still guards fps_num==0 / fps_den!=0 and takes the fps-unknown
 * branch then. Mirror of src/probe/genlock.rs genlock_fps_cached. */
static bool genlock_video_fps(uint32_t *fps_num, uint32_t *fps_den)
{
	struct obs_video_info a;
	struct obs_video_info b;
	for (int attempt = 0; attempt < 4; attempt++) {
		if (!obs_get_video_info(&a) || !obs_get_video_info(&b))
			break;
		if (a.fps_num == b.fps_num && a.fps_den == b.fps_den) {
			*fps_num = a.fps_num;
			*fps_den = a.fps_den;
			/* publish a CHANGED good (nonzero) pair; steady state matches the
			 * cache so it takes no lock. The compare reads the cache via the
			 * lock-free seqlock (no data race), then the rare write serialises. */
			if (a.fps_num != 0) {
				uint32_t cn = 0, cd = 0;
				const bool cached = genlock_fps_cache_load(&cn, &cd);
				if (!cached || cn != a.fps_num || cd != a.fps_den) {
					pthread_mutex_lock(&genlock_fps_cache_lock);
					os_atomic_inc_long(&genlock_fps_cache_seq); /* -> odd */
					genlock_fps_cache_num = a.fps_num;
					genlock_fps_cache_den = a.fps_den;
					os_atomic_inc_long(&genlock_fps_cache_seq); /* -> even */
					pthread_mutex_unlock(&genlock_fps_cache_lock);
				}
			}
			return true;
		}
	}
	/* a tear persisted past the retry budget: return the cached last-good pair if one
	 * was ever recorded; only a never-initialized cache rejects (fps-unknown fallback). */
	if (genlock_fps_cache_load(fps_num, fps_den))
		return true;
	*fps_num = 0;
	*fps_den = 0;
	return false;
}

/* Per-source async-FIFO drop-cap (#97). A NON-genlock source keeps libobs' fixed
 * MAX_ASYNC_FRAMES (those sources never deliberately buffer). A genlock source's cap
 * = max(MAX_ASYNC_FRAMES, preload + RESERVE), capped at GENLOCK_PRELOAD_MAX + RESERVE.
 *
 * The MAX_ASYNC_FRAMES floor matters: BEFORE #97 every source (genlock included) had
 * the fixed 30-frame cap, which absorbed NDI catch-up bursts after a LAN hiccup.
 * Scaling the cap to preload+RESERVE ALONE would, at the production default preload=1,
 * drop the cap to 5 — a 6x cut in burst tolerance on exactly the jittery sources the
 * genlock FIFO exists to protect (a momentary stall delivering a 5-frame catch-up
 * burst would force-drain the whole buffer). Keeping the 30-frame floor preserves the
 * pre-#97 burst tolerance; the cap only GROWS above it once the operator dials in a
 * deep delay. Mirrored & unit-tested in camera-box tests/genlock_preload.rs
 * (genlock_drop_cap). */
static size_t genlock_source_drop_cap(const obs_source_t *source)
{
	if (!source->genlock_fifo)
		return MAX_ASYNC_FRAMES;
	/* camera-box #245: a per-source latency override (ms) is a deliberate VIDEO DELAY —
	 * the FIFO must hold its FULL frame-equivalent before the ms release deadline frees
	 * the oldest frame, or the overrun force-drain would cap the delay short (a 1000 ms
	 * override ≈ 30 frames, a 2000 ms one ≈ 60 frames @ 30 fps, both above the 30-frame
	 * floor). So the depth budget is max(preload, latency-in-frames). The GLOBAL latency
	 * stays ≤100 ms (≤3 frames, under the floor) so this only GROWS the cap for a deep
	 * PER-SOURCE override; an un-overridden source keeps the historic preload+RESERVE cap.
	 * Mirror of src/probe/genlock.rs genlock_drop_cap(fifo, max(preload, latency_frames)). */
	uint32_t depth = source->genlock_preload;
	if (source->genlock_latency_ms > 0) {
		/* camera-box #292: the ts-align deadline holds every queued frame younger than
		 * latency_ms, so the FIFO fills at the SOURCE ARRIVAL rate, which can EXCEED the
		 * canvas OUTPUT rate (a 60 fps NDI feed into a 30 fps stream canvas). Budgeting at
		 * the canvas fps undercounted the held depth ~2x → a deep latency force-drained at
		 * ~450 ms. Budget at the worst-case arrival rate (GENLOCK_MAX_SOURCE_FPS) so the
		 * configured latency is DELIVERED regardless of canvas fps. round-to-nearest
		 * (+rate/2) faithfully mirrors the Rust ms_to_frames(). Mirror of
		 * src/probe/genlock.rs genlock_latency_depth_frames. */
		uint32_t latency_frames =
			(uint32_t)(((uint64_t)source->genlock_latency_ms * GENLOCK_MAX_SOURCE_FPS +
				    500) /
				   1000);
		/* camera-box #200: tear-checked fps snapshot (see genlock_video_fps). Honour the
		 * canvas rate too, should a future canvas ever run faster than the source. */
		uint32_t fps_num, fps_den;
		if (genlock_video_fps(&fps_num, &fps_den) && fps_den != 0) {
			const uint64_t lat_den = 1000ULL * fps_den;
			const uint32_t canvas_frames =
				(uint32_t)(((uint64_t)source->genlock_latency_ms * fps_num +
					    lat_den / 2) /
					   lat_den);
			if (canvas_frames > latency_frames)
				latency_frames = canvas_frames;
		}
		if (latency_frames > depth)
			depth = latency_frames;
	}
	uint32_t want = depth + GENLOCK_DROP_CAP_RESERVE;
	const uint32_t abs_max = GENLOCK_PRELOAD_MAX + GENLOCK_DROP_CAP_RESERVE;
	if (want > abs_max || want < depth /* overflow guard */)
		want = abs_max;
	if (want < MAX_ASYNC_FRAMES) /* never below the pre-#97 burst-tolerance floor */
		want = MAX_ASYNC_FRAMES;
	return (size_t)want;
}

/* Convert a preload depth (frames of video delay) to milliseconds at the current
 * output frame rate. ms = frames * 1000 * fps_den / fps_num. Returns 0 when no
 * valid video info is available (fps_num == 0). Mirrored & unit-tested in
 * camera-box tests/genlock_preload.rs (preload_to_ms). */
static uint64_t genlock_preload_ms(uint32_t frames)
{
	/* camera-box #200: tear-checked fps snapshot (see genlock_video_fps). */
	uint32_t fps_num, fps_den;
	if (!genlock_video_fps(&fps_num, &fps_den) || fps_num == 0)
		return 0;
	return (uint64_t)frames * 1000 * fps_den / fps_num;
}

/* camera-box #102: the genlock consume decision for one render tick. Mirrored by
 * the camera-box unit test (src/probe/genlock.rs genlock_decide / tests).
 *
 * `filled` is the per-source one-time startup-fill latch (passed by value; the
 * caller writes back `.filled`):
 *
 *  - BUILD (`!filled`): establish the delay line. Hold (consume=false) until the
 *    queue is deeper than `preload`; the moment it exceeds preload, LATCH filled
 *    and consume the first (preload-frames-late) frame. This is the ONLY place a
 *    repeat is emitted, and only once at startup.
 *  - STEADY (`filled`): consume a distinct frame on EVERY tick a frame is queued
 *    (queue_depth >= 1). A jitter dip below the reserve still delivers a distinct
 *    frame (no repeat) — the reserve just shrinks and refills. The ONLY hold is a
 *    TRUE empty (queue_depth == 0), an unavoidable underrun. filled stays set so a
 *    transient empty does NOT re-trigger the whole startup refill (which is exactly
 *    what made the old #70 `depth>preload` gate lose ~34% of distinct frames at a
 *    deep preload). The latch is reset to false only on an overrun force-drain
 *    (cache_video), so the delay line rebuilds after a drain. */
struct genlock_decision {
	bool consume; /* hand a distinct queued frame to the compositor this tick */
	bool filled;  /* new value of the startup-fill latch */
};

static inline struct genlock_decision genlock_decide(size_t queue_depth, uint32_t preload, bool filled)
{
	struct genlock_decision d;
	if (!filled) {
		if (queue_depth > (size_t)preload) {
			d.consume = true;
			d.filled = true; /* delay line established */
		} else {
			d.consume = false;
			d.filled = false; /* still building the preload delay */
		}
	} else {
		d.consume = queue_depth >= 1; /* emit whenever a distinct frame is queued */
		d.filled = true;
	}
	return d;
}

/* camera-box #116: how many OLDEST frames to ERASE at the build-latch instant so the
 * FIFO settles at exactly the target depth (= preload + 1), regardless of the NDI
 * startup burst. Mirrored & unit-tested in camera-box src/probe/genlock.rs
 * (genlock_build_drain / tests).
 *
 * The #102 build latch (genlock_decide) latched filled=true at WHATEVER depth the
 * startup burst left (queue_depth>preload) and consumed one — it never trimmed down,
 * so: (1) two inputs with the same preload but different bursts froze at different
 * depths => unequal per-camera latency + a time-jump on switch; (2) a preload
 * DECREASE re-latched at the OLD deep depth => the lower delay never took effect
 * ("only goes up"); (3) each restart's random arrival phase froze at a different
 * depth => non-deterministic latency. Erasing queue_depth-target oldest frames at
 * the latch (and on a preload-change re-arm) makes every input + every restart settle
 * at the IDENTICAL deterministic target, and makes the preload knob BIDIRECTIONAL.
 *
 * Returns 0 below the latch (still building) and at/under the target (size_t is
 * unsigned; the explicit guard avoids a wraparound to a huge erase count). */
static inline size_t genlock_build_drain(size_t queue_depth, uint32_t preload)
{
	const size_t target = (size_t)preload + 1; /* steady_state_depth(preload) */
	return queue_depth > target ? queue_depth - target : 0;
}

/* ---- camera-box #136: timestamp-aligned release (multi-source IN-SYNC) -----
 * The count-based genlock_decide above keeps a fixed-DEPTH per-source jitter
 * buffer; it cannot hold MULTIPLE sources in sync because each source's depth
 * drifts independently (the render pass consumes slightly slower than the cams
 * produce, and any per-source dropout/reconnect/preload-change leaves that source
 * at a different depth that never re-converges) — measured ~300 ms / 9-frame spread
 * live. Timestamp-aligned release instead presents, from every source, the frame
 * captured at the SAME shared wall-clock instant present_ts = wall_now -
 * preload*interval; identical capture instant on every source => in-sync by
 * construction, latency bounded+uniform = preload*interval, and a slow/lagged
 * render pass just drops the stale past-due frames uniformly (NO buffer fill toward
 * the overrun cap). preload (a frame COUNT) is reinterpreted as a TIME delay.
 *
 * Mirrored & unit-tested in camera-box src/probe/genlock.rs (genlock_release /
 * genlock_present_ts / is_wallclock_ts). Gated behind OBS_GENLOCK_TS_ALIGN (default
 * OFF => behaviour is byte-identical to the count gate) AND a per-frame wall-clock
 * sanity check, so non-camera sources (CG/preview, no wall-clock timecode) always
 * fall back to the count gate and the rollout is reversible by env alone. */

/* Plausible DanteSync wall-clock bounds (Unix epoch ns): 2020-01-01 .. 2100-01-01.
 * Mirror of camera-box src/probe/genlock.rs WALLCLOCK_TS_{MIN,MAX}_NS. */
#define GENLOCK_WALLCLOCK_TS_MIN_NS 1577836800000000000ULL
#define GENLOCK_WALLCLOCK_TS_MAX_NS 4102444800000000000ULL

static inline bool genlock_is_wallclock_ts(uint64_t ts_ns)
{
	return ts_ns >= GENLOCK_WALLCLOCK_TS_MIN_NS && ts_ns < GENLOCK_WALLCLOCK_TS_MAX_NS;
}

/* camera-box #184/#235/#257: the SUB-FRAME MS-GRANULAR jitter reserve — the held latency.
 * The ts-align deadline is wall_now - latency_ms, so the held latency is ≈ latency_ms
 * (single-digit ms = just the measured arrival jitter) instead of a full 33ms frame.
 *
 * #257 hard-locks the genlock latency to a BUILD CONST — no OBS_GENLOCK_LATENCY_MS /
 * OBS_GENLOCK_RESERVE_MS env any more. The GLOBAL default is GENLOCK_LATENCY_MS_DEFAULT
 * (= 3 ms), with a hard FLOOR of GENLOCK_LATENCY_MS_MIN (= 3); per-source overrides ride
 * source->genlock_latency_ms (the DistroAV ms field, also floored at 3). The reserve
 * default/max #defines are kept ONLY so the Rust mirror's legacy parse-helper lock-step
 * guards still have a literal to match. */
#define GENLOCK_RESERVE_MS_DEFAULT 0
#define GENLOCK_RESERVE_MS_MAX 100
/* #257: the genlock latency is a build const — default AND floor are 3 ms (no env). The
 * MAX is kept = the legacy reserve max (mirror lock-step). */
#define GENLOCK_LATENCY_MS_MIN 3
#define GENLOCK_LATENCY_MS_DEFAULT 3
#define GENLOCK_LATENCY_MS_MAX GENLOCK_RESERVE_MS_MAX
/* camera-box #245: the PER-SOURCE latency override cap (ms), set in the OBS source UI.
 * A per-source override is a deliberate VIDEO DELAY (the live-event need was 1000 ms on a
 * single source while the others stayed low). 2000 ms ≈ 60 frames @ 30 fps, inside the
 * FIFO drop-cap abs-max (GENLOCK_PRELOAD_MAX + GENLOCK_DROP_CAP_RESERVE = 132 frames) so a
 * source at the cap buffers its full delay without an overrun force-drain. Mirror of
 * src/probe/genlock.rs GENLOCK_SOURCE_LATENCY_MS_MAX + the DistroAV UI int range. */
#define GENLOCK_SOURCE_LATENCY_MS_MAX 2000

/* camera-box #257: GENLOCK_LATENCY_MS_MIN_INIT (defined early, near obs_source_init, so the
 * per-source field can be seeded at create before this block) MUST equal the canonical floor
 * GENLOCK_LATENCY_MS_MIN — otherwise the create-seed and the setter clamp would disagree. Pin
 * them equal at compile time so a future floor change can never silently desync the two. */
_Static_assert(GENLOCK_LATENCY_MS_MIN == GENLOCK_LATENCY_MS_MIN_INIT,
	       "genlock latency create-seed floor (_INIT) must equal the canonical clamp floor (#257)");

/* camera-box #257: the GLOBAL genlock latency a source without a per-source override falls
 * back to — now a BUILD CONST (GENLOCK_LATENCY_MS_DEFAULT = 3 ms), no env. Per-source is
 * always >= GENLOCK_LATENCY_MS_MIN (3) so this fallback is effectively never reached, but
 * it is kept (audit log + render-path fallback) for completeness. Logged once at launch
 * (the `genlock: latency = N ms` line the launch wrapper + drift-guard key on). */
static uint32_t genlock_latency_ms(void)
{
	static int logged = -1;
	if (logged == -1) {
		logged = 1;
		blog(LOG_INFO,
		     "genlock: latency = %d ms (build default, floor %d ms, ts-align always ON) "
		     "— single hard-locked latency knob, per-source override in the OBS UI (#257)",
		     GENLOCK_LATENCY_MS_DEFAULT, GENLOCK_LATENCY_MS_MIN);
	}
	return GENLOCK_LATENCY_MS_DEFAULT;
}

/* #184 back-compat: genlock_reserve_ms() returns the single genlock latency (now the #257
 * build const). Kept under its old name so the #184 render-path call site + the
 * tests/genlock_preload.rs vendored-source guards remain in lock-step. */
static uint32_t genlock_reserve_ms(void)
{
	return genlock_latency_ms();
}

/* camera-box #257: the #136 timestamp-aligned multi-source release is ALWAYS ON in the
 * fork build (the ms-latency path) — no OBS_GENLOCK_TS_ALIGN env. ts-align is what makes
 * the per-source ms latency deadline (wall_now - latency_ms) hold, so the production fork
 * always wants it. Logged once at launch (the `timestamp-aligned release` line drift-guard
 * keys on). */
static bool genlock_ts_align_enabled(void)
{
	static int logged = -1;
	if (logged == -1) {
		logged = 1;
		blog(LOG_INFO,
		     "genlock: timestamp-aligned release ENABLED (build default) — multi-source "
		     "in-sync (#136/#235/#257)");
	}
	return true;
}

/* Real DanteSync wall clock NOW in ns since the Unix epoch — the SAME basis the
 * wall-clock-slaved render tick (obs-video.c genlock_wall_ns) and the cam-box NDI
 * timecode (src/ndi.rs) use, so frame->timestamp and present_ts share one timeline. */
static inline uint64_t genlock_wall_now_ns(void)
{
#ifdef _WIN32
	FILETIME ft;
	ULARGE_INTEGER u;
	GetSystemTimePreciseAsFileTime(&ft);
	u.LowPart = ft.dwLowDateTime;
	u.HighPart = ft.dwHighDateTime;
	/* FILETIME (100ns since 1601) -> unix epoch ns */
	return (u.QuadPart - 116444736000000000ULL) * 100ULL;
#else
	struct timespec ts;
	clock_gettime(CLOCK_REALTIME, &ts);
	return (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
#endif
}

/* camera-box #800: wall(RTC)-vs-monotonic(QPC) clock drift since the first audit tick, in ms.
 * The video release deadline keys on the WALL clock (genlock_wall_now_ns() =
 * GetSystemTimePreciseAsFileTime on Windows, disciplined by NTP/DanteSync), while the
 * render tick and audio capture ride the MONOTONIC clock (os_gettime_ns() = QPC, free-
 * running). If the two clock domains drift apart over a long event, the wall-slaved video
 * and the QPC-slaved audio diverge — the leading remaining candidate for the #800 all-day
 * A/V shift the instrumented FIFO already ruled out. Both clocks are read back-to-back
 * (the #269 single-read discipline) and anchored ONCE at the first tick; drift =
 * (wall_now - wall_anchor) - (mono_now - mono_anchor). Positive = wall ran FASTER than QPC
 * (video deadline advancing ahead of audio). Process-global; called only from
 * genlock_audit_log on the graphics thread, so the function-local static anchors need no
 * lock (same single-thread assumption as the non-atomic genlock counters and
 * genlock_ts_align_enabled's own static). Cheap (5s-gated, per source). */
static long long genlock_wall_qpc_drift_ms(void)
{
	static uint64_t wall_anchor_ns = 0;
	static uint64_t mono_anchor_ns = 0;
	const uint64_t wall_now_ns = genlock_wall_now_ns();
	const uint64_t mono_now_ns = os_gettime_ns();
	if (wall_anchor_ns == 0) {
		wall_anchor_ns = wall_now_ns;
		mono_anchor_ns = mono_now_ns;
		return 0;
	}
	/* The mono elapsed is non-negative (monotonic clock, anchor is earlier); the WALL elapsed
	 * may go NEGATIVE if NTP/DanteSync steps the RTC back — the signed cast captures that
	 * correctly (two's-complement), and such a step IS a wall-vs-QPC clock-domain divergence,
	 * exactly what #800 wants to record. int64 holds a day of ns (8.6e13) with vast headroom. */
	const long long wall_elapsed = (long long)(wall_now_ns - wall_anchor_ns);
	const long long mono_elapsed = (long long)(mono_now_ns - mono_anchor_ns);
	return (wall_elapsed - mono_elapsed) / 1000000;
}

/* One output frame interval in ns from the current video info (0 if unknown). */
static inline uint64_t genlock_frame_interval_ns(void)
{
	/* camera-box #200: tear-checked fps snapshot (see genlock_video_fps). */
	uint32_t fps_num, fps_den;
	if (!genlock_video_fps(&fps_num, &fps_den) || fps_num == 0)
		return 0;
	return (uint64_t)1000000000ULL * fps_den / fps_num;
}

/* The presentation deadline for this tick: the wall-clock instant whose frame is due
 * now = wall_now - delay_frames*interval + interval/2 (saturating, never wraps below 0).
 * The +interval/2 is the #136 boundary-churn fix: a frame whose timestamp lands exactly
 * on the nominal deadline jitters in/out of the `ts <= present_ts` test from tick to tick
 * (the render tick has ±slew), producing hold/drop churn (~3 fps on the deep-preload
 * chained strih->stream PGM feed). Biasing forward by half a frame makes a boundary frame
 * robustly due (±2ms slew << interval/2 ≈ 16ms @ 30fps); all sources share the bias so
 * in-sync is preserved. Mirror of camera-box src/probe/genlock.rs genlock_present_ts. */
static inline uint64_t genlock_present_ts(uint64_t wall_now_ns, uint32_t delay_frames, uint64_t interval_ns)
{
	const uint64_t delay = (uint64_t)delay_frames * interval_ns;
	const uint64_t base = wall_now_ns > delay ? wall_now_ns - delay : 0;
	return base + interval_ns / 2;
}

/* camera-box #184: the presentation deadline under a MS-GRANULAR jitter reserve.
 * present_ts = wall_now - reserve_ms*1e6 (saturating). A frame is due once it has aged
 * reserve_ms, so the held latency is EXACTLY reserve_ms — a pure sub-frame time delay,
 * NOT quantized to a whole frame, and with NO +interval/2 churn bias (the deadline is
 * an absolute wall-clock instant, not a frame multiple, so frames never cluster on it;
 * reserve_ms is itself the slew tolerance). Every source shares wall_now - reserve, so
 * the #136 multi-source in-sync invariant is preserved. The buffer need only cover the
 * measured arrival jitter (1.6ms strih->stream, 8.1ms cam1->strih), so a ~3ms reserve
 * replaces the 33ms whole-frame preload while staying zero-loss. Mirror of camera-box
 * src/probe/genlock.rs genlock_present_ts_reserve. */
static inline uint64_t genlock_present_ts_reserve(uint64_t wall_now_ns, uint32_t reserve_ms)
{
	const uint64_t delay = (uint64_t)reserve_ms * 1000000ULL;
	return wall_now_ns > delay ? wall_now_ns - delay : 0;
}
/* ---- end #136 ------------------------------------------------------------- */

/* camera-box #940 piece 3: PHASE-PIN the ts-align RESERVE deadline to the absolute wall-
 * clock frame GRID -- floor(deadline_ns / interval_ns) * interval_ns -- so which-frame-
 * releases-now becomes a pure FUNCTION of wall time instead of a hidden per-lock-episode
 * state re-sampled by every ACQUIRE/relock. Root cause this removes: the pre-#940 deadline
 * (genlock_present_ts_reserve above) is a raw continuous quantity, so "which frame is due"
 * depends on the EXACT sub-ms instant a relock happens to fire -- measured live as a
 * ±1-2-frame A/V-offset step between lock episodes at deep latency (issue #940). This
 * helper is applied to genlock_present_ts_reserve()'s OUTPUT, not folded into it --
 * genlock_present_ts_reserve() itself stays byte-identical (its own pre-#940 tests pin its
 * exact arithmetic). A degenerate interval_ns (0 -- unknown video info) returns the
 * deadline unchanged rather than dividing by zero. Mirror of camera-box
 * src/genlock_backlog.rs phase_pinned_deadline (Tier-0 unit-tested). */
static inline uint64_t genlock_phase_pin_deadline(uint64_t deadline_ns, uint64_t interval_ns)
{
	if (interval_ns == 0)
		return deadline_ns;
	return (deadline_ns / interval_ns) * interval_ns;
}

/* camera-box #940 piece 3: the grid-comparison HYSTERESIS slack -- a frame captured
 * essentially exactly on a grid line must not flip due/not-due from ordinary sub-ms
 * render-tick jitter on genlock_phase_pin_deadline()'s floor division (the design's own
 * documented risk). Sized well below one frame interval at any rig fps (33.3ms @ 30fps,
 * 16.6ms @ 60fps) so it can never pull in an extra frame -- the same shape of guard
 * genlock_present_ts's existing +interval/2 boundary-churn bias already applies to the
 * (separate, frame-count) preload path, sized here to the sub-frame reserve-ms deadline
 * this quantizes. Mirror: src/genlock_backlog.rs PHASE_PIN_HYSTERESIS_NS. */
#define GENLOCK_PHASE_PIN_HYSTERESIS_NS 5000000ULL /* 5 ms */

/* camera-box #1003: PHASE-CONTINUITY RELOCK (history-anchored selection).
 *
 * #940 piece 3 (the grid pin above) removed the deadline's dependence on the exact sub-ms
 * instant a relock fired, but the release phase is minted by the SELECTION, and that
 * selection stayed an instant-sampled, STATELESS comparison with two independently flippable
 * edges: (1) the pin quantizes to the RECEIVER grid, and the floor's step point sits at
 * tick-phase `latency_ms mod interval` (~23.0 ms at the live 923 ms knob), so ±2 ms of
 * render-tick slew there moves the whole pinned cell by one interval; (2) the stamps being
 * compared sit on the SENDER's own floor grid (33,333,300 ns in 100 ns units vs the receiver's
 * 33,333,333 ns) -- a 33 ns/frame beat (~3.6 ms/h) plus DanteSync inter-box skew wander, so
 * GENLOCK_PHASE_PIN_HYSTERESIS_NS is a FIXED edge inside a DRIFTING relative phase. Two edges
 * = up to four outcomes spanning two frames, which is exactly the -64.5 / +56..63 ms
 * per-episode steps measured live. No hysteresis SIZE fixes this; it only moves the coin.
 *
 * The fix is structural: track the steady conveyor's own on-air age and have a relock present
 * the queued frame NEAREST that remembered age. Nearest-neighbour selection is CONTINUOUS --
 * the selection point sits half a stamp interval from the operating point BY CONSTRUCTION --
 * so there is no threshold for slew or beat to flip. DEPTH is still corrected by whole frames:
 * the caller turns the returned index into `release = index + 1`, so the unchanged
 * `to_drop = release - 1` erase loop retires exactly the older frames into genlock_dropped_due.
 *
 * Mirror of src/genlock_backlog.rs relock_select_nearest / relock_anchor_age_ns (Tier-0
 * unit-tested) -- keep both in lock-step. */
static inline uint64_t genlock_abs_diff_ns(uint64_t a, uint64_t b)
{
	return a > b ? a - b : b - a;
}

/* camera-box #1003: the AGE the relock selection targets -- the tracked phase anchor when it
 * is SET, else the source's configured latency. Anchor 0 is the UNSET sentinel (bzalloc
 * zero-init), so a source that has never presented a steady frame (cold start, post-flush,
 * just after a backward-step regime ended) falls back to the phase the wall-deadline path
 * would have produced anyway. Mirror of src/genlock_backlog.rs relock_anchor_age_ns. */
static inline uint64_t genlock_relock_target_age_ns(const obs_source_t *source, uint32_t latency_ms)
{
	const uint64_t configured = (uint64_t)latency_ms * 1000000ULL;
	const uint64_t anchor = source->genlock_phase_anchor_ns;
	/* FLOORED at the configured latency. The conveyor always holds AT LEAST the
	 * configured hold, so in the intended regime the anchor is at or above it and this
	 * is inert. It matters as a bound: genlock_phase_anchor_from_present saturates, and
	 * nothing in the types stops a degenerate/stale anchor coming back SMALLER than the
	 * hold -- an anchor near 0 would target the live edge and erase the entire delay
	 * line in a single relock. Mirror: src/genlock_backlog.rs relock_anchor_age_ns. */
	return anchor > configured ? anchor : configured;
}

/* camera-box #1003: the relock selection itself -- the INDEX into async_frames (arrival order,
 * OLDEST first) of the frame whose stamp is NEAREST `wall_now_ns - target_age`. Ties resolve
 * toward the OLDER frame (strict <), which keeps the selection monotone as the target sweeps
 * forward so a tie can never oscillate between neighbours on successive episodes. Callers are
 * only ever reached with num >= 1 (ready_async_frame guards on it and both relock branches
 * additionally require due > 0); the num == 0 early return is defensive, never a live path.
 * Mirror of src/genlock_backlog.rs relock_select_nearest. */
static inline size_t genlock_relock_select_nearest(const obs_source_t *source, uint64_t wall_now_ns,
						   uint32_t latency_ms)
{
	const uint64_t age = genlock_relock_target_age_ns(source, latency_ms);
	const uint64_t target = wall_now_ns > age ? wall_now_ns - age : 0;
	size_t best = 0;
	uint64_t best_d;

	if (source->async_frames.num == 0)
		return 0;

	best_d = genlock_abs_diff_ns(source->async_frames.array[0]->timestamp, target);
	for (size_t i = 1; i < source->async_frames.num; i++) {
		const uint64_t d = genlock_abs_diff_ns(source->async_frames.array[i]->timestamp, target);
		/* STRICT < -- an equal distance keeps the already-chosen OLDER frame. */
		if (d < best_d) {
			best = i;
			best_d = d;
		}
	}
	return best;
}

/* camera-box #1003: the anchor to remember after presenting `presented_ts_ns` at wall instant
 * `wall_now_ns` -- the conveyor's own measured on-air age. Saturating: a frame stamped AHEAD of
 * the receiver's wall clock (the sender's deliberate ceil-to-boundary future bias, issue 1009
 * defect B) would otherwise underflow, and saturating to 0 makes such a degenerate sample read
 * as UNSET, i.e. the next relock falls back to the configured latency rather than targeting a
 * nonsense age. Mirror of src/genlock_backlog.rs phase_anchor_from_present. */
static inline uint64_t genlock_phase_anchor_from_present(uint64_t wall_now_ns, uint64_t presented_ts_ns)
{
	return wall_now_ns > presented_ts_ns ? wall_now_ns - presented_ts_ns : 0;
}

/* camera-box #1009: the backward-step trigger margin -- must be >> the sender's deliberate
 * ceil-to-boundary future bias (<= one interval; ndi-output.cpp genlock_emit_timecode_100ns /
 * src/ndi.rs) plus any sane network delay + inter-box skew, so plain sender-ahead stamp skew
 * can never fire the #147 guard. The 2026-08-07 overnight -900 ms collapse fired it at a
 * measured excess of 0.3-45 ms against the old ONE-interval margin; a REAL NTP/PTP backward
 * step is hundreds of ms to seconds, so max(3 intervals, 250 ms) cleanly separates the two
 * populations. Mirror of src/genlock_backlog.rs backward_step_margin_ns /
 * BACKWARD_STEP_MIN_MARGIN_NS / BACKWARD_STEP_MARGIN_INTERVALS (Tier-0 unit-tested). */
#define GENLOCK_BACKWARD_STEP_MIN_MARGIN_NS 250000000ULL /* 250 ms */
#define GENLOCK_BACKWARD_STEP_MARGIN_INTERVALS 3ULL
/* camera-box #1009: the over-margin condition must SUSTAIN this many CONSECUTIVE due==0 ticks
 * before the first re-anchor -- never a single-tick hair-trigger (a 1-2 tick excursion falls
 * through to the #401 cadence, which presents/holds normally off its locked boundary).
 * Mirror of src/genlock_backlog.rs BACKWARD_STEP_SUSTAIN_TICKS. */
#define GENLOCK_BACKWARD_STEP_SUSTAIN_TICKS 3
/* camera-box #1009: a re-anchor regime older than WARN_AFTER re-warns on a bounded cadence
 * (at most one WARN per WARN_INTERVAL). The once-per-latch entry WARN alone let the overnight
 * collapse run SILENT for 3+ hours (last log 05:09, still collapsed at 08:20). Mirror of
 * src/genlock_backlog.rs BACKWARD_REGIME_WARN_AFTER_NS / BACKWARD_REGIME_WARN_INTERVAL_NS. */
#define GENLOCK_BACKWARD_REGIME_WARN_AFTER_NS 2000000000ULL   /* 2 s */
#define GENLOCK_BACKWARD_REGIME_WARN_INTERVAL_NS 5000000000ULL /* 5 s */

static inline uint64_t genlock_backward_step_margin_ns(uint64_t interval_ns)
{
	const uint64_t scaled = GENLOCK_BACKWARD_STEP_MARGIN_INTERVALS * interval_ns;
	return scaled > GENLOCK_BACKWARD_STEP_MIN_MARGIN_NS ? scaled : GENLOCK_BACKWARD_STEP_MIN_MARGIN_NS;
}

/* camera-box #1009 SELF-HEAL: leave a qualified backward-step regime and re-establish the
 * CONFIGURED hold. Zeroing the locked boundary is the existing #401 ACQUIRE state: the
 * wall-deadline path holds (genlock_holds) while the queue rebuilds to the configured latency
 * depth, then re-locks -- a bounded ~latency_ms transient. Before #1009 NOTHING restored the
 * hold after the condition cleared, so a re-anchor regime left the FIFO consuming at the live
 * edge FOREVER (the -900 ms overnight collapse: depth 0-1 at a 894 ms knob for 4+ hours, only
 * an OBS relaunch cleared it). Mirror of src/genlock_backlog.rs BackwardStepGuard::SelfHeal. */
static void genlock_backward_regime_end(obs_source_t *source, uint32_t latency_ms)
{
	source->genlock_in_backward_step = false;
	source->genlock_locked_next_boundary_ns = 0;
	/* #726 STICKY-N: leaving the regime is a source-timeline seam like the re-anchor
	 * itself -- the fresh ACQUIRE re-confirms the multiple from scratch. */
	source->genlock_last_known_n = 0;
	/* camera-box #1003: CLEAR the phase anchor too. The receiver wall clock moved by the
	 * step, so every `wall_now - stamp` age sampled before the correction is wrong by
	 * exactly that much -- re-acquiring against it would re-establish the hold at a phase
	 * off by the whole clock step. Unset falls back to the CONFIGURED latency, which is
	 * exactly this function's own stated contract (re-acquire the configured hold). */
	source->genlock_phase_anchor_ns = 0;
	source->genlock_acquire_bracket_ticks = 0; /* #1161: regime end zeroes the boundary -> a fresh ACQUIRE; reset the bracket-hold counter so the next re-acquire's fail-open cap counts from 0. */
	blog(LOG_WARNING,
	     "genlock-fifo backward-step regime ENDED '%s': reanchor_ticks=%llu — re-acquiring the "
	     "configured hold (latency %u ms) from the wall deadline (#1009)",
	     source->context.name ? source->context.name : "?",
	     (unsigned long long)source->genlock_backward_regime_ticks, latency_ms);
}

/* camera-box #148 follow-up (#269 finding [5]): the ts-align decision sample
 * (genlock_last_present_ts / _due / _head_skew_ns) is written ONLY on a tick the ts-align
 * path ran, but genlock_audit_log prints it unconditionally — so a ts-align source that
 * FALLS THROUGH to the count gate (interval==0, non-wallclock head ts) or a true-empty
 * tick printed the STALE sample from an earlier tick. Reset the three fields to the
 * all-zero sentinel on every count-gate / true-empty tick (BEFORE that tick's
 * genlock_audit_log call) so the 5s audit never prints a stale present/due/skew (the
 * "0 on non-ts-align" the audit comment promises). Mirror of src/probe/genlock.rs
 * genlock_ts_audit_sample. */
static inline void genlock_clear_ts_sample(obs_source_t *source)
{
	source->genlock_last_present_ts = 0;
	source->genlock_last_due = 0;
	source->genlock_last_head_skew_ns = 0;
}

/* Periodic audit log: emit the FIFO health counters every ~5 s so underruns are
 * visible in the OBS log before AND after the fix (the verification evidence).
 * `now_ns` is the monotonic render-tick stamp (obs->video.video_time). */
static void genlock_audit_log(obs_source_t *source, uint64_t now_ns)
{
	if (source->genlock_last_log_ns == 0)
		source->genlock_last_log_ns = now_ns;
	if (now_ns - source->genlock_last_log_ns < GENLOCK_AUDIT_LOG_INTERVAL_NS)
		return;
	source->genlock_last_log_ns = now_ns;
	/* camera-box #97: print the per-source preload AND its ms-equivalent video
	 * delay (preload=N (=M ms @ Ffps)) so the live delay is visible in the OBS log
	 * for the operator + post-deploy verification. */
	/* camera-box #200: tear-checked fps snapshot (see genlock_video_fps) — the audit
	 * path read the unlocked ovi pair, which obs_reset_video can tear.
	 * camera-box #259: guard fps_den too (not fps_num alone). The latency_frames integer
	 * divide below is `/ (1000ULL * fps_den)` — a SIGFPE on the render thread if
	 * fps_den==0. Mirror genlock_source_drop_cap, which guards the identical divide with
	 * `fps_den != 0`; when false we fall through to the fps-unknown branch (fps=0.0,
	 * latency_frames=0). */
	uint32_t fps_num, fps_den;
	const bool have_vi = genlock_video_fps(&fps_num, &fps_den) && fps_num != 0 && fps_den != 0;
	const double fps = have_vi ? (double)fps_num / (double)fps_den : 0.0;
	/* camera-box #184/#235/#245: print the active genlock latency — now PER-SOURCE: the
	 * source's own override when set (>0) else the GLOBAL default. The headline
	 * `latency_ms=N (≈M frames)` is the EFFECTIVE held latency for THIS source, so the
	 * audit log of two sources with different overrides reads e.g. `latency_ms=1000` vs
	 * `latency_ms=3` — the per-source proof the rig validation reads live. The explicit
	 * `src_latency_ms=` (the raw per-source override, 0 = follows global) +
	 * `global_latency_ms=` fields disambiguate override-vs-global. The `reserve_ms=N`
	 * field is KEPT (the #128 launch wrapper's log verify keys on it) and equals the
	 * effective latency_ms. Mirror of src/probe/genlock.rs effective_latency_ms. */
	const uint32_t global_latency_ms = genlock_latency_ms();
	const uint32_t latency_ms = source->genlock_latency_ms > 0 ? source->genlock_latency_ms : global_latency_ms;
	const unsigned long long latency_frames =
		have_vi ? ((unsigned long long)latency_ms * fps_num + (1000ULL * fps_den) / 2) /
				  (1000ULL * fps_den)
			: 0ULL;
	blog(LOG_INFO,
	     "genlock-fifo audit '%s': received=%llu consumed=%llu underruns=%llu "
	     "holds=%llu overruns=%llu backward_steps=%llu dropped_due=%llu relocks=%llu late_holds=%llu locked=%d "
	     "depth=%zu peak=%u latency_ms=%u (≈%llu frames @ %.3ffps) "
	     "src_latency_ms=%u global_latency_ms=%u "
	     "preload=%u (=%llu ms) reserve_ms=%u cap=%zu empty_run=%u (re-arm@%u) "
	     "ts_present=%llu ts_due=%u ts_head_skew_ms=%lld "
	     /* camera-box #1009: cumulative backward-step RE-ANCHOR TICKS (backward_steps=
	      * counts EVENTS; this counts every re-anchored tick, so a sustained regime is
	      * visible as a climbing rate). Healthy operation keeps it at 0 -- the E2E/drift
	      * gates assert the delta stays 0 across a run. Appended AFTER the existing
	      * fields (scripts parse by field name; #401 anchor pins the earlier run). */
	     "backward_regime_ticks=%llu "
	     /* camera-box #1049: cumulative SETTLE-BACK PHASE-CONVERGENCE sheds — a converge shed
	      * counts into genlock_dropped_due like every other drop, so this distinguishes it (the
	      * genlock-hold-collapse playbook lesson: log silence lies). A climbing rate = the shed
	      * is actively pulling a per-camera acquire-phase back toward configured; it must go
	      * QUIET once the phase converged. Post-deploy verification of this ticket reads it. */
	     "converge_sheds=%u "
	     /* camera-box #800: wall(RTC/GetSystemTimePreciseAsFileTime)-vs-monotonic
	      * (QPC/os_gettime_ns) clock drift since the first audit tick, in ms. The video release deadline
	      * is WALL-slaved; the render tick + audio capture are QPC-slaved. A day-long
	      * divergence of the two clock domains is the leading remaining candidate for the #800
	      * A/V shift the instrumented FIFO already ruled out — one grep of this field over a
	      * captured log answers it. Process-global (identical on every source's line); positive
	      * = wall ran faster than QPC (video deadline ahead of audio). Parsed by the input-side
	      * AuditSample.wall_qpc_drift_ms in src/jitter_audit.rs. */
	     "wall_qpc_drift_ms=%lld "
	     "(#70/#97/#126/#147/#148/#184/#235/#245/#401/#1049/#800)",
	     source->context.name ? source->context.name : "?",
	     (unsigned long long)source->genlock_frames_received,
	     (unsigned long long)source->genlock_frames_consumed,
	     (unsigned long long)source->genlock_underruns,
	     (unsigned long long)source->genlock_holds,
	     (unsigned long long)source->genlock_overruns,
	     (unsigned long long)source->genlock_backward_steps,
	     /* camera-box #401: the phase-locked cadence's honest loss/state signals —
	      * dropped_due (frames the release DISCARDED — the pre-#401 silent erase),
	      * relocks (drift-guard catch-up jumps), late_holds (boundary matured but the
	      * frame never arrived — upstream late/lost, distinct from the benign
	      * source-early holds=), locked (0 = cadence unlocked / re-acquiring). */
	     (unsigned long long)source->genlock_dropped_due, (unsigned long long)source->genlock_relocks,
	     (unsigned long long)source->genlock_late_holds,
	     source->genlock_locked_next_boundary_ns != 0 ? 1 : 0, source->async_frames.num,
	     source->genlock_peak_depth, latency_ms, latency_frames, fps,
	     source->genlock_latency_ms, global_latency_ms,
	     source->genlock_preload, (unsigned long long)genlock_preload_ms(source->genlock_preload),
	     latency_ms /* reserve_ms == effective latency_ms; kept for the #128 log verify */,
	     genlock_source_drop_cap(source), source->genlock_empty_run,
	     (unsigned)GENLOCK_REARM_EMPTY_TICKS,
	     /* camera-box #148: the ts-align decision sample (present_ts / due / head-frame
	      * skew in ms) — the per-tick present/hold/drop relationship the #136 churn
	      * diagnosis lacked. Sampled on the ts-align path; reset to 0 (the sentinel) on
	      * every count-gate / true-empty tick by genlock_clear_ts_sample (#269 [5]) so
	      * this never prints a STALE sample from an earlier ts-align tick. */
	     (unsigned long long)source->genlock_last_present_ts,
	     source->genlock_last_due,
	     (long long)(source->genlock_last_head_skew_ns / 1000000),
	     (unsigned long long)source->genlock_backward_regime_ticks,
	     source->genlock_converge_sheds,
	     genlock_wall_qpc_drift_ms());
}
/* ---- end genlock FIFO preload + audit ------------------------------------ */

/* camera-box #726 STICKY-N: FRESHLY measure this genlock source's integer rate multiple N from
 * the STAMP GRID of the front queued frames, or 0 when it cannot be measured this tick. The delta
 * of a strictly-increasing consecutive stamp pair is the true source frame interval regardless of
 * arrival jitter (a single NDI source delivers in monotonic capture order). A 60fps source into
 * a 30fps canvas stamps every ~16.6ms, so canvas_interval / src_interval ~= 2 => N==2; a 1:1 source
 * (30fps into 30fps) stamps every canvas interval => N==1 (the present-oldest lossless-drain STEADY
 * path is then unchanged). #741/#707 B2: SCAN the first GENLOCK_MEASURE_SCAN_DEPTH entries for that
 * pair rather than reading only array[0..1] — a DUPLICATE front stamp / arrival-non-monotonic seam
 * at array[0..1] used to read INCONCLUSIVE and (sustained) crawl. Returns 0 = INCONCLUSIVE: fewer
 * than 2 queued frames, or NO strictly-increasing pair in the scan window. A non-zero return
 * (N>=1) is a genuine measurement — the CONFIRMATION authority; the sticky latch
 * (genlock_effective_source_multiple) bridges ONLY the 0 ticks. Mirror of src/probe/genlock.rs
 * ReleaseCadence::measure_source_multiple. */
static inline uint32_t genlock_measure_source_multiple(const obs_source_t *source, uint64_t canvas_interval_ns)
{
	if (canvas_interval_ns == 0 || source->async_frames.num < 2)
		return 0; /* inconclusive: fewer than 2 queued frames */
	/* #741/#707 B2 ROBUST: the very front pair (array[0],array[1]) can be momentarily
	 * degenerate — a DUPLICATE capture stamp (t1==t0) or an arrival-order non-monotonic
	 * seam (t1<t0). Reading ONLY array[0..1] then returned INCONCLUSIVE, and a SUSTAINED
	 * inconclusive run on a jittery 60-into-30 input dropped the release to the present-oldest
	 * CRAWL (#707 B2: a window uniform=0.481, histogram {1:295,2:407,3:102,7:39}). Scan the
	 * first K queued entries and take the MINIMUM strictly-increasing CONSECUTIVE delta as the
	 * source frame interval. #1042: every source stamps on the monotonic, evenly-spaced
	 * DanteSync grid, so the TRUE frame interval is the SMALLEST adjacent gap — a duplicate or a
	 * dropped/decimated frame only ever ENLARGES a gap, never shrinks it below the true period.
	 * Taking the FIRST increasing pair (pre-#1042) over-stated the interval whenever that pair
	 * straddled a dropped frame (two source intervals), under-stating the multiple and
	 * collapsing the backlog-relock threshold on a genuine 60-into-30 source (stream box's
	 * 'Zaloha kamera', ~1/sec spurious relocks — the #796 health-signal complaint). The minimum
	 * is byte-identical to the first pair on any clean grid-stamped window. Still 0
	 * (inconclusive) when NO increasing pair exists in the window: the sticky latch bridges the
	 * 0. Mirror of src/genlock_backlog.rs source_interval_from_stamps (Tier-0 unit-tested) and
	 * src/probe/genlock.rs ReleaseCadence::measure_source_multiple. */
	const size_t scan = source->async_frames.num < (size_t)GENLOCK_MEASURE_SCAN_DEPTH
				    ? source->async_frames.num
				    : (size_t)GENLOCK_MEASURE_SCAN_DEPTH;
	uint64_t src_interval = 0;
	for (size_t i = 0; i + 1 < scan; i++) {
		const uint64_t a = source->async_frames.array[i]->timestamp;
		const uint64_t b = source->async_frames.array[i + 1]->timestamp;
		if (b > a) {
			const uint64_t d = b - a;
			if (src_interval == 0 || d < src_interval)
				src_interval = d; /* #1042: MIN adjacent grid delta, not the first */
		}
	}
	if (src_interval == 0)
		return 0; /* inconclusive: no strictly-increasing pair in the first K entries */
	/* round-to-nearest N = canvas / src; clamp to >=1 (a slower-than-canvas source reads 1). */
	const uint64_t n = (canvas_interval_ns + src_interval / 2) / src_interval;
	return (uint32_t)(n < 1 ? 1 : n);
}

/* camera-box #726 STICKY-N: the EFFECTIVE source rate multiple to release at THIS tick. A fresh
 * measurement (genlock_measure_source_multiple) is the CONFIRMATION authority — when the front pair
 * is measurable it WINS and updates the latch (so a genuine 1:1 rate re-latches to 1 => the
 * present-oldest lossless path, byte-identical). When the front pair is INCONCLUSIVE (momentary
 * async_frames.num < 2 / a non-monotonic clock-step seam) it BRIDGES with the last confirmed
 * multiple instead of crawling — the #726 win5/win6 residual fix (a jittery input's sustained
 * inconclusive run under-drained the queue into the backlog storm). It NEVER invents a multiple: an
 * unconfirmed latch (0) reads 1. The latch is CLEARED on acquire/relock/gap/backward-step (see the
 * callers below) so a stale N cannot outlive its rate. Takes a NON-const source (writes the latch).
 * Mirror of src/probe/genlock.rs ReleaseCadence::effective_source_multiple. */
static inline uint32_t genlock_effective_source_multiple(obs_source_t *source, uint64_t canvas_interval_ns)
{
	const uint32_t fresh = genlock_measure_source_multiple(source, canvas_interval_ns);
	if (fresh >= 1) {
		source->genlock_last_known_n = fresh; /* fresh measurement is the confirmation authority */
		return fresh;
	}
	/* inconclusive — bridge with the last confirmed multiple; never invent (0 -> 1). */
	return source->genlock_last_known_n >= 1 ? source->genlock_last_known_n : 1;
}

/* camera-box #859: the BACKLOG-STORM queue-depth threshold for a source, relative to the depth
 * that source's OWN configured latency implies. A queue is only in backlog when it exceeds the
 * buffer it was deliberately configured to hold, plus the original GENLOCK_QDEPTH_RELOCK_MARGIN.
 *
 * The source-rate multiple matters: async_frames counts frames as the SOURCE delivered them, so a
 * 60 fps input on a 30 fps canvas queues two entries per canvas interval and its implied depth is
 * twice what the canvas rate alone would suggest.
 *
 * READ-ONLY on purpose — it uses the PURE genlock_measure_source_multiple with the sticky latch as
 * fallback, NOT genlock_effective_source_multiple, which WRITES source->genlock_last_known_n. This
 * threshold is consulted on ticks that never latched before, and merely computing a threshold must
 * not acquire a write path. Same value, no new side effect.
 *
 * Round-to-nearest, matching genlock_source_drop_cap's own latency->frames rounding so the two
 * latency-derived quantities in this FIFO agree. A degenerate interval yields the bare margin —
 * identical to the pre-#859 behaviour — rather than dividing by zero.
 *
 * Mirror of src/genlock_backlog.rs backlog_relock_threshold (Tier-0 unit-tested) and
 * src/probe/genlock.rs ReleaseCadence::backlog_relock_qdepth — keep all three in lock-step. */
static size_t genlock_backlog_relock_qdepth(const obs_source_t *source, uint32_t reserve_ms, uint64_t interval)
{
	if (interval == 0)
		return GENLOCK_QDEPTH_RELOCK_MARGIN;
	const uint32_t measured = genlock_measure_source_multiple(source, interval);
	const uint32_t n = measured >= 1 ? measured
					 : (source->genlock_last_known_n >= 1 ? source->genlock_last_known_n : 1);
	const uint64_t held_ns = (uint64_t)reserve_ms * 1000000ULL * (uint64_t)n;
	const uint64_t depth = (held_ns + interval / 2) / interval;
	/* camera-box #940 piece 2: scale the MARGIN by the source's own rate multiple n -- a
	 * 60-into-30 camera ingest queues an ARRIVAL SURPLUS of n frames per canvas tick (plus
	 * measured cam->strih jitter that bunches them further), which permanently exceeds the
	 * bare (n==1) margin at the rig's shallow per-source latencies and fires the
	 * backlog-relock branch on ~every tick (~35-70/5min window, live #940 audit). n==1
	 * (every 30-into-30 source, incl. this ticket's own 'NDI 2ME PGM') is BYTE-IDENTICAL to
	 * the pre-#940 threshold. Mirror: src/genlock_backlog.rs backlog_relock_threshold
	 * (Tier-0 unit-tested). */
	return (size_t)(depth + (uint64_t)GENLOCK_QDEPTH_RELOCK_MARGIN * (uint64_t)n);
}

/* camera-box #859 follow-up: SLEW-LIMITED SETTLE-BACK DRAIN decision — should this tick shed
 * exactly ONE EXTRA frame to settle the queue back toward the depth its OWN configured latency
 * implies, after a setpoint change? The #859 fix above stopped the backlog-relock branch firing
 * every tick in steady state, but that branch was ALSO the FIFO's only mechanism for shedding
 * excess queue depth after a genlock latency SETPOINT INCREASE. With it gated off, the plain
 * N==1 steady release (exactly one frame per tick) holds depth CONSTANT forever: it never falls
 * further behind, but it never catches up either. Measured live: a +34 ms setpoint step produced
 * +134 ms of ACTUAL delay, stable across 6 consecutive samples 20+ minutes apart — a parked
 * overshoot, not a decaying transient.
 *
 * This is a bounded, ADDITIONAL path alongside the unchanged backlog-relock branch: at most once
 * every GENLOCK_DRAIN_MIN_TICK_INTERVAL ticks, while depth exceeds the target implied by
 * reserve_ms plus GENLOCK_DRAIN_HYSTERESIS_FRAMES, shed exactly one extra frame. The rate is
 * bounded by construction, so it can never reproduce the every-tick paired duplicate/skip storm
 * the disabled branch used to cause.
 *
 * READ-ONLY like genlock_backlog_relock_qdepth (same rationale: a decision getter must not
 * acquire a write path) — uses the pure measurement with the sticky latch as fallback.
 *
 * camera-box #998: the TARGET below is CEIL, not round-to-nearest (unlike
 * genlock_backlog_relock_qdepth above, which stays round — a different quantity with a
 * different, already-analyzed caller). The ts-align hold's own natural steady depth is
 * ceil(latency/interval) + 1..+2 (plus arrival jitter) — strictly ABOVE the floor of
 * latency/interval. Round-to-nearest picks the WRONG side of that whenever
 * frac(latency/interval) < 0.5 (round == floor): the target undershoots the hold's true
 * steady depth by exactly one frame, so `depth > target + GENLOCK_DRAIN_HYSTERESIS_FRAMES`
 * holds PERMANENTLY even at the queue's own correct depth — this branch fires every
 * GENLOCK_DRAIN_MIN_TICK_INTERVAL ticks, sheds a frame, the boundary re-anchors low, and the
 * very next tick's hold regains it via a late hold: one duplicated + one skipped program
 * frame every ~GENLOCK_DRAIN_MIN_TICK_INTERVAL ticks, forever, on any source whose latency
 * happens to land below-half-frac. Measured live on the stream box's 'NDI 2ME PGM':
 * +152 genlock_dropped_due / +151 late holds per ~355s run at reserve_ms=941 (frac .23),
 * +161/+162 at 915 (frac .45); +0 at 856/891/930 (frac .68/.73/.90, where round==ceil
 * already, so the bug was silent there — this is why it looked intermittent rather than
 * a plain regression). CEIL is an upper bound of the natural depth, so it can never sit
 * below it; at frac >= 0.5 ceil == round, so every previously-clean source is unaffected,
 * and a genuine backlog (depth far past even the corrected target) still drains.
 *
 * This is a SEPARATE claim from the "self-cancelling no-op" comment further below, near the
 * actual drop-older/present-newest call site: that comment's simulation validated the DROP
 * IDIOM at a correctly-computed target. It did not (and could not) rule out this target
 * itself picking the wrong value at frac<0.5 -- a different bug that reproduces the exact
 * same visible symptom (one dup + one skip) via a different path.
 *
 * Mirror of src/genlock_backlog.rs should_drain_one / drain_target_frames (Tier-0
 * unit-tested) and src/probe/genlock.rs ReleaseCadence::should_drain_one (a pure delegator
 * to the Rust should_drain_one above -- no independent arithmetic there, so it inherits
 * this fix automatically) — keep all three in lock-step. */
static bool genlock_should_drain_one(const obs_source_t *source, uint32_t reserve_ms, uint64_t interval)
{
	if (interval == 0)
		return false;
	const uint32_t measured = genlock_measure_source_multiple(source, interval);
	const uint32_t n = measured >= 1 ? measured
					 : (source->genlock_last_known_n >= 1 ? source->genlock_last_known_n : 1);
	const uint64_t held_ns = (uint64_t)reserve_ms * 1000000ULL * (uint64_t)n;
	const uint64_t target = (held_ns + interval - 1) / interval; /* #998: CEIL, not round */
	const uint64_t depth = (uint64_t)source->async_frames.num;
	return depth > target + GENLOCK_DRAIN_HYSTERESIS_FRAMES &&
	       source->genlock_ticks_since_drain >= GENLOCK_DRAIN_MIN_TICK_INTERVAL;
}

/* camera-box #1049: the STEADY-conveyor PHASE-CONVERGENCE shed decision, PURE part.
 * Self-contained (only stdint + the scalars) so tests/genlock_relock_selection_parity.rs can
 * lift it standalone and prove it byte-identical to the Rust authority
 * src/genlock_backlog.rs should_converge_phase.
 *
 * The phase-locked conveyor (genlock_locked_next_boundary_ns) is a pure FOLLOWER: it re-anchors
 * to the presented stamp every STEADY present and has no restoring force toward the configured
 * latency, so whatever phase it locks at ACQUIRE (or after a walk event -- a GAP RESYNC adopting
 * the oldest frame's age, a sticky-N present-oldest crawl, a connect-burst ACQUIRE) is carried
 * forward forever. The #1003 anchor-nearest relock PRESERVES that phase by design, and the #859
 * depth drain cannot catch a 1-2 canvas-frame phase error (2-frame hysteresis) -- so the strih
 * 60-into-30 ingests locked a per-camera frame-quantized A/V-offset ladder that never converged
 * (issue 1049, 5 E2E runs 2026-08-14).
 *
 * The comparator is the conveyor's own on-air age S = wall_now - boundary (at decision time
 * boundary == last_presented_ts + interval and render ticks are one interval apart, so this ==
 * the last tick's on-air age but for the +-2 ms slew the hysteresis absorbs -- the boundary is
 * always live, unlike the saturating phase anchor). The TARGET it converges toward is
 * max(reserve, floor), floor = wall_now - newest_stamp_ns, the age of the FRESHEST queued frame
 * (the smallest on-air age physically presentable -- a frame cannot go on air before it ARRIVES).
 * #1049 review finding: a reserve-only target ignored the transport-skew floor, so when
 * skew > reserve + interval/n + hysteresis (the rig's ~20 ms skew at the 3 ms prod floor) the shed
 * fired forever at the natural phase -- the #998 drop/regain limit cycle. Fires when S has drifted
 * a shed-quantum (one SOURCE interval interval/n) + the 5 ms hysteresis ABOVE that target,
 * throttled by the SHARED #859 drain counter. post-shed S' = S - interval/n is always
 * > target >= floor, so it cannot rebuild the #998 limit cycle. A DEEP source (#1003, Zaloha
 * 1000 ms) has S ~= configured, and a shallow high-skew source has S ~= floor, both far below the
 * threshold -> inert by the SAME comparison, not a special case. Mirror of
 * src/genlock_backlog.rs should_converge_phase (Tier-0 unit-tested) -- keep both in lock-step. */
static inline bool genlock_phase_converge_due(uint64_t wall_now_ns, uint64_t boundary_ns,
					      uint64_t newest_stamp_ns, uint32_t latency_ms,
					      uint64_t interval_ns, uint32_t n, uint64_t ticks_since_drain)
{
	if (interval_ns == 0 || boundary_ns == 0)
		return false;
	/* camera-box #1049 (coordinator's live finding): N>=2 ONLY. An N==1 source (30-into-30)
	 * delivers one frame per tick, so a phase shed cannot stick -- the queue holds and regains
	 * within the throttle window (shed->hold->shed, the #998 dup+skip signature; measured live on
	 * the stream box's deep NDI 2ME PGM, 990 ms, natural grid-quantized hold ~1033 ms one frame
	 * above configured at frac 0.7). N>=2 delivers >=2 frames/tick so the shed sticks -- and only
	 * N>=2 carries the per-camera ladder pathology. Mirror: src/genlock_backlog.rs
	 * should_converge_phase source_multiple < 2 early return. */
	if (n < 2)
		return false;
	const uint64_t nn = n >= 1 ? (uint64_t)n : 1;
	const uint64_t reserve_ns = (uint64_t)latency_ms * 1000000ULL;
	const uint64_t floor_ns = wall_now_ns > newest_stamp_ns ? wall_now_ns - newest_stamp_ns : 0;
	const uint64_t target = reserve_ns > floor_ns ? reserve_ns : floor_ns;
	const uint64_t quantum = interval_ns / nn;
	const uint64_t threshold = target + quantum + GENLOCK_PHASE_PIN_HYSTERESIS_NS;
	const uint64_t age = wall_now_ns > boundary_ns ? wall_now_ns - boundary_ns : 0;
	return age > threshold && ticks_since_drain >= GENLOCK_DRAIN_MIN_TICK_INTERVAL;
}

/* camera-box #1049: the source-bound wrapper -- reads the live n (READ-ONLY, same as
 * genlock_should_drain_one), the locked boundary + shared throttle, and the FRESHEST queued frame
 * (async_frames.array[num-1], the achievable-floor reference), delegates the arithmetic to
 * genlock_phase_converge_due above. */
static bool genlock_should_converge_phase(const obs_source_t *source, uint32_t reserve_ms,
					  uint64_t interval, uint64_t wall_now)
{
	if (interval == 0 || source->async_frames.num == 0)
		return false;
	const uint32_t measured = genlock_measure_source_multiple(source, interval);
	const uint32_t n = measured >= 1 ? measured
					 : (source->genlock_last_known_n >= 1 ? source->genlock_last_known_n : 1);
	const uint64_t newest_stamp =
		source->async_frames.array[source->async_frames.num - 1]->timestamp;
	return genlock_phase_converge_due(wall_now, source->genlock_locked_next_boundary_ns, newest_stamp,
					  reserve_ms, interval, n, source->genlock_ticks_since_drain);
}

/* camera-box #1161: the fail-open MARGIN (ticks) the ACQUIRE bracketing gate
 * (genlock_relock_acquire_should_hold) adds on top of ceil(reserve/interval) before it
 * force-acquires regardless of queue depth. Mirror of src/genlock_backlog.rs
 * ACQUIRE_BRACKET_FAILOPEN_TICKS. */
#define GENLOCK_ACQUIRE_BRACKET_FAILOPEN_TICKS 3ULL

/* camera-box #1161: the STAGE-2 ACQUIRE BRACKETING GATE decision, PURE part. Self-contained
 * (only stdint scalars) so tests/genlock_relock_selection_parity.rs can lift it standalone and
 * prove it byte-identical to the Rust authority src/genlock_backlog.rs relock_acquire_should_hold.
 *
 * The floor-3 aligner raises a source's latency pin to move its presented frame DEEPER, but a
 * per-source pin INCREASE is structurally inert: obs_source_set_genlock_latency_ms re-arms the
 * ms-path-inert fill latch + clears the phase anchor but (pre-#1161) never zeroed the conveyor
 * boundary, so the ACQUIRE branch never re-ran; and genlock_phase_converge_due sheds DOWNWARD
 * only. The PRIMARY frame-mover is obs_source_set_genlock_latency_ms zeroing the boundary on a
 * pin RISE (forcing a re-acquire): that alone lets the ACQUIRE branch rebuild the FIFO to the
 * raised depth, moving the presented frame off the shallow old-depth toward the new reserve (it is
 * what closes the ticket's one-canvas-frame residual). THIS gate is the PRECISION half. Without
 * it a bare re-acquire acquires as soon as a frame is `due`; because genlock_phase_pin_deadline
 * FLOORS the deadline to the receiver grid, a `due` frame is at least
 * reserve - GENLOCK_PHASE_PIN_HYSTERESIS_NS (5 ms) old, so the bare acquire lands up to ~5 ms
 * BELOW the raised target (and genlock_relock_select_nearest could pick a slightly-younger
 * neighbour) -- a SUB-FRAME residual the downward-only shed can never raise back. This gate HOLDs
 * the acquire until the OLDEST queued frame has aged to the FULL reserve (the queue deepens ~one
 * interval/tick and brackets the target within ceil(reserve/interval) ticks), so a frame AT the
 * target depth exists for the selection to land on; the caller then runs the existing
 * genlock_relock_select_nearest byte-identical (phase re-anchored via history, never free-run).
 * The fail-open cap (ceil(reserve/interval) + GENLOCK_ACQUIRE_BRACKET_FAILOPEN_TICKS ticks)
 * degrades a pathological never-deepening queue (an overrun-capped delay line) to today's
 * acquire rather than freezing -- no new hold-collapse mode. INERT at the production 3 ms pin
 * (the ACQUIRE branch is only entered at cold start / a forced re-acquire, and the oldest queued
 * frame is essentially always older than 3 ms), and gated to N>=2 at the call site (the deep
 * N==1 source is already deterministic on cold acquire). Mirror of src/genlock_backlog.rs
 * relock_acquire_should_hold (Tier-0 unit-tested) -- keep both in lock-step. */
static inline bool genlock_relock_acquire_should_hold(uint64_t oldest_queued_age_ns,
						      uint64_t reserve_ns, uint64_t interval_ns,
						      uint64_t ticks_held)
{
	if (interval_ns == 0)
		return false;
	if (oldest_queued_age_ns >= reserve_ns)
		return false;
	const uint64_t cap = (reserve_ns + interval_ns - 1) / interval_ns +
			     GENLOCK_ACQUIRE_BRACKET_FAILOPEN_TICKS;
	return ticks_held < cap;
}

static bool genlock_release_tick(obs_source_t *source, uint64_t wall_now, uint64_t present_ts,
				 size_t due, uint64_t interval, uint32_t reserve_ms, uint64_t now_ns)
{
	/* ---- camera-box #401: PHASE-LOCKED release cadence (v2) ---------
	 * WHY: this release used to re-derive the deadline from the wall
	 * clock EVERY tick (present_ts above) and present the NEWEST due
	 * frame, silently erasing the older due ones (to_drop = due - 1
	 * with NO counter). With render ticks and capture stamps on the
	 * same DanteSync 60 Hz grid, a reserve near a multiple of the frame
	 * interval puts the deadline ON a stamp: the ±2 ms render-tick slew
	 * then flips that frame due/not-due tick-to-tick — alternating HOLD
	 * + silent DROP. Measured live (run 7020001, 'NDI cam5'): 43.9–57.7
	 * distinct fps of a 60 fps flow, 8,511 ids lost, invisible in the
	 * audit. FIX: key the release on a LOCKED boundary that advances
	 * exactly one interval per presented frame — slew-immune by
	 * construction. The wall deadline (present_ts) is consulted ONLY
	 * to ACQUIRE the lock and to AGE frames (backlog/gap/late paths);
	 * it is NEVER compared against the boundary to force a re-lock.
	 * v2 (live canary of v1, 2026-07-02, strih 'NDI cam5'): v1's
	 * wall-based drift guard (present_ts > boundary + 2.25*interval)
	 * EMBEDS the constant stamp->arrival skew (59 ms live at the 3 ms
	 * reserve) and relock-stormed — dropped_due 2918 of 4202 received
	 * (69 %), relocks 1076. v2 guards backlog QUEUE-RELATIVE (depth >
	 * genlock_backlog_relock_qdepth() — #859: the depth the source's OWN
	 * configured latency implies, plus GENLOCK_QDEPTH_RELOCK_MARGIN. The
	 * pre-#859 bare constant assumed "steady depth is ~1-2 at any skew",
	 * true only for a SHALLOW source; a source pinned deep for A/V
	 * alignment sat permanently above it and relocked every tick)
	 * and releases strict FIFO on the STEADY
	 * path (present the OLDEST matured frame; a transient 2-frame
	 * maturation drains losslessly next tick). EVERY discarded frame
	 * counts into genlock_dropped_due (steady state drops ZERO).
	 * #136 in-sync is preserved: every source stamps on the shared
	 * grid, so locked boundaries are grid-aligned across sources.
	 * Mirror of src/probe/genlock.rs ReleaseCadence::tick (v2) — keep
	 * the C and the Rust reference in lock-step (its cadence tests,
	 * incl. the deep-skew and mid-run-skew-shift regression locks,
	 * are the proof harness). */
	size_t release;
	/* camera-box #859 follow-up: true only on the plain N==1 STEADY
	 * release path below (release=1/tick) — the ONE case this
	 * ticket's evidence found holds queue depth CONSTANT forever
	 * after a setpoint-change overshoot. Every other path already has
	 * its own catch-up behaviour and is left untouched. */
	bool drain_eligible = false;
	/* camera-box #1003: true ONLY on the STEADY and GAP-RESYNC paths --
	 * the conveyor presents. Those are the presents whose measured
	 * on-air age IS the phase anchor. The relock paths (ACQUIRE /
	 * BACKLOG) deliberately leave it false: a relock INHERITS the
	 * phase, it must never redefine it, or every episode re-mints one
	 * and the whole fix is undone. */
	bool anchor_update = false;
	/* camera-box #1049: true ONLY on the two STEADY presents (N==1 and N>=2) -- the conveyor
	 * paths that carry a persistent presentation phase and can shed one extra frame to converge
	 * it toward the configured latency. NOT the GAP-RESYNC (it RE-DERIVES the phase from the
	 * frame it puts on air, so there is nothing to converge) nor the relock paths. Sheds via the
	 * genlock_should_converge_phase decision in the present tail, sharing the #859 drain throttle
	 * (genlock_ticks_since_drain). */
	bool converge_eligible = false;
	if (source->genlock_locked_next_boundary_ns == 0) {
		/* UNLOCKED — ACQUIRE: the first wall-due frame locks the
		 * cadence. #1003: the frame PRESENTED is the one nearest the
		 * tracked phase anchor (on a genuine cold start the anchor is
		 * unset, so the target is the configured latency and this
		 * behaves as before); the older ones are counted dropped. It
		 * is NOT the newest due any more -- that instant-sampled rule
		 * re-minted a fresh release phase on every lock episode. */
		/* #726 STICKY-N: a fresh acquire (cold start OR after a source
		 * reset that zeroed the boundary) re-confirms the source
		 * multiple from scratch -- clear the latch so a stale N from a
		 * previous lock can't outlive it. */
		source->genlock_last_known_n = 0;
		/* #859 follow-up: a fresh lock starts the settle clock over —
		 * nothing has overshot yet immediately after acquiring. */
		source->genlock_ticks_since_drain = 0;
		/* camera-box #1161: ACQUIRE BRACKETING GATE (N>=2 only) -- the PRECISION half of the
		 * frame-mover (the primary half is obs_source_set_genlock_latency_ms zeroing the boundary
		 * on a pin RISE, which forces THIS re-acquire and rebuilds the FIFO to the raised depth).
		 * A bare re-acquire acquires as soon as a frame is `due`; because genlock_phase_pin_deadline
		 * FLOORS the deadline, a `due` frame is only >= reserve - GENLOCK_PHASE_PIN_HYSTERESIS_NS
		 * (5 ms) old, so it lands up to ~5 ms BELOW the raised target (and relock-select could pick
		 * a slightly-younger neighbour) -- a sub-frame residual the downward-only #1049 shed can
		 * never raise. HOLD until the oldest queued frame has aged to the FULL reserve, THEN fall
		 * through to the existing genlock_relock_select_nearest below (phase re-anchored via
		 * history, never free-run).
		 * The fail-open cap degrades a queue that never deepens to today's acquire -- no new
		 * hold-collapse mode. N>=2 ONLY: the deep N==1 source is already deterministic on cold
		 * acquire, so leave it untouched. Mirror of src/genlock_backlog.rs
		 * relock_acquire_should_hold (Tier-0 unit-tested) + the C-vs-Rust parity gate. */
		if (genlock_effective_source_multiple(source, interval) >= 2) {
			const uint64_t oldest_age =
				wall_now > source->async_frames.array[0]->timestamp
					? wall_now - source->async_frames.array[0]->timestamp
					: 0;
			const bool bracket_hold = genlock_relock_acquire_should_hold(oldest_age,
							       (uint64_t)reserve_ms * 1000000ULL,
							       interval,
							       source->genlock_acquire_bracket_ticks);
			/* camera-box #1161 OBSERVABILITY (debug direction 3): one line per ACQUIRE-branch
			 * tick — a RARE, BOUNDED (re)acquire episode (cold start, a pin-RISE forced
			 * re-acquire via obs_source_set_genlock_latency_ms, or a backward-regime-end), never
			 * the STEADY path — exposing WHY a raised per-source pin did or did not deepen the
			 * FIFO. The merged Stage-2 gate was SILENT here, so a live pin rise that stayed
			 * HOLD-INERT (issue 1161) left no trace beyond the setter's own `(#245)` line. A pin
			 * BELOW the source's arrival transport floor prints oldest_queued_age_ms >= reserve_ms
			 * with decision=ACQUIRE -> the bracketing gate cannot engage and
			 * genlock_relock_select_nearest re-locks at the UNCHANGED shallow phase (the FIFO
			 * cannot present a frame fresher than the arrival edge; the floor-3 aligner must target
			 * an above-floor pin). decision=HOLD with a climbing ticks_held is the gate deepening
			 * the queue toward the raised reserve. Marker string mutually-non-substring vs
			 * genlock-fifo audit / genlock-relock / genlock-ndi-output per the jitter-audit-parser
			 * rule; NOT parsed by src/jitter_audit.rs (a one-shot diagnostic, no periodic metric to
			 * add). */
			blog(LOG_INFO,
			     "genlock-acquire-bracket '%s': reserve_ms=%u oldest_queued_age_ms=%llu "
			     "decision=%s ticks_held=%u depth=%zu (#1161)",
			     source->context.name ? source->context.name : "?", reserve_ms,
			     (unsigned long long)(oldest_age / 1000000ULL),
			     bracket_hold ? "HOLD" : "ACQUIRE",
			     source->genlock_acquire_bracket_ticks, source->async_frames.num);
			if (bracket_hold) {
				source->genlock_acquire_bracket_ticks++;
				source->genlock_holds++;
				genlock_audit_log(source, now_ns);
				return false;
			}
		}
		source->genlock_acquire_bracket_ticks = 0;
		if (due == 0) {
			/* #148: a BENIGN source-early HOLD (frames queued, none
			 * yet due) -> genlock_holds, NOT a true-empty
			 * genlock_underruns. Repeat the current frame this tick. */
			source->genlock_holds++;
			genlock_audit_log(source, now_ns);
			return false;
		}
		/* camera-box #1003: select by PHASE CONTINUITY, not
		 * newest-due. The `due` scan above is UNCHANGED and still
		 * QUALIFIES this branch (due > 0); it simply no longer
		 * SELECTS. release = index + 1 so the unchanged
		 * `to_drop = release - 1` erase loop below retires exactly
		 * the older frames into genlock_dropped_due. On a cold
		 * ACQUIRE the anchor is unset and the target is the
		 * configured latency -- the phase the wall deadline would
		 * have produced anyway. */
		release = genlock_relock_select_nearest(source, wall_now, reserve_ms) + 1;
	} else if (source->async_frames.num >
			   genlock_backlog_relock_qdepth(source, reserve_ms,
							 interval) &&
		   due > 0) {
		/* BACKLOG STORM (v2 — queue-relative, NEVER wall-boundary
		 * drift): a stall's burst or a persistent inflow>presentation
		 * imbalance shows up as QUEUE DEPTH, which is immune to the
		 * constant stamp->arrival skew that relock-stormed v1's
		 * wall-based guard live (skew 59 ms, reserve 3 ms:
		 * dropped_due 2918/4202, relocks 1076). Re-lock (#1003: to the
		 * frame nearest the tracked phase anchor, no longer the newest
		 * due one), counting every jumped frame — the catch-up keeps
		 * the IMAG latency contract and the drop is VISIBLE (the
		 * pre-#401 release erased silently). A deep queue with
		 * due == 0 (a just-landed burst of FRESH frames, nothing aged
		 * past the reserve yet) deliberately falls through: the
		 * STEADY path drains it, or this branch fires once it ages. */
		source->genlock_relocks++;
		/* #741/#707 B2: do NOT clear genlock_last_known_n here. A
		 * backlog re-lock is a QUEUE-DEPTH event, NOT evidence the
		 * source RATE changed — the #726 clear made the very next
		 * INCONCLUSIVE tick crawl at N=1, which under a steady 60-into-30
		 * backlog re-grew the queue and re-triggered THIS relock: a
		 * self-sustaining crawl->relock loop (the #707 B2 crawl window,
		 * uniform=0.481). The latch bridges the post-relock inconclusive
		 * ticks with the still-correct multiple; a genuine rate change is
		 * re-confirmed by the next measurable front pair, and a real
		 * source-timeline discontinuity still clears it at
		 * acquire / gap resync / backward clock-step / flush. */
		/* camera-box #940 piece 1: INSTRUMENT each relock event with the
		 * phase evidence needed to attribute a future A/V-offset step to
		 * (or rule it out from) a specific relock: current depth, the
		 * depth the source's OWN configured latency implies
		 * (steady_depth_frames), due count, how many frames this event
		 * erases, head skew, and wall_grid_phase_ns — the deadline's own
		 * remainder mod the frame interval. Today that phase wanders
		 * tick to tick; piece 3 (phase-pinning) drives it to a FIXED
		 * value — this line is what lets a future analysis prove that
		 * empirically instead of assuming it. Logged once PER EVENT
		 * (relocks are the exact events #940's investigation traced the
		 * stepping to), never folded into the periodic 5s audit line
		 * (a snapshot, not a per-event trace). Mirror: none — the
		 * fields are plain arithmetic already available at this call
		 * site, nothing to port to Rust. Guarded in
		 * tests/genlock_release_cadence.rs (Tier-0) + both
		 * windows-genlock*.yml (the #912 lock-step-anchor lesson). */
		/* camera-box #1003: same phase-continuity selection as the
		 * ACQUIRE branch. A backlog relock must still SHED the
		 * stall's burst -- it does, by erasing every frame OLDER
		 * than the selected one (release - 1 of them) -- but it must
		 * no longer re-mint the release PHASE while doing it. */
		size_t sel_1003 =
			genlock_relock_select_nearest(source, wall_now, reserve_ms);
		/* camera-box #1003 (adversarial review finding): a BACKLOG relock
		 * that would shed NOTHING is proof the anchor is STALE. This
		 * branch only fires ABOVE the latency-implied depth, so an anchor
		 * pointing at (or before) the queue head cannot describe a queue
		 * this deep. Carrying it re-fires the branch every tick shedding
		 * nothing -- and since the branch pre-empts STEADY, drain_eligible
		 * is never set and the settle-back drain never runs either. Drop
		 * the stale anchor and re-select against the CONFIGURED latency:
		 * one relock sheds the overshoot, and the anchor rebuilds from the
		 * next STEADY present. ACQUIRE is deliberately exempt -- index 0
		 * there just means "present the head", and the fresh lock stops the
		 * branch re-firing. Mirror: the Tier-0 sim's relock_present. */
		if (sel_1003 == 0 && source->genlock_phase_anchor_ns != 0) {
			source->genlock_phase_anchor_ns = 0;
			sel_1003 = genlock_relock_select_nearest(source, wall_now,
								reserve_ms);
		}
		{
			/* #940 piece 1: re-derive n the SAME way genlock_backlog_relock_qdepth()
			 * did internally (READ-ONLY, same tick -> same result) so the logged
			 * steady_depth_frames subtracts the FULL scaled margin (#940 piece 2:
			 * MARGIN * n, not the bare MARGIN) -- otherwise a 60-into-30 source
			 * (n>=2) would log an inflated steady_depth_frames by MARGIN*(n-1). */
			const uint32_t measured_n_for_log =
				genlock_measure_source_multiple(source, interval);
			const uint32_t n_for_log =
				measured_n_for_log >= 1
					? measured_n_for_log
					: (source->genlock_last_known_n >= 1 ? source->genlock_last_known_n
									: 1);
			const size_t steady_depth_frames_for_log =
				(size_t)genlock_backlog_relock_qdepth(
					source, reserve_ms, interval) -
				(size_t)GENLOCK_QDEPTH_RELOCK_MARGIN * (size_t)n_for_log;
			/* camera-box #1003: wall_grid_phase_ns was BLIND. It
			 * logged present_ts %% interval, but #940 piece 3 floors
			 * present_ts to that very interval, so a floored value
			 * mod its own divisor is IDENTICALLY 0 on every run at
			 * every latency -- the one field meant to prove phase
			 * determinism could never report anything else. The
			 * three fields that replace it are the live post-deploy
			 * evidence for this ticket: tick_phase_ns (where in the
			 * grid this relock actually fired -- the Edge 1 input,
			 * and NOT floored), anchor_ns (the phase being
			 * inherited; 0 = unset -> the configured-latency
			 * fallback) and sel_vs_newest_due (how many frames the
			 * phase-continuity selection differs from the old rule
			 * -- non-zero is this fix actively preventing a step). */
			blog(LOG_INFO,
			     "genlock-relock '%s': depth=%zu steady_depth_frames=%zu "
			     "due=%zu erased=%zu head_skew_ms=%lld "
			     "tick_phase_ns=%llu anchor_ns=%llu sel_vs_newest_due=%lld "
			     "interval_ns=%llu latency_ms=%u",
			     source->context.name ? source->context.name : "?",
			     source->async_frames.num, steady_depth_frames_for_log,
			     due, sel_1003,
			     (long long)(source->genlock_last_head_skew_ns /
					 1000000),
			     (unsigned long long)(interval != 0 ? wall_now % interval : 0),
			     (unsigned long long)source->genlock_phase_anchor_ns,
			     (long long)((long long)sel_1003 - (long long)(due - 1)),
			     (unsigned long long)interval, reserve_ms);
		}
		release = sel_1003 + 1;
	} else if (source->async_frames.array[0]->timestamp <=
		   source->genlock_locked_next_boundary_ns) {
		/* STEADY (strict FIFO): the queue head matured by the LOCKED
		 * boundary. */
		if (genlock_effective_source_multiple(source, interval) >= 2) {
			/* camera-box #726: the source runs at an integer multiple
			 * N>=2 of the canvas render-tick rate (a 60fps NDI source
			 * into a 30fps canvas). The present-OLDEST path CRAWLS here:
			 * one canvas interval lands a HAIR under N source intervals
			 * (30fps 33_333_333 ns vs 2*60fps 33_333_334 ns), so the
			 * boundary matures only ONE frame per tick while N arrive —
			 * content plays at ~1/N speed and the queue grows until the
			 * backlog storm above catches up with a JUMP (the live-event
			 * "like 15fps" judder, #726). Instead mature every frame up
			 * to the boundary PLUS a half-interval slack (so the frame
			 * ~one canvas interval ahead — the hair-past-boundary one —
			 * is included, the #136 boundary-churn tolerance), release
			 * the NEWEST and retire the older matured one(s) into
			 * genlock_dropped_due (the erase loop below). The boundary
			 * re-anchors to the presented stamp, advancing ONE canvas
			 * interval (= N source frames) per tick: a uniform
			 * every-Nth-frame cadence tracking real time, slew-immune
			 * (keys on the boundary, not the wall). Mirror of
			 * src/probe/genlock.rs ReleaseCadence::tick N>=2 path. */
			const uint64_t mature_deadline =
				source->genlock_locked_next_boundary_ns +
				interval / 2;
			size_t matured_n = 0;
			while (matured_n < source->async_frames.num &&
			       source->async_frames.array[matured_n]->timestamp <=
				       mature_deadline)
				matured_n++;
			release = matured_n > 0 ? matured_n : 1;
			/* #1003: a STEADY present -- the conveyor. */
			anchor_update = true;
			/* #1049: the N>=2 conveyor has NO drain path and locks a
			 * persistent phase -- eligible for the bounded phase shed. */
			converge_eligible = true;
		} else {
			/* present the OLDEST matured frame, exactly one in steady
			 * state at any arrival skew. Presenting oldest (v1 presented
			 * the newest matured and dropped the rest) is what makes a
			 * transient 2-frame maturation LOSSLESS: the extra frame
			 * drains on the next tick (depth-bounded by the backlog
			 * guard above). The boundary re-anchors to the presented
			 * stamp below so small stamp jitter cannot accumulate.
			 * N==1 is byte-identical to pre-#726. */
			release = 1;
			/* #859 follow-up: this is the ONE path the ticket's
			 * evidence identified as holding depth CONSTANT forever
			 * — eligible for the bounded settle-back drain below. */
			drain_eligible = true;
			/* #1003: a STEADY present -- the conveyor. */
			anchor_update = true;
			/* #1049: a residual N==1 phase (below the #859 depth
			 * drain's 2-frame hysteresis) is eligible for the shed too. */
			converge_eligible = true;
		}
	} else if (present_ts >= source->async_frames.array[0]->timestamp) {
		/* GAP RESYNC: nothing matured, but the oldest queued frame is
		 * BEYOND the boundary and has aged past the reserve —
		 * upstream skipped stamps (sender restart, upstream loss).
		 * Present it and re-anchor the boundary to the real stream;
		 * not a drop of ours (nothing is discarded), not a relock (no
		 * catch-up jump). */
		/* #726 STICKY-N: a GAP RESYNC means upstream skipped stamps
		 * (sender restart / upstream loss) -- the source timeline (and
		 * possibly its rate) changed; clear the latch so the post-gap
		 * stream re-confirms its multiple. */
		source->genlock_last_known_n = 0;
		/* #1003: a GAP RESYNC RE-DERIVES the phase anchor from the
		 * frame it puts on air. Upstream skipped stamps, so the
		 * pre-gap age describes a timeline that no longer exists --
		 * this present is both the "update on GAP" and the "do not
		 * carry the pre-seam value forward" rule, in one assignment
		 * (the same seam that clears STICKY-N above). */
		anchor_update = true;
		release = 1;
	} else {
		/* HOLD: the boundary's frame has not arrived. LATE only if
		 * the wall says it should have been here (the boundary aged
		 * past the reserve — upstream late/lost); EARLY otherwise
		 * (benign, the #148 source-early hold). */
		if (present_ts >= source->genlock_locked_next_boundary_ns)
			source->genlock_late_holds++;
		else
			source->genlock_holds++;
		genlock_audit_log(source, now_ns);
		return false;
	}
	/* Present the LAST frame of the released prefix — #1003: the frame
	 * nearest the tracked phase anchor at ACQUIRE / backlog re-lock
	 * (release = selected index + 1, so this idiom is unchanged; it was
	 * the newest DUE frame before #1003), the queue head on the STEADY /
	 * GAP paths (release = 1, nothing erased).
	 * The (release-1) stale older ones are erased via the same
	 * da_erase(.,0)+remove_async_frame() idiom — COUNTING each into
	 * genlock_dropped_due (#401: this erase used to be silent, which
	 * is how run 7020001 lost 8,511 ids with zero audit movement). */
	size_t to_drop = release - 1;
	while (to_drop-- && source->async_frames.num > 1) {
		struct obs_source_frame *dropped = source->async_frames.array[0];
		da_erase(source->async_frames, 0);
		remove_async_frame(source, dropped);
		source->genlock_dropped_due++;
	}
	/* camera-box #859 follow-up: SLEW-LIMITED SETTLE-BACK DRAIN. Only on
	 * the plain N==1 steady path (drain_eligible). Drops the CURRENT
	 * oldest (array[0] — what would otherwise be presented this tick)
	 * and presents the NEXT one instead (array[0] AFTER this erase) —
	 * the same drop-older/present-newest idiom the ACQUIRE / backlog
	 * relock / N>=2 paths above already use. This is NOT equivalent to
	 * keeping the same presented frame and dropping the one behind it:
	 * that alternative desyncs the re-anchored boundary from the real
	 * (evenly-spaced) frame timeline, so the VERY NEXT tick reads as a
	 * HOLD and the queue regains via GAP RESYNC exactly what the drain
	 * just shed — a self-cancelling no-op, confirmed by simulation
	 * before landing here (that simulation validated THIS DROP IDIOM at a
	 * correctly-computed target; camera-box #998 found a SEPARATE bug in
	 * genlock_should_drain_one()'s target itself — round-to-nearest instead
	 * of ceil — that reproduced this exact same dup+skip symptom via a
	 * different mechanism at frac<0.5; see that function's own comment).
	 * At most ONE extra frame leaves the queue
	 * this tick, bounded to at most once per
	 * GENLOCK_DRAIN_MIN_TICK_INTERVAL ticks by genlock_should_drain_one(),
	 * so it can never reproduce the every-tick backlog-relock burst.
	 * Mirror: src/genlock_backlog.rs should_drain_one (Tier-0 tested) /
	 * src/probe/genlock.rs ReleaseCadence::should_drain_one. */
	if (drain_eligible) {
		if (genlock_should_drain_one(source, reserve_ms, interval) &&
		    source->async_frames.num > 1) {
			struct obs_source_frame *drained =
				source->async_frames.array[0];
			da_erase(source->async_frames, 0);
			remove_async_frame(source, drained);
			source->genlock_dropped_due++;
			source->genlock_ticks_since_drain = 0;
		} else {
			source->genlock_ticks_since_drain++;
		}
	}
	/* camera-box #1049: SLEW-LIMITED PHASE CONVERGENCE. On the two STEADY presents
	 * (converge_eligible) the conveyor has a persistent presentation phase (the N>=2 path has NO
	 * depth drain at all; the N==1 depth drain's 2-frame hysteresis swallows a 1-2 canvas-frame
	 * phase error). Shed one extra frame with the SAME drop-older/present-fresher idiom the #859
	 * drain uses -- drop the would-be-presented array[0] and present the next (on N>=2 that is one
	 * SOURCE interval fresher; on N==1 byte-identical to the drain shed) -- re-anchoring the
	 * boundary below to the fresher stamp, so the reduced phase STICKS. The throttle counter is
	 * SHARED with the #859 drain: after a drain reset it to 0, genlock_should_converge_phase reads
	 * ticks < GENLOCK_DRAIN_MIN_TICK_INTERVAL and returns false, so at most ONE extra frame ever
	 * leaves the queue per tick (the drain and the converge can never both fire) and the drain's
	 * own block above is left byte-identical. On the N>=2 path (!drain_eligible) this block also
	 * maintains the shared counter (the drain block did not run). Mirror of
	 * src/genlock_backlog.rs should_converge_phase / the SimConveyor1049 shed. */
	if (converge_eligible) {
		if (genlock_should_converge_phase(source, reserve_ms, interval, wall_now) &&
		    source->async_frames.num > 1) {
			struct obs_source_frame *shed =
				source->async_frames.array[0];
			da_erase(source->async_frames, 0);
			remove_async_frame(source, shed);
			source->genlock_dropped_due++;
			source->genlock_converge_sheds++; /* #1049: distinct observability */
			source->genlock_ticks_since_drain = 0;
		} else if (!drain_eligible) {
			source->genlock_ticks_since_drain++;
		}
	}
	struct obs_source_frame *next_frame = source->async_frames.array[0];
	/* camera-box #1003: remember the conveyor's own on-air age, but ONLY
	 * on a STEADY / GAP present (anchor_update). A relock reads this
	 * anchor to inherit the phase; letting a relock WRITE it would let
	 * each lock episode re-mint a phase from whatever frame it happened
	 * to select, which is precisely the defect. Mirror of
	 * src/genlock_backlog.rs phase_anchor_from_present. */
	if (anchor_update)
		source->genlock_phase_anchor_ns = genlock_phase_anchor_from_present(
			wall_now, next_frame->timestamp);
	/* The lock advances exactly one interval past the presented stamp
	 * (ACQUIRE, RE-LOCK and STEADY alike) — the next boundary the
	 * cadence will mature. */
	source->genlock_locked_next_boundary_ns =
		next_frame->timestamp + interval;
	source->genlock_frames_consumed++;
	source->last_frame_ts = next_frame->timestamp;
	genlock_audit_log(source, now_ns);
	return true;
}

static bool ready_async_frame(obs_source_t *source, uint64_t sys_time)
{
	struct obs_source_frame *next_frame = source->async_frames.array[0];
	struct obs_source_frame *frame = NULL;
	uint64_t sys_offset = sys_time - source->last_sys_timestamp;
	uint64_t frame_time = next_frame->timestamp;
	uint64_t frame_offset = 0;

	if (source->genlock_fifo) {
		/* camera-box #42/#70/#102: FIFO genlock with a preload VIDEO DELAY.
		 * #102: BUILD to the preload delay depth once at startup, then consume
		 * a distinct frame on EVERY tick a frame is queued (repeat only on a
		 * TRUE empty) — so NDI arrival jitter below the reserve no longer loses
		 * a distinct frame (the old #70 `depth>preload` hard-hold gate did, up
		 * to ~34% at a deep preload). The audit counters record
		 * received/consumed/underruns(now build-fill + true-empty only)/overruns
		 * and the queue high-water mark, logged periodically as the
		 * before/after evidence that distinct-frame loss drops to ~zero. */
		/* camera-box #97: read the PER-SOURCE preload (video-delay depth),
		 * not the global env default. The whole render path runs under
		 * async_mutex (get_closest_frame is only called there), so this read
		 * is serialised with obs_source_set_genlock_preload() — no unlocked
		 * mutation of a field the A/V thread reads (the #93 UAF lesson).
		 * source->genlock_filled (the #102 startup-fill latch) is read+written
		 * under the same async_mutex. */
		const uint32_t preload = source->genlock_preload;
		const uint64_t now_ns = sys_time; /* monotonic render-tick stamp */

		if (source->async_frames.num > source->genlock_peak_depth)
			source->genlock_peak_depth = (uint32_t)source->async_frames.num;

		/* camera-box #136: timestamp-aligned release (multi-source IN-SYNC).
		 * When enabled AND the head frame carries a real wall-clock capture ts (a
		 * camera-box genlock input in Source-Timecode mode), present the frame
		 * captured at the shared deadline present_ts = wall_now - preload*interval:
		 * drop the stale past-due frames, hold (repeat) when none are due. Every
		 * genlock source shares the strih wall-clock tick + the cam DanteSync capture
		 * timeline, so the presented timestamp is identical across sources => in-sync.
		 * Falls through to the count gate below for non-wallclock sources (CG/preview),
		 * when the interval is unknown, or when OBS_GENLOCK_TS_ALIGN is off. Mirrors
		 * src/probe/genlock.rs genlock_release(). */
		/* camera-box #245: a PER-SOURCE latency override implies ts-align ON for THIS
		 * source even when the global gate is off (mirror of the #235 global
		 * implication, extended per source) — otherwise an override set on a box whose
		 * global latency is 0 would be inert (the ms-reserve deadline below only runs on
		 * this ts-align path). Read under async_mutex (held across the whole render
		 * path). */
		const bool ts_align_on = genlock_ts_align_enabled() || source->genlock_latency_ms > 0;
		if (ts_align_on && genlock_is_wallclock_ts(next_frame->timestamp)) {
			const uint64_t interval = genlock_frame_interval_ns();
			if (interval != 0) {
				/* camera-box #184: a configured sub-frame ms reserve replaces the
				 * whole-frame preload deadline (held latency ≈ reserve_ms, not a full
				 * 33ms frame); reserve_ms=0 keeps the #136 frame-based path verbatim. */
				/* camera-box #245: the held latency is PER-SOURCE — the source's
				 * own genlock_latency_ms override wins when set (>0), else the global
				 * default genlock_reserve_ms(). So each NDI source holds a DIFFERENT
				 * latency from the OBS source UI (the #235 per-source regression fix).
				 * Mirror of src/probe/genlock.rs effective_latency_ms. Read under
				 * async_mutex (held), serialised with obs_source_set_genlock_latency_ms. */
				const uint32_t reserve_ms = source->genlock_latency_ms > 0
								    ? source->genlock_latency_ms
								    : genlock_reserve_ms();
				/* camera-box #269 finding [3]: read the precise wall clock ONCE per
				 * tick and reuse it for BOTH the deadline and the head-skew sample.
				 * On Windows genlock_wall_now_ns() is GetSystemTimePreciseAsFileTime
				 * (non-trivial); the old code called it a SECOND time for the skew,
				 * doubling the per-frame precise-clock read on this hot path. The
				 * single read also makes the skew measured at the SAME instant as the
				 * deadline. */
				const uint64_t wall_now = genlock_wall_now_ns();
				uint64_t present_ts =
					reserve_ms > 0
						? genlock_present_ts_reserve(wall_now, reserve_ms)
						: genlock_present_ts(wall_now, preload, interval);
				/* camera-box #940 piece 3: PHASE-PIN the deadline to the wall-
				 * clock frame grid -- ONLY the ms-granular reserve_ms>0 path
				 * (the effectively-unused-on-this-build frame-count preload
				 * path below is untouched, byte-identical). due_hysteresis_ns
				 * absorbs ordinary sub-ms render-tick jitter on the floor
				 * division (0 on the untouched path). Mirror:
				 * src/genlock_backlog.rs phase_pinned_deadline /
				 * PHASE_PIN_HYSTERESIS_NS. */
				const uint64_t due_hysteresis_ns =
					reserve_ms > 0 ? GENLOCK_PHASE_PIN_HYSTERESIS_NS : 0;
				if (reserve_ms > 0)
					present_ts = genlock_phase_pin_deadline(present_ts, interval);
				/* due = prefix of queued frames at/before the deadline (a single
				 * NDI source delivers in monotonic capture order). */
				size_t due = 0;
				while (due < source->async_frames.num &&
				       source->async_frames.array[due]->timestamp <=
					       present_ts + due_hysteresis_ns)
					due++;
				/* camera-box #148: SAMPLE the ts-align decision inputs for the
				 * periodic 5s audit line (present_ts / due / head-frame-vs-wall
				 * skew). Cheap per-tick field writes; the blog() in
				 * genlock_audit_log stays 5s-gated. #269 [3]: the skew reuses the
				 * single `wall_now` read above (same instant as the deadline, no
				 * second precise-clock read). ready_async_frame is only entered with
				 * num>=1, so array[0] is valid. */
				source->genlock_last_present_ts = present_ts;
				source->genlock_last_due = (uint32_t)due;
				source->genlock_last_head_skew_ns =
					(int64_t)(wall_now -
						  source->async_frames.array[0]->timestamp);
				/* count-gate machinery (empty_run re-arm / fill latch) is unused on
				 * this path — ts-align self-heals after a transient via the real ts. */
				source->genlock_empty_run = 0;
				source->genlock_filled = true;
				/* camera-box #147: backward wall-clock step (NTP/PTP sawtooth)
				 * recovery. present_ts = wall_now - reserve; on a BACKWARD clock
				 * step wall_now (and present_ts) regress below every already-queued
				 * (pre-step, higher) frame timestamp, so due==0 every tick and an
				 * unguarded wall-deadline release would HOLD (repeat the last
				 * frame) INDEFINITELY — the live program feed FREEZES until the
				 * clock climbs back. Evaluated BEFORE the #401 cadence below (the
				 * regressed stamp timeline must re-anchor, not be "matured" against
				 * a pre-step boundary). Mirror of src/probe/genlock.rs
				 * genlock_release_guarded and the cam-EMIT guard #131 (a boundary
				 * impossibly far in the future re-anchors). */
				if (due == 0) {
					/* camera-box #269 [3]: detect the backward step on the NEWEST
					 * (max-ts) queued frame, NOT array[0] (the oldest). The newest
					 * captured frame is ~wall_now in normal operation; one stamped MORE
					 * THAN one interval AHEAD of the real wall clock is impossible for a
					 * live capture — the shared DanteSync clock stepped backward. Testing
					 * the OLDEST frame made the trigger depend on each source's queue
					 * depth (a step smaller than a deep source's buffer left its oldest
					 * frame NOT-future, so that source stayed frozen while a shallow one
					 * jumped to live — the cross-source DESYNC genlock prevents). The MAX
					 * is depth-independent, so all genlock sources re-anchor UNIFORMLY
					 * once the step exceeds one interval. async_frames is in ARRIVAL order
					 * (non-monotonic across the backward-step seam), so scan for the true
					 * max rather than read a positional head. */
					uint64_t max_ts = source->async_frames.array[0]->timestamp;
					for (size_t i = 1; i < source->async_frames.num; i++) {
						const uint64_t ts = source->async_frames.array[i]->timestamp;
						if (ts > max_ts)
							max_ts = ts;
					}
					/* camera-box #1009: RE-QUALIFIED trigger. The margin is
					 * max(3 intervals, 250 ms) — far above the sender's deliberate
					 * ceil-to-boundary future bias — AND the condition must SUSTAIN
					 * GENLOCK_BACKWARD_STEP_SUSTAIN_TICKS consecutive due==0 ticks
					 * before the FIRST re-anchor. The old one-interval single-tick
					 * trigger fired on 0.3-45 ms of sender-ahead stamp skew (the
					 * 2026-08-07 overnight -900 ms collapse) and its per-tick
					 * re-anchor bypassed the configured hold permanently. While
					 * qualifying (pending) the tick falls through to the #401
					 * cadence below, which presents/holds normally off its locked
					 * stamp-relative boundary — nothing freezes. Mirror of
					 * src/genlock_backlog.rs BackwardStepGuard::tick_due0
					 * (Tier-0 unit-tested). */
					const uint64_t backward_margin = genlock_backward_step_margin_ns(interval);
					const bool head_future = max_ts > wall_now + backward_margin;
					if (head_future && !source->genlock_in_backward_step)
						source->genlock_backward_pending_ticks++;
					if (head_future &&
					    (source->genlock_in_backward_step ||
					     source->genlock_backward_pending_ticks >=
						     GENLOCK_BACKWARD_STEP_SUSTAIN_TICKS)) {
						/* RE-ANCHOR. #269 [0]: present the OLDEST queued frame and drop
						 * NOTHING extra. The pre-step frames are real captures;
						 * get_closest_frame erases the presented head each tick, so the buffer
						 * drains one frame per tick (the genlock consume rate) and the
						 * configured latency-depth buffer is PRESERVED — a smooth few-frame
						 * blip at ANY latency. The old "present newest, drop num-1" drained the
						 * queue to empty, so for a deep per-source latency override the feed
						 * then FROZE for ~latency_ms while the buffer refilled.
						 * #269 [2]: count + LOG_WARNING ONCE per EVENT (on the transition INTO
						 * the re-anchor state), not every recovery tick — the old per-tick
						 * increment counted one step as N and logged at frame rate, breaking
						 * the 5 s audit-log gating. #1009: a PERSISTENT regime additionally
						 * re-warns on a bounded cadence (below) — the entry-only WARN let the
						 * overnight collapse run silent for 3+ hours. */
						if (!source->genlock_in_backward_step) {
							source->genlock_backward_steps++;
							source->genlock_backward_regime_start_ns = now_ns;
							source->genlock_backward_last_warn_ns = 0;
							blog(LOG_WARNING,
							     "genlock-fifo backward clock step '%s': max queued ts %llu > "
							     "wall_now+margin (%llu, margin %llu ms, sustained %u ticks) — "
							     "re-anchoring (present oldest of %zu queued, preserve buffer) "
							     "instead of freezing the program feed (#147/#1009)",
							     source->context.name ? source->context.name : "?",
							     (unsigned long long)max_ts,
							     (unsigned long long)(wall_now + backward_margin),
							     (unsigned long long)(backward_margin / 1000000),
							     (unsigned)GENLOCK_BACKWARD_STEP_SUSTAIN_TICKS,
							     source->async_frames.num);
						} else if (now_ns - source->genlock_backward_regime_start_ns >
								   GENLOCK_BACKWARD_REGIME_WARN_AFTER_NS &&
							   now_ns - source->genlock_backward_last_warn_ns >=
								   GENLOCK_BACKWARD_REGIME_WARN_INTERVAL_NS) {
							/* #1009: bounded-cadence WARN — the regime is abnormal-old
							 * (>2 s) and still re-anchoring every tick; last_warn_ns=0 at
							 * entry so the FIRST cadence warn lands the moment the age
							 * threshold is crossed, then at most one per 5 s. */
							source->genlock_backward_last_warn_ns = now_ns;
							blog(LOG_WARNING,
							     "genlock-fifo backward-step regime persists '%s': %.1f s, "
							     "reanchor_ticks=%llu depth=%zu — the configured hold is "
							     "bypassed while the condition lasts (#1009)",
							     source->context.name ? source->context.name : "?",
							     (double)(now_ns - source->genlock_backward_regime_start_ns) /
								     1e9,
							     (unsigned long long)source->genlock_backward_regime_ticks,
							     source->async_frames.num);
						}
						source->genlock_in_backward_step = true;
						/* #1009 review hardening: while the regime is active,
						 * pending_ticks doubles as the consecutive-CLEAR counter for
						 * the qualified EXIT — an over-margin tick (this re-anchor)
						 * breaks any clear run. Also zeroes the entry run at entry.
						 * Mirror of BackwardStepGuard (pending reset in the Reanchor
						 * arms). */
						source->genlock_backward_pending_ticks = 0;
						/* #1009: count every re-anchored TICK (backward_steps counts
						 * EVENTS) — the audit/gate counter a healthy run keeps at 0.
						 * Incremented AFTER the cadence-warn blog above, so a warn
						 * prints the PRE-increment value (the Rust mirror increments
						 * before computing warn — cosmetic only, the warn there
						 * carries no count; noted so a parity audit doesn't chase it). */
						source->genlock_backward_regime_ticks++;
						next_frame = source->async_frames.array[0];
						/* camera-box #401: the re-anchor presented a frame OUTSIDE the
						 * cadence — re-lock the boundary to it (presented + interval) so
						 * the cadence's STEADY path continues the pre-step stamp grid
						 * coherently while the regime lasts (draining the seam one
						 * matured frame per tick, no hold gap), instead of keying on a
						 * boundary the step invalidated. (#1009: on regime EXIT the
						 * boundary is ZEROED instead — see genlock_backward_regime_end.) */
						source->genlock_locked_next_boundary_ns =
							next_frame->timestamp + interval;
						/* #726 STICKY-N: a backward clock-step re-anchor is a
						 * source-timeline discontinuity -- clear the confirmed-N
						 * latch so the post-step stream re-confirms its multiple. */
						source->genlock_last_known_n = 0;
						source->genlock_frames_consumed++;
						source->last_frame_ts = next_frame->timestamp;
						genlock_audit_log(source, now_ns);
						return true;
					}
					if (!head_future) {
						if (source->genlock_in_backward_step) {
							/* #1009 review hardening: the EXIT is qualified like the
							 * entry — the condition must stay clear for
							 * GENLOCK_BACKWARD_STEP_SUSTAIN_TICKS CONSECUTIVE due==0
							 * ticks before the regime ends (pending_ticks doubles as
							 * the clear-run counter while in the regime). A condition
							 * FLAPPING around the margin (a slewing clock at the
							 * crossing: max_ts advances in interval quanta, so
							 * head_future sawtooths) must NOT exit-and-re-enter per
							 * flap — every exit costs a bounded ~latency_ms
							 * re-ACQUIRE hold. While the clear run qualifies, fall
							 * through to the #401 cadence (its live-edge boundary
							 * keeps presenting). Mirror of
							 * BackwardStepGuard::tick_due0's clear path. */
							source->genlock_backward_pending_ticks++;
							if (source->genlock_backward_pending_ticks >=
							    GENLOCK_BACKWARD_STEP_SUSTAIN_TICKS) {
								source->genlock_backward_pending_ticks = 0;
								genlock_backward_regime_end(source, reserve_ms);
							}
						} else {
							/* No regime: a not-yet-sustained entry run is a transient
							 * — reset. due==0 is no longer a hold verdict by itself:
							 * the #401 cadence below may still PRESENT a frame the
							 * LOCKED boundary has matured even though the slewed wall
							 * deadline says not-due (#269 [2]). */
							source->genlock_backward_pending_ticks = 0;
						}
					}
					/* head_future while still PENDING (not yet sustained): fall
					 * through — the cadence presents/holds off its locked boundary;
					 * the pending counter rides until the condition either sustains
					 * (fires above) or clears (reset above). Never a single-tick
					 * hair-trigger (#1009). */
				} else {
					/* #269 [2]/#1009: a normal due tick ends any backward-step
					 * regime — same SELF-HEAL contract as the due==0 clear path, but
					 * IMMEDIATE (no sustain): frames aged past the reserve against
					 * the wall deadline is structural proof the receiver clock
					 * genuinely caught up (a marginal flap at the live edge only
					 * ever produces young frames). Mirror of
					 * BackwardStepGuard::tick_due_positive. */
					source->genlock_backward_pending_ticks = 0;
					if (source->genlock_in_backward_step)
						genlock_backward_regime_end(source, reserve_ms);
				}

				return genlock_release_tick(source, wall_now, present_ts, due,
							    interval, reserve_ms, now_ns);
			}
		}

		/* camera-box #269 finding [5]: the ts-align path did NOT present/hold this tick
		 * (interval==0, a non-wallclock head ts, or ts-align off) — reset the ts-align
		 * decision sample to the sentinel so the 5s audit prints 0, never a STALE sample
		 * left over from an earlier ts-align tick. */
		genlock_clear_ts_sample(source);

		/* camera-box #126: frames have RESUMED (this branch is reached only with
		 * num>=1). If a SUSTAINED true-empty run preceded this resume while the source
		 * was in steady state, that is a reconnect (e.g. an upstream strih OBS restart):
		 * DistroAV KEEP_CONTENT blocked the NULL-emit reset and an underrun never fired
		 * the overrun force-drain, so genlock_filled is still TRUE and the #102 steady
		 * gate would consume 1/tick WITHOUT rebuilding the preload reserve — collapsing
		 * the deliberate video delay to ~0 (A/V drift) until a manual nudge. Re-arm the
		 * build latch (genlock_filled=false) so the EXISTING #102 build path + #116
		 * build-latch drain rebuild the reserve to exactly preload+1 — no new draining
		 * logic. The >= GENLOCK_REARM_EMPTY_TICKS guard keeps normal arrival jitter (a
		 * brief sub-threshold dip, esp. at the shallow cam preload=1) from spuriously
		 * re-arming (a spurious re-arm would force a ~preload-frame rebuild hold on every
		 * blip). Decided BEFORE genlock_decide so the build branch engages this tick;
		 * read/written under async_mutex (same as genlock_filled). */
		if (source->genlock_filled &&
		    source->genlock_empty_run >= GENLOCK_REARM_EMPTY_TICKS) {
			blog(LOG_INFO,
			     "genlock-fifo reconnect re-arm '%s': %u consecutive empty tick(s) "
			     ">= %u (≈%llu ms @ tick) — re-arming build latch to rebuild the "
			     "preload reserve (preload=%u, target=%u) (#126)",
			     source->context.name ? source->context.name : "?",
			     source->genlock_empty_run, (unsigned)GENLOCK_REARM_EMPTY_TICKS,
			     /* genlock_preload_ms() is a generic frames->ms (ticks * period)
			      * converter; here it turns the empty-RUN tick count into the
			      * outage duration in ms (not a preload value) — the "@ tick"
			      * label disambiguates. */
			     (unsigned long long)genlock_preload_ms(source->genlock_empty_run),
			     preload, preload + 1);
			source->genlock_filled = false; /* re-enter the #102 BUILD phase */
		}
		/* The empty run has ended — a frame is queued again (this branch is reached only
		 * with num>=1). Reset UNCONDITIONALLY on entry (before genlock_decide), so a
		 * flickering empty/non-empty queue (ordinary jitter) can never creep up to the
		 * threshold; only a genuine sustained disconnect can. The counter is only ever
		 * nonzero in steady state, where num>=1 always consumes (#102), so resetting "on
		 * the next queued tick" and "on the next consume" coincide — this mirrors
		 * genlock_empty_run_next(_, true). NB this runs AFTER the re-arm guard above reads
		 * the counter (the read must precede the zeroing). */
		source->genlock_empty_run = 0;

		const bool was_building = !source->genlock_filled;
		const struct genlock_decision gd =
			genlock_decide(source->async_frames.num, preload, source->genlock_filled);
		source->genlock_filled = gd.filled; /* latch the build->steady transition */

		if (!gd.consume) {
			/* hold: still BUILDING the preload delay (depth>0 but not yet past
			 * preload, so the latch is unset) — repeat the last frame this tick
			 * while the reserve fills. ready_async_frame is only reached with
			 * num>=1 (get_closest_frame returns earlier on a TRUE empty, counting
			 * that underrun at its num==0 guard), so once filled this branch is not
			 * taken: a steady-state queued frame is ALWAYS consumed (the #102 fix).
			 * camera-box #269 finding [4]: this count-gate build-fill is a BENIGN
			 * hold (its own comment says "still BUILDING"; it also recurs on every
			 * #126 reconnect re-arm) — count it as genlock_holds, NOT genlock_underruns.
			 * underruns must mean TRUE-EMPTY starvation only (the num==0 guard in
			 * get_closest_frame); folding the build-fill in inflated the underrun
			 * count. Mirror of src/probe/genlock.rs classify_count_gate_tick. */
			source->genlock_holds++;
			genlock_audit_log(source, now_ns);
			return false;
		}

		/* camera-box #116: the build latch just FIRED (was_building && now consuming).
		 * Trim the startup burst down to the target depth (preload+1) by ERASING the
		 * excess OLDEST frames, so every input — and every restart — settles at the
		 * IDENTICAL deterministic depth (equal per-camera latency) and a preload change
		 * takes effect immediately in BOTH directions (a decrease re-arms the latch
		 * (obs_source_set_genlock_preload) and this drains the deep queue straight to
		 * the new lower target on the next build latch). This fires ONLY at the build
		 * latch — NEVER in steady state — so the #102 consume-when-queued 0-loss gate is
		 * untouched. Each dropped frame is freed once via the same da_erase(.,0) +
		 * remove_async_frame() idiom the async_unbuffered drain uses (no leak / no
		 * double-free); the kept frames slide forward and next_frame is re-read. */
		if (was_building) {
			size_t to_drain = genlock_build_drain(source->async_frames.num, preload);
			if (to_drain) {
				blog(LOG_INFO,
				     "genlock-fifo build drain '%s': depth %zu -> target %u (preload %u), "
				     "erasing %zu oldest frame(s) (#116)",
				     source->context.name ? source->context.name : "?",
				     source->async_frames.num, preload + 1, preload, to_drain);
				while (to_drain-- && source->async_frames.num > 1) {
					struct obs_source_frame *dropped = source->async_frames.array[0];
					da_erase(source->async_frames, 0);
					remove_async_frame(source, dropped);
				}
				next_frame = source->async_frames.array[0];
			}
		}

		source->genlock_frames_consumed++;
		source->last_frame_ts = next_frame->timestamp;
		genlock_audit_log(source, now_ns);
		return true;
	}

	if (source->async_unbuffered) {
		while (source->async_frames.num > 1) {
			da_erase(source->async_frames, 0);
			remove_async_frame(source, next_frame);
			next_frame = source->async_frames.array[0];
		}

		source->last_frame_ts = next_frame->timestamp;
		return true;
	}

#if DEBUG_ASYNC_FRAMES
	blog(LOG_DEBUG,
	     "source->last_frame_ts: %llu, frame_time: %llu, "
	     "sys_offset: %llu, frame_offset: %llu, "
	     "number of frames: %lu",
	     source->last_frame_ts, frame_time, sys_offset, frame_time - source->last_frame_ts,
	     (unsigned long)source->async_frames.num);
#endif

	/* account for timestamp invalidation */
	if (frame_out_of_bounds(source, frame_time)) {
#if DEBUG_ASYNC_FRAMES
		blog(LOG_DEBUG, "timing jump");
#endif
		source->last_frame_ts = next_frame->timestamp;
		return true;
	} else {
		frame_offset = frame_time - source->last_frame_ts;
		source->last_frame_ts += sys_offset;
	}

	while (source->last_frame_ts > next_frame->timestamp) {

		/* this tries to reduce the needless frame duplication, also
		 * helps smooth out async rendering to frame boundaries.  In
		 * other words, tries to keep the framerate as smooth as
		 * possible */
		if (frame && (source->last_frame_ts - next_frame->timestamp) < 2000000)
			break;

		if (frame)
			da_erase(source->async_frames, 0);

#if DEBUG_ASYNC_FRAMES
		blog(LOG_DEBUG,
		     "new frame, "
		     "source->last_frame_ts: %llu, "
		     "next_frame->timestamp: %llu",
		     source->last_frame_ts, next_frame->timestamp);
#endif

		remove_async_frame(source, frame);

		if (source->async_frames.num == 1)
			return true;

		frame = next_frame;
		next_frame = source->async_frames.array[1];

		/* more timestamp checking and compensating */
		if ((next_frame->timestamp - frame_time) > MAX_TS_VAR) {
#if DEBUG_ASYNC_FRAMES
			blog(LOG_DEBUG, "timing jump");
#endif
			source->last_frame_ts = next_frame->timestamp - frame_offset;
		}

		frame_time = next_frame->timestamp;
		frame_offset = frame_time - source->last_frame_ts;
	}

#if DEBUG_ASYNC_FRAMES
	if (!frame)
		blog(LOG_DEBUG, "no frame!");
#endif

	return frame != NULL;
}

static inline struct obs_source_frame *get_closest_frame(obs_source_t *source, uint64_t sys_time)
{
	if (!source->async_frames.num) {
		/* camera-box #70: an empty FIFO at a render tick is the worst underrun
		 * (the compositor repeats the last frame). ready_async_frame is never
		 * reached when num==0, so count it here. Only once the FIFO has started
		 * (last_frame_ts set): a not-yet-active source isn't an underrun, it
		 * just hasn't received its first frame. NB an overrun force-drain
		 * (cache_video) resets last_frame_ts to 0, so the empty ticks right
		 * after a drain are re-bootstrap, not counted as underruns - the
		 * overrun counter already records that episode. */
		if (source->genlock_fifo && source->last_frame_ts) {
			source->genlock_underruns++;
			/* camera-box #126: count CONSECUTIVE true-empty ticks in steady state.
			 * last_frame_ts!=0 here means at least one consume already happened, so
			 * genlock_filled is latched true — this is a steady-state underrun, the
			 * reconnect signature (NOT a build-phase or post-overrun-drain empty: a
			 * drain resets last_frame_ts=0, so those ticks don't reach this counter).
			 * When frames resume, ready_async_frame compares this run against
			 * GENLOCK_REARM_EMPTY_TICKS to decide whether to re-arm the build latch.
			 * Saturating so a very long outage can't wrap the counter. */
			if (source->genlock_empty_run != UINT32_MAX)
				source->genlock_empty_run++;
			/* camera-box #269 finding [5]: a true-empty tick has no ts-align
			 * sample — reset to the sentinel so the audit prints 0, not a stale
			 * present/due/skew from an earlier ts-align tick. */
			genlock_clear_ts_sample(source);
			genlock_audit_log(source, sys_time);
		}
		return NULL;
	}

	/* camera-box #102: a genlock source must ALWAYS route through ready_async_frame
	 * so genlock_decide governs even the bootstrap/first frame. The stock
	 * `!last_frame_ts` short-circuit (kept for non-genlock sources, which need it to
	 * emit their first frame) would otherwise bypass the build phase after every
	 * overrun force-drain (cache_video resets last_frame_ts=0 AND genlock_filled=false)
	 * and on the first frame after a source resume — emitting one undelayed distinct
	 * frame (a ~preload-frame phase jump) before the delay line rebuilds. Excluding
	 * genlock from the bypass makes the cache_video/resume rebuild actually engage; the
	 * genlock branch seeds last_frame_ts itself on its first consume. */
	const bool bootstrap_bypass = !source->last_frame_ts && !source->genlock_fifo;
	if (bootstrap_bypass || ready_async_frame(source, sys_time)) {
		struct obs_source_frame *frame = source->async_frames.array[0];
		da_erase(source->async_frames, 0);

		if (!source->last_frame_ts)
			source->last_frame_ts = frame->timestamp;

		return frame;
	}

	return NULL;
}

/*
 * Ensures that cached frames are displayed on time.  If multiple frames
 * were cached between renders, then releases the unnecessary frames and uses
 * the frame with the closest timing to ensure sync.  Also ensures that timing
 * with audio is synchronized.
 */
struct obs_source_frame *obs_source_get_frame(obs_source_t *source)
{
	struct obs_source_frame *frame = NULL;

	if (!obs_source_valid(source, "obs_source_get_frame"))
		return NULL;

	pthread_mutex_lock(&source->async_mutex);

	frame = source->cur_async_frame;
	source->cur_async_frame = NULL;

	if (frame) {
		os_atomic_inc_long(&frame->refs);
	}

	pthread_mutex_unlock(&source->async_mutex);

	return frame;
}

void obs_source_release_frame(obs_source_t *source, struct obs_source_frame *frame)
{
	if (!frame)
		return;

	if (!source) {
		obs_source_frame_destroy(frame);
	} else {
		pthread_mutex_lock(&source->async_mutex);

		if (os_atomic_dec_long(&frame->refs) == 0)
			obs_source_frame_destroy(frame);
		else
			remove_async_frame(source, frame);

		pthread_mutex_unlock(&source->async_mutex);
	}
}

const char *obs_source_get_name(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_get_name") ? source->context.name : NULL;
}

const char *obs_source_get_uuid(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_get_uuid") ? source->context.uuid : NULL;
}

void obs_source_set_name(obs_source_t *source, const char *name)
{
	if (!obs_source_valid(source, "obs_source_set_name"))
		return;

	if (!name || !*name || !source->context.name || strcmp(name, source->context.name) != 0) {
		if (requires_canvas(source)) {
			obs_canvas_rename_source(source, name);
		} else {
			struct calldata data;
			char *prev_name = bstrdup(source->context.name);

			if (!source->context.private) {
				obs_context_data_setname_ht(&source->context, name, &obs->data.public_sources);
			} else {
				obs_context_data_setname(&source->context, name);
			}

			calldata_init(&data);
			calldata_set_ptr(&data, "source", source);
			calldata_set_string(&data, "new_name", source->context.name);
			calldata_set_string(&data, "prev_name", prev_name);
			if (!source->context.private)
				signal_handler_signal(obs->signals, "source_rename", &data);
			signal_handler_signal(source->context.signals, "rename", &data);
			calldata_free(&data);
			bfree(prev_name);
		}
	}
}

enum obs_source_type obs_source_get_type(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_get_type") ? source->info.type : OBS_SOURCE_TYPE_INPUT;
}

const char *obs_source_get_id(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_get_id") ? source->info.id : NULL;
}

const char *obs_source_get_unversioned_id(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_get_unversioned_id") ? source->info.unversioned_id : NULL;
}

static inline void render_filter_bypass(obs_source_t *target, gs_effect_t *effect, const char *tech_name)
{
	gs_technique_t *tech = gs_effect_get_technique(effect, tech_name);
	size_t passes, i;

	passes = gs_technique_begin(tech);
	for (i = 0; i < passes; i++) {
		gs_technique_begin_pass(tech, i);
		obs_source_video_render(target);
		gs_technique_end_pass(tech);
	}
	gs_technique_end(tech);
}

static inline void render_filter_tex(gs_texture_t *tex, gs_effect_t *effect, uint32_t width, uint32_t height,
				     const char *tech_name)
{
	gs_technique_t *tech = gs_effect_get_technique(effect, tech_name);
	gs_eparam_t *image = gs_effect_get_param_by_name(effect, "image");
	size_t passes, i;

	const bool linear_srgb = gs_get_linear_srgb();

	const bool previous = gs_framebuffer_srgb_enabled();
	gs_enable_framebuffer_srgb(linear_srgb);

	if (linear_srgb)
		gs_effect_set_texture_srgb(image, tex);
	else
		gs_effect_set_texture(image, tex);

	passes = gs_technique_begin(tech);
	for (i = 0; i < passes; i++) {
		gs_technique_begin_pass(tech, i);
		gs_draw_sprite(tex, 0, width, height);
		gs_technique_end_pass(tech);
	}
	gs_technique_end(tech);

	gs_enable_framebuffer_srgb(previous);
}

static inline bool can_bypass(obs_source_t *target, obs_source_t *parent, uint32_t filter_flags, uint32_t parent_flags,
			      enum obs_allow_direct_render allow_direct, enum gs_color_space space)
{
	return (target == parent) && (allow_direct == OBS_ALLOW_DIRECT_RENDERING) &&
	       ((parent_flags & OBS_SOURCE_CUSTOM_DRAW) == 0) && ((parent_flags & OBS_SOURCE_ASYNC) == 0) &&
	       ((filter_flags & OBS_SOURCE_SRGB) == (parent_flags & OBS_SOURCE_SRGB) && space == gs_get_color_space());
}

bool obs_source_process_filter_begin(obs_source_t *filter, enum gs_color_format format,
				     enum obs_allow_direct_render allow_direct)
{
	return obs_source_process_filter_begin_with_color_space(filter, format, GS_CS_SRGB, allow_direct);
}

bool obs_source_process_filter_begin_with_color_space(obs_source_t *filter, enum gs_color_format format,
						      enum gs_color_space space,
						      enum obs_allow_direct_render allow_direct)
{
	obs_source_t *target, *parent;
	uint32_t filter_flags, parent_flags;
	int cx, cy;

	if (!obs_ptr_valid(filter, "obs_source_process_filter_begin_with_color_space"))
		return false;

	filter->filter_bypass_active = false;

	target = obs_filter_get_target(filter);
	parent = obs_filter_get_parent(filter);

	if (!target) {
		blog(LOG_INFO, "filter '%s' being processed with no target!", filter->context.name);
		return false;
	}
	if (!parent) {
		blog(LOG_INFO, "filter '%s' being processed with no parent!", filter->context.name);
		return false;
	}

	filter_flags = filter->info.output_flags;
	parent_flags = parent->info.output_flags;
	cx = get_base_width(target);
	cy = get_base_height(target);

	filter->allow_direct = allow_direct;

	/* if the parent does not use any custom effects, and this is the last
	 * filter in the chain for the parent, then render the parent directly
	 * using the filter effect instead of rendering to texture to reduce
	 * the total number of passes */
	if (can_bypass(target, parent, filter_flags, parent_flags, allow_direct, space)) {
		filter->filter_bypass_active = true;
		return true;
	}

	if (!cx || !cy) {
		obs_source_skip_video_filter(filter);
		return false;
	}

	if (filter->filter_texrender && (gs_texrender_get_format(filter->filter_texrender) != format)) {
		gs_texrender_destroy(filter->filter_texrender);
		filter->filter_texrender = NULL;
	}

	if (!filter->filter_texrender) {
		filter->filter_texrender = gs_texrender_create(format, GS_ZS_NONE);
	}

	if (gs_texrender_begin_with_color_space(filter->filter_texrender, cx, cy, space)) {
		gs_blend_state_push();
		gs_blend_function_separate(GS_BLEND_SRCALPHA, GS_BLEND_INVSRCALPHA, GS_BLEND_ONE, GS_BLEND_INVSRCALPHA);

		bool custom_draw = (parent_flags & OBS_SOURCE_CUSTOM_DRAW) != 0;
		bool async = (parent_flags & OBS_SOURCE_ASYNC) != 0;
		struct vec4 clear_color;

		vec4_zero(&clear_color);
		gs_clear(GS_CLEAR_COLOR, &clear_color, 0.0f, 0);
		gs_ortho(0.0f, (float)cx, 0.0f, (float)cy, -100.0f, 100.0f);

		if (target == parent && !custom_draw && !async)
			obs_source_default_render(target);
		else
			obs_source_video_render(target);

		gs_blend_state_pop();

		gs_texrender_end(filter->filter_texrender);
	}
	return true;
}

void obs_source_process_filter_tech_end(obs_source_t *filter, gs_effect_t *effect, uint32_t width, uint32_t height,
					const char *tech_name)
{
	obs_source_t *target, *parent;
	gs_texture_t *texture;
	uint32_t filter_flags;

	if (!filter)
		return;

	const bool filter_bypass_active = filter->filter_bypass_active;
	filter->filter_bypass_active = false;

	target = obs_filter_get_target(filter);
	parent = obs_filter_get_parent(filter);

	if (!target || !parent)
		return;

	filter_flags = filter->info.output_flags;

	const bool previous = gs_set_linear_srgb((filter_flags & OBS_SOURCE_SRGB) != 0);

	const char *tech = tech_name ? tech_name : "Draw";

	if (filter_bypass_active) {
		render_filter_bypass(target, effect, tech);
	} else {
		texture = gs_texrender_get_texture(filter->filter_texrender);
		if (texture) {
			render_filter_tex(texture, effect, width, height, tech);
		}
	}

	gs_set_linear_srgb(previous);
}

void obs_source_process_filter_end(obs_source_t *filter, gs_effect_t *effect, uint32_t width, uint32_t height)
{
	if (!obs_ptr_valid(filter, "obs_source_process_filter_end"))
		return;

	obs_source_process_filter_tech_end(filter, effect, width, height, "Draw");
}

void obs_source_skip_video_filter(obs_source_t *filter)
{
	obs_source_t *target, *parent;
	bool custom_draw, async;
	uint32_t parent_flags;

	if (!obs_ptr_valid(filter, "obs_source_skip_video_filter"))
		return;

	target = obs_filter_get_target(filter);
	parent = obs_filter_get_parent(filter);
	parent_flags = parent->info.output_flags;
	custom_draw = (parent_flags & OBS_SOURCE_CUSTOM_DRAW) != 0;
	async = (parent_flags & OBS_SOURCE_ASYNC) != 0;

	if (target == parent) {
		if (!custom_draw && !async)
			obs_source_default_render(target);
		else if (target->info.video_render)
			obs_source_main_render(target);
		else if (deinterlacing_enabled(target))
			deinterlace_render(target);
		else
			obs_source_render_async_video(target);

	} else {
		obs_source_video_render(target);
	}
}

signal_handler_t *obs_source_get_signal_handler(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_get_signal_handler") ? source->context.signals : NULL;
}

proc_handler_t *obs_source_get_proc_handler(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_get_proc_handler") ? source->context.procs : NULL;
}

void obs_source_set_volume(obs_source_t *source, float volume)
{
	if (obs_source_valid(source, "obs_source_set_volume")) {
		struct audio_action action = {.timestamp = os_gettime_ns(), .type = AUDIO_ACTION_VOL, .vol = volume};

		struct calldata data;
		uint8_t stack[128];

		calldata_init_fixed(&data, stack, sizeof(stack));
		calldata_set_ptr(&data, "source", source);
		calldata_set_float(&data, "volume", volume);

		signal_handler_signal(source->context.signals, "volume", &data);
		if (!source->context.private)
			signal_handler_signal(obs->signals, "source_volume", &data);

		volume = (float)calldata_float(&data, "volume");

		pthread_mutex_lock(&source->audio_actions_mutex);
		da_push_back(source->audio_actions, &action);
		pthread_mutex_unlock(&source->audio_actions_mutex);

		source->user_volume = volume;
	}
}

float obs_source_get_volume(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_get_volume") ? source->user_volume : 0.0f;
}

void obs_source_set_sync_offset(obs_source_t *source, int64_t offset)
{
	if (obs_source_valid(source, "obs_source_set_sync_offset")) {
		struct calldata data;
		uint8_t stack[128];

		calldata_init_fixed(&data, stack, sizeof(stack));
		calldata_set_ptr(&data, "source", source);
		calldata_set_int(&data, "offset", offset);

		signal_handler_signal(source->context.signals, "audio_sync", &data);

		source->sync_offset = calldata_int(&data, "offset");
	}
}

int64_t obs_source_get_sync_offset(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_get_sync_offset") ? source->sync_offset : 0;
}

struct source_enum_data {
	obs_source_enum_proc_t enum_callback;
	void *param;
};

static void enum_source_active_tree_callback(obs_source_t *parent, obs_source_t *child, void *param)
{
	struct source_enum_data *data = param;
	bool is_transition = child->info.type == OBS_SOURCE_TYPE_TRANSITION;

	if (is_transition)
		obs_transition_enum_sources(child, enum_source_active_tree_callback, param);
	if (child->info.enum_active_sources) {
		if (child->context.data) {
			child->info.enum_active_sources(child->context.data, enum_source_active_tree_callback, data);
		}
	}

	data->enum_callback(parent, child, data->param);
}

void obs_source_enum_active_sources(obs_source_t *source, obs_source_enum_proc_t enum_callback, void *param)
{
	bool is_transition;
	if (!data_valid(source, "obs_source_enum_active_sources"))
		return;

	is_transition = source->info.type == OBS_SOURCE_TYPE_TRANSITION;
	if (!is_transition && !source->info.enum_active_sources)
		return;

	source = obs_source_get_ref(source);
	if (!data_valid(source, "obs_source_enum_active_sources"))
		return;

	if (is_transition)
		obs_transition_enum_sources(source, enum_callback, param);
	if (source->info.enum_active_sources)
		source->info.enum_active_sources(source->context.data, enum_callback, param);

	obs_source_release(source);
}

void obs_source_enum_active_tree(obs_source_t *source, obs_source_enum_proc_t enum_callback, void *param)
{
	struct source_enum_data data = {enum_callback, param};
	bool is_transition;

	if (!data_valid(source, "obs_source_enum_active_tree"))
		return;

	is_transition = source->info.type == OBS_SOURCE_TYPE_TRANSITION;
	if (!is_transition && !source->info.enum_active_sources)
		return;

	source = obs_source_get_ref(source);
	if (!data_valid(source, "obs_source_enum_active_tree"))
		return;

	if (source->info.type == OBS_SOURCE_TYPE_TRANSITION)
		obs_transition_enum_sources(source, enum_source_active_tree_callback, &data);
	if (source->info.enum_active_sources)
		source->info.enum_active_sources(source->context.data, enum_source_active_tree_callback, &data);

	obs_source_release(source);
}

static void enum_source_full_tree_callback(obs_source_t *parent, obs_source_t *child, void *param)
{
	struct source_enum_data *data = param;
	bool is_transition = child->info.type == OBS_SOURCE_TYPE_TRANSITION;

	if (is_transition)
		obs_transition_enum_sources(child, enum_source_full_tree_callback, param);
	if (child->info.enum_all_sources) {
		if (child->context.data) {
			child->info.enum_all_sources(child->context.data, enum_source_full_tree_callback, data);
		}
	} else if (child->info.enum_active_sources) {
		if (child->context.data) {
			child->info.enum_active_sources(child->context.data, enum_source_full_tree_callback, data);
		}
	}

	data->enum_callback(parent, child, data->param);
}

void obs_source_enum_full_tree(obs_source_t *source, obs_source_enum_proc_t enum_callback, void *param)
{
	struct source_enum_data data = {enum_callback, param};
	bool is_transition;

	if (!data_valid(source, "obs_source_enum_full_tree"))
		return;

	is_transition = source->info.type == OBS_SOURCE_TYPE_TRANSITION;
	if (!is_transition && !source->info.enum_active_sources)
		return;

	source = obs_source_get_ref(source);
	if (!data_valid(source, "obs_source_enum_full_tree"))
		return;

	if (source->info.type == OBS_SOURCE_TYPE_TRANSITION)
		obs_transition_enum_sources(source, enum_source_full_tree_callback, &data);

	if (source->info.enum_all_sources) {
		source->info.enum_all_sources(source->context.data, enum_source_full_tree_callback, &data);

	} else if (source->info.enum_active_sources) {
		source->info.enum_active_sources(source->context.data, enum_source_full_tree_callback, &data);
	}

	obs_source_release(source);
}

struct descendant_info {
	bool exists;
	obs_source_t *target;
};

static void check_descendant(obs_source_t *parent, obs_source_t *child, void *param)
{
	struct descendant_info *info = param;
	if (child == info->target || parent == info->target)
		info->exists = true;
}

bool obs_source_add_active_child(obs_source_t *parent, obs_source_t *child)
{
	struct descendant_info info = {false, parent};

	if (!obs_ptr_valid(parent, "obs_source_add_active_child"))
		return false;
	if (!obs_ptr_valid(child, "obs_source_add_active_child"))
		return false;
	if (parent == child) {
		blog(LOG_WARNING, "obs_source_add_active_child: "
				  "parent == child");
		return false;
	}

	obs_source_enum_full_tree(child, check_descendant, &info);
	if (info.exists)
		return false;

	for (int i = 0; i < parent->show_refs; i++) {
		enum view_type type;
		type = (i < parent->activate_refs) ? MAIN_VIEW : AUX_VIEW;
		obs_source_activate(child, type);
	}

	return true;
}

void obs_source_remove_active_child(obs_source_t *parent, obs_source_t *child)
{
	if (!obs_ptr_valid(parent, "obs_source_remove_active_child"))
		return;
	if (!obs_ptr_valid(child, "obs_source_remove_active_child"))
		return;

	for (int i = 0; i < parent->show_refs; i++) {
		enum view_type type;
		type = (i < parent->activate_refs) ? MAIN_VIEW : AUX_VIEW;
		obs_source_deactivate(child, type);
	}
}

void obs_source_save(obs_source_t *source)
{
	if (!data_valid(source, "obs_source_save"))
		return;

	obs_source_dosignal(source, "source_save", "save");

	if (source->info.save)
		source->info.save(source->context.data, source->context.settings);
}

void obs_source_load(obs_source_t *source)
{
	if (!data_valid(source, "obs_source_load"))
		return;
	if (source->info.load)
		source->info.load(source->context.data, source->context.settings);

	obs_source_dosignal(source, "source_load", "load");
}

void obs_source_load2(obs_source_t *source)
{
	if (!data_valid(source, "obs_source_load2"))
		return;

	obs_source_load(source);

	for (size_t i = source->filters.num; i > 0; i--) {
		obs_source_t *filter = source->filters.array[i - 1];
		obs_source_load(filter);
	}
}

bool obs_source_active(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_active") ? source->activate_refs != 0 : false;
}

bool obs_source_showing(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_showing") ? source->show_refs != 0 : false;
}

static inline void signal_flags_updated(obs_source_t *source)
{
	struct calldata data;
	uint8_t stack[128];

	calldata_init_fixed(&data, stack, sizeof(stack));
	calldata_set_ptr(&data, "source", source);
	calldata_set_int(&data, "flags", source->flags);

	signal_handler_signal(source->context.signals, "update_flags", &data);
}

void obs_source_set_flags(obs_source_t *source, uint32_t flags)
{
	if (!obs_source_valid(source, "obs_source_set_flags"))
		return;

	if (flags != source->flags) {
		source->flags = flags;
		signal_flags_updated(source);
	}
}

void obs_source_set_default_flags(obs_source_t *source, uint32_t flags)
{
	if (!obs_source_valid(source, "obs_source_set_default_flags"))
		return;

	source->default_flags = flags;
}

uint32_t obs_source_get_flags(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_get_flags") ? source->flags : 0;
}

void obs_source_set_audio_mixers(obs_source_t *source, uint32_t mixers)
{
	struct calldata data;
	uint8_t stack[128];

	if (!obs_source_valid(source, "obs_source_set_audio_mixers"))
		return;
	if (!source->owns_info_id && (source->info.output_flags & OBS_SOURCE_AUDIO) == 0)
		return;

	if (source->audio_mixers == mixers)
		return;

	calldata_init_fixed(&data, stack, sizeof(stack));
	calldata_set_ptr(&data, "source", source);
	calldata_set_int(&data, "mixers", mixers);

	signal_handler_signal(source->context.signals, "audio_mixers", &data);

	mixers = (uint32_t)calldata_int(&data, "mixers");

	source->audio_mixers = mixers;
}

uint32_t obs_source_get_audio_mixers(const obs_source_t *source)
{
	if (!obs_source_valid(source, "obs_source_get_audio_mixers"))
		return 0;
	if (!source->owns_info_id && (source->info.output_flags & OBS_SOURCE_AUDIO) == 0)
		return 0;

	return source->audio_mixers;
}

void obs_source_draw_set_color_matrix(const struct matrix4 *color_matrix, const struct vec3 *color_range_min,
				      const struct vec3 *color_range_max)
{
	struct vec3 color_range_min_def;
	struct vec3 color_range_max_def;

	vec3_set(&color_range_min_def, 0.0f, 0.0f, 0.0f);
	vec3_set(&color_range_max_def, 1.0f, 1.0f, 1.0f);

	gs_effect_t *effect = gs_get_effect();
	gs_eparam_t *matrix;
	gs_eparam_t *range_min;
	gs_eparam_t *range_max;

	if (!effect) {
		blog(LOG_WARNING, "obs_source_draw_set_color_matrix: no "
				  "active effect!");
		return;
	}

	if (!obs_ptr_valid(color_matrix, "obs_source_draw_set_color_matrix"))
		return;

	if (!color_range_min)
		color_range_min = &color_range_min_def;
	if (!color_range_max)
		color_range_max = &color_range_max_def;

	matrix = gs_effect_get_param_by_name(effect, "color_matrix");
	range_min = gs_effect_get_param_by_name(effect, "color_range_min");
	range_max = gs_effect_get_param_by_name(effect, "color_range_max");

	gs_effect_set_matrix4(matrix, color_matrix);
	gs_effect_set_val(range_min, color_range_min, sizeof(float) * 3);
	gs_effect_set_val(range_max, color_range_max, sizeof(float) * 3);
}

void obs_source_draw(gs_texture_t *texture, int x, int y, uint32_t cx, uint32_t cy, bool flip)
{
	if (!obs_ptr_valid(texture, "obs_source_draw"))
		return;

	gs_effect_t *effect = gs_get_effect();
	if (!effect) {
		blog(LOG_WARNING, "obs_source_draw: no active effect!");
		return;
	}

	const bool linear_srgb = gs_get_linear_srgb();

	const bool previous = gs_framebuffer_srgb_enabled();
	gs_enable_framebuffer_srgb(linear_srgb);

	gs_eparam_t *image = gs_effect_get_param_by_name(effect, "image");
	if (linear_srgb)
		gs_effect_set_texture_srgb(image, texture);
	else
		gs_effect_set_texture(image, texture);

	const bool change_pos = (x != 0 || y != 0);
	if (change_pos) {
		gs_matrix_push();
		gs_matrix_translate3f((float)x, (float)y, 0.0f);
	}

	gs_draw_sprite(texture, flip ? GS_FLIP_V : 0, cx, cy);

	if (change_pos)
		gs_matrix_pop();

	gs_enable_framebuffer_srgb(previous);
}

void obs_source_inc_showing(obs_source_t *source)
{
	if (obs_source_valid(source, "obs_source_inc_showing"))
		obs_source_activate(source, AUX_VIEW);
}

void obs_source_inc_active(obs_source_t *source)
{
	if (obs_source_valid(source, "obs_source_inc_active"))
		obs_source_activate(source, MAIN_VIEW);
}

void obs_source_dec_showing(obs_source_t *source)
{
	if (obs_source_valid(source, "obs_source_dec_showing"))
		obs_source_deactivate(source, AUX_VIEW);
}

void obs_source_dec_active(obs_source_t *source)
{
	if (obs_source_valid(source, "obs_source_dec_active"))
		obs_source_deactivate(source, MAIN_VIEW);
}

void obs_source_enum_filters(obs_source_t *source, obs_source_enum_proc_t callback, void *param)
{
	if (!obs_source_valid(source, "obs_source_enum_filters"))
		return;
	if (!obs_ptr_valid(callback, "obs_source_enum_filters"))
		return;

	pthread_mutex_lock(&source->filter_mutex);

	for (size_t i = source->filters.num; i > 0; i--) {
		struct obs_source *filter = source->filters.array[i - 1];
		callback(source, filter, param);
	}

	pthread_mutex_unlock(&source->filter_mutex);
}

void obs_source_set_hidden(obs_source_t *source, bool hidden)
{
	source->temp_removed = hidden;
}

bool obs_source_is_hidden(obs_source_t *source)
{
	return source->temp_removed;
}

obs_source_t *obs_source_get_filter_by_name(obs_source_t *source, const char *name)
{
	obs_source_t *filter = NULL;

	if (!obs_source_valid(source, "obs_source_get_filter_by_name"))
		return NULL;
	if (!obs_ptr_valid(name, "obs_source_get_filter_by_name"))
		return NULL;

	pthread_mutex_lock(&source->filter_mutex);

	for (size_t i = 0; i < source->filters.num; i++) {
		struct obs_source *cur_filter = source->filters.array[i];
		if (strcmp(cur_filter->context.name, name) == 0) {
			filter = obs_source_get_ref(cur_filter);
			break;
		}
	}

	pthread_mutex_unlock(&source->filter_mutex);

	return filter;
}

size_t obs_source_filter_count(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_filter_count") ? source->filters.num : 0;
}

bool obs_source_enabled(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_enabled") ? source->enabled : false;
}

void obs_source_set_enabled(obs_source_t *source, bool enabled)
{
	struct calldata data;
	uint8_t stack[128];

	if (!obs_source_valid(source, "obs_source_set_enabled"))
		return;

	source->enabled = enabled;

	calldata_init_fixed(&data, stack, sizeof(stack));
	calldata_set_ptr(&data, "source", source);
	calldata_set_bool(&data, "enabled", enabled);

	signal_handler_signal(source->context.signals, "enable", &data);
}

bool obs_source_muted(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_muted") ? source->user_muted : false;
}

void obs_source_set_muted(obs_source_t *source, bool muted)
{
	struct calldata data;
	uint8_t stack[128];
	struct audio_action action = {.timestamp = os_gettime_ns(), .type = AUDIO_ACTION_MUTE, .set = muted};

	if (!obs_source_valid(source, "obs_source_set_muted"))
		return;

	source->user_muted = muted;

	calldata_init_fixed(&data, stack, sizeof(stack));
	calldata_set_ptr(&data, "source", source);
	calldata_set_bool(&data, "muted", muted);

	signal_handler_signal(source->context.signals, "mute", &data);

	pthread_mutex_lock(&source->audio_actions_mutex);
	da_push_back(source->audio_actions, &action);
	pthread_mutex_unlock(&source->audio_actions_mutex);
}

static void source_signal_push_to_changed(obs_source_t *source, const char *signal, bool enabled)
{
	struct calldata data;
	uint8_t stack[128];

	calldata_init_fixed(&data, stack, sizeof(stack));
	calldata_set_ptr(&data, "source", source);
	calldata_set_bool(&data, "enabled", enabled);

	signal_handler_signal(source->context.signals, signal, &data);
}

static void source_signal_push_to_delay(obs_source_t *source, const char *signal, uint64_t delay)
{
	struct calldata data;
	uint8_t stack[128];

	calldata_init_fixed(&data, stack, sizeof(stack));
	calldata_set_ptr(&data, "source", source);
	calldata_set_int(&data, "delay", delay);

	signal_handler_signal(source->context.signals, signal, &data);
}

bool obs_source_push_to_mute_enabled(obs_source_t *source)
{
	bool enabled;
	if (!obs_source_valid(source, "obs_source_push_to_mute_enabled"))
		return false;

	pthread_mutex_lock(&source->audio_mutex);
	enabled = source->push_to_mute_enabled;
	pthread_mutex_unlock(&source->audio_mutex);

	return enabled;
}

void obs_source_enable_push_to_mute(obs_source_t *source, bool enabled)
{
	if (!obs_source_valid(source, "obs_source_enable_push_to_mute"))
		return;

	pthread_mutex_lock(&source->audio_mutex);
	bool changed = source->push_to_mute_enabled != enabled;
	if (obs_source_get_output_flags(source) & OBS_SOURCE_AUDIO && changed)
		blog(LOG_INFO, "source '%s' %s push-to-mute", obs_source_get_name(source),
		     enabled ? "enabled" : "disabled");

	source->push_to_mute_enabled = enabled;

	if (changed)
		source_signal_push_to_changed(source, "push_to_mute_changed", enabled);
	pthread_mutex_unlock(&source->audio_mutex);
}

uint64_t obs_source_get_push_to_mute_delay(obs_source_t *source)
{
	uint64_t delay;
	if (!obs_source_valid(source, "obs_source_get_push_to_mute_delay"))
		return 0;

	pthread_mutex_lock(&source->audio_mutex);
	delay = source->push_to_mute_delay;
	pthread_mutex_unlock(&source->audio_mutex);

	return delay;
}

void obs_source_set_push_to_mute_delay(obs_source_t *source, uint64_t delay)
{
	if (!obs_source_valid(source, "obs_source_set_push_to_mute_delay"))
		return;

	pthread_mutex_lock(&source->audio_mutex);
	source->push_to_mute_delay = delay;

	source_signal_push_to_delay(source, "push_to_mute_delay", delay);
	pthread_mutex_unlock(&source->audio_mutex);
}

bool obs_source_push_to_talk_enabled(obs_source_t *source)
{
	bool enabled;
	if (!obs_source_valid(source, "obs_source_push_to_talk_enabled"))
		return false;

	pthread_mutex_lock(&source->audio_mutex);
	enabled = source->push_to_talk_enabled;
	pthread_mutex_unlock(&source->audio_mutex);

	return enabled;
}

void obs_source_enable_push_to_talk(obs_source_t *source, bool enabled)
{
	if (!obs_source_valid(source, "obs_source_enable_push_to_talk"))
		return;

	pthread_mutex_lock(&source->audio_mutex);
	bool changed = source->push_to_talk_enabled != enabled;
	if (obs_source_get_output_flags(source) & OBS_SOURCE_AUDIO && changed)
		blog(LOG_INFO, "source '%s' %s push-to-talk", obs_source_get_name(source),
		     enabled ? "enabled" : "disabled");

	source->push_to_talk_enabled = enabled;

	if (changed)
		source_signal_push_to_changed(source, "push_to_talk_changed", enabled);
	pthread_mutex_unlock(&source->audio_mutex);
}

uint64_t obs_source_get_push_to_talk_delay(obs_source_t *source)
{
	uint64_t delay;
	if (!obs_source_valid(source, "obs_source_get_push_to_talk_delay"))
		return 0;

	pthread_mutex_lock(&source->audio_mutex);
	delay = source->push_to_talk_delay;
	pthread_mutex_unlock(&source->audio_mutex);

	return delay;
}

void obs_source_set_push_to_talk_delay(obs_source_t *source, uint64_t delay)
{
	if (!obs_source_valid(source, "obs_source_set_push_to_talk_delay"))
		return;

	pthread_mutex_lock(&source->audio_mutex);
	source->push_to_talk_delay = delay;

	source_signal_push_to_delay(source, "push_to_talk_delay", delay);
	pthread_mutex_unlock(&source->audio_mutex);
}

void *obs_source_get_type_data(obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_get_type_data") ? source->info.type_data : NULL;
}

static float get_source_volume(obs_source_t *source, uint64_t os_time)
{
	if (source->push_to_mute_enabled && source->push_to_mute_pressed)
		source->push_to_mute_stop_time = os_time + source->push_to_mute_delay * 1000000;

	if (source->push_to_talk_enabled && source->push_to_talk_pressed)
		source->push_to_talk_stop_time = os_time + source->push_to_talk_delay * 1000000;

	bool push_to_mute_active = source->push_to_mute_pressed || os_time < source->push_to_mute_stop_time;
	bool push_to_talk_active = source->push_to_talk_pressed || os_time < source->push_to_talk_stop_time;

	bool muted = !source->enabled || source->muted || (source->push_to_mute_enabled && push_to_mute_active) ||
		     (source->push_to_talk_enabled && !push_to_talk_active);

	if (muted || close_float(source->volume, 0.0f, 0.0001f))
		return 0.0f;
	if (close_float(source->volume, 1.0f, 0.0001f))
		return 1.0f;

	return source->volume;
}

static inline void multiply_output_audio(obs_source_t *source, size_t mix, size_t channels, float vol)
{
	register float *out = source->audio_output_buf[mix][0];
	register float *end = out + AUDIO_OUTPUT_FRAMES * channels;

	while (out < end)
		*(out++) *= vol;
}

static inline void multiply_vol_data(obs_source_t *source, size_t mix, size_t channels, float *vol_data)
{
	for (size_t ch = 0; ch < channels; ch++) {
		register float *out = source->audio_output_buf[mix][ch];
		register float *end = out + AUDIO_OUTPUT_FRAMES;
		register float *vol = vol_data;

		while (out < end)
			*(out++) *= *(vol++);
	}
}

static inline void apply_audio_action(obs_source_t *source, const struct audio_action *action)
{
	switch (action->type) {
	case AUDIO_ACTION_VOL:
		source->volume = action->vol;
		break;
	case AUDIO_ACTION_MUTE:
		source->muted = action->set;
		break;
	case AUDIO_ACTION_PTT:
		source->push_to_talk_pressed = action->set;
		break;
	case AUDIO_ACTION_PTM:
		source->push_to_mute_pressed = action->set;
		break;
	}
}

static void apply_audio_actions(obs_source_t *source, size_t channels, size_t sample_rate)
{
	float vol_data[AUDIO_OUTPUT_FRAMES];
	float cur_vol = get_source_volume(source, source->audio_ts);
	size_t frame_num = 0;

	pthread_mutex_lock(&source->audio_actions_mutex);

	for (size_t i = 0; i < source->audio_actions.num; i++) {
		struct audio_action action = source->audio_actions.array[i];
		uint64_t timestamp = action.timestamp;
		size_t new_frame_num;

		if (timestamp < source->audio_ts)
			timestamp = source->audio_ts;

		new_frame_num = conv_time_to_frames(sample_rate, timestamp - source->audio_ts);

		if (new_frame_num >= AUDIO_OUTPUT_FRAMES)
			break;

		da_erase(source->audio_actions, i--);

		apply_audio_action(source, &action);

		if (new_frame_num > frame_num) {
			for (; frame_num < new_frame_num; frame_num++)
				vol_data[frame_num] = cur_vol;
		}

		cur_vol = get_source_volume(source, timestamp);
	}

	for (; frame_num < AUDIO_OUTPUT_FRAMES; frame_num++)
		vol_data[frame_num] = cur_vol;

	pthread_mutex_unlock(&source->audio_actions_mutex);

	for (size_t mix = 0; mix < MAX_AUDIO_MIXES; mix++) {
		if ((source->audio_mixers & (1 << mix)) != 0)
			multiply_vol_data(source, mix, channels, vol_data);
	}
}

static void apply_audio_volume(obs_source_t *source, uint32_t mixers, size_t channels, size_t sample_rate)
{
	struct audio_action action;
	bool actions_pending;
	float vol;

	pthread_mutex_lock(&source->audio_actions_mutex);

	actions_pending = source->audio_actions.num > 0;
	if (actions_pending)
		action = source->audio_actions.array[0];

	pthread_mutex_unlock(&source->audio_actions_mutex);

	if (actions_pending) {
		uint64_t duration = conv_frames_to_time(sample_rate, AUDIO_OUTPUT_FRAMES);

		if (action.timestamp < (source->audio_ts + duration)) {
			apply_audio_actions(source, channels, sample_rate);
			return;
		}
	}

	vol = get_source_volume(source, source->audio_ts);
	if (vol == 1.0f)
		return;

	if (vol == 0.0f || mixers == 0) {
		memset(source->audio_output_buf[0][0], 0,
		       AUDIO_OUTPUT_FRAMES * sizeof(float) * MAX_AUDIO_CHANNELS * MAX_AUDIO_MIXES);
		return;
	}

	for (size_t mix = 0; mix < MAX_AUDIO_MIXES; mix++) {
		uint32_t mix_and_val = (1 << mix);
		if ((source->audio_mixers & mix_and_val) != 0 && (mixers & mix_and_val) != 0)
			multiply_output_audio(source, mix, channels, vol);
	}
}

static void custom_audio_render(obs_source_t *source, uint32_t mixers, size_t channels, size_t sample_rate)
{
	struct obs_source_audio_mix audio_data;
	bool success;
	uint64_t ts;

	for (size_t mix = 0; mix < MAX_AUDIO_MIXES; mix++) {
		for (size_t ch = 0; ch < channels; ch++) {
			audio_data.output[mix].data[ch] = source->audio_output_buf[mix][ch];
		}

		if ((source->audio_mixers & mixers & (1 << mix)) != 0) {
			memset(source->audio_output_buf[mix][0], 0, sizeof(float) * AUDIO_OUTPUT_FRAMES * channels);
		}
	}

	success = source->info.audio_render(source->context.data, &ts, &audio_data, mixers, channels, sample_rate);
	source->audio_ts = success ? ts : 0;
	source->audio_pending = !success;

	if (!success || !source->audio_ts || !mixers)
		return;

	for (size_t mix = 0; mix < MAX_AUDIO_MIXES; mix++) {
		uint32_t mix_bit = 1 << mix;

		if ((mixers & mix_bit) == 0)
			continue;

		if ((source->audio_mixers & mix_bit) == 0) {
			memset(source->audio_output_buf[mix][0], 0, sizeof(float) * AUDIO_OUTPUT_FRAMES * channels);
		}
	}

	apply_audio_volume(source, mixers, channels, sample_rate);
}

static void audio_submix(obs_source_t *source, size_t channels, size_t sample_rate)
{
	struct audio_output_data audio_data;
	struct obs_source_audio audio = {0};
	bool success;
	uint64_t ts;

	for (size_t ch = 0; ch < channels; ch++) {
		audio_data.data[ch] = source->audio_mix_buf[ch];
	}

	memset(source->audio_mix_buf[0], 0, sizeof(float) * AUDIO_OUTPUT_FRAMES * channels);

	success = source->info.audio_mix(source->context.data, &ts, &audio_data, channels, sample_rate);

	if (!success)
		return;

	for (size_t i = 0; i < channels; i++)
		audio.data[i] = (const uint8_t *)audio_data.data[i];

	audio.samples_per_sec = (uint32_t)sample_rate;
	audio.frames = AUDIO_OUTPUT_FRAMES;
	audio.format = AUDIO_FORMAT_FLOAT_PLANAR;
	audio.speakers = (enum speaker_layout)channels;
	audio.timestamp = ts;

	obs_source_output_audio(source, &audio);
}

static inline void process_audio_source_tick(obs_source_t *source, uint32_t mixers, size_t channels, size_t sample_rate,
					     size_t size)
{
	bool audio_submix = !!(source->info.output_flags & OBS_SOURCE_SUBMIX);

	pthread_mutex_lock(&source->audio_buf_mutex);

	if (source->audio_input_buf[0].size < size) {
		source->audio_pending = true;
		pthread_mutex_unlock(&source->audio_buf_mutex);
		return;
	}

	for (size_t ch = 0; ch < channels; ch++)
		deque_peek_front(&source->audio_input_buf[ch], source->audio_output_buf[0][ch], size);

	pthread_mutex_unlock(&source->audio_buf_mutex);

	for (size_t mix = 1; mix < MAX_AUDIO_MIXES; mix++) {
		uint32_t mix_and_val = (1 << mix);

		if (audio_submix) {
			if (mix > 1)
				break;

			mixers = 1;
			mix_and_val = 1;
		}

		if ((source->audio_mixers & mix_and_val) == 0 || (mixers & mix_and_val) == 0) {
			memset(source->audio_output_buf[mix][0], 0, size * channels);
			continue;
		}

		for (size_t ch = 0; ch < channels; ch++)
			memcpy(source->audio_output_buf[mix][ch], source->audio_output_buf[0][ch], size);
	}

	if (audio_submix) {
		source->audio_pending = false;
		return;
	}

	if ((source->audio_mixers & 1) == 0 || (mixers & 1) == 0)
		memset(source->audio_output_buf[0][0], 0, size * channels);

	apply_audio_volume(source, mixers, channels, sample_rate);
	source->audio_pending = false;
}

void obs_source_audio_render(obs_source_t *source, uint32_t mixers, size_t channels, size_t sample_rate, size_t size)
{
	if (!source->audio_output_buf[0][0]) {
		source->audio_pending = true;
		return;
	}

	if (source->info.audio_render) {
		if (!source->context.data) {
			source->audio_pending = true;
			return;
		}
		custom_audio_render(source, mixers, channels, sample_rate);
		return;
	}

	if (source->info.audio_mix) {
		audio_submix(source, channels, sample_rate);
	}

	if (!source->audio_ts) {
		source->audio_pending = true;
		return;
	}

	process_audio_source_tick(source, mixers, channels, sample_rate, size);
}

bool obs_source_audio_pending(const obs_source_t *source)
{
	if (!obs_source_valid(source, "obs_source_audio_pending"))
		return true;

	if (obs_source_removed(source))
		return true;

	return (is_composite_source(source) || is_audio_source(source)) ? source->audio_pending : true;
}

uint64_t obs_source_get_audio_timestamp(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_get_audio_timestamp") ? source->audio_ts : 0;
}

void obs_source_get_audio_mix(const obs_source_t *source, struct obs_source_audio_mix *audio)
{
	if (!obs_source_valid(source, "obs_source_get_audio_mix"))
		return;
	if (!obs_ptr_valid(audio, "audio"))
		return;

	for (size_t mix = 0; mix < MAX_AUDIO_MIXES; mix++) {
		for (size_t ch = 0; ch < MAX_AUDIO_CHANNELS; ch++) {
			audio->output[mix].data[ch] = source->audio_output_buf[mix][ch];
		}
	}
}

void obs_source_add_audio_pause_callback(obs_source_t *source, signal_callback_t callback, void *param)
{
	if (!obs_source_valid(source, "obs_source_add_audio_pause_callback"))
		return;

	signal_handler_t *handler = obs_source_get_signal_handler(source);

	signal_handler_connect(handler, "media_pause", callback, param);
	signal_handler_connect(handler, "media_stopped", callback, param);
}

void obs_source_remove_audio_pause_callback(obs_source_t *source, signal_callback_t callback, void *param)
{
	if (!obs_source_valid(source, "obs_source_remove_audio_pause_callback"))
		return;

	signal_handler_t *handler = obs_source_get_signal_handler(source);

	signal_handler_disconnect(handler, "media_pause", callback, param);
	signal_handler_disconnect(handler, "media_stopped", callback, param);
}

void obs_source_add_audio_capture_callback(obs_source_t *source, obs_source_audio_capture_t callback, void *param)
{
	struct audio_cb_info info = {callback, param};

	if (!obs_source_valid(source, "obs_source_add_audio_capture_callback"))
		return;

	pthread_mutex_lock(&source->audio_cb_mutex);
	da_push_back(source->audio_cb_list, &info);
	pthread_mutex_unlock(&source->audio_cb_mutex);
}

void obs_source_remove_audio_capture_callback(obs_source_t *source, obs_source_audio_capture_t callback, void *param)
{
	struct audio_cb_info info = {callback, param};

	if (!obs_source_valid(source, "obs_source_remove_audio_capture_callback"))
		return;

	pthread_mutex_lock(&source->audio_cb_mutex);
	da_erase_item(source->audio_cb_list, &info);
	pthread_mutex_unlock(&source->audio_cb_mutex);
}

void obs_source_set_monitoring_type(obs_source_t *source, enum obs_monitoring_type type)
{
	struct calldata data;
	uint8_t stack[128];
	bool was_on;
	bool now_on;

	if (!obs_source_valid(source, "obs_source_set_monitoring_type"))
		return;
	if (source->monitoring_type == type)
		return;

	calldata_init_fixed(&data, stack, sizeof(stack));
	calldata_set_ptr(&data, "source", source);
	calldata_set_int(&data, "type", type);

	signal_handler_signal(source->context.signals, "audio_monitoring", &data);

	was_on = source->monitoring_type != OBS_MONITORING_TYPE_NONE;
	now_on = type != OBS_MONITORING_TYPE_NONE;

	if (was_on != now_on) {
		if (!was_on) {
			source->monitor = audio_monitor_create(source);
		} else {
			audio_monitor_destroy(source->monitor);
			source->monitor = NULL;
		}
	}

	source->monitoring_type = type;
}

enum obs_monitoring_type obs_source_get_monitoring_type(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_get_monitoring_type") ? source->monitoring_type
									  : OBS_MONITORING_TYPE_NONE;
}

void obs_source_set_genlock_fifo(obs_source_t *source, bool enabled)
{
	if (!obs_source_valid(source, "obs_source_set_genlock_fifo"))
		return;

	source->genlock_fifo = enabled;
	blog(LOG_INFO, "genlock: FIFO frame consumption %s for source '%s'", enabled ? "ENABLED" : "disabled",
	     obs_source_get_name(source));
}

bool obs_source_get_genlock_fifo(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_get_genlock_fifo") ? source->genlock_fifo : false;
}

void obs_source_set_genlock_preload(obs_source_t *source, uint32_t frames)
{
	if (!obs_source_valid(source, "obs_source_set_genlock_preload"))
		return;

	/* camera-box #97: clamp to [0, GENLOCK_PRELOAD_MAX] and write UNDER
	 * async_mutex — the A/V thread reads source->genlock_preload in
	 * ready_async_frame()/cache_video() (both run holding async_mutex), so an
	 * unlocked write would race that read (the #93 UAF lesson the spec calls out).
	 * The mutex is recursive, so this is safe even if a caller already holds it. */
	const uint32_t clamped = genlock_clamp_preload_u32(frames);
	pthread_mutex_lock(&source->async_mutex);
	const uint32_t prev = source->genlock_preload;
	source->genlock_preload = clamped;
	/* camera-box #102: a runtime preload change re-arms the startup-fill latch so
	 * the delay line rebuilds to the new depth. On an INCREASE the FIFO holds while
	 * it fills up to the deeper delay (the #97 1->30 transition); on a DECREASE the
	 * current depth already exceeds the new preload, so genlock_decide re-latches
	 * filled on the very next tick with no repeat. Written under async_mutex (the
	 * A/V thread reads it in ready_async_frame). */
	if (clamped != prev) {
		source->genlock_filled = false;
		/* camera-box #126: keep the invariant "every site that re-arms the build latch
		 * also clears the consecutive-empty run" total across all five latch sites
		 * (create, overrun drain, flush, resume re-arm, and here). Harmless today (this
		 * path sets filled=false and the resume re-arm requires filled==true, so a stale
		 * count is already suppressed), but clearing it removes a latent spurious-re-arm
		 * hazard if a future preload path ever left filled==true. Under async_mutex. */
		source->genlock_empty_run = 0;
	}
	pthread_mutex_unlock(&source->async_mutex);

	if (clamped != prev)
		blog(LOG_INFO,
		     "genlock: preload (video delay) set to %u frame(s) (=%llu ms) for source '%s' (#97)",
		     clamped, (unsigned long long)genlock_preload_ms(clamped), obs_source_get_name(source));
}

uint32_t obs_source_get_genlock_preload(const obs_source_t *source)
{
	if (!obs_source_valid(source, "obs_source_get_genlock_preload"))
		return 0;
	/* Read under async_mutex (cast away const for the lock op only — the field
	 * value is not mutated here) to pair with the locked write above. */
	pthread_mutex_lock(&((obs_source_t *)source)->async_mutex);
	const uint32_t v = source->genlock_preload;
	pthread_mutex_unlock(&((obs_source_t *)source)->async_mutex);
	return v;
}

void obs_source_set_genlock_latency_ms(obs_source_t *source, uint32_t ms)
{
	if (!obs_source_valid(source, "obs_source_set_genlock_latency_ms"))
		return;

	/* camera-box #245/#257: per-source genlock LATENCY override in ms. Clamp to
	 * [GENLOCK_LATENCY_MS_MIN, GENLOCK_SOURCE_LATENCY_MS_MAX] — #257 hard-floors it at 3 ms
	 * (was 0 = follow-global; there is no env global any more, so the floor IS the minimum
	 * held latency, and 1 → 3, 0 → 3). Write UNDER async_mutex — the A/V thread reads
	 * source->genlock_latency_ms in ready_async_frame()/cache_video() (both hold
	 * async_mutex), so an unlocked write would race that read (the #93 UAF lesson, same as
	 * obs_source_set_genlock_preload). The mutex is recursive, so this is safe even if a
	 * caller already holds it. */
	const uint32_t clamped = ms < GENLOCK_LATENCY_MS_MIN ? GENLOCK_LATENCY_MS_MIN
			       : (ms > GENLOCK_SOURCE_LATENCY_MS_MAX ? GENLOCK_SOURCE_LATENCY_MS_MAX : ms);
	pthread_mutex_lock(&source->async_mutex);
	const uint32_t prev = source->genlock_latency_ms;
	source->genlock_latency_ms = clamped;
	/* A latency change re-arms the startup-fill latch so the FIFO rebuilds its delay
	 * line to the new depth (same rationale as the preload setter): on the ms path
	 * genlock_filled is forced true each tick, but re-arming on a change keeps the drop
	 * cap / build path consistent if the operator dials a deep delay live. */
	if (clamped != prev) {
		source->genlock_filled = false;
		source->genlock_empty_run = 0;
		/* camera-box #1003: the remembered on-air age describes a hold that no longer
		 * exists. Carried across a setpoint change it targets the OLD phase forever --
		 * and on a DECREASE it is actively harmful: the relock selection would return
		 * index 0 (shed NOTHING) while the lowered latency has already dropped
		 * genlock_backlog_relock_qdepth() below the unchanged depth, so the backlog
		 * branch qualifies EVERY tick, and because that branch pre-empts STEADY the
		 * issue-859/998 settle-back drain never runs either -- a permanent per-tick
		 * relock storm at the old hold. Clearing it re-targets the CONFIGURED latency,
		 * which sheds the overshoot in one relock. Mirror of the Tier-0 sim's
		 * set_reserve_ms (src/genlock_backlog.rs). */
		source->genlock_phase_anchor_ns = 0;
		/* camera-box #1161: force a bounded RE-ACQUIRE on a pin RISE. A per-source latency
		 * INCREASE asks the conveyor to present an OLDER frame (hold DEEPER), but the phase-
		 * locked boundary is a downward-only follower with no mechanism to ADD hold --
		 * genlock_phase_converge_due sheds only toward max(reserve, floor), and clearing only
		 * the anchor above leaves genlock_locked_next_boundary_ns locked at the OLD shallow
		 * depth, so the raised pin never moves the presented frame (issue 1161). Zeroing the
		 * locked boundary re-enters the ACQUIRE branch, where the #1161 bracketing gate holds
		 * until the queue has deepened to the new reserve and the existing
		 * genlock_relock_select_nearest then locks AT the raised depth. A DECREASE is
		 * deliberately left to the existing anchor-clear + backlog relock shed above (a
		 * re-acquire there would be needless churn -- the FIFO already holds MORE than the
		 * lowered target and sheds it in one relock). genlock_acquire_bracket_ticks is reset
		 * so the fail-open cap counts fresh for the new acquire episode.
		 * NB: `prev` is the RAW stored genlock_latency_ms. It is create-seeded to
		 * GENLOCK_LATENCY_MS_MIN_INIT (3) and every set clamps to >= GENLOCK_LATENCY_MS_MIN (3),
		 * so on this build it is never the 0 "unset -> follow global genlock_reserve_ms()"
		 * sentinel -- `clamped > prev` is a true effective-hold RISE. If the global reserve is
		 * ever re-enabled (GENLOCK_RESERVE_MS_DEFAULT != 0, a source left at 0), compare
		 * effective reserves here (`prev > 0 ? prev : genlock_reserve_ms()`) instead. */
		if (clamped > prev) {
			source->genlock_locked_next_boundary_ns = 0;
			source->genlock_acquire_bracket_ticks = 0;
		}
	}
	pthread_mutex_unlock(&source->async_mutex);

	if (clamped != prev)
		blog(LOG_INFO, "genlock: per-source latency set to %u ms for source '%s' (#245)", clamped,
		     obs_source_get_name(source));
}

uint32_t obs_source_get_genlock_latency_ms(const obs_source_t *source)
{
	if (!obs_source_valid(source, "obs_source_get_genlock_latency_ms"))
		return 0;
	/* Read under async_mutex (cast away const for the lock op only) to pair with the
	 * locked write above. */
	pthread_mutex_lock(&((obs_source_t *)source)->async_mutex);
	const uint32_t v = source->genlock_latency_ms;
	pthread_mutex_unlock(&((obs_source_t *)source)->async_mutex);
	return v;
}

/* camera-box #257: per-source MEASUREMENT-BURN toggle, runtime hot-apply (NO restart).
 * The DistroAV PROP_BURN field drives this via ndi_source_update; the QR burn filter reads
 * obs_source_get_genlock_burn(parent) each render to gate the QR composite. A plain bool
 * write (same shape as obs_source_set_genlock_fifo) — the burn filter's graphics-thread
 * read of a single bool needs no lock (a torn bool is not a concept on the target ABIs,
 * exactly as genlock_fifo). Default OFF; ON only in TEST mode. */
void obs_source_set_genlock_burn(obs_source_t *source, bool enabled)
{
	if (!obs_source_valid(source, "obs_source_set_genlock_burn"))
		return;
	const bool prev = source->genlock_burn;
	source->genlock_burn = enabled;
	if (prev != enabled)
		blog(LOG_INFO, "genlock: measurement burn %s for source '%s' (#257)",
		     enabled ? "ON" : "OFF", obs_source_get_name(source));
}

bool obs_source_get_genlock_burn(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_get_genlock_burn") ? source->genlock_burn : false;
}

/* camera-box #803: per-source ASRC toggle. A plain bool write (same shape as
 * obs_source_set_genlock_burn) -- the audio thread's read in process_audio()/asrc_process_audio()
 * is a single bool check (not torn on the target ABIs), and the servo itself is only ever mutated
 * from that same single audio-ingest call path, so no additional lock is needed here. Turning it
 * ON when the source's resampler doesn't exist yet is picked up lazily on the NEXT audio callback
 * (process_audio()'s reset_resampler() condition includes `asrc_enabled && !resampler`) rather
 * than forcing the reset from this (potentially different) calling thread.
 *
 * #912: obs_source_create_internal() now defaults every source's asrc_enabled to true (a BUILD
 * DEFAULT, mirroring issue 257) -- this setter is kept EXPORTed as an optional override path (a
 * future GUI property could still flip it off for some source class), but it is no longer the
 * normal way ASRC gets turned on; nothing in this vendored tree calls it today. */
void obs_source_set_asrc_enabled(obs_source_t *source, bool enabled)
{
	if (!obs_source_valid(source, "obs_source_set_asrc_enabled"))
		return;
	const bool prev = source->asrc_enabled;
	source->asrc_enabled = enabled;
	if (prev != enabled)
		blog(LOG_INFO, "asrc: %s for source '%s' (#803)", enabled ? "ENABLED" : "disabled",
		     obs_source_get_name(source));
}

bool obs_source_get_asrc_enabled(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_get_asrc_enabled") ? source->asrc_enabled : false;
}

/* camera-box #806: the OUTER-loop bias setter/getter. A plain forward to source->asrc's own
 * setter (media-io/asrc-compensator.c), which does the actual +/-10ppm clamp -- this function adds
 * no logic of its own beyond validity + a telemetry log line on genuine change, same shape as
 * obs_source_set_asrc_enabled above. Single-writer: only ever called from the obs-websocket
 * request thread (SetAsrcOuterBiasPpm), and asrc_compensator_set_outer_bias_ppm() only ever writes
 * the one outer_bias_ppm double field the audio-ingest thread reads once per callback -- an
 * ordinary (non-atomic) plain double read/write race here is the SAME accepted shape as the
 * pre-existing asrc_enabled bool above (a torn read is, at worst, one stale callback's worth of
 * bias, self-correcting on the next callback; never a crash). */
void obs_source_set_asrc_outer_bias_ppm(obs_source_t *source, double bias_ppm)
{
	if (!obs_source_valid(source, "obs_source_set_asrc_outer_bias_ppm"))
		return;
	const double prev = asrc_compensator_get_outer_bias_ppm(&source->asrc);
	asrc_compensator_set_outer_bias_ppm(&source->asrc, bias_ppm);
	const double applied = asrc_compensator_get_outer_bias_ppm(&source->asrc);
	if (prev != applied)
		blog(LOG_INFO, "asrc: outer-loop bias %.3fppm -> %.3fppm for source '%s' (#806)", prev, applied,
		     obs_source_get_name(source));
}

double obs_source_get_asrc_outer_bias_ppm(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_get_asrc_outer_bias_ppm")
		       ? asrc_compensator_get_outer_bias_ppm(&source->asrc)
		       : 0.0;
}

/* camera-box #926: read-only telemetry -- the servo's own live estimated/applied ppm, already
 * maintained fields of struct asrc_compensator (asrc_process_audio() writes them every audio
 * callback via asrc_compensator_compensate()). Plain forwards, no clamp/logic of their own; same
 * torn-read tolerance as every other ASRC accessor in this file. */
double obs_source_get_asrc_estimated_ppm(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_get_asrc_estimated_ppm") ? source->asrc.estimated_ppm : 0.0;
}

double obs_source_get_asrc_applied_ppm(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_get_asrc_applied_ppm") ? source->asrc.applied_ppm : 0.0;
}

void obs_source_set_async_unbuffered(obs_source_t *source, bool unbuffered)
{
	if (!obs_source_valid(source, "obs_source_set_async_unbuffered"))
		return;

	source->async_unbuffered = unbuffered;
}

bool obs_source_async_unbuffered(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_async_unbuffered") ? source->async_unbuffered : false;
}

obs_data_t *obs_source_get_private_settings(obs_source_t *source)
{
	if (!obs_ptr_valid(source, "obs_source_get_private_settings"))
		return NULL;

	obs_data_addref(source->private_settings);
	return source->private_settings;
}

void obs_source_set_async_decoupled(obs_source_t *source, bool decouple)
{
	if (!obs_ptr_valid(source, "obs_source_set_async_decoupled"))
		return;

	source->async_decoupled = decouple;
	if (decouple) {
		pthread_mutex_lock(&source->audio_buf_mutex);
		source->timing_set = false;
		reset_audio_data(source, 0);
		pthread_mutex_unlock(&source->audio_buf_mutex);
	}
}

bool obs_source_async_decoupled(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_async_decoupled") ? source->async_decoupled : false;
}

/* hidden/undocumented export to allow source type redefinition for scripts */
EXPORT void obs_enable_source_type(const char *name, bool enable)
{
	struct obs_source_info *info = get_source_info(name);
	if (!info)
		return;

	if (enable)
		info->output_flags &= ~OBS_SOURCE_CAP_DISABLED;
	else
		info->output_flags |= OBS_SOURCE_CAP_DISABLED;
}

enum speaker_layout obs_source_get_speaker_layout(obs_source_t *source)
{
	if (!obs_source_valid(source, "obs_source_get_audio_channels"))
		return SPEAKERS_UNKNOWN;

	return source->sample_info.speakers;
}

void obs_source_set_balance_value(obs_source_t *source, float balance)
{
	if (obs_source_valid(source, "obs_source_set_balance_value")) {
		struct calldata data;
		uint8_t stack[128];

		calldata_init_fixed(&data, stack, sizeof(stack));
		calldata_set_ptr(&data, "source", source);
		calldata_set_float(&data, "balance", balance);

		signal_handler_signal(source->context.signals, "audio_balance", &data);

		source->balance = (float)calldata_float(&data, "balance");
	}
}

float obs_source_get_balance_value(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_get_balance_value") ? source->balance : 0.5f;
}

void obs_source_set_audio_active(obs_source_t *source, bool active)
{
	if (!obs_source_valid(source, "obs_source_set_audio_active"))
		return;

	if (os_atomic_set_bool(&source->audio_active, active) == active)
		return;

	if (active)
		obs_source_dosignal(source, "source_audio_activate", "audio_activate");
	else
		obs_source_dosignal(source, "source_audio_deactivate", "audio_deactivate");
}

bool obs_source_audio_active(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_audio_active") ? os_atomic_load_bool(&source->audio_active) : false;
}

uint32_t obs_source_get_last_obs_version(const obs_source_t *source)
{
	return obs_source_valid(source, "obs_source_get_last_obs_version") ? source->last_obs_ver : 0;
}

enum obs_icon_type obs_source_get_icon_type(const char *id)
{
	const struct obs_source_info *info = get_source_info(id);
	return (info) ? info->icon_type : OBS_ICON_TYPE_UNKNOWN;
}

const char *obs_source_get_dark_icon(const char *id)
{
	if (obs_source_get_icon_type(id) != OBS_ICON_TYPE_CUSTOM)
		return NULL;

	const struct obs_source_info *info = get_source_info(id);
	return (info && info->get_dark_icon) ? info->get_dark_icon(info->type_data) : NULL;
}

const char *obs_source_get_light_icon(const char *id)
{
	if (obs_source_get_icon_type(id) != OBS_ICON_TYPE_CUSTOM)
		return NULL;

	const struct obs_source_info *info = get_source_info(id);
	return (info && info->get_light_icon) ? info->get_light_icon(info->type_data) : NULL;
}

void obs_source_media_play_pause(obs_source_t *source, bool pause)
{
	if (!data_valid(source, "obs_source_media_play_pause"))
		return;

	if ((source->info.output_flags & OBS_SOURCE_CONTROLLABLE_MEDIA) == 0)
		return;
	if (!source->info.media_play_pause)
		return;

	struct media_action action = {
		.type = MEDIA_ACTION_PLAY_PAUSE,
		.pause = pause,
	};

	pthread_mutex_lock(&source->media_actions_mutex);
	da_push_back(source->media_actions, &action);
	pthread_mutex_unlock(&source->media_actions_mutex);
}

void obs_source_media_restart(obs_source_t *source)
{
	if (!data_valid(source, "obs_source_media_restart"))
		return;

	if ((source->info.output_flags & OBS_SOURCE_CONTROLLABLE_MEDIA) == 0)
		return;
	if (!source->info.media_restart)
		return;

	struct media_action action = {
		.type = MEDIA_ACTION_RESTART,
	};

	pthread_mutex_lock(&source->media_actions_mutex);
	da_push_back(source->media_actions, &action);
	pthread_mutex_unlock(&source->media_actions_mutex);
}

void obs_source_media_stop(obs_source_t *source)
{
	if (!data_valid(source, "obs_source_media_stop"))
		return;

	if ((source->info.output_flags & OBS_SOURCE_CONTROLLABLE_MEDIA) == 0)
		return;
	if (!source->info.media_stop)
		return;

	struct media_action action = {
		.type = MEDIA_ACTION_STOP,
	};

	pthread_mutex_lock(&source->media_actions_mutex);
	da_push_back(source->media_actions, &action);
	pthread_mutex_unlock(&source->media_actions_mutex);
}

void obs_source_media_next(obs_source_t *source)
{
	if (!data_valid(source, "obs_source_media_next"))
		return;

	if ((source->info.output_flags & OBS_SOURCE_CONTROLLABLE_MEDIA) == 0)
		return;
	if (!source->info.media_next)
		return;

	struct media_action action = {
		.type = MEDIA_ACTION_NEXT,
	};

	pthread_mutex_lock(&source->media_actions_mutex);
	da_push_back(source->media_actions, &action);
	pthread_mutex_unlock(&source->media_actions_mutex);
}

void obs_source_media_previous(obs_source_t *source)
{
	if (!data_valid(source, "obs_source_media_previous"))
		return;

	if ((source->info.output_flags & OBS_SOURCE_CONTROLLABLE_MEDIA) == 0)
		return;
	if (!source->info.media_previous)
		return;

	struct media_action action = {
		.type = MEDIA_ACTION_PREVIOUS,
	};

	pthread_mutex_lock(&source->media_actions_mutex);
	da_push_back(source->media_actions, &action);
	pthread_mutex_unlock(&source->media_actions_mutex);
}

int64_t obs_source_media_get_duration(obs_source_t *source)
{
	if (!data_valid(source, "obs_source_media_get_duration"))
		return 0;

	if ((source->info.output_flags & OBS_SOURCE_CONTROLLABLE_MEDIA) == 0)
		return 0;
	if (source->info.media_get_duration)
		return source->info.media_get_duration(source->context.data);
	else
		return 0;
}

int64_t obs_source_media_get_time(obs_source_t *source)
{
	if (!data_valid(source, "obs_source_media_get_time"))
		return 0;

	if ((source->info.output_flags & OBS_SOURCE_CONTROLLABLE_MEDIA) == 0)
		return 0;
	if (source->info.media_get_time)
		return source->info.media_get_time(source->context.data);
	else
		return 0;
}

void obs_source_media_set_time(obs_source_t *source, int64_t ms)
{
	if (!data_valid(source, "obs_source_media_set_time"))
		return;
	if ((source->info.output_flags & OBS_SOURCE_CONTROLLABLE_MEDIA) == 0)
		return;
	if (!source->info.media_set_time)
		return;

	struct media_action action = {
		.type = MEDIA_ACTION_SET_TIME,
		.ms = ms,
	};

	pthread_mutex_lock(&source->media_actions_mutex);
	da_push_back(source->media_actions, &action);
	pthread_mutex_unlock(&source->media_actions_mutex);
}

enum obs_media_state obs_source_media_get_state(obs_source_t *source)
{
	if (!data_valid(source, "obs_source_media_get_state"))
		return OBS_MEDIA_STATE_NONE;
	if ((source->info.output_flags & OBS_SOURCE_CONTROLLABLE_MEDIA) == 0)
		return OBS_MEDIA_STATE_NONE;

	if (source->info.media_get_state)
		return source->info.media_get_state(source->context.data);
	else
		return OBS_MEDIA_STATE_NONE;
}

void obs_source_media_started(obs_source_t *source)
{
	if (!obs_source_valid(source, "obs_source_media_started"))
		return;
	if ((source->info.output_flags & OBS_SOURCE_CONTROLLABLE_MEDIA) == 0)
		return;

	obs_source_dosignal(source, NULL, "media_started");
}

void obs_source_media_ended(obs_source_t *source)
{
	if (!obs_source_valid(source, "obs_source_media_ended"))
		return;
	if ((source->info.output_flags & OBS_SOURCE_CONTROLLABLE_MEDIA) == 0)
		return;

	obs_source_dosignal(source, NULL, "media_ended");
}

obs_data_array_t *obs_source_backup_filters(obs_source_t *source)
{
	if (!obs_source_valid(source, "obs_source_backup_filters"))
		return NULL;

	obs_data_array_t *array = obs_data_array_create();

	pthread_mutex_lock(&source->filter_mutex);
	for (size_t i = 0; i < source->filters.num; i++) {
		struct obs_source *filter = source->filters.array[i];
		obs_data_t *data = obs_save_source(filter);
		obs_data_array_push_back(array, data);
		obs_data_release(data);
	}
	pthread_mutex_unlock(&source->filter_mutex);

	return array;
}

void obs_source_restore_filters(obs_source_t *source, obs_data_array_t *array)
{
	if (!obs_source_valid(source, "obs_source_restore_filters"))
		return;
	if (!obs_ptr_valid(array, "obs_source_restore_filters"))
		return;

	DARRAY(obs_source_t *) cur_filters;
	DARRAY(obs_source_t *) new_filters;
	obs_source_t *prev = NULL;

	da_init(cur_filters);
	da_init(new_filters);

	pthread_mutex_lock(&source->filter_mutex);

	/* clear filter list */
	da_reserve(cur_filters, source->filters.num);
	da_reserve(new_filters, source->filters.num);
	for (size_t i = 0; i < source->filters.num; i++) {
		obs_source_t *filter = source->filters.array[i];
		da_push_back(cur_filters, &filter);
		filter->filter_parent = NULL;
		filter->filter_target = NULL;
	}

	da_free(source->filters);
	pthread_mutex_unlock(&source->filter_mutex);

	/* add backed up filters */
	size_t count = obs_data_array_count(array);
	for (size_t i = 0; i < count; i++) {
		obs_data_t *data = obs_data_array_item(array, i);
		const char *name = obs_data_get_string(data, "name");
		obs_source_t *filter = NULL;

		/* if backed up filter already exists, don't create */
		for (size_t j = 0; j < cur_filters.num; j++) {
			obs_source_t *cur = cur_filters.array[j];
			const char *cur_name = cur->context.name;
			if (cur_name && strcmp(cur_name, name) == 0) {
				filter = obs_source_get_ref(cur);
				break;
			}
		}

		if (!filter)
			filter = obs_load_source(data);

		/* add filter */
		if (prev)
			prev->filter_target = filter;
		prev = filter;
		filter->filter_parent = source;
		da_push_back(new_filters, &filter);

		obs_data_release(data);
	}

	if (prev)
		prev->filter_target = source;

	pthread_mutex_lock(&source->filter_mutex);
	da_move(source->filters, new_filters);
	pthread_mutex_unlock(&source->filter_mutex);

	/* release filters */
	for (size_t i = 0; i < cur_filters.num; i++) {
		obs_source_t *filter = cur_filters.array[i];
		obs_source_release(filter);
	}

	da_free(cur_filters);
}

uint64_t obs_source_get_last_async_ts(const obs_source_t *source)
{
	return source->async_last_rendered_ts;
}

obs_canvas_t *obs_source_get_canvas(const obs_source_t *source)
{
	return obs_weak_canvas_get_canvas(source->canvas);
}
