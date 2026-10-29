/******************************************************************************
	camera-box issue 1387 slice 3: the JPEG XS PROGRAM output of the strih OBS.

	The strih program reaches stream-snv as NDI (`NDI 2ME PGM`, SpeedHQ). This output sends the
	same program at JPEG XS quality over the camera-box link (our own UDP, the 16-byte header,
	each slice on the wire the moment it is coded, the automatic bpp on the receiver's report),
	BESIDE the NDI output, which stays (owner decision 6488036928).

	- It takes the program's raw video the way the NDI output does (`raw_video` on libobs' video
	  output thread) in the canvas format as it is (NV12 / I420 / I422 / I444: JPEG XS codes
	  4:2:0, 4:2:2 and 4:4:4 as they are).
	- Every frame is stamped with genlock_emit_timecode_100ns (genlock-emit-timecode.h), the ONE
	  stamp the NDI output puts on the same frame, so the stream side's camera_box_jxs_source
	  places it on the per-second grid exactly like `NDI 2ME PGM`.
	- The frame goes to jxs/send-ffi, the C ABI over camera-box's own sender (src/jxs_send*): one
	  copy into one of two reusable pictures on this thread, never a wait; with both in use the
	  frame is dropped and counted (dropped_busy). Coding, slicing, UDP GSO and the auto bpp run on
	  the sender's own two threads.

	The output is created by the plugin at FINISHED_LOADING (obs-websocket can start, stop and
	configure an existing output, never create one) from <module config>/jxs-output.json, and is
	started only when that file says "enabled": true. It is stopped before a profile change
	(a video reset refuses active outputs) and at exit.

	Built only where the strih Linux build sets CAMERA_BOX_JXS_SOURCE (linux-genlock.yml, the
	strih variant), like the source: the sender is Linux-only (UDP GSO).
******************************************************************************/

#include <obs-module.h>
#include <util/bmem.h>
#include <util/platform.h>

#include <atomic>
#include <cinttypes>
#include <cstdio>
#include <mutex>
#include <string>

#include "plugin-support.h"
#include "camera_box_jxs_send.h"
#include "genlock-emit-timecode.h"
#include "jxs-output-decide.h"

#define JXS_OUTPUT_ID "camera_box_jxs_output"
#define JXS_OUTPUT_NAME "camera-box JPEG XS program"

#define PROP_JXS_TARGET "jxs_target"
#define PROP_JXS_STREAM_ID "jxs_stream_id"
#define PROP_JXS_BPP_MIN "jxs_bpp_min"
#define PROP_JXS_BPP_MAX "jxs_bpp_max"
#define PROP_JXS_CORES "jxs_cores"
#define PROP_JXS_PACE_MBPS "jxs_pace_mbps"

struct jxs_output {
	obs_output_t *output = nullptr;
	/* settings (update() may run on any thread; start() reads them) */
	std::mutex settings_mutex;
	std::string target;
	std::string cores;
	uint8_t stream_id = CB_JXS_OUT_STREAM_ID_DEFAULT;
	uint8_t bpp_min = 0;
	uint8_t bpp_max = 0;
	uint32_t pace_mbps = CB_JXS_OUT_PACE_MBPS_DEFAULT;
	/* running state */
	std::mutex tx_mutex;
	cbjxs_sender *tx = nullptr;
	std::atomic<bool> started{false};
	uint32_t send_format = 0;
	double framerate = 0.0;
	/* raw_video's own counters of the current status window */
	uint64_t offered = 0;
	uint64_t taken = 0;
	uint64_t not_taken = 0;
	uint64_t refused = 0;
	uint64_t stamp_dup = 0;
	/* the last stamp handed to the sender (0 = none), the duplicate guard */
	int64_t last_stamp_100ns = 0;
};

/* The sender's own lines (link ON, encoder open, every bpp step, the wedge alarm) into the OBS
 * log; its 5 s status line is logged by the output itself (jxs_output_log_status). */
static void jxs_output_forward_log(int level, const char *message)
{
	if (!cb_jxs_out_forward_line(message))
		return;
	const int obs_level = level == CBJXS_SEND_LOG_ERROR  ? LOG_ERROR
			      : level == CBJXS_SEND_LOG_WARN ? LOG_WARNING
							     : LOG_INFO;
	obs_log(obs_level, "jxs-output: %s", message);
}

/* The sender's picture format for the canvas format; 0 = not one JPEG XS codes. */
static uint32_t jxs_output_send_format(enum video_format format)
{
	switch (format) {
	case VIDEO_FORMAT_NV12:
		return CBJXS_SEND_NV12;
	case VIDEO_FORMAT_I420:
		return CBJXS_SEND_I420;
	case VIDEO_FORMAT_I422:
		return CBJXS_SEND_I422;
	case VIDEO_FORMAT_I444:
		return CBJXS_SEND_I444;
	default:
		return 0;
	}
}

static const char *jxs_output_getname(void *)
{
	return "Camera-box JPEG XS program output (issue 1387)";
}

static void jxs_output_getdefaults(obs_data_t *settings)
{
	obs_data_set_default_string(settings, PROP_JXS_TARGET, "");
	obs_data_set_default_int(settings, PROP_JXS_STREAM_ID, CB_JXS_OUT_STREAM_ID_DEFAULT);
	obs_data_set_default_int(settings, PROP_JXS_BPP_MIN, 0);
	obs_data_set_default_int(settings, PROP_JXS_BPP_MAX, 0);
	obs_data_set_default_string(settings, PROP_JXS_CORES, "");
	obs_data_set_default_int(settings, PROP_JXS_PACE_MBPS, CB_JXS_OUT_PACE_MBPS_DEFAULT);
}

static obs_properties_t *jxs_output_getproperties(void *)
{
	obs_properties_t *props = obs_properties_create();
	obs_properties_add_text(props, PROP_JXS_TARGET, "Target (host:port, IPv4)", OBS_TEXT_DEFAULT);
	obs_properties_add_int(props, PROP_JXS_STREAM_ID, "Stream id (outside the CAM range)", 1, 255, 1);
	obs_properties_add_int(props, PROP_JXS_BPP_MIN, "Lowest bpp (0 = 4)", 0, CB_JXS_OUT_MAX_BPP, 1);
	obs_properties_add_int(props, PROP_JXS_BPP_MAX, "Highest bpp (0 = 8; = lowest: fixed)", 0,
			       CB_JXS_OUT_MAX_BPP, 1);
	obs_properties_add_text(props, PROP_JXS_CORES, "Encoder cores (cpulist, empty = all)", OBS_TEXT_DEFAULT);
	obs_properties_add_int(props, PROP_JXS_PACE_MBPS, "Send pace, Mb/s of wire (0 = line rate)", 0,
			       CB_JXS_OUT_PACE_MBPS_MAX, 50);
	return props;
}

/* Stores the settings; they apply at the next start. */
static void jxs_output_update(void *data, obs_data_t *settings)
{
	auto o = static_cast<jxs_output *>(data);
	std::lock_guard<std::mutex> lock(o->settings_mutex);
	o->target = obs_data_get_string(settings, PROP_JXS_TARGET);
	o->cores = obs_data_get_string(settings, PROP_JXS_CORES);
	o->stream_id = cb_jxs_out_stream_id(obs_data_get_int(settings, PROP_JXS_STREAM_ID));
	o->bpp_min = cb_jxs_out_bpp(obs_data_get_int(settings, PROP_JXS_BPP_MIN));
	o->bpp_max = cb_jxs_out_bpp(obs_data_get_int(settings, PROP_JXS_BPP_MAX));
	o->pace_mbps = cb_jxs_out_pace_mbps(obs_data_get_int(settings, PROP_JXS_PACE_MBPS));
}

static void *jxs_output_create(obs_data_t *settings, obs_output_t *output)
{
	auto o = new jxs_output();
	o->output = output;
	jxs_output_update(o, settings);
	return o;
}

static bool jxs_output_start(void *data)
{
	auto o = static_cast<jxs_output *>(data);
	const char *name = obs_output_get_name(o->output);
	if (o->started.load())
		return false;
	obs_output_set_last_error(o->output, "");
	video_t *video = obs_output_video(o->output);
	if (!video) {
		obs_output_set_last_error(o->output, "no program video");
		return false;
	}
	const uint32_t send_format = jxs_output_send_format(video_output_get_format(video));
	if (send_format == 0) {
		obs_log(LOG_ERROR,
			"jxs-output '%s': the canvas pixel format %d is not one JPEG XS codes (NV12, I420, I422, I444) -- not started (issue 1387)",
			name, (int)video_output_get_format(video));
		obs_output_set_last_error(o->output, "the canvas pixel format is not NV12/I420/I422/I444");
		return false;
	}
	const struct video_output_info *vinfo = video_output_get_info(video);
	/* an sRGB canvas is converted to YUV with the same BT.709 matrix */
	const bool cs_709 = vinfo && (vinfo->colorspace == VIDEO_CS_709 || vinfo->colorspace == VIDEO_CS_SRGB);
	const bool range_partial = vinfo && vinfo->range != VIDEO_RANGE_FULL;
	if (!cb_jxs_out_colour_ok(cs_709, range_partial)) {
		obs_log(LOG_ERROR,
			"jxs-output '%s': the canvas is colour space %d range %d, the receiving source shows BT.709 limited range -- not started (issue 1387)",
			name, vinfo ? (int)vinfo->colorspace : -1, vinfo ? (int)vinfo->range : -1);
		obs_output_set_last_error(o->output, "the canvas is not BT.709 limited range");
		return false;
	}
	const double framerate = video_output_get_frame_rate(video);
	std::string target, cores;
	cbjxs_send_config cfg = {};
	{
		std::lock_guard<std::mutex> lock(o->settings_mutex);
		target = o->target;
		cores = o->cores;
		cfg.stream_id = o->stream_id;
		cfg.bpp_min = o->bpp_min;
		cfg.bpp_max = o->bpp_max;
		cfg.pace_mbps = o->pace_mbps;
	}
	cfg.target = target.c_str();
	cfg.cores = cores.c_str();
	cfg.emit_fps = cb_jxs_out_fps(framerate);
	char err[256] = "";
	cbjxs_sender *tx = cbjxs_send_open(&cfg, err, sizeof err);
	if (!tx) {
		obs_log(LOG_ERROR, "jxs-output '%s': cannot start the sender to '%s': %s (issue 1387)", name,
			target.c_str(), err);
		obs_output_set_last_error(o->output, err);
		return false;
	}
	{
		std::lock_guard<std::mutex> lock(o->tx_mutex);
		o->tx = tx;
		o->send_format = send_format;
		o->framerate = framerate;
		o->offered = o->taken = o->not_taken = o->refused = o->stamp_dup = 0;
		o->last_stamp_100ns = 0;
	}
	o->started = true;
	if (!obs_output_begin_data_capture(o->output, 0)) {
		o->started = false;
		{
			std::lock_guard<std::mutex> lock(o->tx_mutex);
			o->tx = nullptr;
		}
		cbjxs_send_close(tx);
		obs_log(LOG_ERROR, "jxs-output '%s': the program capture did not start (issue 1387)", name);
		obs_output_set_last_error(o->output, "the program capture did not start");
		return false;
	}
	/* every frame carries the genlock wall stamp: the LOCK indicator counts it as a sender */
	obs_output_set_genlock_wall_stamping(o->output, true);
	obs_log(LOG_INFO,
		"jxs-output '%s': started -- the program %ux%u %s at %.2f fps to %s (stream id %u, bpp %u..%u, 0 = the sender's default; cores '%s', empty = all; pace %u Mb/s, 0 = line rate), each frame stamped with the genlock emit timecode of the NDI output, send ABI %u (issue 1387)",
		name, video_output_get_width(video), video_output_get_height(video),
		get_video_format_name(video_output_get_format(video)), framerate, target.c_str(),
		(unsigned)cfg.stream_id, (unsigned)cfg.bpp_min, (unsigned)cfg.bpp_max, cores.c_str(),
		(unsigned)cfg.pace_mbps,
		cbjxs_send_abi_version());
	return true;
}

/* The 5 s status line: the sender's window plus raw_video's own counters (tx_mutex held). */
static void jxs_output_log_status(jxs_output *o, const cbjxs_send_stats &st)
{
	obs_log(LOG_INFO,
		"jxs-output '%s': fps=%.2f frames=%" PRIu64 " dropped_busy=%" PRIu64
		" offered=%" PRIu64 " taken=%" PRIu64 " not_taken=%" PRIu64 " refused=%" PRIu64
		" stamp_dup=%" PRIu64
		" encode_ms_avg/max=%.2f/%.2f first_slice_ms_avg=%.2f copy_ms_avg/max=%.2f/%.2f bpp=%u auto=%s"
		" reports=%" PRIu64 " report_age_s=%.1f send_errors=%" PRIu64 " encode_errors=%" PRIu64
		" pace_mbps=%u wire_ms_avg/max=%.2f/%.2f (issue 1387)",
		obs_output_get_name(o->output), st.window_s > 0.0 ? (double)st.frames / st.window_s : 0.0,
		st.frames, st.dropped_busy, o->offered, o->taken, o->not_taken, o->refused, o->stamp_dup,
		st.encode_ms_avg,
		st.encode_ms_max, st.first_slice_ms_avg, st.copy_ms_avg, st.copy_ms_max, st.bpp,
		st.auto_bpp ? "on" : "off", st.reports, st.report_age_s, st.send_errors, st.encode_errors,
		st.pace_mbps, st.wire_ms_avg, st.wire_ms_max);
	o->offered = o->taken = o->not_taken = o->refused = o->stamp_dup = 0;
}

/* libobs' video output thread: one copy into the sender, never a wait. */
static void jxs_output_rawvideo(void *data, struct video_data *frame)
{
	auto o = static_cast<jxs_output *>(data);
	if (!o->started.load() || !frame)
		return;
	std::lock_guard<std::mutex> lock(o->tx_mutex);
	if (!o->tx)
		return;
	video_t *video = obs_output_video(o->output);
	cbjxs_send_picture pic = {};
	pic.format = o->send_format;
	pic.width = video_output_get_width(video);
	pic.height = video_output_get_height(video);
	for (int i = 0; i < 3; i++) {
		pic.planes[i] = frame->data[i];
		pic.strides[i] = frame->linesize[i];
	}
	/* the NDI output's stamp function, read at this callback (it runs after the NDI output's on
	 * the same thread, so near a slot edge it can be the next slot) */
	pic.timecode_100ns = genlock_emit_timecode_100ns(o->framerate);
	o->offered++;
	if (!cb_jxs_out_stamp_fresh(o->last_stamp_100ns, pic.timecode_100ns)) {
		/* a second frame in one slot: the receiver would drop it unseen, count it here */
		o->stamp_dup++;
		return;
	}
	const int r = cbjxs_send_frame(o->tx, &pic);
	if (r == 1) {
		/* only a frame really handed over claims its slot: after a busy drop the slot's next
		 * frame may still go */
		o->last_stamp_100ns = pic.timecode_100ns;
		o->taken++;
	} else if (r == 0) {
		o->not_taken++;
	} else {
		if (o->refused++ == 0) {
			char e[256] = "";
			cbjxs_send_take_error(o->tx, e, sizeof e);
			obs_log(LOG_WARNING, "jxs-output '%s': frame not sent (%d): %s (issue 1387)",
				obs_output_get_name(o->output), r, e);
		}
	}
	cbjxs_send_stats st = {};
	if (cbjxs_send_get_stats(o->tx, &st) == 1)
		jxs_output_log_status(o, st);
}

static void jxs_output_stop(void *data, uint64_t)
{
	auto o = static_cast<jxs_output *>(data);
	if (!o->started.exchange(false))
		return;
	obs_output_set_genlock_wall_stamping(o->output, false);
	/* libobs ends the capture on its own thread, so a raw_video can still run after this returns:
	 * tx_mutex + tx = nullptr below is what keeps it off a closed sender (keep both) */
	obs_output_end_data_capture(o->output);
	cbjxs_sender *tx = nullptr;
	{
		std::lock_guard<std::mutex> lock(o->tx_mutex);
		tx = o->tx;
		o->tx = nullptr;
	}
	/* bounded: the sender's threads stop within ~2 s even with a wedged codec call */
	cbjxs_send_close(tx);
	obs_log(LOG_INFO, "jxs-output '%s': stopped (issue 1387)", obs_output_get_name(o->output));
}

static void jxs_output_destroy(void *data)
{
	auto o = static_cast<jxs_output *>(data);
	if (o->started.load())
		jxs_output_stop(o, 0);
	delete o;
}

struct obs_output_info create_jxs_output_info()
{
	/* the sender's own lines reach the OBS log (installed once, here at registration) */
	if (!cbjxs_send_set_log(jxs_output_forward_log))
		obs_log(LOG_WARNING,
			"jxs-output: another Rust log subscriber owns the process -- the sender's own lines are not logged (issue 1387)");
	struct obs_output_info info = {};
	info.id = JXS_OUTPUT_ID;
	info.flags = OBS_OUTPUT_VIDEO;
	info.get_name = jxs_output_getname;
	info.get_defaults = jxs_output_getdefaults;
	info.get_properties = jxs_output_getproperties;
	info.create = jxs_output_create;
	info.update = jxs_output_update;
	info.start = jxs_output_start;
	info.stop = jxs_output_stop;
	info.destroy = jxs_output_destroy;
	info.raw_video = jxs_output_rawvideo;
	return info;
}

/* The one program output, created at FINISHED_LOADING. */
static obs_output_t *g_jxs_output = nullptr;

void camera_box_jxs_output_init()
{
	if (g_jxs_output)
		return;
	char *path = obs_module_config_path("jxs-output.json");
	obs_data_t *file = path ? obs_data_create_from_json_file_safe(path, "bak") : nullptr;
	obs_data_t *settings = obs_data_create();
	if (file)
		obs_data_apply(settings, file);
	const bool enabled = file && obs_data_get_bool(file, "enabled");
	g_jxs_output = obs_output_create(JXS_OUTPUT_ID, JXS_OUTPUT_NAME, settings, nullptr);
	obs_log(LOG_INFO,
		"jxs-output: '%s' created from %s (%s; enabled=%s, target '%s') -- obs-websocket StartOutput / StopOutput / SetOutputSettings drive it (issue 1387)",
		JXS_OUTPUT_NAME, path ? path : "(no config path)", file ? "read" : "absent: defaults",
		enabled ? "true" : "false", obs_data_get_string(settings, PROP_JXS_TARGET));
	if (enabled && g_jxs_output) {
		if (!obs_output_start(g_jxs_output))
			obs_log(LOG_ERROR, "jxs-output: '%s' did not start: %s (issue 1387)", JXS_OUTPUT_NAME,
				obs_output_get_last_error(g_jxs_output) ? obs_output_get_last_error(g_jxs_output)
									: "?");
	}
	obs_data_release(settings);
	obs_data_release(file);
	bfree(path);
}

void camera_box_jxs_output_deinit()
{
	if (!g_jxs_output)
		return;
	obs_output_stop(g_jxs_output);
	obs_output_release(g_jxs_output);
	g_jxs_output = nullptr;
}
