/******************************************************************************
	Copyright (C) 2016-2024 DistroAV <contact@distroav.org>

	This program is free software; you can redistribute it and/or
	modify it under the terms of the GNU General Public License
	as published by the Free Software Foundation; either version 2
	of the License, or (at your option) any later version.

	This program is distributed in the hope that it will be useful,
	but WITHOUT ANY WARRANTY; without even the implied warranty of
	MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
	GNU General Public License for more details.

	You should have received a copy of the GNU General Public License
	along with this program; if not, see <https://www.gnu.org/licenses/>.
******************************************************************************/

#include "plugin-main.h"

#include <util/platform.h>
#include <util/threading.h>
#include <media-io/video-frame.h>
#include <chrono>
#include <cstdint>

#include <QDesktopServices>
#include <QUrl>

#define TEXFORMAT GS_BGRA
#define FLT_PROP_NAME "ndi_filter_ndiname"
#define FLT_PROP_GROUPS "ndi_filter_ndigroups"

typedef struct {
	obs_source_t *obs_source;

	NDIlib_send_instance_t ndi_sender;

	pthread_mutex_t ndi_sender_video_mutex;
	pthread_mutex_t ndi_sender_audio_mutex;

	obs_video_info ovi;
	obs_audio_info oai;

	uint32_t known_width;
	uint32_t known_height;
	bool rendered;

	gs_texrender_t *texrender;
	gs_stagesurf_t *stagesurface;
	uint8_t *video_data;
	uint32_t video_linesize;

	video_t *video_output;
	bool is_audioonly;

	uint8_t *audio_conv_buffer;
	size_t audio_conv_buffer_size;
	int32_t no_video_connections;
	std::chrono::time_point<std::chrono::steady_clock> last_video_conn_check;
	int32_t no_audio_connections;
	std::chrono::time_point<std::chrono::steady_clock> last_audio_conn_check;

	// camera-box #874: NDI FILTER-side send-path audit -- the aux-sender
	// counterpart of ndi-output.cpp's audit_* fields (see there for the full
	// rationale). `ndi_filter` is the send path for the DEDICATED NDI OUTPUT
	// filters (interkom, MULTIVIEW, Grading, ...), which the #874 output-only
	// audit does not cover -- so a stall on one of these aux senders (the
	// issue-707 interkom culprit) was previously invisible in the log.
	// `audit_offered`/`audit_sent`/`audit_send_wait_ns`/`audit_max_send_wait_ns`
	// mirror ndi-output.cpp's fields exactly (same meaning, same cumulative-
	// since-creation semantics, never reset except at filter creation).
	// `audit_mutex_wait_ns`/`audit_max_mutex_wait_ns` are UNIQUE to this file:
	// they isolate time spent ACQUIRING ndi_sender_video_mutex from time spent
	// INSIDE send_send_video_v2 -- a high mutex wait with a low send wait means
	// this sender is queued behind another lock holder, not slow on its own
	// send; that distinction decides the shape of the issue-707 fix.
	uint64_t audit_offered;
	uint64_t audit_sent;
	uint64_t audit_send_wait_ns;
	uint64_t audit_max_send_wait_ns;
	uint64_t audit_mutex_wait_ns;
	uint64_t audit_max_mutex_wait_ns;
	std::chrono::time_point<std::chrono::steady_clock> audit_last_log;

	// camera-box #879: aux-sender ADAPTIVE render-budget gate. An ndi_filter is a
	// throttleable monitoring/aux NDI republish (interkom / MULTIVIEW / Grading on strih) --
	// NEVER the program output (that is ndi_output, a separate path). Under graphics-thread
	// budget pressure this sender skips its VIDEO render + send this tick so the program
	// render (which runs first, in output_frames()) always has ABSOLUTE priority; the audio
	// path (ndi_filter_asyncaudio, interkom talkback) is never throttled. Counters mirror
	// obs-display.c's per-display fields exactly; the decision is the pure
	// obs_aux_sender_should_skip() (obs.c -> obs_display_should_skip, #278/#293/#756/#776).
	uint32_t render_divisor;           // throttleable marker + cadence upper bound (default 2)
	uint32_t render_frame_counter;     // per-instance tick counter (#756 cadence-floor input)
	uint64_t render_ewma_ns;           // EWMA (alpha=1/4) of this filter's graphics-thread render cost
	uint32_t render_consecutive_skips; // #293 anti-starvation: renders at least every K+1 ticks
} ndi_filter_t;

const char *ndi_filter_getname(void *)
{
	return obs_module_text("NDIPlugin.FilterName");
}

const char *ndi_audiofilter_getname(void *)
{
	return obs_module_text("NDIPlugin.AudioFilterName");
}

void ndi_filter_update(void *data, obs_data_t *settings);
void ndi_sender_destroy(ndi_filter_t *filter);
void ndi_sender_create(ndi_filter_t *filter, obs_data_t *settings);

// Callback fired when the parent source is renamed. Recreate the NDI sender to pick up the new name.
void on_renamed(void *data, calldata_t *)
{
	auto f = (ndi_filter_t *)data;
	if (!f)
		return;
	obs_log(LOG_DEBUG, "on_parent_renamed: parent source renamed; recreating NDI sender for filter '%s'",
		obs_source_get_name(f->obs_source));
	// Recreate sender using current settings
	ndi_sender_create(f, nullptr);
}

static void ndi_filter_disconnect_rename_handlers(ndi_filter_t *filter)
{
	if (!filter || !filter->obs_source) {
		return;
	}

	obs_source_t *parent = obs_filter_get_parent(filter->obs_source);
	if (parent) {
		signal_handler_t *parent_sh = obs_source_get_signal_handler(parent);
		if (parent_sh) {
			signal_handler_disconnect(parent_sh, "rename", on_renamed, filter);
		}
	}

	signal_handler_t *filter_sh = obs_source_get_signal_handler(filter->obs_source);
	if (filter_sh) {
		signal_handler_disconnect(filter_sh, "rename", on_renamed, filter);
	}
}

obs_properties_t *ndi_filter_getproperties(void *)
{
	obs_log(LOG_DEBUG, "+ndi_filter_getproperties(...)");
	obs_properties_t *props = obs_properties_create();
	obs_properties_set_flags(props, OBS_PROPERTIES_DEFER_UPDATE);

	obs_property_t *ndi_name_property = obs_properties_add_text(
		props, FLT_PROP_NAME, obs_module_text("NDIPlugin.FilterProps.NDIName"), OBS_TEXT_DEFAULT);
	obs_property_set_long_description(ndi_name_property,
					  obs_module_text("NDIPlugin.FilterProps.NDIName.Description"));

	obs_properties_add_text(props, FLT_PROP_GROUPS, obs_module_text("NDIPlugin.FilterProps.NDIGroups"),
				OBS_TEXT_DEFAULT);

	obs_properties_add_button(props, "ndi_apply", obs_module_text("NDIPlugin.FilterProps.ApplySettings"),
				  [](obs_properties_t *, obs_property_t *, void *private_data) {
					  auto s = (ndi_filter_t *)private_data;
					  auto settings = obs_source_get_settings(s->obs_source);
					  ndi_filter_update(s, settings);
					  obs_data_release(settings);
					  return true;
				  });

	obs_log(LOG_DEBUG, "-ndi_filter_getproperties(...)");
	return props;
}

void ndi_filter_getdefaults(obs_data_t *defaults)
{
	obs_log(LOG_DEBUG, "+ndi_filter_getdefaults(...)");
	obs_data_set_default_string(defaults, FLT_PROP_NAME, obs_module_text("NDIPlugin.FilterProps.NDIName.Default"));
	obs_data_set_default_string(defaults, FLT_PROP_GROUPS, "");
	obs_log(LOG_DEBUG, "-ndi_filter_getdefaults(...)");
}

bool is_filter_valid(ndi_filter_t *filter)
{
	obs_source_t *target = obs_filter_get_target(filter->obs_source);
	obs_source_t *parent = obs_filter_get_parent(filter->obs_source);
	if (!target || !parent) {
		return false;
	}

	uint32_t width = obs_source_get_width(filter->obs_source);
	uint32_t height = obs_source_get_height(filter->obs_source);

	// Valid if parent width/height are nonzero, source is enabled, and parent is showing somewhere in OBS's windows
	bool is_valid = (width != 0) && (height != 0) && obs_source_enabled(filter->obs_source) &&
			obs_source_showing(parent);

	return is_valid;
}

void ndi_filter_raw_video(void *data, video_data *frame)
{
	auto f = (ndi_filter_t *)data;

	// camera-box #874: count EVERY entry -- a frame handed to this filter's
	// send path -- before any guards, mirroring ndi-output.cpp's audit_offered
	// so a frame that never reaches the send call still shows up as a gap
	// between offered and sent.
	f->audit_offered++;

	pthread_mutex_lock(&f->ndi_sender_video_mutex);

	if (!f->ndi_sender) {
		pthread_mutex_unlock(&f->ndi_sender_video_mutex);
		return;
	}

	auto now = std::chrono::steady_clock::now();
	if (now - f->last_video_conn_check >= std::chrono::seconds(1)) {
		f->last_video_conn_check = now;
		int nc = ndiLib->send_get_no_connections(f->ndi_sender, 10);
		if (nc != f->no_video_connections) {
			auto ndi_source = ndiLib->send_get_source_name(f->ndi_sender);
			if (nc <= 0)
				obs_log(LOG_DEBUG, "Dedicated NDI Output video '%s' has no connections",
					ndi_source->p_ndi_name);
			else if (f->no_video_connections == 0)
				obs_log(LOG_DEBUG, "Dedicated NDI Output video '%s' has %d connections",
					ndi_source->p_ndi_name, nc);
			f->no_video_connections = nc;
		}
	}

	pthread_mutex_unlock(&f->ndi_sender_video_mutex);

	NDIlib_video_frame_v2_t video_frame = {0};

	// Only fill out the data if we have a valid frame
	if (frame && frame->data[0]) {
		video_frame.xres = f->known_width;
		video_frame.yres = f->known_height;
		video_frame.FourCC = NDIlib_FourCC_type_BGRA;
		video_frame.frame_rate_N = f->ovi.fps_num;
		video_frame.frame_rate_D = f->ovi.fps_den;
		video_frame.picture_aspect_ratio = 0; // square pixels
		video_frame.frame_format_type = NDIlib_frame_format_type_progressive;
		video_frame.timecode = NDIlib_send_timecode_synthesize;
		video_frame.p_data = frame->data[0];
		video_frame.line_stride_in_bytes = frame->linesize[0];
	}

	// camera-box #874: time acquiring the send mutex separately from the send
	// call itself -- timestamps only around the EXISTING lock, no new lock and
	// no widening of what it protects. A high mutex_wait with a low send_wait
	// means this sender is queued behind another lock holder, not slow on its
	// own send -- that distinction decides the shape of the issue-707 fix.
	auto mutex_wait_start = std::chrono::steady_clock::now();
	pthread_mutex_lock(&f->ndi_sender_video_mutex);
	auto mutex_wait_end = std::chrono::steady_clock::now();
	uint64_t mutex_wait_ns =
		(uint64_t)std::chrono::duration_cast<std::chrono::nanoseconds>(mutex_wait_end - mutex_wait_start)
			.count();
	f->audit_mutex_wait_ns += mutex_wait_ns;
	if (mutex_wait_ns > f->audit_max_mutex_wait_ns)
		f->audit_max_mutex_wait_ns = mutex_wait_ns;

	if (f->ndi_sender) {
		// camera-box #874: time the synchronous send call itself -- this is
		// the load-bearing number. send_send_video_v2 blocks until the frame
		// is accepted, so a large cumulative/peak wait here proves THIS
		// sender is stalling on its own send (e.g. interkom under load,
		// issue 707), independent of any queueing on the mutex above.
		auto send_start = std::chrono::steady_clock::now();
		ndiLib->send_send_video_v2(f->ndi_sender, &video_frame);
		auto send_end = std::chrono::steady_clock::now();

		f->audit_sent++;
		uint64_t send_wait_ns =
			(uint64_t)std::chrono::duration_cast<std::chrono::nanoseconds>(send_end - send_start).count();
		f->audit_send_wait_ns += send_wait_ns;
		if (send_wait_ns > f->audit_max_send_wait_ns)
			f->audit_max_send_wait_ns = send_wait_ns;

		// One audit line per filter every ~5s, mirroring ndi-output.cpp's
		// genlock-ndi-output audit cadence and guard shape -- the first call
		// only seeds audit_last_log so the very first line waits a full
		// interval rather than firing immediately. Logged here (sender known
		// non-null, lock still held) so send_get_source_name is safe to call,
		// matching the connection-check log earlier in this same function.
		if (f->audit_last_log == std::chrono::steady_clock::time_point())
			f->audit_last_log = send_end;
		if (send_end - f->audit_last_log >= std::chrono::seconds(5)) {
			f->audit_last_log = send_end;
			auto ndi_source = ndiLib->send_get_source_name(f->ndi_sender);
			obs_log(LOG_INFO,
				"genlock-ndi-filter audit '%s': offered=%llu sent=%llu "
				"send_wait_ms=%.3f max_send_wait_ms=%.3f mutex_wait_ms=%.3f "
				"max_mutex_wait_ms=%.3f (#874)",
				ndi_source->p_ndi_name, (unsigned long long)f->audit_offered,
				(unsigned long long)f->audit_sent, (double)f->audit_send_wait_ns / 1.0e6,
				(double)f->audit_max_send_wait_ns / 1.0e6, (double)f->audit_mutex_wait_ns / 1.0e6,
				(double)f->audit_max_mutex_wait_ns / 1.0e6);
		}
	}
	pthread_mutex_unlock(&f->ndi_sender_video_mutex);
}

void ndi_filter_render_video(void *data, gs_effect_t *)
{
	auto f = (ndi_filter_t *)data;
	obs_source_skip_video_filter(f->obs_source);

	if (f->rendered)
		return;

	obs_source_t *target = obs_filter_get_target(f->obs_source);
	obs_source_t *parent = obs_filter_get_parent(f->obs_source);

	if (!target || !parent) {
		return;
	}

	if (!is_filter_valid(f)) {
		// Send over an empty frame to indicate that the filter is invalid
		ndi_filter_raw_video(data, nullptr);
		return;
	}

	// camera-box #879: adaptive render-budget gate. Count this candidate tick, then ask the
	// pure libobs decision whether this THROTTLEABLE aux sender must yield to the program
	// this tick. Skipping produces no new staged frame -> ndi_filter_raw_video sends nothing
	// this tick (a genuine cadence reduction), so the program (ndi_output, rendered first in
	// output_frames()) keeps its full budget. #293 caps consecutive skips so it never freezes;
	// render_ewma_ns == 0 (not yet warmed) always renders once to measure. Program priority is
	// STRUCTURAL: the program is a different source type never routed through this gate.
	f->render_frame_counter++;
	if (obs_aux_sender_should_skip(f->render_divisor, f->render_frame_counter, f->render_ewma_ns,
				       f->render_consecutive_skips)) {
		f->render_consecutive_skips++;
		f->rendered = true; // do not retry this tick; ndi_filter_tick resets it next tick
		return;
	}

	// Time this filter's graphics-thread render cost so the budget gate above can predict
	// the next tick (#278 EWMA, alpha=1/4). A real render clears the #293 skip run.
	const uint64_t render_begin_879 = os_gettime_ns();

	uint32_t width = obs_source_get_width(f->obs_source);
	uint32_t height = obs_source_get_height(f->obs_source);

	if (f->known_width != width || f->known_height != height) {
		gs_stagesurface_destroy(f->stagesurface);
		f->stagesurface = gs_stagesurface_create(width, height, TEXFORMAT);

		video_output_info vi = {0};
		vi.format = VIDEO_FORMAT_BGRA;
		vi.width = width;
		vi.height = height;
		vi.fps_den = f->ovi.fps_den;
		vi.fps_num = f->ovi.fps_num;
		vi.cache_size = 16;
		vi.colorspace = VIDEO_CS_DEFAULT;
		vi.range = VIDEO_RANGE_DEFAULT;
		vi.name = obs_source_get_name(f->obs_source);

		video_output_close(f->video_output);
		video_output_open(&f->video_output, &vi);
		video_output_connect(f->video_output, nullptr, ndi_filter_raw_video, f);

		f->known_width = width;
		f->known_height = height;
	}

	gs_texrender_reset(f->texrender);

	if (gs_texrender_begin(f->texrender, width, height)) {
		vec4 background;
		vec4_zero(&background);

		gs_clear(GS_CLEAR_COLOR, &background, 0.0f, 0);
		gs_ortho(0.0f, (float)width, 0.0f, (float)height, -100.0f, 100.0f);

		gs_blend_state_push();
		gs_blend_function(GS_BLEND_ONE, GS_BLEND_ZERO);

		if (target == parent) {
			obs_source_skip_video_filter(f->obs_source);
		} else {
			obs_source_video_render(target);
		}

		gs_blend_state_pop();
		gs_texrender_end(f->texrender);

		gs_stage_texture(f->stagesurface, gs_texrender_get_texture(f->texrender));
		if (gs_stagesurface_map(f->stagesurface, &f->video_data, &f->video_linesize)) {
			video_frame output_frame;
			if (video_output_lock_frame(f->video_output, &output_frame, 1, os_gettime_ns())) {
				uint32_t linesize = output_frame.linesize[0];
				for (uint32_t i = 0; i < f->known_height; ++i) {
					uint32_t dst_offset = linesize * i;
					uint32_t src_offset = f->video_linesize * i;
					memcpy(output_frame.data[0] + dst_offset, f->video_data + src_offset, linesize);
				}

				video_output_unlock_frame(f->video_output);
			}

			gs_stagesurface_unmap(f->stagesurface);
		}

		// camera-box #879: update this sender's render-cost EWMA and clear the #293 skip
		// run -- only when a render actually ran (gs_texrender_begin succeeded), mirroring
		// render_display()'s `if (render_display_begin(...))` timing so a begin failure does
		// not drag the EWMA down or reset the skip run without a produced frame.
		const uint64_t render_dur_879 = os_gettime_ns() - render_begin_879;
		f->render_ewma_ns = f->render_ewma_ns ? (f->render_ewma_ns * 3 + render_dur_879) / 4 : render_dur_879;
		f->render_consecutive_skips = 0;
	}

	f->rendered = true;
}

void ndi_sender_destroy(ndi_filter_t *filter)
{
	if (!filter || !filter->ndi_sender) {
		return;
	}

	if (!filter->is_audioonly) {
		pthread_mutex_lock(&filter->ndi_sender_video_mutex);
	}

	pthread_mutex_lock(&filter->ndi_sender_audio_mutex);
	ndiLib->send_destroy(filter->ndi_sender);
	filter->ndi_sender = nullptr;
	pthread_mutex_unlock(&filter->ndi_sender_audio_mutex);

	if (!filter->is_audioonly) {
		pthread_mutex_unlock(&filter->ndi_sender_video_mutex);
	}
}

void ndi_sender_create(ndi_filter_t *filter, obs_data_t *settings)
{
	bool allocating_settings = false;

	if (!filter || !filter->obs_source) {
		return;
	}

	auto obs_source = filter->obs_source;
	if (!settings) {
		settings = obs_source_get_settings(obs_source);
		allocating_settings = true;
	}

	auto parent_source = obs_filter_get_parent(filter->obs_source);

	if (!parent_source) {
		// Don't create sender if the parent source is not available
		// It will be created in filter_tick
		if (allocating_settings)
			obs_data_release(settings);
		return;
	}

	QString ndi_name = obs_data_get_string(settings, FLT_PROP_NAME);

	// Check the original template for tokens before any replacements are made,
	// so injected source/filter names cannot trigger unintended token expansion.
	const bool has_source_token = ndi_name.contains("${source}");
	const bool has_filter_token = ndi_name.contains("${filter}");

	// If the name contains ${source} then replace it with the source name.
	if (has_source_token) {
		auto parent_name = obs_source_get_name(parent_source);
		ndi_name.replace("${source}", QString(parent_name));
		signal_handler_t *sh = obs_source_get_signal_handler(parent_source);
		if (sh) {
			signal_handler_disconnect(sh, "rename", on_renamed, filter);
			signal_handler_connect(sh, "rename", on_renamed, filter);
		}
	}

	// If the name contains ${filter} then replace it with the filter name.
	if (has_filter_token) {
		auto filter_name = obs_source_get_name(filter->obs_source);
		ndi_name.replace("${filter}", QString(filter_name));
		signal_handler_t *sh = obs_source_get_signal_handler(filter->obs_source);
		if (sh) {
			signal_handler_disconnect(sh, "rename", on_renamed, filter);
			signal_handler_connect(sh, "rename", on_renamed, filter);
		}
	}

	QByteArray ndi_name_utf8 = ndi_name.toUtf8();

	NDIlib_send_create_t send_desc{};
	send_desc.p_ndi_name = ndi_name_utf8.constData();

	auto groups = obs_data_get_string(settings, FLT_PROP_GROUPS);

	if (groups && groups[0])
		send_desc.p_groups = groups;
	else
		send_desc.p_groups = nullptr;
	send_desc.clock_video = false;
	send_desc.clock_audio = false;

	if (!filter->is_audioonly) {
		pthread_mutex_lock(&filter->ndi_sender_video_mutex);
	}

	pthread_mutex_lock(&filter->ndi_sender_audio_mutex);
	ndiLib->send_destroy(filter->ndi_sender);
	filter->ndi_sender = ndiLib->send_create(&send_desc);

	if (filter->ndi_sender) {
		obs_log(LOG_INFO, "Dedicated NDI Output sender created: '%s'", send_desc.p_ndi_name);
		obs_log(LOG_DEBUG, "'%s' ndi_sender_create: ndi output started", send_desc.p_ndi_name);
	} else {
		obs_log(LOG_WARNING, "WARN-416 - Dedicated NDI Output sender initialisation failed. '%s'",
			send_desc.p_ndi_name);
		obs_log(LOG_DEBUG, "'%s' ndi_sender_create: ndi sender init failed", send_desc.p_ndi_name);
	}

	filter->no_audio_connections = -1;
	pthread_mutex_unlock(&filter->ndi_sender_audio_mutex);

	if (!filter->is_audioonly) {
		pthread_mutex_unlock(&filter->ndi_sender_video_mutex);
	}

	if (allocating_settings) {
		obs_data_release(settings);
	}
}

void ndi_filter_update(void *data, obs_data_t *settings)
{
	auto f = (ndi_filter_t *)data;
	auto obs_source = f->obs_source;
	auto name = obs_source_get_name(obs_source);
	obs_log(LOG_DEBUG, "+ndi_filter_update(name='%s')", name);

	// camera-box #879: optional per-sender cadence override (interkom vs MULTIVIEW tolerate
	// different reductions). Unset -> obs_data_get_int returns 0 -> keep the default 2. No
	// operator UI (that is an operator-visible decision + genuine scope); this is the config
	// seam only.
	long long ndi_filter_render_div = obs_data_get_int(settings, "ndi_filter_render_divisor");
	if (ndi_filter_render_div >= 1)
		f->render_divisor = (uint32_t)ndi_filter_render_div;

	ndi_sender_create(f, settings);

	auto groups = obs_data_get_string(settings, FLT_PROP_GROUPS);

	obs_log(LOG_INFO, "NDI Filter Updated: '%s'", name);
	obs_log(LOG_DEBUG, "-ndi_filter_update(name='%s', groups='%s')", name, groups);
}

void *ndi_filter_create(obs_data_t *settings, obs_source_t *obs_source)
{
	auto name = obs_data_get_string(settings, FLT_PROP_NAME);
	auto groups = obs_data_get_string(settings, FLT_PROP_GROUPS);
	obs_log(LOG_DEBUG, "+ndi_filter_create(name='%s', groups='%s')", name, groups);

	auto f = (ndi_filter_t *)bzalloc(sizeof(ndi_filter_t));
	f->obs_source = obs_source;
	f->texrender = gs_texrender_create(TEXFORMAT, GS_ZS_NONE);
	pthread_mutex_init(&f->ndi_sender_video_mutex, NULL);
	pthread_mutex_init(&f->ndi_sender_audio_mutex, NULL);
	obs_get_video_info(&f->ovi);
	obs_get_audio_info(&f->oai);

	// camera-box #874: explicit zero-init for clarity, mirroring
	// ndi-output.cpp's ndi_output_create (bzalloc already zero-fills; this
	// states the intent). Only the video-capable filter type sends video, so
	// ndi_filter_create_audioonly does not need these.
	f->audit_offered = 0;
	f->audit_sent = 0;
	f->audit_send_wait_ns = 0;
	f->audit_max_send_wait_ns = 0;
	f->audit_mutex_wait_ns = 0;
	f->audit_max_mutex_wait_ns = 0;
	f->audit_last_log = std::chrono::steady_clock::time_point();

	// camera-box #879: throttleable aux sender -- default divisor 2 (canvas-rate derived to
	// ~30fps cells at gate time; effective 1 on a 30fps strih canvas = pure budget gate).
	// Counters start clean; ndi_filter_update() below applies any per-sender override.
	f->render_divisor = 2;
	f->render_frame_counter = 0;
	f->render_ewma_ns = 0;
	f->render_consecutive_skips = 0;

	ndi_filter_update(f, settings);

	obs_log(LOG_INFO, "NDI Filter Created: '%s'", name);
	obs_log(LOG_DEBUG, "-ndi_filter_create(...)");

	return f;
}

void *ndi_filter_create_audioonly(obs_data_t *settings, obs_source_t *obs_source)
{
	auto name = obs_data_get_string(settings, FLT_PROP_NAME);
	auto groups = obs_data_get_string(settings, FLT_PROP_GROUPS);
	obs_log(LOG_DEBUG, "+ndi_filter_create_audioonly(name='%s', groups='%s')", name, groups);

	auto f = (ndi_filter_t *)bzalloc(sizeof(ndi_filter_t));
	f->is_audioonly = true;
	f->obs_source = obs_source;
	pthread_mutex_init(&f->ndi_sender_audio_mutex, NULL);
	obs_get_audio_info(&f->oai);

	ndi_filter_update(f, settings);

	obs_log(LOG_INFO, "NDI Audio-Only Filter Created: '%s'", name);
	obs_log(LOG_DEBUG, "-ndi_filter_create_audioonly(...)");

	return f;
}

void ndi_filter_destroy(void *data)
{
	auto f = (ndi_filter_t *)data;
	auto name = obs_source_get_name(f->obs_source);
	obs_log(LOG_DEBUG, "+ndi_filter_destroy('%s'...)", name);

	ndi_filter_disconnect_rename_handlers(f);

	video_output_close(f->video_output);

	ndi_sender_destroy(f);

	gs_stagesurface_unmap(f->stagesurface);
	gs_stagesurface_destroy(f->stagesurface);
	gs_texrender_destroy(f->texrender);

	if (f->audio_conv_buffer) {
		obs_log(LOG_DEBUG, "ndi_filter_destroy: freeing %zu bytes", f->audio_conv_buffer_size);
		bfree(f->audio_conv_buffer);
		f->audio_conv_buffer = nullptr;
	}

	bfree(f);

	obs_log(LOG_INFO, "NDI Filter Destroyed: '%s'", name);
	obs_log(LOG_DEBUG, "-ndi_filter_destroy('%s'...)", name);
}

void ndi_filter_destroy_audioonly(void *data)
{
	auto f = (ndi_filter_t *)data;
	auto name = obs_source_get_name(f->obs_source);
	obs_log(LOG_DEBUG, "+ndi_filter_destroy_audioonly('%s'...)", name);

	ndi_filter_disconnect_rename_handlers(f);
	ndi_sender_destroy(f);

	if (f->audio_conv_buffer) {
		bfree(f->audio_conv_buffer);
		f->audio_conv_buffer = nullptr;
	}

	bfree(f);

	obs_log(LOG_INFO, "NDI Audio-Only Filter Destroyed: '%s'", name);
	obs_log(LOG_DEBUG, "-ndi_filter_destroy_audioonly('%s'...)", name);
}

void ndi_filter_tick(void *data, float)
{
	auto f = (ndi_filter_t *)data;

	obs_get_video_info(&f->ovi);

	f->rendered = false;
}

void ndi_filter_add(void *data, obs_source_t * /* parent */)
{
	auto f = (ndi_filter_t *)data;
	if (!f->ndi_sender)
		ndi_sender_create(f, nullptr);
}

void ndi_filter_remove(void *data, obs_source_t * /* parent */)
{
	auto f = (ndi_filter_t *)data;
	if (!f)
		return;

	ndi_filter_disconnect_rename_handlers(f);
	ndi_sender_destroy(f);
}

obs_audio_data *ndi_filter_asyncaudio(void *data, obs_audio_data *audio_data)
{
	// NOTE: The logic in this function should be similar to
	// ndi-output.cpp/ndi_output_raw_audio(...)
	auto f = (ndi_filter_t *)data;

	pthread_mutex_lock(&f->ndi_sender_audio_mutex);

	if (!f->ndi_sender) {
		pthread_mutex_unlock(&f->ndi_sender_audio_mutex);
		return audio_data;
	}

	auto now = std::chrono::steady_clock::now();
	if (now - f->last_audio_conn_check >= std::chrono::seconds(1)) {
		f->last_audio_conn_check = now;
		int nc = ndiLib->send_get_no_connections(f->ndi_sender, 10);
		if (nc != f->no_audio_connections) {
			auto ndi_source = ndiLib->send_get_source_name(f->ndi_sender);
			if (nc <= 0)
				obs_log(LOG_DEBUG, "Dedicated NDI Output audio '%s' has no connections.",
					ndi_source->p_ndi_name);
			else if (f->no_audio_connections == 0)
				obs_log(LOG_DEBUG, "Dedicated NDI Output audio '%s' has %d connections.",
					ndi_source->p_ndi_name, nc);
			f->no_audio_connections = nc;
		}
	}

	pthread_mutex_unlock(&f->ndi_sender_audio_mutex);

	obs_get_audio_info(&f->oai);

	NDIlib_audio_frame_v3_t audio_frame = {0};
	audio_frame.sample_rate = f->oai.samples_per_sec;
	audio_frame.no_channels = f->oai.speakers;
	audio_frame.timecode = NDIlib_send_timecode_synthesize;
	audio_frame.no_samples = audio_data->frames;
	audio_frame.channel_stride_in_bytes =
		audio_frame.no_samples *
		4; // TODO: Check if this correct or should 4 be replaced by number of channels.
	// audio_frame.FourCC = NDIlib_FourCC_audio_type_FLTP;
	// audio_frame.p_data = p_frame;
	audio_frame.p_metadata = NULL; // No metadata support yet!

	const size_t data_size = audio_frame.no_channels * audio_frame.channel_stride_in_bytes;

	if (data_size > f->audio_conv_buffer_size) {
		obs_log(LOG_DEBUG, "ndi_filter_asyncaudio: growing audio_conv_buffer from %zu to %zu bytes",
			f->audio_conv_buffer_size, data_size);
		if (f->audio_conv_buffer) {
			obs_log(LOG_DEBUG, "ndi_filter_asyncaudio: freeing %zu bytes", f->audio_conv_buffer_size);
			bfree(f->audio_conv_buffer);
		}
		obs_log(LOG_DEBUG, "ndi_filter_asyncaudio: allocating %zu bytes", data_size);
		f->audio_conv_buffer = (uint8_t *)bmalloc(data_size);
		f->audio_conv_buffer_size = data_size;
	}

	for (int i = 0; i < audio_frame.no_channels; ++i) {
		memcpy(f->audio_conv_buffer + (i * audio_frame.channel_stride_in_bytes), audio_data->data[i],
		       audio_frame.channel_stride_in_bytes);
	}

	audio_frame.p_data = f->audio_conv_buffer;

	pthread_mutex_lock(&f->ndi_sender_audio_mutex);
	if (f->ndi_sender)
		ndiLib->send_send_audio_v3(f->ndi_sender, &audio_frame);
	pthread_mutex_unlock(&f->ndi_sender_audio_mutex);

	return audio_data;
}

obs_source_info create_ndi_filter_info()
{
	obs_source_info ndi_filter_info = {};
	ndi_filter_info.id = "ndi_filter";
	ndi_filter_info.type = OBS_SOURCE_TYPE_FILTER;
	ndi_filter_info.output_flags = OBS_SOURCE_VIDEO;

	ndi_filter_info.get_name = ndi_filter_getname;
	ndi_filter_info.get_properties = ndi_filter_getproperties;
	ndi_filter_info.get_defaults = ndi_filter_getdefaults;

	ndi_filter_info.create = ndi_filter_create;
	ndi_filter_info.filter_add = ndi_filter_add;
	ndi_filter_info.filter_remove = ndi_filter_remove;
	ndi_filter_info.destroy = ndi_filter_destroy;
	ndi_filter_info.update = ndi_filter_update;

	ndi_filter_info.video_tick = ndi_filter_tick;
	ndi_filter_info.video_render = ndi_filter_render_video;

	// Audio is available only with async sources
	ndi_filter_info.filter_audio = ndi_filter_asyncaudio;

	return ndi_filter_info;
}

obs_source_info create_ndi_audiofilter_info()
{
	obs_source_info ndi_filter_info = {};
	ndi_filter_info.id = "ndi_audiofilter";
	ndi_filter_info.type = OBS_SOURCE_TYPE_FILTER;
	ndi_filter_info.output_flags = OBS_SOURCE_AUDIO;

	ndi_filter_info.get_name = ndi_audiofilter_getname;
	ndi_filter_info.get_properties = ndi_filter_getproperties;
	ndi_filter_info.get_defaults = ndi_filter_getdefaults;

	ndi_filter_info.create = ndi_filter_create_audioonly;
	ndi_filter_info.filter_add = ndi_filter_add;
	ndi_filter_info.filter_remove = ndi_filter_remove;
	ndi_filter_info.update = ndi_filter_update;
	ndi_filter_info.destroy = ndi_filter_destroy_audioonly;

	ndi_filter_info.filter_audio = ndi_filter_asyncaudio;

	return ndi_filter_info;
}
