/*
 * camera-box issue 1372 -- the VBAN send pacing of the obs-vban output thread.
 *
 * obs-vban 0.3.1 sent at most ONE packet per wake and woke on the audio callback or after a
 * truncated 4 ms timeout. On a box whose OBS audio callback takes 14-28 ms of each 21.3 ms tick
 * and stalls now and then, the stream left in bursts and gaps and a VBAN receiver underran.
 *
 * The send thread now keeps a jitter buffer of a configurable target depth (default 64 ms,
 * clamped 20-200 ms) and sends packet n at t0 + n * packet_duration on os_gettime_ns():
 *
 *  - Anchor: t0 = the moment a full packet is first buffered + the target.
 *  - Send: every packet whose deadline is at or before now goes out in this wake.
 *  - Underflow: a due packet with less than a packet buffered stops the schedule; nothing is sent
 *    or fabricated (no zero-fill), underflows counts it, and the next full packet re-anchors.
 *  - Overflow: more than target + 200 ms buffered drops the oldest whole packets down to the
 *    target, and overflows counts it.
 *  - Trim: a re-anchor after an underflow lands on top of the audio thread's catch-up backlog, so
 *    the depth would stay at target + backlog for good. While running, the minimum depth after
 *    each wake is tracked over a 2 s window; when it stayed above target + max(target / 2, 20 ms)
 *    the excess is dropped back to the target (whole packets) and trims counts it.
 *  - Retarget: a new target while running moves the schedule later (up) or drops the difference
 *    at the next wake (down, counted as a trim); the counters carry on.
 *
 * Pure: no OBS dependency. The Tier-0 authority is src/vban_pacing.rs in camera-box, and
 * tests/vban_pacing_parity_1372.rs compiles THIS header and requires identical decisions.
 */

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define VBAN_PACING_TARGET_MS_DEFAULT 64
#define VBAN_PACING_TARGET_MS_MIN 20
#define VBAN_PACING_TARGET_MS_MAX 200
#define VBAN_PACING_OVERFLOW_HEADROOM_MS 200ULL
#define VBAN_PACING_TRIM_WINDOW_NS 2000000000ULL
#define VBAN_PACING_TRIM_HYSTERESIS_MIN_MS 20ULL
#define VBAN_PACING_LOG_INTERVAL_NS 10000000000ULL
/* Wait for audio with this timeout while no deadline is scheduled (priming, after an underflow). */
#define VBAN_PACING_IDLE_WAIT_MS 10
#define VBAN_PACING_NS_PER_SEC 1000000000ULL

struct vban_pacing {
	uint32_t target_ms;
	uint64_t target_ns;
	uint64_t target_samples;
	uint64_t overflow_samples;
	uint64_t trim_threshold_samples; /* a running window whose minimum stays above is trimmed */
	uint32_t packet_samples;
	uint32_t rate;
	bool running;
	bool primed;
	uint64_t prime_ns;
	uint64_t t0_ns;
	uint64_t n_sent;
	uint64_t underflows;
	uint64_t overflows;
	uint64_t late_max_ns;
	uint64_t trims; /* sustained-excess and retarget drops */
	bool win_open;
	uint64_t win_start_ns;
	uint64_t win_min_samples;
	uint64_t pending_trim_samples; /* dropped at the very next wake after a lower target */
};

struct vban_pacing_step {
	uint32_t send;         /* packets of packet_samples to send now, oldest first */
	uint64_t drop_samples; /* oldest samples to drop BEFORE sending (whole packets) */
	uint64_t wake_ns;      /* the next deadline to sleep to; 0 = wait for audio */
};

/* The target depth the thread uses for an output setting value. */
static inline uint32_t vban_pacing_clamp_target_ms(int64_t ms)
{
	if (ms <= 0)
		return VBAN_PACING_TARGET_MS_DEFAULT;
	if (ms < VBAN_PACING_TARGET_MS_MIN)
		return VBAN_PACING_TARGET_MS_MIN;
	if (ms > VBAN_PACING_TARGET_MS_MAX)
		return VBAN_PACING_TARGET_MS_MAX;
	return (uint32_t)ms;
}

/* floor(samples * 1e9 / rate) without overflowing 64 bits for any realistic sample count. */
static inline uint64_t vban_pacing_samples_to_ns(uint64_t samples, uint32_t rate)
{
	const uint64_t r = rate ? (uint64_t)rate : 1;
	return (samples / r) * VBAN_PACING_NS_PER_SEC + (samples % r) * VBAN_PACING_NS_PER_SEC / r;
}

/* floor(ms * rate / 1000). */
static inline uint64_t vban_pacing_ms_to_samples(uint64_t ms, uint32_t rate)
{
	const uint64_t r = rate ? (uint64_t)rate : 1;
	return ms * r / 1000;
}

static inline void vban_pacing_set_target(struct vban_pacing *p, uint32_t target_ms)
{
	uint64_t hysteresis_ms = (uint64_t)target_ms / 2;
	if (hysteresis_ms < VBAN_PACING_TRIM_HYSTERESIS_MIN_MS)
		hysteresis_ms = VBAN_PACING_TRIM_HYSTERESIS_MIN_MS;
	p->target_ms = target_ms;
	p->target_ns = (uint64_t)target_ms * 1000000ULL;
	p->target_samples = vban_pacing_ms_to_samples(target_ms, p->rate);
	p->overflow_samples = p->target_samples + vban_pacing_ms_to_samples(VBAN_PACING_OVERFLOW_HEADROOM_MS, p->rate);
	p->trim_threshold_samples = p->target_samples + vban_pacing_ms_to_samples(hysteresis_ms, p->rate);
}

static inline void vban_pacing_init(struct vban_pacing *p, int64_t target_ms, uint32_t packet_samples,
				    uint32_t rate)
{
	p->packet_samples = packet_samples ? packet_samples : 1;
	p->rate = rate ? rate : 1;
	p->running = false;
	p->primed = false;
	p->prime_ns = 0;
	p->t0_ns = 0;
	p->n_sent = 0;
	p->underflows = 0;
	p->overflows = 0;
	p->late_max_ns = 0;
	p->trims = 0;
	p->win_open = false;
	p->win_start_ns = 0;
	p->win_min_samples = 0;
	p->pending_trim_samples = 0;
	vban_pacing_set_target(p, vban_pacing_clamp_target_ms(target_ms));
}

/* A new target (an output setting value) while the output exists. Running: a higher target moves
 * the schedule later by the difference, a lower one drops the difference at the next wake. Not
 * running: the anchor simply uses the new target. The counters carry on. */
static inline void vban_pacing_retarget(struct vban_pacing *p, int64_t target_ms)
{
	const uint32_t new_ms = vban_pacing_clamp_target_ms(target_ms);
	if (new_ms == p->target_ms)
		return;
	const uint32_t old_ms = p->target_ms;
	vban_pacing_set_target(p, new_ms);
	if (p->running) {
		if (new_ms > old_ms)
			p->t0_ns += (uint64_t)(new_ms - old_ms) * 1000000ULL;
		else
			p->pending_trim_samples += vban_pacing_ms_to_samples(old_ms - new_ms, p->rate);
	}
	p->win_open = false;
}

/* The deadline of packet n of the current schedule. */
static inline uint64_t vban_pacing_deadline_ns(const struct vban_pacing *p, uint64_t n)
{
	return p->t0_ns + vban_pacing_samples_to_ns(n * (uint64_t)p->packet_samples, p->rate);
}

/* The decision for a wake at now_ns with buffered samples waiting. */
static inline struct vban_pacing_step vban_pacing_step(struct vban_pacing *p, uint64_t now_ns, uint64_t buffered)
{
	const uint64_t ps = p->packet_samples;
	struct vban_pacing_step d = {0, 0, 0};
	uint64_t avail = buffered;

	if (avail > p->overflow_samples) {
		const uint64_t excess = avail - p->target_samples;
		d.drop_samples = excess - excess % ps;
		avail -= d.drop_samples;
		p->overflows++;
		p->win_open = false;
	}

	if (!p->running) {
		if (!p->primed) {
			if (avail < ps)
				return d;
			p->primed = true;
			p->prime_ns = now_ns;
		}
		const uint64_t start = p->prime_ns + p->target_ns;
		if (now_ns < start) {
			d.wake_ns = start;
			return d;
		}
		p->running = true;
		p->t0_ns = start;
		p->n_sent = 0;
		p->win_open = false;
	} else if (p->pending_trim_samples > 0) {
		uint64_t t = p->pending_trim_samples < avail ? p->pending_trim_samples : avail;
		t -= t % ps;
		avail -= t;
		d.drop_samples += t;
		p->pending_trim_samples = 0;
		if (t > 0)
			p->trims++;
		p->win_open = false;
	} else if (p->win_open && now_ns >= p->win_start_ns &&
		   now_ns - p->win_start_ns >= VBAN_PACING_TRIM_WINDOW_NS) {
		if (p->win_min_samples > p->trim_threshold_samples) {
			const uint64_t excess = p->win_min_samples - p->target_samples;
			const uint64_t t = excess - excess % ps;
			avail -= t;
			d.drop_samples += t;
			p->trims++;
		}
		p->win_open = false;
	}

	uint64_t deadline = vban_pacing_deadline_ns(p, p->n_sent);
	while (deadline <= now_ns) {
		if (avail < ps) {
			p->running = false;
			p->primed = false;
			p->underflows++;
			p->win_open = false;
			d.wake_ns = 0;
			return d;
		}
		if (now_ns - deadline > p->late_max_ns)
			p->late_max_ns = now_ns - deadline;
		avail -= ps;
		d.send++;
		p->n_sent++;
		deadline = vban_pacing_deadline_ns(p, p->n_sent);
	}

	if (p->win_open) {
		if (avail < p->win_min_samples)
			p->win_min_samples = avail;
	} else {
		p->win_open = true;
		p->win_start_ns = now_ns;
		p->win_min_samples = avail;
	}
	d.wake_ns = deadline;
	return d;
}

/* The largest lateness since the last call, then reset (the 10 s log window). */
static inline uint64_t vban_pacing_take_late_max_ns(struct vban_pacing *p)
{
	const uint64_t v = p->late_max_ns;
	p->late_max_ns = 0;
	return v;
}
