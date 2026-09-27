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
 *   3. the producer is never blocked by a slow worker (a 20 ms handler never holds a publish past
 *      2 ms); a full FIFO drops the NEW block and counts it, and accepted + dropped == published;
 *   4. a dropped block flags the next one CB_AUDIO_GAP_DROPPED, counted as a reset, and with the
 *      dock's reset in on_gap a marker cut by the dropped block is NOT decoded -- while without the
 *      reset the two halves stitch into a (false) marker;
 *   5. end_session() hands the worker exactly one on_session_end, after the blocks published before
 *      it and before the blocks published after it; outside a session it is a no-op;
 *   6. stop() waits for an in-flight block and joins; publish() after stop() is a no-op; a restarted
 *      FIFO starts a new session; on_thread_start runs once on the worker;
 *   7. the per-block handling time is measured (max and sum, read-and-reset).
 *
 * Dependency-free (STL + threads): `g++ -std=c++11 -O2 -Wall -Wextra -Werror -pthread`.
 * Driven by tests/av_sync_dock_audio_worker_1381.rs on every CI run. Exit 0 + "ALL PASS" = pass.
 */

#include "../src/camera-box-audio-worker.hpp"
#include "../src/camera-box-channel-pick.hpp"
#include "cb-marker-emitter.hpp"

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

/* A gate the worker waits at inside a handler, so the test controls what is queued behind it. */
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
		cv.wait(lock, [this]() { return open; });
	}
	void wait_entered()
	{
		std::unique_lock<std::mutex> lock(m);
		cv.wait(lock, [this]() { return entered; });
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
	double worst = 0;
	uint64_t accepted = 0;
	const uint64_t published = 200;
	for (uint64_t k = 0; k < published; k++) {
		const steady::time_point t0 = steady::now();
		if (fifo.publish(planes, 2, BLOCK, k))
			accepted++;
		const double ms = ms_since(t0);
		if (ms > worst)
			worst = ms;
	}
	CHECK(worst < 2.0, "producer: a 20 ms decode never blocks a publish past 2 ms");
	CHECK(fifo.dropped() > 0, "producer: a full FIFO drops blocks");
	CHECK(accepted + fifo.dropped() == published, "producer: accepted + dropped == published");
	CHECK(wait_taken(fifo, accepted), "producer: every accepted block is handled");
	fifo.stop();
	std::printf("producer: worst publish %.3f ms, %llu accepted, %llu dropped\n", worst,
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
	fifo.end_session(4); // already ended: nothing
	fifo.publish(p, 1, BLOCK, 5);
	latch.release();
	CHECK(wait_taken(fifo, 3), "session: every block handled");
	{
		const steady::time_point t0 = steady::now();
		while (ms_since(t0) < 200)
			std::this_thread::sleep_for(std::chrono::milliseconds(5));
	}
	fifo.stop();
	std::string trace;
	for (size_t k = 0; k < events.size(); k++)
		trace += (k ? " " : "") + events[k];
	CHECK(trace == "gap2@1 b1 b2 end3 gap2@5 b5",
	      "session: the end comes after the session's blocks and before the next session's");
	if (trace != "gap2@1 b1 b2 end3 gap2@5 b5")
		std::printf("  trace: %s\n", trace.c_str());
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
	h.process = [&](const CbAudioBlock &) {
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
	std::this_thread::sleep_for(std::chrono::milliseconds(5));
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
	test_gate();
	test_order_and_contents();
	test_producer_never_blocked();
	test_drop_resets_and_never_stitches();
	test_sessions();
	test_lifecycle_and_timing();
	if (g_failures == 0) {
		std::printf("audio-worker-selftest: ALL PASS\n");
		return 0;
	}
	std::printf("audio-worker-selftest: %d FAILURE(S)\n", g_failures);
	return 1;
}
