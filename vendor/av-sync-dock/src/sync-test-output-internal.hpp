#pragma once

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

/* camera-box issue 1386: the output's shared declarations. sync-test-output.cpp had grown past 2000
 * lines, so the output compiles as three translation units, each including this header:
 * - sync-test-output.cpp: the obs_output_info callbacks, the output lifecycle, the registration;
 * - sync-test-output-video.cpp: the video path -- the QR decode worker (issue 1367), the camera-box
 *   top-band decode, norihiro's whole-frame decode and marker search, and norihiro's sync_index list;
 * - sync-test-output-audio.cpp: the audio path -- norihiro's demod and the camera-box decode (the
 *   audio-thread gate + FIFO copy and the audio decode worker, issue 1381).
 * All of it lives in namespace av_sync_output. A function one TU calls in another is declared at the
 * bottom of this header with UNNAMED parameters, so a text anchor keyed on its named signature can
 * only ever find the definition; every other function stays static in its own TU. The text anchors
 * (the Rust tests and the pwsh steps of both windows-genlock workflows) read these files as one
 * source: tests/support/av_sync_dock_output.rs and vendor/av-sync-dock/test/dock-output-source.ps1. */

#include <obs-module.h>
#include <util/threading.h>
#include <util/platform.h>
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
#include <cstring>
#include "quirc.h"
#include "sync-test-output.hpp"
#include "peak-finder.hpp"
#include "camera-box-qr.hpp"
#include "camera-box-audio.hpp"
#include "camera-box-channel-pick.hpp"
#include "camera-box-video.hpp"
#include "camera-box-frame-copy.hpp"
#include "camera-box-decode-mailbox.hpp"
#include "camera-box-audio-worker.hpp"

#define N_CORNERS 4

#define N_AUDIO_SYMBOLS 16
#define N_SYMBOL_BUFFER 20

/* #398 fix: the live camera-box video<->audio ring (`cb_video_ts_ns` below) is keyed on
 * `frame_id_to_index` (the frame_id's low byte, see src/qpsk_marker.rs), so its natural cycle
 * length is 256 frames at the FIXED camera-box painter rate (60 fps) -- independent of whatever
 * fps the dock itself happens to capture at. Used by `resolve_ring_lap_offset_ns`
 * (sync-test-output-audio.cpp) to disambiguate which lap of the ring a stored slot value
 * belongs to. Mirrors `AV_SYNC_RING_CYCLE_NS` in src/qpsk_marker.rs -- keep both in sync. */
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

/* issue 1381: the camera-box audio decode runs only while the test signal is fresh -- the same window
 * as above since the last camera-box QR decode (camera-box-audio-worker.hpp cb_audio_decode_gate) --
 * and only on a box that has the measurement source it exists for (CAMERA_BOX_MEASURE_SOURCE_NAME in
 * camera-box-audio.hpp, shared with sync-test-dock.cpp). resolume and strih never enter it.
 * The source check takes the sources mutex, so it runs on the video decode worker, at most once per
 * CAMERA_BOX_MEASURE_SOURCE_RECHECK_NS of frame time, never on the audio thread. */
#define CAMERA_BOX_MEASURE_SOURCE_RECHECK_NS 5000000000ULL

/* There are several reason to limit the width and the height.
 * - Since a square of 3/8 QR-code-length is calculated using uint32_t,
 *   the 3/8 of width or height cannot exceed the square root of uint32_t max.
 * - Since a sum of the pixels in a line is accumurated on uint32_t,
 *   the width must be less than 1/255 of uint32_t max.
 *   */
#define MAX_WIDTH_HEIGHT 87378u

namespace av_sync_output {

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

struct corner_type
{
	uint32_t x, y;
	uint32_t r = 0;
};

/* issue 1367: one decode job = the bounded copy st_raw_video makes of what the decoders read. It is
 * taken on libobs's single video-output thread and decoded on the dock's own worker thread
 * (camera-box-decode-mailbox.hpp), so a slow QR decode can never make video-io skip output frames.
 * The buffers live in the mailbox's two slots and are reused, so there is no per-frame allocation
 * once they have grown. Since issue 1381 st_start sizes and writes `band` and `grid` in both slots
 * (prepare_slots + st_video_decode_job_prepare), so the video thread's first frame neither allocates
 * nor first-touches a page; only the phone-mode marker `patches` still grow on use. */
struct st_marker_patch
{
	camerabox::CbPatchRect rect; // the circle's bounding box in the full-res frame
	std::vector<uint8_t> luma;   // rect.w x rect.h intensity samples, origin (rect.x0, rect.y0)
};

struct st_video_decode_job
{
	uint64_t timestamp = 0;    // the frame's own timestamp, so the marker/QR timing is unchanged
	std::vector<uint8_t> band; // camera-box top band, video_width x plan.band_h luma
	/* Outside camera-box mode: norihiro's whole-frame grid and the marker window are copied too. */
	bool norihiro = false;
	std::vector<uint8_t> grid;                  // qr_grid_w x qr_grid_h, what quirc_begin() takes
	struct corner_type corners[N_CORNERS] = {}; // the corners the marker patches were cut around
	struct st_marker_patch patches[N_CORNERS];
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
	/* issue 1367: written once by the video-output thread (first frame), read by the decode
	 * worker and the audio decode worker -- atomic, no lock needed. */
	std::atomic<uint64_t> start_ts{0};

	struct quirc *qr = nullptr;
	uint32_t qr_step;
	/* issue 1367: the size quirc_resize() gave `qr` in st_start -- the video thread samples
	 * norihiro's grid at exactly this size, the worker hands it to quirc_begin(). */
	uint32_t qr_grid_w = 0, qr_grid_h = 0;
	/* `qr_corners`, `qr_data`, `video_level_prev*`, `video_marker_max_ts`, `qr`, `cb_qr` and
	 * `cb_qr_resize_cache` are owned by the decode worker thread (issue 1367). */
	struct corner_type qr_corners[N_CORNERS] = {};
	/* issue 1367: the worker's latest norihiro corners, published under `mutex` so the video
	 * thread can cut the marker window around them for the next frame. */
	struct corner_type marker_corners[N_CORNERS] = {};
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
	 * CAMERA_BOX_TEST_SIGNAL_FRESH_NS). Written on the decode worker thread in `cb_video_qr_record`, read
	 * on the audio decode worker (issue 1381) under the same mutex as the other cb_video_* fields. */
	uint64_t cb_video_last_decode_ts_ns = 0;

	/* issue 1404: the per-run rate limit of the "ignoring reserved origin run N" line (at most one line
	 * per run per CAMERA_BOX_IGNORED_ORIGIN_LOG_NS of frame time, however the QRs alternate). Decode
	 * worker only. */
	CameraBoxIgnoredOriginLog cb_ignored_origin_log;

	/* #398 fix: rolling history of recently-resolved (audio_ts, offset_ns) samples for
	 * `cb_smooth_offset_ns` — median-smooths the displayed offset so a single false CRC-4 accept
	 * (~1/16 likely on real program audio) can't show garbage (review MEDIUM finding). Touched
	 * only from the audio decode worker; no cross-thread sharing, but guarded by the same mutex
	 * as the other cb_* fields for consistency. */
	std::deque<std::pair<uint64_t, int64_t>> cb_offset_history;

	/* #398 fix (audio index never locked): norihiro's own audio demod is broken at the rig's c=1
	 * (its `c1 = c/2` half-symbol resolution is 0, collapsing the preamble finder; and it decodes
	 * only 6 symbols). These drive camera-box's OWN proven demod instead — the streaming
	 * `decode_markers` (round-trip tested for all 256 indices at c=1) + the robust rolling
	 * densest-cluster estimator (survives the CRC-4 false-decode flood the offline path also fights,
	 * where a plain 1 s median would not). Mirrors `src/av_sync_dock.rs`; touched only on the audio
	 * thread. `cb_qr` is a SECOND quirc context sized to the better-scaled top-band decode (below).
	 * The top band itself is gathered into the decode job's reused buffer (issue 1367).
	 * `cb_audio_dec` decodes every audio channel with its own decoder and keeps one channel
	 * (camera-box-channel-pick.hpp, issue 1367), never their average. */
	struct quirc *cb_qr = nullptr;
	camerabox::ChannelMarkerPicker *cb_audio_dec = nullptr;
	/* issue 1367: switches of the paired audio channel (a switch moves the measured offset by
	 * ~10 ms) -- counted, and when to log them (camera-box-channel-pick.hpp). Audio decode worker only. */
	camerabox::CbChannelSwitchLog cb_switch_log;
	camerabox::RollingOffsetCluster cb_offset_cluster = camerabox::RollingOffsetCluster::dock();
	uint64_t cb_audio_pushed = 0;

	/* #921: skips the redundant per-frame quirc_resize(cb_qr, ...) once the decode-plan geometry
	 * (a pure function of video_width/video_height, fixed for the output's lifetime) stops
	 * changing -- see camera-box-video.hpp's own doc comment on CbQrResizeCache for why this
	 * matters. Touched only on the decode worker thread (same thread that owns cb_qr itself). */
	camerabox::CbQrResizeCache cb_qr_resize_cache;

	/* #634: audit-log lock/unlock/offset-update transitions of the cluster above, so a live
	 * desync (like the closed #529) can be diagnosed from the OBS log alone. Pure/tested in
	 * camera-box-audio.hpp (tests/av_sync_dock_audit_log.rs) — touched only on the audio decode worker. */
	camerabox::CbLockAuditTracker cb_lock_audit;

	/* #926: holds CAMERA_BOX_LOCK_SOURCE_NAME's genlock_latency_ms_src so the dock's own displayed
	 * offset (audio_ts - video_ts) never rests negative ("audio early", a forbidden steady state).
	 * Only ever acts on a Locked/Updated lock-audit transition above; an Unlocked transition (real
	 * event, no test signal) freezes it -- see camera-box-audio.hpp's own doc comment. Touched only
	 * on the audio decode worker. */
	camerabox::CbDockLockCorrector cb_lock_corrector;

	/* #690: periodic live diagnostic -- tells a live session WHY the audio index/latency never
	 * lock (does the demod see nothing / decode garbage / decode fine but never ring-hit or
	 * cluster) and how well the video-QR decode is doing, from the OBS log alone (no rig access
	 * needed to read it). video counters are written on the video DECODE WORKER and read on the AUDIO
	 * decode worker (which owns the periodic log) -- atomic, no lock needed for plain counters. Ring
	 * hit/miss and the log-rate-limit timestamp are touched only on the audio decode worker. */
	std::atomic<uint64_t> cb_video_frames_seen{0};
	std::atomic<uint64_t> cb_video_frames_decoded{0};
	uint64_t cb_ring_hits = 0;   // decoded audio marker whose idx8 already had a valid video ring slot
	uint64_t cb_ring_misses = 0; // decoded audio marker with no video ring slot yet (too early / lap gap)
	bool cb_lock_state = false;  // last-known cluster lock state (mirrors CbLockAuditTracker's own)
	uint64_t cb_diag_last_log_ns = 0;

	/* #1177: watches whether the measurement INPUT is still advancing (video_decoded + crc_ok). When
	 * the marker/QR input disappears (EVENT mode) every existing unlock path is dead (all are
	 * decoded-marker-driven), so cb_lock_state would hold `yes` and the dock would show the last
	 * offset forever. Evaluated once per diag tick (below) -- the audio decode worker ticks while the
	 * issue-1381 gate is open (a QR is still decoding), so this catches the marker going away under a
	 * live QR; the whole test signal going away closes the gate, and its session end shows STALE --
	 * and drives the sync_stale_changed signal + the diag line's state=LIVE/STALE token.
	 * Pure/tested in av_sync_dock.rs, mirrored in camera-box-audio.hpp. Touched only on the audio
	 * decode worker (the thread that owns the diag block). */
	camerabox::CbDockInputStaleness cb_input_staleness;

	/* #1153: dead-pairing watchdog -- fires when the marker<->QR pairing stays dead (no
	 * meaningful ring-hit advance, no genuine lock) for a full epoch while the measurement input
	 * itself keeps flowing; the diag tick then resets ALL in-dock pairing state and re-acquires
	 * from scratch, so a manual OBS restart is never the only cure for a sticky
	 * post-latency-step unlock. Pure/tested in av_sync_dock.rs, mirrored in
	 * camera-box-audio.hpp. Touched only on the audio decode worker (same thread that owns the diag
	 * block). */
	camerabox::CbDockPairingWatchdog cb_pairing_watchdog;

	/* #926 fix-up (review finding 9/16): latches so each condition logs ONCE (and again after it
	 * clears and re-occurs) instead of spamming a blog() line per trusted marker while the
	 * condition persists. Touched only on the audio decode worker. */
	bool cb_lock_source_missing_logged = false; // CAMERA_BOX_LOCK_SOURCE_NAME not found
	bool cb_rail_pinned_logged = false;         // pinned at a hardware rail with audio still early
	/* #1319 Part 2: last genlock_latency_ms_src pin observed on a trusted push; -1 = none seen yet.
	 * A CHANGE is logged once (`pin-change observed <old> -> <new>`) so a wrong-cluster pick after a
	 * pin move is visible in the log. Touched only on the audio decode worker. */
	int32_t cb_last_seen_pin_ms = -1;

	/* issue 1367: the video decode's latest-pending mailbox + worker thread. st_raw_video (libobs's
	 * video-output thread) only publishes a bounded copy of the frame here; the worker runs the
	 * decoders on it. Started in st_start, stopped + joined in st_stop / st_destroy. */
	camerabox::CbDecodeMailbox<st_video_decode_job> cb_decode_mailbox;
	/* issue 1367: the max ns st_raw_video itself took (snapshot + copy + publish) since the last diag
	 * line -- the video-output thread's remaining cost, reported as publish_max_us. Raised on the
	 * video thread with cb_atomic_max_u64, read-and-reset on the audio decode worker's diag tick. */
	std::atomic<uint64_t> cb_publish_max_ns{0};

	/* issue 1381: the camera-box audio decode's FIFO + worker thread. st_raw_audio (libobs's audio
	 * thread) only runs the gate and copies the block here; the worker runs st_raw_audio_camera_box
	 * on the copy, so every piece of state that function touches is owned by the audio worker.
	 * Started in st_start, stopped + joined in st_stop / st_destroy / the destructor. */
	camerabox::CbAudioBlockFifo cb_audio_fifo;
	/* issue 1381: the longest gate + copy on the audio thread since the last diag line
	 * (audio_publish_max_us); raised on the audio thread, read-and-reset on the audio worker. */
	std::atomic<uint64_t> cb_audio_publish_max_ns{0};
	/* issue 1381: whether CAMERA_BOX_MEASURE_SOURCE_NAME exists; written on the video decode worker,
	 * read on the audio thread. The check's own state is decode-worker only. */
	std::atomic<bool> cb_measure_source_present{false};
	bool cb_measure_source_checked = false;
	uint64_t cb_measure_source_check_ts = 0;
	/* issue 1381: audio worker only -- the gate closed the last session (the dock was told STALE),
	 * and when the last "blocks dropped" warning went out. */
	bool cb_audio_paused = false;
	bool cb_audio_drop_logged = false;
	uint64_t cb_audio_drop_log_ns = 0;

	~sync_test_output()
	{
		/* issue 1367: join the decode worker FIRST -- it decodes with `qr` / `cb_qr`, which the
		 * lines below free. */
		cb_decode_mailbox.stop();
		/* issue 1381: the same for the audio decode worker and the picker it decodes with. */
		cb_audio_fifo.stop();
		if (qr)
			quirc_destroy(qr);
		if (cb_qr)
			quirc_destroy(cb_qr);
		delete cb_audio_dec;
	}
};

/* issue 1386: the functions one TU calls in another. */

/* sync-test-output-video.cpp */
void st_raw_video(void *, struct video_data *);
void st_video_decode_job_run(struct sync_test_output *, st_video_decode_job &);
void st_video_decode_job_prepare(const struct sync_test_output *, st_video_decode_job &);
void st_decode_worker_thread_setup();
void signal_sync_found(obs_output_t *, const struct sync_index *);
void sync_index_found(struct sync_test_output *, int, uint64_t, bool, uint32_t);

/* sync-test-output-audio.cpp */
void st_raw_audio(void *, struct audio_data *);
void st_audio_block_run(struct sync_test_output *, const camerabox::CbAudioBlock &);
void st_audio_block_gap(struct sync_test_output *, const camerabox::CbAudioBlock &);
void st_audio_session_end(struct sync_test_output *, unsigned);
void st_audio_worker_thread_setup();

} // namespace av_sync_output
