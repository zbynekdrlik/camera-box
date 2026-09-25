/*
 * camera-box issue 1346 — the selectable VIEW of the DRM-lease HDMI output (owner 24.9.2026).
 *
 * The issue-1152 module (obs-drm-output.c) leases the HDMI connector out of X and page-flips GBM
 * scanout buffers; its frame hook used to copy only the Program. This TU adds the second view:
 * the frontend's BUILT-IN Multiview (labels, PVW/PGM tally, the issue-1242 twin cells), never a
 * custom scene. The frontend registers a renderer (obs_drm_output_set_view_renderer); on a
 * MULTIVIEW tick this TU renders it into an sRGB-capable texrender at the connector mode size
 * (the built-in Multiview's sRGB-aware draws — obs_render_main_texture for the Program cell, the
 * scene cells' linear-sRGB path — need an sRGB render target to encode, exactly like the
 * projector's window surface; the dma-buf scanout buffer is LINEAR storage and would drop that
 * encode, a review finding), then claims a scanout buffer through the shared mailbox seam
 * (obs-drm-output-internal.h) and fills it with the SAME raw byte-faithful blit the Program uses.
 *
 * Never degrade the Program (standing rule): the Multiview renders ONLY while the view is
 * MULTIVIEW, and every tick first asks the monitoring-surface budget gate the aux NDI senders use
 * (obs_aux_sender_should_skip -> obs_display_should_skip + the canvas-rate effective divisor): a
 * tick whose remaining budget cannot fit the measured Multiview cost keeps the last frame on
 * scanout, and the anti-starvation floor still renders at least every K+1 ticks. The view calls
 * the self-excluding form with its own previous render, which the previous tick's total already
 * contains (counted twice, a view that fits ran at half rate). A 5 s
 * `drm-output: multiview-render` line reports the real cadence + cost so it can be read next to
 * the Program render audit line's `lagged` counter.
 *
 * The view comes from ~/.camera-box/drm-output.json ("view": program | multiview; absent =
 * program, so imag is unchanged). obs_drm_output_set_view() switches live and persists the key
 * back into that file on one compact line (every other key kept).
 *
 * Linux-only, built only via libobs/cmake/os-linux.cmake. The pure helpers drm_output_parse_view
 * and drm_output_view_tick_action are lift-compiled + truth-tabled by
 * tests/drm_output_view_1346.rs (the view grammar table is shared with the Python mirror).
 */

#if defined(__linux__)

#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "obs.h"
#include "obs-display-budget.h"
#include "obs-drm-output.h"
#include "obs-drm-output-internal.h"
#include "graphics/vec4.h"
#include "util/dstr.h"
#include "util/platform.h"
#include "util/threading.h"

/* -------------------------------------------------------------------------------------------------
 * Pure decision helpers (Tier-0, lift-compiled + truth-tabled by tests/drm_output_view_1346.rs).
 * ------------------------------------------------------------------------------------------------- */

/* The "view" config grammar: absent/empty -> 0 (PROGRAM, the issue-1152 default), "program" -> 0,
 * "multiview" -> 1, anything else -> -1 (unknown; the caller fails OPEN to the Program + WARNs).
 * Exact lowercase match only — the same table the Python mirror (strih_scenes.drm_output_view_of)
 * reads from tests/fixtures/drm_output_view_parity.tsv. */
static int drm_output_parse_view(const char *s)
{
	if (!s || s[0] == '\0')
		return 0;
	if (strcmp(s, "program") == 0)
		return 0;
	if (strcmp(s, "multiview") == 0)
		return 1;
	return -1;
}

/* This tick's action: 1 = PROGRAM copy, 2 = MULTIVIEW render, 0 = keep the last frame. The Program
 * path is never budget-skipped; the Multiview renders only with a registered renderer on a tick
 * the budget gate did not skip; any view other than MULTIVIEW fails OPEN to the Program. */
static int drm_output_view_tick_action(int view, bool have_renderer, bool skip)
{
	if (view != 1)
		return 1;
	if (!have_renderer || skip)
		return 0;
	return 2;
}

_Static_assert(OBS_DRM_OUTPUT_VIEW_PROGRAM == 0 && OBS_DRM_OUTPUT_VIEW_MULTIVIEW == 1,
	       "drm_output_parse_view returns the enum values as literals");
_Static_assert(DRM_OUTPUT_TICK_NOTHING == 0 && DRM_OUTPUT_TICK_PROGRAM == 1 && DRM_OUTPUT_TICK_MULTIVIEW == 2,
	       "drm_output_view_tick_action returns the DRM_OUTPUT_TICK_* values as literals");

/* The throttle MARKER + upper bound the built-in Multiview projector uses (OBSProjector.cpp sets
 * render divisor 2); obs_aux_sender_should_skip derives the effective cadence from the canvas rate
 * (30 fps canvas -> every tick, budget-gated only; 60 fps canvas -> every other tick). */
#define DRM_OUTPUT_MV_RENDER_DIVISOR 2u

/* -------------------------------------------------------------------------------------------------
 * State. `view` is atomic (UI thread writes, graphics thread reads). The renderer + the budget and
 * audit fields are guarded by the GRAPHICS CONTEXT: the frame hook holds it for the whole tick and
 * obs_drm_output_set_view_renderer takes it around the swap. config_path is guarded by persist_lock.
 * ------------------------------------------------------------------------------------------------- */
static struct {
	volatile long view;
	pthread_mutex_t persist_lock;
	char config_path[1024];

	obs_drm_output_view_render_t render;
	void *render_param;
	gs_texrender_t *texrender; /* the Multiview's sRGB render target, created on first render */
	bool warned_no_renderer;
	bool warned_no_texrender;
	bool bind_live_logged;
	uint32_t frame_counter;
	uint32_t consecutive_skips;
	uint64_t ewma_ns;
	/* main design 5840501628: the ns of the render in the PREVIOUS call, handed to the budget gate
	 * once (read then cleared), so the gate does not count this view's own render twice. 0 after a
	 * skip, a program tick or a missed call. last_self_ns = the value the gate last used (audit). */
	uint64_t last_render_ns;
	uint64_t last_self_ns;

	uint64_t win_start_ns;
	uint32_t win_renders;
	uint32_t win_skips;
	uint64_t win_sum_ns;
	uint64_t win_max_ns;
} g_view = {
	.view = OBS_DRM_OUTPUT_VIEW_PROGRAM,
	.persist_lock = PTHREAD_MUTEX_INITIALIZER,
};

static const char *drm_output_view_name(long view)
{
	return view == OBS_DRM_OUTPUT_VIEW_MULTIVIEW ? "multiview" : "program";
}

static void drm_output_view_reset_window_locked(void)
{
	g_view.win_start_ns = 0;
	g_view.win_renders = 0;
	g_view.win_skips = 0;
	g_view.win_sum_ns = 0;
	g_view.win_max_ns = 0;
}

void drm_output_view_configure(const char *view, const char *config_path)
{
	int v = drm_output_parse_view(view);
	if (v < 0) {
		blog(LOG_WARNING,
		     "drm-output: unknown \"view\":\"%s\" in %s -- using program (want program or multiview)",
		     view, config_path ? config_path : "<no config>");
		v = OBS_DRM_OUTPUT_VIEW_PROGRAM;
	}
	os_atomic_set_long(&g_view.view, v);
	pthread_mutex_lock(&g_view.persist_lock);
	snprintf(g_view.config_path, sizeof(g_view.config_path), "%s", config_path ? config_path : "");
	pthread_mutex_unlock(&g_view.persist_lock);
	blog(LOG_INFO, "drm-output: view=%s (from %s)", drm_output_view_name(v),
	     config_path ? config_path : "<no config>");
}

enum obs_drm_output_view obs_drm_output_get_view(void)
{
	return os_atomic_load_long(&g_view.view) == OBS_DRM_OUTPUT_VIEW_MULTIVIEW ? OBS_DRM_OUTPUT_VIEW_MULTIVIEW
										  : OBS_DRM_OUTPUT_VIEW_PROGRAM;
}

void obs_drm_output_set_view_renderer(obs_drm_output_view_render_t render, void *param)
{
	/* The frame hook calls the renderer while it holds the graphics context, so taking the context
	 * here means no render is in flight across the swap: after this returns, the previous renderer
	 * is never called again and the frontend may free its Multiview. */
	obs_enter_graphics();
	g_view.render = render;
	g_view.render_param = param;
	if (!render && g_view.texrender) { /* no Multiview any more: free its render target too */
		gs_texrender_destroy(g_view.texrender);
		g_view.texrender = NULL;
	}
	g_view.warned_no_renderer = false;
	g_view.warned_no_texrender = false;
	g_view.bind_live_logged = false;
	g_view.frame_counter = 0;
	g_view.consecutive_skips = 0;
	g_view.ewma_ns = 0;
	g_view.last_render_ns = 0;
	g_view.last_self_ns = 0;
	drm_output_view_reset_window_locked();
	obs_leave_graphics();
	blog(LOG_INFO, "drm-output: multiview renderer %s", render ? "registered" : "cleared");
}

void drm_output_view_gl_teardown(void)
{
	obs_enter_graphics();
	if (g_view.texrender) {
		gs_texrender_destroy(g_view.texrender);
		g_view.texrender = NULL;
	}
	obs_leave_graphics();
}

/* Rewrite the persisted config with the new "view" (every other key kept, one compact line, an
 * atomic temp-file rename). Caller holds persist_lock. Returns NULL on success, else the reason
 * the choice was NOT persisted (named in the log line). */
static const char *drm_output_view_persist_locked(const char *name)
{
	if (g_view.config_path[0] == '\0')
		return "no drm-output config path (the autostart found no config)";
	obs_data_t *data = obs_data_create_from_json_file(g_view.config_path);
	if (!data)
		return "config unreadable (missing or not JSON)";
	obs_data_set_string(data, "view", name);
	const char *reason = "config unreadable (JSON serialisation failed)";
	const char *json = obs_data_get_json(data);
	if (json) {
		struct dstr line;
		dstr_init_copy(&line, json);
		dstr_cat(&line, "\n");
		reason = os_quick_write_utf8_file_safe(g_view.config_path, line.array, line.len, false, "tmp", NULL)
				 ? NULL
				 : "write failed (temp file or rename)";
		dstr_free(&line);
	}
	obs_data_release(data);
	return reason;
}

bool obs_drm_output_set_view(enum obs_drm_output_view view)
{
	const long v = view == OBS_DRM_OUTPUT_VIEW_MULTIVIEW ? OBS_DRM_OUTPUT_VIEW_MULTIVIEW
							     : OBS_DRM_OUTPUT_VIEW_PROGRAM;
	const long prev = os_atomic_set_long(&g_view.view, v);
	const char *name = drm_output_view_name(v);

	pthread_mutex_lock(&g_view.persist_lock);
	const char *reason = drm_output_view_persist_locked(name);
	pthread_mutex_unlock(&g_view.persist_lock);

	if (!reason)
		blog(LOG_INFO, "drm-output: view %s -> %s (persisted)", drm_output_view_name(prev), name);
	else
		blog(LOG_WARNING, "drm-output: view %s -> %s (NOT persisted -- %s)", drm_output_view_name(prev), name,
		     reason);
	return reason == NULL;
}

/* Render the registered Multiview into the sRGB texrender, then blit it raw into a claimed
 * scanout buffer. Graphics thread, context held. */
static void drm_output_view_render_multiview(void)
{
	uint32_t w, h;
	drm_output_mode_size(&w, &h);
	if (!g_view.texrender)
		g_view.texrender = gs_texrender_create(GS_BGRA, GS_ZS_NONE);

	const uint64_t t0 = os_gettime_ns();

	if (g_view.texrender)
		gs_texrender_reset(g_view.texrender);
	if (!g_view.texrender || !gs_texrender_begin(g_view.texrender, w, h)) {
		if (!g_view.warned_no_texrender) {
			g_view.warned_no_texrender = true;
			blog(LOG_WARNING,
			     "drm-output: multiview texrender unavailable (%ux%u) -- the HDMI keeps the last frame",
			     w, h);
		}
		return;
	}
	/* The frame a projector's draw callback gets (render_display_begin): black clear, depth off,
	 * no culling, an ortho over the whole target; the texrender set the viewport. The Multiview
	 * letterboxes the canvas aspect into it itself. */
	struct vec4 black;
	vec4_zero(&black);
	black.w = 1.0f;
	gs_clear(GS_CLEAR_COLOR, &black, 1.0f, 0);
	gs_enable_depth_test(false);
	gs_set_cull_mode(GS_NEITHER);
	gs_ortho(0.0f, (float)w, 0.0f, (float)h, -100.0f, 100.0f);
	gs_blend_state_push();
	gs_reset_blend_state();

	g_view.render(g_view.render_param, w, h);

	gs_blend_state_pop();
	gs_texrender_end(g_view.texrender);

	const int idx = drm_output_claim_render_buf();
	if (idx < 0)
		return; /* nothing writable this tick — the last frame stays on scanout */
	if (drm_output_blit_raw(gs_texrender_get_texture(g_view.texrender), idx))
		drm_output_publish_render_buf(idx);
	else
		return; /* the claimed buffer stays role-free; nothing new reached the scanout */

	const uint64_t t1 = os_gettime_ns();
	const uint64_t dt = t1 > t0 ? t1 - t0 : 0;
	g_view.ewma_ns = g_view.ewma_ns ? (g_view.ewma_ns * 3 + dt) / 4 : dt;
	g_view.last_render_ns = dt;
	g_view.consecutive_skips = 0;
	g_view.win_renders++;
	g_view.win_sum_ns += dt;
	if (dt > g_view.win_max_ns)
		g_view.win_max_ns = dt;

	if (!g_view.bind_live_logged) {
		g_view.bind_live_logged = true;
		blog(LOG_INFO,
		     "drm-output: multiview bind LIVE (built-in Multiview rendered at %ux%u and blitted into the "
		     "scanout buffer)",
		     w, h);
	}
}

/* The ~5 s multiview render-cost line (report-only), read next to the Program render audit `lagged`. */
static void drm_output_view_audit(uint64_t now)
{
	if (g_view.win_start_ns == 0) {
		g_view.win_start_ns = now;
		return;
	}
	const uint64_t elapsed = now > g_view.win_start_ns ? now - g_view.win_start_ns : 0;
	if (elapsed < MULTIVIEW_AUDIT_WINDOW_NS)
		return;
	const double win_s = (double)elapsed / 1000000000.0;
	const double avg_ms =
		g_view.win_renders ? ((double)g_view.win_sum_ns / (double)g_view.win_renders) / 1000000.0 : 0.0;
	uint32_t w, h;
	drm_output_mode_size(&w, &h);
	blog(LOG_INFO,
	     "drm-output: multiview-render rendered_fps=%.1f skipped=%u avg_ms=%.2f max_ms=%.2f ewma_ms=%.2f "
	     "cx=%u cy=%u self_ns=%llu",
	     (double)g_view.win_renders / win_s, g_view.win_skips, avg_ms, (double)g_view.win_max_ns / 1000000.0,
	     (double)g_view.ewma_ns / 1000000.0, w, h, (unsigned long long)g_view.last_self_ns);
	g_view.win_start_ns = now;
	g_view.win_renders = 0;
	g_view.win_skips = 0;
	g_view.win_sum_ns = 0;
	g_view.win_max_ns = 0;
}

int drm_output_view_frame(void)
{
	const int view = (int)os_atomic_load_long(&g_view.view);
	/* main design 5840501628: the previous call's render cost goes to the gate exactly once. A skip,
	 * a program tick or a call the backend skipped leaves 0, so a render is never subtracted from a
	 * tick that did not contain it (the vk-direct hook skips this call while a READY image waits). */
	const uint64_t self_last_ns = g_view.last_render_ns;
	g_view.last_render_ns = 0;
	if (view != OBS_DRM_OUTPUT_VIEW_MULTIVIEW) {
		if (g_view.win_start_ns != 0)
			drm_output_view_reset_window_locked();
		return drm_output_view_tick_action(view, g_view.render != NULL, false);
	}

	const bool have_renderer = g_view.render != NULL;
	bool skip = false;
	if (have_renderer) {
		g_view.frame_counter++;
		g_view.last_self_ns = self_last_ns;
		skip = obs_aux_sender_should_skip_excluding(DRM_OUTPUT_MV_RENDER_DIVISOR, g_view.frame_counter,
							    g_view.ewma_ns, g_view.consecutive_skips, self_last_ns);
	} else if (!g_view.warned_no_renderer) {
		g_view.warned_no_renderer = true;
		blog(LOG_INFO, "drm-output: view=multiview but no Multiview renderer registered yet -- keeping "
			       "the last frame");
	}

	const int action = drm_output_view_tick_action(view, have_renderer, skip);
	if (action == DRM_OUTPUT_TICK_MULTIVIEW) {
		drm_output_view_render_multiview();
	} else if (skip) {
		g_view.consecutive_skips++;
		g_view.win_skips++;
	}
	if (have_renderer)
		drm_output_view_audit(os_gettime_ns());
	return action;
}

#endif /* defined(__linux__) */
