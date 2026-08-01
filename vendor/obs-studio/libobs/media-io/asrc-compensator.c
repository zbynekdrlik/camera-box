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

	/* This block's instantaneous rate ratio, straight from the observation -- exactly the
	 * "delivered samples / wall-clock window" measurement issue #803 specifies, using the
	 * SAME master-clock basis the video FIFO release already uses (genlock_wall_now_ns() in
	 * obs-source.c). */
	const double instantaneous_ppm = (raw_advance_s / master_block_s - 1.0) * 1000000.0;

	/* TIME-based EMA smoothing factor: alpha = 1 - exp(-block/tau), so convergence speed is
	 * independent of how the caller chunks audio callbacks (a real device may deliver
	 * anywhere from a few ms to tens of ms per callback). */
	const double alpha = 1.0 - exp(-master_block_s / ASRC_TIME_CONSTANT_S);
	c->estimated_ppm = alpha * instantaneous_ppm + (1.0 - alpha) * c->estimated_ppm;

	c->elapsed_lock_s += master_block_s;

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

	/* Slew-limit the APPLIED correction toward the target -- caps how fast the resample-ratio
	 * nudge may change, independent of how fast the estimate itself moves. */
	const double max_step = ASRC_MAX_SLEW_PPM_PER_S * master_block_s;
	const double delta = asrc_clamp(target_ppm - c->applied_ppm, -max_step, max_step);
	c->applied_ppm += delta;

	const double corrected_advance_s = raw_advance_s / (1.0 + c->applied_ppm / 1000000.0);

	/* Telemetry accumulator: cumulative |raw - corrected| advance, in ms, since the last log
	 * line (issue #803: "kumulatívneho rezídua"). */
	c->cumulative_correction_ms += fabs(raw_advance_s - corrected_advance_s) * 1000.0;
	c->time_since_log_s += master_block_s;

	if (applied_ppm_out)
		*applied_ppm_out = c->applied_ppm;
	return corrected_advance_s;
}

bool asrc_compensator_should_log(struct asrc_compensator *c, double *cumulative_correction_ms_out)
{
	if (c->time_since_log_s < ASRC_LOG_INTERVAL_S)
		return false;

	if (cumulative_correction_ms_out)
		*cumulative_correction_ms_out = c->cumulative_correction_ms;

	c->time_since_log_s = 0.0;
	c->cumulative_correction_ms = 0.0;
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
