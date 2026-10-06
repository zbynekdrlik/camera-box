/*
 * issue 1404 -- the program-audio guard's QPSK marker decode: a thin C ABI over the EXISTING dock
 * decoder. It decodes; it decides nothing.
 *
 * The stream program-audio guard (scripts/program_audio.py + program_audio_sampler.py, the dev1
 * YouTube channel guard) may only say MEASUREMENT when the cam2 QPSK marker itself is in the program
 * (design: issue 1404 comment 6026235697; ROZHODNUTÉ 6026577906). This file adds NO decoder of its
 * own: it calls `cb_scan_markers` from vendor/av-sync-dock/src/camera-box-marker-scan.hpp -- the
 * byte-for-byte port of qpsk_marker::scan_markers that the live dock runs and that
 * test/camera-box-selftest.cpp and tests/qpsk_channel_pick_parity_1367.rs pin to the Rust -- with the
 * dock's own constants from camera-box-audio.hpp (carrier 442 Hz, c = 1, preamble-screen threshold
 * 0.35; the same as AudioParams::rig60(), DOCK_QPSK_THRESHOLD and the `# qpsk-params sr=48000
 * carrier=442 c=1` line of the painter's marker log).
 *
 * One call decodes ONE channel of an interleaved buffer and returns every CRC-valid word (preamble
 * 0xF, zero nibble, CRC-4) as (start sample, 8-bit index). A raw CRC-valid word is NOT yet a marker:
 * in-band tonal audio yields runs of them (issue 1404 Design-question 6026559236), so the rule that
 * makes a decode a marker -- the timecode chain -- lives in scripts/program_audio.py, next to the
 * classifier's other constants. The caller decodes every channel on its own, never a downmix: the
 * measurement track carries the marker on L and R ~10 ms apart and their sum comb-filters it.
 *
 * Built by scripts/build-qpsk-guard-shim.sh (g++ -O2 -fPIC -shared, no cargo), loaded by
 * scripts/program_audio_marker.py through ctypes. Errors never cross the C ABI: a bad argument
 * returns QPSK_GUARD_EBADARG, any C++ exception QPSK_GUARD_EINTERNAL; the Python side reads every
 * negative return as "no decode" = UNKNOWN.
 */

#include "camera-box-audio.hpp"

#include <cstdint>
#include <vector>

#ifndef QPSK_GUARD_SOURCE_SHA256
#define QPSK_GUARD_SOURCE_SHA256 "unknown"
#endif

namespace {

const int QPSK_GUARD_ABI = 2;
const int QPSK_GUARD_EBADARG = -1;
const int QPSK_GUARD_EINTERNAL = -2;
/* An NDI program output is stereo; anything past this is not an audio window this guard reads. */
const int QPSK_GUARD_MAX_CHANNELS = 64;

int decode_channel(const float *interleaved, int frames, int channels, int channel, int sample_rate,
		   int64_t *starts, uint8_t *indices, int cap)
{
	const size_t n = (size_t)frames;
	const size_t ch = (size_t)channels;
	std::vector<float> mono(n);
	for (size_t i = 0; i < n; i++)
		mono[i] = interleaved[i * ch + (size_t)channel];
	camerabox::CbMarkerScan scan;
	camerabox::cb_scan_markers(mono.data(), n, (uint32_t)sample_rate, camerabox::CB_AUDIO_CARRIER_HZ,
				   camerabox::CB_AUDIO_C, camerabox::CB_QPSK_THRESHOLD, 0, nullptr, scan);
	const size_t found = scan.markers.size();
	for (size_t k = 0; k < found && k < (size_t)cap; k++) {
		starts[k] = (int64_t)scan.markers[k].first;
		indices[k] = scan.markers[k].second;
	}
	return (int)found;
}

} // namespace

extern "C" {

/* The ABI revision of this file; the loader refuses any other. */
int qpsk_guard_abi(void)
{
	return QPSK_GUARD_ABI;
}

/* The decoder parameters compiled in, for the loader's log line and the parity test. */
void qpsk_guard_params(uint32_t *rig_sample_rate, uint32_t *carrier_hz, uint32_t *c, double *threshold)
{
	if (rig_sample_rate)
		*rig_sample_rate = camerabox::CB_AUDIO_SAMPLE_RATE;
	if (carrier_hz)
		*carrier_hz = camerabox::CB_AUDIO_CARRIER_HZ;
	if (c)
		*c = camerabox::CB_AUDIO_C;
	if (threshold)
		*threshold = camerabox::CB_QPSK_THRESHOLD;
}

/* sha256 of the decoder sources this build was compiled from (build-qpsk-guard-shim.sh), so the
 * sampler can tell a library built from other sources. */
const char *qpsk_guard_source_sha256(void)
{
	return QPSK_GUARD_SOURCE_SHA256;
}

/* Decode channel `channel` of `frames` interleaved float samples of `channels` channels at
 * `sample_rate`. Writes up to `cap` CRC-valid words as (start sample, index) into `starts` /
 * `indices` in time order and returns how many were found -- more than `cap` means the caller's
 * arrays were too small and it calls again with room for all. < 0 on an error. */
int qpsk_guard_decode_channel(const float *interleaved, int frames, int channels, int channel,
			      int sample_rate, int64_t *starts, uint8_t *indices, int cap)
{
	if (interleaved == nullptr || frames < 0 || channels < 1 || channels > QPSK_GUARD_MAX_CHANNELS ||
	    channel < 0 || channel >= channels || sample_rate <= 0 || cap < 0 ||
	    (cap > 0 && (starts == nullptr || indices == nullptr)))
		return QPSK_GUARD_EBADARG;
	try {
		return decode_channel(interleaved, frames, channels, channel, sample_rate, starts, indices, cap);
	} catch (...) {
		return QPSK_GUARD_EINTERNAL;
	}
}

} // extern "C"
