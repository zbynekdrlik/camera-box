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

/* camera-box issue 1386: the output's video path, split out of sync-test-output.cpp -- the QR decode
 * worker (issue 1367: st_raw_video on libobs's video-output thread only copies, st_video_decode_job_run
 * decodes), the camera-box top-band decode, norihiro's whole-frame decode and marker search, and
 * norihiro's sync_index list, which the audio path shares (sync_index_found and signal_sync_found,
 * declared in sync-test-output-internal.hpp). */

#include "sync-test-output-internal.hpp"

#include "plugin-macros.generated.h"

namespace av_sync_output {

static void video_marker_found(struct sync_test_output *st, uint64_t timestamp, float score);

/* issue 1367: norihiro's sq / diff_u32 / sqrt_u32 circle-row math moved, unchanged, into
 * camera-box-frame-copy.hpp (cb_marker_circle_row / cb_isqrt_u32), where the self-test proves the
 * copied marker patches cover every pixel it reads. */

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

/* issue 1381: whether this box has the measurement source (CAMERA_BOX_MEASURE_SOURCE_NAME) the
 * camera-box audio decode exists for. Decode worker only (the lookup takes the sources mutex), at most
 * once per CAMERA_BOX_MEASURE_SOURCE_RECHECK_NS of frame time; the audio thread reads the atomic. The
 * first answer and every change are logged, at INFO: resolume and strih have no such source by
 * design, so "not found" there is the expected state, not a warning. */
static void cb_refresh_measure_source(struct sync_test_output *st, uint64_t video_ts)
{
	if (st->cb_measure_source_checked && video_ts >= st->cb_measure_source_check_ts &&
	    video_ts - st->cb_measure_source_check_ts < CAMERA_BOX_MEASURE_SOURCE_RECHECK_NS)
		return;
	const bool first = !st->cb_measure_source_checked;
	st->cb_measure_source_checked = true;
	st->cb_measure_source_check_ts = video_ts;
	obs_source_t *src = obs_get_source_by_name(CAMERA_BOX_MEASURE_SOURCE_NAME);
	const bool present = src != nullptr;
	obs_source_release(src);
	const bool was = st->cb_measure_source_present.exchange(present);
	if (first || was != present)
		blog(LOG_INFO, "av-sync-dock: measurement source '%s' %s (issue 1381)",
		     CAMERA_BOX_MEASURE_SOURCE_NAME,
		     present ? "found -- the camera-box audio decode runs while the test signal is fresh"
			     : "not found on this box -- the camera-box audio decode stays off");
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
		// Same mutex the audio side locks to READ these — the video ring + f/c/q_ms are written
		// here (decode worker thread) and read on the audio thread (gate) and the audio decode worker; all sides must take the lock.
		std::unique_lock<std::mutex> lock(st->mutex);
		st->cb_video_ts_ns[low] = video_ts;
		st->cb_video_valid[low] = true;
		st->cb_mode_active = true;
		st->cb_video_last_decode_ts_ns = video_ts; // #926 fix-up finding 6: overall freshness signal
		st->f = CAMERA_BOX_AUDIO_F_HZ;
		st->c = CAMERA_BOX_AUDIO_C;
		st->q_ms = CAMERA_BOX_AUDIO_Q_MS;
	}
	cb_refresh_measure_source(st, video_ts);
	// Reuse the existing dock-UI plumbing (video index / missed% / frequency labels).
	st->qr_data.f = CAMERA_BOX_AUDIO_F_HZ;
	st->qr_data.c = CAMERA_BOX_AUDIO_C;
	st->qr_data.q_ms = CAMERA_BOX_AUDIO_Q_MS;
	st->qr_data.index = low;
	st->qr_data.index_max = 256;
	st->qr_data.valid = true;
}

/* issue 1367: the frame's first plane as camera-box-frame-copy.hpp reads it. */
static camerabox::CbPlaneView st_plane_view(const struct sync_test_output *st, const struct video_data *frame)
{
	camerabox::CbPlaneView v;
	v.data = frame->data[0];
	v.linesize = frame->linesize[0];
	v.pixelsize = st->video_pixelsize;
	v.pixeloffset = st->video_pixeloffset;
	v.intensity = st->video_get_intensity;
	return v;
}

/* issue 1367 (producer, libobs's video-output thread): sample norihiro's whole-frame QR grid --
 * every qr_step-th pixel of every qr_step-th row, the sampling st_raw_video_qrcode_decode used to do
 * straight into quirc's buffer -- into the decode job, at the size quirc_resize() got in st_start. */
/* issue 1381: the bytes st_norihiro_gather_grid copies per frame -- also what
 * st_video_decode_job_prepare sizes each slot's grid to. */
static size_t st_norihiro_grid_bytes(const struct sync_test_output *st)
{
	return (size_t)st->qr_grid_w * st->qr_grid_h;
}

static void st_norihiro_gather_grid(const struct sync_test_output *st, const struct video_data *frame,
				    std::vector<uint8_t> &dst)
{
	const size_t need = st_norihiro_grid_bytes(st);
	if (dst.size() < need)
		dst.resize(need);
	camerabox::cb_copy_step_grid(st_plane_view(st, frame), st->qr_step, st->qr_grid_w, st->qr_grid_h,
				     dst.data());
}

/* Decode worker thread (issue 1367): `grid` is the qr_grid_w x qr_grid_h sample
 * st_norihiro_gather_grid took on the video thread, `timestamp` that frame's own timestamp. */
static void st_raw_video_qrcode_decode(struct sync_test_output *st, const uint8_t *grid, uint64_t timestamp)
{
	int w, h;
	auto qr = st->qr;
	uint8_t *qrbuf = quirc_begin(qr, &w, &h);
	memcpy(qrbuf, grid, (size_t)w * (size_t)h);
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
			signal_qrcode_found(st->context, timestamp - st->start_ts, st->qr_corners);
			cb_video_qr_record(st, cb.frame_id, timestamp - st->start_ts);
			video_marker_found(st, timestamp, 1.0f);
			continue;
		}

		if (!st->qr_data.decode((char *)data.payload))
			continue;

		for (int j = 0; j < 4; j++) {
			st->qr_corners[j].x = code.corners[j].x * st->qr_step;
			st->qr_corners[j].y = code.corners[j].y * st->qr_step;
		}

		signal_qrcode_found(st->context, timestamp - st->start_ts, st->qr_corners);

		adjust_corners(st->qr_corners);

		if (st->qr_data.f > 0 && st->qr_data.c > 0) {
			std::unique_lock<std::mutex> lock(st->mutex);
			st->f = st->qr_data.f;
			st->c = st->qr_data.c;
			st->q_ms = st->qr_data.q_ms;
		}

		st->video_marker_max_ts = timestamp + st->qr_data.q_ms * 3 * 1000000;
		st->video_level_prev = 0;
	}
}

/* issue 1367 (producer, libobs's video-output thread): copy the intensity inside each corner's
 * circle bounding box -- the only pixels st_raw_video_find_marker reads -- around the corner
 * snapshot already stored in `job.corners`. A corner with r == 0 or a box outside the frame gets an
 * empty patch; the marker search reads nothing there, exactly as its own loop bounds dictate. */
static void st_marker_cut_patches(const struct sync_test_output *st, const struct video_data *frame,
				  struct st_video_decode_job &job)
{
	const camerabox::CbPlaneView v = st_plane_view(st, frame);
	for (size_t i = 0; i < N_CORNERS; i++) {
		const struct corner_type c = job.corners[i];
		struct st_marker_patch &p = job.patches[i];
		p.rect = camerabox::cb_marker_patch_rect(c.x, c.y, c.r, st->video_width, st->video_height);
		const size_t need = (size_t)p.rect.w * p.rect.h;
		if (p.luma.size() < need)
			p.luma.resize(need);
		camerabox::cb_copy_patch(v, p.rect, p.luma.data());
	}
}

/* Decode worker thread (issue 1367): the same circle sums as before, read from the patches the video
 * thread cut around `job.corners` (the corners the worker published after its previous norihiro
 * decode), with the frame's own timestamp. */
static void st_raw_video_find_marker(struct sync_test_output *st, const struct st_video_decode_job &job)
{
	int64_t sum = 0;

	if (job.timestamp > st->video_marker_max_ts) {
		st->video_level_prev = 0;
		return;
	}

	for (size_t i = 0; i < N_CORNERS; i++) {
		const struct corner_type c = job.corners[i];
		if (c.r == 0)
			return;
		const struct st_marker_patch &p = job.patches[i];
		uint32_t y0 = c.y > c.r ? c.y - c.r : 0;
		uint32_t y1 = std::min(c.y + c.r, st->video_height);

		for (uint32_t y = y0; y < y1; y++) {
			const camerabox::CbSpan s = camerabox::cb_marker_circle_row(c.x, c.y, c.r, y, st->video_width);

			uint32_t line_sum = 0;

			/* The span lies inside the patch the video thread copied (cb_marker_patch_rect), which
			 * the self-test's corner sweep proves over every frame edge. */
			if (s.x0 < s.x1) {
				const uint8_t *data =
					p.luma.data() + (size_t)(y - p.rect.y0) * p.rect.w + (s.x0 - p.rect.x0);
				for (uint32_t x = s.x0; x < s.x1; x++)
					line_sum += *data++;
			}

			if (i & 1)
				sum += line_sum;
			else
				sum -= line_sum;
		}
	}

	// blog(LOG_INFO, "st_raw_video-plot: %.03f %f", (job.timestamp - st->start_ts) * 1e-9, (double)sum / (255.0 * M_PI * sq(st->qr_corners[0].r)));

	if (st->qr_data.valid && st->video_level_prev < 0 && sum >= 0) {
		/* Calculate the time half frame later than the zero-cross of `sum`. */
		uint64_t t = job.timestamp - st->video_level_prev_ts;
		uint64_t add = util_mul_div64(t, sum - st->video_level_prev * 3, (sum - st->video_level_prev) * 2);
		video_marker_found(st, st->video_level_prev_ts + add, (float)(sum - st->video_level_prev));
	}
	st->video_level_prev = sum;
	st->video_level_prev_ts = job.timestamp;
}

static bool is_overlapped(uint32_t index, uint32_t index_max, uint32_t next_index)
{
	return index_max && ((index_max + next_index - index) % index_max) > index_max / 2;
}

void signal_sync_found(obs_output_t *ctx, const struct sync_index *si)
{
	uint8_t stack[64];
	struct calldata cd;
	calldata_init_fixed(&cd, stack, sizeof(stack));
	auto *sh = obs_output_get_signal_handler(ctx);

	calldata_set_ptr(&cd, "data", const_cast<sync_index *>(si));
	signal_handler_signal(sh, "sync_found", &cd);
}

void sync_index_found(struct sync_test_output *st, int index, uint64_t ts, bool is_video, uint32_t index_max)
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
 * true if any camera-box QR decoded (and records it into the ring).
 *
 * issue 1367: this runs on the decode worker thread. `src` is the top band st_cb_gather_top_band
 * copied on libobs's video-output thread (video_width x plan.band_h luma), `timestamp` that frame's
 * own timestamp. */
static bool st_raw_video_camera_box_decode(struct sync_test_output *st, const uint8_t *src, uint64_t timestamp)
{
	st->cb_video_frames_seen.fetch_add(1, std::memory_order_relaxed);

	camerabox::CbTopBandPlan plan = camerabox::cb_top_band_decode_plan(st->video_width, st->video_height);
	if (plan.band_h == 0 || plan.dst_w == 0 || plan.dst_h == 0)
		return false;

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
			signal_qrcode_found(st->context, timestamp - st->start_ts, st->qr_corners);
			cb_video_qr_record(st, cb.frame_id, timestamp - st->start_ts);
			video_marker_found(st, timestamp, 1.0f);
			found_any = true;
		}
	}
	if (found_any)
		st->cb_video_frames_decoded.fetch_add(1, std::memory_order_relaxed);
	return found_any;
}

/* issue 1381: the bytes st_cb_gather_top_band copies per frame for `plan` (0 when the plan has no
 * band) -- also what st_video_decode_job_prepare sizes each slot's band to. */
static size_t st_cb_top_band_bytes(const struct sync_test_output *st, const camerabox::CbTopBandPlan &plan)
{
	if (plan.band_h == 0 || plan.dst_w == 0 || plan.dst_h == 0)
		return 0;
	return (size_t)st->video_width * plan.band_h;
}

/* issue 1367 (producer, libobs's video-output thread): gather the TOP band (rows 0..band_h) into a
 * tight full-res luma buffer, honoring the pixel format's stride / offset / intensity extractor --
 * the gather st_raw_video_camera_box_decode used to do inline, now into the decode job. */
static void st_cb_gather_top_band(const struct sync_test_output *st, const struct video_data *frame,
				  std::vector<uint8_t> &dst)
{
	camerabox::CbTopBandPlan plan = camerabox::cb_top_band_decode_plan(st->video_width, st->video_height);
	const size_t need = st_cb_top_band_bytes(st, plan);
	if (need == 0)
		return;

	if (dst.size() < need)
		dst.resize(need);
	camerabox::cb_copy_top_band(st_plane_view(st, frame), st->video_width, plan.band_h, dst.data());
}

/* issue 1381 (the starting thread, through the mailbox's prepare_slots in st_start): size AND write
 * one slot's copy buffers for this output's geometry -- the top band, and norihiro's grid, which
 * st_raw_video fills on every frame until camera-box mode latches -- so the video-output thread's
 * first frame into the slot neither allocates nor first-touches these pages (the band alone
 * is 1.5 MB at 1080p, 6 MB at 4K). assign() writes every byte, also on a restart where the buffer
 * already has the size. The marker patches are left to grow on use: their size follows the circle
 * radius of a decoded PHONE QR (norihiro mode, never the camera-box rig path), and a worst-case
 * pre-size (a QR as tall as the frame) would hold ~9 MB per 4K output for a mode the rig never runs. */
void st_video_decode_job_prepare(const struct sync_test_output *st, st_video_decode_job &job)
{
	const camerabox::CbTopBandPlan plan = camerabox::cb_top_band_decode_plan(st->video_width, st->video_height);
	job.band.assign(st_cb_top_band_bytes(st, plan), 0);
	job.grid.assign(st_norihiro_grid_bytes(st), 0);
}

/* issue 1367: libobs calls this on its ONE video-output thread, shared by every raw output on the
 * box (NDI outputs, recordings). A QR decode here took longer than the frame budget and made
 * video-io skip output frames (28 % on the cg OBS, 26.9.2026). So this only copies what the
 * decoders read -- the top band, and outside camera-box mode norihiro's grid + the marker window --
 * into the decode mailbox and returns. The worker (st_video_decode_job_run) decodes the copy with
 * this frame's own timestamp; a frame that arrives while the worker is busy replaces the pending one
 * and is counted as decode_dropped on the dock diag line. */
void st_raw_video(void *data, struct video_data *frame)
{
	auto *st = (struct sync_test_output *)data;

	if (!st->video_pixelsize)
		return;

	if (!st->start_ts)
		st->start_ts = frame->timestamp;

	const uint64_t publish_start_ns = os_gettime_ns();
	bool cb_active;
	struct corner_type corners[N_CORNERS];
	{
		std::unique_lock<std::mutex> lock(st->mutex);
		cb_active = st->cb_mode_active;
		std::copy(st->marker_corners, st->marker_corners + N_CORNERS, corners);
	}

	st->cb_decode_mailbox.publish([&](st_video_decode_job &job) {
		job.timestamp = frame->timestamp;
		st_cb_gather_top_band(st, frame, job.band);
		job.norihiro = !cb_active;
		if (job.norihiro) {
			st_norihiro_gather_grid(st, frame, job.grid);
			std::copy(corners, corners + N_CORNERS, job.corners);
			st_marker_cut_patches(st, frame, job);
		}
	});
	camerabox::cb_atomic_max_u64(st->cb_publish_max_ns, os_gettime_ns() - publish_start_ns);
}

/* issue 1367: once, on the decode worker thread before its first job. Names it (15 characters: the
 * Linux thread-name limit; visible in top -H / gdb, and to an attached debugger on Windows).
 * The worker deliberately stays at NORMAL priority: the video-output thread takes `st->mutex` and
 * the mailbox lock every frame and the worker holds both briefly, and a Windows std::mutex (SRW
 * lock) has no priority inheritance -- a starved below-normal worker preempted inside one of those
 * sections would stall the video thread, the very stall this worker exists to remove. */
void st_decode_worker_thread_setup()
{
	os_set_thread_name("avsync-decode");
}

/* issue 1367: the decode worker thread's per-frame body -- the decode st_raw_video used to run
 * inline, now on the copied frame. */
void st_video_decode_job_run(struct sync_test_output *st, st_video_decode_job &job)
{
	// #398: camera-box's own better-scaled top-band decode first. Once camera-box mode is active it
	// is the SOLE video-QR source (norihiro's ÷qr_step whole-frame pass misses our big top QR), so
	// skip norihiro's decode + marker-window logic — those are kept only for the phone-based method
	// when NOT in camera-box mode.
	st_raw_video_camera_box_decode(st, job.band.data(), job.timestamp);
	bool cb_active;
	{
		std::unique_lock<std::mutex> lock(st->mutex);
		cb_active = st->cb_mode_active;
	}
	/* `norihiro` is false only when camera-box mode was already active when the frame was copied;
	 * the mode never switches back off, so this is the same gate one frame earlier. */
	if (cb_active || !job.norihiro)
		return;

	if (job.grid.size() >= (size_t)st->qr_grid_w * st->qr_grid_h)
		st_raw_video_qrcode_decode(st, job.grid.data(), job.timestamp);
	{
		/* Publish the corners the decode just left (adjust_corners() output) for the video thread
		 * to cut the NEXT frame's marker window around. */
		std::unique_lock<std::mutex> lock(st->mutex);
		std::copy(st->qr_corners, st->qr_corners + N_CORNERS, st->marker_corners);
	}
	st_raw_video_find_marker(st, job);
}

} // namespace av_sync_output
