//! Issue 1367 — the live A/V-sync dock decodes the QPSK marker per channel, never on their average.
//!
//! `st_raw_audio_camera_box` in `vendor/av-sync-dock/src/sync-test-output-audio.cpp` used to mix
//! every channel to mono before its one streaming decoder. The stream box's `mbc` input is stereo
//! with the marker on L and R 10.17 ms apart, and that sum is undecodable. The decode and the
//! channel rule now live in `camera-box-channel-pick.hpp` (proven against the Rust reference by
//! `tests/qpsk_channel_pick_parity_1367.rs`); these checks prove the OBS glue is WIRED to it:
//! - the audio callback hands every channel's plane to the picker and pairs only what it returns;
//! - no channel sum remains anywhere in the callback;
//! - the pairing recovery, the staleness detector and the diag line read the picker;
//! - the diag line appends the chosen channel, every channel's cluster and the switch count AFTER
//!   the existing tokens;
//! - a switch of the paired channel is logged when it happens (rate-limited, with a count).
//!
//! The dock compiles ONLY on the Windows runner, so the same checks are mirrored by the pwsh step
//! "Assert dock decodes the marker per channel (issue 1367)" in BOTH windows-genlock workflows
//! (`.claude/rules/av-sync-dock-anchor-refactor-safety.md`).

#[path = "support/cpp_source.rs"]
mod cpp_source;
use cpp_source::{squish, strip_cpp_comments, unique_body_of};
#[allow(dead_code)]
#[path = "support/av_sync_dock_output.rs"]
mod av_sync_dock_output;

const DOCK_OUTPUT: &str = av_sync_dock_output::LABEL;
const AUDIO_SIG: &str =
    "static void st_raw_audio_camera_box(struct sync_test_output *st, struct audio_data *frames)";

fn code() -> String {
    squish(&strip_cpp_comments(&av_sync_dock_output::source()))
}

#[test]
fn the_audio_callback_decodes_every_channel_through_the_picker() {
    let code = code();
    for needle in [
        "#include \"camera-box-channel-pick.hpp\"",
        "camerabox::ChannelMarkerPicker *cb_audio_dec = nullptr;",
    ] {
        assert!(code.contains(needle), "{DOCK_OUTPUT}: `{needle}` is gone");
    }
    let body = unique_body_of(&code, AUDIO_SIG);
    for needle in [
        "const size_t nch = cb_ensure_audio_picker(st); if (nch == 0) return;",
        "planes[cix] = (const float *)frames->data[cix];",
        "st->cb_audio_dec->push(planes, nf);",
        "cb_audio_diag_tick(st, frames);",
    ] {
        assert!(
            body.contains(needle),
            "{DOCK_OUTPUT}: the audio callback no longer has `{needle}` — the per-channel decode \
             (issue 1367) regressed"
        );
    }
    // The picker lifecycle: one picker per channel layout, rebuilt (with a fresh sample count)
    // when the layout changes, built with the dock configuration.
    let ensure = unique_body_of(
        &code,
        "static size_t cb_ensure_audio_picker(struct sync_test_output *st)",
    );
    for needle in [
        "const size_t nch = st->audio_channels < MAX_AV_PLANES ? st->audio_channels : MAX_AV_PLANES;",
        "st->cb_audio_dec->channels() != nch",
        "st->cb_audio_pushed = 0;",
        "camerabox::ChannelMarkerPicker::dock( nch, st->audio_sample_rate, CAMERA_BOX_AUDIO_F_HZ, CAMERA_BOX_AUDIO_C)",
    ] {
        assert!(
            ensure.contains(needle),
            "{DOCK_OUTPUT}: cb_ensure_audio_picker no longer has `{needle}` (issue 1367)"
        );
    }
}

#[test]
fn no_channel_sum_remains_in_the_audio_callback() {
    let code = code();
    let body = unique_body_of(&code, AUDIO_SIG);
    for banned in [
        "acc +=",
        "/ (float)ch",
        "std::vector<float> mono",
        "mono.data()",
        "camerabox::StreamingMarkerDecoder(",
    ] {
        assert!(
            !body.contains(banned),
            "{DOCK_OUTPUT}: the audio callback mixes channels again (`{banned}`) — the stereo mbc \
             marker sits on L and R 10.17 ms apart and their sum is undecodable (issue 1367)"
        );
    }
    assert!(
        !code.contains("camerabox::StreamingMarkerDecoder *cb_audio_dec"),
        "{DOCK_OUTPUT}: the single mono decoder member is back"
    );
}

#[test]
fn the_diag_line_appends_the_channel_pick_after_the_existing_tokens() {
    let code = code();
    assert!(
        code.contains(
            "locked=%s state=%s decode_dropped=%llu publish_max_us=%llu \" \"marker_channel=%zu channel_clusters=%s channel_switches=%llu\""
        ),
        "{DOCK_OUTPUT}: the diag line must end `... publish_max_us=%llu marker_channel=%zu \
         channel_clusters=%s channel_switches=%llu` (existing tokens unchanged, appended last)"
    );
    assert!(code.contains(
        "(unsigned long long)(st->cb_publish_max_ns.exchange(0) / 1000), st->cb_audio_dec->chosen, channel_clusters.c_str(), (unsigned long long)st->cb_switch_log.total,"
    ));
    assert!(code.contains(
        "const std::string channel_clusters = camerabox::cb_channel_clusters_text(st->cb_audio_dec->clusters);"
    ));
}

/// A switch of the paired channel moves the measured offset by ~10 ms (R is 10.17 ms behind L) and
/// the offset cluster is not reset, so it must show in the OBS log when it happens, not only in the
/// 10 s diag sample: logged at once, then at most one line per diag interval carrying how many
/// switches it stands for, and counted on the diag line.
#[test]
fn a_channel_switch_is_logged_and_counted() {
    let code = code();
    // the decision itself is the pure CbChannelSwitchLog (camera-box-channel-pick.hpp), pinned
    // against the Rust reference by tests/qpsk_channel_pick_parity_1367.rs
    assert!(code.contains("camerabox::CbChannelSwitchLog cb_switch_log;"));
    let body = unique_body_of(&code, AUDIO_SIG);
    for needle in [
        "const size_t prev_channel = st->cb_audio_dec->chosen; const uint64_t base = st->cb_audio_pushed;",
        "st->cb_audio_pushed += (uint64_t)nf; cb_note_channel_switch(st, prev_channel, frames);",
    ] {
        assert!(
            body.contains(needle),
            "{DOCK_OUTPUT}: the audio callback no longer has `{needle}` (issue 1367)"
        );
    }
    let note = unique_body_of(
        &code,
        "static void cb_note_channel_switch(struct sync_test_output *st, size_t prev, const struct audio_data *frames)",
    );
    for needle in [
        "uint64_t switches = 0; if (!st->cb_switch_log.observe(prev, st->cb_audio_dec->chosen, frames->timestamp, CAMERA_BOX_DIAG_LOG_INTERVAL_NS, &switches)) return;",
        "\"av-sync-dock: marker channel %zu -> %zu (channel_clusters=%s, %llu switch(es) since the last line)\", prev, st->cb_audio_dec->chosen, clusters.c_str(), (unsigned long long)switches);",
    ] {
        assert!(
            note.contains(needle),
            "{DOCK_OUTPUT}: cb_note_channel_switch no longer has `{needle}` (issue 1367)"
        );
    }
}
