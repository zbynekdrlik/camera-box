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
const DOCK_MAIN: &str = "vendor/av-sync-dock/src/sync-test-dock.cpp";

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

/// #999 — the display-path sign-convention WIRING (not the pure fn, which
/// tests/av_sync_dock_latency_display_999.rs twin-tests): both camera-box emit sites must tag
/// their sync_index events `si.gate_convention = true;`, and on_sync_found must route the event
/// through `cb_dock_latency_display_ms`. A subtree pull (or careless revert) dropping any of
/// these still COMPILES and every pure-fn test stays green — the exact "the fix never reached
/// the display path" class issue 999 itself was. Mirrored pwsh anchors live in BOTH
/// windows-genlock*.yml (the #269 lock-step pattern).
#[test]
fn on_sync_found_display_path_gate_conversion_is_wired_999() {
    let out = squish(&vendor_file(DOCK_OUTPUT));
    assert_eq!(
        out.matches("si.gate_convention = true;").count(),
        2,
        "{DOCK_OUTPUT}: exactly TWO camera-box emit sites (the smoothed-ring one and the \
         direct-ring one) must tag their events `si.gate_convention = true;` — a count drift \
         means half the #999 display sign fix was dropped or a new emit site forgot the tag"
    );
    let dock = squish(&vendor_file(DOCK_MAIN));
    assert!(
        dock.contains("cb_dock_latency_display_ms(dock_native_ts_ns, data.gate_convention)"),
        "{DOCK_MAIN}: on_sync_found no longer routes through cb_dock_latency_display_ms — the \
         #999 display sign-convention fix was reverted from the UI handler"
    );
}

/// #1005 — the garbage-clamp fix WIRING (not the pure fn, which
/// tests/av_sync_dock_video_ts_valid_1005.rs twin-tests): both camera-box emit sites must guard
/// their `sync_index` construction+emission with `cb_corrected_video_ts_is_valid`, and the OLD
/// clamp-to-zero-and-emit-anyway ternary must be gone entirely — a subtree pull (or careless
/// revert) dropping the guard still COMPILES and every pure-fn test stays green, exactly the
/// class of gap issue 999's own display-path fix taught this repo to guard against structurally.
#[test]
fn corrected_video_ts_garbage_clamp_fix_is_wired_1005() {
    let out = squish(&vendor_file(DOCK_OUTPUT));
    assert_eq!(
        out.matches("camerabox::cb_corrected_video_ts_is_valid(corrected_video_ts)")
            .count(),
        2,
        "{DOCK_OUTPUT}: exactly TWO camera-box emit sites (the smoothed-ring one and the \
         direct-ring one) must guard their sync_index emission with \
         cb_corrected_video_ts_is_valid(corrected_video_ts) — a count drift means half the #1005 \
         garbage-clamp fix was dropped or a new emit site forgot the guard"
    );
    assert!(
        !out.contains("corrected_video_ts > 0 ? (uint64_t)corrected_video_ts : 0"),
        "{DOCK_OUTPUT}: the OLD clamp-to-zero-and-emit-anyway ternary is still present — #1005 \
         (a clamped video_ts=0 manufactures a garbage whole-timeline-scale offset) was reverted"
    );
}
