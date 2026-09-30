/* camera-box issue 1381 (design 5900385541) -- the C harness of tests/genlock_audio_relabel_ingest_1381.rs.
 *
 * NOT compiled on its own: the test substitutes the shipped obs-source.c code, lifted verbatim, for
 * the at-sign markers below, compiles the result with cc and compares the printed trace with its
 * truth table. The lifted code is the whole ingest branch that decides place-vs-append for a packet:
 * the relabel decision, OBS's raw-domain TS smoothing (the 70 ms snap and the > 2 s handle_ts_jump
 * reset) and the system-domain push-back check (with its second > 2 s reset). Everything else here is
 * a stub libobs (the source fields that branch touches, the deques, blog) and a tail that runs the
 * shipped skew hold the way the real ingest does after the branch. Design 5901213031: the branch also
 * decides the PENDING relabel (the sender's box stepped first), and the tail runs the shipped relabel
 * remainder booking (the slew the packet adds) lifted verbatim too. */
#include <inttypes.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include "util/util_uint64.h"
#include "obs-genlock-wall-step.h"

#define MAX_AUDIO_CHANNELS 8
#define LOG_INFO 300
#define LOG_DEBUG 400

/* pthread_mutex_t comes from <sys/types.h>; the ingest runs single-threaded here. */
#define pthread_mutex_lock(m) ((void)(m))
#define pthread_mutex_unlock(m) ((void)(m))

@LIFTED_MAX_TS_VAR@

/* libobs util/deque.h, the size only (the harness never reads the samples). */
struct deque {
	size_t size;
};
static inline void deque_pop_front(struct deque *dq, void *out, size_t n)
{
	(void)out;
	dq->size -= n;
}

/* blog: the LOG_DEBUG lines are counted (the stock "exceeded TS_SMOOTHING_THRESHOLD" / "jumped" lines),
 * the LOG_INFO line of the packet is kept (the genlock-audio-step-hold line under test) */
static int h_debug_lines;
static char h_info[512];
static bool h_info_set;
__attribute__((format(printf, 2, 3))) static void blog(int level, const char *fmt, ...)
{
	if (level == LOG_DEBUG) {
		h_debug_lines++;
		return;
	}
	va_list ap;
	va_start(ap, fmt);
	vsnprintf(h_info, sizeof(h_info), fmt, ap);
	va_end(ap);
	h_info_set = true;
}

struct audio_data {
	uint8_t *data[MAX_AUDIO_CHANNELS];
	uint32_t frames;
	uint64_t timestamp;
};

/* the timecode ASRC's level setpoint: only the slew booking touches it in this branch */
struct h_asrc {
	double shifted_ms;
};
static void asrc_compensator_shift_level_target(struct h_asrc *a, double ms)
{
	a->shifted_ms += ms;
}

struct h_context {
	const char *name;
};

typedef struct obs_source obs_source_t;
struct obs_source {
	struct h_context context;
	bool timing_set;
	uint64_t timing_adjust;
	uint64_t next_audio_ts_min;
	uint64_t next_audio_sys_ts_min;
	uint64_t audio_ts;
	struct deque audio_input_buf[MAX_AUDIO_CHANNELS];
	size_t last_audio_input_buf_size;
	int audio_buf_mutex;
	bool async_unbuffered;
	bool async_decoupled;
	int64_t genlock_audio_slew_step_ns;
	int64_t genlock_audio_slew_remaining_ns;
	struct h_asrc asrc;
	uint32_t genlock_audio_relabels;
	bool genlock_audio_step_relabel_pending;
	uint64_t genlock_audio_step_prev_arrival_ns;
	int64_t genlock_audio_step_unmatched_ns;
	uint64_t genlock_audio_step_unmatched_at_ns;
	bool genlock_audio_step_active;
	int64_t genlock_audio_step_prev_off_ns;
	uint64_t genlock_audio_step_prev_raw_ns;
	uint64_t genlock_audio_step_prev_packet_ns;
	int64_t genlock_audio_step_nominal_age_ns;
	uint64_t genlock_audio_step_nominal_dev_since_ns;
	uint32_t genlock_audio_step_nominal_warm;
	int64_t genlock_audio_step_held_off_ns;
	uint64_t genlock_audio_step_start_ns;
	int64_t genlock_audio_step_step_ns;
	uint32_t genlock_audio_step_holds;
};

@LIFTED_BLOCK@

@LIFTED_FUNCTIONS@

/* one packet: the lifted branch, then the skew hold and the stock timeline tail the real ingest runs
 * after it, and an append / placement into the stub buffer */
struct h_out {
	bool relabel;
	/* design 5901213031: this packet started a pending relabel */
	bool pending;
	bool push_back;
	bool reset;
	bool dropped;
	/* the raw-domain timeline continues from THIS packet's own stamp (not a snapped old one) */
	bool on_raw;
	int release;
	/* the packet logged a genlock-audio-step-hold line (kept in h_info) */
	bool logged;
	/* design 5901213031: the slew the relabel remainder booking added (ns) */
	int64_t book_ns;
};

static struct h_out h_ingest(obs_source_t *source, const struct audio_data *data, int genlock_hold_mode,
			     int64_t genlock_off_live_ns, uint64_t os_time)
{
	size_t sample_rate = 48000;
	struct audio_data in = *data;
	uint64_t diff;
	bool using_direct_ts = false;
	bool push_back = false;
	bool genlock_timeline_reset = false;
	const size_t buf_before = source->audio_input_buf[0].size;
	h_info_set = false;
	const uint64_t genlock_step_packet_ns = conv_frames_to_time(sample_rate, in.frames);
@INGEST_BRANCH@
	struct h_out out = {genlock_relabel,
			    genlock_relabel_pending,
			    push_back,
			    genlock_timeline_reset,
			    source->audio_input_buf[0].size < buf_before,
			    source->next_audio_ts_min == data->timestamp + genlock_step_packet_ns,
			    0,
			    false,
			    0};
	int64_t genlock_off_ns = genlock_off_live_ns;
	const int genlock_step_release = genlock_audio_step_hold_source(
		source, genlock_hold_mode == GENLOCK_AUDIO_HOLD_TIMECODE, genlock_off_live_ns, data->timestamp,
		genlock_step_packet_ns, os_time, genlock_timeline_reset, &genlock_off_ns);
	out.release = genlock_step_release;
	/* the shipped booking reads the timecode ASRC flag (a timecode source with a resampler here) */
	const bool genlock_asrc_tc = true;
	const int64_t h_slew_before = source->genlock_audio_slew_remaining_ns;
@BOOK_SLICE@
	out.book_ns = source->genlock_audio_slew_remaining_ns - h_slew_before;
	genlock_audio_step_log(source, genlock_step_release, genlock_relabel, genlock_relabel_off_jump_ns,
			       genlock_relabel_move_ns, genlock_off_live_ns, os_time);
	out.logged = h_info_set;
	source->next_audio_sys_ts_min = source->next_audio_ts_min + source->timing_adjust;
	if (push_back && source->audio_ts) {
		for (size_t i = 0; i < MAX_AUDIO_CHANNELS; i++)
			source->audio_input_buf[i].size += in.frames * sizeof(float);
	} else if (!source->audio_ts) {
		source->audio_ts = in.timestamp;
	}
	return out;
}

/* ---------------------------------------------------------------------------------------------- */

#define H_PACKET_FRAMES 1600u
#define H_PACKET_NS 33333333ull
#define H_WALL 1790000000000000000ull
#define H_MONO 86400000000000ull
#define H_OFF ((int64_t)(H_MONO - H_WALL))
/* one 30 fps slot on the per-second grid, and the arrival lag behind the stamp */
#define H_SLOT(n) ((int64_t)(n) * 1000000000ll / 30)
#define H_LAG_NS 3000000ull

static obs_source_t h_src;
static uint64_t h_k;
static int64_t h_shift;
static int64_t h_off;
static uint64_t h_late;
/* review round 2: this packet submitted late by this much (its stamp and its arrival) */
static uint64_t h_delay;

static void h_reset(const char *name)
{
	memset(&h_src, 0, sizeof(h_src));
	h_src.context.name = name;
	h_k = 0;
	h_shift = 0;
	h_off = H_OFF;
	h_late = 0;
	h_delay = 0;
	printf("== %s\n", name);
}

/* one packet of slot h_k: stamp = its slot on the sender's grid moved by the sender's relabel shift,
 * arriving H_LAG_NS after its slot on the receiver's monotonic clock (minus a catch-up h_late) */
static struct h_out h_packet(void)
{
	struct audio_data d;
	memset(&d, 0, sizeof(d));
	d.frames = H_PACKET_FRAMES;
	d.timestamp = H_WALL + h_k * H_PACKET_NS + (uint64_t)h_shift + h_delay;
	const uint64_t now = H_MONO + h_k * H_PACKET_NS + H_LAG_NS - h_late + h_delay;
	h_k++;
	return h_ingest(&h_src, &d, GENLOCK_AUDIO_HOLD_TIMECODE, h_off, now);
}

static void h_print(const char *what, struct h_out o)
{
	printf("%s relabel=%d pending=%d push=%d reset=%d dropped=%d on_raw=%d release=%d active=%d relabels=%u "
	       "book=%+.1f\n",
	       what, o.relabel ? 1 : 0, o.pending ? 1 : 0, o.push_back ? 1 : 0, o.reset ? 1 : 0, o.dropped ? 1 : 0,
	       o.on_raw ? 1 : 0, o.release, h_src.genlock_audio_step_active ? 1 : 0, h_src.genlock_audio_relabels,
	       (double)o.book_ns / 1e6);
	if (o.logged)
		printf("log %s\n", h_info);
}

/* steady packets; each must append (the source's very first packet places, as in OBS) */
static void h_steady(int n)
{
	for (int i = 0; i < n; i++) {
		const bool first = h_k == 0;
		struct h_out o = h_packet();
		if (o.relabel || o.pending || o.reset || o.dropped || !o.on_raw || o.release || o.logged || o.book_ns ||
		    o.push_back == first)
			h_print("STEADY-BROKEN", o);
	}
}

int main(void)
{
	/* a relabel on the receiver's step packet (the stamps and the offset jump together) */
	static const struct {
		const char *name;
		int64_t step_ns;
	} joint[] = {
		{"joint_40ms", 40000000},
		{"joint_260ms", 260000000},
		{"joint_682ms", 682474000},
		{"joint_back_1500ms", -1500000000},
		{"joint_2500ms", 2500000000},
	};
	for (size_t j = 0; j < sizeof(joint) / sizeof(joint[0]); j++) {
		h_reset(joint[j].name);
		h_steady(40);
		const int64_t s = joint[j].step_ns;
		const int64_t n = s >= 0 ? s * 30 / 1000000000 : -((-s * 30 + 999999999) / 1000000000);
		h_off -= s;
		h_shift += H_SLOT(n);
		h_print("relabel", h_packet());
		h_print("after", h_packet());
		h_steady(5);
	}
	/* the split shape: the receiver's step lands on a packet whose stamp has not moved (the hold
	 * starts), the relabelled stamps on the next one */
	h_reset("split_682ms");
	h_steady(40);
	h_off -= 682474000;
	h_print("step", h_packet());
	h_shift += H_SLOT(20);
	h_print("relabel", h_packet());
	h_print("after", h_packet());
	h_steady(5);
	/* a sender that catches up (continuous stamps, a burst) is today's path: never a relabel */
	h_reset("catchup_682ms");
	h_steady(40);
	h_off -= 682474000;
	h_print("step", h_packet());
	for (int i = 0; i < 3; i++) {
		h_late += 25000000ull;
		h_print("burst", h_packet());
	}
	/* the 1367 stamp leap with no wall step (a slot never sent, the stamps 47 ms on: an 80 ms jump after an
	 * arrival gap): placed at its stamp, as today -- the gap keeps it off the pending relabel */
	h_reset("leap_80ms");
	h_steady(40);
	h_k++;
	h_shift += 47000000;
	h_print("leap", h_packet());
	/* a sender restart (3 s silent, then on its own wall: the stamps AND the arrival jump 3 s):
	 * handle_ts_jump drops the buffer, as today */
	h_reset("restart_3000ms");
	h_steady(40);
	h_k += 90;
	h_print("restart", h_packet());
	/* a pause of 500 ms (15 slots never sent): the stamp jump comes with its arrival gap -- placed at its
	 * stamp, as today */
	h_reset("pause_500ms");
	h_steady(40);
	h_k += 15;
	h_print("pause", h_packet());
	/* a duplicated slot (the same stamp again, no later): the 70 ms smoothing appends it, as today */
	h_reset("dup_slot");
	h_steady(40);
	h_k--;
	h_print("dup", h_packet());
	/* design 5901213031: the SENDER's box steps first -- its stamps jump N slots with continuous arrival
	 * (the emit re-phased r earlier) while this box's offset stays: a pending relabel, appended on the
	 * continuous timeline; this box's own step 15 packets later resolves it (the remainder -r booked on
	 * the slew) */
	static const struct {
		const char *name;
		int64_t step_ns;
	} pending[] = {
		{"pending_682ms", 682474000},
		{"pending_back_1500ms", -1500000000},
		{"pending_2500ms", 2500000000},
	};
	for (size_t j = 0; j < sizeof(pending) / sizeof(pending[0]); j++) {
		h_reset(pending[j].name);
		h_steady(40);
		const int64_t s = pending[j].step_ns;
		const int64_t n = s >= 0 ? s * 30 / 1000000000 : -((-s * 30 + 999999999) / 1000000000);
		h_shift += H_SLOT(n);
		h_late += (uint64_t)(s - H_SLOT(n));
		h_print("start", h_packet());
		for (int i = 0; i < 15; i++) {
			const struct h_out o = h_packet();
			if (o.relabel || o.pending || !o.push_back || o.release || o.logged || o.book_ns)
				h_print("PENDING-BROKEN", o);
		}
		h_off -= s;
		h_print("resolve", h_packet());
		h_print("after", h_packet());
		h_steady(5);
	}
	/* review round 1: a 500 ms pause INSIDE the pending window (its arrival gap with the jump) takes the
	 * stock path (placed at its stamp) and never moves the held offset, so this box's own step still
	 * resolves the pending */
	h_reset("pending_pause_682ms");
	h_steady(40);
	h_shift += H_SLOT(20);
	h_late += (uint64_t)(682474000 - H_SLOT(20));
	h_print("start", h_packet());
	for (int i = 0; i < 5; i++) {
		const struct h_out o = h_packet();
		if (o.relabel || o.pending || !o.push_back || o.release || o.logged || o.book_ns)
			h_print("PENDING-BROKEN", o);
	}
	h_k += 15;
	h_print("pause", h_packet());
	for (int i = 0; i < 5; i++) {
		const struct h_out o = h_packet();
		if (o.relabel || o.pending || !o.push_back || o.release || o.logged || o.book_ns)
			h_print("PENDING-BROKEN", o);
	}
	h_off -= 682474000;
	h_print("resolve", h_packet());
	h_print("after", h_packet());
	/* review round 1: a second relabel-shaped jump INSIDE the pending window (the sender's box steps again
	 * by 110 ms: 3 slots, continuous arrival) moves the held offset with it, so this box's one step by
	 * both still resolves the pending (the residual both remainders); a joint relabel on the same source
	 * afterwards prints pending=0 (the kept flag belongs to the pending's own release line only) */
	h_reset("pending_twice_682ms");
	h_steady(40);
	h_shift += H_SLOT(20);
	h_late += (uint64_t)(682474000 - H_SLOT(20));
	h_print("start", h_packet());
	for (int i = 0; i < 5; i++) {
		const struct h_out o = h_packet();
		if (o.relabel || o.pending || !o.push_back || o.release || o.logged || o.book_ns)
			h_print("PENDING-BROKEN", o);
	}
	h_shift += H_SLOT(3);
	h_late += (uint64_t)(110000000 - H_SLOT(3));
	h_print("again", h_packet());
	for (int i = 0; i < 5; i++) {
		const struct h_out o = h_packet();
		if (o.relabel || o.pending || !o.push_back || o.release || o.logged || o.book_ns)
			h_print("PENDING-BROKEN", o);
	}
	h_off -= 682474000 + 110000000;
	h_print("resolve", h_packet());
	h_print("after", h_packet());
	h_steady(5);
	h_off -= 260000000;
	h_shift += H_SLOT(7);
	h_print("joint", h_packet());
	h_print("after", h_packet());
	/* review round 2: a raw-clock sender (its stamps its submission wall) steps first by 682 ms and submits
	 * the step-carrying packet 8 ms late (its stamp and its arrival): the pending starts on S + 8 ms, the
	 * next on-time packet's -8 ms move folds back, and this box's step resolves it with no residual */
	h_reset("pending_late_682ms");
	h_steady(40);
	h_shift += 682474000;
	h_delay = 8000000;
	h_print("start", h_packet());
	h_delay = 0;
	for (int i = 0; i < 5; i++) {
		const struct h_out o = h_packet();
		if (o.relabel || o.pending || !o.push_back || o.release || o.logged || o.book_ns)
			h_print("PENDING-BROKEN", o);
	}
	h_off -= 682474000;
	h_print("resolve", h_packet());
	h_print("after", h_packet());
	/* a pending relabel this box never follows: released at the 10 s bound (J applied once by the ingest's
	 * step placement, outside this branch) */
	h_reset("pending_timeout_682ms");
	h_steady(40);
	h_shift += H_SLOT(20);
	h_print("start", h_packet());
	/* bounded: 400 packets are 13.3 s, past the 10 s bound; no release at all prints its own line */
	bool h_released = false;
	for (int i = 0; i < 400 && !h_released; i++) {
		const struct h_out o = h_packet();
		if (o.release) {
			h_print("timeout", o);
			h_released = true;
		}
	}
	if (!h_released)
		printf("NO-RELEASE within 400 packets\n");
	/* slice 3 (design 5902870861, ROZHODNUTE 5902983227): the sender's box steps first by ONE slot. On its
	 * 100 ns grid the stamps jump one packet - 66 ns (the grid position slice 2 missed) or one packet
	 * + 34 ns, with continuous arrival: a pending relabel, resolved by this box's own step of -(one slot
	 * + r) 15 packets later */
	static const struct {
		const char *name;
		int64_t step_ns;
		int64_t jump_ns;
	} one_slot[] = {
		{"pending_one_slot_40ms", 40000000, 33333267},
		{"pending_one_slot_60ms", 60000000, 33333367},
	};
	for (size_t j = 0; j < sizeof(one_slot) / sizeof(one_slot[0]); j++) {
		h_reset(one_slot[j].name);
		h_steady(40);
		h_shift += one_slot[j].jump_ns;
		h_late += (uint64_t)(one_slot[j].step_ns - one_slot[j].jump_ns);
		h_print("start", h_packet());
		for (int i = 0; i < 15; i++) {
			const struct h_out o = h_packet();
			if (o.relabel || o.pending || !o.push_back || o.release || o.logged || o.book_ns)
				h_print("PENDING-BROKEN", o);
		}
		h_off -= one_slot[j].step_ns;
		h_print("resolve", h_packet());
		h_print("after", h_packet());
		h_steady(5);
	}
	/* a one-slot pending relabel this box never follows: released at the 10 s bound, its residual J within
	 * one packet (the release does not place it; the timecode ASRC books it, outside this branch) */
	h_reset("pending_one_slot_timeout");
	h_steady(40);
	h_shift += 33333267;
	h_late += 6666733;
	h_print("start", h_packet());
	h_released = false;
	for (int i = 0; i < 400 && !h_released; i++) {
		const struct h_out o = h_packet();
		if (o.release) {
			h_print("timeout", o);
			h_released = true;
		}
	}
	if (!h_released)
		printf("NO-RELEASE within 400 packets\n");
	/* a skipped slot on the same grid position: the stamps jump one packet - 66 ns like an N = +1 relabel,
	 * but the arrival gaps one packet -- the stock path, as in slice 2 (never a pending relabel) */
	h_reset("skip_slot_one_slot");
	h_steady(40);
	h_k++;
	h_shift -= 66;
	h_print("skip", h_packet());
	h_print("after", h_packet());
	/* an N = -1 relabel (the sender's box steps back 10 ms first): its stamps repeat one packet, exactly a
	 * duplicated slot, with continuous arrival (the emit re-phased r = 23.3 ms earlier) -- the stock path,
	 * as in slice 2; this box's own step 15 packets later starts no hold (10 ms, under one packet) */
	h_reset("n_minus_one_10ms");
	h_steady(40);
	h_shift -= (int64_t)H_PACKET_NS;
	h_late += 23333333;
	h_print("start", h_packet());
	for (int i = 0; i < 15; i++) {
		const struct h_out o = h_packet();
		if (o.relabel || o.pending || !o.push_back || o.release || o.logged || o.book_ns)
			h_print("N-MINUS-ONE-BROKEN", o);
	}
	h_off += 10000000;
	h_print("step", h_packet());
	h_print("after", h_packet());
	/* slice 3 review round 1: a skipped slot at the - 66 ns grid position INSIDE a one-slot pending (its
	 * arrival one packet late): the stock path for that packet, the held offset kept, so this box's own step
	 * still resolves the pending with -r */
	h_reset("pending_skip_one_slot_40ms");
	h_steady(40);
	h_shift += 33333267;
	h_late += 6666733;
	h_print("start", h_packet());
	for (int i = 0; i < 5; i++) {
		const struct h_out o = h_packet();
		if (o.relabel || o.pending || !o.push_back || o.release || o.logged || o.book_ns)
			h_print("PENDING-BROKEN", o);
	}
	h_k++;
	h_shift -= 66;
	h_print("skip", h_packet());
	for (int i = 0; i < 5; i++) {
		const struct h_out o = h_packet();
		if (o.relabel || o.pending || !o.push_back || o.release || o.logged || o.book_ns)
			h_print("PENDING-BROKEN", o);
	}
	h_off -= 40000000;
	h_print("resolve", h_packet());
	h_print("after", h_packet());
	/* slice 3 review round 1: this box steps first by 60 ms and its hold times out; the sender relabels one
	 * slot only after that (its first relabelled block not re-phased yet, r = 26.7 ms): a late follow, never
	 * a pending relabel -- nothing starts, nothing is released again */
	h_reset("late_follow_one_slot_60ms");
	h_steady(40);
	h_off -= 60000000;
	h_print("step", h_packet());
	h_released = false;
	for (int i = 0; i < 400 && !h_released; i++) {
		const struct h_out o = h_packet();
		if (o.release) {
			h_print("timeout", o);
			h_released = true;
		}
	}
	if (!h_released)
		printf("NO-RELEASE within 400 packets\n");
	h_shift += H_SLOT(1);
	h_print("follow", h_packet());
	h_late += 26666667;
	for (int i = 0; i < 400; i++) {
		const struct h_out o = h_packet();
		if (o.pending || o.release || o.relabel) {
			h_print("FALSE-PENDING", o);
			break;
		}
	}
	/* slice 3 review round 2: the same late follow, 66 ms at the - 66 ns grid position, its first relabelled
	 * block 5 ms late (every packet before it 5 ms early): r + 5 ms passes one packet, which slice 2 read as
	 * away -- still a follow, never a pending relabel */
	h_reset("late_follow_one_slot_66ms_late_block");
	h_late = 5000000;
	h_steady(40);
	h_off -= 66000000;
	h_print("step", h_packet());
	h_released = false;
	for (int i = 0; i < 400 && !h_released; i++) {
		const struct h_out o = h_packet();
		if (o.release) {
			h_print("timeout", o);
			h_released = true;
		}
	}
	if (!h_released)
		printf("NO-RELEASE within 400 packets\n");
	h_shift += 33333267;
	h_late -= 5000000;
	h_print("follow", h_packet());
	/* the sender re-phases its emit r = 32.67 ms earlier; the next block, emitted 0.67 ms after the late one,
	 * arrives in order right behind it (an arrival gap of 0), the rest on the re-phased schedule */
	h_late += 33333333;
	{
		const struct h_out o = h_packet();
		if (o.pending || o.release || o.relabel)
			h_print("FALSE-PENDING", o);
	}
	h_late += 5000000 + 32666733 - 33333333;
	for (int i = 0; i < 400; i++) {
		const struct h_out o = h_packet();
		if (o.pending || o.release || o.relabel) {
			h_print("FALSE-PENDING", o);
			break;
		}
	}
	/* ROZHODNUTE 5903945145 (slice-3 review round 2): this box steps first by 34 ms, one slot + 0.7 ms.
	 * Its packets alternate 0 / 2 ms early (arrival jitter), so the skew hold releases EARLY on the age
	 * test (placed: the skew hold's known limit) and the in-band nominal tracks the unmatched step for
	 * 60 s. The sender's late one-slot relabel (the - 66 ns grid position) then brings the age back
	 * toward the pre-step nominal: the remembered step's follow, never a pending relabel */
	h_reset("late_follow_34ms_jittered");
	h_steady(40);
	h_off -= 34000000;
	h_print("step", h_packet());
	h_late = 2000000;
	h_print("early", h_packet());
	for (int i = 0; i < 1800; i++) {
		h_late = (i % 2) ? 2000000u : 0u;
		const struct h_out o = h_packet();
		if (o.pending || o.release || o.relabel) {
			h_print("UNEXPECTED", o);
			break;
		}
	}
	h_late = 0;
	h_shift += 33333267;
	h_print("follow", h_packet());
	for (int i = 0; i < 400; i++) {
		h_late = (i % 2) ? 0u : 2000000u;
		const struct h_out o = h_packet();
		if (o.pending || o.release || o.relabel) {
			h_print("FALSE-PENDING", o);
			break;
		}
	}
	printf("debug_lines=%d\n", h_debug_lines);
	return 0;
}
