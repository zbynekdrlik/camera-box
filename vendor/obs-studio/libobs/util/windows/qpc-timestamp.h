/*
 * camera-box issue 1372: map a raw-QPC timestamp onto os_gettime_ns().
 *
 * On Windows os_gettime_ns() runs at the dantesync-disciplined system-time rate, not at raw QPC
 * (platform-windows.c, the issue-1372 block). A source that stamps its data with a RAW QPC time --
 * WASAPI IAudioCaptureClient::GetBuffer's qpcPosition (100 ns units) -- would otherwise drift
 * against the mixer at the discipline rate (~72 ms per hour at 20 ppm) until libobs' 2 s jump
 * detection snaps it. The timestamp's AGE is measured on raw QPC and subtracted from the
 * disciplined now; over an age of milliseconds the rate difference (<= 1000 ppm) is well below a
 * microsecond.
 *
 * Rust authority: src/os_clock_discipline.rs (map_raw_qpc_ns); parity gate:
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

static inline uint64_t os_qpc_ns_map_to_gettime_ns(uint64_t raw_ts_ns, uint64_t raw_now_ns, uint64_t now_ns)
{
	if (raw_ts_ns >= raw_now_ns)
		return now_ns + (raw_ts_ns - raw_now_ns);

	const uint64_t age_ns = raw_now_ns - raw_ts_ns;
	return age_ns < now_ns ? now_ns - age_ns : 0;
}

static inline uint64_t os_raw_qpc_100ns_to_gettime_ns(uint64_t raw_qpc_100ns)
{
	LARGE_INTEGER freq;
	LARGE_INTEGER count;

	QueryPerformanceFrequency(&freq);
	const uint64_t now_ns = os_gettime_ns();
	QueryPerformanceCounter(&count);
	const uint64_t raw_now_ns = util_mul_div64((uint64_t)count.QuadPart, 1000000000, (uint64_t)freq.QuadPart);
	return os_qpc_ns_map_to_gettime_ns(raw_qpc_100ns * 100, raw_now_ns, now_ns);
}

#ifdef __cplusplus
}
#endif
