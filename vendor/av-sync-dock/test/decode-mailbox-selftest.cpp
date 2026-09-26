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
 *   6. publish() after stop() is a no-op, and a stopped mailbox can be started again;
 *   7. on_thread_start runs once on the worker, before the first job;
 *   8. the publish-time high-water mark keeps the true maximum under concurrent updates;
 *   9. the frame copies (camera-box-frame-copy.hpp) equal a plain reference read of the frame, and
 *      every pixel norihiro's marker search reads lies inside the copied circle patch.
 *
 * Dependency-free (STL + threads): `g++ -std=c++11 -O2 -Wall -Wextra -Werror -pthread`.
 * Driven by tests/av_sync_dock_decode_mailbox_1367.rs on every CI run. Exit 0 + "ALL PASS" = pass.
 */

#include "../src/camera-box-decode-mailbox.hpp"
#include "../src/camera-box-frame-copy.hpp"

#include <atomic>
#include <chrono>
#include <condition_variable>
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

/* The dock's 10-bit little-endian intensity extractor (get_intensity_10le in sync-test-output.cpp). */
static uint8_t intensity_10le(const uint8_t *data)
{
	uint16_t v = (uint16_t)((data[0] >> 2) | (data[1] << 6));
	return (uint8_t)(v > 0xFF ? 0xFF : v);
}

/* norihiro's sqrt_u32, kept verbatim as the reference for cb_isqrt_u32. */
static uint32_t ref_sqrt_u32(uint32_t x)
{
	uint32_t r = 0;
	for (uint32_t b = 1 << 15; b; b >>= 1) {
		if ((r | b) * (r | b) <= x)
			r |= b;
	}
	return r;
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
			const uint64_t id_at_start = job.id;
			const bool intact = payload_intact(job);
			{
				std::lock_guard<std::mutex> lock(seen_mutex);
				decoded_ids.push_back(job.id);
				if (!intact)
					torn = true;
			}
			std::this_thread::sleep_for(std::chrono::milliseconds(DECODE_MS));
			/* Re-check after the sleep: the producer kept publishing meanwhile, so a buffer
			 * shared with the producer would now hold a newer frame. */
			if (job.id != id_at_start || !payload_intact(job)) {
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

	/* 2-4, deterministic: a burst published while the worker is held inside a decode. Frame 1 is
	 * taken and held; frames 2..5 arrive meanwhile. Frames 2, 3 and 4 are replaced (3 drops), the
	 * held frame is never overwritten under the decoder, and the worker's next frame is 5. */
	{
		CbDecodeMailbox<FakeJob> mb;
		std::mutex m;
		std::condition_variable cv;
		bool release = false;
		std::atomic<bool> holding{false};
		std::vector<uint64_t> ids;
		bool held_frame_changed = false;
		CHECK(mb.start([&](FakeJob &job) {
			const uint64_t id_at_start = job.id;
			if (id_at_start == 1) {
				holding = true;
				std::unique_lock<std::mutex> lock(m);
				/* Bounded: a broken mailbox makes this test fail, never hang. */
				(void)cv.wait_for(lock, std::chrono::seconds(2), [&]() { return release; });
			}
			std::lock_guard<std::mutex> lock(m);
			if (job.id != id_at_start || !payload_intact(job))
				held_frame_changed = true;
			ids.push_back(job.id);
		}),
		      "start() for the burst case");
		CHECK(mb.publish([&](FakeJob &job) { fill_job(job, 1); }), "publish frame 1");
		CHECK(wait_for([&]() { return holding.load(); }, 1000), "the worker holds frame 1");
		for (uint64_t id = 2; id <= 5; id++)
			CHECK(mb.publish([&](FakeJob &job) { fill_job(job, id); }), "publish during the held decode");
		CHECK(mb.dropped() == 3, "frames 2, 3 and 4 were replaced before the worker took them");
		{
			std::lock_guard<std::mutex> lock(m);
			release = true;
		}
		cv.notify_all();
		CHECK(wait_for(
			      [&]() {
				      std::lock_guard<std::mutex> lock(m);
				      return ids.size() == 2;
			      },
			      1000),
		      "the worker decodes exactly one more frame after the burst");
		mb.stop();
		std::lock_guard<std::mutex> lock(m);
		CHECK(ids.size() == 2 && ids[0] == 1 && ids[1] == 5, "latest wins: the frame after 1 is 5");
		CHECK(!held_frame_changed, "the held frame is never overwritten while it is decoded");
		CHECK(mb.taken() == 2 && mb.dropped() == 3, "taken + dropped == published for the burst");
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

	/* 7: on_thread_start runs once, on the worker thread, before the first job. */
	{
		CbDecodeMailbox<FakeJob> mb;
		std::mutex mm;
		int starts = 0;
		std::thread::id start_tid;
		std::thread::id job_tid;
		bool started_before_job = false;
		CHECK(mb.start(
			      [&](FakeJob &) {
				      std::lock_guard<std::mutex> lock(mm);
				      job_tid = std::this_thread::get_id();
				      started_before_job = starts == 1;
			      },
			      [&]() {
				      std::lock_guard<std::mutex> lock(mm);
				      starts++;
				      start_tid = std::this_thread::get_id();
			      }),
		      "start() with an on_thread_start hook");
		CHECK(mb.publish([&](FakeJob &job) { fill_job(job, 1); }), "publish for the hook case");
		CHECK(wait_for(
			      [&]() {
				      std::lock_guard<std::mutex> lock(mm);
				      return job_tid != std::thread::id();
			      },
			      1000),
		      "the hook case decodes a job");
		mb.stop();
		std::lock_guard<std::mutex> lock(mm);
		CHECK(starts == 1, "on_thread_start runs exactly once");
		CHECK(start_tid == job_tid, "on_thread_start runs on the worker thread");
		CHECK(start_tid != std::this_thread::get_id(), "on_thread_start never runs on the caller");
		CHECK(started_before_job, "on_thread_start runs before the first job");
	}

	/* 8: the producer's publish-time high-water mark (cb_atomic_max_u64). */
	{
		std::atomic<uint64_t> hw{0};
		cb_atomic_max_u64(hw, 5);
		cb_atomic_max_u64(hw, 3);
		CHECK(hw.load() == 5, "a smaller value never lowers the max");
		cb_atomic_max_u64(hw, 9);
		CHECK(hw.load() == 9, "a larger value raises the max");
		std::atomic<uint64_t> racing{0};
		std::vector<std::thread> ts;
		for (int t = 0; t < 4; t++)
			ts.emplace_back([&racing, t]() {
				for (uint64_t v = 1; v <= 20000; v++)
					cb_atomic_max_u64(racing, v * 4 + (uint64_t)t);
			});
		for (size_t t = 0; t < ts.size(); t++)
			ts[t].join();
		CHECK(racing.load() == 20000 * 4 + 3, "concurrent updates keep the true maximum");
	}

	/* 9: the frame copies the video-output thread makes (camera-box-frame-copy.hpp) against a
	 * plain reference read, for an 8-bit planar plane (the memcpy path), a packed RGBA-like plane
	 * and a 10-bit plane read through an intensity function; odd sizes and row padding. */
	{
		struct Fmt {
			uint32_t pixelsize;
			uint32_t pixeloffset;
			CbIntensityFn fn;
			const char *name;
		};
		const Fmt fmts[] = {
			{1, 0, nullptr, "8-bit planar"},
			{4, 1, nullptr, "packed 4-byte"},
			{2, 0, intensity_10le, "10-bit"},
		};
		const uint32_t W = 67, H = 41;
		uint32_t rng = 12345;
		for (size_t f = 0; f < sizeof(fmts) / sizeof(fmts[0]); f++) {
			const Fmt &fmt = fmts[f];
			const uint32_t linesize = W * fmt.pixelsize + 13;
			std::vector<uint8_t> frame((size_t)linesize * H);
			for (size_t i = 0; i < frame.size(); i++) {
				rng = rng * 1103515245u + 12345u;
				frame[i] = (uint8_t)(rng >> 16);
			}
			const CbPlaneView v = {frame.data(), linesize, fmt.pixelsize, fmt.pixeloffset, fmt.fn};
			auto ref = [&](uint32_t x, uint32_t y) -> uint8_t {
				const uint8_t *p = frame.data() + (size_t)y * linesize + fmt.pixeloffset + (size_t)x * fmt.pixelsize;
				return fmt.fn ? fmt.fn(p) : *p;
			};

			const uint32_t rows = 30;
			std::vector<uint8_t> band((size_t)W * rows, 0xAA);
			cb_copy_top_band(v, W, rows, band.data());
			bool band_ok = true;
			for (uint32_t y = 0; y < rows; y++)
				for (uint32_t x = 0; x < W; x++)
					if (band[(size_t)y * W + x] != ref(x, y))
						band_ok = false;
			CHECK(band_ok, "the top band equals the reference read (memcpy and per-pixel paths)");

			const uint32_t step = 4, gw = W / step, gh = H / step;
			std::vector<uint8_t> grid((size_t)gw * gh, 0xAA);
			cb_copy_step_grid(v, step, gw, gh, grid.data());
			bool grid_ok = true;
			for (uint32_t gy = 0; gy < gh; gy++)
				for (uint32_t gx = 0; gx < gw; gx++)
					if (grid[(size_t)gy * gw + gx] != ref(step / 2 + gx * step, step / 2 + gy * step))
						grid_ok = false;
			CHECK(grid_ok, "the step grid equals the reference sampling");

			/* Every corner the marker search can meet: inside, on every edge, cx/cy <= r,
			 * and boxes partly or wholly outside the frame. */
			bool spans_inside = true, sums_equal = true, spans_match_norihiro = true;
			int nonempty = 0;
			const uint32_t cs[] = {0, 1, 5, 20, 33, 60, 66, 67, 70, 90};
			const uint32_t rs[] = {0, 1, 3, 7, 12, 25, 40};
			for (uint32_t cx : cs) {
				for (uint32_t cy : cs) {
					for (uint32_t r : rs) {
						const CbPatchRect rect = cb_marker_patch_rect(cx, cy, r, W, H);
						std::vector<uint8_t> patch((size_t)rect.w * rect.h + 1, 0xAA);
						cb_copy_patch(v, rect, patch.data());
						if (r == 0)
							continue;
						const uint32_t y0 = cy > r ? cy - r : 0;
						const uint32_t y1 = cy + r < H ? cy + r : H;
						for (uint32_t y = y0; y < y1; y++) {
							const CbSpan s = cb_marker_circle_row(cx, cy, r, y, W);
							/* norihiro's own row formula, kept here as the reference. */
							uint32_t dd = y > cy ? y - cy : cy - y;
							uint32_t dx = ref_sqrt_u32(r * r - dd * dd);
							const uint32_t rx0 = cx > dx ? cx - dx : 0;
							const uint32_t rx1 = cx + dx < W ? cx + dx : W;
							if (s.x0 != rx0 || s.x1 != rx1)
								spans_match_norihiro = false;
							if (s.x0 >= s.x1)
								continue;
							nonempty++;
							if (y < rect.y0 || y >= rect.y0 + rect.h || s.x0 < rect.x0 ||
							    s.x1 > rect.x0 + rect.w) {
								spans_inside = false;
								continue;
							}
							uint32_t from_patch = 0, from_frame = 0;
							for (uint32_t x = s.x0; x < s.x1; x++) {
								from_patch += patch[(size_t)(y - rect.y0) * rect.w + (x - rect.x0)];
								from_frame += ref(x, y);
							}
							if (from_patch != from_frame)
								sums_equal = false;
						}
					}
				}
			}
			CHECK(nonempty > 1000, "the corner sweep reaches many non-empty circle rows");
			CHECK(spans_match_norihiro, "cb_marker_circle_row is norihiro's row span");
			CHECK(spans_inside, "every pixel the marker search reads lies inside the copied patch");
			CHECK(sums_equal, "the patch-based circle sums equal the full-frame sums");
		}
	}

	if (g_failures == 0) {
		std::printf("decode-mailbox-selftest: ALL PASS\n");
		return 0;
	}
	std::printf("decode-mailbox-selftest: %d FAILURE(S)\n", g_failures);
	return 1;
}
