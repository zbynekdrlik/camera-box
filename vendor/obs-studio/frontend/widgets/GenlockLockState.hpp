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
} genlock_lock_reason_t;

typedef struct genlock_lock_facets {
	int n_inputs;               /* genlock-FIFO inputs present */
	int n_locked;               /* of those, currently locked */
	int recent_event;           /* bool: relock/underrun/late-hold/backward-step in last 60 s */
	int qpc_drift_beyond_bound; /* bool */
	int clock_present;          /* bool: dantesync :8898/status answered */
	int clock_locked;           /* bool: is_locked */
	int clock_ntp_failed;       /* bool: ntp_failed */
	int output_present;         /* bool: a genlock NDI sender is active here */
	int output_stamping;        /* bool: ...and it is stamping wall-clock timecodes */
} genlock_lock_facets_t;

/* Mirror of camera_box::genlock_lock_state::decide (src/genlock_lock_state.rs) — keep
 * both in lock-step. UNLOCKED precedence: clock > output > no-input-locked. DEGRADED
 * precedence: some-input-unlocked > recent-event > ntp-failed > qpc-drift. Else LOCKED.
 * Writes the dominant reason to *reason_out (if non-NULL) and returns the state. */
static inline genlock_lock_state_t genlock_decide_lock_state(const genlock_lock_facets_t *f,
							     genlock_lock_reason_t *reason_out)
{
	genlock_lock_reason_t reason = GENLOCK_LOCK_REASON_NONE;
	genlock_lock_state_t state;

	if (!f->clock_present || !f->clock_locked) {
		reason = GENLOCK_LOCK_REASON_CLOCK;
		state = GENLOCK_LOCK_UNLOCKED;
	} else if (f->output_present && !f->output_stamping) {
		reason = GENLOCK_LOCK_REASON_OUTPUT;
		state = GENLOCK_LOCK_UNLOCKED;
	} else if (f->n_locked <= 0) {
		reason = (f->n_inputs <= 0) ? GENLOCK_LOCK_REASON_NO_GENLOCK : GENLOCK_LOCK_REASON_NO_INPUT_LOCKED;
		state = GENLOCK_LOCK_UNLOCKED;
	} else if (f->n_locked < f->n_inputs) {
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
	} else {
		reason = GENLOCK_LOCK_REASON_NONE;
		state = GENLOCK_LOCK_LOCKED;
	}

	if (reason_out)
		*reason_out = reason;
	return state;
}

#ifdef __cplusplus
}
#endif
