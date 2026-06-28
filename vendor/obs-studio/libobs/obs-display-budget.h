/*
 * camera-box #278/#293 — pure, OBS-dependency-free skip decision for the ADAPTIVE
 * budget-based monitoring-display throttle in render_display() (obs-display.c).
 *
 * Extracted into its own header (no libobs deps — only <stdbool.h>/<stdint.h>) so the
 * exact decision the production graphics thread uses is directly unit-testable from a
 * standalone C harness (tests/obs_display_budget.rs compiles + runs this header), instead
 * of being buried inline in render_display() where it cannot be exercised without the whole
 * OBS core.
 *
 * #278 decoupled the heavy strih Multiview projector from the 60fps program render by
 * skipping a throttleable monitoring display (render_divisor > 1) whenever its measured
 * render cost would not fit the budget remaining after the program rendered this tick.
 *
 * #293 (regression of #278): a SINGLE 4-live-cam Multiview render (~18-23ms) ALONE exceeds
 * the ~15ms budget (90% of a 60fps interval), so `elapsed + ewma > budget` was true on
 * EVERY tick -> the Multiview was skipped FOREVER -> it FROZE solid for a whole live event.
 * The decouple must THROTTLE the monitoring display, NEVER disable it (the
 * multiview-must-not-affect-program rule = decouple, not freeze; the program render,
 * render_divisor <= 1, is always left untouched).
 *
 * #293 GREEN: an over-budget monitoring display is skipped at most
 * OBS_DISPLAY_MAX_CONSECUTIVE_SKIPS ticks in a row; the next over-budget tick is FORCED to
 * render (and render_display() resets the per-display skip counter), so the Multiview throttles
 * to a reduced-but-NONZERO cadence (~15fps at a 60fps tick for K=3) instead of freezing.
 */

#pragma once

#include <stdbool.h>
#include <stdint.h>

/*
 * Liveness floor (#293): the maximum number of CONSECUTIVE ticks an over-budget monitoring
 * display may be skipped. After this many skips in a row the next tick is FORCED to render
 * (and the caller resets the per-display skip counter), so a heavy monitoring display renders
 * at >= 1/(K+1) of the tick rate (K=3 -> >= 15fps at a 60fps tick) instead of 0fps, while the
 * program render is never touched on the skipped ticks.
 */
#define OBS_DISPLAY_MAX_CONSECUTIVE_SKIPS 3u

/*
 * Returns true iff this display should be SKIPPED (not rendered) this tick.
 *
 *   render_divisor      0/1 = program output + preview (NEVER throttled); >1 = throttleable
 *                       monitoring display (the Multiview projector, set to 2).
 *   ewma_ns             EWMA of this display's measured render cost (0 = not warmed up yet).
 *   elapsed_ns          ns already consumed on the graphics thread this tick before this
 *                       display (the program + earlier displays have already rendered).
 *   budget_ns           ns of frame budget that may be used this tick (90% of the interval).
 *   consecutive_skips   how many ticks IN A ROW this display has already been skipped.
 *
 * Guarantees:
 *   - render_divisor <= 1  -> never skip (the program is sacred, always renders).
 *   - ewma_ns == 0         -> never skip (render once to measure; never pre-starved to 0).
 *   - fits the budget      -> never skip (genuine slack remains -> render).
 *   - over budget          -> skip, BUT only while it has not yet been skipped K times in a
 *                             row; the (K+1)-th over-budget tick renders (no permanent freeze).
 */
static inline bool obs_display_should_skip(uint32_t render_divisor, uint64_t ewma_ns,
					   uint64_t elapsed_ns, uint64_t budget_ns,
					   uint32_t consecutive_skips)
{
	if (render_divisor <= 1) /* program output + preview: never throttled */
		return false;
	if (ewma_ns == 0) /* not warmed up: render once to measure its cost */
		return false;
	if (elapsed_ns + ewma_ns <= budget_ns) /* fits the remaining budget this tick: render */
		return false;

	/* over budget: skip — but NEVER starve (#293). Skip only while it has not yet been
	 * skipped K ticks in a row; the (K+1)-th over-budget tick renders, so the heavy
	 * Multiview keeps updating at >= 1/(K+1) of the tick rate and can never freeze solid. */
	return consecutive_skips < OBS_DISPLAY_MAX_CONSECUTIVE_SKIPS;
}
