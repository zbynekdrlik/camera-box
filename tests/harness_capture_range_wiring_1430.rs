//! Issue 1430 -- static anchors for the capture-range WIRING in `src/main.rs`.
//!
//! The decision (`camera_box::capture_range`) and the conversion (`camera_box::yuyv_to_uyvy`)
//! are unit-tested in their modules; `main.rs` has no unit test of its own. This file pins what
//! those tests cannot see:
//!
//! * the resolved range reaches the sender exactly once, AFTER the sender is created and BEFORE it
//!   is moved into `capture_sender` (from which the burn thread takes it), so the production send
//!   and the probe burn send both scale;
//! * the range is resolved from the SAME detected card the grabber model uses;
//! * the `capture chroma:` / `capture luma:` samplers keep reading the RAW capture buffer `data`,
//!   never the converted UYVY.

use std::path::PathBuf;

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The byte offset of the ONE occurrence of `needle` in `hay` (fails on 0 or 2+).
fn unique(hay: &str, needle: &str) -> usize {
    let n = hay.matches(needle).count();
    assert_eq!(n, 1, "expected exactly one {needle:?}, found {n}");
    hay.find(needle).unwrap()
}

#[test]
fn the_range_reaches_the_sender_before_it_moves_to_the_capture_or_burn_thread_1430() {
    let main = read("src/main.rs");
    let created = unique(
        &main,
        "let mut sender = NdiSender::new(ndi_name, send_rate)?;",
    );
    let set = unique(&main, "sender.set_capture_range(");
    let moved = unique(
        &main,
        "let mut capture_sender: Option<NdiSender> = Some(sender);",
    );
    assert!(
        created < set && set < moved,
        "set_capture_range must sit between NdiSender::new and the move into capture_sender"
    );
    let call = &main[set..set + 160];
    assert!(
        call.contains("capture_range::resolve_and_log(") && call.contains("detected_card"),
        "the range must be resolved from the detected card: {call}"
    );
    let detected = unique(
        &main,
        "let detected_card = camera_box::capture::query_card_name(device_path);",
    );
    assert!(
        detected < set,
        "the card is detected before the range is resolved"
    );
}

#[test]
fn the_luma_and_chroma_samplers_read_the_raw_capture_buffer_1430() {
    let main = read("src/main.rs");
    for sampler in [
        "camera_box::capture::mean_chroma(",
        "camera_box::luma_levels::luma_levels(",
    ] {
        let at = unique(&main, sampler);
        let args = main[at + sampler.len()..].trim_start();
        assert!(
            args.starts_with("data,"),
            "{sampler} must read the raw capture buffer `data`, found: {}",
            &args[..args.len().min(40)]
        );
    }
    assert!(
        !main.contains("uyvy_buffer"),
        "main.rs must never read the sender's converted buffer"
    );
}
