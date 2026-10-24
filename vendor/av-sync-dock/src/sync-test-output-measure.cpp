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

/* camera-box issue 1470: the camera-box audio decode reads the measurement source's OWN share of the
 * program mix, never the whole track-1 mix.
 *
 * Live 24.10.2026 on stream-snv the dock read -900.9 ms while the stream was in sync (+24.5 ms by
 * recording-verdict --av-sync). The program mixed the measurement clip (run 911016, a synthesized
 * copy of the cam2 painter's QPSK marker) next to `mbc`, and the clip's digital markers out-clustered
 * the room marker in the ring pairing. An 8-bit marker cannot tell two emitters apart, so the dock
 * decodes only what CAMERA_BOX_MEASURE_SOURCE_NAME itself puts into the program.
 *
 * - The share is the source's per-source mix buffer: libobs renders it once per tick (volume, mute
 *   and track routing applied) and the mix adds exactly those samples at the source's timeline, so
 *   it carries the sync offset and every placement term. The source's own capture callback does
 *   not: it hands over the packet as it arrived, before sync offset and placement.
 * - st_cb_mix_tick runs on libobs's audio thread after the mix of each tick (the dock connects it
 *   with obs_add_raw_audio_callback, on the output's own mixer); the tick's start is its timestamp.
 *   The output's raw_audio cannot do this: libobs re-chunks a raw output's audio off the mix ticks.
 * - The share is taken only when camera-box-measure-share.hpp proves it is the whole tick at offset
 *   0; any other tick is silence. A source out of the program for a second ends the decode session
 *   (CbAudioGate::NotInProgram: the dock STALE, one log line). Never a fallback to the mix.
 * - Audio-thread work stays a bounded copy (issue 1381): the per-source buffer into pre-sized planes,
 *   then the existing gate + FIFO copy. No lookup, no log line, no allocation, no decode here. */

#include "sync-test-output-internal.hpp"

#include "plugin-macros.generated.h"

namespace av_sync_output {

/* issue 1381: the audio thread's whole share of the camera-box audio decode -- the gate (a fresh
 * camera-box QR, the measurement source on this box) and a copy of every channel into the FIFO. The
 * worker decodes the copy; a closed gate ends the worker's session (decoders reset, dock STALE).
 * Bounded work: no decode, no log line, no source lookup, no signal here. Since issue 1470 its one
 * caller is the mix-tick callback, with the measurement source's share as the block. */
static void cb_audio_gate_and_publish(struct sync_test_output *st, const struct audio_data *frames, uint64_t last_qr_ns)
{
	const uint64_t publish_start_ns = os_gettime_ns();
	const uint64_t start_ts = st->start_ts;
	const uint64_t now = frames->timestamp > start_ts ? frames->timestamp - start_ts : 0;
	const camerabox::CbAudioGate gate = camerabox::cb_audio_decode_gate(
		true, st->cb_measure_source_present.load(std::memory_order_relaxed), now, last_qr_ns,
		CAMERA_BOX_TEST_SIGNAL_FRESH_NS);
	if (gate == camerabox::CbAudioGate::Open) {
		const size_t nch = st->audio_channels < MAX_AV_PLANES ? st->audio_channels : MAX_AV_PLANES;
		const float *planes[MAX_AV_PLANES];
		for (size_t c = 0; c < nch; c++)
			planes[c] = (const float *)frames->data[c];
		st->cb_audio_fifo.publish(planes, nch, frames->frames, frames->timestamp);
	} else {
		st->cb_audio_fifo.end_session((unsigned)gate);
	}
	camerabox::cb_atomic_max_u64(st->cb_audio_publish_max_ns, os_gettime_ns() - publish_start_ns);
}

/* issue 1470: hand the audio thread a (strong) reference to the measurement source, or none. The
 * decode worker calls it with each lookup's result, st_destroy with nullptr. The swap is under the
 * lock the audio thread copies the share under; the old reference is released after it, so a last
 * release (the source deleted meanwhile) never runs while the audio thread waits on that lock. */
void cb_measure_source_hold(struct sync_test_output *st, obs_source_t *src)
{
	obs_source_t *old;
	{
		std::lock_guard<std::mutex> lock(st->cb_measure_src_mutex);
		old = st->cb_measure_src;
		st->cb_measure_src = src;
	}
	obs_source_release(old);
}

/* issue 1470: libobs's audio thread, right after the mix of one tick (`mix->timestamp` = its start).
 * In camera-box mode it builds the tick's block from the measurement source's own share and hands it
 * to the gate + FIFO copy. The mix samples this callback receives are never read. Its whole cost (the
 * lock, the source reads, the fill and the copy) is what audio_publish_max_us= reports. A new source
 * object resets the tracker by pointer: a freed object's address reused by the next lookup would skip
 * that reset, which costs at most one tick (the tracker's own start check still holds). */
void st_cb_mix_tick(void *param, size_t mix_idx, struct audio_data *mix)
{
	auto *st = (struct sync_test_output *)param;
	const uint64_t tick_cost_start_ns = os_gettime_ns();
	if (!st->start_ts || mix_idx >= MAX_AUDIO_MIXES)
		return;

	bool cb_active;
	uint64_t last_qr_ns;
	{
		std::unique_lock<std::mutex> lock(st->mutex);
		cb_active = st->cb_mode_active;
		last_qr_ns = st->cb_video_last_decode_ts_ns;
	}
	if (!cb_active)
		return;

	const size_t nch = st->audio_channels < MAX_AV_PLANES ? st->audio_channels : MAX_AV_PLANES;
	const size_t frames = mix->frames < AUDIO_OUTPUT_FRAMES ? mix->frames : AUDIO_OUTPUT_FRAMES;
	camerabox::CbShareTick tick;
	tick.tick_start_ns = mix->timestamp;
	tick.tick_end_ns = mix->timestamp + util_mul_div64(frames, 1000000000ULL, st->audio_sample_rate);
	float *out[MAX_AV_PLANES] = {};
	for (size_t c = 0; c < nch; c++)
		out[c] = st->cb_share_planes[c].data();

	camerabox::CbShareDecision decision;
	{
		std::lock_guard<std::mutex> lock(st->cb_measure_src_mutex);
		obs_source_t *src = st->cb_measure_src;
		if (src != st->cb_share_src_seen) {
			st->cb_share_tracker.reset();
			st->cb_share_src_seen = src;
		}
		tick.source_found = src != nullptr;
		tick.source_active = src && obs_source_active(src);
		tick.source_pending = !src || obs_source_audio_pending(src);
		tick.source_ts_after_ns = src ? obs_source_get_audio_timestamp(src) : 0;
		decision = st->cb_share_tracker.observe(tick);
		const float *share[MAX_AV_PLANES] = {};
		if (decision.take) {
			struct obs_source_audio_mix m;
			obs_source_get_audio_mix(src, &m);
			for (size_t c = 0; c < nch; c++)
				share[c] = m.output[mix_idx].data[c];
		}
		camerabox::cb_measure_share_fill(decision.take, share, nch, frames, out);
	}

	if (decision.absent_long && st->cb_measure_source_present.load(std::memory_order_relaxed)) {
		st->cb_audio_fifo.end_session((unsigned)camerabox::CbAudioGate::NotInProgram);
		camerabox::cb_atomic_max_u64(st->cb_audio_publish_max_ns, os_gettime_ns() - tick_cost_start_ns);
		return;
	}
	struct audio_data share_frames = {};
	for (size_t c = 0; c < nch; c++)
		share_frames.data[c] = (uint8_t *)out[c];
	share_frames.frames = (uint32_t)frames;
	share_frames.timestamp = mix->timestamp;
	cb_audio_gate_and_publish(st, &share_frames, last_qr_ns);
	camerabox::cb_atomic_max_u64(st->cb_audio_publish_max_ns, os_gettime_ns() - tick_cost_start_ns);
}

/* issue 1470: st_start, after the FIFO started and before data capture. The share planes are sized
 * AND written here, so the audio thread never allocates or first-touches them (issue 1381). */
void cb_measure_share_connect(struct sync_test_output *st)
{
	const size_t nch = st->audio_channels < MAX_AV_PLANES ? st->audio_channels : MAX_AV_PLANES;
	for (size_t c = 0; c < nch; c++)
		st->cb_share_planes[c].assign(AUDIO_OUTPUT_FRAMES, 0.0f);
	st->cb_share_tracker = camerabox::CbMeasureShareTracker();
	st->cb_share_src_seen = nullptr;
	st->cb_share_mix_idx = obs_output_get_mixer(st->context);
	obs_add_raw_audio_callback(st->cb_share_mix_idx, nullptr, st_cb_mix_tick, st);
	st->cb_share_connected = true;
	blog(LOG_INFO,
	     "av-sync-dock: camera-box audio decode reads '%s''s own share of mix %zu, never the whole mix "
	     "(issue 1470)",
	     CAMERA_BOX_MEASURE_SOURCE_NAME, st->cb_share_mix_idx);
}

/* issue 1470: st_start (a previous start's callback), st_stop and st_destroy. libobs calls an audio
 * callback under the mix's input lock, so none runs once this returns. */
void cb_measure_share_disconnect(struct sync_test_output *st)
{
	if (!st->cb_share_connected)
		return;
	obs_remove_raw_audio_callback(st->cb_share_mix_idx, st_cb_mix_tick, st);
	st->cb_share_connected = false;
}

} // namespace av_sync_output
