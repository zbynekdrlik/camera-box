//! #398 (Option A) — the ONLY intentional camera-box patch on the vendored `av-sync-dock`.
//!
//! Every other file under `vendor/av-sync-dock/` is stock norihiro `obs-audio-video-sync-dock`
//! (Phase 1, #392, built as-is against the genlock OBS SDK). The plan's own §1.3D said explicitly
//! "no dock-side patch — if one is ever added, add the lock-step Linux-guard + pwsh-assert pair
//! (the #269 pattern)" — Option A (the user's 2026-07-01-evening decision) IS that patch: the
//! dock's video reader now decodes the rig's OWN dual-QR (`P{run_id}.{frame_id}.{gen_ts_ns}.
//! {crc32}`, see src/probe/payload.rs) instead of norihiro's `q=,i=,f=,c=` QR, so the guarded
//! zero-loss dual-QR gate stays untouched. This is a SOURCE-presence guard (same convention as
//! tests/genlock_release_cadence.rs / tests/distroav_genlock_lockdown.rs): runs on default
//! features, reads the vendored C++ as text, fails loudly if a future `git subtree pull` of
//! av-sync-dock silently drops the patch — the dock would then silently revert to phone-only
//! sync with no live cam2 reading, and nothing else would catch it (the compile gate still
//! builds fine; only behavior regresses).

use std::path::PathBuf;

const DOCK_OUTPUT: &str = "vendor/av-sync-dock/src/sync-test-output.cpp";
const DOCK_QR_HEADER: &str = "vendor/av-sync-dock/src/camera-box-qr.hpp";

fn vendor_file(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Collapse every run of ASCII whitespace to a single space so the assertions survive
/// reformatting (e.g. a future clang-format pass re-wrapping a line).
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn camera_box_qr_header_exists_and_matches_payload_rs() {
    let hdr = vendor_file(DOCK_QR_HEADER);
    // The header must decode OUR exact wire format, not invent a different one.
    for marker in [
        "struct CameraBoxQrData",
        "decode_camera_box_qr",
        "0xEDB88320", // the CRC-32/ISO-HDLC reflected polynomial — matches src/probe/payload.rs's
        // `crc::CRC_32_ISO_HDLC` (the same standard algorithm the Rust `crc` crate and
        // Python's `zlib.crc32` both implement).
        "CAMERA_BOX_AUDIO_F_HZ",
        "CAMERA_BOX_AUDIO_C",
        "CAMERA_BOX_AUDIO_Q_MS",
    ] {
        assert!(
            hdr.contains(marker),
            "{DOCK_QR_HEADER}: #398 marker `{marker}` is gone — the camera-box dual-QR decode \
             header regressed or was reverted by an upstream sync. Re-apply."
        );
    }
}

#[test]
fn sync_test_output_tries_camera_box_format_before_norihiros() {
    let src = squish(&vendor_file(DOCK_OUTPUT));
    assert!(
        src.contains("#include \"camera-box-qr.hpp\""),
        "{DOCK_OUTPUT}: #398 — the camera-box-qr.hpp include is gone; the dock no longer knows \
         our dual-QR format. Re-apply."
    );
    for marker in [
        "CameraBoxQrData cb",
        "decode_camera_box_qr((char *)data.payload, &cb)",
    ] {
        assert!(
            src.contains(marker),
            "{DOCK_OUTPUT}: #398 marker `{marker}` is gone — the dock no longer tries the \
             camera-box dual-QR format before falling back to norihiro's `q=,i=,f=,c=` QR. \
             Re-apply (Option A, #398)."
        );
    }
}

#[test]
fn audio_decode_pairs_via_the_camera_box_video_ring() {
    let src = squish(&vendor_file(DOCK_OUTPUT));
    // The direct index->video_ts lookup that makes LIVE pairing possible with no side channel
    // (the audio index IS the dual-QR frame_id's low byte — see frame_id_to_index in
    // src/qpsk_marker.rs). If this regresses, the dock silently stops showing a live offset for
    // camera-box's own audio marker (norihiro's phone method keeps working; ours goes silent).
    for marker in [
        "cb_video_ts_ns[256]",
        "cb_video_valid[256]",
        "cb_mode_active",
        "signal_sync_found(st->context, &si)",
    ] {
        assert!(
            src.contains(marker),
            "{DOCK_OUTPUT}: #398 marker `{marker}` is gone — the camera-box live audio/video \
             pairing ring regressed or was reverted. Re-apply (Option A, #398)."
        );
    }
}
