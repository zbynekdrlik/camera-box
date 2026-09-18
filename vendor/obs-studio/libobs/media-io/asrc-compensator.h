/******************************************************************************
    camera-box #803 (epic #800 A/V-desync endgame round)

    Per-source ASRC (async sample-rate conversion) servo: continuously holds a
    source's audio timeline on the video master clock, killing the unbounded
    linear drift a foreign audio clock domain (Waves/Dante program audio, at
    events) otherwise accumulates against the DanteSync-disciplined video
    master clock (OBS timestamps audio by sample COUNT, so a ppm crystal
    offset never shows up in any buffer/timing_adjust metric -- see
    obs-source.c's genlock_wall_now_ns() doc comment for the shared clock
    basis this servo measures against).

    This is a LINE-BY-LINE C mirror of the Rust reference implementation
    `RealtimeAsrcCompensator` in camera-box's own crate root
    (src/asrc_bench.rs), which is unit-tested (Tier-0, default features) and
    validated against the issue-804 bench harness's 4h/50ppm/40ms gate before
    this C port was written -- see that file's own module doc comment and
    issue #803's design comment (`gh issue view 803 --comments`) for why the
    algorithm looks the way it does. Keep the two in sync: any constant or
    logic change here should be mirrored back into the Rust reference (and
    re-validated there) rather than drifting apart.

    This program is free software: you can redistribute it and/or modify
    it under the terms of the GNU General Public License as published by
    the Free Software Foundation, either version 2 of the License, or
    (at your option) any later version.
******************************************************************************/

#pragma once

#include "../util/c99defs.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Hard bound the servo clamps applied compensation to, in parts-per-million --
 * an order of magnitude above any measured worst case (epic #800: ~25-50 ppm
 * live), so it only ever engages as a safety backstop against a bad
 * measurement, never in ordinary operation. Mirror of src/asrc_bench.rs
 * MAX_PPM. */
#define ASRC_MAX_PPM 300.0

/* Hard bound on how fast the APPLIED compensation may change, in ppm per
 * second of master-clock time -- keeps the resample-ratio nudge inaudible
 * (issue #803: "nepočuteľné, žiadne kliky") even if the estimator's target
 * jumps abruptly. Mirror of src/asrc_bench.rs MAX_SLEW_PPM_PER_S. */
#define ASRC_MAX_SLEW_PPM_PER_S 5.0

/* camera-box #1084: span cap of the sliding least-squares RATE regression, in
 * seconds of master-clock time. The pre-#1084 estimator was a fixed-gain
 * time-EMA (20 s) over 1 s windows; live on the `mbc` source its estimated sd
 * was 178 ppm (-> applied sd 20-28 ppm ~= +-75-103 ms/h of global A/V wander)
 * because the 1 s window master time TELESCOPES to two wall reads and the
 * audio-thread scheduling jitter in those endpoints does NOT average down with
 * more callbacks per window (see issue #1084's design comment). A regression
 * over the cumulative (master-time, audio-minus-master) points uses that
 * endpoint noise near-optimally and is robust to BOTH the white and the
 * anti-correlated MA(1) window-noise colors an EMA retune could only cover one
 * of. Mirror of src/asrc_bench.rs REGRESSION_SPAN_S -- keep numerically
 * identical. */
#define ASRC_REGRESSION_SPAN_S 600.0

/* camera-box #1084: minimum number of accepted-window points before the
 * regression computes a slope at all (a fit through fewer points is dominated
 * by the endpoint noise). Mirror of src/asrc_bench.rs REGRESSION_MIN_POINTS. */
#define ASRC_REGRESSION_MIN_POINTS 30

/* camera-box #1084: minimum buffer SPAN (seconds of master-clock time between
 * the oldest and newest point) before ANY compensation is applied -- the
 * "default-safe: zero compensation when the servo has no lock" guarantee
 * (replaces the pre-#1084 5 s MIN_LOCK_S, which was calibrated to the EMA's
 * fast convergence; the noise-limited regression needs a longer baseline before
 * its slope is trustworthy). Mirror of src/asrc_bench.rs
 * REGRESSION_LOCK_SPAN_S. */
#define ASRC_REGRESSION_LOCK_SPAN_S 60.0

/* camera-box #1084: capacity of the point ring buffer. A 600 s span of windows
 * that each close at >=1 s of master time holds <= ~601 points; 640 leaves
 * headroom so age-based eviction, never a capacity overflow, bounds the buffer.
 * Mirror of src/asrc_bench.rs REGRESSION_CAP. */
#define ASRC_REGRESSION_CAP 640

/* Hard bound on the OUTER-loop (camera-box #806) bias this servo will accept,
 * in ppm -- the ticket's own "max +/-10 ppm uprava od inner-loop odhadu"
 * safety rail. Applied at the setter below regardless of what the caller
 * (the obs-websocket SetAsrcOuterBiasPpm request) already clamped -- never
 * trust a single caller alone. Mirror of src/asrc_bench.rs
 * OUTER_BIAS_MAX_PPM. */
#define ASRC_OUTER_BIAS_MAX_PPM 10.0

/* How often (seconds of master-clock time) the servo emits its telemetry log
 * line (issue #803: "každých 60 s log odhadnutého ppm + aplikovanej
 * kompenzácie + kumulatívneho rezídua"). Not present in the Rust reference
 * (which has no logging concern) -- a C-side-only constant. */
#define ASRC_LOG_INTERVAL_S 60.0

/* issue #960: sanity ceiling on the (issue #962: WINDOWED, duration-weighted-summed) measured
 * ppm -- above this, the measurement carries no real timing information (a starved or bursting
 * audio source, e.g. a muted/idle device path delivering near-zero samples) and must be REJECTED
 * rather than folded into the EMA. Live incident: a starved source (~26.24% of the samples its
 * elapsed wall-clock window implies) produced a measured ppm of ~-737,600, and with no gate the
 * EMA converged toward it and the servo railed at -ASRC_MAX_PPM permanently. 100,000 ppm (10%)
 * clears three boundaries with margin: two orders of magnitude above ASRC_MAX_PPM (itself already
 * an order of magnitude above any measured worst case), a clean 2x above the largest synthetic
 * stress value the Rust reference's own test suite feeds to exercise the hard-clamp/slew-limit
 * logic (50,000 ppm), and more than 7x below the observed live defect (737,600 ppm). Mirror of
 * src/asrc_bench.rs MAX_SANE_INSTANTANEOUS_PPM -- keep numerically identical. */
#define ASRC_MAX_SANE_INSTANTANEOUS_PPM 100000.0

/* issue #962: duration of the measurement WINDOW, in seconds of master-clock time, over which
 * raw_advance_s and master_block_s are duration-weighted SUMMED before computing a single
 * windowed ppm value to feed the EMA (and to gate against ASRC_MAX_SANE_INSTANTANEOUS_PPM) --
 * fixes per-block instantaneous ppm being unmeasurable noise for small, bursty-delivery blocks
 * (e.g. mbc's 128-sample Dante VSC blocks, 2.667ms each, 100% starved-rejected under the pre-#962
 * per-block guard). 1.0s spans ~375 of those blocks -- ample duration-weighted averaging for
 * arrival-timing jitter to cancel -- and each closed window becomes ONE point fed to the issue
 * #1084 regression (was: the pre-#1084 EMA). Mirror of src/asrc_bench.rs WINDOW_S -- keep
 * numerically identical. */
#define ASRC_WINDOW_S 1.0

/* camera-box #1335: integral gain of the buffer-LEVEL holding term, in ppm per (ms of level error x
 * second of closed-window master time). The #1084 regression is a pure RATE loop -- it never reads
 * the mix-buffer LEVEL, so any residual it cannot remove (live: the 600 s window lagging a +/-1 ppm
 * wandering true rate => ~0.8 ppm mean error) INTEGRATES into the buffer and drifts it ~3 ms/h until
 * an underrun. This slow integral, driven by buffered_ms, nulls exactly that residual. 0.0002 => a
 * 30 ms level error moves the correction 0.36 ppm/min; the +/-3 ms level noise floor moves it
 * +/-0.04 ppm/min (below the #1016 quantization resolution) so it never fights the fast rate loop.
 * Mirror of src/asrc_bench.rs LEVEL_KI_PPM_PER_MS_S -- keep numerically identical. */
#define ASRC_LEVEL_KI_PPM_PER_MS_S 0.0002

/* camera-box #1335: hard clamp on the buffer-LEVEL integral, in ppm (+/-). Bounds the level term far
 * below the rate loop's own ASRC_MAX_PPM so a stuck/misreported buffer level can never rail the
 * servo; the live residual it corrects is ~0.8 ppm, well inside +/-3. Anti-windup pairs with this
 * clamp: the integral is not advanced while the composite rate target is saturated at +/-ASRC_MAX_PPM.
 * Mirror of src/asrc_bench.rs LEVEL_INTEGRAL_MAX_PPM -- keep numerically identical. */
#define ASRC_LEVEL_INTEGRAL_MAX_PPM 3.0

/* camera-box #1335 follow-up 2: residual threshold, in ms, above which a newly-closed window is a
 * STEP (a permanent input sample-loss/dup or a wall-clock jump) rather than a real rate point. Live
 * 17.9. 18:52: an OBS StartStream stall lost ~50 ms of mbc input samples permanently (buffered_ms 108
 * -> 51, starved_blocks=0); that 50 ms step entered the 600 s regression and biased the slope by ~=
 * step/span = 50 ms / 600 s = 83 ppm (est +16 -> -83 -> -152 after a 2nd step). When the single-window
 * residual (this window's own advance increment minus the locked slope's expected increment) exceeds
 * this, the servo RE-BASEs (shifts cum_ymm_s onto the pre-step fit, keeps the lock+applied, does NOT
 * insert the step point) instead of flushing. 10 ms sits above the 1 s-window residual noise floor
 * (ASIO callback jitter ~1-2 ms) so ordinary noise never re-bases. Mirror of src/asrc_bench.rs
 * STEP_RESIDUAL_MS -- keep numerically identical. */
#define ASRC_STEP_RESIDUAL_MS 10.0

/* camera-box #1335 follow-up 2: proportional gain of the FAST bounded level-RESTORE burst, in ppm per
 * ms of level error. Entered only when a re-base's step is corroborated by the buffer level (a real
 * sample loss/dup, not a wall-clock-only jump); adds clamp(Kr*(buffered - target), +/-100) to the
 * correction target so a big deficit drives a strong stretch that decays as the buffer refills; exits
 * at |buffered - target| < 5 ms. SIGN follows the proven #1335 integral (deficit => negative =>
 * stretch => raises the buffer). Mirror of src/asrc_bench.rs LEVEL_RESTORE_K_PPM_PER_MS -- keep
 * numerically identical. */
#define ASRC_LEVEL_RESTORE_K_PPM_PER_MS 2.0

/* camera-box #1335 follow-up 2: hard clamp on the fast level-RESTORE burst, in ppm (+/-). 100 ppm is
 * large yet a 0.17-cent (inaudible) pitch nudge; bounds the restore far below the rate loop's own
 * ASRC_MAX_PPM. Mirror of src/asrc_bench.rs LEVEL_RESTORE_MAX_PPM -- keep numerically identical. */
#define ASRC_LEVEL_RESTORE_MAX_PPM 100.0

/* camera-box #1335 follow-up 3: arm band for the FAST level RESTORE when a DELIBERATE setpoint shift
 * (asrc_compensator_shift_level_target) moves level_target_ms, in ms. A shift whose |delta| is at
 * least this arms the restore burst so a deliberate audio sync-offset trim settles in minutes with
 * the integral frozen (follow-up 2), instead of the ~1 h the +/-3 ppm I term needs (the 18.9. 12 h
 * series: a 12 ms shift railed the integral for ~1 h and rang for hours). Set to the restore's own
 * exit band (|buffered - target| < 5 ms) -- arming below it would exit on the first tick. Mirror of
 * src/asrc_bench.rs LEVEL_RESTORE_ARM_MS -- keep numerically identical. */
#define ASRC_LEVEL_RESTORE_ARM_MS 5.0

/* camera-box #1335 follow-up 4: sustained-level-error band, in ms, that arms the FAST bounded level
 * RESTORE from the ACCEPTED-window branch, whatever caused the error. A level disturbance that
 * arrives with NO same-window residual step (an OBS StartStream input-sample loss, a mic/Dante
 * re-plug, a mixer hiccup) is invisible to the step arm (follow-up 2) and the shift arm (follow-up 3),
 * so the +/-3 ppm I term alone would take hours; this arm catches it. 12 ms sits well above the
 * +/-8 ms 1-s level scatter so ordinary noise never arms, yet below the ~14-32 ms StartStream drops
 * the 18.9. live incident produced (level 100 -> 68 ms). Mirror of src/asrc_bench.rs
 * LEVEL_RESTORE_ARM_ERR_MS -- keep numerically identical. */
#define ASRC_LEVEL_RESTORE_ARM_ERR_MS 12.0

/* camera-box #1335 follow-up 4: number of CONSECUTIVE accepted windows whose |level - target| is at
 * least ASRC_LEVEL_RESTORE_ARM_ERR_MS required before the sustained-error arm fires (10 windows =
 * 10 s at ASRC_WINDOW_S 1.0). A below-band window resets the count, so a false arm needs 10 consecutive
 * windows each >= 12 ms in MAGNITUDE (either sign -- the arm is on |level - target|, not a direction),
 * which the +/-8 ms scatter cannot produce (8 < 12); the 10 s detection delay plus a
 * bounded burst is the trade-off, and the burst only brings the level back within 5 ms of the
 * setpoint (harmless). Mirror of src/asrc_bench.rs LEVEL_RESTORE_ARM_WINDOWS -- keep numerically
 * identical. */
#define ASRC_LEVEL_RESTORE_ARM_WINDOWS 10

/* camera-box #1335 follow-up 5: proportional gain of the buffer-LEVEL P term, in ppm per ms of level
 * error, now the NORMAL LAW of the level loop -- folded into the correction target every call (once
 * locked) as clamp(Kp*level_err_ema_ms, +/-ASRC_LEVEL_KP_MAX_PPM), driven by the SMOOTHED error
 * (level_err_ema_ms, an EMA with tau ASRC_LEVEL_EMA_TAU_S) rather than the raw per-window level.
 * SIGN matches the proven #1335 integral (deficit => negative => stretch). At Kp=2.0 the loop time
 * constant is ~= 1/(Kp*1e-3) = 500 s: a 15 ms error is within ~3 ms in ~13 min (measured ~777 s),
 * at inaudible rates (a 25 ms error saturates at ASRC_LEVEL_KP_MAX_PPM = 50 ppm = 3 ms/min).
 * The 18.9. live test showed the raw-error 0.03/+/-1 term (follow-ups 2-4) could not hold the level:
 * the mean level wandered +/-10-15 ms around the setpoint over hour-scale spans and the E2E A/V
 * reading inherited it (-0.6 ms vs +15.0 ms 40 min apart, identical pins). The 66x stronger gain is
 * usable only BECAUSE the error is smoothed first: a raw 2 ppm/ms on the +/-10 ms mixer-tick phase
 * noise would jitter the rate by +/-20 ppm/s; the EMA attenuates that below the +/-50 clamp's own
 * resolution. The integral (Ki, +/-3) is kept for the DC residual only; the restore paths
 * (follow-ups 2-4) become rare backstops. Mirror of src/asrc_bench.rs LEVEL_KP_PPM_PER_MS -- keep
 * numerically identical. */
#define ASRC_LEVEL_KP_PPM_PER_MS 2.0

/* camera-box #1335 follow-up 5: hard clamp on the buffer-LEVEL P term, in ppm (+/-). Replaces the
 * follow-ups 2-4 literal +/-1.0 clamp. A 25 ms error saturates it (Kp*25 = 50 ppm = 3 ms/min = a
 * 0.005 % pitch offset while a large error decays, well inside the +-300 ppm ASRC envelope and the
 * 100 ppm bursts follow-up 2 already accepts). Bounds the P term far below the rate loop's own
 * ASRC_MAX_PPM. Mirror of src/asrc_bench.rs LEVEL_KP_MAX_PPM -- keep numerically identical. */
#define ASRC_LEVEL_KP_MAX_PPM 50.0

/* camera-box #1335 follow-up 5: time constant, in seconds of master-clock time, of the EMA that
 * smooths the per-window level error before the P term (ASRC_LEVEL_KP_PPM_PER_MS) reads it. The
 * per-window level carries +/-10 ms mixer-tick phase noise (18.9. live: consecutive 1-min samples
 * 75.2 / 95.3 / 85.1 / 96.2 around a ~86 mean); a 10 s EMA kills that noise while adding only ~10 s
 * of lag, irrelevant at the loop's 500 s time constant. alpha = window_master_s /
 * (ASRC_LEVEL_EMA_TAU_S + window_master_s) per accepted window (~1 s window => alpha ~= 0.091).
 * Mirror of src/asrc_bench.rs LEVEL_EMA_TAU_S -- keep numerically identical. */
#define ASRC_LEVEL_EMA_TAU_S 10.0

/* Per-source servo state. One instance lives per obs_source_t (see
 * obs-internal.h's `struct asrc_compensator asrc` field) and is mutated only
 * from the audio-ingest call path (process_audio(), always invoked from the
 * same source's own audio thread) -- single-writer, no lock needed for the
 * struct itself; obs_source_set_asrc_enabled() only flips the separate
 * `asrc_enabled` bool, following the same convention as genlock_burn. */
struct asrc_compensator {
	/* Running rate estimate of the source's true offset from master, in ppm -- camera-box #1084:
	 * the least-squares slope of the point buffer times 1e6 (was the EMA estimate pre-#1084). */
	double estimated_ppm;
	/* The correction actually being applied right now (post-clamp, post-slew), in ppm. */
	double applied_ppm;
	/* Cumulative |raw - corrected| advance, in ms, since the last telemetry log line --
	 * the "kumulatívneho rezídua" the issue asks for. Reset each time a log line fires. */
	double cumulative_correction_ms;
	/* Master-clock time since the last telemetry log line -- gates ASRC_LOG_INTERVAL_S. */
	double time_since_log_s;
	/* camera-box #806: the OUTER-loop bias, in ppm -- folded additively into estimated_ppm
	 * before the ASRC_MAX_PPM clamp inside asrc_compensator_compensate(). Zero (no-op) until
	 * asrc_compensator_set_outer_bias_ppm() is called; every pre-#806 caller sees identical
	 * behavior. Mirror of src/asrc_bench.rs RealtimeAsrcCompensator::outer_bias_ppm. */
	double outer_bias_ppm;
	/* camera-box #960: cumulative count of blocks REJECTED as starved/bursting since the last
	 * telemetry read -- reset to 0 whenever asrc_compensator_should_log() returns true (same
	 * reset-on-read convention as cumulative_correction_ms above). Mirror of src/asrc_bench.rs
	 * RealtimeAsrcCompensator::starved_block_count (which never resets -- a C-only logging-
	 * cadence concern, same as ASRC_LOG_INTERVAL_S itself). camera-box #962: on a rejected WINDOW
	 * close, incremented by window_block_count (every block that fed the rejected window), not
	 * just by one. */
	uint32_t starved_block_count;
	/* camera-box #962: duration-weighted sum of raw_advance_s observed in the CURRENT (not yet
	 * closed) measurement window. Mirror of src/asrc_bench.rs
	 * RealtimeAsrcCompensator::window_raw_s. */
	double window_raw_s;
	/* camera-box #962: duration-weighted sum of master_block_s observed in the CURRENT window --
	 * once this reaches ASRC_WINDOW_S, the window closes: a single windowed ppm is computed from
	 * window_raw_s/window_master_s, pushed to the #1084 regression (or rejected under the #960
	 * ceiling), and both sums reset to 0.0 for the next window. Mirror of src/asrc_bench.rs
	 * RealtimeAsrcCompensator::window_master_s. */
	double window_master_s;
	/* camera-box #962: count of individual audio blocks folded into the CURRENT (not yet closed)
	 * window -- reset to 0 alongside the sums above whenever the window closes. Mirror of
	 * src/asrc_bench.rs RealtimeAsrcCompensator::window_block_count. */
	uint32_t window_block_count;
	/* camera-box #1084: the regression point buffer, a fixed-capacity ring (ASRC_REGRESSION_CAP) --
	 * reg_x[] is cumulative ACCEPTED-window master time, reg_y[] the cumulative (raw - master) at
	 * each accepted window close, iterated oldest->newest from reg_head for reg_count entries. The
	 * Rust authority (src/asrc_bench.rs) uses a Vec that pushes+age-evicts in the identical order,
	 * so both feed the LS sums the SAME point sequence in the SAME iteration order (the numerical
	 * contract -- memory layout need not match). */
	double reg_x[ASRC_REGRESSION_CAP];
	double reg_y[ASRC_REGRESSION_CAP];
	/* camera-box #1084: ring head (index of the oldest live point) and live point count. */
	uint32_t reg_head;
	uint32_t reg_count;
	/* camera-box #1084: running cumulative accepted-window master time (the newest reg_x value)
	 * and cumulative (raw - master) (the newest reg_y value). */
	double cum_master_s;
	double cum_ymm_s;
	/* camera-box #1084: whether the buffer span has reached ASRC_REGRESSION_LOCK_SPAN_S and the
	 * servo may apply compensation (replaces the pre-#1084 elapsed-lock gate). Cleared by a flush. */
	bool reg_locked;
	/* camera-box #1335: the buffer-LEVEL setpoint, in ms -- captured the FIRST time the rate
	 * regression locks (the depth the mixer had settled at), re-captured after every flush/relock
	 * (via level_captured). The level integral drives buffered_ms back toward this. Mirror of
	 * src/asrc_bench.rs RealtimeAsrcCompensator::level_target_ms. */
	double level_target_ms;
	/* camera-box #1335: the integral of the level error, in ppm, folded ADDITIVELY into the
	 * correction target INSIDE the servo loop (alongside estimated_ppm + outer_bias_ppm), clamped to
	 * +/-ASRC_LEVEL_INTEGRAL_MAX_PPM. Reset to 0 on a flush/relock. Mirror of src/asrc_bench.rs
	 * RealtimeAsrcCompensator::level_integral_ppm. */
	double level_integral_ppm;
	/* camera-box #1335: the most recent buffered_ms observed at an accepted window close -- telemetry
	 * only (obs-source.c prints it as the asrc: line's level= field). Mirror of src/asrc_bench.rs
	 * RealtimeAsrcCompensator::level_last_ms. */
	double level_last_ms;
	/* camera-box #1335: whether level_target_ms has been captured since the last (re)lock -- gates
	 * the one-shot setpoint capture. Cleared by a flush so a relock re-captures. Mirror of
	 * src/asrc_bench.rs RealtimeAsrcCompensator::level_captured. */
	bool level_captured;
	/* camera-box #1335 follow-up 2: cumulative count of STEP re-base events (a closed window whose
	 * single-window residual exceeded ASRC_STEP_RESIDUAL_MS, re-based instead of inserted) -- printed
	 * as the asrc: line's steps= field. Running total, never reset. Mirror of src/asrc_bench.rs
	 * RealtimeAsrcCompensator::step_count. */
	uint32_t step_count;
	/* camera-box #1335 follow-up 2: the residual (ms) of the most recent re-base -- telemetry only
	 * (last_step_ms=). Sign preserved. Mirror of src/asrc_bench.rs
	 * RealtimeAsrcCompensator::last_step_ms. */
	double last_step_ms;
	/* camera-box #1335 follow-up 2: whether the FAST bounded level-restore burst is currently active
	 * -- entered on a level-corroborated re-base, exited at |buffered - target| < 5 ms. Cleared by a
	 * flush. Printed as restore=0|1. Mirror of src/asrc_bench.rs
	 * RealtimeAsrcCompensator::level_restore. */
	bool level_restore;
	/* camera-box #1335 follow-up 4: count of CONSECUTIVE accepted windows whose |level - target| is
	 * at least ASRC_LEVEL_RESTORE_ARM_ERR_MS -- reaching ASRC_LEVEL_RESTORE_ARM_WINDOWS arms the FAST
	 * level restore from a SUSTAINED level error (a disturbance with no same-window residual step, the
	 * case the step arm and the shift arm both miss). Reset on a below-band window, on arm, and next
	 * to every level_restore reset (flush/init/restore-exit). Mirror of src/asrc_bench.rs
	 * RealtimeAsrcCompensator::level_err_windows. */
	uint32_t level_err_windows;
	/* camera-box #1335 follow-up 5: the EMA (tau ASRC_LEVEL_EMA_TAU_S) of the per-window level error
	 * (buffered_ms - level_target_ms), in ms -- the SMOOTHED error the P term reads so the 66x
	 * stronger Kp=2.0 gain does not amplify the +/-10 ms mixer-tick phase noise. Seeded with the
	 * first error after capture (level_err_ema_seeded), reset on flush/relock. A deliberate setpoint
	 * shift moves BOTH level_target_ms AND the buffer level by the same delta, so the error
	 * (buffered - target) is unchanged and this EMA is left untouched there (see
	 * asrc_compensator_shift_level_target). Mirror of
	 * src/asrc_bench.rs RealtimeAsrcCompensator::level_err_ema_ms. */
	double level_err_ema_ms;
	/* camera-box #1335 follow-up 5: whether level_err_ema_ms has been seeded since the last (re)lock
	 * -- gates the one-shot EMA seed (first accepted window seeds ema = err, later windows blend).
	 * Cleared by a flush so a relock re-seeds. Mirror of src/asrc_bench.rs
	 * RealtimeAsrcCompensator::level_err_ema_seeded. */
	bool level_err_ema_seeded;
};

/* Reset a servo to its just-constructed state: 0 ppm estimated/applied (assume
 * locked until observations correct it), an empty regression buffer, unlocked
 * -- mirrors RealtimeAsrcCompensator::new() in src/asrc_bench.rs. */
EXPORT void asrc_compensator_init(struct asrc_compensator *c);

/* Given the RAW (uncompensated, sample-count-stamped) audio-timeline advance
 * for one audio callback (`raw_advance_s` = frames / samples_per_sec) and that
 * callback's true master-clock duration (`master_block_s`, measured via the
 * SAME wall-clock basis genlock_wall_now_ns() uses for the video FIFO
 * release), returns the advance AFTER compensation and reports the CURRENT
 * applied ppm via `*applied_ppm_out` (for translating into a
 * swr_set_compensation() call and for telemetry). camera-box #962: internally
 * accumulates raw_advance_s/master_block_s into a duration-weighted WINDOW
 * (ASRC_WINDOW_S) and only estimates/gates once per window close -- the
 * ESTIMATE (and the correction TARGET derived from it) only actually change on
 * a call that closes an ACCEPTED window. applied_ppm itself still slews
 * toward the current target on every call (not just window-close calls),
 * EXCEPT a call that closes a REJECTED window, which HOLDS applied_ppm at
 * exactly its pre-rejection value (no target recompute, no slew step) -- a
 * starved window must not be allowed to keep advancing an already-decided,
 * legitimate slew transition either. Every call still returns a corrected
 * advance using whatever applied_ppm is currently in effect. camera-box #1084:
 * each accepted window becomes one point in a sliding least-squares RATE
 * regression (ASRC_REGRESSION_SPAN_S) that produces the ppm estimate; a #960
 * rejection OR a non-positive master_block_s FLUSHES that buffer (a level shift
 * would corrupt the slope for a full span). The flush drops the lock, so
 * applied_ppm is held on the flushing call, then slews back to 0 over the
 * ~ASRC_REGRESSION_LOCK_SPAN_S re-lock window before re-converging. camera-box
 * #1335: `buffered_ms` is the source's current mix-buffer depth (obs-source.c
 * reads it from audio_input_buf[0].size via obs_source_input_buf_ms()); a slow
 * LEVEL integral captured at first lock folds into the correction target so the
 * buffer holds its setpoint instead of drifting on a residual the pure RATE loop
 * cannot remove. Mirror of RealtimeAsrcCompensator::compensate_with_level() in
 * src/asrc_bench.rs -- keep the two numerically identical. */
EXPORT double asrc_compensator_compensate(struct asrc_compensator *c, double raw_advance_s, double master_block_s,
					   double buffered_ms, double *applied_ppm_out);

/* Whether ASRC_LOG_INTERVAL_S has elapsed since the last telemetry line, and
 * if so, reset the log-interval accumulator (the caller is expected to blog()
 * immediately after a true return, using estimated_ppm/applied_ppm and the
 * cumulative correction below). Returns the cumulative |raw-corrected| ms
 * since the previous log line via `*cumulative_correction_ms_out`, then resets
 * that accumulator to 0 for the next interval. Also returns the count of
 * blocks REJECTED as starved/bursting (camera-box #960) since the previous
 * log line via `*starved_block_count_out`, then resets that accumulator to 0
 * too -- so a starved/invalid-block state is explicit in telemetry instead of
 * silently hiding behind an estimated/applied pair alone. */
EXPORT bool asrc_compensator_should_log(struct asrc_compensator *c, double *cumulative_correction_ms_out,
					 uint32_t *starved_block_count_out);

/* camera-box #806: set the OUTER-loop bias, in ppm -- clamped to
 * +/-ASRC_OUTER_BIAS_MAX_PPM unconditionally (never trust the caller alone to
 * have already clamped; this field is reachable from outside this file via
 * the obs-websocket SetAsrcOuterBiasPpm request). Takes effect on the NEXT
 * asrc_compensator_compensate() call. Mirror of src/asrc_bench.rs
 * RealtimeAsrcCompensator::set_outer_bias_ppm. */
EXPORT void asrc_compensator_set_outer_bias_ppm(struct asrc_compensator *c, double bias_ppm);

/* camera-box #806: the outer-loop bias currently in effect, in ppm -- exposed
 * for telemetry (GetAsrcOuterBiasPpm) and tests. */
EXPORT double asrc_compensator_get_outer_bias_ppm(const struct asrc_compensator *c);

/* camera-box #1335 follow-up: shift the captured buffer-LEVEL setpoint by
 * `delta_ms` to track a DELIBERATE audio sync-offset change. A sync-offset
 * change of Delta (ns -> ms) moves this source's audio placement -- and thus
 * its mix-buffer depth -- by exactly Delta (obs-source.c applies
 * `in.timestamp += sync_offset` BEFORE placement). Without this, the level
 * integral would keep the OLD setpoint and REFILL the buffer back toward it,
 * silently cancelling the deliberate audio trim within ~1-2 h. Move the
 * setpoint by the SAME Delta so the integral holds the NEW depth. level_last_ms
 * is shifted too so the telemetry `level=` reads consistently with the shifted
 * `target=` until the next accepted window refreshes it (it is telemetry-only --
 * the error term reads the LIVE buffered_ms, never level_last_ms). The integral
 * itself is left untouched (no windup).
 * UNINTENDED discontinuities (dropout/relock) must NOT call this -- they flush
 * the regression (dropping level_captured) so the setpoint re-captures and the
 * buffer self-heals its calibrated depth. No-op until the setpoint has been
 * captured (first rate lock). Mirror of src/asrc_bench.rs
 * RealtimeAsrcCompensator::shift_level_target. */
EXPORT void asrc_compensator_shift_level_target(struct asrc_compensator *c, double delta_ms);

#ifdef __cplusplus
}
#endif
