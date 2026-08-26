//! #1153 (sticky-unlock recovery) — the dead-pairing watchdog + full in-dock pairing-state reset.
//!
//! Live evidence (2026-08-26): after the E2E's ±1 s video-latency restore step the dock's
//! marker↔QR pairing died (ring_hit frozen at chance level, crc_ok rate ~1/256 chance floor) and
//! STAYED dead for 2+ hours until a manual OBS restart, while a fresh instance locked within
//! ~2.5 min under identical ambient conditions. Every pre-existing unlock/reset path is
//! decoded-marker-driven, and the #1177 staleness detector is display-only — nothing ever reset
//! the pairing state, so an OBS restart was the only cure. The fix: a pure, parity-mirrored
//! epoch watchdog observed at the #690 diag tick, whose fire resets ALL in-dock pairing state
//! (ring, cluster, offset history, audit tracker, decoder rolling window) and logs one
//! PAIRING-RECOVER evidence line; plus non-finite-sample hardening in the mixdown + the shared
//! decode kernel (a NaN prefix-sum poison is the one upstream latch class the dock can neutralize
//! itself).
//!
//! This file covers ONLY the OBS-glue half (`sync-test-output.cpp`, which pulls in libobs/quirc
//! and is NOT compiled by this repo's Linux CI) plus source-presence of the mirrored seams —
//! the pure watchdog/kernel logic itself is covered by the `av_sync_dock`/`qpsk_marker` unit
//! tests and the committed C++ self-test (`av_sync_dock_cpp_mirror_gate`). Source-presence
//! guard, same convention as `av_sync_dock_audio_diag_stats.rs`.

use std::path::PathBuf;

const DOCK_OUTPUT: &str = "vendor/av-sync-dock/src/sync-test-output.cpp";
const DOCK_AUDIO_HPP: &str = "vendor/av-sync-dock/src/camera-box-audio.hpp";
const RUST_DOCK: &str = "src/av_sync_dock.rs";
const RUST_KERNEL: &str = "src/qpsk_marker.rs";

fn repo_file(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn pairing_watchdog_is_wired_into_the_diag_tick_and_resets_all_pairing_state() {
    let src = squish(&repo_file(DOCK_OUTPUT));
    for marker in [
        // the watchdog instance + its observe at the diag tick
        "camerabox::CbDockPairingWatchdog cb_pairing_watchdog;",
        "st->cb_pairing_watchdog.observe(",
        // the reset scope: ring + cluster + history + audit + decoder window
        "st->cb_offset_cluster = camerabox::RollingOffsetCluster::dock();",
        "st->cb_offset_history.clear();",
        "st->cb_lock_audit = camerabox::CbLockAuditTracker();",
        "st->cb_audio_dec->reset_window();",
        // the one-shot evidence line (mutually non-substring marker token)
        "av-sync-dock: PAIRING-RECOVER",
    ] {
        assert!(
            src.contains(marker),
            "{DOCK_OUTPUT}: #1153 marker `{marker}` is gone — the dead-pairing recovery \
             (watchdog observe at the diag tick + full in-dock pairing-state reset) regressed. \
             Without it a sticky post-latency-step unlock is only curable by an OBS restart. \
             Re-apply."
        );
    }
}

#[test]
fn mono_mixdown_skips_non_finite_samples_per_channel() {
    let src = squish(&repo_file(DOCK_OUTPUT));
    assert!(
        src.contains("if (std::isfinite(s)) acc += s;"),
        "{DOCK_OUTPUT}: #1153 per-channel non-finite guard is gone from the mono mixdown — a \
         poisoned upstream channel would again wipe a marker riding another channel. Re-apply."
    );
}

#[test]
fn cpp_mirror_carries_the_watchdog_and_the_kernel_sanitize() {
    let src = squish(&repo_file(DOCK_AUDIO_HPP));
    for marker in [
        "constexpr uint64_t CB_DOCK_PAIRING_DEAD_NS",
        "constexpr uint64_t CB_DOCK_PAIRING_MIN_RING_HITS",
        "class CbDockPairingWatchdog",
        "void reset_window()",
        "if (!std::isfinite(x)) x = 0.0;",
    ] {
        assert!(
            src.contains(marker),
            "{DOCK_AUDIO_HPP}: #1153 marker `{marker}` is gone — the C++ mirror of the \
             dead-pairing watchdog / decoder window reset / kernel non-finite sanitize regressed. \
             Re-apply in lockstep with src/av_sync_dock.rs + src/qpsk_marker.rs."
        );
    }
}

#[test]
fn rust_reference_carries_the_watchdog_and_the_kernel_sanitize() {
    let dock = squish(&repo_file(RUST_DOCK));
    for marker in [
        "pub const DOCK_PAIRING_DEAD_NS",
        "pub const DOCK_PAIRING_MIN_RING_HITS",
        "pub struct DockPairingWatchdog",
        "pub fn reset_window(&mut self)",
    ] {
        assert!(
            dock.contains(marker),
            "{RUST_DOCK}: #1153 marker `{marker}` is gone — the Rust reference of the \
             dead-pairing watchdog / decoder window reset regressed. Re-apply."
        );
    }
    let kernel = squish(&repo_file(RUST_KERNEL));
    assert!(
        kernel.contains("let x = if x.is_finite() { x } else { 0.0 };"),
        "{RUST_KERNEL}: #1153 non-finite input sanitize is gone from the decode kernel's \
         prefix-sum loop — one NaN sample would again poison the whole window. Re-apply."
    );
}
