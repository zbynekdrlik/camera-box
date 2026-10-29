/* camera-box issue 1387 -- the in-OBS JPEG XS indicator's decision (owner request 29.10.2026,
 * design 6506884476): each row DistroAV's JPEG XS source / program output publishes is graded OK /
 * DEGRADED / DOWN, then the box OFF / OK / DEGRADED / DOWN, plus the Sources-dock transport tag of
 * a source kind.
 *
 * C, header-only, no Qt or OBS calls: the authority is src/genlock_jxs_state.rs and
 * tests/genlock_jxs_state_parity_1387.rs compiles THIS file as C11 and as C++17 and compares every
 * vector against it. Keep the two sides in lock-step (names, numbers, precedence). */
#pragma once

#include <stdint.h>
#include <string.h>

/* The rows' bounds (src/genlock_jxs_state.rs JXS_DOWN_AGE_MS / JXS_MIN_FPS_X100 /
 * JXS_REPORT_MAX_AGE_MS). */
#define GENLOCK_JXS_DOWN_AGE_MS INT64_C(3000)
#define GENLOCK_JXS_MIN_FPS_X100 UINT32_C(2700)
#define GENLOCK_JXS_REPORT_MAX_AGE_MS INT64_C(3000)

typedef enum genlock_jxs_kind {
	GENLOCK_JXS_KIND_INPUT = 0,
	GENLOCK_JXS_KIND_OUTPUT = 1,
} genlock_jxs_kind_t;

typedef enum genlock_jxs_health {
	GENLOCK_JXS_HEALTH_OK = 0,
	GENLOCK_JXS_HEALTH_DEGRADED = 1,
	GENLOCK_JXS_HEALTH_DOWN = 2,
} genlock_jxs_health_t;

typedef enum genlock_jxs_state {
	GENLOCK_JXS_OFF = 0,
	GENLOCK_JXS_OK = 1,
	GENLOCK_JXS_DEGRADED = 2,
	GENLOCK_JXS_DOWN = 3,
} genlock_jxs_state_t;

typedef enum genlock_jxs_reason {
	GENLOCK_JXS_REASON_NONE = 0,
	GENLOCK_JXS_REASON_NO_JXS = 1,
	GENLOCK_JXS_REASON_OUTPUT_DOWN = 2,
	GENLOCK_JXS_REASON_ALL_INPUTS_DOWN = 3,
	GENLOCK_JXS_REASON_INPUT_DOWN = 4,
	GENLOCK_JXS_REASON_OUTPUT_DEGRADED = 5,
	GENLOCK_JXS_REASON_INPUT_DEGRADED = 6,
} genlock_jxs_reason_t;

/* One table row, scalarised (age_ms / report_age_ms negative = none yet). */
typedef struct genlock_jxs_entry {
	int kind; /* genlock_jxs_kind_t */
	int64_t age_ms;
	int window;
	uint32_t fps_x100;
	uint64_t lost;
	uint64_t undecodable;
	uint64_t dropped;
	uint64_t errors;
	int64_t report_age_ms;
} genlock_jxs_entry_t;

/* DOWN with no frame for the bound; DEGRADED when the last window ran short, lost, dropped or
 * failed, or (the output) heard no receiver report for the bound; else OK. */
static inline genlock_jxs_health_t genlock_jxs_entry_health(const genlock_jxs_entry_t *e)
{
	if (e->age_ms < 0 || e->age_ms > GENLOCK_JXS_DOWN_AGE_MS)
		return GENLOCK_JXS_HEALTH_DOWN;
	if (!e->window)
		return GENLOCK_JXS_HEALTH_OK;
	if (e->fps_x100 < GENLOCK_JXS_MIN_FPS_X100 || e->lost > 0 || e->undecodable > 0 || e->dropped > 0 ||
	    e->errors > 0)
		return GENLOCK_JXS_HEALTH_DEGRADED;
	if (e->kind == GENLOCK_JXS_KIND_OUTPUT &&
	    (e->report_age_ms < 0 || e->report_age_ms > GENLOCK_JXS_REPORT_MAX_AGE_MS))
		return GENLOCK_JXS_HEALTH_DEGRADED;
	return GENLOCK_JXS_HEALTH_OK;
}

typedef struct genlock_jxs_counts {
	int n_inputs;
	int n_inputs_down;
	int n_inputs_degraded;
	int n_outputs;
	int n_outputs_down;
	int n_outputs_degraded;
} genlock_jxs_counts_t;

/* OFF with no row; DOWN: an output down > every input down; DEGRADED: some input down > an output
 * degraded > an input degraded; else OK. */
static inline genlock_jxs_state_t genlock_jxs_decide(const genlock_jxs_counts_t *c, genlock_jxs_reason_t *reason)
{
	if (c->n_inputs <= 0 && c->n_outputs <= 0) {
		*reason = GENLOCK_JXS_REASON_NO_JXS;
		return GENLOCK_JXS_OFF;
	}
	if (c->n_outputs > 0 && c->n_outputs_down > 0) {
		*reason = GENLOCK_JXS_REASON_OUTPUT_DOWN;
		return GENLOCK_JXS_DOWN;
	}
	if (c->n_inputs > 0 && c->n_inputs_down >= c->n_inputs) {
		*reason = GENLOCK_JXS_REASON_ALL_INPUTS_DOWN;
		return GENLOCK_JXS_DOWN;
	}
	if (c->n_inputs > 0 && c->n_inputs_down > 0) {
		*reason = GENLOCK_JXS_REASON_INPUT_DOWN;
		return GENLOCK_JXS_DEGRADED;
	}
	if (c->n_outputs > 0 && c->n_outputs_degraded > 0) {
		*reason = GENLOCK_JXS_REASON_OUTPUT_DEGRADED;
		return GENLOCK_JXS_DEGRADED;
	}
	if (c->n_inputs > 0 && c->n_inputs_degraded > 0) {
		*reason = GENLOCK_JXS_REASON_INPUT_DEGRADED;
		return GENLOCK_JXS_DEGRADED;
	}
	*reason = GENLOCK_JXS_REASON_NONE;
	return GENLOCK_JXS_OK;
}

/* The Sources-dock transport tag of a source kind (its unversioned id), never of its name:
 * "NDI" for DistroAV's ndi_source, "JPEG XS" for camera_box_jxs_source, NULL otherwise. */
static inline const char *genlock_jxs_transport_tag(const char *unversioned_id)
{
	if (!unversioned_id)
		return 0;
	if (strcmp(unversioned_id, "ndi_source") == 0)
		return "NDI";
	if (strcmp(unversioned_id, "camera_box_jxs_source") == 0)
		return "JPEG XS";
	return 0;
}

static inline const char *genlock_jxs_state_name(genlock_jxs_state_t s)
{
	switch (s) {
	case GENLOCK_JXS_OFF:
		return "OFF";
	case GENLOCK_JXS_OK:
		return "OK";
	case GENLOCK_JXS_DEGRADED:
		return "DEGRADED";
	case GENLOCK_JXS_DOWN:
		return "DOWN";
	}
	return "?";
}

static inline const char *genlock_jxs_reason_key(genlock_jxs_reason_t r)
{
	switch (r) {
	case GENLOCK_JXS_REASON_NONE:
		return "none";
	case GENLOCK_JXS_REASON_NO_JXS:
		return "no_jxs";
	case GENLOCK_JXS_REASON_OUTPUT_DOWN:
		return "output_down";
	case GENLOCK_JXS_REASON_ALL_INPUTS_DOWN:
		return "all_inputs_down";
	case GENLOCK_JXS_REASON_INPUT_DOWN:
		return "input_down";
	case GENLOCK_JXS_REASON_OUTPUT_DEGRADED:
		return "output_degraded";
	case GENLOCK_JXS_REASON_INPUT_DEGRADED:
		return "input_degraded";
	}
	return "?";
}
