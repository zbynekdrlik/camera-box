/* camera-box issue 1387 slice 2: the C ABI of the JPEG XS receiver (jxs/recv-ffi).
 *
 * A thin C face over camera_box::jxs_recv::JxsReceiver, the SAME receive loop jxs-recv-probe runs:
 * UDP reassembly, slice-wise SVT-JPEG-XS decode, concealment of a lost slice from the previous
 * frame, and the 32-byte receiver report back to the sender once a second (the cambox's automatic
 * bpp climbs only on it). Linux only. Written by hand; tests/abi.rs in this crate compiles it with
 * the system C compiler and pins every size, offset and function name against the Rust side.
 *
 * Threading: one receiver is used from one thread at a time (open, next_frame, stats and close may
 * run on different threads, never at once). Every call is bounded: next_frame never waits longer
 * than its timeout plus one frame's decode.
 */
#ifndef CAMERA_BOX_JXS_H
#define CAMERA_BOX_JXS_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Bumped on any change of a struct or a signature below. */
#define CBJXS_ABI_VERSION 1u

typedef struct cbjxs_receiver cbjxs_receiver;

/* One decoded frame: planar 4:2:2, 8 bit, BT.709 limited range (the cambox capture's format).
 * The plane pointers stay valid until the next cbjxs_next_frame or cbjxs_close on the receiver. */
typedef struct cbjxs_frame {
	uint32_t width;
	uint32_t height;
	const uint8_t *y;
	const uint8_t *u;
	const uint8_t *v;
	uint32_t y_stride;  /* bytes per luma row (= width) */
	uint32_t uv_stride; /* bytes per chroma row (= width / 2) */
	/* The sender's genlock timecode: 100 ns since the Unix epoch (the same stamp its NDI frame
	 * carries). x 100 = the wall-clock epoch nanoseconds libobs' genlock FIFO expects. */
	int64_t timecode_100ns;
	/* Kernel receive time of the first / last datagram and the decode time (100 ns, epoch). */
	int64_t first_arrival_100ns;
	int64_t last_arrival_100ns;
	int64_t decoded_at_100ns;
	uint32_t lost_slices; /* slices concealed from the previous frame */
	uint8_t camera_id;
	uint8_t reserved[3];
} cbjxs_frame;

/* Counters since the receiver opened. */
typedef struct cbjxs_stats {
	uint64_t frames; /* decoded (concealed ones included) */
	uint64_t concealed_frames;
	uint64_t lost_slices;
	uint64_t undecodable_frames;
	uint64_t datagrams;
	uint64_t bytes; /* payload bytes of the taken camera */
	uint64_t duplicate;
	uint64_t stale;
	uint64_t malformed;
	uint64_t resyncs;
	uint64_t foreign; /* datagrams of another camera id */
	uint64_t reports_sent;
	uint64_t report_errors;
	uint64_t header_changes; /* decoder reopens at a sender bpp change */
	uint64_t dropped_unread; /* decoded frames the caller did not take in time */
	uint8_t camera_id;       /* the camera being taken, 0 = none heard yet */
	uint8_t reserved[7];
} cbjxs_stats;

/* CBJXS_ABI_VERSION of the linked library. */
uint32_t cbjxs_abi_version(void);

/* Listen on UDP 0.0.0.0:port (0 = any free port). camera = the CAM<N> id to take (0 = the first
 * heard); max_bpp = the highest bpp this receiver takes, sent in every report (0 = no limit).
 * NULL on failure, with the reason in err (always NUL-terminated when err_len > 0). */
cbjxs_receiver *cbjxs_open(uint16_t port, uint8_t camera, uint8_t max_bpp, char *err, size_t err_len);

/* The bound UDP port (0 for NULL). */
uint16_t cbjxs_local_port(const cbjxs_receiver *rx);

/* Wait up to timeout_ms for the next decoded frame.
 * 1 = *out filled, 0 = none in time, -1 = a socket error (cbjxs_take_error names it; close and
 * reopen), -2 = a NULL argument or an internal failure (a panic inside the library: the receiver
 * is unusable from then on; close it). */
int cbjxs_next_frame(cbjxs_receiver *rx, uint32_t timeout_ms, cbjxs_frame *out);

/* Change the highest bpp this receiver takes (0 = no limit). The next report carries it; the
 * socket, the decoder and the report sequence stay, so the sender steps at once (a receiver cap).
 * 0 = done, -1 = NULL. */
int cbjxs_set_max_bpp(cbjxs_receiver *rx, uint8_t max_bpp);

/* The kernel's effective receive buffer of the socket in bytes (SO_RCVBUF as the kernel reports
 * it, capped by net.core.rmem_max); -1 for NULL or an unreadable value. */
int64_t cbjxs_recv_buffer_bytes(const cbjxs_receiver *rx);

/* 0 = *out filled, -1 = a NULL argument. */
int cbjxs_get_stats(const cbjxs_receiver *rx, cbjxs_stats *out);

/* The newest error (socket or decoder) since the last call, NUL-terminated into buf. Returns the
 * bytes written without the NUL, 0 = none. */
size_t cbjxs_take_error(cbjxs_receiver *rx, char *buf, size_t len);

/* Close the socket and free everything (NULL is a no-op). */
void cbjxs_close(cbjxs_receiver *rx);

#ifdef __cplusplus
}
#endif

#endif /* CAMERA_BOX_JXS_H */
