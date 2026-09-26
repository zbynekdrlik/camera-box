/*
 * camera-box A/V-sync dock — decode-worker mailbox self-test (issue 1367).
 *
 * `camera-box-decode-mailbox.hpp` moves the dock's per-frame video decode (top-band gather + up to
 * two quirc passes, and norihiro's whole-frame quirc + marker search) off libobs's single
 * video-output thread. Live 26.9.2026 on the resolume cg OBS: with QR content on screen the
 * synchronous decode made OBS skip 28 % of output frames (the cg-obs NDI output fell to 16-18.7 fps)
 * until `sync-test-output` was stopped.
 *
 * This self-test pins the mailbox POLICY the fix rests on, with a fake 50 ms decode:
 *   1. the producer (the video thread) is never blocked for more than 2 ms by a decode;
 *   2. the latest frame wins — the decoder never sees an older frame after a newer one, and the
 *      last published frame is always decoded;
 *   3. every frame replaced before the worker took it is counted as dropped, and
 *      decoded + dropped == published;
 *   4. a decoded frame is never torn (the producer never writes the buffer the worker reads);
 *   5. stop() while a decode is in flight waits for it and joins; the destructor does the same;
 *   6. publish() after stop() is a no-op, and a stopped mailbox can be started again.
 *
 * Dependency-free (STL + threads): `g++ -std=c++11 -O2 -Wall -Wextra -Werror -pthread`.
 * Driven by tests/av_sync_dock_decode_mailbox_1367.rs on every CI run. Exit 0 + "ALL PASS" = pass.
 */

#include "../src/camera-box-decode-mailbox.hpp"

#include <atomic>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <mutex>
#include <thread>
#include <vector>

using namespace camerabox;
using steady = std::chrono::steady_clock;

static int g_failures = 0;
#define CHECK(cond, msg)                                                         \
	do {                                                                     \
		if (!(cond)) {                                                   \
			std::printf("FAIL: %s  (%s:%d)\n", msg, __FILE__, __LINE__); \
			g_failures++;                                            \
		}                                                                \
	} while (0)

struct FakeJob {
	uint64_t id = 0;
	std::vector<uint8_t> payload; // stands in for the copied top band
};

static const size_t PAYLOAD_BYTES = 64 * 1024;
static const int DECODE_MS = 50;
static const double PRODUCER_BUDGET_MS = 2.0;

static double ms_since(steady::time_point t0)
{
	return std::chrono::duration<double, std::milli>(steady::now() - t0).count();
}

static void fill_job(FakeJob &job, uint64_t id)
{
	job.id = id;
	job.payload.assign(PAYLOAD_BYTES, (uint8_t)(id & 0xFFu));
}

static bool payload_intact(const FakeJob &job)
{
	if (job.payload.size() != PAYLOAD_BYTES)
		return false;
	for (size_t i = 0; i < job.payload.size(); i++) {
		if (job.payload[i] != (uint8_t)(job.id & 0xFFu))
			return false;
	}
	return true;
}

/* Wait (bounded) for a condition the worker makes true. */
template<typename Pred> static bool wait_for(Pred pred, int timeout_ms)
{
	steady::time_point t0 = steady::now();
	while (!pred()) {
		if (ms_since(t0) > timeout_ms)
			return false;
		std::this_thread::sleep_for(std::chrono::milliseconds(1));
	}
	return true;
}

int main()
{
	/* 1-4: a 30 fps producer against a 50 ms decode. */
	{
		CbDecodeMailbox<FakeJob> mb;
		std::mutex seen_mutex;
		std::vector<uint64_t> decoded_ids;
		bool torn = false;

		CHECK(mb.start([&](FakeJob &job) {
			const bool intact = payload_intact(job);
			{
				std::lock_guard<std::mutex> lock(seen_mutex);
				decoded_ids.push_back(job.id);
				if (!intact)
					torn = true;
			}
			std::this_thread::sleep_for(std::chrono::milliseconds(DECODE_MS));
			/* Re-check after the sleep: the producer kept publishing meanwhile, so a shared
			 * buffer would have been overwritten under the decoder by now. */
			if (!payload_intact(job)) {
				std::lock_guard<std::mutex> lock(seen_mutex);
				torn = true;
			}
		}),
		      "start() succeeds on a stopped mailbox");
		CHECK(mb.running(), "running() after start()");

		const uint64_t published = 30;
		double worst_ms = 0.0;
		for (uint64_t id = 1; id <= published; id++) {
			steady::time_point t0 = steady::now();
			const bool ok = mb.publish([&](FakeJob &job) { fill_job(job, id); });
			const double dt = ms_since(t0);
			if (dt > worst_ms)
				worst_ms = dt;
			CHECK(ok, "publish() while running returns true");
			std::this_thread::sleep_for(std::chrono::milliseconds(33));
		}
		std::printf("producer worst publish = %.3f ms (budget %.1f ms, decode %d ms)\n", worst_ms,
			    PRODUCER_BUDGET_MS, DECODE_MS);
		CHECK(worst_ms <= PRODUCER_BUDGET_MS, "a 50 ms decode never blocks the producer for more than 2 ms");

		const bool last_seen = wait_for(
			[&]() {
				std::lock_guard<std::mutex> lock(seen_mutex);
				return !decoded_ids.empty() && decoded_ids.back() == published;
			},
			2000);
		CHECK(last_seen, "the last published frame is decoded (latest wins)");
		/* Let the last decode finish so taken/dropped are final. */
		std::this_thread::sleep_for(std::chrono::milliseconds(DECODE_MS + 20));
		mb.stop();
		CHECK(!mb.running(), "running() is false after stop()");

		std::vector<uint64_t> ids;
		{
			std::lock_guard<std::mutex> lock(seen_mutex);
			ids = decoded_ids;
		}
		bool increasing = true;
		for (size_t i = 1; i < ids.size(); i++) {
			if (ids[i] <= ids[i - 1])
				increasing = false;
		}
		CHECK(increasing, "the decoder never sees an older frame after a newer one");
		CHECK(!torn, "a decoded frame is never torn by a concurrent publish");
		CHECK(ids.size() < published, "a 50 ms decode against a 33 ms producer must drop frames");
		CHECK(mb.dropped() > 0, "dropped frames are counted");
		CHECK(mb.taken() == (uint64_t)ids.size(), "taken() counts the frames handed to the decoder");
		CHECK(mb.taken() + mb.dropped() == published, "decoded + dropped == published");
		std::printf("published=%llu decoded=%llu dropped=%llu\n", (unsigned long long)published,
			    (unsigned long long)mb.taken(), (unsigned long long)mb.dropped());
	}

	/* 5: stop() while a decode is in flight waits for it, then joins. */
	{
		CbDecodeMailbox<FakeJob> mb;
		std::atomic<bool> in_decode{false};
		std::atomic<bool> finished{false};
		CHECK(mb.start([&](FakeJob &) {
			in_decode = true;
			std::this_thread::sleep_for(std::chrono::milliseconds(DECODE_MS));
			finished = true;
		}),
		      "start() for the in-flight stop case");
		CHECK(mb.publish([&](FakeJob &job) { fill_job(job, 1); }), "publish one frame");
		CHECK(wait_for([&]() { return in_decode.load(); }, 1000), "the worker takes the frame");
		steady::time_point t0 = steady::now();
		mb.stop();
		CHECK(finished.load(), "stop() returns only after the in-flight decode finished");
		CHECK(ms_since(t0) < 1000.0, "stop() is bounded by one decode");

		/* 6: publish after stop is a no-op; restart works. */
		const uint64_t dropped_before = mb.dropped();
		CHECK(!mb.publish([&](FakeJob &job) { fill_job(job, 2); }), "publish() after stop() returns false");
		CHECK(mb.dropped() == dropped_before, "publish() after stop() counts nothing");

		std::atomic<uint64_t> restarted_id{0};
		CHECK(mb.start([&](FakeJob &job) { restarted_id = job.id; }), "a stopped mailbox starts again");
		CHECK(!mb.start([&](FakeJob &) {}), "start() on a running mailbox returns false");
		CHECK(mb.publish([&](FakeJob &job) { fill_job(job, 7); }), "publish() after the restart");
		CHECK(wait_for([&]() { return restarted_id.load() == 7; }, 1000),
		      "the restarted worker decodes the new frame, not the one published while stopped");
		mb.stop();
		mb.stop(); /* idempotent */
	}

	/* 5b: the destructor while a decode is in flight joins cleanly. */
	{
		std::atomic<bool> in_decode{false};
		std::atomic<bool> finished{false};
		{
			CbDecodeMailbox<FakeJob> mb;
			mb.start([&](FakeJob &) {
				in_decode = true;
				std::this_thread::sleep_for(std::chrono::milliseconds(DECODE_MS));
				finished = true;
			});
			mb.publish([&](FakeJob &job) { fill_job(job, 1); });
			wait_for([&]() { return in_decode.load(); }, 1000);
		}
		CHECK(finished.load(), "the destructor waits for the in-flight decode");
	}

	if (g_failures == 0) {
		std::printf("decode-mailbox-selftest: ALL PASS\n");
		return 0;
	}
	std::printf("decode-mailbox-selftest: %d FAILURE(S)\n", g_failures);
	return 1;
}
