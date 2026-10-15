#pragma once

/*
 * camera-box A/V-sync dock -- the page faults the calling thread has taken (issue 1381), for the
 * dock's self-tests and the demod bench.
 *
 * WHY: a page fault is kernel work done in the faulting thread's own context: a fresh page is
 * zeroed, and under memory pressure the kernel may reclaim first. On libobs's audio or video-output
 * thread one fault can stall the tick for milliseconds. A slot buffer the producer writes for the
 * FIRST time inside publish() faults right there, on the first pass over the slots after each
 * output start. So the producer side of the dock's worker FIFO and mailbox is checked by COUNTING
 * the producer thread's faults across the publishes after start(): a deterministic number. A
 * timing bound on the worst push cannot attribute an outlier and flakes on a loaded runner (one
 * push read 2.42 ms, CI run 37345820890).
 *
 * Linux only (getrusage RUSAGE_THREAD). The self-tests and the bench run on the Linux CI
 * (tests/av_sync_dock_audio_worker_1381.rs, tests/av_sync_dock_decode_mailbox_1367.rs,
 * tests/av_sync_dock_demod_bench_1381.rs); the dock itself never includes this file.
 */

#include <sys/resource.h>

#include <cstdint>
#include <cstdio>
#include <cstdlib>

/* 1 when this build's fault count is exact, 0 under a sanitizer: the TSAN / ASAN runtime faults on
 * its own shadow and metadata pages inside the bracketed window (the mailbox self-test read 1 fault
 * under TSAN on a publish into a slot an earlier publish had already written). A sanitizer build
 * REPORTS the count; the plain build every CI run compiles CHECKS it. */
#if defined(__SANITIZE_THREAD__) || defined(__SANITIZE_ADDRESS__)
#define CB_THREAD_FAULTS_EXACT 0
#elif defined(__has_feature)
#if __has_feature(thread_sanitizer) || __has_feature(address_sanitizer)
#define CB_THREAD_FAULTS_EXACT 0
#endif
#endif
#ifndef CB_THREAD_FAULTS_EXACT
#define CB_THREAD_FAULTS_EXACT 1
#endif

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
