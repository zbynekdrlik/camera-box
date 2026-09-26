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
	/* camera-box #1335: a level shift invalidates the captured setpoint AND the integral it built
	 * up; drop both so a relock re-captures the setpoint and re-integrates from 0 (default-safe). */
	c->level_integral_ppm = 0.0;
	c->level_captured = false;
	/* camera-box #1335 follow-up 2: a flush is an UNINTENDED discontinuity that re-captures the
	 * setpoint from the post-relock depth, so any in-progress fast level-restore is abandoned (the
	 * buffer self-heals). step_count/last_step_ms are running telemetry -- never reset here. */
	c->level_restore = false;
	/* camera-box #1335 follow-up 4: a flush abandons any in-progress restore, so the sustained-error
	 * window counter resets too. */
	c->level_err_windows = 0;
	/* camera-box #1335 follow-up 5: a flush re-captures the setpoint from the post-relock depth, so
	 * the smoothed level error re-seeds from the first post-relock window. */
	c->level_err_ema_ms = 0.0;
	c->level_err_ema_seeded = false;
	/* camera-box #1355: a flush re-captures the (absolute) setpoint, so the unreachable-walk count
	 * and the one-fallback-per-capture latch restart with it. level_offset_ms / level_absolute are
	 * caller state and survive. */
	c->level_unconverged_windows = 0;
	c->level_fallback_done = false;
	/* camera-box issue 1372: a flush re-captures the setpoint, so a step still owed is abandoned with
	 * the capture it was booked against. */
	c->step_recover_ms = 0.0;
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
	c->level_target_ms = 0.0; /* camera-box #1335 */
	c->level_last_ms = 0.0; /* camera-box #1335 */
	c->step_count = 0; /* camera-box #1335 follow-up 2 */
	c->last_step_ms = 0.0; /* camera-box #1335 follow-up 2 */
	c->level_restore = false; /* camera-box #1335 follow-up 2 */
	c->level_err_windows = 0; /* camera-box #1335 follow-up 4 */
	c->level_err_ema_ms = 0.0; /* camera-box #1335 follow-up 5 */
	c->level_err_ema_seeded = false; /* camera-box #1335 follow-up 5 */
	c->level_offset_ms = 0.0; /* camera-box #1355 */
	c->level_unconverged_windows = 0; /* camera-box #1355 */
	c->level_fallback_count = 0; /* camera-box #1355 */
	c->level_fallback_from_ms = 0.0; /* camera-box #1355 */
	c->level_fallback_pending = false; /* camera-box #1355 */
	c->level_fallback_done = false; /* camera-box #1355 */
	c->level_absolute = true; /* camera-box #1355 */
	c->window_level_sum_ms = 0.0; /* camera-box #1367 */
	c->window_level_count = 0; /* camera-box #1367 */
	c->level_avg_ms = 0.0; /* camera-box #1367 */
	c->step_recover_ms = 0.0; /* camera-box issue 1372 */
	c->step_recover_ppm = 0.0; /* camera-box issue 1372 */
	asrc_regression_flush(c); /* camera-box #1084/#1335: empty buffer, 0 cumulatives, 0 integral, unlocked */
}

double asrc_compensator_compensate(struct asrc_compensator *c, double raw_advance_s, double master_block_s,
				    double buffered_ms, double *applied_ppm_out)
{
	/* camera-box issue 1372: the recovery rate is per call; only the owed-step block below sets it. */
	c->step_recover_ppm = 0.0;
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
	/* camera-box #1367: fold EVERY callback's level into the window, so the level loop reads the
	 * window MEAN rather than one reading on the mixer-tick sawtooth. Mirror of the Rust Some path. */
	c->window_level_sum_ms += buffered_ms;
	c->window_level_count++;

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
		/* camera-box #1367: the window's MEAN level (every callback's buffered_ms), reset with the
		 * other window sums. Always >= 1 reading here (the closing call itself adds); the guard
		 * mirrors the Rust rate-only entry, which never reads it. */
		const double window_level_ms =
			c->window_level_count > 0 ? c->window_level_sum_ms / (double)c->window_level_count : 0.0;
		c->window_level_sum_ms = 0.0;
		c->window_level_count = 0;

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
			/* camera-box #1335 follow-up 2: STEP DETECTION -> RE-BASE. The prospective new
			 * cumulative point (advance the anchors by this closed window), and the single-window
			 * RESIDUAL vs the locked fit's RATE (how far THIS window's own advance increment deviates
			 * from the slope's expected increment). It is a per-window quantity -- the cumulative
			 * noise cancels (pt_ymm - cum_ymm_s == this window's increment) -- so ordinary +-1-3 ms
			 * window jitter stays under ASRC_STEP_RESIDUAL_MS, while a real sample-loss/dup or
			 * wall-clock step (tens of ms in ONE window) exceeds it. On a step, RE-BASE (shift
			 * cum_ymm_s onto the pre-step fit line, do NOT insert the step point, keep the lock +
			 * slope + applied) instead of inserting it (which would bias the 600 s slope; the live
			 * 17.9. 18:52 est +16 -> -83 -> -152 swing). The Rust None (rate-only bench) path skips
			 * this; the C path ALWAYS has buffered_ms, so here it is unconditional (mirror of the
			 * Rust Some path). */
			const double pt_master = c->cum_master_s + window_master_s;
			const double pt_ymm = c->cum_ymm_s + (window_raw_s - window_master_s);
			const double r_s =
				(window_raw_s - window_master_s) - (c->estimated_ppm / 1000000.0) * window_master_s;
			bool rebased = false;
			if (c->reg_locked && fabs(r_s * 1000.0) > ASRC_STEP_RESIDUAL_MS) {
				/* RE-BASE: cum_ymm_s -= r leaves the anchor on the pre-step fit line
				 * (cum_ymm_before + slope*window_master), so future points align; keep the lock,
				 * slope, and applied (no 60 s decay). */
				c->cum_master_s = pt_master;
				c->cum_ymm_s = pt_ymm - r_s;
				c->step_count++;
				c->last_step_ms = r_s * 1000.0;
				c->level_last_ms = buffered_ms;
				/* camera-box #1367: telemetry only here; the restore corroboration below stays on
				 * the live reading of this same call. */
				c->level_avg_ms = window_level_ms;
				/* FAST bounded level restore, but ONLY if the buffer level corroborates a real
				 * sample loss/dup (|level err| >= half the residual magnitude). A wall-clock-only
				 * jump leaves buffered_ms unchanged => re-base only, no restore. */
				/* camera-box issue 1372 (ROZHODNUTE 5841039244): a CONFIRMED step is booked, not
				 * restored proportionally. The owed amount is the measured sample-count step
				 * itself; the setpoint moves with the buffer (so the level loop, the restore arms and
				 * the unreachable bound see no disturbance) and walks back as the recovery pays it
				 * at ASRC_STEP_RECOVER_PPM below -- the live 44 ms loss in ~44 s, not 3-4 min. */
				if (c->level_captured &&
				    fabs(buffered_ms - c->level_target_ms) >= 0.5 * fabs(r_s * 1000.0)) {
					const double owed_ms = -r_s * 1000.0;
					c->step_recover_ms += owed_ms;
					c->level_target_ms -= owed_ms;
				}
				rebased = true;
			}

			if (!rebased) {
				/* camera-box #1084: push one regression point -- (cumulative accepted-window
				 * master time, cumulative raw-minus-master) -- into the fixed-capacity ring, slide
				 * it to the last ASRC_REGRESSION_SPAN_S, and re-fit the rate slope. The Rust authority
				 * (src/asrc_bench.rs) uses a Vec that evict-before-appends + age-evicts in this
				 * identical oldest->newest order. The evict-before-append capacity guard is defensive;
				 * age eviction already bounds a >=1 s-window buffer well below ASRC_REGRESSION_CAP, so
				 * neither the guard nor a ring wrap ever fires in practice. */
				c->cum_master_s = pt_master;
				c->cum_ymm_s = pt_ymm;
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

				/* camera-box #1335: buffer-LEVEL holding integral, updated ONCE per closed ACCEPTED
				 * window (a rejected window took the branch above; a re-based window took the branch
				 * above; an unlocked servo skips the update). buffered_ms is the source's current
				 * mix-buffer depth (obs-source.c reads it from audio_input_buf[0].size). Mirror of
				 * src/asrc_bench.rs compensate_with_level. camera-box #1367: level= keeps the one
				 * raw reading (telemetry); every level-loop decision below (capture, EMA, I,
				 * sustained arm, unreachable bound) reads the window MEAN. The per-call restore
				 * burst/exit further down stays on the live buffered_ms. */
				c->level_last_ms = buffered_ms;
				c->level_avg_ms = window_level_ms;
				if (c->reg_locked) {
					if (!c->level_captured) {
						/* camera-box #1355: setpoint = the ABSOLUTE target plus the deliberate
						 * placement offset the buffered samples carry -- NOT whatever depth the mixer
						 * happened to have at lock (that froze a random per-launch A/V level).
						 * Re-captured (to the same absolute value) after every flush/relock; the P term
						 * plus the restore burst the sustained-error arm fires walk the buffer there. */
						c->level_target_ms = c->level_absolute ? ASRC_LEVEL_TARGET_MS + c->level_offset_ms
										       : window_level_ms;
						c->level_captured = true;
					}
					/* camera-box #1335 follow-up 5: SMOOTH the per-window level error with an EMA (tau
					 * ASRC_LEVEL_EMA_TAU_S) BEFORE the P term reads it, so the 66x stronger Kp=2.0 gain does not
					 * amplify the +/-10 ms mixer-tick phase noise (the 18.9. live test: the raw-error term could
					 * not hold the level, the mean wandered +/-10-15 ms). Seed with the first error after capture;
					 * later windows blend with alpha = window_master_s / (tau + window_master_s). A deliberate
					 * setpoint shift moves BOTH the target and the buffer by the same delta, so the error is
					 * unchanged and this EMA is left untouched there (see asrc_compensator_shift_level_target).
					 * Mirror of src/asrc_bench.rs compensate_with_level. */
					{
						const double level_err = window_level_ms - c->level_target_ms;
						if (!c->level_err_ema_seeded) {
							c->level_err_ema_ms = level_err;
							c->level_err_ema_seeded = true;
						} else {
							const double alpha = window_master_s / (ASRC_LEVEL_EMA_TAU_S + window_master_s);
							c->level_err_ema_ms += alpha * (level_err - c->level_err_ema_ms);
						}
					}
					/* Anti-windup: integrate only while the composite rate target is not clamped at
					 * the hard +/-ASRC_MAX_PPM bound AND the fast level-restore burst is not active
					 * (camera-box #1335 follow-up 2: freeze the integral during a restore so the two
					 * level correctors don't wind against each other). err_ms = target - buffered; a
					 * DEFICIT (buffer below setpoint) drives the integral MORE NEGATIVE =>
					 * more-negative applied => STRETCH => raises the buffer (sign confirmed by the
					 * #1335 live -5 ppm outer-bias test, 17.9.). window_master_s is this closed
					 * window's master duration (~1 s). */
					const double rate_target = c->estimated_ppm + c->outer_bias_ppm + c->level_integral_ppm;
					const bool saturated = rate_target <= -ASRC_MAX_PPM || rate_target >= ASRC_MAX_PPM;
					if (!saturated && !c->level_restore) {
						const double err_ms = c->level_target_ms - window_level_ms;
						c->level_integral_ppm =
							asrc_clamp(c->level_integral_ppm -
									   ASRC_LEVEL_KI_PPM_PER_MS_S * err_ms * window_master_s,
								   -ASRC_LEVEL_INTEGRAL_MAX_PPM, ASRC_LEVEL_INTEGRAL_MAX_PPM);
					}
					/* camera-box #1335 follow-up 4: arm the FAST bounded level restore on a SUSTAINED level
					 * error, whatever caused it (a StartStream input-sample loss, a mic/Dante re-plug, a mixer
					 * hiccup) -- the case the step arm (follow-up 2) and the shift arm (follow-up 3) both miss
					 * because it arrives with NO same-window residual step (the 18.9. 12:00 StartStream: level
					 * 100 -> 68 ms, steps=1 last_step_ms=-14.3 detected before the level drained, restore never
					 * armed, run A/V +15 ms). Count consecutive accepted windows >= the band; a below-band window
					 * resets; reaching the threshold arms and resets. The integral above is frozen while restoring
					 * so the two level correctors never wind against each other; the step arm and the shift arm
					 * stay as the immediate paths. */
					if (c->level_captured && !c->level_restore) {
						if (fabs(window_level_ms - c->level_target_ms) >= ASRC_LEVEL_RESTORE_ARM_ERR_MS) {
							if (++c->level_err_windows >= ASRC_LEVEL_RESTORE_ARM_WINDOWS) {
								c->level_restore = true;
								c->level_err_windows = 0;
							}
						} else {
							c->level_err_windows = 0;
						}
					}
					/* camera-box #1355: BOUND an unreachable setpoint. With an absolute target a mixer
					 * that cannot reach it (a buffer pinned by OBS's own buffering, a source whose
					 * placement does not follow the stretch) would keep the restore burst and the P
					 * term pushing forever. Count consecutive accepted windows whose SMOOTHED error
					 * stays outside the restore's exit band; at the bound FALL BACK to the SMOOTHED live
					 * depth (target + ema, not one noisy reading): stop the restore, zero the P error,
					 * count it and raise the one-shot flag obs-source.c logs LOUDLY. At most ONE fallback
					 * per capture (level_fallback_done, cleared only by a re-capture): a steady residual
					 * the P+I terms hold at >= 5 ms would otherwise re-trip the bound every 40 min and
					 * ratchet the target away. Mirror of src/asrc_bench.rs compensate_with_level. */
					if (!c->level_fallback_done && fabs(c->level_err_ema_ms) >= ASRC_LEVEL_RESTORE_ARM_MS) {
						if (++c->level_unconverged_windows >= ASRC_LEVEL_TARGET_UNREACHABLE_WINDOWS) {
							c->level_fallback_from_ms = c->level_target_ms;
							c->level_target_ms += c->level_err_ema_ms;
							c->level_fallback_done = true;
							c->level_restore = false;
							c->level_err_windows = 0;
							c->level_err_ema_ms = 0.0;
							c->level_unconverged_windows = 0;
							if (c->level_fallback_count < UINT32_MAX)
								c->level_fallback_count++;
							c->level_fallback_pending = true;
						}
					} else {
						c->level_unconverged_windows = 0;
					}
				}
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
		/* camera-box #1335: the buffer-LEVEL integral is folded in alongside the outer bias, then
		 * the SUM is clamped to the hard ppm bound (level_integral_ppm is 0 until the first lock).
		 * camera-box #1335 follow-up 2: the LEVEL P term + the fast bounded restore burst are folded
		 * in too, both driven by the LIVE buffered_ms. SIGN matches the proven #1335 integral:
		 * (buffered - target) < 0 (deficit) => negative => stretch => raises the buffer. (The main
		 * design wrote these with the opposite argument order; see the issue-1335-follow-up-2
		 * anchors-confirmed comment for the derivation.) */
		double target_ppm;
		if (!c->reg_locked) {
			target_ppm = 0.0;
		} else {
			double t = c->estimated_ppm + c->outer_bias_ppm + c->level_integral_ppm;
			if (c->level_captured) {
				const double err = buffered_ms - c->level_target_ms;
				/* camera-box #1335 follow-up 5: P term is now the NORMAL LAW of the level loop --
				 * Kp=2.0 on the SMOOTHED error (level_err_ema_ms) clamped +/-ASRC_LEVEL_KP_MAX_PPM,
				 * loop time constant ~500 s. The raw err below still drives the restore burst/exit. */
				t += asrc_clamp(ASRC_LEVEL_KP_PPM_PER_MS * c->level_err_ema_ms, -ASRC_LEVEL_KP_MAX_PPM,
					ASRC_LEVEL_KP_MAX_PPM);
				/* Fast bounded restore: a big proportional stretch/compress that refills a
				 * sample-loss step, then exits once the buffer is back within 5 ms. */
				if (c->level_restore) {
					if (fabs(err) < 5.0) {
						c->level_restore = false;
						/* camera-box #1335 follow-up 4: the restore just brought the level within band; reset the
						 * sustained-error counter alongside clearing level_restore. */
						c->level_err_windows = 0;
					} else {
						t += asrc_clamp(ASRC_LEVEL_RESTORE_K_PPM_PER_MS * err,
								-ASRC_LEVEL_RESTORE_MAX_PPM, ASRC_LEVEL_RESTORE_MAX_PPM);
					}
				}
			}
			target_ppm = asrc_clamp(t, -ASRC_MAX_PPM, ASRC_MAX_PPM);
		}

		/* Slew-limit the APPLIED correction toward the target -- caps how fast the
		 * resample-ratio nudge may change, independent of how fast the estimate itself moves. */
		const double max_step = ASRC_MAX_SLEW_PPM_PER_S * master_block_s;
		const double delta = asrc_clamp(target_ppm - c->applied_ppm, -max_step, max_step);
		c->applied_ppm += delta;

		/* camera-box issue 1372: pay a CONFIRMED step back at ASRC_STEP_RECOVER_PPM (1 ms per second
		 * of master time), on top of the servo's own applied_ppm -- its clamp and slew limit are
		 * untouched. The buffer grows (loss) or shrinks (dup) by the paid amount, so the setpoint and
		 * the open window's level sum move with it and the level loop never reads the recovery as an
		 * error. Only while the setpoint is captured: the step was booked against that capture.
		 * Mirror of src/asrc_bench.rs compensate_core. */
		if (c->level_captured && c->step_recover_ms != 0.0) {
			const double budget_ms = ASRC_STEP_RECOVER_PPM / 1000000.0 * master_block_s * 1000.0;
			const double paid_ms = asrc_clamp(c->step_recover_ms, -budget_ms, budget_ms);
			c->step_recover_ms -= paid_ms;
			c->step_recover_ppm = -paid_ms / (master_block_s * 1000.0) * 1000000.0;
			c->level_target_ms += paid_ms;
			c->window_level_sum_ms += paid_ms * (double)c->window_level_count;
		} else {
			c->step_recover_ms = 0.0;
		}
	}

	/* camera-box issue 1372: a confirmed step's recovery rides on the same resampler, so the advance
	 * the source actually produces carries it too (0 on every call without an owed step). */
	const double corrected_advance_s = raw_advance_s / (1.0 + (c->applied_ppm + c->step_recover_ppm) / 1000000.0);

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

/* camera-box #1335 follow-up: move the captured buffer-LEVEL setpoint by a deliberate audio
 * sync-offset delta so the level integral holds the NEW depth instead of refilling toward the old
 * one and cancelling the deliberate trim. No-op until the setpoint has been captured (first rate
 * lock). Mirror of src/asrc_bench.rs RealtimeAsrcCompensator::shift_level_target -- keep identical.
 *
 * camera-box #1335 follow-up 3: a shift whose |delta| >= ASRC_LEVEL_RESTORE_ARM_MS (5 ms) ALSO arms
 * the fast bounded level restore, so the level reaches the new depth in minutes with the integral
 * frozen (follow-up 2) instead of the ~1 h / hours-of-ringing the +/-3 ppm I term needs (the 18.9.
 * 12 h series). A sub-band shift arms nothing -- the gentle I+P loop absorbs it. */
void asrc_compensator_shift_level_target(struct asrc_compensator *c, double delta_ms)
{
	/* camera-box #1367: the level loop reads the per-window MEAN. The buffer moves by delta in the
	 * same callback as this shift, so the readings already folded into the open window are moved by
	 * delta too: the closing mean is then all in the new frame, and the smoothed error sees no blended
	 * half-old/half-new transient. Done whether or not the setpoint is captured yet, so a shift inside
	 * the capture window of a non-absolute source captures the new-frame mean too. Mirror of
	 * src/asrc_bench.rs RealtimeAsrcCompensator::shift_level_target. */
	c->window_level_sum_ms += delta_ms * (double)c->window_level_count;
	if (c->level_captured) {
		c->level_target_ms += delta_ms;
		c->level_last_ms += delta_ms;
		/* camera-box #1367: level_avg_ms (telemetry) follows like level_last_ms. */
		c->level_avg_ms += delta_ms;
		/* camera-box #1335 follow-up 5: the deliberate shift moves BOTH level_target_ms (+delta, this
		 * line) AND the buffer level itself (+delta, via the sync-offset re-stamp -- the 18.9. live
		 * test: level 80 -> 108 ms in the same second as a +12 ms shift), so the smoothed error
		 * (buffered - target) is UNCHANGED and level_err_ema_ms needs NO adjustment -- leaving it
		 * alone is exactly what "the smoothed error must not see a false transient" requires. (The
		 * design's Architektura wrote `level_err_ema_ms -= delta_ms` here; a standalone-rustc probe
		 * shows that INJECTS a -delta transient, spiking the P term to the +/-5 ppm/window slew cap on
		 * the very next window and FAILING the design's own follow-up-5 test (c) `|Δapplied| <= 2 ppm`;
		 * with no adjustment the swing is 0. Same class as the load-bearing follow-up-2 SIGN CORRECTION
		 * -- flagged for the main's review on the ticket.) The follow-up-3 arm below is unchanged. */
		/* camera-box #1335 follow-up 3: a deliberate setpoint shift of at least the restore's exit
		 * band arms the FAST bounded level restore, so the level reaches the new depth in minutes
		 * (integral frozen per follow-up 2) instead of the ~1 h / hours-of-ringing the +/-3 ppm I
		 * term needs (the 18.9. 12 h series). Below the band the existing I+P loop settles it. */
		if (fabs(delta_ms) >= ASRC_LEVEL_RESTORE_ARM_MS)
			c->level_restore = true;
	}
}

/* camera-box #1355: store the source's deliberate placement offset (ms) the buffered samples
 * already carry; it only decides the NEXT capture (level_target_ms = ASRC_LEVEL_TARGET_MS +
 * level_offset_ms). A captured setpoint follows a later deliberate change through
 * asrc_compensator_shift_level_target(), never through this store. Mirror of src/asrc_bench.rs
 * RealtimeAsrcCompensator::set_level_offset_ms -- keep identical. */
void asrc_compensator_set_level_offset_ms(struct asrc_compensator *c, double offset_ms)
{
	c->level_offset_ms = offset_ms;
}

/* camera-box #1355: choose the capture rule (ABSOLUTE target + offset, or the pre-#1355 depth at
 * lock). A change while captured drops the capture for the level loop only -- the rate regression,
 * its lock and the integral are kept -- so the next accepted window re-captures under the new rule.
 * Mirror of src/asrc_bench.rs RealtimeAsrcCompensator::set_level_absolute -- keep identical. */
void asrc_compensator_set_level_absolute(struct asrc_compensator *c, bool absolute)
{
	if (absolute != c->level_absolute) {
		c->level_absolute = absolute;
		c->level_captured = false;
		c->level_restore = false;
		c->level_err_windows = 0;
		c->level_err_ema_ms = 0.0;
		c->level_err_ema_seeded = false;
		c->level_unconverged_windows = 0;
		c->level_fallback_done = false;
		/* camera-box issue 1372: the owed step belongs to the dropped capture. */
		c->step_recover_ms = 0.0;
	}
}
