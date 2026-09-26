#pragma once

/*
 * camera-box A/V-sync dock — decode-worker mailbox (issue 1367).
 *
 * RED stub: the API the dock and the self-test use, with today's behaviour — publish() runs the
 * decode synchronously on the caller's (the video-output) thread. The self-test proves this blocks
 * the producer for the whole decode.
 */

#include <atomic>
#include <cstdint>
#include <functional>

namespace camerabox {

template<typename Job> class CbDecodeMailbox {
public:
	CbDecodeMailbox() {}
	~CbDecodeMailbox() { stop(); }
	CbDecodeMailbox(const CbDecodeMailbox &) = delete;
	CbDecodeMailbox &operator=(const CbDecodeMailbox &) = delete;

	bool start(std::function<void(Job &)> decode)
	{
		if (running_)
			return false;
		decode_ = decode;
		running_ = true;
		return true;
	}

	void stop() { running_ = false; }

	template<typename Fill> bool publish(Fill fill)
	{
		if (!running_)
			return false;
		fill(job_);
		taken_++;
		decode_(job_);
		return true;
	}

	bool running() const { return running_; }
	uint64_t dropped() const { return dropped_.load(); }
	uint64_t taken() const { return taken_.load(); }

private:
	Job job_;
	bool running_ = false;
	std::function<void(Job &)> decode_;
	std::atomic<uint64_t> dropped_{0};
	std::atomic<uint64_t> taken_{0};
};

} // namespace camerabox
