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
//! - The painter path first reads a short head with a request that names nothing, so no head
//!   frame goes robust. A head that shows the self-marked measurement clip and neither a cam2
//!   tick nor any of the [`PAINTER_PATH_NODE_BURNS`] rig burns is the clip's own recording: the
//!   path stops there ([`PainterHead::NoCam2Tick`]) instead of decoding every frame through the
//!   robust recovery. Any rig frame in the head, and any head without the clip (a QR-less
//!   pre-roll), goes on to the full decode. Two residual edges, both accepted: a burn-free
//!   recording whose head shows only the clip but which later switches to painter content is
//!   refused; and a clip-only recording that opens on a QR-less pre-roll longer than the head is
//!   decoded in full through the robust recovery, then fails on the coverage guard.
//! - The painter path's full decode asks for exactly the node burns the head read
//!   ([`painter_full_request`], ROZHODNUTÉ 6051603225). The YouTube-leg stream recordings of the
//!   5.10 sessions ran with the burns off: every frame carries only the painter run and the aux
//!   pair, so the old cam1/strih/stream request sent all 1200 frames of each window through the
//!   robust recovery (the video decode alone: 101-190 s instead of 32-58 s per 40 s clip, 8
//!   workers). A burns-on rig recording keeps every strih / stream burn its head read mandatory and
//!   gets the camera group as the issue-632 any-of group, so the deployed camera's burn (whichever
//!   camera) satisfies it.
//!
//!   Why the result cannot move: `--av-sync` pairs the painter tick with the audio marker, and
//!   node burns are tick-excluded. A frame the new request keeps on the fast path skips the
//!   bottom-band tiles (the bottom 45 % of the frame) and the burn slot looks. Neither can hold a
//!   whole dual-QR primary, which spans more than half the frame height on the rig. A
//!   cam1-deployed frame always took this same fast path under the old request. Measured: on the
//!   10 800 frames of the 9 re-made stream-recording fixtures, no frame read a different optical
//!   payload. Every frame is fast now, and the output stayed byte-identical (issue 1404 comment
//!   6051594115).
//!
//! Pure (no I/O). `probe::av_sync_recording` decodes the head and the recording with these
//! requests; `probe::recording::analyze_recording_head` stops ffmpeg after the head.

/// The painter path's rig burns: cam1, strih, stream (the `recording_latency` run ids 911001 /
/// 911002 / 911004). It mirrors `probe::recording::GENERIC_DIAGNOSTIC_BURN_IDS`, the set
/// `analyze_recording` decodes with. The head verdict reads them as a rig signal; the full decode
/// requires all three only after a head that read no QR, and otherwise requires the strih / stream
/// members the head read ([`painter_full_request`]).
pub const PAINTER_PATH_NODE_BURNS: [u32; 3] = [911_001, 911_002, 911_004];

/// The dual-QR Vernier shows two halves of its run on a frame (the clip's frame 0 shows one).
pub const DUAL_QR_HALVES: usize = 2;

/// How many leading frames the painter path reads before its full decode: 2 s of a 30 fps
/// recording. A rig recording already shows a node burn and a cam2 tick on its first frame, but the
/// head still decodes all of these frames on the fast path (about 7 CPU-s in release at 1080p,
/// about 4x that for a 4K strih recording) before the full decode starts.
pub const PAINTER_HEAD_FRAMES: u64 = 60;

/// What one decode asks the #207 fast-path gate for (the arguments of
/// `probe::recording::analyze_recording_with_grouped_burns_optical`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvDecodeRequest {
    /// Node burns that must ALL decode on a frame before the fast path may skip the recovery.
    pub mandatory_burns: Vec<u32>,
    /// The issue-632 any-of group: ONE of these must decode. Empty = no such group.
    pub any_of_burns: Vec<u32>,
    /// `Some((run, n))`: the fast path also needs `n` distinct `frame_id`s of `run`.
    pub min_distinct_optical: Option<(u32, usize)>,
}

/// The decode request of `--av-sync`: `Some(run)` = `--av-run <run>`; `None` = the painter path's
/// request when its head read no QR (all of [`PAINTER_PATH_NODE_BURNS`]). Every other painter-path
/// request comes from [`painter_full_request`].
pub fn av_decode_request(av_run: Option<u32>) -> AvDecodeRequest {
    match av_run {
        Some(run) => AvDecodeRequest {
            mandatory_burns: Vec::new(),
            any_of_burns: Vec::new(),
            min_distinct_optical: Some((run, DUAL_QR_HALVES)),
        },
        None => painter_request(),
    }
}

/// The request of the painter path's full decode.
fn painter_request() -> AvDecodeRequest {
    AvDecodeRequest {
        mandatory_burns: PAINTER_PATH_NODE_BURNS.to_vec(),
        any_of_burns: Vec::new(),
        min_distinct_optical: None,
    }
}

/// The painter path's full-decode request, built from what its head read (ROZHODNUTÉ 6051603225).
/// `head` holds one `(cam2 tick, run ids read)` per head frame, as for [`painter_head_verdict`];
/// `node_burn_table` is the probe's one node-burn table (`probe::recording::NODE_BURN_RUN_IDS`).
///
/// The request asks for exactly the node burns the head saw:
/// - the hop burns of [`PAINTER_PATH_NODE_BURNS`] (strih, stream) the head read are mandatory;
/// - the camera group (every camera capture burn of the table, by its
///   `crate::burn_regions::slot_for_run_id` slot) is the any-of group, only when the head read one
///   of them;
/// - a head that read QRs but no node burn (a burns-off recording) requires nothing;
/// - a head that read no QR at all (an empty head, a QR-less pre-roll) keeps today's request
///   ([`av_decode_request`] `(None)`).
pub fn painter_full_request(
    head: &[(Option<u32>, Vec<u32>)],
    node_burn_table: &[u32],
) -> AvDecodeRequest {
    let read_a_qr = head
        .iter()
        .any(|(tick, run_ids)| tick.is_some() || !run_ids.is_empty());
    if !read_a_qr {
        return painter_request();
    }
    let read = |id: u32| head.iter().any(|(_, run_ids)| run_ids.contains(&id));
    let mandatory_burns: Vec<u32> = PAINTER_PATH_NODE_BURNS
        .iter()
        .copied()
        .filter(|&id| !is_camera_burn(id) && read(id))
        .collect();
    let cameras: Vec<u32> = node_burn_table
        .iter()
        .copied()
        .filter(|&id| is_camera_burn(id))
        .collect();
    let any_of_burns = if cameras.iter().any(|&id| read(id)) {
        cameras
    } else {
        Vec::new()
    };
    AvDecodeRequest {
        mandatory_burns,
        any_of_burns,
        min_distinct_optical: None,
    }
}

/// A camera capture burn: its overlay slot is the centred camera slot (`crate::burn_regions`).
fn is_camera_burn(run_id: u32) -> bool {
    crate::burn_regions::slot_for_run_id(run_id)
        == Some(crate::burn_regions::BurnSlot::CameraCapture)
}

/// The request of the painter path's head: nothing required, so every head frame stays on the fast
/// path (plain + Otsu, plus the #754 top-band look on a frame short of a dual-QR). The head only
/// has to show whether a cam2 tick or a rig burn is there; the plain pass reads both.
pub fn painter_head_request() -> AvDecodeRequest {
    AvDecodeRequest {
        mandatory_burns: Vec::new(),
        any_of_burns: Vec::new(),
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
    /// cam3's capture burn.
    const CAM3: u32 = 911_008;
    /// imag's render burn and the SongPlayer origin burn: node burns outside the painter set.
    const IMAG: u32 = 911_003;
    const SONGPLAYER: u32 = 911_014;
    /// The painted aux tick pair: tick-excluded, no overlay slot.
    const AUX: u32 = 911_013;
    /// The painter run of the 5.10 session-1 recording.
    const PAINTER: u32 = 1_791_214_272;
    /// `probe::recording::NODE_BURN_RUN_IDS` (cam1, cam2, cam3, cam4, cam5, cam6, cam7, strih,
    /// stream, imag, SongPlayer, cg, aux, clip), pinned to that table below.
    const TABLE: [u32; 14] = [
        911_001, 911_009, 911_008, 911_007, 911_010, 911_011, 911_012, 911_002, 911_004, 911_003,
        911_014, 911_015, 911_013, 911_016,
    ];
    /// The table's camera capture burns, in table order: the any-of group.
    const CAMERAS: [u32; 7] = [
        911_001, 911_009, 911_008, 911_007, 911_010, 911_011, 911_012,
    ];

    /// `n` head frames, each with a cam2 tick and the run ids `runs`.
    fn ticked_head(n: usize, runs: &[u32]) -> Vec<(Option<u32>, Vec<u32>)> {
        (0..n)
            .map(|i| (Some(4_000 + 2 * i as u32), runs.to_vec()))
            .collect()
    }

    fn request(mandatory: &[u32], any_of: &[u32]) -> AvDecodeRequest {
        AvDecodeRequest {
            mandatory_burns: mandatory.to_vec(),
            any_of_burns: any_of.to_vec(),
            min_distinct_optical: None,
        }
    }

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
                any_of_burns: vec![],
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
                any_of_burns: vec![],
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
                any_of_burns: vec![],
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

    /// ROZHODNUTÉ 6051603225: a burns-off recording (the 5.10 YouTube-leg sessions: the painter run
    /// and the aux pair on every frame, no node burn) requires nothing, so its frames stay on the
    /// fast path instead of chasing burns it cannot carry.
    #[test]
    fn a_burns_off_head_requires_nothing_1404() {
        let n = PAINTER_HEAD_FRAMES as usize;
        assert_eq!(
            painter_full_request(&ticked_head(n, &[PAINTER, PAINTER, AUX, AUX]), &TABLE),
            request(&[], &[])
        );
        // a head whose optical never read (the aux pair only) proves the same: QRs, no node burn
        assert_eq!(
            painter_full_request(&head(n, &[AUX]), &TABLE),
            request(&[], &[])
        );
    }

    /// A burns-on rig recording keeps every burn its head read: the hop burns stay mandatory and
    /// the camera slot is the any-of group, so the deployed camera's burn (cam3 here, never cam1)
    /// satisfies it.
    #[test]
    fn a_burns_on_head_keeps_its_hop_burns_and_the_camera_group_1404() {
        let n = PAINTER_HEAD_FRAMES as usize;
        assert_eq!(
            painter_full_request(
                &ticked_head(n, &[PAINTER, PAINTER, STRIH, STREAM, CAM3]),
                &TABLE
            ),
            request(&[STRIH, STREAM], &CAMERAS)
        );
        // cam1 deployed: the same request (cam1 is one member of the group, no longer mandatory)
        assert_eq!(
            painter_full_request(&ticked_head(n, &[PAINTER, STRIH, STREAM, CAM1]), &TABLE),
            request(&[STRIH, STREAM], &CAMERAS)
        );
    }

    /// Only the burns the head read: a strih recording (camera + strih, never stream), a head with
    /// no camera burn (no any-of group), and one burn on ONE head frame is enough.
    #[test]
    fn only_the_burns_the_head_read_are_requested_1404() {
        let n = PAINTER_HEAD_FRAMES as usize;
        assert_eq!(
            painter_full_request(&ticked_head(n, &[PAINTER, CAM3, STRIH]), &TABLE),
            request(&[STRIH], &CAMERAS)
        );
        assert_eq!(
            painter_full_request(&ticked_head(n, &[PAINTER, STRIH, STREAM]), &TABLE),
            request(&[STRIH, STREAM], &[])
        );
        let mut one_frame = ticked_head(n, &[PAINTER]);
        one_frame[n - 1].1.extend([STREAM, CAM3]);
        assert_eq!(
            painter_full_request(&one_frame, &TABLE),
            request(&[STREAM], &CAMERAS)
        );
    }

    /// Node burns outside the painter set (imag, the SongPlayer origin, the cg OBS hop) are never
    /// required: today's request never asked for them either.
    #[test]
    fn burns_outside_the_painter_set_are_never_required_1404() {
        let n = PAINTER_HEAD_FRAMES as usize;
        assert_eq!(
            painter_full_request(
                &ticked_head(n, &[PAINTER, IMAG, SONGPLAYER, CG_OBS]),
                &TABLE
            ),
            request(&[], &[])
        );
    }

    /// A head that read no QR at all proves nothing about the recording (an empty head, a black
    /// or slate pre-roll): today's request, unchanged.
    #[test]
    fn a_head_that_read_no_qr_keeps_todays_request_1404() {
        let today = av_decode_request(None);
        assert_eq!(painter_full_request(&[], &TABLE), today);
        let n = PAINTER_HEAD_FRAMES as usize;
        assert_eq!(painter_full_request(&head(n, &[]), &TABLE), today);
    }

    /// The any-of group is the table's camera capture burns, by their burn slot: an id the table
    /// does not list is no camera here, and a table without cameras gives no group.
    #[test]
    fn the_camera_group_is_the_tables_camera_slots_1404() {
        let n = PAINTER_HEAD_FRAMES as usize;
        let cam3_head = ticked_head(n, &[PAINTER, CAM3]);
        assert_eq!(
            painter_full_request(&cam3_head, &TABLE).any_of_burns,
            CAMERAS
        );
        let no_cameras: Vec<u32> = TABLE
            .iter()
            .copied()
            .filter(|id| !CAMERAS.contains(id))
            .collect();
        assert_eq!(
            painter_full_request(&cam3_head, &no_cameras),
            request(&[], &[])
        );
    }

    /// `TABLE` is the probe's `NODE_BURN_RUN_IDS`, resolved through the `recording_latency` consts.
    #[test]
    fn the_table_is_the_probe_node_burn_table_1404() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let read = |p: &str| std::fs::read_to_string(root.join(p)).expect(p);
        let latency = read("src/probe/recording_latency.rs");
        let recording = read("src/probe/recording.rs");
        let table = recording
            .split("pub const NODE_BURN_RUN_IDS: [u32; 14] = [")
            .nth(1)
            .and_then(|rest| rest.split("];").next())
            .expect("NODE_BURN_RUN_IDS in probe/recording.rs");
        let ids: Vec<u32> = table
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("//"))
            .map(|l| {
                let name = l
                    .trim_end_matches(',')
                    .strip_prefix("crate::probe::recording_latency::")
                    .unwrap_or_else(|| panic!("a recording_latency const: {l}"));
                let decl = format!("pub const {name}: u32 = ");
                latency
                    .split(&decl)
                    .nth(1)
                    .and_then(|rest| rest.split(';').next())
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or_else(|| panic!("{decl}"))
            })
            .collect();
        assert_eq!(ids, TABLE);
    }

    /// The probe glue wiring (it compiles in CI only): the painter path reads its head and builds the
    /// full-decode request from what the head read, with the probe's one node-burn table;
    /// `--av-run` keeps its own request; the full decode passes both groups of the request.
    #[test]
    fn the_full_decode_asks_for_what_the_painter_head_read_1404() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let glue = std::fs::read_to_string(root.join("src/probe/av_sync_recording.rs"))
            .expect("src/probe/av_sync_recording.rs");
        let body = glue
            .split("pub fn av_sync_from_recording(")
            .nth(1)
            .expect("av_sync_from_recording in the glue");
        let request = body
            .find(concat!(
                "let request = match av_run {\n",
                "        Some(_) => av_decode_request(av_run),\n",
                "        None => painter_full_request(&check_painter_head(recording)?, &NODE_BURN_RUN_IDS),\n",
                "    };",
            ))
            .expect("the painter path's request comes from its head and the node-burn table");
        let decode = body
            .find(concat!(
                "analyze_recording_with_grouped_burns_optical(\n",
                "        recording,\n",
                "        &request.mandatory_burns,\n",
                "        &request.any_of_burns,\n",
                "        request.min_distinct_optical,\n",
                "    )",
            ))
            .expect("the full decode uses both groups of the plan's request");
        assert!(request < decode, "the request, then the full decode");
        assert_eq!(
            body.matches("check_painter_head(").count(),
            1,
            "the head is read once, on the painter path only"
        );
        assert!(
            !body.contains("analyze_recording(recording)"),
            "the --av-sync decode must go through the plan's request"
        );
        let imports = glue
            .split("use crate::probe::recording::{")
            .nth(1)
            .and_then(|rest| rest.split("};").next())
            .expect("the probe::recording import");
        assert!(
            imports.contains("NODE_BURN_RUN_IDS"),
            "the table is the probe's one node-burn table: {imports}"
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
