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

#include "graphics/vec4.h"
#include "obs.h"
#include "obs-internal.h"
#include "obs-display-budget.h" /* camera-box #293: pure, testable monitoring-skip decision */

bool obs_display_init(struct obs_display *display, const struct gs_init_data *graphics_data)
{
	pthread_mutex_init_value(&display->draw_callbacks_mutex);
	pthread_mutex_init_value(&display->draw_info_mutex);

#if defined(_WIN32)
	/* Conservative test for NVIDIA flickering in multi-GPU setups */
	display->use_clear_workaround = gs_get_adapter_count() > 1 && !gs_can_adapter_fast_clear();
#else
	display->use_clear_workaround = false;
#endif

	if (graphics_data) {
		display->swap = gs_swapchain_create(graphics_data);
		if (!display->swap) {
			blog(LOG_ERROR, "obs_display_init: Failed to "
					"create swap chain");
			return false;
		}

		const uint32_t cx = graphics_data->cx;
		const uint32_t cy = graphics_data->cy;
		display->cx = cx;
		display->cy = cy;
		display->next_cx = cx;
		display->next_cy = cy;
	}

	if (pthread_mutex_init(&display->draw_callbacks_mutex, NULL) != 0) {
		blog(LOG_ERROR, "obs_display_init: Failed to create mutex");
		return false;
	}
	if (pthread_mutex_init(&display->draw_info_mutex, NULL) != 0) {
		blog(LOG_ERROR, "obs_display_init: Failed to create mutex");
		return false;
	}

	display->enabled = true;
	return true;
}

obs_display_t *obs_display_create(const struct gs_init_data *graphics_data, uint32_t background_color)
{
	struct obs_display *display = bzalloc(sizeof(struct obs_display));

	gs_enter_context(obs->video.graphics);

	display->background_color = background_color;

	if (!obs_display_init(display, graphics_data)) {
		obs_display_destroy(display);
		display = NULL;
	} else {
		pthread_mutex_lock(&obs->data.displays_mutex);
		display->prev_next = &obs->data.first_display;
		display->next = obs->data.first_display;
		obs->data.first_display = display;
		if (display->next)
			display->next->prev_next = &display->next;
		pthread_mutex_unlock(&obs->data.displays_mutex);
	}

	gs_leave_context();

	return display;
}

void obs_display_free(obs_display_t *display)
{
	pthread_mutex_destroy(&display->draw_callbacks_mutex);
	pthread_mutex_destroy(&display->draw_info_mutex);
	da_free(display->draw_callbacks);

	if (display->swap) {
		gs_swapchain_destroy(display->swap);
		display->swap = NULL;
	}
}

void obs_display_destroy(obs_display_t *display)
{
	if (display) {
		pthread_mutex_lock(&obs->data.displays_mutex);
		if (display->prev_next)
			*display->prev_next = display->next;
		if (display->next)
			display->next->prev_next = display->prev_next;
		pthread_mutex_unlock(&obs->data.displays_mutex);

		obs_enter_graphics();
		obs_display_free(display);
		obs_leave_graphics();

		bfree(display);
	}
}

void obs_display_resize(obs_display_t *display, uint32_t cx, uint32_t cy)
{
	if (!display)
		return;

	pthread_mutex_lock(&display->draw_info_mutex);

	display->next_cx = cx;
	display->next_cy = cy;

	pthread_mutex_unlock(&display->draw_info_mutex);
}

void obs_display_update_color_space(obs_display_t *display)
{
	if (!display)
		return;

	pthread_mutex_lock(&display->draw_info_mutex);

	display->update_color_space = true;

	pthread_mutex_unlock(&display->draw_info_mutex);
}

void obs_display_add_draw_callback(obs_display_t *display, void (*draw)(void *param, uint32_t cx, uint32_t cy),
				   void *param)
{
	if (!display)
		return;

	struct draw_callback data = {draw, param};

	pthread_mutex_lock(&display->draw_callbacks_mutex);
	da_push_back(display->draw_callbacks, &data);
	pthread_mutex_unlock(&display->draw_callbacks_mutex);
}

void obs_display_remove_draw_callback(obs_display_t *display, void (*draw)(void *param, uint32_t cx, uint32_t cy),
				      void *param)
{
	if (!display)
		return;

	struct draw_callback data = {draw, param};

	pthread_mutex_lock(&display->draw_callbacks_mutex);
	da_erase_item(display->draw_callbacks, &data);
	pthread_mutex_unlock(&display->draw_callbacks_mutex);
}

static inline bool render_display_begin(struct obs_display *display, uint32_t cx, uint32_t cy, bool update_color_space)
{
	struct vec4 clear_color;

	gs_load_swapchain(display->swap);

	if ((display->cx != cx) || (display->cy != cy)) {
		gs_resize(cx, cy);
		display->cx = cx;
		display->cy = cy;
	} else if (update_color_space) {
		gs_update_color_space();
	}

	const bool success = gs_is_present_ready();
	if (success) {
		gs_begin_scene();

		/*
		 * In contrast to OpenGL or Direct3D 11, Metal and Direct3D 12 require the clear color to use linear gamma
		 * as either the load command to clear the render target (Metal) or the explicit clear command seem to operate
		 * on the render target in linear space.
		 *
		 * As OpenGL is implemented via Metal on Apple Silicon Macs and "glClear" has to be emulated via an explicit
		 * render pass that returns the clear color for every fragment, the color becomes subject to automatic sRGB
		 * gamma encoding if the render target uses an sRGB color format.
		 */
#if defined(__APPLE__) && defined(__aarch64__)
		vec4_from_rgba_srgb(&clear_color, display->background_color);
#else
		if (gs_get_color_space() == GS_CS_SRGB)
			vec4_from_rgba(&clear_color, display->background_color);
		else
			vec4_from_rgba_srgb(&clear_color, display->background_color);
#endif
		clear_color.w = 1.0f;

		const bool use_clear_workaround = display->use_clear_workaround;

		uint32_t clear_flags = GS_CLEAR_DEPTH | GS_CLEAR_STENCIL;
		if (!use_clear_workaround)
			clear_flags |= GS_CLEAR_COLOR;
		gs_clear(clear_flags, &clear_color, 1.0f, 0);

		gs_enable_depth_test(false);
		/* gs_enable_blending(false); */
		gs_set_cull_mode(GS_NEITHER);

		gs_ortho(0.0f, (float)cx, 0.0f, (float)cy, -100.0f, 100.0f);
		gs_set_viewport(0, 0, cx, cy);

		if (use_clear_workaround) {
			gs_effect_t *const solid_effect = obs->video.solid_effect;
			gs_effect_set_vec4(gs_effect_get_param_by_name(solid_effect, "color"), &clear_color);
			while (gs_effect_loop(solid_effect, "Solid"))
				gs_draw_sprite(NULL, 0, cx, cy);
		}
	}

	return success;
}

static inline void render_display_end()
{
	gs_end_scene();
}

void render_display(struct obs_display *display)
{
	uint32_t cx, cy;
	bool update_color_space;

	if (!display || !display->enabled)
		return;

	/* camera-box #278: ADAPTIVE budget-based skip for a heavy monitoring display, BEFORE
	 * render_display_begin(). render_display() runs on the SINGLE graphics thread for ALL
	 * displays sequentially AFTER output_frames(); a heavy monitoring render there pushes
	 * the tick past the frame deadline → the NEXT program frame starts late → renderSkip.
	 * render_divisor>1 marks a throttleable monitoring display (the built-in Multiview
	 * projector, set to 2; 0/1 = program output + preview = NEVER throttled). #276 skipped
	 * it every-other-frame, but a SINGLE 4-live-cam multiview render (~18-23ms) alone
	 * exceeds the 16.6ms 60fps budget, so even every other frame the rendered frames overran
	 * → ~29% program renderSkip (rig-measured). So instead, render a monitoring display ONLY
	 * when its measured cost (render_ewma_ns) fits the budget REMAINING after the program has
	 * already rendered this tick: skip when elapsed-this-tick + this display's EWMA render
	 * time would exceed 90% of the frame interval. Skipping HERE (before begin) costs
	 * ~nothing — no gs_load_swapchain, no gs_clear, no gs_present — and leaves the last
	 * presented frame on screen, so there is no flicker. ewma==0 (not yet warmed) → render
	 * once to measure (never starved to 0). Result: the program renders 60fps with ~0
	 * renderSkip no matter how heavy the monitoring display is; the monitoring display
	 * self-throttles to whatever slack is left.
	 *
	 * #293 (regression of #278): a SINGLE 4-live-cam multiview render (~18-23ms) ALONE exceeds
	 * the budget every tick, so the skip fired on EVERY tick and the strih Multiview FROZE
	 * solid for a whole live event. The skip now has an anti-starvation floor
	 * (obs_display_should_skip(), obs-display-budget.h): an over-budget monitoring display is
	 * skipped at most OBS_DISPLAY_MAX_CONSECUTIVE_SKIPS ticks in a row; the (K+1)-th over-budget
	 * tick is FORCED to render (and the per-display skip counter resets after the draw, below).
	 * So the Multiview throttles to a reduced-but-NONZERO cadence (~15fps at K=3) instead of
	 * freezing — decouple, not disable (the multiview-must-not-affect-program rule).
	 *
	 * #756 (imag-nb live finding): the budget gate above is SOFT — a monitoring display cheap
	 * enough to always fit under the remaining budget (imag's Multiview: ~6.7-10.45ms EWMA
	 * under a ~15ms budget) is NEVER actually throttled by it, even though render_divisor
	 * marks it for 1/divisor cadence. render_frame_counter is a hard per-instance tick
	 * counter, incremented every tick this display is throttleable (regardless of
	 * skip/render outcome); obs_display_should_skip() uses it to ALWAYS skip a
	 * cadence-ineligible tick regardless of cost/budget, closing that gap while the
	 * existing budget-based over-budget throttle (and its #293 floor) still applies on top,
	 * on the cadence-eligible ticks. */
	if (display->render_divisor > 1) {
		display->render_frame_counter++;
		const uint64_t interval = obs->video.video_frame_interval_ns;
		const uint64_t tick_start = obs->video.graphics_frame_start_ns;
		const uint64_t ewma = display->render_ewma_ns;
		/* camera-box #776: the frontend's divisor (2, OBSProjector.cpp) is calibrated for
		 * 60fps-class canvases (60/2 = 30fps multiview cells). On a 30fps canvas the SAME
		 * constant halves the multiview to a visibly choppy 15fps while the budget gate
		 * below shows real headroom (strih: ~15ms free of the ~30ms budget; user-caught
		 * 2026-07-15). Treat the frontend's value as the throttleable-display MARKER plus
		 * an UPPER BOUND, and derive the EFFECTIVE divisor from the canvas rate targeting
		 * ~30fps cells: round(33.3ms / frame_interval), clamped to [1, frontend divisor].
		 * 60fps canvas -> 2 (imag, unchanged); 30fps canvas -> 1 (multiview renders every
		 * tick). An effective divisor of 1 stays budget-gated (obs-display-budget.h #776):
		 * the program always has priority on a tight tick, and the #293 anti-starvation
		 * floor still applies. */
		uint32_t effective_divisor = display->render_divisor;
		if (interval != 0) {
			const uint64_t target_cell_interval_ns = 33333333; /* ~30fps cells */
			uint32_t derived = (uint32_t)((target_cell_interval_ns + interval / 2) / interval);
			if (derived < 1)
				derived = 1;
			if (derived < effective_divisor)
				effective_divisor = derived;
		}
		/* camera-box #771: MV fps observability. Maintain a ~5s window counting the ACTUAL
		 * renders of this throttleable projector and emit its measured cadence so operators +
		 * drift-guard + the E2E preflight can SEE the multiview fps and alarm on a collapse
		 * (the user's binding "multiview musí byť plynulé a merané" requirement). This runs
		 * BEFORE the skip decision below, so the line still emits during a stall — exactly when
		 * the fps has collapsed and must be visible. render_audit_render_count is bumped after a
		 * real render further down; the floor here is the SAME pure obs_multiview_floor_fps()
		 * the E2E gate + drift-guard apply. */
		{
			const uint64_t audit_now = os_gettime_ns();
			if (display->render_audit_window_start_ns == 0)
				display->render_audit_window_start_ns = audit_now;
			const uint64_t audit_elapsed = audit_now - display->render_audit_window_start_ns;
			if (audit_elapsed >= MULTIVIEW_AUDIT_WINDOW_NS) {
				const double win_s = (double)audit_elapsed / 1000000000.0;
				const double rendered_fps = (double)display->render_audit_render_count / win_s;
				const double canvas_fps = (interval != 0) ? 1000000000.0 / (double)interval : 0.0;
				const double target_fps =
					(effective_divisor != 0) ? canvas_fps / (double)effective_divisor : canvas_fps;
				/* #776: floor tracks the effective TARGET (canvas/effective_divisor), not
				 * canvas/2 -- a 30fps-canvas box renders MV at divisor 1 = 30fps, so a
				 * canvas/2 floor (13) would be half the real target.
				 * #1212: the floor is area-independent (the issue-1110 4K report-only
				 * sentinel is retired) -- a 4K MV holds median 30fps, so it floors at 28
				 * like any other; the bursty-sample tolerance now lives in the gate. The
				 * cx=/cy= fields stay on the printed line below for observability. */
				const double floor_fps = obs_multiview_floor_fps(target_fps);
				blog(LOG_INFO,
				     "multiview-audit: monitor=%u divisor=%u rendered_fps=%.1f target=%.0f floor=%.1f cx=%u cy=%u",
				     display->render_audit_id, effective_divisor, rendered_fps, target_fps, floor_fps,
				     display->cx, display->cy);
				display->render_audit_window_start_ns = audit_now;
				display->render_audit_render_count = 0;
			}
		}
		if (ewma != 0 && interval != 0 && tick_start != 0) {
			const uint64_t now = os_gettime_ns();
			const uint64_t elapsed = (now > tick_start) ? (now - tick_start) : 0;
			const uint64_t budget = interval - interval / 10; /* 90% safety margin */
			if (obs_display_should_skip(effective_divisor, display->render_frame_counter,
						    ewma, elapsed, budget, display->render_consecutive_skips)) {
				display->render_consecutive_skips++;
				return;
			}
		}
	}

	/* -------------------------------------------- */

	pthread_mutex_lock(&display->draw_info_mutex);

	cx = display->next_cx;
	cy = display->next_cy;
	update_color_space = display->update_color_space;

	display->update_color_space = false;

	pthread_mutex_unlock(&display->draw_info_mutex);

	/* -------------------------------------------- */

	/* camera-box #278: time the actual draw of a monitoring display so the budget gate
	 * above can predict the next frame's cost. 0 for program/preview (divisor 0/1) — that
	 * path stays untouched. */
	const uint64_t render_begin_ns = (display->render_divisor > 1) ? os_gettime_ns() : 0;

	if (render_display_begin(display, cx, cy, update_color_space)) {
		GS_DEBUG_MARKER_BEGIN(GS_DEBUG_COLOR_DISPLAY, "obs_display");

		pthread_mutex_lock(&display->draw_callbacks_mutex);

		for (size_t i = 0; i < display->draw_callbacks.num; i++) {
			struct draw_callback *callback;
			callback = display->draw_callbacks.array + i;

			callback->draw(callback->param, cx, cy);
		}

		pthread_mutex_unlock(&display->draw_callbacks_mutex);

		render_display_end();

		GS_DEBUG_MARKER_END();

		/* camera-box #1107: arm this display's present-vsync mode immediately before the
		 * swap. Re-armed every tick per display on the single graphics thread (no race) →
		 * the device-level flag is per-display-correct even across swapchain recreation. */
		gs_present_vsync(display->vsync);
		gs_present();

		/* camera-box #278: update this monitoring display's render-cost EWMA (α=1/4) from
		 * the draw we just did — only after a real render (begin returned true), only for a
		 * monitoring display (divisor>1). prev==0 (cold) seeds with the first measurement. */
		if (display->render_divisor > 1) {
			const uint64_t dur = os_gettime_ns() - render_begin_ns;
			const uint64_t prev = display->render_ewma_ns;
			display->render_ewma_ns = prev ? (prev * 3 + dur) / 4 : dur;
			/* #293: a real render clears the skip run so the anti-starvation floor
			 * counts only CONSECUTIVE skips. */
			display->render_consecutive_skips = 0;
			/* camera-box #771: count this real render toward the projector's audit window
			 * (the multiview-audit line above divides this by the window seconds). */
			display->render_audit_render_count++;
		}
	}
}

void obs_display_set_enabled(obs_display_t *display, bool enable)
{
	if (display)
		display->enabled = enable;
}

bool obs_display_enabled(obs_display_t *display)
{
	return display ? display->enabled : false;
}

void obs_display_set_background_color(obs_display_t *display, uint32_t color)
{
	if (display)
		display->background_color = color;
}

/* camera-box #278: mark a display as a THROTTLEABLE monitoring surface. divisor <= 1
 * renders every frame (default — program output + preview); divisor > 1 enables the
 * adaptive budget-based skip in render_display() (the value itself is just the
 * >1 marker now — the #278 gate is driven by the display's measured EWMA render cost vs
 * the remaining frame budget, not a fixed every-Nth cadence). Used by the frontend to cap
 * the heavy built-in Multiview projector (set to 2) so monitoring never steals the
 * program-output render budget at 60fps. Unguarded single write (set once at display
 * create from the Qt thread), mirroring obs_display_set_background_color; the graphics
 * thread reads it. */
void obs_display_set_render_divisor(obs_display_t *display, uint32_t divisor)
{
	if (display) {
		display->render_divisor = divisor;
		/* camera-box #771: assign a stable per-projector audit id the first time this
		 * display becomes a throttleable monitoring surface, so its multiview-audit line
		 * carries a stable monitor=N across the run. A SHARED monotonic counter (the
		 * OPPOSITE of the per-instance cadence counters — an audit id MUST be distinct per
		 * projector), set-once from the Qt create thread exactly like render_divisor.
		 * CONCURRENCY (review): `++next_audit_id` is a shared read-modify-write, NOT
		 * atomic. This is safe ONLY because projectors are created SERIALLY on the single
		 * Qt UI thread (the same single-writer assumption render_divisor's own store relies
		 * on). If OBS ever created displays from multiple threads concurrently, two could
		 * race to the same id and log a duplicate monitor=N — it would never corrupt memory
		 * or affect the program render, only make two audit lines ambiguous. */
		if (divisor > 1 && display->render_audit_id == 0) {
			static uint32_t next_audit_id;
			display->render_audit_id = ++next_audit_id;
		}
	}
}

/* camera-box #1107: mark this display's present as vsync'd (tear-free, eglSwapInterval 1 on the
 * EGL winsys) or not. Set once from the Qt create thread (OBSProjector, for the fullscreen
 * program projector only), read + armed on the graphics thread — same single-writer discipline
 * as obs_display_set_render_divisor above. */
void obs_display_set_vsync(obs_display_t *display, bool vsync)
{
	if (!display)
		return;

	/* camera-box #1146: one-shot-on-change observability of the #1107 present-vsync
	 * decision. The #1107 EGL present logs only on eglSwapInterval FAILURE, never the
	 * armed state, so nothing (operator, drift-guard, the E2E [0/8] preflight) could
	 * confirm from the OBS log whether the fullscreen program projector actually
	 * presents tear-free. Log here - the single source of truth for the per-display
	 * decision (called ONLY from OBSProjector.cpp) - but ONLY when the flag actually
	 * changes: the program projector emits exactly one `ARMED` line at open, the
	 * multiview (flag stays at its false default) emits nothing, and the hot per-tick
	 * gs_present_vsync() arm in render_display() is untouched (no per-frame spam). */
	const bool changed = display->vsync != vsync;
	display->vsync = vsync;
	if (changed)
		blog(LOG_INFO,
		     "projector-vsync: present-vsync %s (GL/EGL swap interval %d; no-op on D3D11)",
		     vsync ? "ARMED" : "cleared", vsync ? 1 : 0);
}

void obs_display_size(obs_display_t *display, uint32_t *width, uint32_t *height)
{
	*width = 0;
	*height = 0;

	if (display) {
		pthread_mutex_lock(&display->draw_info_mutex);

		*width = display->cx;
		*height = display->cy;

		pthread_mutex_unlock(&display->draw_info_mutex);
	}
}
