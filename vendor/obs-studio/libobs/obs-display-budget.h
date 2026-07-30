/*
 * camera-box #278/#293/#756 — pure, OBS-dependency-free skip decision for the
 * budget+cadence monitoring-display throttle in render_display() (obs-display.c).
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
 *
 * #756 (imag-nb live finding, 2026-07-15): the #278/#293 budget gate above is SOFT — it only
 * skips when the display's measured cost does not fit the remaining per-tick budget. On imag
 * the Multiview render (~6.7-10.45ms EWMA) fit comfortably under the ~15ms (90%) budget on
 * nearly every tick (elapsed-before-MV ~3.6ms + ewma ~6.7-10.45ms <= 15ms), so the adaptive
 * gate almost never fired and the Multiview rendered EVERY tick at full 60fps — render_divisor
 * was correctly set to 2 (confirmed live: `nm -D -u` on the deployed frontend has the symbol,
 * OBSProjector.cpp unconditionally calls obs_display_set_render_divisor(GetDisplay(), 2) for
 * every Multiview projector) but never actually halved the render cost, because nothing
 * enforced the halving when the display was cheap enough to always fit under budget.
 *
 * #756 GREEN adds a HARD CADENCE FLOOR, layered on top of (never replacing) the budget gate:
 * a throttleable display ALWAYS skips on a tick whose frame_counter is not a multiple of
 * render_divisor — regardless of measured cost or budget headroom — guaranteeing a genuine
 * 1/render_divisor cadence cap even for an always-cheap display. The existing budget-based
 * over-budget throttle (and its #293 anti-starvation floor) still applies on the
 * cadence-eligible ticks, so a genuinely heavy display keeps its never-freezes guarantee too.
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
 *   frame_counter       (#756) this display's own per-instance tick counter, incremented every
 *                       tick BEFORE this call (mirrors the pre-#278 `#276` per-instance
 *                       counter). Used ONLY for the hard cadence floor below; irrelevant when
 *                       render_divisor <= 1.
 *   ewma_ns             EWMA of this display's measured render cost (0 = not warmed up yet).
 *   elapsed_ns          ns already consumed on the graphics thread this tick before this
 *                       display (the program + earlier displays have already rendered).
 *   budget_ns           ns of frame budget that may be used this tick (90% of the interval).
 *   consecutive_skips   how many ticks IN A ROW this display has already been skipped (for ANY
 *                       reason — cadence-forced or budget-forced; both count towards the #293
 *                       anti-starvation cap so the never-freezes guarantee holds regardless of
 *                       skip cause).
 *
 * Guarantees:
 *   - render_divisor <= 1  -> never skip (the program is sacred, always renders).
 *   - ewma_ns == 0         -> never skip (render once to measure; never pre-starved to 0),
 *                             EVEN on a cadence-ineligible frame_counter — a display must be
 *                             measured at least once before either gate throttles it.
 *   - (#756) cadence floor -> once warmed up, ALWAYS skip when
 *                             `frame_counter % render_divisor != 0`, regardless of cost or
 *                             budget — a hard 1/render_divisor cap that the soft budget gate
 *                             alone cannot provide for an always-cheap display.
 *   - fits the budget      -> never skip (on a cadence-eligible tick, genuine slack remains).
 *   - over budget          -> skip, BUT only while it has not yet been skipped K times in a
 *                             row; the (K+1)-th over-budget tick renders (no permanent freeze).
 */
static inline bool obs_display_should_skip(uint32_t render_divisor, uint32_t frame_counter,
					   uint64_t ewma_ns, uint64_t elapsed_ns,
					   uint64_t budget_ns, uint32_t consecutive_skips)
{
	if (render_divisor < 1) /* program output + preview (divisor 0): never throttled */
		return false;
	if (ewma_ns == 0) /* not warmed up: render once to measure its cost */
		return false;

	/* #756 hard cadence floor: a throttleable, already-warmed display ALWAYS skips on a
	 * tick whose frame_counter is not a multiple of render_divisor -- regardless of
	 * measured cost or remaining budget. Without this, a display cheap enough to always
	 * fit under budget (the imag Multiview's live behavior) is never actually throttled by
	 * the soft budget gate below, defeating the render_divisor marker entirely.
	 *
	 * #776: an EFFECTIVE divisor of 1 (a 30fps canvas deriving cadence-uncapped multiview
	 * cells -- see render_display()'s canvas-rate derivation) means NO cadence skipping,
	 * but the display is STILL a monitoring display: the budget gate + #293 floor below
	 * keep applying, so the program always has priority on a tight tick. The old
	 * `<= 1 -> never throttled` early-return wrongly disabled the budget brake for that
	 * case; only divisor 0 (program/preview, never routed here by render_display()'s own
	 * `render_divisor > 1` guard anyway) is exempt. */
	if (render_divisor > 1 && (frame_counter % render_divisor) != 0)
		return true;

	if (elapsed_ns + ewma_ns <= budget_ns) /* fits the remaining budget this tick: render */
		return false;

	/* over budget: skip — but NEVER starve (#293). Skip only while it has not yet been
	 * skipped K ticks in a row; the (K+1)-th over-budget tick renders, so the heavy
	 * Multiview keeps updating at >= 1/(K+1) of the tick rate and can never freeze solid. */
	return consecutive_skips < OBS_DISPLAY_MAX_CONSECUTIVE_SKIPS;
}
