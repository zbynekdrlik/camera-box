/* camera-box issue 1387 slice 2: the pure decisions of the JPEG XS source (jxs-source.cpp).
 *
 * C, header-only, no OBS calls (libc only): tests/distroav_jxs_source_1387.rs and
 * tests/distroav_jxs_pick_1387.rs include this very file into truth-table programs and run them under
 * `cargo test` (CI is otherwise the first compiler of the plugin). */
#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

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

/* The receiver settings. A new port or camera reopens the receiver (a new socket); a new max bpp
 * only changes the report's cap (cbjxs_set_max_bpp), never a reopen: a reopen drops the decoder and
 * restarts the report sequence, which the sender's auto bpp would read as reordered reports. */
struct cb_jxs_config {
	uint16_t port;
	uint8_t camera;
	uint8_t max_bpp;
	uint32_t latency_ms; /* the input's genlock latency: sets the late bound, never reopens */
};

/* A frame is late only past what this input's genlock FIFO holds, never under 100 ms (live
 * 31.10.2026: a 1087 ms strih program input read 100 ms as late and stepped its sender down). */
static inline uint32_t cb_jxs_late_ms(uint32_t latency_ms)
{
	return latency_ms > 100u ? latency_ms : 100u;
}

static inline bool cb_jxs_reopen_needed(struct cb_jxs_config a, struct cb_jxs_config b)
{
	return a.port != b.port || a.camera != b.camera;
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

/* issue 1387 (owner 1.11.2026): the "Kamera" list of a JPEG XS input, like the NDI source's list.
 * Its items are the cameras the box declares (/opt/camera-box/jxs-seed.json): label
 * "CAM<N> (UDP <port>)", value "<port>:<camera>"; one custom entry leaves the two numbers to the
 * operator. A UDP port binds one input, so a second input on the same camera stays black. */
#define CB_JXS_PICK_CUSTOM "custom"
#define CB_JXS_PICK_VALUE_MAX 12 /* "65535:255" + NUL */
#define CB_JXS_PICK_LABEL_MAX 24 /* "CAM255 (UDP 65535)" + NUL */

static inline bool cb_jxs_pick_pair_ok(long long port, long long camera)
{
	return port >= 1 && port <= 65535 && camera >= 0 && camera <= 255;
}

/* The list value of a camera, "<port>:<camera>". false (and "" when the buffer has room) for a pair
 * out of range or a buffer too small. */
static inline bool cb_jxs_pick_value(char *buf, size_t len, long long port, long long camera)
{
	if (!buf || len == 0)
		return false;
	buf[0] = '\0';
	if (!cb_jxs_pick_pair_ok(port, camera))
		return false;
	const int n = snprintf(buf, len, "%u:%u", (unsigned)port, (unsigned)camera);
	if (n < 0 || (size_t)n >= len) {
		buf[0] = '\0';
		return false;
	}
	return true;
}

/* The list label of a camera, "CAM<camera> (UDP <port>)"; false like cb_jxs_pick_value. */
static inline bool cb_jxs_pick_label(char *buf, size_t len, long long port, long long camera)
{
	if (!buf || len == 0)
		return false;
	buf[0] = '\0';
	if (!cb_jxs_pick_pair_ok(port, camera))
		return false;
	const int n = snprintf(buf, len, "CAM%u (UDP %u)", (unsigned)camera, (unsigned)port);
	if (n < 0 || (size_t)n >= len) {
		buf[0] = '\0';
		return false;
	}
	return true;
}

/* One decimal field of a list value: 1..max_digits ASCII digits, no sign, no leading zero (one
 * spelling per value), at most max. Moves *p past it. */
static inline bool cb_jxs_pick_field(const char **p, unsigned max_digits, unsigned long max, unsigned long *out)
{
	const char *s = *p;
	unsigned long v = 0;
	unsigned n = 0;
	while (s[n] >= '0' && s[n] <= '9') {
		if (n == max_digits)
			return false;
		v = v * 10u + (unsigned long)(s[n] - '0');
		n++;
	}
	if (n == 0 || (n > 1 && s[0] == '0') || v > max)
		return false;
	*p = s + n;
	*out = v;
	return true;
}

/* A list value back to its pair: exactly "<port>:<camera>", port 1..65535, camera 0..255. Anything
 * else (the custom entry, empty, a sign, trailing junk) is false and leaves the outputs alone. */
static inline bool cb_jxs_pick_parse(const char *value, uint16_t *port, uint8_t *camera)
{
	unsigned long p = 0, c = 0;
	const char *s = value;
	if (!s || !cb_jxs_pick_field(&s, 5u, 65535ul, &p) || p < 1 || *s != ':')
		return false;
	s++;
	if (!cb_jxs_pick_field(&s, 3u, 255ul, &c) || *s != '\0')
		return false;
	if (port)
		*port = (uint16_t)p;
	if (camera)
		*camera = (uint8_t)c;
	return true;
}

/* The value the list shows for an input on (port, camera): that pair's value when it is one of the n
 * listed camera values, else the custom entry. The shown value is always an item of the list (the
 * properties view applies the first item on its own to a value it cannot find). buf needs
 * CB_JXS_PICK_VALUE_MAX bytes. */
static inline void cb_jxs_pick_shown(char *buf, size_t len, long long port, long long camera,
				     const char *const *listed, size_t n)
{
	if (!buf || len == 0)
		return;
	char v[CB_JXS_PICK_VALUE_MAX];
	if (listed && cb_jxs_pick_value(v, sizeof v, port, camera)) {
		for (size_t i = 0; i < n; i++) {
			if (listed[i] && strcmp(listed[i], v) == 0) {
				snprintf(buf, len, "%s", v);
				return;
			}
		}
	}
	snprintf(buf, len, "%s", CB_JXS_PICK_CUSTOM);
}

/* Whether the source counts as connected (frames arriving) for the genlock LOCK indicator. */
static inline bool cb_jxs_connected(uint64_t now_ns, uint64_t last_frame_ns)
{
	if (last_frame_ns == 0 || now_ns < last_frame_ns)
		return last_frame_ns != 0;
	return now_ns - last_frame_ns < CB_JXS_DISCONNECTED_NS;
}
