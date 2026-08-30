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
#include <chrono>
#include "ndi-finder.h"

#include <util/platform.h>
#include <util/threading.h>

#include <QDesktopServices>
#include <QUrl>

#include <thread>
/* camera-box #257: the OBS_GENLOCK_* / OBS_BURN_* env reads + the read-only info-text
 * labels were removed (hard-lock whitelist UI), so cstdio/cstdlib are no longer needed here. */

#define PROP_SOURCE "ndi_source_name"
#define PROP_BEHAVIOR "ndi_behavior"
#define PROP_TIMEOUT "ndi_behavior_timeout"
#define PROP_BANDWIDTH "ndi_bw_mode"
#define PROP_SYNC "ndi_sync"
#define PROP_FRAMESYNC "ndi_framesync"
#define PROP_GENLOCK_FIFO "genlock_fifo"            /* camera-box #42: WHITELIST — bool "Genlock", default ON */
#define PROP_GENLOCK_LATENCY_MS_SRC "genlock_latency_ms_src" /* camera-box #245: WHITELIST — per-source latency (ms), default 3, min 3 */
#define PROP_BURN "genlock_burn"                    /* camera-box #257: WHITELIST — bool "Measurement burn (test only)", default OFF, runtime */
#define PROP_GENLOCK_MONITOR "genlock_monitor"       /* camera-box #501: WHITELIST — bool "Monitor-only (low-bandwidth NDI)", default OFF */
#define PROP_GENLOCK_SOURCE_LATENCY_MS_MAX 2000     /* mirrors libobs GENLOCK_SOURCE_LATENCY_MS_MAX (#245) */
#define PROP_GENLOCK_LATENCY_MS_MIN 3               /* camera-box #257: latency floor (ms), mirrors libobs GENLOCK_LATENCY_MS_MIN */
#define PROP_GENLOCK_LATENCY_MS_DEFAULT 3           /* camera-box #257: per-source latency default (ms) = the floor */

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

/* camera-box #764: GETTER counterpart of resolve_set_genlock_fifo, same runtime-resolve
 * rationale (stock SDK headers at DistroAV build time have no genlock symbols). Used by the
 * receiver thread's keep-alive check below — never a link-time call. A stock (unpatched) OBS
 * build resolves nullptr here, so genlock_source_is_active() below safely reports "not
 * genlocked" and the thread keeps stock DistroAV's original hide/deactivate behavior. */
typedef bool (*get_genlock_fifo_fn)(const obs_source_t *);
static get_genlock_fifo_fn resolve_get_genlock_fifo()
{
	static get_genlock_fifo_fn fn = nullptr;
	static bool tried = false;
	if (!tried) {
		tried = true;
		fn = (get_genlock_fifo_fn)resolve_obs_export("obs_source_get_genlock_fifo");
		if (!fn)
			obs_log(LOG_WARNING,
				"genlock: obs_source_get_genlock_fifo not exported by this OBS build — "
				"the NDI receiver keep-alive fix is inert (stock OBS?)");
	}
	return fn;
}

/* camera-box #764 (event-critical, 2026-07-15): is `source` a genlocked source RIGHT NOW?
 * Pure passthrough to the runtime-resolved getter -- honest false (never a guess) when the
 * export isn't available. */
static bool genlock_source_is_active(obs_source_t *source)
{
	if (auto get_genlock = resolve_get_genlock_fifo())
		return get_genlock(source);
	return false;
}

/* camera-box #767 (event-critical, 2026-08-13): a genlocked source that stays CONNECTED but
 * silent this long has a stuck (half-open, post sender-reboot) NDI connection -- force a full
 * receiver rebind. 10 s = 3x process_empty_frame's own 3 s blank timeout: past any
 * genlock-FIFO/network transient, far below the 41-min live silent-black incident. */
static const uint64_t GENLOCK_RECONNECT_STALE_NS = 10ULL * 1000ULL * 1000ULL * 1000ULL;

/* camera-box #767: should this genlocked, CONNECTED source that has delivered no new frame for
 * `stale_ns` force a full NDI receiver rebind? PURE decision (no OBS/NDI calls, only primitives)
 * so it lift-compiles + truth-table-tests offline -- CI is otherwise the first compiler for this
 * file (tests/distroav_ndi_reconnect_767.rs). Root cause it addresses: the receiver is recreated
 * ONLY via the reset_ndi_receiver flag; the steady loop never rebinds a stuck connection when the
 * sender instance restarts, and NDI's own name-based reconnect only re-resolves once
 * no_connections drops to 0 (which a hard-reboot half-open connection never does). See
 * ndi_source_thread's watchdog call. */
static inline bool genlock_reconnect_decision(bool genlock_active, int no_connections, uint64_t now_ns,
					      uint64_t last_frame_ns, uint64_t stale_ns)
{
	if (!genlock_active)
		return false; /* scoped to genlocked sources only (mirrors the #764 keep-alive scope) */
	if (no_connections <= 0)
		return false; /* no sender connected -> NDI's finder rebinds; not the stuck-connection case */
	if (last_frame_ns == 0)
		return false; /* never received a frame yet -> don't judge a warming-up receiver */
	if (now_ns <= last_frame_ns)
		return false; /* clock has not advanced past the last frame -> no measurable age */
	return (now_ns - last_frame_ns) >= stale_ns;
}

/* camera-box #1080: back-off (ns) before the next recv_create_v3 retry after a create FAILURE.
 * recv_create_v3 realistically fails only under transient resource exhaustion; hammering it in a
 * tight loop worsens the exhaustion. Exponential from 250 ms, doubling, capped at 3 s -- fast
 * recovery from a one-off blip, gentle under sustained pressure. `consecutive_failures` is 1-based
 * (the count INCLUDING this failure); 0 folds to the base; a large count is shift-clamped (no shift
 * UB) and stays at the cap. PURE (only primitives) so it lift-compiles + truth-table-tests offline
 * -- CI is otherwise the first compiler for this file (tests/distroav_recv_create_retry_1080.rs).
 * This helper NEVER caps the retry COUNT (the caller loops on it forever): the receiver thread must
 * never die on a create failure, or it becomes a permanent, reattach-proof black -- a break there
 * leaves s->running true, so ndi_source_update's `if (s->running)` never restarts it. */
static inline uint64_t ndi_recv_create_retry_backoff_ns(unsigned consecutive_failures)
{
	const uint64_t base_ns = 250ULL * 1000ULL * 1000ULL;        /* 250 ms */
	const uint64_t cap_ns = 3ULL * 1000ULL * 1000ULL * 1000ULL; /* 3 s */
	unsigned shift = consecutive_failures > 1 ? consecutive_failures - 1 : 0;
	if (shift > 5)
		shift = 5; /* 250 ms << 5 = 8 s already exceeds the 3 s cap; also caps the shift < 64 (no UB) */
	uint64_t backoff_ns = base_ns << shift;
	return backoff_ns < cap_ns ? backoff_ns : cap_ns;
}

/* camera-box #1096: bounded wait budget for the fresh-finder resolution in the reset block. The
 * restarted sender is already announcing (the wedge case), so it resolves on the first wait; the
 * cap bounds a source that is genuinely mid-restart. The loop respects s->running so OBS shutdown
 * is never blocked more than one wait interval. */
static const uint32_t NDI_FRESH_FIND_WAIT_MS = 500;
static const unsigned NDI_FRESH_FIND_MAX_WAITS = 4;

/* camera-box #1180: bounded fresh-finder budget for the post-connect BY-URL identity verify. Shorter
 * than the reset-block resolution (NDI_FRESH_FIND_MAX_WAITS) because the correct sender for our name
 * is expected to be advertising already -- SOMETHING at our URL just delivered frames -- so this only
 * needs to catch the settled sender set, not wait out a genuine mid-restart. Bounds the brief
 * ONE-SHOT stall right after a reconnect; the verify runs once per bind (see below), never in steady
 * state, so it never stalls the live frame loop on a healthy connection. */
static const unsigned NDI_IDENTITY_VERIFY_MAX_WAITS = 2;

/* camera-box #1096: pick the CURRENT network address for a source name from a FRESH finder's
 * discovered list, so the receiver can connect BY-ADDRESS and BYPASS the poisoned long-lived
 * in-process NDI finder. The wedge: a restarted cambox sender rotates its NDI port; recv_create_v3
 * connect-by-name re-consults the SAME per-process finder, which keeps the stale name->address
 * entry and never re-resolves -- only an OBS process restart (fresh SDK finder) recovers. Returns
 * the matched source's p_url_address (owned by the finder -- COPY it before find_destroy), or NULL
 * when the name is empty, absent from the list, or carries no address (then the caller keeps the
 * name-based connect, i.e. no worse than upstream). Exact name match -- mirrors recv_create's own
 * name-equality. PURE (only primitives + the two const char* fields) so it lift-compiles +
 * truth-table-tests offline -- CI is otherwise the first compiler for this file
 * (tests/distroav_fresh_finder_connect_1096.rs). */
static inline const char *ndi_find_url_for_source_name(const char *requested_name,
						       const NDIlib_source_t *sources, uint32_t n_sources)
{
	if (!requested_name || !requested_name[0])
		return NULL; /* no name to match -> keep the name path (nothing to bypass) */
	if (!sources || n_sources == 0)
		return NULL; /* fresh finder saw nothing (yet) -> fall back to name */
	for (uint32_t i = 0; i < n_sources; ++i) {
		const char *name = sources[i].p_ndi_name;
		if (name && strcmp(name, requested_name) == 0) {
			const char *url = sources[i].p_url_address;
			if (url && url[0])
				return url; /* current address -> connect BY-URL, bypassing the poison */
			return NULL;   /* matched but no usable address -> fall back to name */
		}
	}
	return NULL; /* not discovered (yet) -> fall back to name */
}

/* camera-box #1180: after a BY-URL-connected receiver starts delivering frames, decide whether the
 * configured source name still maps to the URL the receiver is bound to. A BY-URL connect (the
 * #1096 fresh-finder path) never verifies the sender's NAME -- after a sender OBS restart the NDI
 * output ports can RESHUFFLE, so a DIFFERENT sender can inherit the URL we cached and frames flow
 * from the WRONG camera under our configured label with nothing re-checking (the 2026-08-23 P0:
 * every "2ME PGM" receiver showed the Grading/cam3 feed). `connected_url` is the URL the receiver is
 * bound to (owned_source_url from the #1096 connect); `resolved_url_for_name` is what a FRESH finder
 * currently resolves for the SAME configured name (via ndi_find_url_for_source_name). Returns true
 * ONLY on a CONFIRMED mismatch -- both known AND different -> force a fresh BY-NAME reset. Returns
 * false when we were not BY-URL (nothing to verify -- never fires for a BY-NAME bind) or the name is
 * not currently discoverable (INCONCLUSIVE -- never tear down a working feed on a can't-confirm).
 * PURE (only the two const char*) so it lift-compiles + truth-table-tests offline -- CI is otherwise
 * the first compiler for this file (tests/distroav_by_url_identity_verify_1180.rs). */
static inline bool ndi_by_url_identity_mismatch(const char *connected_url, const char *resolved_url_for_name)
{
	if (!connected_url || !connected_url[0])
		return false; /* not a BY-URL bind -> nothing to verify (a BY-NAME bind never enters here) */
	if (!resolved_url_for_name || !resolved_url_for_name[0])
		return false; /* name not currently discoverable -> INCONCLUSIVE, keep the feed */
	return strcmp(connected_url, resolved_url_for_name) != 0; /* both known + differ -> MISMATCH */
}

/* camera-box #257: per-source MEASUREMENT-BURN setter, runtime-resolved by name — same
 * rationale as the fifo/latency setters: the Windows DistroAV build fetches stock OBS SDK
 * headers (no genlock symbols), so a link-time call cannot build; resolve at runtime so
 * the plugin still loads on any OBS (the control is just inert on a stock build). The
 * burn QR filter reads obs_source_get_genlock_burn(parent) each render; this setter writes
 * the per-source flag live from PROP_BURN in ndi_source_update (no OBS restart). */
typedef void (*set_genlock_burn_fn)(obs_source_t *, bool);
static set_genlock_burn_fn resolve_set_genlock_burn()
{
	static set_genlock_burn_fn fn = nullptr;
	static bool tried = false;
	if (!tried) {
		tried = true;
		fn = (set_genlock_burn_fn)resolve_obs_export("obs_source_set_genlock_burn");
		if (!fn)
			obs_log(LOG_WARNING,
				"genlock: obs_source_set_genlock_burn not exported by this OBS build — "
				"the per-source Measurement burn toggle is inert (stock OBS?)");
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

	/* camera-box #764: logs the "NDI receiver keep-alive" line exactly ONCE per source, the
	 * first time the receiver thread actually skips the hidden-source pause because genlock
	 * keep-alive is active — never spammed every ~5ms loop tick. */
	bool logged_genlock_keepalive;
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

/* camera-box #257: the HARD-LOCK source-UI WHITELIST. ndi_source_getproperties exposes
 * EXACTLY these four props and NOTHING else — source selection, the Genlock gate (default
 * ON), the per-source latency (ms, floor 3), and the measurement-burn toggle (default OFF,
 * runtime). Every other DistroAV property is removed from the UI and FORCED to a certified
 * value (GENLOCK_FORCED_SETTINGS below — the exact COMPLEMENT of this list), so an upstream
 * DistroAV property add/remove can never reintroduce a live drift knob. A vendored-source
 * test asserts the exposed set == this whitelist. */
[[maybe_unused]] static const char *const GENLOCK_WHITELIST_PROPS[] = {
	PROP_SOURCE,
	PROP_GENLOCK_FIFO,
	PROP_GENLOCK_LATENCY_MS_SRC,
	PROP_BURN,
	PROP_GENLOCK_MONITOR,
};

/* camera-box #150/#257: the certified value FORCED into every non-whitelist (hidden) prop
 * when genlock is on — a single const table that is the COMPLEMENT of GENLOCK_WHITELIST_PROPS.
 * force_genlock_certified_settings() iterates it, so a value can never silently drift and an
 * upstream property the operator could otherwise see is pinned to its zero-loss certified
 * value. The values were read live from the working prod input `NDI cam5`:
 *   ndi_sync=2 (SOURCE_TIMECODE), ndi_behavior=0 (KEEP_ACTIVE, #764), ndi_bw_mode=0 (highest),
 *   latency=0 (NORMAL), ndi_recv_hw_accel=true, ndi_audio=false, ndi_framesync=false,
 *   ndi_fix_alpha_blending=false, yuv_range=partial, yuv_colorspace=BT.709,
 *   timeout=KEEP_CONTENT, ptz=off.
 *
 * camera-box #764 (event-critical, 2026-07-15): ndi_behavior changed from
 * STOP_RESUME_LAST_FRAME to KEEP_ACTIVE. Root cause: a strih/imag NDI cam source that is
 * hidden (not on program/preview/any active view) had its receiver THREAD FULLY TORN DOWN
 * (ndi_source_hidden -> ndi_source_thread_stop -> ndiLib->recv_destroy) because
 * STOP_RESUME_LAST_FRAME is the ONE behavior value that does NOT satisfy
 * ndi_source_hidden's own `s->config.behavior != PROP_BEHAVIOR_KEEP_ACTIVE` guard — every cut
 * TO a previously-hidden camera therefore paid a full NDI reconnect (recv_create + renegotiate
 * + resync) before the FIRST warm frame, discovered as dropped/delayed frames on program cuts.
 * KEEP_ACTIVE keeps the receiver thread (and, with the #764 change to the thread loop below,
 * frame decode) running continuously regardless of visibility -- a cut is then just OBS
 * switching which existing warm source to render, no reconnect. */
struct genlock_forced_setting {
	const char *prop;
	bool is_bool;
	long long ival; /* used when !is_bool */
	bool bval;      /* used when is_bool */
};
static const struct genlock_forced_setting GENLOCK_FORCED_SETTINGS[] = {
	{PROP_SYNC, false, PROP_SYNC_NDI_SOURCE_TIMECODE, false},
	{PROP_BEHAVIOR, false, PROP_BEHAVIOR_KEEP_ACTIVE, false}, /* #764: was STOP_RESUME_LAST_FRAME */
	{PROP_BANDWIDTH, false, PROP_BW_HIGHEST, false},
	{PROP_LATENCY, false, PROP_LATENCY_NORMAL, false},
	{PROP_TIMEOUT, false, PROP_TIMEOUT_KEEP_CONTENT, false},
	{PROP_YUV_RANGE, false, PROP_YUV_RANGE_PARTIAL, false},
	{PROP_YUV_COLORSPACE, false, PROP_YUV_SPACE_BT709, false},
	{PROP_HW_ACCEL, true, 0, true},
	{PROP_AUDIO, true, 0, false},
	{PROP_FRAMESYNC, true, 0, false},
	{PROP_FIX_ALPHA, true, 0, false},
	{PROP_PTZ, true, 0, false},
};

/* camera-box #150/#257: FORCE every certified zero-loss genlock value into `settings`,
 * regardless of any saved scene value, WS-set value, or harness-set value. Called from
 * ndi_source_update ONLY when genlock_fifo is on (update is the authoritative enforcement
 * point; ndi_source_create calls update at the end, so a newly-added genlock source is
 * forced at creation). Driven by the single GENLOCK_FORCED_SETTINGS const table (the
 * complement of the whitelist), so a genlock NDI source — prod, probe, or new, in ANY
 * scene — is correct by construction. This closes the misconfig class root-caused live
 * 2026-06-22 (an incompletely-configured probe ingest decoded 0 while the certified prod
 * input decoded 100%, same NDI source). The whitelist knobs (PROP_SOURCE,
 * PROP_GENLOCK_LATENCY_MS_SRC, PROP_BURN) and PROP_GENLOCK_FIFO (the operator's gate) are
 * NEVER touched here. Writing into `settings` (not just s->config) persists the values into
 * the saved scene JSON, so the source stays correct across restarts. */
static void force_genlock_certified_settings(obs_data_t *settings)
{
	for (const struct genlock_forced_setting &f : GENLOCK_FORCED_SETTINGS) {
		if (f.is_bool)
			obs_data_set_bool(settings, f.prop, f.bval);
		else
			obs_data_set_int(settings, f.prop, f.ival);
	}
	/* camera-box #501: MONITOR-SOURCE bandwidth exception. A source flagged
	 * genlock_monitor never feeds program (it only feeds the built-in OBS multiview,
	 * which is view-only for the Stream Deck cutter workflow) — root-caused live on
	 * imag-nb (issue #501): the multiview costs ~80ms/render because every cell
	 * synchronously uploads ALL 6 cameras' FULL-1080p NDI textures (their async
	 * upload otherwise only happens when something renders them). Feeding the
	 * multiview from NDI LOW-bandwidth receivers instead (~9x cheaper) fits the
	 * #276/#278/#293 render-budget decouple back inside the 16.6ms tick. This
	 * NARROWLY overrides ONLY PROP_BANDWIDTH, applied AFTER the base certified table
	 * above so every other certified value (sync/behavior/latency/timeout/yuv/
	 * hw_accel/audio/framesync/alpha/ptz) stays locked exactly as before. */
	if (obs_data_get_bool(settings, PROP_GENLOCK_MONITOR))
		obs_data_set_int(settings, PROP_BANDWIDTH, PROP_BW_LOWEST);
}

/* camera-box #795: keep the currently-saved NDI source name selectable under the list-only source
 * combo (ndi_source_getproperties below). OBS's OBSPropertiesView::AddList
 * (vendor/obs-studio/shared/properties-view/properties-view.cpp) ends with
 * `if (count && idx == -1) info->ControlChanged();` — for a non-editable LIST combo whose saved
 * value is NOT among the (non-empty) list items, it writes the combo's index-0 default back into
 * settings, silently CLOBBERING the stored source name on properties-open. The NDI finder is
 * asynchronous and can momentarily return only OTHER sources (or nothing on a sick network), so we
 * always inject the saved name as a list item → `idx != -1` → that writeback never fires and the
 * configured source survives an empty/partial finder. Idempotent: skips an empty or already-listed
 * name so a discovered source is never duplicated. */
static void genlock_ensure_saved_source_listed(obs_property_t *source_list, ndi_source_t *s)
{
	/* camera-box #1224: guard the property-list consumer against a NULL/stale source_list too
	 * (not just !s). The async finder callback below can fire on a DETACHED thread after the
	 * owning props/source_list is gone, and obs_properties_add_list can return NULL under the
	 * OOM/render-stall that produced the c0000005 in obs.dll!new_prop. */
	if (!source_list || !s || !s->obs_source)
		return;
	obs_data_t *settings = obs_source_get_settings(s->obs_source);
	if (!settings)
		return;
	const char *saved = obs_data_get_string(settings, PROP_SOURCE);
	if (saved && *saved) {
		bool present = false;
		size_t count = obs_property_list_item_count(source_list);
		for (size_t i = 0; i < count; i++) {
			const char *item = obs_property_list_item_string(source_list, i);
			if (item && strcmp(item, saved) == 0) {
				present = true;
				break;
			}
		}
		if (!present)
			obs_property_list_add_string(source_list, saved, saved);
	}
	obs_data_release(settings);
}

obs_properties_t *ndi_source_getproperties(void *data)
{
	auto s = (ndi_source_t *)data;
	obs_log(LOG_DEBUG, "+ndi_source_getproperties(…)");

	/* camera-box #257: the HARD-LOCK source UI. Expose EXACTLY the GENLOCK_WHITELIST_PROPS
	 * — source · Genlock · Latency (ms) · Measurement burn — and NOTHING else. Every other
	 * DistroAV property (behavior/timeout/bandwidth/sync/framesync/hw_accel/alpha/yuv×2/
	 * audio/latency-combo/ptz + the old read-only info labels + the legacy preload slider)
	 * is REMOVED from the UI and FORCED to its certified value (force_genlock_certified_settings
	 * → GENLOCK_FORCED_SETTINGS, the complement of the whitelist). The operator can no longer
	 * mis-set a knob that does nothing. A vendored-source test asserts the exposed set ==
	 * GENLOCK_WHITELIST_PROPS. */
	obs_properties_t *props = obs_properties_create();

	/* camera-box #1224: guard-at-consumer before ANY obs_properties composition (new_prop).
	 * obs_properties_create bzalloc-fails to NULL under render-stall OOM; a NULL props fed into
	 * obs_properties_add_* is exactly the c0000005 in obs.dll!new_prop this ticket fixes. */
	if (!props) {
		obs_log(LOG_WARNING,
			"[distroav] ndi_source_getproperties: obs_properties_create returned NULL (OOM?); returning no properties");
		return nullptr;
	}

	/* (1) PROP_SOURCE — the NDI source selection. camera-box #795: LIST-only (non-editable) so free
	 * text can NEVER replace the configured source name. An editable combo was the 2026-07-17
	 * live-event black-screen trap: with the NDI finder EMPTY on a sick network, an operator's
	 * keystrokes mangled 'NDI 2ME PGM' → nonexistent source → black, recoverable only by an OBS
	 * restart. genlock_ensure_saved_source_listed() (above) keeps the saved name selectable so the
	 * LIST combo can never clobber a saved-but-undiscovered source on properties-open. */
	obs_property_t *source_list = obs_properties_add_list(props, PROP_SOURCE,
							      obs_module_text("NDIPlugin.SourceProps.SourceName"),
							      OBS_COMBO_TYPE_LIST, OBS_COMBO_FORMAT_STRING);
	NDIFinder finder;
	// Create a callback that is called when the NDI source list is complete
	auto finder_callback = [source_list, s](void *ndi_names) {
		/* camera-box #1224: this callback runs on a DETACHED finder thread (ndi-finder.cpp fires it
		 * 5+ s later, after ndi_source_getproperties returned), so the captured source_list/s may be
		 * NULL/stale. Guard-at-consumer before ANY deref — this also stops obs_source_update_properties
		 * from re-triggering a getproperties→new_prop build over a dead source (the #1224 c0000005). */
		if (!source_list || !s || !s->obs_source) {
			obs_log(LOG_WARNING,
				"[distroav] ndi finder callback: NULL/stale source_list or source; skipping refresh");
			return;
		}
		auto ndi_sources = (std::vector<std::string> *)ndi_names;
		for (auto &source : *ndi_sources) {
			obs_property_list_add_string(source_list, source.c_str(), source.c_str());
		}
		genlock_ensure_saved_source_listed(source_list, s); // #795: never clobber the saved source
		obs_source_update_properties(s->obs_source);
	};
	auto ndi_sources = finder.getNDISourceList(finder_callback);
	for (auto &source : ndi_sources) {
		obs_property_list_add_string(source_list, source.c_str(), source.c_str());
	}
	genlock_ensure_saved_source_listed(source_list, s); // #795: never clobber the saved source

	/* (2) PROP_GENLOCK_FIFO — the Genlock gate (bool, default ON via ndi_source_getdefaults). */
	obs_properties_add_bool(props, PROP_GENLOCK_FIFO, "Genlock (FIFO frame consumption, camera-box #42)");

	/* (3) PROP_GENLOCK_LATENCY_MS_SRC — the SINGLE per-source latency knob (ms). #257: floor
	 * GENLOCK_LATENCY_MS_MIN (3), max PROP_GENLOCK_SOURCE_LATENCY_MS_MAX (2000), default 3.
	 * Applied at runtime via obs_source_set_genlock_latency_ms (resolved by name so the plugin
	 * still builds against stock SDK headers; libobs clamps to [3, 2000] too). No env, no global
	 * label, no read-only hint — the operator sets ONE ms value. */
	obs_property_t *src_latency =
		obs_properties_add_int(props, PROP_GENLOCK_LATENCY_MS_SRC, "Latency (ms)", PROP_GENLOCK_LATENCY_MS_MIN,
				       PROP_GENLOCK_SOURCE_LATENCY_MS_MAX, 1);
	obs_property_int_set_suffix(src_latency, " ms");

	/* (4) PROP_BURN — the measurement-burn toggle (bool, default OFF). #257: applied LIVE via
	 * obs_source_set_genlock_burn in ndi_source_update (no OBS restart); the QR burn filter reads
	 * the per-source flag each render. TEST-mode only. */
	obs_properties_add_bool(props, PROP_BURN, "Measurement burn (test only)");

	/* (5) PROP_GENLOCK_MONITOR — the monitor-only toggle (bool, default OFF). #501: when set,
	 * force_genlock_certified_settings narrows PROP_BANDWIDTH to PROP_BW_LOWEST for this source
	 * (every other certified value stays locked). Set true ONLY on a source that feeds the
	 * built-in OBS multiview and never feeds program. */
	obs_properties_add_bool(props, PROP_GENLOCK_MONITOR, "Monitor-only (low-bandwidth NDI, camera-box #501)");

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
	/* camera-box #257: genlock is DEFAULT ON — the fork exists to be genlocked in production,
	 * so a newly-added NDI source is locked down + forced to the certified config by default. */
	obs_data_set_default_bool(settings, PROP_GENLOCK_FIFO, true);
	/* camera-box #245/#257: the per-source latency defaults to the floor (3 ms) — no env, no
	 * "0 = follow global" any more; 3 ms is the validated zero-loss held latency. */
	obs_data_set_default_int(settings, PROP_GENLOCK_LATENCY_MS_SRC, PROP_GENLOCK_LATENCY_MS_DEFAULT);
	/* camera-box #257: measurement burn OFF by default (TEST mode turns it on at runtime). */
	obs_data_set_default_bool(settings, PROP_BURN, false);
	/* camera-box #501: monitor-only OFF by default — a source is full-bandwidth (feeds program)
	 * unless explicitly flagged as a multiview-only monitoring receiver. */
	obs_data_set_default_bool(settings, PROP_GENLOCK_MONITOR, false);
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
	/* camera-box #1096: the fresh-finder-resolved source URL (owned copy — the finder frees its
	 * source pointers on find_destroy). Refreshed in the reset block, freed on thread exit. */
	char *owned_source_url = nullptr;

	NDIlib_recv_instance_t ndi_receiver = nullptr;
	NDIlib_video_frame_v2_t video_frame;

	NDIlib_metadata_frame_t metadata_frame;
	NDIlib_framesync_instance_t ndi_frame_sync = nullptr;
	NDIlib_audio_frame_v3_t audio_frame;
	NDIlib_frame_type_e frame_received = NDIlib_frame_type_none;

	int64_t timestamp_audio = 0;
	int64_t timestamp_video = 0;

	/* camera-box #767: tracks the disconnect->reconnect edge for the stale watchdog below. Starts
	 * true so the FIRST connection (and every reconnect) gets a fresh stale window instead of being
	 * judged against frames from the previous (or never-existent) connection epoch. */
	bool was_disconnected = true;

	/* camera-box #1080: consecutive recv_create_v3 failures, driving the retry backoff below. Reset
	 * to 0 on the next successful create so a one-off blip does not leave the backoff escalated. */
	unsigned recv_create_fail_count = 0;

	/* camera-box #1097: consecutive framesync_create failures, driving the SAME #1080 exponential
	 * backoff for the (currently dormant -- framesync forced OFF) framesync-create retry-in-place.
	 * Its OWN counter (not the shared recv_create_fail_count) so the live #1080 recv_create retry
	 * path stays byte-for-byte unchanged, and so a pure-framesync-failure loop escalates correctly
	 * (a successful recv_create would otherwise reset a shared counter every iteration). Reset to 0
	 * on framesync-create success, mirroring recv_create_fail_count. */
	unsigned framesync_create_fail_count = 0;

	/* camera-box #1096: os_gettime_ns() at which no_connections first became 0 (0 = currently
	 * connected). A GRACEFUL cambox restart drops the strih receiver to no_connections==0
	 * (clean FIN), where the #767 watchdog (no_connections>0 only) never fires and a by-URL
	 * receiver cannot self-rebind. Used to force a FRESH-finder reset after a stale window. */
	uint64_t no_conn_since_ns = 0;

	/* camera-box #1180: post-connect BY-URL identity-verify state machine (all thread-local, mirroring
	 * the #767/#1096 thread-locals above). connected_by_url is armed ONLY when the current receiver was
	 * created via the #1096 BY-URL path -- the only bind that needs an identity re-check; a BY-NAME bind
	 * leaves it false so the verify path never runs and upstream behaviour is byte-identical.
	 * identity_verify_pending fires the ONE-SHOT verify after the first frames of a BY-URL bind; it is
	 * re-armed on EVERY reset (every BY-URL reconnect routes through a reset -- #767 stale rebind, the
	 * #1096 no_conn==0 rebind, or a config change), so the verify is EVENT-DRIVEN per reconnect (the
	 * reshuffle window) with NO steady-state finder poll stalling the live frame loop (review #1180 🟡).
	 * frames_seen_since_reset gates it on frames ACTUALLY flowing (the issue's "starts delivering
	 * frames" trigger); force_by_name_next_reset makes the NEXT reset skip the fresh-finder BY-URL path
	 * and connect BY-NAME -- the corrective action on a confirmed identity mismatch. */
	bool connected_by_url_1180 = false;
	bool identity_verify_pending_1180 = false;
	bool frames_seen_since_reset_1180 = false;
	bool force_by_name_next_reset_1180 = false;

	/* camera-box #797 recv-timing instrumentation: locate the ~50-of-60fps pull-loop
	 * throttle. Times recv_capture_v3 (wait for SDK) vs process_video2+free (our cost,
	 * dominated by obs_source_output_video) per VIDEO frame; logs a 5s summary per
	 * source. Pure diagnosis — remove after #797 closes. */
	uint64_t t797_n = 0;
	double t797_cap_sum = 0, t797_cap_max = 0, t797_out_sum = 0, t797_out_max = 0;
	auto t797_last_log = std::chrono::steady_clock::now();

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

			//
			// camera-box #1096: BEFORE creating the new receiver, resolve the source through a
			// FRESH NDIlib_find (create+wait+read+destroy — the SAME sequence ndi-finder.cpp uses)
			// and connect BY-ADDRESS. recv_create_v3 connect-by-name re-consults the long-lived
			// per-process finder, which stays poisoned with a restarted sender's stale (rotated-
			// port) address — the wedge only an OBS restart otherwise clears. Connecting by the
			// fresh p_url_address bypasses it. Fallback: no fresh URL resolved -> keep the name-
			// based connect (no worse than upstream). This blocks a bounded window and NEVER holds
			// config_mutex (dropped above), matching the 'no blocking NDI call under the lock' rule.
			//
			bool url_resolved_1096 = false;
			// camera-box #1180: a confirmed BY-URL identity mismatch forces THIS one reset to connect
			// BY-NAME (skip the #1096 fresh-finder BY-URL resolution), abandoning the wrong-sender URL
			// and letting NDI's own name resolution re-point at whatever now advertises our name (the
			// same recovery reopening Studio Monitor did live). Consumed here so only this single reset
			// is forced; the next reset resumes the normal #1096 BY-URL path.
			bool force_by_name_1180 = force_by_name_next_reset_1180;
			force_by_name_next_reset_1180 = false;
			if (!force_by_name_1180 && owned_source_name && owned_source_name[0]) {
				NDIlib_find_create_t fresh_find_desc = {0};
				fresh_find_desc.show_local_sources = true;
				fresh_find_desc.p_groups = nullptr;
				NDIlib_find_instance_t fresh_finder = ndiLib->find_create_v2(&fresh_find_desc);
				if (fresh_finder) {
					for (unsigned w = 0; w < NDI_FRESH_FIND_MAX_WAITS && s->running; ++w) {
						ndiLib->find_wait_for_sources(fresh_finder, NDI_FRESH_FIND_WAIT_MS);
						uint32_t n_fresh = 0;
						const NDIlib_source_t *fresh_sources =
							ndiLib->find_get_current_sources(fresh_finder, &n_fresh);
						const char *fresh_url = ndi_find_url_for_source_name(owned_source_name,
												     fresh_sources, n_fresh);
						if (fresh_url && fresh_url[0]) {
							// Copy the URL out while the finder (owner of the string) is alive.
							bfree(owned_source_url);
							owned_source_url = bstrdup(fresh_url);
							url_resolved_1096 = true;
							break;
						}
					}
					ndiLib->find_destroy(fresh_finder);
				} else {
					obs_log(LOG_WARNING,
						"'%s' ndi_source_thread: reset_ndi_receiver: #1096 fresh finder create failed; keeping name-based connect",
						obs_source_name);
				}
			}
			if (url_resolved_1096) {
				// Empty p_ndi_name => the SDK uses p_url_address directly (bypass the finder).
				recv_desc.source_to_connect_to.p_ndi_name = "";
				recv_desc.source_to_connect_to.p_url_address = owned_source_url;
				obs_log(LOG_INFO,
					"'%s' ndi_source_thread: reset_ndi_receiver: #1096 connect BY-URL '%s' (fresh finder; bypassing poisoned name resolver)",
					obs_source_name, owned_source_url);
			} else {
				// Name-based connect. Two reasons: the fresh finder resolved no URL (the #1096
				// upstream fallback), OR #1180 forced BY-NAME after a confirmed identity mismatch.
				recv_desc.source_to_connect_to.p_ndi_name = owned_source_name;
				recv_desc.source_to_connect_to.p_url_address = nullptr;
				if (force_by_name_1180)
					obs_log(LOG_WARNING,
						"'%s' ndi_source_thread: reset_ndi_receiver: #1180 connect BY-NAME '%s' (forced after a BY-URL identity mismatch; abandoning the wrong-sender URL)",
						obs_source_name, owned_source_name);
				else
					obs_log(LOG_INFO,
						"'%s' ndi_source_thread: reset_ndi_receiver: #1096 connect BY-NAME '%s' (fresh finder resolved no URL; no worse than upstream)",
						obs_source_name, owned_source_name);
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
				//
				// camera-box #1080: NEVER break here. A break exits the receiver loop but leaves
				// s->running TRUE, so ndi_source_update's `if (s->running)` never restarts the
				// thread -- the source is permanently, reattach-proof black until a human recreates
				// it. Since #767 the stale-reconnect watchdog enters this reset path AUTONOMOUSLY,
				// so a transient recv_create_v3 failure here would be an UNATTENDED permanent death.
				// Instead keep the thread (and the #767 watchdog living in it) ALIVE: blank the
				// source, back off (bounded exponential, NEVER capping the retry COUNT), re-arm
				// reset_ndi_receiver, and let the next loop iteration re-attempt the create.
				//
				recv_create_fail_count++;
				obs_log(LOG_ERROR,
					"ERR-407 - Error creating the NDI Receiver '%s' (url='%s') set for '%s' (attempt %u); keeping the receiver thread alive and retrying with backoff",
					owned_source_name ? owned_source_name : "", owned_source_url ? owned_source_url : "",
					obs_source_name, recv_create_fail_count);
				process_empty_frame(s);
				// Give the freshly (re)connected receiver a full #767 stale window once it comes
				// back, instead of judging it against this failed epoch.
				was_disconnected = true;
				// Back off before the retry, chunked in 100 ms slices so OBS shutdown
				// (s->running=false) is never blocked for the whole backoff.
				uint64_t retry_backoff_ns = ndi_recv_create_retry_backoff_ns(recv_create_fail_count);
				uint64_t retry_waited_ns = 0;
				while (s->running && retry_waited_ns < retry_backoff_ns) {
					std::this_thread::sleep_for(std::chrono::milliseconds(100));
					retry_waited_ns += 100ULL * 1000ULL * 1000ULL;
				}
				// camera-box #1180: a forced-BY-NAME reset whose recv_create failed must STAY BY-NAME on
				// the retry (else the #1080 re-entry, with force_by_name_1180 already consumed, would
				// reconnect BY-URL to the wrong-sender URL again).
				if (force_by_name_1180)
					force_by_name_next_reset_1180 = true;
				pthread_mutex_lock(&s->config_mutex);
				s->config.reset_ndi_receiver = true;
				pthread_mutex_unlock(&s->config_mutex);
				continue;
			}
			// camera-box #1080: a successful create clears the retry backoff.
			recv_create_fail_count = 0;

			// camera-box #1180: arm the post-connect identity verify IFF this receiver connected
			// BY-URL (url_resolved_1096). A BY-NAME bind leaves connected_by_url false, so the verify
			// path below never runs for it -- upstream/default behaviour stays byte-identical. Reset
			// the per-bind gates so the one-shot fires on THIS bind's first frames (re-armed on every
			// reset, so it re-fires on every BY-URL reconnect -- the reshuffle window).
			connected_by_url_1180 = url_resolved_1096;
			identity_verify_pending_1180 = url_resolved_1096;
			frames_seen_since_reset_1180 = false;

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
					//
					// camera-box #1097: NEVER break here -- same permanent, reattach-proof death as
					// the #1080 recv_create_v3 break. A break exits the receiver loop but leaves
					// s->running TRUE, so ndi_source_update's `if (s->running)` never restarts the
					// (dead) thread; since #767 the stale watchdog reaches this reset block
					// unattended, so a transient framesync_create failure here is an UNATTENDED
					// permanent death. Retry in place instead: blank the source, back off (its own
					// counter drives the SHARED #1080 exponential backoff), re-arm reset_ndi_receiver,
					// and continue. A valid ndi_receiver already exists, so the NEXT iteration's reset
					// block (recv_destroy frees the valid receiver; framesync_destroy is skipped by its null-guard) cleans up before recreating.
					// Dormant on this appliance (GENLOCK_FORCED_SETTINGS forces PROP_FRAMESYNC false),
					// kept correct so a future framesync-on config can never wedge here.
					//
					framesync_create_fail_count++;
					obs_log(LOG_ERROR,
						"ERR-408 - Error creating the NDI Frame Sync for '%s' for '%s' (attempt %u); keeping the receiver thread alive and retrying with backoff",
						recv_desc.source_to_connect_to.p_ndi_name, obs_source_name,
						framesync_create_fail_count);
					process_empty_frame(s);
					// Give the freshly (re)connected receiver a full #767 stale window once it comes
					// back, instead of judging it against this failed epoch.
					was_disconnected = true;
					// Back off before the retry, chunked in 100 ms slices so OBS shutdown
					// (s->running=false) is never blocked for the whole backoff.
					uint64_t fs_retry_backoff_ns = ndi_recv_create_retry_backoff_ns(framesync_create_fail_count);
					uint64_t fs_retry_waited_ns = 0;
					while (s->running && fs_retry_waited_ns < fs_retry_backoff_ns) {
						std::this_thread::sleep_for(std::chrono::milliseconds(100));
						fs_retry_waited_ns += 100ULL * 1000ULL * 1000ULL;
					}
					// camera-box #1180: preserve a forced-BY-NAME intent across a framesync-create retry too.
					if (force_by_name_1180)
						force_by_name_next_reset_1180 = true;
					pthread_mutex_lock(&s->config_mutex);
					s->config.reset_ndi_receiver = true;
					pthread_mutex_unlock(&s->config_mutex);
					continue;
				}
				// camera-box #1097: a successful framesync create clears its retry backoff.
				framesync_create_fail_count = 0;
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
		// camera-box #1080: defensive -- a create failure left ndi_receiver NULL and re-armed
		// reset_ndi_receiver; if a racing ndi_source_update cleared that flag before this iteration
		// read it, the reset block was skipped with ndi_receiver still NULL. Re-arm + retry rather
		// than dereference NULL in recv_get_no_connections below.
		if (!ndi_receiver) {
			pthread_mutex_lock(&s->config_mutex);
			s->config.reset_ndi_receiver = true;
			pthread_mutex_unlock(&s->config_mutex);
			continue;
		}
		int no_conn = ndiLib->recv_get_no_connections(ndi_receiver);
		if (no_conn == 0) {
#if 0
			obs_log(LOG_DEBUG,
				"'%s' ndi_source_thread: No connection; sleep and restart loop",
				obs_source_name);
#endif
			process_empty_frame(s);

			// camera-box #767: remember we saw a genuine disconnect, so the watchdog gives the
			// next connection a fresh stale window (see the transition refresh below).
			was_disconnected = true;

			// camera-box #1096: a receiver stuck at no_connections==0 (a GRACEFUL sender restart
			// drops the connection cleanly to 0) has NO autonomous recovery -- #767 requires
			// no_connections>0, a by-URL receiver has no name for NDI's internal rebind, and a
			// name-based one re-consults the poisoned finder. For a GENLOCKED source, after the
			// same GENLOCK_RECONNECT_STALE_NS window as #767, force reset_ndi_receiver so the reset
			// block's FRESH finder re-resolves the restarted sender's rotated port. Own timer
			// (last_frame_timestamp froze on disconnect); re-armed at most once per window (natural
			// backoff while the sender is genuinely down); scoped like #767 so non-genlock/aux and
			// stock-OBS inputs are untouched.
			uint64_t now_nc = os_gettime_ns();
			if (no_conn_since_ns == 0)
				no_conn_since_ns = now_nc;
			if ((now_nc - no_conn_since_ns) >= GENLOCK_RECONNECT_STALE_NS &&
			    genlock_source_is_active(s->obs_source)) {
				obs_log(LOG_INFO,
					"genlock: NDI receiver disconnected (no_connections==0) past the stale window -- forcing fresh-finder rebind (sender restart?) '%s'",
					obs_source_name);
				pthread_mutex_lock(&s->config_mutex);
				s->config.reset_ndi_receiver = true;
				pthread_mutex_unlock(&s->config_mutex);
				no_conn_since_ns = now_nc; /* backoff: next re-arm one full window later */
			}

			// This will also slow down the shutdown of OBS when no NDI feed is received.
			std::this_thread::sleep_for(std::chrono::milliseconds(100));
			continue;
		}

		//
		// camera-box #767 (event-critical, 2026-08-13): reconnect-on-sender-restart watchdog.
		// A genlocked, still-CONNECTED source that has delivered no new frame for
		// GENLOCK_RECONNECT_STALE_NS has a stuck (half-open, post sender-reboot) NDI connection --
		// NDI's own name-based reconnect only fires once no_connections drops to 0, which a
		// hard-reboot half-open connection never does (a rebooted sender box gives no graceful TCP
		// close). Force the SAME rebind the manual SetInputSettings recovery triggered live: set
		// reset_ndi_receiver so the next loop iteration's reset block recv_destroy+recv_create_v3
		// re-resolves the (restarted) sender by name. Scoped to genlocked sources
		// (genlock_source_is_active) -- a non-genlock/aux input, or a stock/unpatched OBS where the
		// getter resolves nullptr, is untouched. PURELY ADDITIVE: it never changes the frame-pull
		// throttle, so a source delivering frames continuously never trips the window (the #797
		// steady-state-fps concern is unaffected). Refresh last_frame_timestamp so the fresh
		// receiver gets a full window before it can be judged stale again.
		//
		// Reconnect-epoch guard: while no_conn was 0 the loop took the early-continue above, so
		// last_frame_timestamp froze at the moment the OLD connection went silent. On the FIRST
		// iteration after NDI's own name-based reconnect flips no_conn back to a connection, that
		// frozen timestamp is the WHOLE absence duration old -- evaluating the watchdog now would
		// force a spurious rebind of a connection that just recovered on its own (extra ~1s black
		// on an already-recovering feed). Give the freshly (re)connected receiver a full stale
		// window first; the next real frame (or a genuinely stuck new connection after the full
		// window) drives the decision from here on.
		// camera-box #1096: connected again (no_conn>0) -> clear the no-connection timer so a
		// future disconnect starts a fresh stale window.
		no_conn_since_ns = 0;
		if (was_disconnected) {
			s->last_frame_timestamp = os_gettime_ns();
			was_disconnected = false;
		}
		if (genlock_reconnect_decision(genlock_source_is_active(s->obs_source), no_conn,
					       os_gettime_ns(), s->last_frame_timestamp,
					       GENLOCK_RECONNECT_STALE_NS)) {
			obs_log(LOG_INFO,
				"genlock: NDI receiver stale while connected -- forcing rebind (sender restart?) '%s'",
				obs_source_name);
			pthread_mutex_lock(&s->config_mutex);
			s->config.reset_ndi_receiver = true;
			pthread_mutex_unlock(&s->config_mutex);
			s->last_frame_timestamp = os_gettime_ns();
			continue;
		}

		//
		// camera-box #1180: post-connect BY-URL identity verify. BEGIN
		//
		// A BY-URL bind (#1096) never verifies the connected sender's NAME, so after a sender OBS
		// restart reshuffles the NDI output ports a DIFFERENT sender can inherit our cached URL and
		// deliver frames from the WRONG camera under the configured label -- and once frames flow the
		// #767 stale watchdog (silence-based) never fires, so nothing re-checks. Here, once a BY-URL
		// bind has actually started delivering frames, re-resolve our configured name through a FRESH
		// finder and confirm it still maps to the URL we are bound to; on a confirmed mismatch force a
		// fresh BY-NAME reset. One-shot at first-frames (the required minimum) + a low-rate periodic
		// re-verify (belt-and-braces). Scoped to genlocked sources (mirrors #767/#1096); a
		// BY-NAME-connected receiver never enters here (connected_by_url_1180 stays false), so its
		// behaviour is byte-identical. The fresh finder blocks a bounded window and NEVER holds
		// config_mutex, matching the 'no blocking NDI call under the lock' rule.
		if (connected_by_url_1180 && frames_seen_since_reset_1180 &&
		    genlock_source_is_active(s->obs_source)) {
			if (identity_verify_pending_1180) {
				// ONE-SHOT: clear the flag first so this runs exactly once per bind and never
				// re-fires until the NEXT reset re-arms it (every BY-URL reconnect routes through
				// a reset). No steady-state finder poll, so the blocking finder below never stalls
				// a healthy frame loop (review #1180).
				identity_verify_pending_1180 = false;
				char *verify_url_1180 = nullptr;
				NDIlib_find_create_t verify_find_desc = {0};
				verify_find_desc.show_local_sources = true;
				verify_find_desc.p_groups = nullptr;
				NDIlib_find_instance_t verify_finder = ndiLib->find_create_v2(&verify_find_desc);
				if (verify_finder) {
					for (unsigned w = 0; w < NDI_IDENTITY_VERIFY_MAX_WAITS && s->running; ++w) {
						ndiLib->find_wait_for_sources(verify_finder, NDI_FRESH_FIND_WAIT_MS);
						uint32_t n_v = 0;
						const NDIlib_source_t *v_sources =
							ndiLib->find_get_current_sources(verify_finder, &n_v);
						const char *v_url =
							ndi_find_url_for_source_name(owned_source_name, v_sources, n_v);
						if (v_url && v_url[0]) {
							bfree(verify_url_1180);
							verify_url_1180 = bstrdup(v_url);
							break;
						}
					}
					ndiLib->find_destroy(verify_finder);
				}
				bool mismatch_1180 = ndi_by_url_identity_mismatch(owned_source_url, verify_url_1180);
				if (mismatch_1180) {
					obs_log(LOG_WARNING,
						"genlock: #1180 BY-URL identity MISMATCH '%s' -- configured name now maps to '%s' but the receiver is bound to '%s'; forcing a fresh BY-NAME reset (sender NDI port reshuffle after an OBS restart?)",
						obs_source_name, verify_url_1180 ? verify_url_1180 : "",
						owned_source_url ? owned_source_url : "");
					bfree(verify_url_1180);
					// Force the next reset to connect BY-NAME (abandon the wrong-sender URL), give
					// the fresh connection a full #767 stale window, and re-arm the reset.
					force_by_name_next_reset_1180 = true;
					was_disconnected = true;
					pthread_mutex_lock(&s->config_mutex);
					s->config.reset_ndi_receiver = true;
					pthread_mutex_unlock(&s->config_mutex);
					continue;
				}
				bfree(verify_url_1180);
			}
		}
		//
		// camera-box #1180: post-connect BY-URL identity verify. END
		//

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
		// camera-box #764 (event-critical, 2026-07-15): UNCONDITIONAL keep-alive for genlocked
		// sources -- decode+output (this frame-pull loop) never pauses on hidden, regardless of
		// the stock FPS concern above. Root cause of the original concern: the vanilla
		// worry conflates DECODE cost with UPLOAD (GPU texture) cost. Decode/output here is
		// obs_source_output_video2/obs_source_output_audio, which for an ASYNC source only
		// enqueues into OBS's own frame cache -- the actual GPU texture upload happens lazily,
		// LATER, only for a source that is actually rendered (on program/preview/multiview).
		// A hidden source therefore costs decode CPU but ZERO extra render-thread/GPU budget,
		// live-measured and proven safe (imag: 7 full-1080p decodes hold 60fps; it was the
		// UPLOAD side of #501's monitor-bandwidth finding that was ever expensive, not decode).
		// Scoped to genlocked sources only (genlock_source_is_active) -- a non-genlock/aux
		// input (or this fix running on a stock, unpatched OBS where the getter resolves
		// nullptr) keeps the ORIGINAL stock behavior exactly as before, unchanged.
		if (!obs_source_showing(s->obs_source) && !genlock_source_is_active(s->obs_source)) {
			// Avoid busy-waiting when the source is hidden but kept active.
			std::this_thread::sleep_for(std::chrono::milliseconds(5));
			continue;
		}
		if (!obs_source_showing(s->obs_source) && !s->logged_genlock_keepalive) {
			s->logged_genlock_keepalive = true;
			obs_log(LOG_INFO, "genlock: NDI receiver keep-alive (no sleep on hide) '%s'", obs_source_name);
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
				frames_seen_since_reset_1180 = true; // camera-box #1180: a frame delivered -> arm the identity verify
			}
			ndiLib->framesync_free_video(ndi_frame_sync, &video_frame);

			// TODO: More accurate sleep that subtracts the duration of this loop iteration?
			std::this_thread::sleep_for(std::chrono::milliseconds(5));
		} else {
			//
			// !ndi_frame_sync
			//
			auto t797_c0 = std::chrono::steady_clock::now();
			frame_received =
				ndiLib->recv_capture_v3(ndi_receiver, &video_frame, &audio_frame, nullptr, 100);
			auto t797_c1 = std::chrono::steady_clock::now();

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
				frames_seen_since_reset_1180 = true; // camera-box #1180: a frame delivered -> arm the identity verify

				ndiLib->recv_free_video_v2(ndi_receiver, &video_frame);
				{
					auto t797_c2 = std::chrono::steady_clock::now();
					double cap_ms = std::chrono::duration<double, std::milli>(t797_c1 - t797_c0).count();
					double out_ms = std::chrono::duration<double, std::milli>(t797_c2 - t797_c1).count();
					t797_n++;
					t797_cap_sum += cap_ms;
					t797_out_sum += out_ms;
					if (cap_ms > t797_cap_max)
						t797_cap_max = cap_ms;
					if (out_ms > t797_out_max)
						t797_out_max = out_ms;
					if (std::chrono::duration<double>(t797_c2 - t797_last_log).count() >= 5.0 && t797_n > 0) {
						obs_log(LOG_INFO,
							"recv-timing #797 '%s': n=%llu cap_avg=%.2fms cap_max=%.2fms out_avg=%.2fms out_max=%.2fms",
							obs_source_name, (unsigned long long)t797_n,
							t797_cap_sum / (double)t797_n, t797_cap_max,
							t797_out_sum / (double)t797_n, t797_out_max);
						t797_n = 0;
						t797_cap_sum = t797_cap_max = t797_out_sum = t797_out_max = 0;
						t797_last_log = t797_c2;
					}
				}
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
	bfree(owned_source_url);
	owned_source_url = nullptr;

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
	 * are entirely unaffected (#150 constraint #3). The whitelist knobs —
	 * PROP_SOURCE, PROP_GENLOCK_LATENCY_MS_SRC, PROP_BURN — are never touched by the forcer. */
	const bool genlock_lockdown = obs_data_get_bool(settings, PROP_GENLOCK_FIFO);
	if (genlock_lockdown) {
		force_genlock_certified_settings(settings);
		obs_log(LOG_INFO,
			"'%s' ndi_source_update: #150/#257 genlock lockdown ACTIVE — forced certified "
			"values (ndi_sync=2, ndi_behavior=2, ndi_bw_mode=0, latency=0, "
			"ndi_recv_hw_accel=true, ndi_audio=false, ndi_framesync=false, "
			"ndi_fix_alpha_blending=false, ptz=off); only source + latency + burn are operator-set",
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

	/* camera-box #245/#257: apply the per-source genlock LATENCY (ms). Runtime-resolved like
	 * the fifo setter. libobs clamps to [GENLOCK_LATENCY_MS_MIN, GENLOCK_SOURCE_LATENCY_MS_MAX]
	 * and writes under async_mutex (crash-safe, the #93 UAF lesson). #257 FLOOR is 3 ms (no
	 * "0 = follow global" any more): clamp to [3, 2000] at the input boundary — only reachable
	 * outside that range via a corrupt/hand-edited scene. Floor BEFORE the uint32_t cast so a
	 * negative value cannot wrap to UINT32_MAX; the libobs setter re-clamps authoritatively. */
	if (auto set_latency = resolve_set_genlock_latency_ms()) {
		long long ms = obs_data_get_int(settings, PROP_GENLOCK_LATENCY_MS_SRC);
		if (ms < PROP_GENLOCK_LATENCY_MS_MIN)
			ms = PROP_GENLOCK_LATENCY_MS_MIN;
		else if (ms > PROP_GENLOCK_SOURCE_LATENCY_MS_MAX)
			ms = PROP_GENLOCK_SOURCE_LATENCY_MS_MAX;
		set_latency(obs_source, (uint32_t)ms);
	}

	/* camera-box #257: apply the per-source MEASUREMENT-BURN toggle LIVE (no OBS restart).
	 * Runtime-resolved like the fifo/latency setters; libobs stores the per-source flag and
	 * the QR burn filter reads obs_source_get_genlock_burn(parent) each render to gate the
	 * burn. Persists in the scene via PROP_BURN; toggled at runtime over OBS WebSocket
	 * SetInputSettings genlock_burn (rig-mode test/event, recording-e2e burn-on/off). */
	if (auto set_burn = resolve_set_genlock_burn())
		set_burn(obs_source, obs_data_get_bool(settings, PROP_BURN));

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
