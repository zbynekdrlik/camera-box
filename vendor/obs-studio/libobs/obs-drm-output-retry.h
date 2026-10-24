#pragma once

/*
 * camera-box issue 1424 (owner 24.10.2026: "to musi vediet fungovat aj bez monitora aj po zapojeni
 * monitora aj po zapnuti!!!"; main design 6386467303, layer 3) -- the PURE retry decisions of a
 * vk-direct HDMI output. Std-only C, no libobs dependency: tests/drm_output_retry_1424.rs compiles it
 * for real and requires the Rust twin src/drm_output_retry.rs to print the same answers.
 *
 * Two decisions:
 *
 *   1. the supervisor (obs-drm-output-retry.c, one thread, two looks a second) per output:
 *        started && alive  -> LIVE, nothing to do;
 *        started && !alive -> RELEASE: its present loop died (a lost device, a display that now offers
 *                             another mode size, a wedged queue, or a lone display whose rebuild gave
 *                             up), so the output is stopped and goes back to the retry;
 *        !started          -> DORMANT: a whole start is retried on a bounded schedule -- 2 s for the
 *                             first DRM_OUTPUT_RETRY_FAST_TRIES failures, then 5 s, then 30 s from
 *                             DRM_OUTPUT_RETRY_SLOW_TRIES failures on (a screen connected but off: a
 *                             fresh Vulkan instance per try); 30 s while X RandR reads the connector
 *                             disconnected (nothing to drive), and AT ONCE when it reads connected
 *                             again (a monitor plugged in or switched on: the tracked `unplugged`).
 *   2. the present loop (obs-drm-output-vk.c) on an out-of-date / lost presentation (a monitor
 *      unplugged or switched off): it keeps the display ACQUIRED and rebuilds, 1 s apart for
 *      DRM_OUTPUT_REBUILD_FAST_TRIES tries, then every 5 s for as long as ANOTHER display of the process
 *      is open -- NVIDIA refuses to re-acquire a display this process released while the shared
 *      VkInstance lives on (issue 1424 STEP 0, live strih-pp), so a release there would be final.
 *      Alone it gives up after the fast tries: the release then tears the device down and the
 *      supervisor's retry opens it on a fresh instance, at whatever mode size the monitor offers.
 *
 * Every time is CLOCK_MONOTONIC nanoseconds (os_gettime_ns on Linux).
 */

#include <stdbool.h>
#include <stdint.h>

#define DRM_OUTPUT_RETRY_FAST_NS 2000000000ULL
#define DRM_OUTPUT_RETRY_SLOW_NS 5000000000ULL
#define DRM_OUTPUT_RETRY_FAST_TRIES 5U
#define DRM_OUTPUT_RETRY_SLOW_TRIES 20U
#define DRM_OUTPUT_RETRY_IDLE_NS 30000000000ULL
#define DRM_OUTPUT_RETRY_UNPLUGGED_NS 30000000000ULL

#define DRM_OUTPUT_REBUILD_FAST_NS 1000000000ULL
#define DRM_OUTPUT_REBUILD_SLOW_NS 5000000000ULL
#define DRM_OUTPUT_REBUILD_FAST_TRIES 10U

/* The supervisor's view of one output. */
#define DRM_OUTPUT_RETRY_LIVE 0
#define DRM_OUTPUT_RETRY_DORMANT 1

/* What the supervisor does after one look. */
#define DRM_OUTPUT_RETRY_NONE 0
#define DRM_OUTPUT_RETRY_START 1
#define DRM_OUTPUT_RETRY_RELEASE 2

/* The present loop's rebuild log lines. */
#define DRM_OUTPUT_REBUILD_LOG_NONE 0
#define DRM_OUTPUT_REBUILD_LOG_TRY 1  /* the per-try line of a fast try */
#define DRM_OUTPUT_REBUILD_LOG_SLOW 2 /* the ONE "retrying every 5 s" line, then quiet */

struct drm_output_retry {
	int state;        /* DRM_OUTPUT_RETRY_LIVE | _DORMANT; a zeroed struct = LIVE (nothing seen yet) */
	uint32_t fails;   /* failed starts since it went dormant */
	uint64_t next_ns; /* the next start (DORMANT) */
	bool unplugged;   /* the last look read the connector disconnected: a connected look tries at once */
};

/* The wait after `fails` failed starts: 30 s while the connector reads disconnected, else 2 s for the
 * first DRM_OUTPUT_RETRY_FAST_TRIES failures, 5 s up to DRM_OUTPUT_RETRY_SLOW_TRIES, then 30 s. */
static inline uint64_t drm_output_retry_delay_ns(uint32_t fails, bool connected)
{
	if (!connected)
		return DRM_OUTPUT_RETRY_UNPLUGGED_NS;
	if (fails < DRM_OUTPUT_RETRY_FAST_TRIES)
		return DRM_OUTPUT_RETRY_FAST_NS;
	return fails < DRM_OUTPUT_RETRY_SLOW_TRIES ? DRM_OUTPUT_RETRY_SLOW_NS : DRM_OUTPUT_RETRY_IDLE_NS;
}

/* One look at an output: `started` = the backend reports it open, `alive` = its present loop runs,
 * `connected` = X RandR does not read its connector disconnected (an unreadable X counts as connected:
 * try rather than wait). Returns DRM_OUTPUT_RETRY_NONE / _START / _RELEASE. */
static inline int drm_output_retry_step(struct drm_output_retry *r, bool started, bool alive, bool connected,
					uint64_t now_ns)
{
	if (started && alive) {
		r->state = DRM_OUTPUT_RETRY_LIVE;
		r->fails = 0;
		r->unplugged = false;
		return DRM_OUTPUT_RETRY_NONE;
	}
	if (started)
		return DRM_OUTPUT_RETRY_RELEASE;
	if (r->state == DRM_OUTPUT_RETRY_LIVE) {
		r->state = DRM_OUTPUT_RETRY_DORMANT;
		r->fails = 0;
		r->next_ns = now_ns + drm_output_retry_delay_ns(0U, connected);
		r->unplugged = !connected;
		return DRM_OUTPUT_RETRY_NONE;
	}
	/* a connector that comes back (plugged in, switched on) is tried now, not after any wait */
	if (!connected) {
		r->unplugged = true;
	} else if (r->unplugged) {
		r->unplugged = false;
		r->next_ns = now_ns;
	}
	return now_ns >= r->next_ns ? DRM_OUTPUT_RETRY_START : DRM_OUTPUT_RETRY_NONE;
}

/* The supervisor released a LIVE output whose present loop died: dormant, the first retry after the
 * first wait. */
static inline void drm_output_retry_released(struct drm_output_retry *r, bool connected, uint64_t now_ns)
{
	r->state = DRM_OUTPUT_RETRY_DORMANT;
	r->fails = 0;
	r->next_ns = now_ns + drm_output_retry_delay_ns(0U, connected);
	r->unplugged = !connected;
}

/* The result of a start the supervisor attempted at `now_ns`. */
static inline void drm_output_retry_attempted(struct drm_output_retry *r, bool ok, bool connected, uint64_t now_ns)
{
	if (ok) {
		r->state = DRM_OUTPUT_RETRY_LIVE;
		r->fails = 0;
		r->next_ns = 0;
		r->unplugged = false;
		return;
	}
	if (r->fails < UINT32_MAX)
		r->fails++;
	r->next_ns = now_ns + drm_output_retry_delay_ns(r->fails, connected);
	r->unplugged = !connected;
}

/* The present loop after `failed_tries` failed rebuilds (1 = the first failed): true + the wait before
 * the next try, or false = give up (release the display; the supervisor retries the whole open). */
static inline bool drm_output_rebuild_next(uint32_t failed_tries, bool others_open, uint64_t *wait_ns)
{
	if (failed_tries < DRM_OUTPUT_REBUILD_FAST_TRIES) {
		*wait_ns = DRM_OUTPUT_REBUILD_FAST_NS;
		return true;
	}
	if (others_open) {
		*wait_ns = DRM_OUTPUT_REBUILD_SLOW_NS;
		return true;
	}
	return false;
}

/* Which line the present loop logs for rebuild try number `tries` (1-based): the per-try line for the
 * fast tries, ONE line when it switches to the slow wait, then nothing until it is rebuilt. */
static inline int drm_output_rebuild_log_kind(uint32_t tries)
{
	if (tries >= 1U && tries <= DRM_OUTPUT_REBUILD_FAST_TRIES)
		return DRM_OUTPUT_REBUILD_LOG_TRY;
	if (tries == DRM_OUTPUT_REBUILD_FAST_TRIES + 1U)
		return DRM_OUTPUT_REBUILD_LOG_SLOW;
	return DRM_OUTPUT_REBUILD_LOG_NONE;
}
