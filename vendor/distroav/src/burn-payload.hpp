/******************************************************************************
	#111 — QR render-time burn payload encoder (C++ mirror of src/probe/payload.rs)

	Wire format: `P{run_id}.{frame_id}.{gen_ts_ns}.{crc32}` where crc32 is the
	CRC-32/ISO-HDLC of the body `{run_id}.{frame_id}.{gen_ts_ns}` (the SAME CRC the
	Rust `crc::CRC_32_ISO_HDLC` computes — standard reflected CRC-32, poly 0xEDB88320,
	init 0xFFFFFFFF, refin/refout true, xorout 0xFFFFFFFF). ASCII so it round-trips
	cleanly through QR text decoding and is decoded UNCHANGED by the existing
	`rqrr` reader `Payload::decode` (src/probe/payload.rs) and the recorded-file
	decoder (src/probe/recording.rs).

	This header is the single source of truth for the burned payload string. It is
	included by:
	  - ndi-burn-filter.cpp (the actual render-time burn into the recording), and
	  - the Rust byte-identity test harness (tests/burn_payload_parity.rs compiles a
	    tiny main around it and asserts byte-for-byte equality with Payload::encode).
	Keeping ONE implementation means the burned string can never drift from what the
	decoder expects.

	Header-only + freestanding (only <cstdint>/<string>), so it adds NO build
	dependency and compiles identically inside the DistroAV plugin and the test main.
******************************************************************************/

#pragma once

#include <cstdint>
#include <string>

namespace burn_payload {

// Standard reflected CRC-32 (CRC-32/ISO-HDLC == zlib/PNG CRC). Computed on the fly
// (no 256-entry table) — the body is tiny (a few dozen bytes/frame) so the bit-loop
// cost is negligible and the code stays dependency-free and obviously-correct.
// Matches Rust `crc::Crc::<u32>::new(&crc::CRC_32_ISO_HDLC).checksum(bytes)`.
inline uint32_t crc32_iso_hdlc(const std::string &data)
{
	uint32_t crc = 0xFFFFFFFFu;
	for (unsigned char byte : data) {
		crc ^= static_cast<uint32_t>(byte);
		for (int bit = 0; bit < 8; ++bit) {
			// Reflected polynomial 0xEDB88320 (== bit-reverse of 0x04C11DB7).
			crc = (crc & 1u) ? (crc >> 1) ^ 0xEDB88320u : (crc >> 1);
		}
	}
	return crc ^ 0xFFFFFFFFu;
}

// The body `{run_id}.{frame_id}.{gen_ts_ns}` (no prefix, no CRC) — the bytes the CRC
// covers. gen_ts_ns is signed (epoch nanoseconds; matches Rust i64) so it round-trips
// through Payload::decode's `i64` parse.
inline std::string body(uint32_t run_id, uint32_t frame_id, int64_t gen_ts_ns)
{
	return std::to_string(run_id) + "." + std::to_string(frame_id) + "." +
	       std::to_string(gen_ts_ns);
}

// Full wire string `P{run_id}.{frame_id}.{gen_ts_ns}.{crc32}` — byte-identical to
// Rust `Payload { run_id, frame_id, gen_ts_ns }.encode()`.
inline std::string encode(uint32_t run_id, uint32_t frame_id, int64_t gen_ts_ns)
{
	const std::string b = body(run_id, frame_id, gen_ts_ns);
	return "P" + b + "." + std::to_string(crc32_iso_hdlc(b));
}

} // namespace burn_payload
