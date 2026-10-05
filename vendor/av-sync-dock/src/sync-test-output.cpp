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

/* camera-box issue 1386: this file keeps the obs_output_info callbacks, the output lifecycle and the
 * registration. The video path is in sync-test-output-video.cpp, the audio path in
 * sync-test-output-audio.cpp, the shared state in sync-test-output-internal.hpp. */

#include "sync-test-output-internal.hpp"

#include "plugin-macros.generated.h"

namespace av_sync_output {

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
	/* issue 1367: libobs has disconnected the raw callbacks by now (obs_output_destroy joins its
	 * end-capture thread first); wait for an in-flight decode, then free. */
	st->cb_decode_mailbox.stop();
	st->cb_audio_fifo.stop();
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

	/* issue 1367: st_start rewrites state the decode worker reads (the quirc size, the video
	 * geometry) -- join any worker left from a previous start first. A no-op when none runs. */
	st->cb_decode_mailbox.stop();
	/* issue 1381: the audio worker reads the channel layout rewritten below -- join it too. */
	st->cb_audio_fifo.stop();

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
	st->qr_grid_w = qr_width;
	st->qr_grid_h = qr_height;

	st->audio_sample_rate = audio_output_get_sample_rate(audio);
	st->audio_channels = audio_output_get_channels(audio);

	/* issue 1381: size and write both decode jobs' copy buffers here, on the starting thread, so the
	 * video-output thread's first frame into each slot neither allocates nor takes a page fault. */
	if (!st->cb_decode_mailbox.prepare_slots(
		    [st](st_video_decode_job &job) { st_video_decode_job_prepare(st, job); })) {
		blog(LOG_ERROR, "av-sync-dock: the video decode worker is still running, cannot size its buffers");
		return false;
	}

	/* issue 1367: the QR decode runs on this worker, never on libobs's video-output thread (a
	 * decode there made OBS skip 28 % of output frames on the cg OBS). Started before data capture
	 * so the first frame already has somewhere to go. */
	if (!st->cb_decode_mailbox.start([st](st_video_decode_job &job) { st_video_decode_job_run(st, job); },
					 st_decode_worker_thread_setup)) {
		blog(LOG_ERROR, "av-sync-dock: failed to start the video decode worker thread");
		return false;
	}
	blog(LOG_INFO, "av-sync-dock: video decode worker started (off the video-output thread, issue 1367)");

	/* issue 1381: the camera-box audio decode runs on this worker, never on libobs's audio thread
	 * (a decode there put the cg OBS mixer 13-22 s behind real time). Started before data capture;
	 * every slot is sized for a full AUDIO_OUTPUT_FRAMES block so the audio thread never allocates. */
	camerabox::CbAudioBlockFifo::Handlers audio_handlers;
	audio_handlers.process = [st](const camerabox::CbAudioBlock &block) { st_audio_block_run(st, block); };
	audio_handlers.on_gap = [st](const camerabox::CbAudioBlock &block) { st_audio_block_gap(st, block); };
	audio_handlers.on_session_end = [st](unsigned reason) { st_audio_session_end(st, reason); };
	audio_handlers.on_thread_start = st_audio_worker_thread_setup;
	if (!st->cb_audio_fifo.start(audio_handlers, st->audio_channels < MAX_AV_PLANES ? st->audio_channels : MAX_AV_PLANES,
				     AUDIO_OUTPUT_FRAMES)) {
		st->cb_decode_mailbox.stop();
		blog(LOG_ERROR, "av-sync-dock: failed to start the audio decode worker thread");
		return false;
	}
	blog(LOG_INFO, "av-sync-dock: audio decode worker started (off the audio thread, issue 1381)");

	obs_output_begin_data_capture(st->context, OBS_OUTPUT_VIDEO | OBS_OUTPUT_AUDIO);

	return true;
}

static void st_stop(void *data, uint64_t)
{
	auto *st = (struct sync_test_output *)data;

	/* issue 1367: libobs disconnects the raw callbacks on its own end-capture thread, so one last
	 * raw_video can still arrive after this returns -- publishing into a stopped mailbox is a
	 * no-op. */
	obs_output_end_data_capture(st->context);
	st->cb_decode_mailbox.stop();
	st->cb_audio_fifo.stop();
	blog(LOG_INFO, "av-sync-dock: video decode worker stopped (decoded=%llu decode_dropped=%llu)",
	     (unsigned long long)st->cb_decode_mailbox.taken(), (unsigned long long)st->cb_decode_mailbox.dropped());
	blog(LOG_INFO, "av-sync-dock: audio decode worker stopped (blocks=%llu audio_dropped=%llu decode_resets=%llu)",
	     (unsigned long long)st->cb_audio_fifo.taken(), (unsigned long long)st->cb_audio_fifo.dropped(),
	     (unsigned long long)st->cb_audio_fifo.resets());
}

} // namespace av_sync_output

extern "C" void register_sync_test_output()
{
	struct obs_output_info info = {};
	info.id = OUTPUT_ID;
	info.flags = OBS_OUTPUT_AV;
	info.get_name = av_sync_output::st_get_name;
	info.create = av_sync_output::st_create;
	info.destroy = av_sync_output::st_destroy;
	info.start = av_sync_output::st_start;
	info.stop = av_sync_output::st_stop;
	info.raw_video = av_sync_output::st_raw_video;
	info.raw_audio = av_sync_output::st_raw_audio;

	obs_register_output(&info);
}
