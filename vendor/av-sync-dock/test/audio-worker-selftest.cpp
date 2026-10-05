/*
 * camera-box A/V-sync dock -- audio decode gate + worker FIFO self-test (issue 1381).
 *
 * `camera-box-audio-worker.hpp` moves the dock's camera-box audio decode off libobs's audio thread
 * and gates it on a fresh test signal. Live 27.9.2026 the decode ran on that thread for the whole
 * program mix, latched by one QR, and the resolume cg OBS mixer fell 13-22 s behind real time.
 *
 * This self-test pins the policy the fix rests on:
 *   1. the gate: off outside camera-box mode, off without the measurement source, off once the last
 *      QR is older than the freshness window, on at the window edge and for a QR stamped after the
 *      block;
 *   2. blocks reach the worker in publish order, with their own timestamps and samples, the first
 *      block of a session flagged CB_AUDIO_GAP_SESSION;
 *   3. the producer is never blocked by a slow worker (with a 20 ms handler, 95 % of publishes take
 *      under 2 ms and none takes 15 ms -- a lock held across the handler blocks most of them for up to
 *      20 ms); a full FIFO drops the NEW block and counts it, and accepted + dropped == published;
 *   4. a dropped block flags the next one CB_AUDIO_GAP_DROPPED, counted as a reset, and with the
 *      dock's reset in on_gap a marker cut by the dropped block is NOT decoded -- while without the
 *      reset the two halves stitch into a (false) marker;
 *   5. every end_session() hands the worker one on_session_end, after the blocks published before
 *      it and before the blocks published after it -- two sessions that end inside the worker's
 *      backlog get both ends, in order; outside a session it is a no-op; a session whose blocks were
 *      all dropped merges its end into the previous one (the worker never saw it);
 *   6. stop() waits for an in-flight block and joins; publish() after stop() is a no-op; a restarted
 *      FIFO starts a new session; on_thread_start runs once on the worker;
 *   7. the per-block handling time is measured (max and sum, read-and-reset);
 *   8. no publish() after start() takes a page fault on the producer thread (getrusage
 *      RUSAGE_THREAD, cb-thread-faults.hpp): start() makes every slot plane resident, so libobs's
 *      audio thread never writes a slot page for the first time.
 *
 * Dependency-free (STL + threads): `g++ -std=c++11 -O2 -Wall -Wextra -Werror -pthread`.
 * Driven by tests/av_sync_dock_audio_worker_1381.rs on every CI run. Exit 0 + "ALL PASS" = pass.
 */

#include "../src/camera-box-audio-worker.hpp"
#include "../src/camera-box-channel-pick.hpp"
#include "cb-marker-emitter.hpp"
#include "cb-thread-faults.hpp"

#include <algorithm>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <cstdio>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

using namespace camerabox;
using steady = std::chrono::steady_clock;

static int g_failures = 0;
#define CHECK(cond, msg)                                                             \
	do {                                                                         \
		if (!(cond)) {                                                       \
			std::printf("FAIL: %s  (%s:%d)\n", msg, __FILE__, __LINE__); \
			g_failures++;                                                \
		}                                                                    \
	} while (0)

static const size_t BLOCK = 1024;

static double ms_since(steady::time_point t0)
{
	return std::chrono::duration<double, std::milli>(steady::now() - t0).count();
}

/* A gate the worker waits at inside a handler, so the test controls what is queued behind it.
 * Both waits are bounded (5 s) and a timeout is a FAIL: a FIFO that deadlocks its producer (say, a
 * lock held across the handler) then fails the run instead of hanging it. */
struct Latch {
	std::mutex m;
	std::condition_variable cv;
	bool open = false;
	bool entered = false;
	void wait_inside()
	{
		std::unique_lock<std::mutex> lock(m);
		entered = true;
		cv.notify_all();
		CHECK(cv.wait_for(lock, std::chrono::seconds(5), [this]() { return open; }),
		      "latch: the test released the worker within 5 s");
	}
	void wait_entered()
	{
		std::unique_lock<std::mutex> lock(m);
		CHECK(cv.wait_for(lock, std::chrono::seconds(5), [this]() { return entered; }),
		      "latch: the worker entered the handler within 5 s");
	}
	void release()
	{
		std::lock_guard<std::mutex> lock(m);
		open = true;
		cv.notify_all();
	}
};

/* Wait (bounded) until `fifo` handled `n` blocks. */
static bool wait_taken(const CbAudioBlockFifo &fifo, uint64_t n)
{
	const steady::time_point t0 = steady::now();
	while (fifo.taken() < n) {
		if (ms_since(t0) > 5000)
			return false;
		std::this_thread::sleep_for(std::chrono::milliseconds(1));
	}
	return true;
}

/* 8. No publish() after start() takes a page fault on the producer thread -- libobs's audio thread
 * in the dock. A page fault is kernel work in the faulting thread (a fresh page zeroed, a reclaim
 * under memory pressure), and a reserve()-only start() left the first write of every slot page to
 * publish(): 385 faults over the first 64 pushes of a 4096-frame stereo FIFO on dev1's glibc, on
 * every session start. The 1024-frame FIFO is the dock's own AUDIO_OUTPUT_FRAMES block; on glibc
 * malloc's chunk headers already touch nearly every page of a one-page plane, so that case faults 0-1
 * times even without the pre-fault, and the multi-page case is what proves start() on any allocator
 * (the dock runs on the Windows heap).
 *
 * Deterministic, never timed: the worker is held inside block 0, so the next 63 publishes fill every
 * slot once with nothing dropped, and each publish is bracketed by the producer thread's own fault
 * count. A warm-up FIFO pages publish()'s own code in before anything is counted, and every FIFO
 * here lives to the end of the test, so no FIFO under test can reuse another one's resident memory.
 * main() runs this first, before the other tests free FIFO memory a new FIFO could reuse. */
static void test_publish_never_faults_after_start()
{
	const size_t nch = 2;
	const size_t sizes[2] = {BLOCK, 4 * BLOCK};
	std::vector<float> src(4 * BLOCK, 0.25f); // one source block, written (resident) before any count
	const float *planes[2] = {src.data(), src.data()};

	CbAudioBlockFifo warm(2);
	CbAudioBlockFifo::Handlers wh;
	wh.process = [](const CbAudioBlock &) {};
	CHECK(warm.start(wh, nch, 4 * BLOCK), "faults: warm-up start");
	CHECK(warm.publish(planes, nch, 4 * BLOCK, 0), "faults: warm-up publish");
	CHECK(wait_taken(warm, 1), "faults: warm-up block handled");

	CbAudioBlockFifo fifos[2];
	Latch latches[2];
	for (size_t s = 0; s < 2; s++) {
		const size_t frames = sizes[s];
		Latch &latch = latches[s];
		CbAudioBlockFifo::Handlers h;
		h.process = [&latch](const CbAudioBlock &b) {
			if (b.timestamp == 0)
				latch.wait_inside();
		};
		CHECK(fifos[s].start(h, nch, frames), "faults: start");
		uint64_t faults = 0, accepted = 0;
		for (uint64_t k = 0; k < CB_AUDIO_FIFO_SLOTS; k++) {
			const uint64_t f0 = cb_thread_page_faults();
			const bool ok = fifos[s].publish(planes, nch, frames, k);
			faults += cb_thread_page_faults() - f0;
			if (ok)
				accepted++;
			if (k == 0)
				latch.wait_entered();
		}
		latch.release();
		std::printf("faults: %zu-frame stereo FIFO, %llu of %zu slots written, %llu page fault(s) on the "
			    "producer thread\n",
			    frames, (unsigned long long)accepted, CB_AUDIO_FIFO_SLOTS, (unsigned long long)faults);
		CHECK(accepted == CB_AUDIO_FIFO_SLOTS,
		      "faults: every slot was written once (the worker held block 0, nothing dropped)");
		CHECK(faults == 0, "faults: no publish() after start() takes a page fault on the producer thread");
		CHECK(wait_taken(fifos[s], accepted), "faults: every block handled");
	}
	for (size_t s = 0; s < 2; s++)
		fifos[s].stop();
	warm.stop();
}

static void test_gate()
{
	const uint64_t S = 1000000000ull, FRESH = 20 * S;
	CHECK(cb_audio_decode_gate(false, true, 100 * S, 99 * S, FRESH) == CbAudioGate::NotCameraBoxMode,
	      "gate: off outside camera-box mode");
	CHECK(cb_audio_decode_gate(true, false, 100 * S, 99 * S, FRESH) == CbAudioGate::NoMeasurementSource,
	      "gate: off without the measurement source");
	CHECK(cb_audio_decode_gate(true, true, 100 * S, 79 * S, FRESH) == CbAudioGate::TestSignalStale,
	      "gate: off once the last QR is older than the window");
	CHECK(cb_audio_decode_gate(true, true, 100 * S, 80 * S, FRESH) == CbAudioGate::Open,
	      "gate: on at the window edge");
	CHECK(cb_audio_decode_gate(true, true, 100 * S, 101 * S, FRESH) == CbAudioGate::Open,
	      "gate: on for a QR stamped after the block");
	CHECK(cb_audio_decode_gate(true, true, 0, 0, FRESH) == CbAudioGate::Open, "gate: on at time zero");
	CHECK(std::string(cb_audio_gate_text(CbAudioGate::TestSignalStale)).find("stale") != std::string::npos,
	      "gate: the stale reason reads as stale");
}

static void test_order_and_contents()
{
	CbAudioBlockFifo fifo(8);
	std::mutex m;
	std::vector<uint64_t> seen_ts;
	std::vector<unsigned> seen_gap;
	bool samples_ok = true;
	CbAudioBlockFifo::Handlers h;
	h.process = [&](const CbAudioBlock &b) {
		std::lock_guard<std::mutex> lock(m);
		seen_ts.push_back(b.timestamp);
		seen_gap.push_back(b.gap);
		for (size_t c = 0; c < b.channels; c++)
			for (size_t i = 0; i < b.frames; i++)
				if (b.planes[c][i] != (float)(b.timestamp * 10 + c) + (float)i * 0.5f)
					samples_ok = false;
		if (b.channels != 2 || b.frames != BLOCK)
			samples_ok = false;
	};
	CHECK(fifo.start(h, 2, BLOCK), "order: start");
	std::vector<float> l(BLOCK), r(BLOCK);
	for (uint64_t k = 0; k < 50; k++) {
		for (size_t i = 0; i < BLOCK; i++) {
			l[i] = (float)(k * 10 + 0) + (float)i * 0.5f;
			r[i] = (float)(k * 10 + 1) + (float)i * 0.5f;
		}
		const float *planes[2] = {l.data(), r.data()};
		while (!fifo.publish(planes, 2, BLOCK, k))
			std::this_thread::sleep_for(std::chrono::milliseconds(1)); // full: retry (the dock never does)
	}
	CHECK(wait_taken(fifo, 50), "order: every block handled");
	fifo.stop();
	bool in_order = seen_ts.size() == 50;
	for (size_t k = 0; in_order && k < seen_ts.size(); k++)
		in_order = seen_ts[k] == k;
	CHECK(in_order, "order: blocks arrive in publish order with their own timestamps");
	CHECK(samples_ok, "order: every channel's samples arrive intact");
	CHECK(!seen_gap.empty() && seen_gap[0] == CB_AUDIO_GAP_SESSION, "order: the first block starts a session");
	bool quiet = true;
	for (size_t k = 1; k < seen_gap.size(); k++)
		quiet = quiet && seen_gap[k] == 0;
	/* The retries above never drop, since a refused publish while stopped/full only counts when full. */
	CHECK(quiet == (fifo.dropped() == 0), "order: no gap flag without a drop");
}

static void test_producer_never_blocked()
{
	CbAudioBlockFifo fifo(8);
	CbAudioBlockFifo::Handlers h;
	h.process = [](const CbAudioBlock &) { std::this_thread::sleep_for(std::chrono::milliseconds(20)); };
	CHECK(fifo.start(h, 2, BLOCK), "producer: start");
	std::vector<float> l(BLOCK, 0.1f), r(BLOCK, 0.2f);
	const float *planes[2] = {l.data(), r.data()};
	std::vector<double> ms;
	uint64_t accepted = 0;
	const uint64_t published = 200;
	for (uint64_t k = 0; k < published; k++) {
		const steady::time_point t0 = steady::now();
		if (fifo.publish(planes, 2, BLOCK, k))
			accepted++;
		ms.push_back(ms_since(t0));
		/* Paced like a real audio thread (faster, so the FIFO still fills): a publish that had to
		 * wait for the worker then shows up in most samples, not only the first. */
		std::this_thread::sleep_for(std::chrono::milliseconds(1));
	}
	std::sort(ms.begin(), ms.end());
	const double p95 = ms[ms.size() * 95 / 100], worst = ms.back();
	/* A scheduler hiccup on a loaded CI runner can stretch one publish; a lock held across the
	 * handler stretches most of them (to up to 20 ms). */
	CHECK(p95 < 2.0, "producer: 95 % of publishes take under 2 ms while the worker decodes for 20 ms");
	CHECK(worst < 15.0, "producer: no publish waits for a 20 ms decode");
	CHECK(fifo.dropped() > 0, "producer: a full FIFO drops blocks");
	CHECK(accepted + fifo.dropped() == published, "producer: accepted + dropped == published");
	CHECK(wait_taken(fifo, accepted), "producer: every accepted block is handled");
	fifo.stop();
	std::printf("producer: publish p95 %.3f ms, worst %.3f ms, %llu accepted, %llu dropped\n", p95, worst,
		    (unsigned long long)accepted, (unsigned long long)fifo.dropped());
}

/* The dock's worker glue in miniature: on_gap resets the picker (unless `reset` is off), process
 * pushes the block. Blocks b0 (held in the handler), b1, b2 fill a 3-slot FIFO; J is dropped;
 * b3 follows. b2 ends with the first 500 samples of a marker and b3 starts with the rest. Returns
 * the markers decoded and fills `gaps` with the gap flag each block arrived with. */
static std::vector<std::pair<uint64_t, uint8_t>> run_stitch(bool reset, std::vector<unsigned> &gaps,
							    uint64_t *resets, uint64_t *dropped)
{
	const size_t cut = 500;
	std::vector<float> marker = cbtest::marker_signal(77);
	std::vector<float> b0(BLOCK, 0.f), b1(BLOCK, 0.f), b2(BLOCK, 0.f), junk(BLOCK, 0.f), b3(BLOCK, 0.f);
	for (size_t j = 0; j < cut; j++)
		b2[BLOCK - cut + j] = marker[j];
	for (size_t j = cut; j < marker.size(); j++)
		b3[j - cut] = marker[j];
	for (size_t i = 0; i < BLOCK; i++)
		junk[i] = 0.3f * (float)((i * 7919u) % 101u) / 101.0f - 0.15f;

	ChannelMarkerPicker picker =
		ChannelMarkerPicker::dock(1, CB_AUDIO_SAMPLE_RATE, CB_AUDIO_CARRIER_HZ, CB_AUDIO_C);
	std::vector<std::pair<uint64_t, uint8_t>> got;
	Latch latch;
	CbAudioBlockFifo fifo(3);
	CbAudioBlockFifo::Handlers h;
	h.on_gap = [&](const CbAudioBlock &b) {
		gaps.push_back(b.gap);
		if (reset)
			picker.reset_window();
	};
	h.process = [&](const CbAudioBlock &b) {
		if (b.timestamp == 0)
			latch.wait_inside();
		if (!b.gap)
			gaps.push_back(0);
		const float *planes[1] = {b.planes[0].data()};
		std::vector<std::pair<uint64_t, uint8_t>> m = picker.push(planes, b.frames);
		got.insert(got.end(), m.begin(), m.end());
	};
	fifo.start(h, 1, BLOCK);
	const float *p0[1] = {b0.data()}, *p1[1] = {b1.data()}, *p2[1] = {b2.data()}, *pj[1] = {junk.data()},
		    *p3[1] = {b3.data()};
	fifo.publish(p0, 1, BLOCK, 0);
	latch.wait_entered();                                 // b0 is in the handler
	fifo.publish(p1, 1, BLOCK, 1);                        // queued
	fifo.publish(p2, 1, BLOCK, 2);                        // queued: the FIFO is full now
	const bool j_accepted = fifo.publish(pj, 1, BLOCK, 3); // dropped
	latch.release();
	CHECK(!j_accepted, "stitch: the block published into a full FIFO is dropped");
	CHECK(wait_taken(fifo, 3), "stitch: b0..b2 handled");
	fifo.publish(p3, 1, BLOCK, 4);
	CHECK(wait_taken(fifo, 4), "stitch: b3 handled");
	fifo.stop();
	*resets = fifo.resets();
	*dropped = fifo.dropped();
	return got;
}

static void test_drop_resets_and_never_stitches()
{
	std::vector<unsigned> gaps;
	uint64_t resets = 0, dropped = 0;
	std::vector<std::pair<uint64_t, uint8_t>> got = run_stitch(true, gaps, &resets, &dropped);
	CHECK(dropped == 1, "drop: one block dropped");
	CHECK(resets == 1, "drop: the dropped block is counted as one reset");
	CHECK(gaps.size() == 4 && gaps[0] == CB_AUDIO_GAP_SESSION && gaps[1] == 0 && gaps[2] == 0 &&
		      gaps[3] == CB_AUDIO_GAP_DROPPED,
	      "drop: the block after the drop carries CB_AUDIO_GAP_DROPPED, only it");
	CHECK(got.empty(), "drop: no marker decoded across the dropped block");

	/* The control: the same audio without the reset stitches the two halves into a marker. */
	std::vector<unsigned> gaps2;
	std::vector<std::pair<uint64_t, uint8_t>> stitched = run_stitch(false, gaps2, &resets, &dropped);
	CHECK(stitched.size() == 1 && stitched[0].second == 77,
	      "drop control: without the reset the halves decode as marker 77");
}

static void test_sessions()
{
	std::vector<std::string> events;
	std::mutex m;
	Latch latch;
	CbAudioBlockFifo fifo(8);
	CbAudioBlockFifo::Handlers h;
	h.on_gap = [&](const CbAudioBlock &b) {
		std::lock_guard<std::mutex> lock(m);
		events.push_back("gap" + std::to_string(b.gap) + "@" + std::to_string(b.timestamp));
	};
	h.process = [&](const CbAudioBlock &b) {
		if (b.timestamp == 1)
			latch.wait_inside();
		std::lock_guard<std::mutex> lock(m);
		events.push_back("b" + std::to_string(b.timestamp));
	};
	h.on_session_end = [&](unsigned reason) {
		std::lock_guard<std::mutex> lock(m);
		events.push_back("end" + std::to_string(reason));
	};
	CHECK(fifo.start(h, 1, BLOCK), "session: start");
	fifo.end_session(9); // outside a session: nothing
	std::vector<float> x(BLOCK, 0.f);
	const float *p[1] = {x.data()};
	fifo.publish(p, 1, BLOCK, 1);
	latch.wait_entered();
	fifo.publish(p, 1, BLOCK, 2);
	fifo.end_session(3);
	fifo.end_session(8); // already ended: nothing
	fifo.publish(p, 1, BLOCK, 5);
	fifo.end_session(4); // the second session ends while the first end is still queued
	latch.release();
	/* A block published after the last end is handled after it: once b9 is handled, every end is. */
	fifo.publish(p, 1, BLOCK, 9);
	CHECK(wait_taken(fifo, 4), "session: every block handled");
	fifo.stop();
	std::string trace;
	for (size_t k = 0; k < events.size(); k++)
		trace += (k ? " " : "") + events[k];
	const std::string want = "gap2@1 b1 b2 end3 gap2@5 b5 end4 gap2@9 b9";
	CHECK(trace == want, "session: each end comes after its session's blocks and before the next session's");
	if (trace != want)
		std::printf("  trace: %s\n", trace.c_str());
}

/* A session whose every block was dropped (the FIFO stayed full) is invisible to the worker: its end
 * merges into the previous session's end (with the later reason), and the next accepted block starts
 * a session with the drop flagged. */
static void test_dropped_session_merges_its_end()
{
	std::vector<std::string> events;
	std::mutex m;
	Latch latch;
	CbAudioBlockFifo fifo(2);
	CbAudioBlockFifo::Handlers h;
	h.on_gap = [&](const CbAudioBlock &b) {
		std::lock_guard<std::mutex> lock(m);
		events.push_back("gap" + std::to_string(b.gap) + "@" + std::to_string(b.timestamp));
	};
	h.process = [&](const CbAudioBlock &b) {
		if (b.timestamp == 1)
			latch.wait_inside();
		std::lock_guard<std::mutex> lock(m);
		events.push_back("b" + std::to_string(b.timestamp));
	};
	h.on_session_end = [&](unsigned reason) {
		std::lock_guard<std::mutex> lock(m);
		events.push_back("end" + std::to_string(reason));
	};
	CHECK(fifo.start(h, 1, BLOCK), "merge: start");
	std::vector<float> x(BLOCK, 0.f);
	const float *p[1] = {x.data()};
	fifo.publish(p, 1, BLOCK, 1);
	latch.wait_entered();
	fifo.publish(p, 1, BLOCK, 2); // the 2-slot FIFO is full now
	fifo.end_session(3);
	CHECK(!fifo.publish(p, 1, BLOCK, 6), "merge: the next session's only block is dropped");
	fifo.end_session(7);
	latch.release();
	CHECK(wait_taken(fifo, 2), "merge: b1 and b2 handled");
	fifo.publish(p, 1, BLOCK, 8);
	CHECK(wait_taken(fifo, 3), "merge: b8 handled");
	fifo.stop();
	std::string trace;
	for (size_t k = 0; k < events.size(); k++)
		trace += (k ? " " : "") + events[k];
	CHECK(trace == "gap2@1 b1 b2 end7 gap3@8 b8",
	      "merge: one end for the unseen session, then a session start with the drop flagged");
	if (trace != "gap2@1 b1 b2 end7 gap3@8 b8")
		std::printf("  trace: %s\n", trace.c_str());
}

/* Many sessions end inside the worker's backlog: every end is delivered, in order, merged only over
 * the sessions whose blocks were all dropped. (The ring's slots + 1 bound is stressed by
 * test_ring_holds_slots_plus_one_ends below.) */
static void test_every_end_in_a_full_backlog()
{
	std::vector<std::string> events;
	std::mutex m;
	Latch latch;
	CbAudioBlockFifo fifo(4);
	CbAudioBlockFifo::Handlers h;
	h.process = [&](const CbAudioBlock &b) {
		if (b.timestamp == 0)
			latch.wait_inside();
		std::lock_guard<std::mutex> lock(m);
		events.push_back("b" + std::to_string(b.timestamp));
	};
	h.on_session_end = [&](unsigned reason) {
		std::lock_guard<std::mutex> lock(m);
		events.push_back("end" + std::to_string(reason));
	};
	CHECK(fifo.start(h, 1, BLOCK), "backlog: start");
	std::vector<float> x(BLOCK, 0.f);
	const float *p[1] = {x.data()};
	fifo.publish(p, 1, BLOCK, 0);
	latch.wait_entered();
	fifo.end_session(100);
	for (uint64_t k = 1; k <= 12; k++) { // 3 accepted (the FIFO holds 4, b0 included), 9 dropped
		fifo.publish(p, 1, BLOCK, k);
		fifo.end_session((unsigned)(100 + k));
	}
	latch.release();
	CHECK(wait_taken(fifo, 4), "backlog: the accepted blocks are handled");
	CHECK(fifo.publish(p, 1, BLOCK, 13), "backlog: a block after the backlog is accepted");
	CHECK(wait_taken(fifo, 5), "backlog: the block after every earlier end is handled");
	fifo.stop();
	std::string trace;
	for (size_t k = 0; k < events.size(); k++)
		trace += (k ? " " : "") + events[k];
	const std::string want = "b0 end100 b1 end101 b2 end102 b3 end112 b13";
	CHECK(trace == want, "backlog: every end is delivered in order, merged only over dropped sessions");
	if (trace != want)
		std::printf("  trace: %s\n", trace.c_str());
	CHECK(fifo.dropped() == 9, "backlog: nine blocks dropped");
}

/* The ring's (slots + 1)-th entry: pending ends have distinct `after` values within
 * [handled blocks, accepted blocks], so there are at most count + 1 of them, and the + 1 is an end
 * queued after the worker handled every block, before it retakes the lock to deliver it. Only a race
 * reaches that state (the worker woken, not yet running), so this is a stress loop: each round ends a
 * fully handled session and at once publishes + ends two more sessions into a 2-slot FIFO. Every
 * delivered event must be the expected one; a correct FIFO can never fail it, while a ring of only
 * `slots` entries overwrites its front end in the rounds that fill it (most of them on dev1). */
static void test_ring_holds_slots_plus_one_ends()
{
	const int rounds = 2000;
	std::mutex m;
	std::vector<int64_t> events; // a block = its timestamp, an end = -(reason)
	events.reserve((size_t)rounds * 6 + 1);
	CbAudioBlockFifo fifo(2);
	CbAudioBlockFifo::Handlers h;
	h.process = [&](const CbAudioBlock &b) {
		std::lock_guard<std::mutex> lock(m);
		events.push_back((int64_t)b.timestamp);
	};
	h.on_session_end = [&](unsigned reason) {
		std::lock_guard<std::mutex> lock(m);
		events.push_back(-(int64_t)reason);
	};
	CHECK(fifo.start(h, 1, BLOCK), "ring: start");
	std::vector<float> x(BLOCK, 0.f);
	const float *p[1] = {x.data()};
	std::vector<int64_t> want;
	uint64_t handled = 0;
	bool first_two_accepted = true;
	int third_dropped = 0;
	for (int r = 0; r < rounds; r++) {
		const uint64_t a = (uint64_t)r * 3 + 1;
		if (!wait_taken(fifo, handled)) // the previous round's blocks, so `a` finds a free slot
			break;
		first_two_accepted = fifo.publish(p, 1, BLOCK, a) && first_two_accepted;
		handled++;
		if (!wait_taken(fifo, handled))
			break;
		fifo.end_session((unsigned)a);
		first_two_accepted = fifo.publish(p, 1, BLOCK, a + 1) && first_two_accepted;
		fifo.end_session((unsigned)(a + 1));
		/* `taken()` counts a block before the worker frees its slot, so block `a` can still hold
		 * one: then this third block finds both slots full and is dropped, and its session (never
		 * seen by the worker) merges its end into the previous one. */
		const bool third = fifo.publish(p, 1, BLOCK, a + 2);
		fifo.end_session((unsigned)(a + 2));
		want.push_back((int64_t)a);
		want.push_back(-(int64_t)a);
		want.push_back((int64_t)(a + 1));
		if (third) {
			handled += 2;
			want.push_back(-(int64_t)(a + 1));
			want.push_back((int64_t)(a + 2));
		} else {
			handled += 1;
			third_dropped++;
		}
		want.push_back(-(int64_t)(a + 2));
	}
	const uint64_t last = (uint64_t)rounds * 3 + 1;
	CHECK(wait_taken(fifo, handled), "ring: the last round's blocks handled");
	first_two_accepted = fifo.publish(p, 1, BLOCK, last) && first_two_accepted; // after every end
	handled++;
	want.push_back((int64_t)last);
	CHECK(wait_taken(fifo, handled), "ring: every block handled");
	fifo.stop();
	CHECK(first_two_accepted, "ring: the first two blocks of a round always fit");
	std::printf("ring: %d rounds, %d with the third block dropped\n", rounds, third_dropped);
	size_t first_bad = 0;
	while (first_bad < want.size() && first_bad < events.size() && want[first_bad] == events[first_bad])
		first_bad++;
	CHECK(events.size() == want.size() && first_bad == want.size(),
	      "ring: every block and every end arrives once, in order, over 2000 racing rounds");
	if (first_bad < want.size() || events.size() != want.size())
		std::printf("  ring: %zu events (want %zu), first difference at %zu\n", events.size(), want.size(),
			    first_bad);
}

static void test_lifecycle_and_timing()
{
	std::atomic<int> starts{0};
	std::atomic<bool> on_worker{false};
	const std::thread::id main_id = std::this_thread::get_id();
	std::atomic<bool> finished{false};
	CbAudioBlockFifo fifo(4);
	CbAudioBlockFifo::Handlers h;
	h.on_thread_start = [&]() {
		starts++;
		on_worker = std::this_thread::get_id() != main_id;
	};
	std::atomic<bool> entered{false};
	h.process = [&](const CbAudioBlock &) {
		entered = true;
		std::this_thread::sleep_for(std::chrono::milliseconds(30));
		finished = true;
	};
	std::vector<unsigned> gaps;
	std::mutex gm;
	h.on_gap = [&](const CbAudioBlock &b) {
		std::lock_guard<std::mutex> lock(gm);
		gaps.push_back(b.gap);
	};
	CHECK(fifo.start(h, 1, BLOCK), "lifecycle: start");
	CHECK(!fifo.start(h, 1, BLOCK), "lifecycle: a second start is refused");
	std::vector<float> x(BLOCK, 0.f);
	const float *p[1] = {x.data()};
	fifo.publish(p, 1, BLOCK, 1);
	{
		const steady::time_point t0 = steady::now();
		while (!entered && ms_since(t0) < 5000)
			std::this_thread::sleep_for(std::chrono::milliseconds(1));
	}
	CHECK(entered, "lifecycle: the worker took the block");
	fifo.stop(); // the 30 ms block is in flight
	CHECK(finished, "lifecycle: stop() waits for the in-flight block");
	CHECK(starts == 1 && on_worker, "lifecycle: on_thread_start ran once, on the worker");
	CHECK(fifo.take_process_max_ns() >= 25000000ull, "timing: the max covers the 30 ms block");
	CHECK(fifo.take_process_max_ns() == 0, "timing: read-and-reset");
	CHECK(!fifo.publish(p, 1, BLOCK, 2), "lifecycle: publish after stop is a no-op");
	CHECK(fifo.dropped() == 0, "lifecycle: a refused publish on a stopped FIFO is not a drop");
	CHECK(fifo.start(h, 1, BLOCK), "lifecycle: a stopped FIFO starts again");
	fifo.publish(p, 1, BLOCK, 3);
	fifo.publish(p, 1, BLOCK, 4);
	CHECK(wait_taken(fifo, 3), "lifecycle: blocks after the restart are handled");
	CHECK(fifo.take_process_sum_ns() >= 55000000ull, "timing: the sum covers both 30 ms blocks");
	fifo.stop();
	std::lock_guard<std::mutex> lock(gm);
	CHECK(gaps.size() == 2 && gaps[0] == CB_AUDIO_GAP_SESSION && gaps[1] == CB_AUDIO_GAP_SESSION,
	      "lifecycle: each start begins a new session");
}

int main()
{
	/* Line-buffered: a FAIL line reaches the log even if a later check hangs or crashes. */
	std::setvbuf(stdout, nullptr, _IOLBF, 0);
	test_publish_never_faults_after_start();
	test_gate();
	test_order_and_contents();
	test_producer_never_blocked();
	test_drop_resets_and_never_stitches();
	test_sessions();
	test_dropped_session_merges_its_end();
	test_every_end_in_a_full_backlog();
	test_ring_holds_slots_plus_one_ends();
	test_lifecycle_and_timing();
	if (g_failures == 0) {
		std::printf("audio-worker-selftest: ALL PASS\n");
		return 0;
	}
	std::printf("audio-worker-selftest: %d FAILURE(S)\n", g_failures);
	return 1;
}
