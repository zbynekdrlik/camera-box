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
