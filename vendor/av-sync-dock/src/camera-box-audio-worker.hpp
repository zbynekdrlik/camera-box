#pragma once

/*
 * camera-box A/V-sync dock -- the camera-box audio decode's gate and its worker FIFO (issue 1381).
 *
 * WHY this exists: libobs calls a raw output's `raw_audio` on its AUDIO thread, the thread that
 * mixes every source for every output. The dock used to decode the QPSK marker right there, on the
 * whole program mix, and camera-box mode latched on one video QR decode and never cleared. Live
 * 27.9.2026 on the resolume cg OBS a CG_CHAIN run's burn QR latched it at 05:59. With music on
 * program the mixer then ran 13-22 s behind real time, and the FOH VBAN feed turned into silence
 * and dropped audio until OBS exited at 18:21.
 *
 * Two pieces:
 *   - `cb_audio_decode_gate`: the camera-box audio decode runs only while the test signal is FRESH
 *     (a camera-box QR decoded within the dock's freshness window) and only on a box that has the
 *     measurement source (`mbc`). Everywhere else it stays off, whatever latched camera-box mode.
 *   - `CbAudioBlockFifo`: `raw_audio` only copies the block (every channel) into a bounded FIFO of
 *     reused slots and returns; ONE worker thread takes the blocks in order and decodes each with
 *     its own timestamp. It is a FIFO, not the video's latest-pending mailbox, because the marker
 *     decoder needs contiguous samples:
 *       * a full FIFO DROPS the new block and COUNTS it (`dropped()`), and the next block the worker
 *         takes carries CB_AUDIO_GAP_DROPPED, so the caller resets its decoders before decoding it
 *         (`resets()` counts those) -- a gap can never stitch two stretches into a false marker;
 *       * the first block of a session (after start() or end_session()) carries
 *         CB_AUDIO_GAP_SESSION;
 *       * every `end_session()` hands the worker one on_session_end call, after every block
 *         published before it and before any block published after it. The pending ends are a
 *         queue: a gate that closes, reopens and closes again inside the worker's backlog gets both
 *         ends (review round 1). Two ends with no accepted block between them (a session whose
 *         blocks were all dropped) are one end: the worker never saw that session.
 *     The producer copies into a slot the worker is not reading, under the FIFO lock, and never
 *     waits for a decode. The worker measures each block's handling (`take_process_*_ns`).
 *
 * Lifecycle, as the video mailbox (camera-box-decode-mailbox.hpp): start() spawns the worker, stop()
 * wakes it, waits for an in-flight block and joins; queued blocks and pending session ends are
 * discarded; publish()/end_session() on a stopped FIFO are no-ops (libobs can deliver one last
 * raw_audio after the output's stop callback returned). The destructor calls stop(). stop() must not
 * be called from inside a handler.
 *
 * Dependency-free (STL + <thread>), C++11; pinned by the g++ self-test
 * `vendor/av-sync-dock/test/audio-worker-selftest.cpp`, which tests/av_sync_dock_audio_worker_1381.rs
 * compiles and runs.
 */

#include <algorithm>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <functional>
#include <mutex>
#include <system_error>
#include <thread>
#include <utility>
#include <vector>

namespace camerabox {

/* ---- the gate ---- */

enum class CbAudioGate {
	Open,                // decode this block
	NotCameraBoxMode,    // no camera-box QR has ever been decoded
	NoMeasurementSource, // this box has no measurement source (resolume, strih)
	TestSignalStale,     // no camera-box QR decoded within the freshness window
};

/* Whether the camera-box audio decode runs for a block at `now_ns`, given the last camera-box QR
 * decode at `last_qr_ns` (both on the dock's own start_ts-relative clock). A QR stamped after the
 * block (video and audio arrive up to ~2 s apart either way) is fresh. */
inline CbAudioGate cb_audio_decode_gate(bool camera_box_mode, bool measurement_source_present, uint64_t now_ns,
					uint64_t last_qr_ns, uint64_t fresh_ns)
{
	if (!camera_box_mode)
		return CbAudioGate::NotCameraBoxMode;
	if (!measurement_source_present)
		return CbAudioGate::NoMeasurementSource;
	const uint64_t age = now_ns > last_qr_ns ? now_ns - last_qr_ns : 0;
	if (age > fresh_ns)
		return CbAudioGate::TestSignalStale;
	return CbAudioGate::Open;
}

inline const char *cb_audio_gate_text(CbAudioGate gate)
{
	switch (gate) {
	case CbAudioGate::Open:
		return "test signal fresh";
	case CbAudioGate::NotCameraBoxMode:
		return "not in camera-box mode";
	case CbAudioGate::NoMeasurementSource:
		return "no measurement source on this box";
	case CbAudioGate::TestSignalStale:
		return "test signal stale (no camera-box QR decoded within the freshness window)";
	}
	return "unknown";
}

/* ---- the FIFO ---- */

static const size_t CB_AUDIO_FIFO_MAX_CHANNELS = 8; // libobs MAX_AV_PLANES
static const size_t CB_AUDIO_FIFO_SLOTS = 64;       // ~1.4 s of 1024-frame blocks at 48 kHz

/* What happened between the previous block the worker took and this one (bit flags). */
static const unsigned CB_AUDIO_GAP_DROPPED = 1u; // blocks were dropped: the audio is not contiguous
static const unsigned CB_AUDIO_GAP_SESSION = 2u; // a new session starts here

struct CbAudioBlock {
	std::vector<float> planes[CB_AUDIO_FIFO_MAX_CHANNELS];
	size_t channels = 0;
	size_t frames = 0;
	uint64_t timestamp = 0; // the block's own timestamp, so the marker timing is unchanged
	unsigned gap = 0;       // CB_AUDIO_GAP_* flags
};

class CbAudioBlockFifo {
public:
	struct Handlers {
		std::function<void(const CbAudioBlock &)> process;  // once per block, in order
		std::function<void(const CbAudioBlock &)> on_gap;   // before process() of a block with gap != 0
		std::function<void(unsigned)> on_session_end;       // once per end_session(), with its reason
		std::function<void()> on_thread_start;              // once, on the worker, before the first call
	};

	/* The ends ring holds slots + 1 entries: a pending end ends after an accepted block (a later
	 * one than the previous pending end, or it merges with it), and at most `slots` accepted
	 * blocks are unhandled, so the ring never fills and end_session() never allocates. */
	explicit CbAudioBlockFifo(size_t slots = CB_AUDIO_FIFO_SLOTS)
		: slots_(slots < 2 ? 2 : slots), ends_(slots_.size() + 1)
	{
	}
	~CbAudioBlockFifo() { stop(); }
	CbAudioBlockFifo(const CbAudioBlockFifo &) = delete;
	CbAudioBlockFifo &operator=(const CbAudioBlockFifo &) = delete;

	/* Spawn the worker. Every slot gets `reserve_channels` x `reserve_frames` samples of room here,
	 * so publish() allocates nothing for blocks up to that size. Returns false when already running
	 * or when the thread cannot be created (the FIFO then stays stopped). */
	bool start(Handlers handlers, size_t reserve_channels, size_t reserve_frames)
	{
		std::lock_guard<std::mutex> lock(mutex_);
		if (running_ || thread_.joinable())
			return false;
		h_ = std::move(handlers);
		const size_t nch = std::min(reserve_channels, CB_AUDIO_FIFO_MAX_CHANNELS);
		for (size_t s = 0; s < slots_.size(); s++)
			for (size_t c = 0; c < nch; c++)
				slots_[s].planes[c].reserve(reserve_frames);
		head_ = count_ = 0;
		accepted_ = taken_seq_ = 0;
		in_session_ = false;
		pending_gap_ = 0;
		ends_head_ = ends_count_ = 0;
		stop_requested_ = false;
		running_ = true;
		try {
			thread_ = std::thread(&CbAudioBlockFifo::run, this);
		} catch (const std::system_error &) {
			running_ = false;
			return false;
		}
		return true;
	}

	/* Wake the worker, wait for an in-flight block and join; queued blocks and pending session ends
	 * are discarded. Idempotent. */
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
		head_ = count_ = 0;
		in_session_ = false;
		pending_gap_ = 0;
		ends_head_ = ends_count_ = 0;
	}

	/* Producer (libobs's audio thread): copy one block into the FIFO. False when stopped (nothing
	 * counted) or full (the block is dropped, counted, and the next taken block carries
	 * CB_AUDIO_GAP_DROPPED). Bounded work: a memcpy per channel under the lock. */
	bool publish(const float *const *planes, size_t channels, size_t frames, uint64_t timestamp)
	{
		{
			std::lock_guard<std::mutex> lock(mutex_);
			if (!running_)
				return false;
			if (!in_session_) {
				in_session_ = true;
				pending_gap_ |= CB_AUDIO_GAP_SESSION;
			}
			if (count_ == slots_.size()) {
				dropped_.fetch_add(1, std::memory_order_relaxed);
				pending_gap_ |= CB_AUDIO_GAP_DROPPED;
				return false;
			}
			CbAudioBlock &b = slots_[(head_ + count_) % slots_.size()];
			b.channels = std::min(channels, CB_AUDIO_FIFO_MAX_CHANNELS);
			b.frames = frames;
			b.timestamp = timestamp;
			b.gap = pending_gap_;
			pending_gap_ = 0;
			for (size_t c = 0; c < b.channels; c++)
				b.planes[c].assign(planes[c], planes[c] + frames);
			count_++;
			accepted_++;
		}
		cv_.notify_one();
		return true;
	}

	/* Producer: the gate closed. The worker gets one on_session_end(reason) after every block
	 * published so far; the next publish() starts a new session. A no-op outside a session. */
	void end_session(unsigned reason)
	{
		{
			std::lock_guard<std::mutex> lock(mutex_);
			if (!running_ || !in_session_)
				return;
			in_session_ = false;
			SessionEnd *last = ends_count_ ? &ends_[(ends_head_ + ends_count_ - 1) % ends_.size()] : nullptr;
			if (last && last->after == accepted_) {
				last->reason = reason; // no accepted block since the last end: the same end
			} else {
				SessionEnd &e = ends_[(ends_head_ + ends_count_) % ends_.size()];
				e.after = accepted_;
				e.reason = reason;
				ends_count_++;
			}
		}
		cv_.notify_one();
	}

	bool running() const
	{
		std::lock_guard<std::mutex> lock(mutex_);
		return running_;
	}
	/* Blocks dropped because the FIFO was full (monotonic across restarts). */
	uint64_t dropped() const { return dropped_.load(std::memory_order_relaxed); }
	/* Blocks handed to process() (monotonic across restarts). */
	uint64_t taken() const { return taken_.load(std::memory_order_relaxed); }
	/* Gaps from dropped blocks handed to on_gap(), i.e. decoder resets (monotonic). */
	uint64_t resets() const { return resets_.load(std::memory_order_relaxed); }
	/* The longest / the summed on_gap + process() time of the blocks handled since the previous
	 * call (read-and-reset). */
	uint64_t take_process_max_ns() { return process_max_ns_.exchange(0); }
	uint64_t take_process_sum_ns() { return process_sum_ns_.exchange(0); }

private:
	bool end_due() const { return ends_count_ > 0 && taken_seq_ >= ends_[ends_head_].after; }

	void run()
	{
		/* h_ is written in start() under the lock before this thread exists and not touched again
		 * until the thread is joined, so it is read here without the lock. */
		if (h_.on_thread_start)
			h_.on_thread_start();
		std::unique_lock<std::mutex> lock(mutex_);
		for (;;) {
			cv_.wait(lock, [this]() { return stop_requested_ || count_ > 0 || end_due(); });
			if (stop_requested_)
				return;
			if (end_due()) {
				const unsigned reason = ends_[ends_head_].reason;
				ends_head_ = (ends_head_ + 1) % ends_.size();
				ends_count_--;
				lock.unlock();
				if (h_.on_session_end)
					h_.on_session_end(reason);
				lock.lock();
				continue;
			}
			/* The slot stays counted while it is handled, so publish() never writes it. */
			const CbAudioBlock &b = slots_[head_];
			lock.unlock();
			const std::chrono::steady_clock::time_point t0 = std::chrono::steady_clock::now();
			if (b.gap) {
				if (b.gap & CB_AUDIO_GAP_DROPPED)
					resets_.fetch_add(1, std::memory_order_relaxed);
				if (h_.on_gap)
					h_.on_gap(b);
			}
			if (h_.process)
				h_.process(b);
			const uint64_t ns = (uint64_t)std::chrono::duration_cast<std::chrono::nanoseconds>(
						    std::chrono::steady_clock::now() - t0)
						    .count();
			uint64_t cur = process_max_ns_.load(std::memory_order_relaxed);
			while (ns > cur && !process_max_ns_.compare_exchange_weak(cur, ns, std::memory_order_relaxed))
				;
			process_sum_ns_.fetch_add(ns, std::memory_order_relaxed);
			taken_.fetch_add(1, std::memory_order_relaxed);
			lock.lock();
			head_ = (head_ + 1) % slots_.size();
			count_--;
			taken_seq_++;
		}
	}

	mutable std::mutex mutex_;
	std::condition_variable cv_;
	std::thread thread_;
	std::vector<CbAudioBlock> slots_;
	size_t head_ = 0;  // the oldest queued block (the one being handled, while one is)
	size_t count_ = 0; // queued blocks, the one being handled included
	bool running_ = false;
	bool stop_requested_ = false;
	bool in_session_ = false;
	unsigned pending_gap_ = 0;
	struct SessionEnd {
		uint64_t after = 0; // the session ends once this many blocks were accepted and handled
		unsigned reason = 0;
	};
	std::vector<SessionEnd> ends_; // the pending ends, a ring of ends_[ends_head_ ..]
	size_t ends_head_ = 0;
	size_t ends_count_ = 0;
	uint64_t accepted_ = 0;  // blocks accepted into the FIFO since start()
	uint64_t taken_seq_ = 0; // blocks handled since start() (under the lock)
	Handlers h_;
	std::atomic<uint64_t> dropped_{0};
	std::atomic<uint64_t> taken_{0};
	std::atomic<uint64_t> resets_{0};
	std::atomic<uint64_t> process_max_ns_{0};
	std::atomic<uint64_t> process_sum_ns_{0};
};

} // namespace camerabox
