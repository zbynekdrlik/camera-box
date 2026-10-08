//! Issue 1404 (the CI timeout of comment 6049927841): what the `recording-verdict --av-sync` video
//! decode asks the #207 fast-path gate for, and when the painter path stops early.
//!
//! The #207 gate (`probe::recording_decode`) skips the robust recovery (the #202 bottom tiles and
//! the issue-1370 slot looks) only on a frame whose plain pass already read every requested QR.
//! Requesting a QR the recording cannot carry sends EVERY frame through that recovery: the
//! issue-423 class, see `.config/nextest.toml`. The measurement clip carries no node burn, so
//! decoding it with the cam1/strih/stream set cost 434 ms a frame instead of 124 ms (the runner's
//! release decoder, STEP 0 comment 6050103179), and three CI tests ran into nextest's 480 s kill.
//!
//! - `--av-run <run>` requests exactly what its frames carry: the run's own dual-QR Vernier (both
//!   halves), no node burn. A rig recording of a CG segment still reads its burns on the plain
//!   pass; they are only no longer required.
//! - The painter path keeps today's request ([`PAINTER_PATH_NODE_BURNS`]) for every recording it
//!   decodes in full. It first reads a short head with a request that names nothing, so no head
//!   frame goes robust. A head that shows the self-marked measurement clip and neither a cam2
//!   tick nor any of those rig burns is the clip's own recording: the path stops there
//!   ([`PainterHead::NoCam2Tick`]) instead of decoding every frame through the robust recovery.
//!   Any rig frame in the head, and any head without the clip (a QR-less pre-roll), goes on to
//!   the unchanged full decode. Two residual edges, both accepted: a burn-free recording whose
//!   head shows only the clip but which later switches to painter content is refused; and a
//!   clip-only recording that opens on a QR-less pre-roll longer than the head is decoded in full
//!   through the robust recovery, then fails on the coverage guard as before this change.
//!
//! Pure (no I/O). `probe::av_sync_recording` decodes the head and the recording with these
//! requests; `probe::recording::analyze_recording_head` stops ffmpeg after the head.

/// The node burns the painter path requires on the #207 fast path: cam1, strih, stream (the
/// `recording_latency` run ids 911001 / 911002 / 911004). Today's set: it mirrors
/// `probe::recording::GENERIC_DIAGNOSTIC_BURN_IDS`, the set `analyze_recording` decodes with.
pub const PAINTER_PATH_NODE_BURNS: [u32; 3] = [911_001, 911_002, 911_004];

/// The dual-QR Vernier shows two halves of its run on a frame (the clip's frame 0 shows one).
pub const DUAL_QR_HALVES: usize = 2;

/// How many leading frames the painter path reads before its full decode: 2 s of a 30 fps
/// recording. A rig recording already shows a node burn and a cam2 tick on its first frame, but the
/// head still decodes all of these frames on the fast path (about 7 CPU-s in release at 1080p,
/// about 4x that for a 4K strih recording) before the full decode starts.
pub const PAINTER_HEAD_FRAMES: u64 = 60;

/// What one decode asks the #207 fast-path gate for (the arguments of
/// `probe::recording::analyze_recording_with_grouped_burns_optical`, with no any-of group).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvDecodeRequest {
    /// Node burns that must ALL decode on a frame before the fast path may skip the recovery.
    pub mandatory_burns: Vec<u32>,
    /// `Some((run, n))`: the fast path also needs `n` distinct `frame_id`s of `run`.
    pub min_distinct_optical: Option<(u32, usize)>,
}

/// The decode request of `--av-sync`: `None` = the painter path (today's set), `Some(run)` =
/// `--av-run <run>`.
pub fn av_decode_request(av_run: Option<u32>) -> AvDecodeRequest {
    match av_run {
        Some(run) => AvDecodeRequest {
            mandatory_burns: Vec::new(),
            min_distinct_optical: Some((run, DUAL_QR_HALVES)),
        },
        None => painter_request(),
    }
}

/// The request of the painter path's full decode.
fn painter_request() -> AvDecodeRequest {
    AvDecodeRequest {
        mandatory_burns: PAINTER_PATH_NODE_BURNS.to_vec(),
        min_distinct_optical: None,
    }
}

/// The request of the painter path's head: nothing required, so every head frame stays on the fast
/// path (plain + Otsu, plus the #754 top-band look on a frame short of a dual-QR). The head only
/// has to show whether a cam2 tick or a rig burn is there; the plain pass reads both.
pub fn painter_head_request() -> AvDecodeRequest {
    AvDecodeRequest {
        mandatory_burns: Vec::new(),
        min_distinct_optical: None,
    }
}

/// What the painter path's head decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PainterHead {
    /// The head carries a cam2 tick or a rig node burn, or shows no self-marked run (an empty head,
    /// a QR-less pre-roll): decode the whole recording with today's request, unchanged.
    FullDecode,
    /// The head's `frames` frames show a self-marked run (the measurement clip) and none of them a
    /// cam2 tick or a rig node burn: the clip's own recording, not a cam2-painter one.
    NoCam2Tick { frames: usize },
}

/// The painter head's verdict. `head` holds one `(cam2 tick, run ids read)` per decoded head
/// frame, in any order: the tick is `RecordingFrame::tick` (reserved ids already excluded).
///
/// The head stops only a recording it positively recognises: one that shows a self-marked run
/// (`av_run_pairing::SELF_MARKED_RUN_IDS`) and no rig signal at all. A head that shows nothing
/// proves nothing (a black or slate pre-roll, NDI inputs still reconnecting after an OBS restart),
/// so that recording goes on to the full decode, which reads its later frames exactly as before.
pub fn painter_head_verdict(head: &[(Option<u32>, Vec<u32>)]) -> PainterHead {
    let rig_signal = head.iter().any(|(tick, run_ids)| {
        tick.is_some() || run_ids.iter().any(|r| PAINTER_PATH_NODE_BURNS.contains(r))
    });
    let self_marked_run = head.iter().any(|(_, run_ids)| {
        run_ids
            .iter()
            .any(|r| crate::av_run_pairing::SELF_MARKED_RUN_IDS.contains(r))
    });
    if self_marked_run && !rig_signal {
        PainterHead::NoCam2Tick { frames: head.len() }
    } else {
        PainterHead::FullDecode
    }
}

/// The error the painter path stops with on [`PainterHead::NoCam2Tick`].
pub fn no_cam2_tick_message(frames: usize) -> String {
    let clip = crate::av_run_pairing::SELF_MARKED_RUN_IDS[0];
    format!(
        "no cam2 painter tick and no rig node burn {PAINTER_PATH_NODE_BURNS:?} on any of the \
         first {frames} frames, only the self-marked measurement clip (run {clip}): not a \
         cam2-painter rig recording, nothing to measure on the painter path (measure the clip \
         with --av-run {clip})"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIP: u32 = 911_016;
    const CAM1: u32 = 911_001;
    const STRIH: u32 = 911_002;
    const STREAM: u32 = 911_004;
    /// The cg OBS hop burn: a reserved node burn, but not in the painter path's set.
    const CG_OBS: u32 = 911_015;

    /// `n` head frames, each with no cam2 tick and the run ids `runs`.
    fn head(n: usize, runs: &[u32]) -> Vec<(Option<u32>, Vec<u32>)> {
        (0..n).map(|_| (None, runs.to_vec())).collect()
    }

    #[test]
    fn the_painter_path_keeps_todays_request_1404() {
        assert_eq!(
            av_decode_request(None),
            AvDecodeRequest {
                mandatory_burns: vec![CAM1, STRIH, STREAM],
                min_distinct_optical: None,
            }
        );
    }

    #[test]
    fn av_run_requests_exactly_its_own_dual_qr_and_no_node_burn_1404() {
        assert_eq!(
            av_decode_request(Some(CLIP)),
            AvDecodeRequest {
                mandatory_burns: vec![],
                min_distinct_optical: Some((CLIP, 2)),
            },
            "the clip carries no node burn: requiring one sends every frame through the robust \
             recovery"
        );
    }

    #[test]
    fn the_painter_head_requests_no_qr_so_no_head_frame_goes_robust_1404() {
        assert_eq!(
            painter_head_request(),
            AvDecodeRequest {
                mandatory_burns: vec![],
                min_distinct_optical: None,
            }
        );
    }

    #[test]
    fn a_clip_head_without_a_cam2_tick_or_a_rig_burn_stops_the_painter_path_1404() {
        let frames = PAINTER_HEAD_FRAMES as usize;
        assert_eq!(
            painter_head_verdict(&head(frames, &[CLIP, CLIP])),
            PainterHead::NoCam2Tick { frames }
        );
        // the clip after a black pre-roll, and next to a cg-path burn (no painter-set rig burn)
        let mut pre_roll = head(frames, &[]);
        pre_roll[frames - 1].1 = vec![CLIP, CLIP];
        pre_roll[frames - 2].1 = vec![CG_OBS, CLIP];
        assert_eq!(
            painter_head_verdict(&pre_roll),
            PainterHead::NoCam2Tick { frames }
        );
    }

    /// A head that shows neither the rig NOR a self-marked run (a black or slate pre-roll, NDI
    /// inputs still reconnecting after an OBS restart) proves nothing: the recording goes on to
    /// the unchanged full decode, which reads its later frames as before.
    #[test]
    fn a_pre_roll_head_goes_on_to_the_full_decode_1404() {
        let frames = PAINTER_HEAD_FRAMES as usize;
        assert_eq!(
            painter_head_verdict(&head(frames, &[])),
            PainterHead::FullDecode,
            "a head that read nothing at all"
        );
        assert_eq!(
            painter_head_verdict(&head(frames, &[CG_OBS])),
            PainterHead::FullDecode,
            "a reserved cg-path burn alone is not the clip"
        );
    }

    #[test]
    fn a_short_burn_free_recording_stops_after_its_last_frame_1404() {
        assert_eq!(
            painter_head_verdict(&head(40, &[CLIP])),
            PainterHead::NoCam2Tick { frames: 40 }
        );
    }

    #[test]
    fn a_rig_recording_is_always_decoded_in_full_1404() {
        let n = PAINTER_HEAD_FRAMES as usize;
        // a stream recording whose optical is unreadable for the whole head: its burns decide
        assert_eq!(
            painter_head_verdict(&head(n, &[STRIH, STREAM])),
            PainterHead::FullDecode
        );
        // a strih recording (cam1 + strih, never stream)
        assert_eq!(
            painter_head_verdict(&head(n, &[CAM1, STRIH])),
            PainterHead::FullDecode
        );
        // ONE burn on the LAST head frame is enough, next to the clip's own QRs
        let mut late = head(n, &[CLIP, CLIP]);
        late[n - 1].1.push(CAM1);
        assert_eq!(painter_head_verdict(&late), PainterHead::FullDecode);
        // a cam2 tick without any burn (a burn-free painter recording)
        let mut ticked = head(n, &[]);
        ticked[7].0 = Some(4242);
        assert_eq!(painter_head_verdict(&ticked), PainterHead::FullDecode);
        // ONE cam2 tick is enough even when every head frame also shows the clip
        let mut ticked_next_to_the_clip = head(n, &[CLIP, CLIP]);
        ticked_next_to_the_clip[7].0 = Some(4242);
        assert_eq!(
            painter_head_verdict(&ticked_next_to_the_clip),
            PainterHead::FullDecode
        );
    }

    #[test]
    fn an_empty_head_never_stops_the_painter_path_1404() {
        assert_eq!(painter_head_verdict(&[]), PainterHead::FullDecode);
    }

    #[test]
    fn the_no_tick_message_names_the_frames_the_burns_and_the_av_run_way_1404() {
        let m = no_cam2_tick_message(60);
        for want in [
            "no cam2 painter tick",
            "60 frames",
            "911001",
            "measurement clip",
            "--av-run 911016",
        ] {
            assert!(m.contains(want), "{want:?} in {m}");
        }
    }

    /// The probe glue wiring (it compiles in CI only): the painter path runs the head check, and runs
    /// it BEFORE the full decode, which decodes with this plan's request.
    #[test]
    fn the_painter_head_runs_before_the_full_decode_1404() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let glue = std::fs::read_to_string(root.join("src/probe/av_sync_recording.rs"))
            .expect("src/probe/av_sync_recording.rs");
        let body = glue
            .split("pub fn av_sync_from_recording(")
            .nth(1)
            .expect("av_sync_from_recording in the glue");
        let head = body
            .find("if av_run.is_none() {\n        check_painter_head(recording)?;\n    }")
            .expect("the painter path runs the head check");
        let request = body
            .find("let request = av_decode_request(av_run);")
            .expect("the full decode request comes from the plan");
        let decode = body
            .find("analyze_recording_with_grouped_burns_optical(\n        recording,\n        &request.mandatory_burns,")
            .expect("the full decode uses the plan's request");
        assert!(
            head < request && request < decode,
            "head check, then the plan's request, then the full decode"
        );
        assert!(
            !body.contains("analyze_recording(recording)"),
            "the --av-sync decode must go through the plan's request"
        );
    }

    #[test]
    fn the_painter_burns_mirror_the_probe_ids_1404() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let read = |p: &str| std::fs::read_to_string(root.join(p)).expect(p);
        let latency = read("src/probe/recording_latency.rs");
        for (name, id) in [("CAM1", CAM1), ("STRIH", STRIH), ("STREAM", STREAM)] {
            let decl = format!("pub const BURN_RUN_ID_{name}: u32 = {id};");
            assert!(latency.contains(&decl), "{decl}");
        }
        let recording = read("src/probe/recording.rs");
        let generic = recording
            .split("const GENERIC_DIAGNOSTIC_BURN_IDS: [u32; 3] = [")
            .nth(1)
            .and_then(|rest| rest.split("];").next())
            .expect("GENERIC_DIAGNOSTIC_BURN_IDS in probe/recording.rs");
        let names: Vec<&str> = generic
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(
            names,
            [
                "crate::probe::recording_latency::BURN_RUN_ID_CAM1",
                "crate::probe::recording_latency::BURN_RUN_ID_STRIH",
                "crate::probe::recording_latency::BURN_RUN_ID_STREAM",
            ],
            "the painter path's set is the one analyze_recording decodes with"
        );
    }
}
