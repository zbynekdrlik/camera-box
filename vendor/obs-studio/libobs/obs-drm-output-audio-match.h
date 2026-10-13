#pragma once

/*
 * camera-box issue 1424 -- the PURE decisions of the fixed HDMI output's audio leg (owner 13.10.2026:
 * the output plays the OBS program mix on the SAME connector's HDMI audio by itself, straight into
 * that connector's ALSA hw PCM -- no PipeWire, no monitoring device, no extra mixer, never a per-box
 * setting; no match = no audio + one named line).
 *
 * Header-only, std-only C (no libobs types), so a test lifts it by #include and compiles it for real.
 * The Rust twin is src/drm_output_audio_match.rs; tests/drm_output_audio_1424.rs runs both over the
 * same vectors (the strih-pp fixtures) and requires identical answers.
 *
 *   connector EDID  --drm_output_edid_monitor_id-->  monitor id (vendor bytes + name)
 *   per-PCM ELD     --drm_output_eld_monitor_id--->  monitor id of the monitor on that PCM
 *   drm_output_audio_pick_pcm      which ALSA hw PCM (card, device) carries THIS connector's monitor
 *   drm_output_audio_iec958_index  which "IEC958 Playback Switch" of the card belongs to that PCM
 *   drm_output_audio_plan_block    how to place one mix block against the picture (hold / trim / offset)
 *
 * The vendor bytes: EDID bytes 8..11 are the manufacturer PNP id (big-endian, e.g. 09 d1 = BenQ) and
 * the product code (little-endian, 24 80 = 0x8024). The HDA ELD carries THE SAME FOUR BYTES at offsets
 * 16..19; the kernel's /proc eld file prints them as little-endian reads (manufacture_id 0xd109,
 * product_id 0x8024). Comparing the raw bytes needs no byte swap at all.
 */

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>

#define DRM_OUTPUT_AUDIO_NAME_MAX 16
#define DRM_OUTPUT_ELD_FIXED_BYTES 20

struct drm_output_monitor_id {
	bool valid;
	uint8_t vendor[4]; /* manufacturer (2) + product (2), in EDID byte order == ELD byte order */
	char name[DRM_OUTPUT_AUDIO_NAME_MAX + 1];
};

/* Results. MATCH is the only one that plays; every other one is a named reason for NO audio. */
#define DRM_OUTPUT_AUDIO_MATCH 0
#define DRM_OUTPUT_AUDIO_NO_EDID 1
#define DRM_OUTPUT_AUDIO_NO_MONITOR 2
#define DRM_OUTPUT_AUDIO_NO_MATCH 3
#define DRM_OUTPUT_AUDIO_AMBIGUOUS 4

/* Copy a monitor name: at most n bytes, ends at NUL or LF (the EDID terminator), trailing spaces
 * (the EDID padding) dropped, non-printable bytes replaced by '?' so the log line stays one line. */
static inline void drm_output_audio_copy_name(char *dst, const uint8_t *src, size_t n)
{
	size_t len = 0;
	if (n > DRM_OUTPUT_AUDIO_NAME_MAX)
		n = DRM_OUTPUT_AUDIO_NAME_MAX;
	while (len < n && src[len] != 0 && src[len] != 0x0A) {
		uint8_t c = src[len];
		dst[len] = (c >= 0x20 && c < 0x7F) ? (char)c : '?';
		len++;
	}
	while (len > 0 && dst[len - 1] == ' ')
		len--;
	dst[len] = '\0';
}

/* The connector's monitor from its EDID base block: the header, the block checksum, the vendor bytes
 * and the display-name descriptor (tag 0xFC) if there is one. false = not a valid EDID. */
static inline bool drm_output_edid_monitor_id(const uint8_t *edid, size_t len, struct drm_output_monitor_id *out)
{
	static const uint8_t header[8] = {0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00};
	memset(out, 0, sizeof(*out));
	if (!edid || len < 128 || memcmp(edid, header, sizeof(header)) != 0)
		return false;
	uint8_t sum = 0;
	for (size_t i = 0; i < 128; i++)
		sum = (uint8_t)(sum + edid[i]);
	if (sum != 0)
		return false;
	memcpy(out->vendor, edid + 8, 4);
	for (size_t d = 54; d + 18 <= 126; d += 18) {
		const uint8_t *p = edid + d;
		if (p[0] == 0 && p[1] == 0 && p[2] == 0 && p[3] == 0xFC) {
			drm_output_audio_copy_name(out->name, p + 5, 13);
			break;
		}
	}
	out->valid = true;
	return true;
}

/* The monitor on one HDMI/DP PCM from its raw ELD (the per-PCM ALSA control `ELD`): an empty ELD (no
 * monitor, or no mode driven on the connector yet) or anything but a full CEA-861-D ELD is false. */
static inline bool drm_output_eld_monitor_id(const uint8_t *eld, size_t len, struct drm_output_monitor_id *out)
{
	memset(out, 0, sizeof(*out));
	if (!eld || len < DRM_OUTPUT_ELD_FIXED_BYTES || (eld[0] >> 3) != 2)
		return false;
	size_t mnl = (size_t)(eld[4] & 0x1F);
	if (mnl > DRM_OUTPUT_AUDIO_NAME_MAX || DRM_OUTPUT_ELD_FIXED_BYTES + mnl > len)
		return false;
	memcpy(out->vendor, eld + 16, 4);
	drm_output_audio_copy_name(out->name, eld + DRM_OUTPUT_ELD_FIXED_BYTES, mnl);
	out->valid = true;
	return true;
}

struct drm_output_audio_pcm {
	int card;
	int device;
	struct drm_output_monitor_id id;
};

/* Which PCM carries the connector's monitor. The vendor bytes decide; the monitor name only breaks a
 * tie between PCMs with the same vendor bytes (it never matches on its own). Two PCMs that are still
 * equal (two identical monitors) = AMBIGUOUS: no guess. */
static inline int drm_output_audio_pick_pcm(const struct drm_output_monitor_id *want,
					    const struct drm_output_audio_pcm *pcms, size_t n, size_t *out_idx)
{
	if (!want || !want->valid)
		return DRM_OUTPUT_AUDIO_NO_EDID;
	size_t present = 0, hits = 0, hit = 0, named = 0, named_hit = 0;
	for (size_t i = 0; i < n; i++) {
		if (!pcms[i].id.valid)
			continue;
		present++;
		if (memcmp(pcms[i].id.vendor, want->vendor, 4) != 0)
			continue;
		hits++;
		hit = i;
		if (want->name[0] != '\0' && strcmp(pcms[i].id.name, want->name) == 0) {
			named++;
			named_hit = i;
		}
	}
	if (present == 0)
		return DRM_OUTPUT_AUDIO_NO_MONITOR;
	if (hits == 0)
		return DRM_OUTPUT_AUDIO_NO_MATCH;
	if (hits == 1) {
		*out_idx = hit;
		return DRM_OUTPUT_AUDIO_MATCH;
	}
	if (named == 1) {
		*out_idx = named_hit;
		return DRM_OUTPUT_AUDIO_MATCH;
	}
	return DRM_OUTPUT_AUDIO_AMBIGUOUS;
}

/* The HDA codec driver creates one "IEC958 Playback Switch" per HDMI/DP PCM, in PCM order, with
 * index 0, 1, 2, ... (strih-snv card0: PCMs 3/7/8/9 <-> switch indexes 0/1/2/3). So the switch of
 * PCM `device` is its rank among the card's ELD-carrying PCM devices; -1 when `device` is not one
 * of them. Duplicates in `devices` never count twice. */
static inline int drm_output_audio_iec958_index(const int *devices, size_t n, int device)
{
	int rank = 0;
	bool found = false;
	for (size_t i = 0; i < n; i++) {
		bool dup = false;
		for (size_t j = 0; j < i; j++)
			dup = dup || devices[j] == devices[i];
		if (dup)
			continue;
		if (devices[i] == device)
			found = true;
		else if (devices[i] < device)
			rank++;
	}
	return found ? rank : -1;
}

/* Whether a failed resolve is worth another read inside the bounded window: the NVIDIA HDA driver
 * publishes a connector's ELD only once a mode is driven on it. A missing EDID never comes back by
 * waiting. */
static inline bool drm_output_audio_retryable(int result)
{
	return result != DRM_OUTPUT_AUDIO_MATCH && result != DRM_OUTPUT_AUDIO_NO_EDID;
}

static inline const char *drm_output_audio_reason(int result)
{
	switch (result) {
	case DRM_OUTPUT_AUDIO_MATCH:
		return "matched";
	case DRM_OUTPUT_AUDIO_NO_EDID:
		return "the connector has no readable EDID";
	case DRM_OUTPUT_AUDIO_NO_MONITOR:
		return "no HDMI/DP PCM reports a monitor (no ELD)";
	case DRM_OUTPUT_AUDIO_NO_MATCH:
		return "no PCM's ELD matches the connector's EDID";
	case DRM_OUTPUT_AUDIO_AMBIGUOUS:
		return "two PCMs match the connector's EDID (identical monitors)";
	default:
		return "unknown";
	}
}

/* Placing one mix block. The block's first sample belongs to the picture of OBS time block_ts, which
 * reaches the screen present_delay later; written now it plays after stream_latency (snd_pcm_delay of
 * the hw PCM). It must never play sooner than now + floor (the smallest safe buffer). The hold
 * is (the wanted play time, never under the floor) - (the natural play time):
 *   hold >  threshold  -> write hold of silence first (the audio is early: wait for the picture)
 *   hold < -threshold  -> trim min(-hold, block) off the block's front (the buffer grew past the floor)
 *   else               -> write it as it is.
 * offset = when the first KEPT sample plays - when its picture shows (+ = audio behind the picture). */
struct drm_output_audio_plan {
	uint64_t silence_ns;
	uint64_t drop_ns;
	int64_t offset_ns;
};

static inline struct drm_output_audio_plan drm_output_audio_plan_block(uint64_t block_ts_ns, uint64_t present_delay_ns,
								       uint64_t now_ns, uint64_t stream_latency_ns,
								       uint64_t floor_ns, uint64_t threshold_ns,
								       uint64_t block_ns)
{
	struct drm_output_audio_plan plan = {0, 0, 0};
	const int64_t want = (int64_t)(block_ts_ns + present_delay_ns);
	const int64_t natural = (int64_t)(now_ns + stream_latency_ns);
	const int64_t safe = (int64_t)(now_ns + floor_ns);
	const int64_t desired = want > safe ? want : safe;
	const int64_t hold = desired - natural;
	const int64_t thr = (int64_t)threshold_ns;
	if (hold > thr) {
		plan.silence_ns = (uint64_t)hold;
	} else if (-hold > thr) {
		const int64_t blk = (int64_t)block_ns;
		plan.drop_ns = (uint64_t)(-hold < blk ? -hold : blk);
	}
	plan.offset_ns = natural + (int64_t)plan.silence_ns - want - (int64_t)plan.drop_ns;
	return plan;
}

/* The output's present delay (render tick -> on screen), smoothed: the first sample seeds it, later
 * ones move it 1/8 of the way. A sample outside (0, 1 s) is a stale or crossed record: ignored. */
static inline uint64_t drm_output_present_ewma(uint64_t prev_ns, uint64_t sample_ns)
{
	if (sample_ns == 0 || sample_ns >= 1000000000ULL)
		return prev_ns;
	if (prev_ns == 0)
		return sample_ns;
	return prev_ns - prev_ns / 8 + sample_ns / 8;
}
