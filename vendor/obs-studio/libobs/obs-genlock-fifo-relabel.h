/* camera-box issue 1372 part B (design 6026394143): the genlock receive FIFO relabels its OLD-EPOCH
 * frames by the booked wall step, so the nightly dantesync fleet date step costs no frame.
 *
 * A genlock source's frames carry the sender's wall clock as their stamp and the FIFO releases them
 * against this box's wall clock. A coordinated fleet DATE step moves every wall clock by the same S;
 * the render tick re-grids in one tick (obs-genlock-wall-step.h), but the queued frames -- and the ones
 * a sender stamped just before ITS step -- keep the old label and read S off the release target (live
 * 6./7.10.2026, +1543 / +1549 ms: stream 'NDI 2ME PGM' late_holds +1 / dropped_due +1, 'Zaloha kamera'
 * late_holds +28 / +29, 5 repeats + 4 skips on the program recording). Now:
 *
 * - genlock_fifo_relabel_book() runs the render tick's detector (genlock_wall_step_observe) on the
 *   bracketed read the release takes its wall_now from, and books the step box-wide (render thread)
 *   BEFORE the release -- the render tick's own detector runs at the end of the tick, after it;
 * - genlock_fifo_relabel_apply() (once per source per booking, at its release, under async_mutex)
 *   relabels the old-epoch queued frames by +S: in FIFO order the prefix before the first stamp jump
 *   that carries S within one frame; with none, the epoch of the last presented frame (old unless a
 *   jump that carries S arrived within one latency window before the step: the sender stepped
 *   first). The locked boundary and the stamp tracker's last stamp move with their frames, and the
 *   arrival window opens;
 * - genlock_fifo_relabel_receive() (every push, under async_mutex, before the stamp tracker) relabels
 *   an arriving frame whose stamp + S continues the relabelled timeline within one frame and whose
 *   raw stamp does not; a raw stamp that continues it (the sender stepped) or neither (a sender
 *   restart, a song change: never relabelled) ends the window, and so does a relabelled stamp past
 *   the step instant + one latency window (the presented age, at least the pin);
 * - only a step of GENLOCK_FIFO_RELABEL_MIN_STEP_NS or more is relabelled.
 *
 * Pure: stdint/stdbool/stddef + the wall-step header, no libobs types; the queue is reached through
 * the two callbacks of struct genlock_fifo_relabel_queue. tests/genlock_fifo_relabel_parity_1372.rs
 * compiles this header as-is and requires byte-identical results from the Tier-0 Rust authority
 * src/genlock_fifo_relabel.rs. Keep both in lock-step. */
#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "obs-genlock-wall-step.h"

/* The smallest wall step the FIFO relabels, ns: below two canvas frames a continuous stamp and a
 * stepped one cannot be told apart within one frame; half the dantesync daily quantum (200 ms).
 * Mirror of src/genlock_fifo_relabel.rs MIN_STEP_NS. */
#define GENLOCK_FIFO_RELABEL_MIN_STEP_NS 100000000LL
/* A raw stamp delta more than this off the source's own step is remembered as a stamp jump, ns.
 * Mirror of JUMP_RECORD_DEV_NS. */
#define GENLOCK_FIFO_RELABEL_JUMP_RECORD_DEV_NS 50000000ULL
/* A trusted booking read more than this after the previous trusted one re-seeds the booking
 * detector instead of booking, ns. Mirror of BOOK_MAX_GAP_NS. */
#define GENLOCK_FIFO_RELABEL_BOOK_MAX_GAP_NS 1000000000ULL
/* A source whose previous release is more than this before the current one takes the booking
 * without relabelling, ns. Mirror of APPLY_MAX_GAP_NS. */
#define GENLOCK_FIFO_RELABEL_APPLY_MAX_GAP_NS 1000000000ULL
/* The presented age a window takes is capped here, ns. Mirror of WINDOW_MAX_AGE_NS. */
#define GENLOCK_FIFO_RELABEL_WINDOW_MAX_AGE_NS 2000000000ULL

/* The box-wide booking (render thread only). Mirror of src/genlock_fifo_relabel.rs Booking. */
struct genlock_fifo_relabel_booking {
	struct genlock_wall_step_state detector;
	uint64_t seq;
	int64_t step_ns;
	uint64_t wall_ns;
	uint64_t mono_ns;
	uint64_t last_mono_ns;
};

/* Which frames are old-epoch at a booking. Mirror of Plan. */
struct genlock_fifo_relabel_plan {
	size_t queue_old;
	bool prev_old;
	bool newest_old;
};

/* The arrival window a booking opens on one source. Mirror of Arrival. */
struct genlock_fifo_relabel_arrival {
	int64_t step_ns;
	uint64_t until_ns;
	uint64_t frame_ns;
	bool old_epoch;
};

/* The per-source state (in obs_source, under async_mutex). Mirror of RelabelState. */
struct genlock_fifo_relabel_state {
	uint64_t seq;
	struct genlock_fifo_relabel_arrival arrival;
	int64_t jump_ns;
	uint64_t jump_mono_ns;
	uint64_t relabelled;
	uint64_t last_release_mono_ns;
};

/* A source's queue, FIFO order: n stamps read and written through the callbacks. */
struct genlock_fifo_relabel_queue {
	void *ctx;
	size_t n;
	uint64_t (*get)(const void *ctx, size_t i);
	void (*set)(void *ctx, size_t i, uint64_t ts);
};

/* |delta - src_ns|, wrapping like int64_t. Mirror of dev_ns. */
static inline uint64_t genlock_fifo_relabel_dev_ns(int64_t delta_ns, uint64_t src_ns)
{
	const int64_t d = (int64_t)((uint64_t)delta_ns - src_ns);
	return d >= 0 ? (uint64_t)d : 0ULL - (uint64_t)d;
}

/* The delta continues a timeline: within one frame of the source's own step. Mirror of continuous. */
static inline bool genlock_fifo_relabel_continuous(int64_t delta_ns, uint64_t src_ns, uint64_t frame_ns)
{
	return genlock_fifo_relabel_dev_ns(delta_ns, src_ns) <= frame_ns;
}

/* The delta carries the booked step: delta - step continues within one frame, closer than the delta
 * itself. Mirror of delta_carries_step. */
static inline bool genlock_fifo_relabel_delta_carries_step(int64_t delta_ns, int64_t step_ns, uint64_t src_ns,
							   uint64_t frame_ns)
{
	const uint64_t off = genlock_fifo_relabel_dev_ns((int64_t)((uint64_t)delta_ns - (uint64_t)step_ns), src_ns);
	return off <= frame_ns && off < genlock_fifo_relabel_dev_ns(delta_ns, src_ns);
}

/* |step| >= GENLOCK_FIFO_RELABEL_MIN_STEP_NS. Mirror of step_relabels. */
static inline bool genlock_fifo_relabel_step_relabels(int64_t step_ns)
{
	const uint64_t mag = step_ns >= 0 ? (uint64_t)step_ns : 0ULL - (uint64_t)step_ns;
	return mag >= (uint64_t)GENLOCK_FIFO_RELABEL_MIN_STEP_NS;
}

/* A raw received delta remembered as a stamp jump. Mirror of jump_recorded. */
static inline bool genlock_fifo_relabel_jump_recorded(int64_t delta_ns, uint64_t src_ns)
{
	return genlock_fifo_relabel_dev_ns(delta_ns, src_ns) > GENLOCK_FIFO_RELABEL_JUMP_RECORD_DEV_NS;
}

/* The sender's step reached this source before the booking: a remembered jump carries the step and
 * arrived at most one window before it (monotonic). Mirror of sender_stepped_before. */
static inline bool genlock_fifo_relabel_sender_stepped_before(int64_t jump_ns, uint64_t jump_mono_ns,
							      int64_t step_ns, uint64_t src_ns,
							      uint64_t frame_ns, uint64_t booking_mono_ns,
							      uint64_t window_ns)
{
	const uint64_t reach = jump_mono_ns > UINT64_MAX - window_ns ? UINT64_MAX : jump_mono_ns + window_ns;
	return jump_ns != 0 && genlock_fifo_relabel_delta_carries_step(jump_ns, step_ns, src_ns, frame_ns) &&
	       booking_mono_ns <= reach;
}

/* One latency window: the presented age, at least the configured latency. Mirror of window_ns. */
static inline uint64_t genlock_fifo_relabel_window_ns(uint64_t reserve_ns, uint64_t presented_age_ns)
{
	return reserve_ns > presented_age_ns ? reserve_ns : presented_age_ns;
}

/* Feed the release's bracketed read; true = a new step was booked. Mirror of Booking::observe. */
static inline bool genlock_fifo_relabel_book(struct genlock_fifo_relabel_booking *b, uint64_t mono_before,
					     uint64_t wall, uint64_t mono_after)
{
	const int64_t step = genlock_wall_step_observe(&b->detector, mono_before, wall, mono_after);
	if (step == 0)
		return false;
	b->seq++;
	b->step_ns = step;
	b->wall_ns = wall;
	b->mono_ns = mono_after;
	return true;
}

/* Split the sequence (prev = the last presented stamp, 0 = none; then the queue) at the booked step.
 * Mirror of plan. */
static inline struct genlock_fifo_relabel_plan genlock_fifo_relabel_plan(uint64_t prev,
									const struct genlock_fifo_relabel_queue *q,
									int64_t step_ns, uint64_t src_ns,
									uint64_t frame_ns, bool stepped_before)
{
	struct genlock_fifo_relabel_plan p = {0, false, false};
	uint64_t pred = prev;
	for (size_t k = 0; k < q->n; k++) {
		const uint64_t ts = q->get(q->ctx, k);
		if (pred != 0 &&
		    genlock_fifo_relabel_delta_carries_step((int64_t)(ts - pred), step_ns, src_ns, frame_ns)) {
			p.queue_old = k;
			p.prev_old = prev != 0;
			return p;
		}
		pred = ts;
	}
	if (stepped_before)
		return p;
	p.queue_old = q->n;
	p.prev_old = prev != 0;
	p.newest_old = true;
	return p;
}

/* What to add to an arriving stamp (0 or the step); ends the window per the rules above. Mirror of
 * arrival_add. */
static inline int64_t genlock_fifo_relabel_arrival_add(struct genlock_fifo_relabel_arrival *a, uint64_t prev,
						       uint64_t stamp, uint64_t src_ns)
{
	if (!a->old_epoch || a->step_ns == 0 || prev == 0)
		return 0;
	const uint64_t relabelled = stamp + (uint64_t)a->step_ns;
	if (relabelled > a->until_ns) {
		a->old_epoch = false;
		return 0;
	}
	const uint64_t raw = genlock_fifo_relabel_dev_ns((int64_t)(stamp - prev), src_ns);
	const uint64_t rel = genlock_fifo_relabel_dev_ns((int64_t)(relabelled - prev), src_ns);
	if (raw <= a->frame_ns && raw <= rel) {
		a->old_epoch = false;
		return 0;
	}
	if (rel <= a->frame_ns)
		return a->step_ns;
	a->old_epoch = false;
	return 0;
}

/* Apply a booking this source has not applied yet (see the header comment). *locked_boundary: 0 =
 * unlocked; the presented stamp is boundary - interval. *rx_last: 0 = none. Returns true with *plan_out
 * set when it relabelled or opened a window, false when the booking was applied already or does not
 * relabel. Mirror of RelabelState::apply. */
static inline bool genlock_fifo_relabel_apply(struct genlock_fifo_relabel_state *s,
					      const struct genlock_fifo_relabel_booking *b,
					      const struct genlock_fifo_relabel_queue *q, uint64_t *locked_boundary,
					      uint64_t *rx_last, uint64_t interval_ns, uint64_t src_ns,
					      uint64_t reserve_ns, uint64_t wall_now, uint64_t mono_now,
					      struct genlock_fifo_relabel_plan *plan_out)
{
	s->last_release_mono_ns = mono_now;
	if (s->seq == b->seq)
		return false;
	s->seq = b->seq;
	const struct genlock_fifo_relabel_arrival closed = {0, 0, 0, false};
	s->arrival = closed;
	const int64_t step = b->step_ns;
	if (!genlock_fifo_relabel_step_relabels(step) || interval_ns == 0)
		return false;
	const uint64_t src = src_ns != 0 ? src_ns : interval_ns;
	const uint64_t prev = *locked_boundary > interval_ns ? *locked_boundary - interval_ns : 0;
	const uint64_t prev_new = prev + (uint64_t)step;
	const uint64_t age = prev != 0 && wall_now > prev_new ? wall_now - prev_new : 0;
	const uint64_t window = genlock_fifo_relabel_window_ns(reserve_ns, age);
	const bool stepped_before = genlock_fifo_relabel_sender_stepped_before(s->jump_ns, s->jump_mono_ns, step, src,
									       interval_ns, b->mono_ns, window);
	const struct genlock_fifo_relabel_plan p =
		genlock_fifo_relabel_plan(prev, q, step, src, interval_ns, stepped_before);
	for (size_t k = 0; k < p.queue_old; k++)
		q->set(q->ctx, k, q->get(q->ctx, k) + (uint64_t)step);
	if (p.prev_old)
		*locked_boundary += (uint64_t)step;
	if (p.newest_old && *rx_last != 0)
		*rx_last += (uint64_t)step;
	s->relabelled += (uint64_t)p.queue_old;
	s->arrival.step_ns = step;
	s->arrival.until_ns = b->wall_ns > UINT64_MAX - window ? UINT64_MAX : b->wall_ns + window;
	s->arrival.frame_ns = interval_ns;
	s->arrival.old_epoch = p.newest_old;
	s->jump_ns = 0;
	*plan_out = p;
	return true;
}

/* One received frame: the stamp to queue (relabelled while the window judges it so); remembers a raw
 * stamp jump it did not relabel. Mirror of RelabelState::receive. */
static inline uint64_t genlock_fifo_relabel_receive(struct genlock_fifo_relabel_state *s, uint64_t rx_last,
						    uint64_t stamp, uint64_t src_ns, uint64_t mono_now)
{
	const uint64_t src = src_ns != 0 ? src_ns : s->arrival.frame_ns;
	const int64_t add = genlock_fifo_relabel_arrival_add(&s->arrival, rx_last, stamp, src);
	if (add != 0) {
		s->relabelled++;
		return stamp + (uint64_t)add;
	}
	if (rx_last != 0) {
		const int64_t delta = (int64_t)(stamp - rx_last);
		if (genlock_fifo_relabel_jump_recorded(delta, src_ns)) {
			s->jump_ns = delta;
			s->jump_mono_ns = mono_now;
		}
	}
	return stamp;
}
