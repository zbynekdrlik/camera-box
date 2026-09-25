/*
 * camera-box issue 1372: map a stamp taken on ANOTHER clock onto os_gettime_ns().
 *
 * On Windows os_gettime_ns() runs at the dantesync-disciplined system-time rate, not at raw QPC
 * (platform-windows.c, the issue-1372 block). Sources that stamp their data on raw QPC used to sit on
 * the os_gettime_ns() timeline by construction; now they would drift against the mixer at the
 * discipline rate (~72 ms per hour at 20 ppm), and libobs keeps direct timestamps (timing_adjust = 0)
 * until the 2 s jump detection snaps them. Such sources:
 *   - WASAPI: IAudioCaptureClient::GetBuffer's qpcPosition (100 ns units, raw QPC);
 *   - obs-browser: CEF's audio pts (base::TimeTicks ms, QPC-based on Windows).
 * (vlc-video stamps on libvlc_clock() and would map the same way with its own clock's now, but the
 * camera-box Windows bundle is built with ENABLE_VLC=OFF.)
 *
 * The stamp's AGE is measured on its own clock and subtracted from the disciplined now. Over an age of
 * milliseconds the rate difference (<= 1000 ppm) is well below a microsecond. A stamp further than
 * OS_FOREIGN_STAMP_MAX_AGE_NS from that clock's now is not on it (e.g. a Unix-epoch value) and passes
 * through unchanged, exactly as before issue 1372.
 *
 * Rust authority: src/os_clock_discipline.rs (map_foreign_clock_ns); parity gate:
 * tests/os_clock_discipline_parity_1372.rs.
 */

#pragma once

#include <windows.h>

#include "../c99defs.h"
#include "../platform.h"
#include "../util_uint64.h"

#ifdef __cplusplus
extern "C" {
#endif

#define OS_FOREIGN_STAMP_MAX_AGE_NS 60000000000ULL

static inline uint64_t os_foreign_clock_map_ns(uint64_t stamp_ns, uint64_t clock_now_ns, uint64_t now_ns)
{
	if (stamp_ns >= clock_now_ns) {
		const uint64_t ahead_ns = stamp_ns - clock_now_ns;
		return ahead_ns <= OS_FOREIGN_STAMP_MAX_AGE_NS ? now_ns + ahead_ns : stamp_ns;
	}

	const uint64_t age_ns = clock_now_ns - stamp_ns;
	if (age_ns > OS_FOREIGN_STAMP_MAX_AGE_NS)
		return stamp_ns;
	return age_ns < now_ns ? now_ns - age_ns : 0;
}

/* A stamp on another clock whose "now" the caller read right before this call. */
static inline uint64_t os_foreign_clock_ns_to_gettime_ns(uint64_t stamp_ns, uint64_t clock_now_ns)
{
	return os_foreign_clock_map_ns(stamp_ns, clock_now_ns, os_gettime_ns());
}

/* A stamp on the raw QPC timeline, in ns. The disciplined now is bracketed by two counter reads and
 * paired with their midpoint, so a preemption between the reads cannot skew the age (retried while
 * the reads are more than 50 us apart). */
static inline uint64_t os_raw_qpc_ns_to_gettime_ns(uint64_t raw_qpc_ns)
{
	LARGE_INTEGER freq;
	LARGE_INTEGER before;
	LARGE_INTEGER after;
	uint64_t now_ns = 0;

	QueryPerformanceFrequency(&freq);
	for (int attempt = 0; attempt < 4; attempt++) {
		QueryPerformanceCounter(&before);
		now_ns = os_gettime_ns();
		QueryPerformanceCounter(&after);
		if ((uint64_t)(after.QuadPart - before.QuadPart) <= (uint64_t)freq.QuadPart / 20000)
			break;
	}

	const uint64_t mid = ((uint64_t)before.QuadPart + (uint64_t)after.QuadPart) / 2;
	const uint64_t raw_now_ns = util_mul_div64(mid, 1000000000, (uint64_t)freq.QuadPart);
	return os_foreign_clock_map_ns(raw_qpc_ns, raw_now_ns, now_ns);
}

/* WASAPI qpcPosition (100 ns units on the raw QPC timeline). */
static inline uint64_t os_raw_qpc_100ns_to_gettime_ns(uint64_t raw_qpc_100ns)
{
	return os_raw_qpc_ns_to_gettime_ns(raw_qpc_100ns * 100);
}

#ifdef __cplusplus
}
#endif
