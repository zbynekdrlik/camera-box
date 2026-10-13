#pragma once

/*
 * camera-box issue 1424 (owner 13.10.2026) -- the fixed HDMI output's AUDIO leg. While the in-OBS
 * DRM output (issue 1152 lease / issue 1346 vk-direct) shows the Program, the OBS program mix
 * (mix 0, what the stream and the recording carry) is written STRAIGHT into the ALSA hw PCM of the
 * SAME connector -- "Ziadnych milion vrstiev po ceste": no sound server, no monitoring device, no
 * mixer in between, the same monolithic shape as the NDI program out. Nothing is configured per box:
 *
 *   connector EDID (X RandR "EDID", read before the backend takes the connector off X)
 *     == per-PCM ALSA ELD (control iface PCM "ELD", /dev/snd/controlC<card>)  -> hw:<card>,<device>
 *
 * The NVIDIA HDA driver publishes a connector's ELD only once a mode is driven on it, so the leg
 * opens AFTER the backend start succeeded and re-reads for a bounded window. No match, two matches
 * or a PCM that will not open (busy) = NO audio and ONE `drm-output audio: NO AUDIO on
 * '<connector>' -- <reason>` line; never another device. alsa-lib is dlopen'd (libasound.so.2), so
 * there is no link dependency. Each block is placed at its OBS timestamp + the output's measured
 * present delay (render tick -> on screen) against snd_pcm_delay of the PCM
 * (obs-drm-output-audio-match.h). The open line names the PCM, the hold and the measured offset.
 * Setup keeps the box's sound server off every HDMI/DP PCM (scripts/lib/drm-output-audio.sh).
 *
 * One leg per output instance: drm_output_audio_open() returns its own state, the raw callback's
 * param is that state, nothing is file-static except the present clock of the module's one output.
 * Linux-only, like the rest of the module.
 */

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* The present delay of one output: the render tick of each scanout buffer (set at publish) and the
 * smoothed tick -> on-screen delay (updated when a buffer reaches the screen). */
#define DRM_OUTPUT_PRESENT_SLOTS 8
struct drm_output_present_clock {
	volatile long tick_ns[DRM_OUTPUT_PRESENT_SLOTS];
	volatile long delay_ns;
};

/* The module's one output (lease or vk-direct, whichever owns it). */
extern struct drm_output_present_clock drm_output_present_clock_main;

/* Graphics thread, at publish: buffer `idx` carries the render tick obs_get_video_frame_time(). */
void drm_output_present_note_published(int idx);

/* Scanout thread, when buffer `idx` reached the screen at `shown_ns` (os_gettime_ns clock). A buffer
 * re-shown with no new publish counts nothing (its tick was consumed). */
void drm_output_present_note_shown(int idx, uint64_t shown_ns);

/* The connector's EDID: the X RandR output property "EDID" of the output named `output_name` (the
 * name `xrandr` prints, the config's "connector"). Returns the byte count, 0 when unreadable. */
size_t drm_output_audio_read_edid(const char *output_name, uint8_t *buf, size_t cap);

struct drm_output_audio;

/* Open the leg of one output, after its backend start succeeded: resolves + opens the PCM on a worker
 * thread (bounded re-read window), then plays mix 0. Never fails the output; NULL only on bad args. */
struct drm_output_audio *drm_output_audio_open(const char *connector, const uint8_t *edid, size_t edid_len,
					       struct drm_output_present_clock *clock);

/* Stop the leg: join the resolver, disconnect the mix callback (no callback in flight after it),
 * close the PCM. Safe with NULL. */
void drm_output_audio_close(struct drm_output_audio *a);

#ifdef __cplusplus
}
#endif
