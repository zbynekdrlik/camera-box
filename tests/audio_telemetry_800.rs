//! #800 — audio-side telemetry (the audio twin of the genlock-fifo audit).
//!
//! The live A/V-desync investigations kept dying on a log blind spot: the video chain is
//! instrumented per hop, the audio path logged nothing between "adding audio buffering"
//! events. These anchors pin the periodic `audio-telemetry #800` dump in the vendored
//! libobs audio tick so a vendor bump can't silently drop it.

use std::fs;

const OBS_AUDIO: &str = "vendor/obs-studio/libobs/obs-audio.c";

#[test]
fn audio_telemetry_dump_is_pinned_in_the_audio_tick_800() {
    let src = fs::read_to_string(OBS_AUDIO).expect("read vendored obs-audio.c");

    // Subsystem line: total buffering + which source forced it.
    assert!(
        src.contains("audio-telemetry #800: total_buffering="),
        "#800 telemetry: subsystem total-buffering line missing from obs-audio.c"
    );
    // Per-source line: timeline lag vs OS clock + buffered depth + timing adjust —
    // the fields that discriminate an in-OBS audio-timeline shift from an external one.
    for token in ["ts_lag_ms=", "buffered_ms=", "timing_adjust_ms="] {
        assert!(
            src.contains(token),
            "#800 telemetry: per-source field `{token}` missing from obs-audio.c"
        );
    }
    // 60 s rate limit — telemetry must never spam the log at tick rate.
    assert!(
        src.contains("60000000000ULL"),
        "#800 telemetry: 60s rate limit missing from obs-audio.c"
    );
    // The per-source walk must hold the audio-sources mutex (same discipline as
    // calc_min_ts) — pin the lock call inside the telemetry block.
    let block = src
        .split("audio-telemetry #800: total_buffering=")
        .nth(1)
        .expect("telemetry block present");
    let tail = &block[..block.len().min(2500)];
    assert!(
        tail.contains("pthread_mutex_lock(&data->audio_sources_mutex)"),
        "#800 telemetry: per-source walk lost its audio_sources_mutex lock"
    );
}

/// Issue 1367 (the FOH-click report, 25.9.2026): the obs-vban raw-audio output on resolume sent with
/// 308-378 ms gaps while the recording was clean. The mixer tick must PROVE it never stalls: the
/// largest gap between two audio-thread ticks and the longest tick of every 60 s window are logged
/// on their own line (a marker no other audio/genlock line contains).
#[test]
fn audio_thread_stall_probe_is_logged_every_window_1367() {
    let src = fs::read_to_string(OBS_AUDIO).expect("read vendored obs-audio.c");
    for token in [
        "audio-stall #1367: tick_gap_max_ms=",
        "callback_max_ms=",
        "ticks_over=",
        "static void audio_stall_probe_exit(uint64_t entry_ns)",
        "const uint64_t stall_entry_ns = os_gettime_ns();",
    ] {
        assert!(
            src.contains(token),
            "issue 1367: the audio-thread stall probe `{token}` is missing from obs-audio.c"
        );
    }
    // both returns of audio_callback close the tick (the buffering-wait early return included).
    assert_eq!(
        src.matches("audio_stall_probe_exit(stall_entry_ns);")
            .count(),
        2,
        "issue 1367: every return of audio_callback must close the stall probe's tick"
    );
}
