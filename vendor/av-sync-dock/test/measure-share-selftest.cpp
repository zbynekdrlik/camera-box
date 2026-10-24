/*
 * camera-box A/V-sync dock -- the measurement source's own share of the mix, self-test (issue 1470).
 *
 * Live 24.10.2026 on stream-snv the dock read -900.9 ms while the stream was in sync (+24.5 ms by
 * `recording-verdict --av-sync`): the program played the measurement clip (run 911016) next to `mbc`,
 * the clip's digital QPSK marker (scatter 0-1 ms, more readings) out-clustered the room marker
 * (~+50 ms, scatter ~8 ms), and the dock paired every marker of the whole mix with the painter's QR.
 *
 * This self-test pins the fix (`camera-box-measure-share.hpp`):
 *   1. the take rule: a tick's per-source share is taken only when it is the whole tick at offset 0
 *      (found, active, not pending, the timeline ended the previous tick at this tick's start within
 *      1 ns and ends this tick at its end); the first tick, a mid-tick start, a pending tick, an
 *      inactive source, a missing one and a reset all give silence;
 *   2. a source not taken for CB_SHARE_ABSENT_END_NS reads absent_long (the dock ends its session:
 *      STALE), and a taken tick clears it;
 *   3. the two-emitter case from the live log, end to end through the dock's own decode + ring
 *      pairing + rolling cluster: decoding the whole mix locks on the clip (the bug, kept as the
 *      premise); decoding the measurement source's share locks on the room marker, at the SAME
 *      offset as a mix that carries the room marker alone (the program timeline is kept);
 *   4. the clip-only program (`mbc` not in it): the mix locks on the clip, the share path reads
 *      absent_long within a second and never locks;
 *   5. cb_measure_share_fill writes the share when taken and silence otherwise, and never faults on
 *      pre-sized planes (the audio thread writes them every tick).
 *
 * Driven by tests/av_sync_dock_measure_share_1470.rs on every CI run. Exit 0 + "ALL PASS" = pass.
 */

#include "../src/camera-box-measure-share.hpp"
#include "../src/camera-box-audio.hpp"
#include "../src/camera-box-channel-pick.hpp"
#include "cb-marker-emitter.hpp"
#include "cb-thread-faults.hpp"

#include <cmath>
#include <cstdint>
#include <cstdio>
#include <vector>

using namespace camerabox;

static int g_failures = 0;
#define CHECK(cond, msg)                                                             \
	do {                                                                         \
		if (!(cond)) {                                                       \
			std::printf("FAIL: %s  (%s:%d)\n", msg, __FILE__, __LINE__); \
			g_failures++;                                                \
		}                                                                    \
	} while (0)

static const uint64_t NS = 1000000000ull;
static const uint32_t SR = CB_AUDIO_SAMPLE_RATE;
static const size_t FRAMES = 1024; // libobs AUDIO_OUTPUT_FRAMES

/* The tick start of block `b` on the mix clock (libobs counts frames: start + frames * 1e9 / rate). */
static uint64_t tick_ns(uint64_t t0, uint64_t b)
{
	return t0 + (b * FRAMES * NS) / SR;
}

static CbShareTick steady(uint64_t start, uint64_t end)
{
	CbShareTick t;
	t.tick_start_ns = start;
	t.tick_end_ns = end;
	t.source_found = true;
	t.source_active = true;
	t.source_pending = false;
	t.source_ts_after_ns = end;
	return t;
}

/* ---- 1 + 2: the take rule ---- */

static void test_take_rule()
{
	const uint64_t t0 = 5 * NS;
	CbMeasureShareTracker tr;

	/* The first tick has no previous timeline: its start offset is unknown. */
	CbShareDecision d = tr.observe(steady(tick_ns(t0, 0), tick_ns(t0, 1)));
	CHECK(!d.take, "take rule: the first tick is silence (its offset is unknown)");
	CHECK(!d.absent_long, "take rule: one silent tick is not an absence");

	d = tr.observe(steady(tick_ns(t0, 1), tick_ns(t0, 2)));
	CHECK(d.take, "take rule: the previous tick ended at this tick's start -> the whole tick, taken");
	d = tr.observe(steady(tick_ns(t0, 2), tick_ns(t0, 3)));
	CHECK(d.take, "take rule: steady state takes every tick");

	/* The start must be EXACT. mix_audio (obs-audio.c) skips a source whose timeline is before the
	 * tick start, even by 1 ns, while discard_audio still pops it as aligned: such a tick was NOT
	 * mixed. discard_audio leaves the timeline exactly on the tick end, so the steady state is exact;
	 * a timeline just after the start is left silent too (conservative, one tick). */
	{
		CbMeasureShareTracker t1;
		t1.observe(steady(tick_ns(t0, 0), tick_ns(t0, 1) - 1));
		CHECK(!t1.observe(steady(tick_ns(t0, 1), tick_ns(t0, 2))).take,
		      "take rule: a timeline 1 ns before the tick start was not mixed (mix_audio skips it)");
		CbMeasureShareTracker t2;
		CbShareTick late1 = steady(tick_ns(t0, 0), tick_ns(t0, 1));
		late1.source_ts_after_ns = tick_ns(t0, 1) + 1;
		t2.observe(late1);
		CHECK(!t2.observe(steady(tick_ns(t0, 1), tick_ns(t0, 2))).take,
		      "take rule: a timeline off the tick start is silence (only the exact steady state is taken)");
	}
	/* The end: the dock computes it as start + floor(frames * 1e9 / rate); libobs counts its tick
	 * ends from the frame total, so its end is the same or 1 ns later -- never earlier. */
	{
		CbMeasureShareTracker t3;
		t3.observe(steady(tick_ns(t0, 0), tick_ns(t0, 1)));
		CbShareTick plus1 = steady(tick_ns(t0, 1), tick_ns(t0, 2));
		plus1.source_ts_after_ns = tick_ns(t0, 2) + 1;
		CHECK(t3.observe(plus1).take, "take rule: libobs's tick end 1 ns after the dock's is the whole tick");
		CbMeasureShareTracker t4;
		t4.observe(steady(tick_ns(t0, 0), tick_ns(t0, 1)));
		CbShareTick minus1 = steady(tick_ns(t0, 1), tick_ns(t0, 2));
		minus1.source_ts_after_ns = tick_ns(t0, 2) - 1;
		CHECK(!t4.observe(minus1).take, "take rule: a timeline that stops before the tick end is not whole");
		CbShareTick plus2 = steady(tick_ns(t0, 2), tick_ns(t0, 3));
		plus2.source_ts_after_ns = tick_ns(t0, 3) + 2;
		CbMeasureShareTracker t5;
		t5.observe(steady(tick_ns(t0, 1), tick_ns(t0, 2)));
		CHECK(!t5.observe(plus2).take, "take rule: 2 ns past the tick end is not this tick's end");
	}

	/* A source that started mid-tick: libobs placed it at an offset the dock cannot see any more. */
	{
		CbMeasureShareTracker t3;
		CbShareTick late = steady(tick_ns(t0, 0), tick_ns(t0, 1));
		late.source_ts_after_ns = tick_ns(t0, 1) + 7 * NS / 1000; // started 7 ms into the next tick
		t3.observe(late);
		CHECK(!t3.observe(steady(tick_ns(t0, 1), tick_ns(t0, 2))).take,
		      "take rule: a mid-tick start is silence for that tick");
		CHECK(t3.observe(steady(tick_ns(t0, 2), tick_ns(t0, 3))).take,
		      "take rule: the tick after a mid-tick start is whole again");
	}

	/* A tick whose timeline does not end at the tick end was not consumed whole. */
	{
		CbMeasureShareTracker t4;
		t4.observe(steady(tick_ns(t0, 0), tick_ns(t0, 1)));
		CbShareTick short_end = steady(tick_ns(t0, 1), tick_ns(t0, 2));
		short_end.source_ts_after_ns = tick_ns(t0, 2) - 5000;
		CHECK(!t4.observe(short_end).take, "take rule: a timeline that stops before the tick end is silence");
	}

	/* Not rendered this tick: pending, inactive, missing. Each is silence; the tick after a pending
	 * or inactive tick is whole again (discard_audio advanced the timeline to the tick end anyway). */
	struct Case {
		const char *what;
		bool found, active, pending;
	} cases[] = {
		{"pending", true, true, true},
		{"inactive (not in the program)", true, false, false},
		{"missing", false, false, false},
	};
	for (size_t i = 0; i < sizeof(cases) / sizeof(cases[0]); i++) {
		CbMeasureShareTracker t5;
		t5.observe(steady(tick_ns(t0, 0), tick_ns(t0, 1)));
		CbShareTick bad = steady(tick_ns(t0, 1), tick_ns(t0, 2));
		bad.source_found = cases[i].found;
		bad.source_active = cases[i].active;
		bad.source_pending = cases[i].pending;
		CHECK(!t5.observe(bad).take, cases[i].what);
		const bool back = t5.observe(steady(tick_ns(t0, 2), tick_ns(t0, 3))).take;
		if (cases[i].found)
			CHECK(back, "take rule: the tick after a pending / inactive tick is whole again");
		else
			CHECK(!back, "take rule: a source found again has no previous timeline yet");
	}

	/* A new source object: reset() forgets the old timeline. */
	{
		CbMeasureShareTracker t6;
		t6.observe(steady(tick_ns(t0, 0), tick_ns(t0, 1)));
		t6.reset();
		CHECK(!t6.observe(steady(tick_ns(t0, 1), tick_ns(t0, 2))).take,
		      "take rule: after reset() the first tick is silence again");
	}

	/* 2. absent_long: a source not taken for a second ends the session; a taken tick clears it. */
	{
		CbMeasureShareTracker t7;
		uint64_t b = 0;
		bool long_seen_early = false;
		const uint64_t start = tick_ns(t0, 0);
		CbShareDecision last;
		last.take = false;
		last.absent_long = false;
		for (; tick_ns(t0, b + 1) - start < CB_SHARE_ABSENT_END_NS + 25 * NS / 1000; b++) {
			CbShareTick off = steady(tick_ns(t0, b), tick_ns(t0, b + 1));
			off.source_active = false;
			last = t7.observe(off);
			if (last.absent_long && tick_ns(t0, b + 1) - start < CB_SHARE_ABSENT_END_NS)
				long_seen_early = true;
		}
		CHECK(!long_seen_early, "absent_long: not before CB_SHARE_ABSENT_END_NS of not taken");
		CHECK(last.absent_long, "absent_long: a source not taken for a second ends the session");
		t7.observe(steady(tick_ns(t0, b), tick_ns(t0, b + 1)));
		b++;
		CbShareDecision back = t7.observe(steady(tick_ns(t0, b), tick_ns(t0, b + 1)));
		CHECK(back.take && !back.absent_long, "absent_long: a taken tick clears it");
	}
}

/* ---- 3 + 4: the live two-emitter case through the dock's decode, ring and cluster ---- */

/* The dock's ring lap resolution (sync-test-output-audio.cpp resolve_ring_lap_offset_ns, mirrored by
 * src/qpsk_marker.rs): the raw difference reduced into (-cycle/2, +cycle/2]. */
static int64_t lap_offset_ns(int64_t audio_ns, int64_t video_ns, int64_t cycle)
{
	int64_t r = (audio_ns - video_ns) % cycle;
	if (r < 0)
		r += cycle;
	if (r > cycle / 2)
		r -= cycle;
	return r;
}

/* The scene, on one clock (ns, the mix clock):
 *   - the painter ticks at 60 Hz from P0; tick k is on program video at V(k) = P0 + k/60 + LV;
 *   - the room marker (on `mbc`) for painter tick k (k % 30 == 0) is heard ROOM_LEAD before its
 *     picture: video - audio = +50 ms, the gate convention; acoustic: +-8 ms of timing jitter, 0.3
 *     gain, and one in three markers lost (the live room cluster had fewer readings than the clip's);
 *   - the clip marker for clip tick j (j % 30 == 0) plays at C0 + j/60, exactly (digital), gain 0.8.
 * CLIP_PHASE puts the clip's markers 450 ms after the room's, and its pairing with the painter's ring
 * at +900 ms (the dock's live -900.9 ms in the gate convention). */
static const int64_t P0 = 123456789;
static const int64_t LV = 1000000000;
static const int64_t ROOM_LEAD = 50000000;
static const int64_t CLIP_PHASE = 900000000;
static const int64_t TICK_NUM = NS; // tick k at k * TICK_NUM / 60
static const int64_t CYCLE = (256 * (int64_t)NS) / 60;

static int64_t painter_video_ns(int64_t k)
{
	return P0 + (k * TICK_NUM) / 60 + LV;
}

static uint32_t lcg(uint32_t &s)
{
	s = s * 1664525u + 1013904223u;
	return s >> 8;
}

struct Signals {
	std::vector<float> room; // the measurement source's share (`mbc`)
	std::vector<float> clip; // the measurement clip, another source of the same mix
};

static Signals make_signals(uint64_t t0, size_t blocks)
{
	Signals s;
	const size_t n = blocks * FRAMES;
	s.room.assign(n, 0.0f);
	s.clip.assign(n, 0.0f);
	const int64_t end = (int64_t)tick_ns(t0, blocks);
	uint32_t rng = 1470u;
	for (int64_t k = 0;; k += 30) {
		const int64_t a = painter_video_ns(k) - ROOM_LEAD + ((int64_t)(lcg(rng) % 16001u) - 8000) * 1000;
		if (a >= end)
			break;
		const bool lost = lcg(rng) % 3u == 0u;
		if (a < (int64_t)t0 || lost)
			continue;
		cbtest::add_marker(s.room, (size_t)(((a - (int64_t)t0) * (int64_t)SR) / (int64_t)NS),
				   (uint8_t)(k & 0xFF), 0.3f);
	}
	const int64_t c0 = P0 + LV + CLIP_PHASE;
	for (int64_t j = 0;; j += 30) {
		const int64_t a = c0 + (j * TICK_NUM) / 60;
		if (a >= end)
			break;
		if (a < (int64_t)t0)
			continue;
		cbtest::add_marker(s.clip, (size_t)(((a - (int64_t)t0) * (int64_t)SR) / (int64_t)NS),
				   (uint8_t)(j & 0xFF), 0.8f);
	}
	return s;
}

struct Run {
	CbAvOffset last;     // the cluster after the last marker
	size_t markers;      // markers decoded
	bool absent_long;    // the share path's absence verdict at the end
};

/* One block stream through the dock: the picker decode, the ring lookup (the newest painter frame
 * with that low byte already on program video when the block is decoded, 20 ms of slack), the lap
 * resolution and the rolling cluster. `share_mode` = the fix: each block is the measurement source's
 * share per the tracker (`mbc_in_program` = whether it is in the program), else the whole mix. */
static Run run_dock(const std::vector<float> &mix, const std::vector<float> &room, uint64_t t0, size_t blocks,
		    bool share_mode, bool mbc_in_program)
{
	ChannelMarkerPicker picker = ChannelMarkerPicker::dock(1, SR, CB_AUDIO_CARRIER_HZ, CB_AUDIO_C);
	RollingOffsetCluster cluster = RollingOffsetCluster::dock();
	CbMeasureShareTracker tracker;
	std::vector<float> block(FRAMES, 0.0f);
	Run r;
	r.last.ok = false;
	r.last.offset_ms = 0.0;
	r.last.matched = 0;
	r.last.mad_ms = 0.0;
	r.markers = 0;
	r.absent_long = false;
	uint64_t pushed = 0;
	for (size_t b = 0; b < blocks; b++) {
		const uint64_t start = tick_ns(t0, b), end = tick_ns(t0, b + 1);
		if (share_mode) {
			CbShareTick t = steady(start, end);
			t.source_active = mbc_in_program;
			const CbShareDecision d = tracker.observe(t);
			r.absent_long = d.absent_long;
			const float *share[1] = {&room[b * FRAMES]};
			float *out[1] = {block.data()};
			cb_measure_share_fill(d.take, share, 1, FRAMES, out);
		} else {
			for (size_t i = 0; i < FRAMES; i++)
				block[i] = mix[b * FRAMES + i];
		}
		const float *planes[1] = {block.data()};
		const uint64_t base = pushed;
		std::vector<std::pair<uint64_t, uint8_t>> m = picker.push(planes, FRAMES);
		pushed += FRAMES;
		for (size_t k = 0; k < m.size(); k++) {
			r.markers++;
			const int64_t rel = (int64_t)m[k].first - (int64_t)base;
			const int64_t a = (int64_t)start + (int64_t)std::llround((double)rel * 1e9 / (double)SR);
			/* the newest painter tick with this low byte already on program video at decode time */
			const int64_t now = (int64_t)end + 20000000;
			int64_t kk = (int64_t)m[k].second;
			if (painter_video_ns(kk) > now)
				continue;
			while (painter_video_ns(kk + 256) <= now)
				kk += 256;
			const int64_t off = lap_offset_ns(a, painter_video_ns(kk), CYCLE);
			r.last = cluster.push((uint64_t)a, (double)off / 1e6);
		}
	}
	return r;
}

static void test_two_emitters_and_clip_only()
{
	const uint64_t t0 = 30 * NS;
	const size_t blocks = (size_t)(90ull * SR / FRAMES); // 90 s
	const Signals s = make_signals(t0, blocks);
	std::vector<float> mix(s.room.size());
	for (size_t i = 0; i < mix.size(); i++)
		mix[i] = s.room[i] + s.clip[i];

	/* Premise (the bug): the whole mix locks on the clip's digital cluster. */
	const Run whole = run_dock(mix, s.room, t0, blocks, false, true);
	std::printf("two emitters, whole mix: ok=%d offset=%.1f ms matched=%zu mad=%.1f markers=%zu\n",
		    (int)whole.last.ok, whole.last.offset_ms, whole.last.matched, whole.last.mad_ms, whole.markers);
	CHECK(whole.last.ok && std::fabs(whole.last.offset_ms - 900.0) < 5.0 && whole.last.mad_ms < 2.0,
	      "premise: the whole mix locks on the clip's +900 ms digital cluster (the live -900.9 ms)");

	/* The room marker alone in the mix: the reference the fix must reproduce. */
	const Run room_only = run_dock(s.room, s.room, t0, blocks, false, true);
	std::printf("room marker alone in the mix: ok=%d offset=%.1f ms matched=%zu mad=%.1f\n", (int)room_only.last.ok,
		    room_only.last.offset_ms, room_only.last.matched, room_only.last.mad_ms);
	CHECK(room_only.last.ok && std::fabs(room_only.last.offset_ms + 50.0) < 5.0,
	      "reference: the room marker alone locks at -50 ms raw (+50 ms video - audio)");

	/* The fix: the measurement source's share locks on the room marker, at the same offset. */
	const Run share = run_dock(mix, s.room, t0, blocks, true, true);
	std::printf("two emitters, measurement source share: ok=%d offset=%.1f ms matched=%zu mad=%.1f\n",
		    (int)share.last.ok, share.last.offset_ms, share.last.matched, share.last.mad_ms);
	CHECK(share.last.ok && std::fabs(share.last.offset_ms + 50.0) < 5.0,
	      "fix: the measurement source's share locks on the room marker (-50 ms raw), never the clip");
	CHECK(share.last.ok && room_only.last.ok && std::fabs(share.last.offset_ms - room_only.last.offset_ms) < 1.0,
	      "fix: the share sits on the program timeline (the same offset as the room marker alone in the mix)");
	CHECK(!share.absent_long, "fix: a measurement source in the program is never absent");

	/* Clip only (the source not in the program, the stream-pp design): the mix locks on the clip, the
	 * share path ends its session (STALE) and never locks. */
	std::vector<float> clip_mix = s.clip;
	const Run clip_whole = run_dock(clip_mix, s.room, t0, blocks, false, true);
	CHECK(clip_whole.last.ok && std::fabs(clip_whole.last.offset_ms - 900.0) < 5.0,
	      "premise: a clip-only mix locks on the clip");
	const Run clip_share = run_dock(clip_mix, s.room, t0, blocks, true, false);
	std::printf("clip only, measurement source share: ok=%d markers=%zu absent_long=%d\n", (int)clip_share.last.ok,
		    clip_share.markers, (int)clip_share.absent_long);
	CHECK(!clip_share.last.ok && clip_share.markers == 0,
	      "fix: with the measurement source out of the program nothing is decoded, nothing locks");
	CHECK(clip_share.absent_long, "fix: the measurement source out of the program reads absent_long (STALE)");
}

/* ---- 5: the fill ---- */

static void test_fill()
{
	std::vector<float> a(FRAMES), b(FRAMES);
	for (size_t i = 0; i < FRAMES; i++) {
		a[i] = (float)i;
		b[i] = -(float)i;
	}
	std::vector<float> o0(FRAMES, 9.0f), o1(FRAMES, 9.0f);
	const float *share[2] = {a.data(), b.data()};
	float *out[2] = {o0.data(), o1.data()};
	cb_measure_share_fill(true, share, 2, FRAMES, out);
	CHECK(o0[17] == 17.0f && o1[FRAMES - 1] == -(float)(FRAMES - 1), "fill: a taken tick is the share");
	cb_measure_share_fill(false, share, 2, FRAMES, out);
	bool silent = true;
	for (size_t i = 0; i < FRAMES; i++)
		silent = silent && o0[i] == 0.0f && o1[i] == 0.0f;
	CHECK(silent, "fill: a tick not taken is silence (never the mix)");
	const float *none[2] = {nullptr, nullptr};
	cb_measure_share_fill(true, none, 2, FRAMES, out);
	CHECK(o0[3] == 0.0f, "fill: a missing plane is silence");

	/* The planes are sized before the audio thread writes them: no fault on a fill. */
	std::vector<float> p0, p1;
	p0.assign(FRAMES, 0.0f);
	p1.assign(FRAMES, 0.0f);
	float *pre[2] = {p0.data(), p1.data()};
	cb_measure_share_fill(true, share, 2, FRAMES, pre); // warm-up: the fill's own code pages
	const uint64_t f0 = cb_thread_page_faults();
	for (int k = 0; k < 100; k++)
		cb_measure_share_fill((k & 1) != 0, share, 2, FRAMES, pre);
	const uint64_t faults = cb_thread_page_faults() - f0;
#if CB_THREAD_FAULTS_EXACT
	CHECK(faults == 0, "fill: no page fault writing pre-sized planes");
#else
	std::printf("fill: %llu faults (sanitizer build, report only)\n", (unsigned long long)faults);
#endif
}

int main()
{
	setvbuf(stdout, nullptr, _IOLBF, 0);
	test_fill();
	test_take_rule();
	test_two_emitters_and_clip_only();
	if (g_failures == 0) {
		std::printf("measure-share-selftest: ALL PASS\n");
		return 0;
	}
	std::printf("measure-share-selftest: %d FAILED\n", g_failures);
	return 1;
}
