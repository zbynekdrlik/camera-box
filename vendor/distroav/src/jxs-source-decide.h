/* camera-box issue 1387 slice 2: the pure decisions of the JPEG XS source (jxs-source.cpp).
 *
 * C, header-only, no OBS or library calls: tests/distroav_jxs_source_1387.rs includes this very
 * file into a truth-table program and runs it under `cargo test` (CI is otherwise the first
 * compiler of the plugin). */
#pragma once

#include <stdbool.h>
#include <stdint.h>

#define CB_JXS_PORT_DEFAULT 7000
#define CB_JXS_LATENCY_MS_MIN 3     /* mirrors ndi-source.cpp PROP_GENLOCK_LATENCY_MS_MIN */
#define CB_JXS_LATENCY_MS_MAX 2000  /* mirrors ndi-source.cpp PROP_GENLOCK_SOURCE_LATENCY_MS_MAX */
#define CB_JXS_MAX_BPP_LIMIT 12     /* camera_box::jxs_auto_bpp::MAX_BPP */
#define CB_JXS_STATUS_PERIOD_NS 5000000000ULL
/* No frame for this long = the sender is gone: the LOCK indicator's "not connected". */
#define CB_JXS_DISCONNECTED_NS 3000000000ULL

/* The frame timestamp libobs gets: the sender's genlock timecode (100 ns since the Unix epoch)
 * as epoch nanoseconds, the stamp genlock_is_wallclock_ts recognises, exactly what ndi-source
 * hands over for an NDI frame (timecode x 100). A timecode <= 0 is no stamp (0). */
static inline uint64_t cb_jxs_timestamp_ns(int64_t timecode_100ns)
{
	if (timecode_100ns <= 0)
		return 0;
	if ((uint64_t)timecode_100ns > UINT64_MAX / 100u)
		return 0;
	return (uint64_t)timecode_100ns * 100u;
}

/* The per-source genlock latency, clamped like ndi-source.cpp (floor before any cast). */
static inline uint32_t cb_jxs_clamp_latency_ms(long long ms)
{
	if (ms < CB_JXS_LATENCY_MS_MIN)
		return CB_JXS_LATENCY_MS_MIN;
	if (ms > CB_JXS_LATENCY_MS_MAX)
		return CB_JXS_LATENCY_MS_MAX;
	return (uint32_t)ms;
}

/* The listen port setting: 1..65535, anything else the default. */
static inline uint16_t cb_jxs_port(long long v)
{
	return (v >= 1 && v <= 65535) ? (uint16_t)v : (uint16_t)CB_JXS_PORT_DEFAULT;
}

/* The camera id (0 = the first one heard) and the max bpp sent in the report (0 = no limit):
 * out of range = 0. */
static inline uint8_t cb_jxs_camera(long long v)
{
	return (v >= 0 && v <= 255) ? (uint8_t)v : 0;
}

static inline uint8_t cb_jxs_max_bpp(long long v)
{
	return (v >= 0 && v <= CB_JXS_MAX_BPP_LIMIT) ? (uint8_t)v : 0;
}

/* The receiver settings; a change reopens the receiver (a new socket), nothing else does. */
struct cb_jxs_config {
	uint16_t port;
	uint8_t camera;
	uint8_t max_bpp;
};

static inline bool cb_jxs_reopen_needed(struct cb_jxs_config a, struct cb_jxs_config b)
{
	return a.port != b.port || a.camera != b.camera || a.max_bpp != b.max_bpp;
}

/* The average coded bits per pixel over a status window: payload bytes x 8 / (frames x pixels),
 * x 100 and rounded (the sender codes every frame to its bpp's exact size, so this reads the bpp
 * the camera sends, e.g. 800 = 8 bpp). 0 when nothing was received. */
static inline uint32_t cb_jxs_bpp_x100(uint64_t bytes, uint64_t frames, uint32_t width, uint32_t height)
{
	const uint64_t pixels = (uint64_t)width * (uint64_t)height;
	if (frames == 0 || pixels == 0)
		return 0;
	const uint64_t den = frames * pixels;
	if (bytes > (UINT64_MAX - den / 2) / 800u)
		return UINT32_MAX;
	const uint64_t v = (bytes * 800u + den / 2) / den;
	return v > UINT32_MAX ? UINT32_MAX : (uint32_t)v;
}

/* Whether the source counts as connected (frames arriving) for the genlock LOCK indicator. */
static inline bool cb_jxs_connected(uint64_t now_ns, uint64_t last_frame_ns)
{
	if (last_frame_ns == 0 || now_ns < last_frame_ns)
		return last_frame_ns != 0;
	return now_ns - last_frame_ns < CB_JXS_DISCONNECTED_NS;
}
