/* camera-box issue 1387 slice 3: the pure decisions of the JPEG XS program output (jxs-output.cpp).
 *
 * C, header-only, no OBS or library calls: tests/distroav_jxs_output_1387.rs includes this very
 * file into a truth-table program and runs it under `cargo test` (CI is otherwise the first
 * compiler of the plugin). */
#pragma once

#include <stdbool.h>
#include <stdint.h>
#include <string.h>

/* The program's stream id on the wire: outside the CAM<N> range of the camboxes, so the stream
 * side's source can take it by id. */
#define CB_JXS_OUT_STREAM_ID_DEFAULT 100u
/* The highest bpp the wire carries (camera_box::jxs_send::MAX_BPP). */
#define CB_JXS_OUT_MAX_BPP 12
#define CB_JXS_OUT_STATUS_PREFIX "jxs send: fps="

/* The stream id setting: 1..255, anything else the default. */
static inline uint8_t cb_jxs_out_stream_id(long long v)
{
	return (v >= 1 && v <= 255) ? (uint8_t)v : (uint8_t)CB_JXS_OUT_STREAM_ID_DEFAULT;
}

/* A bpp bound setting: 0 = the sender's default, 1..12, anything else 0. */
static inline uint8_t cb_jxs_out_bpp(long long v)
{
	return (v >= 0 && v <= CB_JXS_OUT_MAX_BPP) ? (uint8_t)v : 0;
}

/* The send pacing default: 0 = AUTOMATIC (send ABI 3, issue 1387, owner 29.10.2026: no number
 * that a network reconfiguration silently breaks). The sender paces to 90 % of the receiver's link
 * from its report when that link is slower than strih's own (stream-snv's 1 GbE port -> 900, the
 * value the 23:35 live run proved with 0 lost slices; a 2.5 GbE PP receiver -> 2250), at line rate
 * when it is as fast. A value above 0 is a fixed override. */
#define CB_JXS_OUT_PACE_MBPS_DEFAULT 900
/* The sender's bounds (camera_box::jxs_link::pace::PACE_MBPS_MIN / _MAX); 0 = automatic. */
#define CB_JXS_OUT_PACE_MBPS_MIN 100
#define CB_JXS_OUT_PACE_MBPS_MAX 100000

/* The pace setting: 0 (or below) = automatic, else clamped into the sender's bounds, so a setting
 * never makes the start fail. */
static inline uint32_t cb_jxs_out_pace_mbps(long long v)
{
	if (v <= 0)
		return 0;
	if (v < CB_JXS_OUT_PACE_MBPS_MIN)
		return CB_JXS_OUT_PACE_MBPS_MIN;
	if (v > CB_JXS_OUT_PACE_MBPS_MAX)
		return CB_JXS_OUT_PACE_MBPS_MAX;
	return (uint32_t)v;
}

/* The status line's word for cbjxs_send_stats.pace_source (CBJXS_SEND_PACE_*; the Rust
 * PaceSource tokens). */
static inline const char *cb_jxs_out_pace_source(uint32_t code)
{
	switch (code) {
	case 0:
		return "line-rate";
	case 1:
		return "fixed";
	case 2:
		return "receiver-link";
	case 3:
		return "start-spread";
	default:
		return "unknown";
	}
}

/* The canvas frame rate the sender is told (it sends every frame at 30 or below, the 30 fps grid
 * frames above), rounded; a non-positive or absurd rate is 0 (the open refuses nothing for it,
 * the sender then sends every frame). */
static inline uint32_t cb_jxs_out_fps(double fps)
{
	if (!(fps > 0.0) || fps > 1000.0)
		return 0;
	return (uint32_t)(fps + 0.5);
}

/* Whether a frame's stamp may go: never the stamp just sent. The stamp is taken when libobs hands
 * the frame to this output (genlock_emit_timecode_100ns, the NDI output's function), so two
 * callbacks inside one slot (libobs' video thread catching up after a lag) carry the same stamp;
 * the receiver would drop the second as a duplicate, unseen by the sender. A stamp BEHIND the last
 * one goes: that is a wall-clock step back (the nightly dantesync date step), which the receiver
 * resyncs on; holding frames until the clock passes the old stamp would cost up to ~200 ms of
 * program. 0 = none sent. */
static inline bool cb_jxs_out_stamp_fresh(int64_t last_sent_100ns, int64_t stamp_100ns)
{
	return last_sent_100ns == 0 || stamp_100ns != last_sent_100ns;
}

/* The receiving source shows the program as BT.709 limited range (jxs-source.cpp): the output
 * starts only on a canvas that is that (the strih profile pins 709 / Partial). `cs_709` /
 * `range_partial` are the caller's reads of the canvas (an sRGB canvas counts as 709, its YUV
 * conversion uses the 709 matrix; a default range counts as partial). */
static inline bool cb_jxs_out_colour_ok(bool cs_709, bool range_partial)
{
	return cs_709 && range_partial;
}

/* Whether a line the sender logs goes on to the OBS log: everything but the sender's own 5 s
 * status line, which the output logs itself, named, from the same window (its counters plus the
 * output's own). */
static inline bool cb_jxs_out_forward_line(const char *message)
{
	if (!message)
		return false;
	return strncmp(message, CB_JXS_OUT_STATUS_PREFIX, strlen(CB_JXS_OUT_STATUS_PREFIX)) != 0;
}
