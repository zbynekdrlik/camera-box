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
 * program, so imag is unchanged). obs_drm_output_set_view_of() switches live and persists the key
 * back into that output's entry of the file on one compact line (every other key kept).
 *
 * issue 1424 part 2: everything here is PER OUTPUT INSTANCE (struct drm_output_view_state inside
 * struct drm_output_inst): two outputs each carry their own view, renderer, texrender and budget
 * fields, so HDMI-0 can show the Multiview while HDMI-1 shows the Program.
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

/* main design 5840501628 (review round 1): the render cost the budget gate may leave out on THIS
 * call. obs_aux_sender_should_skip_excluding() reads the previous graphics tick's total, which holds
 * the view's render only when that render ran in the immediately previous tick. obs_get_total_frames()
 * advances once per processed tick (by the lag count on a lagged one), so that is exactly a difference
 * of 1. Anything else -- a frame-hook call a backend skipped (a disarmed output, a dead vk-direct
 * present loop, a GL bind failure), a lagged tick, no render yet -- leaves nothing out, which counts
 * the render in full (the pre-fix term). The u32 counter wraps. */
static uint64_t drm_output_view_self_last_ns(uint64_t last_render_ns, uint32_t render_frame, uint32_t now_frame)
{
	return (uint32_t)(now_frame - render_frame) == 1u ? last_render_ns : 0;
}

_Static_assert(OBS_DRM_OUTPUT_VIEW_PROGRAM == 0 && OBS_DRM_OUTPUT_VIEW_MULTIVIEW == 1,
	       "drm_output_parse_view returns the enum values as literals");
_Static_assert(DRM_OUTPUT_TICK_NOTHING == 0 && DRM_OUTPUT_TICK_PROGRAM == 1 && DRM_OUTPUT_TICK_MULTIVIEW == 2,
	       "drm_output_view_tick_action returns the DRM_OUTPUT_TICK_* values as literals");

/* The throttle MARKER + upper bound the built-in Multiview projector uses (OBSProjector.cpp sets
 * render divisor 2); obs_aux_sender_should_skip derives the effective cadence from the canvas rate
 * (30 fps canvas -> every tick, budget-gated only; 60 fps canvas -> every other tick). */
#define DRM_OUTPUT_MV_RENDER_DIVISOR 2u

static const char *drm_output_view_name(long view)
{
	return view == OBS_DRM_OUTPUT_VIEW_MULTIVIEW ? "multiview" : "program";
}

/* The "view" grammar for the config TU (obs-drm-output-outputs.c), which cannot see the static parser. */
int drm_output_view_from_config(const char *value)
{
	return drm_output_parse_view(value);
}

static void drm_output_view_reset_window_locked(struct drm_output_view_state *v)
{
	v->win_start_ns = 0;
	v->win_renders = 0;
	v->win_skips = 0;
	v->win_sum_ns = 0;
	v->win_max_ns = 0;
}

void drm_output_view_configure(struct drm_output_inst *inst, int view)
{
	const long v = view == OBS_DRM_OUTPUT_VIEW_MULTIVIEW ? OBS_DRM_OUTPUT_VIEW_MULTIVIEW : OBS_DRM_OUTPUT_VIEW_PROGRAM;
	os_atomic_set_long(&inst->view.view, v);
	blog(LOG_INFO, "drm-output: view=%s on '%s'", drm_output_view_name(v), inst->connector);
}

enum obs_drm_output_view obs_drm_output_get_view_of(int idx)
{
	struct drm_output_inst *inst = drm_output_inst_at(idx);
	if (!inst)
		return OBS_DRM_OUTPUT_VIEW_PROGRAM;
	return os_atomic_load_long(&inst->view.view) == OBS_DRM_OUTPUT_VIEW_MULTIVIEW ? OBS_DRM_OUTPUT_VIEW_MULTIVIEW
										     : OBS_DRM_OUTPUT_VIEW_PROGRAM;
}

enum obs_drm_output_view obs_drm_output_get_view(void)
{
	return obs_drm_output_get_view_of(0);
}

void obs_drm_output_set_view_renderer_of(int idx, obs_drm_output_view_render_t render, void *param)
{
	struct drm_output_inst *inst = drm_output_inst_at(idx);
	if (!inst) {
		blog(LOG_WARNING, "drm-output: multiview renderer for output %d ignored -- no such output", idx);
		return;
	}
	struct drm_output_view_state *v = &inst->view;
	/* The frame hook calls the renderer while it holds the graphics context, so taking the context
	 * here means no render is in flight across the swap: after this returns, the previous renderer
	 * is never called again and the frontend may free its Multiview. */
	obs_enter_graphics();
	v->render = render;
	v->render_param = param;
	if (!render && v->texrender) { /* no Multiview any more: free its render target too */
		gs_texrender_destroy(v->texrender);
		v->texrender = NULL;
	}
	v->warned_no_renderer = false;
	v->warned_no_texrender = false;
	v->bind_live_logged = false;
	v->frame_counter = 0;
	v->consecutive_skips = 0;
	v->ewma_ns = 0;
	v->last_render_ns = 0;
	v->last_render_frame = 0;
	v->last_self_ns = 0;
	drm_output_view_reset_window_locked(v);
	obs_leave_graphics();
	blog(LOG_INFO, "drm-output: multiview renderer %s on '%s'", render ? "registered" : "cleared", inst->connector);
}

void obs_drm_output_set_view_renderer(obs_drm_output_view_render_t render, void *param)
{
	obs_drm_output_set_view_renderer_of(0, render, param);
}

void drm_output_view_gl_teardown(struct drm_output_inst *inst)
{
	obs_enter_graphics();
	if (inst->view.texrender) {
		gs_texrender_destroy(inst->view.texrender);
		inst->view.texrender = NULL;
	}
	obs_leave_graphics();
}

bool obs_drm_output_set_view_of(int idx, enum obs_drm_output_view view)
{
	struct drm_output_inst *inst = drm_output_inst_at(idx);
	if (!inst) {
		blog(LOG_WARNING, "drm-output: view switch for output %d ignored -- no such output", idx);
		return false;
	}
	const long v = view == OBS_DRM_OUTPUT_VIEW_MULTIVIEW ? OBS_DRM_OUTPUT_VIEW_MULTIVIEW
							     : OBS_DRM_OUTPUT_VIEW_PROGRAM;
	const long prev = os_atomic_set_long(&inst->view.view, v);
	const char *name = drm_output_view_name(v);
	const char *reason = drm_output_config_persist_view(inst, name);

	if (!reason)
		blog(LOG_INFO, "drm-output: view %s -> %s on '%s' (persisted)", drm_output_view_name(prev), name,
		     inst->connector);
	else
		blog(LOG_WARNING, "drm-output: view %s -> %s on '%s' (NOT persisted -- %s)", drm_output_view_name(prev),
		     name, inst->connector, reason);
	return reason == NULL;
}

bool obs_drm_output_set_view(enum obs_drm_output_view view)
{
	return obs_drm_output_set_view_of(0, view);
}

/* Render the registered Multiview of `inst` into its sRGB texrender, then blit it raw into a claimed
 * scanout buffer. Graphics thread, context held. */
static void drm_output_view_render_multiview(struct drm_output_inst *inst)
{
	struct drm_output_view_state *v = &inst->view;
	uint32_t w, h;
	drm_output_mode_size(inst, &w, &h);
	if (!v->texrender)
		v->texrender = gs_texrender_create(GS_BGRA, GS_ZS_NONE);

	const uint64_t t0 = os_gettime_ns();

	if (v->texrender)
		gs_texrender_reset(v->texrender);
	if (!v->texrender || !gs_texrender_begin(v->texrender, w, h)) {
		if (!v->warned_no_texrender) {
			v->warned_no_texrender = true;
			blog(LOG_WARNING,
			     "drm-output: multiview texrender unavailable (%ux%u) on '%s' -- the HDMI keeps the last frame",
			     w, h, inst->connector);
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

	v->render(v->render_param, w, h);

	gs_blend_state_pop();
	gs_texrender_end(v->texrender);

	/* The Multiview cost is spent in this tick even when no scanout buffer is free or the blit fails,
	 * so record it (and the tick) now; a published render overwrites it with the full cost below. */
	const uint64_t t_rendered = os_gettime_ns();
	v->last_render_ns = t_rendered > t0 ? t_rendered - t0 : 0;
	v->last_render_frame = obs_get_total_frames();

	const int idx = drm_output_claim_render_buf(inst);
	if (idx < 0)
		return; /* nothing writable this tick — the last frame stays on scanout */
	if (drm_output_blit_raw(inst, gs_texrender_get_texture(v->texrender), idx))
		drm_output_publish_render_buf(inst, idx);
	else
		return; /* the claimed buffer stays role-free; nothing new reached the scanout */

	const uint64_t t1 = os_gettime_ns();
	const uint64_t dt = t1 > t0 ? t1 - t0 : 0;
	v->ewma_ns = v->ewma_ns ? (v->ewma_ns * 3 + dt) / 4 : dt;
	v->last_render_ns = dt;
	v->consecutive_skips = 0;
	v->win_renders++;
	v->win_sum_ns += dt;
	if (dt > v->win_max_ns)
		v->win_max_ns = dt;

	if (!v->bind_live_logged) {
		v->bind_live_logged = true;
		blog(LOG_INFO,
		     "drm-output: multiview bind LIVE (built-in Multiview rendered at %ux%u and blitted into the "
		     "scanout buffer of '%s')",
		     w, h, inst->connector);
	}
}

/* The ~5 s multiview render-cost line of `inst` (report-only), read next to the Program render audit
 * `lagged`. */
static void drm_output_view_audit(struct drm_output_inst *inst, uint64_t now)
{
	struct drm_output_view_state *v = &inst->view;
	if (v->win_start_ns == 0) {
		v->win_start_ns = now;
		return;
	}
	const uint64_t elapsed = now > v->win_start_ns ? now - v->win_start_ns : 0;
	if (elapsed < MULTIVIEW_AUDIT_WINDOW_NS)
		return;
	const double win_s = (double)elapsed / 1000000000.0;
	const double avg_ms = v->win_renders ? ((double)v->win_sum_ns / (double)v->win_renders) / 1000000.0 : 0.0;
	uint32_t w, h;
	drm_output_mode_size(inst, &w, &h);
	blog(LOG_INFO,
	     "drm-output: multiview-render rendered_fps=%.1f skipped=%u avg_ms=%.2f max_ms=%.2f ewma_ms=%.2f "
	     "cx=%u cy=%u self_ns=%llu on '%s'",
	     (double)v->win_renders / win_s, v->win_skips, avg_ms, (double)v->win_max_ns / 1000000.0,
	     (double)v->ewma_ns / 1000000.0, w, h, (unsigned long long)v->last_self_ns, inst->connector);
	v->win_start_ns = now;
	v->win_renders = 0;
	v->win_skips = 0;
	v->win_sum_ns = 0;
	v->win_max_ns = 0;
}

int drm_output_view_frame(struct drm_output_inst *inst)
{
	struct drm_output_view_state *v = &inst->view;
	const int view = (int)os_atomic_load_long(&v->view);
	/* main design 5840501628: leave the view's own render out of the previous tick's total only when
	 * it ran in that tick (drm_output_view_self_last_ns), never across a skipped call. */
	const uint64_t self_last_ns =
		drm_output_view_self_last_ns(v->last_render_ns, v->last_render_frame, obs_get_total_frames());
	if (view != OBS_DRM_OUTPUT_VIEW_MULTIVIEW) {
		if (v->win_start_ns != 0)
			drm_output_view_reset_window_locked(v);
		return drm_output_view_tick_action(view, v->render != NULL, false);
	}

	const bool have_renderer = v->render != NULL;
	bool skip = false;
	if (have_renderer) {
		v->frame_counter++;
		v->last_self_ns = self_last_ns;
		skip = obs_aux_sender_should_skip_excluding(DRM_OUTPUT_MV_RENDER_DIVISOR, v->frame_counter, v->ewma_ns,
							    v->consecutive_skips, self_last_ns);
	} else if (!v->warned_no_renderer) {
		v->warned_no_renderer = true;
		blog(LOG_INFO,
		     "drm-output: view=multiview on '%s' but no Multiview renderer registered yet -- keeping the last "
		     "frame",
		     inst->connector);
	}

	const int action = drm_output_view_tick_action(view, have_renderer, skip);
	if (action == DRM_OUTPUT_TICK_MULTIVIEW) {
		drm_output_view_render_multiview(inst);
	} else if (skip) {
		v->consecutive_skips++;
		v->win_skips++;
	}
	if (have_renderer)
		drm_output_view_audit(inst, os_gettime_ns());
	return action;
}

#endif /* defined(__linux__) */
