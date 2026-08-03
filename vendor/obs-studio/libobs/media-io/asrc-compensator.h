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

/* EMA time constant, in seconds, of the rate estimator. Long enough to be
 * robust to per-callback jitter (issue #803: "dlhý horizont, robustný na
 * callback jitter") while still meeting the ticket's own convergence text
 * (<5 ppm at ~2 min, ~1 ppm at ~10 min, at the #800 worst-case 50 ppm --
 * proven in src/asrc_bench.rs's estimator_converges_within_the_tickets_own_bounds
 * test). Mirror of src/asrc_bench.rs TIME_CONSTANT_S. */
#define ASRC_TIME_CONSTANT_S 20.0

/* Minimum wall-clock time the estimator must observe before ANY compensation
 * is applied -- "default-safe: zero compensation when the servo has no lock".
 * Mirror of src/asrc_bench.rs MIN_LOCK_S. */
#define ASRC_MIN_LOCK_S 5.0

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
 * arrival-timing jitter to cancel completely -- while staying small relative to
 * ASRC_TIME_CONSTANT_S (20s), so it barely perturbs the EMA's own convergence (the time-based EMA
 * discretization is mathematically exact for any window length when the driving ratio is
 * constant over it). Mirror of src/asrc_bench.rs WINDOW_S -- keep numerically identical. */
#define ASRC_WINDOW_S 1.0

/* Per-source servo state. One instance lives per obs_source_t (see
 * obs-internal.h's `struct asrc_compensator asrc` field) and is mutated only
 * from the audio-ingest call path (process_audio(), always invoked from the
 * same source's own audio thread) -- single-writer, no lock needed for the
 * struct itself; obs_source_set_asrc_enabled() only flips the separate
 * `asrc_enabled` bool, following the same convention as genlock_burn. */
struct asrc_compensator {
	/* Running EMA estimate of the source's true rate offset from master, in ppm. */
	double estimated_ppm;
	/* The correction actually being applied right now (post-clamp, post-slew), in ppm. */
	double applied_ppm;
	/* Cumulative master-clock time observed since asrc_compensator_init() -- gates the
	 * ASRC_MIN_LOCK_S startup delay. */
	double elapsed_lock_s;
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
	 * window_raw_s/window_master_s, fed to the EMA (or rejected under the #960 ceiling), and both
	 * sums reset to 0.0 for the next window. Mirror of src/asrc_bench.rs
	 * RealtimeAsrcCompensator::window_master_s. */
	double window_master_s;
	/* camera-box #962: count of individual audio blocks folded into the CURRENT (not yet closed)
	 * window -- reset to 0 alongside the sums above whenever the window closes. Mirror of
	 * src/asrc_bench.rs RealtimeAsrcCompensator::window_block_count. */
	uint32_t window_block_count;
};

/* Reset a servo to its just-constructed state: 0 ppm estimated/applied (assume
 * locked until observations correct it), no elapsed lock time -- mirrors
 * RealtimeAsrcCompensator::new() in src/asrc_bench.rs. */
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
 * estimate/applied ppm only actually CHANGE on a call that closes a window;
 * every call still returns a corrected advance using whatever applied_ppm is
 * currently in effect. Mirror of RealtimeAsrcCompensator::compensate() in
 * src/asrc_bench.rs -- keep the two numerically identical. */
EXPORT double asrc_compensator_compensate(struct asrc_compensator *c, double raw_advance_s, double master_block_s,
					   double *applied_ppm_out);

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

#ifdef __cplusplus
}
#endif
