/* camera-box issue 1387 slice 3: the C ABI of the JPEG XS SENDER (jxs/send-ffi).
 *
 * A thin C face over camera_box::jxs_send (the SAME sender the cambox runs): the picture is copied
 * into one of two reusable planar pictures, coded by SVT-JPEG-XS (latency profile, 16-line slices)
 * on two sender threads, each slice put on the wire the moment it is coded (UDP GSO, the 16-byte
 * camera-box header), and the bpp follows the receiver's once-a-second report (auto bpp). Linux
 * only. Written by hand; tests/abi.rs in this crate compiles it with the system C compiler and
 * pins every size, offset and function name against the Rust side.
 *
 * The library is carried inside the jxs-recv-ffi static library (libjxs_recv_ffi.a): DistroAV
 * links ONE Rust archive (two would each carry std, camera-box and a second SVT-JPEG-XS).
 *
 * Threading: one sender is used from one thread at a time (open, send_frame, get_stats and close
 * may run on different threads, never at once). send_frame never blocks: it copies the picture
 * and returns.
 */
#ifndef CAMERA_BOX_JXS_SEND_H
#define CAMERA_BOX_JXS_SEND_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Bumped on any change of a struct or a signature below. */
#define CBJXS_SEND_ABI_VERSION 3u /* 2: cbjxs_send_config.pace_mbps, the stats' pace + wire time;
                                   * 3: pace_mbps 0 = AUTOMATIC from the receiver's link, the stats'
                                   * pace_source + both links */

/* cbjxs_send_stats.pace_source: where the window's pace came from (issue 1387). */
#define CBJXS_SEND_PACE_LINE_RATE 0u     /* the receiver's link is as fast as the sender's */
#define CBJXS_SEND_PACE_FIXED 1u         /* cbjxs_send_config.pace_mbps above 0 */
#define CBJXS_SEND_PACE_RECEIVER_LINK 2u /* 90 % of the receiver's slower link (its report) */
#define CBJXS_SEND_PACE_START_SPREAD 3u  /* no receiver link known yet: the frame spread */
#define CBJXS_SEND_PACE_CONGESTION 4u    /* the automatic pace lowered by the path's congestion */

/* cbjxs_send_stats.path_state: the path as the delay-based congestion control judged it last. */
#define CBJXS_SEND_PATH_UNKNOWN 0u
#define CBJXS_SEND_PATH_CLEAN 1u
#define CBJXS_SEND_PATH_HOLD 2u
#define CBJXS_SEND_PATH_OVERUSE 3u

/* cbjxs_send_picture.format: the picture's memory layout. JPEG XS codes 4:2:0, 4:2:2 and 4:4:4 as
 * they are; NV12 is 4:2:0 with U and V interleaved in plane 1 (split on the copy). */
#define CBJXS_SEND_I420 1u
#define CBJXS_SEND_I422 2u
#define CBJXS_SEND_I444 3u
#define CBJXS_SEND_NV12 4u

/* cbjxs_send_set_log levels. */
#define CBJXS_SEND_LOG_ERROR 1
#define CBJXS_SEND_LOG_WARN 2
#define CBJXS_SEND_LOG_INFO 3

typedef struct cbjxs_sender cbjxs_sender;

typedef struct cbjxs_send_config {
	const char *target; /* "host:port", IPv4 */
	const char *cores;  /* the encoder's cores as a cpulist ("4-7"); NULL or "" = every online core */
	uint32_t emit_fps;  /* the caller's frame rate: at 30 or below every frame goes, above it only
	                     * the frames on the 30 fps grid of the genlock timecode */
	uint8_t stream_id;  /* the wire's camera id (a stream outside the CAM range, e.g. 100) */
	uint8_t bpp_min;    /* the automatic bpp's floor, 0 = 4 */
	uint8_t bpp_max;    /* its ceiling, 0 = 8, at most 12; min == max = that bpp, fixed (auto off) */
	uint8_t reserved;
	/* The send pacing in Mb/s of Ethernet wire (payload + 66 bytes per datagram). 0 = AUTOMATIC
	 * (ABI 3): 90 % of the receiver's link (its report) when that is slower than the sender's own
	 * link, else line rate; the frame spread until the first report names a link. 100..100000 =
	 * that rate, fixed (an override). Paced slices go at most 16 datagrams per send. */
	uint32_t pace_mbps;
	uint32_t reserved2;
} cbjxs_send_config;

typedef struct cbjxs_send_picture {
	uint32_t format; /* CBJXS_SEND_* */
	uint32_t width;
	uint32_t height;
	uint32_t reserved;
	/* Y, U, V (NV12: Y, UV, plane 2 unused). Read during the call only. */
	const uint8_t *planes[3];
	uint32_t strides[3]; /* bytes per row of each plane */
	uint32_t reserved2;
	/* The genlock wall-clock timecode: 100 ns since the Unix epoch (the stamp an NDI sender puts
	 * on the same frame). */
	int64_t timecode_100ns;
} cbjxs_send_picture;

/* One 5 s status window of the sender (the same counters the cambox logs as its
 * `jxs send: fps= ...` line). */
typedef struct cbjxs_send_stats {
	uint64_t seq; /* 1, 2, 3 ... one per window */
	double window_s;
	uint64_t frames; /* sent whole */
	uint64_t bytes;  /* codestream bytes */
	uint64_t dropped_busy; /* both pictures in use */
	uint64_t send_errors;
	uint64_t encode_errors; /* not coded or not sent whole, a refused picture included */
	uint64_t reports;       /* receiver reports read */
	double encode_ms_avg;   /* picture handed over -> its last packet sent */
	double encode_ms_max;
	double first_slice_ms_avg; /* picture copied -> its first packet sent */
	double copy_ms_avg;        /* the copy inside cbjxs_send_frame */
	double copy_ms_max;
	double report_age_s; /* since the last receiver report, -1 = never one */
	uint32_t bpp;        /* the bpp of the frames that went out last */
	uint8_t auto_bpp;    /* 1 = automatic */
	uint8_t reserved[3];
	uint32_t pace_mbps;   /* the pacing the window ended at, 0 = line rate */
	uint32_t pace_source; /* CBJXS_SEND_PACE_* (ABI 3; was reserved) */
	double wire_ms_avg; /* a whole frame's first send -> its last send returned */
	double wire_ms_max;
	uint32_t rx_link_mbps;  /* ABI 3: the receiver's link from its last report, 0 = unknown */
	uint32_t own_link_mbps; /* ABI 3: the sender's own link, 0 = unknown */
	uint32_t path_state;     /* ABI 3: CBJXS_SEND_PATH_* */
	uint32_t congestion_pct; /* ABI 3: the congestion control's pace factor, percent (0 = none yet) */
	uint32_t queue_delay_us; /* ABI 3: the receiver's newest queuing delay, UINT32_MAX = none */
	uint32_t reserved3;
	uint64_t fec_sent;     /* ABI 3: FEC parity datagrams sent (one per 12 of a slice) */
	uint64_t fec_repaired; /* ABI 3: datagrams the receiver's reports say its FEC rebuilt */
} cbjxs_send_stats;

typedef void (*cbjxs_send_log_fn)(int level, const char *message);

/* CBJXS_SEND_ABI_VERSION of the linked library. */
uint32_t cbjxs_send_abi_version(void);

/* Route the sender's own log lines (link ON, encoder open, every bpp step, the encoder-wedge
 * alarm, its 5 s status line) to fn, INFO and above, one NUL-terminated line per call, from any
 * of the sender's threads. NULL stops the forwarding. Returns 1 when the forwarding is in place,
 * 0 when the process already has another Rust log subscriber (then nothing is forwarded). */
int cbjxs_send_set_log(cbjxs_send_log_fn fn);

/* Start a sender (its socket and its two threads). NULL on failure, with the reason in err
 * (always NUL-terminated when err_len > 0). */
cbjxs_sender *cbjxs_send_open(const cbjxs_send_config *cfg, char *err, size_t err_len);

/* Hand one picture over. 1 = copied and queued; 0 = not taken (both pictures in use, counted as
 * dropped_busy; or a frame off the 30 fps grid, never counted); -1 = refused (an unknown format,
 * a geometry the format cannot take, a NULL plane or a stride shorter than its row; counted as an
 * encode error, cbjxs_send_take_error names it); -2 = a NULL argument or an internal failure (a
 * panic inside the library: the sender is unusable from then on; close it). */
int cbjxs_send_frame(cbjxs_sender *tx, const cbjxs_send_picture *pic);

/* 1 = *out holds a status window not handed out before, 0 = none new (out untouched), -1 = NULL. */
int cbjxs_send_get_stats(cbjxs_sender *tx, cbjxs_send_stats *out);

/* The newest error since the last call, NUL-terminated into buf. Returns the bytes written
 * without the NUL, 0 = none. */
size_t cbjxs_send_take_error(cbjxs_sender *tx, char *buf, size_t len);

/* Stop the threads (bounded: at most ~2 s for a wedged codec call) and free everything (NULL is a
 * no-op). */
void cbjxs_send_close(cbjxs_sender *tx);

#ifdef __cplusplus
}
#endif

#endif /* CAMERA_BOX_JXS_SEND_H */
