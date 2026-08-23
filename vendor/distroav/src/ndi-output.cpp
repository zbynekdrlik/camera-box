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
#include <util/threading.h>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <mutex>
#include <string>

// #include "plugin-support.h"

static FORCE_INLINE uint32_t min_uint32(uint32_t a, uint32_t b)
{
	return a < b ? a : b;
}

// ---------------------------------------------------------------------------
// Genlock-aligned EMIT timecode (camera-box genlock fork).
//
// Stock DistroAV stamps the outgoing NDI frame timecode with
// NDIlib_send_timecode_synthesize — a counter the NDI SDK seeds ONCE from system
// time at stream start, then advances by frame period. That freezes the
// start-time pipeline buffering into a FIXED ~150ms lag between the timecode and
// the real wall-clock emit. The bias cancels between two OBS senders but breaks
// any comparison against a sender that stamps the real clock (the camera boxes do
// — src/ndi.rs). The QR latency instrument needs the timecode to be the ACTUAL
// per-frame emit time so cam->OBS and OBS<->OBS share one timebase.
//
// Fix: stamp the OS wall clock (DanteSync-disciplined on the broadcast boxes,
// every node sub-ms-locked) snapped to the 1/fps frame boundary AT OR BEFORE the
// emit instant, in 100ns units since the Unix epoch — the SAME per-second grid
// and boundary math as the camera-box sender (src/ndi.rs), so the two timebases
// align.
//
// camera-box #1009: the boundary is the FLOOR (at-or-before), never the
// strictly-next (ceil) one. The ceil stamp dated every outgoing frame 0..1
// interval into the RECEIVER'S FUTURE at the emit instant by construction,
// leaving only network delay as margin against the receiver's issue-147
// backward-step guard — the 2026-08-07 overnight −900 ms hold collapse fired on
// 0.3-45 ms of measured excess exactly because of this bias. Floor keeps grid
// alignment (an exact boundary, identical for any sender observing the same
// instant) while guaranteeing stamps are never future-dated. Changed in
// lock-step with the camera sender (src/ndi.rs floor_boundary_100ns).
// ---------------------------------------------------------------------------
static const int64_t GENLOCK_UNITS_PER_SECOND = 10000000; // 100ns units / second

// Current wall clock in 100ns units since the Unix epoch. std::chrono::system_clock
// is the OS wall clock (DanteSync owns it on strih/stream); its epoch is the Unix
// epoch on all supported platforms.
static int64_t genlock_wall_now_100ns()
{
	using namespace std::chrono;
	return duration_cast<duration<int64_t, std::ratio<1, 10000000>>>(system_clock::now().time_since_epoch())
		.count();
}

// The 1/fps frame boundary AT OR BEFORE now_100ns (camera-box #1009: FLOOR, never
// the strictly-next/ceil boundary — see the header comment above), computed
// relative to each second to avoid drift. Direct port of camera-box
// floor_boundary_100ns (src/ndi.rs) so OBS emit timecodes fall on the SAME grid
// the cameras use. fps <= 0 -> now (no alignment).
static int64_t genlock_floor_boundary_100ns(int64_t now_100ns, int64_t fps)
{
	if (fps <= 0)
		return now_100ns;
	int64_t current_second = (now_100ns / GENLOCK_UNITS_PER_SECOND) * GENLOCK_UNITS_PER_SECOND;
	int64_t offset_in_second = now_100ns - current_second;
	// Which frame slot the instant falls in (0..fps-1); its own boundary is at-or-before.
	int64_t frame_in_second = (offset_in_second * fps) / GENLOCK_UNITS_PER_SECOND;
	// camera-box #1009 review fix: a boundary b_k = floor(k*UNITS/fps) can sit up to one
	// unit BELOW the exact rational, so the slot recovery above under-counts by one for an
	// instant exactly ON such a boundary — promote when the NEXT slot's boundary is still
	// at-or-before the instant (the under-count is at most one slot). Keep in lock-step
	// with camera-box floor_boundary_100ns (src/ndi.rs).
	int64_t next_slot_boundary = ((frame_in_second + 1) * GENLOCK_UNITS_PER_SECOND) / fps;
	if (next_slot_boundary <= offset_in_second)
		frame_in_second += 1;
	// Multiply before divide to maintain precision (same as the camera-box mirror).
	return current_second + (frame_in_second * GENLOCK_UNITS_PER_SECOND / fps);
}

// Boundary-aligned real wall-clock emit timecode for a frame at `framerate` fps.
// camera-box #1009: the boundary at-or-before the emit instant (floor).
static int64_t genlock_emit_timecode_100ns(double framerate)
{
	int64_t fps = (int64_t)llround(framerate);
	return genlock_floor_boundary_100ns(genlock_wall_now_100ns(), fps);
}

typedef void (*uyvy_conv_function)(uint8_t *input[], uint32_t in_linesize[], uint32_t start_y, uint32_t end_y,
				   uint8_t *output, uint32_t out_linesize);

static void convert_i444_to_uyvy(uint8_t *input[], uint32_t in_linesize[], uint32_t start_y, uint32_t end_y,
				 uint8_t *output, uint32_t out_linesize)
{
	uint8_t *_Y;
	uint8_t *_U;
	uint8_t *_V;
	uint8_t *_out;
	uint32_t width = min_uint32(in_linesize[0], out_linesize);
	for (uint32_t y = start_y; y < end_y; ++y) {
		_Y = input[0] + ((size_t)y * (size_t)in_linesize[0]);
		_U = input[1] + ((size_t)y * (size_t)in_linesize[1]);
		_V = input[2] + ((size_t)y * (size_t)in_linesize[2]);

		_out = output + ((size_t)y * (size_t)out_linesize);

		for (uint32_t x = 0; x < width; x += 2) {
			// Quality loss here. Some chroma samples are ignored.
			*(_out++) = *(_U++);
			_U++;
			*(_out++) = *(_Y++);
			*(_out++) = *(_V++);
			_V++;
			*(_out++) = *(_Y++);
		}
	}
}

typedef struct {
	obs_output_t *output;
	const char *ndi_name;
	const char *ndi_groups;
	bool uses_video;
	bool uses_audio;

	bool started;

	NDIlib_send_instance_t ndi_sender;
	pthread_mutex_t ndi_sender_mutex;

	uint32_t frame_width;
	uint32_t frame_height;
	NDIlib_FourCC_video_type_e frame_fourcc;
	double video_framerate;

	size_t audio_channels;
	uint32_t audio_samplerate;

	uint8_t *conv_buffer;
	uint32_t conv_linesize;
	uyvy_conv_function conv_function;

	uint8_t *audio_conv_buffer;
	size_t audio_conv_buffer_size;
	int32_t no_connections;
	std::chrono::time_point<std::chrono::steady_clock> last_conn_check;

	// camera-box #874: NDI OUTPUT-side send-path audit -- mirrors the input-side
	// genlock-fifo audit (obs-source.c genlock_audit_log) so a send-side stall is
	// as visible as a receive-side one. `audit_offered` counts every
	// ndi_output_rawvideo entry (a frame libobs handed to this output);
	// `audit_sent` counts every send_send_video_async_v2 call actually made (the
	// SDK call returns void, so this is "attempted", not confirmed-delivered).
	// `audit_send_wait_ns`/`audit_max_send_wait_ns` are the cumulative/peak time
	// spent inside that call -- a large cumulative value proves the async send is
	// serialising on the receiver; a near-zero value with offered far below the
	// canvas rate moves the fault upstream into libobs's output path.
	uint64_t audit_offered;
	uint64_t audit_sent;
	uint64_t audit_send_wait_ns;
	uint64_t audit_max_send_wait_ns;
	std::chrono::time_point<std::chrono::steady_clock> audit_last_log;
} ndi_output_t;

const char *ndi_output_getname(void *)
{
	return obs_module_text("NDIPlugin.OutputName");
}

obs_properties_t *ndi_output_getproperties(void *)
{
	obs_log(LOG_DEBUG, "+ndi_output_getproperties()");

	obs_properties_t *props = obs_properties_create();
	obs_properties_set_flags(props, OBS_PROPERTIES_DEFER_UPDATE);

	obs_properties_add_text(props, "ndi_name", obs_module_text("NDIPlugin.OutputProps.NDIName"), OBS_TEXT_DEFAULT);
	obs_properties_add_text(props, "ndi_groups", obs_module_text("NDIPlugin.OutputProps.NDIGroups"),
				OBS_TEXT_DEFAULT);

	obs_log(LOG_DEBUG, "-ndi_output_getproperties()");

	return props;
}

void ndi_output_getdefaults(obs_data_t *settings)
{
	obs_log(LOG_DEBUG, "+ndi_output_getdefaults()");
	obs_data_set_default_string(settings, "ndi_name", "DistroAV output (changeme)");
	obs_data_set_default_string(settings, "ndi_groups", "DistroAV output (changeme)");
	obs_data_set_default_bool(settings, "uses_video", true);
	obs_data_set_default_bool(settings, "uses_audio", true);
	obs_log(LOG_DEBUG, "-ndi_output_getdefaults()");
}

void ndi_output_update(void *data, obs_data_t *settings);

void *ndi_output_create(obs_data_t *settings, obs_output_t *output)
{
	auto name = obs_data_get_string(settings, "ndi_name");
	auto groups = obs_data_get_string(settings, "ndi_groups");
	obs_log(LOG_DEBUG, "+ndi_output_create(name='%s', groups='%s', ...)", name, groups);
	auto o = (ndi_output_t *)bzalloc(sizeof(ndi_output_t));
	o->output = output;
	pthread_mutex_init(&o->ndi_sender_mutex, NULL);
	ndi_output_update(o, settings);

	// initialize last_conn_check so first check will occur immediately
	o->no_connections = -1;
	o->last_conn_check = std::chrono::steady_clock::time_point();

	// camera-box #874: explicit zero-init for clarity, mirroring last_conn_check
	// above (bzalloc already zero-fills, but state the intent).
	o->audit_offered = 0;
	o->audit_sent = 0;
	o->audit_send_wait_ns = 0;
	o->audit_max_send_wait_ns = 0;
	o->audit_last_log = std::chrono::steady_clock::time_point();

	obs_log(LOG_DEBUG, "-ndi_output_create(name='%s', groups='%s', ...)", name, groups);
	return o;
}

static const std::map<video_format, std::string> video_to_color_format_map = {{VIDEO_FORMAT_P010, "P010"},
									      {VIDEO_FORMAT_I010, "I010"},
									      {VIDEO_FORMAT_P216, "P216"},
									      {VIDEO_FORMAT_P416, "P416"}};

// ---------------------------------------------------------------------------
// camera-box #1185: PGM-first-port reservation.
//
// libndi assigns each NDIlib_send_create a TCP port sequentially from 5961 in
// CREATION ORDER. DistroAV defers main_output_init()/preview_output_init() to
// OBS_FRONTEND_EVENT_FINISHED_LOADING (plugin-main.cpp), which fires AFTER the
// scene collection loads -- so the per-source ndi_filter republishes
// (Grading/MULTIVIEW/interkom) win the low ports and the program (2ME PGM)
// lands on a HIGH one. A stock NDI Studio Monitor / building TV that reconnects
// by CACHED PORT is then handed the wrong sender for the program after any OBS
// restart (issue 1180 / issue 1181).
//
// Fix: RESERVE the program's NDI send instance at obs_module_post_load time --
// BEFORE the scene collection loads -- so it grabs :5961, then have the real
// ndi_output_start ADOPT that reserved instance (by exact name+groups match)
// instead of calling send_create again. The reserved instance persists across
// the whole load, holding :5961, so the program's port is pinned regardless of
// how many ndi_filter republishes are created afterward.
//
// Bounded caveats (see #1185): the reserved instance advertises the program
// name FRAMELESS for the ~seconds of OBS load (bounded, and better than the
// wrong-source-indefinitely reshuffle); only PGM is pinned (PVW + filters still
// reshuffle among the remaining ports, mitigated by the issue-1181 watchdog);
// and this is gated (in obs_module_post_load) on the main output being
// ENABLED+NAMED so a disabled PGM is never advertised.
//
// Thread-safety: reserve() runs on the module-load thread (obs_module_post_load);
// take() / release() run on the main (UI) thread (the queued FINISHED_LOADING
// init and obs_module_unload). g_reserved_main_mutex guards the holder either
// way. take() never calls send_destroy and never touches a per-output mutex, so
// there is no lock-order inversion against o->ndi_sender_mutex.
// ---------------------------------------------------------------------------
static NDIlib_send_instance_t g_reserved_main_sender = nullptr;
static std::string g_reserved_main_name;
static std::string g_reserved_main_groups;
static std::mutex g_reserved_main_mutex;

// Create the main output's NDI send instance NOW so it reserves the first free
// NDI port (:5961). Called from obs_module_post_load with the configured main
// output name+groups when the main output is enabled. Idempotent: a second call
// while a reservation is already live is a no-op.
void ndi_output_reserve_main_sender(const char *name, const char *groups)
{
	if (!ndiLib || !name || !name[0])
		return;
	std::lock_guard<std::mutex> lock(g_reserved_main_mutex);
	if (g_reserved_main_sender)
		return; // already reserved

	NDIlib_send_create_t send_desc{};
	send_desc.p_ndi_name = name;
	if (groups && groups[0])
		send_desc.p_groups = groups;
	else
		send_desc.p_groups = nullptr;
	send_desc.clock_video = false;
	send_desc.clock_audio = false;

	g_reserved_main_sender = ndiLib->send_create(&send_desc);
	if (g_reserved_main_sender) {
		g_reserved_main_name = name;
		g_reserved_main_groups = (groups && groups[0]) ? groups : "";
		obs_log(LOG_INFO,
			"ndi_output_reserve_main_sender: reserved the first NDI port for main output '%s' at module post-load (#1185)",
			name);
	} else {
		obs_log(LOG_WARNING,
			"WARN-1185 - ndi_output_reserve_main_sender: failed to reserve NDI send instance for '%s'",
			name);
	}
}

// Destroy a reservation that was never adopted by ndi_output_start (main output
// disabled after reservation, name changed, or OBS closed before finishing
// load), so it never leaks the port / a frameless source. Called from
// obs_module_unload BEFORE ndiLib->destroy().
void ndi_output_release_reserved_main_sender()
{
	std::lock_guard<std::mutex> lock(g_reserved_main_mutex);
	if (g_reserved_main_sender && ndiLib) {
		ndiLib->send_destroy(g_reserved_main_sender);
		obs_log(LOG_DEBUG,
			"ndi_output_release_reserved_main_sender: destroyed the unadopted reserved main sender '%s' (#1185)",
			g_reserved_main_name.c_str());
	}
	g_reserved_main_sender = nullptr;
	g_reserved_main_name.clear();
	g_reserved_main_groups.clear();
}

// If a reserved main sender matches (name+groups) the output about to start,
// hand it over (transferring ownership) and clear the reservation; else return
// nullptr and the caller creates its own. A non-matching output (preview, the
// random-named support-test, a renamed PGM) never adopts it.
static NDIlib_send_instance_t ndi_output_take_reserved_sender(const char *name, const char *groups)
{
	std::lock_guard<std::mutex> lock(g_reserved_main_mutex);
	if (!g_reserved_main_sender)
		return nullptr;
	std::string want_name = name ? name : "";
	std::string want_groups = (groups && groups[0]) ? groups : "";
	if (want_name != g_reserved_main_name || want_groups != g_reserved_main_groups)
		return nullptr; // not for this output
	NDIlib_send_instance_t s = g_reserved_main_sender;
	g_reserved_main_sender = nullptr;
	g_reserved_main_name.clear();
	g_reserved_main_groups.clear();
	return s;
}

bool ndi_output_start(void *data)
{
	auto o = (ndi_output_t *)data;
	auto name = o->ndi_name;
	auto groups = o->ndi_groups;
	obs_log(LOG_DEBUG, "+ndi_output_start(name='%s', groups='%s', ...)", name, groups);
	if (o->started) {
		obs_log(LOG_INFO, "NDI Output already started: '%s'", name);
		obs_log(LOG_DEBUG, "-ndi_output_start(name='%s', groups='%s', ...)", name, groups);
		return false;
	}

	uint32_t flags = 0;
	video_t *video = obs_output_video(o->output);
	audio_t *audio = obs_output_audio(o->output);
	obs_output_set_last_error(o->output, "");

	if (!video && !audio) {
		obs_log(LOG_WARNING, "WARN-413 - NDI Output could not start. No Audio/Video data available. ('%s')",
			name);
		obs_log(LOG_DEBUG, "'%s'('%s') ndi_output_start: no video nor audio available", name, groups);
		return false;
	}

	if (o->uses_video && video) {
		video_format format = video_output_get_format(video);
		uint32_t width = video_output_get_width(video);
		uint32_t height = video_output_get_height(video);

		switch (format) {
		case VIDEO_FORMAT_I444:
			o->conv_function = convert_i444_to_uyvy;
			o->frame_fourcc = NDIlib_FourCC_video_type_UYVY;
			o->conv_linesize = width * 2;
			o->conv_buffer = new uint8_t[(size_t)height * (size_t)o->conv_linesize * 2]();
			break;

		case VIDEO_FORMAT_NV12:
			o->frame_fourcc = NDIlib_FourCC_video_type_NV12;
			break;

		case VIDEO_FORMAT_I420:
			o->frame_fourcc = NDIlib_FourCC_video_type_I420;
			break;

		case VIDEO_FORMAT_RGBA:
			o->frame_fourcc = NDIlib_FourCC_video_type_RGBA;
			break;

		case VIDEO_FORMAT_BGRA:
			o->frame_fourcc = NDIlib_FourCC_video_type_BGRA;
			break;

		case VIDEO_FORMAT_BGRX:
			o->frame_fourcc = NDIlib_FourCC_video_type_BGRX;
			break;

		default:
			obs_log(LOG_ERROR, "ERR-410 - NDI Output cannot start : Unsupported pixel format %d. ('%s')",
				format, name);
			obs_log(LOG_DEBUG, "-ndi_output_start(name='%s', groups='%s', ...)", name, groups);
			auto error_string = obs_module_text("NDIPlugin.OutputSettings.LastError") +
					    video_to_color_format_map.at(format);
			obs_output_set_last_error(o->output, error_string.c_str());
			return false;
		}

		o->frame_width = width;
		o->frame_height = height;
		o->video_framerate = video_output_get_frame_rate(video);
		flags |= OBS_OUTPUT_VIDEO;
	}

	if (o->uses_audio && audio) {
		o->audio_samplerate = audio_output_get_sample_rate(audio);
		o->audio_channels = audio_output_get_channels(audio);
		flags |= OBS_OUTPUT_AUDIO;
	}

	NDIlib_send_create_t send_desc{};
	send_desc.p_ndi_name = name;
	if (groups && groups[0])
		send_desc.p_groups = groups;
	else
		send_desc.p_groups = nullptr;
	send_desc.clock_video = false;
	send_desc.clock_audio = false;

	pthread_mutex_lock(&o->ndi_sender_mutex);
	// camera-box #1185: adopt the port-reserved main sender if this output IS the
	// main output (name+groups match the reservation made at obs_module_post_load);
	// else create a fresh sender as stock. Adopting reuses the instance that already
	// holds :5961, pinning the program's NDI port across restarts.
	o->ndi_sender = ndi_output_take_reserved_sender(name, groups);
	if (o->ndi_sender) {
		obs_log(LOG_INFO, "ndi_output_start: adopted the port-reserved main NDI sender for '%s' (#1185)",
			name);
	} else {
		o->ndi_sender = ndiLib->send_create(&send_desc);
	}

	if (o->ndi_sender) {
		o->started = obs_output_begin_data_capture(o->output, flags);
		if (o->started) {
			obs_log(LOG_INFO, "NDI Output started successfully. '%s'", name);
			obs_log(LOG_DEBUG, "'%s' ndi_output_start: ndi output started", name);
		} else {
			obs_log(LOG_WARNING, "WARN-415 - NDI Sender data capture failed. '%s'", name);
			obs_log(LOG_DEBUG, "'%s' ndi_output_start: data capture start failed", name);
		}
	} else {
		obs_log(LOG_WARNING, "WARN-416 - NDI Sender initialisation failed. '%s'", name);
		obs_log(LOG_DEBUG, "'%s' ndi_output_start: ndi sender init failed", name);
	}

	obs_log(LOG_DEBUG, "-ndi_output_start(name='%s', groups='%s'...)", name, groups);
	pthread_mutex_unlock(&o->ndi_sender_mutex);

	return o->started;
}

void ndi_output_update(void *data, obs_data_t *settings)
{
	auto o = (ndi_output_t *)data;
	auto name = obs_data_get_string(settings, "ndi_name");
	auto groups = obs_data_get_string(settings, "ndi_groups");
	obs_log(LOG_DEBUG, "ndi_output_update(name='%s', groups='%s', ...)", name, groups);

	o->ndi_name = name;
	o->ndi_groups = groups;
	o->uses_video = obs_data_get_bool(settings, "uses_video");
	o->uses_audio = obs_data_get_bool(settings, "uses_audio");

	obs_log(LOG_INFO, "NDI Output Updated. '%s'", name);
	obs_log(LOG_DEBUG, "ndi_output_update(name='%s', groups='%s', uses_video='%s', uses_audio='%s')", name, groups,
		o->uses_video ? "true" : "false", o->uses_audio ? "true" : "false");
}

void ndi_output_stop(void *data, uint64_t)
{
	auto o = (ndi_output_t *)data;
	auto name = o->ndi_name;
	auto groups = o->ndi_groups;
	obs_log(LOG_DEBUG, "+ndi_output_stop(name='%s', groups='%s', ...)", name, groups);
	if (o->started) {
		o->started = false;

		obs_output_end_data_capture(o->output);

		if (o->ndi_sender) {
			obs_log(LOG_DEBUG, "ndi_output_stop: +ndiLib->send_destroy(o->ndi_sender)");
			pthread_mutex_lock(&o->ndi_sender_mutex);
			ndiLib->send_destroy(o->ndi_sender);
			obs_log(LOG_DEBUG, "ndi_output_stop: -ndiLib->send_destroy(o->ndi_sender)");
			o->ndi_sender = nullptr;
			pthread_mutex_unlock(&o->ndi_sender_mutex);
		}

		if (o->conv_buffer) {
			delete[] o->conv_buffer;
			o->conv_buffer = nullptr;
			o->conv_function = nullptr;
		}

		o->frame_width = 0;
		o->frame_height = 0;
		o->video_framerate = 0.0;
		o->audio_channels = 0;
		o->audio_samplerate = 0;

		obs_log(LOG_INFO, "NDI Output Stopped. '%s'", name);
	}

	obs_log(LOG_DEBUG, "-ndi_output_stop(name='%s', groups='%s', ...)", name, groups);
}

void ndi_output_destroy(void *data)
{
	auto o = (ndi_output_t *)data;
	auto name = o->ndi_name;
	auto groups = o->ndi_groups;

	pthread_mutex_destroy(&o->ndi_sender_mutex);

	obs_log(LOG_DEBUG, "+ndi_output_destroy(name='%s', groups='%s', ...)", name, groups);

	if (o->audio_conv_buffer) {
		obs_log(LOG_DEBUG, "ndi_output_destroy: freeing %zu bytes", o->audio_conv_buffer_size);
		bfree(o->audio_conv_buffer);
		o->audio_conv_buffer = nullptr;
	}
	obs_log(LOG_DEBUG, "-ndi_output_destroy(name='%s', groups='%s', ...)", name, groups);
	bfree(o);
}

void ndi_output_rawvideo(void *data, video_data *frame)
{
	auto o = (ndi_output_t *)data;

	// camera-box #874: count EVERY entry -- a frame libobs handed to this output --
	// before any of the guards below, so a frame that never reaches the send call
	// still shows up as `dropped` in the audit line.
	o->audit_offered++;

	if (!o->started || !o->frame_width || !o->frame_height)
		return;

	pthread_mutex_lock(&o->ndi_sender_mutex);
	if (!o->ndi_sender) {
		pthread_mutex_unlock(&o->ndi_sender_mutex);
		return;
	}

	// Throttle calls to send_get_no_connections to at most once per second
	auto now = std::chrono::steady_clock::now();
	if (now - o->last_conn_check >= std::chrono::seconds(1)) {
		int nc = ndiLib->send_get_no_connections(o->ndi_sender, 10);
		o->last_conn_check = now;

		if (nc != o->no_connections) {
			auto ndi_source = ndiLib->send_get_source_name(o->ndi_sender);
			if (nc <= 0)
				obs_log(LOG_DEBUG, "NDI Output video '%s' has no connections.", ndi_source->p_ndi_name);
			else if (o->no_connections == 0)
				obs_log(LOG_DEBUG, "NDI Output video '%s' has %d connections.", ndi_source->p_ndi_name,
					nc);
			o->no_connections = nc;
		}
	}

	pthread_mutex_unlock(&o->ndi_sender_mutex);

	uint32_t width = o->frame_width;
	uint32_t height = o->frame_height;

	NDIlib_video_frame_v2_t video_frame = {0};
	video_frame.xres = width;
	video_frame.yres = height;
	video_frame.frame_rate_N = (int)(o->video_framerate * 100);
	// TODO fixme: broken on fractional framerates
	video_frame.frame_rate_D =
		100; // TODO : investigate if there is a better way to get both _D & _N set to the proper framerate from OBS output.
	video_frame.frame_format_type = NDIlib_frame_format_type_progressive;
	// Genlock fork: real DanteSync wall-clock boundary, NOT synthesize (see helper
	// above) — so the emitted timecode is the ACTUAL per-frame emit instant and
	// cam->OBS / OBS<->OBS latency share one timebase.
	video_frame.timecode = genlock_emit_timecode_100ns(o->video_framerate);
	video_frame.FourCC = o->frame_fourcc;

	if (video_frame.FourCC == NDIlib_FourCC_type_UYVY) {
		o->conv_function(frame->data, frame->linesize, 0, height, o->conv_buffer, o->conv_linesize);
		video_frame.p_data = o->conv_buffer;
		video_frame.line_stride_in_bytes = o->conv_linesize;
	} else {
		video_frame.p_data = frame->data[0];
		video_frame.line_stride_in_bytes = frame->linesize[0];
	}

	// camera-box #874: time the async send call itself -- this is the load-bearing
	// number. send_send_video_async_v2 blocks until the PREVIOUS async frame was
	// consumed (NDI's async contract), so a large cumulative wait here proves the
	// send is serialising on the receiver; a near-zero wait with `offered` far
	// above `sent`'s rate would instead point upstream, into libobs's output path.
	auto send_start = std::chrono::steady_clock::now();
	ndiLib->send_send_video_async_v2(o->ndi_sender, &video_frame);
	auto send_end = std::chrono::steady_clock::now();

	o->audit_sent++;
	uint64_t send_wait_ns =
		(uint64_t)std::chrono::duration_cast<std::chrono::nanoseconds>(send_end - send_start).count();
	o->audit_send_wait_ns += send_wait_ns;
	if (send_wait_ns > o->audit_max_send_wait_ns)
		o->audit_max_send_wait_ns = send_wait_ns;

	// One audit line per output every ~5s, mirroring genlock_audit_log's cadence
	// and guard shape (libobs/obs-source.c) -- the first call only seeds
	// audit_last_log so the very first line waits a full interval rather than
	// firing immediately.
	if (o->audit_last_log == std::chrono::steady_clock::time_point())
		o->audit_last_log = send_end;
	if (send_end - o->audit_last_log >= std::chrono::seconds(5)) {
		o->audit_last_log = send_end;
		obs_log(LOG_INFO,
			"genlock-ndi-output audit '%s': offered=%llu sent=%llu dropped=%llu "
			"send_wait_ms=%.3f max_send_wait_ms=%.3f (#874)",
			o->ndi_name, (unsigned long long)o->audit_offered, (unsigned long long)o->audit_sent,
			(unsigned long long)(o->audit_offered - o->audit_sent),
			(double)o->audit_send_wait_ns / 1.0e6, (double)o->audit_max_send_wait_ns / 1.0e6);
	}
}

void ndi_output_rawaudio(void *data, audio_data *frame)
{
	// NOTE: The logic in this function should be similar to
	// ndi-filter.cpp/ndi_filter_asyncaudio(...)
	auto o = (ndi_output_t *)data;
	if (!o->started || !o->audio_samplerate || !o->audio_channels)
		return;

	pthread_mutex_lock(&o->ndi_sender_mutex);
	if (!o->ndi_sender) {
		pthread_mutex_unlock(&o->ndi_sender_mutex);
		return;
	}

	auto now = std::chrono::steady_clock::now();
	if (now - o->last_conn_check >= std::chrono::seconds(1)) {
		o->last_conn_check = now;

		int nc = ndiLib->send_get_no_connections(o->ndi_sender, 10);

		if (nc != o->no_connections) {
			auto ndi_source = ndiLib->send_get_source_name(o->ndi_sender);
			if (nc <= 0)
				obs_log(LOG_DEBUG, "NDI Output audio '%s' has no connections.", ndi_source->p_ndi_name);
			else if (o->no_connections == 0)
				obs_log(LOG_DEBUG, "NDI Output audio '%s' has %d connections.", ndi_source->p_ndi_name,
					nc);
			o->no_connections = nc;
		}
	}

	pthread_mutex_unlock(&o->ndi_sender_mutex);

	NDIlib_audio_frame_v3_t audio_frame = {0};
	audio_frame.sample_rate = o->audio_samplerate;
	audio_frame.no_channels = (int)o->audio_channels;
	// Genlock fork: real DanteSync wall-clock emit time (not synthesize), so audio
	// timecodes share the same timebase as video + the cameras. Audio is not frame-
	// gridded, so stamp raw wall-clock now (no boundary snap).
	audio_frame.timecode = genlock_wall_now_100ns();
	audio_frame.no_samples = frame->frames;
	audio_frame.channel_stride_in_bytes = frame->frames * 4;
	audio_frame.FourCC = NDIlib_FourCC_audio_type_FLTP;

	const size_t data_size = audio_frame.no_channels * audio_frame.channel_stride_in_bytes;

	if (data_size > o->audio_conv_buffer_size) {
		obs_log(LOG_DEBUG, "ndi_output_rawaudio('%s'): growing audio_conv_buffer from %zu to %zu bytes",
			o->ndi_name, o->audio_conv_buffer_size, data_size);
		if (o->audio_conv_buffer) {
			obs_log(LOG_DEBUG, "ndi_output_rawaudio('%s'): freeing %zu bytes", o->ndi_name,
				o->audio_conv_buffer_size);
			bfree(o->audio_conv_buffer);
		}
		obs_log(LOG_DEBUG, "ndi_output_rawaudio('%s'): allocating %zu bytes", o->ndi_name, data_size);
		o->audio_conv_buffer = (uint8_t *)bmalloc(data_size);
		o->audio_conv_buffer_size = data_size;
	}

	for (int i = 0; i < audio_frame.no_channels; ++i) {
		memcpy(o->audio_conv_buffer + (i * audio_frame.channel_stride_in_bytes), frame->data[i],
		       audio_frame.channel_stride_in_bytes);
	}

	audio_frame.p_data = o->audio_conv_buffer;

	ndiLib->send_send_audio_v3(o->ndi_sender, &audio_frame);
}

obs_output_info create_ndi_output_info()
{
	obs_output_info ndi_output_info = {};
	ndi_output_info.id = "ndi_output";
	ndi_output_info.flags = OBS_OUTPUT_AV;

	ndi_output_info.get_name = ndi_output_getname;
	ndi_output_info.get_properties = ndi_output_getproperties;
	ndi_output_info.get_defaults = ndi_output_getdefaults;

	ndi_output_info.create = ndi_output_create;
	ndi_output_info.start = ndi_output_start;
	ndi_output_info.update = ndi_output_update;
	ndi_output_info.stop = ndi_output_stop;
	ndi_output_info.destroy = ndi_output_destroy;

	ndi_output_info.raw_video = ndi_output_rawvideo;
	ndi_output_info.raw_audio = ndi_output_rawaudio;

	return ndi_output_info;
}
