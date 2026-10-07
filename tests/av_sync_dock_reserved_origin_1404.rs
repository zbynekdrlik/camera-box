//! Issue 1404 Task 5 part b (design comment 6048239795): the stream av-sync dock pairs only the cam2
//! painter's QRs, never a reserved ORIGIN run of the CG path.
//!
//! The dock's audio is the `mbc` room marker of the cam2 painter. During a CG segment the program
//! shows the measurement clip's own tick line (run 911016), an unrelated timeline; pairing the two
//! fed a meaningless offset into the dock's 180 s cluster, the LOCK-CORRECT suggestions and the
//! av-step watchdog (ROZHODNUTE issue 1404 comment 6048179415 item 3). So `cb_video_qr_record`
//! (`vendor/av-sync-dock/src/sync-test-output-video.cpp`) refuses a QR whose run is in
//! `CAMERA_BOX_RESERVED_ORIGIN_RUN_IDS` (`camera-box-qr.hpp`) as its FIRST statement, and both
//! callers drop such a QR entirely (no qrcode_found, no marker, not counted as decoded).
//!
//! Pinned here, by the pwsh step "Assert dock never pairs a reserved origin QR (issue 1404)" in
//! BOTH windows-genlock workflows, and by the g++ self-test `camera-box-selftest.cpp` (a 911016 QR
//! is not recorded, a painter QR is), which `tests/av_sync_dock_cpp_mirror_gate.rs` runs:
//! - the dock's id list equals the Rust reserved origin ids (`BURN_RUN_ID_SONGPLAYER`,
//!   `BURN_RUN_ID_CG`, `MEASUREMENT_CLIP_RUN_ID` in `src/probe/recording_latency.rs`);
//! - the record starts with the predicate and returns false before touching the ring;
//! - both callers run the guarded record right after the decode, before any signal.

use std::path::PathBuf;

#[path = "support/cpp_source.rs"]
mod cpp_source;
use cpp_source::{squish, strip_cpp_comments, unique_body_of};
#[allow(dead_code)]
#[path = "support/av_sync_dock_output.rs"]
mod av_sync_dock_output;

const QR_HEADER: &str = "vendor/av-sync-dock/src/camera-box-qr.hpp";
const RECORDING_LATENCY: &str = "src/probe/recording_latency.rs";
const RECORD_SIG: &str = "static bool cb_video_qr_record(struct sync_test_output *st, uint32_t run_id, uint32_t frame_id, uint64_t video_ts)";
const GUARDED_CALL: &str =
    "if (!cb_video_qr_record(st, cb.run_id, cb.frame_id, timestamp - st->start_ts)) continue;";

fn read(rel: &str) -> String {
    let p: PathBuf = [env!("CARGO_MANIFEST_DIR"), rel].iter().collect();
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

fn code() -> String {
    squish(&strip_cpp_comments(&av_sync_dock_output::source()))
}

/// `pub const NAME: u32 = N;` of the Rust source, as a number.
fn rust_u32(text: &str, name: &str) -> u32 {
    let needle = format!("pub const {name}: u32 = ");
    let at = text
        .find(&needle)
        .unwrap_or_else(|| panic!("{RECORDING_LATENCY}: `{needle}` is gone"))
        + needle.len();
    let digits: String = text[at..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '_')
        .filter(|c| *c != '_')
        .collect();
    digits
        .parse()
        .unwrap_or_else(|e| panic!("{name}: `{digits}`: {e}"))
}

/// The ids of `#define CAMERA_BOX_RESERVED_ORIGIN_RUN_IDS {a, b, c}` in the dock header.
fn dock_ids() -> Vec<u32> {
    let hdr = squish(&strip_cpp_comments(&read(QR_HEADER)));
    let needle = "#define CAMERA_BOX_RESERVED_ORIGIN_RUN_IDS {";
    assert_eq!(
        hdr.matches(needle).count(),
        1,
        "{QR_HEADER}: one CAMERA_BOX_RESERVED_ORIGIN_RUN_IDS list"
    );
    let at = hdr.find(needle).expect("the list") + needle.len();
    let end = at + hdr[at..].find('}').expect("the list's closing brace");
    hdr[at..end]
        .split(',')
        .map(|s| {
            let v = s.trim().trim_end_matches('u');
            v.parse()
                .unwrap_or_else(|e| panic!("{QR_HEADER}: id `{s}`: {e}"))
        })
        .collect()
}

#[test]
fn the_dock_list_is_the_rust_reserved_origin_ids_1404() {
    let rl = read(RECORDING_LATENCY);
    let rust = vec![
        rust_u32(&rl, "BURN_RUN_ID_SONGPLAYER"),
        rust_u32(&rl, "BURN_RUN_ID_CG"),
        rust_u32(&rl, "MEASUREMENT_CLIP_RUN_ID"),
    ];
    assert_eq!(rust, vec![911_014, 911_015, 911_016]);
    assert_eq!(
        dock_ids(),
        rust,
        "{QR_HEADER}: the dock's reserved origin ids must be the Rust ones, in the same order"
    );
}

#[test]
fn the_predicate_reads_the_one_list_1404() {
    let hdr = squish(&strip_cpp_comments(&read(QR_HEADER)));
    let body = unique_body_of(
        &hdr,
        "inline bool camera_box_qr_is_paired_run(uint32_t run_id)",
    );
    for need in [
        "static const uint32_t reserved[] = CAMERA_BOX_RESERVED_ORIGIN_RUN_IDS;",
        "if (reserved[i] == run_id) return false;",
        "return true;",
    ] {
        assert!(
            body.contains(need),
            "{QR_HEADER}: camera_box_qr_is_paired_run lost `{need}`"
        );
    }
}

#[test]
fn the_record_refuses_a_reserved_origin_before_touching_the_ring_1404() {
    let src = code();
    let body = unique_body_of(&src, RECORD_SIG);
    assert!(
        body.starts_with("{ if (!camera_box_qr_is_paired_run(run_id)) {"),
        "{}: cb_video_qr_record must START with the reserved-origin refusal, got: {}",
        av_sync_dock_output::LABEL,
        &body[..body.len().min(160)]
    );
    let refuse = body
        .find("return false;")
        .expect("the refusal returns false");
    let ring = body
        .find("st->cb_video_ts_ns[low] = video_ts;")
        .expect("the ring write");
    let mode = body
        .find("st->cb_mode_active = true;")
        .expect("the mode latch");
    assert!(
        refuse < ring && refuse < mode,
        "the refusal must return before the ring write and the camera-box mode latch"
    );
    assert!(
        body.ends_with("return true; }"),
        "a recorded QR returns true"
    );
}

#[test]
fn both_callers_drop_a_refused_qr_before_any_signal_1404() {
    let src = code();
    assert_eq!(
        src.matches(GUARDED_CALL).count(),
        2,
        "both QR decodes must run the guarded record"
    );
    assert_eq!(
        src.matches("cb_video_qr_record(").count(),
        3,
        "the definition + the two guarded calls, nothing else"
    );
    assert!(
        !src.contains("cb_video_qr_record(st, cb.frame_id,"),
        "the old unguarded call form is gone"
    );
    // the guard is the first thing after each decode, so nothing is signalled for a refused QR
    for need in [
        format!("if (decode_camera_box_qr((char *)data.payload, &cb)) {{ {GUARDED_CALL}"),
        format!("if (!decode_camera_box_qr((char *)data.payload, &cb)) continue; {GUARDED_CALL}"),
    ] {
        assert_eq!(
            src.matches(need.as_str()).count(),
            1,
            "missing the decode -> guarded record adjacency `{need}`"
        );
    }
    for (pos, _) in src.match_indices(GUARDED_CALL) {
        let after = &src[pos..];
        let signal = after
            .find("signal_qrcode_found(")
            .expect("a qrcode_found signal follows");
        let marker = after
            .find("video_marker_found(st, timestamp, 1.0f);")
            .expect("a marker follows");
        assert!(
            signal > 0 && marker > signal,
            "the signals come after the guard"
        );
    }
}

#[test]
fn the_ignored_run_is_logged_once_per_segment_not_per_frame_1404() {
    let src = code();
    let body = unique_body_of(&src, RECORD_SIG);
    for need in [
        "if (st->cb_ignored_origin_run != run_id) { st->cb_ignored_origin_run = run_id;",
        "st->cb_ignored_origin_run = 0;",
    ] {
        assert!(body.contains(need), "cb_video_qr_record lost `{need}`");
    }
    assert!(
        src.contains("uint32_t cb_ignored_origin_run = 0;"),
        "the worker-owned field is declared"
    );
}
