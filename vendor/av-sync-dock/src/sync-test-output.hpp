#pragma once

#include <obs-module.h>

struct st_qr_data
{
	uint32_t f = 0;
	uint32_t c = 0;
	uint32_t q_ms = 0;
	uint32_t index = -1;
	uint32_t index_max = 256;
	uint32_t type_flags = 0;
	bool valid = 0;

	bool _decode_kv(char *param)
	{
		char *saveptr;
		char *key = strtok_r(param, "=", &saveptr);
		if (!key || key[1] != 0)
			return false;

		char *val = strtok_r(NULL, "=", &saveptr);
		if (!val)
			return false;

		switch (key[0]) {
		case 'f':
			f = (uint32_t)atoi(val);
			return true;
		case 'c':
			c = (uint32_t)atoi(val);
			return true;
		case 'q':
			q_ms = (uint32_t)atoi(val);
			return true;
		case 'i':
			index = (uint32_t)atoi(val);
			return true;
		case 'I':
			index_max = (uint32_t)atoi(val);
			return true;
		case 't':
			type_flags = (uint32_t)atoi(val);
			return true;
		default:
			/* Ignored */
			return true;
		}

		return false;
	}

	bool check()
	{
		if (f < 10 || 32000 < f) {
			blog(LOG_WARNING, "f: out of range: %u", f);
			return false;
		}
		if (c < 1 || f < c) {
			blog(LOG_WARNING, "c: out of range: %u", c);
			return false;
		}
		if (q_ms < 1 || 1000 < q_ms) {
			blog(LOG_WARNING, "q: out of range: %u", q_ms);
			return false;
		}
		if (index & ~0xFF) {
			blog(LOG_WARNING, "i: out of range: %u", index);
			return false;
		}
		return true;
	}

	bool decode(char *payload)
	{
		valid = false;
		char *saveptr;
		char *param = strtok_r(payload, ",", &saveptr);
		while (param) {
			if (!_decode_kv(param))
				return false;
			param = strtok_r(NULL, ",", &saveptr);
		}
		if (!check())
			return false;
		valid = true;
		return true;
	}
};

struct video_marker_found_s
{
	uint64_t timestamp;
	float score;
	struct st_qr_data qr_data;
};

struct audio_marker_found_s
{
	uint64_t timestamp;
	int index;
	float score;
	uint32_t index_max;
	/* #398: camera-box's audio index is the frame_id LOW BYTE, sampled sparsely (~one marker every
	 * few seconds), NOT a +1-per-marker counter — so the `missed_markers()` percentage is
	 * meaningless for it. When true, the dock shows the locked index alone (no bogus missed%). */
	bool sparse_index = false;
};

struct sync_index
{
	int index = -1;
	uint64_t video_ts = 0;
	uint64_t audio_ts = 0;
	uint32_t index_max = 256;
	/* #999: true when this sync_found event came from camera-box's OWN direct ring lookup
	 * (st_raw_audio_camera_box), whose ts = audio_ts - video_ts is in DOCK-NATIVE convention --
	 * the SAME quantity cb_dock_lock_display_offset_ms() already negates into GATE convention
	 * (video_time - audio_time) for every OBS-log line since #953. false for norihiro's own
	 * legacy sync_index_found()-produced events (the vestigial phone-based method, mutually
	 * exclusive with camera-box mode -- see st_raw_video/st_raw_audio's cb_active gating), whose
	 * native convention this flag leaves untouched. Mirrors audio_marker_found_s::sparse_index's
	 * exact purpose: tell the dock UI handler which regime produced a given calldata event. */
	bool gate_convention = false;
};
