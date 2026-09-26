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
#define VBAN_PACING_LOG_INTERVAL_NS 10000000000ULL
/* Wait for audio with this timeout while no deadline is scheduled (priming, after an underflow). */
#define VBAN_PACING_IDLE_WAIT_MS 10
#define VBAN_PACING_NS_PER_SEC 1000000000ULL

struct vban_pacing {
	uint32_t target_ms;
	uint64_t target_ns;
	uint64_t target_samples;
	uint64_t overflow_samples;
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

static inline void vban_pacing_init(struct vban_pacing *p, int64_t target_ms, uint32_t packet_samples,
				    uint32_t rate)
{
	const uint32_t t = vban_pacing_clamp_target_ms(target_ms);
	const uint32_t r = rate ? rate : 1;
	p->target_ms = t;
	p->target_ns = (uint64_t)t * 1000000ULL;
	p->target_samples = vban_pacing_ms_to_samples(t, r);
	p->overflow_samples = p->target_samples + vban_pacing_ms_to_samples(VBAN_PACING_OVERFLOW_HEADROOM_MS, r);
	p->packet_samples = packet_samples ? packet_samples : 1;
	p->rate = r;
	p->running = false;
	p->primed = false;
	p->prime_ns = 0;
	p->t0_ns = 0;
	p->n_sent = 0;
	p->underflows = 0;
	p->overflows = 0;
	p->late_max_ns = 0;
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
	}

	uint64_t deadline = vban_pacing_deadline_ns(p, p->n_sent);
	while (deadline <= now_ns) {
		if (avail < ps) {
			p->running = false;
			p->primed = false;
			p->underflows++;
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
