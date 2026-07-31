/******************************************************************************
    Copyright (C) 2023 by Lain Bailey <lain@obsproject.com>

    This program is free software: you can redistribute it and/or modify
    it under the terms of the GNU General Public License as published by
    the Free Software Foundation, either version 2 of the License, or
    (at your option) any later version.

    This program is distributed in the hope that it will be useful,
    but WITHOUT ANY WARRANTY; without even the implied warranty of
    MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
    GNU General Public License for more details.

    You should have received a copy of the GNU General Public License
    along with this program.  If not, see <http://www.gnu.org/licenses/>.
******************************************************************************/

#pragma once

#include "../util/c99defs.h"
#include "audio-io.h"

#ifdef __cplusplus
extern "C" {
#endif

struct audio_resampler;
typedef struct audio_resampler audio_resampler_t;

struct resample_info {
	uint32_t samples_per_sec;
	enum audio_format format;
	enum speaker_layout speakers;
};

EXPORT audio_resampler_t *audio_resampler_create(const struct resample_info *dst, const struct resample_info *src);
EXPORT void audio_resampler_destroy(audio_resampler_t *resampler);

EXPORT bool audio_resampler_resample(audio_resampler_t *resampler, uint8_t *output[], uint32_t *out_frames,
				     uint64_t *ts_offset, const uint8_t *const input[], uint32_t in_frames);

/* camera-box #803: soft (click-free) resample-ratio nudge for continuous per-source ASRC --
 * a thin wrapper over libswresample's swr_set_compensation() so callers (obs-source.c's
 * process_audio()) never touch swresample directly, matching how audio_resampler already
 * layers over it for the plain resample path. `ppm` is the desired steady-state rate offset
 * (positive = stretch the output longer / effectively slow the source down to match master);
 * `distance_ms` is the window (in output milliseconds) the correction is spread across --
 * call this again before the window elapses to keep the compensation continuously applied
 * (swr_set_compensation replaces any still-pending compensation on each call). A no-op if
 * `resampler` is NULL or has no swresample context (e.g. audio_resampler_create() failed). */
EXPORT void audio_resampler_set_compensation_ppm(audio_resampler_t *resampler, double ppm, uint32_t distance_ms);

#ifdef __cplusplus
}
#endif
