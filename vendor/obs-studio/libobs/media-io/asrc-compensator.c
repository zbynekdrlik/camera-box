/******************************************************************************
    camera-box #803 -- see asrc-compensator.h for the full design writeup and
    the pointer to the Rust reference implementation this is a line-by-line
    mirror of (src/asrc_bench.rs's RealtimeAsrcCompensator).

    This program is free software: you can redistribute it and/or modify
    it under the terms of the GNU General Public License as published by
    the Free Software Foundation, either version 2 of the License, or
    (at your option) any later version.
******************************************************************************/

#include <math.h>
#include "asrc-compensator.h"

static inline double asrc_clamp(double v, double lo, double hi)
{
	return v < lo ? lo : (v > hi ? hi : v);
}

void asrc_compensator_init(struct asrc_compensator *c)
{
	c->estimated_ppm = 0.0;
	c->applied_ppm = 0.0;
	c->elapsed_lock_s = 0.0;
	c->cumulative_correction_ms = 0.0;
	c->time_since_log_s = 0.0;
	c->outer_bias_ppm = 0.0; /* camera-box #806 */
	c->starved_block_count = 0; /* camera-box #960 */
	c->window_raw_s = 0.0; /* camera-box #962 */
	c->window_master_s = 0.0; /* camera-box #962 */
	c->window_block_count = 0; /* camera-box #962 */
}

double asrc_compensator_compensate(struct asrc_compensator *c, double raw_advance_s, double master_block_s,
				    double *applied_ppm_out)
{
	if (master_block_s <= 0.0) {
		/* A non-positive block duration carries no timing information (e.g. a duplicate
		 * or backward wall-clock read) -- pass through unchanged rather than divide by a
		 * non-positive number. Mirror of the Rust reference's identical guard. */
		if (applied_ppm_out)
			*applied_ppm_out = c->applied_ppm;
		return raw_advance_s;
	}

	/* camera-box #962: accumulate this block's DURATION-WEIGHTED contribution into the current
	 * measurement window -- summing first (rather than ratio-ing this one block alone) is what
	 * cancels arrival-timing jitter: a genuinely bursty-but-otherwise-healthy source (e.g.
	 * mbc's 128-sample Dante VSC blocks) delivers real samples at an uneven wall-clock cadence,
	 * but the SUM of delivered-sample-duration over the SUM of elapsed wall time still
	 * converges to the source's true clock ratio, regardless of how unevenly the underlying
	 * blocks were chunked. Mirror of src/asrc_bench.rs RealtimeAsrcCompensator::compensate --
	 * keep numerically identical. */
	c->window_raw_s += raw_advance_s;
	c->window_master_s += master_block_s;
	c->window_block_count++;

	/* camera-box #962: true only for a call that just closed a REJECTED window -- gates the
	 * target/slew block below OFF (HOLDING applied_ppm at exactly its pre-rejection value, even
	 * mid-transition toward an already-decided target) while still letting the corrected-advance
	 * computation and the UNCONDITIONAL telemetry accumulation below run every call, exactly like
	 * the pre-#962 per-block guard did (a sustained starve must never go silent in the ~60s log
	 * cadence -- see the telemetry comment further down). */
	bool window_rejected_this_call = false;

	if (c->window_master_s >= ASRC_WINDOW_S) {
		/* This window closes -- compute ONE windowed ppm value from the duration-weighted
		 * sums (not this block's own instantaneous ratio) and feed THAT into the EMA/
		 * lock-credit logic below, exactly the shape the pre-#962 code applied per-block. */
		const double window_ppm = (c->window_raw_s / c->window_master_s - 1.0) * 1000000.0;
		const double window_master_s = c->window_master_s;
		const uint32_t window_block_count = c->window_block_count;
		c->window_raw_s = 0.0;
		c->window_master_s = 0.0;
		c->window_block_count = 0;

		/* camera-box #960 (now applied to the WINDOW value, not a single block's
		 * instantaneous ratio -- camera-box #962): a window whose aggregate ppm magnitude
		 * clears the sanity ceiling carries no real timing information (the source was
		 * genuinely starved/bursting for MOST of this window, not just jittery in how it
		 * delivered otherwise-real samples) -- REJECT the whole window: no EMA update, no
		 * elapsed_lock_s credit. Attribute every block that fed this window to
		 * starved_block_count, preserving the pre-#962 telemetry meaning ("how many audio
		 * blocks were part of an unusable measurement") at window granularity. */
		if (fabs(window_ppm) > ASRC_MAX_SANE_INSTANTANEOUS_PPM) {
			c->starved_block_count += window_block_count;
			window_rejected_this_call = true;
		} else {
			/* TIME-based EMA smoothing factor over the WINDOW's real duration --
			 * mathematically identical to updating per-block with the same total elapsed
			 * time (the discretization alpha = 1 - exp(-dt/tau) is exact for a
			 * piecewise-constant driving value; see ASRC_WINDOW_S's own doc comment). */
			const double alpha = 1.0 - exp(-window_master_s / ASRC_TIME_CONSTANT_S);
			c->estimated_ppm = alpha * window_ppm + (1.0 - alpha) * c->estimated_ppm;
			c->elapsed_lock_s += window_master_s;
		}
	}

	/* camera-box #962: a REJECTED window HOLDS applied_ppm at EXACTLY its pre-rejection value --
	 * no target recompute, no slew step -- even if it was still mid-transition toward an
	 * already-decided, legitimate target from an earlier accepted window (a garbage window must
	 * not be allowed to continue advancing that transition either). Mirrors the pre-#962
	 * per-block early-return exactly, now at window granularity, while still letting the shared
	 * corrected-advance/telemetry tail below run unconditionally. */
	if (!window_rejected_this_call) {
		/* Default-safe: no lock yet -> target zero compensation, never guess from a
		 * still-converging estimate (camera-box #806: the outer-loop bias is folded in HERE, so
		 * it is just as inert as the inner estimate before lock -- never applied on its own).
		 * Once locked, add the outer-loop bias to the inner estimate and clamp the SUM to the
		 * hard ppm bound before ever using it as a target. Mirror of src/asrc_bench.rs
		 * RealtimeAsrcCompensator::compensate. */
		const double target_ppm = (c->elapsed_lock_s < ASRC_MIN_LOCK_S)
						   ? 0.0
						   : asrc_clamp(c->estimated_ppm + c->outer_bias_ppm, -ASRC_MAX_PPM,
								ASRC_MAX_PPM);

		/* Slew-limit the APPLIED correction toward the target -- caps how fast the
		 * resample-ratio nudge may change, independent of how fast the estimate itself moves. */
		const double max_step = ASRC_MAX_SLEW_PPM_PER_S * master_block_s;
		const double delta = asrc_clamp(target_ppm - c->applied_ppm, -max_step, max_step);
		c->applied_ppm += delta;
	}

	const double corrected_advance_s = raw_advance_s / (1.0 + c->applied_ppm / 1000000.0);

	/* Telemetry accumulator: cumulative |raw - corrected| advance, in ms, since the last log
	 * line (issue #803: "kumulatívneho rezídua"). camera-box #960: kept UNCONDITIONAL (runs on
	 * every call regardless of window-open/window-closed/starved-window state above) so the
	 * ~60s log cadence never goes silent during a sustained starve -- exactly the moment the
	 * new starved_blocks=N telemetry is most needed. camera-box #962: uses whatever applied_ppm
	 * is in effect (HELD bit-exact on a rejected window -- see window_rejected_this_call above),
	 * so it still reports the real correction being applied to this real audio. */
	c->cumulative_correction_ms += fabs(raw_advance_s - corrected_advance_s) * 1000.0;
	c->time_since_log_s += master_block_s;

	if (applied_ppm_out)
		*applied_ppm_out = c->applied_ppm;
	return corrected_advance_s;
}

bool asrc_compensator_should_log(struct asrc_compensator *c, double *cumulative_correction_ms_out,
				  uint32_t *starved_block_count_out)
{
	if (c->time_since_log_s < ASRC_LOG_INTERVAL_S)
		return false;

	if (cumulative_correction_ms_out)
		*cumulative_correction_ms_out = c->cumulative_correction_ms;
	if (starved_block_count_out)
		*starved_block_count_out = c->starved_block_count;

	c->time_since_log_s = 0.0;
	c->cumulative_correction_ms = 0.0;
	c->starved_block_count = 0; /* camera-box #960 */
	return true;
}

void asrc_compensator_set_outer_bias_ppm(struct asrc_compensator *c, double bias_ppm)
{
	c->outer_bias_ppm = asrc_clamp(bias_ppm, -ASRC_OUTER_BIAS_MAX_PPM, ASRC_OUTER_BIAS_MAX_PPM);
}

double asrc_compensator_get_outer_bias_ppm(const struct asrc_compensator *c)
{
	return c->outer_bias_ppm;
}
