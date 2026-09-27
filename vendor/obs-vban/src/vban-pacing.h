/*
 * camera-box issues 1372 + 1381 -- the VBAN send pacing of the obs-vban output thread.
 *
 * obs-vban 0.3.1 sent at most ONE packet per wake, so the stream left in bursts and gaps (issue
 * 1372). The send thread keeps a jitter buffer of a configurable target depth (default 64 ms,
 * clamped 20-200 ms) and sends packet n at t0 + n * packet_duration on os_gettime_ns().
 *
 * Issue 1381 made the schedule a FIXED timeline (the model SongPlayer's own VBAN runs on): the
 * pacer never throws audio away because the OBS audio thread was late.
 *
 *  - Anchor: t0 = the moment a full packet is first buffered + the target, set once. Only an
 *    operator retarget moves it (up), and only the hard-ceiling resync skips slots of it.
 *  - Send: every packet whose slot is due goes out in this wake, audio from the buffer.
 *  - Late audio: a due slot whose audio is not buffered yet waits for it, up to
 *    VBAN_PACING_GRACE_MS past the slot. The audio then leaves late but COMPLETE (late_sends).
 *    The catch-up after it is capped at twice real time: the next packet leaves at the earliest
 *    half a packet duration after the previous one, until the schedule is met again. A thread
 *    that merely woke late (its audio was there) still sends every due packet at once.
 *  - Silence: a slot still without audio VBAN_PACING_GRACE_MS after its deadline is filled with
 *    a silence packet, and so is every following slot, on schedule, until the buffer holds the
 *    target again. One silence episode is ONE counted discontinuity.
 *  - Stale repay: each silence packet stands in for a packet of audio still to come. When the
 *    pacer is back on schedule and the buffer holds target + that debt (the stall's backlog has
 *    arrived), the debt is dropped in ONE drop of whole packets and the latency is the anchored
 *    one again. Until then the stalled audio plays late, so the drop is a forward skip, its own
 *    audible splice: repays counts it. The caller advances nuFrame across every dropped packet, so
 *    the receiver's own loss counter sees it. A buffering hole in OBS brings no backlog: its debt
 *    is never repaid (the silence IS the hole) and it is forgiven when the next silence episode
 *    starts.
 *  - Hard ceiling: more than VBAN_PACING_CEILING_MS buffered, or the next slot more than
 *    VBAN_PACING_CEILING_MS overdue (a send thread frozen for seconds, a host that slept), is one
 *    counted resync back to the target, continuing at the first slot at or after now instead of
 *    chasing an old grid at twice real time.
 *  - Retarget: a new target while running moves the schedule later (up) or drops the
 *    difference at the next wake, never below the new target (down, a counted discontinuity).
 *
 * There is no trim and no overflow drop any more.
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
/* How long past its deadline a slot waits for late audio before it is filled with silence. */
#define VBAN_PACING_GRACE_MS 100ULL
/* Buffered audio above this, or a next slot more than this overdue, is a counted resync. */
#define VBAN_PACING_CEILING_MS 2000ULL
#define VBAN_PACING_LOG_INTERVAL_NS 10000000000ULL
/* The longest single wait on the audio event. */
#define VBAN_PACING_IDLE_WAIT_MS 10U
#define VBAN_PACING_NS_PER_SEC 1000000000ULL
#define VBAN_PACING_NS_PER_MS 1000000ULL

struct vban_pacing {
	uint32_t target_ms;
	uint64_t target_ns;
	uint64_t target_samples;
	uint64_t grace_ns;
	uint64_t ceiling_samples;
	uint64_t ceiling_ns;
	uint64_t half_packet_ns; /* the catch-up spacing: twice real time */
	uint32_t packet_samples;
	uint32_t rate;
	bool running;
	bool primed;
	uint64_t prime_ns;
	uint64_t t0_ns;
	uint64_t n_sent;     /* slots used so far, audio and silence */
	uint64_t catchup_ns; /* the earliest instant the next packet may leave; 0 = no cap */
	bool starved;        /* the next slot is due and waiting for its audio */
	bool silent;         /* in a silence episode */
	uint64_t stale_samples;        /* silence the audio still owes (the episode's debt) */
	bool repay_ready;              /* the debt is dropped at the start of the next wake */
	uint64_t pending_drop_samples; /* dropped at the next wake after a lower target */
	uint64_t late_sends;           /* audio packets that left late because the audio came late */
	uint64_t discontinuities;      /* silence episodes, resyncs and retarget drops */
	uint64_t repays;               /* stale-debt drops after the audio resumed (forward skips) */
	uint64_t silence_samples;
	uint64_t discarded_samples;
	uint64_t resyncs;
	uint64_t late_max_ns;
};

/* One wake's decision, applied in this order: drop, audio packets, silence packets. */
struct vban_pacing_step {
	uint32_t send;         /* audio packets of packet_samples from the head of the buffer */
	uint32_t silence;      /* silence packets after the audio ones */
	uint64_t drop_samples; /* oldest samples to drop BEFORE sending, always whole packets; nuFrame
	                        * advances by drop_samples / packet_samples */
	uint64_t wake_ns;      /* the next deadline; 0 = none (wait for audio) */
	bool wait_audio;       /* wait on the audio event (at most until wake_ns, see
	                        * vban_pacing_wait_ms) instead of sleeping to wake_ns */
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

/* floor(ns * rate / 1e9) without overflowing 64 bits for any realistic span. */
static inline uint64_t vban_pacing_ns_to_samples(uint64_t ns, uint32_t rate)
{
	const uint64_t r = rate ? (uint64_t)rate : 1;
	return (ns / VBAN_PACING_NS_PER_SEC) * r + (ns % VBAN_PACING_NS_PER_SEC) * r / VBAN_PACING_NS_PER_SEC;
}

/* floor(ms * rate / 1000). */
static inline uint64_t vban_pacing_ms_to_samples(uint64_t ms, uint32_t rate)
{
	const uint64_t r = rate ? (uint64_t)rate : 1;
	return ms * r / 1000;
}

/* The audio-event wait (whole ms) for a step that asked to wait for audio: until wake_ns rounded
 * UP to the next ms (0 when it has passed), never longer than VBAN_PACING_IDLE_WAIT_MS, and
 * VBAN_PACING_IDLE_WAIT_MS when there is no deadline (wake_ns 0). */
static inline uint32_t vban_pacing_wait_ms(uint64_t now_ns, uint64_t wake_ns)
{
	if (wake_ns == 0)
		return VBAN_PACING_IDLE_WAIT_MS;
	if (wake_ns <= now_ns)
		return 0;
	const uint64_t ms = (wake_ns - now_ns + VBAN_PACING_NS_PER_MS - 1) / VBAN_PACING_NS_PER_MS;
	return ms < VBAN_PACING_IDLE_WAIT_MS ? (uint32_t)ms : VBAN_PACING_IDLE_WAIT_MS;
}

static inline void vban_pacing_set_target(struct vban_pacing *p, uint32_t target_ms)
{
	p->target_ms = target_ms;
	p->target_ns = (uint64_t)target_ms * VBAN_PACING_NS_PER_MS;
	p->target_samples = vban_pacing_ms_to_samples(target_ms, p->rate);
}

static inline void vban_pacing_init(struct vban_pacing *p, int64_t target_ms, uint32_t packet_samples,
				    uint32_t rate)
{
	p->packet_samples = packet_samples ? packet_samples : 1;
	p->rate = rate ? rate : 1;
	p->grace_ns = VBAN_PACING_GRACE_MS * VBAN_PACING_NS_PER_MS;
	p->ceiling_samples = vban_pacing_ms_to_samples(VBAN_PACING_CEILING_MS, p->rate);
	p->ceiling_ns = VBAN_PACING_CEILING_MS * VBAN_PACING_NS_PER_MS;
	p->half_packet_ns = vban_pacing_samples_to_ns(p->packet_samples, p->rate) / 2;
	p->running = false;
	p->primed = false;
	p->prime_ns = 0;
	p->t0_ns = 0;
	p->n_sent = 0;
	p->catchup_ns = 0;
	p->starved = false;
	p->silent = false;
	p->stale_samples = 0;
	p->repay_ready = false;
	p->pending_drop_samples = 0;
	p->late_sends = 0;
	p->discontinuities = 0;
	p->repays = 0;
	p->silence_samples = 0;
	p->discarded_samples = 0;
	p->resyncs = 0;
	p->late_max_ns = 0;
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
			p->t0_ns += (uint64_t)(new_ms - old_ms) * VBAN_PACING_NS_PER_MS;
		else
			p->pending_drop_samples += vban_pacing_ms_to_samples(old_ms - new_ms, p->rate);
	}
}

/* The deadline of slot n of the schedule. */
static inline uint64_t vban_pacing_deadline_ns(const struct vban_pacing *p, uint64_t n)
{
	return p->t0_ns + vban_pacing_samples_to_ns(n * (uint64_t)p->packet_samples, p->rate);
}

/* The buffer depth at which a silence episode ends. */
static inline uint64_t vban_pacing_resume_samples(const struct vban_pacing *p)
{
	return p->target_samples > p->packet_samples ? p->target_samples : (uint64_t)p->packet_samples;
}

/* The first slot at or after now_ns (never before the next unsent one). */
static inline uint64_t vban_pacing_first_slot_at_or_after(const struct vban_pacing *p, uint64_t now_ns)
{
	uint64_t k = 0;
	if (now_ns > p->t0_ns)
		k = vban_pacing_ns_to_samples(now_ns - p->t0_ns, p->rate) / p->packet_samples;
	if (k < p->n_sent)
		k = p->n_sent;
	while (vban_pacing_deadline_ns(p, k) < now_ns)
		k++;
	return k;
}

/* After a packet that virtually left at v: the next one may leave half a packet later, until that
 * is no later than its own deadline (caught up). */
static inline void vban_pacing_catch_up_from(struct vban_pacing *p, uint64_t v)
{
	const uint64_t next = v + p->half_packet_ns;
	p->catchup_ns = next > vban_pacing_deadline_ns(p, p->n_sent) ? next : 0;
}

/* The decision for a wake at now_ns with buffered samples waiting. */
static inline struct vban_pacing_step vban_pacing_step(struct vban_pacing *p, uint64_t now_ns, uint64_t buffered)
{
	const uint64_t ps = p->packet_samples;
	struct vban_pacing_step d = {0, 0, 0, 0, false};
	uint64_t avail = buffered;

	if (p->running &&
	    (avail > p->ceiling_samples || now_ns > vban_pacing_deadline_ns(p, p->n_sent) + p->ceiling_ns)) {
		/* the hard ceiling: one counted resync back to the target, on the grid */
		const uint64_t excess = avail > p->target_samples ? avail - p->target_samples : 0;
		const uint64_t t = excess - excess % ps;
		avail -= t;
		d.drop_samples = t;
		p->discarded_samples += t;
		p->resyncs++;
		p->discontinuities++;
		p->n_sent = vban_pacing_first_slot_at_or_after(p, now_ns);
		p->catchup_ns = 0;
		p->starved = false;
		p->silent = false;
		p->stale_samples = 0;
		p->repay_ready = false;
		p->pending_drop_samples = 0;
	} else if (!p->running) {
		if (!p->primed) {
			if (avail < ps) {
				d.wait_audio = true;
				return d;
			}
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
		p->catchup_ns = 0;
		p->starved = false;
		p->silent = false;
		p->stale_samples = 0;
		p->repay_ready = false;
	} else {
		if (p->repay_ready) {
			/* the backlog is here: the debt goes in one drop of whole packets */
			const uint64_t whole = avail - avail % ps;
			const uint64_t t = p->stale_samples < whole ? p->stale_samples : whole;
			avail -= t;
			d.drop_samples += t;
			p->discarded_samples += t;
			p->stale_samples -= t;
			p->repay_ready = false;
			if (t > 0)
				p->repays++;
		}
		if (p->pending_drop_samples > 0) {
			/* never below the new target: a dip takes only what is above it */
			const uint64_t above = avail > p->target_samples ? avail - p->target_samples : 0;
			uint64_t t = p->pending_drop_samples < above ? p->pending_drop_samples : above;
			t -= t % ps;
			avail -= t;
			d.drop_samples += t;
			p->pending_drop_samples = 0;
			if (t > 0) {
				p->discarded_samples += t;
				p->discontinuities++;
			}
		}
	}

	const uint64_t resume = vban_pacing_resume_samples(p);
	for (;;) {
		const uint64_t deadline = vban_pacing_deadline_ns(p, p->n_sent);
		const uint64_t eligible = p->catchup_ns > deadline ? p->catchup_ns : deadline;
		if (eligible > now_ns) {
			d.wake_ns = eligible;
			break;
		}
		if (avail >= ps && (!p->silent || avail >= resume)) {
			/* waited for its audio and really left after its (possibly moved) deadline */
			const bool late = (p->starved && now_ns > deadline) || p->catchup_ns > deadline;
			const uint64_t v = p->starved ? now_ns : eligible;
			p->starved = false;
			p->silent = false;
			if (late)
				p->late_sends++;
			if (now_ns - deadline > p->late_max_ns)
				p->late_max_ns = now_ns - deadline;
			avail -= ps;
			d.send++;
			p->n_sent++;
			vban_pacing_catch_up_from(p, v);
		} else if (p->silent || now_ns - deadline >= p->grace_ns) {
			const uint64_t v = p->silent ? eligible : now_ns;
			if (!p->silent) {
				/* a new episode; an earlier debt never got its backlog and is forgiven */
				p->silent = true;
				p->starved = false;
				p->discontinuities++;
				p->stale_samples = 0;
				p->repay_ready = false;
			}
			/* a silence slot is late by design; late_max is the lateness of AUDIO packets */
			p->silence_samples += ps;
			p->stale_samples += ps;
			d.silence++;
			p->n_sent++;
			vban_pacing_catch_up_from(p, v);
		} else {
			p->starved = true;
			d.wake_ns = deadline + p->grace_ns;
			d.wait_audio = true;
			break;
		}
	}

	if (!p->starved && !p->silent && p->catchup_ns == 0 && p->stale_samples > 0 &&
	    avail >= p->target_samples + p->stale_samples)
		p->repay_ready = true;
	return d;
}

/* The largest lateness since the last call, then reset (the 10 s log window). */
static inline uint64_t vban_pacing_take_late_max_ns(struct vban_pacing *p)
{
	const uint64_t v = p->late_max_ns;
	p->late_max_ns = 0;
	return v;
}
