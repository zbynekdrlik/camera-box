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

#pragma once

#include "util/c99defs.h"
#include "util/darray.h"
#include "util/deque.h"
#include "util/dstr.h"
#include "util/threading.h"
#include "util/platform.h"
#include "util/profiler.h"
#include "util/task.h"
#include "util/uthash.h"
#include "util/array-serializer.h"
#include "callback/signal.h"
#include "callback/proc.h"

#include "graphics/graphics.h"
#include "graphics/matrix4.h"

#include "media-io/audio-resampler.h"
#include "media-io/asrc-compensator.h" /* camera-box #803 */
#include "media-io/video-io.h"
#include "media-io/audio-io.h"

#include "obs.h"

#include <obsversion.h>
#include <caption/caption.h>

/* Custom helpers for the UUID hash table */
#define HASH_FIND_UUID(head, uuid, out) HASH_FIND(hh_uuid, head, uuid, UUID_STR_LENGTH, out)
#define HASH_ADD_UUID(head, uuid_field, add) HASH_ADD(hh_uuid, head, uuid_field[0], UUID_STR_LENGTH, add)

#define NUM_TEXTURES 2
#define NUM_CHANNELS 3
#define MICROSECOND_DEN 1000000
#define NUM_ENCODE_TEXTURES 10
#define NUM_ENCODE_TEXTURE_FRAMES_TO_WAIT 1

static inline int64_t packet_dts_usec(struct encoder_packet *packet)
{
	return packet->dts * MICROSECOND_DEN / packet->timebase_den;
}

struct tick_callback {
	void (*tick)(void *param, float seconds);
	void *param;
};

struct draw_callback {
	void (*draw)(void *param, uint32_t cx, uint32_t cy);
	void *param;
};

struct rendered_callback {
	void (*rendered)(void *param);
	void *param;
};

struct packet_callback {
	void (*packet_cb)(obs_output_t *output, struct encoder_packet *pkt, struct encoder_packet_time *pkt_time,
			  void *param);
	void *param;
};

struct reconnect_callback {
	bool (*reconnect_cb)(void *data, obs_output_t *output, int code);
	void *param;
};

/* ------------------------------------------------------------------------- */
/* validity checks */

static inline bool obs_object_valid(const void *obj, const char *f, const char *t)
{
	if (!obj) {
		blog(LOG_DEBUG, "%s: Null '%s' parameter", f, t);
		return false;
	}

	return true;
}

#define obs_ptr_valid(ptr, func) obs_object_valid(ptr, func, #ptr)
#define obs_source_valid obs_ptr_valid
#define obs_output_valid obs_ptr_valid
#define obs_encoder_valid obs_ptr_valid
#define obs_service_valid obs_ptr_valid

/* ------------------------------------------------------------------------- */
/* modules */

struct obs_module {
	char *mod_name;
	const char *file;
	char *bin_path;
	char *data_path;
	void *module;
	bool loaded;

	enum obs_module_load_state load_state;

	bool (*load)(void);
	void (*unload)(void);
	void (*post_load)(void);
	void (*set_locale)(const char *locale);
	bool (*get_string)(const char *lookup_string, const char **translated_string);
	void (*free_locale)(void);
	uint32_t (*ver)(void);
	void (*set_pointer)(obs_module_t *module);
	const char *(*name)(void);
	const char *(*description)(void);
	const char *(*author)(void);

	struct obs_module_metadata *metadata;

	struct obs_module *next;

	DARRAY(char *) sources;
	DARRAY(char *) outputs;
	DARRAY(char *) encoders;
	DARRAY(char *) services;
};

struct obs_disabled_module {
	char *mod_name;

	enum obs_module_load_state load_state;

	struct obs_module_metadata *metadata;
	struct obs_disabled_module *next;

	DARRAY(char *) sources;
	DARRAY(char *) outputs;
	DARRAY(char *) encoders;
	DARRAY(char *) services;
};

extern void free_module(struct obs_module *mod);

struct obs_module_path {
	char *bin;
	char *data;
};

static inline void free_module_path(struct obs_module_path *omp)
{
	if (omp) {
		bfree(omp->bin);
		bfree(omp->data);
	}
}

struct obs_module_metadata {
	char *display_name;
	char *version;
	char *id;
	char *os_arch;
	char *description;
	char *long_description;
	bool has_icon;
	bool has_banner;
	char *repository_url;
	char *support_url;
	char *website_url;
	char *name;
};

static inline void free_module_metadata(struct obs_module_metadata *omi)
{
	if (omi) {
		bfree(omi->display_name);
		bfree(omi->version);
		bfree(omi->id);
		bfree(omi->os_arch);
		bfree(omi->description);
		bfree(omi->long_description);
		bfree(omi->repository_url);
		bfree(omi->support_url);
		bfree(omi->website_url);
		bfree(omi->name);
	}
}

static inline bool check_path(const char *data, const char *path, struct dstr *output)
{
	dstr_copy(output, path);
	dstr_cat(output, data);

	return os_file_exists(output->array);
}

/* ------------------------------------------------------------------------- */
/* hotkeys */

struct obs_hotkey {
	obs_hotkey_id id;
	char *name;
	char *description;

	obs_hotkey_func func;
	void *data;
	int pressed;

	obs_hotkey_registerer_t registerer_type;
	void *registerer;

	obs_hotkey_id pair_partner_id;

	UT_hash_handle hh;
};

struct obs_hotkey_pair {
	obs_hotkey_pair_id pair_id;
	obs_hotkey_id id[2];
	obs_hotkey_active_func func[2];
	bool pressed0;
	bool pressed1;
	void *data[2];

	UT_hash_handle hh;
};

typedef struct obs_hotkey_pair obs_hotkey_pair_t;

typedef struct obs_hotkeys_platform obs_hotkeys_platform_t;

void *obs_hotkey_thread(void *param);

struct obs_core_hotkeys;
bool obs_hotkeys_platform_init(struct obs_core_hotkeys *hotkeys);
void obs_hotkeys_platform_free(struct obs_core_hotkeys *hotkeys);
bool obs_hotkeys_platform_is_pressed(obs_hotkeys_platform_t *context, obs_key_t key);

const char *obs_get_hotkey_translation(obs_key_t key, const char *def);

struct obs_context_data;
void obs_hotkeys_context_release(struct obs_context_data *context);

void obs_hotkeys_free(void);

struct obs_hotkey_binding {
	obs_key_combination_t key;
	bool pressed;
	bool modifiers_match;

	obs_hotkey_id hotkey_id;
	obs_hotkey_t *hotkey;
};

struct obs_hotkey_name_map_item;
void obs_hotkey_name_map_free(void);

/* ------------------------------------------------------------------------- */
/* views */

enum view_type {
	INVALID_VIEW,
	MAIN_VIEW,
	AUX_VIEW,
};

struct obs_view {
	pthread_mutex_t channels_mutex;
	obs_source_t *channels[MAX_CHANNELS];
	enum view_type type;
};

extern bool obs_view_init(struct obs_view *view, enum view_type type);
extern void obs_view_free(struct obs_view *view);

/* ------------------------------------------------------------------------- */
/* displays */

struct obs_display {
	bool update_color_space;
	bool enabled;
	uint32_t cx, cy;
	uint32_t next_cx, next_cy;
	uint32_t background_color;
	gs_swapchain_t *swap;
	pthread_mutex_t draw_callbacks_mutex;
	pthread_mutex_t draw_info_mutex;
	DARRAY(struct draw_callback) draw_callbacks;
	bool use_clear_workaround;

	/* camera-box #278: ADAPTIVE budget-based throttle so a heavy monitoring surface
	 * (the built-in Multiview projector) cannot steal the 60fps program render budget.
	 * render_display() runs on the SINGLE graphics thread for ALL displays (program
	 * output, preview, every projector) sequentially AFTER output_frames(); a 9-23ms
	 * multiview render there pushes the tick past the frame deadline → the NEXT program
	 * frame starts late → renderSkip. render_divisor>1 marks a THROTTLEABLE monitoring
	 * display (set to 2 on the multiview only; 0/1 = program output + preview = never
	 * throttled). #276 skipped such a display every-Nth-frame, but a SINGLE 4-live-cam
	 * multiview render (~18-23ms) alone exceeds the 16.6ms budget, so even every other
	 * frame the rendered frames overran → ~29% program renderSkip. So instead we render a
	 * monitoring display ONLY when its measured cost (render_ewma_ns, an EWMA of the actual
	 * draw, α=1/4; 0 = not warmed up) fits the budget REMAINING after the program this tick.
	 * Both fields are PER-INSTANCE (never static — a static counter would lockstep every
	 * projector) and read+written only on the graphics thread; render_divisor is set once
	 * from the Qt thread at display create (same unguarded pattern as background_color).
	 * render_consecutive_skips (#293) is the anti-starvation counter: how many ticks in a row
	 * an over-budget monitoring display has been skipped — capped at
	 * OBS_DISPLAY_MAX_CONSECUTIVE_SKIPS (obs-display-budget.h) so the Multiview can never
	 * freeze; reset to 0 after every real render. render_frame_counter (#756) is a hard
	 * cadence counter, incremented every tick this display is considered (regardless of
	 * skip/render outcome) — obs_display_should_skip() uses
	 * `frame_counter % render_divisor != 0` to ALWAYS skip a throttleable display on an
	 * ineligible tick, closing the gap where a display cheap enough to always fit under
	 * budget was never actually throttled by the #278/#293 budget gate alone (imag-nb live
	 * finding: the Multiview rendered every tick despite render_divisor=2). All three
	 * fields are per-instance, graphics-thread-only. */
	uint64_t render_ewma_ns;
	uint32_t render_divisor;
	uint32_t render_consecutive_skips;
	uint32_t render_frame_counter;

	/* camera-box #1107: when true, this display's present uses vsync (eglSwapInterval 1) so
	 * its scanout is tear-free. Set ONLY for the fullscreen program projector (OBSProjector,
	 * savedMonitor > -1 && !isMultiview) via obs_display_set_vsync(); every other display —
	 * the OBS main window, the preview, the multiview — leaves it false → interval 0 → no
	 * added blocking present. Armed each tick by render_display() (graphics-thread-only,
	 * single-writer, same discipline as render_divisor). */
	bool vsync;

	/* camera-box #771: MV fps observability. A throttleable monitoring display (the
	 * Multiview projector) emits a `multiview-audit:` line every ~5s carrying its ACTUAL
	 * measured render cadence (real renders / window) so operators + drift-guard + the E2E
	 * preflight can SEE the multiview fps and alarm on a collapse (render_display(),
	 * obs-display.c; floor = obs_multiview_floor_fps(), obs-display-budget.h).
	 * render_audit_id is a stable monitor=N assigned once when the display first becomes
	 * throttleable; render_audit_window_start_ns marks the current audit window; and
	 * render_audit_render_count counts the real renders within it. Per-instance,
	 * graphics-thread-only (id set once on the Qt create thread, all read/written on the
	 * graphics thread), exactly like the #278/#293/#756 fields above. */
	uint32_t render_audit_id;
	uint64_t render_audit_window_start_ns;
	uint32_t render_audit_render_count;

	/* camera-box #1260: budget-gate PHASE split for the multiview-audit line. The MV collapse
	 * (rendered_fps below floor) is the #278 budget gate skipping when the pre-MV cost (ns
	 * already consumed on the graphics thread THIS tick before this display: tick_sources +
	 * output_frames + earlier displays) plus this display's render EWMA exceeds the ~30ms
	 * budget. Only rendered_fps (the outcome) was ever logged, never the terms, so which phase
	 * ate the budget could not be read from the log. Accumulate the pre-MV elapsed per tick
	 * (sum + max over the window; tick_count is the mean divisor) and print
	 * pre_mv_ms/pre_mv_max_ms alongside mv_ewma_ms/budget_ms at emit. Per-instance,
	 * graphics-thread-only, reset each audit window exactly like render_audit_render_count
	 * above. REPORT-ONLY observability: the skip DECISION (obs_display_should_skip) is
	 * untouched, so this changes what is LOGGED, never the throttle behaviour. */
	uint64_t render_audit_pre_mv_sum_ns;
	uint64_t render_audit_pre_mv_max_ns;
	uint32_t render_audit_tick_count;

	/* camera-box #1260 lever (1): per-cell MV render instrumentation. The #1260 budget-split
	 * above showed the MV PHASE itself is the variable cost (mv_ewma_ms 15-16 healthy vs 21-22
	 * collapsed) while pre_mv stays flat, and the 4K->1080p A/B falsified fill-rate as the
	 * lever — so the open question is WHICH cells cost what, and whether the phase is per-cell
	 * CPU draw-submission or a GPU/present-wait tail. render_display() cannot break this down:
	 * the cells are iterated one TU away, in the FRONTEND multiview draw callback
	 * (Multiview::Render, frontend/components/Multiview.cpp). That callback times each
	 * scene-cell obs_source_video_render on the graphics thread and publishes the per-render
	 * aggregate via obs_display_report_multiview_cells(), which folds it into these window
	 * accumulators; render_display() emits mv_cells / mv_cell_ms / mv_cell_max_ms / mv_top1 /
	 * mv_top2 alongside mv_ewma_ms, so `mv_ewma_ms - mv_cell_ms` is the present/GPU-sync tail.
	 * Per-instance, graphics-thread-only (the frontend callback runs inside render_display() on
	 * the single graphics thread — same single-writer discipline as the fields above, NO
	 * locks), reset each audit window exactly like render_audit_render_count. REPORT-ONLY: the
	 * skip DECISION is untouched. cell_render_count counts the renders that reported cells this
	 * window (the mean divisor for cell_sum_ns); top1/top2 hold the two fattest cells (ns +
	 * sanitized name, <= 63 chars) of the window's WORST render (largest per-render cell sum). */
	uint64_t render_audit_cell_sum_ns;
	uint64_t render_audit_cell_max_ns;
	uint32_t render_audit_cell_render_count;
	uint32_t render_audit_cell_count;
	uint64_t render_audit_top1_ns;
	uint64_t render_audit_top2_ns;
	char render_audit_top1_name[64];
	char render_audit_top2_name[64];

	struct obs_display *next;
	struct obs_display **prev_next;
};

extern bool obs_display_init(struct obs_display *display, const struct gs_init_data *graphics_data);
extern void obs_display_free(struct obs_display *display);

/* ------------------------------------------------------------------------- */
/* core */

struct obs_vframe_info {
	uint64_t timestamp;
	int count;
};

struct obs_tex_frame {
	gs_texture_t *tex;
	gs_texture_t *tex_uv;
	uint32_t handle;
	uint64_t timestamp;
	uint64_t lock_key;
	int count;
	bool released;
};

struct obs_task_info {
	obs_task_t task;
	void *param;
};

struct obs_core_video_mix {
	struct obs_view *view;

	gs_stagesurf_t *active_copy_surfaces[NUM_TEXTURES][NUM_CHANNELS];
	gs_stagesurf_t *copy_surfaces[NUM_TEXTURES][NUM_CHANNELS];
	gs_texture_t *convert_textures[NUM_CHANNELS];
	gs_texture_t *convert_textures_encode[NUM_CHANNELS];
#ifdef _WIN32
	gs_stagesurf_t *copy_surfaces_encode[NUM_TEXTURES];
#endif
	gs_texture_t *render_texture;
	gs_texture_t *output_texture;
	enum gs_color_space render_space;
	bool texture_rendered;
	bool textures_copied[NUM_TEXTURES];
	bool texture_converted;
	bool using_nv12_tex;
	bool using_p010_tex;
	struct deque vframe_info_buffer;
	struct deque vframe_info_buffer_gpu;
	gs_stagesurf_t *mapped_surfaces[NUM_CHANNELS];
	int cur_texture;
	volatile long raw_active;
	volatile long gpu_encoder_active;
	bool gpu_was_active;
	bool raw_was_active;
	bool was_active;
	pthread_mutex_t gpu_encoder_mutex;
	struct deque gpu_encoder_queue;
	struct deque gpu_encoder_avail_queue;
	DARRAY(obs_encoder_t *) gpu_encoders;
	os_sem_t *gpu_encode_semaphore;
	os_event_t *gpu_encode_inactive;
	pthread_t gpu_encode_thread;
	bool gpu_encode_thread_initialized;
	volatile bool gpu_encode_stop;

	video_t *video;
	struct obs_video_info ovi;

	bool gpu_conversion;
	const char *conversion_techs[NUM_CHANNELS];
	bool conversion_needed;
	float conversion_width_i;
	float conversion_height_i;

	float color_matrix[16];

	bool encoder_only_mix;
	long encoder_refs;

	bool mix_audio;
};

extern struct obs_core_video_mix *obs_create_video_mix(struct obs_video_info *ovi);
extern void obs_free_video_mix(struct obs_core_video_mix *video);

struct obs_core_video {
	graphics_t *graphics;
	gs_effect_t *default_effect;
	gs_effect_t *default_rect_effect;
	gs_effect_t *opaque_effect;
	gs_effect_t *solid_effect;
	gs_effect_t *repeat_effect;
	gs_effect_t *conversion_effect;
	gs_effect_t *bicubic_effect;
	gs_effect_t *lanczos_effect;
	gs_effect_t *area_effect;
	gs_effect_t *bilinear_lowres_effect;
	gs_effect_t *premultiplied_alpha_effect;
	gs_samplerstate_t *point_sampler;

	uint64_t video_time;
	uint64_t video_frame_interval_ns;
	/* camera-box #278: os_gettime_ns() captured at the TOP of the current graphics tick
	 * (obs_graphics_thread_loop). render_display() reads it to compute how much of the
	 * frame budget the program (output_frames + earlier displays) has already consumed,
	 * so a monitoring display only renders when slack remains. Written + read only on the
	 * graphics thread. */
	uint64_t graphics_frame_start_ns;
	/* camera-box #1063: the PREVIOUS graphics tick's completed total frame_time_ns, published
	 * at the END of obs_graphics_thread_loop(). obs_aux_sender_should_skip() gates on
	 * max(elapsed, last_tick_total_ns) so an aux ndi_filter that decides EARLY in the tick
	 * (before output_frames() has accrued into `elapsed`) still throttles on a genuinely-heavy
	 * tick regardless of render order. 0 before the first completed tick (fail-open). Written +
	 * read only on the graphics thread. */
	uint64_t last_tick_total_ns;
	uint64_t video_half_frame_interval_ns;
	uint64_t video_avg_frame_time_ns;
	double video_fps;
	pthread_t video_thread;
	uint32_t total_frames;
	uint32_t lagged_frames;
	bool thread_initialized;

	gs_texture_t *transparent_texture;

	gs_effect_t *deinterlace_discard_effect;
	gs_effect_t *deinterlace_discard_2x_effect;
	gs_effect_t *deinterlace_linear_effect;
	gs_effect_t *deinterlace_linear_2x_effect;
	gs_effect_t *deinterlace_blend_effect;
	gs_effect_t *deinterlace_blend_2x_effect;
	gs_effect_t *deinterlace_yadif_effect;
	gs_effect_t *deinterlace_yadif_2x_effect;

	float sdr_white_level;
	float hdr_nominal_peak_level;

	pthread_mutex_t task_mutex;
	struct deque tasks;

	pthread_mutex_t encoder_group_mutex;
	DARRAY(obs_weak_encoder_t *) ready_encoder_groups;

	pthread_mutex_t mixes_mutex;
	DARRAY(struct obs_core_video_mix *) mixes;
};

extern void add_ready_encoder_group(obs_encoder_t *encoder);

struct audio_monitor;

struct obs_core_audio {
	audio_t *audio;

	DARRAY(struct obs_source *) render_order;
	DARRAY(struct obs_source *) root_nodes;

	uint64_t buffered_ts;
	struct deque buffered_timestamps;
	uint64_t buffering_wait_ticks;
	int total_buffering_ticks;
	int max_buffering_ticks;
	bool fixed_buffer;

	pthread_mutex_t monitoring_mutex;
	DARRAY(struct audio_monitor *) monitors;
	char *monitoring_device_name;
	char *monitoring_device_id;

	pthread_mutex_t task_mutex;
	struct deque tasks;

	struct obs_source *monitoring_duplicating_source;
};

/* user sources, output channels, and displays */
struct obs_core_data {
	/* Hash tables (uthash) */
	struct obs_source *sources;        /* Lookup by UUID (hh_uuid) */
	struct obs_source *public_sources; /* Lookup by name (hh) */

	struct obs_canvas *canvases;       /* Lookup by UUID (hh_uuid) */
	struct obs_canvas *named_canvases; /* Lookup by name (hh) */

	/* Linked lists */
	struct obs_source *first_audio_source;
	struct obs_display *first_display;
	struct obs_output *first_output;
	struct obs_encoder *first_encoder;
	struct obs_service *first_service;

	pthread_mutex_t sources_mutex;
	pthread_mutex_t displays_mutex;
	pthread_mutex_t outputs_mutex;
	pthread_mutex_t encoders_mutex;
	pthread_mutex_t services_mutex;
	pthread_mutex_t audio_sources_mutex;
	pthread_mutex_t draw_callbacks_mutex;
	pthread_mutex_t canvases_mutex;
	DARRAY(struct draw_callback) draw_callbacks;
	DARRAY(struct rendered_callback) rendered_callbacks;
	DARRAY(struct tick_callback) tick_callbacks;

	/* Main canvas, guaranteed to exist for the lifetime of the program */
	struct obs_canvas *main_canvas;

	long long unnamed_index;

	obs_data_t *private_data;

	volatile bool valid;

	DARRAY(char *) protocols;
	DARRAY(obs_source_t *) sources_to_tick;
};

/* user hotkeys */
struct obs_core_hotkeys {
	pthread_mutex_t mutex;
	obs_hotkey_t *hotkeys;
	obs_hotkey_id next_id;
	obs_hotkey_pair_t *hotkey_pairs;
	obs_hotkey_pair_id next_pair_id;

	pthread_t hotkey_thread;
	bool hotkey_thread_initialized;
	os_event_t *stop_event;
	bool thread_disable_press;
	bool strict_modifiers;
	bool reroute_hotkeys;
	DARRAY(obs_hotkey_binding_t) bindings;

	obs_hotkey_callback_router_func router_func;
	void *router_func_data;

	obs_hotkeys_platform_t *platform_context;

	pthread_once_t name_map_init_token;
	struct obs_hotkey_name_map_item *name_map;

	signal_handler_t *signals;

	char *translations[OBS_KEY_LAST_VALUE];
	char *mute;
	char *unmute;
	char *push_to_mute;
	char *push_to_talk;
	char *sceneitem_show;
	char *sceneitem_hide;
};

typedef DARRAY(struct obs_source_info) obs_source_info_array_t;

struct obs_core {
	struct obs_module *first_module;
	struct obs_module *first_disabled_module;

	DARRAY(struct obs_module_path) module_paths;
	DARRAY(char *) safe_modules;
	DARRAY(char *) disabled_modules;
	DARRAY(char *) core_modules;

	obs_source_info_array_t source_types;
	obs_source_info_array_t input_types;
	obs_source_info_array_t filter_types;
	obs_source_info_array_t transition_types;
	DARRAY(struct obs_output_info) output_types;
	DARRAY(struct obs_encoder_info) encoder_types;
	DARRAY(struct obs_service_info) service_types;

	signal_handler_t *signals;
	proc_handler_t *procs;

	char *locale;
	char *module_config_path;
	bool name_store_owned;
	profiler_name_store_t *name_store;

	/* segmented into multiple sub-structures to keep things a bit more
	 * clean and organized */
	struct obs_core_video video;
	struct obs_core_audio audio;
	struct obs_core_data data;
	struct obs_core_hotkeys hotkeys;

	os_task_queue_t *destruction_task_thread;

	obs_task_handler_t ui_task_handler;
};

extern struct obs_core *obs;

struct obs_graphics_context {
	uint64_t last_time;
	uint64_t interval;
	uint64_t frame_time_total_ns;
	uint64_t fps_total_ns;
	uint32_t fps_total_frames;
	const char *video_thread_name;
	/* camera-box #1029: ~5s PROGRAM-render observability window. Distinct from the obs_display
	 * render_audit_* fields (#771, the multiview monitoring surfaces) — these track the PROGRAM
	 * output's own total_frames/lagged_frames deltas so a burn-square forward JUMP can be
	 * attributed to the render path (lagged>0). Report-only; single-writer (this graphics
	 * thread). */
	uint64_t program_render_audit_window_start_ns;
	uint32_t program_render_audit_total_at_start;
	uint32_t program_render_audit_lagged_at_start;
};

extern void *obs_graphics_thread(void *param);
extern bool obs_graphics_thread_loop(struct obs_graphics_context *context);
#ifdef __APPLE__
extern void *obs_graphics_thread_autorelease(void *param);
extern bool obs_graphics_thread_loop_autorelease(struct obs_graphics_context *context);
#endif

extern gs_effect_t *obs_load_effect(gs_effect_t **effect, const char *file);

extern bool audio_callback(void *param, uint64_t start_ts_in, uint64_t end_ts_in, uint64_t *out_ts, uint32_t mixers,
			   struct audio_output_data *mixes);

extern struct obs_core_video_mix *get_mix_for_video(video_t *video);

extern void start_raw_video(video_t *video, const struct video_scale_info *conversion, uint32_t frame_rate_divisor,
			    void (*callback)(void *param, struct video_data *frame), void *param);
extern void stop_raw_video(video_t *video, void (*callback)(void *param, struct video_data *frame), void *param);

/* ------------------------------------------------------------------------- */
/* obs shared context data */

struct obs_weak_ref {
	volatile long refs;
	volatile long weak_refs;
};

struct obs_weak_object {
	struct obs_weak_ref ref;
	struct obs_context_data *object;
};

typedef void (*obs_destroy_cb)(void *obj);

struct obs_context_data {
	char *name;
	const char *uuid;
	void *data;
	obs_data_t *settings;
	signal_handler_t *signals;
	proc_handler_t *procs;
	enum obs_obj_type type;

	struct obs_weak_object *control;
	obs_destroy_cb destroy;

	DARRAY(obs_hotkey_id) hotkeys;
	DARRAY(obs_hotkey_pair_id) hotkey_pairs;
	obs_data_t *hotkey_data;

	DARRAY(char *) rename_cache;
	pthread_mutex_t rename_cache_mutex;

	pthread_mutex_t *mutex;
	struct obs_context_data *next;
	struct obs_context_data **prev_next;

	UT_hash_handle hh;
	UT_hash_handle hh_uuid;

	bool private;
};

extern bool obs_context_data_init(struct obs_context_data *context, enum obs_obj_type type, obs_data_t *settings,
				  const char *name, const char *uuid, obs_data_t *hotkey_data, bool private);
extern void obs_context_init_control(struct obs_context_data *context, void *object, obs_destroy_cb destroy);
extern void obs_context_data_free(struct obs_context_data *context);

extern void obs_context_data_insert(struct obs_context_data *context, pthread_mutex_t *mutex, void *first);
extern void obs_context_data_insert_name(struct obs_context_data *context, pthread_mutex_t *mutex, void *first);
extern void obs_context_data_insert_uuid(struct obs_context_data *context, pthread_mutex_t *mutex, void *first_uuid);

extern void obs_context_data_remove(struct obs_context_data *context);
extern void obs_context_data_remove_name(struct obs_context_data *context, pthread_mutex_t *mutex, void *phead);
extern void obs_context_data_remove_uuid(struct obs_context_data *context, pthread_mutex_t *mutex, void *puuid_head);

extern void obs_context_wait(struct obs_context_data *context);

extern void obs_context_data_setname(struct obs_context_data *context, const char *name);
extern void obs_context_data_setname_ht(struct obs_context_data *context, const char *name, void *phead);

/* ------------------------------------------------------------------------- */
/* ref-counting  */

static inline void obs_ref_addref(struct obs_weak_ref *ref)
{
	os_atomic_inc_long(&ref->refs);
}

static inline bool obs_ref_release(struct obs_weak_ref *ref)
{
	return os_atomic_dec_long(&ref->refs) == -1;
}

static inline void obs_weak_ref_addref(struct obs_weak_ref *ref)
{
	os_atomic_inc_long(&ref->weak_refs);
}

static inline bool obs_weak_ref_release(struct obs_weak_ref *ref)
{
	return os_atomic_dec_long(&ref->weak_refs) == -1;
}

static inline bool obs_weak_ref_get_ref(struct obs_weak_ref *ref)
{
	long owners = os_atomic_load_long(&ref->refs);
	while (owners > -1) {
		if (os_atomic_compare_exchange_long(&ref->refs, &owners, owners + 1)) {
			return true;
		}
	}

	return false;
}

static inline bool obs_weak_ref_expired(struct obs_weak_ref *ref)
{
	long owners = os_atomic_load_long(&ref->refs);
	return owners < 0;
}

/* ------------------------------------------------------------------------- */
/* canvases */

struct obs_weak_canvas {
	struct obs_weak_ref ref;
	struct obs_canvas *canvas;
};

struct obs_canvas {
	struct obs_context_data context;

	/* obs_canvas_flags */
	uint32_t flags;
	/* Video info for this canvas, FPS ignored */
	struct obs_video_info ovi;

	/* Hash table containing scenes (and groups) associated with this canvas */
	struct obs_source *sources;
	pthread_mutex_t sources_mutex;

	/* For now, canvas objects mainly act as a proxy for the existing view and video mix objects,
	 * though this may change in the future. */
	struct obs_view view;
	struct obs_core_video_mix *mix;
};

extern obs_canvas_t *obs_create_main_canvas(void);
extern void obs_canvas_destroy(obs_canvas_t *canvas);
extern void obs_canvas_clear_mix(obs_canvas_t *canvas);
extern void obs_free_canvas_mixes(void);
extern bool obs_canvas_has_valid_video_info(obs_canvas_t *canvas);
extern bool obs_canvas_reset_video_internal(obs_canvas_t *canvas, struct obs_video_info *ovi);
extern void obs_canvas_insert_source(obs_canvas_t *canvas, obs_source_t *source);
extern void obs_canvas_remove_source(obs_source_t *source);
extern void obs_canvas_rename_source(obs_source_t *source, const char *name);

/* ------------------------------------------------------------------------- */
/* sources  */

struct async_frame {
	struct obs_source_frame *frame;
	long unused_count;
	bool used;
};

enum audio_action_type {
	AUDIO_ACTION_VOL,
	AUDIO_ACTION_MUTE,
	AUDIO_ACTION_PTT,
	AUDIO_ACTION_PTM,
};

struct audio_action {
	uint64_t timestamp;
	enum audio_action_type type;
	union {
		float vol;
		bool set;
	};
};

struct obs_weak_source {
	struct obs_weak_ref ref;
	struct obs_source *source;
};

struct audio_cb_info {
	obs_source_audio_capture_t callback;
	void *param;
};

struct caption_cb_info {
	obs_source_caption_t callback;
	void *param;
};

enum media_action_type {
	MEDIA_ACTION_NONE,
	MEDIA_ACTION_PLAY_PAUSE,
	MEDIA_ACTION_RESTART,
	MEDIA_ACTION_STOP,
	MEDIA_ACTION_NEXT,
	MEDIA_ACTION_PREVIOUS,
	MEDIA_ACTION_SET_TIME,
};

struct media_action {
	enum media_action_type type;
	union {
		bool pause;
		int64_t ms;
	};
};

struct obs_source {
	struct obs_context_data context;
	struct obs_source_info info;

	/* general exposed flags that can be set for the source */
	uint32_t flags;
	uint32_t default_flags;
	uint32_t last_obs_ver;

	/* indicates ownership of the info.id buffer */
	bool owns_info_id;

	/* signals to call the source update in the video thread */
	long defer_update_count;

	/* ensures show/hide are only called once */
	volatile long show_refs;

	/* ensures activate/deactivate are only called once */
	volatile long activate_refs;

	/* source is in the process of being destroyed */
	volatile long destroying;

	/* used to indicate that the source has been removed and all
	 * references to it should be released (not exactly how I would prefer
	 * to handle things but it's the best option) */
	bool removed;

	/*  used to indicate if the source should show up when queried for user ui */
	bool temp_removed;

	bool active;
	bool showing;

	/* used to temporarily disable sources if needed */
	bool enabled;

	/* hint to allow sources to render more quickly */
	bool texcoords_centered;

	/* timing (if video is present, is based upon video) */
	volatile bool timing_set;
	volatile uint64_t timing_adjust;
	uint64_t resample_offset;
	uint64_t next_audio_ts_min;
	uint64_t next_audio_sys_ts_min;
	uint64_t last_frame_ts;
	uint64_t last_sys_timestamp;
	bool async_rendered;

	/* audio */
	bool audio_failed;
	bool audio_pending;
	bool pending_stop;
	bool audio_active;
	bool user_muted;
	bool muted;
	struct obs_source *next_audio_source;
	struct obs_source **prev_next_audio_source;
	uint64_t audio_ts;
	struct deque audio_input_buf[MAX_AUDIO_CHANNELS];
	size_t last_audio_input_buf_size;
	DARRAY(struct audio_action) audio_actions;
	float *audio_output_buf[MAX_AUDIO_MIXES][MAX_AUDIO_CHANNELS];
	float *audio_mix_buf[MAX_AUDIO_CHANNELS];
	struct resample_info sample_info;
	audio_resampler_t *resampler;
	/* camera-box #803: per-source ASRC (async sample-rate conversion) servo, continuously
	 * holding this source's audio timeline on the video master clock. `asrc_enabled` is a
	 * plain runtime bool -- #912: default ON (a BUILD DEFAULT set in
	 * obs_source_create_internal(), mirroring issue 257's render-tick/ts-align hard-lock), NOT
	 * default OFF any more; it CAN still be toggled live via obs_source_set_asrc_enabled() (no
	 * restart needed), but that setter is now only an optional override path, never the normal
	 * way ASRC gets turned on. `asrc` is the servo's own state (see
	 * media-io/asrc-compensator.h), mutated ONLY from process_audio() on this source's own
	 * audio-ingest call path (single-writer, no lock needed for the struct itself).
	 * `asrc_last_wall_ns`/`asrc_has_last_wall` track the wall-clock timestamp of the PREVIOUS
	 * audio callback (genlock_wall_now_ns(), the same basis the video FIFO release uses) so
	 * process_audio() can measure this callback's true master-clock block duration. */
	bool asrc_enabled;
	struct asrc_compensator asrc;
	uint64_t asrc_last_wall_ns;
	bool asrc_has_last_wall;
	pthread_mutex_t audio_actions_mutex;
	pthread_mutex_t audio_buf_mutex;
	pthread_mutex_t audio_mutex;
	pthread_mutex_t audio_cb_mutex;
	DARRAY(struct audio_cb_info) audio_cb_list;
	struct obs_audio_data audio_data;
	size_t audio_storage_size;
	uint32_t audio_mixers;
	float user_volume;
	float volume;
	int64_t sync_offset;
	int64_t last_sync_offset;
	float balance;
	/* audio_is_duplicated: tracks whether a source appears multiple times in the audio tree during this tick */
	bool audio_is_duplicated;

	/* async video data */
	gs_texture_t *async_textures[MAX_AV_PLANES];
	gs_texrender_t *async_texrender;
	struct obs_source_frame *cur_async_frame;
	bool async_gpu_conversion;
	enum video_format async_format;
	bool async_full_range;
	uint8_t async_trc;
	enum video_format async_cache_format;
	bool async_cache_full_range;
	uint8_t async_cache_trc;
	enum gs_color_format async_texture_formats[MAX_AV_PLANES];
	int async_channel_count;
	long async_rotation;
	bool async_flip;
	bool async_linear_alpha;
	bool async_active;
	bool async_update_texture;
	bool async_unbuffered;
	bool async_decoupled;
	bool genlock_fifo; /* camera-box #42: consume exactly one queued frame per render tick */
	bool genlock_burn; /* camera-box #257: per-source measurement-burn toggle (runtime, no restart) */
	/* camera-box #70: genlock FIFO preload reserve + audit counters. The FIFO
	 * holds `genlock_preload` frames of jitter buffer (set once at startup from
	 * OBS_GENLOCK_PRELOAD_FRAMES) and consumes one per tick only once the queue
	 * exceeds that depth, so NDI arrival jitter no longer empties it (underrun =
	 * a dropped/repeated frame). The counters are the audit evidence that
	 * underruns happen before the fix and stop after. */
	/* camera-box #97: per-source, runtime-settable preload reserve = frames of
	 * genlock-disciplined VIDEO DELAY. Initialized at source create from the
	 * OBS_GENLOCK_PRELOAD_FRAMES env default (back-compat with #70), then settable
	 * live via obs_source_set_genlock_preload() from the DistroAV slider to push
	 * the program video back ~1 s to line up with late audio on stream.lan. Read by
	 * the A/V thread in ready_async_frame()/cache_video() UNDER async_mutex, so
	 * set/get also take the lock — no unlocked mutation of a field the A/V thread
	 * reads (the #93 UAF lesson). One preload frame = one frame of delay. */
	uint32_t genlock_preload;          /* per-source jitter reserve / video delay (#97) */
	/* camera-box #245: per-source genlock latency override, in MS. 0 = follow the global
	 * default (genlock_reserve_ms() / OBS_GENLOCK_LATENCY_MS); >0 = THIS source holds
	 * exactly this latency (sub-frame ms ts-align deadline wall_now - latency_ms),
	 * overriding the global so each NDI source can hold a DIFFERENT latency (the #245
	 * per-source ask; #235 had collapsed it to one global knob). Set live via
	 * obs_source_set_genlock_latency_ms() from the DistroAV per-source ms field; read by
	 * the A/V thread in ready_async_frame() UNDER async_mutex (same lock discipline as
	 * genlock_preload, the #93 UAF lesson). Mirror of src/probe/genlock.rs
	 * effective_latency_ms(). */
	uint32_t genlock_latency_ms;       /* per-source latency override, ms (#245); 0 = global */
	/* camera-box #102: one-time startup-fill latch. While false the FIFO BUILDS to
	 * the `genlock_preload` delay depth (holding/repeating only during this initial
	 * fill); once the queue first exceeds preload it latches true and the FIFO then
	 * consumes a distinct frame on EVERY tick a frame is queued — repeating only on a
	 * TRUE empty, never on a jitter dip below the reserve (which the old #70 gate did,
	 * losing a distinct frame each time: 11.6%->34.3% on the live rig). Reset to false
	 * on an overrun force-drain (cache_video) so the delay line rebuilds. Read/written
	 * by the A/V thread under async_mutex (same as genlock_preload, the #93 lesson). */
	bool genlock_filled;
	/* camera-box #126: consecutive true-empty (underrun) render ticks in steady
	 * state. On an upstream OBS restart the NDI source underruns to empty but
	 * genlock_filled stays true (DistroAV KEEP_CONTENT blocks the NULL-emit reset; an
	 * underrun never fires the overrun force-drain), so the #102 steady gate consumes
	 * 1/tick on reconnect WITHOUT rebuilding the preload reserve — the video delay
	 * silently collapses to ~0. This counter increments at the get_closest_frame
	 * num==0 underrun and resets to 0 on every consume; when frames RESUME after a
	 * SUSTAINED run (>= GENLOCK_REARM_EMPTY_TICKS) ready_async_frame re-arms
	 * genlock_filled=false so the existing build path + #116 drain rebuild the reserve.
	 * Read/written by the A/V thread under async_mutex (same as genlock_filled). */
	uint32_t genlock_empty_run;
	uint64_t genlock_frames_received;  /* video frames queued onto async_frames */
	uint64_t genlock_frames_consumed;  /* frames handed to the compositor (one per tick) */
	uint64_t genlock_underruns;        /* #269 [4]: real FIFO starvation = TRUE-EMPTY only (the num==0 guard in get_closest_frame). The count-gate build-fill hold moved to genlock_holds — do NOT fold it back. */
	uint64_t genlock_holds;            /* camera-box #148/#269 [4]: BENIGN repeats, distinct from a real underrun — the ts-align source-early hold (frames queued, none yet due) AND the count-gate build-fill hold (still building the preload delay; recurs on every #126 reconnect re-arm). */
	uint64_t genlock_overruns;         /* per-source drop-cap drains (queue forced empty) */
	uint64_t genlock_backward_steps;   /* camera-box #147: ts-align re-anchors after a BACKWARD wall-clock step (NTP/PTP sawtooth). A queued frame stamped > one interval AHEAD of wall_now is impossible for a live capture (= captured before the step); instead of HOLDing (freezing the program feed) indefinitely, present the OLDEST queued frame and drain the stale pre-step seam one-per-tick, preserving the latency buffer. #269: counted ONCE per EVENT (genlock_in_backward_step latch), detected on the MAX queued ts (depth-independent, uniform across sources). Mirror of src/probe/genlock.rs genlock_release_guarded / the cam-EMIT guard #131. */
	bool genlock_in_backward_step;     /* camera-box #269 [2]: per-EVENT latch for genlock_backward_steps + the re-anchor LOG_WARNING. A backward step recovers over MANY ticks; this is true for the whole recovery so the counter/log fire only on the rising edge (one per event, not per tick — keeps the 5s audit gating). A normal/benign tick clears it — and since #1009 that clear is genlock_backward_regime_end(): the locked boundary is ZEROED so the release re-ACQUIREs the configured hold (the pre-#1009 clear left the FIFO consuming at the live edge forever). Mirror of src/probe/genlock.rs genlock_backward_step_latch / src/genlock_backlog.rs BackwardStepGuard. */
	uint32_t genlock_backward_pending_ticks;   /* camera-box #1009: consecutive over-margin due==0 ticks toward the SUSTAINED qualification (GENLOCK_BACKWARD_STEP_SUSTAIN_TICKS) — the re-anchor may only fire after the condition persisted this many ticks, never single-tick. Reset the moment the condition is absent (a 1-2 tick excursion is a transient, not a step). Zeroed at create (bzalloc). Mirror of src/genlock_backlog.rs BackwardStepGuard::pending_ticks. */
	uint64_t genlock_backward_regime_start_ns; /* camera-box #1009: monotonic (os_gettime_ns basis — immune to the very wall steps being handled) stamp of the current regime's entry; drives the bounded-cadence persistent-regime WARN (GENLOCK_BACKWARD_REGIME_WARN_AFTER_NS). Mirror of BackwardStepGuard::regime_start_ns. */
	uint64_t genlock_backward_last_warn_ns;    /* camera-box #1009: monotonic stamp of the last bounded-cadence regime WARN; 0 = none yet this regime, so the FIRST fires the moment the regime crosses the age threshold, then at most one per GENLOCK_BACKWARD_REGIME_WARN_INTERVAL_NS. Mirror of BackwardStepGuard::last_warn_ns. */
	uint64_t genlock_backward_regime_ticks;    /* camera-box #1009: CUMULATIVE backward-step re-anchor TICKS across all regimes (genlock_backward_steps counts EVENTS; this counts every re-anchored tick, so a sustained hold-bypass regime is visible as a climbing rate). Healthy operation keeps it at 0 — printed on the 5s audit line (backward_regime_ticks=) for the E2E/drift gates to assert on. Zeroed at create (bzalloc). Mirror of BackwardStepGuard::reanchor_ticks. */
	uint64_t genlock_locked_next_boundary_ns; /* camera-box #401: the capture-stamp boundary the ts-align release matures NEXT tick; 0 = UNLOCKED (acquire from the wall deadline). The pre-#401 release re-derived present_ts from the wall clock EVERY tick, so a reserve near a frame-interval multiple put the deadline ON a stamp and the ±2 ms render slew churned that frame due/not-due (hold ↔ silent drop; measured 43.9–57.7 distinct fps of a 60 fps flow, run 7020001). The locked boundary advances exactly one interval per presented frame — slew-immune by construction; the wall clock only acquires the lock and ages frames (v2: backlog is guarded by QUEUE DEPTH, never wall-boundary drift — that embedded the constant stamp->arrival skew and relock-stormed live). Zeroed at create (bzalloc, like the counters) and on a #147 backward-step re-anchor (the stamp timeline regressed — re-acquire coherently). Mirror of src/probe/genlock.rs ReleaseCadence. */
	uint64_t genlock_phase_anchor_ns;  /* camera-box #1003: the steady conveyor's own measured ON-AIR AGE (wall_now - presented stamp), updated on every STEADY / GAP-RESYNC present; 0 = UNSET (bzalloc zero-init) and the relock selection falls back to the source's configured latency. This is what makes a relock INHERIT the release phase instead of re-minting it: the pre-#1003 newest-due selection was an instant-sampled, STATELESS comparison of the RECEIVER-grid-floored deadline (#940 piece 3) against SENDER-grid stamps (33,333,300 ns vs 33,333,333 ns), carrying two independently flippable edges — ±2 ms of render-tick slew near the floor's step point moved the pinned cell a whole interval, and the fixed 5 ms hysteresis is a FIXED edge inside a phase that DRIFTS 33 ns/frame. Every lock episode therefore re-rolled a ±1-2 frame release phase (measured live: -64.5 / +56..63 ms steps between episodes at a ~923 ms knob). Selecting the frame NEAREST this remembered age is CONTINUOUS, so no edge exists for slew or beat to flip. PRESERVED across ACQUIRE / backlog relocks (a relock corrects DEPTH, never PHASE); CLEARED on a backward-step regime end (the wall clock moved, so every sampled age is wrong by the step) and on an async flush (the delay line is gone); RE-DERIVED by a GAP RESYNC present (upstream skipped stamps, so the pre-gap age describes a timeline that no longer exists). Mirror of src/genlock_backlog.rs relock_select_nearest / relock_anchor_age_ns / phase_anchor_from_present (Tier-0 unit-tested). */
	uint64_t genlock_dropped_due;      /* camera-box #401: frames DISCARDED by the ts-align release (stale catch-up at lock/relock; a late arrival whose boundary slot already passed). The pre-#401 release erased these with NO counter — run 7020001 lost 8,511 distinct ids invisibly. Steady state drops ZERO (exactly one frame matures per boundary); any sustained movement here is a real flow problem. Mirror of ReleaseCadence CadenceOutcome::dropped. */
	uint64_t genlock_relocks;          /* camera-box #401 v2: backlog re-locks — the queue depth exceeded genlock_backlog_relock_qdepth() (a stall's backlog landed / persistent inflow>presentation) and the cadence jumped to the newest due frame (catch-up keeps the IMAG latency contract). v1 counted wall-drift re-locks instead; the wall-based guard embedded the constant stamp->arrival skew and relock-stormed live (2026-07-02 canary: dropped_due 2918/4202, relocks 1076). Mirror of CadenceOutcome::relocked. */
	uint32_t genlock_converge_sheds;   /* camera-box #1049: cumulative SETTLE-BACK PHASE-CONVERGENCE sheds (genlock_should_converge_phase fired one extra drop to pull a per-camera acquire-phase back toward the configured latency). A converge shed ALSO counts into genlock_dropped_due; this distinguishes it so post-deploy verification can see the shed fire AND go QUIET once the phase converged (the genlock-hold-collapse playbook's "log silence lies" lesson). Printed on the 5s audit line (converge_sheds=). Zeroed at create (bzalloc). Decision authority: src/genlock_backlog.rs should_converge_phase. */
	uint64_t genlock_late_holds;       /* camera-box #401: holds because the matured boundary's frame has NOT ARRIVED (late/lost upstream) — distinct from the benign not-yet-due genlock_holds, so the audit separates \"source early\" from \"source late\". Mirror of CadenceOutcome::late_hold. */
	uint32_t genlock_last_known_n;     /* camera-box #726 STICKY-N: the last CONFIRMED integer source-rate multiple (0 = none yet). The STEADY per-tick front-2 measurement (genlock_measure_source_multiple) reads INCONCLUSIVE whenever async_frames.num < 2 OR the front pair is non-monotonic (a DanteSync clock-step seam / out-of-order arrival) — on a jittery 60-into-30 input a SUSTAINED run of inconclusive detections dropped the release back to the present-oldest CRAWL, under-drained the queue and backlog-stormed (win5/win6 / 'NDI cam5'->CAM1 live, 2026-07-13: relocks climbing ~2/s while sibling inputs stayed flat). This latch bridges an inconclusive tick with the last confirmed N instead of crawling; a fresh measurement is the confirmation authority and updates it (a genuine 1:1 rate re-latches to 1 -> byte-identical), and it is CLEARED on acquire/relock/gap/backward-step so a stale N cannot outlive its rate. Zeroed at create (bzalloc, like the counters). Mirror of src/probe/genlock.rs ReleaseCadence::last_known_n. */
	uint32_t genlock_peak_depth;       /* high-water async_frames.num seen */
	uint64_t genlock_ticks_since_drain; /* camera-box #859 follow-up: render ticks since the last SLEW-LIMITED SETTLE-BACK DRAIN fired (genlock_should_drain_one()). The #859 latency-relative backlog threshold stopped the backlog-relock branch firing every tick in steady state, but that branch was ALSO the FIFO's only mechanism for shedding excess queue depth after a genlock latency SETPOINT INCREASE — with it gated off, the plain N==1 steady release (one frame per tick) held depth CONSTANT forever (measured: a +34 ms setpoint step produced +134 ms of actual delay, stable across 6 samples). This counter rate-limits an ADDITIONAL bounded drain to at most one extra frame per GENLOCK_DRAIN_MIN_TICK_INTERVAL ticks — never a replacement for the backlog-relock branch. Reset to 0 whenever a drain fires; incremented every other steady N==1 tick. Zeroed at create (bzalloc, like the counters). Mirror of src/probe/genlock.rs ReleaseCadence::ticks_since_last_drain / src/genlock_backlog.rs DRAIN_MIN_TICK_INTERVAL (Tier-0 unit-tested). */
	uint32_t genlock_acquire_bracket_ticks; /* camera-box #1161: consecutive ACQUIRE ticks the STAGE-2 bracketing gate (genlock_relock_acquire_should_hold) has HELD in the CURRENT re-acquire episode, so the fail-open cap can bound the hold. A per-source latency INCREASE forces a re-acquire (obs_source_set_genlock_latency_ms zeroes genlock_locked_next_boundary_ns on a RISE); the ACQUIRE branch then holds — presenting nothing while the FIFO deepens to the raised reserve — until the OLDEST queued frame has aged to the target, so the existing genlock_relock_select_nearest locks AT the raised depth instead of one canvas frame below it (the #1161 residual). Incremented on each holding ACQUIRE tick, reset to 0 on any non-holding ACQUIRE tick; and — like genlock_phase_anchor_ns — zeroed at EVERY boundary-invalidation seam that starts a fresh ACQUIRE episode (the pin-rise re-acquire in obs_source_set_genlock_latency_ms, the three free_async_cache sites, and genlock_backward_regime_end), so a stale count from a prior episode can never undercut the next re-acquire's fail-open cap. Zeroed at create (bzalloc). Decision authority: src/genlock_backlog.rs relock_acquire_should_hold / ACQUIRE_BRACKET_FAILOPEN_TICKS (Tier-0 unit-tested). */
	uint64_t genlock_last_log_ns;      /* last periodic audit-log wall stamp */
	/* camera-box #148: last ts-align decision, SAMPLED per tick for the 5s audit line (the
	 * blog() stays 5s-gated; only these cheap field writes are per-tick) — so a future
	 * ts-align regression (clock skew, wrong interval, drift) is debuggable from the log.
	 * #269 [5]: written fresh ONLY on a ts-align tick; genlock_clear_ts_sample() resets all
	 * three to 0 (the sentinel) on every count-gate / true-empty tick, so the audit never
	 * prints a STALE sample from an earlier ts-align tick. 0 ⇒ "no ts-align sample this tick". */
	uint64_t genlock_last_present_ts;  /* most recent ts-align presentation deadline (ns); 0 = not sampled this tick */
	uint32_t genlock_last_due;         /* most recent ts-align due-frame count; 0 = not sampled this tick */
	int64_t genlock_last_head_skew_ns; /* most recent (wall_now - head frame->timestamp) skew (ns); 0 = not sampled this tick */
	struct obs_source_frame *async_preload_frame;
	DARRAY(struct async_frame) async_cache;
	DARRAY(struct obs_source_frame *) async_frames;
	pthread_mutex_t async_mutex;
	uint32_t async_width;
	uint32_t async_height;
	uint32_t async_cache_width;
	uint32_t async_cache_height;
	uint32_t async_convert_width[MAX_AV_PLANES];
	uint32_t async_convert_height[MAX_AV_PLANES];
	uint64_t async_last_rendered_ts;

	pthread_mutex_t caption_cb_mutex;
	DARRAY(struct caption_cb_info) caption_cb_list;

	/* async video deinterlacing */
	uint64_t deinterlace_offset;
	uint64_t deinterlace_frame_ts;
	gs_effect_t *deinterlace_effect;
	struct obs_source_frame *prev_async_frame;
	gs_texture_t *async_prev_textures[MAX_AV_PLANES];
	gs_texrender_t *async_prev_texrender;
	uint32_t deinterlace_half_duration;
	enum obs_deinterlace_mode deinterlace_mode;
	bool deinterlace_top_first;
	bool deinterlace_rendered;

	/* filters */
	struct obs_source *filter_parent;
	struct obs_source *filter_target;
	DARRAY(struct obs_source *) filters;
	pthread_mutex_t filter_mutex;
	gs_texrender_t *filter_texrender;
	enum obs_allow_direct_render allow_direct;
	bool rendering_filter;
	bool filter_bypass_active;

	/* sources specific hotkeys */
	obs_hotkey_pair_id mute_unmute_key;
	obs_hotkey_id push_to_mute_key;
	obs_hotkey_id push_to_talk_key;
	bool push_to_mute_enabled;
	bool push_to_mute_pressed;
	bool user_push_to_mute_pressed;
	bool push_to_talk_enabled;
	bool push_to_talk_pressed;
	bool user_push_to_talk_pressed;
	uint64_t push_to_mute_delay;
	uint64_t push_to_mute_stop_time;
	uint64_t push_to_talk_delay;
	uint64_t push_to_talk_stop_time;

	/* transitions */
	uint64_t transition_start_time;
	uint64_t transition_duration;
	pthread_mutex_t transition_tex_mutex;
	gs_texrender_t *transition_texrender[2];
	pthread_mutex_t transition_mutex;
	obs_source_t *transition_sources[2];
	float transition_manual_clamp;
	float transition_manual_torque;
	float transition_manual_target;
	float transition_manual_val;
	bool transitioning_video;
	bool transitioning_audio;
	bool transition_source_active[2];
	uint32_t transition_alignment;
	uint32_t transition_actual_cx;
	uint32_t transition_actual_cy;
	uint32_t transition_cx;
	uint32_t transition_cy;
	uint32_t transition_fixed_duration;
	bool transition_use_fixed_duration;
	enum obs_transition_mode transition_mode;
	enum obs_transition_scale_type transition_scale_type;
	struct matrix4 transition_matrices[2];

	/* color space */
	gs_texrender_t *color_space_texrender;

	/* audio monitoring */
	struct audio_monitor *monitor;
	enum obs_monitoring_type monitoring_type;

	/* media action queue */
	DARRAY(struct media_action) media_actions;
	pthread_mutex_t media_actions_mutex;

	/* private data */
	obs_data_t *private_settings;

	/* canvas this source belongs to (only used for scenes) */
	obs_weak_canvas_t *canvas;
};

extern struct obs_source_info *get_source_info(const char *id);
extern struct obs_source_info *get_source_info2(const char *unversioned_id, uint32_t ver);
extern bool obs_source_init_context(struct obs_source *source, obs_data_t *settings, const char *name, const char *uuid,
				    obs_data_t *hotkey_data, bool private);

extern bool obs_transition_init(obs_source_t *transition);
extern void obs_transition_free(obs_source_t *transition);
extern void obs_transition_tick(obs_source_t *transition, float t);
extern void obs_transition_enum_sources(obs_source_t *transition, obs_source_enum_proc_t enum_callback, void *param);
extern void obs_transition_save(obs_source_t *source, obs_data_t *data);
extern void obs_transition_load(obs_source_t *source, obs_data_t *data);

struct audio_monitor *audio_monitor_create(obs_source_t *source);
void audio_monitor_reset(struct audio_monitor *monitor);
extern void audio_monitor_destroy(struct audio_monitor *monitor);

extern obs_source_t *obs_source_create_canvas(obs_canvas_t *canvas, const char *id, const char *name,
					      obs_data_t *settings, obs_data_t *hotkey_data);
extern obs_source_t *obs_source_create_set_last_ver(obs_canvas_t *canvas, const char *id, const char *name,
						    const char *uuid, obs_data_t *settings, obs_data_t *hotkey_data,
						    uint32_t last_obs_ver, bool is_private);

extern void obs_source_destroy(struct obs_source *source);
extern void obs_source_addref(obs_source_t *source);

static inline void obs_source_dosignal(struct obs_source *source, const char *signal_obs, const char *signal_source)
{
	struct calldata data;
	uint8_t stack[128];

	calldata_init_fixed(&data, stack, sizeof(stack));
	calldata_set_ptr(&data, "source", source);
	if (signal_obs && !source->context.private)
		signal_handler_signal(obs->signals, signal_obs, &data);
	if (signal_source)
		signal_handler_signal(source->context.signals, signal_source, &data);
}

static inline void obs_source_dosignal_canvas(struct obs_source *source, struct obs_canvas *canvas,
					      const char *signal_obs, const char *signal_source)
{
	struct calldata data;
	uint8_t stack[128];

	calldata_init_fixed(&data, stack, sizeof(stack));
	calldata_set_ptr(&data, "source", source);
	calldata_set_ptr(&data, "canvas", canvas);
	if (signal_obs && !source->context.private)
		signal_handler_signal(obs->signals, signal_obs, &data);
	if (signal_source)
		signal_handler_signal(source->context.signals, signal_source, &data);
}

/* maximum timestamp variance in nanoseconds */
#define MAX_TS_VAR 2000000000ULL

static inline bool frame_out_of_bounds(const obs_source_t *source, uint64_t ts)
{
	if (ts < source->last_frame_ts)
		return ((source->last_frame_ts - ts) > MAX_TS_VAR);
	else
		return ((ts - source->last_frame_ts) > MAX_TS_VAR);
}

static inline enum gs_color_format convert_video_format(enum video_format format, enum video_trc trc)
{
	switch (trc) {
	case VIDEO_TRC_PQ:
	case VIDEO_TRC_HLG:
		return GS_RGBA16F;
	default:
		switch (format) {
		case VIDEO_FORMAT_RGBA:
			return GS_RGBA;
		case VIDEO_FORMAT_BGRA:
		case VIDEO_FORMAT_I40A:
		case VIDEO_FORMAT_I42A:
		case VIDEO_FORMAT_YUVA:
		case VIDEO_FORMAT_AYUV:
			return GS_BGRA;
		case VIDEO_FORMAT_I010:
		case VIDEO_FORMAT_P010:
		case VIDEO_FORMAT_I210:
		case VIDEO_FORMAT_I412:
		case VIDEO_FORMAT_YA2L:
		case VIDEO_FORMAT_P216:
		case VIDEO_FORMAT_P416:
		case VIDEO_FORMAT_V210:
		case VIDEO_FORMAT_R10L:
			return GS_RGBA16F;
		default:
			return GS_BGRX;
		}
	}
}

static inline enum gs_color_space convert_video_space(enum video_format format, enum video_trc trc)
{
	enum gs_color_space space = GS_CS_SRGB;
	if (convert_video_format(format, trc) == GS_RGBA16F) {
		switch (trc) {
		case VIDEO_TRC_DEFAULT:
		case VIDEO_TRC_SRGB:
			space = GS_CS_SRGB_16F;
			break;
		case VIDEO_TRC_PQ:
		case VIDEO_TRC_HLG:
			space = GS_CS_709_EXTENDED;
		}
	}

	return space;
}

extern void obs_source_set_texcoords_centered(obs_source_t *source, bool centered);
extern void obs_source_activate(obs_source_t *source, enum view_type type);
extern void obs_source_deactivate(obs_source_t *source, enum view_type type);
extern void obs_source_video_tick(obs_source_t *source, float seconds);
extern float obs_source_get_target_volume(obs_source_t *source, obs_source_t *target);
extern uint64_t obs_source_get_last_async_ts(const obs_source_t *source);

extern void obs_source_audio_render(obs_source_t *source, uint32_t mixers, size_t channels, size_t sample_rate,
				    size_t size);

extern void add_alignment(struct vec2 *v, uint32_t align, int cx, int cy);

extern struct obs_source_frame *filter_async_video(obs_source_t *source, struct obs_source_frame *in);
extern bool update_async_texture(struct obs_source *source, const struct obs_source_frame *frame, gs_texture_t *tex,
				 gs_texrender_t *texrender);
extern bool update_async_textures(struct obs_source *source, const struct obs_source_frame *frame,
				  gs_texture_t *tex[MAX_AV_PLANES], gs_texrender_t *texrender);
extern bool set_async_texture_size(struct obs_source *source, const struct obs_source_frame *frame);
extern void remove_async_frame(obs_source_t *source, struct obs_source_frame *frame);

extern void set_deinterlace_texture_size(obs_source_t *source);
extern void deinterlace_process_last_frame(obs_source_t *source, uint64_t sys_time);
extern void deinterlace_update_async_video(obs_source_t *source);
extern void deinterlace_render(obs_source_t *s);

/* ------------------------------------------------------------------------- */
/* outputs  */

enum delay_msg {
	DELAY_MSG_PACKET,
	DELAY_MSG_START,
	DELAY_MSG_STOP,
};

struct delay_data {
	enum delay_msg msg;
	uint64_t ts;
	struct encoder_packet packet;
	bool packet_time_valid;
	struct encoder_packet_time packet_time;
};

typedef void (*encoded_callback_t)(void *data, struct encoder_packet *packet, struct encoder_packet_time *frame_time);

struct obs_weak_output {
	struct obs_weak_ref ref;
	struct obs_output *output;
};

#define CAPTION_LINE_CHARS (32)
#define CAPTION_LINE_BYTES (4 * CAPTION_LINE_CHARS)
struct caption_text {
	char text[CAPTION_LINE_BYTES + 1];
	double display_duration;
	struct caption_text *next;
};

struct caption_track_data {
	struct caption_text *caption_head;
	struct caption_text *caption_tail;
	pthread_mutex_t caption_mutex;
	double caption_timestamp;
	double last_caption_timestamp;
	struct deque caption_data;
};

struct pause_data {
	pthread_mutex_t mutex;
	uint64_t last_video_ts;
	uint64_t ts_start;
	uint64_t ts_end;
	uint64_t ts_offset;
};

extern bool video_pause_check(struct pause_data *pause, uint64_t timestamp);
extern bool audio_pause_check(struct pause_data *pause, struct audio_data *data, size_t sample_rate);
extern void pause_reset(struct pause_data *pause);

enum keyframe_group_track_status {
	KEYFRAME_TRACK_STATUS_NOT_SEEN = 0,
	KEYFRAME_TRACK_STATUS_SEEN = 1,
	KEYFRAME_TRACK_STATUS_SKIPPED = 2,
};

struct keyframe_group_data {
	uintptr_t group_id;
	int64_t pts;
	uint32_t required_tracks;
	enum keyframe_group_track_status seen_on_track[MAX_OUTPUT_VIDEO_ENCODERS];
};

struct obs_output {
	struct obs_context_data context;
	struct obs_output_info info;

	/* indicates ownership of the info.id buffer */
	bool owns_info_id;

	bool received_video[MAX_OUTPUT_VIDEO_ENCODERS];
	DARRAY(struct keyframe_group_data) keyframe_group_tracking;
	bool received_audio;
	volatile bool data_active;
	volatile bool end_data_capture_thread_active;
	int64_t video_offsets[MAX_OUTPUT_VIDEO_ENCODERS];
	int64_t audio_offsets[MAX_OUTPUT_AUDIO_ENCODERS];
	int64_t highest_audio_ts;
	int64_t highest_video_ts[MAX_OUTPUT_VIDEO_ENCODERS];
	pthread_t end_data_capture_thread;
	os_event_t *stopping_event;
	pthread_mutex_t interleaved_mutex;
	DARRAY(struct encoder_packet) interleaved_packets;
	size_t interleaver_max_batch_size;
	int stop_code;

	int reconnect_retry_sec;
	int reconnect_retry_max;
	int reconnect_retries;
	uint32_t reconnect_retry_cur_msec;
	float reconnect_retry_exp;
	pthread_t reconnect_thread;
	os_event_t *reconnect_stop_event;
	volatile bool reconnecting;
	volatile bool reconnect_thread_active;

	uint32_t starting_drawn_count;
	uint32_t starting_lagged_count;

	int total_frames;

	volatile bool active;
	volatile bool paused;
	video_t *video;
	audio_t *audio;
	obs_encoder_t *video_encoders[MAX_OUTPUT_VIDEO_ENCODERS];
	obs_encoder_t *audio_encoders[MAX_OUTPUT_AUDIO_ENCODERS];
	obs_service_t *service;
	size_t mixer_mask;

	struct pause_data pause;

	struct deque audio_buffer[MAX_AUDIO_MIXES][MAX_AV_PLANES];
	uint64_t audio_start_ts;
	uint64_t video_start_ts;
	size_t audio_size;
	size_t planes;
	size_t sample_rate;
	size_t total_audio_frames;

	uint32_t scaled_width;
	uint32_t scaled_height;

	bool video_conversion_set;
	bool audio_conversion_set;
	struct video_scale_info video_conversion;
	struct audio_convert_info audio_conversion;

	// captions are output per track
	struct caption_track_data *caption_tracks[MAX_OUTPUT_VIDEO_ENCODERS];

	DARRAY(struct encoder_packet_time)
	encoder_packet_times[MAX_OUTPUT_VIDEO_ENCODERS];

	/* Packet callbacks */
	pthread_mutex_t pkt_callbacks_mutex;
	DARRAY(struct packet_callback) pkt_callbacks;

	struct reconnect_callback reconnect_callback;

	bool valid;

	uint64_t active_delay_ns;
	encoded_callback_t delay_callback;
	struct deque delay_data; /* struct delay_data */
	pthread_mutex_t delay_mutex;
	uint32_t delay_sec;
	uint32_t delay_flags;
	uint32_t delay_cur_flags;
	volatile long delay_restart_refs;
	volatile bool delay_active;
	volatile bool delay_capturing;

	char *last_error_message;

	float audio_data[MAX_AUDIO_CHANNELS][AUDIO_OUTPUT_FRAMES];
};

static inline void do_output_signal(struct obs_output *output, const char *signal)
{
	struct calldata params = {0};
	calldata_set_ptr(&params, "output", output);
	signal_handler_signal(output->context.signals, signal, &params);
	calldata_free(&params);
}

extern void process_delay(void *data, struct encoder_packet *packet, struct encoder_packet_time *packet_time);
extern void obs_output_cleanup_delay(obs_output_t *output);
extern bool obs_output_delay_start(obs_output_t *output);
extern void obs_output_delay_stop(obs_output_t *output);
extern bool obs_output_actual_start(obs_output_t *output);
extern void obs_output_actual_stop(obs_output_t *output, bool force, uint64_t ts);

extern const struct obs_output_info *find_output(const char *id);

extern void obs_output_remove_encoder(struct obs_output *output, struct obs_encoder *encoder);

extern void obs_encoder_packet_create_instance(struct encoder_packet *dst, const struct encoder_packet *src);
void obs_output_destroy(obs_output_t *output);

/* ------------------------------------------------------------------------- */
/* encoders  */

struct obs_weak_encoder {
	struct obs_weak_ref ref;
	struct obs_encoder *encoder;
};

struct encoder_callback {
	bool sent_first_packet;
	encoded_callback_t new_packet;
	void *param;
};

struct obs_encoder_group {
	pthread_mutex_t mutex;
	/* allows group to be destroyed even if some encoders are active */
	bool destroy_on_stop;

	/* holds strong references to all encoders */
	DARRAY(struct obs_encoder *) encoders;

	uint32_t num_encoders_started;
	uint64_t start_timestamp;

	uint32_t frame_rate_divisors_lcm;

	uint64_t reconfigure_request;
	int64_t next_pts;
	uint32_t encoders_updated_next_pts;
	uint32_t encoders_reconfigured;
	bool reconfigure_again;
};

struct obs_encoder {
	struct obs_context_data context;
	struct obs_encoder_info info;

	/* allows re-routing to another encoder */
	struct obs_encoder_info orig_info;

	pthread_mutex_t init_mutex;

	uint32_t samplerate;
	size_t planes;
	size_t blocksize;
	size_t framesize;
	size_t framesize_bytes;

	size_t mixer_idx;

	/* OBS_SCALE_DISABLE indicates GPU scaling is disabled */
	enum obs_scale_type gpu_scale_type;

	uint32_t scaled_width;
	uint32_t scaled_height;
	enum video_format preferred_format;
	enum video_colorspace preferred_space;
	enum video_range_type preferred_range;

	volatile bool active;
	volatile bool paused;
	bool initialized;

	/* indicates ownership of the info.id buffer */
	bool owns_info_id;

	uint32_t timebase_num;
	uint32_t timebase_den;

	// allow outputting at fractions of main composition FPS,
	// e.g. 60 FPS with frame_rate_divisor = 1 turns into 30 FPS
	//
	// a separate counter is used in favor of using remainder calculations
	// to allow "inputs" started at the same time to start on the same frame
	// whereas with remainder calculation the frame alignment would depend on
	// the total frame count at the time the encoder was started
	uint32_t frame_rate_divisor;
	uint32_t frame_rate_divisor_counter; // only used for GPU encoders
	video_t *fps_override;

	// Number of frames successfully encoded
	uint32_t encoded_frames;

	/* Regions of interest to prioritize during encoding */
	pthread_mutex_t roi_mutex;
	DARRAY(struct obs_encoder_roi) roi;
	uint32_t roi_increment;

	int64_t cur_pts;

	struct deque audio_input_buffer[MAX_AV_PLANES];
	uint8_t *audio_output_buffer[MAX_AV_PLANES];

	/* if a video encoder is paired with an audio encoder, make it start
	 * up at the specific timestamp.  if this is the audio encoder,
	 * it waits until it's ready to sync up with video */
	bool first_received;
	DARRAY(struct obs_weak_encoder *) paired_encoders;
	int64_t offset_usec;
	uint64_t first_raw_ts;
	uint64_t start_ts;

	/* track encoders that are part of a gop-aligned multi track group */
	struct obs_encoder_group *encoder_group;
	uint64_t last_reconfigure_request;
	uint64_t last_handled_reconfigure_request;

	pthread_mutex_t outputs_mutex;
	DARRAY(obs_output_t *) outputs;

	/* stores the video/audio media output pointer.  video_t *or audio_t **/
	void *media;
	/* Stores the original video if GPU scaling is enabled and `media` can be overwritten. */
	video_t *original_video;

	pthread_mutex_t callbacks_mutex;
	DARRAY(struct encoder_callback) callbacks;

	DARRAY(struct encoder_packet_time) encoder_packet_times;

	struct pause_data pause;

	const char *profile_encoder_encode_name;
	char *last_error_message;

	/* reconfigure encoder at next possible opportunity */
	bool reconfigure_requested;
};

extern struct obs_encoder_info *find_encoder(const char *id);

extern bool obs_encoder_initialize(obs_encoder_t *encoder);
extern void obs_encoder_shutdown(obs_encoder_t *encoder);

extern void obs_encoder_start(obs_encoder_t *encoder, encoded_callback_t new_packet, void *param);
extern void obs_encoder_stop(obs_encoder_t *encoder, encoded_callback_t new_packet, void *param);

extern void obs_encoder_add_output(struct obs_encoder *encoder, struct obs_output *output);
extern void obs_encoder_remove_output(struct obs_encoder *encoder, struct obs_output *output);

extern bool start_gpu_encode(obs_encoder_t *encoder);
extern void stop_gpu_encode(obs_encoder_t *encoder);

extern bool do_encode(struct obs_encoder *encoder, struct encoder_frame *frame, const uint64_t *frame_cts);
extern void send_off_encoder_packet(obs_encoder_t *encoder, bool success, bool received, struct encoder_packet *pkt);

void obs_encoder_destroy(obs_encoder_t *encoder);

/* ------------------------------------------------------------------------- */
/* services */

struct obs_weak_service {
	struct obs_weak_ref ref;
	struct obs_service *service;
};

struct obs_service {
	struct obs_context_data context;
	struct obs_service_info info;

	/* indicates ownership of the info.id buffer */
	bool owns_info_id;

	bool active;
	bool destroy;
	struct obs_output *output;
};

extern const struct obs_service_info *find_service(const char *id);

extern void obs_service_activate(struct obs_service *service);
extern void obs_service_deactivate(struct obs_service *service, bool remove);
extern bool obs_service_initialize(struct obs_service *service, struct obs_output *output);

void obs_service_destroy(obs_service_t *service);

void obs_output_remove_encoder_internal(struct obs_output *output, struct obs_encoder *encoder);

/** Internal Source Profiler functions **/

/* Start of frame in graphics loop */
extern void source_profiler_frame_begin(void);
/* Process data collected during frame */
extern void source_profiler_frame_collect(void);

/* Start/end of outputs being rendered (GPU timer begin/end) */
extern void source_profiler_render_begin(void);
extern void source_profiler_render_end(void);

/* Reset settings, buffers, and GPU timers when video settings change */
extern void source_profiler_reset_video(struct obs_video_info *ovi);

/* Signal that source received an async frame */
extern void source_profiler_async_frame_received(obs_source_t *source);

/* Get timestamp for start of tick */
extern uint64_t source_profiler_source_tick_start(void);
/* Submit start timestamp for source */
extern void source_profiler_source_tick_end(obs_source_t *source, uint64_t start);

/* Obtain GPU timer and start timestamp for render start of a source. */
extern uint64_t source_profiler_source_render_begin(gs_timer_t **timer);
/* Submit start timestamp and GPU timer after rendering source */
extern void source_profiler_source_render_end(obs_source_t *source, uint64_t start, gs_timer_t *timer);

/* Remove source from profiler hashmaps */
extern void source_profiler_remove_source(obs_source_t *source);
