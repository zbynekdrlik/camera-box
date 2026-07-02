#pragma once

/*
 * camera-box dual-QR payload decode (#398 — Option A dock adaptation).
 *
 * The camera-box rig does NOT paint norihiro's `q=,i=,f=,c=` sync-pattern QR (that would risk the
 * guarded zero-loss dual-QR Vernier gate the rig depends on — see the #398 plan's Decision 1). It
 * paints ITS OWN dual-QR carrying frame identity: `P{run_id}.{frame_id}.{gen_ts_ns}.{crc32}`,
 * crc32 = the standard reflected CRC-32 (aka CRC-32/ISO-HDLC, poly 0xEDB88320, init/xorout
 * 0xFFFFFFFF — same algorithm as zlib/gzip/PKZIP) of the body `{run_id}.{frame_id}.{gen_ts_ns}`.
 * This header MIRRORS `src/probe/payload.rs::Payload` exactly — keep both in sync if the wire
 * format ever changes.
 *
 * Golden vectors (independently cross-checked via `python3 -c "import zlib; zlib.crc32(b'...')"`,
 * the same standard CRC-32 algorithm as Rust's `crc::CRC_32_ISO_HDLC`):
 *   body "42.9001.1234567890" -> crc32 0x4DAE4DF9 (1303268857)
 *   body "0.0.0"              -> crc32 0x2E5B4B86 (777735046)
 *   body "1.2.3"              -> crc32 0x89B6E7E2 (2310465506)
 *
 * The AUDIO marker is unchanged norihiro-compatible QPSK (by design, so this dock's EXISTING
 * `st_raw_audio_decode_data` demod applies to our marker with no code change) — but our audio
 * params (carrier/cycles-per-symbol/cadence) are FIXED by the rig, never transmitted in a QR (our
 * QR carries frame identity, not audio params). See CAMERA_BOX_AUDIO_* below; must match
 * `AudioParams::rig60()` in `src/qpsk_marker.rs`.
 */

#include <cstdint>
#include <cstdlib>
#include <cstring>

/* Fixed audio-marker params for the camera-box rig (60 fps, cam2 HDMI emitter). Must match
 * `AudioParams::rig60()`: carrier 442 Hz, c = auto_c(q=2, f=442, vr=60/1) = 1, q_ms = 2*1000/60 = 33. */
#define CAMERA_BOX_AUDIO_F_HZ 442u
#define CAMERA_BOX_AUDIO_C 1u
#define CAMERA_BOX_AUDIO_Q_MS 33u

struct CameraBoxQrData
{
	uint32_t run_id = 0;
	uint32_t frame_id = 0;
	int64_t gen_ts_ns = 0;
	bool valid = false;
};

/* Standard reflected CRC-32 (CRC-32/ISO-HDLC): poly 0xEDB88320, init 0xFFFFFFFF, xorout
 * 0xFFFFFFFF. Bit-at-a-time (no table) — this runs once per decoded QR frame, not per audio
 * sample, so the table-free cost is negligible. */
inline uint32_t camera_box_crc32(const char *data, size_t len)
{
	uint32_t crc = 0xFFFFFFFFu;
	for (size_t i = 0; i < len; i++) {
		crc ^= (uint8_t)data[i];
		for (int b = 0; b < 8; b++) {
			uint32_t mask = (uint32_t)(-(int32_t)(crc & 1u));
			crc = (crc >> 1) ^ (0xEDB88320u & mask);
		}
	}
	return crc ^ 0xFFFFFFFFu;
}

/* Decode `payload` (the QR text, NUL-terminated) as our dual-QR format. Returns false (and leaves
 * `*out` untouched beyond `valid=false`) on ANY malformed input or CRC mismatch, so the caller can
 * safely fall through to norihiro's own `st_qr_data::decode()` for the phone-based sync-pattern
 * method — both formats are tried, never assumed. `payload` is NOT modified (unlike
 * `st_qr_data::decode`, which mutates its argument via strtok). */
inline bool decode_camera_box_qr(const char *payload, CameraBoxQrData *out)
{
	out->valid = false;
	if (!payload || payload[0] != 'P')
		return false;

	const char *body_start = payload + 1;
	const char *last_dot = strrchr(body_start, '.');
	if (!last_dot || last_dot == body_start)
		return false;

	size_t body_len = (size_t)(last_dot - body_start);
	char body[128];
	if (body_len == 0 || body_len >= sizeof(body))
		return false;
	memcpy(body, body_start, body_len);
	body[body_len] = 0;

	/* strtoul/strtoll on an EMPTY token return 0 with endptr==tok (no digits consumed) — checking
	 * only `*endptr == 0` would wrongly accept an empty field as "0" (Rust's `"".parse()` rejects
	 * it). Require endptr to have advanced past `tok` AND landed exactly on the terminator. */
	char *endptr = nullptr;
	unsigned long crc_parsed = strtoul(last_dot + 1, &endptr, 10);
	if (endptr == last_dot + 1 || *endptr != 0)
		return false;

	uint32_t crc = camera_box_crc32(body, body_len);
	if ((uint32_t)crc_parsed != crc)
		return false;

	char *save = nullptr;
	char *tok = strtok_r(body, ".", &save);
	/* run_id/frame_id are unsigned in the Rust wire format; reject a leading '-' (strtoul would
	 * otherwise silently wrap it into a huge value instead of failing like Rust's u32::parse). */
	if (!tok || tok[0] == '-')
		return false;
	unsigned long run_id = strtoul(tok, &endptr, 10);
	if (endptr == tok || *endptr != 0)
		return false;

	tok = strtok_r(nullptr, ".", &save);
	if (!tok || tok[0] == '-')
		return false;
	unsigned long frame_id = strtoul(tok, &endptr, 10);
	if (endptr == tok || *endptr != 0)
		return false;

	tok = strtok_r(nullptr, ".", &save);
	if (!tok)
		return false;
	long long gen_ts_ns = strtoll(tok, &endptr, 10);
	if (endptr == tok || *endptr != 0)
		return false;

	/* Exactly 3 body fields — a 4th token means the body had an extra dot (malformed). */
	if (strtok_r(nullptr, ".", &save))
		return false;

	out->run_id = (uint32_t)run_id;
	out->frame_id = (uint32_t)frame_id;
	out->gen_ts_ns = (int64_t)gen_ts_ns;
	out->valid = true;
	return true;
}
