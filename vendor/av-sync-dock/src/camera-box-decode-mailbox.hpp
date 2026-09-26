#pragma once

/*
 * camera-box A/V-sync dock — decode-worker mailbox (issue 1367).
 *
 * WHY this exists: libobs calls every raw output's `raw_video` callback on its ONE video-output
 * thread. The dock used to decode QR codes right there (the camera-box top-band gather + up to two
 * quirc passes, and norihiro's whole-frame quirc + marker search). A frame with QR content took
 * longer than the 33 ms frame budget, and video-io then skipped frames for EVERY raw output on the
 * box. Live 26.9.2026 on the resolume cg OBS: 28 % of output frames skipped, the cg-obs NDI output
 * at 16-18.7 fps, back to 30.0 fps the moment `sync-test-output` stopped.
 *
 * The policy: `st_raw_video` copies only what the decoders read into this mailbox and returns; a
 * worker thread decodes the copy.
 *
 *   - Two buffers, one slot. The producer fills the PENDING buffer under the lock; the worker, when
 *     idle, swaps it with its own WORKING buffer (O(1) under the lock) and decodes it with the lock
 *     released. The producer never touches the buffer being decoded, so a decoded frame is never
 *     torn, and the producer never waits for a decode — only for a pointer swap.
 *   - Latest pending wins. A frame published while the previous one is still pending (the worker is
 *     busy decoding) REPLACES it; `dropped()` counts every replaced frame. The decoder therefore
 *     always sees frames in publish order and the newest frame is never lost.
 *   - Lifecycle. `start()` spawns the worker; `stop()` wakes it, waits for an in-flight decode and
 *     joins. A frame still pending at stop is discarded, and `publish()` on a stopped mailbox is a
 *     no-op returning false (libobs can deliver one last `raw_video` after the output's stop callback
 *     returned, because it disconnects the raw callbacks on its own end-capture thread). The
 *     destructor calls `stop()`. `stop()` must not be called from inside the decode callback.
 *
 * Dependency-free (STL + <thread>); pinned by the g++ self-test
 * `vendor/av-sync-dock/test/decode-mailbox-selftest.cpp`, which
 * tests/av_sync_dock_decode_mailbox_1367.rs compiles and runs on every CI run.
 */

#include <atomic>
#include <condition_variable>
#include <cstdint>
#include <functional>
#include <mutex>
#include <system_error>
#include <thread>
#include <utility>

namespace camerabox {

/* Raise `a` to `v` if `v` is larger (lock-free high-water mark). The dock keeps the per-diag-window
 * max of st_raw_video's own cost this way: written on the video-output thread, read-and-reset with
 * exchange(0) on the audio thread's diag tick. */
inline void cb_atomic_max_u64(std::atomic<uint64_t> &a, uint64_t v)
{
	uint64_t cur = a.load(std::memory_order_relaxed);
	while (v > cur && !a.compare_exchange_weak(cur, v, std::memory_order_relaxed))
		;
}

template<typename Job> class CbDecodeMailbox {
public:
	CbDecodeMailbox() {}
	~CbDecodeMailbox() { stop(); }
	CbDecodeMailbox(const CbDecodeMailbox &) = delete;
	CbDecodeMailbox &operator=(const CbDecodeMailbox &) = delete;

	/* Spawn the worker; `decode` runs on it once per job taken, and `on_thread_start` (optional)
	 * once on the new thread before the first job -- the place to name the thread and lower its
	 * priority. Returns false when already running or when the thread cannot be created (the
	 * mailbox then stays stopped). Clears any frame left pending by a previous run, so a restart
	 * never decodes a stale frame. */
	bool start(std::function<void(Job &)> decode, std::function<void()> on_thread_start = std::function<void()>())
	{
		std::lock_guard<std::mutex> lock(mutex_);
		if (running_ || thread_.joinable())
			return false;
		decode_ = std::move(decode);
		on_thread_start_ = std::move(on_thread_start);
		pending_full_ = false;
		stop_requested_ = false;
		running_ = true;
		try {
			thread_ = std::thread(&CbDecodeMailbox::run, this);
		} catch (const std::system_error &) {
			running_ = false;
			return false;
		}
		return true;
	}

	/* Wake the worker, wait for an in-flight decode to finish and join. Idempotent. */
	void stop()
	{
		{
			std::lock_guard<std::mutex> lock(mutex_);
			if (!running_ && !thread_.joinable())
				return;
			stop_requested_ = true;
			running_ = false;
		}
		cv_.notify_all();
		if (thread_.joinable())
			thread_.join();
		std::lock_guard<std::mutex> lock(mutex_);
		pending_full_ = false;
	}

	/* Producer side. `fill(Job &)` writes the frame copy into the pending buffer, under the lock
	 * (it must be bounded work — a copy, never a decode). Replacing a job the worker has not taken
	 * yet counts as one dropped frame. Returns false (and calls nothing) when stopped. */
	template<typename Fill> bool publish(Fill fill)
	{
		{
			std::lock_guard<std::mutex> lock(mutex_);
			if (!running_)
				return false;
			fill(*pending_);
			if (pending_full_)
				dropped_.fetch_add(1, std::memory_order_relaxed);
			pending_full_ = true;
		}
		cv_.notify_one();
		return true;
	}

	bool running() const
	{
		std::lock_guard<std::mutex> lock(mutex_);
		return running_;
	}
	/* Frames replaced in the slot before the worker took them (monotonic across restarts). */
	uint64_t dropped() const { return dropped_.load(std::memory_order_relaxed); }
	/* Frames handed to the decode callback (monotonic across restarts). */
	uint64_t taken() const { return taken_.load(std::memory_order_relaxed); }

private:
	void run()
	{
		/* on_thread_start_ is written in start() under the lock before this thread exists and
		 * not touched again until the thread is joined, so it is read here without the lock. */
		if (on_thread_start_)
			on_thread_start_();
		std::unique_lock<std::mutex> lock(mutex_);
		for (;;) {
			cv_.wait(lock, [this]() { return stop_requested_ || pending_full_; });
			if (stop_requested_)
				return;
			std::swap(pending_, working_);
			pending_full_ = false;
			taken_.fetch_add(1, std::memory_order_relaxed);
			lock.unlock();
			decode_(*working_);
			lock.lock();
		}
	}

	mutable std::mutex mutex_;
	std::condition_variable cv_;
	std::thread thread_;
	Job slots_[2];
	Job *pending_ = &slots_[0]; // producer-owned, guarded by mutex_
	Job *working_ = &slots_[1]; // worker-owned while a decode runs
	bool pending_full_ = false;
	bool running_ = false;
	bool stop_requested_ = false;
	std::function<void(Job &)> decode_;
	std::function<void()> on_thread_start_;
	std::atomic<uint64_t> dropped_{0};
	std::atomic<uint64_t> taken_{0};
};

} // namespace camerabox
