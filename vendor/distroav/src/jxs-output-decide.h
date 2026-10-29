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

/* The send pacing default, Mb/s of Ethernet wire: strih-snv sends on 10 GbE and its one target,
 * stream-snv, receives on a 1 GbE port, whose switch buffer drops the tail of a line-rate slice
 * burst (the slice-3 live run lost slices in 10 of 21 windows). 900 leaves NDI's own share of the
 * port aside from it and still carries a 12 bpp frame in ~29.5 ms, under the 33.3 ms of 30 fps. */
#define CB_JXS_OUT_PACE_MBPS_DEFAULT 900
/* The sender's bounds (camera_box::jxs_link::pace::PACE_MBPS_MIN / _MAX); 0 = line rate. */
#define CB_JXS_OUT_PACE_MBPS_MIN 100
#define CB_JXS_OUT_PACE_MBPS_MAX 100000

/* The pace setting: 0 (or below) = line rate, else clamped into the sender's bounds, so a setting
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
