#pragma once

/*
 * camera-box A/V-sync dock -- the page faults the calling thread has taken (issue 1381), for the
 * dock's self-tests and the demod bench.
 *
 * WHY: a page fault is kernel work done in the faulting thread's own context: a fresh page is
 * zeroed, and under memory pressure the kernel may reclaim first. On libobs's audio or video-output
 * thread one fault can stall the tick for milliseconds. A slot buffer the producer writes for the
 * FIRST time inside publish() faults right there, at every session start. So the producer side of
 * the dock's worker FIFO and mailbox is checked by COUNTING the producer thread's faults across the
 * publishes after start(): a deterministic number. A timing bound on the worst push cannot see the
 * cause and flakes on a loaded runner (2.42 ms on one push, CI run 37345820890).
 *
 * Linux only (getrusage RUSAGE_THREAD). The self-tests and the bench run on the Linux CI
 * (tests/av_sync_dock_audio_worker_1381.rs, tests/av_sync_dock_decode_mailbox_1367.rs,
 * tests/av_sync_dock_demod_bench_1381.rs); the dock itself never includes this file.
 */

#include <sys/resource.h>

#include <cstdint>
#include <cstdio>
#include <cstdlib>

/* Minor + major page faults the CALLING thread has taken so far. A refused read aborts loudly, so
 * no check can pass on a read that never happened. */
inline uint64_t cb_thread_page_faults()
{
	struct rusage ru;
	if (getrusage(RUSAGE_THREAD, &ru) != 0) {
		std::perror("cb_thread_page_faults: getrusage(RUSAGE_THREAD)");
		std::abort();
	}
	return (uint64_t)ru.ru_minflt + (uint64_t)ru.ru_majflt;
}
