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
	GENLOCK_LOCK_REASON_QPC_DRIFT = 8,       /* wall-vs-monotonic drift beyond bound */
	GENLOCK_LOCK_REASON_AUDIO_PAIRING = 9,   /* #1303 audio-enabled source unpaired with its video FIFO hold */
} genlock_lock_reason_t;

typedef struct genlock_lock_facets {
	int n_inputs;               /* genlock-FIFO inputs present */
	int n_locked;               /* of those, currently locked */
	int n_absent;               /* #1299: of n_inputs, how many have NO live NDI receiver connection (sender not running); n_connected = n_inputs - n_absent is the DEGRADED-gate denominator */
	int recent_event;           /* bool: relock/underrun/late-hold/backward-step in last 60 s */
	int qpc_drift_beyond_bound; /* bool */
	int clock_present;          /* bool: dantesync :8898/status answered */
	int clock_locked;           /* bool: is_locked */
	int clock_ntp_failed;       /* bool: ntp_failed */
	int output_present;         /* bool: a genlock NDI sender is active here */
	int output_stamping;        /* bool: ...and it is stamping wall-clock timecodes */
	int audio_unpaired;         /* bool: #1303 an audio-enabled genlock source's audio is unpaired with its video FIFO hold */
} genlock_lock_facets_t;

/* Mirror of camera_box::genlock_lock_state::decide (src/genlock_lock_state.rs) — keep
 * both in lock-step. UNLOCKED precedence: clock > output > no-input-locked. DEGRADED
 * precedence: some-input-unlocked > recent-event > ntp-failed > qpc-drift > audio-pairing. Else LOCKED.
 * #1299: the input decisions judge only CONNECTED inputs (n_connected = n_inputs - n_absent); a
 * senderless input is idle (never DEGRADES), and inputs-present-but-ALL-senderless is HEALTHY-idle
 * (LOCKED), not UNLOCKED. n_inputs<=0 stays UNLOCKED/no_genlock.
 * Writes the dominant reason to *reason_out (if non-NULL) and returns the state. */
static inline genlock_lock_state_t genlock_decide_lock_state(const genlock_lock_facets_t *f,
							     genlock_lock_reason_t *reason_out)
{
	genlock_lock_reason_t reason = GENLOCK_LOCK_REASON_NONE;
	genlock_lock_state_t state;

	/* #1299: connected inputs only (saturating at 0 keeps the decision total under a transient
	 * n_absent > n_inputs). */
	int n_connected = f->n_inputs - f->n_absent;
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
	} else if (f->audio_unpaired) {
		reason = GENLOCK_LOCK_REASON_AUDIO_PAIRING; /* #1303 lowest-precedence DEGRADED axis */
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
 * decision-block lift is unaffected. */
static inline uint64_t genlock_input_phase_events(int connected, uint64_t relocks,
						  uint64_t late_holds, uint64_t backward_steps)
{
	uint64_t sum;
	if (!connected)
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

#ifdef __cplusplus
}
#endif
