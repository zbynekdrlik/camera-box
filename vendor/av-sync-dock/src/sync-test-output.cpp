/*
OBS Audio Video Sync Dock
Copyright (C) 2023 Norihiro Kamae <norihiro@nagater.net>

This program is free software; you can redistribute it and/or modify
it under the terms of the GNU General Public License as published by
the Free Software Foundation; either version 2 of the License, or
(at your option) any later version.

This program is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
GNU General Public License for more details.

You should have received a copy of the GNU General Public License along
with this program; if not, write to the Free Software Foundation, Inc.,
51 Franklin Street, Fifth Floor, Boston, MA  02110-1301, USA.
*/

#include <obs-module.h>
#include <inttypes.h>
#include <deque>
#include <list>
#include <stdlib.h>
#include <algorithm>
#include <mutex>
#include <atomic>
#include <complex>
#include <vector>
#include <utility>
#include "quirc.h"
#include "sync-test-output.hpp"
#include "peak-finder.hpp"
#include "camera-box-qr.hpp"
#include "camera-box-audio.hpp"
#include "camera-box-video.hpp"

#include "plugin-macros.generated.h"

#define N_CORNERS 4

#define N_AUDIO_SYMBOLS 16
#define N_SYMBOL_BUFFER 20

/* #398 fix: the live camera-box video<->audio ring (`cb_video_ts_ns` below) is keyed on
 * `frame_id_to_index` (the frame_id's low byte, see src/qpsk_marker.rs), so its natural cycle
 * length is 256 frames at the FIXED camera-box painter rate (60 fps) -- independent of whatever
 * fps the dock itself happens to capture at. Used by `resolve_ring_lap_offset_ns` below to
 * disambiguate which lap of the ring a stored slot value belongs to. Mirrors
 * `AV_SYNC_RING_CYCLE_NS` in src/qpsk_marker.rs -- keep both in sync. */
#define CAMERA_BOX_RING_SLOTS 256ULL
#define CAMERA_BOX_SOURCE_FPS 60ULL
#define CAMERA_BOX_RING_CYCLE_NS (CAMERA_BOX_RING_SLOTS * 1000000000ULL / CAMERA_BOX_SOURCE_FPS)

/* #398 fix: rolling window for the live-display median smoothing, see `cb_smooth_offset_ns`. */
#define CAMERA_BOX_SMOOTH_WINDOW_NS 1000000000ULL

/* #690: rate limit for the periodic audio/video decode diagnostic blog() line -- see
 * st_raw_audio_camera_box's own comment for what it answers. 10s: frequent enough to be useful
 * within a short live-check session, rare enough to never spam the OBS log. */
#define CAMERA_BOX_DIAG_LOG_INTERVAL_NS 10000000000ULL

/* #926: the video-delay actuator `CbDockLockCorrector` drives -- the SAME per-source
 * `genlock_latency_ms_src` knob `scripts/av_sync_calibrate.py` already nudges OFFLINE, on the
 * SAME 'NDI 2ME PGM' program NDI source (`av_sync_calibrate.py`'s own DEFAULT_SOURCE). Hardcoded,
 * no env var, per this repo's hard-lock philosophy (issue #257: no forgettable/mysterious knobs) --
 * matching how every other rig constant in this file is a compile-time literal, not a runtime
 * override. */
#define CAMERA_BOX_LOCK_SOURCE_NAME "NDI 2ME PGM"

/* #926 fix-up (review finding 6): how long (ns) a video-QR-decode signal is trusted as "the test
 * signal is genuinely still here" before the corrector refuses to actuate on it. Generously above
 * the ~3-5s real marker cadence and the video ring's own ~4.3s lap (CAMERA_BOX_RING_CYCLE_NS), so
 * normal jitter never trips it, while a video path that has GENUINELY stopped (real event: camera
 * unplugged, QR obscured) ages out well within one operator glance at the log. `cb_video_valid[]`
 * otherwise latches true FOREVER once any frame with a given idx8 ever decoded -- this bounds how
 * old a paired video timestamp is allowed to be before it is trusted for either ring pairing or
 * actuation. */
#define CAMERA_BOX_TEST_SIGNAL_FRESH_NS 20000000000ULL

/* There are several reason to limit the width and the height.
 * - Since a square of 3/8 QR-code-length is calculated using uint32_t,
 *   the 3/8 of width or height cannot exceed the square root of uint32_t max.
 * - Since a sum of the pixels in a line is accumurated on uint32_t,
 *   the width must be less than 1/255 of uint32_t max.
 *   */
#define MAX_WIDTH_HEIGHT 87378u

struct st_audio_buffer
{
	std::deque<std::pair<int32_t, int32_t>> buffer;

	void push_back(int16_t xr, int16_t xi, size_t length)
	{
		int32_t vr = xr, vi = xi;
		if (buffer.size()) {
			vr += buffer.back().first;
			vi += buffer.back().second;
		}
		buffer.push_back(std::make_pair(vr, vi));

		if (buffer.size() <= length)
			return;

		buffer.pop_front();
	};

	std::pair<int32_t, int32_t> sum(size_t n_from_last)
	{
		if (buffer.size() <= 0)
			return std::make_pair(0, 0);
		if (n_from_last >= buffer.size())
			return buffer[0];
		return buffer[buffer.size() - n_from_last - 1];
	}
};

std::pair<int32_t, int32_t> operator-(std::pair<int32_t, int32_t> a, std::pair<int32_t, int32_t> b)
{
	return std::make_pair(a.first - b.first, a.second - b.second);
}

std::complex<float> int16_to_complex(std::pair<int32_t, int32_t> x)
{
	return std::complex<float>((float)x.first / 32768.0f, (float)x.second / 32768.0f);
}

struct corner_type
{
	uint32_t x, y;
	uint32_t r = 0;
};

struct sync_test_output
{
	obs_output_t *context;

	/* Configuration from OBS output context */
	uint32_t video_width = 0, video_height = 0;
	uint32_t video_pixelsize = 0;
	uint32_t video_pixeloffset = 0;
	uint8_t (*video_get_intensity)(const uint8_t *data) = nullptr;

	uint32_t audio_sample_rate = 0;
	size_t audio_channels = 0;

	/* Sync pattern detection from video */
	uint64_t start_ts = 0;

	struct quirc *qr = nullptr;
	uint32_t qr_step;
	struct corner_type qr_corners[N_CORNERS];
	st_qr_data qr_data;

	int64_t video_level_prev = 0;
	uint64_t video_level_prev_ts = 0;
	uint64_t video_marker_max_ts = 0;

	/* Sync pattern detection from audio */
	struct st_audio_buffer audio_buffer;
	struct peak_finder audio_marker_finder;
	uint32_t last_audio_index_max = 256;

	/* Multiplex sync pattern detection result */
	std::list<struct sync_index> sync_indices;

	std::mutex mutex;

	/* Audio pattern information from video to audio */
	uint32_t f = 0;
	uint32_t c = 0;
	uint32_t q_ms = 0;

	uint32_t f_last = 0;
	uint32_t c_last = 0;

	/* #398 Option A: camera-box's own dual-QR video path. Decoupled from norihiro's
	 * `sync_indices` list (that mechanism assumes a video report per DETECTED marker cycle,
	 * roughly every `q_ms*3` — our QR reports every SINGLE painted frame, 60/s, which would fill
	 * and evict the 128-entry list long before a ~3-5 s-cadence audio marker arrives). Instead: a
	 * direct ring indexed by the frame_id low byte (the SAME value the audio index carries, see
	 * `frame_id_to_index` in src/qpsk_marker.rs), overwritten every ~4.3 s (256 frames @ 60 fps).
	 * The video and audio paths for the SAME frame can arrive up to ~2 s apart in EITHER
	 * direction (the OBS program VIDEO track carries extra genlock A/V-alignment latency the
	 * near-zero-latency QPSK AUDIO track does not — audio usually decodes FIRST in production), so
	 * a stored slot can be stale by exactly one lap by the time its audio marker arrives;
	 * `resolve_ring_lap_offset_ns` (#398 review fix) corrects for that. */
	uint64_t cb_video_ts_ns[256] = {0};
	bool cb_video_valid[256] = {false};
	bool cb_mode_active = false;

	/* #926 fix-up (review finding 6): the video_ts of the MOST RECENT successful video-QR decode,
	 * across ANY ring slot -- the overall "is the test signal genuinely still here right now"
	 * signal, distinct from a single ring slot's own per-idx8 freshness (both are checked against
	 * CAMERA_BOX_TEST_SIGNAL_FRESH_NS). Written on the video thread in `cb_video_qr_record`, read
	 * on the audio thread under the same mutex as the other cb_video_* fields. */
	uint64_t cb_video_last_decode_ts_ns = 0;

	/* #398 fix: rolling history of recently-resolved (audio_ts, offset_ns) samples for
	 * `cb_smooth_offset_ns` — median-smooths the displayed offset so a single false CRC-4 accept
	 * (~1/16 likely on real program audio) can't show garbage (review MEDIUM finding). Touched
	 * only from the audio-decode thread; no cross-thread sharing, but guarded by the same mutex
	 * as the other cb_* fields for consistency. */
	std::deque<std::pair<uint64_t, int64_t>> cb_offset_history;

	/* #398 fix (audio index never locked): norihiro's own audio demod is broken at the rig's c=1
	 * (its `c1 = c/2` half-symbol resolution is 0, collapsing the preamble finder; and it decodes
	 * only 6 symbols). These drive camera-box's OWN proven demod instead — the streaming
	 * `decode_markers` (round-trip tested for all 256 indices at c=1) + the robust rolling
	 * densest-cluster estimator (survives the CRC-4 false-decode flood the offline path also fights,
	 * where a plain 1 s median would not). Mirrors `src/av_sync_dock.rs`; touched only on the audio
	 * thread. `cb_qr` is a SECOND quirc context sized to the better-scaled top-band decode (below).
	 * `cb_src_buf` is a reused top-band gather buffer (no per-frame alloc). */
	struct quirc *cb_qr = nullptr;
	camerabox::StreamingMarkerDecoder *cb_audio_dec = nullptr;
	camerabox::RollingOffsetCluster cb_offset_cluster = camerabox::RollingOffsetCluster::dock();
	uint64_t cb_audio_pushed = 0;
	std::vector<uint8_t> cb_src_buf;

	/* #921: skips the redundant per-frame quirc_resize(cb_qr, ...) once the decode-plan geometry
	 * (a pure function of video_width/video_height, fixed for the output's lifetime) stops
	 * changing -- see camera-box-video.hpp's own doc comment on CbQrResizeCache for why this
	 * matters. Touched only on the video thread (same thread that owns cb_qr itself). */
	camerabox::CbQrResizeCache cb_qr_resize_cache;

	/* #634: audit-log lock/unlock/offset-update transitions of the cluster above, so a live
	 * desync (like the closed #529) can be diagnosed from the OBS log alone. Pure/tested in
	 * camera-box-audio.hpp (tests/av_sync_dock_audit_log.rs) — touched only on the audio thread. */
	camerabox::CbLockAuditTracker cb_lock_audit;

	/* #926: holds CAMERA_BOX_LOCK_SOURCE_NAME's genlock_latency_ms_src so the dock's own displayed
	 * offset (audio_ts - video_ts) never rests negative ("audio early", a forbidden steady state).
	 * Only ever acts on a Locked/Updated lock-audit transition above; an Unlocked transition (real
	 * event, no test signal) freezes it -- see camera-box-audio.hpp's own doc comment. Touched only
	 * on the audio thread. */
	camerabox::CbDockLockCorrector cb_lock_corrector;

	/* #690: periodic live diagnostic -- tells a live session WHY the audio index/latency never
	 * lock (does the demod see nothing / decode garbage / decode fine but never ring-hit or
	 * cluster) and how well the video-QR decode is doing, from the OBS log alone (no rig access
	 * needed to read it). video counters are written on the VIDEO thread and read on the AUDIO
	 * thread (which owns the periodic log) -- atomic, no lock needed for plain counters. Ring
	 * hit/miss and the log-rate-limit timestamp are touched only on the audio thread. */
	std::atomic<uint64_t> cb_video_frames_seen{0};
	std::atomic<uint64_t> cb_video_frames_decoded{0};
	uint64_t cb_ring_hits = 0;   // decoded audio marker whose idx8 already had a valid video ring slot
	uint64_t cb_ring_misses = 0; // decoded audio marker with no video ring slot yet (too early / lap gap)
	bool cb_lock_state = false;  // last-known cluster lock state (mirrors CbLockAuditTracker's own)
	uint64_t cb_diag_last_log_ns = 0;

	/* #1177: watches whether the measurement INPUT is still advancing (video_decoded + crc_ok). When
	 * the marker/QR input disappears (EVENT mode) every existing unlock path is dead (all are
	 * decoded-marker-driven), so cb_lock_state would hold `yes` and the dock would show the last
	 * offset forever. Evaluated once per diag tick (below) -- the audio thread keeps ticking in EVENT
	 * mode -- and drives the sync_stale_changed signal + the diag line's state=LIVE/STALE token.
	 * Pure/tested in av_sync_dock.rs, mirrored in camera-box-audio.hpp. Touched only on the audio
	 * thread (same thread that owns the diag block). */
	camerabox::CbDockInputStaleness cb_input_staleness;

	/* #1153: dead-pairing watchdog -- fires when the marker<->QR pairing stays dead (no
	 * meaningful ring-hit advance, no genuine lock) for a full epoch while the measurement input
	 * itself keeps flowing; the diag tick then resets ALL in-dock pairing state and re-acquires
	 * from scratch, so a manual OBS restart is never the only cure for a sticky
	 * post-latency-step unlock. Pure/tested in av_sync_dock.rs, mirrored in
	 * camera-box-audio.hpp. Touched only on the audio thread (same thread that owns the diag
	 * block). */
	camerabox::CbDockPairingWatchdog cb_pairing_watchdog;

	/* #926 fix-up (review finding 9/16): latches so each condition logs ONCE (and again after it
	 * clears and re-occurs) instead of spamming a blog() line per trusted marker while the
	 * condition persists. Touched only on the audio thread. */
	bool cb_lock_source_missing_logged = false; // CAMERA_BOX_LOCK_SOURCE_NAME not found
	bool cb_rail_pinned_logged = false;         // pinned at a hardware rail with audio still early

	~sync_test_output()
	{
		if (qr)
			quirc_destroy(qr);
		if (cb_qr)
			quirc_destroy(cb_qr);
		delete cb_audio_dec;
	}
};

static void video_marker_found(struct sync_test_output *st, uint64_t timestamp, float score);

static const char *st_get_name(void *)
{
	return "sync-test-output";
}

static void *st_create(obs_data_t *, obs_output_t *output)
{
	static const char *signals[] = {
		"void video_marker_found(ptr data)",
		"void audio_marker_found(ptr data)",
		"void qrcode_found(int timestamp, int x0, int y0, int x1, int y1, int x2, int y2, int x3, int y3)",
		"void sync_found(ptr data)",
		/* #926: the Locked/Unlocked half of the SAME cb_lock_audit transitions already blog()'d --
		 * so the dock UI can show a plain "locked, aligning" / "no test signal, holding" status
		 * without polling. Deliberately NOT fired on every Updated (no coarse state change). */
		"void lock_state_changed(bool locked)",
		/* #1177: fired on the boundary crossing when the measurement INPUT (marker/QR decode) stops
		 * advancing (EVENT mode) or resumes -- distinct from lock_state_changed, which is driven by a
		 * DECODED marker and so can never fire when the input is exactly what went away. Lets the dock
		 * show an explicit STALE/NO-SIGNAL state instead of holding the last offset as if live. */
		"void sync_stale_changed(bool stale)",
		NULL,
	};
	signal_handler_add_array(obs_output_get_signal_handler(output), signals);

	auto *st = new sync_test_output;
	st->context = output;

	return st;
}

static void st_destroy(void *data)
{
	auto *st = (struct sync_test_output *)data;
	delete st;
}

static uint8_t get_intensity_10le(const uint8_t *data)
{
	uint16_t v = (data[0] >> 2) | (data[1] << 6);
	return (uint8_t)std::min<uint16_t>(v, 0xFF);
}

static bool st_start(void *data)
{
	auto *st = (struct sync_test_output *)data;

	const video_t *video = obs_output_video(st->context);
	if (!video) {
		blog(LOG_ERROR, "no video");
		return false;
	}
	const audio_t *audio = obs_output_audio(st->context);
	if (!audio) {
		blog(LOG_ERROR, "no audio");
		return false;
	}

	st->video_width = video_output_get_width(video);
	st->video_height = video_output_get_height(video);
	if (st->video_width > MAX_WIDTH_HEIGHT || st->video_height > MAX_WIDTH_HEIGHT) {
		blog(LOG_ERROR, "Requested size %ux%u exceeds maximum size %ux%u", st->video_width, st->video_height,
		     MAX_WIDTH_HEIGHT, MAX_WIDTH_HEIGHT);
		return false;
	}

	enum video_format video_format = video_output_get_format(video);
	switch (video_format) {
	case VIDEO_FORMAT_I420:
	case VIDEO_FORMAT_NV12:
	case VIDEO_FORMAT_I444:
	case VIDEO_FORMAT_I422:
	case VIDEO_FORMAT_I40A:
	case VIDEO_FORMAT_I42A:
	case VIDEO_FORMAT_YUVA:
		st->video_pixelsize = 1;
		st->video_pixeloffset = 0;
		st->video_get_intensity = nullptr;
		break;
	case VIDEO_FORMAT_I010:
		st->video_pixelsize = 2;
		st->video_pixeloffset = 0;
		st->video_get_intensity = get_intensity_10le;
		break;
	case VIDEO_FORMAT_P010:
		st->video_pixelsize = 2;
		st->video_pixeloffset = 1;
		st->video_get_intensity = nullptr;
		break;
#if LIBOBS_API_VER >= MAKE_SEMANTIC_VERSION(29, 1, 0)
	case VIDEO_FORMAT_P216:
	case VIDEO_FORMAT_P416:
		st->video_pixelsize = 2;
		st->video_pixeloffset = 1; // little endian
		st->video_get_intensity = nullptr;
		break;
#endif
	case VIDEO_FORMAT_RGBA:
	case VIDEO_FORMAT_BGRA:
	case VIDEO_FORMAT_BGRX:
		st->video_pixelsize = 4;
		st->video_pixeloffset = 1; // green channel
		st->video_get_intensity = nullptr;
		break;
	default:
		blog(LOG_ERROR, "unsupported pixel format %d", video_format);
		return false;
	}

	uint32_t qr_width = st->video_width;
	uint32_t qr_height = st->video_height;
	st->qr_step = 1;
	while (qr_width * qr_height > 640 * 480) {
		qr_width /= 2;
		qr_height /= 2;
		st->qr_step *= 2;
	}
	if (!st->qr)
		st->qr = quirc_new();
	if (!st->qr) {
		blog(LOG_ERROR, "failed to create QR code encoding context");
		return false;
	}
	if (quirc_resize(st->qr, qr_width, qr_height) < 0) {
		blog(LOG_ERROR, "failed to set-up QR code encoding context");
		return false;
	}

	st->audio_sample_rate = audio_output_get_sample_rate(audio);
	st->audio_channels = audio_output_get_channels(audio);

	obs_output_begin_data_capture(st->context, OBS_OUTPUT_VIDEO | OBS_OUTPUT_AUDIO);

	return true;
}

static void st_stop(void *data, uint64_t)
{
	auto *st = (struct sync_test_output *)data;

	obs_output_end_data_capture(st->context);
}

template<typename T> T sq(T x)
{
	return x * x;
}

static inline uint32_t diff_u32(uint32_t x, uint32_t y)
{
	if (x < y)
		return y - x;
	else
		return x - y;
}

static inline uint32_t sqrt_u32(uint32_t x)
{
	uint32_t r = 0;
	for (uint32_t b = 1 << 15; b; b >>= 1) {
		if (sq(r | b) <= x)
			r |= b;
	}
	return r;
}

static inline int qrcode_length(const struct corner_type *cc)
{
	auto l02 = hypotf((float)((int)cc[0].x - (int)cc[2].x), (float)((int)cc[0].y - (int)cc[2].y));
	auto l13 = hypotf((float)((int)cc[1].x - (int)cc[3].x), (float)((int)cc[1].y - (int)cc[3].y));
	return (int)((l02 + l13) * (float)(M_SQRT1_2 / 2.0f));
}

static inline void adjust_corners(struct corner_type *cc)
{
	int cx = 0, cy = 0;
	for (int i = 0; i < 4; i++) {
		cx += cc[i].x;
		cy += cc[i].y;
	}

	cx /= 4;
	cy /= 4;
	int r = qrcode_length(cc) / 4;

	// Move (x, y) to center side so that the circles will cover the pattern.
	for (int i = 0; i < 4; i++) {
		cc[i].x = (cc[i].x * 15 + cx * 9) / 24;
		cc[i].y = (cc[i].y * 15 + cy * 9) / 24;
		cc[i].r = r;
	}
}

static void signal_qrcode_found(obs_output_t *ctx, uint64_t timestamp, const struct corner_type *corners)
{
	uint8_t stack[384];
	struct calldata cd;
	calldata_init_fixed(&cd, stack, sizeof(stack));
	auto *sh = obs_output_get_signal_handler(ctx);

	calldata_set_int(&cd, "timestamp", timestamp);
	calldata_set_int(&cd, "x0", corners[0].x);
	calldata_set_int(&cd, "y0", corners[0].y);
	calldata_set_int(&cd, "x1", corners[1].x);
	calldata_set_int(&cd, "y1", corners[1].y);
	calldata_set_int(&cd, "x2", corners[2].x);
	calldata_set_int(&cd, "y2", corners[2].y);
	calldata_set_int(&cd, "x3", corners[3].x);
	calldata_set_int(&cd, "y3", corners[3].y);
	signal_handler_signal(sh, "qrcode_found", &cd);
}

/* #398 Option A: record a decoded camera-box dual-QR into the direct video<->audio ring keyed on the
 * frame_id low byte (the SAME value the audio index carries) and set the FIXED rig audio params +
 * dock-UI qr_data. The SINGLE source of truth for that update, called by BOTH the norihiro
 * whole-frame decode (kept for the phone method) and the #398 better-scaled top-band decode below.
 * `video_ts` is already frame-relative (`frame->timestamp - start_ts`). */
static void cb_video_qr_record(struct sync_test_output *st, uint32_t frame_id, uint64_t video_ts)
{
	uint8_t low = (uint8_t)(frame_id & 0xFFu);
	{
		// Same mutex the audio thread locks to READ these — the video ring + f/c/q_ms are written
		// here (video thread) and read on the audio thread; both sides must take the lock.
		std::unique_lock<std::mutex> lock(st->mutex);
		st->cb_video_ts_ns[low] = video_ts;
		st->cb_video_valid[low] = true;
		st->cb_mode_active = true;
		st->cb_video_last_decode_ts_ns = video_ts; // #926 fix-up finding 6: overall freshness signal
		st->f = CAMERA_BOX_AUDIO_F_HZ;
		st->c = CAMERA_BOX_AUDIO_C;
		st->q_ms = CAMERA_BOX_AUDIO_Q_MS;
	}
	// Reuse the existing dock-UI plumbing (video index / missed% / frequency labels).
	st->qr_data.f = CAMERA_BOX_AUDIO_F_HZ;
	st->qr_data.c = CAMERA_BOX_AUDIO_C;
	st->qr_data.q_ms = CAMERA_BOX_AUDIO_Q_MS;
	st->qr_data.index = low;
	st->qr_data.index_max = 256;
	st->qr_data.valid = true;
}

static void st_raw_video_qrcode_decode(struct sync_test_output *st, struct video_data *frame)
{
	int w, h;
	auto qr = st->qr;
	uint8_t *qrbuf = quirc_begin(qr, &w, &h);

	const auto qr_step = st->qr_step;
	const auto pixelsize = st->video_pixelsize * qr_step;
	const uint8_t *linedata = frame->data[0] + frame->linesize[0] * (qr_step / 2);
	auto *ptr = qrbuf;
	for (int y = 0; y < h; y++) {
		const uint8_t *data = linedata + st->video_pixeloffset + st->video_pixelsize * (qr_step / 2);
		if (!st->video_get_intensity) {
			for (int x = 0; x < w; x++) {
				*ptr++ = *data;
				data += pixelsize;
			}
		}
		else {
			for (int x = 0; x < w; x++) {
				*ptr++ = st->video_get_intensity(data);
				data += pixelsize;
			}
		}

		linedata += frame->linesize[0] * qr_step;
	}

	quirc_end(qr);

	int num_codes = quirc_count(qr);

	for (int i = 0; i < num_codes; i++) {
		// (x0, y0): top left
		// (x1, y1): top right
		// (x2, y2): bottom right
		// (x3, y3): bottom left

		struct quirc_code code;
		struct quirc_data data;
		quirc_extract(qr, i, &code);
		auto err = quirc_decode(&code, &data);
		if (err == QUIRC_ERROR_DATA_ECC) {
			quirc_flip(&code);
			err = quirc_decode(&code, &data);
		}

		if (err)
			continue;

		data.payload[QUIRC_MAX_PAYLOAD - 1] = 0;

		/* #398 Option A: try camera-box's own dual-QR format FIRST. It carries frame identity
		 * (frame_id), not audio params, so on success we record the video timestamp directly by
		 * frame_id low byte and set the FIXED rig audio params — then move to the next QR code
		 * without touching norihiro's own decode/marker-window state at all (his phone-based
		 * method, still fully supported below, is untouched). */
		CameraBoxQrData cb;
		if (decode_camera_box_qr((char *)data.payload, &cb)) {
			for (int j = 0; j < 4; j++) {
				st->qr_corners[j].x = code.corners[j].x * st->qr_step;
				st->qr_corners[j].y = code.corners[j].y * st->qr_step;
			}
			signal_qrcode_found(st->context, frame->timestamp - st->start_ts, st->qr_corners);
			cb_video_qr_record(st, cb.frame_id, frame->timestamp - st->start_ts);
			video_marker_found(st, frame->timestamp, 1.0f);
			continue;
		}

		if (!st->qr_data.decode((char *)data.payload))
			continue;

		for (int j = 0; j < 4; j++) {
			st->qr_corners[j].x = code.corners[j].x * st->qr_step;
			st->qr_corners[j].y = code.corners[j].y * st->qr_step;
		}

		signal_qrcode_found(st->context, frame->timestamp - st->start_ts, st->qr_corners);

		adjust_corners(st->qr_corners);

		if (st->qr_data.f > 0 && st->qr_data.c > 0) {
			std::unique_lock<std::mutex> lock(st->mutex);
			st->f = st->qr_data.f;
			st->c = st->qr_data.c;
			st->q_ms = st->qr_data.q_ms;
		}

		st->video_marker_max_ts = frame->timestamp + st->qr_data.q_ms * 3 * 1000000;
		st->video_level_prev = 0;
	}
}

static void st_raw_video_find_marker(struct sync_test_output *st, struct video_data *frame)
{
	int64_t sum = 0;

	if (frame->timestamp > st->video_marker_max_ts) {
		st->video_level_prev = 0;
		return;
	}

	const uint8_t *linedata = frame->data[0];
	const uint32_t pixelsize = st->video_pixelsize;

	for (size_t i = 0; i < N_CORNERS; i++) {
		const struct corner_type c = st->qr_corners[i];
		if (c.r == 0)
			return;
		uint32_t y0 = c.y > c.r ? c.y - c.r : 0;
		uint32_t y1 = std::min(c.y + c.r, st->video_height);
		uint32_t sq_r = sq(c.r);

		for (uint32_t y = y0; y < y1; y++) {
			uint32_t dx = sqrt_u32(sq_r - sq(diff_u32(y, c.y)));
			uint32_t x0 = c.x > dx ? c.x - dx : 0;
			uint32_t x1 = std::min(c.x + dx, st->video_width);

			const uint8_t *data =
				linedata + frame->linesize[0] * y + st->video_pixeloffset + st->video_pixelsize * x0;

			uint32_t line_sum = 0;

			if (!st->video_get_intensity) {
				for (uint32_t x = x0; x < x1; x++) {
					line_sum += *data;
					data += pixelsize;
				}
			}
			else {
				for (uint32_t x = x0; x < x1; x++) {
					line_sum += st->video_get_intensity(data);
					data += pixelsize;
				}
			}

			if (i & 1)
				sum += line_sum;
			else
				sum -= line_sum;
		}
	}

	// blog(LOG_INFO, "st_raw_video-plot: %.03f %f", (frame->timestamp - st->start_ts) * 1e-9, (double)sum / (255.0 * M_PI * sq(st->qr_corners[0].r)));

	if (st->qr_data.valid && st->video_level_prev < 0 && sum >= 0) {
		/* Calculate the time half frame later than the zero-cross of `sum`. */
		uint64_t t = frame->timestamp - st->video_level_prev_ts;
		uint64_t add = util_mul_div64(t, sum - st->video_level_prev * 3, (sum - st->video_level_prev) * 2);
		video_marker_found(st, st->video_level_prev_ts + add, (float)(sum - st->video_level_prev));
	}
	st->video_level_prev = sum;
	st->video_level_prev_ts = frame->timestamp;
}

static bool is_overlapped(uint32_t index, uint32_t index_max, uint32_t next_index)
{
	return index_max && ((index_max + next_index - index) % index_max) > index_max / 2;
}

static void signal_sync_found(obs_output_t *ctx, const struct sync_index *si)
{
	uint8_t stack[64];
	struct calldata cd;
	calldata_init_fixed(&cd, stack, sizeof(stack));
	auto *sh = obs_output_get_signal_handler(ctx);

	calldata_set_ptr(&cd, "data", const_cast<sync_index *>(si));
	signal_handler_signal(sh, "sync_found", &cd);
}

/* #926: fired on the Locked/Unlocked half of a cb_lock_audit transition -- see the signal's own
 * registration comment in st_create(). */
static void signal_lock_state_changed(obs_output_t *ctx, bool locked)
{
	uint8_t stack[64];
	struct calldata cd;
	calldata_init_fixed(&cd, stack, sizeof(stack));
	auto *sh = obs_output_get_signal_handler(ctx);

	calldata_set_bool(&cd, "locked", locked);
	signal_handler_signal(sh, "lock_state_changed", &cd);
}

/* #1177: fired on the boundary crossing when the measurement input goes STALE (no marker/QR decode
 * advance for CB_DOCK_INPUT_STALE_NS) or recovers -- see the signal's own registration comment. */
static void signal_stale_changed(obs_output_t *ctx, bool stale)
{
	uint8_t stack[64];
	struct calldata cd;
	calldata_init_fixed(&cd, stack, sizeof(stack));
	auto *sh = obs_output_get_signal_handler(ctx);

	calldata_set_bool(&cd, "stale", stale);
	signal_handler_signal(sh, "sync_stale_changed", &cd);
}

static void sync_index_found(struct sync_test_output *st, int index, uint64_t ts, bool is_video, uint32_t index_max)
{
	std::unique_lock<std::mutex> lock(st->mutex);

	for (auto it = st->sync_indices.begin(); it != st->sync_indices.end();) {
		if ((it->video_ts && is_video) || (it->audio_ts && !is_video)) {
			if (is_overlapped(it->index, it->index_max, index)) {
				st->sync_indices.erase(it++);
				continue;
			}
		}

		if (it->index != index) {
			it++;
			continue;
		}

		if ((it->video_ts && !is_video) || (it->audio_ts && is_video)) {
			(is_video ? it->video_ts : it->audio_ts) = ts;
			if (is_video)
				it->index_max = index_max;

			signal_sync_found(st->context, &*it);

			/* Do not erase `it` so that `identify_audio_index_max` can refer the last found pattern.
			 * Current `it` will be erased at the next call of this function. */
			return;
		}

		/* Remove the old one. Later, insert the new one to the end */
		st->sync_indices.erase(it);
		break;
	}

	while (st->sync_indices.size() >= 128)
		st->sync_indices.erase(st->sync_indices.begin());

	auto &ref = st->sync_indices.emplace_back();
	ref.index = index;
	(is_video ? ref.video_ts : ref.audio_ts) = ts;
	ref.index_max = index_max;
}

static void video_marker_found(struct sync_test_output *st, uint64_t timestamp, float score)
{
	uint8_t stack[64];
	struct calldata cd;
	calldata_init_fixed(&cd, stack, sizeof(stack));
	auto *sh = obs_output_get_signal_handler(st->context);

	struct video_marker_found_s data;
	data.timestamp = timestamp - st->start_ts;
	data.score = score;
	data.qr_data = st->qr_data;

	calldata_set_ptr(&cd, "data", &data);
	signal_handler_signal(sh, "video_marker_found", &cd);

	/* #398 fix (review LOW finding): once camera-box mode is active, the direct video<->audio
	 * ring in `st_raw_audio_decode_data` is the SOLE authoritative sync_found source (lap-resolved
	 * + smoothed, see below). Feeding the SAME index into norihiro's legacy list-based
	 * `sync_index_found` here too would let it emit a SECOND, uncorrected `sync_found` (no lap
	 * fix, no smoothing) for the same marker — a duplicate signal path flashing a conflicting
	 * number. Skip it while camera-box mode is active. */
	bool cb_active;
	{
		std::unique_lock<std::mutex> lock(st->mutex);
		cb_active = st->cb_mode_active;
	}
	if (!cb_active)
		sync_index_found(st, data.qr_data.index, data.timestamp, true, data.qr_data.index_max);
}

/* #398 fix (video index 98% missed): norihiro's whole-frame decode subsamples the WHOLE frame by
 * `qr_step` (÷8 at a 4K program output) with NEAREST sampling, shrinking each ~700 px dual-QR half to
 * ~87 px so quirc misses ~98 % of frames and the ring is almost never populated. This gives quirc a
 * fair look: decode only the TOP band (where the top-anchored dual-QR lives), AREA-averaged (not
 * nearest) to a scale that keeps each QR large, with an Otsu-binarized retry — the techniques
 * `src/probe/qr.rs` proved on the real soft optical stream frames. The plan geometry + downscale +
 * Otsu are the Tier-0-tested `camera-box-video.hpp` mirror; only the quirc driving is here. Returns
 * true if any camera-box QR decoded (and records it into the ring). */
static bool st_raw_video_camera_box_decode(struct sync_test_output *st, struct video_data *frame)
{
	st->cb_video_frames_seen.fetch_add(1, std::memory_order_relaxed);

	camerabox::CbTopBandPlan plan = camerabox::cb_top_band_decode_plan(st->video_width, st->video_height);
	if (plan.band_h == 0 || plan.dst_w == 0 || plan.dst_h == 0)
		return false;

	// Gather the TOP band (rows 0..band_h) into a tight full-res luma buffer (reused, no per-frame
	// alloc), honoring the pixel format's stride / offset / intensity extractor.
	const size_t need = (size_t)st->video_width * plan.band_h;
	if (st->cb_src_buf.size() < need)
		st->cb_src_buf.resize(need);
	uint8_t *src = st->cb_src_buf.data();
	const uint32_t pixelsize = st->video_pixelsize;
	for (uint32_t y = 0; y < plan.band_h; y++) {
		const uint8_t *line = frame->data[0] + (size_t)frame->linesize[0] * y + st->video_pixeloffset;
		uint8_t *dstrow = src + (size_t)y * st->video_width;
		if (!st->video_get_intensity) {
			const uint8_t *d = line;
			for (uint32_t x = 0; x < st->video_width; x++) {
				dstrow[x] = *d;
				d += pixelsize;
			}
		} else {
			const uint8_t *d = line;
			for (uint32_t x = 0; x < st->video_width; x++) {
				dstrow[x] = st->video_get_intensity(d);
				d += pixelsize;
			}
		}
	}

	if (!st->cb_qr)
		st->cb_qr = quirc_new();
	if (!st->cb_qr)
		return false;
	/* #921: geometry is a pure function of video_width/video_height, fixed for the output's
	 * lifetime -- skip the resize once it has already been applied at this size (quirc_resize()
	 * has no early-out of its own and unconditionally reallocs 3 buffers every call). On failure,
	 * reset the cache so the very next frame retries fresh instead of wrongly trusting a failed
	 * resize. */
	if (camerabox::cb_qr_resize_needed(st->cb_qr_resize_cache, plan.dst_w, plan.dst_h)) {
		if (quirc_resize(st->cb_qr, plan.dst_w, plan.dst_h) < 0) {
			st->cb_qr_resize_cache = camerabox::CbQrResizeCache();
			return false;
		}
	}

	bool found_any = false;
	// Pass 0: plain area-downscale + quirc's own adaptive threshold. Pass 1 (only if our QR was not
	// found): Otsu-binarize the same downscaled band — the hard black/white cut that locks quirc's
	// finder on a soft optical capture (#363).
	for (int pass = 0; pass < 2 && !found_any; pass++) {
		int w = 0, h = 0;
		uint8_t *qbuf = quirc_begin(st->cb_qr, &w, &h);
		camerabox::cb_box_downscale_luma(src, st->video_width, plan.band_h, qbuf, (uint32_t)w,
		                                 (uint32_t)h);
		if (pass == 1)
			camerabox::cb_binarize_otsu(qbuf, (size_t)w * (size_t)h);
		quirc_end(st->cb_qr);

		int num_codes = quirc_count(st->cb_qr);
		for (int i = 0; i < num_codes; i++) {
			struct quirc_code code;
			struct quirc_data data;
			quirc_extract(st->cb_qr, i, &code);
			auto err = quirc_decode(&code, &data);
			if (err == QUIRC_ERROR_DATA_ECC) {
				quirc_flip(&code);
				err = quirc_decode(&code, &data);
			}
			if (err)
				continue;
			data.payload[QUIRC_MAX_PAYLOAD - 1] = 0;

			CameraBoxQrData cb;
			if (!decode_camera_box_qr((char *)data.payload, &cb))
				continue;

			// Map quirc corners (downscaled top-band coords) back to FRAME coords (the band is
			// top-anchored at y=0). Cosmetic — the ring/marker use frame_id, not the corners.
			for (int j = 0; j < 4; j++) {
				st->qr_corners[j].x =
					(uint32_t)((uint64_t)code.corners[j].x * st->video_width / (w > 0 ? w : 1));
				st->qr_corners[j].y =
					(uint32_t)((uint64_t)code.corners[j].y * plan.band_h / (h > 0 ? h : 1));
			}
			signal_qrcode_found(st->context, frame->timestamp - st->start_ts, st->qr_corners);
			cb_video_qr_record(st, cb.frame_id, frame->timestamp - st->start_ts);
			video_marker_found(st, frame->timestamp, 1.0f);
			found_any = true;
		}
	}
	if (found_any)
		st->cb_video_frames_decoded.fetch_add(1, std::memory_order_relaxed);
	return found_any;
}

static void st_raw_video(void *data, struct video_data *frame)
{
	auto *st = (struct sync_test_output *)data;

	if (!st->video_pixelsize)
		return;

	if (!st->start_ts)
		st->start_ts = frame->timestamp;

	// #398: camera-box's own better-scaled top-band decode first. Once camera-box mode is active it
	// is the SOLE video-QR source (norihiro's ÷qr_step whole-frame pass misses our big top QR), so
	// skip norihiro's decode + marker-window logic — those are kept only for the phone-based method
	// when NOT in camera-box mode.
	st_raw_video_camera_box_decode(st, frame);
	bool cb_active;
	{
		std::unique_lock<std::mutex> lock(st->mutex);
		cb_active = st->cb_mode_active;
	}
	if (cb_active)
		return;

	st_raw_video_qrcode_decode(st, frame);
	st_raw_video_find_marker(st, frame);
}

static uint32_t identify_audio_index_max(struct sync_test_output *st, int index)
{
	/* Find `index_max` for video marker that have the biggest index but
	 * the index is less than or equal to the given index.
	 * In other words, find the closest but not future video marker.
	 */

	std::unique_lock<std::mutex> lock(st->mutex);
	uint32_t last_index_max = 256;
	uint32_t cand = st->last_audio_index_max;
	uint32_t cand_diff = 256;

	for (auto it = st->sync_indices.begin(); it != st->sync_indices.end(); it++) {
		if (!it->video_ts || !it->index_max)
			continue;
		uint32_t diff = (last_index_max + index - it->index) % last_index_max;
		if (diff < cand_diff) {
			cand = it->index_max;
			cand_diff = diff;
		}
		last_index_max = it->index_max;
	}

	return st->last_audio_index_max = cand;
}

static uint32_t crc4_check(uint32_t data, uint32_t size)
{
	uint32_t p = 0x13 << (size - 5);
	while (size > 4) {
		if (data & (1 << (size - 1)))
			data ^= p;
		size--;
		p >>= 1;
	}
	return data;
}

/* #398 fix (review HIGH finding): the ring only keeps the LATEST video write for a low byte, so by
 * the time a matching audio marker decodes, the stored value can be either the TRUE match (if its
 * video already arrived) or the PREVIOUS lap's write, one whole `cycle_ns` earlier — the expected
 * production regime, since the OBS program VIDEO track carries extra genlock A/V-alignment latency
 * (up to 2000 ms) the near-zero-latency QPSK AUDIO track does not. A real A/V offset is always far
 * smaller than half a cycle, so reducing the raw difference modulo `cycle_ns` into
 * `(-cycle_ns/2, +cycle_ns/2]` recovers the true offset regardless of which side leads — no
 * assumption about direction. Mirrors `resolve_ring_lap_offset_ns` (same name) in
 * src/qpsk_marker.rs — keep both in sync. */
static int64_t resolve_ring_lap_offset_ns(uint64_t audio_ts_ns, uint64_t stored_video_ts_ns, uint64_t cycle_ns)
{
	int64_t cycle = (int64_t)cycle_ns;
	int64_t half = cycle / 2;
	int64_t raw = (int64_t)audio_ts_ns - (int64_t)stored_video_ts_ns;
	int64_t r = raw % cycle;
	if (r < 0)
		r += cycle; // Euclidean modulo: always land in [0, cycle)
	if (r > half)
		r -= cycle;
	return r;
}

/* #398 fix (review MEDIUM finding): CRC-4 is only 4 bits, so on real program audio a false accept
 * is roughly 1 in 16 decode attempts; the live dock previously showed every raw pass, real or
 * false. Smooth by taking the MEDIAN of resolved offsets within `window_ns` of the latest sample
 * (dropping older ones first) — a single false blip cannot move a multi-sample median far, while
 * the real markers (sharing one near-constant pipeline delay) dominate. Mirrors
 * `smoothed_offset_ns` in src/qpsk_marker.rs — keep both in sync. */
static int64_t cb_smooth_offset_ns(std::deque<std::pair<uint64_t, int64_t>> &history, uint64_t sample_ts_ns,
                                    int64_t sample_offset_ns, uint64_t window_ns)
{
	history.push_back(std::make_pair(sample_ts_ns, sample_offset_ns));
	while (!history.empty()) {
		uint64_t front_ts = history.front().first;
		uint64_t age = (sample_ts_ns > front_ts) ? (sample_ts_ns - front_ts) : 0;
		if (age > window_ns)
			history.pop_front();
		else
			break;
	}

	std::vector<int64_t> vals;
	vals.reserve(history.size());
	for (auto &e : history)
		vals.push_back(e.second);
	std::sort(vals.begin(), vals.end());

	size_t n = vals.size();
	if (n == 0)
		return sample_offset_ns;
	if (n % 2 == 1)
		return vals[n / 2];
	// Even count: average the two middle values (matches src/qpsk_marker.rs's `median`).
	return (vals[n / 2 - 1] + vals[n / 2]) / 2;
}

static inline void st_raw_audio_decode_data(struct sync_test_output *st, std::complex<float> phase, uint64_t ts)
{
	uint32_t symbol_num = st->audio_sample_rate * st->c_last;
	uint32_t symbol_den = st->f_last;

	uint16_t index = 0;
	for (int i = 0; i < 12; i += 2) {
		auto s0 = st->audio_buffer.sum(symbol_num * i / 2 / symbol_den);
		auto s1 = st->audio_buffer.sum(symbol_num * (i / 2 + 1) / symbol_den);
		auto x = int16_to_complex(s0 - s1);
		auto real = (x / phase).real();
		auto imag = (x / phase).imag();
		if (real > 0.0f)
			index |= 1 << i;
		if (imag > 0.0f)
			index |= 2 << i;
	}

	auto crc4 = crc4_check(0xF0000 | index, 20);

	if (crc4 != 0) {
		blog(LOG_DEBUG, "st_raw_audio_decode_data: CRC mismatch: received data=0x%03X crc=0x%X", index, crc4);
		return;
	}

	const uint8_t idx8 = (uint8_t)(index >> 4);
	const uint64_t audio_ts = ts - st->start_ts;

	uint8_t stack[64];
	struct calldata cd;
	calldata_init_fixed(&cd, stack, sizeof(stack));
	auto *sh = obs_output_get_signal_handler(st->context);

	struct audio_marker_found_s data;
	data.timestamp = audio_ts;
	data.index = idx8;
	data.score = 0.0f;
	data.index_max = identify_audio_index_max(st, idx8);

	calldata_set_ptr(&cd, "data", &data);
	signal_handler_signal(sh, "audio_marker_found", &cd);

	/* #398 Option A: direct camera-box video-ring lookup, independent of the list-based
	 * `sync_index_found` below (which is GATED OFF while camera-box mode is active — the video
	 * side is gated the same way in `video_marker_found`, and #999 made the audio side symmetric:
	 * video entries queued into st->sync_indices BEFORE cb mode activated could otherwise still
	 * pair against a later audio marker DURING cb mode and emit a legacy gate_convention=false
	 * event, which post-#999 renders sign-flipped interleaved with converted ones — so THIS path
	 * is the sole authoritative sync_found source for camera-box's own marker). `idx8` is exactly
	 * the frame_id low byte the emitter encoded (`frame_id_to_index` in src/qpsk_marker.rs) — a
	 * direct hit means we know which video frame was on screen when this marker sounded, MODULO
	 * the lap-aliasing `resolve_ring_lap_offset_ns` corrects for (#398 review HIGH finding), and
	 * smoothed against CRC-4 false accepts by `cb_smooth_offset_ns` (#398 review MEDIUM
	 * finding). */
	bool cb_active, cb_valid;
	uint64_t cb_video_ts;
	{
		std::unique_lock<std::mutex> lock(st->mutex);
		cb_active = st->cb_mode_active;
		cb_valid = st->cb_video_valid[idx8];
		cb_video_ts = st->cb_video_ts_ns[idx8];
	}
	if (!cb_active)
		sync_index_found(st, idx8, audio_ts, false, data.index_max);
	// #926 fix-up (review finding 6): same ring-slot freshness gate as st_raw_audio_camera_box's
	// own lookup -- see that call site's comment for why.
	if (cb_valid) {
		uint64_t age = audio_ts > cb_video_ts ? audio_ts - cb_video_ts : cb_video_ts - audio_ts;
		if (age > CAMERA_BOX_TEST_SIGNAL_FRESH_NS)
			cb_valid = false;
	}
	if (cb_active && cb_valid) {
		int64_t raw_offset_ns = resolve_ring_lap_offset_ns(audio_ts, cb_video_ts, CAMERA_BOX_RING_CYCLE_NS);

		int64_t smoothed_ns;
		{
			std::unique_lock<std::mutex> lock(st->mutex);
			smoothed_ns = cb_smooth_offset_ns(st->cb_offset_history, audio_ts, raw_offset_ns,
			                                  CAMERA_BOX_SMOOTH_WINDOW_NS);
		}

		int64_t corrected_video_ts = (int64_t)audio_ts - smoothed_ns;
		// #1005 -- a corrected_video_ts that could not legitimately be produced (still negative
		// after smoothing, e.g. early in a session before the estimate converges) must be
		// DROPPED, never clamped to 0 and emitted anyway -- a clamped video_ts=0 manufactures a
		// garbage whole-timeline-scale offset downstream.
		if (camerabox::cb_corrected_video_ts_is_valid(corrected_video_ts)) {
			struct sync_index si;
			si.index = idx8;
			si.video_ts = (uint64_t)corrected_video_ts;
			si.audio_ts = audio_ts;
			si.index_max = 256;
			// #999 -- this event is camera-box's own direct-ring measurement (dock-native
			// convention); on_sync_found must gate-convert it, same as every other displayed
			// offset since #953.
			si.gate_convention = true;
			signal_sync_found(st->context, &si);
		}
	}
}

static inline void st_raw_audio_test_preamble(struct sync_test_output *st, uint64_t ts, float v0)
{
	uint32_t f = st->f_last;
	uint32_t c1 = st->c_last / 2;
	uint64_t symbol_ns = util_mul_div64(c1, 1000000000ULL, f);
	size_t buffer_length = (size_t)(st->audio_sample_rate * c1 * N_SYMBOL_BUFFER / f);

	/* Test the preamble pattern 0xF0  */
	auto s0 = st->audio_buffer.sum(0);
	auto s4 = st->audio_buffer.sum(buffer_length * 4 / N_SYMBOL_BUFFER);
	auto s8 = st->audio_buffer.sum(buffer_length * 8 / N_SYMBOL_BUFFER);
	auto s12 = st->audio_buffer.sum(buffer_length * 12 / N_SYMBOL_BUFFER);

	float det8_0 = std::abs(int16_to_complex(s4 - s0) - int16_to_complex(s8 - s4));
	float det12_8 = det8_0 * 0.5f - std::abs(int16_to_complex(s12 - s8));
	float det = det8_0 + det12_8;

	UNUSED_PARAMETER(v0);
	// auto dbg = int16_to_complex(st->audio_buffer.sum(1) - s0);
	// blog(LOG_INFO, "st_raw_audio-plot: %.05f %f %f %f %f", (ts - st->start_ts) * 1e-9, v0, det, dbg.real(), dbg.imag());

	if (st->audio_marker_finder.append(det, ts, symbol_ns * 12)) {
		auto s12 = st->audio_buffer.sum(buffer_length * 12 / N_SYMBOL_BUFFER);
		auto s16 = st->audio_buffer.sum(buffer_length * 16 / N_SYMBOL_BUFFER);
		auto s20 = st->audio_buffer.sum(buffer_length * 20 / N_SYMBOL_BUFFER);

		auto x = int16_to_complex(s16 - s20) - int16_to_complex(s12 - s16);
		x *= std::complex(1.0f, -1.0f);

		ts = st->audio_marker_finder.last_ts - symbol_ns * N_AUDIO_SYMBOLS / 2;

		st_raw_audio_decode_data(st, x / std::abs(x), ts);
	}
}

/* #926: read CAMERA_BOX_LOCK_SOURCE_NAME's CURRENT genlock_latency_ms_src -- always read fresh
 * (never cached) so a concurrent manual/scripted change (an operator, or av_sync_calibrate.py) is
 * respected rather than clobbered. Returns false if the source does not exist right now (e.g. the
 * scene collection hasn't loaded it yet) -- the caller must not apply a correction without a real
 * current value to correct FROM. */
static bool cb_read_lock_latency_ms(int32_t *out_ms)
{
	obs_source_t *src = obs_get_source_by_name(CAMERA_BOX_LOCK_SOURCE_NAME);
	if (!src)
		return false;
	obs_data_t *settings = obs_source_get_settings(src);
	*out_ms = (int32_t)obs_data_get_int(settings, "genlock_latency_ms_src");
	obs_data_release(settings);
	obs_source_release(src);
	return true;
}

/* #926 fix-up (review finding 4, covers 8/12): apply a NEW absolute genlock_latency_ms_src to
 * CAMERA_BOX_LOCK_SOURCE_NAME, mirroring the SAME settings-update mechanism
 * `scripts/av_sync_calibrate.py`'s `apply_latency()` performs over the OBS WebSocket
 * (GetInputSettings/SetInputSettings), done here in-process instead -- but marshaled onto the OBS
 * UI thread via `obs_queue_task`, never mutated directly from the raw-audio callback (the core
 * AUDIO thread): mutating a LIVE source's settings `obs_data` (no internal mutex of its own) from
 * a real-time audio callback races the video thread's own reads/writes of the same source and any
 * UI/WebSocket access, and `obs_get_source_by_name` itself takes the global sources-list mutex,
 * which the UI thread is the expected/serialized caller of throughout the rest of this codebase.
 * The queued task uses a FRESH `obs_data_create()` holding only the ONE key -- never the shared
 * `obs_source_get_settings()` object handed back in place (finding 12's fragile "mutate the live
 * settings object" idiom) -- and reads the value back right after the update to catch a mismatch
 * (finding 8) close to the write. Fire-and-forget (`wait=false`): the audio thread must never
 * block on the UI thread's own scheduling. */
struct CbApplyLockLatencyTask {
	int32_t new_delay_ms;
};

static void cb_apply_lock_latency_task(void *param)
{
	auto *task = (CbApplyLockLatencyTask *)param;
	const int32_t new_ms = task->new_delay_ms;
	delete task;

	obs_source_t *src = obs_get_source_by_name(CAMERA_BOX_LOCK_SOURCE_NAME);
	if (!src) {
		blog(LOG_WARNING, "av-sync-dock: LOCK-CORRECT apply skipped (UI thread) -- source '%s' not found",
		     CAMERA_BOX_LOCK_SOURCE_NAME);
		return;
	}

	obs_data_t *settings = obs_data_create();
	obs_data_set_int(settings, "genlock_latency_ms_src", (long long)new_ms);
	obs_source_update(src, settings);
	obs_data_release(settings);

	obs_data_t *readback = obs_source_get_settings(src);
	const int32_t got = (int32_t)obs_data_get_int(readback, "genlock_latency_ms_src");
	obs_data_release(readback);
	obs_source_release(src);

	if (got != new_ms) {
		blog(LOG_WARNING,
		     "av-sync-dock: LOCK-CORRECT read-back mismatch on '%s' -- wrote %d, read back %d",
		     CAMERA_BOX_LOCK_SOURCE_NAME, (int)new_ms, (int)got);
	}
}

static void cb_apply_lock_latency_ms(int32_t new_ms)
{
	auto *task = new CbApplyLockLatencyTask{new_ms};
	obs_queue_task(OBS_TASK_UI, cb_apply_lock_latency_task, task, false);
}

/* #398 fix (Audio Index + Latency never locked): camera-box's OWN audio decode path, used once the
 * video QR has put us in camera-box mode. norihiro's `st_raw_audio*` demod cannot decode our marker
 * at c=1 (its `c1 = c/2` = 0 collapses the preamble finder; its 6-symbol read can't recover the
 * 8-bit index) — so this drives the streaming `decode_markers` mirror (round-trip tested for all 256
 * indices at c=1) and the rolling densest-cluster estimator (robust to the CRC-4 false-decode flood
 * that a plain median cannot survive) from `camera-box-audio.hpp`. Every decoded marker's index is
 * the frame_id low byte; the ring lookup + `resolve_ring_lap_offset_ns` give its A/V offset, the
 * cluster locks the trustworthy value, and only THEN is `sync_found` (Latency) / `audio_marker_found`
 * (Audio Index) emitted — so the dock shows a number only when it is real, never a false blip. */
static void st_raw_audio_camera_box(struct sync_test_output *st, struct audio_data *frames)
{
	if (!st->cb_audio_dec) {
		size_t sig = camerabox::cb_signal_len(st->audio_sample_rate, CAMERA_BOX_AUDIO_F_HZ,
		                                      CAMERA_BOX_AUDIO_C);
		if (sig == 0)
			return;
		// window ≥ 3 marker lengths so any marker is wholly present; dedup gap one marker length.
		st->cb_audio_dec = new camerabox::StreamingMarkerDecoder(
			st->audio_sample_rate, CAMERA_BOX_AUDIO_F_HZ, CAMERA_BOX_AUDIO_C,
			camerabox::CB_QPSK_THRESHOLD, sig * 3, (uint64_t)sig);
	}

	// Mix all channels to mono (the marker survives the mix; the QPSK decode is amplitude-tolerant),
	// matching the offline `recording-verdict --av-sync` `-ac 1` extraction.
	size_t nf = frames->frames;
	std::vector<float> mono(nf, 0.0f);
	size_t ch = st->audio_channels;
	for (size_t i = 0; i < nf; i++) {
		float acc = 0.0f;
		for (size_t cix = 0; cix < ch; cix++) {
			/* #1153: skip non-finite samples per channel -- a poisoned upstream channel must
			 * never wipe a marker riding another channel through the mono mixdown (and the
			 * decode kernel's prefix sums must never see NaN/Inf at all). */
			float s = ((float *)frames->data[cix])[i];
			if (std::isfinite(s))
				acc += s;
		}
		mono[i] = ch ? acc / (float)ch : 0.0f;
	}

	const uint64_t base = st->cb_audio_pushed; // absolute index of this callback's first sample
	std::vector<std::pair<uint64_t, uint8_t>> markers = st->cb_audio_dec->push(mono.data(), nf);
	st->cb_audio_pushed += (uint64_t)nf;

	const double sr = (double)st->audio_sample_rate;
	for (size_t k = 0; k < markers.size(); k++) {
		const uint64_t abs = markers[k].first;
		const uint8_t idx8 = markers[k].second;
		// OBS timestamp of the marker: this callback's first sample is at `frames->timestamp`, so a
		// marker at absolute index `abs` is (abs - base) samples from it (may be negative — a marker
		// that entered on a prior callback and is still in the window).
		const int64_t rel = (int64_t)abs - (int64_t)base;
		const int64_t marker_ts_i =
			(int64_t)frames->timestamp + (int64_t)std::llround((double)rel * 1000000000.0 / sr);
		if (marker_ts_i < (int64_t)st->start_ts)
			continue;
		const uint64_t audio_ts = (uint64_t)marker_ts_i - st->start_ts;

		bool valid;
		uint64_t video_ts;
		uint64_t video_last_decode_ts;
		{
			std::unique_lock<std::mutex> lock(st->mutex);
			valid = st->cb_video_valid[idx8];
			video_ts = st->cb_video_ts_ns[idx8];
			video_last_decode_ts = st->cb_video_last_decode_ts_ns;
		}
		/* #926 fix-up (review finding 6): age out a stale ring slot -- `cb_video_valid[]`
		 * otherwise latches true FOREVER once any frame with this idx8 ever decoded, so a video
		 * path that has genuinely stopped (real event: camera unplugged, QR obscured) would keep
		 * pairing fresh audio markers against a video timestamp from potentially hours ago. A slot
		 * written within CAMERA_BOX_TEST_SIGNAL_FRESH_NS is trusted; anything older is treated
		 * exactly like "no video ring hit yet". */
		if (valid) {
			uint64_t age = audio_ts > video_ts ? audio_ts - video_ts : video_ts - audio_ts;
			if (age > CAMERA_BOX_TEST_SIGNAL_FRESH_NS)
				valid = false;
		}
		if (!valid) {
			st->cb_ring_misses++;
			continue;
		}
		st->cb_ring_hits++;

		const int64_t offset_ns =
			resolve_ring_lap_offset_ns(audio_ts, video_ts, CAMERA_BOX_RING_CYCLE_NS);
		const double offset_ms = (double)offset_ns / 1000000.0;
		camerabox::CbAvOffset est = st->cb_offset_cluster.push(audio_ts, offset_ms);

		/* #634: audit-log the lock/unlock/offset-update transition (if any) BEFORE the est.ok
		 * gate below, so an unlock (est.ok going false) is also logged, not silently swallowed
		 * by the `continue`. CbLockAuditTracker is pure/tested; this is only the blog() glue --
		 * PURELY for the UI/log lock-state status now (#926 fix-up finding 2 moved the actuator
		 * off this classifier entirely, see below). Deliberately NOT logging `idx8` here (review
		 * finding): this loop's CbAvOffset comes from EVERY CRC-4-accepted marker candidate,
		 * including the ~1/16 false-decode rate this file documents below -- a false marker can
		 * still recompute an already-locked cluster, so idx8 at this point is not reliably "the
		 * frame this lock belongs to". The offset/matched/mad_ms are the real "source of the
		 * value" (the densest cluster), and those are unaffected by which single candidate
		 * triggered the recompute. */
		camerabox::CbLockAuditEvent audit_ev = st->cb_lock_audit.push(est);
		switch (audit_ev.kind) {
		case camerabox::CbLockEventKind::Locked:
		case camerabox::CbLockEventKind::Updated:
			st->cb_lock_state = true;
			/* #953 -- logged in GATE convention (offset_ms = video_time - audio_time), via
			 * cb_dock_lock_display_offset_ms(), so this number agrees in SIGN with the E2E gate's
			 * own av_offset_ms (#952 established the two disagreed: dock ~= -gate - 55). The
			 * residual ~55ms bias is not compensated here -- see that function's own doc comment. */
			blog(LOG_INFO, "av-sync-dock: %s offset=%.1fms source=cluster matched=%zu mad=%.1fms",
			     audit_ev.kind == camerabox::CbLockEventKind::Locked ? "LOCKED" : "UPDATED",
			     camerabox::cb_dock_lock_display_offset_ms(audit_ev.offset_ms), audit_ev.matched,
			     audit_ev.mad_ms);
			/* #926: fire the coarse locked/unlocked status signal only on the ACTUAL boundary
			 * crossing (Locked), not on every Updated -- Updated means "still locked, offset
			 * moved", never a state change the dock's plain-language status text needs to know
			 * about again. */
			if (audit_ev.kind == camerabox::CbLockEventKind::Locked)
				signal_lock_state_changed(st->context, true);
			break;
		case camerabox::CbLockEventKind::Unlocked:
			st->cb_lock_state = false;
			// #953 -- same gate-convention sign fix as the LOCKED/UPDATED line above.
			blog(LOG_WARNING, "av-sync-dock: UNLOCKED last_offset=%.1fms source=cluster",
			     camerabox::cb_dock_lock_display_offset_ms(audit_ev.offset_ms));
			signal_lock_state_changed(st->context, false);
			break;
		case camerabox::CbLockEventKind::None:
		default:
			break;
		}

		/* #926 fix-up (review finding 2): drive the corrector from EVERY trusted measurement
		 * (est.ok), never from the audit classifier above -- CbLockAuditTracker's Updated only
		 * fires on a >5ms MOVE of the (window-smoothed) median, which stalls convergence once the
		 * window itself lags a landed correction. decide()'s own cooldown + dead-zone gate is what
		 * makes calling it on every trusted push safe.
		 *
		 * #926 fix-up (review finding 6): additionally require the OVERALL test signal to be
		 * FRESH (a video QR decode within CAMERA_BOX_TEST_SIGNAL_FRESH_NS) before actuating --
		 * never gating the measurement/telemetry below (est.ok / signal_sync_found are untouched).
		 * Without this, a stray CRC-4 false decode landing on some idx8 ring slot could eventually
		 * build a spurious cluster (over a long enough deployment) and actuate the program
		 * source's latency during a REAL live show that has genuinely lost its test signal, since
		 * cb_mode_active never resets on its own. */
		if (est.ok) {
			const uint64_t age_signal =
				audio_ts > video_last_decode_ts ? audio_ts - video_last_decode_ts : 0;
			const bool signal_fresh = age_signal <= CAMERA_BOX_TEST_SIGNAL_FRESH_NS;
			if (signal_fresh) {
				int32_t current_ms = 0;
				if (cb_read_lock_latency_ms(&current_ms)) {
					st->cb_lock_source_missing_logged = false;
					camerabox::CbDockLockAction act = st->cb_lock_corrector.decide(
						true, est.offset_ms, est.mad_ms, current_ms, audio_ts);
					/* #955 -- the Write/Suggest/RailWarn/Quiet branch selection below is a pure,
					 * behaviorally-tested function (cb_dock_lock_outcome(),
					 * tests/av_sync_dock_outcome_955.rs) instead of an inline if/else-if-else
					 * chain that only a source-text grep could ever regression-guard. decide()'s
					 * own band/step/cooldown logic is UNCHANGED (`.claude/rules/dock-lock-hold-
					 * band.md`) -- this only names the decision the caller already makes. The
					 * gate is now the only CONTINUOUS/closed-loop writer of genlock_latency_ms_src
					 * (a bounded, snapshot-and-restore exception exists around a single
					 * delivery-verify test run -- scripts/obs_phase2.py::
					 * _snapshot_and_set_test_latency, #358/#691 -- which is not a second
					 * closed-loop actuator); two independent CONTINUOUS actuators on one plant
					 * never converge (root-cause evidence on the #942 ticket). */
					camerabox::CbDockLockOutcome outcome = camerabox::cb_dock_lock_outcome(
						act, camerabox::cb_dock_lock_may_actuate(), est.offset_ms, current_ms);
					switch (outcome) {
					case camerabox::CbDockLockOutcome::Write: {
						const double delta_ms = (double)(act.new_delay_ms - current_ms);
						cb_apply_lock_latency_ms(act.new_delay_ms);
						/* #926 fix-up (review finding 1/7): shift every retained cluster
						 * sample by the applied delta so the window reflects the
						 * POST-correction state immediately -- see
						 * RollingOffsetCluster::rebase()'s own doc comment for the full
						 * closed-form justification (mirrors src/av_sync_dock.rs). */
						st->cb_offset_cluster.rebase(delta_ms);
						st->cb_rail_pinned_logged = false;
						// #953 -- same gate-convention sign fix as every other displayed offset
						// in this function (currently unreachable while actuation is hard-locked
						// off, #942, but kept consistent for if/when it is ever re-enabled).
						blog(LOG_INFO,
						     "av-sync-dock: LOCK-CORRECT requested genlock_latency_ms_src %d "
						     "-> %dms (measured offset=%.1fms)",
						     (int)current_ms, (int)act.new_delay_ms,
						     camerabox::cb_dock_lock_display_offset_ms(est.offset_ms));
						break;
					}
					case camerabox::CbDockLockOutcome::Suggest: {
						/* #942 -- monitor-only: decide() computed a real correction, but the
						 * gate is the only continuous writer, so this is DISPLAYED as a
						 * suggestion and never applied -- no cb_apply_lock_latency_ms(), no
						 * rebase() (rebase assumes a real actuator move happened, which this is
						 * not). Reaching here means we are not currently stuck at a rail
						 * (cb_dock_lock_outcome() only returns Suggest when decide() returned
						 * Apply, which never happens exactly at a value already pinned to
						 * itself); reset the dedup flag the same way the write branch above does,
						 * so a LATER genuine rail-pinned state still gets its own fresh warning.
						 *
						 * #953 -- the DISPLAYED value is no longer decide()'s own actuator-era
						 * act.new_delay_ms (step-capped to CB_DOCK_LOCK_MAX_STEP_MS=5ms, which
						 * live evidence showed printed a constant "-5ms" regardless of the true
						 * measured offset). Instead: convert to gate convention (sign fix, #952)
						 * and compute the FULL, uncapped alignment target -- and print NOTHING
						 * when that target says the offset is already within the noise floor
						 * (quiet inside the noise band, never "-5ms forever"). */
						st->cb_rail_pinned_logged = false;
						double gate_offset_ms =
							camerabox::cb_dock_lock_display_offset_ms(est.offset_ms);
						camerabox::CbDockLockSuggestion suggestion =
							camerabox::cb_dock_lock_suggested_target(gate_offset_ms, est.mad_ms,
												  current_ms);
						if (suggestion.has_value)
							blog(LOG_INFO,
							     "av-sync-dock: LOCK-CORRECT SUGGESTED genlock_latency_ms_src %d "
							     "-> %dms (measured offset=%.1fms) [monitor-only -- the E2E gate "
							     "is the only continuous writer]",
							     (int)current_ms, (int)suggestion.target_ms, gate_offset_ms);
						break;
					}
					case camerabox::CbDockLockOutcome::RailWarn:
						/* #926 fix-up (review finding 9): pinned at a hardware rail with
						 * the invariant still violated -- a genuine hardware limit, not a
						 * corrector bug, but it must be VISIBLE rather than silently
						 * persisting. */
						if (!st->cb_rail_pinned_logged) {
							st->cb_rail_pinned_logged = true;
							blog(LOG_WARNING,
							     "av-sync-dock: LOCK-CORRECT pinned at the hardware %s "
							     "(%dms) with audio still EARLY by %.1fms -- cannot "
							     "correct further",
							     current_ms <= camerabox::CB_DOCK_LOCK_LATENCY_MIN_MS
								     ? "floor"
								     : "ceiling",
							     (int)current_ms, -est.offset_ms);
						}
						break;
					case camerabox::CbDockLockOutcome::Quiet:
						st->cb_rail_pinned_logged = false;
						break;
					}
				} else if (!st->cb_lock_source_missing_logged) {
					/* #926 fix-up (review finding 16): log the missing lock source ONCE
					 * (not per trusted marker) -- STRIH runs the same DLL but has neither
					 * CAMERA_BOX_LOCK_SOURCE_NAME nor CAMERA_BOX_ASRC_SOURCE_NAME. */
					st->cb_lock_source_missing_logged = true;
					blog(LOG_WARNING,
					     "av-sync-dock: LOCK-CORRECT unavailable -- source '%s' not found on "
					     "this box (further occurrences suppressed until it appears)",
					     CAMERA_BOX_LOCK_SOURCE_NAME);
				}
			}
		} else {
			// No trusted measurement right now -- FREEZE, never chase drift on program material
			// (requirement 5). decide(locked=false, ...) is an explicit no-op by construction.
			(void)st->cb_lock_corrector.decide(false, 0.0, 0.0, 0, audio_ts);
		}

		if (!est.ok)
			continue; // still measuring — never display an untrustworthy number

		// Latency (sync_found): the locked cluster offset, as audio_ts - video_ts (dock convention).
		const int64_t locked_ns = (int64_t)std::llround(est.offset_ms * 1000000.0);
		const int64_t corrected_video_ts = (int64_t)audio_ts - locked_ns;
		// #1005 -- same DROP-not-clamp fix as the "still measuring" sync_found above: never
		// manufacture a garbage whole-timeline-scale offset from a video_ts that could not
		// legitimately be produced.
		if (camerabox::cb_corrected_video_ts_is_valid(corrected_video_ts)) {
			struct sync_index si;
			si.index = idx8;
			si.video_ts = (uint64_t)corrected_video_ts;
			si.audio_ts = audio_ts;
			si.index_max = 256;
			// #999 -- same as the "still measuring" sync_found above: this is camera-box's own
			// direct-ring measurement, gate-convert it for on_sync_found.
			si.gate_convention = true;
			signal_sync_found(st->context, &si);
		}

		// Audio Index (audio_marker_found): only for a marker whose own offset agrees with the lock
		// — a believed-REAL marker — so the displayed index is a genuine one, not a false blip.
		if (std::fabs(offset_ms - est.offset_ms) <= camerabox::CB_CLUSTER_TOL_MS) {
			uint8_t stack[64];
			struct calldata cd;
			calldata_init_fixed(&cd, stack, sizeof(stack));
			auto *sh = obs_output_get_signal_handler(st->context);
			struct audio_marker_found_s data;
			data.timestamp = audio_ts;
			data.index = idx8;
			data.score = 0.0f;
			data.index_max = 256;
			data.sparse_index = true; // frame_id low byte, sampled sparsely — no +1 missed% math
			calldata_set_ptr(&cd, "data", &data);
			signal_handler_signal(sh, "audio_marker_found", &cd);
		}
	}

	/* #690: rate-limited (~10s) INFO diagnostic -- answers, from the OBS log alone, whether the
	 * demod sees nothing (preambles=0), sees candidates but they're garbage (preambles>0, crc_ok=0),
	 * decodes fine but never ring-hits (crc_ok>0, ring_hit=0 — video QR isn't decoding the same
	 * frame ids), or ring-hits but never clusters tight enough to lock (ring_hit>0, locked=no). Also
	 * carries the video-QR pair rate so a low decode% doesn't need a separate investigation to see.
	 * Low-noise by construction: one line per ~10s of live audio, never per-callback. */
	if (st->cb_diag_last_log_ns == 0 ||
	    frames->timestamp - st->cb_diag_last_log_ns >= CAMERA_BOX_DIAG_LOG_INTERVAL_NS) {
		st->cb_diag_last_log_ns = frames->timestamp;
		const uint64_t vseen = st->cb_video_frames_seen.load(std::memory_order_relaxed);
		const uint64_t vdec = st->cb_video_frames_decoded.load(std::memory_order_relaxed);
		const double vpct = vseen > 0 ? 100.0 * (double)vdec / (double)vseen : 0.0;

		/* #1177: evaluate measurement-input staleness at this SAME ~10s cadence -- the audio thread
		 * keeps ticking in EVENT mode even though the marker/QR decode counters (video_decoded +
		 * crc_ok) do not. On the boundary crossing, fire a one-shot log line + the sync_stale_changed
		 * signal so the dock stops presenting the last locked offset as if it were live. */
		const camerabox::CbDockStaleTransition strans = st->cb_input_staleness.observe(
			vdec, st->cb_audio_dec->stats.crc_ok, frames->timestamp,
			camerabox::CB_DOCK_INPUT_STALE_NS);
		const bool input_stale = st->cb_input_staleness.is_stale();
		if (strans == camerabox::CbDockStaleTransition::EnteredStale) {
			blog(LOG_WARNING,
			     "av-sync-dock: measurement input LOST -> STALE (no marker/QR decode advance for "
			     ">=%llus -- EVENT mode? cam2 QPSK/QR off) -- display frozen on last offset, no longer live",
			     (unsigned long long)(camerabox::CB_DOCK_INPUT_STALE_NS / 1000000000ull));
			signal_stale_changed(st->context, true);
		} else if (strans == camerabox::CbDockStaleTransition::RecoveredLive) {
			blog(LOG_INFO,
			     "av-sync-dock: measurement input RESTORED -> LIVE (marker/QR decode resumed)");
			signal_stale_changed(st->context, false);
		}

		blog(LOG_INFO,
		     "av-sync-dock: diag video_frames=%llu video_decoded=%llu(%.1f%%) "
		     "audio_samples=%llu preambles=%llu crc_ok=%llu crc_fail=%llu "
		     "ring_hit=%llu ring_miss=%llu locked=%s state=%s",
		     (unsigned long long)vseen, (unsigned long long)vdec, vpct,
		     (unsigned long long)st->cb_audio_pushed,
		     (unsigned long long)st->cb_audio_dec->stats.preamble_screens_passed,
		     (unsigned long long)st->cb_audio_dec->stats.crc_ok,
		     (unsigned long long)st->cb_audio_dec->stats.crc_fail,
		     (unsigned long long)st->cb_ring_hits, (unsigned long long)st->cb_ring_misses,
		     st->cb_lock_state ? "yes" : "no", input_stale ? "STALE" : "LIVE");

		/* #1153: dead-pairing recovery, evaluated at the SAME ~10s cadence. When the pairing has
		 * been dead for a full epoch (no meaningful ring-hit advance, no genuine lock) while
		 * video QRs and audio candidates BOTH keep flowing, reset every piece of in-dock pairing
		 * state and re-acquire from scratch -- the in-process poison a large video-latency step
		 * leaves behind must never make a manual OBS restart the only cure. The epoch deltas in
		 * the evidence line discriminate the poison class from the log alone: crc_ok near the
		 * ~1/256 chance floor of the preamble delta = the marker waveform is degraded upstream
		 * of the dock; a healthy crc_ok rate with a dead ring = in-dock pairing state (which
		 * this reset clears). Input-dead states (EVENT mode) never fire -- they are the
		 * staleness detector's domain above. Cumulative counters/stats are deliberately NOT
		 * reset, so the diag line stays monotonic across recoveries. */
		const camerabox::CbDockPairingRecovery rec = st->cb_pairing_watchdog.observe(
			vdec, st->cb_audio_dec->stats.preamble_screens_passed,
			st->cb_audio_dec->stats.crc_ok, st->cb_ring_hits, st->cb_lock_state,
			frames->timestamp, camerabox::CB_DOCK_PAIRING_DEAD_NS,
			camerabox::CB_DOCK_PAIRING_MIN_RING_HITS);
		if (rec.fire) {
			{
				std::unique_lock<std::mutex> lock(st->mutex);
				for (size_t slot = 0; slot < CAMERA_BOX_RING_SLOTS; slot++)
					st->cb_video_valid[slot] = false;
			}
			st->cb_offset_cluster = camerabox::RollingOffsetCluster::dock();
			st->cb_offset_history.clear();
			st->cb_lock_audit = camerabox::CbLockAuditTracker();
			st->cb_audio_dec->reset_window();
			if (st->cb_lock_state) {
				// A stale-held lock (zero ring hits all epoch) must not survive the reset on
				// the UI either -- the audit tracker could never fire its own Unlocked
				// transition without a decode to push.
				st->cb_lock_state = false;
				signal_lock_state_changed(st->context, false);
			}
			blog(LOG_WARNING,
			     "av-sync-dock: PAIRING-RECOVER dead pairing window (ring_hit +%llu, crc_ok "
			     "+%llu, preambles +%llu, video_decoded +%llu in %llus) -- reset "
			     "ring+cluster+decoder window, re-acquiring from scratch",
			     (unsigned long long)rec.ring_hit_delta, (unsigned long long)rec.crc_ok_delta,
			     (unsigned long long)rec.preambles_delta,
			     (unsigned long long)rec.video_decoded_delta,
			     (unsigned long long)(rec.window_ns / 1000000000ull));
		}
	}
}

static void st_raw_audio(void *data, struct audio_data *frames)
{
	auto *st = (struct sync_test_output *)data;

	if (!st->start_ts)
		return;

	// #398: once the video QR has activated camera-box mode, decode the audio with camera-box's own
	// proven demod (norihiro's is broken at c=1). Skip norihiro's audio path entirely then.
	bool cb_active;
	{
		std::unique_lock<std::mutex> lock(st->mutex);
		cb_active = st->cb_mode_active;
	}
	if (cb_active) {
		st_raw_audio_camera_box(st, frames);
		return;
	}

	std::unique_lock<std::mutex> lock(st->mutex);
	uint32_t f = st->f;
	uint32_t c = st->c;
	uint32_t q_ms = st->q_ms;
	lock.unlock();

	if (f <= 0 || c <= 0)
		return;

	if (f != st->f_last || c != st->c_last) {
		st->f_last = f;
		st->c_last = c;
		st->audio_buffer.buffer.clear();
	}

	if (q_ms > 0)
		st->audio_marker_finder.dumping_range = q_ms * 1000000 * 6 * 2;

	float phase = (frames->timestamp % 1000000000) * (float)(1e-9 * 2 * M_PI * f);
	float phase_step = (float)(2 * M_PI * f) / st->audio_sample_rate;
	size_t buffer_length = (size_t)(st->audio_sample_rate * c * N_SYMBOL_BUFFER / f);

	for (uint32_t i = 0; i < frames->frames; i++) {
		float osc0 = sinf(phase + phase_step * i);
		float osc1 = cosf(phase + phase_step * i);
		uint64_t ts = frames->timestamp + util_mul_div64(i, 1000000000ULL, st->audio_sample_rate);

		float v0 = ((float *)frames->data[0])[i];
		float v1 = st->audio_channels >= 2 ? ((float *)frames->data[1])[i] : 0.0f;
		int16_t vr = (int16_t)((v0 * osc0 - v1 * osc1) * 16383.0f);
		int16_t vi = (int16_t)((v0 * osc1 + v1 * osc0) * 16383.0f);
		st->audio_buffer.push_back(vr, vi, buffer_length);

		if (st->audio_buffer.buffer.size() < buffer_length)
			continue;

		st_raw_audio_test_preamble(st, ts, v0);
	}
}

extern "C" void register_sync_test_output()
{
	struct obs_output_info info = {};
	info.id = OUTPUT_ID;
	info.flags = OBS_OUTPUT_AV;
	info.get_name = st_get_name;
	info.create = st_create;
	info.destroy = st_destroy;
	info.start = st_start;
	info.stop = st_stop;
	info.raw_video = st_raw_video;
	info.raw_audio = st_raw_audio;

	obs_register_output(&info);
}
