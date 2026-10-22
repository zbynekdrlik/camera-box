/*
 * camera-box issue 1439 -- the receive timeline of the obs-vban source.
 *
 * Stock obs-vban 0.3.1 stamped every packet at its arrival minus its length, and padded a lost
 * packet with copies of the next one. A program feed from a network sender (lv1's VBAN `stream`
 * into the Linux stream box) then carried the sender's network jitter straight into OBS, which
 * mixes at 0 ms audio buffering: a late packet was re-anchored by OBS, and that was a click.
 *
 * This decision gives every packet a stamp on a FIXED timeline that follows the sender's clock:
 *
 *  - Anchor: the first packet is stamped at its arrival - its length + R, R being the fixed
 *    "Receive Buffer" (default 40 ms, read at create/update only, never adapted at runtime). R is
 *    measured from the packet's START, so the timeline uses max(R, one packet +
 *    VBAN_RECV_RESERVE_MARGIN_MS) (reserve_eff_ns, fixed at the anchor; the source warns once when it
 *    lifts R). Within
 *    the first VBAN_RECV_ACQUIRE_S s of the sender's audio (the packet's END position) a packet that
 *    arrived earlier against the line moves the line to it, so the anchor is the EARLIEST arrival of
 *    that second (ROZHODNUTE 6343922049): later packets are never early against it. After that
 *    second the line's phase is fixed.
 *  - Hold (ROZHODNUTE 6345255813 item 1): nothing is emitted for that first second: the anchor and
 *    every packet ending within it are HOLD (they refine the anchor and fold, no slot is filled). The
 *    first packet past it is the first PACKET, on the final line, so libobs never sees a stamp step
 *    and its stamp-basis ASRC never walks; the slots lost just before it are not filled. Every full
 *    re-anchor holds again.
 *  - Fit: a least-squares line of arrival time over the packet's END position in the sender's
 *    samples (its nuFrame order), over the last VBAN_RECV_FIT_WINDOW_S one-second buckets of the
 *    sender's audio. A packet later than the reference line by more than VBAN_RECV_OUTLIER_MS is
 *    not folded into it (a gap's delayed packets never pull the line late). Until the fit is used,
 *    the reference is the anchored line - R.
 *  - Stamps: each packet is stamped on a line of its own, stamp(pos) = base + the position's
 *    duration x (1 + step_ppb / 1e9). Once per fitted bucket the step moves toward the fitted RATE
 *    by at most 5 ppm per update, clamped to +-500 ppm: the latency set at the anchor is held, only
 *    the rate is followed. A phase term (phase error / 20 s, at most 100 ppm) joins the target ONLY
 *    while the stamps are more than R / VBAN_RECV_PHASE_BAND_DIV off fit + R (a real sender-clock
 *    event, never the arrival jitter), until they are back inside that dead band. The line is rebased
 *    on every change, so the stamps never jump. The genlock ASRC (basis=stamp) then resamples the
 *    sender's rate to the mix clock.
 *  - Loss: a nuFrame gap is silence for exactly the lost slots, stamped on the same line.
 *  - Stale: a duplicate, or a nuFrame at most VBAN_RECV_STALE_PACKETS behind the last one (a network
 *    reorder), is dropped and counted: its slot has passed (it went out as silence), the line runs on.
 *  - Late = lost (ROZHODNUTE 6345255813 item 2; the round-3 late shift is withdrawn): a fixed latency
 *    is a fixed jitter buffer. A packet arriving after its stamp (once the hold is over) is LATE:
 *    dropped and counted, its slot (with the slots lost right before it) silence at once on the
 *    unchanged line. The line, the fit and the latency stay where the anchor put them.
 *  - Re-anchor (counted, one per event, then the hold): a STARVE -- no packet in time for
 *    VBAN_RECV_PERSIST_S on OUR clock: any packet arriving more than that after the last in-time
 *    packet did (an outage, a sender pause, every packet late on a delay step), late or not, with
 *    nothing filled after the fact -- a nuFrame further behind (a sender restart), a forward jump of more
 *    than VBAN_RECV_JUMP_S of audio (a sender restart), another rate or channel count, another R, or a
 *    fit that cannot hold: the fitted rate beyond +-500 ppm or the stamps more than R off fit + R. The
 *    packet that re-anchors is the new anchor. Packets still in time never re-anchor.
 *
 * Pure: no OBS dependency, no libm, no 128-bit integers (MSVC builds it). The Tier-0 authority is
 * src/vban_receive_timeline.rs in camera-box; tests/vban_receive_timeline_parity_1439.rs compiles
 * THIS header and requires identical decisions and state, the doubles compared bit for bit.
 */

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>

#define VBAN_RECV_RESERVE_MS_DEFAULT 40
#define VBAN_RECV_RESERVE_MS_MIN 10
#define VBAN_RECV_RESERVE_MS_MAX 500
/* The fit window, in one-second buckets of the sender's audio. */
#define VBAN_RECV_FIT_WINDOW_S 64
/* The fit is used once its window holds this many non-empty buckets. */
#define VBAN_RECV_FIT_MIN_BUCKETS 10
/* A packet later than the reference line by more than this is not folded into the fit. */
#define VBAN_RECV_OUTLIER_MS 10
/* No packet in time for this long on our clock (an outage, a sender pause, every packet late on a
 * network delay step) = one full re-anchor, a starve (ROZHODNUTE 6345255813 item 2). */
#define VBAN_RECV_PERSIST_S 1
/* Nothing folded for this much of the sender's audio while the fit is valid = the fit is FROZEN (it holds
 * its last rate and level; the status line says so, review round 4). Never a re-anchor. */
#define VBAN_RECV_FROZEN_S 3
/* A forward nuFrame jump over this much audio is a sender restart. */
#define VBAN_RECV_JUMP_S 2
/* A duplicate, or a nuFrame at most this many packets behind the last one, is a stale packet (a
 * network reorder or duplicate): dropped and counted, never a re-anchor. Further behind = a restart. */
#define VBAN_RECV_STALE_PACKETS 64u
/* The effective Receive Buffer is at least one packet + this (ROZHODNUTE 6343922049): R is measured
 * from the packet's start, its arrival is its end. */
#define VBAN_RECV_RESERVE_MARGIN_MS 10
/* The anchor is the earliest arrival among the packets that end within this many seconds of the anchor
 * packet's start (ROZHODNUTE 6343922049). */
#define VBAN_RECV_ACQUIRE_S 1
/* The phase term acts only while the stamps are more than R / this off fit + R. */
#define VBAN_RECV_PHASE_BAND_DIV 4
/* The stamp step stays within +-500 ppm of nominal. */
#define VBAN_RECV_PPB_CLAMP 500000
/* The stamp step moves by at most 5 ppm per fit update (about one a second). */
#define VBAN_RECV_PPB_SLEW 5000
/* Outside the dead band a phase error is walked out at error / 20 s ... */
#define VBAN_RECV_PHASE_TAU_S 20
/* ... with at most 100 ppm of the step spent on it. */
#define VBAN_RECV_PHASE_PPB_MAX 100000
#define VBAN_RECV_LOG_INTERVAL_NS 10000000000ULL
/* Silence for lost slots goes to OBS in pieces of at most this many frames. */
#define VBAN_RECV_SILENCE_CHUNK_FRAMES 1024
#define VBAN_RECV_NS_PER_SEC 1000000000ULL
#define VBAN_RECV_NS_PER_MS 1000000ULL

enum vban_recv_event {
	VBAN_RECV_PACKET = 0,   /* stamped on the running timeline */
	VBAN_RECV_ANCHOR = 1,   /* the first packet: the timeline starts here */
	VBAN_RECV_REANCHOR = 2, /* the timeline restarts at this packet (counted) */
	VBAN_RECV_STALE = 3,    /* a duplicate / reordered packet whose slot has passed: dropped, counted */
	VBAN_RECV_HOLD = 4,     /* inside an anchor's first second: refines + folds, never output */
	VBAN_RECV_LATE = 5,     /* past its stamp: dropped, counted, its slot silence on the unchanged line */
};

enum vban_recv_reason {
	VBAN_RECV_REASON_NONE = 0,
	VBAN_RECV_REASON_STARVE = 1,   /* no packet in time for VBAN_RECV_PERSIST_S on our clock */
	VBAN_RECV_REASON_BACKWARD = 2, /* a nuFrame more than VBAN_RECV_STALE_PACKETS behind */
	VBAN_RECV_REASON_JUMP = 3,     /* a forward nuFrame jump over VBAN_RECV_JUMP_S */
	VBAN_RECV_REASON_FORMAT = 4,   /* another sample rate or channel count */
	VBAN_RECV_REASON_CONFIG = 5,   /* another Receive Buffer */
	VBAN_RECV_REASON_FIT = 6,      /* the fit cannot hold */
};

struct vban_recv_bucket {
	int64_t k;   /* the bucket index (the end position / rate), -1 = empty */
	uint64_t n;  /* folded packets */
	uint64_t x0; /* the first folded end position (samples since the anchor) */
	uint64_t y0; /* its arrival (ns since the anchor's arrival) */
	double su;   /* sums over (x - x0, y - y0) */
	double sv;
	double suu;
	double suv;
};

struct vban_recv_timeline {
	bool anchored;
	uint32_t rate;
	uint32_t channels;
	uint64_t reserve_ns;     /* the Receive Buffer as configured (another value re-anchors) */
	uint64_t reserve_eff_ns; /* max(reserve_ns, the anchor packet's length + the margin), fixed at the anchor */
	uint64_t anchor_ns;     /* the anchor packet's arrival */
	uint64_t anchor_end;    /* the anchor packet's end position (its frame count) */
	uint32_t last_nu;       /* the last packet's nuFrame */
	uint64_t next_pos;      /* the next packet's start, in samples since the anchor */
	uint64_t base_stamp_ns; /* the stamp line: stamp(pos) = base + T(pos - base_pos) */
	uint64_t base_pos;
	int64_t step_ppb;       /* the stamp step's deviation from nominal */
	int64_t last_k;         /* the bucket of the last packet's end position */
	uint64_t last_intime_ns; /* the arrival of the last packet that came in time */
	uint64_t last_fold_end;  /* the end position of the last packet folded into the fit */
	struct vban_recv_bucket buckets[VBAN_RECV_FIT_WINDOW_S];
	bool fit_valid;
	int64_t fit_ppb; /* the fitted step deviation (positive = the sender's clock is slow) */
	double fit_b;    /* ns per sample */
	double fit_xbar;
	double fit_ybar;
	int64_t phase_ns; /* fit + R - stamp at the last update */
	uint64_t received;
	uint64_t lost;            /* lost packets (silence slots) */
	uint64_t silence_samples; /* the silence those slots carried */
	uint64_t late;            /* packets that arrived after their stamp (each dropped, its slot silence) */
	uint64_t re_anchors;
	uint64_t stale; /* duplicate / reordered packets dropped (their slot had passed) */
};

/* What the source does with one packet: silence first (when any), then the packet. */
struct vban_recv_decision {
	int event;  /* enum vban_recv_event */
	int reason; /* enum vban_recv_reason, for a REANCHOR */
	uint64_t silence_samples;
	uint64_t silence_pos; /* the first silent sample's position (vban_recv_stamp_at) */
	uint64_t stamp_ns;    /* the packet's stamp */
	int64_t lateness_ns;  /* arrival - stamp (negative = the margin left) */
	int64_t late_ns;      /* a LATE packet: how far after its stamp; a STARVE re-anchor: how long no
			       * packet came in time (its arrival - the last in-time arrival) */
};

/* The Receive Buffer in ms for a source setting value: 0 or less = the default. */
static inline uint32_t vban_recv_clamp_reserve_ms(int64_t ms)
{
	if (ms <= 0)
		return VBAN_RECV_RESERVE_MS_DEFAULT;
	if (ms < VBAN_RECV_RESERVE_MS_MIN)
		return VBAN_RECV_RESERVE_MS_MIN;
	if (ms > VBAN_RECV_RESERVE_MS_MAX)
		return VBAN_RECV_RESERVE_MS_MAX;
	return (uint32_t)ms;
}

/* floor(samples * 1e9 / rate) without overflowing 64 bits for any realistic sample count. */
static inline uint64_t vban_recv_samples_to_ns(uint64_t samples, uint32_t rate)
{
	const uint64_t r = rate ? (uint64_t)rate : 1;
	return (samples / r) * VBAN_RECV_NS_PER_SEC + (samples % r) * VBAN_RECV_NS_PER_SEC / r;
}

/* ns x ppb / 1e9, truncated toward zero, without overflowing 64 bits. */
static inline int64_t vban_recv_scale_ppb(uint64_t ns, int64_t ppb)
{
	const uint64_t mag = ppb < 0 ? (uint64_t)(-ppb) : (uint64_t)ppb;
	const uint64_t v = (ns / VBAN_RECV_NS_PER_SEC) * mag + (ns % VBAN_RECV_NS_PER_SEC) * mag / VBAN_RECV_NS_PER_SEC;
	return ppb < 0 ? -(int64_t)v : (int64_t)v;
}

/* Round half away from zero (no libm). A value at or beyond +-2^62 saturates there and NaN reads 0:
 * casting an out-of-range double to int64_t is undefined in C (Rust `as` saturates), and 2^62 is far
 * beyond any sane fit yet clear of overflow when the anchor and R are added. */
#define VBAN_RECV_ROUND_LIMIT INT64_C(4611686018427387904)

static inline int64_t vban_recv_round(double x)
{
	const double lim = (double)VBAN_RECV_ROUND_LIMIT;
	if (x != x)
		return 0;
	if (x >= lim)
		return VBAN_RECV_ROUND_LIMIT;
	if (x <= -lim)
		return -VBAN_RECV_ROUND_LIMIT;
	return x >= 0.0 ? (int64_t)(x + 0.5) : -(int64_t)(-x + 0.5);
}

static inline int64_t vban_recv_clamp_i64(int64_t v, int64_t lim)
{
	if (v > lim)
		return lim;
	if (v < -lim)
		return -lim;
	return v;
}

/* The stamp of the sample at POS (samples since the anchor, never before the line's base). */
static inline uint64_t vban_recv_stamp_at(const struct vban_recv_timeline *t, uint64_t pos)
{
	const uint64_t span = pos > t->base_pos ? pos - t->base_pos : 0;
	const uint64_t ns = vban_recv_samples_to_ns(span, t->rate);
	return t->base_stamp_ns + (uint64_t)((int64_t)ns + vban_recv_scale_ppb(ns, t->step_ppb));
}

static inline void vban_recv_fold(struct vban_recv_timeline *t, int64_t k, uint64_t x, uint64_t y)
{
	struct vban_recv_bucket *b = &t->buckets[(size_t)(k % VBAN_RECV_FIT_WINDOW_S)];
	if (b->k != k) {
		b->k = k;
		b->n = 0;
		b->x0 = x;
		b->y0 = y;
		b->su = 0.0;
		b->sv = 0.0;
		b->suu = 0.0;
		b->suv = 0.0;
	}
	const double u = (double)(x - b->x0);
	const double v = (double)(y - b->y0);
	b->n++;
	b->su += u;
	b->sv += v;
	b->suu += u * u;
	b->suv += u * v;
	t->last_fold_end = x;
}

/* The arrival (ns since the anchor's arrival) the reference line expects at end position X: the
 * fit once it is valid, else the anchored line - R (the earliest arrival of the first second at the
 * nominal rate; the first packet itself may have come late). */
static inline double vban_recv_reference(const struct vban_recv_timeline *t, uint64_t x)
{
	if (t->fit_valid)
		return t->fit_ybar + t->fit_b * ((double)x - t->fit_xbar);
	return (double)((int64_t)vban_recv_stamp_at(t, x) - (int64_t)t->reserve_eff_ns - (int64_t)t->anchor_ns);
}

/* Whether the fit is FROZEN: valid, but nothing folded for VBAN_RECV_FROZEN_S of the sender's audio. */
static inline bool vban_recv_fit_frozen(const struct vban_recv_timeline *t)
{
	return t->fit_valid && t->next_pos > t->last_fold_end &&
	       t->next_pos - t->last_fold_end > (uint64_t)VBAN_RECV_FROZEN_S * t->rate;
}

/* Start (or restart) the timeline at this packet. */
static inline void vban_recv_anchor(struct vban_recv_timeline *t, uint64_t now_ns, uint32_t nu, uint32_t frames,
				    uint32_t rate, uint32_t channels, uint64_t reserve_ns)
{
	t->anchored = true;
	t->rate = rate;
	t->channels = channels;
	t->reserve_ns = reserve_ns;
	const uint64_t len = vban_recv_samples_to_ns(frames, rate);
	const uint64_t floor_ns = len + (uint64_t)VBAN_RECV_RESERVE_MARGIN_MS * VBAN_RECV_NS_PER_MS;
	t->reserve_eff_ns = reserve_ns > floor_ns ? reserve_ns : floor_ns;
	t->anchor_ns = now_ns;
	t->anchor_end = frames;
	t->last_nu = nu;
	t->next_pos = frames;
	t->base_stamp_ns = (now_ns > len ? now_ns - len : 0) + t->reserve_eff_ns;
	t->base_pos = 0;
	t->step_ppb = 0;
	for (size_t i = 0; i < VBAN_RECV_FIT_WINDOW_S; i++)
		t->buckets[i].k = -1;
	t->fit_valid = false;
	t->fit_ppb = 0;
	t->fit_b = 0.0;
	t->fit_xbar = 0.0;
	t->fit_ybar = 0.0;
	t->phase_ns = 0;
	t->last_intime_ns = now_ns;
	t->last_k = (int64_t)(frames / rate);
	vban_recv_fold(t, t->last_k, frames, 0);
}

static inline void vban_recv_init(struct vban_recv_timeline *t)
{
	memset(t, 0, sizeof(*t));
	for (size_t i = 0; i < VBAN_RECV_FIT_WINDOW_S; i++)
		t->buckets[i].k = -1;
}

/* Refit over the completed buckets and move the stamp step, rebasing the line at POS. Returns
 * false when the fit cannot hold. */
static inline bool vban_recv_update(struct vban_recv_timeline *t, uint64_t pos)
{
	const int64_t lo = t->last_k - VBAN_RECV_FIT_WINDOW_S + 1;
	uint64_t total = 0;
	int buckets = 0;
	double sx = 0.0, sy = 0.0;
	for (int64_t k = lo; k <= t->last_k; k++) {
		if (k < 0)
			continue;
		const struct vban_recv_bucket *b = &t->buckets[(size_t)(k % VBAN_RECV_FIT_WINDOW_S)];
		if (b->k != k || b->n == 0)
			continue;
		const double n = (double)b->n;
		total += b->n;
		buckets++;
		sx += n * ((double)b->x0 + b->su / n);
		sy += n * ((double)b->y0 + b->sv / n);
	}
	if (buckets < VBAN_RECV_FIT_MIN_BUCKETS)
		return true;
	const double nt = (double)total;
	const double xbar = sx / nt;
	const double ybar = sy / nt;
	double sxx = 0.0, sxy = 0.0;
	for (int64_t k = lo; k <= t->last_k; k++) {
		if (k < 0)
			continue;
		const struct vban_recv_bucket *b = &t->buckets[(size_t)(k % VBAN_RECV_FIT_WINDOW_S)];
		if (b->k != k || b->n == 0)
			continue;
		const double n = (double)b->n;
		const double dx = (double)b->x0 + b->su / n - xbar;
		const double dy = (double)b->y0 + b->sv / n - ybar;
		sxx += (b->suu - b->su * b->su / n) + n * dx * dx;
		sxy += (b->suv - b->su * b->sv / n) + n * dx * dy;
	}
	if (!(sxx > 0.0))
		return true;
	const double slope = sxy / sxx;
	const int64_t fit_ppb = vban_recv_round(slope * (double)t->rate - (double)VBAN_RECV_NS_PER_SEC);
	if (fit_ppb > VBAN_RECV_PPB_CLAMP || fit_ppb < -VBAN_RECV_PPB_CLAMP)
		return false;
	t->fit_valid = true;
	t->fit_ppb = fit_ppb;
	t->fit_b = slope;
	t->fit_xbar = xbar;
	t->fit_ybar = ybar;
	const int64_t target = (int64_t)t->anchor_ns + vban_recv_round(ybar + slope * ((double)pos - xbar)) +
			       (int64_t)t->reserve_eff_ns;
	const uint64_t stamp = vban_recv_stamp_at(t, pos);
	const int64_t phase = target - (int64_t)stamp;
	t->phase_ns = phase;
	const int64_t hold = (int64_t)t->reserve_eff_ns;
	if (phase > hold || phase < -hold)
		return false;
	/* the dead band: inside it the latency is held and only the rate is followed */
	const int64_t band = hold / VBAN_RECV_PHASE_BAND_DIV;
	const int64_t corr = (phase >= -band && phase <= band)
				     ? 0
				     : vban_recv_clamp_i64(phase / VBAN_RECV_PHASE_TAU_S, VBAN_RECV_PHASE_PPB_MAX);
	const int64_t want = vban_recv_clamp_i64(fit_ppb + corr, VBAN_RECV_PPB_CLAMP);
	const int64_t delta = vban_recv_clamp_i64(want - t->step_ppb, VBAN_RECV_PPB_SLEW);
	if (delta != 0) {
		t->base_stamp_ns = stamp;
		t->base_pos = pos;
		t->step_ppb += delta;
	}
	return true;
}

/* The decision for a packet arriving at NOW_NS. */
static inline struct vban_recv_decision vban_recv_packet(struct vban_recv_timeline *t, uint64_t now_ns, uint32_t nu,
							  uint32_t frames, uint32_t rate, uint32_t channels,
							  uint64_t reserve_ns)
{
	struct vban_recv_decision d = {0, 0, 0, 0, 0, 0, 0};
	if (frames == 0)
		frames = 1;
	if (rate == 0)
		rate = 1;
	t->received++;
	if (!t->anchored) {
		vban_recv_anchor(t, now_ns, nu, frames, rate, channels, reserve_ns);
		d.event = VBAN_RECV_ANCHOR;
		d.stamp_ns = t->base_stamp_ns;
		d.lateness_ns = (int64_t)now_ns - (int64_t)d.stamp_ns;
		return d;
	}
	int reason = VBAN_RECV_REASON_NONE;
	uint64_t lost = 0, lost_samples = 0;
	if (rate != t->rate || channels != t->channels) {
		reason = VBAN_RECV_REASON_FORMAT;
	} else if (reserve_ns != t->reserve_ns) {
		reason = VBAN_RECV_REASON_CONFIG;
	} else {
		const uint32_t delta = nu - t->last_nu;
		if (delta == 0 || delta >= 0x80000000u) {
			if (t->last_nu - nu <= VBAN_RECV_STALE_PACKETS) {
				t->stale++;
				d.event = VBAN_RECV_STALE;
				return d;
			}
			reason = VBAN_RECV_REASON_BACKWARD;
		} else {
			lost = (uint64_t)(delta - 1u);
			lost_samples = lost * frames;
			if (lost_samples > (uint64_t)VBAN_RECV_JUMP_S * rate)
				reason = VBAN_RECV_REASON_JUMP;
		}
	}
	/* a starve: no packet in time for VBAN_RECV_PERSIST_S on our clock (signed: an out-of-order arrival
	 * before the last in-time one is never a starve) */
	if (reason == VBAN_RECV_REASON_NONE &&
	    (int64_t)now_ns - (int64_t)t->last_intime_ns > (int64_t)(VBAN_RECV_PERSIST_S * VBAN_RECV_NS_PER_SEC))
		reason = VBAN_RECV_REASON_STARVE;
	uint64_t pos = 0, end = 0;
	int64_t k = 0;
	if (reason == VBAN_RECV_REASON_NONE) {
		pos = t->next_pos + lost_samples;
		end = pos + frames;
		k = (int64_t)(end / rate);
		if (k > t->last_k) {
			const uint64_t at = t->next_pos;
			if (!vban_recv_update(t, at))
				reason = VBAN_RECV_REASON_FIT;
		}
	}
	/* the first second: a packet that arrived earlier against the line moves the line to it. The line
	 * is still base + T(pos) there (no fit has moved it), so the candidate is the anchor this packet
	 * would give (arrival - length + R) carried back to position 0 by the line's own arithmetic: a
	 * packet exactly on the line never moves it by a rounding ns. */
	if (reason == VBAN_RECV_REASON_NONE && end <= (uint64_t)VBAN_RECV_ACQUIRE_S * rate && t->base_pos == 0 &&
	    t->step_ppb == 0) {
		const uint64_t back = vban_recv_samples_to_ns(frames, rate) + vban_recv_samples_to_ns(pos, rate);
		const uint64_t cand = (now_ns > back ? now_ns - back : 0) + t->reserve_eff_ns;
		if (cand < t->base_stamp_ns)
			t->base_stamp_ns = cand;
	}
	if (reason == VBAN_RECV_REASON_NONE) {
		const uint64_t stamp = vban_recv_stamp_at(t, pos);
		/* the hold (ROZHODNUTE 6345255813 item 1): nothing is emitted while the packet ends within the
		 * first second; a slot is filled only once the previous packet was emitted */
		const uint64_t window = (uint64_t)VBAN_RECV_ACQUIRE_S * rate;
		const bool held = end <= window;
		const bool emitting = t->next_pos > window;
		const bool late = !held && now_ns > stamp;
		if (late) {
			d.late_ns = (int64_t)(now_ns - stamp);
			t->late++;
		}
		/* a late packet is LOST (item 2): its audio dropped, its slot silence on the unchanged line
		 * together with the slots lost before it */
		if (late)
			d.event = VBAN_RECV_LATE;
		else if (held)
			d.event = VBAN_RECV_HOLD;
		else
			d.event = VBAN_RECV_PACKET;
		d.stamp_ns = stamp;
		d.lateness_ns = (int64_t)now_ns - (int64_t)stamp;
		const uint64_t filled = lost_samples + (late ? (uint64_t)frames : 0);
		if (filled > 0 && emitting) {
			d.silence_samples = filled;
			d.silence_pos = t->next_pos;
			t->lost += lost;
			t->silence_samples += filled;
		}
		if (!late)
			t->last_intime_ns = now_ns;
		/* fold the packet into the fit unless it came late against the reference line */
		const uint64_t y = now_ns - t->anchor_ns;
		if ((double)y - vban_recv_reference(t, end) <= (double)(VBAN_RECV_OUTLIER_MS * VBAN_RECV_NS_PER_MS))
			vban_recv_fold(t, k, end, y);
		if (k > t->last_k)
			t->last_k = k;
		t->last_nu = nu;
		t->next_pos = end;
		return d;
	}
	if (reason == VBAN_RECV_REASON_STARVE)
		d.late_ns = (int64_t)now_ns - (int64_t)t->last_intime_ns;
	t->re_anchors++;
	vban_recv_anchor(t, now_ns, nu, frames, rate, channels, reserve_ns);
	d.event = VBAN_RECV_REANCHOR;
	d.reason = reason;
	d.stamp_ns = t->base_stamp_ns;
	d.lateness_ns = (int64_t)now_ns - (int64_t)d.stamp_ns;
	return d;
}
