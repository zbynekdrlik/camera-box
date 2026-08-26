---
paths:
  - "src/qpsk_marker.rs"
  - "vendor/av-sync-dock/src/camera-box-audio.hpp"
  - "vendor/av-sync-dock/test/camera-box-selftest.cpp"
---

# QPSK A/V-sync marker demod — the word's full redundancy + Tier-0 verification of a gate change

## The 20-bit marker word carries 12 bits of redundancy over an 8-bit index — gate ALL of it (#1153)
`payload_word(index) = (0xF000 | index) << 4 | crc4(...)`. Bit layout (verify with a rustc scratch, the
docstring at `payload_word` is imprecise): **preamble nibble bits[19:16]=0xF (symbols 0,1), ZERO nibble
bits[15:12]=0 (symbols 2,3), index bits[11:4] (`(word>>4)&0xFF`), CRC-4 bits[3:0]**. Every valid emitted
marker has the zero nibble == 0 by construction.
- The accept gate MUST use ALL the redundancy: `(word>>16)&0xF==0xF && (word>>12)&0xF==0 && crc4_check(word,20)==0`.
  Before #1153 it checked only preamble + CRC (8 bits), leaving the zero nibble unchecked → of the 4096
  words that pass preamble+CRC, only **256 are valid vs 3840 "poison"** (nonzero zero-nibble) that a music
  mix decodes from noise → a **16× false-positive flood** that drowns the offset cluster (live dock:
  matched only ~26, mad ~30ms). The gate lives in ONE Rust kernel (`decode_markers_with_stats`, feeding
  BOTH offline `recording-verdict --av-sync` AND the live-dock `StreamingMarkerDecoder`) mirrored
  byte-for-byte into `cb_decode_markers_with_stats`. Change BOTH in lockstep.
- **"98.7% CRC fail" is inherent CRC-4 physics, NOT a bug** — a 4-bit CRC passes ~1/16 of preamble-screened
  noise, so a high crc_fail rate is expected and can never go >50% on a music mix. The reliability metric is
  the CLUSTER (matched size / mad / offset stability), never the crc_ok/crc_fail ratio (a stronger gate
  correctly LOWERS that ratio by moving false decodes into crc_fail). Don't tune to the ratio.

## Reading the live dock's own decode health (diagnosis before touching code)
The deployed stream-box dock logs a ~10s diag line — read it via win-stream-snv MCP, not ssh:
`Select-String "$env:APPDATA\obs-studio\logs\<latest>.txt" -Pattern 'av-sync-dock: (diag|LOCKED|UPDATED)'`.
`preambles == crc_ok + crc_fail` (screened candidates); `ring_hit` is NOT a false-positive filter (the audio
index is only the 8-bit frame_id low byte, which cycles every ~4.3s, so any index matches some recent frame)
→ the CLUSTER is the only real discriminator. `locked=yes` + crc_ok flowing ⇒ audio level is fine (rules out
the #689 silence/clipping class), so a weak matched/mad is a discrimination problem, not a level one.

## Tier-0 verification of a demod / parity-kernel change (cargo is BLOCKED, incl. --no-run per #557)
No local cargo compiles the probe/vendor code. The working proof chain, all local:
1. **rustc scratch** — copy the pure decode kernel + emitter into a standalone `.rs`, add OLD/NEW gate
   variants, render the case (a synthesized word) + a real marker + noise, `rustc -O` it and run. Proves
   RED→GREEN (OLD accepts / NEW rejects) and real-marker preservation without cargo. Enumerate the accept
   space exhaustively here too to QUANTIFY the change (e.g. 4096→256 = 16×), never estimate it.
2. **Direct g++ self-test** — the C++ mirror's self-test is dependency-free STL; compile it DIRECTLY (this is
   NOT cargo, so Tier-0 allows it): `g++ -std=c++17 -Wall -Wextra -Werror -o /tmp/st
   vendor/av-sync-dock/test/camera-box-selftest.cpp && /tmp/st` → `ALL PASS`. Add a CHECK for your new gate
   case; the "all 256 indices round-trip" case proves no real marker is dropped. This is the SAME proof the
   `av_sync_dock_cpp_mirror_gate` CI job runs.
3. **`cargo fmt --all --check`** (non-compiling, Tier-0-allowed) — proves the Rust (incl. probe-gated + new
   test files) parses / is brace-balanced. CI is the FIRST place the Rust actually type-checks + runs.
To render an ARBITRARY (non-marker) word in a test, both sides expose a pure `marker_signal_for_word(word,p)`
(Rust) / `marker_signal_from_word(word)` (C++ self-test); `marker_signal(index)` delegates to it.
