/*
 * camera-box issue 1424 (owner 24.10.2026: "to musi vediet fungovat aj bez monitora aj po zapojeni
 * monitora aj po zapnuti!!!"; main design 6386467303, layer 3) -- the RETRY SUPERVISOR of the vk-direct
 * HDMI outputs.
 *
 * Before: each output started ONCE, at the OBS autostart. strih-pp booted 24.10.2026 with the venue
 * dark: the Multiview output's acquire was refused, the Program output's monitor was off ("no Vulkan
 * device drives output 'HDMI-1'"), both stayed dormant, and switching the monitors on changed nothing
 * until OBS was restarted. And a LIVE output whose monitor went away gave up after ten 1 s rebuild tries
 * and stayed dark too.
 *
 * Now one thread looks at every vk-direct output ~4 times a second and runs the pure decision of
 * obs-drm-output-retry.h (parity-gated against src/drm_output_retry.rs):
 *   - a DORMANT output (never started, or released) retries its whole start -- the same
 *     drm_output_inst_start_locked the autostart runs (its EDID, the vk-direct open, its audio leg) --
 *     2 s apart for the first attempts, then 5 s; 30 s while X RandR reads its connector disconnected
 *     (nothing there to drive), and at once when it reads connected again;
 *   - an output that is open but whose present loop died (a lost device, a display that now offers
 *     another mode size, a lone display whose rebuild gave up -- the present loop itself keeps a display
 *     acquired through an unplug while another output is open) is stopped (its display released) and
 *     goes back to the retry;
 *   - never an OBS restart.
 *
 * Logging: one line per state change. A failed attempt's own lines (the open path's acquire / no-device
 * / start FAILED lines, a dozen per attempt) are CAPTURED on this thread (drm_output_blog, the module's
 * one log entry, obs-drm-output-logcap.h): only a change of its first warning (the cause) is logged. A
 * successful attempt flushes its captured lines in order, then `retry: '<c>' LIVE again`. The present
 * thread an open starts logs on its own thread, so its first `solid-present #1` line can appear before
 * the flushed open lines.
 *
 * Locking: the registry lock (g_outs_lock, obs-drm-output-outputs.c) around each look + its action, the
 * same lock the autostart and obs_drm_output_stop hold around their starts / stops; obs_drm_output_stop
 * stops + joins this thread FIRST, outside that lock. The X RandR read runs outside the lock on this
 * thread's own X connection. A stop of a dead output takes the graphics context inside the registry
 * lock, exactly as obs_drm_output_stop always has (the graphics thread never takes the registry lock).
 *
 * Linux-only, built only via libobs/cmake/os-linux.cmake.
 */

#if defined(__linux__)

#include <pthread.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <xcb/randr.h>
#include <xcb/xcb.h>

#include "obs.h"
#include "obs-drm-output.h"
#include "obs-drm-output-internal.h"
#include "obs-drm-output-retry.h"
#include "util/platform.h"
#include "util/threading.h"

/* ---- the module's one log entry: plain blogva, or captured on the supervisor's attempt ---- */

#define DRM_OUTPUT_LOGCAP_LINES 64
#define DRM_OUTPUT_LOGCAP_LEN 384

struct drm_output_logcap {
	int n;
	unsigned dropped;
	int level[DRM_OUTPUT_LOGCAP_LINES];
	char line[DRM_OUTPUT_LOGCAP_LINES][DRM_OUTPUT_LOGCAP_LEN];
	/* the FIRST LOG_WARNING / LOG_ERROR line of the attempt: the cause (the vk open's own refusal); the
	 * lines after it are the generic "start FAILED" echoes of each layer */
	char reason[DRM_OUTPUT_LOGCAP_LEN];
};

static __thread struct drm_output_logcap *t_logcap; /* set only on the supervisor's thread, per attempt */
static struct drm_output_logcap g_logcap;            /* the supervisor's one capture buffer */

void drm_output_blog(int log_level, const char *format, ...)
{
	va_list ap;
	va_start(ap, format);
	struct drm_output_logcap *c = t_logcap;
	if (!c) {
		blogva(log_level, format, ap);
		va_end(ap);
		return;
	}
	char buf[DRM_OUTPUT_LOGCAP_LEN];
	vsnprintf(buf, sizeof(buf), format, ap);
	va_end(ap);
	if (log_level <= LOG_WARNING && c->reason[0] == '\0')
		snprintf(c->reason, sizeof(c->reason), "%s", buf);
	if (c->n < DRM_OUTPUT_LOGCAP_LINES) {
		c->level[c->n] = log_level;
		snprintf(c->line[c->n], sizeof(c->line[c->n]), "%s", buf);
		c->n++;
	} else {
		c->dropped++;
	}
}

static void drm_output_logcap_flush(const struct drm_output_logcap *c)
{
	for (int i = 0; i < c->n; i++)
		blog(c->level[i], "%s", c->line[i]);
	if (c->dropped)
		blog(LOG_INFO, "drm-output: retry: %u more line(s) of that start were not kept", c->dropped);
}

/* The reason without its `drm-output: ` prefix (a shorter state line). */
static const char *drm_output_reason_text(const char *reason)
{
	static const char prefix[] = "drm-output: ";
	if (strncmp(reason, prefix, sizeof(prefix) - 1) == 0)
		return reason + sizeof(prefix) - 1;
	return reason[0] ? reason : "no reason was logged";
}

/* ---- the X RandR connection status of a connector (the supervisor's own X connection) ---- */

/* false only when X RandR reads `name` DISCONNECTED; an unreadable X, an unknown status or a name X
 * does not list reads as connected (try rather than wait). */
static bool drm_output_retry_connected(xcb_connection_t **xc, const char *name)
{
	if (*xc && xcb_connection_has_error(*xc)) {
		xcb_disconnect(*xc);
		*xc = NULL;
	}
	if (!*xc) {
		*xc = xcb_connect(NULL, NULL);
		if (xcb_connection_has_error(*xc)) {
			xcb_disconnect(*xc);
			*xc = NULL;
			return true;
		}
	}
	const xcb_setup_t *setup = xcb_get_setup(*xc);
	xcb_screen_iterator_t it = xcb_setup_roots_iterator(setup);
	if (!it.rem)
		return true;
	xcb_randr_get_screen_resources_current_reply_t *res = xcb_randr_get_screen_resources_current_reply(
		*xc, xcb_randr_get_screen_resources_current(*xc, it.data->root), NULL);
	if (!res)
		return true;
	bool connected = true;
	const xcb_randr_output_t *outputs = xcb_randr_get_screen_resources_current_outputs(res);
	const int n_out = xcb_randr_get_screen_resources_current_outputs_length(res);
	const size_t name_len = strlen(name);
	for (int i = 0; i < n_out; i++) {
		xcb_randr_get_output_info_reply_t *oi = xcb_randr_get_output_info_reply(
			*xc, xcb_randr_get_output_info(*xc, outputs[i], res->config_timestamp), NULL);
		if (!oi)
			continue;
		const int nlen = xcb_randr_get_output_info_name_length(oi);
		const bool match = nlen >= 0 && (size_t)nlen == name_len &&
				   memcmp(xcb_randr_get_output_info_name(oi), name, name_len) == 0;
		if (match)
			connected = oi->connection != XCB_RANDR_CONNECTION_DISCONNECTED;
		free(oi);
		if (match)
			break;
	}
	free(res);
	return connected;
}

/* ---- the supervisor thread ---- */

static pthread_t g_retry_thread;
static bool g_retry_started; /* the obs_startup / obs_shutdown thread only (the autostart, the stop) */
static volatile bool g_retry_run;
static os_event_t *g_retry_stop;

/* Per output: the pure state, the last reason logged, the attempts since it went dormant. Supervisor
 * thread only. */
struct drm_output_retry_slot {
	struct drm_output_retry st;
	char reason[DRM_OUTPUT_LOGCAP_LEN];
	unsigned attempts;
};

static void drm_output_retry_look(struct drm_output_retry_slot *rs, struct drm_output_inst *inst, bool connected)
{
	const bool started = inst->started && inst->ops->active(inst);
	const bool alive = started && inst->ops->alive(inst);
	const int prev = rs->st.state;
	const int act = drm_output_retry_step(&rs->st, started, alive, connected, os_gettime_ns());

	if (prev == DRM_OUTPUT_RETRY_LIVE && rs->st.state == DRM_OUTPUT_RETRY_DORMANT) {
		rs->attempts = 0;
		rs->reason[0] = '\0';
		blog(LOG_INFO,
		     "drm-output: retry: '%s' dormant -- its start at the OBS launch failed (read the lines above); "
		     "retrying every %u s (%u s while X reads it disconnected, at once when it comes back)",
		     inst->connector, (unsigned)(DRM_OUTPUT_RETRY_FAST_NS / 1000000000ULL),
		     (unsigned)(DRM_OUTPUT_RETRY_UNPLUGGED_NS / 1000000000ULL));
		return;
	}
	if (act == DRM_OUTPUT_RETRY_RELEASE) {
		blog(LOG_WARNING,
		     "drm-output: retry: '%s' lost its present loop -- releasing the display and retrying its start "
		     "(no OBS restart)",
		     inst->connector);
		drm_output_inst_stop_locked(inst);
		drm_output_retry_released(&rs->st, connected, os_gettime_ns());
		rs->attempts = 0;
		rs->reason[0] = '\0';
		return;
	}
	if (act != DRM_OUTPUT_RETRY_START)
		return;

	rs->attempts++;
	g_logcap.n = 0;
	g_logcap.dropped = 0;
	g_logcap.reason[0] = '\0';
	t_logcap = &g_logcap;
	const bool ok = drm_output_inst_start_locked(inst);
	t_logcap = NULL;
	drm_output_retry_attempted(&rs->st, ok, connected, os_gettime_ns());
	if (ok) {
		drm_output_logcap_flush(&g_logcap);
		blog(LOG_INFO, "drm-output: retry: '%s' LIVE again after %u attempt(s) -- no OBS restart", inst->connector,
		     rs->attempts);
		rs->attempts = 0;
		rs->reason[0] = '\0';
		return;
	}
	if (strcmp(g_logcap.reason, rs->reason) != 0) {
		snprintf(rs->reason, sizeof(rs->reason), "%s", g_logcap.reason);
		blog(LOG_INFO,
		     "drm-output: retry: '%s' dormant -- %s (attempt %u, X reads it %s; this line repeats only when the "
		     "reason changes)",
		     inst->connector, drm_output_reason_text(rs->reason), rs->attempts,
		     connected ? "connected" : "disconnected");
	}
}

static void *drm_output_retry_thread(void *arg)
{
	(void)arg;
	os_set_thread_name("drm-output-retry");
	struct drm_output_retry_slot slots[DRM_OUTPUT_MAX_OUTPUTS];
	memset(slots, 0, sizeof(slots));
	xcb_connection_t *xc = NULL;
	while (os_atomic_load_bool(&g_retry_run)) {
		if (os_event_timedwait(g_retry_stop, 250) == 0)
			break;
		const int count = drm_output_inst_count();
		for (int i = 0; i < count && i < DRM_OUTPUT_MAX_OUTPUTS; i++) {
			struct drm_output_inst *inst = drm_output_inst_at(i);
			if (!inst || inst->backend != OBS_DRM_OUTPUT_BACKEND_VK_DIRECT || !inst->ops->alive)
				continue;
			const bool connected = drm_output_retry_connected(&xc, inst->connector);
			drm_output_registry_lock();
			if (os_atomic_load_bool(&g_retry_run))
				drm_output_retry_look(&slots[i], inst, connected);
			drm_output_registry_unlock();
		}
	}
	if (xc)
		xcb_disconnect(xc);
	return NULL;
}

void drm_output_retry_start(void)
{
	bool any = false;
	for (int i = 0; i < drm_output_inst_count(); i++) {
		const struct drm_output_inst *inst = drm_output_inst_at(i);
		if (inst && inst->backend == OBS_DRM_OUTPUT_BACKEND_VK_DIRECT)
			any = true;
	}
	if (!any || g_retry_started)
		return;
	if (os_event_init(&g_retry_stop, OS_EVENT_TYPE_MANUAL) != 0) {
		blog(LOG_WARNING, "drm-output: retry: could not create the supervisor's stop event -- no retry");
		return;
	}
	os_atomic_set_bool(&g_retry_run, true);
	if (pthread_create(&g_retry_thread, NULL, drm_output_retry_thread, NULL) != 0) {
		os_atomic_set_bool(&g_retry_run, false);
		os_event_destroy(g_retry_stop);
		g_retry_stop = NULL;
		blog(LOG_WARNING, "drm-output: retry: could not create the supervisor thread -- no retry");
		return;
	}
	g_retry_started = true;
	blog(LOG_INFO,
	     "drm-output: retry: supervisor running -- a dormant vk-direct output retries its start, one that loses "
	     "its present loop is released and retried, never an OBS restart (issue 1424)");
}

void drm_output_retry_stop(void)
{
	if (!g_retry_started)
		return;
	os_atomic_set_bool(&g_retry_run, false);
	os_event_signal(g_retry_stop);
	pthread_join(g_retry_thread, NULL);
	os_event_destroy(g_retry_stop);
	g_retry_stop = NULL;
	g_retry_started = false;
}

#endif /* defined(__linux__) */
