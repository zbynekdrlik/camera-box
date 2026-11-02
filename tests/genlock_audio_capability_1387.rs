//! Issue 1387, 1.11.2026: the genlock LOCK indicator read DEGRADED `audio_pairing` on strih-snv and
//! stream-snv for the whole SNV production, with no audio fault anywhere.
//!
//! The libobs stats fill set `audio_enabled` from `obs_source_audio_active()` alone. libobs creates
//! every source with `audio_active = true`, so a source that carries no audio at all reads as an
//! audio-enabled genlock source unless its plugin switches the flag off. The JPEG XS receiver
//! (`camera_box_jxs_source`, video only) never does, so its "audio" was never held to the video
//! (`audio_pairing_offset_ms` = -latency = -36 ms), and the widget's #1303 pairing bound (33 ms)
//! degraded the whole box. A source type without the `OBS_SOURCE_AUDIO` capability carries no
//! audio, so the stats must say so whatever its `audio_active` flag reads.

use std::fs;
use std::path::PathBuf;

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The body of `genlock_fill_stats` (from its definition to the next top-level `}`).
fn fill_stats_body(src: &str) -> &str {
    let start = src
        .find("\nstatic void genlock_fill_stats(")
        .or_else(|| src.find("\nvoid genlock_fill_stats("))
        .or_else(|| src.find("\nstatic bool genlock_fill_stats("))
        .expect("genlock_fill_stats definition in obs-source.c");
    let body = &src[start..];
    let end = body.find("\n}\n").expect("end of genlock_fill_stats");
    &body[..end]
}

#[test]
fn a_source_without_audio_capability_is_never_an_audio_enabled_genlock_source() {
    let src = read("vendor/obs-studio/libobs/obs-source.c");
    let body = fill_stats_body(&src);
    let line = body
        .lines()
        .find(|l| l.trim_start().starts_with("stats->audio_enabled ="))
        .expect("the audio_enabled fill");
    assert!(
        line.contains("OBS_SOURCE_AUDIO"),
        "audio_enabled must require the source type's OBS_SOURCE_AUDIO capability: {line}"
    );
    assert!(
        line.contains("obs_source_audio_active(source)"),
        "an audio-capable source still reads its live audio_active flag: {line}"
    );
}

/// Why the capability matters: the JPEG XS receiver is video only and never clears audio_active.
#[test]
fn the_jpeg_xs_receiver_declares_no_audio() {
    let src = read("vendor/distroav/src/jxs-source.cpp");
    let flags = src
        .lines()
        .find(|l| l.contains("info.output_flags ="))
        .expect("the JPEG XS source's output flags");
    assert!(flags.contains("OBS_SOURCE_ASYNC_VIDEO"), "{flags}");
    assert!(
        !flags.contains("OBS_SOURCE_AUDIO"),
        "the JPEG XS receiver carries no audio: {flags}"
    );
}
