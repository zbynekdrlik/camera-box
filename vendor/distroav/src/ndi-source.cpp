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
#include "ndi-finder.h"

#include <util/platform.h>
#include <util/threading.h>

#include <QDesktopServices>
#include <QUrl>

#include <thread>
#include <cstdio>  /* camera-box #97: snprintf for the preload ms info-text label */
#include <cstdlib> /* camera-box #97: getenv/strtol for OBS_GENLOCK_PRELOAD_FRAMES default */

#define PROP_SOURCE "ndi_source_name"
#define PROP_BEHAVIOR "ndi_behavior"
#define PROP_TIMEOUT "ndi_behavior_timeout"
#define PROP_BANDWIDTH "ndi_bw_mode"
#define PROP_SYNC "ndi_sync"
#define PROP_FRAMESYNC "ndi_framesync"
#define PROP_GENLOCK_FIFO "genlock_fifo"            /* camera-box #42 */
#define PROP_GENLOCK_PRELOAD "genlock_preload"      /* camera-box #97: video-delay slider */
#define PROP_GENLOCK_PRELOAD_MS "genlock_preload_ms" /* camera-box #97: read-only ms label */
#define PROP_GENLOCK_PRELOAD_MAX 128                /* mirrors libobs GENLOCK_PRELOAD_MAX (#97) */
#define PROP_GENLOCK_LATENCY_MS "genlock_latency_ms" /* camera-box #235: read-only GLOBAL latency label */
#define PROP_GENLOCK_LATENCY_MS_SRC "genlock_latency_ms_src" /* camera-box #245: EDITABLE per-source latency (ms) override */
#define PROP_GENLOCK_LATENCY_MS_SRC_HINT "genlock_latency_ms_src_hint" /* camera-box #245: read-only frame-equiv hint */
#define PROP_GENLOCK_SOURCE_LATENCY_MS_MAX 2000 /* mirrors libobs GENLOCK_SOURCE_LATENCY_MS_MAX (#245) */

/* camera-box #42: resolve the genlock export at RUNTIME. On Windows the
 * DistroAV build system fetches stock OBS SDK headers (no genlock symbols), so
 * a link-time call cannot build; runtime binding works against any headers AND
 * leaves the plugin loadable on a stock OBS (checkbox becomes an inert no-op
 * with a loud warning instead of a load failure). */
#ifdef _WIN32
#include <windows.h>
#else
#include <dlfcn.h>
#endif
/* camera-box #97: resolve an OBS genlock export at RUNTIME by name. Same rationale
 * as the genlock-fifo resolver below: the Windows DistroAV build fetches stock OBS
 * SDK headers (no genlock symbols), so a link-time call cannot build; runtime
 * binding works against any headers AND keeps the plugin loadable on a stock OBS
 * (the control becomes an inert no-op with a loud warning instead of a load fail). */
static void *resolve_obs_export(const char *name)
{
#ifdef _WIN32
	HMODULE m = GetModuleHandleA("obs.dll");
	if (!m)
		m = GetModuleHandleA("libobs.dll");
	return m ? (void *)GetProcAddress(m, name) : nullptr;
#else
	return dlsym(RTLD_DEFAULT, name);
#endif
}

typedef void (*set_genlock_fifo_fn)(obs_source_t *, bool);
static set_genlock_fifo_fn resolve_set_genlock_fifo()
{
	static set_genlock_fifo_fn fn = nullptr;
	static bool tried = false;
	if (!tried) {
		tried = true;
		fn = (set_genlock_fifo_fn)resolve_obs_export("obs_source_set_genlock_fifo");
		if (!fn)
			obs_log(LOG_WARNING,
				"genlock: obs_source_set_genlock_fifo not exported by this OBS build — "
				"the Genlock checkbox is inert (stock OBS?)");
	}
	return fn;
}

/* camera-box #97: the default genlock-preload depth for a NDI source the operator
 * never touched — derived from OBS_GENLOCK_PRELOAD_FRAMES so the #70 env mechanism
 * ("tune depth without a rebuild") still works on the very DistroAV sources it was
 * built for. Without this, a hardcoded default of 1 in ndi_source_getdefaults would
 * OVERWRITE the libobs env seed on every scene load (defaults are applied before
 * create → update reads the setting → set_preload), silently reverting a non-1 env
 * value to 1 (review finding). Clamped to [0, PROP_GENLOCK_PRELOAD_MAX]; invalid /
 * unset → 1 (GENLOCK_PRELOAD_DEFAULT, mirrors the libobs parse). */
static long genlock_preload_env_default()
{
	const char *env = getenv("OBS_GENLOCK_PRELOAD_FRAMES");
	if (!env || !*env)
		return 1;
	char *end = nullptr;
	long v = strtol(env, &end, 10);
	if (end == env || *end != '\0' || v < 0)
		return 1;
	if (v > PROP_GENLOCK_PRELOAD_MAX)
		return PROP_GENLOCK_PRELOAD_MAX;
	return v;
}

/* camera-box #235: resolve the SINGLE genlock latency (ms) from the canonical
 * OBS_GENLOCK_LATENCY_MS knob, falling back to the OBS_GENLOCK_RESERVE_MS back-compat
 * alias (canonical wins; same strtol contract + [0,100] clamp as the libobs side). 0 =
 * disabled (whole-frame preload fallback). Mirror of src/probe/genlock.rs
 * resolve_latency_ms — used to drive the read-only "genlock latency = N ms (≈ M frames)"
 * label so the operator reads the ACTUAL deployed latency in the source properties. */
static long resolve_genlock_latency_ms()
{
	const long LATENCY_MS_MAX = 100; /* == GENLOCK_LATENCY_MS_MAX */
	auto parse = [&](const char *env, bool &set) -> long {
		set = false;
		if (!env || !*env)
			return 0;
		char *end = nullptr;
		long v = strtol(env, &end, 10);
		if (end == env || *end != '\0' || v < 0)
			return 0; /* unset/junk/negative -> not set */
		set = true;
		return v > LATENCY_MS_MAX ? LATENCY_MS_MAX : v;
	};
	bool set = false;
	long ms = parse(getenv("OBS_GENLOCK_LATENCY_MS"), set);
	if (set)
		return ms; /* the canonical knob wins */
	return parse(getenv("OBS_GENLOCK_RESERVE_MS"), set); /* back-compat alias */
}

/* camera-box #235: format the read-only "genlock latency = N ms (≈ M frames @ Ffps)"
 * label — MS PRIMARY, the whole-frame equivalent in PARENTHESES (the user's exact ask).
 * Sourced from the resolved env latency, NOT a per-source value (latency is a launch-time
 * env). Mirror of src/probe/genlock.rs format_latency_label. */
static void format_genlock_latency_label(char *buf, size_t buflen)
{
	const long ms = resolve_genlock_latency_ms();
	struct obs_video_info ovi;
	if (obs_get_video_info(&ovi) && ovi.fps_num != 0) {
		const double fps = (double)ovi.fps_num / (double)ovi.fps_den;
		/* frames = round(ms * fps_num / (1000 * fps_den)) — inverse of preload_to_ms. */
		const unsigned long long num = (unsigned long long)ms * ovi.fps_num;
		const unsigned long long den = 1000ULL * ovi.fps_den;
		const unsigned long long frames = (num + den / 2) / den;
		snprintf(buf, buflen, "genlock latency = %ld ms (≈ %llu frames @ %.3f fps)", ms, frames, fps);
	} else {
		snprintf(buf, buflen, "genlock latency = %ld ms (≈ ? frames — fps unknown)", ms);
	}
}

/* camera-box #245: format the read-only frame-equivalent hint for the EDITABLE per-source
 * genlock latency (ms) field — "≈ M frames @ Ffps" (the user's ask: show the frame
 * equivalent in parens). 0 means the source follows the GLOBAL default, so the hint says
 * so instead of "≈ 0 frames". Description-only; never written back into settings. */
static void format_src_latency_label(obs_data_t *settings, char *buf, size_t buflen)
{
	const long long ms = obs_data_get_int(settings, PROP_GENLOCK_LATENCY_MS_SRC);
	if (ms <= 0) {
		snprintf(buf, buflen, "0 = use global default (see Genlock latency above)");
		return;
	}
	struct obs_video_info ovi;
	if (obs_get_video_info(&ovi) && ovi.fps_num != 0) {
		const double fps = (double)ovi.fps_num / (double)ovi.fps_den;
		const unsigned long long num = (unsigned long long)ms * ovi.fps_num;
		const unsigned long long den = 1000ULL * ovi.fps_den;
		const unsigned long long frames = (num + den / 2) / den;
		snprintf(buf, buflen, "≈ %llu frames @ %.3f fps", frames, fps);
	} else {
		snprintf(buf, buflen, "≈ ? frames — output fps unknown");
	}
}

/* camera-box #97: per-source genlock preload (video-delay) setter, runtime-resolved. */
typedef void (*set_genlock_preload_fn)(obs_source_t *, uint32_t);
static set_genlock_preload_fn resolve_set_genlock_preload()
{
	static set_genlock_preload_fn fn = nullptr;
	static bool tried = false;
	if (!tried) {
		tried = true;
		fn = (set_genlock_preload_fn)resolve_obs_export("obs_source_set_genlock_preload");
		if (!fn)
			obs_log(LOG_WARNING,
				"genlock: obs_source_set_genlock_preload not exported by this OBS build — "
				"the Genlock preload (video delay) slider is inert (stock OBS?)");
	}
	return fn;
}

/* camera-box #245: per-source genlock LATENCY (ms) override setter, runtime-resolved by
 * name — same rationale as the preload setter: the Windows DistroAV build fetches stock
 * OBS SDK headers (no genlock symbols), so a link-time call cannot build; resolve at
 * runtime so the plugin still loads on any OBS (the export is just inert on stock). */
typedef void (*set_genlock_latency_ms_fn)(obs_source_t *, uint32_t);
static set_genlock_latency_ms_fn resolve_set_genlock_latency_ms()
{
	static set_genlock_latency_ms_fn fn = nullptr;
	static bool tried = false;
	if (!tried) {
		tried = true;
		fn = (set_genlock_latency_ms_fn)resolve_obs_export("obs_source_set_genlock_latency_ms");
		if (!fn)
			obs_log(LOG_WARNING,
				"genlock: obs_source_set_genlock_latency_ms not exported by this OBS build — "
				"the per-source Genlock latency (ms) field is inert (stock OBS?)");
	}
	return fn;
}
#define PROP_HW_ACCEL "ndi_recv_hw_accel"
#define PROP_FIX_ALPHA "ndi_fix_alpha_blending"
#define PROP_YUV_RANGE "yuv_range"
#define PROP_YUV_COLORSPACE "yuv_colorspace"
#define PROP_LATENCY "latency"
#define PROP_AUDIO "ndi_audio"
#define PROP_PTZ "ndi_ptz"
#define PROP_PAN "ndi_pan"
#define PROP_TILT "ndi_tilt"
#define PROP_ZOOM "ndi_zoom"

#define PROP_BW_UNDEFINED -1
#define PROP_BW_HIGHEST 0
#define PROP_BW_LOWEST 1
#define PROP_BW_AUDIO_ONLY 2

#define PROP_BEHAVIOR_KEEP_ACTIVE 0
#define PROP_BEHAVIOR_STOP_RESUME_BLANK 1
#define PROP_BEHAVIOR_STOP_RESUME_LAST_FRAME 2

#define PROP_TIMEOUT_CLEAR_CONTENT 0
#define PROP_TIMEOUT_KEEP_CONTENT 1

// sync mode "Internal" got removed 2020/04/28 ccbdf30f4929969fe58ede691b3030d1fc5ef590
#define PROP_SYNC_INTERNAL 0
#define PROP_SYNC_NDI_TIMESTAMP 1
#define PROP_SYNC_NDI_SOURCE_TIMECODE 2

#define PROP_YUV_RANGE_PARTIAL 1
#define PROP_YUV_RANGE_FULL 2

#define PROP_YUV_SPACE_BT601 1
#define PROP_YUV_SPACE_BT709 2
#define PROP_YUV_SPACE_BT2100 3

#define PROP_LATENCY_UNDEFINED -1
#define PROP_LATENCY_NORMAL 0
#define PROP_LATENCY_LOW 1
#define PROP_LATENCY_LOWEST 2

typedef struct ptz_t {
	bool enabled;
	float pan;
	float tilt;
	float zoom;

	ptz_t(bool enabled_ = false, float pan_ = 0.0f, float tilt_ = 0.0f, float zoom_ = 0.0f)
		: enabled(enabled_),
		  pan(pan_),
		  tilt(tilt_),
		  zoom(zoom_)
	{
	}
} ptz_t;

typedef struct ndi_source_config_t {
	bool reset_ndi_receiver = true;
	// Initialize value to true to ensure a receiver reset on OBS launch.

	//
	// Changes that require the NDI receiver to be reset:
	//
	char *ndi_receiver_name;
	char *ndi_source_name;
	int bandwidth;
	int latency;
	bool framesync_enabled;
	bool hw_accel_enabled;

	//
	// Changes that do NOT require the NDI receiver to be reset:
	//
	int behavior;
	int timeout_action;
	int sync_mode;
	video_range_type yuv_range;
	video_colorspace yuv_colorspace;
	bool audio_enabled;
	ptz_t ptz;
	NDIlib_tally_t tally;
} ndi_source_config_t;

typedef struct ndi_source_t {
	obs_source_t *obs_source;
	ndi_source_config_t config;

	/* camera-box #93: serialises the config-mutation section of ndi_source_update
	 * (the bfree/bstrdup of the name strings + scalar config writes + thread
	 * start/stop) against the av_thread's reset_ndi_receiver block, which reads
	 * those strings into recv_desc. Without it, update (UI/obs-websocket thread)
	 * frees the string the av_thread is mid-read on → STATUS_HEAP_CORRUPTION
	 * (the strih OBS crash). Held for microseconds only — NEVER across the
	 * blocking recv_capture_v3 — and NEVER on the render path (OBS already
	 * guards the async frame queue with source->async_mutex). */
	pthread_mutex_t config_mutex;

	bool running;
	pthread_t av_thread;

	uint32_t width;
	uint32_t height;

	uint64_t last_frame_timestamp;
} ndi_source_t;

static obs_source_t *find_filter_by_id(obs_source_t *context, const char *id)
{
	if (!context)
		return nullptr;

	typedef struct {
		const char *query;
		obs_source_t *result;
	} search_context_t;

	search_context_t filter_search = {};
	filter_search.query = id;
	filter_search.result = nullptr;

	obs_source_enum_filters(
		context,
		[](obs_source_t *, obs_source_t *filter, void *param) {
			search_context_t *filter_search_ = static_cast<search_context_t *>(param);
			const char *obs_source_id = obs_source_get_id(filter);
			if (strcmp(obs_source_id, filter_search_->query) == 0) {
				obs_source_get_ref(filter);
				filter_search_->result = filter;
			}
		},
		&filter_search);

	return filter_search.result;
}

static speaker_layout channel_count_to_layout(int channels)
{
	switch (channels) {
	case 1:
		return SPEAKERS_MONO;
	case 2:
		return SPEAKERS_STEREO;
	case 3:
		return SPEAKERS_2POINT1;
	case 4:
#if LIBOBS_API_VER >= MAKE_SEMANTIC_VERSION(21, 0, 0)
		return SPEAKERS_4POINT0;
#else
		return SPEAKERS_QUAD;
#endif
	case 5:
		return SPEAKERS_4POINT1;
	case 6:
		return SPEAKERS_5POINT1;
	case 8:
		return SPEAKERS_7POINT1;
	default:
		return SPEAKERS_UNKNOWN;
	}
}

static video_colorspace prop_to_colorspace(int index)
{
	switch (index) {
	case PROP_YUV_SPACE_BT601:
		return VIDEO_CS_601;
	case PROP_YUV_SPACE_BT2100:
		return VIDEO_CS_2100_HLG;
	default:
	case PROP_YUV_SPACE_BT709:
		return VIDEO_CS_709;
	}
}

static video_range_type prop_to_range_type(int index)
{
	switch (index) {
	case PROP_YUV_RANGE_FULL:
		return VIDEO_RANGE_FULL;
	default:
	case PROP_YUV_RANGE_PARTIAL:
		return VIDEO_RANGE_PARTIAL;
	}
}

const char *ndi_source_getname(void *)
{
	return obs_module_text("NDIPlugin.NDISourceName");
}

/* camera-box #97: format the read-only "≈ N ms (@ F fps)" video-delay label for the
 * current PROP_GENLOCK_PRELOAD value at the current output fps. Shared by the initial
 * property build (so the label shows immediately on first dialog open, before any
 * callback fires — review finding) AND the slider/checkbox modified_callbacks. The
 * label is for the property DESCRIPTION only; it is NEVER written back into settings
 * (no derived string persisted into the saved scene JSON). When genlock_fifo is OFF
 * the preload is inert (ready_async_frame's genlock branch is skipped), so the label
 * says so rather than implying a delay that is not applied. */
static void format_preload_ms_label(obs_data_t *settings, char *buf, size_t buflen)
{
	const long long frames = obs_data_get_int(settings, PROP_GENLOCK_PRELOAD);
	const bool fifo_on = obs_data_get_bool(settings, PROP_GENLOCK_FIFO);
	struct obs_video_info ovi;
	if (obs_get_video_info(&ovi) && ovi.fps_num != 0) {
		const unsigned long long ms = (unsigned long long)frames * 1000ULL * ovi.fps_den / ovi.fps_num;
		const double fps = (double)ovi.fps_num / (double)ovi.fps_den;
		if (fifo_on)
			snprintf(buf, buflen, "≈ %llu ms (@ %.3f fps)", ms, fps);
		else
			snprintf(buf, buflen, "≈ %llu ms (@ %.3f fps) — enable Genlock to apply", ms, fps);
	} else {
		snprintf(buf, buflen, "≈ ? ms (output fps unknown)");
	}
}

/* camera-box #150: FORCE every certified zero-loss genlock value into `settings`,
 * regardless of any saved scene value, UI edit, or harness-set value. Called from
 * ndi_source_update ONLY when genlock_fifo is on (NOT from ndi_source_getdefaults —
 * defaults run before create with no per-source genlock state to gate on; update is
 * the authoritative enforcement point and ndi_source_create calls update at the end,
 * so a newly-added genlock source is forced at creation), so a genlock NDI source —
 * prod, probe, or a newly-added one, in ANY scene — is correct by construction. This
 * closes the misconfig class root-caused live 2026-06-22 (an
 * incompletely-configured probe ingest decoded 0 at the strih output while the
 * fully-configured prod input decoded 100%, same NDI source). The LEGITIMATE operator
 * knobs — PROP_SOURCE, PROP_GENLOCK_PRELOAD and the #245 PROP_GENLOCK_LATENCY_MS_SRC
 * per-source latency override — are NEVER touched here, and
 * PROP_GENLOCK_FIFO itself is the operator's gate (left as the operator set it). The
 * certified values were read live from the working prod input `NDI cam5`:
 *   ndi_sync=2 (SOURCE_TIMECODE / source timing), ndi_behavior=2 (LAST_FRAME),
 *   ndi_bw_mode=0 (highest), latency=0 (NORMAL), ndi_recv_hw_accel=true,
 *   ndi_audio=false, ndi_framesync=false, ndi_fix_alpha_blending=false,
 *   yuv_range=partial, yuv_colorspace=BT.709, timeout=KEEP_CONTENT.
 * Writing into `settings` (not just s->config) means the values persist into the saved
 * scene JSON on the next OBS save, so the source stays correct across restarts. */
static void force_genlock_certified_settings(obs_data_t *settings)
{
	obs_data_set_int(settings, PROP_SYNC, PROP_SYNC_NDI_SOURCE_TIMECODE);
	obs_data_set_int(settings, PROP_BEHAVIOR, PROP_BEHAVIOR_STOP_RESUME_LAST_FRAME);
	obs_data_set_int(settings, PROP_BANDWIDTH, PROP_BW_HIGHEST);
	obs_data_set_int(settings, PROP_LATENCY, PROP_LATENCY_NORMAL);
	obs_data_set_int(settings, PROP_TIMEOUT, PROP_TIMEOUT_KEEP_CONTENT);
	obs_data_set_int(settings, PROP_YUV_RANGE, PROP_YUV_RANGE_PARTIAL);
	obs_data_set_int(settings, PROP_YUV_COLORSPACE, PROP_YUV_SPACE_BT709);
	obs_data_set_bool(settings, PROP_HW_ACCEL, true);
	obs_data_set_bool(settings, PROP_AUDIO, false);
	obs_data_set_bool(settings, PROP_FRAMESYNC, false);
	obs_data_set_bool(settings, PROP_FIX_ALPHA, false);
}

/* camera-box #150: hide every non-essential property when genlock is enabled, so a
 * human or a tool CANNOT set a forced key wrong from the UI — leaving ONLY the two
 * legitimate knobs (PROP_SOURCE selection + PROP_GENLOCK_PRELOAD video delay) plus the
 * PROP_GENLOCK_FIFO toggle visible. When genlock is OFF the FULL normal property set is
 * shown (non-genlock aux/preview inputs — NDI 2ME PVW / Bible / Camera info, ndi_sync=1
 * — are unaffected, #150 constraint #3). Returns true (properties UI changed → refresh)
 * so it can drive the genlock-checkbox modified-callback directly. PROP_SOURCE,
 * PROP_GENLOCK_FIFO, PROP_GENLOCK_PRELOAD and the #245 editable PROP_GENLOCK_LATENCY_MS_SRC
 * per-source latency field (+ the read-only ms label + the #235 read-only genlock-latency
 * label + the #245 frame-equiv hint) are deliberately NEVER hidden — they are legitimate
 * operator knobs that must stay settable under the genlock lockdown. */
static bool apply_genlock_lockdown_visibility(obs_properties_t *props, bool genlock_on)
{
	/* The forced (non-essential) properties: shown only when genlock is OFF. */
	static const char *const locked_props[] = {
		PROP_BEHAVIOR, PROP_BANDWIDTH, PROP_SYNC,           PROP_FRAMESYNC,
		PROP_HW_ACCEL, PROP_LATENCY,   PROP_AUDIO,          PROP_YUV_RANGE,
		PROP_YUV_COLORSPACE, PROP_FIX_ALPHA, PROP_TIMEOUT,
	};
	for (const char *name : locked_props) {
		obs_property_t *p = obs_properties_get(props, name);
		if (p)
			obs_property_set_visible(p, !genlock_on);
	}
	return true;
}

obs_properties_t *ndi_source_getproperties(void *data)
{
	auto s = (ndi_source_t *)data;
	obs_log(LOG_DEBUG, "+ndi_source_getproperties(…)");

	obs_properties_t *props = obs_properties_create();

	obs_property_t *source_list = obs_properties_add_list(props, PROP_SOURCE,
							      obs_module_text("NDIPlugin.SourceProps.SourceName"),
							      OBS_COMBO_TYPE_EDITABLE, OBS_COMBO_FORMAT_STRING);
	NDIFinder finder;
	// Create a callback that is called when the NDI source list is complete
	auto finder_callback = [source_list, s](void *ndi_names) {
		auto ndi_sources = (std::vector<std::string> *)ndi_names;
		for (auto &source : *ndi_sources) {
			obs_property_list_add_string(source_list, source.c_str(), source.c_str());
		}
		obs_source_update_properties(s->obs_source);
	};
	auto ndi_sources = finder.getNDISourceList(finder_callback);
	for (auto &source : ndi_sources) {
		obs_property_list_add_string(source_list, source.c_str(), source.c_str());
	}

	obs_property_t *behavior_list = obs_properties_add_list(props, PROP_BEHAVIOR,
								obs_module_text("NDIPlugin.SourceProps.Behavior"),
								OBS_COMBO_TYPE_LIST, OBS_COMBO_FORMAT_INT);
	obs_property_list_add_int(behavior_list, obs_module_text("NDIPlugin.SourceProps.Behavior.KeepActive"),
				  PROP_BEHAVIOR_KEEP_ACTIVE);
	obs_property_list_add_int(behavior_list, obs_module_text("NDIPlugin.SourceProps.Behavior.StopResumeBlank"),
				  PROP_BEHAVIOR_STOP_RESUME_BLANK);
	obs_property_list_add_int(behavior_list, obs_module_text("NDIPlugin.SourceProps.Behavior.StopResumeLastFrame"),
				  PROP_BEHAVIOR_STOP_RESUME_LAST_FRAME);

	obs_property_t *timeout_list = obs_properties_add_list(props, PROP_TIMEOUT,
							       obs_module_text("NDIPlugin.SourceProps.Timeout"),
							       OBS_COMBO_TYPE_LIST, OBS_COMBO_FORMAT_INT);
	obs_property_list_add_int(timeout_list, obs_module_text("NDIPlugin.SourceProps.Timeout.KeepContent"),
				  PROP_TIMEOUT_KEEP_CONTENT);
	obs_property_list_add_int(timeout_list, obs_module_text("NDIPlugin.SourceProps.Timeout.ClearContent"),
				  PROP_TIMEOUT_CLEAR_CONTENT);

	obs_property_t *bw_modes = obs_properties_add_list(props, PROP_BANDWIDTH,
							   obs_module_text("NDIPlugin.SourceProps.Bandwidth"),
							   OBS_COMBO_TYPE_LIST, OBS_COMBO_FORMAT_INT);
	obs_property_list_add_int(bw_modes, obs_module_text("NDIPlugin.BWMode.Highest"), PROP_BW_HIGHEST);
	obs_property_list_add_int(bw_modes, obs_module_text("NDIPlugin.BWMode.Lowest"), PROP_BW_LOWEST);
	obs_property_list_add_int(bw_modes, obs_module_text("NDIPlugin.BWMode.AudioOnly"), PROP_BW_AUDIO_ONLY);
	obs_property_set_modified_callback(bw_modes, [](obs_properties_t *props_, obs_property_t *,
							obs_data_t *settings_) {
		bool is_audio_only = (obs_data_get_int(settings_, PROP_BANDWIDTH) == PROP_BW_AUDIO_ONLY);
		/* camera-box #150: this callback also governs the two YUV controls' visibility,
		 * so it MUST be genlock-aware — otherwise, under the genlock lockdown (which
		 * forces bandwidth=HIGHEST, i.e. NOT audio-only), a property refresh that
		 * re-fires this callback would set the YUV controls visible again and LEAK the
		 * lockdown for exactly those two props. When genlock is on, keep them hidden
		 * regardless of audio-only — the lockdown is authoritative. */
		bool genlock_on = obs_data_get_bool(settings_, PROP_GENLOCK_FIFO);
		bool yuv_visible = !is_audio_only && !genlock_on;

		obs_property_t *yuv_range = obs_properties_get(props_, PROP_YUV_RANGE);
		obs_property_t *yuv_colorspace = obs_properties_get(props_, PROP_YUV_COLORSPACE);

		obs_property_set_visible(yuv_range, yuv_visible);
		obs_property_set_visible(yuv_colorspace, yuv_visible);

		return true;
	});

	obs_property_t *sync_modes = obs_properties_add_list(props, PROP_SYNC,
							     obs_module_text("NDIPlugin.SourceProps.Sync"),
							     OBS_COMBO_TYPE_LIST, OBS_COMBO_FORMAT_INT);
	obs_property_list_add_int(sync_modes, obs_module_text("NDIPlugin.SyncMode.NDITimestamp"),
				  PROP_SYNC_NDI_TIMESTAMP);
	obs_property_list_add_int(sync_modes, obs_module_text("NDIPlugin.SyncMode.NDISourceTimecode"),
				  PROP_SYNC_NDI_SOURCE_TIMECODE);

	obs_properties_add_bool(props, PROP_FRAMESYNC, obs_module_text("NDIPlugin.NDIFrameSync"));

	obs_properties_add_bool(props, PROP_GENLOCK_FIFO, "Genlock (FIFO frame consumption, camera-box #42)");

	/* camera-box #235: the SINGLE user-facing genlock latency display — read-only info
	 * text "genlock latency = N ms (≈ M frames @ Ffps)" (MS PRIMARY, frames in parens).
	 * Sourced from the resolved env latency (OBS_GENLOCK_LATENCY_MS, alias
	 * OBS_GENLOCK_RESERVE_MS) so the operator reads the ACTUAL deployed latency. This is
	 * the consolidated knob; the preload slider below is now an INTERNAL/legacy control
	 * (auto-derived under the ms knob — not a competing latency knob). */
	obs_property_t *latency_label =
		obs_properties_add_text(props, PROP_GENLOCK_LATENCY_MS, "Genlock latency (global default)", OBS_TEXT_INFO);
	{
		char lat_buf[160];
		format_genlock_latency_label(lat_buf, sizeof(lat_buf));
		obs_property_set_description(latency_label, lat_buf);
	}

	/* camera-box #245: the EDITABLE PER-SOURCE genlock latency override (ms). #235
	 * collapsed latency to ONE GLOBAL env knob and lost per-source control — the
	 * live-event regression (operator could not set 1000 ms on a single source while the
	 * others stayed low). This int field restores it IN THE OBS SOURCE UI (no env): each
	 * NDI source holds its OWN latency. 0 = follow the global default shown above. Range
	 * 0..2000 ms (a deliberate per-source VIDEO DELAY, far above the global sub-frame
	 * reserve). Applied at runtime via obs_source_set_genlock_latency_ms (resolved by
	 * name so the plugin still builds against stock SDK headers). The read-only hint below
	 * shows the frame-equivalent (the user's "frame-equivalent in parens" ask). */
	obs_property_t *src_latency =
		obs_properties_add_int(props, PROP_GENLOCK_LATENCY_MS_SRC,
				       "Genlock latency (per source, 0 = use global default)", 0,
				       PROP_GENLOCK_SOURCE_LATENCY_MS_MAX, 1);
	obs_property_int_set_suffix(src_latency, " ms");
	obs_property_t *src_latency_hint =
		obs_properties_add_text(props, PROP_GENLOCK_LATENCY_MS_SRC_HINT, "↳ per-source delay", OBS_TEXT_INFO);
	/* Seed the frame-equiv hint from the current settings so it shows on FIRST dialog
	 * open (OBS does not fire modified-callbacks at initial population — the #97 lesson).
	 * The data ptr is the source on a populated dialog; guard the null-data add case. */
	if (s && s->obs_source) {
		obs_data_t *cur = obs_source_get_settings(s->obs_source);
		if (cur) {
			char init_buf[160];
			format_src_latency_label(cur, init_buf, sizeof(init_buf));
			obs_property_set_description(src_latency_hint, init_buf);
			obs_data_release(cur);
		}
	}
	/* Recompute the frame-equiv hint whenever the per-source ms field changes. Non-
	 * capturing lambda so it converts to the C obs_property_modified_t pointer. */
	auto update_src_latency_hint = [](obs_properties_t *props_, obs_property_t *, obs_data_t *settings_) -> bool {
		char buf[160];
		format_src_latency_label(settings_, buf, sizeof(buf));
		obs_property_t *hint = obs_properties_get(props_, PROP_GENLOCK_LATENCY_MS_SRC_HINT);
		if (hint)
			obs_property_set_description(hint, buf);
		return true; /* properties UI changed -> refresh */
	};
	obs_property_set_modified_callback(src_latency, update_src_latency_hint);

	/* camera-box #97/#235: the per-source genlock preload (FIFO depth). #235 demoted this
	 * from a user latency knob to an INTERNAL/legacy frame control: when the ms latency
	 * knob is set the depth is auto-derived (the ms deadline holds the latency, not the
	 * preload), so this slider only governs the legacy whole-frame fallback (latency_ms=0)
	 * path. Kept for back-compat + the legacy video-delay use; the read-only ms hint below
	 * shows its frame→ms equivalent. */
	obs_property_t *preload_slider =
		obs_properties_add_int_slider(props, PROP_GENLOCK_PRELOAD,
					      "Genlock preload (internal FIFO depth — legacy frame control)", 0,
					      PROP_GENLOCK_PRELOAD_MAX, 1);
	obs_property_t *preload_ms = obs_properties_add_text(props, PROP_GENLOCK_PRELOAD_MS, "↳ delay", OBS_TEXT_INFO);
	/* camera-box #97: set the ms label immediately from the current settings so it
	 * shows on FIRST dialog open — OBS does not fire modified_callbacks at initial
	 * property population, so without this the operator would see the bare "↳ delay"
	 * placeholder until they move the slider (review finding). The data ptr is the
	 * source on a populated dialog; guard for the null-data (add-source) case. */
	if (s && s->obs_source) {
		obs_data_t *cur = obs_source_get_settings(s->obs_source);
		if (cur) {
			char init_buf[160];
			format_preload_ms_label(cur, init_buf, sizeof(init_buf));
			obs_property_set_description(preload_ms, init_buf);
			obs_data_release(cur);
		}
	}
	/* Recompute the read-only ms label whenever EITHER the preload slider OR the
	 * genlock-fifo checkbox changes, via the shared formatter (description only — never
	 * written back into settings). Non-capturing lambda so it converts to the C
	 * obs_property_modified_t function pointer. */
	auto update_preload_ms = [](obs_properties_t *props_, obs_property_t *, obs_data_t *settings_) -> bool {
		char buf[160];
		format_preload_ms_label(settings_, buf, sizeof(buf));
		obs_property_t *ms_prop = obs_properties_get(props_, PROP_GENLOCK_PRELOAD_MS);
		if (ms_prop)
			obs_property_set_description(ms_prop, buf);
		return true; /* properties UI changed -> refresh */
	};
	obs_property_set_modified_callback(preload_slider, update_preload_ms);
	/* camera-box #150 + #97: when the genlock-fifo checkbox is toggled, do BOTH —
	 * update the preload ms-label hint AND re-apply the lockdown visibility so the
	 * non-essential properties hide (genlock on) / re-appear (genlock off) live. A
	 * property has a single modified-callback, so the two actions are combined here.
	 * Non-capturing lambda so it converts to the C obs_property_modified_t pointer. */
	auto on_genlock_fifo_changed = [](obs_properties_t *props_, obs_property_t *,
					  obs_data_t *settings_) -> bool {
		char buf[160];
		format_preload_ms_label(settings_, buf, sizeof(buf));
		obs_property_t *ms_prop = obs_properties_get(props_, PROP_GENLOCK_PRELOAD_MS);
		if (ms_prop)
			obs_property_set_description(ms_prop, buf);
		apply_genlock_lockdown_visibility(props_, obs_data_get_bool(settings_, PROP_GENLOCK_FIFO));
		return true; /* properties UI changed -> refresh */
	};
	obs_property_t *genlock_fifo_prop = obs_properties_get(props, PROP_GENLOCK_FIFO);
	if (genlock_fifo_prop)
		obs_property_set_modified_callback(genlock_fifo_prop, on_genlock_fifo_changed);

	obs_properties_add_bool(props, PROP_HW_ACCEL, obs_module_text("NDIPlugin.SourceProps.HWAccel"));

	obs_properties_add_bool(props, PROP_FIX_ALPHA, obs_module_text("NDIPlugin.SourceProps.AlphaBlendingFix"));

	obs_property_t *yuv_ranges = obs_properties_add_list(props, PROP_YUV_RANGE,
							     obs_module_text("NDIPlugin.SourceProps.ColorRange"),
							     OBS_COMBO_TYPE_LIST, OBS_COMBO_FORMAT_INT);
	obs_property_list_add_int(yuv_ranges, obs_module_text("NDIPlugin.SourceProps.ColorRange.Partial"),
				  PROP_YUV_RANGE_PARTIAL);
	obs_property_list_add_int(yuv_ranges, obs_module_text("NDIPlugin.SourceProps.ColorRange.Full"),
				  PROP_YUV_RANGE_FULL);

	obs_property_t *yuv_spaces = obs_properties_add_list(props, PROP_YUV_COLORSPACE,
							     obs_module_text("NDIPlugin.SourceProps.ColorSpace"),
							     OBS_COMBO_TYPE_LIST, OBS_COMBO_FORMAT_INT);
	obs_property_list_add_int(yuv_spaces, "BT.709", PROP_YUV_SPACE_BT709);
	obs_property_list_add_int(yuv_spaces, "BT.601", PROP_YUV_SPACE_BT601);
	obs_property_list_add_int(yuv_spaces, "BT.2100", PROP_YUV_SPACE_BT2100);

	obs_property_t *latency_modes = obs_properties_add_list(props, PROP_LATENCY,
								obs_module_text("NDIPlugin.SourceProps.Latency"),
								OBS_COMBO_TYPE_LIST, OBS_COMBO_FORMAT_INT);
	obs_property_list_add_int(latency_modes, obs_module_text("NDIPlugin.SourceProps.Latency.Normal"),
				  PROP_LATENCY_NORMAL);
	obs_property_list_add_int(latency_modes, obs_module_text("NDIPlugin.SourceProps.Latency.Low"),
				  PROP_LATENCY_LOW);
	obs_property_list_add_int(latency_modes, obs_module_text("NDIPlugin.SourceProps.Latency.Lowest"),
				  PROP_LATENCY_LOWEST);

	obs_properties_add_bool(props, PROP_AUDIO, obs_module_text("NDIPlugin.SourceProps.Audio"));

	obs_properties_t *group_ptz = obs_properties_create();
	obs_properties_add_float_slider(group_ptz, PROP_PAN, obs_module_text("NDIPlugin.SourceProps.Pan"), -1.0, 1.0,
					0.001);
	obs_properties_add_float_slider(group_ptz, PROP_TILT, obs_module_text("NDIPlugin.SourceProps.Tilt"), -1.0, 1.0,
					0.001);
	obs_properties_add_float_slider(group_ptz, PROP_ZOOM, obs_module_text("NDIPlugin.SourceProps.Zoom"), 0.0, 1.0,
					0.001);
	obs_properties_add_group(props, PROP_PTZ, obs_module_text("NDIPlugin.SourceProps.PTZ"), OBS_GROUP_CHECKABLE,
				 group_ptz);

	/* camera-box #150: apply the lockdown visibility ONCE on first dialog open from
	 * the source's CURRENT genlock_fifo state — OBS does not fire modified-callbacks
	 * at initial property population, so without this a genlock source would show all
	 * the (forced, non-editable-in-effect) properties until the operator toggled the
	 * checkbox. Guard the null-data (add-source) case: a brand-new source has genlock
	 * off, so the full set shows until the operator enables genlock. */
	bool genlock_on_now = false;
	if (s && s->obs_source) {
		obs_data_t *cur = obs_source_get_settings(s->obs_source);
		if (cur) {
			genlock_on_now = obs_data_get_bool(cur, PROP_GENLOCK_FIFO);
			obs_data_release(cur);
		}
	}
	apply_genlock_lockdown_visibility(props, genlock_on_now);

	obs_log(LOG_DEBUG, "-ndi_source_getproperties(…)");

	return props;
}

void ndi_source_getdefaults(obs_data_t *settings)
{
	obs_log(LOG_DEBUG, "+ndi_source_getdefaults(…)");
	obs_data_set_default_int(settings, PROP_BANDWIDTH, PROP_BW_HIGHEST);
	obs_data_set_default_int(settings, PROP_BEHAVIOR, PROP_BEHAVIOR_STOP_RESUME_LAST_FRAME);
	obs_data_set_default_int(settings, PROP_TIMEOUT, PROP_TIMEOUT_KEEP_CONTENT);
	obs_data_set_default_int(settings, PROP_SYNC, PROP_SYNC_NDI_SOURCE_TIMECODE);
	obs_data_set_default_int(settings, PROP_YUV_RANGE, PROP_YUV_RANGE_PARTIAL);
	obs_data_set_default_int(settings, PROP_YUV_COLORSPACE, PROP_YUV_SPACE_BT709);
	obs_data_set_default_int(settings, PROP_LATENCY, PROP_LATENCY_NORMAL);
	obs_data_set_default_bool(settings, PROP_AUDIO, true);
	/* camera-box #97: default genlock preload (video delay) = the OBS_GENLOCK_PRELOAD_FRAMES
	 * env value (or 1 when unset), matching the libobs #70 env default — so a source the
	 * operator never touches keeps the env-tuned jitter reserve instead of silently
	 * reverting it to 1 on scene load (review finding). */
	obs_data_set_default_int(settings, PROP_GENLOCK_PRELOAD, genlock_preload_env_default());
	/* camera-box #245: the per-source latency override defaults to 0 = follow the global
	 * OBS_GENLOCK_LATENCY_MS default, so a source the operator never touches behaves
	 * exactly as before this field existed (no per-source delay). */
	obs_data_set_default_int(settings, PROP_GENLOCK_LATENCY_MS_SRC, 0);
	obs_log(LOG_DEBUG, "-ndi_source_getdefaults(…)");
}

void deactivate_source_output_video_texture(ndi_source_t *source)
{
	// Per https://docs.obsproject.com/reference-sources#c.obs_source_output_video
	// ```
	// void obs_source_output_video(obs_source_t *source, const struct obs_source_frame *frame)
	// Outputs asynchronous video data. Set to NULL to deactivate the texture.
	// ```
	if (source->width == 0 && source->height == 0)
		return;

	source->width = 0;
	source->height = 0;
	obs_log(LOG_DEBUG, "'%s' deactivate_source_output_video_texture(…)", obs_source_get_name(source->obs_source));
	obs_source_output_video(source->obs_source, NULL);
}

void process_empty_frame(ndi_source_t *source)
{
	if (source->config.timeout_action == PROP_TIMEOUT_KEEP_CONTENT)
		return;

	uint64_t now = os_gettime_ns();

	// 3 second timeout on no new data received for the source
	uint64_t source_timeout = std::chrono::duration_cast<std::chrono::nanoseconds>(std::chrono::seconds(3)).count();

	uint64_t target_timestamp = source->last_frame_timestamp + source_timeout;

	if (now > target_timestamp) {
		deactivate_source_output_video_texture(source);
	}
}

void ndi_source_thread_process_audio3(ndi_source_config_t *config, NDIlib_audio_frame_v3_t *ndi_audio_frame,
				      obs_source_t *obs_source, obs_source_audio *obs_audio_frame);

void ndi_source_thread_process_video2(ndi_source_t *source, NDIlib_video_frame_v2_t *ndi_video_frame,
				      obs_source *obs_source, obs_source_frame *obs_video_frame);

void *ndi_source_thread(void *data)
{
	auto s = (ndi_source_t *)data;
	auto obs_source_name = obs_source_get_name(s->obs_source);
	obs_log(LOG_DEBUG, "'%s' +ndi_source_thread(…)", obs_source_name);

	auto config = Config::Current();
	ptz_t ptz;
	NDIlib_tally_t tally;

	obs_source_audio obs_audio_frame = {};
	obs_source_frame obs_video_frame = {};

	NDIlib_recv_create_v3_t recv_desc;
	recv_desc.allow_video_fields = true;

	/* camera-box #93 (defense in depth): the av_thread owns DUPLICATED copies of
	 * the NDI name strings. recv_desc binds to these owned copies, NEVER to the
	 * live s->config.* pointers — which ndi_source_update frees/reallocs. So even
	 * a future caller that mutates config WITHOUT taking config_mutex cannot
	 * use-after-free the receiver-create path. Refreshed inside the locked
	 * reset_ndi_receiver block; freed on thread exit. */
	char *owned_source_name = nullptr;
	char *owned_receiver_name = nullptr;

	NDIlib_recv_instance_t ndi_receiver = nullptr;
	NDIlib_video_frame_v2_t video_frame;

	NDIlib_metadata_frame_t metadata_frame;
	NDIlib_framesync_instance_t ndi_frame_sync = nullptr;
	NDIlib_audio_frame_v3_t audio_frame;
	NDIlib_frame_type_e frame_received = NDIlib_frame_type_none;

	int64_t timestamp_audio = 0;
	int64_t timestamp_video = 0;

	//
	// Main NDI receiver loop: BEGIN
	//
	while (s->running) {
		//
		// reset_ndi_receiver: BEGIN
		//
		if (s->config.reset_ndi_receiver) {
			//
			// camera-box #93: read the (mutable) config into recv_desc + local
			// snapshots UNDER config_mutex, so ndi_source_update cannot
			// free/realloc the name strings or flip the scalars while we copy
			// them. The lock is dropped before recv_create_v3 — it is held only
			// for the few microseconds of the copy below, NEVER across a blocking
			// NDI call, and NEVER on the render path.
			//
			bool snap_hw_accel_enabled = false;
			bool snap_framesync_enabled = false;
			pthread_mutex_lock(&s->config_mutex);

			s->config.reset_ndi_receiver = false;

			// If config.ndi_receiver_name changed, then so did obs_source_name
			obs_source_name = obs_source_get_name(s->obs_source);

			//
			// Refresh the av_thread-OWNED copies of the name strings (defense in
			// depth): bind recv_desc to these, NOT to the live config.* pointers
			// that ndi_source_update frees. bfree(nullptr) is a safe no-op.
			//
			bfree(owned_source_name);
			owned_source_name = bstrdup(s->config.ndi_source_name);
			bfree(owned_receiver_name);
			owned_receiver_name = bstrdup(s->config.ndi_receiver_name);

			//
			// Update recv_desc.p_ndi_recv_name (owned copy)
			//
			recv_desc.p_ndi_recv_name = owned_receiver_name;
			obs_log(LOG_DEBUG,
				"'%s' ndi_source_thread: reset_ndi_receiver; Setting recv_desc.p_ndi_recv_name='%s'",
				obs_source_name, //
				recv_desc.p_ndi_recv_name);

			//
			// Update recv_desc.source_to_connect_to.p_ndi_name (owned copy)
			//
			recv_desc.source_to_connect_to.p_ndi_name = owned_source_name;
			obs_log(LOG_DEBUG,
				"'%s' ndi_source_thread: reset_ndi_receiver; Setting recv_desc.source_to_connect_to.p_ndi_name='%s'",
				obs_source_name, //
				recv_desc.source_to_connect_to.p_ndi_name);

			//
			// Update recv_desc.bandwidth
			//
			switch (s->config.bandwidth) {
			case PROP_BW_HIGHEST:
			default:
				recv_desc.bandwidth = NDIlib_recv_bandwidth_highest;
				break;
			case PROP_BW_LOWEST:
				recv_desc.bandwidth = NDIlib_recv_bandwidth_lowest;
				break;
			case PROP_BW_AUDIO_ONLY:
				recv_desc.bandwidth = NDIlib_recv_bandwidth_audio_only;
				break;
			}
			obs_log(LOG_DEBUG, "'%s' ndi_source_thread: reset_ndi_receiver; Setting recv_desc.bandwidth=%d",
				obs_source_name, //
				recv_desc.bandwidth);

			//
			// Update recv_desc.latency
			//
			if (s->config.latency == PROP_LATENCY_NORMAL)
				recv_desc.color_format = NDIlib_recv_color_format_UYVY_BGRA;
			else
				recv_desc.color_format = NDIlib_recv_color_format_fastest;
			obs_log(LOG_DEBUG,
				"'%s' ndi_source_thread: reset_ndi_receiver; Setting recv_desc.color_format=%d",
				obs_source_name, //
				recv_desc.color_format);

			video_format_get_parameters(s->config.yuv_colorspace, s->config.yuv_range,
						    obs_video_frame.color_matrix, obs_video_frame.color_range_min,
						    obs_video_frame.color_range_max);

			// Snapshot the remaining reset-relevant scalars while still locked, so
			// the (slower) receiver create/destroy below reads stable values.
			snap_hw_accel_enabled = s->config.hw_accel_enabled;
			snap_framesync_enabled = s->config.framesync_enabled;

			pthread_mutex_unlock(&s->config_mutex);

			//
			// recv_desc is fully populated;
			// now reset the NDI receiver, destroying any existing ndi_frame_sync or ndi_receiver.
			//
			obs_log(LOG_DEBUG, "'%s' ndi_source_thread: reset_ndi_receiver: Resetting NDI receiver…",
				obs_source_name);

			if (ndi_frame_sync) {
				obs_log(LOG_DEBUG, "'%s' ndi_source_thread: ndiLib->framesync_destroy(ndi_frame_sync)",
					obs_source_name);
				ndiLib->framesync_destroy(ndi_frame_sync);
				ndi_frame_sync = nullptr;
			}

			if (ndi_receiver) {
				obs_log(LOG_DEBUG,
					"'%s' ndi_source_thread: reset_ndi_receiver: ndiLib->recv_destroy(ndi_receiver)",
					obs_source_name);
				ndiLib->recv_destroy(ndi_receiver);
				ndi_receiver = nullptr;
			}

			obs_log(LOG_DEBUG,
				"'%s' ndi_source_thread: reset_ndi_receiver: recv_desc = { p_ndi_recv_name='%s', source_to_connect_to.p_ndi_name='%s' }",
				obs_source_name, //
				recv_desc.p_ndi_recv_name, recv_desc.source_to_connect_to.p_ndi_name);
			obs_log(LOG_DEBUG,
				"'%s' ndi_source_thread: reset_ndi_receiver: +ndi_receiver = ndiLib->recv_create_v3(&recv_desc)",
				obs_source_name);

			ndi_receiver = ndiLib->recv_create_v3(&recv_desc);

			obs_log(LOG_DEBUG,
				"'%s' ndi_source_thread: reset_ndi_receiver: -ndi_receiver = ndiLib->recv_create_v3(&recv_desc)",
				obs_source_name);
			if (!ndi_receiver) {
				obs_log(LOG_ERROR, "ERR-407 - Error creating the NDI Receiver '%s' set for '%s'",
					recv_desc.source_to_connect_to.p_ndi_name, obs_source_name);
				obs_log(LOG_DEBUG,
					"'%s' ndi_source_thread: reset_ndi_receiver: Cannot create ndi_receiver for NDI source '%s'",
					obs_source_name, recv_desc.source_to_connect_to.p_ndi_name);
				break;
			}

			if (snap_hw_accel_enabled) {
				//
				// From https://docs.ndi.video/docs/sdk/performance-and-implementation#receiving-video :
				// > * In the modern versions of NDI, there are internal heuristics that attempt to guess whether hardware
				// > acceleration would enable better performance. That said, it is possible to explicitly enable hardware
				// > acceleration if you believe that it would be beneficial for your application. This can be enabled by
				// > sending an XML metadata message to a receiver as follows:
				// >	<ndi_video_codec type="hardware"/>
				//
				// The wording of this says very unambiguously "it is possible to explicitly enable hardware acceleration",
				// but this can in reality only ever be a **REQUEST** to enable. The enable could possibly fail for the
				// obvious reason that the device may not have/support hardware acceleration.
				//
				// Furthermore, there is no documented way to request to *disable* hardware acceleration.
				// I have tried setting the metadata to `<ndi_video_codec type=""/>` or `<ndi_video_codec/>` and it does not
				// crash, but I was unable to confirm if this actually disabled hardware acceleration, and am skeptical that
				// it could/would.
				// So, it seems like there is no way to disable this.
				// I have asked on the NewTek NDI SDK forum here:
				// https://forum.vizrt.com/index.php?threads/any-way-to-explicitly-turn-off-hardware-acceleration.253766/
				//
				// Regardless, it makes little sense to have a checkbox that requests to enable this when
				// checked but do nothing when unchecked.
				// But that is basically what we are going to do here.
				//
				// One other way we try to mitigate this is to reset the NDI receiver when hw_accel_enabled is changed
				// [in `ndi_source_update`]
				// The theory is that the below `recv_send_metadata` is bound to the NDI receiver instance.
				// Destroy that receiver instance and you also destroy the metadata and thus the hardware acceleration.
				// There is no confirmation that this works as theorized.
				//
				NDIlib_metadata_frame_t hwAccelMetadata;
				hwAccelMetadata.p_data = (char *)"<ndi_video_codec type=\"hardware\"/>";
				obs_log(LOG_DEBUG,
					"'%s' ndi_source_thread: reset_ndi_receiver; Sending NDI Hardware Acceleration metadata: '%s'",
					obs_source_name, hwAccelMetadata.p_data);
				ndiLib->recv_send_metadata(ndi_receiver, &hwAccelMetadata);
			}

			if (snap_framesync_enabled) {
				timestamp_audio = 0;
				timestamp_video = 0;
				obs_log(LOG_DEBUG,
					"'%s' ndi_source_thread: +ndi_frame_sync = ndiLib->framesync_create(ndi_receiver)",
					obs_source_name);
				ndi_frame_sync = ndiLib->framesync_create(ndi_receiver);
				obs_log(LOG_DEBUG,
					"'%s' ndi_source_thread: -ndi_frame_sync = ndiLib->framesync_create(ndi_receiver); ndi_frame_sync=%p",
					obs_source_name, //
					ndi_frame_sync);
				if (!ndi_frame_sync) {
					obs_log(LOG_ERROR,
						"ERR-408 - Error creating the NDI Frame Sync for '%s' for '%s'",
						recv_desc.source_to_connect_to.p_ndi_name, obs_source_name);
					obs_log(LOG_DEBUG,
						"'%s' ndi_source_thread: Cannot create ndi_frame_sync for NDI source '%s'",
						obs_source_name, recv_desc.source_to_connect_to.p_ndi_name);
					break;
				}
			}
		}
		//
		// reset_ndi_receiver: END
		//

		//
		// Now that we have a stable usable ndi_receiver,
		// check if there are any connections.
		// If not then micro-pause and restart the loop.
		//
		if (ndiLib->recv_get_no_connections(ndi_receiver) == 0) {
#if 0
			obs_log(LOG_DEBUG,
				"'%s' ndi_source_thread: No connection; sleep and restart loop",
				obs_source_name);
#endif
			process_empty_frame(s);

			// This will also slow down the shutdown of OBS when no NDI feed is received.
			std::this_thread::sleep_for(std::chrono::milliseconds(100));
			continue;
		}

		//
		// Change PTZ: Realtime updated from Source settings UI
		//
		if (s->config.ptz.enabled) {
			const static float tollerance = 0.001f;
			if (fabs(s->config.ptz.pan - ptz.pan) > tollerance ||
			    fabs(s->config.ptz.tilt - ptz.tilt) > tollerance ||
			    fabs(s->config.ptz.zoom - ptz.zoom) > tollerance) {
				ptz = s->config.ptz;
				if (ndiLib->recv_ptz_is_supported(ndi_receiver)) {
					obs_log(LOG_DEBUG,
						"'%s' ndi_source_thread: ptz changed; Sending PTZ pan=%f, tilt=%f, zoom=%f",
						obs_source_name, //
						ptz.pan, ptz.tilt, ptz.zoom);
					ndiLib->recv_ptz_pan_tilt(ndi_receiver, ptz.pan, ptz.tilt);
					ndiLib->recv_ptz_zoom(ndi_receiver, ptz.zoom);
				}
			}
		}

		//
		// Change Tally: Enable/Disable updated from Plugin settings UI
		//
#if 0
		obs_log(LOG_DEBUG, "'%s' t{pre=%d,pro=%d}",
			obs_source_name, //
			s->config.tally2.on_preview,
			s->config.tally2.on_program);
#endif
		if ((config->TallyPreviewEnabled && s->config.tally.on_preview != tally.on_preview) ||
		    (config->TallyProgramEnabled && s->config.tally.on_program != tally.on_program)) {
			tally.on_preview = s->config.tally.on_preview;
			tally.on_program = s->config.tally.on_program;
			obs_log(LOG_INFO, "'%s': Tally status : on_preview=%d, on_program=%d", obs_source_name,
				tally.on_preview, tally.on_program);
			obs_log(LOG_DEBUG,
				"'%s' ndi_source_thread: tally changed; Sending tally on_preview=%d, on_program=%d",
				obs_source_name, tally.on_preview, tally.on_program);
			ndiLib->recv_set_tally(ndi_receiver, &tally);
		}

		//
		// If this source isn't showing in OBS then don't receive any frames from NDI. This occurs when multiple
		// scenes have NDI sources that are not being shown and behavior is set to Keep Active. Without this check,
		// the fps of OBS can decrease dramatically, especially with multiple 4K 60 sources.
		//
		if (!obs_source_showing(s->obs_source)) {
			// Avoid busy-waiting when the source is hidden but kept active.
			std::this_thread::sleep_for(std::chrono::milliseconds(5));
			continue;
		}

		if (ndi_frame_sync) {
			//
			// ndi_frame_sync
			//

			//
			// AUDIO
			//
			audio_frame = {};
			ndiLib->framesync_capture_audio_v2(
				ndi_frame_sync, &audio_frame,
				0,     // "The desired sample rate. 0 to get the source value."
				0,     // "The desired channel count. 0 to get the source value."
				1024); // "The desired sample count. 0 to get the source value."
			// Note: "This function will always return data immediately, inserting silence if no current audio data is present."
			if (audio_frame.p_data && (audio_frame.timestamp > timestamp_audio)) {
				timestamp_audio = audio_frame.timestamp;
				// obs_log(LOG_DEBUG, "%s: New Audio Frame (Framesync ON): ts=%d tc=%d", obs_source_name, audio_frame.timestamp, audio_frame.timecode);
				ndi_source_thread_process_audio3(&s->config, &audio_frame, s->obs_source,
								 &obs_audio_frame);
			}
			ndiLib->framesync_free_audio_v2(ndi_frame_sync, &audio_frame);

			//
			// VIDEO
			//
			video_frame = {};
			ndiLib->framesync_capture_video(ndi_frame_sync, &video_frame,
							NDIlib_frame_format_type_progressive);
			if (video_frame.p_data && (video_frame.timestamp > timestamp_video)) {
				timestamp_video = video_frame.timestamp;
				// obs_log(LOG_DEBUG, "%s: New Video Frame (Framesync ON): ts=%d tc=%d", obs_source_name, video_frame.timestamp, video_frame.timecode);
				ndi_source_thread_process_video2(s, &video_frame, s->obs_source, &obs_video_frame);
			}
			ndiLib->framesync_free_video(ndi_frame_sync, &video_frame);

			// TODO: More accurate sleep that subtracts the duration of this loop iteration?
			std::this_thread::sleep_for(std::chrono::milliseconds(5));
		} else {
			//
			// !ndi_frame_sync
			//
			frame_received =
				ndiLib->recv_capture_v3(ndi_receiver, &video_frame, &audio_frame, nullptr, 100);

			if (frame_received == NDIlib_frame_type_audio) {
				//
				// AUDIO
				//
				// obs_log(LOG_DEBUG, "%s: New Audio Frame (Framesync OFF): ts=%d tc=%d", obs_source_name, audio_frame.timestamp, audio_frame.timecode);
				ndi_source_thread_process_audio3(&s->config, &audio_frame, s->obs_source,
								 &obs_audio_frame);

				ndiLib->recv_free_audio_v3(ndi_receiver, &audio_frame);
				continue;
			}

			if (frame_received == NDIlib_frame_type_video) {
				//
				// VIDEO
				//
				// obs_log(LOG_DEBUG, "%s: New Video Frame (Framesync OFF): ts=%d tc=%d", obs_source_name, video_frame.timestamp, video_frame.timecode);
				ndi_source_thread_process_video2(s, &video_frame, s->obs_source, &obs_video_frame);

				ndiLib->recv_free_video_v2(ndi_receiver, &video_frame);
				continue;
			}

			if (frame_received == NDIlib_frame_type_none) {
				process_empty_frame(s);
			}
		}
	}
	//
	// Main NDI receiver loop: END
	//

	if (ndi_frame_sync) {
		if (ndiLib) {
			obs_log(LOG_DEBUG,
				"'%s' ndi_source_thread: (out of loop) ndiLib->framesync_destroy(ndi_frame_sync)",
				obs_source_name);
			ndiLib->framesync_destroy(ndi_frame_sync);
		}
		ndi_frame_sync = nullptr; // TODO: Investigate if this should be put right after framesync_destroy() ?
		obs_log(LOG_DEBUG, "'%s' ndi_source_thread: Reset NDI Frame Sync", obs_source_name);
	}

	if (ndi_receiver) {
		if (ndiLib) {
			obs_log(LOG_DEBUG, "'%s' ndi_source_thread: ndiLib->recv_destroy(ndi_receiver)",
				obs_source_name);
			ndiLib->recv_destroy(ndi_receiver);
		}
		obs_log(LOG_DEBUG, "'%s' ndi_source_thread: Reset NDI Receiver", obs_source_name);
		ndi_receiver = nullptr;
	}

	// camera-box #93: free the av_thread-owned name copies. bfree(nullptr) is a no-op.
	bfree(owned_source_name);
	owned_source_name = nullptr;
	bfree(owned_receiver_name);
	owned_receiver_name = nullptr;

	obs_log(LOG_DEBUG, "'%s' -ndi_source_thread(…)", obs_source_name);

	return nullptr;
}

void ndi_source_thread_process_audio3(ndi_source_config_t *config, NDIlib_audio_frame_v3_t *ndi_audio_frame,
				      obs_source_t *obs_source, obs_source_audio *obs_audio_frame)
{
	if (!config->audio_enabled) {
		return;
	}

	const int channelCount = ndi_audio_frame->no_channels > 8 ? 8 : ndi_audio_frame->no_channels;

	obs_audio_frame->speakers = channel_count_to_layout(channelCount);

	switch (config->sync_mode) {
	case PROP_SYNC_NDI_TIMESTAMP:
		obs_audio_frame->timestamp = (uint64_t)(ndi_audio_frame->timestamp * 100);
		break;

	case PROP_SYNC_NDI_SOURCE_TIMECODE:
		obs_audio_frame->timestamp = (uint64_t)(ndi_audio_frame->timecode * 100);
		break;
	}

	obs_audio_frame->samples_per_sec = ndi_audio_frame->sample_rate;
	obs_audio_frame->format = AUDIO_FORMAT_FLOAT_PLANAR;
	obs_audio_frame->frames = ndi_audio_frame->no_samples;
	for (int i = 0; i < channelCount; ++i) {
		obs_audio_frame->data[i] =
			(uint8_t *)ndi_audio_frame->p_data + (i * ndi_audio_frame->channel_stride_in_bytes);
	}

	obs_source_output_audio(obs_source, obs_audio_frame);
}

void ndi_source_thread_process_video2(ndi_source_t *source, NDIlib_video_frame_v2_t *ndi_video_frame,
				      obs_source *obs_source, obs_source_frame *obs_video_frame)
{
	switch (ndi_video_frame->FourCC) {
	case NDIlib_FourCC_type_BGRA:
		obs_video_frame->format = VIDEO_FORMAT_BGRA;
		break;

	case NDIlib_FourCC_type_BGRX:
		obs_video_frame->format = VIDEO_FORMAT_BGRX;
		break;

	case NDIlib_FourCC_type_RGBA:
	case NDIlib_FourCC_type_RGBX:
		obs_video_frame->format = VIDEO_FORMAT_RGBA;
		break;

	case NDIlib_FourCC_type_UYVY:
	case NDIlib_FourCC_type_UYVA:
		obs_video_frame->format = VIDEO_FORMAT_UYVY;
		break;

	case NDIlib_FourCC_type_I420:
		obs_video_frame->format = VIDEO_FORMAT_I420;
		break;

	case NDIlib_FourCC_type_NV12:
		obs_video_frame->format = VIDEO_FORMAT_NV12;
		break;

	default:
		obs_log(LOG_ERROR, "ERR-430 - NDI Source uses an unsupported video pixel format: %d.",
			ndi_video_frame->FourCC);
		obs_log(LOG_DEBUG, "ndi_source_thread_process_video2: warning: unsupported video pixel format: %d",
			ndi_video_frame->FourCC);
		break;
	}

	auto config = &source->config;

	switch (config->sync_mode) {
	case PROP_SYNC_NDI_TIMESTAMP:
		obs_video_frame->timestamp = (uint64_t)(ndi_video_frame->timestamp * 100);
		break;

	case PROP_SYNC_NDI_SOURCE_TIMECODE:
		obs_video_frame->timestamp = (uint64_t)(ndi_video_frame->timecode * 100);
		break;
	}

	source->width = ndi_video_frame->xres;
	source->height = ndi_video_frame->yres;
	source->last_frame_timestamp = obs_get_video_frame_time();

	obs_video_frame->width = ndi_video_frame->xres;
	obs_video_frame->height = ndi_video_frame->yres;
	obs_video_frame->linesize[0] = ndi_video_frame->line_stride_in_bytes;
	obs_video_frame->data[0] = ndi_video_frame->p_data;

	obs_source_output_video(obs_source, obs_video_frame);
}

void ndi_source_thread_start(ndi_source_t *s)
{
	s->config.reset_ndi_receiver = true;
	s->running = true;
	pthread_create(&s->av_thread, nullptr, ndi_source_thread, s);
	obs_log(LOG_INFO, "'Started Receiver Thread for OBS source: '%s' and NDI Source Name: %s'",
		obs_source_get_name(s->obs_source), s->config.ndi_source_name);
	obs_log(LOG_DEBUG, "'%s' ndi_source_thread_start: Started A/V ndi_source_thread for NDI source '%s'",
		obs_source_get_name(s->obs_source), s->config.ndi_source_name);
}

void ndi_source_thread_stop(ndi_source_t *s)
{
	if (s->running) {
		s->running = false;
		pthread_join(s->av_thread, NULL);
		auto obs_source = s->obs_source;
		auto obs_source_name = obs_source_get_name(obs_source);
		obs_log(LOG_DEBUG, "'%s' ndi_source_thread_stop: Stopped A/V ndi_source_thread for NDI source '%s'",
			obs_source_name, s->config.ndi_source_name);
	}
}

int safe_strcmp(const char *str1, const char *str2)
{
	if (str1 == str2)
		return 0;
	if (!str1)
		return -1;
	if (!str2)
		return 1;
	return strcmp(str1, str2);
}

bool tally_on_preview(obs_source_t *source)
{
	return (Config::Current())->TallyPreviewEnabled && obs_source_showing(source) && !obs_source_active(source);
}

bool tally_on_program(obs_source_t *source)
{
	return (Config::Current())->TallyProgramEnabled && obs_source_active(source);
}

void ndi_source_update(void *data, obs_data_t *settings)
{
	auto s = (ndi_source_t *)data;
	auto obs_source = s->obs_source;
	auto obs_source_name = obs_source_get_name(obs_source);
	obs_log(LOG_DEBUG, "'%s' +ndi_source_update(…)", obs_source_name);

	//
	// camera-box #93: take config_mutex around the ENTIRE config-mutation section
	// below — the bfree/bstrdup of the name strings and every scalar s->config.*
	// write — so the av_thread's reset_ndi_receiver block can never read a string
	// mid-free or a half-written scalar. This thread (UI / obs-websocket) never
	// calls back into the av_thread, and the lock is RELEASED before the thread
	// start/stop block (whose pthread_join would otherwise deadlock against the
	// av_thread taking config_mutex). It is NOT the render path (OBS guards the
	// async frame queue with source->async_mutex).
	//
	pthread_mutex_lock(&s->config_mutex);

	/* camera-box #150: LOCK DOWN the genlock path. When the operator has enabled
	 * genlock (PROP_GENLOCK_FIFO), FORCE every certified zero-loss value into
	 * `settings` BEFORE any of the per-key reads below — so the rest of this function
	 * (and the persisted scene JSON) sees the certified config regardless of any saved
	 * value, UI edit, or harness-set value. This makes every genlock NDI source —
	 * prod, probe, or a newly-added one, in ANY scene — correct by construction, which
	 * is the root fix for the misconfig class found live 2026-06-22 (an
	 * incompletely-configured probe ingest decoded 0 while the certified prod input
	 * decoded 100% off the SAME NDI source). The forcing is GATED on genlock_fifo, so
	 * non-genlock aux/preview inputs (NDI 2ME PVW / Bible / Camera info, ndi_sync=1)
	 * are entirely unaffected (#150 constraint #3). The two legitimate operator knobs —
	 * PROP_SOURCE and PROP_GENLOCK_PRELOAD — are never touched by the forcer. */
	const bool genlock_lockdown = obs_data_get_bool(settings, PROP_GENLOCK_FIFO);
	if (genlock_lockdown) {
		force_genlock_certified_settings(settings);
		obs_log(LOG_INFO,
			"'%s' ndi_source_update: #150 genlock lockdown ACTIVE — forced certified "
			"values (ndi_sync=2, ndi_behavior=2, ndi_bw_mode=0, latency=0, "
			"ndi_recv_hw_accel=true, ndi_audio=false, ndi_framesync=false, "
			"ndi_fix_alpha_blending=false); only source + genlock preload are operator-set",
			obs_source_name);
	}

	//
	// reset_ndi_receiver: BEGIN
	//

	bool reset_ndi_receiver = false;
	// TODO : Should this ba a if statement and simplify each following check ?

	auto new_ndi_source_name = obs_data_get_string(settings, PROP_SOURCE);
	reset_ndi_receiver |= safe_strcmp(s->config.ndi_source_name, new_ndi_source_name) != 0;
	obs_log(LOG_DEBUG,
		"'%s' ndi_source_update: Check for 'NDI Source Name' changes: new_ndi_source_name='%s' vs config.ndi_source_name='%s'",
		obs_source_name, new_ndi_source_name, s->config.ndi_source_name);

	if (s->config.ndi_source_name != nullptr) {
		bfree(s->config.ndi_source_name);
	}

	s->config.ndi_source_name = bstrdup(new_ndi_source_name);

	auto new_bandwidth = (int)obs_data_get_int(settings, PROP_BANDWIDTH);
	reset_ndi_receiver |= (s->config.bandwidth != new_bandwidth);
	obs_log(LOG_DEBUG,
		"'%s' ndi_source_update: Check for 'Bandwidth' setting changes: new_bandwidth='%d' vs config.bandwidth='%d'",
		obs_source_name, new_bandwidth, s->config.bandwidth);
	s->config.bandwidth = new_bandwidth;

	auto new_latency = (int)obs_data_get_int(settings, PROP_LATENCY);
	reset_ndi_receiver |= (s->config.latency != new_latency);
	obs_log(LOG_DEBUG,
		"'%s' ndi_source_update: Check for 'Latency' setting changes: new_latency='%d' vs config.latency='%d'",
		obs_source_name, new_latency, s->config.latency);
	s->config.latency = new_latency;

	auto new_framesync_enabled = obs_data_get_bool(settings, PROP_FRAMESYNC);
	reset_ndi_receiver |= (s->config.framesync_enabled != new_framesync_enabled);
	obs_log(LOG_DEBUG,
		"'%s' ndi_source_update: Check for 'Framesync' setting changes: new_framesync_enabled='%s' vs config.framesync_enabled='%s'",
		obs_source_name, new_framesync_enabled ? "true" : "false",
		s->config.framesync_enabled ? "true" : "false");
	s->config.framesync_enabled = new_framesync_enabled;

	/* camera-box #42: pure-FIFO consumption of this source's frames by the
	 * compositor (exactly one per render tick, nothing erased ahead). Takes
	 * effect immediately; no receiver reset needed. Runtime-resolved so the
	 * plugin builds against stock SDK headers and loads on any OBS. */
	if (auto set_genlock = resolve_set_genlock_fifo())
		set_genlock(obs_source, obs_data_get_bool(settings, PROP_GENLOCK_FIFO));

	/* camera-box #97: apply the per-source genlock preload (video delay). Runtime-
	 * resolved like the fifo setter. libobs clamps to [0, 128] and writes under
	 * async_mutex, so the live change is crash-safe (the #93 UAF lesson). Persists
	 * in the scene via PROP_GENLOCK_PRELOAD. Floor a negative value (only reachable
	 * via a corrupt/hand-edited scene, never the 0-128 slider) at 0 BEFORE the
	 * uint32_t cast — otherwise e.g. -1 would wrap to UINT32_MAX and libobs would
	 * clamp it to the MAXIMUM delay instead of zero (review finding). */
	if (auto set_preload = resolve_set_genlock_preload()) {
		long long pl = obs_data_get_int(settings, PROP_GENLOCK_PRELOAD);
		if (pl < 0)
			pl = 0;
		set_preload(obs_source, (uint32_t)pl);
	}

	/* camera-box #245: apply the per-source genlock LATENCY override (ms). Runtime-
	 * resolved like the preload setter. libobs clamps to [0, 2000] and writes under
	 * async_mutex (crash-safe, the #93 UAF lesson). 0 = follow the global default.
	 * Persists in the scene via PROP_GENLOCK_LATENCY_MS_SRC. Floor a negative value
	 * (only reachable via a corrupt/hand-edited scene, never the 0-2000 field) at 0
	 * BEFORE the uint32_t cast — otherwise -1 would wrap to UINT32_MAX and libobs would
	 * clamp it to the MAXIMUM delay instead of zero. */
	if (auto set_latency = resolve_set_genlock_latency_ms()) {
		long long ms = obs_data_get_int(settings, PROP_GENLOCK_LATENCY_MS_SRC);
		if (ms < 0)
			ms = 0;
		set_latency(obs_source, (uint32_t)ms);
	}

	auto new_hw_accel_enabled = obs_data_get_bool(settings, PROP_HW_ACCEL);
	reset_ndi_receiver |= (s->config.hw_accel_enabled != new_hw_accel_enabled);
	obs_log(LOG_DEBUG,
		"'%s' ndi_source_update: Check for 'Hardware Acceleration' setting changes: new_hw_accel_enabled='%s' vs config.hw_accel_enabled='%s'",
		obs_source_name, new_hw_accel_enabled ? "true" : "false",
		s->config.hw_accel_enabled ? "true" : "false");
	s->config.hw_accel_enabled = new_hw_accel_enabled;

	auto new_yuv_range = prop_to_range_type((int)obs_data_get_int(settings, PROP_YUV_RANGE));
	reset_ndi_receiver |= (s->config.yuv_range != new_yuv_range);
	obs_log(LOG_DEBUG,
		"'%s' ndi_source_update: Check for 'YUV Range' setting changes: new_yuv_range='%d' vs config.yuv_range='%d'",
		obs_source_name, new_yuv_range, s->config.yuv_range);
	s->config.yuv_range = new_yuv_range;

	auto new_yuv_colorspace = prop_to_colorspace((int)obs_data_get_int(settings, PROP_YUV_COLORSPACE));
	reset_ndi_receiver |= (s->config.yuv_colorspace != new_yuv_colorspace);
	obs_log(LOG_DEBUG,
		"'%s' ndi_source_update: Check for 'YUV Colorspace' setting changes: new_yuv_colorspace='%d' vs config.yuv_colorspace='%d'",
		obs_source_name, new_yuv_colorspace, s->config.yuv_colorspace);
	s->config.yuv_colorspace = new_yuv_colorspace;

	//
	// reset_ndi_receiver: END
	//

#if 0
	// Test overloading these in the config file at:
	// Linux: ~/.config/obs-studio/basic/scenes/...
	// MacOS: ~/Library/Application Support/obs-studio/basic/scenes/...
	// Windows: %APPDATA%\obs-studio\basic\scenes\...
	Example:
	        "name": "NDI™ Source MACBOOK",
            "uuid": "be1ef1d6-5eb6-404d-8cb9-7f6d0755f7f1",
            "id": "ndi_source",
            "versioned_id": "ndi_source",
            "settings": {
                "ndi_fix_alpha_blending": false,
                "ndi_source_name": "MACBOOK.LOCAL (Scan Converter)",
                "ndi_behavior_lastframe": true,
                "ndi_bw_mode": 0,
                "ndi_behavior": 1
            },
#endif

	// Source visibility settings update: START
	// In 4.14.x, the "Visibility Behavior" property was used to control the visibility of the source via dropdown and an additional tickbox, creating confusion.
	// In 6.0.0, the "Visibility Behavior" property was replaced with a single dropdown.
	// This is a breaking change in v6.0.0 and invalid "Visibility Behavior" are set to "Keep Active" which is the default from previous versions.

	auto behavior = obs_data_get_int(settings, PROP_BEHAVIOR);

	obs_log(LOG_DEBUG,
		"'%s' ndi_source_update: Check for 'Behavior' setting changes: behavior='%d' vs config.behavior='%d'",
		obs_source_name, behavior, s->config.behavior);

	if (behavior == PROP_BEHAVIOR_KEEP_ACTIVE) {
		// Keep connection active.
		s->config.behavior = PROP_BEHAVIOR_KEEP_ACTIVE;

	} else if (behavior == PROP_BEHAVIOR_STOP_RESUME_BLANK) {
		// Stop the connection and resume it with a clean frame.
		s->config.behavior = PROP_BEHAVIOR_STOP_RESUME_BLANK;

	} else if (behavior == PROP_BEHAVIOR_STOP_RESUME_LAST_FRAME) {
		// Stop the connection and resume it with the last diplayed frame.
		s->config.behavior = PROP_BEHAVIOR_STOP_RESUME_LAST_FRAME;

	} else {
		// Fallback option. If the behavior is invalid, force it to "Keep Active" as it most likely came from the 4.14.x version.
		obs_log(LOG_DEBUG, "'%s' ndi_source_update: Invalid or unknown behavior detected :'%d' forced to '%d'",
			obs_source_name, behavior, PROP_BEHAVIOR_KEEP_ACTIVE);
		obs_log(LOG_WARNING,
			"WARN-414 - Invalid or unknown behavior detected in config file for source '%s': '%d' forced to '%d'",
			obs_source_name, behavior, PROP_BEHAVIOR_KEEP_ACTIVE);
		obs_data_set_int(settings, PROP_BEHAVIOR, PROP_BEHAVIOR_KEEP_ACTIVE);
		s->config.behavior = PROP_BEHAVIOR_KEEP_ACTIVE;
	}

	s->config.timeout_action = obs_data_get_int(settings, PROP_TIMEOUT);

	// Clean the source content when settings change unless requested otherwise.
	// Always clean if the source is set to Audio Only.
	// Always clean if the receiver is reset as well.
	if (s->config.bandwidth == PROP_BW_AUDIO_ONLY || s->config.behavior == PROP_BEHAVIOR_STOP_RESUME_BLANK ||
	    reset_ndi_receiver) {
		obs_log(LOG_DEBUG,
			"'%s' ndi_source_update: Deactivate source output video (Actively reset the frame content)",
			obs_source_name);
		deactivate_source_output_video_texture(s);
	}

	//
	// Source visibility settings update END
	//

	s->config.sync_mode = (int)obs_data_get_int(settings, PROP_SYNC);
	// if sync mode is set to the unsupported "Internal" mode, set it
	// to "Source Timing" mode and apply that change to the settings data
	if (s->config.sync_mode == PROP_SYNC_INTERNAL) {
		s->config.sync_mode = PROP_SYNC_NDI_SOURCE_TIMECODE;
		obs_data_set_int(settings, PROP_SYNC, PROP_SYNC_NDI_SOURCE_TIMECODE);
	}

	bool alpha_filter_enabled = obs_data_get_bool(settings, PROP_FIX_ALPHA);
	// Prevent duplicate filters by not persisting this value in settings
	obs_data_set_bool(settings, PROP_FIX_ALPHA, false);
	if (alpha_filter_enabled) {
		obs_source_t *existing_filter = find_filter_by_id(obs_source, OBS_NDI_ALPHA_FILTER_ID);
		if (!existing_filter) {
			obs_source_t *new_filter = obs_source_create(
				OBS_NDI_ALPHA_FILTER_ID, obs_module_text("NDIPlugin.PremultipliedAlphaFilterName"),
				nullptr, nullptr);
			obs_source_filter_add(obs_source, new_filter);
			obs_source_release(new_filter);
		}
	}

	// Disable OBS buffering only for "Lowest" latency mode
	const bool is_unbuffered = (s->config.latency == PROP_LATENCY_LOWEST);
	obs_source_set_async_unbuffered(obs_source, is_unbuffered);

	s->config.audio_enabled = obs_data_get_bool(settings, PROP_AUDIO);
	obs_source_set_audio_active(obs_source, s->config.audio_enabled);

	bool ptz_enabled = obs_data_get_bool(settings, PROP_PTZ);
	float pan = (float)obs_data_get_double(settings, PROP_PAN);
	float tilt = (float)obs_data_get_double(settings, PROP_TILT);
	float zoom = (float)obs_data_get_double(settings, PROP_ZOOM);
	s->config.ptz = ptz_t(ptz_enabled, pan, tilt, zoom);

	// Update tally status
	s->config.tally.on_preview = tally_on_preview(obs_source);
	s->config.tally.on_program = tally_on_program(obs_source);

	// camera-box #93: config mutation done. Snapshot the empty-name decision while
	// still holding the lock, then RELEASE before the thread lifecycle block so the
	// pthread_join inside ndi_source_thread_stop cannot deadlock against the
	// av_thread taking config_mutex in its reset block.
	bool ndi_source_name_empty = (strlen(s->config.ndi_source_name) == 0);
	pthread_mutex_unlock(&s->config_mutex);

	if (ndi_source_name_empty) {
		obs_log(LOG_DEBUG, "'%s' ndi_source_update: No NDI Source selected; Requesting Source Thread Stop.",
			obs_source_name);
		ndi_source_thread_stop(s);
	} else {
		obs_log(LOG_DEBUG, "'%s' ndi_source_update: NDI Source selected.", obs_source_name);
		if (s->running) {
			//
			// Thread is running; notify it if it needs to reset the NDI receiver.
			// camera-box #93: the reset flag is read by the av_thread's reset
			// block (also under config_mutex), so set it under the lock too.
			//
			pthread_mutex_lock(&s->config_mutex);
			s->config.reset_ndi_receiver = reset_ndi_receiver;
			pthread_mutex_unlock(&s->config_mutex);
		} else {
			//
			// Thread is not running; start it if either:
			// 1. the source is active
			//    -or-
			// 2. the behavior property is set to keep the NDI receiver running
			//
			if (obs_source_active(obs_source) || s->config.behavior == PROP_BEHAVIOR_KEEP_ACTIVE) {
				obs_log(LOG_DEBUG, "'%s' ndi_source_update: Requesting Source Thread Start.",
					obs_source_name);
				ndi_source_thread_start(s);
			}
		}
	}
	// Provide all the source config when updated.
	// camera-box #93: reads s->config.* AFTER config_mutex was released — safe because
	// only ndi_source_update / ndi_source_destroy ever FREE these, OBS serializes
	// update() calls per source, and the av_thread only reads them. Do NOT "fix" this
	// by widening the lock to here: it is a log line, not a config write, and the read
	// cannot race a free.
	obs_log(LOG_INFO,
		"NDI Source Updated: '%s', 'Bandwidth'='%d', Latency='%d', Framesync='%s', HardwareAcceleration='%s', behavior='%d', timeoutmode='%d', sync_mode='%d', yuv_range='%d', yuv_colorspace='%d'",
		s->config.ndi_source_name, s->config.bandwidth, s->config.latency,
		s->config.framesync_enabled ? "enabled" : "disabled",
		s->config.hw_accel_enabled ? "enabled" : "disabled", s->config.behavior, s->config.timeout_action,
		s->config.sync_mode, s->config.yuv_range, s->config.yuv_colorspace);

	obs_log(LOG_DEBUG, "'%s' -ndi_source_update(…)", obs_source_name);
}

void ndi_source_shown(void *data)
{
	// NOTE: This does NOT fire when showing a source in Preview that is also in Program.
	auto s = (ndi_source_t *)data;
	auto obs_source_name = obs_source_get_name(s->obs_source);
	obs_log(LOG_DEBUG, "'%s' ndi_source_shown(…)", obs_source_name);
	s->config.tally.on_preview = tally_on_preview(s->obs_source);
	if (!s->running) {
		obs_log(LOG_DEBUG, "'%s' ndi_source_shown: Requesting Source Thread Start.", obs_source_name);
		ndi_source_thread_start(s);
	}
}

void ndi_source_hidden(void *data)
{
	// NOTE: This does NOT fire when hiding a source in Preview that is also in Program.
	auto s = (ndi_source_t *)data;
	auto obs_source_name = obs_source_get_name(s->obs_source);
	obs_log(LOG_DEBUG, "'%s' ndi_source_hidden(…)", obs_source_name);
	s->config.tally.on_preview = false;
	if (s->running && s->config.behavior != PROP_BEHAVIOR_KEEP_ACTIVE) {
		obs_log(LOG_DEBUG, "'%s' ndi_source_hidden: Requesting Source Thread Stop.", obs_source_name);
		// Stopping the thread may result in `on_preview=false` not getting sent,
		// but the thread's `ndiLib->recv_destroy` results in an implicit tally off.
		ndi_source_thread_stop(s);
	}
}

void ndi_source_activated(void *data)
{
	auto s = (ndi_source_t *)data;
	auto obs_source_name = obs_source_get_name(s->obs_source);
	obs_log(LOG_DEBUG, "'%s' ndi_source_activated(…)", obs_source_name);
	s->config.tally.on_preview = tally_on_preview(s->obs_source);
	s->config.tally.on_program = tally_on_program(s->obs_source);
	if (!s->running) {
		obs_log(LOG_DEBUG, "'%s' ndi_source_activated: Requesting Source Thread Start.", obs_source_name);
		ndi_source_thread_start(s);
	}
}

void ndi_source_deactivated(void *data)
{
	auto s = (ndi_source_t *)data;
	obs_log(LOG_DEBUG, "'%s' ndi_source_deactivated(…)", obs_source_get_name(s->obs_source));
	s->config.tally.on_preview = tally_on_preview(s->obs_source);
	s->config.tally.on_program = false;
}

void new_ndi_receiver_name(const char *obs_source_name, char **ndi_receiver_name)
{
	if (*ndi_receiver_name) {
		bfree(*ndi_receiver_name);
	}
	*ndi_receiver_name = bstrdup(QT_TO_UTF8(QString("%1 '%2'").arg(PLUGIN_NAME, obs_source_name)));
#if 0
	obs_log(LOG_DEBUG, "'%s' new_ndi_receiver_name: ndi_receiver_name='%s'",
		obs_source_name, *ndi_receiver_name);
#endif
}

void on_ndi_source_renamed(void *data, calldata_t *)
{
	auto s = (ndi_source_t *)data;
	auto obs_source_name = obs_source_get_name(s->obs_source);
	// camera-box #93: this is the SECOND writer of config.ndi_receiver_name (the
	// other is ndi_source_update). new_ndi_receiver_name() bfree()s + bstrdup()s it,
	// and the av_thread reads it under config_mutex (the bstrdup into
	// owned_receiver_name in reset_ndi_receiver). Take the lock here too so this
	// free/realloc can never race that read. Snapshot for the log so the lock is
	// dropped before logging.
	pthread_mutex_lock(&s->config_mutex);
	new_ndi_receiver_name(obs_source_name, &(s->config.ndi_receiver_name));
	s->config.reset_ndi_receiver = true;
	char *renamed_copy = bstrdup(s->config.ndi_receiver_name);
	pthread_mutex_unlock(&s->config_mutex);
	obs_log(LOG_DEBUG, "'%s' on_ndi_source_renamed: new ndi_receiver_name='%s'", obs_source_name,
		renamed_copy);
	bfree(renamed_copy);
}

void *ndi_source_create(obs_data_t *settings, obs_source_t *obs_source)
{
	auto obs_source_name = obs_source_get_name(obs_source);
	obs_log(LOG_DEBUG, "'%s' +ndi_source_create(…)", obs_source_name);

	auto s = (ndi_source_t *)bzalloc(sizeof(ndi_source_t));
	s->obs_source = obs_source;
	// camera-box #93: init the config lock BEFORE ndi_source_update (called below)
	// can take it. A recursive mutex is unnecessary (update never re-enters itself
	// while holding it), but it costs nothing to be explicit about the default.
	pthread_mutex_init(&s->config_mutex, nullptr);
	new_ndi_receiver_name(obs_source_name, &(s->config.ndi_receiver_name));

	auto sh = obs_source_get_signal_handler(s->obs_source);
	signal_handler_connect(sh, "rename", on_ndi_source_renamed, s);

	ndi_source_update(s, settings);

	obs_log(LOG_DEBUG, "'%s' -ndi_source_create(…)", obs_source_name);

	return s;
}

void ndi_source_destroy(void *data)
{
	auto s = (ndi_source_t *)data;
	auto obs_source_name = obs_source_get_name(s->obs_source);
	obs_log(LOG_DEBUG, "'%s' +ndi_source_destroy(…)", obs_source_name);

	auto sh = obs_source_get_signal_handler(s->obs_source);
	signal_handler_disconnect(sh, "rename", on_ndi_source_renamed, s);

	ndi_source_thread_stop(s);

	if (s->config.ndi_receiver_name) {
		bfree(s->config.ndi_receiver_name);
		s->config.ndi_receiver_name = nullptr;
	}

	if (s->config.ndi_source_name) {
		bfree(s->config.ndi_source_name);
		s->config.ndi_source_name = nullptr;
	}

	// camera-box #93: the av_thread is joined (ndi_source_thread_stop above), so no
	// one can be holding config_mutex now — safe to destroy it.
	pthread_mutex_destroy(&s->config_mutex);

	bfree(s);

	obs_log(LOG_DEBUG, "'%s' -ndi_source_destroy(…)", obs_source_name);
}

uint32_t ndi_source_get_width(void *data)
{
	auto s = (ndi_source_t *)data;
	return s->width;
}

uint32_t ndi_source_get_height(void *data)
{
	auto s = (ndi_source_t *)data;
	return s->height;
}

obs_source_info create_ndi_source_info()
{
	// https://docs.obsproject.com/reference-sources#source-definition-structure-obs-source-info
	obs_source_info ndi_source_info = {};
	ndi_source_info.id = "ndi_source";
	ndi_source_info.type = OBS_SOURCE_TYPE_INPUT;
	ndi_source_info.icon_type = OBS_ICON_TYPE_CAMERA;
	ndi_source_info.output_flags = OBS_SOURCE_ASYNC_VIDEO | OBS_SOURCE_AUDIO | OBS_SOURCE_DO_NOT_DUPLICATE;

	ndi_source_info.get_name = ndi_source_getname;
	ndi_source_info.get_properties = ndi_source_getproperties;
	ndi_source_info.get_defaults = ndi_source_getdefaults;

	ndi_source_info.create = ndi_source_create;
	ndi_source_info.activate = ndi_source_activated;
	ndi_source_info.show = ndi_source_shown;
	ndi_source_info.update = ndi_source_update;
	ndi_source_info.hide = ndi_source_hidden;
	ndi_source_info.deactivate = ndi_source_deactivated;
	ndi_source_info.destroy = ndi_source_destroy;

	ndi_source_info.get_width = ndi_source_get_width;
	ndi_source_info.get_height = ndi_source_get_height;

	return ndi_source_info;
}
