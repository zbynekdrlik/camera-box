/******************************************************************************
	camera-box issue 1387 slice 2: the JPEG XS receiving source on the strih OBS.

	A cambox sends its camera at ~2x the NDI bitrate as JPEG XS over its own UDP link (the
	`CAMERA_BOX_JXS_TARGET` sender, 30 fps on the strih grid). This source receives it through
	jxs/recv-ffi -- the SAME Rust receive loop jxs-recv-probe runs (reassembly, slice-wise SVT
	decode, concealment, and the 32-byte receiver report back to the cambox, on which its automatic
	bpp climbs) -- and hands each decoded frame to libobs exactly like the NDI source hands an NDI
	frame: obs_source_output_video with timestamp = the sender's genlock timecode x 100 (epoch
	nanoseconds), so genlock_is_wallclock_ts engages the genlock FIFO and the per-second grid, and
	libobs writes this input's own `genlock-fifo audit '<name>'` line.

	It sits BESIDE the camera's NDI input (owner decision: NDI stays). Built only where the strih
	Linux build sets CAMERA_BOX_JXS_SOURCE (linux-genlock.yml, the strih variant); every other
	DistroAV build compiles it out (the receiver is Linux-only, the Windows strih is retired).

	Threads: the receiver thread owns the receiver (open / next frame / stats / close). update()
	only stores the settings and bumps a generation counter (it runs on the graphics thread for
	an async video source: never block there, the issue 1320 lesson); destroy() stops and joins
	the thread, which waits at most one 100 ms poll plus one decode.
******************************************************************************/

#include <obs-module.h>
#include <util/platform.h>
#include <util/threading.h>

#include <atomic>
#include <cinttypes>
#include <cstdio>
#include <mutex>
#include <string>
#include <thread>

#include <dlfcn.h>

#include "plugin-support.h"
#include "camera_box_jxs.h"
#include "jxs-source-decide.h"
#include "jxs-state-table.h"

#define JXS_SOURCE_ID "camera_box_jxs_source"

#define PROP_JXS_PORT "jxs_port"
#define PROP_JXS_CAMERA "jxs_camera"
#define PROP_JXS_MAX_BPP "jxs_max_bpp"
/* the genlock whitelist keys of ndi-source.cpp that apply to this source. NOT genlock_burn: the
 * burn sweeps (obs_burn_filter.py, the EVENT contract, the leak guards) enumerate ndi_source inputs
 * only, so a burn left on here would escape them and burn on the strih program. */
#define PROP_GENLOCK_FIFO "genlock_fifo"
#define PROP_GENLOCK_LATENCY_MS_SRC "genlock_latency_ms_src"

/* How long one receiver poll waits (bounds the stop latency). */
#define JXS_POLL_MS 100u
/* A failed open is retried after this long. */
#define JXS_REOPEN_DELAY_NS 1000000000ULL

/* The genlock exports, resolved at runtime by name like ndi-source.cpp does (a stock OBS without
 * them leaves the source a plain async source with a loud warning, never a load failure). */
static void *jxs_resolve(const char *name)
{
	void *fn = dlsym(RTLD_DEFAULT, name);
	if (!fn)
		obs_log(LOG_WARNING, "jxs-source: %s not exported by this OBS build -- inert (stock OBS?) (issue 1387)",
			name);
	return fn;
}

typedef void (*jxs_set_bool_fn)(obs_source_t *, bool);
typedef void (*jxs_set_u32_fn)(obs_source_t *, uint32_t);

struct jxs_genlock_api {
	jxs_set_bool_fn set_fifo;
	jxs_set_u32_fn set_latency_ms;
	jxs_set_bool_fn set_connected;
};

static const jxs_genlock_api &jxs_genlock()
{
	static const jxs_genlock_api api = {
		(jxs_set_bool_fn)jxs_resolve("obs_source_set_genlock_fifo"),
		(jxs_set_u32_fn)jxs_resolve("obs_source_set_genlock_latency_ms"),
		(jxs_set_bool_fn)jxs_resolve("obs_source_set_genlock_connected"),
	};
	return api;
}

struct jxs_source {
	obs_source_t *source = nullptr;
	std::thread thread;
	std::atomic<bool> running{false};
	std::mutex config_mutex;
	cb_jxs_config config = {CB_JXS_PORT_DEFAULT, 0, 0};
	std::atomic<uint64_t> config_gen{0};
};

static cb_jxs_config jxs_read_config(jxs_source *s, uint64_t *gen)
{
	std::lock_guard<std::mutex> lock(s->config_mutex);
	*gen = s->config_gen.load();
	return s->config;
}

static void jxs_set_connected(jxs_source *s, bool connected)
{
	if (auto fn = jxs_genlock().set_connected)
		fn(s->source, connected);
}

/* The libobs format of a decoded frame's sampling (ABI 2): a cambox sends 4:2:2, the strih
 * program 4:2:0 (issue 1387 slice 3); anything else is never handed to libobs. */
static enum video_format jxs_video_format(uint8_t chroma)
{
	switch (chroma) {
	case CBJXS_CHROMA_420:
		return VIDEO_FORMAT_I420;
	case CBJXS_CHROMA_422:
		return VIDEO_FORMAT_I422;
	case CBJXS_CHROMA_444:
		return VIDEO_FORMAT_I444;
	default:
		return VIDEO_FORMAT_NONE;
	}
}

/* Hand one decoded frame to libobs: planar in its own sampling (I420 / I422 / I444), BT.709
 * limited range, stamped with the sender's genlock timecode in epoch nanoseconds. libobs copies
 * the planes before it returns. */
static void jxs_output_frame(jxs_source *s, const cbjxs_frame &f)
{
	const enum video_format format = jxs_video_format(f.chroma);
	if (format == VIDEO_FORMAT_NONE)
		return;
	obs_source_frame frame = {};
	frame.format = format;
	frame.width = f.width;
	frame.height = f.height;
	frame.data[0] = const_cast<uint8_t *>(f.y);
	frame.data[1] = const_cast<uint8_t *>(f.u);
	frame.data[2] = const_cast<uint8_t *>(f.v);
	frame.linesize[0] = f.y_stride;
	frame.linesize[1] = f.uv_stride;
	frame.linesize[2] = f.uv_stride;
	frame.timestamp = cb_jxs_timestamp_ns(f.timecode_100ns);
	frame.full_range = false;
	video_format_get_parameters_for_format(VIDEO_CS_709, VIDEO_RANGE_PARTIAL, format,
					       frame.color_matrix, frame.color_range_min, frame.color_range_max);
	obs_source_output_video(s->source, &frame);
}

/* This source's row in the JPEG XS state table (the frontend indicator, issue 1387): its name
 * (refreshed: a rename) and what it listens on. */
static void jxs_state_entry(jxs_source *s, const cb_jxs_config &cfg)
{
	char detail[96];
	snprintf(detail, sizeof detail, "UDP %u, camera %u", (unsigned)cfg.port, (unsigned)cfg.camera);
	cb_jxs_state().set_entry(s, CB_JXS_STATE_INPUT, obs_source_get_name(s->source), detail);
}

/* The 5 s status line: this window's deltas of the receiver counters (also the window the state
 * table publishes). */
static void jxs_log_status(jxs_source *s, const cbjxs_stats &now, const cbjxs_stats &prev, double window_s,
			   uint32_t width, uint32_t height, uint64_t reopens)
{
	const uint64_t frames = now.frames - prev.frames;
	const uint64_t undecodable = now.undecodable_frames - prev.undecodable_frames;
	const uint32_t bpp_x100 = cb_jxs_bpp_x100(now.bytes - prev.bytes, frames + undecodable, width, height);
	obs_log(LOG_INFO,
		"jxs-source '%s': fps=%.2f frames=%" PRIu64 " lost_slices=%" PRIu64 " concealed=%" PRIu64
		" undecodable=%" PRIu64 " reopens=%" PRIu64 " decoder_reopens=%" PRIu64 " bpp=%u.%02u"
		" reports=%" PRIu64 " report_errors=%" PRIu64 " dropped_unread=%" PRIu64 " foreign=%" PRIu64
		" camera=%u link_mbps=%u (issue 1387)",
		obs_source_get_name(s->source), window_s > 0.0 ? (double)frames / window_s : 0.0, frames,
		now.lost_slices - prev.lost_slices, now.concealed_frames - prev.concealed_frames, undecodable,
		reopens, now.header_changes - prev.header_changes, bpp_x100 / 100u, bpp_x100 % 100u,
		now.reports_sent - prev.reports_sent, now.report_errors - prev.report_errors,
		now.dropped_unread - prev.dropped_unread, now.foreign - prev.foreign, (unsigned)now.camera_id,
		(unsigned)now.link_mbps);
	const double fps = window_s > 0.0 ? (double)frames / window_s : 0.0;
	cb_jxs_state().publish_window(s, (uint32_t)(fps * 100.0 + 0.5), bpp_x100, now.lost_slices - prev.lost_slices,
				      undecodable, now.dropped_unread - prev.dropped_unread,
				      now.report_errors - prev.report_errors, -1);
}

/* Sleep up to `ns` in 50 ms steps, returning early when the source stops. */
static void jxs_sleep(jxs_source *s, uint64_t ns)
{
	const uint64_t end = os_gettime_ns() + ns;
	while (s->running.load() && os_gettime_ns() < end)
		os_sleep_ms(50);
}

static void jxs_thread(jxs_source *s)
{
	os_set_thread_name("jxs-source");
	{
		/* in the table from the start: a source no sender reaches reads DOWN, not absent */
		uint64_t gen0 = 0;
		jxs_state_entry(s, jxs_read_config(s, &gen0));
	}
	cbjxs_receiver *rx = nullptr;
	uint64_t open_gen = 0;
	cb_jxs_config open_cfg = {};
	cbjxs_stats prev = {};
	uint64_t last_status_ns = 0;
	uint64_t last_frame_ns = 0;
	/* libobs starts every source as connected (obs_source_init), so start from true: the first
	 * evaluation then pushes "not connected" for a source no sender has reached yet. */
	bool connected = true;
	bool open_failure_logged = false;
	uint64_t reopens_window = 0;
	uint64_t opens = 0;
	uint32_t width = 0, height = 0;
	char err[256];
	/* The LOCK indicator's connected flag follows the frames, on every path (a port that stays
	 * taken, a failing socket) -- never only on the frame path. */
	auto note_connected = [&](uint64_t now) {
		const bool c = cb_jxs_connected(now, last_frame_ns);
		if (c == connected)
			return;
		connected = c;
		jxs_set_connected(s, c);
		obs_log(LOG_INFO, "jxs-source '%s': %s (issue 1387)", obs_source_get_name(s->source),
			c ? "frames arriving" : "not connected -- no frame for 3 s (or none yet)");
	};

	while (s->running.load()) {
		uint64_t gen = 0;
		const cb_jxs_config cfg = jxs_read_config(s, &gen);
		if (rx && gen != open_gen && cb_jxs_reopen_needed(open_cfg, cfg)) {
			obs_log(LOG_INFO, "jxs-source '%s': settings changed -- reopening the receiver (issue 1387)",
				obs_source_get_name(s->source));
			cbjxs_close(rx);
			rx = nullptr;
		}
		open_gen = gen;
		if (rx && cfg.max_bpp != open_cfg.max_bpp) {
			/* only the report's cap changes: the socket, the decoder and the report sequence stay */
			cbjxs_set_max_bpp(rx, cfg.max_bpp);
			obs_log(LOG_INFO, "jxs-source '%s': max bpp %u -> %u (0 = no limit), sent in the next report (issue 1387)",
				obs_source_get_name(s->source), (unsigned)open_cfg.max_bpp, (unsigned)cfg.max_bpp);
			open_cfg.max_bpp = cfg.max_bpp;
		}
		if (!rx) {
			err[0] = '\0';
			rx = cbjxs_open(cfg.port, cfg.camera, cfg.max_bpp, err, sizeof err);
			if (!rx) {
				if (!open_failure_logged)
					obs_log(LOG_WARNING,
						"jxs-source '%s': cannot listen on UDP %u: %s -- retrying every second (issue 1387)",
						obs_source_get_name(s->source), (unsigned)cfg.port, err);
				open_failure_logged = true;
				note_connected(os_gettime_ns());
				jxs_sleep(s, JXS_REOPEN_DELAY_NS);
				continue;
			}
			open_failure_logged = false;
			open_cfg = cfg;
			jxs_state_entry(s, cfg);
			if (opens++ > 0)
				reopens_window++;
			cbjxs_get_stats(rx, &prev);
			last_status_ns = os_gettime_ns();
			obs_log(LOG_INFO,
				"jxs-source '%s': listening on UDP %u (camera %u, 0 = the first heard; max bpp %u, 0 = no limit; receive buffer %" PRId64 " bytes) -- JPEG XS on the genlock FIFO, the receiver report back to the sender (ABI %u) (issue 1387)",
				obs_source_get_name(s->source), (unsigned)cbjxs_local_port(rx), (unsigned)cfg.camera,
				(unsigned)cfg.max_bpp, cbjxs_recv_buffer_bytes(rx), cbjxs_abi_version());
		}

		cbjxs_frame f = {};
		const int r = cbjxs_next_frame(rx, JXS_POLL_MS, &f);
		const uint64_t now = os_gettime_ns();
		if (r == 1) {
			jxs_output_frame(s, f);
			width = f.width;
			height = f.height;
			last_frame_ns = now;
			cb_jxs_state().note_frame(s, now);
		} else if (r < 0) {
			char e[256] = "";
			cbjxs_take_error(rx, e, sizeof e);
			obs_log(LOG_WARNING, "jxs-source '%s': receiver failed (%d): %s -- reopening (issue 1387)",
				obs_source_get_name(s->source), r, e);
			cbjxs_close(rx);
			rx = nullptr;
			note_connected(now);
			jxs_sleep(s, JXS_REOPEN_DELAY_NS);
			continue;
		}
		note_connected(now);
		if (now - last_status_ns >= CB_JXS_STATUS_PERIOD_NS) {
			cbjxs_stats st = {};
			cbjxs_get_stats(rx, &st);
			jxs_state_entry(s, open_cfg);
			jxs_log_status(s, st, prev, (double)(now - last_status_ns) / 1e9, width, height, reopens_window);
			char e[256] = "";
			if (cbjxs_take_error(rx, e, sizeof e) > 0)
				obs_log(LOG_WARNING, "jxs-source '%s': last error: %s (issue 1387)",
					obs_source_get_name(s->source), e);
			prev = st;
			reopens_window = 0;
			last_status_ns = now;
		}
	}
	if (rx)
		cbjxs_close(rx);
	jxs_set_connected(s, false);
	cb_jxs_state().remove(s);
}

static const char *jxs_source_getname(void *)
{
	return "Camera-box JPEG XS (issue 1387)";
}

static void jxs_source_getdefaults(obs_data_t *settings)
{
	obs_data_set_default_int(settings, PROP_JXS_PORT, CB_JXS_PORT_DEFAULT);
	obs_data_set_default_int(settings, PROP_JXS_CAMERA, 0);
	obs_data_set_default_int(settings, PROP_JXS_MAX_BPP, 0);
	obs_data_set_default_bool(settings, PROP_GENLOCK_FIFO, true);
	obs_data_set_default_int(settings, PROP_GENLOCK_LATENCY_MS_SRC, CB_JXS_LATENCY_MS_MIN);
}

static obs_properties_t *jxs_source_getproperties(void *)
{
	obs_properties_t *props = obs_properties_create();
	obs_properties_add_int(props, PROP_JXS_PORT, "UDP port", 1, 65535, 1);
	obs_properties_add_int(props, PROP_JXS_CAMERA, "Camera id (CAM<N>, 0 = the first heard)", 0, 255, 1);
	obs_properties_add_int(props, PROP_JXS_MAX_BPP, "Highest bpp taken (0 = no limit)", 0, CB_JXS_MAX_BPP_LIMIT,
			       1);
	obs_properties_add_bool(props, PROP_GENLOCK_FIFO, "Genlock");
	obs_properties_add_int(props, PROP_GENLOCK_LATENCY_MS_SRC, "Genlock latency (ms)", CB_JXS_LATENCY_MS_MIN,
			       CB_JXS_LATENCY_MS_MAX, 1);
	return props;
}

/* Runs on the graphics thread (async video source): store, never block. */
static void jxs_source_update(void *data, obs_data_t *settings)
{
	auto s = static_cast<jxs_source *>(data);
	const cb_jxs_config cfg = {cb_jxs_port(obs_data_get_int(settings, PROP_JXS_PORT)),
				   cb_jxs_camera(obs_data_get_int(settings, PROP_JXS_CAMERA)),
				   cb_jxs_max_bpp(obs_data_get_int(settings, PROP_JXS_MAX_BPP))};
	{
		std::lock_guard<std::mutex> lock(s->config_mutex);
		s->config = cfg;
		s->config_gen++;
	}
	const jxs_genlock_api &g = jxs_genlock();
	if (g.set_fifo)
		g.set_fifo(s->source, obs_data_get_bool(settings, PROP_GENLOCK_FIFO));
	if (g.set_latency_ms)
		g.set_latency_ms(s->source,
				 cb_jxs_clamp_latency_ms(obs_data_get_int(settings, PROP_GENLOCK_LATENCY_MS_SRC)));
}

static void *jxs_source_create(obs_data_t *settings, obs_source_t *source)
{
	auto s = new jxs_source();
	s->source = source;
	jxs_source_update(s, settings);
	s->running = true;
	s->thread = std::thread(jxs_thread, s);
	return s;
}

static void jxs_source_destroy(void *data)
{
	auto s = static_cast<jxs_source *>(data);
	s->running = false;
	if (s->thread.joinable())
		s->thread.join();
	delete s;
}

struct obs_source_info create_jxs_source_info()
{
	struct obs_source_info info = {};
	info.id = JXS_SOURCE_ID;
	info.type = OBS_SOURCE_TYPE_INPUT;
	info.output_flags = OBS_SOURCE_ASYNC_VIDEO | OBS_SOURCE_DO_NOT_DUPLICATE;
	info.get_name = jxs_source_getname;
	info.get_defaults = jxs_source_getdefaults;
	info.get_properties = jxs_source_getproperties;
	info.update = jxs_source_update;
	info.create = jxs_source_create;
	info.destroy = jxs_source_destroy;
	info.icon_type = OBS_ICON_TYPE_CAMERA;
	return info;
}

/* camera-box issue 1387: the JPEG XS state table, read by the frontend's JPEG XS indicator once a
 * second. A libobs proc handler on the global handler (no new link between the plugin and the
 * frontend); a build without this plugin part (the Windows bundle) has no such procedure, and the
 * frontend reads that as OFF. The returned JSON is a copy: nothing outside the table's mutex is
 * shared. */
static void jxs_state_proc(void *, calldata_t *cd)
{
	const std::string json = cb_jxs_state().json(os_gettime_ns());
	calldata_set_string(cd, "json", json.c_str());
}

void camera_box_jxs_state_register()
{
	proc_handler_add(obs_get_proc_handler(), "void camera_box_jxs_state(out string json)", jxs_state_proc,
			 nullptr);
	obs_log(LOG_INFO, "jxs-state-proc: camera_box_jxs_state registered on the global proc handler (issue 1387)");
}
