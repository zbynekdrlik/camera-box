//! Issue 1367 — the live A/V-sync dock decodes the QPSK marker per channel, never on their average.
//!
//! `st_raw_audio_camera_box` in `vendor/av-sync-dock/src/sync-test-output.cpp` used to mix every
//! channel to mono before its one streaming decoder. The stream box's `mbc` input is stereo with
//! the marker on L and R 10.17 ms apart, and that sum is undecodable. The decode and the channel
//! rule now live in `camera-box-channel-pick.hpp` (proven against the Rust reference by
//! `tests/qpsk_channel_pick_parity_1367.rs`); these checks prove the OBS glue is WIRED to it:
//! - the audio callback hands every channel's plane to the picker and pairs only what it returns;
//! - no channel sum remains anywhere in the callback;
//! - the pairing recovery, the staleness detector and the diag line read the picker;
//! - the diag line appends the chosen channel and every channel's cluster AFTER the existing tokens.
//!
//! The dock compiles ONLY on the Windows runner, so the same checks are mirrored by the pwsh step
//! "Assert dock decodes the marker per channel (issue 1367)" in BOTH windows-genlock workflows
//! (`.claude/rules/av-sync-dock-anchor-refactor-safety.md`).

use std::path::PathBuf;

const DOCK_OUTPUT: &str = "vendor/av-sync-dock/src/sync-test-output.cpp";
const AUDIO_SIG: &str =
    "static void st_raw_audio_camera_box(struct sync_test_output *st, struct audio_data *frames)";

fn vendor_file(rel: &str) -> String {
    let p: PathBuf = [env!("CARGO_MANIFEST_DIR"), rel].iter().collect();
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Drop `/* ... */` and `// ...` comments so prose in a comment never satisfies or breaks an anchor.
fn strip_cpp_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    let mut in_str = false;
    while let Some(c) = rest.chars().next() {
        if in_str {
            if c == '\\' {
                let esc: String = rest.chars().take(2).collect();
                out.push_str(&esc);
                rest = &rest[esc.len()..];
                continue;
            }
            in_str = c != '"';
            out.push(c);
            rest = &rest[c.len_utf8()..];
        } else if rest.starts_with("/*") {
            out.push(' ');
            rest = rest[2..].find("*/").map_or("", |e| &rest[2 + e + 2..]);
        } else if rest.starts_with("//") {
            rest = rest.find('\n').map_or("", |e| &rest[e..]);
        } else {
            in_str = c == '"';
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// The balanced-brace body of the function whose signature is `sig`.
fn body_of<'a>(src: &'a str, sig: &str) -> &'a str {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("{DOCK_OUTPUT}: `{sig}` not found"));
    let open = start + src[start..].find('{').expect("function body opening brace");
    let mut depth = 0usize;
    for (off, ch) in src[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &src[open..=open + off];
                }
            }
            _ => {}
        }
    }
    panic!("{DOCK_OUTPUT}: unbalanced body for `{sig}`");
}

fn code() -> String {
    squish(&strip_cpp_comments(&vendor_file(DOCK_OUTPUT)))
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
    let body = body_of(&code, AUDIO_SIG);
    for needle in [
        "camerabox::ChannelMarkerPicker::dock( nch, st->audio_sample_rate, CAMERA_BOX_AUDIO_F_HZ, CAMERA_BOX_AUDIO_C)",
        "planes[cix] = (const float *)frames->data[cix];",
        "st->cb_audio_dec->push(planes, nf);",
        "st->cb_audio_dec->channels() != nch",
    ] {
        assert!(
            body.contains(needle),
            "{DOCK_OUTPUT}: the audio callback no longer has `{needle}` — the per-channel decode \
             (issue 1367) regressed"
        );
    }
}

#[test]
fn no_channel_sum_remains_in_the_audio_callback() {
    let code = code();
    let body = body_of(&code, AUDIO_SIG);
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
            "locked=%s state=%s decode_dropped=%llu publish_max_us=%llu \" \"marker_channel=%zu channel_clusters=%s\""
        ),
        "{DOCK_OUTPUT}: the diag line must end `... publish_max_us=%llu marker_channel=%zu \
         channel_clusters=%s` (existing tokens unchanged, the pick appended last)"
    );
    assert!(code.contains(
        "(unsigned long long)(st->cb_publish_max_ns.exchange(0) / 1000), st->cb_audio_dec->chosen, channel_clusters.c_str());"
    ));
    assert!(code.contains(
        "const std::string channel_clusters = camerabox::cb_channel_clusters_text(st->cb_audio_dec->clusters);"
    ));
}
