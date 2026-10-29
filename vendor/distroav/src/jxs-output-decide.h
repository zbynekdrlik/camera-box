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

/* The canvas frame rate the sender is told (it sends every frame at 30 or below, the 30 fps grid
 * frames above), rounded; a non-positive or absurd rate is 0 (the open refuses nothing for it,
 * the sender then sends every frame). */
static inline uint32_t cb_jxs_out_fps(double fps)
{
	if (!(fps > 0.0) || fps > 1000.0)
		return 0;
	return (uint32_t)(fps + 0.5);
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
