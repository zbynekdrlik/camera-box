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

/* camera-box issue 1386: the output's audio path, split out of sync-test-output.cpp -- norihiro's
 * demod (st_raw_audio outside camera-box mode) and the camera-box decode: the gate + FIFO copy on
 * libobs's audio thread and the audio decode worker (issue 1381), the per-channel marker pick (issue
 * 1367), the video-ring pairing, the lock audit / corrector and the ~10 s diag tick. */

#include "sync-test-output-internal.hpp"

#include "plugin-macros.generated.h"

namespace av_sync_output {

std::pair<int32_t, int32_t> operator-(std::pair<int32_t, int32_t> a, std::pair<int32_t, int32_t> b)
{
	return std::make_pair(a.first - b.first, a.second - b.second);
}

std::complex<float> int16_to_complex(std::pair<int32_t, int32_t> x)
{
	return std::complex<float>((float)x.first / 32768.0f, (float)x.second / 32768.0f);
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

static void cb_audio_forget_lock(struct sync_test_output *);

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
 * UI thread via `obs_queue_task`, never mutated directly from the camera-box audio decode (the core
 * AUDIO thread until issue 1381, its own worker since): mutating a LIVE source's settings `obs_data` (no internal mutex of its own) from
 * a real-time audio callback races the video thread's own reads/writes of the same source and any
 * UI/WebSocket access, and `obs_get_source_by_name` itself takes the global sources-list mutex,
 * which the UI thread is the expected/serialized caller of throughout the rest of this codebase.
 * The queued task uses a FRESH `obs_data_create()` holding only the ONE key -- never the shared
 * `obs_source_get_settings()` object handed back in place (finding 12's fragile "mutate the live
 * settings object" idiom) -- and reads the value back right after the update to catch a mismatch
 * (finding 8) close to the write. Fire-and-forget (`wait=false`): the audio decode must never
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

/* #1153: apply the dead-pairing recovery the diag-tick watchdog decided on -- reset EVERY piece of
 * in-dock pairing state so the chain re-acquires from scratch (a sticky post-latency-step unlock
 * must never need a manual OBS restart). Cumulative counters/stats are deliberately NOT reset (the
 * diag line must stay monotonic -- supervisors parse it); the evidence line's epoch deltas
 * discriminate the poison class from the OBS log alone: crc_ok near the ~1/256 chance floor of the
 * preamble delta = the marker waveform is degraded upstream of the dock, while a healthy crc_ok
 * rate with a dead ring = in-dock pairing state, which this reset clears. Runs on the audio decode
 * worker only, same as its caller (the diag block); the ring and the offset history are the pieces
 * shared under the mutex. */
static void cb_apply_pairing_recovery(struct sync_test_output *st,
                                      const camerabox::CbDockPairingRecovery &rec)
{
	{
		std::unique_lock<std::mutex> lock(st->mutex);
		for (size_t slot = 0; slot < CAMERA_BOX_RING_SLOTS; slot++)
			st->cb_video_valid[slot] = false;
		st->cb_offset_history.clear();
	}
	st->cb_offset_cluster = camerabox::RollingOffsetCluster::dock();
	/* A stale-held lock (zero ring hits all epoch) must not survive the reset on the UI either --
	 * the audit tracker could never fire its own Unlocked transition without a decode to push. */
	cb_audio_forget_lock(st);
	st->cb_audio_dec->reset_window();
	blog(LOG_WARNING,
	     "av-sync-dock: PAIRING-RECOVER dead pairing window (ring_hit +%llu, crc_ok "
	     "+%llu, preambles +%llu, video_decoded +%llu in %llus) -- reset "
	     "ring+cluster+decoder window, re-acquiring from scratch",
	     (unsigned long long)rec.ring_hit_delta, (unsigned long long)rec.crc_ok_delta,
	     (unsigned long long)rec.preambles_delta, (unsigned long long)rec.video_decoded_delta,
	     (unsigned long long)(rec.window_ns / 1000000000ull));
}

/* issue 1367: the audio decode's per-channel picker (camera-box-channel-pick.hpp) for the output's
 * current channel layout, at most MAX_AV_PLANES channels. A new layout starts the decode over, and
 * the absolute sample count with it. Returns the number of channels to push, or 0 when there is
 * nothing to decode. Audio decode worker only, like its caller. */
static size_t cb_ensure_audio_picker(struct sync_test_output *st)
{
	const size_t nch = st->audio_channels < MAX_AV_PLANES ? st->audio_channels : MAX_AV_PLANES;
	if (st->cb_audio_dec && st->cb_audio_dec->channels() != nch) {
		delete st->cb_audio_dec;
		st->cb_audio_dec = nullptr;
		st->cb_audio_pushed = 0;
	}
	if (!st->cb_audio_dec) {
		size_t sig = camerabox::cb_signal_len(st->audio_sample_rate, CAMERA_BOX_AUDIO_F_HZ,
		                                      CAMERA_BOX_AUDIO_C);
		if (sig == 0 || nch == 0)
			return 0;
		// per channel: window 3 marker lengths, dedup gap 1, pick window + floor from the header.
		st->cb_audio_dec = new camerabox::ChannelMarkerPicker(camerabox::ChannelMarkerPicker::dock(
			nch, st->audio_sample_rate, CAMERA_BOX_AUDIO_F_HZ, CAMERA_BOX_AUDIO_C));
	}
	return nch;
}

/* issue 1367: a switch of the paired audio channel moves the measured offset by ~10 ms (R is
 * 10.17 ms behind L on the stereo mbc input) and the offset cluster is not reset, so it is logged
 * when it happens: the first switch at once, then at most one line per diag interval, naming how
 * many switches it stands for (the decision is CbChannelSwitchLog, tested in the header's
 * mirrors). The diag line's channel_switches= is the running total. Audio decode worker only; `prev` is
 * the channel chosen before this callback's push. */
static void cb_note_channel_switch(struct sync_test_output *st, size_t prev, const struct audio_data *frames)
{
	uint64_t switches = 0;
	if (!st->cb_switch_log.observe(prev, st->cb_audio_dec->chosen, frames->timestamp,
				       CAMERA_BOX_DIAG_LOG_INTERVAL_NS, &switches))
		return;
	const std::string clusters = camerabox::cb_channel_clusters_text(st->cb_audio_dec->clusters);
	blog(LOG_INFO, "av-sync-dock: marker channel %zu -> %zu (channel_clusters=%s, %llu switch(es) since the last line)",
	     prev, st->cb_audio_dec->chosen, clusters.c_str(), (unsigned long long)switches);
}

/* The camera-box audio path's ~10 s tick (issue 1367 split it out of st_raw_audio_camera_box): the
 * #1177 staleness evaluation, the #690 diag line and the #1153 dead-pairing recovery, in that order.
 * Audio decode worker only; st->cb_audio_dec is set (the caller returns earlier otherwise). */
static void cb_audio_diag_tick(struct sync_test_output *st, const struct audio_data *frames)
{
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

		/* #1177: evaluate measurement-input staleness at this SAME ~10s cadence -- the audio decode
		 * worker keeps ticking while the issue-1381 gate is open, even when the marker decode counter
		 * (crc_ok) stops. On the boundary crossing, fire a one-shot log line + the sync_stale_changed
		 * signal so the dock stops presenting the last locked offset as if it were live. When the whole
		 * test signal goes away (EVENT mode) the gate closes and st_audio_session_end shows STALE. */
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

		/* issue 1367: video_frames counts the frames the decode worker processed; decode_dropped
		 * (appended at the END, existing tokens unchanged) the frames the video thread replaced in
		 * the mailbox while the worker was still decoding, so video_frames + decode_dropped is every
		 * frame OBS delivered; publish_max_us the longest st_raw_video (the video-output thread's
		 * remaining cost) since the previous diag line. preambles/crc_ok/crc_fail are summed over
		 * every audio channel (one decoder each); marker_channel is the channel whose markers are
		 * paired (0-based), channel_clusters each channel's self-consistency cluster over the pick
		 * window and channel_switches the running count of pick switches, appended last. */
		const std::string channel_clusters = camerabox::cb_channel_clusters_text(st->cb_audio_dec->clusters);
		blog(LOG_INFO,
		     "av-sync-dock: diag video_frames=%llu video_decoded=%llu(%.1f%%) "
		     "audio_samples=%llu preambles=%llu crc_ok=%llu crc_fail=%llu "
		     "ring_hit=%llu ring_miss=%llu locked=%s state=%s decode_dropped=%llu publish_max_us=%llu "
		     "marker_channel=%zu channel_clusters=%s channel_switches=%llu"
		     " decode_ms_max=%.3f decode_ms_sum=%.1f audio_dropped=%llu decode_resets=%llu audio_publish_max_us=%llu",
		     (unsigned long long)vseen, (unsigned long long)vdec, vpct,
		     (unsigned long long)st->cb_audio_pushed,
		     (unsigned long long)st->cb_audio_dec->stats.preamble_screens_passed,
		     (unsigned long long)st->cb_audio_dec->stats.crc_ok,
		     (unsigned long long)st->cb_audio_dec->stats.crc_fail,
		     (unsigned long long)st->cb_ring_hits, (unsigned long long)st->cb_ring_misses,
		     st->cb_lock_state ? "yes" : "no", input_stale ? "STALE" : "LIVE",
		     (unsigned long long)st->cb_decode_mailbox.dropped(),
		     (unsigned long long)(st->cb_publish_max_ns.exchange(0) / 1000), st->cb_audio_dec->chosen,
		     channel_clusters.c_str(), (unsigned long long)st->cb_switch_log.total,
		     (double)st->cb_audio_fifo.take_process_max_ns() / 1e6,
		     (double)st->cb_audio_fifo.take_process_sum_ns() / 1e6,
		     (unsigned long long)st->cb_audio_fifo.dropped(), (unsigned long long)st->cb_audio_fifo.resets(),
		     (unsigned long long)(st->cb_audio_publish_max_ns.exchange(0) / 1000));

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
		if (rec.fire)
			cb_apply_pairing_recovery(st, rec);
	}
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
	/* issue 1367: decode the marker on EVERY channel and keep one, never their average. The stereo
	 * mbc input carries the marker on L and R 10.17 ms apart, and their sum is undecodable. The
	 * picker applies the offline gate's one channel rule (the lowest channel that clears the
	 * decodability floor, else the largest cluster) and returns only the chosen channel's markers.
	 * A non-finite sample stays on its own channel: each channel has its own decoder, whose kernel
	 * reads it as silence (#1153). */
	const size_t nch = cb_ensure_audio_picker(st);
	if (nch == 0)
		return;

	size_t nf = frames->frames;
	const float *planes[MAX_AV_PLANES];
	for (size_t cix = 0; cix < nch; cix++)
		planes[cix] = (const float *)frames->data[cix];

	const size_t prev_channel = st->cb_audio_dec->chosen;
	const uint64_t base = st->cb_audio_pushed; // absolute index of this callback's first sample
	std::vector<std::pair<uint64_t, uint8_t>> markers = st->cb_audio_dec->push(planes, nf);
	st->cb_audio_pushed += (uint64_t)nf;
	cb_note_channel_switch(st, prev_channel, frames);

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
			/* #1319 Part 2: append the chosen cluster's lag bucket + candidate-pool size at the
			 * END (existing tokens byte-identical). lag_idx = the display offset quantized into
			 * +-tol buckets, so a WRONG-cluster pick (offset jumps ~one cluster width) changes it
			 * while a small drift holds it; cands = the total candidate pool the densest window was
			 * chosen from, so matched<<cands reveals a bimodal pool. Both diagnose the dock bias. */
			blog(LOG_INFO,
			     "av-sync-dock: %s offset=%.1fms source=cluster matched=%zu mad=%.1fms lag_idx=%ld cands=%zu",
			     audit_ev.kind == camerabox::CbLockEventKind::Locked ? "LOCKED" : "UPDATED",
			     camerabox::cb_dock_lock_display_offset_ms(audit_ev.offset_ms), audit_ev.matched,
			     audit_ev.mad_ms,
			     (long)std::llround(camerabox::cb_dock_lock_display_offset_ms(audit_ev.offset_ms)
			                        / (2.0 * camerabox::CB_CLUSTER_TOL_MS)),
			     st->cb_offset_cluster.samples.size());
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
					/* #1319 Part 2: a source-pin CHANGE (the E2E gate / operator moved
					 * genlock_latency_ms_src) is exactly when the cluster can re-pick a wrong
					 * QPSK marker lag -- log it once so a wrong pick afterwards is visible.
					 * -1 sentinel = no pin seen yet, so the first read never logs. */
					if (st->cb_last_seen_pin_ms >= 0 && current_ms != st->cb_last_seen_pin_ms)
						blog(LOG_INFO, "av-sync-dock: pin-change observed %d -> %d",
						     (int)st->cb_last_seen_pin_ms, (int)current_ms);
					st->cb_last_seen_pin_ms = current_ms;
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

	cb_audio_diag_tick(st, frames);
}

static_assert(MAX_AV_PLANES <= camerabox::CB_AUDIO_FIFO_MAX_CHANNELS, "the audio FIFO holds every OBS plane");

/* issue 1381: the audio worker's per-block body -- the camera-box decode st_raw_audio used to run
 * inline, now on the copied block, with the block's own timestamp. */
void st_audio_block_run(struct sync_test_output *st, const camerabox::CbAudioBlock &block)
{
	struct audio_data frames = {};
	for (size_t c = 0; c < block.channels; c++)
		frames.data[c] = (uint8_t *)block.planes[c].data();
	frames.frames = (uint32_t)block.frames;
	frames.timestamp = block.timestamp;
	st_raw_audio_camera_box(st, &frames);
}

/* issue 1381: the lock tracker forgets its state and the dock shows unlocked. A session END does
 * it (no measurement is being made, so the last offset must not read as live), so does a session
 * BEGIN (an output restart discards a session end the worker had not reached yet), and so does the
 * dead-pairing recovery. The offset cluster keeps its own window (CB_CLUSTER_WINDOW_NS): after a
 * short pause the first new markers can lock again on offsets measured before it, the same chain. */
static void cb_audio_forget_lock(struct sync_test_output *st)
{
	st->cb_lock_audit = camerabox::CbLockAuditTracker();
	if (st->cb_lock_state) {
		st->cb_lock_state = false;
		signal_lock_state_changed(st->context, false);
	}
}

/* issue 1381: a new decode session -- the gate reopened, or the output restarted. The staleness
 * and pairing watchdogs start a fresh baseline (the pause is not a dead input), the lock tracker
 * starts over, the diag line goes out on the first block, and a dock told STALE at the last session
 * end goes LIVE again. */
static void cb_audio_session_begin(struct sync_test_output *st)
{
	st->cb_input_staleness = camerabox::CbDockInputStaleness();
	st->cb_pairing_watchdog = camerabox::CbDockPairingWatchdog();
	cb_audio_forget_lock(st);
	st->cb_diag_last_log_ns = 0;
	if (st->cb_audio_paused) {
		st->cb_audio_paused = false;
		signal_stale_changed(st->context, false);
	}
	blog(LOG_INFO, "av-sync-dock: camera-box audio decode ON -- test signal fresh, measurement source present (issue 1381)");
}

/* issue 1381: the block follows a gap -- dropped blocks (the FIFO was full), or a new session. The
 * decoders must not see the two stretches of audio as contiguous, or a marker cut by the gap could
 * decode from the stitched halves: every decoder drops its window (histories and pick kept). */
void st_audio_block_gap(struct sync_test_output *st, const camerabox::CbAudioBlock &block)
{
	if (st->cb_audio_dec)
		st->cb_audio_dec->reset_window();
	if (block.gap & camerabox::CB_AUDIO_GAP_SESSION)
		cb_audio_session_begin(st);
	if ((block.gap & camerabox::CB_AUDIO_GAP_DROPPED) &&
	    (!st->cb_audio_drop_logged || block.timestamp - st->cb_audio_drop_log_ns >= CAMERA_BOX_DIAG_LOG_INTERVAL_NS)) {
		st->cb_audio_drop_logged = true;
		st->cb_audio_drop_log_ns = block.timestamp;
		blog(LOG_WARNING,
		     "av-sync-dock: audio decode fell behind -- blocks dropped, marker decoders reset "
		     "(audio_dropped=%llu decode_resets=%llu, issue 1381)",
		     (unsigned long long)st->cb_audio_fifo.dropped(), (unsigned long long)st->cb_audio_fifo.resets());
	}
}

/* issue 1381: the gate closed (`reason` is the camerabox::CbAudioGate). The decoders drop their
 * window, the lock tracker forgets its state, and the dock shows unlocked + STALE -- no measurement
 * is being made, so the last offset must not read as live. The next session starts over. */
void st_audio_session_end(struct sync_test_output *st, unsigned reason)
{
	if (st->cb_audio_dec)
		st->cb_audio_dec->reset_window();
	cb_audio_forget_lock(st);
	if (!st->cb_input_staleness.is_stale())
		signal_stale_changed(st->context, true);
	st->cb_audio_paused = true;
	const camerabox::CbDecodeStats stats = st->cb_audio_dec ? st->cb_audio_dec->stats : camerabox::CbDecodeStats();
	blog(LOG_INFO,
	     "av-sync-dock: camera-box audio decode OFF -- %s; decoders reset, dock STALE "
	     "(preambles=%llu crc_ok=%llu audio_dropped=%llu decode_resets=%llu, issue 1381)",
	     camerabox::cb_audio_gate_text((camerabox::CbAudioGate)reason),
	     (unsigned long long)stats.preamble_screens_passed, (unsigned long long)stats.crc_ok,
	     (unsigned long long)st->cb_audio_fifo.dropped(), (unsigned long long)st->cb_audio_fifo.resets());
}

/* issue 1381: once, on the audio worker before its first block. Named (15 characters, the Linux
 * limit) and left at NORMAL priority, like the video decode worker: the audio thread takes the FIFO
 * lock every callback and the worker holds it briefly, and a Windows std::mutex has no priority
 * inheritance. */
void st_audio_worker_thread_setup()
{
	os_set_thread_name("avsync-audio");
}

/* issue 1381: the audio thread's whole share of the camera-box audio decode -- the gate (a fresh
 * camera-box QR, the measurement source on this box) and a copy of every channel into the FIFO. The
 * worker decodes the copy; a closed gate ends the worker's session (decoders reset, dock STALE).
 * Bounded work: no decode, no log line, no source lookup, no signal here. */
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

void st_raw_audio(void *data, struct audio_data *frames)
{
	auto *st = (struct sync_test_output *)data;

	if (!st->start_ts)
		return;

	// #398: once the video QR has activated camera-box mode, decode the audio with camera-box's own
	// proven demod (norihiro's is broken at c=1). Skip norihiro's audio path entirely then.
	// issue 1381: that decode runs on the audio worker, and only while the test signal is fresh;
	// this thread only runs the gate and copies the block.
	bool cb_active;
	uint64_t last_qr_ns;
	{
		std::unique_lock<std::mutex> lock(st->mutex);
		cb_active = st->cb_mode_active;
		last_qr_ns = st->cb_video_last_decode_ts_ns;
	}
	if (cb_active) {
		cb_audio_gate_and_publish(st, frames, last_qr_ns);
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

} // namespace av_sync_output
