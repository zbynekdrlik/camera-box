// GenlockLockState.hpp — camera-box #1298
//
// The pure, OBS/Qt-FREE genlock LOCKED / DEGRADED / UNLOCKED decision the in-OBS
// statusbar indicator (OBSBasicStatusBar) renders. It takes plain scalars only (no
// pointers into OBS types) so it is a byte-for-byte port of the Tier-0 Rust authority
// camera_box::genlock_lock_state::decide (src/genlock_lock_state.rs). The two are held
// numerically identical by the committed C-vs-Rust parity gate
// tests/genlock_lock_state_parity.rs, which lifts the decision function VERBATIM out of
// this header, compiles it standalone with cc, and compares (state, reason) across a
// vector spread.
//
// Keep the two enums + the struct + the function CONTIGUOUS: the parity gate lifts the
// block from the first enum definition through genlock_decide_lock_state's closing brace,
// so intervening unrelated code would splice into the harness. Keep it valid in BOTH C and
// C++ (the gate compiles it as C; OBSBasicStatusBar.cpp includes it as C++).
#pragma once

#include <stdint.h> /* #1299 Part 3: uint64_t / UINT64_MAX for genlock_input_phase_events (pure C — not <obs>/<Q>, so the parity-lift + purity guard stay green) */

#ifdef __cplusplus
extern "C" {
#endif

typedef enum genlock_lock_state {
	GENLOCK_LOCK_UNLOCKED = 0, /* red   */
	GENLOCK_LOCK_DEGRADED = 1, /* amber */
	GENLOCK_LOCK_LOCKED = 2,   /* green */
} genlock_lock_state_t;

typedef enum genlock_lock_reason {
	GENLOCK_LOCK_REASON_NONE = 0,            /* LOCKED, no reason */
	GENLOCK_LOCK_REASON_NO_GENLOCK = 1,      /* no genlock inputs configured at all */
	GENLOCK_LOCK_REASON_CLOCK = 2,           /* clock absent or not locked */
	GENLOCK_LOCK_REASON_OUTPUT = 3,          /* a genlock output present but NOT stamping wall time */
	GENLOCK_LOCK_REASON_NO_INPUT_LOCKED = 4, /* inputs exist but none locked */
	GENLOCK_LOCK_REASON_INPUT_UNLOCKED = 5,  /* some (not all) inputs unlocked */
	GENLOCK_LOCK_REASON_RECENT_EVENT = 6,    /* relock/underrun/late-hold/backward-step in last 60 s */
	GENLOCK_LOCK_REASON_NTP_FAILED = 7,      /* clock up but NTP phase failed */
	GENLOCK_LOCK_REASON_QPC_DRIFT = 8,       /* the wall clock stepped by more than one frame */
	GENLOCK_LOCK_REASON_AUDIO_PAIRING = 9,   /* #1303 audio-enabled source unpaired with its video FIFO hold */
	GENLOCK_LOCK_REASON_AUDIO_UNEXPECTED = 10, /* #1303 source audible when the certified per-box table expects it silent (double-audio hazard) */
	GENLOCK_LOCK_REASON_MEDIA_CLOCK = 11,      /* issue 1372 part D: the media (audio) clock does not follow the disciplined wall clock */
} genlock_lock_reason_t;

typedef struct genlock_lock_facets {
	int n_inputs;               /* genlock-FIFO inputs present */
	int n_locked;               /* of those, currently locked */
	int n_absent;               /* #1299: of n_inputs, how many have NO live NDI receiver connection (sender not running); n_connected = n_inputs - n_absent is the DEGRADED-gate denominator */
	int n_idle;                 /* #1341: of n_inputs, how many are CONNECTED but IDLE (keep-alive-only, received-frame rate below the idle floor over the window); excluded from n_locked + n_connected = n_inputs - n_absent - n_idle */
	int recent_event;           /* bool: relock/underrun/late-hold/backward-step in last 60 s */
	int qpc_drift_beyond_bound; /* bool */
	int clock_present;          /* bool: dantesync :8898/status answered */
	int clock_locked;           /* bool: is_locked */
	int clock_ntp_failed;       /* bool: ntp_failed */
	int output_present;         /* bool: a genlock NDI sender is active here */
	int output_stamping;        /* bool: ...and it is stamping wall-clock timecodes */
	int audio_unpaired;         /* bool: #1303 an audio-enabled genlock source's audio is unpaired with its video FIFO hold */
	int audio_unexpected;       /* bool: #1303 a source is audible when the certified per-box audio table expects it silent (double-audio hazard) */
	int media_clock;            /* issue 1372 part D: the genlock_media_clock verdict (0 ok, 1 drift, 2 undisciplined); nonzero DEGRADES, never UNLOCKS */
} genlock_lock_facets_t;

/* Mirror of camera_box::genlock_lock_state::decide (src/genlock_lock_state.rs) — keep
 * both in lock-step. UNLOCKED precedence: clock > output > no-input-locked. DEGRADED
 * precedence: some-input-unlocked > recent-event > ntp-failed > qpc-drift > media-clock >
 * audio-pairing > audio-unexpected. Else LOCKED.
 * #1299/#1341: the input decisions judge only CONNECTED-non-idle inputs (n_connected = n_inputs -
 * n_absent - n_idle); a senderless (n_absent) OR a keep-alive-only idle (n_idle) input is excluded
 * (never DEGRADES), and inputs-present-but-ALL-absent/idle is HEALTHY-idle (LOCKED), not UNLOCKED.
 * n_inputs<=0 stays UNLOCKED/no_genlock.
 * Writes the dominant reason to *reason_out (if non-NULL) and returns the state. */
static inline genlock_lock_state_t genlock_decide_lock_state(const genlock_lock_facets_t *f,
							     genlock_lock_reason_t *reason_out)
{
	genlock_lock_reason_t reason = GENLOCK_LOCK_REASON_NONE;
	genlock_lock_state_t state;

	/* #1299/#1341: connected-non-idle inputs only (saturating at 0 keeps the decision total under a
	 * transient n_absent + n_idle > n_inputs). */
	int n_connected = f->n_inputs - f->n_absent - f->n_idle;
	if (n_connected < 0)
		n_connected = 0;

	if (!f->clock_present || !f->clock_locked) {
		reason = GENLOCK_LOCK_REASON_CLOCK;
		state = GENLOCK_LOCK_UNLOCKED;
	} else if (f->output_present && !f->output_stamping) {
		reason = GENLOCK_LOCK_REASON_OUTPUT;
		state = GENLOCK_LOCK_UNLOCKED;
	} else if (f->n_locked <= 0) {
		if (f->n_inputs <= 0) {
			reason = GENLOCK_LOCK_REASON_NO_GENLOCK; /* no genlock configured at all */
			state = GENLOCK_LOCK_UNLOCKED;
		} else if (n_connected <= 0) {
			reason = GENLOCK_LOCK_REASON_NONE; /* #1299: inputs present but all senderless -> HEALTHY-idle */
			state = GENLOCK_LOCK_LOCKED;
		} else {
			reason = GENLOCK_LOCK_REASON_NO_INPUT_LOCKED; /* live senders, none locking -> fault */
			state = GENLOCK_LOCK_UNLOCKED;
		}
	} else if (f->n_locked < n_connected) {
		reason = GENLOCK_LOCK_REASON_INPUT_UNLOCKED;
		state = GENLOCK_LOCK_DEGRADED;
	} else if (f->recent_event) {
		reason = GENLOCK_LOCK_REASON_RECENT_EVENT;
		state = GENLOCK_LOCK_DEGRADED;
	} else if (f->clock_ntp_failed) {
		reason = GENLOCK_LOCK_REASON_NTP_FAILED;
		state = GENLOCK_LOCK_DEGRADED;
	} else if (f->qpc_drift_beyond_bound) {
		reason = GENLOCK_LOCK_REASON_QPC_DRIFT;
		state = GENLOCK_LOCK_DEGRADED;
	} else if (f->media_clock != 0) {
		reason = GENLOCK_LOCK_REASON_MEDIA_CLOCK; /* issue 1372 part D: a clock-class cause, above the audio symptoms */
		state = GENLOCK_LOCK_DEGRADED;
	} else if (f->audio_unpaired) {
		reason = GENLOCK_LOCK_REASON_AUDIO_PAIRING; /* #1303 audio-pairing DEGRADED axis */
		state = GENLOCK_LOCK_DEGRADED;
	} else if (f->audio_unexpected) {
		reason = GENLOCK_LOCK_REASON_AUDIO_UNEXPECTED; /* #1303 lowest-precedence DEGRADED axis */
		state = GENLOCK_LOCK_DEGRADED;
	} else {
		reason = GENLOCK_LOCK_REASON_NONE;
		state = GENLOCK_LOCK_LOCKED;
	}

	if (reason_out)
		*reason_out = reason;
	return state;
}

/* #1299 Part 3 — the pure "phase event" count for ONE genlock input feeding recent_event: the
 * clock/phase class a LOCK verdict owns (relocks + late_holds + backward_steps). UNDERRUNS are
 * EXCLUDED (a latency-budget miss owned by the genlock-fifo audit + cg-chain-verify / issue 1302,
 * and bursty) and a DISCONNECTED input contributes 0 (its #1096 rebind churn is idle, not a fault).
 * Byte-for-byte mirror of camera_box::genlock_lock_state::input_phase_events — the parity gate
 * tests/genlock_lock_state_parity.rs lifts THIS function too. Saturating so a pathological count can
 * never wrap (matches the Rust saturating_add). Placed AFTER genlock_decide_lock_state so the
 * decision-block lift is unaffected. #1341: a connected-but-IDLE input (keep-alive-only) also
 * contributes 0 — its relock churn on each keep-alive frame is not a phase-discipline event. */
static inline uint64_t genlock_input_phase_events(int connected, int idle, uint64_t relocks,
						  uint64_t late_holds, uint64_t backward_steps)
{
	uint64_t sum;
	if (!connected || idle)
		return 0;
	sum = relocks;
	if (sum > UINT64_MAX - late_holds)
		return UINT64_MAX;
	sum += late_holds;
	if (sum > UINT64_MAX - backward_steps)
		return UINT64_MAX;
	sum += backward_steps;
	return sum;
}

/* #1303 — case-insensitive ASCII substring test, a private helper for genlock_name_is_camera below.
 * Returns 1 iff `needle` (assumed non-empty) occurs in `hay`. Pure C (no libc strcasestr, which is
 * non-standard), so it lifts + compiles standalone in the parity gate. */
static inline int genlock_ci_contains(const char *hay, const char *needle)
{
	const char *h;
	if (!hay || !needle || !*needle)
		return 0;
	for (h = hay; *h; ++h) {
		const char *a = h;
		const char *b = needle;
		while (*a && *b) {
			char ca = *a;
			char cb = *b;
			if (ca >= 'A' && ca <= 'Z')
				ca = (char)(ca - 'A' + 'a');
			if (cb >= 'A' && cb <= 'Z')
				cb = (char)(cb - 'A' + 'a');
			if (ca != cb)
				break;
			++a;
			++b;
		}
		if (!*b)
			return 1;
	}
	return 0;
}

/* #1303 — byte-for-byte mirror of camera_box::genlock_forced_table_audit::is_camera_input: a camera
 * NDI input is one whose name contains "(usb)", OR contains "cam" AND an ASCII digit anywhere
 * (CAM3 / cam 2 / camera1). The widget uses it to identify a silent-by-contract input for the
 * #1303 audio_unexpected term. Parity-gated against the Rust canonical by
 * tests/genlock_lock_state_parity.rs, so the two name classifiers can never drift. */
static inline int genlock_name_is_camera(const char *name)
{
	const char *p;
	if (!name)
		return 0;
	if (genlock_ci_contains(name, "(usb)"))
		return 1;
	if (!genlock_ci_contains(name, "cam"))
		return 0;
	for (p = name; *p; ++p)
		if (*p >= '0' && *p <= '9')
			return 1;
	return 0;
}

/* #1299 Part 4 + #1357 scope C — the wall-vs-monotonic qpc_drift verdict + its measured rate. The
 * CUMULATIVE wall_qpc_drift_ms never gates (on a dantesync-disciplined Windows box it grew ~50 ms/h
 * against the free QPC crystal until issue 1372 disciplined os_gettime_ns). The RATE never gates either: on Linux CLOCK_MONOTONIC is
 * kernel-disciplined together with CLOCK_REALTIME, so the measured rate is 0 by construction, while on
 * Windows it was the free crystal (since issue 1372 the Windows os_gettime_ns runs at the
 * disciplined rate too, so it reads ~0 on every box) — a rate check meant a different thing per box
 * and false-DEGRADED both. DEGRADED only on a single-sample wall STEP > step_bound_ms (judged as soon as two samples
 * exist, rate_ready or not) — the one clock hazard for genlock, the same on every box. Writes the
 * measured windowed rate (ppm) to *measured_ppm_out as report-only telemetry. drift_delta_ms/elapsed_ms
 * are the integer-ms cumulative-drift delta + elapsed span across the window; max_step_ms the largest
 * single-sample jump within it. Byte-for-byte mirror of
 * camera_box::genlock_lock_state::qpc_drift_beyond_bound (+ qpc_window_rate_ppm) — the parity gate
 * tests/genlock_lock_state_parity.rs lifts THIS function too. Placed AFTER genlock_name_is_camera so no
 * other lift's contiguity is disturbed. */
static inline int genlock_qpc_drift_beyond_bound(int rate_ready, long long drift_delta_ms,
						 long long elapsed_ms, long long max_step_ms,
						 long long step_bound_ms, double *measured_ppm_out)
{
	double measured_ppm = 0.0;
	if (rate_ready && elapsed_ms > 0)
		measured_ppm = (double)drift_delta_ms / (double)elapsed_ms * 1000000.0;
	if (measured_ppm_out)
		*measured_ppm_out = measured_ppm;
	/* a STEP is the one clock hazard, judged even before the rate window fills. */
	if (max_step_ms < 0)
		max_step_ms = -max_step_ms;
	return max_step_ms > step_bound_ms ? 1 : 0;
}

/* Issue 1372 part D: the MEDIA-clock (audio clock) term. os_gettime_ns() paces the audio mixer, the
 * video thread and every output timestamp; since part A the Windows os_gettime_ns() runs at the
 * dantesync-disciplined rate (Linux's CLOCK_MONOTONIC always did), so the wall-vs-media offset must stay
 * flat on EVERY box apart from wall steps. Its growth over a window is therefore compared with 0 -- the
 * same meaning on every box, unlike the removed #1357 rate-vs-slew term. Mirror of
 * camera_box::genlock_lock_state (MediaDiscipline / MediaClock / media_clock_step_allowance_us /
 * media_clock_window_drift_us / media_clock_verdict); the parity gate
 * tests/genlock_lock_state_parity.rs lifts this block from the discipline enum through the verdict's
 * closing brace. Placed AFTER genlock_qpc_drift_beyond_bound so no earlier lift is disturbed. */

/* The values libobs os_gettime_discipline() returns (enum os_gettime_discipline_state in
 * util/platform.h), plus NOT_APPLICABLE for a box whose OS disciplines the monotonic clock itself. */
typedef enum genlock_media_discipline {
	GENLOCK_MEDIA_DISCIPLINE_UNKNOWN = 0,
	GENLOCK_MEDIA_DISCIPLINE_ACTIVE = 1,
	GENLOCK_MEDIA_DISCIPLINE_DISABLED = 2,
	GENLOCK_MEDIA_DISCIPLINE_READ_FAILED = 3,
	GENLOCK_MEDIA_DISCIPLINE_API_MISSING = 4,
	GENLOCK_MEDIA_DISCIPLINE_NOT_APPLICABLE = 5,
} genlock_media_discipline_t;

typedef enum genlock_media_clock {
	GENLOCK_MEDIA_CLOCK_OK = 0,
	GENLOCK_MEDIA_CLOCK_DRIFT = 1,         /* wall-vs-media drift grew beyond the bound over the window */
	GENLOCK_MEDIA_CLOCK_UNDISCIPLINED = 2, /* Windows: fell back to raw QPC while dantesync runs */
} genlock_media_clock_t;

/* Saturating int64 helpers (MSVC has no __builtin overflow checks). */
static inline int64_t genlock_media_sat_add(int64_t a, int64_t b)
{
	if (b > 0 && a > INT64_MAX - b)
		return INT64_MAX;
	if (b < 0 && a < INT64_MIN - b)
		return INT64_MIN;
	return a + b;
}

static inline int64_t genlock_media_sat_sub(int64_t a, int64_t b)
{
	if (b < 0 && a > INT64_MAX + b)
		return INT64_MAX;
	if (b > 0 && a < INT64_MIN + b)
		return INT64_MIN;
	return a - b;
}

static inline int64_t genlock_media_sat_abs(int64_t v)
{
	if (v == INT64_MIN)
		return INT64_MAX;
	return v < 0 ? -v : v;
}

/* The largest sample-to-sample offset change (us) that is still a RATE over dt_ms:
 * floor_us + floor(dt_ms * max_rate_ppm / 1000). A non-positive interval or rate allows floor_us.
 * Saturating. */
static inline int64_t genlock_media_clock_step_allowance_us(int64_t dt_ms, int64_t max_rate_ppm, int64_t floor_us)
{
	int64_t product;
	if (dt_ms <= 0 || max_rate_ppm <= 0)
		return floor_us;
	product = dt_ms > INT64_MAX / max_rate_ppm ? INT64_MAX : dt_ms * max_rate_ppm;
	return genlock_media_sat_add(floor_us, product / 1000);
}

/* The wall-vs-media GROWTH (us) across n samples (oldest first; t_ms the widget's monotonic ms,
 * offset_us the wall-minus-media offset the widget samples in us): the sum of the sample-to-sample
 * offset changes a rate of at most max_rate_ppm could make over their interval. A larger change is a
 * wall STEP (every dantesync phase step is >= 500 us) and is left out, and a pair more than max_gap_ms
 * apart (a stalled UI) adds nothing, so steps and stalls never read as an audio-clock rate. n < 2 -> 0. */
static inline int64_t genlock_media_clock_window_drift_us(const int64_t *t_ms, const int64_t *offset_us, int n,
							  int64_t max_rate_ppm, int64_t floor_us,
							  int64_t max_gap_ms)
{
	int64_t sum = 0;
	int i;
	for (i = 1; i < n; ++i) {
		const int64_t dt = genlock_media_sat_sub(t_ms[i], t_ms[i - 1]);
		int64_t change;
		if (dt > max_gap_ms)
			continue;
		change = genlock_media_sat_sub(offset_us[i], offset_us[i - 1]);
		if (genlock_media_sat_abs(change) > genlock_media_clock_step_allowance_us(dt, max_rate_ppm, floor_us))
			continue;
		sum = genlock_media_sat_add(sum, change);
	}
	return sum;
}

/* UNDISCIPLINED when the Windows clock fell back to raw QPC while dantesync answers (clock_present);
 * else DRIFT when the window is ready and |drift_us| > drift_bound_us; else OK. */
static inline genlock_media_clock_t genlock_media_clock_verdict(int window_ready, int64_t drift_us,
								int64_t drift_bound_us, int discipline,
								int clock_present)
{
	const int raw_fallback = discipline == GENLOCK_MEDIA_DISCIPLINE_DISABLED ||
				 discipline == GENLOCK_MEDIA_DISCIPLINE_READ_FAILED ||
				 discipline == GENLOCK_MEDIA_DISCIPLINE_API_MISSING;
	if (clock_present && raw_fallback)
		return GENLOCK_MEDIA_CLOCK_UNDISCIPLINED;
	if (window_ready && genlock_media_sat_abs(drift_us) > drift_bound_us)
		return GENLOCK_MEDIA_CLOCK_DRIFT;
	return GENLOCK_MEDIA_CLOCK_OK;
}

#ifdef __cplusplus
}
#endif
