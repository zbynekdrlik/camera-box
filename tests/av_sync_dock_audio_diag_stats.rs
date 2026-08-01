//! #690 — live audio/video decode diagnostics for the A/V-sync dock.
//!
//! The camera-box audio path (`st_raw_audio_camera_box` -> `StreamingMarkerDecoder::push` ->
//! `cb_decode_markers`) used to emit ZERO diagnostic signal — not even the legacy norihiro path's
//! LOG_DEBUG CRC-mismatch line, which isn't even reached once camera-box mode is active. A live
//! session with `Audio Frequency` detected but `Audio Index`/`Latency` stuck on dashes had no way
//! to tell apart "the demod sees nothing" from "sees candidates but they're garbage" from "decodes
//! fine but never ring-hits/clusters". Fix: `DecodeStats` (pure, Tier-0 tested in
//! `src/qpsk_marker.rs` / mirrored in `camera-box-audio.hpp`, cross-checked by
//! `av_sync_dock_cpp_mirror_gate.rs`'s self-test) plus ring-hit/miss + video-frame counters and a
//! rate-limited INFO blog() line in `sync-test-output.cpp`.
//!
//! This test covers ONLY the OBS-glue half (`sync-test-output.cpp`, which pulls in libobs/quirc
//! and is NOT compiled by this repo's Linux CI — see `av_sync_dock_autostart_guard.rs`'s own doc
//! comment for why) — the pure decode-stats logic itself is covered by
//! `av_sync_dock_cpp_mirror_gate.rs` (compiles+runs the self-test) and this crate's own
//! `qpsk_marker`/`av_sync_dock` unit tests. Source-presence guard, same convention as
//! `av_sync_dock_qr_patch_guard.rs` / `av_sync_dock_autostart_guard.rs`.

use std::path::PathBuf;

const DOCK_OUTPUT: &str = "vendor/av-sync-dock/src/sync-test-output.cpp";

fn vendor_file(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn diag_counters_are_declared_and_updated() {
    let src = squish(&vendor_file(DOCK_OUTPUT));
    for marker in [
        "std::atomic<uint64_t> cb_video_frames_seen{0};",
        "std::atomic<uint64_t> cb_video_frames_decoded{0};",
        "uint64_t cb_ring_hits = 0;",
        "uint64_t cb_ring_misses = 0;",
        "st->cb_video_frames_seen.fetch_add(1, std::memory_order_relaxed);",
        "st->cb_video_frames_decoded.fetch_add(1, std::memory_order_relaxed);",
        "st->cb_ring_misses++;",
        "st->cb_ring_hits++;",
    ] {
        assert!(
            src.contains(marker),
            "{DOCK_OUTPUT}: #690 marker `{marker}` is gone — the live audio/video decode \
             diagnostic counters regressed. Re-apply."
        );
    }
}

#[test]
fn periodic_diagnostic_log_line_is_present_and_rate_limited() {
    let src = squish(&vendor_file(DOCK_OUTPUT));
    for marker in [
        "CAMERA_BOX_DIAG_LOG_INTERVAL_NS",
        "av-sync-dock: diag video_frames=",
        "preambles=%llu crc_ok=%llu crc_fail=%llu",
        "st->cb_audio_dec->stats.preamble_screens_passed",
    ] {
        assert!(
            src.contains(marker),
            "{DOCK_OUTPUT}: #690 marker `{marker}` is gone — the rate-limited live audio/video \
             decode diagnostic log regressed. Without it, a live session with `Audio Frequency` \
             detected but `Audio Index`/`Latency` stuck on dashes has no way to tell whether the \
             demod sees nothing, decodes garbage, or decodes fine but never locks. Re-apply."
        );
    }
}
