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

/* camera-box #1084: discard the whole regression point buffer and its cumulative anchors, and drop
 * the lock. Called on any LEVEL SHIFT -- a #960 starved-window rejection or a non-positive
 * master_block_s (a backward/duplicate wall read, e.g. an NTP step) -- because a step in the
 * cumulative would corrupt the slope for a full ASRC_REGRESSION_SPAN_S as it slides through the
 * buffer; re-converging from scratch is bounded (~a minute) and level shifts are rare on this
 * source. Deliberately does NOT reset estimated_ppm/applied_ppm directly -- so applied is HELD on
 * the flushing call itself (no slew step runs that call). But because the flush DROPS the lock,
 * every subsequent call sees !reg_locked -> target 0 -> applied SLEWS back to 0 (at
 * ASRC_MAX_SLEW_PPM_PER_S) over the ~ASRC_REGRESSION_LOCK_SPAN_S re-lock window, then re-converges
 * once the buffer re-fills. Decay-to-zero-then-reconverge is default-safe (a level shift invalidates
 * the old correction) and bounded (one spurious 1 s starved window ~= a few ms of A/V step). Mirror
 * of the Rust RealtimeAsrcCompensator::regression_flush(). reg_x/reg_y need no clearing -- reg_count
 * == 0 means no live points are ever read. */
static void asrc_regression_flush(struct asrc_compensator *c)
{
	c->reg_head = 0;
	c->reg_count = 0;
	c->cum_master_s = 0.0;
	c->cum_ymm_s = 0.0;
	c->reg_locked = false;
}

void asrc_compensator_init(struct asrc_compensator *c)
{
	c->estimated_ppm = 0.0;
	c->applied_ppm = 0.0;
	c->cumulative_correction_ms = 0.0;
	c->time_since_log_s = 0.0;
	c->outer_bias_ppm = 0.0; /* camera-box #806 */
	c->starved_block_count = 0; /* camera-box #960 */
	c->window_raw_s = 0.0; /* camera-box #962 */
	c->window_master_s = 0.0; /* camera-box #962 */
	c->window_block_count = 0; /* camera-box #962 */
	asrc_regression_flush(c); /* camera-box #1084: empty buffer, 0 cumulatives, unlocked */
}

double asrc_compensator_compensate(struct asrc_compensator *c, double raw_advance_s, double master_block_s,
				    double *applied_ppm_out)
{
	if (master_block_s <= 0.0) {
		/* A non-positive block duration carries no timing information (e.g. a duplicate or
		 * backward wall-clock read -- an NTP step) and, because the regression accumulates a
		 * CUMULATIVE master time, it is also a level shift that would corrupt the slope. Flush the
		 * buffer and pass through unchanged; applied_ppm is HELD. Mirror of the Rust guard. */
		asrc_regression_flush(c);
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
	 * blocks were chunked. This WINDOWED measurement is the unchanged DATA SOURCE the camera-box
	 * #1084 regression consumes. Mirror of src/asrc_bench.rs RealtimeAsrcCompensator::compensate
	 * -- keep numerically identical. */
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
		 * sums (not this block's own instantaneous ratio); a valid window becomes ONE regression
		 * point below, exactly the shape the pre-#1084 code fed to the EMA. */
		const double window_ppm = (c->window_raw_s / c->window_master_s - 1.0) * 1000000.0;
		const double window_raw_s = c->window_raw_s;
		const double window_master_s = c->window_master_s;
		const uint32_t window_block_count = c->window_block_count;
		c->window_raw_s = 0.0;
		c->window_master_s = 0.0;
		c->window_block_count = 0;

		/* camera-box #960 (applied to the WINDOW value, not a single block's instantaneous
		 * ratio -- camera-box #962): a window whose aggregate ppm magnitude clears the sanity
		 * ceiling carries no real timing information (the source was genuinely starved/bursting
		 * for MOST of this window) -- REJECT the whole window: no regression point. camera-box
		 * #1084: a starved window is a LEVEL SHIFT (the source delivered a wrong sample count),
		 * so also FLUSH the regression buffer -- keeping the pre-starvation points would corrupt
		 * the slope for a full span. applied_ppm is HELD (window_rejected_this_call gates the slew
		 * below). Attribute every block that fed this window to starved_block_count, preserving
		 * the pre-#962 telemetry meaning ("how many audio blocks were part of an unusable
		 * measurement") at window granularity. */
		if (fabs(window_ppm) > ASRC_MAX_SANE_INSTANTANEOUS_PPM) {
			c->starved_block_count += window_block_count;
			asrc_regression_flush(c);
			window_rejected_this_call = true;
		} else {
			/* camera-box #1084: push one regression point -- (cumulative accepted-window
			 * master time, cumulative raw-minus-master) -- into the fixed-capacity ring, slide
			 * it to the last ASRC_REGRESSION_SPAN_S, and re-fit the rate slope. The Rust authority
			 * (src/asrc_bench.rs) uses a Vec that evict-before-appends + age-evicts in this
			 * identical oldest->newest order. The evict-before-append capacity guard is defensive;
			 * age eviction already bounds a >=1 s-window buffer well below ASRC_REGRESSION_CAP, so
			 * neither the guard nor a ring wrap ever fires in practice. */
			c->cum_master_s += window_master_s;
			c->cum_ymm_s += window_raw_s - window_master_s;
			/* Defensive capacity guard (mirror of the Rust): evict the oldest point BEFORE
			 * appending if the ring is already full, so the newest point never overwrites a live
			 * slot. Age eviction (below) keeps a >=1 s-window buffer at ~601 points, well under
			 * ASRC_REGRESSION_CAP, so this never fires in practice. */
			if (c->reg_count == ASRC_REGRESSION_CAP) {
				c->reg_head = (c->reg_head + 1) % ASRC_REGRESSION_CAP;
				c->reg_count--;
			}
			const uint32_t tail = (c->reg_head + c->reg_count) % ASRC_REGRESSION_CAP;
			c->reg_x[tail] = c->cum_master_s;
			c->reg_y[tail] = c->cum_ymm_s;
			c->reg_count++;
			const double cutoff = c->cum_master_s - ASRC_REGRESSION_SPAN_S;
			while (c->reg_count > 1 && c->reg_x[c->reg_head] < cutoff) {
				c->reg_head = (c->reg_head + 1) % ASRC_REGRESSION_CAP;
				c->reg_count--;
			}
			const uint32_t n = c->reg_count;
			if (n >= ASRC_REGRESSION_MIN_POINTS) {
				/* Re-anchor to the oldest point (bounded magnitudes -> no catastrophic
				 * cancellation over a long run) and recompute the five ordinary-least-squares
				 * sums in FULL, in a fixed oldest->newest iteration order -- deterministic and
				 * bit-identically matching the Rust Vec (no incremental subtract-on-evict, whose
				 * FP rounding would drift the two apart). slope = (n*Sxy - Sx*Sy) / (n*Sxx -
				 * Sx*Sx); the rate offset in ppm is slope * 1e6. */
				const double x0 = c->reg_x[c->reg_head];
				const double y0 = c->reg_y[c->reg_head];
				double sx = 0.0, sy = 0.0, sxx = 0.0, sxy = 0.0;
				for (uint32_t i = 0; i < n; i++) {
					const uint32_t idx = (c->reg_head + i) % ASRC_REGRESSION_CAP;
					const double x = c->reg_x[idx] - x0;
					const double y = c->reg_y[idx] - y0;
					sx += x;
					sy += y;
					sxx += x * x;
					sxy += x * y;
				}
				const double nf = (double)n;
				const double denom = nf * sxx - sx * sx;
				if (fabs(denom) > 1e-9) {
					const double slope = (nf * sxy - sx * sy) / denom;
					c->estimated_ppm = slope * 1000000.0;
				}
				const uint32_t newest = (c->reg_head + n - 1) % ASRC_REGRESSION_CAP;
				if (c->reg_x[newest] - c->reg_x[c->reg_head] >= ASRC_REGRESSION_LOCK_SPAN_S)
					c->reg_locked = true;
			}
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
		 * still-converging (short-baseline) slope (camera-box #806: the outer-loop bias is folded
		 * in HERE, so it is just as inert as the inner estimate before lock -- never applied on its
		 * own). Once locked, add the outer-loop bias to the slope estimate and clamp the SUM to the
		 * hard ppm bound before ever using it as a target. Mirror of src/asrc_bench.rs
		 * RealtimeAsrcCompensator::compensate. */
		const double target_ppm = (!c->reg_locked)
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
