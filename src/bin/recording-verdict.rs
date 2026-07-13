//! recording-verdict: #107 hard-fail loss verdict from recorded OBS program files.
//!
//! Consumes the #106 recorded-file decode (NOT an NDI tap, NOT the lz4 spool) and
//! produces a HARD-FAIL zero-loss verdict, with exactly ONE calibrated tolerance (#376):
//!
//!   PASS (#186) = EVERY node's burn-id sequence is CONTIGUOUS (no missing id — a
//!   missing id, incl. a BURN-UNREADABLE one, FAILS) AND (#363/#376) the cam2 OPTICAL dual-QR
//!   read is COMPLETE across the span WITHIN the rig's calibrated moiré floor (undecodable RATE
//!   <= [`OPTICAL_UNDECODABLE_RATE_MAX`] — the real camera-captured pixel path is the HARD gate,
//!   ABOVE the floor it FAILS same as before) AND (when `--cam1-capture-stats` is given)
//!   cam2→cam1 V4L2 capture-drop = 0. The digital node burns are injected AFTER capture, so
//!   they prove node→node DIGITAL delivery only — they can NEVER substitute for the optical
//!   read (reverts the #360 weakening that let a digital-burn-only frame pass the gate). The
//!   per-recording 60→30 beat metrics and the analyzed span (`--min-secs`) are DIAGNOSTIC
//!   only — they are reported for context but do NOT gate the headline.
//!
//! The free-running 60→30 camera-sampling beat (mean step exactly 2.0, symmetric)
//! is recognized and NOT counted as loss; the burn-id contiguity is the trustworthy verdict.
//!
//! Usage:
//!   recording-verdict --strih strih.mkv [--stream stream.mkv] \
//!       [--painter painter.csv] [--out-dir run-dir] [--min-secs 300]
//!   recording-verdict --extract-partial <strih|stream> --strih|--stream <local rec> \
//!       --out partial.json        # #208 decode ONE box's recording in place
//!   recording-verdict --merge-partials strih=a.json --merge-partials stream=b.json \
//!       [--painter …] [--json …]  # #208 merge the per-box partials on dev1
//!
//! - `--strih` the strih OBS-program recording (the strict hop-1 endpoint).
//! - `--stream` the stream OBS-program recording (the headline endpoint). When
//!   present, the strih→stream hop is verdicted by a direct per-frame tick compare
//!   (the camera beat is common, so it cancels).
//! - `--painter` a CSV of the cam2 painter's displayed logical ticks (one `tick`
//!   per line or the recording-probe `frame_index,n_qr,tick,...` CSV). Enables the
//!   honest cam→strih assessment (no false zero claim).
//! - `--out-dir` where pixel-proof PNGs of every flagged frame are written.
//!
//! Exit code: 0 on PASS for every verdict, non-zero on ANY fail.

use anyhow::{Context, Result};
use camera_box::av_window::{self, AvSyncVerdict};
use camera_box::probe::av_sync_recording::{decode_av_marker_inputs, AvMarkerInputs};
use camera_box::probe::burn_contiguity::{
    burn_contiguity_in_window_with_step, burn_contiguity_in_window_with_step_and_schedule,
    BurnRate, InWindowMissingKind, NodeContiguity, RecordedBurnFrame,
};
use camera_box::probe::recording::{
    analyze_recording_with_burns, analyze_recording_with_grouped_burns_optical,
    extract_frames_png, select_frames_to_extract, RecordingFrame, DEFAULT_MAX_PIXEL_PROOF,
};
use camera_box::probe::recording_latency::{
    burn_ids_in, burn_ids_with_frame_index_in, cam2_cam1_samples, cam2_cam1_samples_from_burn,
    cam2_cam1_samples_from_flip, cam_strih_samples, chain_hop_samples_from_stream, hop_latency,
    n_camera_strih_samples, painter_internal_gen_to_flip, per_frame_latency_csv_rows,
    strih_stream_samples, strih_stream_samples_from_stream, write_latency_csv, HopLatency,
    LatencySample, RunIds, BURN_RUN_ID_CAM1, BURN_RUN_ID_CAM2, BURN_RUN_ID_CAM3, BURN_RUN_ID_CAM4,
    BURN_RUN_ID_CAM5, BURN_RUN_ID_CAM6, BURN_RUN_ID_IMAG, BURN_RUN_ID_STREAM, BURN_RUN_ID_STRIH,
};
use camera_box::probe::recording_partial::RecordingPartial;
use camera_box::probe::recording_segments::{
    load_switch_schedule, place_frame_in_window, segment_continuity, SegmentFrame, SwitchWindow,
    WindowPlacement, DEFAULT_TRANSITION_GUARD_NS,
};
use camera_box::probe::recording_verdict::{
    cam_strih_assessment, verdict, FrameTick, RecordingVerdict, VerdictConfig,
};
use camera_box::qpsk_marker::{av_offset_candidates_deduped, DEDUPE_SAME_FID_WINDOW_S};
use clap::Parser;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(about = "Hard-fail zero-loss verdict from recorded OBS program files (#107)")]
struct Args {
    /// strih OBS-program recording (.mkv / .mp4) — the strict hop-1 endpoint.
    /// Optional: omit it for a cam1-ONLY optical-readability check (the fast PART-B
    /// loop — `--cam1 grab.mkv --painter painter.csv --cam2-run-id N`), which decodes
    /// just the cam1 grab and reports the cam2→cam1 decode rate without a 4-node run.
    #[arg(long)]
    strih: Option<PathBuf>,
    /// stream OBS-program recording — the headline endpoint. Enables strih→stream.
    #[arg(long)]
    stream: Option<PathBuf>,
    /// #461/#463 imag-nb OBS-program recording (EPIC #466 Topology v2, the new 60fps
    /// low-latency IMAG box). Its zero-loss proof is the cam2 OPTICAL tick's own first..=last
    /// contiguity (no 60→30 beat: imag captures the 60Hz painter 1:1 at 60fps) ANDed with its
    /// OWN digital corner burn's contiguity (run_id [`BURN_RUN_ID_IMAG`] = 911003, #463) WHEN
    /// that burn is present in the recording — a recording with no burn decoded at all (a build
    /// not yet carrying the corner burn) falls back to the optical proof alone. Independent of
    /// --strih/--stream; may be supplied alone or alongside them.
    #[arg(long)]
    imag: Option<PathBuf>,
    /// cam1 GRAB recording (#105 node 2) — the camera-box `--record-grab` mkv of
    /// cam1's filmed frames. Enables the STRICT cam1→strih hop verdict and the HONEST
    /// cam2→cam1 optical assessment (and, with --cam1-grab-ts, the cam2→cam1 latency).
    #[arg(long)]
    cam1: Option<PathBuf>,
    /// cam1 grab-timestamp SIDECAR CSV (`frame_index,grab_ts_ns`) the `--record-grab`
    /// mode writes — cam1's per-frame GRAB instant on the DanteSync wall clock. With
    /// --cam1 it yields the REAL cam2→cam1 optical+grab latency (no #111 burn needed).
    #[arg(long)]
    cam1_grab_ts: Option<PathBuf>,
    /// CSV of the cam2 painter's displayed ticks (enables the cam→strih assessment).
    #[arg(long)]
    painter: Option<PathBuf>,
    /// cam1 CAPTURE-STATS sidecar (`v4l2_dropped=N`, `frames_captured=M`) the camera-box
    /// writes on shutdown — cam1's V4L2 capture-drop count. The verdict reports it as the
    /// cam2→cam1 LOSS (the camera leg: a dropped capture = a lost frame), NOT a painter-tick
    /// optical compare (confounded by the 60→30 decimation).
    #[arg(long)]
    cam1_capture_stats: Option<PathBuf>,
    /// Directory for pixel-proof PNGs of flagged frames.
    #[arg(long, default_value = "recording-verdict-run")]
    out_dir: PathBuf,
    /// Minimum analyzed span (s) before a zero-loss PASS may be declared.
    #[arg(long, default_value_t = 300.0)]
    min_secs: f64,
    /// Cap on pixel-proof PNGs written per recording (the first N flagged frames by
    /// index). The verdict needs only a handful of visual examples; extracting
    /// thousands of PNGs was a large slice of the runtime (#166). `0` = no cap.
    #[arg(long, default_value_t = DEFAULT_MAX_PIXEL_PROOF)]
    max_pixel_proof: usize,
    /// Camera capture fps (for the duration gate).
    #[arg(long, default_value_t = 30.0)]
    capture_fps: f64,
    /// Monitor refresh Hz of the painted logical counter.
    #[arg(long, default_value_t = 60.0)]
    refresh_hz: f64,
    /// #11 → #360: the strih OBS render/emit fps. RIG-PINNED constant, never user-tuned. It used to
    /// derive the strih burn's decimation step (`round(strih_emit_fps / stream_capture_fps)` =
    /// 60/30 = 2), but #360 found the strih burn is a FREE-RUNNING render tick with an IRREGULAR
    /// per-frame step (NOT a clean 2), so its forward gaps are jitter, not loss — strih now uses
    /// gap-ignore (see `node_render_step`). This flag is RETAINED on the CLI for provenance/
    /// back-compat; it no longer drives the strih loss step. The struct default here is unused in
    /// practice — the harness (recording-e2e.sh) always threads an explicit value from
    /// STRIH_CAPTURE_FPS, which since Topology v2 (#459) is 30 (strih's own cut-to-stream canvas
    /// rate, not the pre-#459 60fps LED-wall IMAG rate this default historically mirrored).
    #[arg(long, default_value_t = 60.0)]
    strih_emit_fps: f64,
    /// #11 mixed 60/30: the fps the STREAM recording was captured at — the stream OBS output rate
    /// after it decimates the 60fps strih feed to 30. The strih burn is read FROM the stream
    /// recording, so this is the denominator of its decimation step (60/30 = 2). RIG-PINNED, never
    /// user-tuned. NOTE this is DISTINCT from `--capture-fps` (the diagnostic span / optical-beat
    /// rate, which the harness sets per recording — 60 for the strih recording's cam1 diagnostic):
    /// the decimation LOSS step is a topology constant, decoupled from the diagnostic so it is
    /// always correct regardless of which recording's `--capture-fps` is in effect. The stream burn
    /// is emitted AND recorded by the same stream OBS, so its own step is always 1. Default 30.
    #[arg(long, default_value_t = 30.0)]
    stream_capture_fps: f64,
    /// #461: the fps imag-nb's OWN recording was captured at (EPIC #466 Topology v2) — imag's
    /// #373 analyzed-span duration floor is computed against ITS OWN rate, never strih's/
    /// stream's (`recording_span_gate::node_capture_fps`'s third rate slot). RIG-PINNED, never
    /// user-tuned. Default 60 (imag-nb is a 60fps low-latency IMAG box).
    #[arg(long, default_value_t = 60.0)]
    imag_capture_fps: f64,
    /// #108 per-hop ABSOLUTE latency: the strih node's burn-QR run_id (the reserved per-box
    /// constant the burn filter derives from the host role, #257; default mirrors the #111 filter).
    /// When present in the strih recording, cam→strih latency is computed
    /// (strih_burn.gen_ts_ns − cam2.gen_ts_ns).
    #[arg(long, default_value_t = BURN_RUN_ID_STRIH)]
    burn_strih_run_id: u32,
    /// #108 per-hop ABSOLUTE latency: the stream node's burn-QR run_id. When both
    /// recordings carry their node burn, strih→stream latency is computed
    /// (stream_burn.gen_ts_ns − strih_burn.gen_ts_ns, paired by cam2 tick).
    #[arg(long, default_value_t = BURN_RUN_ID_STREAM)]
    burn_stream_run_id: u32,
    /// #174: the cam1-CAPTURE burn run_id (the value `CAMERA_BOX_BURN_RUN_ID` was set to
    /// on cam1). cam1's render-time burn rides through NDI into strih's program and on into
    /// stream's, so the SINGLE stream recording carries it; when present, the full chain
    /// cam1→strih→stream is verdicted by the CLEAN digital burn-id (loss + latency), with
    /// no 60→30 optical-beat ambiguity. When absent in the recording these hops report no
    /// samples (never a wrong number). Default mirrors the cam1 burn's reserved id.
    #[arg(long, default_value_t = BURN_RUN_ID_CAM1)]
    burn_cam1_run_id: u32,
    /// #24: cam3's capture-burn run_id — mirrors `--burn-cam1-run-id` exactly (the SAME #174
    /// capture-burn mechanism, running on cam3 instead of cam1). cam1/cam3/cam4 occupy the SAME
    /// "camera under test" role and are mutually exclusive in any real run (only the camera
    /// actually deployed with `CAMERA_BOX_BURN_RUN_ID` set produces a non-empty id set) — when
    /// absent, cam3 is silently skipped exactly like cam1 is today when its burn is off.
    ///
    /// Defaults to [`BURN_RUN_ID_CAM3`] — a fresh, unique id reserved for cam3 (#24). #463
    /// renamed the OLD `BURN_RUN_ID_CAM3` constant to [`BURN_RUN_ID_IMAG`] and repurposed 911003
    /// for imag-nb's own digital corner burn, which left this default numerically colliding with
    /// it until this fix; the two mechanisms are told apart by run_id alone again.
    #[arg(long, default_value_t = BURN_RUN_ID_CAM3)]
    burn_cam3_run_id: u32,
    /// #24: cam4's capture-burn run_id. See `--burn-cam3-run-id`.
    #[arg(long, default_value_t = BURN_RUN_ID_CAM4)]
    burn_cam4_run_id: u32,
    /// #312: cam2's OWN capture-burn run_id — mirrors `--burn-cam3-run-id` exactly, but on the
    /// fixed dual-QR PAINTER box (its camera-box daemon keeps capturing+emitting throughout a
    /// TEST run since #291; only its framebuffer is freed for the separate painter process).
    /// Defaults to [`BURN_RUN_ID_CAM2`].
    #[arg(long, default_value_t = BURN_RUN_ID_CAM2)]
    burn_cam2_run_id: u32,
    /// #312: cam5's capture-burn run_id (fleet growth 4→6, #451). See `--burn-cam3-run-id`.
    #[arg(long, default_value_t = BURN_RUN_ID_CAM5)]
    burn_cam5_run_id: u32,
    /// #312: cam6's capture-burn run_id (fleet growth 4→6, #451). See `--burn-cam3-run-id`.
    #[arg(long, default_value_t = BURN_RUN_ID_CAM6)]
    burn_cam6_run_id: u32,
    /// #108: cam2's painter run_id (the `--run-id` the cam2 painter used). When set,
    /// cam2's QR is matched EXACTLY by this run_id, so the strih burn forwarded into
    /// the stream recording can NEVER be mistaken for cam2. Strongly recommended for
    /// strih→stream. Unset (0) ⇒ cam2 = the first non-burn QR (safe for the strih
    /// recording, which has no foreign burn).
    #[arg(long, default_value_t = 0)]
    cam2_run_id: u32,
    /// #105 4-node report: write a machine-readable JSON summary (per-node verdict +
    /// per-hop loss + per-hop latency) to this path, consumed by
    /// scripts/recording-e2e-report.py to render the 2-graph report PNG.
    #[arg(long)]
    json: Option<PathBuf>,
    /// #209/#216: write a PER-FRAME latency time-series CSV to this path — one row per
    /// delivered stream frame: `frame_id,gen_ts_ns,flip_ts_ns,cam1_strih_ms,
    /// strih_stream_ms,cam1_stream_ms,cam2_cam1_ms`. This is the LITERAL continuous-line proof
    /// input: `scripts/latency-line-report.py --csv <path>` draws one line per hop (time on x,
    /// latency on y) — the three burn hops draw continuously, while the cam2→cam1 OPTICAL line
    /// (`cam2_cam1_ms`) GAPS honestly where the cam1 camera could not read the cam2 QR (#216).
    /// A gap in a burn line = a lost frame; a gap in cam2→cam1 = an optical-read dropout (not a
    /// chain loss). Defaults to `latency-per-frame.csv` BESIDE the `--json` summary (the JSON's
    /// own directory, NOT `--out-dir`) when `--json` is given but `--latency-csv` is not,
    /// so the time-series sits next to the summary. Requires the cam1/strih/stream burns
    /// in the stream recording (#174).
    #[arg(long)]
    latency_csv: Option<PathBuf>,
    /// #208 PER-BOX decode-in-place. Decode the ONE LOCAL recording on THIS box (passed via
    /// `--strih` for `strih`, `--stream` for `stream`, `--imag` for `imag`, #461) and write a
    /// small PARTIAL JSON (`--out`) of what the cross-box merge needs — the box's burn-id
    /// sequence(s) (empty for imag, which has none) with per-frame ids + timestamps + the cam2
    /// ticks it can see (ids + timestamps, NEVER frames/pixels). The strih recording is decoded ON
    /// the strih box, the stream recording ON the stream box, the imag recording ON the imag-nb
    /// box; a recording is NEVER copied box-to-box (nor to dev1) — only this small JSON moves.
    /// dev1 then runs `--merge-partials` to combine them. `<box>` is `strih`, `stream`, or `imag`.
    #[arg(long, value_name = "BOX")]
    extract_partial: Option<String>,
    /// #208: where `--extract-partial` writes the partial JSON. Default: `partial-<box>.json`.
    #[arg(long)]
    out: Option<PathBuf>,
    /// #208 MERGE the per-box partials into the SAME full-chain verdict the fused path produces
    /// (cam2→cam1, cam1→strih, strih→stream, cam1 contiguity, all loss + latency). PASS is the
    /// #186 gate: EVERY node's burn-id sequence is CONTIGUOUS (no missing id — a missing id, incl.
    /// a BURN-UNREADABLE one, FAILS) AND (when `--cam1-capture-stats` is given) cam2→cam1 V4L2
    /// capture-drop = 0 AND the analyzed optical span clears the >= `--min-secs` duration floor
    /// (#373 — the merge path runs through the SAME `build_and_print_verdict`, so a collapsed
    /// optical span fails the merge headline too, not just the fused one). The per-recording
    /// undecodable 60→30 BEAT metric is DIAGNOSTIC only — it does NOT gate the headline. Repeat per box:
    /// `--merge-partials strih=<json> --merge-partials stream=<json>`. Combined with the small
    /// `--painter` / `--cam1-capture-stats` files (already on dev1) and written to `--json`. NO
    /// recording is read here — only the small partial JSONs.
    #[arg(long, value_name = "BOX=JSON")]
    merge_partials: Vec<String>,
    /// #312 Phase-1 ALL-CAMBOX per-segment continuity. Path to a switch-schedule JSON — an
    /// ordered, non-overlapping array of `{"cambox":<label>,"start_ns":<i64>,"end_ns":<i64>}`
    /// windows on the burn `gen_ts_ns` timeline (the harness logs the sequential program-switch
    /// wall-times). The SINGLE continuous stream recording's decoded frames are partitioned into
    /// these windows (discarding `--switch-guard-ns` on each side of every boundary) and the per-
    /// cambox PAINTED-tick continuity (undecodable / copies / gaps) is reported, gating the
    /// headline alongside the per-node burn verdict. Emitted under `all_cambox_continuity` in
    /// `--json`. Requires the stream recording (`--stream`).
    #[arg(long)]
    switch_schedule: Option<PathBuf>,
    /// #312: the transition guard discarded on EACH side of every schedule boundary (ns). The
    /// program switch + the 60→30 + latency take a few frames to settle; in-guard frames are
    /// excluded from attribution (NOT counted as loss). Default 1s.
    #[arg(long, default_value_t = DEFAULT_TRANSITION_GUARD_NS)]
    switch_guard_ns: i64,
    /// #312: the by-design decimation step of the painted tick in the stream recording (the
    /// painter increments per painted frame, captured at the recording rate). `0` = derive from
    /// `round(--refresh-hz / --stream-capture-fps)` (60/30 = 2 for the stream recording). Pass a
    /// fixed value to override. Kept a parameter so the continuity bakes no 30-vs-60 assumption.
    #[arg(long, default_value_t = 0)]
    switch_expected_step: i64,
    /// #364 — enable the per-camera COLOUR gate. When set, each node's recording (cam1 from the
    /// strih recording; strih/stream from the stream recording) is sampled for the #367 painted
    /// colour scale and a per-camera colour verdict is computed; ANY reference patch wrong on a
    /// majority of sampled frames FAILS the node (a HARD gate, mirrors the optical read). Default
    /// OFF so delivery-only runs are unchanged. Set it on the E2E run where the painter painted the
    /// colour scale (the harness enables it in TEST mode). On-host / fused only — a node with no
    /// recording on this host (merge mode) errors loudly rather than silently skipping the gate.
    #[arg(long)]
    colour_gate: bool,
    /// #364 — number of frames sampled per recording for the colour gate (input-seek, so the cost
    /// is bounded independent of the recording length). Default [`colour_sample::DEFAULT_COLOUR_SAMPLES`].
    #[arg(long, default_value_t = camera_box::probe::colour_sample::DEFAULT_COLOUR_SAMPLES)]
    colour_samples: usize,
    /// #188 A/V-SYNC MODE — measure the video↔audio offset from a recording that carries BOTH the
    /// cam2 dual-QR video AND the mbc audio marker (the stream OBS recording). Requires
    /// `--av-marker-log` (the cam2 emitter's `index,frame_id,emit_ts_ns` CSV). Prints the offset
    /// (video − audio, ms) and exits — a standalone mode, not part of the zero-loss verdict.
    #[arg(long)]
    av_sync: Option<PathBuf>,
    /// #188 A/V-sync: path to the cam2 QPSK emit log (`index,frame_id,emit_ts_ns`) from the painter's
    /// `--marker-log`. Required with `--av-sync`.
    #[arg(long)]
    av_marker_log: Option<PathBuf>,
    /// #188 A/V-sync: which audio track of the recording carries the mbc marker (0-based). The stream
    /// OBS records multiple tracks; pick the one with the hand-mic. Default 0.
    #[arg(long, default_value_t = 0)]
    av_audio_track: u32,
    /// #188 A/V-sync: QPSK preamble detection threshold (0..1). Lower = more sensitive on a weak/
    /// noisy mic'd recording. Default 0.35.
    #[arg(long, default_value_t = 0.35)]
    av_threshold: f64,
    /// #188 A/V-sync: minimum clustered markers required to report an offset. Default 4.
    #[arg(long, default_value_t = 4)]
    av_min_matched: usize,
    /// #188 A/V-sync: half-width (ms) of the offset cluster window. Candidate offsets within
    /// ±this of the densest band are the real markers; the rest (false decodes, wrong-lap matches)
    /// are rejected. #733 (2026-07-13) tightened the default from 60 to 25: a real-data audit of 3
    /// full-path-e2e gate runs found the OLD ±60ms window wide enough to occasionally swallow TWO
    /// nearby sub-clusters into one (one run's mad_ms hit 32.7ms — a genuinely bimodal-looking
    /// blend, not one tight cluster), while a tight-first sweep on the SAME real data showed each
    /// run's true cluster is far more precise than that: even at ±15ms every run cleared
    /// `MIN_AV_SAMPLES`(8) with mad_ms 7-9ms. ±25ms is the conservative middle ground — comfortable
    /// margin above the 8-sample floor (19-24 matched candidates on all 3 real runs) while staying
    /// far tighter than the old default (mad_ms 11-13ms vs 15-33ms). See `.claude/skills/av-sync`
    /// for the full audit.
    #[arg(long, default_value_t = 25.0)]
    av_cluster_tol_ms: f64,
    /// #624 deliverable 4 / #312 item 2 PR B: the expected/dialed A/V offset (ms) — the
    /// operator's live #398 dock reading (nominally ~0, since the dock is dialed to align video
    /// and audio). The per-camera A/V-offset gate measures each camera's DEVIATION from this
    /// value, never from a hardcoded 0 — so a rig intentionally dialed to a nonzero offset still
    /// gates correctly. Default 0.0 (the "operator dials to ~0 in practice" default case).
    #[arg(long, default_value_t = 0.0)]
    av_expected_ms: f64,
}

impl Args {
    /// cam2's pinned painter run_id as an `Option`, applying the `0 ⇒ unpinned` sentinel.
    /// SINGLE source of truth for the sentinel so every consumer (#108 latency pairing, the
    /// #273 optical-window boundary, the per-box extract flagging) agrees on `None` vs `Some`.
    fn cam2_pin(&self) -> Option<u32> {
        (self.cam2_run_id != 0).then_some(self.cam2_run_id)
    }
}

/// Reduce a HopLatency option to a compact JSON object (or null) for the report.
fn hop_lat_json(h: &Option<HopLatency>) -> serde_json::Value {
    match h {
        Some(h) => serde_json::json!({
            "samples": h.samples,
            "p50_ms": h.stats.p50_ms, "p95_ms": h.stats.p95_ms, "p99_ms": h.stats.p99_ms,
            "min_ms": h.stats.min_ms, "mean_ms": h.stats.mean_ms, "max_ms": h.stats.max_ms,
            "jitter_ms": h.jitter_ms, "drift_ms_per_min": h.drift_ms_per_min,
        }),
        None => serde_json::Value::Null,
    }
}

// ====================================================================================
// #186 — the ONE trustworthy, binary zero-loss verdict (replaces the muddled metrics).
//
// THE CHECK (per node): is the node's DIGITAL monotonic burn-id sequence, decoded from
// the STREAM recording, CONTIGUOUS? Each missing id is classified DEFINITIVELY by viewing
// the pixels at that position: a frame DELIVERED but the burn QR unreadable = a
// BURN-READABILITY defect to FIX (not a drop); a frame genuinely ABSENT = a REAL drop.
// No dropped/phantom/gap/painter-beat jargon, no percentage.
// ====================================================================================

/// How one missing burn id was classified by viewing the recorded pixels at its slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum MissingKind {
    /// A recorded frame is present at this optical slot (it carries cam2's optical QR,
    /// i.e. the frame was DELIVERED) but the node's burn QR for this id was not decoded.
    /// NOT a frame drop — a burn-readability defect to FIX (bigger / crisper burn).
    BurnUnreadable,
    /// No recorded frame carries this optical slot — the frame is genuinely ABSENT.
    /// A REAL dropped frame.
    RealDrop,
}

/// One classified missing burn id, with the recorded-frame slot and the pixel-proof PNG.
#[derive(Debug, Clone, serde::Serialize)]
struct ClassifiedMissing {
    /// The missing burn id (one candidate dropped frame).
    id: u32,
    /// Definitive classification from the pixels.
    kind: MissingKind,
    /// The recorded frame_index whose pixels were viewed to classify (the slot the id
    /// would occupy, between its bounding decoded ids). `None` when no bounding frame
    /// could be located (reported as a real drop with no slot).
    frame_index: Option<u64>,
    /// Pixel-proof PNG path of the viewed slot frame (a clickable LAN URL is printed
    /// alongside in the human report).
    png: Option<String>,
}

/// Whether `f` is a CURRENT-RUN DELIVERED optical frame — it carries cam2's optical dual-QR
/// for THIS run. A delivered frame proves an output frame reached the recording at that
/// optical instant, so it is what anchors the per-node burn-contiguity window.
///
/// `cam2_run_id` (#273):
/// - `Some(pin)` (the harness pins `--cam2-run-id`): a frame is current-run delivered ONLY
///   if it carries a payload whose run_id == `pin`. A FOREIGN cam2 run_id (a *previous*
///   run's residual paint still on the monitor when the recording started) is NOT current-run
///   delivery — it is pre-signal residue and must never extend the optical window. This is the
///   #273 fix: the strih recording's lead-in carried the prior run's `2606010` paint, which the
///   old "any non-burn payload" rule counted as delivered, anchoring the window at frame 0 and
///   charging the cam1-burn-absent lead-in as false BURN-UNREADABLE → a false zero-loss FAIL.
/// - `None` (unpinned, `--cam2-run-id 0`): any CRC-valid payload whose run_id is NOT one of the
///   forwarded node burns counts as cam2 — the pre-#273 behaviour, safe for the strih recording
///   (which carries no foreign burn) and unchanged for every existing call site.
fn frame_is_delivered_optical(
    f: &RecordingFrame,
    burn_run_ids: &[u32],
    cam2_run_id: Option<u32>,
) -> bool {
    match cam2_run_id {
        // #273: pinned ⇒ a frame is current-run delivered ONLY if it carries THIS run's paint.
        // A foreign (previous-run) cam2 run_id is pre-signal residue, NOT current-run delivery.
        // The `!burn_run_ids.contains` guard is defense-in-depth: should the pin ever be
        // misconfigured to a node burn run_id, a burn-only frame must NOT be read as optical
        // (that would let in_window membership collapse to "has the burn" and MASK a delivered-
        // but-burn-absent loss). With pin == a burn id no frame is optical ⇒ empty window ⇒
        // first_id None ⇒ is_contiguous() false ⇒ FAILS closed, never a vacuous pass.
        Some(pin) => f
            .payloads
            .iter()
            .any(|p| p.run_id == pin && !burn_run_ids.contains(&p.run_id)),
        // Unpinned ⇒ any non-burn payload is cam2 (pre-#273 behaviour; safe for the strih recording).
        None => f.payloads.iter().any(|p| !burn_run_ids.contains(&p.run_id)),
    }
}

/// #376 — the calibrated ceiling for the cam2 OPTICAL dual-QR undecodable RATE
/// (`optical_undecodable / optical_span_frames`) a node may carry and still pass the #363 HARD
/// gate. This is a CALIBRATION to the rig's real optical physics, not a weakening of the gate —
/// the same move as `colour_verify::NEUTRAL_CHROMA_MAX` (#364): the gate stays HARD, just aimed
/// at the real signal instead of an unreachable theoretical zero.
///
/// **Measured floor (2026-06-30, run 354003, stream recording, post-#363 Otsu-union decoder):**
/// after the #363 decoder fix landed (PR #375) the SAME pixels re-decoded from 86.9% undecodable
/// down to **22/8999 = 0.2445%**. Those 22 frames are the cam2 dual-QR's RIGHT half arriving
/// soft/mottled with heavy diagonal moiré (a camera→monitor optical artifact, the same physics
/// class as the #364 bright-neutral cyan cast) while the LEFT half of the SAME frame decodes
/// clean — proving the optical path CAN deliver a readable QR and that this residual is a rig
/// optical-physics floor, not a decoder or chain defect. The user's explicit call (issue #376,
/// 2026-07-01): *"akceptovateľný optický/moiré artefakt rigu (nakalibrujem prah vyššie)? Ano
/// akceptovatelne!"* — accept it, calibrate the threshold, do not chase the decoder further and
/// do not touch the camera.
///
/// **Why a RATE, not a raw count:** the residual is a per-frame optical-physics probability, so
/// it scales with recording length. A raw absolute-count ceiling would silently get stricter on
/// a long recording and looser on a short one; the rate stays calibrated to the same rig physics
/// regardless of how long the run is.
///
/// **Headroom:** 0.5% is ~2× the measured 0.2445% floor — enough for run-to-run optical variance
/// (moiré interference pattern shifts with tiny camera/monitor relative motion) while staying far
/// below any GENUINE optical dropout. A real dropout is qualitatively different: e.g. the #216
/// slow-shutter window ran ~175 s continuously undecodable — tens of PERCENT of any realistic
/// analyzed span, two orders of magnitude above this ceiling. See the `_376` regression tests:
/// a residual AT this rate passes; a rate materially above it (a real dropout shape) still FAILS.
///
/// **NEVER raise this without new measured evidence** (mirrors `colour_verify`'s calibration
/// discipline) — it tolerates the rig's proven moiré floor, never a genuine read failure.
const OPTICAL_UNDECODABLE_RATE_MAX: f64 = 0.005;

/// #24/#312 — the node labels that occupy the "camera under test" role: whichever physical
/// source camera(s) are deployed with `CAMERA_BOX_BURN_RUN_ID` set this run. In the plain
/// single-camera mode exactly ONE produces a non-empty id set (mutually exclusive); in the
/// ALL-CAMBOX sweep (`scripts/recording-e2e.sh ALL_CAMBOX=1`) ALL SIX are deployed at once, each
/// with its own reserved id, and each is "present" only during the schedule window(s) the sweep
/// actually cuts it into strih program. **cam2 is included here (#312)** — since #291 its
/// camera-box daemon keeps capturing + emitting its own NDI throughout a TEST run (only its
/// framebuffer is freed for the separate dual-QR painter), so its OWN chain is measurable by the
/// SAME digital contiguity check as cam1/cam3/cam4/cam5/cam6. Both the clean-source selection
/// (#133, `cam1_source`/`cam1_rec_path`) and the #356 cross-recording reconciliation apply
/// identically to every member; strih/stream never do.
///
/// NOTE: this is the CONTIGUITY/loss set. It is deliberately BROADER than
/// [`OPTICAL_INJECTION_NODES`] (below), which drives the cam2→camera OPTICAL-INJECTION latency
/// loop and excludes cam2 itself (cam2 cannot optically film its own monitor).
const CAMERA_UNDER_TEST_NODES: [&str; 6] = ["cam1", "cam2", "cam3", "cam4", "cam5", "cam6"];

/// #312 — the SUBSET of [`CAMERA_UNDER_TEST_NODES`] that physically films cam2's painted
/// monitor via the HDMI-splitter optical loopback (a real lens + capture card pointed at cam2's
/// screen). Used ONLY by the `#624` per-camera cam2→camera OPTICAL-INJECTION latency loop
/// (`camera_burn.gen_ts_ns − cam2_optical.gen_ts_ns`) — cam2 is EXCLUDED here because cam2 IS
/// the painter: there is no second camera-vs-monitor optical hop to measure when the "camera
/// under test" is cam2 itself (that would degenerate into measuring cam2 against its own
/// framebuffer paint, not a real optical-injection latency). cam2 still gets its own DIGITAL
/// contiguity/loss proof via [`CAMERA_UNDER_TEST_NODES`] above — only this narrower optical
/// latency measurement excludes it.
const OPTICAL_INJECTION_NODES: [&str; 5] = ["cam1", "cam3", "cam4", "cam5", "cam6"];

/// The full trustworthy verdict for one node: the contiguity result plus, when not
/// contiguous, every missing id classified from the pixels.
#[derive(Debug, Clone, serde::Serialize)]
struct NodeVerdict {
    contiguity: NodeContiguity,
    classified: Vec<ClassifiedMissing>,
    /// #363 — count of in-span frames whose cam2 OPTICAL dual-QR did NOT decode (the real
    /// camera-captured pixel path could not be proven). The cam2 optical read is the HARD gate:
    /// an undecodable RATE above [`OPTICAL_UNDECODABLE_RATE_MAX`] FAILS the node, even when the
    /// digital burn is contiguous. A distinct category — NOT a phantom chain drop (the pre-#360
    /// problem), NOT unconditionally passed (the #360 fraud). See [`optical_span_facts`].
    optical_undecodable: usize,
    /// #364 — number of reference COLOUR patches that FAILED the per-camera colour check on this
    /// node's recording (grayscale collapse / hue-shift / out-of-tolerance / neutral tint), charged
    /// on a strict MAJORITY of the sampled frames. The colour gate is HARD and mirrors
    /// `optical_undecodable`: any failed patch makes the node NOT zero, even when delivery AND the
    /// optical read are perfect — a camera that delivers every frame in the WRONG colour must FAIL.
    /// 0 when colour was not gated this run (`--colour-gate` off), so existing delivery-only runs
    /// are unaffected; the on-host/fused colour pass populates it (see `colour_sample`). NEVER
    /// weakenable.
    colour_fail: usize,
    /// #373 — the number of recorded frames in this node's cam2 OPTICAL span (the FIRST..=LAST frame
    /// whose dual-QR decoded, [`optical_span`]). 0 when there is no optical frame at all. Divided by
    /// the capture rate this is the ANALYZED span in seconds; the headline gates on it being at
    /// least `min_secs` ([`NodeVerdict::span_ok`]) so a COLLAPSED / partial optical read cannot
    /// vacuously pass (over a tiny span undecodable==0 and the burns are trivially contiguous — the
    /// fake-green hole #373 closes). It is NOT part of [`is_zero`] (which stays the per-node
    /// delivery+optical+colour gate, matched to the existing fixtures); the duration FLOOR is a
    /// run-level headline term applied alongside `is_zero`.
    optical_span_frames: usize,
    /// #463 — imag's SECOND independent zero-loss signal: the contiguity of its OWN digital
    /// corner burn (run_id [`BURN_RUN_ID_IMAG`]), separate from `contiguity` (which for imag
    /// carries the cam2 OPTICAL tick contiguity — see [`node_verdict_for_imag`]).
    ///
    /// `None` at the OUTER `Option` level means "this field does not apply to this node at
    /// all" — set for every NON-imag node (cam1/strih/stream never populate this; they have no
    /// "second" burn signal beyond `contiguity` itself). For imag SPECIFICALLY this outer
    /// `Option` is ALWAYS `Some` (`node_verdict_for_imag` always computes it); the "no burn
    /// decoded in this recording at all" case (the pre-#463 optical-only fallback) is
    /// represented ONE LEVEL DEEPER, as `Some(nc)` where `nc.first_id.is_none()` — NOT as the
    /// outer `None`. See [`Self::imag_burn_ok`] for the exact decision and
    /// [`camera_box::imag_tick_gate::optional_signal_ok`] for the shared "absent is fine,
    /// present-but-broken fails" rule this delegates to.
    imag_burn_contiguity: Option<NodeContiguity>,
    /// #580v2 — imag's PRIMARY optical zero-loss decision OVERRIDE: `None` for every non-imag node
    /// (unaffected — those keep using `contiguity.is_contiguous()` directly, see
    /// [`Self::optical_ok`]); for imag, the FULL
    /// [`camera_box::imag_tick_gate::OpticalBeatVerdict`] — its
    /// [`is_live_no_copy`](camera_box::imag_tick_gate::OpticalBeatVerdict::is_live_no_copy) is the
    /// pass/fail (the cam2 optical tick sequence genuinely ADVANCES — never frozen/stuck — AND
    /// carries NO long copy/freeze run; per-frame delivery accounting is the digital burn's job, not
    /// this leg's). The whole verdict (not just the bool) is stored so
    /// [`Self::node_verdict_lines`]'s printer and [`node_verdict_json`] can report the avg_step /
    /// surplus / max_stuck_run / dup+skip counts HONESTLY (a beat-compensated pass is NOT a strictly
    /// "contiguous" read — #580 review findings 1/B/C). Replaces strict step-1 contiguity as imag's
    /// optical decision — `contiguity` itself stays populated with the RAW strict tick_contiguity
    /// values (unchanged, still informative/printed), only the PASS/FAIL judgment moves here.
    imag_optical_beat: Option<camera_box::imag_tick_gate::OpticalBeatVerdict>,
    /// #580v2 (#584/#585) — for imag, whether the digital corner burn is GENUINELY PRESENT enough to
    /// be the SOLE delivery authority ([`camera_box::imag_tick_gate::burn_present_ok`]:
    /// `present_count >= optical_frames * MIN_BURN_PRESENT_FRACTION` — frame-scale to frame-scale,
    /// `step` plays no role). `None` for every
    /// non-imag node (unaffected); for imag ALWAYS `Some`. `Some(false)` = the burn is absent /
    /// occluded / frozen and the node FAILS fail-closed (closes the vacuous
    /// `optional_signal_ok(None) == true` pass and the trivially-"contiguous" single-id pass).
    imag_burn_present_ok: Option<bool>,
}

impl NodeVerdict {
    /// ZERO loss ⇔ the node's PRIMARY signal reads zero-loss ([`Self::optical_ok`] — strict
    /// digital-burn contiguity for every node except imag, #580's beat-aware optical verdict for
    /// imag) AND the cam2 OPTICAL read is complete across the span WITHIN the calibrated moiré
    /// floor (#376: [`Self::optical_undecodable_ok`]) AND, for imag, its digital corner burn
    /// (when present) is ALSO contiguous ([`Self::imag_burn_ok`], #463). The optical read is
    /// still the HARD gate — a run where the filmed dual-QR went undecodable at a rate ABOVE
    /// [`OPTICAL_UNDECODABLE_RATE_MAX`] FAILS even if every node's digital burn is present
    /// (reverts the #360 burn-only weakening); only the rig's PROVEN optical-physics floor
    /// (#376) is tolerated, never a genuine read failure.
    fn is_zero(&self) -> bool {
        self.optical_ok()
            && self.optical_undecodable_ok()
            && self.colour_fail == 0
            && self.imag_burn_ok()
    }
    /// #580 — the PRIMARY signal's pass/fail: [`Self::imag_optical_beat_pass`] when set (imag's
    /// beat-aware optical verdict), else the UNCHANGED strict `contiguity.is_contiguous()` every
    /// other node has always used. Isolating this as its own method (rather than inlining the
    /// `unwrap_or_else` in [`Self::is_zero`]) documents the override as its own named decision.
    fn optical_ok(&self) -> bool {
        self.imag_optical_beat_pass()
            .unwrap_or_else(|| self.contiguity.is_contiguous())
    }
    /// #580v2 — imag's optical PASS/FAIL as an `Option<bool>`: `Some(is_live_no_copy)` for an imag
    /// node, `None` for every other node (which has no beat verdict and falls back to strict
    /// contiguity in [`Self::optical_ok`]). Derived from the stored [`Self::imag_optical_beat`] so
    /// the pass bit and the reportable beat detail can never drift apart.
    fn imag_optical_beat_pass(&self) -> Option<bool> {
        // #580v2 — the HARD optical term is `is_live_no_copy` (advancing AND no long Δ0 copy/freeze
        // run), NOT the old `is_net_zero` (`surplus <= 0`, which false-fails the 572001 clock
        // residual AND fake-greens a content freeze). surplus/avg_step/is_net_zero stay diagnostic.
        self.imag_optical_beat.map(|b| b.is_live_no_copy())
    }
    /// #580v2 (#463/#584/#585) — is imag's digital corner burn a VALID delivery proof? `true` (not
    /// applicable) for every non-imag node (`imag_burn_present_ok` is `None` there — they carry no
    /// imag burn). For imag it requires BOTH the present floor ([`Self::imag_burn_present_ok`] —
    /// genuinely present, not absent/occluded/frozen) AND contiguity (`imag_burn_signal() ==
    /// Some(true)`).
    ///
    /// #580v2 made this FAIL-CLOSED: once the optical surplus is demoted to diagnostic, the burn is
    /// imag's SOLE per-frame delivery authority, so an ABSENT burn must FAIL — closing the pre-#580v2
    /// vacuous pass where a recording with no burn (or a single frozen id, trivially "contiguous")
    /// slipped through `optional_signal_ok(None) == true`. Both adversarial reviews flagged that hole.
    fn imag_burn_ok(&self) -> bool {
        // #580v2 (#584/#585) — for imag the digital corner burn is the SOLE delivery authority (the
        // optical surplus is demoted to diagnostic), so it must be GENUINELY PRESENT (the present
        // floor) AND contiguous. An absent / occluded / frozen burn FAILS fail-closed — closing the
        // vacuous `optional_signal_ok(None) == true` pass and the trivially-"contiguous" single-id
        // pass that both reviews flagged. `imag_burn_present_ok` is `Some` only for imag; for every
        // other node it is `None` and this returns `true` (they carry no imag burn — unaffected).
        match self.imag_burn_present_ok {
            Some(present_ok) => present_ok && self.imag_burn_signal() == Some(true),
            None => true,
        }
    }
    /// #463 — the raw `Option<bool>` burn-contiguity signal: `None` when the field doesn't apply
    /// (non-imag node) or the burn wasn't decoded at all; `Some(contiguous)` = the burn WAS decoded,
    /// is it contiguous. Consumed by [`Self::imag_burn_ok`] (which ANDs it with the #580v2 present
    /// floor) and by [`Self::imag_burn_broken`] / the printer, so the "is the burn present-but-broken"
    /// decision lives in exactly one place.
    fn imag_burn_signal(&self) -> Option<bool> {
        self.imag_burn_contiguity
            .as_ref()
            .and_then(|nc| nc.first_id.is_some().then(|| nc.is_contiguous()))
    }
    /// #463 — imag's digital corner burn IS present in this recording but NOT itself
    /// contiguous (the ONE case [`Self::imag_burn_ok`] returns `false` for). Returns the
    /// burn's own [`NodeContiguity`] only in that specific case (`None` when not applicable, or
    /// present-and-clean) — lets `node_verdict_lines` print the exact missing-id detail without
    /// re-deriving the three-way match itself.
    fn imag_burn_broken(&self) -> Option<&NodeContiguity> {
        match self.imag_burn_signal() {
            Some(false) => self.imag_burn_contiguity.as_ref(),
            _ => None,
        }
    }
    /// #376 — is this node's cam2 OPTICAL undecodable RATE within the calibrated moiré floor?
    /// `optical_span_frames == 0` (no optical frame at all) short-circuits to `true`: there is no
    /// span to compute a rate over (a 0/0 division), and `optical_undecodable` is PROVABLY 0 in
    /// this case too (both fields come from the same [`OpticalSpanFacts`], whose doc guarantees
    /// "no optical frame at all ⇒ both are 0") — an empty span already fails `is_contiguous()`
    /// regardless (see `empty_optical_span_has_zero_frames_and_fails_the_duration_gate_373`), so
    /// this branch exists only to avoid the division, not to re-check the count.
    fn optical_undecodable_ok(&self) -> bool {
        self.optical_span_frames == 0
            || self.optical_undecodable_rate() <= OPTICAL_UNDECODABLE_RATE_MAX
    }
    /// #376 — the fraction (0.0..=1.0) of this node's cam2 OPTICAL span that failed to decode.
    /// Shared by [`Self::optical_undecodable_ok`] (the gate) and the diagnostic printers/JSON (the
    /// displayed percentage) so the calibration formula lives in exactly one place. Callers with
    /// `optical_span_frames == 0` must not call this (division by zero) — check that first.
    fn optical_undecodable_rate(&self) -> f64 {
        self.optical_undecodable as f64 / self.optical_span_frames as f64
    }
    /// #373 — the analyzed optical span in seconds for this node (the cam2 dual-QR FIRST..=LAST
    /// window) from the recorded frame count and the camera capture rate.
    fn analyzed_span_secs(&self, capture_fps: f64) -> f64 {
        camera_box::recording_span_gate::span_secs(self.optical_span_frames, capture_fps)
    }
    /// #373 — does this node clear the >=`min_secs` headline DURATION floor? A collapsed / partial
    /// optical read fails it even when delivery + optical + colour are clean over the truncated span
    /// (the vacuous-pass hole #373 closes). Applied at the headline ALONGSIDE [`is_zero`], never
    /// folded into `is_zero` (which stays the per-node delivery gate).
    fn span_ok(&self, capture_fps: f64, min_secs: f64) -> bool {
        camera_box::recording_span_gate::analyzed_span_long_enough(
            self.analyzed_span_secs(capture_fps),
            min_secs,
        )
    }
    fn real_drops(&self) -> usize {
        self.classified
            .iter()
            .filter(|c| c.kind == MissingKind::RealDrop)
            .count()
    }
    fn burn_unreadable(&self) -> usize {
        self.classified
            .iter()
            .filter(|c| c.kind == MissingKind::BurnUnreadable)
            .count()
    }
}

/// The node's burn id decoded on a recorded frame (the first payload matching `burn_run_id`),
/// or `None` if the frame carried no readable burn for this node.
fn node_burn_id_on(f: &RecordingFrame, burn_run_id: u32) -> Option<u32> {
    f.payloads
        .iter()
        .find(|p| p.run_id == burn_run_id)
        .map(|p| p.frame_id)
}

/// #267 — the maximum length of a node's TRAILING (teardown) burn-absent run that may be clamped
/// off the optical window as an end-of-stream edge artifact rather than charged as loss. The
/// observed teardown overrun (run 2606010) was 23 frames (~0.77 s @ 30 fps emit: cam2's painter
/// outlived cam1 by the held latency + shutdown skew). This bound is ~2× that margin. A trailing
/// burn-absent run LONGER than this is treated as REAL end-of-stream loss (the node emitted those
/// ids and they were lost in transit) and is NEVER clamped — it stays charged as BURN-UNREADABLE
/// and FAILS, so the clamp can never mask a real zero-loss failure.
///
/// TRAILING-ONLY (deep-review correction): the LEADING (lead-in) edge is NEVER clamped. The
/// observed-and-justified case is the teardown tail; the lead-in is UNOBSERVED, and a leading
/// clamp would only open a NEW masking window where a real ≤bound start-of-stream loss false-PASSes
/// the user's HARD 0-gap bar. So this constant governs the trailing teardown tail only.
const TEARDOWN_TAIL_MAX_FRAMES: usize = 45;

/// #363 — the optical signal span: the index range from the FIRST to the LAST recorded frame whose
/// cam2 OPTICAL dual-QR decoded (a delivered frame). `None` ⇒ no optical frame at all (no signal
/// window). Both the in-window burn sequence ([`in_window_burn_frames`]) and the optical-undecodable
/// count ([`optical_span_facts`]) anchor on this span, so the boundary rule lives in ONE
/// place. (`cam2_run_id` honours the #273 pin: a foreign/previous-run paint never anchors the span.)
fn optical_span(
    stream: &[RecordingFrame],
    all_burn_run_ids: &[u32],
    cam2_run_id: Option<u32>,
) -> Option<(usize, usize)> {
    let is_optical =
        |f: &RecordingFrame| frame_is_delivered_optical(f, all_burn_run_ids, cam2_run_id);
    let first = stream.iter().position(is_optical)?;
    let last = stream.iter().rposition(is_optical)?;
    Some((first, last))
}

/// #363/#373 — the optical-signal facts for ONE source recording, both derived from the SAME
/// [`optical_span`] so the scan runs once per source (#374 nit 1: the strih and stream nodes share
/// the stream recording, so their facts were being computed twice — identical work). Compute once
/// via [`optical_span_facts`] and pass the result to each node's [`node_verdict_with_optical`].
///
/// - `undecodable_in_span` (#363): the in-span frames whose cam2 OPTICAL dual-QR did NOT decode —
///   strictly INTERIOR optical holes (the boundaries are optically anchored), each a frame whose
///   real camera-captured pixel path could not be proven. The HARD optical gate (reverts the #360
///   burn-only weakening): a run with ANY such frame FAILS even when every digital burn is present.
/// - `span_frames` (#373): the number of recorded frames in the optical span (FIRST..=LAST decoded
///   optical frame); divided by the capture rate it is the ANALYZED span the headline gates on
///   (>= `min_secs`) so a COLLAPSED / partial read cannot vacuously pass.
///
/// No optical frame at all ⇒ both are 0 (`span_frames == 0` ⇒ the #373 duration floor FAILS; the
/// burn contiguity also FAILS via `first_id == None`).
#[derive(Debug, Clone, Copy)]
struct OpticalSpanFacts {
    undecodable_in_span: usize,
    span_frames: usize,
}

/// Compute both [`OpticalSpanFacts`] for a source recording from a SINGLE [`optical_span`] scan.
fn optical_span_facts(
    stream: &[RecordingFrame],
    all_burn_run_ids: &[u32],
    cam2_run_id: Option<u32>,
) -> OpticalSpanFacts {
    match optical_span(stream, all_burn_run_ids, cam2_run_id) {
        Some((first, last)) => OpticalSpanFacts {
            undecodable_in_span: stream[first..=last]
                .iter()
                .filter(|f| !frame_is_delivered_optical(f, all_burn_run_ids, cam2_run_id))
                .count(),
            span_frames: last - first + 1,
        },
        None => OpticalSpanFacts {
            undecodable_in_span: 0,
            span_frames: 0,
        },
    }
}

/// Build the IN-WINDOW per-recorded-frame burn-presence sequence for a node (#198).
///
/// The window is the leading-discard-trimmed signal body: from the FIRST to the LAST
/// recorded frame that carries cam2's optical QR (a DELIVERED frame). Within that span,
/// every delivered frame is one emitted output frame that MUST carry the node's burn —
/// so each becomes a [`RecordedBurnFrame`] with the node burn id (or `None` if unreadable).
/// Frames OUTSIDE the window (pre-/post-signal lead-in/out, where cam2's QR is absent and
/// only a free-running render-tick burn may sit) are EXCLUDED — so their burn ids can never
/// inflate the range or be counted as missing (#198 point 1). Frames inside the window that
/// are NOT delivered (no cam2 QR — an interior optical hole) are excluded from the burn
/// sequence; #363 charges each such interior hole as a DISTINCT OPTICAL-UNDECODABLE hard-fail
/// (the cam2 optical read is the gate — see [`optical_span_facts`]), never silently
/// passed and never a phantom burn drop.
fn in_window_burn_frames(
    stream: &[RecordingFrame],
    burn_run_id: u32,
    all_burn_run_ids: &[u32],
    rate: BurnRate,
    // #273: cam2's pinned painter run_id (`--cam2-run-id`). `Some(pin)` ⇒ only THIS run's
    // optical paint defines the window boundaries — a foreign/previous-run paint in the
    // lead-in is excluded so it can never inflate the leading edge. `None` ⇒ the pre-#273
    // "any non-burn payload is cam2" rule (safe for the foreign-burn-free strih recording).
    cam2_run_id: Option<u32>,
) -> Vec<RecordedBurnFrame> {
    // #204: what counts as a recorded frame for THIS node's contiguity.
    //
    // The WINDOW BOUNDARIES (lead-in/out trim) always use cam2's optical QR — the optical
    // signal defines where the painted run begins/ends. But WITHIN the window, the
    // per-EMITTED-frame node (cam1) needs a different membership rule than the per-render
    // nodes (strih/stream):
    //
    // - cam1 (PerEmittedFrame): its burn increments once per EMITTED frame, so a frame
    //   carrying the cam1 burn PROVES cam1's emitted frame reached the recording — regardless
    //   of whether THAT frame's cam2 OPTICAL QR (a separate mark, independently blur-prone on
    //   a 60→30 refresh straddle) decoded. The old "optical-delivered only" filter EXCLUDED a
    //   frame whose optical QR was blurred while its cam1 burn was crisp, orphaning that cam1
    //   id and manufacturing a PHANTOM forward-gap "REAL DROP" on the next frame (#204: run
    //   136141133 frame 5905 carried cam1 burn 6485 with a blurred top dual-QR → 6485 dropped
    //   from the window → phantom drop). So for cam1 a frame is in-window if it carries the
    //   cam2 optical QR OR the cam1 burn — KEPT under #363 (the cam1 burn is genuine per-emit
    //   delivery proof, so this avoids the phantom; the optical hole on such a frame is still
    //   charged as OPTICAL-UNDECODABLE below, so it FAILS — never silently passed).
    // - strih/stream (PerRenderTick): #363 REVERTS the #360 extension. #360 included a frame
    //   carrying ONLY the strih/stream digital burn (no cam2 optical) as "delivered" — but that
    //   burn is injected at the OBS render tick, AFTER capture, so it proves node→node DIGITAL
    //   delivery, NOT that the real camera captured the pixel path. At high genlock latency the
    //   filmed cam2 dual-QR went ~87% undecodable (run 354003) and #360 routed the verdict AROUND
    //   the optical read, PASSING on the digital burns alone — the fraud #363 reverts. So a
    //   strih/stream frame is in-window ONLY when its cam2 OPTICAL QR decoded; a burn on a
    //   non-optical in-span frame is an OPTICAL-READ failure (counted as OPTICAL-UNDECODABLE
    //   below — a HARD fail), not a delivered frame. Because strih/stream are PerRenderTick with
    //   gap-ignore (step 1), excluding those frames does NOT manufacture a phantom drop (forward
    //   gaps between the surviving optical frames are ignored) — the pre-#360 phantom came from
    //   the now-removed step-2 decimation math, not from optical-only membership.
    let is_optical =
        |f: &RecordingFrame| frame_is_delivered_optical(f, all_burn_run_ids, cam2_run_id);
    let has_node_burn = |f: &RecordingFrame| node_burn_id_on(f, burn_run_id).is_some();
    // #363 — the cam2 OPTICAL dual-QR read is the HARD gate (reverts the #360 weakening). A frame
    // counts as a delivered burn-window frame ONLY when its cam2 OPTICAL QR decoded (the real
    // camera-captured pixel path). The #360 `|| has_node_burn` fallback let a frame with ONLY a
    // software-injected digital burn (no optical read) count as delivered, so an 87%-optically-
    // undecodable run PASSED on the digital burns alone — the fraud #363 reverts. The in-span
    // frames whose optical QR did NOT decode are charged as a DISTINCT OPTICAL-UNDECODABLE hard-fail
    // (see [`optical_span_facts`] / [`NodeVerdict::optical_undecodable`]) — never a phantom
    // chain drop, never a pass.
    //
    // EXCEPTION — cam1 ([`BurnRate::PerEmittedFrame`]): its burn increments once per EMITTED frame,
    // so a frame carrying the cam1 burn PROVES cam1 emitted+delivered THAT frame regardless of
    // whether its separate cam2 optical QR decoded. KEEPING the cam1 burn in the window is the #204
    // fix — it stops an optically-undecodable-but-cam1-delivered frame from orphaning the cam1 id
    // and manufacturing a PHANTOM forward-gap REAL DROP. That frame is NOT silently passed: it is
    // still counted as OPTICAL-UNDECODABLE and FAILS the verdict. strih/stream
    // ([`BurnRate::PerRenderTick`]) get NO burn fallback — their digital burn is a free-running
    // render tick, not per-emit delivery proof, so an in-span non-optical frame is purely an
    // optical-read failure.
    let in_window = |f: &RecordingFrame| {
        is_optical(f) || (matches!(rate, BurnRate::PerEmittedFrame) && has_node_burn(f))
    };

    // Boundaries: the optical signal span (the painted run). For cam1 this still anchors the
    // window to the optical signal; an in-window non-optical frame carrying the cam1 burn is
    // then included by `in_window` above, but the lead-in/out trim is optical-defined so a
    // pre-signal free-running cam1 burn can never extend the window.
    //
    // #273 boundary assumption: when pinned, `is_optical` is CURRENT-run paint only, so the
    // window starts at the first frame painting THIS run. This relies on cam1 NOT emitting
    // current-run burns onto FOREIGN-paint lead-in frames (true for the observed flow — cam1's
    // burn and cam2's paint advance to the new run together). A loss straddling the foreign→pin
    // boundary is the UNOBSERVED lead-in case #267 declines to special-case; the prior "any
    // paint is optical" rule that appeared to cover it was itself the #273 bug (it charged the
    // foreign residue as loss). The guarantee that IS preserved + tested: a current-paint frame
    // with an absent burn is KEPT and FAILS (pinned_real_leading_current_run_loss_still_fails).
    let (first, last) = match optical_span(stream, all_burn_run_ids, cam2_run_id) {
        Some(fl) => fl,
        // No optical frame at all ⇒ no signal window ⇒ nothing to prove (empty).
        None => return Vec::new(),
    };
    let mut frames: Vec<RecordedBurnFrame> = stream[first..=last]
        .iter()
        .filter(|f| in_window(f))
        .map(|f| RecordedBurnFrame {
            frame_index: f.frame_index,
            burn_id: node_burn_id_on(f, burn_run_id),
        })
        .collect();

    // #267 — BOUNDED TEARDOWN-TAIL EDGE CLAMP (TRAILING only). The window above is anchored to
    // cam2's optical (QR) span. At TEARDOWN a node's burn can be legitimately absent while cam2's
    // painter is still up: at shutdown the node STOPS emitting its burn while cam2 keeps painting a
    // few more frames (run 2606010: cam1 emitted clean through id 9461, then ~23 cam2-only frames at
    // teardown, ≈0.77 s). Those are not lost frames. The OLD fix popped EVERY trailing burn-absent
    // frame UNCONDITIONALLY — which MASKS real loss: an optical-present / burn-absent tail is
    // IDENTICAL in the recording whether the node simply ended there (legit) OR it EMITTED those
    // frames and they were LOST in transit right at shutdown (REAL end-of-stream loss — must FAIL,
    // the user's HARD zero-loss bar). No recorded signal distinguishes the two (the node burn rides
    // only its own recording; cam1-capture-stats is capture-rate ~2×, not a burn id; the painter is
    // cam2's already-over-extended boundary), so the ONLY sound discriminator is the SIZE of the
    // tail: a teardown overrun is small and bounded (≈0.77 s observed), real loss is not. So clamp a
    // TRAILING burn-absent run ONLY when it is within [`TEARDOWN_TAIL_MAX_FRAMES`]; a LONGER tail
    // stays charged as BURN-UNREADABLE and FAILS — never silently clamped.
    //
    // ONLY the TRAILING edge is clamped — the LEADING (lead-in) edge is NOT. A start-of-stream
    // burn-absent run is left CHARGED (BURN-UNREADABLE → FAIL). The lead-in case is UNOBSERVED on
    // the rig, and a leading clamp would open a NEW masking window where a real ≤bound START-of-
    // stream loss (the node emitted those ids; they were lost in transit at startup) false-PASSes
    // the HARD 0-gap bar. A false-FAIL is SAFE; masking start-of-stream loss is not. If a real
    // lead-in artifact is ever OBSERVED, it earns its own evidence-backed fix then — we do not clamp
    // an unobserved case.
    //
    // This NEVER weakens the strict #186 bar for the IN-RANGE span: an INTERIOR burn-less frame (a
    // present burn on BOTH sides — the stream resumed) is neither leading nor trailing, so it is
    // always kept and still FAILS. Rate-agnostic — a per-render tail at teardown is the same artifact.
    let n = frames.len();
    let trailing_absent = frames
        .iter()
        .rev()
        .take_while(|f| f.burn_id.is_none())
        .count();
    let drop_tail = if trailing_absent <= TEARDOWN_TAIL_MAX_FRAMES {
        trailing_absent
    } else {
        0 // a LONG tail is real end-of-stream loss — keep it, do NOT clamp
    };
    // truncate(n - drop_tail) handles the all-burn-absent short window too: trailing_absent == n ≤
    // bound ⇒ drop_tail == n ⇒ truncate(0) ⇒ empty (nothing to prove). A LONG all-absent window has
    // trailing_absent > bound ⇒ drop_tail == 0 ⇒ everything kept ⇒ FAILS, as it must.
    frames.truncate(n - drop_tail);
    frames
}

/// One node's identity AND source for the contiguity verdict: its label, its burn run_id,
/// how its burn counter advances (#198 — cam1 per-emit, strih/stream per-render), and the
/// recording its burn is decoded FROM (#133).
///
/// #133: the SOURCE recording is per-node, not a single shared `stream` slice. The cam1
/// burn rides through NDI into BOTH the strih and stream program recordings, so its
/// contiguity can be read from EITHER — but the stream recording SOFTENS the small cam1 burn
/// QR (it has traversed 2 NDI hops + 2 HEVC encodes by then; NOT a 4K upscale — both strih and
/// stream record at 1080p, verified live 2026-06-24, so the old "#196 4K upscale" premise is
/// invalid). The extra hops/re-encodes blur the small QR so it occasionally mis-decodes or
/// arrives slightly out of order in recorded-frame order. (#216 hardened the contiguity walk
/// so a reordered-but-present cam1 id no longer manufactures a phantom drop; the CLEAN source
/// of truth for cam1→strih is still the 1080p STRIH recording — the cam1 burn is crispest one
/// hop in.) So cam1's `source` is the strih recording; strih and stream remain on the stream
/// recording (the downstream endpoints whose own burns only the stream recording carries
/// co-located with cam2's optical QR).
struct NodeSpec<'a> {
    node: &'a str,
    burn_run_id: u32,
    rate: BurnRate,
    /// The decoded frames this node's burn-id contiguity is read FROM (#133).
    source: &'a [RecordingFrame],
    /// The recording file backing `source`, for pixel-proof extraction of a missing slot.
    /// #208: `None` when `source` came from a merged per-box PARTIAL — the contiguity verdict
    /// (the PASS gate) is unaffected; the pixel-proof PNG is not re-extracted HERE because the
    /// recording is not on dev1. It was already written ON the box during `--extract-partial`
    /// (see `extract_partial_flagged_frames`) and pulled back to dev1 beside the partial JSON in
    /// the `<partial>-pixels` dir, so the #186 "SEE the frame" proof is preserved either way.
    rec_path: Option<&'a Path>,
    /// #273: cam2's pinned painter run_id (`--cam2-run-id`), threaded into the optical-window
    /// boundary so a foreign/previous-run paint in the lead-in cannot inflate the leading edge.
    /// `None` ⇒ the pre-#273 "any non-burn payload is cam2" rule.
    cam2_run_id: Option<u32>,
    /// #11 mixed 60/30 → #360 REVISED → #571 REVISED AGAIN: the by-design per-recorded-frame
    /// burn-id step passed to [`burn_contiguity_in_window_with_step`]. strih/stream stay on
    /// gap-ignore (`1` — strih's burn is a FREE-RUNNING render tick with an IRREGULAR step, not a
    /// clean decimation, see [`node_render_step`]'s doc). #571: for cam1/cam3/cam4
    /// (PerEmittedFrame) `step` is the DECIMATED-HOP discriminator: `>= 2` (the Topology-v2
    /// cam(60fps)->strih(30fps) hop, run 554307) ⇒ forward id gaps are by-design decimation and
    /// are NOT charged at all (genuine loss there = a delivered frame with NO readable burn →
    /// BURN-UNREADABLE, plus strih's own 911002 burn and the optical tick); `== 1` ⇒ the strict
    /// pre-#571 forward-gap scan (a missing emitted id IS a real drop). See [`node_render_step`].
    step: i64,
}

/// #11 → #360 → #571: the per-recorded-frame burn-id step for a node's real-drop detection in
/// [`burn_contiguity_in_window_with_step`]. For cam1/cam3/cam4 (PerEmittedFrame) it is the
/// DECIMATED-HOP discriminator: `>= 2` ⇒ forward id gaps are by-design decimation, never charged;
/// `== 1` ⇒ the strict forward-gap scan (see that function's own doc).
///
/// - **strih** is a FREE-RUNNING DistroAV render-tick, NOT a per-output-frame counter. Read from the
///   30fps stream recording its per-frame step is IRREGULAR (run 354003: 0–10, mean ~4 — NOT the
///   assumed `round(60/30) = 2`), so a forward gap is render-clock jitter, not a lost frame: EVERY
///   strih gap > 8 on 354003 coincided with a CLEAN stream-burn step (the stream burn never gapped
///   ⇒ zero stream-output loss). The old strih=2 charging therefore manufactured ~17 300 phantom
///   REAL DROPs. So strih stays on gap-ignore (`1`): a delivered frame MISSING its strih burn is
///   still BURN-UNREADABLE (FAILS), and real loss is caught by the stream burn (per-output-frame)
///   plus cam1/cam3/cam4 (per-emitted). A strih→stream NDI content-hold loss shows as a SMALL strih
///   step (a held frame), never the large gap the old code charged — that detection belongs to the
///   per-frame continuity reconciliation (#356), not this free-running-tick gap math.
/// - **stream** is emitted AND recorded by the same stream OBS ⇒ `1` (no decimation).
/// - **cam1/cam3/cam4** (#571, `CAMERA_UNDER_TEST_NODES`, `BurnRate::PerEmittedFrame`): the
///   camera-under-test emits at `refresh_hz` (60, unchanged by Topology v2) but its forwarded
///   capture burn is read from the CLEAN strih recording (#133), which since #459/#460 records
///   ITS OWN cut-to-stream canvas at `capture_fps` (30 on the rig) — a CLEAN by-design 2:1
///   DECIMATION at the cam→strih hop, proven live on run 554307: strih's OWN node burn (911002)
///   was fully contiguous while cam1's forwarded burn (911001) read exactly the decimated half
///   (11087 phantom `real_drop`s under the old unconditional step=1 model — the #571 bug). Unlike
///   strih's free-running render tick, this decimation IS a clean integer ratio (the camera's own
///   emit clock genlocked to the strih canvas rate), so `painted_tick_step(refresh_hz, capture_fps)`
///   — the SAME by-design-decimation-ratio formula #467 already uses for the stream/imag
///   recordings — is reused rather than a second hardcoded "2", so the step always tracks the
///   rig-pinned CLI rates instead of being a magic constant.
///
/// `strih_emit_fps` / `stream_capture_fps` are read here for strih/stream's provenance and the
/// separate OPTICAL diagnostic step; they do not drive the strih/stream loss step (gap-ignore).
/// `refresh_hz` / `capture_fps` drive the cam1/cam3/cam4 decimation step.
fn node_render_step(
    node: &str,
    strih_emit_fps: f64,
    stream_capture_fps: f64,
    refresh_hz: f64,
    capture_fps: f64,
) -> i64 {
    if CAMERA_UNDER_TEST_NODES.contains(&node) {
        return camera_box::recording_span_gate::painted_tick_step(refresh_hz, capture_fps);
    }
    // strih/stream: read the rig-pinned fps for provenance; neither is a clean integer
    // decimation (see the docstring above for why strih is NOT a clean step-2) ⇒ gap-ignore.
    let _ = (strih_emit_fps, stream_capture_fps);
    1
}

/// #312 — build the per-frame inputs for the all-cambox segment continuity from the decoded
/// SINGLE stream recording. The attribution `gen_ts_ns` (the schedule's timeline) is taken from
/// a node BURN — the strih burn first (the program-switch box's render time), then the stream
/// burn (both passed in `anchor_run_ids`, priority order) — falling back to the cam2 OPTICAL paint
/// gen_ts (the pinned `cam2_run_id`, else any non-burn payload) so a frame missing both node burns
/// can still be placed. The painted `tick` is the cam2 optical Vernier tick ([`RecordingFrame::
/// tick`], which already excludes node burns). A frame carrying NO usable gen_ts anchor at all is
/// dropped (it cannot be placed on the timeline); the dropped count is returned so it is reported,
/// never hidden (the per-node burn contiguity verdict on the same recording catches such frames).
///
/// The schedule MUST be logged on the SAME timeline the primary anchor uses — the strih burn's
/// render time (the harness logs the program-switch wall-time at the strih box). The optical
/// fallback's paint gen_ts differs from a node burn's render gen_ts only by the small per-hop
/// genlock latency (prod 3ms), far inside the 1s transition guard, so a rare fallback frame near a
/// boundary stays correctly attributed. When `cam2_run_id` is pinned the optical fallback is
/// deliberately STRICT (run_id == pin) so a foreign/previous-run paint can never anchor a frame
/// (#273); an unpinned run uses any non-burn payload.
fn segment_frames_from_recording(
    frames: &[RecordingFrame],
    anchor_run_ids: &[u32],
    all_burn_run_ids: &[u32],
    cam2_run_id: Option<u32>,
) -> (Vec<SegmentFrame>, usize) {
    let mut out = Vec::with_capacity(frames.len());
    let mut no_anchor: usize = 0;
    for f in frames {
        match frame_gen_ts_anchor(f, anchor_run_ids, all_burn_run_ids, cam2_run_id) {
            Some(gen_ts_ns) => out.push(SegmentFrame {
                frame_index: f.frame_index,
                gen_ts_ns,
                tick: f.tick,
            }),
            None => no_anchor += 1,
        }
    }
    (out, no_anchor)
}

/// The `gen_ts_ns` anchor placing one recorded frame on the switch-schedule timeline: the primary
/// node burn's render time (`anchor_run_ids`, the timeline the harness logs switches on), falling
/// back to the cam2 optical paint's gen_ts (STRICT to `cam2_run_id` when pinned, #273; any non-burn
/// payload when unpinned). `None` ⇒ the frame carries no decodable anchor at all. The SINGLE source
/// of truth for the anchor, shared by [`segment_frames_from_recording`] (the strict SegmentFrame
/// path) and [`partition_frames_by_window`] (the #583 honest imag per-segment gate) so the two never
/// derive a different gen_ts for the same frame.
fn frame_gen_ts_anchor(
    f: &RecordingFrame,
    anchor_run_ids: &[u32],
    all_burn_run_ids: &[u32],
    cam2_run_id: Option<u32>,
) -> Option<i64> {
    anchor_run_ids
        .iter()
        .find_map(|rid| {
            f.payloads
                .iter()
                .find(|p| p.run_id == *rid)
                .map(|p| p.gen_ts_ns)
        })
        .or_else(|| {
            f.payloads
                .iter()
                .find(|p| match cam2_run_id {
                    Some(rid) => p.run_id == rid,
                    None => !all_burn_run_ids.contains(&p.run_id),
                })
                .map(|p| p.gen_ts_ns)
        })
}

/// #583 — partition a node's decoded frames into the SAME `--switch-schedule` windows the strict
/// sweep uses (via [`frame_gen_ts_anchor`] + [`place_frame_in_window`]), returning the
/// RecordingFrames per window so each window can be routed through the honest imag zero-loss gate
/// ([`camera_box::imag_tick_gate::imag_zero_loss`]), PLUS the count of frames with no gen_ts anchor
/// at all (mirrors [`segment_frames_from_recording`]'s `no_anchor` return — a #583 correctness-review
/// finding: this diagnostic was silently dropped in an earlier draft; restored so the imag sweep
/// reports it exactly like the stream sweep does). A frame inside a transition guard or outside
/// every window IS anchored but simply attributed to no window — only a frame with NO anchor at all
/// counts here. Reuses the identical window-attribution logic so the honest imag gate and the strict
/// stream sweep can never disagree on which window a frame belongs to.
fn partition_frames_by_window(
    frames: &[RecordingFrame],
    anchor_run_ids: &[u32],
    all_burn_run_ids: &[u32],
    cam2_run_id: Option<u32>,
    schedule: &[SwitchWindow],
    guard_ns: i64,
) -> (Vec<Vec<RecordingFrame>>, usize) {
    let mut out: Vec<Vec<RecordingFrame>> = vec![Vec::new(); schedule.len()];
    let mut no_anchor: usize = 0;
    for f in frames {
        match frame_gen_ts_anchor(f, anchor_run_ids, all_burn_run_ids, cam2_run_id) {
            Some(gen_ts) => {
                if let WindowPlacement::In(wi) = place_frame_in_window(gen_ts, schedule, guard_ns) {
                    out[wi].push(f.clone());
                }
            }
            None => no_anchor += 1,
        }
    }
    (out, no_anchor)
}

/// #706 — the switch-schedule context [`scope_camera_window_to_own_schedule`] needs to restrict a
/// `CAMERA_UNDER_TEST_NODES` node's in-window delivered-frame set to ONLY its own program
/// window(s). `Copy` (both fields are plain slices/an int) so passing it through the #186 loop's
/// 8 iterations is free; `None` throughout means "no switch schedule" (the single-camera-
/// continuously-on-program mode) and leaves every existing caller/fixture unchanged.
#[derive(Debug, Clone, Copy)]
struct ScheduleScope<'a> {
    /// The parsed `--switch-schedule` windows (shared with the #312 sweep — see the hoisted
    /// `switch_schedule` binding in `build_and_print_verdict`).
    schedule: &'a [SwitchWindow],
    /// The SAME anchor priority (a node burn's render time, falling back to cam2's optical paint)
    /// `frame_gen_ts_anchor`/#312's own placement uses — kept identical so the #186 per-camera
    /// scoping here and the #312 per-segment sweep can never attribute a frame to a different
    /// cambox window than each other.
    anchor_run_ids: &'a [u32],
    /// The transition guard (ns) discarded on each side of every schedule boundary — the SAME
    /// `--switch-guard-ns` value the #312 sweep uses (`args.switch_guard_ns`).
    guard_ns: i64,
}

/// #706 — restrict a `CAMERA_UNDER_TEST_NODES` node's in-window delivered-frame set to ONLY the
/// frames that fall inside THIS node's own switch-schedule program window(s) (post-transition-
/// guard), when the ALL-CAMBOX fused sweep supplies a `--switch-schedule`.
///
/// ## The bug this closes
///
/// In the ALL-CAMBOX sweep (#312) every `CAMERA_UNDER_TEST_NODES` entry is on strih PROGRAM for
/// only its OWN ~30s window(s) out of the whole ~300s recording — the other ~5/6 (or more, with 6
/// camboxes) of the time a DIFFERENT cambox is selected and this node's burn CANNOT appear at all
/// (by design — not loss). [`in_window_burn_frames`]'s own boundary is the WHOLE-RECORDING cam2
/// optical span, which does not know this: every OTHER camera's program time is included in "this
/// node's window", so every one of those delivered frames (which genuinely never carry THIS
/// node's burn) is misclassified BURN-UNREADABLE — confirmed live (#706): a 300s / 6-camera sweep
/// reported ~7000-8500 phantom BURN-UNREADABLE PER camera (~46000-47000 total) with 0 REAL DROP
/// and 0 genuine chain loss on any leg. Restricting to this node's OWN schedule window(s) —
/// mirroring EXACTLY how [`partition_frames_by_window`]/[`segment_continuity`] already attribute a
/// frame to a cambox for the #312 sweep, via the SAME [`frame_gen_ts_anchor`] +
/// [`place_frame_in_window`] — makes the two per-camera gates agree on which frames belong to
/// which camera, and removes the phantom count entirely.
///
/// `scope: None` (no `--switch-schedule` — the single-camera-continuously-on-program mode, e.g.
/// #204/#216's existing fixtures) leaves `window` UNCHANGED — this function only ever NARROWS the
/// window, never widens it, and only for [`CAMERA_UNDER_TEST_NODES`] (strih/stream are recorded
/// continuously throughout regardless of which cambox is on program, so they need no scoping and
/// this is a no-op for them even when a schedule IS supplied).
fn scope_camera_window_to_own_schedule(
    window: Vec<RecordedBurnFrame>,
    node: &str,
    source: &[RecordingFrame],
    all_burn_run_ids: &[u32],
    cam2_run_id: Option<u32>,
    scope: Option<ScheduleScope<'_>>,
) -> Vec<RecordedBurnFrame> {
    let Some(scope) = scope else {
        return window;
    };
    if !CAMERA_UNDER_TEST_NODES.contains(&node) {
        return window;
    }
    // frame_index -> gen_ts_ns anchor, computed ONCE over `source` — the SAME anchor priority
    // (a node burn's render time, falling back to cam2's optical paint) #312's own placement uses,
    // so a frame can never be attributed to a different window here than it would be there.
    let gen_ts_by_index: HashMap<u64, i64> = source
        .iter()
        .filter_map(|f| {
            frame_gen_ts_anchor(f, scope.anchor_run_ids, all_burn_run_ids, cam2_run_id)
                .map(|ts| (f.frame_index, ts))
        })
        .collect();
    window
        .into_iter()
        .filter(|rbf| {
            let Some(&gen_ts) = gen_ts_by_index.get(&rbf.frame_index) else {
                // No anchor at all on the original frame ⇒ cannot be placed on the schedule
                // timeline ⇒ excluded (mirrors `segment_frames_from_recording`'s `no_anchor`
                // exclusion — never silently counted as this node's own window).
                return false;
            };
            match place_frame_in_window(gen_ts, scope.schedule, scope.guard_ns) {
                WindowPlacement::In(wi) => scope.schedule[wi].cambox.eq_ignore_ascii_case(node),
                WindowPlacement::Guard | WindowPlacement::Outside => false,
            }
        })
        .collect()
}

/// #708 — for EACH entry of `window`, compute which `--switch-schedule` window index (if any)
/// it belongs to, via the SAME anchor priority + [`place_frame_in_window`] used everywhere else
/// a frame is placed on the schedule timeline ([`scope_camera_window_to_own_schedule`] just
/// above, and the #312 sweep) — so this NEW per-render backward-jump exception can never
/// disagree with the #706/#312 window attribution about which window a frame belongs to.
///
/// `None` at a position ⇒ the frame's own gen_ts anchor is missing, or it fell inside a
/// transition guard / outside every window — [`burn_contiguity_in_window_with_step_and_schedule`]
/// treats an unknown window on EITHER side of a comparison as "assume the SAME window" (never
/// silently suppresses a real anomaly), so returning `None` here is always the conservative,
/// safe choice.
fn attribute_window_indices(
    window: &[RecordedBurnFrame],
    source: &[RecordingFrame],
    all_burn_run_ids: &[u32],
    cam2_run_id: Option<u32>,
    scope: ScheduleScope<'_>,
) -> Vec<Option<usize>> {
    let gen_ts_by_index: HashMap<u64, i64> = source
        .iter()
        .filter_map(|f| {
            frame_gen_ts_anchor(f, scope.anchor_run_ids, all_burn_run_ids, cam2_run_id)
                .map(|ts| (f.frame_index, ts))
        })
        .collect();
    window
        .iter()
        .map(|rbf| {
            gen_ts_by_index.get(&rbf.frame_index).and_then(|&gen_ts| {
                match place_frame_in_window(gen_ts, scope.schedule, scope.guard_ns) {
                    WindowPlacement::In(wi) => Some(wi),
                    WindowPlacement::Guard | WindowPlacement::Outside => None,
                }
            })
        })
        .collect()
}

/// Build the trustworthy verdict for one node from the decoded stream frames: run the
/// IN-WINDOW per-recorded-frame contiguity check (#198 — rate-aware: cam1's burn is per-EMIT
/// so a forward integer gap is a REAL drop, strih/stream's is per-RENDER so a forward gap is
/// not loss), then extract a pixel-proof PNG for each missing slot the check identified.
///
/// The pure [`burn_contiguity_in_window`] is the SINGLE source of truth for both the
/// contiguity result AND each missing slot's (id, recorded frame_index, kind) — this function
/// no longer recomputes the walk or re-classifies; it just attaches the pixel proof.
///
/// `optical` carries the precomputed [`OpticalSpanFacts`] for `spec.source` (the #363 undecodable
/// count + the #373 span frames), computed ONCE per source by the caller so two nodes sharing a
/// recording do not rescan it (#374 nit 1).
///
/// `schedule_scope` (#706): `Some` ⇒ this node's in-window delivered-frame set (the DELIVERED
/// universe `expected_count`/`present_count`/`burn_unreadable` are all computed FROM) is further
/// restricted to ONLY this node's own switch-schedule program window(s) — see
/// [`scope_camera_window_to_own_schedule`]. `None` ⇒ today's unscoped whole-recording-optical-span
/// window, unchanged.
fn node_verdict_with_optical(
    spec: &NodeSpec,
    all_burn_run_ids: &[u32],
    optical: OpticalSpanFacts,
    out_dir: &Path,
    max_pixel_proof: usize,
    schedule_scope: Option<ScheduleScope<'_>>,
) -> Result<NodeVerdict> {
    let node = spec.node;
    // #133: read this node's burn from its OWN source recording (cam1 = the clean 1080p strih
    // recording; strih/stream = the stream recording) — NOT a single shared slice.
    let source = spec.source;
    let rec_path = spec.rec_path;
    // #198: walk only the in-window DELIVERED frames; rate decides whether a forward integer
    // gap is loss; a delivered frame missing its burn IS; out-of-window ids are excluded.
    let window = in_window_burn_frames(
        source,
        spec.burn_run_id,
        all_burn_run_ids,
        spec.rate,
        spec.cam2_run_id,
    );
    // #706 — in the ALL-CAMBOX fused sweep, further restrict to ONLY this node's own
    // switch-schedule program window(s) (no-op for strih/stream, and a no-op whenever no
    // `--switch-schedule` was supplied — see `scope_camera_window_to_own_schedule`'s doc).
    let window = scope_camera_window_to_own_schedule(
        window,
        node,
        source,
        all_burn_run_ids,
        spec.cam2_run_id,
        schedule_scope,
    );
    // #708 — strih's OWN 911002 burn is emitted by SIX INDEPENDENT free-running per-source
    // filter instances (one per raw `NDI camN` input — see `attribute_window_indices`'s doc), so
    // a backward id jump landing EXACTLY at a confirmed program-switch boundary is the EXPECTED
    // counter-instance change, not a lost frame (live-proven on 2 independent CI runs: every
    // flagged id was found present both in strih's own recording and downstream at stream).
    // Scoped to "strih" ONLY — the one node this mechanism is proven for; "stream"'s 911004 burn
    // is a single continuous counter on one fixed input, so this is a pure no-op for it either
    // way, but keeping the exception explicitly node-scoped avoids ever touching its behavior.
    let window_of: Option<Vec<Option<usize>>> = if node == "strih" {
        schedule_scope.map(|scope| {
            attribute_window_indices(&window, source, all_burn_run_ids, spec.cam2_run_id, scope)
        })
    } else {
        None
    };
    let in_window = burn_contiguity_in_window_with_step_and_schedule(
        node,
        &window,
        spec.rate,
        spec.step,
        window_of.as_deref(),
    );
    let contiguity = in_window.contiguity;
    // #363/#373 — the optical facts (undecodable count + span frames) are computed ONCE per source
    // by the caller and passed in (#374 nit 1: strih + stream share the stream recording, so this
    // was recomputed twice). `optical_undecodable` is the #363 HARD gate (an in-span frame whose
    // cam2 dual-QR did not decode FAILS, never passed on the digital burns alone — reverts the #360
    // weakening). `optical_span_frames` feeds the #373 headline DURATION floor — NOT a per-node
    // `is_zero` term; the floor is applied at the #186 headline alongside the delivery/optical/colour
    // gate.
    let optical_undecodable = optical.undecodable_in_span;
    let optical_span_frames = optical.span_frames;

    // The pure check already paired each missing id with the recorded frame to view and WHY
    // it is missing (RealDrop for a per-emit gap / backward jump, BurnUnreadable for a
    // delivered frame with no burn). Carry that classification verbatim — single source of
    // truth — and attach the pixel proof below.
    let mut classified: Vec<ClassifiedMissing> = in_window
        .missing_slots
        .iter()
        .map(|s| ClassifiedMissing {
            id: s.id,
            kind: match s.kind {
                InWindowMissingKind::RealDrop => MissingKind::RealDrop,
                InWindowMissingKind::BurnUnreadable => MissingKind::BurnUnreadable,
            },
            frame_index: Some(s.frame_index),
            png: None,
        })
        .collect();

    // Extract pixel-proof PNGs for every classified slot frame so the user can SEE it.
    // #208: only when the recording is on this host (fused / `--extract-partial`). In merge mode
    // (`rec_path` None) the recording is not on dev1, so these slots are NOT re-extracted here —
    // their pixel proofs were ALREADY written ON the box during `--extract-partial`
    // (`extract_partial_flagged_frames` flags the same missing slots) and pulled back beside the
    // partial JSON. The classification (the PASS gate) is complete regardless.
    let slots: Vec<u64> = classified.iter().filter_map(|c| c.frame_index).collect();
    if let (false, Some(rec_path)) = (slots.is_empty(), rec_path) {
        let png_dir = out_dir.join(format!("{node}-missing"));
        let extracted =
            extract_frames_png(rec_path, &slots, &HashSet::new(), &png_dir, max_pixel_proof)?;
        let idx_to_png: BTreeMap<u64, String> = extracted
            .iter()
            .map(|e| (e.frame_index, e.png_path.display().to_string()))
            .collect();
        for c in &mut classified {
            if let Some(fi) = c.frame_index {
                c.png = idx_to_png.get(&fi).cloned();
            }
        }
    }

    Ok(NodeVerdict {
        contiguity,
        classified,
        optical_undecodable,
        // #364 — the colour gate is populated by the caller (the on-host/fused colour pass) so the
        // pure analysis stays I/O-free for its many unit callers; 0 here means "colour not gated".
        colour_fail: 0,
        optical_span_frames,
        // #463: this second burn signal is imag-specific; every other node stays `None`.
        imag_burn_contiguity: None,
        // #580 — the beat-aware optical override is imag-specific; every other node stays `None`
        // (unaffected — `optical_ok()` falls back to `contiguity.is_contiguous()` for them).
        imag_optical_beat: None,
        // #580v2 — the burn present floor is imag-specific; every other node stays `None`
        // (unaffected — `imag_burn_ok()` returns `true` for them).
        imag_burn_present_ok: None,
    })
}

/// #461/#463/#580 — build imag-nb's verdict from its OWN recording (EPIC #466 Topology v2).
/// imag's PRIMARY zero-loss proof is the cam2 OPTICAL tick sequence.
///
/// **#580 — the optical decision is the BEAT-AWARE net-zero verdict, not strict first..=last
/// contiguity.** cam2's 60Hz monitor and imag's free-running 60fps camera are two UNSYNCHRONIZED
/// same-rate clocks whose tiny clock RESIDUAL is unavoidable — confirmed live (run 572001, post-#575
/// trim + #576 calibration): expected_count=21870, frames_count=21867, missing=19, surplus=+3,
/// avg_step≈1.000137, digital burn 0-missing — a genuinely ZERO-loss run. **#580v2 (two adversarial
/// Opus reviews):** the old `surplus <= 0` gate both FALSE-FAILS that +3 residual AND FAKE-GREENS a
/// content freeze (whose skips ≡ dups conserve the frame count, so `surplus ≈ 0` and `avg_step ≈ 1`).
/// The honest gate is RUN-LENGTH, not aggregates:
/// [`is_live_no_copy`](camera_box::imag_tick_gate::OpticalBeatVerdict::is_live_no_copy) = the read
/// genuinely ADVANCES (a loose liveness band) AND carries NO long copy/freeze Δtick==0 run
/// ([`camera_box::imag_tick_gate::IMAG_OPTICAL_MAX_STUCK_RUN`]). `surplus`/`avg_step`/`is_net_zero`
/// stay DIAGNOSTIC. Per-frame DELIVERY is proven independently by the STRICT digital corner burn
/// (hardened below: present floor + contiguity + calibrate clamp), ANDed in `is_zero`. The RAW strict
/// [`camera_box::imag_tick_gate::tick_contiguity`] result still populates `contiguity` for display
/// (see [`NodeVerdict::imag_optical_beat_pass`] for how the pass/fail JUDGMENT moves to the beat
/// verdict without changing what is shown).
///
/// **#463 — imag NOW ALSO carries its own digital corner burn** (run_id [`BURN_RUN_ID_IMAG`],
/// the OBS filter's `Corner::BottomCenterLeft`). When the recording carries it, its OWN
/// contiguity is ANDed with the optical proof above (stricter — see
/// [`NodeVerdict::imag_burn_ok`]); a recording with NO burn decoded at all falls back to the
/// optical-only proof, unchanged from pre-#463 behaviour.
///
/// **#480 — the burn's OWN contiguity is STEP-AWARE, not strict first..=last.** A live 300s rig
/// recording proved imag's burn free-runs at EXACTLY 2x the recorded rate (Studio-Mode
/// double-render — see [`camera_box::imag_tick_gate::IMAG_BURN_RENDER_STEP`]'s doc for the
/// confirmed root cause): every odd burn id is absent BY DESIGN, which the old strict-1:1 check
/// read as ~18600 phantom drops on a recording that lost ZERO real frames (the optical tick
/// already proved 0 REAL DROP). [`camera_box::imag_tick_gate::burn_step_contiguity`] models the
/// expected step so a forward gap of exactly the step is jitter/design, not loss, while a LARGER
/// gap still charges the excess as a genuine drop — never weakening the gate, only correctly
/// modeling it (`imag_tick_gate.rs`'s #480 test block reproduces the exact false-fail).
///
/// Sibling of [`node_verdict_with_optical`] but structurally simpler: imag has no [`NodeSpec`],
/// no pixel-proof extraction (out of scope for this ticket — the frame indices ARE known via
/// [`RecordingFrame::frame_index`], a future ticket can wire it the same way
/// [`node_verdict_with_optical`] does), and no colour gate (not wired for imag yet). Both the
/// optical tick contiguity AND the step-aware burn contiguity are the Tier-0 pure
/// [`camera_box::imag_tick_gate`] functions — this function is the thin probe-gated glue that
/// extracts [`RecordingFrame::tick`] (+ the burn ids via [`burn_ids_in`]) and converts the result
/// into the SAME [`NodeContiguity`] / [`NodeVerdict`] shape every other node uses, so `is_zero()`
/// / `print_node_verdict` / `node_verdict_json` all work for imag too.
fn node_verdict_for_imag(frames: &[RecordingFrame], cam2_run_id: Option<u32>) -> NodeVerdict {
    // #463: imag's OWN burn is now a known id — exclude it from "any non-burn payload is cam2's
    // optical paint" (mirrors `frame_is_delivered_optical`'s exclusion list for strih/stream), so
    // an imag burn payload can never be mistaken for the cam2 optical mark.
    let optical = optical_span_facts(frames, &[BURN_RUN_ID_IMAG], cam2_run_id);

    // #575: the recording's OWN frame-index bounds — anchoring the boundary trim on THESE
    // (never on a signal's own first/last decoded value) is what makes the trim a frame-POSITION
    // trim rather than a value-range trim (see `recording_boundary_trim`'s module doc). `frames`
    // is always sorted by `frame_index` (`analyze_recording_with_burns`), so first()/last() are
    // the true bounds; an empty recording defaults both to 0, which trims to nothing either way.
    let first_frame_index = frames.first().map(|f| f.frame_index).unwrap_or(0);
    let last_frame_index = frames.last().map(|f| f.frame_index).unwrap_or(0);
    // #575 review: a single closure over the shared bounds/window, so the optical tick and the
    // digital burn (below) can never silently diverge onto different trim windows — a future
    // edit to one call site's trailing args now visibly has to touch this ONE closure, not two
    // independent call sites.
    let trim_to_boundary = |samples: &[(u64, u32)]| {
        camera_box::recording_boundary_trim::trim_boundary_samples(
            samples,
            first_frame_index,
            last_frame_index,
            camera_box::recording_boundary_trim::BOUNDARY_TRIM_LEAD_FRAMES,
            camera_box::recording_boundary_trim::BOUNDARY_TRIM_TAIL_FRAMES,
        )
    };

    // #575: trim the recording start/stop boundary (genlock-fifo pre-roll flush, mux-
    // finalization tail-drain — confirmed live, run 554307) from the cam2 optical tick BEFORE
    // the contiguity check, so a handful of boundary-artifact frames can never manufacture a
    // phantom "missing tick" span. See `recording_boundary_trim`'s module doc for why trimming by
    // frame POSITION (not by decoded VALUE) can never mask a genuine mid-recording drop.
    let tick_samples: Vec<(u64, u32)> = frames
        .iter()
        .filter_map(|f| f.tick.map(|t| (f.frame_index, t)))
        .collect();
    let ticks = trim_to_boundary(&tick_samples);
    let tc = camera_box::imag_tick_gate::tick_contiguity(&ticks);

    // #580: imag's PRIMARY optical decision — cam2's 60Hz monitor and imag's free-running 60fps
    // capture are two unsynchronized same-rate clocks that BEAT (a skip balanced by a duplicate is
    // ZERO NET loss, not a fault). Replaces strict step-1 `tc.is_contiguous()` above as the
    // pass/fail judgment (`contiguity` itself, built below, still carries the RAW strict values —
    // unchanged, still informative/printed); see `imag_tick_gate::OpticalBeatVerdict`'s doc for
    // the confirmed live grounding (run 572001) and the advance-guard that closes the pre-existing
    // frozen-read hole strict-step-1 ALSO vacuously passed.
    // #580 review finding-2: reuse the `tc` already built above (`tick_contiguity(&ticks)` for the
    // raw strict `contiguity` field) instead of walking the identical slice into a second BTreeSet.
    let optical_beat = camera_box::imag_tick_gate::optical_beat_from_contiguity(
        &ticks,
        &tc,
        camera_box::imag_tick_gate::IMAG_OPTICAL_EXPECTED_STEP,
    );

    // #480: imag's OWN digital corner burn, decoded the same way every other node's burn is
    // ([`burn_ids_in`]), but gated with the STEP-AWARE model (`burn_step_contiguity`), not the
    // strict 1:1 `burn_contiguity` — see this function's doc comment for the confirmed root
    // cause. `first_id.is_none()` (nothing decoded at all) is still the "no burn in this
    // recording" fallback case — the NodeVerdict::imag_burn_ok() consumer treats it as
    // pass-through, so a pre-#463-build recording keeps working unchanged.
    //
    // #576: the step is SELF-CALIBRATED from this recording's own observed cadence
    // (`calibrate_burn_step`), not the hardcoded `IMAG_BURN_RENDER_STEP` constant — #480
    // confirmed step 2 at the time, but the #572 live-rig investigation found the real rig now
    // free-running at step 3. Calibrating per-recording means a future render-pipeline timing
    // change can never silently attribute the wrong grid ids to a genuine drop again.
    //
    // #575: the SAME boundary trim (via the SAME `trim_to_boundary` closure above) applies to the
    // burn ids (paired with frame_index via `burn_ids_with_frame_index_in`) before calibration
    // AND before the contiguity check — a boundary-artifact burn id must not skew the calibrated
    // step either.
    let imag_burn_ids = trim_to_boundary(&burn_ids_with_frame_index_in(frames, BURN_RUN_ID_IMAG));
    let imag_burn_step = camera_box::imag_tick_gate::calibrate_burn_step(&imag_burn_ids);
    let burn_sc = camera_box::imag_tick_gate::burn_step_contiguity(&imag_burn_ids, imag_burn_step);
    // #580v2 (#584/#585): the burn is imag's SOLE delivery authority, so it must be genuinely
    // PRESENT enough — measured against the OPTICAL frame count (`optical_beat.frames_count`, an
    // EXTERNAL reference; a burn that decoded only its first few frames must not vacuously clear a
    // floor derived from its own tiny span). An absent / occluded / frozen burn (present_count far
    // below the floor) FAILS fail-closed in `NodeVerdict::imag_burn_ok`.
    let imag_burn_present_ok = camera_box::imag_tick_gate::burn_present_ok(
        burn_sc.present_count,
        optical_beat.frames_count,
        camera_box::imag_tick_gate::MIN_BURN_PRESENT_FRACTION,
    );
    let imag_burn_contiguity = NodeContiguity {
        node: "imag-burn".to_string(),
        first_id: burn_sc.first_id,
        last_id: burn_sc.last_id,
        present_count: burn_sc.present_count,
        expected_count: burn_sc.expected_count,
        missing_ids: burn_sc.missing_ids,
    };

    NodeVerdict {
        contiguity: NodeContiguity {
            node: "imag".to_string(),
            first_id: tc.first_tick,
            last_id: tc.last_tick,
            present_count: tc.present_count,
            expected_count: tc.expected_count,
            missing_ids: tc.missing_ticks,
        },
        classified: Vec::new(),
        optical_undecodable: optical.undecodable_in_span,
        // #461: the colour gate is not wired for imag in this ticket.
        colour_fail: 0,
        optical_span_frames: optical.span_frames,
        imag_burn_contiguity: Some(imag_burn_contiguity),
        // #580v2: the FULL beat verdict is imag's PRIMARY optical signal — `NodeVerdict::optical_ok`
        // consults its `is_live_no_copy()` (advancing AND no copy/freeze run) instead of
        // `contiguity.is_contiguous()`, and the printers/JSON report its avg_step/surplus/dup+skip/
        // max_stuck_run counts honestly (a beat-compensated pass is NOT a strictly "contiguous" read).
        imag_optical_beat: Some(optical_beat),
        imag_burn_present_ok: Some(imag_burn_present_ok),
    }
}

/// #364 — the colour-gate I/O glue. Returns 0 when `--colour-gate` is off; otherwise samples this
/// node's recording for the #367 painted colour scale and returns the number of reference patches
/// WRONG on a majority of sampled frames (the node's `colour_fail`). Errors LOUDLY when the
/// recording is not on this host (merge mode) or the colour scale is not readable — a requested
/// gate must NEVER silently pass.
///
/// ffmpeg / process glue, like `decode_for` / `run_merge` / `extract_partial` — EXCLUDED from the
/// mutation gate (it cannot be unit-tested without a live recording on disk; the supervisor's
/// recorded fixture exercises it on the rig). The JUDGEMENT it wraps IS mutation-tested: the pure
/// `colour_verify` module (`classify_patch` / `summarize_node_colour`) and `NodeVerdict::is_zero`'s
/// `colour_fail == 0` term (locked by `node_verdict_colour_fail_is_a_hard_fail_364`).
fn build_node_colour_fail(
    spec: &NodeSpec,
    carried: Option<&camera_box::colour_verify::NodeColourSummary>,
    args: &Args,
    cache: &mut HashMap<PathBuf, usize>,
) -> Result<usize> {
    // #377 — MERGE mode: the box already sampled the colour scale ON its host during
    // `--extract-partial --colour-gate` and carried the per-recording summary in its partial (the
    // colour gate is fused/on-host — the recording is only on the box). Honor that carried summary
    // here regardless of THIS process's `--colour-gate` flag: a carried summary means the gate WAS
    // requested at extract, and the merge must not silently drop it. The fused path passes `None`
    // and samples the recording below.
    if let Some(summary) = carried {
        anyhow::ensure!(
            summary.any_chromatic_checked(),
            "colour gate: the carried colour summary for node {} had NO checkable CHROMATIC patch \
             (the colour scale was missing / its R/G/B/C/M/Y patches were fully burn-covered in that \
             box's recording) — cannot verify colour / detect a grayscale camera",
            spec.node
        );
        return Ok(summary.fail_count());
    }
    if !args.colour_gate {
        return Ok(0);
    }
    let rec = spec.rec_path.with_context(|| {
        format!(
            "--colour-gate set but node {} has no recording on this host AND no carried colour \
             summary (merge mode without a --colour-gate extract); re-run --extract-partial with \
             --colour-gate so the box samples colour on-host and carries it in its partial",
            spec.node
        )
    })?;
    // #364 — strih and stream share the stream recording; sample each source recording at most once.
    if let Some(&fail) = cache.get(rec) {
        return Ok(fail);
    }
    // The permanent cam2 painter renders the dual-QR + colour column at the default layout, so the
    // gate derives the same central-gap geometry from those defaults (single source of truth with
    // the painter via `colour_scale`).
    let summary = camera_box::probe::colour_sample::extract_recording_colour_summary(
        rec,
        args.colour_samples,
        camera_box::colour_scale::DEFAULT_QR_SIZE,
        camera_box::colour_scale::TOP_MARGIN_PX,
    )?;
    anyhow::ensure!(
        summary.any_chromatic_checked(),
        "colour gate: no CHROMATIC colour patch was checkable in {} for node {} — the colour scale \
         is missing or its R/G/B/C/M/Y patches are fully burn-covered (cannot verify colour / detect \
         a grayscale camera)",
        rec.display(),
        spec.node
    );
    let fail = summary.fail_count();
    cache.insert(rec.to_path_buf(), fail);
    Ok(fail)
}

/// Build the human-readable verdict line(s) for a node — the pure body of [`print_node_verdict`],
/// returned as a `Vec` so the no-double-print rule (#374 nit 2) is unit-testable.
///
/// #374 nit 2 — when no burn id decoded at all (`first_id == None`) the generic "NO burn id
/// decoded" line is emitted ONLY if no more specific fault line (colour / optical-undecodable)
/// already explained the failure. Previously an empty burn window WITH interior optical holes
/// co-printed BOTH the OPTICAL-UNDECODABLE line and the NO-burn line (redundant output); the
/// `explained` guard removes the duplication without dropping any specific reason.
fn node_verdict_lines(v: &NodeVerdict, span_ok: bool) -> Vec<String> {
    let c = &v.contiguity;
    let span = match (c.first_id, c.last_id) {
        (Some(f), Some(l)) => format!("ids {f}..={l}, {} present", c.present_count),
        _ => "no burn ids decoded".to_string(),
    };
    let mut lines: Vec<String> = Vec::new();
    // #373 — the per-node "ZERO loss … optical read complete" line may ONLY be printed when the
    // analyzed span ALSO cleared the duration floor. A delivery-clean node whose optical span
    // COLLAPSED is `is_zero()` but NOT span_ok: printing "ZERO loss … optical read complete" there
    // would be a per-node fake-green that the headline then flatly contradicts with its COLLAPSED
    // line (the no-overstatement rule). When `is_zero() && !span_ok` we fall through; the non-zero
    // branches below are all empty for a clean node, so this returns no per-node line and the
    // headline's COLLAPSED line stands as the sole verdict.
    if v.is_zero() && span_ok {
        // #463 — imag: when the recording ALSO carried a decoded digital corner burn, say so —
        // two independent zero-loss proofs, not just the optical one. Includes the burn's own
        // id range + present count (comprehensive-logging: values, not just a bare label) —
        // matches the detail level the FAILURE branch below already prints for this same field.
        let burn_note = match v
            .imag_burn_contiguity
            .as_ref()
            .and_then(|nc| Some((nc.first_id?, nc.last_id?, nc.present_count)))
        {
            Some((first, last, present)) => {
                format!(" AND digital corner burn CONTIGUOUS (ids {first}..={last}, {present} present, #463)")
            }
            None => String::new(),
        };
        // #580 review finding B — an imag node can be `is_zero()` via the beat-aware verdict while
        // the RAW strict tick sequence is NOT contiguous (a skip compensated by a duplicate). The
        // old flat "burn-id sequence CONTIGUOUS" claim is then factually FALSE (a tick value IS
        // genuinely missing from the range). Report the beat compensation HONESTLY instead — the
        // no-overstatement rule: state exactly what was proven, never more.
        let optical_phrase = match v.imag_optical_beat {
            Some(beat) if !c.is_contiguous() => {
                let dups = beat.frames_count.saturating_sub(beat.present_count);
                let skips = c.missing_ids.len();
                // #580v2 — a beat-compensated PASS is LIVE and copy-free, NOT strictly contiguous
                // and NOT necessarily `surplus <= 0` (a genuinely-zero run can carry a small +N clock
                // residual — run 572001 = +3). State exactly what was proven: advancing + no
                // copy/freeze; the surplus is reported as a diagnostic, never claimed to be ≤ 0.
                format!(
                    "cam2 optical read LIVE and copy-free via BEAT compensation ({skips} skipped \
                     tick(s), {dups} duplicate(s), max identical-tick run {} ≤ {} — 60Hz monitor vs \
                     60fps capture, surplus {} diagnostic, #580v2)",
                    beat.max_stuck_run,
                    camera_box::imag_tick_gate::IMAG_OPTICAL_MAX_STUCK_RUN,
                    beat.surplus
                )
            }
            _ => "burn-id sequence CONTIGUOUS AND cam2 optical read complete".to_string(),
        };
        lines.push(format!(
            "  [{}] ZERO loss — {optical_phrase} ({span}){burn_note}.",
            c.node
        ));
        return lines;
    }
    // #463 — imag's digital corner burn WAS decoded but is NOT itself contiguous: a second,
    // independent proof exists and disagrees with the (possibly clean) optical read — FAIL,
    // never silently overridden by the weaker optical-only proof (strict-test mandate). Reuses
    // `imag_burn_broken()` (the SAME decision `imag_burn_ok()`/`is_zero()` use) instead of
    // re-deriving the "present but not contiguous" condition a third time (the #463 review
    // caught two independent re-derivations of one rule).
    if let Some((nc, first, last)) = v
        .imag_burn_broken()
        .and_then(|nc| Some((nc, nc.first_id?, nc.last_id?)))
    {
        lines.push(format!(
            "  [{}] NOT zero — imag's OWN digital corner burn (run_id {BURN_RUN_ID_IMAG}) \
             is present but NOT contiguous: {} missing id(s) in {first}..={last} ({} \
             present of {} expected). The optical tick may be clean, but the digital burn \
             is a SECOND independent zero-loss proof and BOTH must hold (#463).",
            c.node,
            nc.missing_ids.len(),
            nc.present_count,
            nc.expected_count,
        ));
    }
    // #374 nit 2 — whether a SPECIFIC fault line (colour / optical) already explained the failure.
    let mut explained = false;
    // #580v2 (#585) — imag's digital corner burn is the SOLE delivery authority: absent, occluded,
    // or frozen (present_count below the floor) → fail-closed. Distinct from the present-but-gappy
    // branch above; the `!= Some(false)` guard defers to that branch when the burn IS decoded but
    // broken (so a below-floor-AND-gappy burn prints the gappy line, not both).
    if v.imag_burn_present_ok == Some(false) && v.imag_burn_signal() != Some(false) {
        let present = v
            .imag_burn_contiguity
            .as_ref()
            .map(|nc| nc.present_count)
            .unwrap_or(0);
        lines.push(format!(
            "  [{}] NOT zero — imag's OWN digital corner burn (run_id {BURN_RUN_ID_IMAG}) is ABSENT \
             or below the present floor ({present} decoded): imag's SOLE delivery authority is not \
             proven, so a copy/freeze the render-free-running burn is blind to could hide — \
             fail-closed (#585).",
            c.node,
        ));
        explained = true;
    }
    // #364 — the per-camera COLOUR gate. Surface a colour failure FIRST among the non-delivery
    // faults: a node can deliver every frame, with a complete optical read, and still be WRONG in
    // colour (grayscale / hue-shift / cast). The zero-loss verdict proved DELIVERY; this proves the
    // colour arrived correct, and a failure here FAILS the node like any other (#364).
    if v.colour_fail > 0 {
        lines.push(format!(
            "  [{}] NOT zero — {} reference COLOUR patch(es) WRONG on a majority of sampled frames \
             (grayscale / hue-shift / out-of-tolerance / cast). The camera delivered frames in the \
             WRONG colour — delivery being complete can NEVER substitute for correct colour (#364).",
            c.node, v.colour_fail
        ));
        explained = true;
    }
    // #363/#376 — the cam2 OPTICAL dual-QR read is the HARD gate, calibrated to the rig's proven
    // moiré floor (#376: `optical_undecodable_ok`). Surface an ABOVE-FLOOR undecodable rate FIRST:
    // it is the real-camera-path failure, NOT a digital burn fault. The digital burns can be 100%
    // present and the run still FAILS here (the #360 fraud this reverts). A non-zero count WITHIN
    // the calibrated floor is NOT a fault line here — `v.is_zero()` already returned true for it
    // above (#376), so this branch is reached only when the rate is genuinely above the floor —
    // which also means `optical_span_frames > 0` here (the `== 0` case always passes `_ok()`), so
    // `optical_undecodable_rate()` is safe to call without re-guarding the division.
    if !v.optical_undecodable_ok() {
        // #376 nit — 3 decimals (not 2): at a rate just above the 0.5% floor (e.g. 0.501%) a
        // 2-decimal display rounds both the measured rate AND the floor to "0.50%", making an
        // operator unable to tell why it failed. 3 decimals keeps them visibly distinct.
        lines.push(format!(
            "  [{}] NOT zero — {} OPTICAL-UNDECODABLE frame(s) ({:.3}% of the {}-frame optical span, above the {:.3}% calibrated moiré floor): the cam2 dual-QR (the REAL camera-captured pixel path) did not decode in-span. The digital burn proves node→node delivery only; it can NEVER substitute for the optical read (#363/#376).",
            c.node, v.optical_undecodable, 100.0 * v.optical_undecodable_rate(), v.optical_span_frames, 100.0 * OPTICAL_UNDECODABLE_RATE_MAX
        ));
        explained = true;
    }
    // No burn decoded at all (empty / all-unreadable window) ⇒ NOT a pass, but there is no
    // missing-id list to print — say so plainly instead of "0 missing id(s)". #374 nit 2: emit it
    // only when nothing more specific already explained the failure (no redundant co-print).
    if c.first_id.is_none() {
        if !explained {
            lines.push(format!(
                "  [{}] NOT zero — NO burn id decoded in the signal window (nothing proven; {} delivered frame(s) carried no readable {} burn).",
                c.node, c.expected_count, c.node
            ));
        }
        return lines;
    }
    // #580 — imag's beat-aware verdict FULLY OWNS the optical fault line for imag; the RAW
    // strict-step-1 `missing_ids` (nominal per-value gaps) is NOT itself a fault, so imag never
    // falls through to the generic per-value "N missing id(s)" line below (that would misattribute
    // a compensated beat, or a DIFFERENT failure like the digital burn above, to the optical read).
    // `imag_optical_beat` is `None` for every non-imag node, so this block is skipped there and the
    // generic burn-id logic runs unchanged.
    if let Some(beat) = v.imag_optical_beat {
        if beat.is_live_no_copy() {
            // Optical gate PASSED (live + copy-free): the strict per-value gaps are a beat, not a
            // fault — no optical fault line. Any burn / colour / undecodable / present-floor fault
            // above already printed its own reason (a genuine drop shows as a burn gap, not here).
            return lines;
        }
        // #580v2 — the optical gate FAILED: say WHY, HONESTLY. This node is `is_zero() == false`, so
        // it MUST print a reason — never an empty (silent) verdict. Two failure modes: a FROZEN/blank
        // read (does not advance) or a long COPY/FREEZE run the whole-window aggregates cannot see.
        if !beat.is_advancing() {
            lines.push(format!(
                "  [{}] NOT zero — cam2 optical tick did NOT advance ({span}): average step {:.2}, \
                 expected {} — a FROZEN/stuck read (first==last is trivially 'contiguous' but proves \
                 the camera captured NOTHING moving). #580v2 advance-guard.",
                c.node, beat.avg_step, beat.expected_step,
            ));
        } else if !beat.no_stuck_copy() {
            lines.push(format!(
                "  [{}] NOT zero — cam2 optical COPY/FREEZE ({span}): {} consecutive identical \
                 tick(s) (max Δ0 run {}, above the {} jitter ceiling) — a stalled upstream content \
                 or stuck camera that the whole-window surplus/avg_step AND the render-free-running \
                 digital burn are all blind to (#580v2).",
                c.node,
                beat.max_stuck_run.saturating_add(1),
                beat.max_stuck_run,
                camera_box::imag_tick_gate::IMAG_OPTICAL_MAX_STUCK_RUN,
            ));
        } else {
            // #588 — advancing AND no single long run, yet the optical gate FAILED ⇒ a SYSTEMATIC
            // catch-up JUDDER: MANY SHORT Δ0 runs (each ≤ K) whose AGGREGATE duplication density is
            // above the ceiling. State exactly what was proven (no-overstatement): the density is the
            // fault, NOT a single long freeze — the run-length term, the aggregates, AND the burn all
            // read this as clean.
            lines.push(format!(
                "  [{}] NOT zero — cam2 optical SYSTEMATIC JUDDER ({span}): Δ0 duplication density \
                 {:.2}% above the {:.2}% ceiling, though no single identical-tick run exceeds {} — a \
                 systematic short-run stutter spread across the whole recording that the run-length \
                 term, the whole-window surplus/avg_step, AND the render-free-running digital burn \
                 are all blind to (#588).",
                c.node,
                beat.stuck_density * 100.0,
                camera_box::imag_tick_gate::IMAG_OPTICAL_MAX_STUCK_DENSITY * 100.0,
                camera_box::imag_tick_gate::IMAG_OPTICAL_MAX_STUCK_RUN,
            ));
        }
        return lines;
    }
    // Burn-id faults (may be empty when the failure is PURELY optical — then only the line above
    // prints, and the per-slot loop below is a no-op).
    if c.missing_ids.is_empty() {
        return lines;
    }
    lines.push(format!(
        "  [{}] NOT zero — {} missing id(s) ({span}): {} REAL DROP, {} BURN-UNREADABLE (fix burn).",
        c.node,
        c.missing_ids.len(),
        v.real_drops(),
        v.burn_unreadable(),
    ));
    for cm in &v.classified {
        let label = match cm.kind {
            MissingKind::RealDrop => "REAL DROP",
            MissingKind::BurnUnreadable => "BURN-UNREADABLE (fix burn, frame delivered)",
        };
        let png = cm.png.as_deref().unwrap_or("<no pixel slot>");
        match cm.frame_index {
            Some(fi) => lines.push(format!(
                "    id {} -> {label} (frame {fi}, pixels: {png})",
                cm.id
            )),
            None => lines.push(format!("    id {} -> {label} (no recorded slot)", cm.id)),
        }
    }
    lines
}

/// Print the ONE trustworthy binary verdict for a node, human-readable, no jargon.
fn print_node_verdict(v: &NodeVerdict, span_ok: bool) {
    for line in node_verdict_lines(v, span_ok) {
        println!("{line}");
    }
}

/// JSON for one node's trustworthy verdict. `analyzed_secs` / `span_ok` / `min_secs` are the #373
/// headline duration gate (the analyzed optical span and whether it cleared the floor), so the
/// report explains a FAIL caused by a collapsed/partial optical read — not just a bare
/// `overall_pass: false`. `zero_loss` here is the per-node DELIVERY gate (`is_zero`); the headline
/// `overall_pass` ANDs it with `span_ok`.
fn node_verdict_json(
    v: &NodeVerdict,
    analyzed_secs: f64,
    span_ok: bool,
    min_secs: f64,
) -> serde_json::Value {
    serde_json::json!({
        "node": v.contiguity.node,
        "zero_loss": v.is_zero(),
        "first_id": v.contiguity.first_id,
        "last_id": v.contiguity.last_id,
        "present_count": v.contiguity.present_count,
        "expected_count": v.contiguity.expected_count,
        "missing_ids": v.contiguity.missing_ids,
        "real_drops": v.real_drops(),
        "burn_unreadable": v.burn_unreadable(),
        "optical_undecodable": v.optical_undecodable,
        // #376 — the calibrated moiré-floor gate. `optical_undecodable_ok` is the per-node term
        // `is_zero()` ANDs in; the rate + ceiling are surfaced alongside the raw count so a JSON
        // consumer can see WHY without recomputing (comprehensive-logging: values, not just labels).
        "optical_undecodable_ok": v.optical_undecodable_ok(),
        "optical_undecodable_rate_max": OPTICAL_UNDECODABLE_RATE_MAX,
        "colour_fail": v.colour_fail,
        "optical_span_frames": v.optical_span_frames,
        "analyzed_secs": analyzed_secs,
        "span_ok": span_ok,
        "min_secs": min_secs,
        "classified": v.classified,
        // #463 — imag's SECOND independent zero-loss signal (its own digital corner burn,
        // run_id BURN_RUN_ID_IMAG). `null` for every non-imag node and for an imag recording
        // with no burn decoded at all (the optical-only fallback); `imag_burn_ok` is the term
        // `is_zero()` ANDs in (comprehensive-logging: surfaced so a consumer can see WHY without
        // recomputing).
        "imag_burn_first_id": v.imag_burn_contiguity.as_ref().and_then(|nc| nc.first_id),
        "imag_burn_last_id": v.imag_burn_contiguity.as_ref().and_then(|nc| nc.last_id),
        "imag_burn_missing_ids": v.imag_burn_contiguity.as_ref().map(|nc| nc.missing_ids.clone()),
        "imag_burn_ok": v.imag_burn_ok(),
        // #580v2 (#584/#585) — the digital burn present floor: `false` ⇒ absent/occluded/frozen ⇒
        // fail-closed (`null` for non-imag nodes).
        "imag_burn_present_ok": v.imag_burn_present_ok,
        // #580v2 — the HARD optical gate (the read is LIVE and copy/freeze-free). `null` for every
        // non-imag node (strict contiguity governs there).
        "imag_optical_beat_pass": v.imag_optical_beat_pass(),
        // #580v2 — THE CENTERPIECE metric: max consecutive Δtick==0 run. The supervisor reads this
        // from the live 572001 re-decode to validate `IMAG_OPTICAL_MAX_STUCK_RUN` (K). A benign beat
        // ⇒ ≤ 1; a copy/freeze ⇒ hundreds.
        "imag_optical_max_stuck_run": v.imag_optical_beat.map(|b| b.max_stuck_run),
        // #588 — THE 4th orthogonal no-copy metric: the aggregate Δ0 (duplication) DENSITY over the
        // trimmed window (`stuck_pairs / total_pairs`), ANDed into `is_live_no_copy` via
        // `no_stuck_density`. A benign 60Hz-vs-60fps beat is ~0.1% (run 572001); a systematic
        // catch-up judder (many SHORT Δ0 runs each ≤ K) is tens of % — the exact pattern the
        // `max_stuck_run` (longest-run) term, the surplus/avg_step aggregates, AND the
        // render-free-running digital burn are all blind to. Surfaced so a consumer sees the value the
        // gate judged (comprehensive-logging). `null` for every non-imag node.
        "imag_optical_stuck_density": v.imag_optical_beat.map(|b| b.stuck_density),
        // #604 — THE 5th orthogonal no-copy metric: the MAXIMUM Δ0 duplication density found in any
        // fixed-width sliding window of the trimmed sequence, ANDed into `is_live_no_copy` via
        // `no_localized_stuck_density`. Catches a judder confined to a SHORT SUB-SPAN that the #588
        // WHOLE-window `imag_optical_stuck_density` dilutes below its own ceiling — a benign beat
        // stays well under 1% in any window; a localized catch-up judder burst reads ~25% within
        // its own window even when the whole-recording average is diluted under 1%. Surfaced so a
        // consumer sees the value the gate judged (comprehensive-logging). `null` for every
        // non-imag node.
        "imag_optical_local_stuck_density": v.imag_optical_beat.map(|b| b.local_stuck_density),
        // #580v2 DIAGNOSTIC-ONLY (no longer the pass/fail): `is_net_zero` (`surplus <= 0` AND
        // advancing) explains a beat-compensated read but is NOT the gate — a genuinely-zero run can
        // carry a small `surplus > 0` clock residual (run 572001 = +3). Surfaced so `zero_loss: true`
        // beside a non-empty `missing_ids` is self-explaining, not a consumer bug.
        "imag_optical_beat_net_zero": v.imag_optical_beat.map(|b| b.is_net_zero()),
        "imag_optical_beat_avg_step": v.imag_optical_beat.map(|b| b.avg_step),
        "imag_optical_beat_expected_step": v.imag_optical_beat.map(|b| b.expected_step),
        "imag_optical_beat_present_count": v.imag_optical_beat.map(|b| b.present_count),
        "imag_optical_beat_frames_count": v.imag_optical_beat.map(|b| b.frames_count),
        "imag_optical_beat_surplus": v.imag_optical_beat.map(|b| b.surplus),
    })
}

/// Parse the painter ground-truth ticks from any of the THREE shapes the harness
/// produces, selecting the tick column from the HEADER:
///
/// - `--paint-log` CSV (the cam2 painter ground truth, [`serialize_painter_log`]):
///   header `tick,gen_ts_ns` ⇒ tick is column 0.
/// - recording-probe CSV: header `frame_index,n_qr,tick,run_id,frame_ids` ⇒ tick is
///   column 2.
/// - a bare one-`tick`-per-line file (no header, no comma) ⇒ the whole line is the tick.
///
/// A comma-containing data row with too few columns for the detected layout is a
/// MALFORMED CSV — error loudly (a silently-shrunk painter set would manufacture false
/// phantom faults). Pure (operates on the file text) so the column-detection is
/// unit-testable without a file.
fn parse_painter_ticks_str(text: &str) -> Result<Vec<u32>> {
    // Detect the tick column from the first non-blank line if it is a known header.
    let header = text.lines().map(str::trim).find(|l| !l.is_empty());
    let tick_col: usize = match header {
        Some(h) if h.starts_with("tick,") => 0, // --paint-log: tick,gen_ts_ns
        Some(h) if h.starts_with("frame_index") => 2, // recording-probe: ..,..,tick,..
        _ => 0,                                 // bare one-tick-per-line
    };
    let mut ticks = Vec::new();
    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim();
        // Skip blanks and either known header line.
        if line.is_empty() || line.starts_with("frame_index") || line.starts_with("tick,") {
            continue;
        }
        let field = if line.contains(',') {
            line.split(',').nth(tick_col).with_context(|| {
                format!(
                    "painter CSV row at line {} has too few columns for the detected \
                     tick column {tick_col}: {line:?}",
                    lineno + 1
                )
            })?
        } else {
            line // bare file: the whole line is the tick
        };
        let field = field.trim();
        if field.is_empty() {
            continue; // an undecodable recording-probe row has an empty tick column
        }
        let t: u32 = field
            .parse()
            .with_context(|| format!("painter tick not a u32 at line {}: {field:?}", lineno + 1))?;
        ticks.push(t);
    }
    Ok(ticks)
}

/// Read + parse the painter ground-truth ticks from a file (see
/// [`parse_painter_ticks_str`] for the accepted shapes).
fn parse_painter_ticks(path: &Path) -> Result<Vec<u32>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read painter ticks {}", path.display()))?;
    let ticks = parse_painter_ticks_str(&text)
        .with_context(|| format!("parse painter ticks {}", path.display()))?;
    tracing::info!(file = %path.display(), ticks = ticks.len(), "painter ticks parsed");
    Ok(ticks)
}

/// #194 — parse the painter `--paint-log` CSV (`tick,gen_ts_ns,flip_ts_ns`,
/// [`serialize_painter_log`]) into `(tick → gen_ts_ns, tick → flip_ts_ns)` maps. The
/// `flip_ts_ns` (page-flip-complete = on-screen instant) is the cam2 DISPLAY reference the
/// cam2→cam1 latency uses ([`cam2_cam1_samples_from_flip`]); `gen_ts_ns` is kept so the
/// painter's internal generate→display time can be reported separately
/// ([`painter_internal_gen_to_flip`]).
///
/// Only the 3-column `--paint-log` (header `tick,gen_ts_ns,flip_ts_ns`) carries a flip
/// column. The older 2-column `tick,gen_ts_ns`, a recording-probe CSV, or a bare tick file
/// have NO flip stamp ⇒ both maps come back EMPTY (no flip column to read), so the caller
/// transparently falls back to the gen-based cam2→cam1. A malformed 3-column data row
/// (wrong column count / non-integer) errors loudly — a silently-shrunk flip map would
/// drop legitimate cam2→cam1 samples without any signal. Pure (operates on the file text).
fn parse_painter_flip_str(text: &str) -> Result<(HashMap<u32, i64>, HashMap<u32, i64>)> {
    let header = text.lines().map(str::trim).find(|l| !l.is_empty());
    // Only the explicit 3-column paint-log carries a flip column.
    let has_flip = matches!(header, Some(h) if h.starts_with("tick,gen_ts_ns,flip_ts_ns"));
    let mut gen_by_tick = HashMap::new();
    let mut flip_by_tick = HashMap::new();
    if !has_flip {
        return Ok((gen_by_tick, flip_by_tick));
    }
    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("tick,") {
            continue; // header / blank
        }
        let cols: Vec<&str> = line.split(',').collect();
        if cols.len() != 3 {
            anyhow::bail!(
                "paint-log row at line {} is not `tick,gen_ts_ns,flip_ts_ns`: {line:?}",
                lineno + 1
            );
        }
        let tick: u32 = cols[0].trim().parse().with_context(|| {
            format!(
                "paint-log tick not a u32 at line {}: {:?}",
                lineno + 1,
                cols[0]
            )
        })?;
        let gen: i64 = cols[1].trim().parse().with_context(|| {
            format!(
                "paint-log gen_ts not an i64 at line {}: {:?}",
                lineno + 1,
                cols[1]
            )
        })?;
        let flip: i64 = cols[2].trim().parse().with_context(|| {
            format!(
                "paint-log flip_ts not an i64 at line {}: {:?}",
                lineno + 1,
                cols[2]
            )
        })?;
        gen_by_tick.insert(tick, gen);
        flip_by_tick.insert(tick, flip);
    }
    Ok((gen_by_tick, flip_by_tick))
}

/// Read + parse the painter flip-time maps from a file (see [`parse_painter_flip_str`]).
/// Returns empty maps when the file has no flip column (graceful fallback to gen-based).
fn parse_painter_flip(path: &Path) -> Result<(HashMap<u32, i64>, HashMap<u32, i64>)> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read painter flip log {}", path.display()))?;
    let (gen, flip) = parse_painter_flip_str(&text)
        .with_context(|| format!("parse painter flip log {}", path.display()))?;
    tracing::info!(
        file = %path.display(),
        flip_ticks = flip.len(),
        "painter flip-time map parsed (#194)"
    );
    Ok((gen, flip))
}

/// The cam2→cam1 LOSS, from cam1's V4L2 capture-drop sidecar (the camera leg).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cam1CaptureStats {
    /// Frames the V4L2 capture device dropped (the cam2→cam1 loss count). 0 ⇒ zero loss.
    v4l2_dropped: u64,
    /// Delivered buffers (the loss denominator).
    frames_captured: u64,
}

/// Parse cam1's capture-stats sidecar (`v4l2_dropped=N`, `frames_captured=M`,
/// [`crate::serialize_capture_stats`] on the camera-box side) into [`Cam1CaptureStats`].
/// `v4l2_dropped` is the cam2→cam1 LOSS — capture-card drops, NOT a painter-tick compare.
/// A missing `v4l2_dropped` key is an error (a sidecar with no drop count can't be read as
/// zero loss). Pure (operates on the file text).
fn parse_cam1_capture_stats_str(text: &str) -> Result<Cam1CaptureStats> {
    let mut v4l2_dropped: Option<u64> = None;
    let mut frames_captured: u64 = 0;
    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (k, v) = line.split_once('=').with_context(|| {
            format!(
                "cam1 capture-stats line {} is not key=value: {line:?}",
                lineno + 1
            )
        })?;
        let v = v.trim();
        match k.trim() {
            "v4l2_dropped" => {
                v4l2_dropped = Some(v.parse().with_context(|| {
                    format!("v4l2_dropped not a u64 at line {}: {v:?}", lineno + 1)
                })?)
            }
            "frames_captured" => {
                frames_captured = v.parse().with_context(|| {
                    format!("frames_captured not a u64 at line {}: {v:?}", lineno + 1)
                })?
            }
            _ => {} // forward-compatible: ignore unknown keys
        }
    }
    let v4l2_dropped = v4l2_dropped.context(
        "cam1 capture-stats sidecar is missing the v4l2_dropped key (cannot report cam2→cam1 loss)",
    )?;
    Ok(Cam1CaptureStats {
        v4l2_dropped,
        frames_captured,
    })
}

/// Read + parse cam1's capture-stats sidecar (see [`parse_cam1_capture_stats_str`]).
fn parse_cam1_capture_stats(path: &Path) -> Result<Cam1CaptureStats> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read cam1 capture-stats sidecar {}", path.display()))?;
    parse_cam1_capture_stats_str(&text)
        .with_context(|| format!("parse cam1 capture-stats sidecar {}", path.display()))
}

/// Parse the cam1 grab-timestamp sidecar CSV (`frame_index,grab_ts_ns`, header
/// `frame_index,grab_ts_ns`) the `--record-grab` mode writes into a
/// `frame_index → grab_ts_ns` map.
///
/// Row-error policy:
/// - An EMPTY `grab_ts_ns` cell (`"<idx>,"`) means that frame simply has no recorded grab
///   instant = NO cam2→cam1 pairing for it (`cam2_cam1_samples` already yields no sample when
///   the map has no entry for a frame). It is benign missing data, so it is warn + SKIPPED
///   (#170) — one such row must never abort the whole verdict (run-163163 crashed at the very
///   end on a single empty cell, losing every loss/latency number it had already computed).
/// - A NON-empty but unparseable cell, or a wrong column count, is genuine corruption and still
///   errors loudly — a silently-shrunk map from real corruption would drop valid samples
///   without any signal.
fn parse_grab_ts(path: &Path) -> Result<std::collections::HashMap<u64, i64>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read cam1 grab-ts sidecar {}", path.display()))?;
    let mut m = std::collections::HashMap::new();
    // Kill-time partial-row tolerance (cam1 --record-grab is a BufWriter killed at teardown
    // with NO flush, so the file is cut at an arbitrary byte boundary). A COMPLETE row always
    // ends with '\n' (the writeln! emits the newline LAST, after the full `idx,ts` payload),
    // so a file that does NOT end in '\n' has exactly one partial final line — of ANY shape
    // ("8874,", "8874", "8874,17820"-truncated). That final fragment is skipped, whatever it
    // is; every earlier (newline-terminated) row is parsed STRICTLY. A newline-terminated
    // malformed row is genuine corruption (not a kill cut) and still errors loudly — a
    // silently-shrunk grab-ts map would drop / corrupt real cam2→cam1 latency samples.
    let has_trailing_newline = text.ends_with('\n');
    // The byte-offset line index of the LAST non-blank, non-header data line (only meaningful
    // when there is NO trailing newline — then THIS line is the partial fragment to skip).
    let last_data_line = text
        .lines()
        .enumerate()
        .filter(|(_, l)| {
            let l = l.trim();
            !l.is_empty() && !l.starts_with("frame_index")
        })
        .map(|(i, _)| i)
        .last();
    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("frame_index") {
            continue; // header / blank
        }
        // A no-trailing-newline file's final data line is a partial kill-time fragment — skip
        // it whatever its shape (empty ts, no comma, truncated digits). Everything else parses.
        if !has_trailing_newline && Some(lineno) == last_data_line {
            tracing::warn!(
                line = lineno + 1,
                fragment = %line,
                "grab-ts final row has no trailing newline (partial kill-time write) — skipped"
            );
            continue;
        }
        let mut it = line.split(',');
        let idx_s = it
            .next()
            .with_context(|| format!("grab-ts row at line {} is empty: {line:?}", lineno + 1))?;
        let ts_s = it.next().with_context(|| {
            format!(
                "grab-ts row at line {} has <2 columns (expected frame_index,grab_ts_ns): {line:?}",
                lineno + 1
            )
        })?;
        let idx: u64 = idx_s.trim().parse().with_context(|| {
            format!(
                "grab-ts frame_index not a u64 at line {}: {idx_s:?}",
                lineno + 1
            )
        })?;
        let ts_s = ts_s.trim();
        if ts_s.is_empty() {
            // #170: an empty grab_ts_ns cell = this frame has no recorded grab instant = no
            // cam2→cam1 pairing for it (benign missing data). Warn + skip the row; never crash
            // the verdict over a single empty cell.
            tracing::warn!(
                line = lineno + 1,
                frame_index = %idx_s.trim(),
                "grab-ts row has an empty grab_ts_ns (no recorded grab instant) — skipped"
            );
            continue;
        }
        let ts: i64 = ts_s.parse().with_context(|| {
            format!(
                "grab-ts grab_ts_ns not an i64 at line {}: {ts_s:?}",
                lineno + 1
            )
        })?;
        m.insert(idx, ts);
    }
    tracing::info!(file = %path.display(), entries = m.len(), "cam1 grab-ts sidecar parsed");
    Ok(m)
}

/// Print a per-recording DIAGNOSTIC (#186): the per-frame continuity numbers
/// (undecodable / span) for context only — it does NOT gate the headline verdict
/// and does NOT print a PASS/FAIL "RESULT" (which read as a verdict). The single
/// trustworthy loss verdict is the per-node burn-id contiguity. The 60→30-beat
/// `real_copy`/`real_gap`/`beat_balanced` muddled metrics (which conflated the
/// sampling beat with loss — the false-positive source) are no longer surfaced.
/// `undecodable` (frames with NO readable QR at all) is kept as a diagnostic and
/// its pixel proof extracted.
fn report_recording_diag(
    label: &str,
    // #208: `None` when the frames came from a merged per-box PARTIAL (the recording lives on
    // its own box and is NEVER copied here) — the continuity numbers are unchanged; only the
    // pixel-proof PNG extraction (which needs the recording file) is skipped.
    path: Option<&Path>,
    v: &RecordingVerdict,
    out_dir: &Path,
    max_pixel_proof: usize,
) -> Result<()> {
    match path {
        Some(p) => println!("=== {label} recording DIAGNOSTIC ({}) ===", p.display()),
        None => println!(
            "=== {label} recording DIAGNOSTIC (merged partial — recording not on this host, #208) ==="
        ),
    }
    println!(
        "  frames={} analyzed={:.1}s undecodable={} (diagnostic only — loss is decided by the \
         #186 burn-id contiguity below, not these per-frame beat metrics)",
        v.total_frames,
        v.analyzed_secs,
        v.undecodable_frames.len()
    );
    if v.lead_in_trimmed > 0 || v.lead_out_trimmed > 0 {
        println!(
            "  leading-discard: {} pre-signal (console lead-in) + {} post-signal (teardown) \
             frames trimmed — NOT counted as undecodable",
            v.lead_in_trimmed, v.lead_out_trimmed
        );
    }
    if !v.duration_ok {
        println!(
            "  NOTE: analyzed span {:.1}s < {:.1}s — short run (diagnostic).",
            v.analyzed_secs, v.min_secs
        );
    }

    // Extract pixel proof for undecodable frames (no readable QR at all) for context.
    let undecodable: HashSet<u64> = v.undecodable_frames.iter().copied().collect();
    let mut flagged: Vec<u64> = v.undecodable_frames.to_vec();
    flagged.sort_unstable();
    flagged.dedup();

    // #208: pixel-proof needs the recording file. In merge mode (path None) the recording is not
    // on dev1; the per-box `--extract-partial` run ALREADY wrote these undecodable frames' pixel
    // proofs ON the box (`extract_partial_flagged_frames`) and they were pulled back to dev1 beside
    // the partial JSON in the `<partial>-pixels` dir (run_merge prints the concrete path). So report
    // the count and do not re-extract here.
    let path = match path {
        Some(p) => p,
        None => {
            if !flagged.is_empty() {
                println!(
                    "  {} undecodable frame(s) — pixel proofs were extracted ON the recording's box \
                     during --extract-partial and pulled back to dev1 beside the partial JSON (the \
                     <partial>-pixels dir; see the '#186/#208 pixel proofs' section at the end of \
                     this run for the concrete dev1 paths); not re-extracted here — #208/#186",
                    flagged.len()
                );
            }
            return Ok(());
        }
    };

    if !flagged.is_empty() {
        let png_dir = out_dir.join(label);
        let (_selected, dropped) = select_frames_to_extract(&flagged, max_pixel_proof);
        if dropped > 0 {
            println!(
                "  PIXEL-PROOF CAP: {} undecodable frames, extracting only the first {} PNGs ({} \
                 not extracted; counts above are COMPLETE)",
                flagged.len(),
                flagged.len() - dropped,
                dropped
            );
        }
        let extracted =
            extract_frames_png(path, &flagged, &undecodable, &png_dir, max_pixel_proof)?;
        for e in &extracted {
            if e.sharp_qr_but_flagged_undecodable {
                println!(
                    "  DECODER BUG (Step-1/#106 regression): frame {} flagged undecodable but a \
                     SHARP QR decodes in the pixels -> {}",
                    e.frame_index,
                    e.png_path.display()
                );
            } else {
                println!(
                    "  undecodable frame {} -> {} (no readable QR at all)",
                    e.frame_index,
                    e.png_path.display()
                );
            }
        }
    }
    Ok(())
}

/// Print one #108 per-hop ABSOLUTE latency block (p50, p99, jitter, drift). Returns
/// whether a non-empty hop was computed (so a recording carrying no burn QR is
/// reported as such rather than silently omitted).
fn report_hop_latency(h: &Option<HopLatency>, label: &str, anchor: &str) -> bool {
    match h {
        Some(h) => {
            println!("=== {label} per-hop ABSOLUTE latency (#108, anchor: {anchor}) ===");
            println!(
                "  samples={} p50={:.2}ms p99={:.2}ms jitter(p99-p50)={:.2}ms drift={:+.4}ms/min",
                h.samples, h.stats.p50_ms, h.stats.p99_ms, h.jitter_ms, h.drift_ms_per_min
            );
            println!(
                "  (min={:.2} mean={:.2} p95={:.2} max={:.2} ms)",
                h.stats.min_ms, h.stats.mean_ms, h.stats.p95_ms, h.stats.max_ms
            );
            true
        }
        None => {
            println!(
                "=== {label} per-hop ABSOLUTE latency (#108) ===\n  NO SAMPLES — no node burn QR \
                 paired in the recording(s). Enable the #111/#257 burn (set genlock_burn=on, e.g. \
                 scripts/rig-mode.sh test) on the PROBE scene and pass the matching --burn-*-run-id."
            );
            false
        }
    }
}

/// One recording's decoded frames + (optionally) the recording file backing them. The frames
/// come from EITHER a live decode (fused / `--extract-partial`) OR a merged per-box PARTIAL
/// (#208); `rec_path` is `Some` only when the recording is on THIS host (so pixel-proof PNGs
/// can be extracted). `None` ⇒ merged partial: the contiguity/PASS verdict is unaffected, only
/// pixel-proof extraction is skipped (the per-box `--extract-partial` already wrote it).
struct DecodedRec {
    frames: Vec<RecordingFrame>,
    rec_path: Option<PathBuf>,
}

/// How the OPTIONAL cam1 GRAB recording (#105 node 2) was obtained for the verdict. The cam1
/// grab is no longer recorded by the harness (#179 — the cam1-capture burn rides into the stream
/// recording instead), so the per-box / stream-only flow uses [`Cam1Source::Absent`]. A manual
/// `--cam1` run whose grab decode fails is NON-FATAL (#187): the rest of the verdict still runs,
/// but the failure is recorded in the report's `nodes.cam1.unavailable` so it is never silent.
enum Cam1Source {
    /// cam1 grab decoded OK.
    Decoded(DecodedRec),
    /// cam1 grab decode FAILED non-fatally (#187) — reason surfaced in `nodes.cam1.unavailable`.
    DecodeFailed(String),
    /// No cam1 grab supplied (the default per-box / stream-only flow).
    Absent,
}

/// Decode a (strih/stream) recording IN PLACE into a [`DecodedRec`] (frames + its own path for
/// pixel proof). `None` path ⇒ the recording wasn't supplied (`Ok(None)`); a decode error aborts
/// (strih/stream are required for the hops they feed). The cam1 grab decode is NON-FATAL and is
/// handled separately (see [`Cam1Source`]).
fn decode_for(path: Option<&Path>, expected_node_burns: &[u32]) -> Result<Option<DecodedRec>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let frames = analyze_recording_with_burns(path, expected_node_burns)
        .with_context(|| format!("analyze recording {}", path.display()))?;
    Ok(Some(DecodedRec {
        frames,
        rec_path: Some(path.to_path_buf()),
    }))
}

/// #632 gap 1 — [`decode_for`] with the #207 fast-path gate split into MANDATORY (always
/// required) + ANY-OF (whichever ONE of [`CAMERA_UNDER_TEST_NODES`] is actually deployed this
/// run) groups — see [`analyze_recording_with_grouped_burns_optical`]. Used for the fused strih/stream
/// decode in `main()` so a cam3/cam4/cam5/cam6/cam2-deployed run gets the same #207 fast path a
/// cam1-deployed run always could.
fn decode_for_grouped(
    path: Option<&Path>,
    mandatory_burns: &[u32],
    any_of_burns: &[u32],
    min_distinct_optical: Option<(u32, usize)>,
) -> Result<Option<DecodedRec>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let frames = analyze_recording_with_grouped_burns_optical(
        path,
        mandatory_burns,
        any_of_burns,
        min_distinct_optical,
    )
    .with_context(|| format!("analyze recording {}", path.display()))?;
    Ok(Some(DecodedRec {
        frames,
        rec_path: Some(path.to_path_buf()),
    }))
}

/// #188 A/V-SYNC MODE — measure + report the video↔audio offset from one recording.
fn run_av_sync(args: &Args) -> Result<()> {
    let recording = args.av_sync.as_ref().expect("av_sync set");
    let marker_log_path = args.av_marker_log.as_ref().context(
        "--av-sync requires --av-marker-log <cam2 emit log CSV (index,frame_id,emit_ts_ns)>",
    )?;
    let marker_csv = std::fs::read_to_string(marker_log_path)
        .with_context(|| format!("read av marker log {}", marker_log_path.display()))?;
    let params = camera_box::qpsk_marker::AudioParams::rig60();
    let report = camera_box::probe::av_sync_recording::av_sync_from_recording(
        recording,
        &marker_csv,
        &params,
        args.av_audio_track,
        args.av_threshold,
        args.av_min_matched,
        args.av_cluster_tol_ms,
    )?;
    // The measured offset + the latency ADJUSTMENT it implies: ADD this (signed) to the video
    // source's current genlock latency to zero the offset (offset > 0 ⇒ video lags ⇒ negative
    // adjust). Reported as a raw signed delta — the operator applies it to THEIR current value,
    // then clamps to the genlock range via required_delay_ms (a clamped absolute here would hide
    // the sign whenever the unknown current delay isn't passed in).
    let json = serde_json::json!({
        "av_offset_ms": report.offset.offset_ms,
        "mad_ms": report.offset.mad_ms,
        "matched": report.offset.matched,
        "candidates": report.candidates,
        "audio_markers_decoded": report.audio_markers,
        "video_ticks": report.video_ticks,
        "emit_rows": report.emit_rows,
        "video_fps": report.fps,
        "latency_adjust_ms": -report.offset.offset_ms,
    });
    println!("{}", serde_json::to_string_pretty(&json)?);
    tracing::info!(
        av_offset_ms = report.offset.offset_ms,
        mad_ms = report.offset.mad_ms,
        matched = report.offset.matched,
        audio_markers = report.audio_markers,
        video_ticks = report.video_ticks,
        "A/V-sync offset measured (video − audio; >0 = video lags audio)"
    );
    Ok(())
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();

    // #208 MODE DISPATCH — three exclusive modes (default = the fused single-process verdict):
    //   --extract-partial <box> : decode the ONE local recording in place → a small partial JSON
    //                             (ids + timestamps, no frames/pixels copied); a recording is
    //                             NEVER moved box-to-box, only this small JSON.
    //   --merge-partials ...     : combine per-box partials (+ the small painter CSV on dev1)
    //                             into the SAME full-chain verdict the fused path produces.
    //   (neither)                : fused — decode every supplied recording on THIS host.
    if let Some(box_name) = args.extract_partial.clone() {
        return extract_partial(&args, &box_name);
    }
    if !args.merge_partials.is_empty() {
        return run_merge(&args);
    }
    if args.av_sync.is_some() {
        return run_av_sync(&args);
    }

    tracing::info!(
        strih = ?args.strih.as_ref().map(|p| p.display().to_string()),
        stream = ?args.stream.as_ref().map(|p| p.display().to_string()),
        painter = ?args.painter.as_ref().map(|p| p.display().to_string()),
        out_dir = %args.out_dir.display(),
        min_secs = args.min_secs,
        "recording-verdict start (fused)"
    );

    // FUSED: decode every supplied recording on THIS host (the legacy single-process path),
    // then build the verdict. Each recording is decoded for the burns it is KNOWN to carry
    // (strih: strih render burn + whichever camera is deployed; stream: + stream; cam1 grab:
    // cam1 only) for the #207 fast gate. #632 gap 1: the camera-under-test slot is an ANY-OF
    // group (cam1..cam6 are mutually exclusive — only the deployed one's burn ever appears), not
    // a hardcoded cam1-only mandatory check — see `camera_under_test_burn_ids`.
    // #707: strih/stream also require both cam2 dual-QR Vernier halves before skipping the
    // robust retry — see `extract_partial`'s identical wiring for the full reasoning.
    let min_distinct_optical = args.cam2_pin().map(|run_id| (run_id, 2));
    let strih = decode_for_grouped(
        args.strih.as_deref(),
        &[args.burn_strih_run_id],
        &camera_under_test_burn_ids(&args),
        min_distinct_optical,
    )?;
    let stream = decode_for_grouped(
        args.stream.as_deref(),
        &[args.burn_strih_run_id, args.burn_stream_run_id],
        &camera_under_test_burn_ids(&args),
        min_distinct_optical,
    )?;
    // #463: imag now carries its OWN digital corner burn (run_id BURN_RUN_ID_IMAG) — decode for
    // it so the #207 fast/robust gate looks for it. Backward compatible: a recording with no
    // burn at all (a build predating #463) simply decodes with none found, and the verdict falls
    // back to the cam2 optical tick's own contiguity (see `node_verdict_for_imag`).
    let imag = decode_for(args.imag.as_deref(), &[BURN_RUN_ID_IMAG])?;
    // #187: a cam1 grab decode failure is NON-FATAL — the stream-only hops still run, and the
    // failure is recorded in nodes.cam1 (never silent). The grab is OPTIONAL (#179).
    let cam1 = match args.cam1.as_deref() {
        None => Cam1Source::Absent,
        Some(p) => match analyze_recording_with_burns(p, &[args.burn_cam1_run_id]) {
            Ok(frames) => Cam1Source::Decoded(DecodedRec {
                frames,
                rec_path: Some(p.to_path_buf()),
            }),
            Err(e) => Cam1Source::DecodeFailed(format!("cam1 grab decode failed: {e:#}")),
        },
    };

    // Fused path: no carried colour — `build_node_colour_fail` samples each node's recording
    // directly. Same for A/V-sync (`None`, #312 item 2 PR A): the fused path decodes it directly
    // from `args.av_marker_log` + `args.stream` INSIDE `build_and_print_verdict` when both are
    // given — there is nothing to carry here (carrying only applies to the #208 merge path).
    let (_report, all_pass) =
        build_and_print_verdict(&args, strih, stream, cam1, None, None, imag, None)?;
    if !all_pass {
        std::process::exit(1);
    }
    Ok(())
}

/// The `VerdictConfig` for the STREAM node's per-recording tick DIAGNOSTIC. The stream box
/// records at `stream_capture_fps` (30 in the mixed 60+30 topology), NOT the cam/strih
/// `capture_fps` (60) — run 7020001 wired the base cfg straight through and the diagnostic
/// halved (`analyzed_secs` = 9022 frames / 60 = 150.4 s where the recording really spans
/// 300.7 s @30), with `expected_step` 60/60=1 instead of 60/30=2. Seconds and step must both
/// come from the STREAM rate.
fn stream_diag_cfg(base: &VerdictConfig, stream_capture_fps: f64) -> VerdictConfig {
    let mut cfg = base.clone();
    if stream_capture_fps > 0.0 {
        cfg.capture_fps = stream_capture_fps;
    }
    cfg
}

/// Build the full-chain verdict + print it + write the `--json` report, returning the report
/// JSON and the binary PASS. Operates on ALREADY-DECODED frames so the fused path (live decode)
/// and #208 merge path (deserialized per-box partials) share IDENTICAL logic — the merged
/// verdict is therefore equivalent to the fused output (same fields, same PASS semantics). The
/// ONLY recording-dependent step is pixel-proof PNG extraction, skipped when a `DecodedRec` has
/// no `rec_path` (merge mode); the contiguity/PASS gate is pure and unaffected.
///
/// #312 item 2 (PR A) — found in CI review: `#[allow(clippy::too_many_arguments)]` had been
/// MISPLACED for who knows how long (bound to `stream_diag_cfg` above, a plain 2-arg function
/// that never needed it, instead of to THIS function) — it never mattered while this function
/// sat at exactly 7 args (clippy's default threshold), but adding the 8th (`stream_av_sync`)
/// below immediately tripped `-D warnings` in the `--all-features` Lint CI job (never visible
/// locally — Tier-0 policy bans compiling `--features probe` on this box). Moved to the correct
/// item.
#[allow(clippy::too_many_arguments)]
fn build_and_print_verdict(
    args: &Args,
    strih: Option<DecodedRec>,
    stream: Option<DecodedRec>,
    cam1: Cam1Source,
    // #377 — the per-recording COLOUR summaries carried from the per-box `--extract-partial`
    // (merge mode). `strih_colour` is the strih recording's colour (→ cam1, whose clean source is
    // the strih recording, #133); `stream_colour` is the stream recording's colour (→ strih + stream).
    // Both `None` on the fused path, where colour is sampled directly from each node's `rec_path`.
    strih_colour: Option<camera_box::colour_verify::NodeColourSummary>,
    stream_colour: Option<camera_box::colour_verify::NodeColourSummary>,
    // #461: imag-nb's OWN recording (EPIC #466 Topology v2) — independent of strih/stream, no
    // burn, gated by the cam2 optical tick's own contiguity (see `node_verdict_for_imag`).
    imag: Option<DecodedRec>,
    // #312 item 2 (PR A) — the STREAM recording's carried A/V-sync marker inputs (`Some` only in
    // MERGE mode, when the stream box extracted with `--av-marker-log`; see `run_merge`). `None`
    // on the fused path — there, `all_cambox_av_sync` decodes directly from `args.stream` +
    // `args.av_marker_log` when both are given (see the `--switch-schedule` block below).
    stream_av_sync: Option<AvMarkerInputs>,
) -> Result<(serde_json::Value, bool)> {
    let cfg = VerdictConfig {
        capture_fps: args.capture_fps,
        min_secs: args.min_secs,
        refresh_hz: args.refresh_hz,
    };

    let mut all_pass = true;
    // #105 4-node machine-readable report (per-node verdict + per-hop loss + latency),
    // built incrementally and written to --json for the 2-graph report renderer.
    let mut report = serde_json::json!({ "nodes": {}, "hops": {}, "latency": {} });

    // #706 — load the switch schedule ONCE, up front, so it is available BOTH to the #186
    // per-camera loss loop below (which needs it to scope each CAMERA_UNDER_TEST_NODES node's
    // window to its OWN program segment(s) — see `scope_camera_window_to_own_schedule`) AND to
    // the #312 per-segment sweep further down (which previously re-loaded + re-parsed the same
    // file itself). One parse, one source of truth, both consumers always agree on the windows.
    let switch_schedule: Option<Vec<SwitchWindow>> = args
        .switch_schedule
        .as_ref()
        .map(|p| load_switch_schedule(p))
        .transpose()?;

    // The recording file backing each node's frames, for pixel proof. `None` ⇒ the frames came
    // from a merged per-box partial (the recording is on its own box, never copied here #208).
    let strih_rec: Option<PathBuf> = strih.as_ref().and_then(|d| d.rec_path.clone());
    let stream_rec: Option<PathBuf> = stream.as_ref().and_then(|d| d.rec_path.clone());

    // strih recording verdict (strict hop-1 endpoint). Keep the decoded frames so the
    // #108 per-hop latency engine can read each frame's cam2 + node-burn gen_ts_ns.
    // strih is OPTIONAL: when omitted (the cam1-only optical-readability loop) the
    // strih-dependent hops (cam→strih, strih→stream) are skipped and only the cam1
    // grab is decoded/assessed.
    let strih_data: Option<(Vec<RecordingFrame>, Vec<FrameTick>)> = match strih {
        Some(d) => {
            let strih_frames = d.frames;
            let strih_ticks = FrameTick::from_recording_frames(&strih_frames);
            let strih_v = verdict(&strih_ticks, &cfg);
            // Diagnostic only (#186): the per-recording beat metrics do not gate the
            // headline — the burn-id contiguity below is authoritative.
            report_recording_diag(
                "strih",
                strih_rec.as_deref(),
                &strih_v,
                &args.out_dir,
                args.max_pixel_proof,
            )?;
            report["nodes"]["strih"] = serde_json::json!({
                "frames": strih_v.total_frames,
                "analyzed_secs": strih_v.analyzed_secs, "undecodable": strih_v.undecodable_frames.len(),
                "diagnostic_only": true,
            });
            Some((strih_frames, strih_ticks))
        }
        None => {
            println!(
                "=== strih: SKIPPED (no strih input) — cam1-only optical-readability mode; \
                 cam→strih and strih→stream hops are unavailable ==="
            );
            None
        }
    };

    // stream recording verdict (headline endpoint). The per-recording continuity verdict
    // (undecodable / net copy/gap / 60→30 beat balance) is a DIAGNOSTIC only — it does
    // NOT gate the headline. #186: the SINGLE trustworthy loss verdict is the per-node
    // burn-id contiguity (the #186 block below); the per-recording beat metrics conflate
    // the 60→30 sampling beat with loss (the exact false-positive source the user flagged).
    // It is printed for context but never makes a contiguous-zero run FAIL.
    let mut stream_frames_opt: Option<Vec<RecordingFrame>> = None;
    if let Some(d) = stream {
        let stream_frames = d.frames;
        let stream_ticks = FrameTick::from_recording_frames(&stream_frames);
        // #11/#282: the stream recording runs at stream_capture_fps (30) — seconds AND the
        // expected tick step must come from ITS rate, not the cam/strih 60 (run 7020001:
        // the leaked 60 halved analyzed_secs and mis-set expected_step to 1).
        let stream_cfg = stream_diag_cfg(&cfg, args.stream_capture_fps);
        let stream_v = verdict(&stream_ticks, &stream_cfg);
        // Diagnostic (not a gate): surface undecodable + span. The #186 burn-contiguity
        // verdict is authoritative for loss.
        report_recording_diag(
            "stream",
            stream_rec.as_deref(),
            &stream_v,
            &args.out_dir,
            args.max_pixel_proof,
        )?;
        report["nodes"]["stream"] = serde_json::json!({
            "frames": stream_v.total_frames,
            "analyzed_secs": stream_v.analyzed_secs,
            "undecodable": stream_v.undecodable_frames.len(),
            "diagnostic_only": true,
        });
        stream_frames_opt = Some(stream_frames);
    }

    // cam1 GRAB node (#105 node 2): per-recording continuity verdict, the STRICT
    // cam1→strih hop, and the HONEST cam2→cam1 optical assessment + latency. cam1's
    // grab and strih's program carry the SAME camera frames, so cam1→strih is a strict
    // offset-immune tick-SET compare (the camera beat cancels) — a cam1 tick absent at
    // strih is a real cam1→strih DROP. A NON-FATAL cam1 grab decode failure (#187) is
    // recorded in nodes.cam1.unavailable so it is never silent; Absent ⇒ no grab (#179).
    let mut cam1_frames_opt: Option<Vec<RecordingFrame>> = None;
    match cam1 {
        Cam1Source::Decoded(d) => {
            let cam1_rec = d.rec_path.clone();
            let cam1_frames = d.frames;
            // cam1's own per-recording continuity (undecodable / net copy/gap WITHIN cam1).
            // Diagnostic only (#186) — the burn-id contiguity below is authoritative for loss.
            let cam1_ticks = FrameTick::from_recording_frames(&cam1_frames);
            let cam1_v = verdict(&cam1_ticks, &cfg);
            report_recording_diag(
                "cam1",
                cam1_rec.as_deref(),
                &cam1_v,
                &args.out_dir,
                args.max_pixel_proof,
            )?;
            report["nodes"]["cam1"] = serde_json::json!({
                "frames": cam1_v.total_frames,
                "analyzed_secs": cam1_v.analyzed_secs, "undecodable": cam1_v.undecodable_frames.len(),
                "diagnostic_only": true,
            });
            cam1_frames_opt = Some(cam1_frames);
        }
        Cam1Source::DecodeFailed(reason) => {
            // #187 non-fatal: the rest of the verdict still runs; record the failure so a
            // manual --cam1 run that fails is visible in the report, not silently dropped.
            eprintln!(
                "WARNING: cam1 grab unavailable — {reason} (the stream-only hops still run; #187)."
            );
            report["nodes"]["cam1"] = serde_json::json!({ "unavailable": true, "reason": reason });
        }
        Cam1Source::Absent => {}
    }

    // cam→strih honest assessment (no false zero claim). Needs both strih + painter;
    // skipped in cam1-only optical-readability mode. #186: DIAGNOSTIC only — not a gate.
    let mut cam_strih_clean: Option<bool> = None;
    if let (Some((_, strih_ticks)), Some(painter_path)) = (&strih_data, &args.painter) {
        let painter_ticks = parse_painter_ticks(painter_path)?;
        let a = cam_strih_assessment(strih_ticks, &painter_ticks, &cfg);
        println!("=== cam→strih assessment (DIAGNOSTIC, honest, NOT a zero-loss claim) ===");
        println!("  claims_zero_loss={}", a.claims_zero_loss);
        println!(
            "  unknown_ticks (in-range, never painted = real fault)={}",
            a.unknown_ticks.len()
        );
        println!(
            "  out_of_painter_range_ticks (uncertain, painter CSV didn't cover)={}",
            a.out_of_painter_range_ticks.len()
        );
        if !a.unknown_ticks.is_empty() {
            let shown: Vec<u32> = a.unknown_ticks.iter().copied().take(20).collect();
            println!("  unknown ticks (first 20): {shown:?}");
            // #186: DIAGNOSTIC only — the painter-tick assessment does NOT gate the
            // headline (the burn-id contiguity is the single trustworthy verdict).
        }
        println!("  LIMITATION: {}", a.limitation);
        cam_strih_clean = Some(a.unknown_ticks.is_empty());
        report["hops"]["cam_strih"] = serde_json::json!({
            "strict": false, "claims_zero_loss": a.claims_zero_loss,
            "unknown_ticks": a.unknown_ticks.len(),
            "out_of_painter_range_ticks": a.out_of_painter_range_ticks.len(),
        });
    }

    // #108 — per-hop ABSOLUTE latency from the in-frame node-burn + cam2 gen_ts_ns
    // stamps (the #111 burn must be live on the boxes for these to be non-empty). NO
    // networked record_start, NO idx/30 — every number is a difference of two stamps
    // that already share the DanteSync wall clock. Reported, never gated (a latency
    // gate is a separate decision; #108 asks for the stable, defined numbers).
    println!();
    let cam2_pin = args.cam2_pin();
    // #194: load the painter's per-tick gen→flip stamps from the --paint-log CSV (if it has
    // the 3-column flip format). flip_ts is the cam2 DISPLAY (page-flip) instant — the true
    // reference for cam2→cam1 (cam1_capture − flip_ts). Empty when no --painter / a pre-#194
    // 2-column log ⇒ cam2→cam1 transparently falls back to the gen-based (#179) number.
    let (painter_gen_by_tick, painter_flip_by_tick): (HashMap<u32, i64>, HashMap<u32, i64>) =
        match &args.painter {
            Some(p) => parse_painter_flip(p)?,
            None => (HashMap::new(), HashMap::new()),
        };
    // strih recording: node burn = strih; no foreign burn forwarded INTO strih.
    let strih_ids = RunIds {
        node_burn: args.burn_strih_run_id,
        cam2: cam2_pin,
        other_burns: vec![],
    };
    // cam→strih ABSOLUTE latency needs the strih recording (its in-frame strih-burn +
    // cam2 stamps). Skipped in cam1-only optical-readability mode.
    if let Some((strih_frames, _)) = &strih_data {
        let cam_strih_lat = hop_latency("cam→strih", &cam_strih_samples(strih_frames, &strih_ids));
        report_hop_latency(&cam_strih_lat, "cam→strih", "cam2 paint gen_ts_ns");
        report["latency"]["cam_strih"] = hop_lat_json(&cam_strih_lat);
    }

    // cam2→cam1 OPTICAL+GRAB latency (#105 node 2) — REAL, no #111 burn needed.
    // grab_ts (cam1 grab instant, sidecar) − cam2 paint gen_ts, both wall clock.
    if let Some(cam1_frames) = &cam1_frames_opt {
        match &args.cam1_grab_ts {
            Some(grab_ts_path) => {
                let grab_ts = parse_grab_ts(grab_ts_path)?;
                let c1_lat = hop_latency(
                    "cam2→cam1",
                    &cam2_cam1_samples(cam1_frames, &grab_ts, cam2_pin),
                );
                report_hop_latency(&c1_lat, "cam2→cam1 (optical+grab)", "cam2 paint gen_ts_ns");
                let mut c1_json = hop_lat_json(&c1_lat);
                // #175 PART 2: cam2→cam1 is the TEST-INJECTION hop (cam2 monitor → cam1
                // camera lens → v4l2 capture → grab record), NOT a production hop. In
                // production the camera films the REAL scene; there is no monitor in the
                // path. Label it honestly so the number is never read as a production camera
                // latency. (It IS a real measured optical+capture latency for the test rig.)
                if let Some(obj) = c1_json.as_object_mut() {
                    obj.insert(
                        "note".to_string(),
                        serde_json::Value::String(
                            "TEST-INJECTION hop (cam2 monitor → cam1 camera optical+v4l2 \
                             capture+grab); NOT a production camera latency — in production the \
                             camera films the real scene, no monitor in the path"
                                .to_string(),
                        ),
                    );
                }
                report["latency"]["cam2_cam1"] = c1_json;
                println!(
                    "  NOTE: cam2→cam1 is the TEST-INJECTION optical hop (monitor→camera+capture), \
                     NOT a production camera latency (production films the real scene)."
                );
            }
            None => println!(
                "=== cam2→cam1 (optical+grab) per-hop ABSOLUTE latency (#105 node 2) ===\n  \
                 RELATIVE/UNAVAILABLE — pass --cam1-grab-ts <sidecar.csv> (the --record-grab \
                 grab-timestamp log) to compute it (grab_ts − cam2 paint gen_ts, both wall clock)."
            ),
        }
        // cam1→strih ABSOLUTE latency needs strih's #111 burn paired against cam1's grab
        // instant; the #111 burn is NOT deployed, so this hop's absolute latency is marked
        // unavailable rather than faked. cam2→cam1 (above) and cam→strih (cam2→strih) ARE
        // available; cam1→strih = (cam→strih) − (cam2→cam1) once the burn lands.
        println!(
            "=== cam1→strih per-hop ABSOLUTE latency (#105 node 2) ===\n  \
             RELATIVE/UNAVAILABLE — needs the #111 strih burn QR (not deployed) paired with \
             cam1's grab instant. Derivable as (cam→strih) − (cam2→cam1) once #111 is live."
        );
    }
    if let Some(stream_frames) = &stream_frames_opt {
        // stream recording: node burn = stream; strih's burn is FOREIGN (forwarded in
        // the program feed) and MUST be excluded so it is never read as cam2.
        let stream_ids = RunIds {
            node_burn: args.burn_stream_run_id,
            cam2: cam2_pin,
            other_burns: vec![args.burn_strih_run_id],
        };
        // #111 PART A: prefer the WHOLE strih→stream hop from the STREAM recording
        // ALONE — the stream frames carry the FORWARDED strih burn + stream's own burn,
        // paired per cam2 tick, so the hop needs no separate strih recording (the
        // dispatch's "whole per-hop analysis from the single stream recording"). Fall
        // back to the two-recording method only when the strih burn is NOT forwarded
        // into the stream program (the from-stream pairing then yields no samples).
        let from_stream = strih_stream_samples_from_stream(
            stream_frames,
            cam2_pin,
            args.burn_strih_run_id,
            args.burn_stream_run_id,
        );
        let (ss_samples, source) = if !from_stream.is_empty() {
            (from_stream, "stream recording alone (forwarded strih burn)")
        } else if let Some((strih_frames, _)) = &strih_data {
            (
                strih_stream_samples(strih_frames, stream_frames, &strih_ids, &stream_ids),
                "two recordings (strih burn not forwarded into stream)",
            )
        } else {
            (
                Vec::new(),
                "unavailable (no forwarded strih burn, no --strih)",
            )
        };
        println!("  strih→stream latency source: {source}");
        let ss_lat = hop_latency("strih→stream", &ss_samples);
        report_hop_latency(&ss_lat, "strih→stream", "strih render gen_ts_ns");
        report["latency"]["strih_stream"] = hop_lat_json(&ss_lat);
        report["latency"]["strih_stream_source"] = serde_json::json!(source);
    }

    // #632 gap 2: the resolved camera-under-test's node label for the cam2→SOURCE V4L2
    // capture-drop diagnostic below (`--cam1-capture-stats`, TOP LEVEL / independent of
    // `stream_frames_opt`). Defaults to "cam1" (the pre-#632 behavior, unchanged when no stream
    // recording/burns are supplied at all) and is overwritten below to whichever
    // CAMERA_UNDER_TEST_NODES entry actually decoded in THIS run — mutually exclusive in a real
    // single-camera run, so at most one non-cam1 entry ever overrides the default. The v4l2
    // capture-drop sidecar's CONTENT is already correct for whichever camera the harness
    // deployed (`$CAM1_CAPTURE_STATS` in scripts/recording-e2e.sh is resolved to the actual
    // SOURCE camera's own sidecar, despite the historical "cam1" naming) — only the PRINTED
    // label/JSON key was stale.
    let mut camera_under_test_label: &str = "cam1";

    // ===================================================================================
    // #174 — FULL-CHAIN per-hop verdict from the SINGLE stream recording, paired on the
    // CLEAN DIGITAL BURN IDs. The cam1-capture burn (run_id = burn_cam1_run_id) rides
    // through NDI into strih's program and on into stream's, so ONE stream recording
    // carries every mark: cam2 optical dual-QR + cam1 burn + strih burn + stream burn.
    // Each hop pairs on the burn `frame_id` (the SAME integer end-to-end) — NOT the
    // 60→30 optical beat — so the 259-dropped-vs-real_gap=1 loss artifact and the
    // p99=3.4s latency outliers of run 1530670109 cannot recur. Computed ONLY when the
    // stream recording actually carries the burns (else each hop reports no samples).
    // ===================================================================================
    if let Some(stream_frames) = &stream_frames_opt {
        // #133: cam1's contiguity source-of-truth is the CLEAN 1080p STRIH recording, not the
        // downstream STREAM recording (the small cam1 burn is softened by the extra NDI hop +
        // HEVC re-encode by the time it reaches the stream — NOT a 4K upscale; both boxes record
        // 1080p, #196 premise invalid). #216 hardened the walk so a softened reorder no longer
        // manufactures a phantom drop, but the strih recording is still the crispest source.
        // The cam1 burn rides through NDI into BOTH recordings, so it is present in strih too.
        // Fall back to the stream recording ONLY
        // when there is no --strih (the cam1-only / stream-only mode) — then it is the best
        // available source, with the softening caveat. strih/stream nodes always read from the
        // stream recording (the only recording carrying their own burn co-located with cam2).
        // The stream recording path (for pixel proof) is the same for every site in this block.
        // #208: it is `None` when the stream frames came from a merged partial — pixel proof is
        // then skipped (the recording is on the stream box); the contiguity verdict is unaffected.
        let stream_path: Option<&Path> = stream_rec.as_deref();
        // cam1's source frames, its pixel-proof recording path, AND its label are decided by
        // ONE match on strih_data, so they can never desync (a future change to how strih_data
        // is built can't leave cam1 reading strih frames while extracting pixels from the stream
        // file). When strih frames are present (decode OR partial), cam1 reads its burn from the
        // clean 1080p strih recording; `strih_rec` is its pixel-proof path (None in merge mode).
        let (cam1_source, cam1_rec_path, cam1_source_label): (
            &[RecordingFrame],
            Option<&Path>,
            &str,
        ) = match &strih_data {
            Some((strih_frames, _)) => (
                strih_frames,
                strih_rec.as_deref(),
                "strih 1080p recording (clean, #133)",
            ),
            // no strih input (or strih decode unavailable) ⇒ cam1 falls back to the stream
            // recording for BOTH frames and pixel proof, labelled as the softened source.
            None => (
                stream_frames,
                stream_path,
                "stream recording (no strih input; softened, may over-count — #133)",
            ),
        };
        let cam1_ids = burn_ids_in(cam1_source, args.burn_cam1_run_id);
        // #24/#312 — cam2/cam3/cam4/cam5/cam6 occupy the SAME "camera under test" role as cam1
        // (in the plain single-camera mode, mutually exclusive: only the ONE camera actually
        // deployed with CAMERA_BOX_BURN_RUN_ID set produces a non-empty id set here; in the
        // ALL-CAMBOX sweep all six are deployed at once, each present only in its own schedule
        // window), so they all read from the SAME clean source (`cam1_source`, #133) with their
        // OWN reserved burn run_id.
        let cam2_ids = burn_ids_in(cam1_source, args.burn_cam2_run_id);
        let cam3_ids = burn_ids_in(cam1_source, args.burn_cam3_run_id);
        let cam4_ids = burn_ids_in(cam1_source, args.burn_cam4_run_id);
        let cam5_ids = burn_ids_in(cam1_source, args.burn_cam5_run_id);
        let cam6_ids = burn_ids_in(cam1_source, args.burn_cam6_run_id);
        // #632 gap 2: resolve which CAMERA_UNDER_TEST_NODES entry actually produced ids in this
        // run, for the cam2→SOURCE V4L2 capture-drop label further down (independent of this
        // `if let` block's scope).
        camera_under_test_label = resolve_camera_under_test_label(
            !cam1_ids.is_empty(),
            !cam2_ids.is_empty(),
            !cam3_ids.is_empty(),
            !cam4_ids.is_empty(),
            !cam5_ids.is_empty(),
            !cam6_ids.is_empty(),
        );
        let strih_ids_seq = burn_ids_in(stream_frames, args.burn_strih_run_id);
        let stream_ids_seq = burn_ids_in(stream_frames, args.burn_stream_run_id);
        let any_burn = !cam1_ids.is_empty()
            || !cam2_ids.is_empty()
            || !cam3_ids.is_empty()
            || !cam4_ids.is_empty()
            || !cam5_ids.is_empty()
            || !cam6_ids.is_empty()
            || !strih_ids_seq.is_empty()
            || !stream_ids_seq.is_empty();
        if any_burn {
            println!();
            println!(
                "=== #174 FULL-CHAIN per-hop verdict (camera-under-test from the {cam1_source_label}; strih/stream from the stream recording) ==="
            );
            println!(
                "  burn ids: cam1={} cam2={} cam3={} cam4={} cam5={} cam6={} (from {cam1_source_label}) strih={} stream={} (stream recording)",
                cam1_ids.len(),
                cam2_ids.len(),
                cam3_ids.len(),
                cam4_ids.len(),
                cam5_ids.len(),
                cam6_ids.len(),
                strih_ids_seq.len(),
                stream_ids_seq.len()
            );
            report["full_chain"]["burn_ids_present"] = serde_json::json!({
                "cam1": cam1_ids.len(), "cam2": cam2_ids.len(), "cam3": cam3_ids.len(),
                "cam4": cam4_ids.len(), "cam5": cam5_ids.len(), "cam6": cam6_ids.len(),
                "strih": strih_ids_seq.len(), "stream": stream_ids_seq.len(),
            });
            report["full_chain"]["cam1_source"] = serde_json::json!(cam1_source_label);
            // #133 (review, #24/#312 generalized): if --strih was supplied (so the
            // camera-under-test's source IS the strih recording) but NONE of the six carried a
            // burn there, the camera leg is silently SKIPPED below and an all-zero headline could
            // stand WITHOUT the camera having been measured. The capture burn
            // (CAMERA_BOX_BURN_RUN_ID on whichever camera is under test) rides into strih's
            // program, so its absence in a --strih run means the burn was OFF or never reached
            // strih — loudly WARN so a "ZERO loss" headline is never read as a camera→strih proof
            // when the camera was unmeasured. (No hard fail:
            // a deliberate burn-off / strih+stream-only diagnostic run is still valid.)
            let camera_under_test_measured = !cam1_ids.is_empty()
                || !cam2_ids.is_empty()
                || !cam3_ids.is_empty()
                || !cam4_ids.is_empty()
                || !cam5_ids.is_empty()
                || !cam6_ids.is_empty();
            if strih_data.is_some() && !camera_under_test_measured {
                eprintln!(
                    "WARNING: --strih supplied but NO camera-under-test burn found in the strih \
                     recording (checked cam1={}, cam2={}, cam3={}, cam4={}, cam5={}, cam6={}) — \
                     the camera→strih hop is UNMEASURED this run (burn OFF or not reaching \
                     strih). A ZERO-loss headline below covers strih/stream ONLY.",
                    args.burn_cam1_run_id,
                    args.burn_cam2_run_id,
                    args.burn_cam3_run_id,
                    args.burn_cam4_run_id,
                    args.burn_cam5_run_id,
                    args.burn_cam6_run_id
                );
                report["full_chain"]["cam1_unmeasured"] = serde_json::json!(true);
            }

            // ===========================================================================
            // #186 — the ONE trustworthy, binary LOSS verdict (REPLACES the muddled
            // dropped/phantom/gap/painter-beat metrics). For EACH node, is its DIGITAL
            // monotonic burn-id sequence — decoded from THIS stream recording —
            // CONTIGUOUS? Contiguous ⇒ ZERO loss (every frame the node rendered reached
            // the recording). A missing id ⇒ ONE candidate dropped frame, classified by
            // VIEWING the pixels: a delivered frame whose burn QR was unreadable = a
            // BURN-READABILITY defect to FIX (never silently excluded); a genuinely absent
            // frame = a REAL drop. No percentages, no jargon.
            // ===========================================================================
            let all_burns = [
                args.burn_cam1_run_id,
                args.burn_cam2_run_id,
                args.burn_cam3_run_id,
                args.burn_cam4_run_id,
                args.burn_cam5_run_id,
                args.burn_cam6_run_id,
                args.burn_strih_run_id,
                args.burn_stream_run_id,
            ];
            println!();
            println!(
                "=== #186 ZERO-LOSS VERDICT — per-node burn-id contiguity (the ONE trustworthy check) ==="
            );
            let mut node_verdicts: Vec<NodeVerdict> = Vec::new();
            // `stream_path` (the strih/stream nodes' pixel-proof recording) is computed once
            // above, alongside the cam1 source selection.
            // #198: cam1's burn increments per EMITTED frame (src/main.rs), so its in-window id
            // run must be contiguous integers (a forward gap = a real cam1 drop). strih/stream
            // burn per RENDER tick (DistroAV filter), so a forward gap is expected, not loss.
            // #133: cam1 reads its burn from the CLEAN 1080p strih recording (cam1_source /
            // cam1_rec_path); strih + stream read from the stream recording (their own burns are
            // co-located with cam2 only there).
            // #374 nit 1 — compute the optical facts ONCE per SOURCE recording. cam1 reads its burn
            // from the cam1 source (the clean 1080p strih recording, #133); strih + stream both read
            // from the stream recording, so they SHARE one facts value instead of recomputing the
            // optical-span scan twice.
            let stream_optical = optical_span_facts(stream_frames, &all_burns, cam2_pin);
            let cam1_optical = optical_span_facts(cam1_source, &all_burns, cam2_pin);
            // #706 — the switch-schedule scope for the CAMERA_UNDER_TEST_NODES entries in the
            // loop below (see `scope_camera_window_to_own_schedule`'s doc): restricts each
            // camera's in-window delivered-frame set to ONLY its own program window(s) in the
            // ALL-CAMBOX fused sweep. `None` (no `--switch-schedule`) leaves every node's window
            // unchanged — the pre-#706 single-camera-continuously-on-program behavior.
            // `schedule_anchor_run_ids` mirrors the SAME priority (strih render time, then
            // stream) the #312 sweep further down uses to place a frame on the schedule
            // timeline — `Copy`, so it costs nothing to pass through all 8 loop iterations.
            let schedule_anchor_run_ids = [args.burn_strih_run_id, args.burn_stream_run_id];
            let schedule_scope: Option<ScheduleScope<'_>> =
                switch_schedule.as_deref().map(|schedule| ScheduleScope {
                    schedule,
                    anchor_run_ids: &schedule_anchor_run_ids,
                    guard_ns: args.switch_guard_ns,
                });
            // #364 — the colour gate samples a recording by path; strih and stream both point at the
            // stream recording, so without memoization that recording is colour-sampled TWICE
            // (identical work — mirrors the #374 nit 1 dedup of the optical scan). Cache the per-path
            // colour fail count so each source recording is sampled at most once.
            let mut colour_fail_cache: HashMap<PathBuf, usize> = HashMap::new();
            // #377 — per-node CARRIED colour (merge mode): cam1 takes the colour of its SOURCE
            // recording (the strih recording when --strih is supplied, else the stream fallback);
            // strih + stream take the stream recording's colour. All `None` on the fused path, where
            // `build_node_colour_fail` samples each node's `rec_path` directly.
            let cam1_carried_colour = if strih_data.is_some() {
                strih_colour.as_ref()
            } else {
                stream_colour.as_ref()
            };
            let strih_carried_colour = stream_colour.as_ref();
            let stream_carried_colour = stream_colour.as_ref();
            for (spec, present, optical, carried_colour) in [
                (
                    NodeSpec {
                        node: "cam1",
                        burn_run_id: args.burn_cam1_run_id,
                        rate: BurnRate::PerEmittedFrame,
                        source: cam1_source,
                        rec_path: cam1_rec_path,
                        cam2_run_id: cam2_pin,
                        // #571: cam1 IS decimation-aware now (the cam(60)->strih(30) hop).
                        step: node_render_step(
                            "cam1",
                            args.strih_emit_fps,
                            args.stream_capture_fps,
                            args.refresh_hz,
                            args.capture_fps,
                        ),
                    },
                    !cam1_ids.is_empty(),
                    cam1_optical,
                    cam1_carried_colour,
                ),
                (
                    // #24 — cam3 occupies the SAME camera-under-test role as cam1 (see the
                    // `CAMERA_UNDER_TEST_NODES` doc comment): same clean source, same optical
                    // facts, same carried colour. `present` (`!cam3_ids.is_empty()`) is false in
                    // every existing cam1-only run, so this is purely additive.
                    NodeSpec {
                        node: "cam3",
                        burn_run_id: args.burn_cam3_run_id,
                        rate: BurnRate::PerEmittedFrame,
                        source: cam1_source,
                        rec_path: cam1_rec_path,
                        cam2_run_id: cam2_pin,
                        step: node_render_step(
                            "cam3",
                            args.strih_emit_fps,
                            args.stream_capture_fps,
                            args.refresh_hz,
                            args.capture_fps,
                        ),
                    },
                    !cam3_ids.is_empty(),
                    cam1_optical,
                    cam1_carried_colour,
                ),
                (
                    // #24 — cam4, see the cam3 comment above.
                    NodeSpec {
                        node: "cam4",
                        burn_run_id: args.burn_cam4_run_id,
                        rate: BurnRate::PerEmittedFrame,
                        source: cam1_source,
                        rec_path: cam1_rec_path,
                        cam2_run_id: cam2_pin,
                        step: node_render_step(
                            "cam4",
                            args.strih_emit_fps,
                            args.stream_capture_fps,
                            args.refresh_hz,
                            args.capture_fps,
                        ),
                    },
                    !cam4_ids.is_empty(),
                    cam1_optical,
                    cam1_carried_colour,
                ),
                (
                    // #312 — cam2 occupies the SAME camera-under-test role for the DIGITAL
                    // contiguity check (see the `CAMERA_UNDER_TEST_NODES` doc comment) even
                    // though it is also the fixed optical painter — its own camera-box daemon
                    // keeps capturing+emitting throughout a TEST run (#291). `present`
                    // (`!cam2_ids.is_empty()`) is false unless the ALL-CAMBOX sweep deployed its
                    // burn, so this is purely additive.
                    NodeSpec {
                        node: "cam2",
                        burn_run_id: args.burn_cam2_run_id,
                        rate: BurnRate::PerEmittedFrame,
                        source: cam1_source,
                        rec_path: cam1_rec_path,
                        cam2_run_id: cam2_pin,
                        step: node_render_step(
                            "cam2",
                            args.strih_emit_fps,
                            args.stream_capture_fps,
                            args.refresh_hz,
                            args.capture_fps,
                        ),
                    },
                    !cam2_ids.is_empty(),
                    cam1_optical,
                    cam1_carried_colour,
                ),
                (
                    // #312 — cam5 (fleet growth 4→6, #451), see the cam3 comment above.
                    NodeSpec {
                        node: "cam5",
                        burn_run_id: args.burn_cam5_run_id,
                        rate: BurnRate::PerEmittedFrame,
                        source: cam1_source,
                        rec_path: cam1_rec_path,
                        cam2_run_id: cam2_pin,
                        step: node_render_step(
                            "cam5",
                            args.strih_emit_fps,
                            args.stream_capture_fps,
                            args.refresh_hz,
                            args.capture_fps,
                        ),
                    },
                    !cam5_ids.is_empty(),
                    cam1_optical,
                    cam1_carried_colour,
                ),
                (
                    // #312 — cam6 (fleet growth 4→6, #451), see the cam3 comment above.
                    NodeSpec {
                        node: "cam6",
                        burn_run_id: args.burn_cam6_run_id,
                        rate: BurnRate::PerEmittedFrame,
                        source: cam1_source,
                        rec_path: cam1_rec_path,
                        cam2_run_id: cam2_pin,
                        step: node_render_step(
                            "cam6",
                            args.strih_emit_fps,
                            args.stream_capture_fps,
                            args.refresh_hz,
                            args.capture_fps,
                        ),
                    },
                    !cam6_ids.is_empty(),
                    cam1_optical,
                    cam1_carried_colour,
                ),
                (
                    NodeSpec {
                        node: "strih",
                        burn_run_id: args.burn_strih_run_id,
                        rate: BurnRate::PerRenderTick,
                        source: stream_frames,
                        rec_path: stream_path,
                        cam2_run_id: cam2_pin,
                        // #360: strih's free-running render tick uses gap-ignore (see
                        // node_render_step's doc) — refresh_hz/capture_fps are unused for it.
                        step: node_render_step(
                            "strih",
                            args.strih_emit_fps,
                            args.stream_capture_fps,
                            args.refresh_hz,
                            args.capture_fps,
                        ),
                    },
                    !strih_ids_seq.is_empty(),
                    stream_optical,
                    strih_carried_colour,
                ),
                (
                    NodeSpec {
                        node: "stream",
                        burn_run_id: args.burn_stream_run_id,
                        rate: BurnRate::PerRenderTick,
                        source: stream_frames,
                        rec_path: stream_path,
                        cam2_run_id: cam2_pin,
                        // The stream burn is emitted AND recorded by the same stream OBS ⇒ step 1
                        // (no decimation between its render and its own recording).
                        step: node_render_step(
                            "stream",
                            args.strih_emit_fps,
                            args.stream_capture_fps,
                            args.refresh_hz,
                            args.capture_fps,
                        ),
                    },
                    !stream_ids_seq.is_empty(),
                    stream_optical,
                    stream_carried_colour,
                ),
            ] {
                if !present {
                    continue;
                }
                let mut nv = node_verdict_with_optical(
                    &spec,
                    &all_burns,
                    optical,
                    &args.out_dir,
                    args.max_pixel_proof,
                    schedule_scope,
                )?;
                // #364 — the per-camera COLOUR gate: charge any reference patch wrong on a majority
                // of sampled frames as a HARD fail (mirrors the optical read). 0 when `--colour-gate`
                // is off. The ffmpeg/process glue lives in `build_node_colour_fail` (excluded from
                // mutants like the other I/O glue); the JUDGEMENT is the mutation-tested
                // `colour_verify` module.
                nv.colour_fail =
                    build_node_colour_fail(&spec, carried_colour, args, &mut colour_fail_cache)?;
                // #356 — cross-recording camera-under-test reconciliation (#24: generalized from
                // cam1-only to any of cam1/cam3/cam4 — see [`CAMERA_UNDER_TEST_NODES`]). The
                // camera-under-test's contiguity is read from the CLEAN upstream strih recording
                // (#133); at the high-latency 60→30 hop its small burn QR softens and some ids go
                // UNREADABLE in the strih recording even though the frame was DELIVERED. A
                // REAL-DROP id that IS decoded in the DOWNSTREAM stream recording was proven
                // delivered → re-classify it BURN-UNREADABLE (a strih-recording readability gap),
                // NOT a chain loss — honest headline accounting (the #226 spirit, reconciled ACROSS
                // recordings). Applied ONLY to the camera-under-test node, ONLY when a distinct
                // downstream stream recording backs the claim (strih_data.is_some() ⇒ the camera
                // reads from the strih recording and `stream_frames` is the genuine downstream
                // recording; NEVER when it already falls back to the stream recording itself, where
                // "present in stream" would be vacuously true for every id). SAFETY: an id ABSENT
                // from the stream recording (a genuine loss, OR a 30 fps-decimated id we cannot
                // prove) is NEVER downgraded — it stays REAL DROP. This only moves the CLASSIFICATION
                // bucket; it never touches `missing_ids`, so a downgraded id still makes the sequence
                // non-contiguous — `is_zero()` is unchanged and NO false ZERO can ever be created.
                if CAMERA_UNDER_TEST_NODES.contains(&spec.node) && strih_data.is_some() {
                    let downstream_camera: std::collections::BTreeSet<u32> =
                        burn_ids_in(stream_frames, spec.burn_run_id)
                            .into_iter()
                            .collect();
                    let real_drop_ids: Vec<u32> = nv
                        .classified
                        .iter()
                        .filter(|c| c.kind == MissingKind::RealDrop)
                        .map(|c| c.id)
                        .collect();
                    let downgrade =
                        camera_box::burn_reconcile::cam1_real_drops_proven_delivered_downstream(
                            real_drop_ids,
                            &downstream_camera,
                        );
                    if !downgrade.is_empty() {
                        for c in nv.classified.iter_mut() {
                            if c.kind == MissingKind::RealDrop && downgrade.contains(&c.id) {
                                c.kind = MissingKind::BurnUnreadable;
                            }
                        }
                        println!(
                            "  [{}] #356 cross-recording reconcile: {} REAL-DROP id(s) present in \
                             the downstream stream recording were DELIVERED → re-classified \
                             BURN-UNREADABLE (strih-recording readability gap, not a chain loss).",
                            spec.node,
                            downgrade.len()
                        );
                    }
                }
                // #373 — the analyzed OPTICAL span (the cam2 dual-QR FIRST..=LAST decoded-frame
                // window) must clear the >=min_secs floor, or the headline VACUOUSLY passes over a
                // COLLAPSED / partial read: over a tiny span optical_undecodable==0 and the burn
                // window is trivially contiguous, so `is_zero()` alone would declare a fake green.
                // The PASS/FAIL decision is the pure, Tier-0 `recording_span_gate` module.
                // #373 — divide by the PER-RECORDING capture rate, not one shared rate: cam1's
                // optical span is read from the strih recording (`capture_fps`, 60 on the rig);
                // strih/stream from the stream recording (`stream_capture_fps`, 30). One rate would
                // false-fail strih/stream's real 300 s span on the rig's --capture-fps 60.
                let node_fps = camera_box::recording_span_gate::node_capture_fps(
                    spec.node,
                    args.capture_fps,
                    args.stream_capture_fps,
                    args.imag_capture_fps,
                );
                let span_secs = nv.analyzed_span_secs(node_fps);
                let span_ok = nv.span_ok(node_fps, cfg.min_secs);
                // Printed AFTER span_ok is known so the per-node line can be suppressed on a
                // delivery-clean-but-collapsed-span node (no "ZERO loss" contradicting the headline).
                print_node_verdict(&nv, span_ok);
                if !span_ok {
                    println!(
                        "  [{}] NOT zero — analyzed optical span {:.1}s < {:.1}s floor: the cam2 \
                         dual-QR read COLLAPSED to {} frame(s); a contiguous burn window over so few \
                         frames proves nothing (#373).",
                        spec.node, span_secs, cfg.min_secs, nv.optical_span_frames
                    );
                }
                all_pass &= nv.is_zero() && span_ok;
                report["full_chain"]["loss"][spec.node] =
                    node_verdict_json(&nv, span_secs, span_ok, cfg.min_secs);
                node_verdicts.push(nv);
            }
            // The single binary headline, in plain words.
            let total_real: usize = node_verdicts.iter().map(NodeVerdict::real_drops).sum();
            let total_burn_unreadable: usize =
                node_verdicts.iter().map(NodeVerdict::burn_unreadable).sum();
            // #373 — the headline is ZERO loss only when every node is delivery-clean AND its
            // analyzed optical span cleared the duration floor (no vacuous pass over a collapsed read).
            let all_zero = node_verdicts.iter().all(|nv| {
                nv.is_zero()
                    && nv.span_ok(
                        camera_box::recording_span_gate::node_capture_fps(
                            &nv.contiguity.node,
                            args.capture_fps,
                            args.stream_capture_fps,
                            args.imag_capture_fps,
                        ),
                        cfg.min_secs,
                    )
            });
            if all_zero {
                println!(
                    "  >>> ZERO loss: all burn-id sequences CONTIGUOUS (no missing id on any node)."
                );
            } else {
                println!(
                    "  >>> NOT zero: {total_real} REAL DROP + {total_burn_unreadable} BURN-UNREADABLE \
                     (each id classified above with its pixel slot; fix every burn-unreadable burn)."
                );
            }
            report["full_chain"]["zero_loss"] = serde_json::Value::Bool(all_zero);
            report["full_chain"]["real_drops"] = serde_json::json!(total_real);
            report["full_chain"]["burn_unreadable"] = serde_json::json!(total_burn_unreadable);
            // (The old cam2-tick-keyed strih→stream/cam1→strih dropped/phantom loss was
            // removed in #186 — the burn-id contiguity above is the single trustworthy
            // loss verdict; latency below is a separate, unchanged measurement.)

            // --- per-hop LATENCY co-located in one stream frame (no cam2-tick pairing) ---
            if !cam1_ids.is_empty() && !strih_ids_seq.is_empty() {
                let lat = hop_latency(
                    "cam1→strih",
                    &chain_hop_samples_from_stream(
                        stream_frames,
                        args.burn_cam1_run_id,
                        args.burn_strih_run_id,
                    ),
                );
                report_hop_latency(&lat, "cam1→strih (burn-id)", "cam1 capture gen_ts_ns");
                report["full_chain"]["latency"]["cam1_strih"] = hop_lat_json(&lat);
            }
            if !strih_ids_seq.is_empty() && !stream_ids_seq.is_empty() {
                let lat = hop_latency(
                    "strih→stream",
                    &chain_hop_samples_from_stream(
                        stream_frames,
                        args.burn_strih_run_id,
                        args.burn_stream_run_id,
                    ),
                );
                report_hop_latency(&lat, "strih→stream (burn-id)", "strih render gen_ts_ns");
                report["full_chain"]["latency"]["strih_stream"] = hop_lat_json(&lat);
            }
            // cam1→stream END-TO-END latency (cam1 capture → stream render), one frame.
            if !cam1_ids.is_empty() && !stream_ids_seq.is_empty() {
                let lat = hop_latency(
                    "cam1→stream",
                    &chain_hop_samples_from_stream(
                        stream_frames,
                        args.burn_cam1_run_id,
                        args.burn_stream_run_id,
                    ),
                );
                report_hop_latency(
                    &lat,
                    "cam1→stream (end-to-end, burn-id)",
                    "cam1 capture gen_ts_ns",
                );
                report["full_chain"]["latency"]["cam1_stream"] = hop_lat_json(&lat);
            }

            // #209: PER-FRAME latency time-series CSV — the LITERAL continuous-line proof.
            // One row per delivered stream frame (cam2 tick + co-located burns), carrying
            // the three per-hop latencies on the shared DanteSync clock. Written when the
            // operator passes --latency-csv, OR by default next to --json (so the
            // time-series is produced ALONGSIDE the summary). The plotter
            // scripts/latency-line-report.py turns it into the per-hop line PNG.
            //
            // Default location = the JSON file's OWN directory (`latency-per-frame.csv`
            // beside verdict.json), NOT --out-dir: the CLAUDE.md on-stream pattern passes
            // --json and --out-dir in DIFFERENT directories, and the doc + the consumer
            // expect the CSV next to the JSON summary. Fall back to out-dir only when the
            // JSON path has no parent (e.g. a bare filename in the cwd).
            let csv_path: Option<PathBuf> = args.latency_csv.clone().or_else(|| {
                args.json.as_ref().map(|j| {
                    j.parent()
                        .filter(|p| !p.as_os_str().is_empty())
                        .unwrap_or(&args.out_dir)
                        .join("latency-per-frame.csv")
                })
            });
            // Build the per-frame rows ONCE (the #209 CSV + the #216 optical-dropout finding both
            // read them). The CSV now carries the cam2→cam1 OPTICAL column (#216): present where
            // the cam1 camera read the cam2 QR, empty where it did NOT (the honest gap).
            let csv_rows = per_frame_latency_csv_rows(
                stream_frames,
                args.burn_cam1_run_id,
                args.burn_strih_run_id,
                args.burn_stream_run_id,
                cam2_pin,
                &painter_flip_by_tick,
            );
            if let Some(path) = &csv_path {
                match write_latency_csv(path, &csv_rows) {
                    Ok(n) => {
                        println!(
                            "  PER-FRAME latency CSV ({n} rows) → {} \
                             (plot: scripts/latency-line-report.py --csv {})",
                            path.display(),
                            path.display()
                        );
                        report["full_chain"]["latency_csv"] = serde_json::json!({
                            "path": path.display().to_string(),
                            "rows": n,
                            "columns": camera_box::probe::recording_latency::LatencyCsvRow::HEADER,
                        });
                        tracing::info!(path = %path.display(), rows = n, "#209 per-frame latency CSV written");
                    }
                    Err(e) => {
                        // Never fail the verdict on a CSV-write hiccup — the JSON summary +
                        // the headline are the gate; the CSV is the visual aid.
                        eprintln!(
                            "WARNING: could not write per-frame latency CSV {}: {e}",
                            path.display()
                        );
                    }
                }
            }

            // #216 HONEST FINDING — report the cam2→cam1 OPTICAL-READ dropout windows (stretches
            // where the cam1 camera could not optically read the cam2 monitor QR while the burns
            // kept flowing). This is a REAL readability failure on the cam2→cam1 OPTICAL-injection
            // leg — NOT a chain frame loss — surfaced openly (never hidden behind a drawn-across
            // line). It does NOT affect the zero-loss gate; it is a labeled diagnostic alongside it.
            let optical_dropouts =
                camera_box::probe::recording_latency::optical_read_dropouts(&csv_rows, 2.0);
            let total_dropout_s: f64 = optical_dropouts.iter().map(|d| d.dur_s).sum();
            report["full_chain"]["cam2_cam1_optical_read_dropouts"] = serde_json::json!({
                "count": optical_dropouts.len(),
                "total_seconds": total_dropout_s,
                "windows": optical_dropouts,
                "note": "cam2→cam1 OPTICAL-injection READ DROPOUT — the cam1 camera could not \
                         optically read the cam2 monitor QR for these windows. A test-rig \
                         optical-read failure (NOT a chain frame loss): the digital burn hops \
                         stayed continuous through it. The plotter draws the cam2→cam1 line with \
                         a visible labeled gap here — never drawn across.",
            });
            if optical_dropouts.is_empty() {
                println!(
                    "  [cam2→cam1 optical] no read dropout — the cam1 camera read the cam2 QR \
                     throughout (no optical-injection blackout)."
                );
            } else {
                println!(
                    "  [cam2→cam1 optical] {} READ DROPOUT window(s), {:.0}s total — the cam1 \
                     camera could not optically read the cam2 QR (optical-injection read failure, \
                     NOT a chain frame loss; burn hops stayed continuous):",
                    optical_dropouts.len(),
                    total_dropout_s
                );
                for d in &optical_dropouts {
                    println!(
                        "    · {:.0}s–{:.0}s ({:.0}s, {} frames) — shown as a labeled GAP on the graph",
                        d.start_s, d.end_s, d.dur_s, d.frames
                    );
                }
            }

            // cam2→cam1 OPTICAL-INJECTION latency from the STREAM recording ALONE (no 7.3GB
            // cam1 grab). The cam1-capture burn (#174) rides into the stream recording
            // carrying cam1's CAPTURE wall-clock ts; the cam2 optical QR cam1 FILMED rides in
            // the SAME frame carrying cam2's tick (frame_id) + paint gen_ts.
            //
            // #194: reference the cam2 DISPLAY (page-flip) instant, NOT the paint instant.
            // The QR can only carry gen_ts (rendered pre-flip), so when the painter --paint-log
            // CSV with the flip column is supplied (tick → flip_ts_ns from the SAME painter
            // session as this recording), the cam2→cam1 latency = cam1_capture − flip_ts[tick]
            // (real display→capture). The painter's own generate→display time (render +
            // vblank-wait, ~16-30ms) is REMOVED and reported separately below. WITHOUT a flip
            // map (no --painter, or a pre-#194 2-column log) it falls back to the #179
            // gen-based number (cam1_capture − cam2_paint), labelled as the inflated reference.
            if !cam1_ids.is_empty() {
                let use_flip = !painter_flip_by_tick.is_empty();
                let (samples, anchor_label, ref_desc) = if use_flip {
                    (
                        cam2_cam1_samples_from_flip(
                            stream_frames,
                            cam2_pin,
                            args.burn_cam1_run_id,
                            &[args.burn_strih_run_id, args.burn_stream_run_id],
                            &painter_flip_by_tick,
                        ),
                        "cam2 flip (display) ts_ns",
                        "cam2→cam1 (optical-injection, co-located cam1 burn vs cam2 DISPLAY/flip ts, no grab) [#194]",
                    )
                } else {
                    (
                        cam2_cam1_samples_from_burn(
                            stream_frames,
                            cam2_pin,
                            args.burn_cam1_run_id,
                            &[args.burn_strih_run_id, args.burn_stream_run_id],
                        ),
                        "cam2 paint gen_ts_ns",
                        "cam2→cam1 (optical-injection, co-located cam1 burn vs cam2 PAINT ts, no grab) [#179 — no --painter flip log; INFLATED by painter gen→display, supply --painter for #194]",
                    )
                };
                let c1_lat = hop_latency("cam2→cam1", &samples);
                report_hop_latency(&c1_lat, ref_desc, anchor_label);
                let mut c1_json = hop_lat_json(&c1_lat);
                // cam2→cam1 is the TEST-INJECTION hop (cam2 monitor → cam1 camera lens →
                // v4l2 capture), NOT a production hop — in production the camera films the
                // REAL scene, no monitor in the path. Label it so the number is never read
                // as a production camera latency.
                if let Some(obj) = c1_json.as_object_mut() {
                    obj.insert(
                        "reference".to_string(),
                        serde_json::Value::String(
                            if use_flip {
                                "cam2_display_flip_ts (#194)"
                            } else {
                                "cam2_paint_gen_ts (#179, inflated)"
                            }
                            .to_string(),
                        ),
                    );
                    obj.insert(
                        "note".to_string(),
                        serde_json::Value::String(
                            "TEST-INJECTION hop (cam2 monitor → cam1 camera optical+v4l2 capture), \
                             read CO-LOCATED from the cam1-capture burn + cam2 QR in the stream \
                             recording (no grab decode); NOT a production camera latency"
                                .to_string(),
                        ),
                    );
                }
                report["full_chain"]["latency"]["cam2_cam1"] = c1_json;
                println!(
                    "  NOTE: cam2→cam1 is the TEST-INJECTION optical hop (monitor→camera+capture), \
                     NOT a production camera latency (production films the real scene)."
                );
                if !use_flip {
                    println!(
                        "  NOTE: no --painter flip log → cam2→cam1 referenced to cam2 PAINT (gen) ts, \
                         which is INFLATED by the painter's render + vblank-wait (#194). Supply \
                         --painter <paint-log.csv> for the true display→capture latency."
                    );
                }

                // #194: report the painter's INTERNAL generate→display time separately, so the
                // test-rig artifact removed from cam2→cam1 stays VISIBLE rather than hidden.
                if use_flip && !painter_gen_by_tick.is_empty() {
                    let internal =
                        painter_internal_gen_to_flip(&painter_gen_by_tick, &painter_flip_by_tick);
                    if let Some(pl) = hop_latency("painter gen→flip", &internal) {
                        report_hop_latency(
                            &Some(pl.clone()),
                            "painter INTERNAL generate→display (render + vblank-wait — the test-rig \
                             time REMOVED from cam2→cam1) [#194]",
                            "painter gen_ts_ns",
                        );
                        report["full_chain"]["latency"]["painter_gen_to_flip"] =
                            hop_lat_json(&Some(pl));
                    }
                }
            }
        } else {
            println!(
                "=== #174 FULL-CHAIN burn-id verdict: SKIPPED — no cam1/strih/stream burn QR in the \
                 stream recording. Set CAMERA_BOX_BURN_RUN_ID on cam1 + genlock_burn=on on strih/stream \
                 (scripts/rig-mode.sh test) (+ --burn-*-run-id) and re-run for the clean per-hop loss + latency."
            );
        }
    }

    // cam2→SOURCE LOSS = the camera-under-test's V4L2 CAPTURE-DROP count (the camera leg: cam2
    // monitor → camera lens → its V4L2 capture). A dropped capture = a lost frame on that leg —
    // the kernel `sequence` gap the camera-box tracks (capture.rs), NOT a painter-tick optical
    // compare (which the 60→30 genlock decimation confounds, flagging present readable frames as
    // lost). The burn-id contiguity above covers the DIGITAL chain from the camera's EMITTED
    // frame onward (its burn increments per emit, after the genlock gate), so it cannot see a
    // capture drop UPSTREAM of the burn — this sidecar is that separate signal.
    //
    // #632 gap 2: `--cam1-capture-stats` is historically cam1-named, but the sidecar file it
    // points at is ALWAYS whichever camera the harness actually deployed (scripts/
    // recording-e2e.sh resolves `$CAM1_CAPTURE_STATS` to the SOURCE camera's own sidecar,
    // #24) — only the printed label/JSON key was stale, hardcoded to "cam1" regardless. Use the
    // `camera_under_test_label` resolved above (from whichever CAMERA_UNDER_TEST_NODES burn id
    // actually decoded this run) so a cam3/cam4/cam5/cam6 run reports its OWN name instead of a
    // misleading "cam1".
    //
    // Run at TOP LEVEL (not nested under the full-chain burn block): the cam2→SOURCE loss
    // depends ONLY on --cam1-capture-stats, so a supplied gate flag is ALWAYS parsed + gated
    // and a missing/malformed file ALWAYS errors — even when --stream is absent or the stream
    // carried no burns (otherwise a supplied capture-drop sidecar showing real drops could be
    // silently ignored while OVERALL printed ZERO loss).
    if let Some(stats_path) = &args.cam1_capture_stats {
        let stats = parse_cam1_capture_stats(stats_path)?;
        let capture_zero = stats.v4l2_dropped == 0;
        if capture_zero {
            println!(
                "  [cam2→{camera_under_test_label}] ZERO loss — {camera_under_test_label} V4L2 \
                 capture dropped 0 frames ({} captured).",
                stats.frames_captured
            );
        } else {
            // Denominator is the TOTAL the device should have produced = delivered + dropped
            // (frames_captured counts only delivered buffers, not the lost ones).
            let total = stats.frames_captured.saturating_add(stats.v4l2_dropped);
            println!(
                "  [cam2→{camera_under_test_label}] NOT zero — {camera_under_test_label} V4L2 \
                 capture dropped {} of {} frames ({} delivered; REAL capture-card drops on the \
                 camera leg).",
                stats.v4l2_dropped, total, stats.frames_captured
            );
        }
        all_pass &= capture_zero;
        report["full_chain"]["loss"][format!("cam2_{camera_under_test_label}")] = serde_json::json!({
            "zero_loss": capture_zero,
            "v4l2_dropped": stats.v4l2_dropped,
            "frames_captured": stats.frames_captured,
            "source": format!(
                "{camera_under_test_label} V4L2 sequence-gap capture-drop (camera leg) — not a painter-tick compare"
            ),
        });
    }

    // #461/#463 — imag-nb (EPIC #466 Topology v2): its zero-loss proof is the cam2 OPTICAL
    // tick's own first..=last contiguity (imag captures the 60Hz painter 1:1 at 60fps — no
    // 60→30 beat) ANDed with its OWN digital corner burn's contiguity (run_id BURN_RUN_ID_IMAG,
    // #463) WHEN that burn is present in the recording — see `node_verdict_for_imag`. Run at TOP
    // LEVEL (like the cam2→cam1 capture-stats gate above): --imag is INDEPENDENT of
    // --strih/--stream and must be gated whether or not either is supplied.
    //
    // #467: captured into `imag_frames_opt` (borrowed here) rather than consumed outright — the
    // #312 ALL-CAMBOX --switch-schedule sweep below ALSO needs imag's frames, to place them onto
    // the SAME schedule timeline (anchored on imag's #463 digital burn, falling back to the cam2
    // optical paint) and gate imag's own per-segment continuity alongside the per-cambox windows.
    let imag_frames_opt: Option<Vec<RecordingFrame>> = imag.map(|d| d.frames);
    if let Some(imag_frames) = &imag_frames_opt {
        let nv = node_verdict_for_imag(imag_frames, args.cam2_pin());
        let node_fps = camera_box::recording_span_gate::node_capture_fps(
            "imag",
            args.capture_fps,
            args.stream_capture_fps,
            args.imag_capture_fps,
        );
        let span_secs = nv.analyzed_span_secs(node_fps);
        let span_ok = nv.span_ok(node_fps, cfg.min_secs);
        print_node_verdict(&nv, span_ok);
        if !span_ok {
            println!(
                "  [imag] NOT zero — analyzed optical span {:.1}s < {:.1}s floor: the cam2 \
                 dual-QR read COLLAPSED to {} frame(s); a contiguous tick window over so few \
                 frames proves nothing (#373).",
                span_secs, cfg.min_secs, nv.optical_span_frames
            );
        }
        all_pass &= nv.is_zero() && span_ok;
        let mut imag_json = node_verdict_json(&nv, span_secs, span_ok, cfg.min_secs);
        // #575 — note the boundary trim honestly: the exact lead/tail frame counts excluded from
        // imag's optical tick + digital burn contiguity checks before this verdict was computed.
        imag_json["boundary_trim_lead_frames"] =
            serde_json::json!(camera_box::recording_boundary_trim::BOUNDARY_TRIM_LEAD_FRAMES);
        imag_json["boundary_trim_tail_frames"] =
            serde_json::json!(camera_box::recording_boundary_trim::BOUNDARY_TRIM_TAIL_FRAMES);
        report["full_chain"]["loss"]["imag"] = imag_json;
    }

    // #312 Phase-1 — ALL-CAMBOX per-segment continuity (the all-active splitter proof). When a
    // switch schedule is supplied, partition the SINGLE continuous stream recording into the per-
    // cambox program windows (by burn gen_ts_ns, minus the transition guard on each boundary) and
    // verify the painted-tick continuity PER cambox. Gates the headline alongside the per-node burn
    // verdict so a single cambox dropping in ITS ~30s window fails the run.
    if let Some(schedule) = switch_schedule.as_deref() {
        match &stream_frames_opt {
            Some(stream_frames) => {
                // #706: `schedule` is the ONE parse of `--switch-schedule`, hoisted to the top of
                // this function (shared with the #186 per-camera scoping above) — no longer
                // re-loaded/re-parsed here.
                // The painted tick's by-design step in the stream recording = the decimation of the
                // 60Hz painter at the recording rate (refresh_hz / stream_capture_fps = 2). Derived
                // from the configured fps when --switch-expected-step is 0, else the explicit value.
                // #467: the derivation is now the shared Tier-0 pure `painted_tick_step` (also used
                // below for imag's OWN per-segment sweep, at imag's different native rate).
                let expected_step = if args.switch_expected_step > 0 {
                    args.switch_expected_step
                } else {
                    camera_box::recording_span_gate::painted_tick_step(
                        args.refresh_hz,
                        args.stream_capture_fps,
                    )
                };
                let anchor_run_ids = [args.burn_strih_run_id, args.burn_stream_run_id];
                // #312: widened from cam1-only to all six reserved camera-under-test burn ids
                // (mirroring the SAME widening this PR made to `all_burns` in the #186 contiguity
                // block above and `latency_all_burns` in the #624 per-camera latency block below)
                // — defense-in-depth so a forwarded cam2/cam3/cam4/cam5/cam6 digital burn payload
                // can never be mistaken for cam2's OPTICAL tick when `--cam2-run-id` is unpinned.
                // In practice every recording-e2e.sh invocation always pins `--cam2-run-id`,
                // which takes precedence over this fallback — this widening only hardens the
                // unpinned/manual-invocation path.
                let all_burns = [
                    args.burn_cam1_run_id,
                    args.burn_cam2_run_id,
                    args.burn_cam3_run_id,
                    args.burn_cam4_run_id,
                    args.burn_cam5_run_id,
                    args.burn_cam6_run_id,
                    args.burn_strih_run_id,
                    args.burn_stream_run_id,
                ];
                let (seg_frames, no_anchor) = segment_frames_from_recording(
                    stream_frames,
                    &anchor_run_ids,
                    &all_burns,
                    cam2_pin,
                );
                let seg =
                    segment_continuity(&seg_frames, schedule, args.switch_guard_ns, expected_step);
                println!();
                println!(
                    "=== #312 ALL-CAMBOX per-segment continuity ({} window(s), guard {} ns, painted-tick step {}) ===",
                    seg.segments.len(),
                    seg.guard_ns,
                    seg.expected_step
                );
                for s in &seg.segments {
                    println!(
                        "  {} [{}..{}): frames={} undecodable={} copies={} gaps={} → {}",
                        s.cambox,
                        s.start_ns,
                        s.end_ns,
                        s.frames,
                        s.undecodable,
                        s.copies,
                        s.gaps,
                        if s.pass { "PASS" } else { "FAIL" }
                    );
                    // #333: a frames=0 window is empty by construction (the painter / a non-emitting
                    // box), NOT chain loss — print the explicit diagnostic so it is not misread.
                    if let Some(note) = &s.note {
                        println!("      ⚠ {note}");
                    }
                    // #726: presentation-cadence EVENNESS — REPORTED only (not yet gate-enforced;
                    // see src/presentation_cadence.rs). `None` on any window with no painted tick
                    // (every non-cam2 window in a sweep).
                    if let Some(pc) = &s.presentation_cadence {
                        println!(
                            "      cadence: evenness={:.3} uniform={}/{} duplicate={} catchup={} paired_events={} other={}",
                            pc.evenness_score,
                            pc.uniform_steps,
                            pc.sample_deltas,
                            pc.duplicate_steps,
                            pc.catchup_steps,
                            pc.paired_events,
                            pc.other_steps
                        );
                    }
                }
                if no_anchor > 0 {
                    println!(
                        "  ({no_anchor} recorded frame(s) had no burn/optical gen_ts anchor — not placed)"
                    );
                }
                if seg.unplaceable_frames > 0 {
                    println!(
                        "  ({} frame(s) fell outside every scheduled window — not attributed)",
                        seg.unplaceable_frames
                    );
                }
                println!(
                    "  >>> {}",
                    if seg.overall_pass {
                        "ALL camboxes CONTINUITY-CLEAN across their program windows."
                    } else {
                        "NOT clean: one or more cambox windows FAILED (see per-cambox above)."
                    }
                );
                let mut seg_json = serde_json::to_value(&seg).unwrap_or(serde_json::Value::Null);
                if let Some(obj) = seg_json.as_object_mut() {
                    obj.insert(
                        "frames_without_anchor".to_string(),
                        serde_json::json!(no_anchor),
                    );
                }
                report["all_cambox_continuity"] = seg_json;
                all_pass &= seg.overall_pass;

                // #467/#583 — extend the ALL-CAMBOX sweep to ALSO cover imag-nb's OWN recording:
                // place its frames onto the SAME schedule timeline (anchored on imag's #463 digital
                // corner burn, falling back to the cam2 optical paint — the SAME anchor priority the
                // strih/stream sweep uses, via `frame_gen_ts_anchor`) and gate each ~30s window with
                // the SAME honest #580v2 imag gate `node_verdict_for_imag` uses at the whole-recording
                // scale (`imag_tick_gate::imag_zero_loss`), NOT the strict painted-tick `window_segment`
                // (`copies==0 && gaps==0`). #583: the strict check false-FAILED imag's benign same-rate
                // optical beat (an isolated dup=copy, a skip=gap) per window while the headline PASSED
                // it — the two paths must agree. The honest gate: the cam2 optical read is LIVE with no
                // copy/freeze (`is_live_no_copy`) AND its undecodable stays within the #376 moiré rate
                // floor AND imag's own digital corner burn is a valid delivery proof (present floor +
                // step-aware contiguity). imag is never scene-switched by this harness (fixed on CAM1,
                // #462), so this proves imag's OWN delivery stayed continuous across the WHOLE sweep,
                // segmented at the SAME ~30s granularity as the other camboxes.
                //
                // Short-segment discipline: a ~30s window carries NO 300s duration floor (that #373
                // floor is a whole-recording headline term, applied to `full_chain.loss.imag`, never
                // per window). The honest gate still rejects a COLLAPSED read per window (the
                // advance-guard: `avg_step` must round to the expected step) and a COPY/FREEZE (the
                // run-length term), so a short window neither vacuously PASSES a frozen read nor
                // false-FAILS a legitimately-short-but-clean one. Optional: absent (`--imag` not
                // supplied) never fails the sweep; present, every window must pass.
                if let Some(imag_frames) = &imag_frames_opt {
                    // imag's cam2 optical read is captured 1:1 at its native 60fps (no 60->30
                    // decimation), so its optical expected step is IMAG_OPTICAL_EXPECTED_STEP — the
                    // SAME step `node_verdict_for_imag` uses (never the stream sweep's step 2).
                    let imag_optical_step = camera_box::imag_tick_gate::IMAG_OPTICAL_EXPECTED_STEP;
                    // #583 correctness-review finding: calibrate the digital-burn render step ONCE
                    // over the WHOLE imag recording (the largest sample available), not per short
                    // window — the render step is a property of the whole OBS pipeline session, not
                    // expected to vary window-to-window, and calibrating it independently per window
                    // (a much smaller sample) can under-trust a genuinely-larger step (fewer than
                    // `MIN_IDS_FOR_STEP_CALIBRATION` distinct ids falls back to the conservative
                    // constant) and manufacture a phantom missing id on a clean, merely-few-sampled
                    // window. Mirrors how `node_verdict_for_imag` calibrates once for the whole
                    // recording. All windows below reuse this SAME calibrated step.
                    let imag_burn_step = camera_box::imag_tick_gate::calibrate_burn_step(
                        &burn_ids_in(imag_frames, BURN_RUN_ID_IMAG),
                    );
                    // Partition imag's frames into the SAME schedule windows (by the #463 burn gen_ts
                    // anchor, fallback cam2 optical paint). The 1s transition guard already trims each
                    // window's boundary artifacts (~60 frames at 60fps — far more than the #575
                    // whole-recording 3-frame boundary trim), so NO per-window boundary trim is applied
                    // here (re-applying that whole-recording trim per short window would distort the
                    // beat — the trim is a recording-boundary concern, not a per-window one).
                    let (imag_windows, imag_no_anchor) = partition_frames_by_window(
                        imag_frames,
                        &[BURN_RUN_ID_IMAG],
                        &[BURN_RUN_ID_IMAG],
                        cam2_pin,
                        schedule,
                        args.switch_guard_ns,
                    );
                    println!();
                    println!(
                        "=== #467/#583 imag-nb per-segment continuity (honest #580v2 gate, its OWN \
                         recording, {} window(s), optical step {}, calibrated burn step {}) ===",
                        schedule.len(),
                        imag_optical_step,
                        imag_burn_step
                    );
                    let mut imag_overall_pass = !schedule.is_empty();
                    let mut imag_segments_json: Vec<serde_json::Value> =
                        Vec::with_capacity(schedule.len());
                    for (w, win_frames) in schedule.iter().zip(imag_windows.iter()) {
                        let ticks: Vec<u32> = win_frames.iter().filter_map(|f| f.tick).collect();
                        let burn_ids = burn_ids_in(win_frames, BURN_RUN_ID_IMAG);
                        let facts = optical_span_facts(win_frames, &[BURN_RUN_ID_IMAG], cam2_pin);
                        let zl = camera_box::imag_tick_gate::imag_zero_loss(
                            &ticks,
                            &burn_ids,
                            imag_burn_step,
                            facts.undecodable_in_span as u32,
                            facts.span_frames as u32,
                            imag_optical_step,
                        );
                        let pass = zl.is_zero_loss(OPTICAL_UNDECODABLE_RATE_MAX);
                        imag_overall_pass &= pass;
                        // #333: a frames==0 window is empty by construction (imag not emitting in that
                        // slice). Unlike the swept camboxes (an empty window there = the painter box),
                        // imag is never scene-switched, so an empty imag window IS a real gap in its
                        // own continuous recording — and the honest gate already FAILS it (no ticks ⇒
                        // not advancing ⇒ FAIL); the note only labels it.
                        let note: Option<String> = win_frames.is_empty().then(|| {
                            format!(
                                "imag produced 0 frames in the {} window — a real gap in imag's own \
                                 continuous recording (imag is never scene-switched, #462).",
                                w.cambox
                            )
                        });
                        println!(
                            "  (during {} program) [{}..{}): frames={} undecodable={} \
                             optical_advancing={} max_stuck_run={} avg_step={:.4} \
                             stuck_density={:.3}%(ok={}) local_stuck_density={:.3}%(ok={}) \
                             burn_present={} burn_missing={} → {}",
                            w.cambox,
                            w.start_ns,
                            w.end_ns,
                            win_frames.len(),
                            facts.undecodable_in_span,
                            zl.optical.is_advancing(),
                            zl.optical.max_stuck_run,
                            zl.optical.avg_step,
                            100.0 * zl.optical.stuck_density,
                            zl.optical.no_stuck_density(),
                            100.0 * zl.optical.local_stuck_density,
                            zl.optical.no_localized_stuck_density(),
                            zl.burn_present_ok,
                            zl.burn.missing_ids.len(),
                            if pass { "PASS" } else { "FAIL" }
                        );
                        if let Some(note) = &note {
                            println!("      ⚠ {note}");
                        }
                        imag_segments_json.push(serde_json::json!({
                            "cambox": w.cambox,
                            "start_ns": w.start_ns,
                            "end_ns": w.end_ns,
                            "frames": win_frames.len(),
                            "undecodable": facts.undecodable_in_span,
                            "optical_advancing": zl.optical.is_advancing(),
                            "optical_no_stuck_copy": zl.optical.no_stuck_copy(),
                            "optical_max_stuck_run": zl.optical.max_stuck_run,
                            "optical_avg_step": zl.optical.avg_step,
                            // #681 — these two #588/#604 density terms ALSO gate `pass` (via
                            // `is_live_no_copy`) but were previously omitted from this JSON
                            // entirely: a density-driven failure was therefore unexplainable from
                            // the report alone, reading as a "mystery" every-window failure. See
                            // `docs/autopilot-log.md` for the live RUN_ID 1783727115 investigation.
                            "optical_stuck_density": zl.optical.stuck_density,
                            "optical_no_stuck_density": zl.optical.no_stuck_density(),
                            "optical_local_stuck_density": zl.optical.local_stuck_density,
                            "optical_no_localized_stuck_density": zl.optical.no_localized_stuck_density(),
                            "burn_present_ok": zl.burn_present_ok,
                            "burn_first_id": zl.burn.first_id,
                            "burn_last_id": zl.burn.last_id,
                            "burn_missing_ids": zl.burn.missing_ids,
                            "pass": pass,
                            "note": note,
                        }));
                    }
                    if imag_no_anchor > 0 {
                        println!(
                            "  ({imag_no_anchor} imag recorded frame(s) had no burn/optical gen_ts \
                             anchor — not placed)"
                        );
                    }
                    println!(
                        "  >>> imag: {}",
                        if imag_overall_pass {
                            "CONTINUITY-CLEAN across every schedule window (honest #580v2 gate)."
                        } else {
                            "NOT clean — see the per-window detail above."
                        }
                    );
                    report["all_cambox_continuity"]["imag"] = serde_json::json!({
                        "segments": imag_segments_json,
                        "overall_pass": imag_overall_pass,
                        "guard_ns": args.switch_guard_ns.max(0),
                        "optical_expected_step": imag_optical_step,
                        "burn_render_step": imag_burn_step,
                        "frames_without_anchor": imag_no_anchor,
                    });
                    all_pass &= imag_overall_pass;
                }

                // #624 deliverables 1+3 — generalize the whole-recording, cam1-ONLY cam2→camera
                // OPTICAL-INJECTION hop above (#179/#194, `full_chain.latency.cam2_cam1`, LEFT
                // UNCHANGED) to EVERY OPTICAL-INJECTION camera-under-test node
                // (`OPTICAL_INJECTION_NODES`: cam1/cam3/cam4/cam5/cam6 — #312: NOT cam2, which is
                // the painter itself and has no second camera-vs-monitor optical hop to measure),
                // computed PER `--switch-schedule` window so the ALL_CAMBOX 30s-cut sweep
                // produces a REAL per-camera number: each camera's OWN capture-time burn rides
                // alongside cam2's optical QR only while ITS window is live (the sweep cuts each
                // camera into strih program in turn), so `camera_burn.gen_ts_ns − cam2.gen_ts_ns`,
                // restricted to that camera's window(s) and concatenated across every cycle the
                // sweep repeats it, is exactly that camera's own cam2→camera latency — the #286
                // root-cause per-camera d_X residue. The per-camera medians then feed the hard
                // cross-camera SPREAD gate (`camera_box::switch_latency::spread_verdict`): a
                // differing d_X beyond half a 30fps frame (16ms) can visibly break A/V lipsync
                // when the live program cuts between two cameras.
                //
                // Partition the SAME single stream recording used for the continuity sweep above
                // into the schedule windows, anchored the SAME way (reusing `anchor_run_ids` —
                // strih/stream burn gen_ts, falling back to the cam2 optical paint — so the two
                // sweeps can never disagree on which window a frame belongs to). The burn
                // exclusion list is widened to ALL EIGHT reserved ids (cam1/cam2/cam3/cam4/cam5/
                // cam6 + strih + stream) so a forwarded camera burn is never mistaken for cam2's
                // optical QR when no anchor id was found on a frame (the continuity sweep's own
                // `all_burns` above never needed all of them, since it never reads every payload).
                let latency_all_burns = [
                    args.burn_cam1_run_id,
                    args.burn_cam2_run_id,
                    args.burn_cam3_run_id,
                    args.burn_cam4_run_id,
                    args.burn_cam5_run_id,
                    args.burn_cam6_run_id,
                    args.burn_strih_run_id,
                    args.burn_stream_run_id,
                ];
                let (latency_windows, latency_no_anchor) = partition_frames_by_window(
                    stream_frames,
                    &anchor_run_ids,
                    &latency_all_burns,
                    cam2_pin,
                    schedule,
                    args.switch_guard_ns,
                );
                println!();
                println!(
                    "=== #624 ALL-CAMBOX per-camera cam2->camera latency ({} window(s)) ===",
                    schedule.len()
                );
                let use_flip = !painter_flip_by_tick.is_empty();
                let mut latency_json = serde_json::Map::new();
                // Only cameras that actually produced a sample this run feed the spread gate —
                // an absent camera (its window never in this sweep, or its burn never decoded)
                // must never contribute a fabricated 0ms.
                let mut measured_p50s_ms: Vec<f64> = Vec::new();
                for &camera in OPTICAL_INJECTION_NODES.iter() {
                    let own_burn = match camera {
                        "cam1" => args.burn_cam1_run_id,
                        "cam3" => args.burn_cam3_run_id,
                        "cam4" => args.burn_cam4_run_id,
                        "cam5" => args.burn_cam5_run_id,
                        "cam6" => args.burn_cam6_run_id,
                        _ => unreachable!(
                            "OPTICAL_INJECTION_NODES is exactly cam1/cam3/cam4/cam5/cam6"
                        ),
                    };
                    let other_burns: Vec<u32> = latency_all_burns
                        .iter()
                        .copied()
                        .filter(|&id| id != own_burn)
                        .collect();
                    let mut samples: Vec<LatencySample> = Vec::new();
                    let mut matched_windows = 0usize;
                    for (w, win_frames) in schedule.iter().zip(latency_windows.iter()) {
                        // #312/#333: the `cambox` schedule label is the harness's UPPERCASE
                        // sweep label (e.g. "CAM1"); `CAMERA_UNDER_TEST_NODES` is lowercase —
                        // compare case-insensitively (mirrors the file's existing
                        // `eq_ignore_ascii_case` convention).
                        if !w.cambox.eq_ignore_ascii_case(camera) {
                            continue;
                        }
                        matched_windows += 1;
                        let win_samples = if use_flip {
                            cam2_cam1_samples_from_flip(
                                win_frames,
                                cam2_pin,
                                own_burn,
                                &other_burns,
                                &painter_flip_by_tick,
                            )
                        } else {
                            cam2_cam1_samples_from_burn(
                                win_frames,
                                cam2_pin,
                                own_burn,
                                &other_burns,
                            )
                        };
                        samples.extend(win_samples);
                    }
                    let lat = hop_latency(&format!("cam2->{camera}"), &samples);
                    if let Some(h) = &lat {
                        measured_p50s_ms.push(h.stats.p50_ms);
                    }
                    println!(
                        "  {camera}: {matched_windows} window(s) matched, samples={}, p50={}",
                        samples.len(),
                        lat.as_ref()
                            .map(|h| format!("{:.2}ms", h.stats.p50_ms))
                            .unwrap_or_else(|| "NO SAMPLES".to_string())
                    );
                    latency_json.insert(camera.to_string(), hop_lat_json(&lat));
                }
                if latency_no_anchor > 0 {
                    println!(
                        "  ({latency_no_anchor} recorded frame(s) had no burn/optical gen_ts \
                         anchor — not placed for the #624 per-camera latency sweep)"
                    );
                }
                match camera_box::switch_latency::spread_verdict(&measured_p50s_ms) {
                    Some(sv) => {
                        println!(
                            "  >>> cross-camera spread: max={:.2}ms min={:.2}ms spread={:.2}ms \
                             (threshold {:.1}ms) → {}",
                            sv.max_p50_ms,
                            sv.min_p50_ms,
                            sv.spread_ms,
                            camera_box::switch_latency::SPREAD_THRESHOLD_MS,
                            if sv.pass { "PASS" } else { "FAIL" }
                        );
                        latency_json.insert(
                            "cross_camera_spread_ms".to_string(),
                            serde_json::json!(sv.spread_ms),
                        );
                        latency_json
                            .insert("spread_gate_pass".to_string(), serde_json::json!(sv.pass));
                        all_pass &= sv.pass;
                    }
                    None => {
                        eprintln!(
                            "WARNING: #624 cross-camera spread gate needs at least 2 measured \
                             cameras (cam1/cam3/cam4/cam5/cam6) to compare — only {} produced \
                             samples this run. The gate is UNMEASURED (never a fabricated pass \
                             or fail) and does not affect the run's overall verdict.",
                            measured_p50s_ms.len()
                        );
                        latency_json.insert(
                            "cross_camera_spread_ms".to_string(),
                            serde_json::Value::Null,
                        );
                        latency_json
                            .insert("spread_gate_pass".to_string(), serde_json::Value::Null);
                    }
                }
                report["all_cambox_latency"] = serde_json::Value::Object(latency_json);

                // #286 Gap 1+2 — per-camera DELIVERY latency (needs the STRIH recording, not
                // stream). The block above (`all_cambox_latency`/`cross_camera_spread_ms`)
                // measures each camera's own SOURCE-side photon-to-CAPTURE latency (`d_X`,
                // `camera_burn.gen_ts_ns − cam2.gen_ts_ns`) — architecturally BEFORE and
                // INDEPENDENT of strih's receiver-side per-source genlock hold
                // (`genlock_latency_ms_src`), the exact knob #286's phase-sync fix adjusts. A
                // differentiated per-source hold can NEVER move that number (confirmed live,
                // RUN_ID 1783609415 — see the #286 issue comment + the
                // `.claude/skills/e2e` "all_cambox_latency measures SOURCE-side d_X" gotcha).
                //
                // #286's own Verify criterion needs the DELIVERY latency instead —
                // `strih_burn.gen_ts_ns − camera_burn.gen_ts_ns`, which DOES include whatever
                // the receiver held the frame for. Each camera's OWN digital capture-time burn
                // rides, embedded in its emitted NDI pixels, all the way into strih's PROGRAM
                // output during its `--switch-schedule` window, co-located in the SAME
                // strih-recorded frame as strih's own render-time burn — exactly the pairing
                // `n_camera_strih_samples` already computes (existed since the #286 apply-step
                // comment, unit-tested, but never called from this binary until now). No window
                // partitioning is needed: at any instant strih PROGRAM shows exactly one
                // camera's feed, so a given camera's burn is present ONLY in strih-recorded
                // frames from its own cut-in window(s) — `n_camera_strih_samples`'s exact-match
                // `RunIds` pinning naturally attributes every sample to the right camera,
                // concatenated across every cycle the sweep repeats it (mirrors the source-side
                // sweep's own concatenation).
                //
                // ALL SIX `CAMERA_UNDER_TEST_NODES` are measured here, INCLUDING cam2 — unlike
                // the OPTICAL-INJECTION source-side sweep above (`OPTICAL_INJECTION_NODES`,
                // 5 members, cam2 excluded because it cannot optically film its own monitor),
                // cam2 has its OWN digital capture burn (`BURN_RUN_ID_CAM2`, #312/#637) and its
                // OWN `--switch-schedule` window (`CAMBOX_SWEEP` includes "Cam 2:CAM2") — so its
                // delivery latency is measurable the SAME digital way as every other camera, no
                // optical read required for THIS metric.
                //
                // Report-only for now: `spread_gate_pass` does NOT fold into `all_pass` — #286
                // is not yet closed/proven, and this field's purpose right now is to let a
                // manual re-verification run SEE whether the applied differentiated offsets
                // collapsed the delivery-time spread, not to add a new standing CI requirement.
                //
                // #714: keyed by camera, populated below when a --strih recording is present —
                // consumed further down by the A/V-sync block's `av_window::derive_camera_av_sync`
                // (a camera's own #286 delivery p50, re-centering cam2's whole-recording A/V
                // offset for a camera whose own per-window A/V pooling is sample-starved).
                let mut camera_delivery_p50: std::collections::HashMap<&str, f64> =
                    std::collections::HashMap::new();
                let mut delivery_p50s_ms: Vec<f64> = Vec::new();
                if let Some((strih_frames, _)) = &strih_data {
                    println!();
                    println!(
                        "=== #286 ALL-CAMBOX per-camera DELIVERY latency (strih recording, {} \
                         camera(s)) ===",
                        CAMERA_UNDER_TEST_NODES.len()
                    );
                    let delivery_camera_burn_ids = [
                        args.burn_cam1_run_id,
                        args.burn_cam2_run_id,
                        args.burn_cam3_run_id,
                        args.burn_cam4_run_id,
                        args.burn_cam5_run_id,
                        args.burn_cam6_run_id,
                    ];
                    let delivery_samples = n_camera_strih_samples(
                        strih_frames,
                        args.burn_strih_run_id,
                        &delivery_camera_burn_ids,
                    );
                    let mut delivery_json = serde_json::Map::new();
                    for (&camera, samples) in
                        CAMERA_UNDER_TEST_NODES.iter().zip(delivery_samples.iter())
                    {
                        let lat = hop_latency(&format!("{camera}->strih (delivery)"), samples);
                        if let Some(h) = &lat {
                            delivery_p50s_ms.push(h.stats.p50_ms);
                            camera_delivery_p50.insert(camera, h.stats.p50_ms); // #714
                        }
                        println!(
                            "  {camera}: samples={}, p50={}",
                            samples.len(),
                            lat.as_ref()
                                .map(|h| format!("{:.2}ms", h.stats.p50_ms))
                                .unwrap_or_else(|| "NO SAMPLES".to_string())
                        );
                        delivery_json.insert(camera.to_string(), hop_lat_json(&lat));
                    }
                    match camera_box::switch_latency::spread_verdict(&delivery_p50s_ms) {
                        Some(sv) => {
                            println!(
                                "  >>> delivery cross-camera spread: max={:.2}ms min={:.2}ms \
                                 spread={:.2}ms (threshold {:.1}ms) → {} (report-only — does \
                                 NOT gate all_pass, see #286)",
                                sv.max_p50_ms,
                                sv.min_p50_ms,
                                sv.spread_ms,
                                camera_box::switch_latency::SPREAD_THRESHOLD_MS,
                                if sv.pass { "PASS" } else { "FAIL" }
                            );
                            delivery_json.insert(
                                "cross_camera_spread_ms".to_string(),
                                serde_json::json!(sv.spread_ms),
                            );
                            delivery_json
                                .insert("spread_gate_pass".to_string(), serde_json::json!(sv.pass));
                        }
                        None => {
                            eprintln!(
                                "WARNING: #286 delivery-latency spread needs at least 2 measured \
                                 cameras to compare — only {} produced samples this run. The \
                                 gate is UNMEASURED (never a fabricated pass or fail).",
                                delivery_p50s_ms.len()
                            );
                            delivery_json.insert(
                                "cross_camera_spread_ms".to_string(),
                                serde_json::Value::Null,
                            );
                            delivery_json
                                .insert("spread_gate_pass".to_string(), serde_json::Value::Null);
                        }
                    }
                    report["all_cambox_delivery_latency"] =
                        serde_json::Value::Object(delivery_json);
                }

                // #312 item 2 — fuse the per-camera A/V-sync measurement (#188) into this SAME
                // run/verdict, alongside the loss (all_cambox_continuity) + latency
                // (all_cambox_latency) blocks above. Triggered when EITHER (a) this fused
                // single-process run was handed --av-marker-log directly (VERDICT_ON_STREAM=0,
                // the legacy decode-on-dev1 fallback), OR (b) the stream partial already carries
                // the decoded A/V-sync ingredients from its own on-box
                // `--extract-partial stream --av-marker-log` (VERDICT_ON_STREAM=1, the default —
                // the stream box is the ONLY place that has both the audio marker track and the
                // cam2 dual-QR video co-located, so its extract decodes them there and carries
                // them through the small partial JSON — mirrors #377's `colour` carry exactly).
                //
                // PR A (#639) reported `all_cambox_av_sync` (offsets, sample counts, any UNKNOWN
                // cameras) without gating `all_pass`. PR B (this) wires the ±20ms cross-window
                // bound (#624 deliverable 4, `av_window::av_offset_gate_pass`) into `all_pass` —
                // same severity as the loss/latency-spread gates, no "advisory" tier.
                let av_inputs: Option<AvMarkerInputs> =
                    match (stream_av_sync, &args.av_marker_log, &args.stream) {
                        (Some(carried), _, _) => Some(carried),
                        (None, Some(marker_log_path), Some(stream_path)) => {
                            let marker_csv = std::fs::read_to_string(marker_log_path)
                                .with_context(|| {
                                    format!("read --av-marker-log {}", marker_log_path.display())
                                })?;
                            let params = camera_box::qpsk_marker::AudioParams::rig60();
                            Some(decode_av_marker_inputs(
                                stream_path,
                                &marker_csv,
                                &params,
                                args.av_audio_track,
                                args.av_threshold,
                            )?)
                        }
                        _ => None,
                    };
                if let Some(av) = av_inputs {
                    println!();
                    println!(
                        "=== #312 item 2 ALL-CAMBOX per-camera A/V-sync (fps={:.3}, \
                         emit_log_rows={}, audio_markers={}) ===",
                        av.fps,
                        av.emit_log.len(),
                        av.audio_markers.len()
                    );
                    let mut av_json = serde_json::Map::new();
                    // #624 deliverable 4 / #312 item 2 PR B: every camera under test must PASS
                    // the ±20ms A/V-offset gate for the run's overall verdict to pass — folded
                    // into `all_pass` below, alongside all_cambox_continuity + all_cambox_latency.
                    let mut av_all_pass = true;
                    // #714 pass 1: compute every camera's OWN pooled measurement first (UNCHANGED
                    // logic from before), keyed by camera name — so cam2's own measured offset is
                    // available before ANY camera's derivation below regardless of loop order
                    // (CAMERA_UNDER_TEST_NODES lists cam2 second, not first).
                    let mut cam_syncs: std::collections::HashMap<&str, av_window::CameraAvSync> =
                        std::collections::HashMap::new();
                    for &camera in CAMERA_UNDER_TEST_NODES.iter() {
                        // cam2 is the emitter/painter itself: it has NO own schedule window where
                        // the optical dual-QR is visible (cam2's OWN camera does not film its own
                        // monitor — only cam1/cam3/cam4/cam5/cam6 do, each during THEIR window,
                        // see OPTICAL_INJECTION_NODES above). The A/V relationship being measured
                        // (cam2's paint tick vs cam2's own QPSK marker) is identical no matter
                        // which camera happens to be cut into strih program at that instant, so
                        // cam2's own number pools the WHOLE recording (unwindowed) — mirrors
                        // exactly how the legacy single-camera `.claude/skills/av-sync`
                        // measurement was taken (one continuous recording, no switch-schedule at
                        // all). This is also this PR's own sanity check: cam2's fused number
                        // should reproduce roughly that historical measurement.
                        let whole_recording = camera == "cam2";
                        let (windows_matched, per_window_candidates): (usize, Vec<Vec<f64>>) =
                            if whole_recording {
                                let ticks = av_window::window_ticks(
                                    &stream_frames
                                        .iter()
                                        .map(|f| (f.frame_index, f.tick))
                                        .collect::<Vec<_>>(),
                                    av.fps,
                                    av.video_start_s,
                                );
                                // #733 — deduped: collapse near-simultaneous duplicate decodes of
                                // the SAME marker (a real-data audit found 37-84ms-apart same-fid
                                // pairs) before clustering.
                                let cands = av_offset_candidates_deduped(
                                    &av.emit_log,
                                    &av.audio_markers,
                                    &ticks,
                                    DEDUPE_SAME_FID_WINDOW_S,
                                );
                                (1, vec![cands])
                            } else {
                                let mut matched = 0usize;
                                let mut per_window = Vec::new();
                                for (w, win_frames) in schedule.iter().zip(latency_windows.iter()) {
                                    if !w.cambox.eq_ignore_ascii_case(camera) {
                                        continue;
                                    }
                                    matched += 1;
                                    let ticks = av_window::window_ticks(
                                        &win_frames
                                            .iter()
                                            .map(|f| (f.frame_index, f.tick))
                                            .collect::<Vec<_>>(),
                                        av.fps,
                                        av.video_start_s,
                                    );
                                    per_window.push(av_offset_candidates_deduped(
                                        &av.emit_log,
                                        &av.audio_markers,
                                        &ticks,
                                        DEDUPE_SAME_FID_WINDOW_S,
                                    ));
                                }
                                (matched, per_window)
                            };
                        let cam_sync = av_window::pool_camera_av_sync(
                            windows_matched,
                            &per_window_candidates,
                            av_window::MIN_AV_SAMPLES,
                            args.av_cluster_tol_ms,
                        );
                        cam_syncs.insert(camera, cam_sync);
                    }
                    // #714: cam2's own measured offset — the anchor every Unknown non-cam2
                    // camera's derived estimate re-centers against below. `None` when cam2 itself
                    // is Unknown this run (derive_camera_av_sync then fails closed for every
                    // camera, never fabricating a number from a missing anchor).
                    let cam2_offset_ms = cam_syncs.get("cam2").and_then(|s| s.av_offset_ms);
                    for &camera in CAMERA_UNDER_TEST_NODES.iter() {
                        let whole_recording = camera == "cam2";
                        let cam_sync = cam_syncs.get(camera).expect(
                            "#714: every CAMERA_UNDER_TEST_NODES entry populated in pass 1 above",
                        );
                        // #714: for a non-cam2 camera that came back Unknown (per-window sample
                        // starvation — see av_window::derive_camera_av_sync's own doc comment for
                        // why this is sound, not fabricated), attempt the derived estimate from
                        // cam2's own offset + this camera's #286 delivery-latency delta. Never for
                        // cam2 itself (it has its own real measurement) and never when the real
                        // per-window measurement already succeeded.
                        let derived =
                            if !whole_recording && cam_sync.verdict == AvSyncVerdict::Unknown {
                                av_window::derive_camera_av_sync(
                                    cam2_offset_ms,
                                    camera_delivery_p50.get(camera).copied(),
                                    &delivery_p50s_ms,
                                    args.av_expected_ms,
                                )
                            } else {
                                None
                            };
                        let gate_pass = match &derived {
                            Some(d) => d.gate_pass,
                            None => av_window::av_offset_gate_pass(cam_sync, args.av_expected_ms),
                        };
                        av_all_pass &= gate_pass;
                        println!(
                            "  {camera}: {} windows={} candidates={} cluster_samples={} → {} \
                             [gate {}]",
                            if whole_recording {
                                "(whole-recording pool)"
                            } else {
                                "(per-window pool)"
                            },
                            cam_sync.windows,
                            cam_sync.candidates,
                            cam_sync.cluster_samples,
                            match (cam_sync.verdict, &derived) {
                                (AvSyncVerdict::Measured, _) => format!(
                                    "{:.1}ms (mad {:.1}ms)",
                                    cam_sync.av_offset_ms.unwrap_or(0.0),
                                    cam_sync.mad_ms.unwrap_or(0.0)
                                ),
                                (AvSyncVerdict::Unknown, Some(d)) => format!(
                                    "DERIVED {:.1}ms (#714: cam2 {:.1}ms + this camera's #286 \
                                     delivery delta, cross-camera delivery spread ±{:.1}ms)",
                                    d.derived_offset_ms,
                                    cam2_offset_ms.unwrap_or(0.0),
                                    d.delivery_spread_ms,
                                ),
                                (AvSyncVerdict::Unknown, None) => {
                                    "UNKNOWN (too few samples, no derivation possible)".to_string()
                                }
                            },
                            if gate_pass { "PASS" } else { "FAIL" }
                        );
                        let verdict_label = match (cam_sync.verdict, &derived) {
                            (AvSyncVerdict::Measured, _) => "measured",
                            (AvSyncVerdict::Unknown, Some(_)) => "derived",
                            (AvSyncVerdict::Unknown, None) => "unknown",
                        };
                        let mut cam_json = serde_json::json!({
                            "node": camera,
                            "windowing": if whole_recording { "whole_recording" } else { "per_window" },
                            "windows": cam_sync.windows,
                            "candidates": cam_sync.candidates,
                            "cluster_samples": cam_sync.cluster_samples,
                            "av_offset_ms": cam_sync.av_offset_ms,
                            "mad_ms": cam_sync.mad_ms,
                            "verdict": verdict_label,
                            "gate_pass": gate_pass,
                        });
                        if let Some(d) = &derived {
                            // #714: a DERIVED estimate is reported under its OWN fields, never
                            // written into `av_offset_ms`/`mad_ms` (which stay null — those are
                            // reserved for a genuine per-camera MEASUREMENT) — so no consumer can
                            // mistake a derived number for an independently measured one.
                            cam_json["derived_offset_ms"] = serde_json::json!(d.derived_offset_ms);
                            cam_json["derived_from_cam2_offset_ms"] =
                                serde_json::json!(cam2_offset_ms);
                            cam_json["derived_delivery_spread_ms"] =
                                serde_json::json!(d.delivery_spread_ms);
                            cam_json["derived_note"] = serde_json::json!(
                                "estimated (#714) from cam2's own measured whole-recording A/V \
                                 offset re-centered on this camera's own #286 delivery-latency \
                                 delta -- NOT an independent audio/video measurement for this \
                                 camera"
                            );
                        }
                        av_json.insert(camera.to_string(), cam_json);
                    }
                    println!(
                        "  >>> #624 deliverable 4 A/V-offset gate: expected={:.1}ms tolerance=±{:.1}ms → {}",
                        args.av_expected_ms,
                        av_window::AV_OFFSET_GATE_TOLERANCE_MS,
                        if av_all_pass { "PASS" } else { "FAIL" }
                    );
                    av_json.insert(
                        "expected_ms".to_string(),
                        serde_json::json!(args.av_expected_ms),
                    );
                    av_json.insert(
                        "gate_tolerance_ms".to_string(),
                        serde_json::json!(av_window::AV_OFFSET_GATE_TOLERANCE_MS),
                    );
                    av_json.insert("gate_pass".to_string(), serde_json::json!(av_all_pass));
                    av_json.insert(
                        "gate".to_string(),
                        serde_json::json!(format!(
                            "enforced — every camera under test must be within ±{:.0}ms of \
                             expected_ms (#624 deliverable 4 / #312 item 2 PR B)",
                            av_window::AV_OFFSET_GATE_TOLERANCE_MS
                        )),
                    );
                    report["all_cambox_av_sync"] = serde_json::Value::Object(av_json);
                    // #312 item 2 PR B: the per-camera A/V-offset gate now folds into the run's
                    // overall verdict, same severity as all_cambox_continuity / all_cambox_latency
                    // above — no separate "advisory" tier. See av_window's module doc comment.
                    all_pass &= av_all_pass;
                }
            }
            None => {
                eprintln!(
                    "WARNING: --switch-schedule given but no --stream recording — the all-cambox \
                     per-segment continuity needs the SINGLE continuous stream recording. The \
                     verdict cannot pass without it."
                );
                report["all_cambox_continuity"] = serde_json::json!({
                    "error": "no stream recording supplied (--stream is required for --switch-schedule)",
                });
                all_pass = false;
            }
        }
    }

    // Record the headline verdict and write the machine-readable report (BEFORE any
    // exit, so a FAIL run still produces the JSON the report renderer consumes).
    report["overall_pass"] = serde_json::Value::Bool(all_pass);
    report["cam_strih_clean"] = match cam_strih_clean {
        Some(b) => serde_json::Value::Bool(b),
        None => serde_json::Value::Null,
    };
    report["min_secs"] = serde_json::json!(args.min_secs);
    if let Some(json_path) = &args.json {
        std::fs::write(json_path, serde_json::to_string_pretty(&report)?)
            .with_context(|| format!("write report json {}", json_path.display()))?;
        tracing::info!(path = %json_path.display(), "4-node report JSON written");
    }

    // Headline — the SINGLE trustworthy binary verdict (#186). `all_pass` is driven by
    // the per-node burn-id contiguity (every node's burn sequence contiguous ⇒ ZERO loss).
    // A missing id classified as BURN-UNREADABLE is a real defect (the burn must be made
    // readable) and still FAILS the verdict — never silently excluded.
    println!();
    if all_pass {
        println!(
            "OVERALL: ZERO loss — every node's burn-id sequence is CONTIGUOUS (no missing id). \
             Every frame each node rendered reached the stream recording."
        );
    } else {
        println!(
            "OVERALL: NOT zero — see the per-node missing-id list above (each id classified REAL \
             DROP or BURN-UNREADABLE with its pixel slot). No percentage, no exclusion."
        );
    }
    // Return the report + PASS; the CALLER (main / run_merge) decides the process exit. The
    // builder must NEVER `process::exit` itself — the unit tests call it in-process (#208).
    Ok((report, all_pass))
}

/// #632 gap 2 — PURE (no I/O, unit-testable) resolution of the camera-under-test's node name for
/// the cam2→SOURCE V4L2 capture-drop label (`--cam1-capture-stats`, historically hardcoded to
/// "cam1"): cam1/cam2/cam3/cam4/cam5/cam6 are mutually exclusive in a real single-camera run, so
/// at most one of `cam2_present..cam6_present` is ever true. Returns "cam1" when `cam1_present`
/// (the common case, unchanged pre-#632 behavior) OR when NONE of the others are present either
/// (nothing to resolve — the pre-#632 default). Otherwise returns whichever of cam2..cam6 IS
/// present, checked in that fixed order (irrelevant in practice since they're mutually exclusive).
fn resolve_camera_under_test_label(
    cam1_present: bool,
    cam2_present: bool,
    cam3_present: bool,
    cam4_present: bool,
    cam5_present: bool,
    cam6_present: bool,
) -> &'static str {
    if !cam1_present {
        for (node, present) in [
            ("cam2", cam2_present),
            ("cam3", cam3_present),
            ("cam4", cam4_present),
            ("cam5", cam5_present),
            ("cam6", cam6_present),
        ] {
            if present {
                return node;
            }
        }
    }
    "cam1"
}

/// #638/#632: map a single [`CAMERA_UNDER_TEST_NODES`] entry to its own `--burn-cam*-run-id`
/// arg. Callers always iterate `CAMERA_UNDER_TEST_NODES` itself, so the `_` arm is unreachable
/// in practice — panicking there (rather than returning a bogus default) surfaces a caller bug
/// immediately instead of silently mis-mapping a camera's burn id.
fn burn_run_id_for_camera(node: &str, args: &Args) -> u32 {
    match node {
        "cam1" => args.burn_cam1_run_id,
        "cam2" => args.burn_cam2_run_id,
        "cam3" => args.burn_cam3_run_id,
        "cam4" => args.burn_cam4_run_id,
        "cam5" => args.burn_cam5_run_id,
        "cam6" => args.burn_cam6_run_id,
        _ => unreachable!(
            "burn_run_id_for_camera called with {node:?} — expected a CAMERA_UNDER_TEST_NODES entry"
        ),
    }
}

/// #632 gap 1 — the "any-of" burn-id group for the #207 fast-path gate: every
/// [`CAMERA_UNDER_TEST_NODES`] entry's own reserved burn id. Exactly ONE of these ever appears
/// in a real single-camera run (cam1/cam2/cam3/cam4/cam5/cam6 are mutually exclusive — only the
/// camera actually deployed with `CAMERA_BOX_BURN_RUN_ID` set produces a non-empty id set), so
/// requiring "at least one of these" (rather than "cam1 specifically") lets a cam2/cam3/cam4/
/// cam5/cam6-deployed recording take the fast path exactly like a cam1-deployed one already
/// could. See `decode_qr_luma_all_fast_then_robust_grouped`'s doc for why a flat UNION into one
/// mandatory list (requiring ALL six) would NOT fix this.
fn camera_under_test_burn_ids(args: &Args) -> Vec<u32> {
    CAMERA_UNDER_TEST_NODES
        .iter()
        .map(|&node| burn_run_id_for_camera(node, args))
        .collect()
}

/// #208 + #186: the sibling directory (BESIDE the partial JSON) where `--extract-partial` writes
/// this box's pixel-proof PNGs and `--merge-partials` looks for the pulled-back copies. For a
/// partial `…/strih-partial-42.json` it is `…/strih-partial-42-pixels`. Deriving it the SAME way
/// on both sides means the box writes the PNGs, the harness pulls the dir back beside the partial,
/// and the merge points the operator at the real dev1 location — no path is threaded around.
fn partial_pixels_dir(partial_path: &Path) -> PathBuf {
    let stem = partial_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "partial".to_string());
    partial_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{stem}-pixels"))
}

/// #208 + #186: the recorded-frame indices THIS box must extract pixel proofs for during
/// `--extract-partial`, so the merge's #186 "SEE the missing / undecodable frame" guarantee
/// survives the per-box split (the merge can NEVER re-extract — the recording is not on dev1).
///
/// Returns `(flagged, undecodable)`. `flagged` is the union (sorted, deduped) of
///   • the recording's UNDECODABLE frames (no readable QR at all — the `report_recording_diag` set), and
///   • the missing-burn slots for the nodes this box AUTHORITATIVELY backs — the SAME slots the
///     merge would flag, so the PNGs pulled back are exactly the #186 proofs:
///       - strih box → whichever ONE of [`CAMERA_UNDER_TEST_NODES`] is actually deployed (#638;
///         its burn is read from the clean 1080p strih recording, #133 — mutually exclusive, so
///         at most one of the six ever has a non-empty window),
///       - stream box → strih + stream (their burns are read from the stream recording).
/// `undecodable` is the undecodable subset (so `extract_frames_png` runs its sharp-but-flagged
/// self-check on those frames). PURE (no I/O) so the selection is unit-testable; the PNG write is
/// the thin `extract_frames_png` glue in [`extract_partial`].
fn extract_partial_flagged_frames(
    box_name: &str,
    frames: &[RecordingFrame],
    args: &Args,
) -> (Vec<u64>, HashSet<u64>) {
    // #638: every possible node burn id — all six CAMERA_UNDER_TEST_NODES ids + strih + stream —
    // must be excluded from "is this cam2's optical QR" (see `frame_is_delivered_optical`). Before
    // #638 this list only knew about cam1, so a cam3/cam4/cam5/cam6/cam2 burn on an unpinned
    // extract would have been misread as cam2's optical payload once those cameras could be the
    // one actually under test riding through this box's recording.
    let mut all_burns = camera_under_test_burn_ids(args);
    all_burns.push(args.burn_strih_run_id);
    all_burns.push(args.burn_stream_run_id);
    // UNDECODABLE frames (no readable QR at all) — the exact set `report_recording_diag` extracts.
    let ticks = FrameTick::from_recording_frames(frames);
    let cfg = VerdictConfig {
        capture_fps: args.capture_fps,
        min_secs: args.min_secs,
        refresh_hz: args.refresh_hz,
    };
    let v = verdict(&ticks, &cfg);
    let undecodable: HashSet<u64> = v.undecodable_frames.iter().copied().collect();
    let mut flagged: Vec<u64> = v.undecodable_frames.clone();

    // The missing-burn slots for the nodes THIS box authoritatively backs (#133): the strih box
    // backs whichever ONE of CAMERA_UNDER_TEST_NODES is actually deployed (#638; its burn is
    // crispest in the clean 1080p strih recording); the stream box backs strih + stream (their
    // own burns are co-located with cam2's optical QR only in the stream recording). These are
    // the SAME (node, source) pairings `build_and_print_verdict` uses, so the
    // missing slots — and thus the extracted PNG frame indices — match what the merge would flag.
    // #198: cam1's burn is per-EMITTED-frame (a forward gap is a real drop ON A 1:1 HOP — #571:
    // on the decimated cam(60)->strih(30) hop, step >= 2, forward gaps are by-design decimation
    // and never charged); strih/stream burn per-RENDER-tick (a forward gap is not loss, but a
    // delivered frame missing its burn is).
    // #360/#571: the SAME step the merge verdict uses (node_render_step — gap-ignore for strih's
    // free-running render tick; the cam(60)->strih(30) decimation ratio for cam1, #571), so the
    // on-box pixel-proof flagging matches what the merge flags.
    let strih_step = node_render_step(
        "strih",
        args.strih_emit_fps,
        args.stream_capture_fps,
        args.refresh_hz,
        args.capture_fps,
    );
    let owned: Vec<(&str, u32, BurnRate, i64)> = match box_name {
        // #638: the strih recording carries whichever ONE of CAMERA_UNDER_TEST_NODES is
        // actually deployed (forwarded through, #133) — flag ITS missing-burn slots, mirroring
        // how `build_and_print_verdict`'s own NodeSpec loop already treats all six uniformly.
        // Mutually exclusive in a real run, so at most one of the six ever has a non-empty
        // window here; iterating all six costs nothing extra for the five that never appear.
        "strih" => CAMERA_UNDER_TEST_NODES
            .iter()
            .map(|&node| {
                (
                    node,
                    burn_run_id_for_camera(node, args),
                    BurnRate::PerEmittedFrame,
                    node_render_step(
                        node,
                        args.strih_emit_fps,
                        args.stream_capture_fps,
                        args.refresh_hz,
                        args.capture_fps,
                    ),
                )
            })
            .collect(),
        "stream" => vec![
            (
                "strih",
                args.burn_strih_run_id,
                BurnRate::PerRenderTick,
                strih_step,
            ),
            (
                "stream",
                args.burn_stream_run_id,
                BurnRate::PerRenderTick,
                1,
            ),
        ],
        _ => Vec::new(),
    };
    // #273: thread the cam2 pin so the on-box pixel-proof flagging anchors the optical window to
    // THIS run's paint exactly as the merge verdict does (a foreign-run lead-in is not flagged as
    // delivered). `None` for an unpinned extract (e.g. the strih box runs without --cam2-run-id).
    let cam2_pin = args.cam2_pin();
    for &(node, burn_run_id, rate, step) in owned.iter() {
        let window = in_window_burn_frames(frames, burn_run_id, &all_burns, rate, cam2_pin);
        let iw = burn_contiguity_in_window_with_step(node, &window, rate, step);
        flagged.extend(iw.missing_slots.iter().map(|s| s.frame_index));
    }

    flagged.sort_unstable();
    flagged.dedup();
    (flagged, undecodable)
}

/// The node-burn run_ids a per-box partial is expected to carry, derived from the box name + the
/// `--burn-*-run-id` args: the strih recording carries whichever ONE of
/// [`CAMERA_UNDER_TEST_NODES`] is actually deployed (forwarded) + strih; the stream recording
/// (the chain endpoint) carries the same + stream; the imag recording carries its OWN digital
/// corner burn ([`BURN_RUN_ID_IMAG`], #463 — before #463 this was `Some(vec![])`, since imag-nb
/// had no digital burn and its zero-loss proof was the cam2 optical tick's own contiguity alone;
/// that optical fallback still applies when a recording carries no decoded burn at all — see
/// `node_verdict_for_imag`). `None` for an unknown box. SINGLE source of truth for the
/// `--merge-partials` consistency check (run_merge warns when a loaded partial's
/// `expected_burns` disagree with this) AND for the partial's recorded metadata; #632 gap 1 split
/// the ACTUAL `--extract-partial` #207 fast-path decode into a separate mandatory/any-of call
/// (`analyze_recording_with_grouped_burns`, see [`extract_partial`]) rather than reusing this
/// flat list directly — a flat list containing ALL SIX camera ids as one AND-gate would be
/// permanently unsatisfiable (only one camera is ever deployed per run).
fn args_expected_burns_for(box_name: &str, args: &Args) -> Option<Vec<u32>> {
    match box_name {
        "strih" => {
            let mut v = camera_under_test_burn_ids(args);
            v.push(args.burn_strih_run_id);
            Some(v)
        }
        "stream" => {
            let mut v = camera_under_test_burn_ids(args);
            v.push(args.burn_strih_run_id);
            v.push(args.burn_stream_run_id);
            Some(v)
        }
        "imag" => Some(vec![BURN_RUN_ID_IMAG]),
        _ => None,
    }
}

/// #208 PER-BOX decode-in-place: decode the box's LOCAL recording in place and write a SMALL
/// partial JSON (ids + timestamps, NO frames/pixels) PLUS the #186 pixel-proof PNGs for this box's
/// flagged frames (undecodable + the missing-burn slots for the nodes it backs) into the sibling
/// `<partial>-pixels` dir. The strih box decodes its strih recording (cam1 + strih burns); the
/// stream box decodes its stream recording (all three burns). dev1 then `--merge-partials` the
/// small JSONs (+ pulls back the pixel dirs) — the recording is NEVER copied box-to-box (nor to dev1).
fn extract_partial(args: &Args, box_name: &str) -> Result<()> {
    let expected_burns = args_expected_burns_for(box_name, args).ok_or_else(|| {
        anyhow::anyhow!(
            "--extract-partial: unknown box {box_name:?} (expected `strih`, `stream`, or `imag`)"
        )
    })?;
    let rec_path: &Path = match box_name {
        // The strih recording carries the cam1 (forwarded) + strih burns — never the stream burn.
        "strih" => args
            .strih
            .as_deref()
            .context("--extract-partial strih needs --strih <recording on the strih box>")?,
        // The stream recording is the chain endpoint — it carries all three forwarded burns.
        "stream" => args
            .stream
            .as_deref()
            .context("--extract-partial stream needs --stream <recording on the stream box>")?,
        // #463: the imag recording carries its OWN digital corner burn — expected_burns is
        // Some(vec![BURN_RUN_ID_IMAG]) above.
        "imag" => args
            .imag
            .as_deref()
            .context("--extract-partial imag needs --imag <recording on the imag-nb box>")?,
        // args_expected_burns_for already returned None (→ bailed) for any other box.
        _ => unreachable!("unknown box rejected by args_expected_burns_for above"),
    };
    tracing::info!(
        box_name,
        recording = %rec_path.display(),
        expected_burns = ?expected_burns,
        "extract-partial: decoding the LOCAL recording in place (#208 — nothing copied off-box)"
    );
    // #632 gap 1: strih/stream split the #207 fast-path gate into MANDATORY (the box's own hop
    // burn(s), always required) + ANY-OF (whichever ONE of CAMERA_UNDER_TEST_NODES is actually
    // deployed this run — mutually exclusive, so "at least one" unlocks the fast path exactly
    // like the historically-hardcoded cam1-only check did for cam1 specifically). imag has no
    // camera-under-test any-of group, so it keeps the plain mandatory-only decode unchanged.
    //
    // #707: strih AND stream ALSO carry the cam2 dual-QR Vernier optical read baked into their
    // recorded pixels (whichever cambox is on program at that instant), so both now ALSO require
    // both Vernier halves before skipping the #202 robust retry — see
    // `decode_qr_luma_all_fast_then_robust_grouped_pathed_optical`'s doc for the full reasoning.
    // `args.cam2_pin()` is `None` only for an unpinned (`--cam2-run-id 0`) debug invocation, in
    // which case this is byte-for-byte the pre-#707 gate.
    let min_distinct_optical = args.cam2_pin().map(|run_id| (run_id, 2));
    let frames = match box_name {
        "strih" => analyze_recording_with_grouped_burns_optical(
            rec_path,
            &[args.burn_strih_run_id],
            &camera_under_test_burn_ids(args),
            min_distinct_optical,
        ),
        "stream" => analyze_recording_with_grouped_burns_optical(
            rec_path,
            &[args.burn_strih_run_id, args.burn_stream_run_id],
            &camera_under_test_burn_ids(args),
            min_distinct_optical,
        ),
        _ => analyze_recording_with_burns(rec_path, &expected_burns),
    }
    .with_context(|| format!("analyze recording {}", rec_path.display()))?;

    let out = args
        .out
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("partial-{box_name}.json")));

    // #186/#208: write the pixel proofs for THIS box's flagged frames (undecodable + the
    // missing-burn slots for the nodes this box backs) into the sibling `<partial>-pixels` dir,
    // so the per-box flow does NOT drop the "SEE the frame" guarantee. The recording is on this
    // box during extract, so this is the ONE place the proof can be produced; the merge (dev1) can
    // never re-extract. Only the flagged-frame PNGs are written (a handful), pulled back beside the
    // partial JSON. A clean (zero-loss, fully decodable) run writes none.
    let (flagged, undecodable) = extract_partial_flagged_frames(box_name, &frames, args);
    if !flagged.is_empty() {
        let png_dir = partial_pixels_dir(&out);
        let extracted = extract_frames_png(
            rec_path,
            &flagged,
            &undecodable,
            &png_dir,
            args.max_pixel_proof,
        )?;
        println!(
            "#186/#208 pixel-proof [{box_name}]: {} flagged frame(s) → {} PNG(s) in {} \
             (undecodable + missing-burn slots; pull this dir to dev1 beside the partial).",
            flagged.len(),
            extracted.len(),
            png_dir.display(),
        );
    }

    // #377 — when --colour-gate, sample THIS recording's colour scale ON the box (the colour gate is
    // fused/on-host — the recording is only here) and carry the per-recording summary in the partial,
    // so the dev1 merge can gate colour without ever reading the recording. Errors LOUDLY if the
    // scale is unreadable (never a silent skip of a requested gate).
    let colour = if args.colour_gate {
        let summary = camera_box::probe::colour_sample::extract_recording_colour_summary(
            rec_path,
            args.colour_samples,
            camera_box::colour_scale::DEFAULT_QR_SIZE,
            camera_box::colour_scale::TOP_MARGIN_PX,
        )?;
        anyhow::ensure!(
            summary.any_chromatic_checked(),
            "colour gate: no CHROMATIC colour patch was checkable in {} (the colour scale is missing \
             or its R/G/B/C/M/Y patches are fully burn-covered) — cannot verify colour / detect a \
             grayscale camera for the {box_name} recording",
            rec_path.display()
        );
        Some(summary)
    } else {
        None
    };
    // #312 item 2 (PR A) — when --av-marker-log is given AND this is the STREAM box (the ONLY
    // recording that co-locates the audio marker track with the cam2 dual-QR video), decode the
    // A/V-sync marker inputs ON-HOST here and carry them through the partial (mirrors the
    // --colour-gate carry above exactly). Silently omitted for strih/imag — passing
    // --av-marker-log there is simply a no-op, never an error, since only the stream recording can
    // ever have the marker.
    let av_sync = if box_name == "stream" {
        match &args.av_marker_log {
            Some(marker_log_path) => {
                let marker_csv = std::fs::read_to_string(marker_log_path).with_context(|| {
                    format!("read --av-marker-log {}", marker_log_path.display())
                })?;
                let params = camera_box::qpsk_marker::AudioParams::rig60();
                Some(decode_av_marker_inputs(
                    rec_path,
                    &marker_csv,
                    &params,
                    args.av_audio_track,
                    args.av_threshold,
                )?)
            }
            None => None,
        }
    } else {
        None
    };
    let partial = RecordingPartial::from_frames(box_name, rec_path, &expected_burns, frames)
        .with_colour(colour)
        .with_av_sync(av_sync);
    partial.save(&out)?;
    let bytes = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    println!(
        "#208 partial-extract [{box_name}]: {} frames decoded in place → {} ({bytes} bytes JSON). \
         Pull this small JSON (+ the {}-pixels dir if present) to dev1; the {} recording stays on \
         its box (never copied).",
        partial.frames.len(),
        out.display(),
        partial_pixels_dir(&out)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        box_name,
    );
    Ok(())
}

/// #208 MERGE: combine the per-box partials (+ the small `--painter` / `--cam1-capture-stats`
/// files already on dev1) into the SAME full-chain verdict the fused path produces — with NO
/// recording read here. A strih partial can ONLY fill the `strih` slot and a stream partial the
/// `stream` slot (the file's recorded `box` must match its assignment), so a recording can never
/// be cross-fed; the partials carry only ids + timestamps.
fn run_merge(args: &Args) -> Result<()> {
    let mut strih: Option<DecodedRec> = None;
    let mut stream: Option<DecodedRec> = None;
    // #461/#463: imag now HAS a burn slot to reconcile too (`BURN_RUN_ID_IMAG`,
    // `args_expected_burns_for("imag", ..)` returns `Some(vec![BURN_RUN_ID_IMAG])` — the same
    // `partial.expected_burns != expected` WARN path below applies to an imag partial extracted
    // with a mismatched `--burn-*-run-id` set, exactly like strih/stream). It still needs no
    // colour-carry (colour-gate is not wired for imag in this ticket).
    let mut imag: Option<DecodedRec> = None;
    // #377 — the per-recording colour summaries carried in each partial (Some only when the box
    // extracted with --colour-gate). Threaded into the verdict so the colour gate works through the
    // split decode path (the gate is fused/on-host — the recording is only on the box).
    let mut strih_colour: Option<camera_box::colour_verify::NodeColourSummary> = None;
    let mut stream_colour: Option<camera_box::colour_verify::NodeColourSummary> = None;
    // #312 item 2 (PR A) — the stream partial's carried A/V-sync marker inputs (Some only when
    // that box extracted with `--av-marker-log`). Only the STREAM recording ever carries this
    // (the audio marker track + the cam2 dual-QR video are co-located there only), mirroring
    // `stream_colour` above.
    let mut stream_av_sync: Option<AvMarkerInputs> = None;
    // Each box's partial path, so after the verdict we can point the operator at the #186 pixel
    // proofs that box wrote during `--extract-partial` and the harness pulled back beside it.
    let mut box_paths: Vec<(String, PathBuf)> = Vec::new();
    for spec in &args.merge_partials {
        let (box_name, path) = spec
            .split_once('=')
            .with_context(|| format!("--merge-partials expects BOX=JSON, got {spec:?}"))?;
        let partial = RecordingPartial::load(Path::new(path))
            .with_context(|| format!("load partial {path}"))?;
        // The partial's recorded box name MUST match the slot it is assigned to — a strih
        // partial can NEVER be merged as the stream input (the #208 box-to-box guard, enforced
        // at the data level, not just the path).
        anyhow::ensure!(
            partial.box_name == box_name,
            "--merge-partials {spec}: the partial file's box is {:?} but it was assigned to {box_name:?}",
            partial.box_name
        );
        // Review (#208): WARN — never silently — on (1) a repeated box key (a later partial
        // silently overwrites the earlier slot) and (2) an `expected_burns` mismatch between the
        // loaded partial and this merge's `--burn-*-run-id` args (a manual burn-id mismatch between
        // extract and merge would otherwise pair on the wrong run_id and misverdict).
        if box_paths.iter().any(|(b, _)| b.as_str() == box_name) {
            eprintln!(
                "WARNING: --merge-partials {box_name}= specified more than once — the later partial \
                 ({path}) OVERWRITES the earlier one for the {box_name} slot."
            );
        }
        if let Some(expected) = args_expected_burns_for(box_name, args) {
            if partial.expected_burns != expected {
                eprintln!(
                    "WARNING: --merge-partials {box_name}: the partial was extracted for burns {:?} \
                     but this merge's --burn-*-run-id imply {expected:?} — a burn-id mismatch \
                     between extract and merge can MISVERDICT. Re-extract with matching --burn-* ids.",
                    partial.expected_burns
                );
            }
        }
        box_paths.push((box_name.to_string(), PathBuf::from(path)));
        // #377/#312 — take the carried colour summary + A/V-sync inputs before `frames` moves
        // into the DecodedRec.
        let colour = partial.colour;
        let av_sync = partial.av_sync;
        let rec = DecodedRec {
            frames: partial.frames,
            rec_path: None, // merge: the recording is on its own box, never on dev1
        };
        match box_name {
            "strih" => {
                strih = Some(rec);
                strih_colour = colour;
            }
            "stream" => {
                stream = Some(rec);
                stream_colour = colour;
                stream_av_sync = av_sync;
            }
            // #461: imag carries no burns, so there is no colour to carry either in this ticket.
            "imag" => {
                imag = Some(rec);
            }
            other => anyhow::bail!(
                "--merge-partials: unknown box {other:?} (expected `strih`, `stream`, or `imag`)"
            ),
        }
    }
    anyhow::ensure!(
        strih.is_some() || stream.is_some() || imag.is_some(),
        "--merge-partials needs at least one BOX=JSON partial"
    );
    // #186 note: cam1's pixel proof comes from the STRIH box (cam1's burn is crispest in the clean
    // 1080p strih recording, #133 — its slots are flagged there). A degenerate stream-ONLY merge
    // (no strih partial) therefore has NO cam1 pixel proof: cam1 falls back to the softened stream
    // source for the verdict, but the stream box's --extract-partial deliberately does NOT write
    // cam1 proofs (they would be at softened/reordered slots that don't match the merge's). WARN so
    // a stream-only run never SILENTLY lacks the #186 cam1 proof; the production two-box flow always
    // supplies both partials.
    if stream.is_some() && strih.is_none() {
        eprintln!(
            "WARNING: --merge-partials is stream-only (no strih partial) — cam1's #186 pixel proof \
             is UNAVAILABLE (cam1's clean source is the strih recording, #133). Supply \
             strih=<partial> for full cam1 pixel proof."
        );
    }
    tracing::info!(
        strih = strih.is_some(),
        stream = stream.is_some(),
        painter = ?args.painter.as_ref().map(|p| p.display().to_string()),
        "merge: building the full-chain verdict from per-box partials (#208 — no recording on dev1)"
    );
    // cam1's contiguity source is the strih partial frames (#133); there is no separate cam1
    // grab in the per-box flow (#179 removed it), so the cam1 grab is Absent.
    let (_report, all_pass) = build_and_print_verdict(
        args,
        strih,
        stream,
        Cam1Source::Absent,
        strih_colour,
        stream_colour,
        imag,
        stream_av_sync, // #312 item 2 (PR A): carried from the stream partial's --av-marker-log extract
    )?;
    report_pulled_back_pixel_proofs(&box_paths);
    if !all_pass {
        std::process::exit(1);
    }
    Ok(())
}

/// #186/#208: point the operator at the pixel proofs each box wrote during `--extract-partial`.
/// The merge cannot re-extract (the recording is not on dev1), but the box ALREADY wrote the
/// flagged/undecodable frames' PNGs into the `<partial>-pixels` dir, pulled back beside the partial
/// JSON. Locate that dir per box and report how many PNGs are there — so a FAIL's #186 "SEE the
/// missing frame" guarantee resolves to a real dev1 path, never a phantom one.
fn report_pulled_back_pixel_proofs(box_paths: &[(String, PathBuf)]) {
    if box_paths.is_empty() {
        return;
    }
    println!();
    println!(
        "=== #186/#208 pixel proofs (extracted ON each box during --extract-partial, pulled back to dev1) ==="
    );
    for (box_name, partial_path) in box_paths {
        let dir = partial_pixels_dir(partial_path);
        let pngs = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| {
                        e.path()
                            .extension()
                            .is_some_and(|x| x.eq_ignore_ascii_case("png"))
                    })
                    .count()
            })
            .ok();
        match pngs {
            Some(0) | None => println!(
                "  [{box_name}] {} — no pixel-proof PNGs on dev1 (a clean run writes none; if this \
                 run FAILED, pull the {box_name} box's <partial>-pixels dir — it was written there \
                 during --extract-partial)",
                dir.display()
            ),
            Some(n) => println!(
                "  [{box_name}] {n} pixel-proof PNG(s) in {} — open these to SEE each flagged / \
                 undecodable frame (#186)",
                dir.display()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        frame_is_delivered_optical,
        in_window_burn_frames,
        node_burn_id_on,
        node_verdict_with_optical,
        optical_span_facts,
        parse_cam1_capture_stats_str,
        parse_grab_ts,
        parse_painter_flip_str,
        parse_painter_ticks_str,
        // #706
        scope_camera_window_to_own_schedule,
    };

    /// #312 — locks the key design split: `CAMERA_UNDER_TEST_NODES` (digital contiguity) is
    /// broader than `OPTICAL_INJECTION_NODES` (cam2→camera optical-injection latency) by EXACTLY
    /// one member — cam2 — which participates in the former (its own chain is digitally
    /// measurable, #291) but not the latter (it cannot optically film its own monitor).
    #[test]
    fn optical_injection_nodes_excludes_cam2_camera_under_test_nodes_includes_it_312() {
        assert!(
            super::CAMERA_UNDER_TEST_NODES.contains(&"cam2"),
            "#312: cam2 must be a digital contiguity camera-under-test node"
        );
        assert!(
            !super::OPTICAL_INJECTION_NODES.contains(&"cam2"),
            "#312: cam2 must NOT be an optical-injection node — it is the painter itself, with \
             no second camera-vs-monitor optical hop to measure"
        );
        for cam in ["cam1", "cam3", "cam4", "cam5", "cam6"] {
            assert!(
                super::CAMERA_UNDER_TEST_NODES.contains(&cam)
                    && super::OPTICAL_INJECTION_NODES.contains(&cam),
                "#312: {cam} must be in BOTH sets — it is a real physical camera filming cam2's \
                 monitor AND has its own digitally-measurable chain"
            );
        }
        assert_eq!(
            super::CAMERA_UNDER_TEST_NODES.len(),
            super::OPTICAL_INJECTION_NODES.len() + 1,
            "#312: CAMERA_UNDER_TEST_NODES must be exactly OPTICAL_INJECTION_NODES plus cam2"
        );
    }
    use camera_box::probe::av_sync_recording::AvMarkerInputs;
    use camera_box::probe::burn_contiguity::{BurnRate, InWindowMissingKind};
    use camera_box::probe::payload::Payload;
    use camera_box::probe::recording::RecordingFrame;
    use std::collections::HashSet;
    use std::io::Write;

    /// #24 (BUG, regression-test-first): `--burn-cam3-run-id` defaulted to
    /// [`BURN_RUN_ID_IMAG`] (911003) — a latent run-id COLLISION. #463 renamed the constant that
    /// used to be cam3's own reserved id to `BURN_RUN_ID_IMAG` and repurposed it for imag-nb's
    /// digital corner burn, but left cam3's CLI default numerically pointing at the SAME value
    /// (harmless only because cam3's capture-burn was never actually deployed alongside imag's).
    /// The two mechanisms must be told apart by run_id alone — the cam3 default must never equal
    /// any OTHER reserved burn run_id, imag's included.
    #[test]
    fn burn_cam3_run_id_default_is_unique_among_reserved_burn_ids() {
        use clap::Parser;

        let args = super::Args::parse_from(["recording-verdict"]);
        let other_reserved = [
            super::BURN_RUN_ID_CAM1,
            super::BURN_RUN_ID_STRIH,
            super::BURN_RUN_ID_STREAM,
            super::BURN_RUN_ID_IMAG,
            super::BURN_RUN_ID_CAM4,
        ];
        assert!(
            !other_reserved.contains(&args.burn_cam3_run_id),
            "--burn-cam3-run-id defaults to {}, which collides with another reserved burn \
             run_id {other_reserved:?} (#24) — BURN_RUN_ID_IMAG=911003 is imag-nb's OWN digital \
             corner burn (#463); reserve cam3 a FRESH, unique run_id instead of reusing it",
            args.burn_cam3_run_id
        );
    }

    /// #312 — cam2/cam5/cam6's default capture-burn run_ids must ALSO be unique among every
    /// other reserved id (mirrors the #24 cam3 regression test above — the same class of latent
    /// collision bug is exactly what reserving a FRESH id per new camera-under-test guards
    /// against). All NINE reserved ids must be pairwise distinct.
    #[test]
    fn all_nine_reserved_burn_run_ids_are_pairwise_distinct_312() {
        use clap::Parser;
        use std::collections::HashSet;

        let args = super::Args::parse_from(["recording-verdict"]);
        let ids: [(&str, u32); 9] = [
            ("cam1", super::BURN_RUN_ID_CAM1),
            ("cam2", args.burn_cam2_run_id),
            ("cam3", super::BURN_RUN_ID_CAM3),
            ("cam4", super::BURN_RUN_ID_CAM4),
            ("cam5", args.burn_cam5_run_id),
            ("cam6", args.burn_cam6_run_id),
            ("strih", super::BURN_RUN_ID_STRIH),
            ("stream", super::BURN_RUN_ID_STREAM),
            ("imag", super::BURN_RUN_ID_IMAG),
        ];
        let unique: HashSet<u32> = ids.iter().map(|(_, id)| *id).collect();
        assert_eq!(
            unique.len(),
            ids.len(),
            "#312: every reserved burn run_id must be pairwise distinct — got {ids:?}, which \
             has a duplicate (a colliding id would make the verdict tell two nodes' payloads \
             apart incorrectly)"
        );
    }

    /// #11/#282: the stream node's tick DIAGNOSTIC must use the STREAM capture rate. Run
    /// 7020001: base capture_fps=60 leaked into the 30 fps stream recording's diagnostic —
    /// analyzed_secs halved (150.4 vs 300.7) and expected_step read 1 instead of 2.
    #[test]
    fn stream_diag_cfg_uses_stream_capture_fps() {
        let base = super::VerdictConfig {
            capture_fps: 60.0,
            min_secs: 300.0,
            refresh_hz: 60.0,
        };
        let cfg = super::stream_diag_cfg(&base, 30.0);
        assert_eq!(
            cfg.capture_fps, 30.0,
            "stream diag must run at the stream rate"
        );
        assert_eq!(cfg.min_secs, 300.0);
        assert_eq!(cfg.refresh_hz, 60.0);
        // A zero/unset stream rate must not produce a divide-by-zero cfg — keep the base.
        let cfg0 = super::stream_diag_cfg(&base, 0.0);
        assert_eq!(cfg0.capture_fps, 60.0);
    }

    /// Test helper — build a node verdict computing this node's optical facts from its OWN source
    /// (the headline path passes facts computed ONCE per source via [`node_verdict_with_optical`],
    /// #374 nit 1). Keeps the many existing call sites unchanged after the facts were hoisted.
    fn node_verdict(
        spec: &super::NodeSpec,
        all_burn_run_ids: &[u32],
        out_dir: &std::path::Path,
        max_pixel_proof: usize,
    ) -> anyhow::Result<super::NodeVerdict> {
        let optical = optical_span_facts(spec.source, all_burn_run_ids, spec.cam2_run_id);
        // #706: every existing fixture using this helper predates the switch-schedule scoping —
        // `None` reproduces their exact pre-#706 unscoped behavior (whole-recording optical span).
        node_verdict_with_optical(
            spec,
            all_burn_run_ids,
            optical,
            out_dir,
            max_pixel_proof,
            None,
        )
    }

    // ---- #198 in-window burn-contiguity wiring (the bug-level regression) ----

    const CAM2: u32 = 7; // optical cam2 run_id (not a burn)
    const CAM1B: u32 = 911001; // cam1 per-EMIT capture burn run_id
    const STRIH: u32 = 911002; // strih per-render burn run_id
    const STREAM: u32 = 911004; // stream per-render burn run_id
                                // #273: a CURRENT-run cam2 painter run_id (the `--cam2-run-id` pin) and a FOREIGN one — a
                                // previous run's residual paint still on the cam2 monitor when the recording started. Mirrors
                                // the real run 2706001 (current) vs 2606010 (the prior run's lead-in residue).
    const CAM2_PIN: u32 = 2706001;
    const CAM2_FOREIGN: u32 = 2606010;

    /// Build a recorded frame from (run_id, frame_id) payloads.
    fn frame(frame_index: u64, payloads: &[(u32, u32)]) -> RecordingFrame {
        let payloads: Vec<Payload> = payloads
            .iter()
            .map(|&(run_id, frame_id)| Payload {
                run_id,
                frame_id,
                gen_ts_ns: 1,
            })
            .collect();
        let tick = payloads.iter().map(|p| p.frame_id).max();
        RecordingFrame {
            frame_index,
            payloads,
            tick,
        }
    }

    // ---- #208 PER-BOX decode-in-place: merge of per-box partials reproduces the fused verdict ----

    /// Build a window of N delivered frames carrying the requested burns, contiguous end-to-end.
    /// `with_stream` adds the stream burn (only the stream recording carries it).
    fn window(n: u32, with_stream: bool, cam1_gap_at: Option<u32>) -> Vec<RecordingFrame> {
        (0..n)
            .map(|i| {
                let mut ps: Vec<(u32, u32)> = vec![(CAM2, 100 + i)];
                // cam1 per-emit id: contiguous, UNLESS a forward gap is injected (skip one id).
                let cam1_id = match cam1_gap_at {
                    Some(g) if i >= g => 5000 + i + 1, // from `g` on, ids jump by 1 → id (5000+g) missing
                    _ => 5000 + i,
                };
                ps.push((CAM1B, cam1_id));
                ps.push((STRIH, 1670 + 3 * i)); // per-render tick
                if with_stream {
                    ps.push((STREAM, 9000 + 3 * i));
                }
                frame(i as u64, &ps)
            })
            .collect()
    }

    /// #571 — like [`window`], but frame `cam1_none_at` carries NO cam1 burn payload at all (a
    /// `None`): the frame is still DELIVERED (its cam2 optical QR and the strih burn are present),
    /// its cam1 burn just did not decode. On the DECIMATED cam(60fps)→strih(30fps) hop this — not
    /// a forward id gap — is the genuine-loss signature the verdict must charge (BURN-UNREADABLE).
    /// The frame's `tick` is kept from the fully-burned frame: the real Vernier tick is computed
    /// from the cam2 OPTICAL payloads only (node burns are excluded, see [`RecordingFrame::tick`]),
    /// so losing the cam1 burn cannot change it.
    fn window_none(n: u32, with_stream: bool, cam1_none_at: u32) -> Vec<RecordingFrame> {
        window(n, with_stream, None)
            .into_iter()
            .map(|mut f| {
                if f.frame_index == u64::from(cam1_none_at) {
                    f.payloads.retain(|p| p.run_id != CAM1B);
                }
                f
            })
            .collect()
    }

    // ---- #24 — extend the #186 per-node digital-burn contiguity check to cam3/cam4 ----

    const CAM3B: u32 = super::BURN_RUN_ID_CAM3; // #24 cam3 per-EMIT capture burn run_id (911008)

    /// Build a window of N delivered frames like [`window`], but for CAM3 as the "camera under
    /// test" instead of cam1 (mirrors the #174 cam1 capture-burn mechanism running on cam3). In
    /// any real run only ONE of cam1/cam3/cam4 is ever burned (mutually exclusive), so this omits
    /// the cam1 burn entirely and stamps [`CAM3B`] instead.
    fn window_cam3(n: u32, with_stream: bool, cam3_gap_at: Option<u32>) -> Vec<RecordingFrame> {
        (0..n)
            .map(|i| {
                let mut ps: Vec<(u32, u32)> = vec![(CAM2, 100 + i)];
                let cam3_id = match cam3_gap_at {
                    Some(g) if i >= g => 6000 + i + 1, // from `g` on, ids jump by 1 → id (6000+g) missing
                    _ => 6000 + i,
                };
                ps.push((CAM3B, cam3_id));
                ps.push((STRIH, 1670 + 3 * i));
                if with_stream {
                    ps.push((STREAM, 9000 + 3 * i));
                }
                frame(i as u64, &ps)
            })
            .collect()
    }

    /// #571 — the cam3 mirror of [`window_none`]: frame `cam3_none_at` carries NO cam3 burn
    /// payload (a delivered frame whose cam3 burn did not decode — the decimated hop's genuine
    /// loss signature).
    fn window_cam3_none(n: u32, with_stream: bool, cam3_none_at: u32) -> Vec<RecordingFrame> {
        window_cam3(n, with_stream, None)
            .into_iter()
            .map(|mut f| {
                if f.frame_index == u64::from(cam3_none_at) {
                    f.payloads.retain(|p| p.run_id != CAM3B);
                }
                f
            })
            .collect()
    }

    // ---- #312 — extend the #186 per-node digital-burn contiguity check to cam2 (the fixed
    // dual-QR painter, made measurable by #291) ----------------------------------------------

    const CAM2B: u32 = super::BURN_RUN_ID_CAM2; // #312 cam2's OWN per-EMIT capture burn run_id (911009)

    /// Build a window of N delivered frames like [`window_cam3`], but for CAM2 as the "camera
    /// under test" — cam2's OWN camera-box daemon keeps capturing+emitting throughout a TEST run
    /// (#291), so its own chain is measurable by the SAME digital capture-burn mechanism as
    /// cam1/cam3/cam4/cam5/cam6.
    fn window_cam2(n: u32, with_stream: bool, cam2_gap_at: Option<u32>) -> Vec<RecordingFrame> {
        (0..n)
            .map(|i| {
                let mut ps: Vec<(u32, u32)> = vec![(CAM2, 100 + i)];
                let cam2b_id = match cam2_gap_at {
                    Some(g) if i >= g => 7000 + i + 1,
                    _ => 7000 + i,
                };
                ps.push((CAM2B, cam2b_id));
                ps.push((STRIH, 1670 + 3 * i));
                if with_stream {
                    ps.push((STREAM, 9000 + 3 * i));
                }
                frame(i as u64, &ps)
            })
            .collect()
    }

    /// #312 — extends the #186 per-node digital-burn contiguity check to CAM2. A contiguous cam2
    /// burn end-to-end ⇒ the fused verdict reports node "cam2" ZERO loss, exactly like
    /// cam1/cam3/cam4/cam5/cam6; cam1/cam3/cam4/cam5/cam6 themselves were never emitted this run
    /// and must NOT appear in the loss report at all (they are mutually-exclusive
    /// camera-under-test roles in a real single-camera run).
    #[test]
    fn cam2_digital_burn_extends_the_186_contiguity_check_312() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;

        // --min-secs 1 so the small contiguous window trivially clears the #373 span floor.
        let args = super::Args::parse_from(["recording-verdict", "--min-secs", "1"]);
        let strih_frames = window_cam2(60, false, None);
        let stream_frames = window_cam2(60, true, None);

        let (v, pass) = build_and_print_verdict(
            &args,
            Some(DecodedRec {
                frames: strih_frames,
                rec_path: None,
            }),
            Some(DecodedRec {
                frames: stream_frames,
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            None, // #461: no imag frames in this test
            None, // #312 item 2 (PR A): no carried A/V-sync inputs in this test
        )
        .expect("verdict");

        assert!(pass, "#312: contiguous cam2 burn ⇒ overall PASS: {v}");
        let loss = &v["full_chain"]["loss"];
        assert_eq!(
            loss["cam2"]["zero_loss"],
            serde_json::json!(true),
            "#312: cam2 must be verdicted ZERO loss when its OWN burn is contiguous: {loss}"
        );
        for absent in ["cam1", "cam3", "cam4", "cam5", "cam6"] {
            assert!(
                loss.get(absent).is_none(),
                "#312: {absent} never emitted this run ⇒ must NOT appear in the loss report: {loss}"
            );
        }
        assert_eq!(
            v["full_chain"]["burn_ids_present"]["cam2"],
            serde_json::json!(60),
            "#312: all 60 cam2 burn ids decoded: {}",
            v["full_chain"]["burn_ids_present"]
        );
    }

    /// #312 — a cam2 burn GAP (a delivered frame missing its burn payload, mirroring
    /// [`window_cam3_none`]) must still FAIL the headline — cam2's contiguity check is a real
    /// gate, not a rubber stamp.
    #[test]
    fn cam2_delivered_frame_missing_burn_is_a_real_gate_312() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;

        let args = super::Args::parse_from(["recording-verdict", "--min-secs", "1"]);
        // The gap sits on the STRIH recording (cam1_source — where cam2's contiguity is read
        // from, #133/#571 pattern); the stream recording carries the FULL burn sequence.
        let strih_frames = window_cam2_none(60, false, 30);
        let stream_frames = window_cam2(60, true, None);

        let (v, pass) = build_and_print_verdict(
            &args,
            Some(DecodedRec {
                frames: strih_frames,
                rec_path: None,
            }),
            Some(DecodedRec {
                frames: stream_frames,
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            None,
            None, // #312 item 2 (PR A): no carried A/V-sync inputs in this test
        )
        .expect("verdict");

        assert!(
            !pass,
            "#312: a genuine cam2 burn gap must FAIL the headline, not pass: {v}"
        );
    }

    /// #571-style mirror of [`window_cam3_none`] for cam2: frame `cam2_none_at` carries NO cam2
    /// burn payload (a delivered frame whose cam2 burn did not decode).
    fn window_cam2_none(n: u32, with_stream: bool, cam2_none_at: u32) -> Vec<RecordingFrame> {
        window_cam2(n, with_stream, None)
            .into_iter()
            .map(|mut f| {
                if f.frame_index == u64::from(cam2_none_at) {
                    f.payloads.retain(|p| p.run_id != CAM2B);
                }
                f
            })
            .collect()
    }

    // ---- #312 — extend the #186 per-node digital-burn contiguity check to cam5/cam6 (fleet
    // growth 4→6, #451) ----------------------------------------------------------------------

    const CAM5B: u32 = super::BURN_RUN_ID_CAM5; // #312 cam5's OWN per-EMIT capture burn run_id (911010)
    const CAM6B: u32 = super::BURN_RUN_ID_CAM6; // #312 cam6's OWN per-EMIT capture burn run_id (911011)

    /// Build a window of N delivered frames carrying BOTH cam5's and cam6's digital burns in
    /// every frame (a synthetic-only shortcut — in any REAL run only one of cam1/cam2/cam3/cam4/
    /// cam5/cam6 is ever burned in a given schedule window, they are mutually exclusive; stamping
    /// both here just lets ONE window prove BOTH new `NodeSpec` entries independently instead of
    /// building two separate 60-frame windows).
    fn window_cam5_and_cam6(n: u32, with_stream: bool) -> Vec<RecordingFrame> {
        (0..n)
            .map(|i| {
                let mut ps: Vec<(u32, u32)> =
                    vec![(CAM2, 100 + i), (CAM5B, 8000 + i), (CAM6B, 9000 + i)];
                ps.push((STRIH, 1670 + 3 * i));
                if with_stream {
                    ps.push((STREAM, 9500 + 3 * i));
                }
                frame(i as u64, &ps)
            })
            .collect()
    }

    /// #312 — extends the #186 per-node digital-burn contiguity check to CAM5 and CAM6 (fleet
    /// growth 4→6, #451). A contiguous burn end-to-end for both ⇒ the fused verdict reports BOTH
    /// node "cam5" and node "cam6" ZERO loss — locks that reverting either `NodeSpec` tuple (or
    /// mixing up which `--burn-camN-run-id` feeds which node) would be caught here, not just by
    /// the structural "ids are pairwise distinct" test.
    #[test]
    fn cam5_and_cam6_digital_burns_extend_the_186_contiguity_check_312() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;

        let args = super::Args::parse_from(["recording-verdict", "--min-secs", "1"]);
        let strih_frames = window_cam5_and_cam6(60, false);
        let stream_frames = window_cam5_and_cam6(60, true);

        let (v, pass) = build_and_print_verdict(
            &args,
            Some(DecodedRec {
                frames: strih_frames,
                rec_path: None,
            }),
            Some(DecodedRec {
                frames: stream_frames,
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            None, // #461: no imag frames in this test
            None, // #312 item 2 (PR A): no carried A/V-sync inputs in this test
        )
        .expect("verdict");

        assert!(pass, "#312: contiguous cam5+cam6 burns ⇒ overall PASS: {v}");
        let loss = &v["full_chain"]["loss"];
        for node in ["cam5", "cam6"] {
            assert_eq!(
                loss[node]["zero_loss"],
                serde_json::json!(true),
                "#312: {node} must be verdicted ZERO loss when its OWN burn is contiguous: {loss}"
            );
        }
        for absent in ["cam1", "cam2", "cam3", "cam4"] {
            assert!(
                loss.get(absent).is_none(),
                "#312: {absent} never emitted this run ⇒ must NOT appear in the loss report: {loss}"
            );
        }
        assert_eq!(
            v["full_chain"]["burn_ids_present"]["cam5"],
            serde_json::json!(60),
            "#312: all 60 cam5 burn ids decoded: {}",
            v["full_chain"]["burn_ids_present"]
        );
        assert_eq!(
            v["full_chain"]["burn_ids_present"]["cam6"],
            serde_json::json!(60),
            "#312: all 60 cam6 burn ids decoded: {}",
            v["full_chain"]["burn_ids_present"]
        );
    }

    /// #24 — extends the #186 per-node digital-burn contiguity check (previously cam1-only) to
    /// CAM3. A contiguous cam3 burn end-to-end ⇒ the fused verdict reports node "cam3" ZERO loss,
    /// exactly like cam1 today; cam1 itself was never emitted this run and must NOT appear in the
    /// loss report at all (cam1/cam3/cam4 are mutually-exclusive camera-under-test roles).
    #[test]
    fn cam3_digital_burn_extends_the_186_contiguity_check_24() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;

        // --min-secs 1 so the small contiguous window trivially clears the #373 span floor.
        let args = super::Args::parse_from(["recording-verdict", "--min-secs", "1"]);
        let strih_frames = window_cam3(60, false, None);
        let stream_frames = window_cam3(60, true, None);

        let (v, pass) = build_and_print_verdict(
            &args,
            Some(DecodedRec {
                frames: strih_frames,
                rec_path: None,
            }),
            Some(DecodedRec {
                frames: stream_frames,
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            None, // #461: no imag frames in this test
            None, // #312 item 2 (PR A): no carried A/V-sync inputs in this test
        )
        .expect("verdict");

        assert!(pass, "#24: contiguous cam3 burn ⇒ overall PASS: {v}");
        let loss = &v["full_chain"]["loss"];
        assert_eq!(
            loss["cam3"]["zero_loss"],
            serde_json::json!(true),
            "#24: cam3 must be verdicted ZERO loss when its burn is contiguous: {loss}"
        );
        assert!(
            loss.get("cam1").is_none(),
            "#24: cam1 never emitted this run ⇒ must NOT appear in the loss report: {loss}"
        );
        assert!(
            loss.get("cam4").is_none(),
            "#24: cam4 never emitted this run ⇒ must NOT appear in the loss report: {loss}"
        );
        assert_eq!(
            v["full_chain"]["burn_ids_present"]["cam3"],
            serde_json::json!(60),
            "#24: all 60 cam3 burn ids decoded: {}",
            v["full_chain"]["burn_ids_present"]
        );
    }

    /// #24 → #571 REWORK to the DECIMATED-hop model. With the rig-default rates the cam→strih hop
    /// is decimated (cam3 emits at `--refresh-hz` 60, strih records its own canvas at
    /// `--capture-fps` 30 ⇒ `node_render_step` = 2), so a FORWARD GAP in cam3's digital burn is
    /// by-design decimation, NOT loss (run 554307: 11087 phantom cam1 "drops" while strih's OWN
    /// 911002 burn was fully contiguous). The pre-Topology-v2 1:1 assertion this test used to
    /// encode (forward gap ⇒ REAL DROP) is superseded ON THIS HOP; genuine loss here is a
    /// DELIVERED strih frame carrying NO readable cam3 burn (a None ⇒ BURN-UNREADABLE), plus
    /// strih's own 911002 burn and the optical tick. Both halves locked: (a) the forward gap
    /// alone is ZERO loss; (b) the None still FAILS — nothing weakened, the invalid forward-gap
    /// signal is replaced by the valid ones. (The 1:1 hop's forward-gap REAL DROP is preserved —
    /// see `cam1_decimated_forward_gap_is_zero_loss_but_1to1_gap_stays_real_drop_356` and the
    /// `_571` step-1 tests in burn_contiguity.rs.)
    #[test]
    fn cam3_decimated_forward_gap_is_zero_loss_and_a_none_still_fails_24() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;

        let args = super::Args::parse_from(["recording-verdict", "--min-secs", "1"]);

        // (a) A forward id gap on the decimated hop is by-design decimation ⇒ ZERO loss, PASS.
        let (v, pass) = build_and_print_verdict(
            &args,
            Some(DecodedRec {
                frames: window_cam3(60, false, Some(30)),
                rec_path: None,
            }),
            Some(DecodedRec {
                frames: window_cam3(60, true, Some(30)),
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            None, // #461: no imag frames in this test
            None, // #312 item 2 (PR A): no carried A/V-sync inputs in this test
        )
        .expect("verdict");
        assert!(
            pass,
            "#571: a cam3 forward gap on the decimated hop is decimation, not loss: {v}"
        );
        assert_eq!(
            v["full_chain"]["loss"]["cam3"]["zero_loss"],
            serde_json::json!(true),
            "#571: cam3 must be ZERO loss on a pure forward gap: {}",
            v["full_chain"]["loss"]["cam3"]
        );
        assert_eq!(
            v["full_chain"]["loss"]["cam3"]["real_drops"],
            serde_json::json!(0),
            "#571: no phantom real drop from decimation: {}",
            v["full_chain"]["loss"]["cam3"]
        );

        // (b) GENUINE loss on the decimated hop: a DELIVERED strih frame with NO cam3 burn (a
        // None) is charged BURN-UNREADABLE and FAILS the headline — the gate is NOT weakened.
        let (v2, pass2) = build_and_print_verdict(
            &args,
            Some(DecodedRec {
                frames: window_cam3_none(60, false, 30),
                rec_path: None,
            }),
            Some(DecodedRec {
                frames: window_cam3(60, true, None),
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            None, // #461: no imag frames in this test
            None, // #312 item 2 (PR A): no carried A/V-sync inputs in this test
        )
        .expect("verdict");
        assert!(
            !pass2,
            "#571: a delivered frame missing its cam3 burn must still FAIL: {v2}"
        );
        assert_eq!(
            v2["full_chain"]["loss"]["cam3"]["zero_loss"],
            serde_json::json!(false),
            "#571: {}",
            v2["full_chain"]["loss"]["cam3"]
        );
        assert_eq!(
            v2["full_chain"]["loss"]["cam3"]["burn_unreadable"],
            serde_json::json!(1),
            "#571: the None is exactly one BURN-UNREADABLE: {}",
            v2["full_chain"]["loss"]["cam3"]
        );
        assert_eq!(
            v2["full_chain"]["loss"]["cam3"]["real_drops"],
            serde_json::json!(0),
            "#571: no phantom real drop alongside the None: {}",
            v2["full_chain"]["loss"]["cam3"]
        );
    }

    /// #24 — the #356 cross-recording reconciliation (previously cam1-only) generalizes to cam3:
    /// a cam3 id classified REAL DROP from the (clean, upstream) strih recording but PROVEN
    /// delivered in the downstream stream recording is re-classified BURN-UNREADABLE, not REAL
    /// DROP — exactly as cam1 already does. Locks that generalizing the reconciliation condition
    /// did not silently drop this behaviour for a non-cam1 camera-under-test node.
    ///
    /// #571 REWORK: this reconciliation has forward-gap REAL-DROP candidates to work on ONLY on a
    /// 1:1 (non-decimated) hop — on the rig-default DECIMATED hop (`--capture-fps` 30 ⇒ step 2) a
    /// forward gap is by-design decimation and is never a candidate at all (see
    /// `cam3_decimated_forward_gap_is_zero_loss_and_a_none_still_fails_24`). So this test pins the
    /// hop 1:1 explicitly (`--capture-fps 60` ⇒ `node_render_step` = painted_tick_step(60,60) = 1
    /// — the pre-Topology-v2 shape, or any future non-decimated capture), where the strict
    /// forward-gap scan and the reconciliation behave exactly as pre-#571.
    ///
    /// Mirrors `cam1_real_drop_present_downstream_is_burn_unreadable_not_real_drop_356` exactly:
    /// the reconciliation only ever MOVES an id between loss BUCKETS (REAL DROP →
    /// BURN-UNREADABLE) for honest accounting — it never touches `missing_ids`, so the id is
    /// STILL missing from the burn-id sequence and `NodeVerdict::is_zero()` (see its doc comment:
    /// "a BURN-UNREADABLE missing id is a real DEFECT and still makes the node NOT-zero, never
    /// silently excluded") correctly keeps the node — and therefore the overall headline — NOT
    /// zero / FAIL. Asserting an overall PASS here would contradict that invariant and would mask
    /// a real (if reclassified) defect. This test's earlier `assert!(pass, ...)` was simply wrong
    /// about what the reconciliation guarantees; the reclassification itself (real_drops=0,
    /// burn_unreadable=1) was already correct and unchanged.
    #[test]
    fn cam3_real_drop_present_downstream_is_burn_unreadable_not_real_drop_24() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;

        // #571: --capture-fps 60 pins the cam→strih hop 1:1 (see the doc comment).
        let args = super::Args::parse_from([
            "recording-verdict",
            "--min-secs",
            "1",
            "--capture-fps",
            "60",
        ]);
        const N: u32 = 600;
        // strih (cam3's source, #133): cam3 id 6005 MISSING (unreadable through the deep buffer).
        let strih = window_cam3(N, false, Some(5));
        // stream (downstream): cam3 CONTIGUOUS — 6005 IS present ⇒ that frame WAS delivered.
        let stream = window_cam3(N, true, None);

        let (v, pass) = build_and_print_verdict(
            &args,
            Some(DecodedRec {
                frames: strih,
                rec_path: None,
            }),
            Some(DecodedRec {
                frames: stream,
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            None, // #461: no imag frames in this test
            None, // #312 item 2 (PR A): no carried A/V-sync inputs in this test
        )
        .expect("verdict");

        // The node still FAILs (6005 IS still missing from the strih recording — a real
        // burn-readability defect to fix), but it is charged BURN-UNREADABLE, not REAL DROP: no
        // false ZERO, honest bucket. Exactly mirrors the cam1 #356 test's `!pass` expectation.
        assert!(
            !pass,
            "#24: a still-missing cam3 id keeps the run NOT zero (never a false ZERO): {v}"
        );
        assert_eq!(
            v["full_chain"]["loss"]["cam3"]["zero_loss"],
            serde_json::json!(false),
            "#24: {}",
            v["full_chain"]["loss"]["cam3"]
        );
        assert_eq!(
            v["full_chain"]["loss"]["cam3"]["real_drops"],
            serde_json::json!(0),
            "#24: proven-delivered downstream must NOT be counted a REAL DROP: {}",
            v["full_chain"]["loss"]["cam3"]
        );
        assert_eq!(
            v["full_chain"]["loss"]["cam3"]["burn_unreadable"],
            serde_json::json!(1),
            "#24: the delivered-but-strih-unreadable cam3 id must be charged BURN-UNREADABLE: {}",
            v["full_chain"]["loss"]["cam3"]
        );
    }

    /// #377 — the carried per-recording COLOUR summary flows through the merge path to the RIGHT
    /// nodes and FAILS the headline on a colour fault even when delivery + optical are clean. The
    /// node→recording mapping mirrors the fused path: the strih recording's colour → cam1 (#133),
    /// the stream recording's colour → strih + stream. Proves the #377 carry-through end to end
    /// (the colour gate is fused/on-host, so the merge MUST rely on the carried summary).
    #[test]
    fn merge_carried_colour_maps_to_nodes_and_fails_the_headline_377() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use camera_box::colour_verify::NodeColourSummary;
        use clap::Parser;

        // --min-secs 1 so a small contiguous window trivially clears the #373 span floor, isolating
        // COLOUR as the only possible failure (delivery + optical are clean).
        let args = super::Args::parse_from(["recording-verdict", "--min-secs", "1"]);
        // strih recording: cam1 + strih burns; stream recording: all three. Contiguous ⇒ clean delivery.
        let strih_frames = window(60, false, None);
        let stream_frames = window(60, true, None);

        let clean = NodeColourSummary {
            patch_wrong_counts: vec![0; 13],
            patch_checked_counts: vec![6; 13],
            frames_sampled: 6,
        };
        // 3 chromatic patches wrong on every sampled frame ⇒ fail_count() == 3.
        let failing = NodeColourSummary {
            patch_wrong_counts: vec![6, 0, 6, 0, 6, 0, 0, 0, 0, 0, 0, 0, 0],
            patch_checked_counts: vec![6; 13],
            frames_sampled: 6,
        };
        assert_eq!(
            failing.fail_count(),
            3,
            "test fixture: 3 patches wrong on a majority"
        );

        let build = |strih_colour, stream_colour| {
            build_and_print_verdict(
                &args,
                Some(DecodedRec {
                    frames: strih_frames.clone(),
                    rec_path: None,
                }),
                Some(DecodedRec {
                    frames: stream_frames.clone(),
                    rec_path: None,
                }),
                Cam1Source::Absent,
                strih_colour,
                stream_colour,
                None, // #461: no imag frames in this test
                None, // #312 item 2 (PR A): no carried A/V-sync inputs in this test
            )
            .expect("verdict")
        };

        // FAILING stream recording colour → strih + stream fail; clean strih recording → cam1 OK.
        let (v, pass) = build(Some(clean.clone()), Some(failing.clone()));
        let loss = &v["full_chain"]["loss"];
        assert_eq!(
            loss["cam1"]["colour_fail"],
            serde_json::json!(0),
            "cam1 ← clean strih recording"
        );
        assert_eq!(
            loss["strih"]["colour_fail"],
            serde_json::json!(3),
            "strih ← failing stream recording"
        );
        assert_eq!(
            loss["stream"]["colour_fail"],
            serde_json::json!(3),
            "stream ← failing stream recording"
        );
        assert!(
            !pass,
            "a colour failure FAILS the headline even with clean delivery + optical"
        );

        // Mirror: FAILING strih recording colour → cam1 fails; clean stream → strih + stream OK.
        let (v2, pass2) = build(Some(failing), Some(clean));
        let loss2 = &v2["full_chain"]["loss"];
        assert_eq!(
            loss2["cam1"]["colour_fail"],
            serde_json::json!(3),
            "cam1 ← failing strih recording"
        );
        assert_eq!(
            loss2["strih"]["colour_fail"],
            serde_json::json!(0),
            "strih ← clean stream recording"
        );
        assert!(!pass2, "a cam1 colour failure FAILS the headline");

        // No carried colour (delivery-only merge) ⇒ colour ungated ⇒ clean delivery PASSES.
        let (v3, pass3) = build(None, None);
        assert_eq!(
            v3["full_chain"]["loss"]["stream"]["colour_fail"],
            serde_json::json!(0)
        );
        assert!(
            pass3,
            "no carried colour ⇒ colour ungated ⇒ a clean delivery still passes"
        );
    }

    /// #208: merging the per-box partials (strih partial + stream partial) reproduces the SAME
    /// full-chain verdict the fused single-process path produces — same JSON, same PASS — for a
    /// clean ZERO-loss run, a run with a cam1 forward gap (#571: by-design decimation on the
    /// rig-default decimated hop ⇒ still PASS), AND a run with a genuine cam1 loss (a delivered
    /// frame missing its burn ⇒ FAIL). This is the equivalence the per-box decode-in-place flow
    /// rests on: no recording is copied box-to-box, yet the verdict is identical.
    #[test]
    fn merge_of_partials_reproduces_the_fused_verdict() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use camera_box::probe::recording_partial::RecordingPartial;
        use clap::Parser;
        use std::path::PathBuf;

        // Default args: burn ids default to 911001/911002/911004; cam2_run_id 0; min-secs 300.
        let args = super::Args::parse_from(["recording-verdict"]);

        // Helper: build BOTH ways (fused = frames decoded here; merge = frames round-tripped
        // through the per-box partial JSON) and assert the verdict JSON + PASS are identical.
        let run_both = |strih_frames: Vec<RecordingFrame>, stream_frames: Vec<RecordingFrame>| {
            let (fused, fused_pass) = build_and_print_verdict(
                &args,
                Some(DecodedRec {
                    frames: strih_frames.clone(),
                    rec_path: None,
                }),
                Some(DecodedRec {
                    frames: stream_frames.clone(),
                    rec_path: None,
                }),
                Cam1Source::Absent,
                None,
                None,
                None, // #461: no imag frames in this test
                None, // #312 item 2 (PR A): no carried A/V-sync inputs in this test
            )
            .expect("fused verdict");

            // Round-trip both recordings through the small per-box partial JSON, then merge.
            let strih_p = RecordingPartial::from_frames(
                "strih",
                &PathBuf::from("strih.mkv"),
                &[CAM1B, STRIH],
                strih_frames,
            );
            let stream_p = RecordingPartial::from_frames(
                "stream",
                &PathBuf::from("stream.mp4"),
                &[CAM1B, STRIH, STREAM],
                stream_frames,
            );
            let strih_back = RecordingPartial::from_json(&strih_p.to_json().unwrap()).unwrap();
            let stream_back = RecordingPartial::from_json(&stream_p.to_json().unwrap()).unwrap();
            let (merged, merged_pass) = build_and_print_verdict(
                &args,
                Some(DecodedRec {
                    frames: strih_back.frames,
                    rec_path: None,
                }),
                Some(DecodedRec {
                    frames: stream_back.frames,
                    rec_path: None,
                }),
                Cam1Source::Absent,
                None,
                None,
                None, // #461: no imag frames in this test
                None, // #312 item 2 (PR A): no carried A/V-sync inputs in this test
            )
            .expect("merged verdict");

            assert_eq!(
                merged, fused,
                "#208: the merged verdict JSON must reproduce the fused output exactly"
            );
            assert_eq!(
                merged_pass, fused_pass,
                "#208: merge PASS must equal fused PASS"
            );
            (fused, fused_pass)
        };

        // CLEAN run: contiguous burns end-to-end ⇒ ZERO loss PASS. The window must be a REAL
        // full-length span (9000 recorded frames @ the default 30 fps capture = 300 s) so it clears
        // the #373 headline DURATION floor — a 12-frame synthetic window has a ~0 s analyzed span
        // and (correctly) cannot declare zero loss. This exercises merge==fused over a realistic
        // zero-loss run, keeping the #373 gate strict (its own RED/GREEN tests lock the floor).
        const FULL_SPAN_FRAMES: u32 = 9000; // 300 s @ 30 fps default capture (#373 floor)
        let (clean, clean_pass) = run_both(
            window(FULL_SPAN_FRAMES, false, None),
            window(FULL_SPAN_FRAMES, true, None),
        );
        assert!(clean_pass, "#208: contiguous burns ⇒ overall PASS");
        assert_eq!(clean["overall_pass"], serde_json::json!(true));
        assert_eq!(clean["full_chain"]["zero_loss"], serde_json::json!(true));
        assert_eq!(clean["full_chain"]["real_drops"], serde_json::json!(0));
        assert_eq!(clean["full_chain"]["burn_unreadable"], serde_json::json!(0));
        // The per-node diagnostics + full-chain burn presence must reflect BOTH recordings'
        // frames — proving the strih diag came from the strih partial and stream from the stream
        // partial (the per-box decode), not one fused recording.
        assert_eq!(clean["nodes"]["strih"]["undecodable"], serde_json::json!(0));
        assert_eq!(
            clean["nodes"]["strih"]["diagnostic_only"],
            serde_json::json!(true)
        );
        assert_eq!(
            clean["nodes"]["stream"]["undecodable"],
            serde_json::json!(0)
        );
        assert_eq!(
            clean["full_chain"]["burn_ids_present"]["cam1"],
            serde_json::json!(FULL_SPAN_FRAMES),
            "cam1 burn ids come from the STRIH partial (#133): {}",
            clean["full_chain"]["burn_ids_present"]
        );
        assert_eq!(
            clean["full_chain"]["burn_ids_present"]["stream"],
            serde_json::json!(FULL_SPAN_FRAMES)
        );
        assert!(
            clean["full_chain"]["cam1_source"]
                .as_str()
                .unwrap_or("")
                .contains("strih"),
            "cam1's contiguity source must be the strih recording (#133): {}",
            clean["full_chain"]["cam1_source"]
        );

        // cam1 FORWARD GAP (#571): cam1's contiguity source is the STRIH recording (#133), and
        // with the rig-default rates the cam→strih hop is DECIMATED (refresh 60 / capture 30 ⇒
        // step 2) — a forward id gap is by-design decimation, NOT loss (run 554307: 11087
        // phantom drops while strih's OWN 911002 burn was fully contiguous). The gap run must
        // therefore PASS — and the merge must agree. (The 1:1 hop's forward-gap REAL DROP is
        // locked by `cam1_decimated_forward_gap_is_zero_loss_but_1to1_gap_stays_real_drop_356`.)
        let (gap, gap_pass) = run_both(
            window(FULL_SPAN_FRAMES, false, Some(5)),
            window(FULL_SPAN_FRAMES, true, Some(5)),
        );
        assert!(
            gap_pass,
            "#571: a cam1 forward gap on the decimated hop is decimation ⇒ overall PASS"
        );
        assert_eq!(gap["full_chain"]["zero_loss"], serde_json::json!(true));
        assert_eq!(gap["full_chain"]["real_drops"], serde_json::json!(0));

        // GENUINE cam1 loss on the decimated hop: a DELIVERED strih frame carrying NO cam1 burn
        // (a None ⇒ BURN-UNREADABLE) ⇒ NOT zero ⇒ FAIL. Merge agrees. Same full-length span so
        // the FAIL is the missing burn, not the #373 duration floor.
        let (none_v, none_pass) = run_both(
            window_none(FULL_SPAN_FRAMES, false, 5),
            window(FULL_SPAN_FRAMES, true, None),
        );
        assert!(
            !none_pass,
            "#208/#571: a delivered frame missing its cam1 burn ⇒ overall FAIL"
        );
        assert_eq!(none_v["full_chain"]["zero_loss"], serde_json::json!(false));
        assert_eq!(
            none_v["full_chain"]["real_drops"],
            serde_json::json!(0),
            "#571: the None is BURN-UNREADABLE, never a phantom REAL DROP: {}",
            none_v["full_chain"]
        );
        assert!(
            none_v["full_chain"]["burn_unreadable"].as_u64().unwrap() >= 1,
            "#208/#571: the missing cam1 burn must be charged BURN-UNREADABLE: {}",
            none_v["full_chain"]
        );
    }

    /// #356 — cross-recording reconciliation. A cam1 id that is a REAL DROP in the (clean, upstream)
    /// strih recording but IS decoded in the DOWNSTREAM stream recording was delivered (the frame
    /// reached the stream) — the small cam1 burn was merely UNREADABLE in the strih recording at the
    /// high-latency 60→30 hop. It must be classified BURN-UNREADABLE, NOT REAL DROP, so the merge
    /// headline stops over-counting (the #356 residual cam1 over-count). Runs through the SHARED
    /// `build_and_print_verdict` (fused == merge), so the merge production flow gets it identically.
    ///
    /// #571 REWORK: forward-gap REAL-DROP candidates (the reconciliation's input) exist only on a
    /// 1:1 hop — on the rig-default DECIMATED hop a forward gap is by-design decimation and never
    /// a candidate (see `cam1_decimated_forward_gap_is_zero_loss_but_1to1_gap_stays_real_drop_356`
    /// and `cam1_delivered_frame_missing_burn_is_burn_unreadable_not_real_drop_356`). So this test
    /// pins the hop 1:1 explicitly (`--capture-fps 60` ⇒ `node_render_step` =
    /// painted_tick_step(60,60) = 1), where the strict scan + reconciliation are exactly pre-#571.
    #[test]
    fn cam1_real_drop_present_downstream_is_burn_unreadable_not_real_drop_356() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;
        // --min-secs 1 so the small contiguous window trivially clears the #373 span floor.
        // #571: --capture-fps 60 pins the cam→strih hop 1:1 (see the doc comment).
        let args = super::Args::parse_from([
            "recording-verdict",
            "--min-secs",
            "1",
            "--capture-fps",
            "60",
        ]);
        const N: u32 = 600;
        // strih (cam1's source, #133): cam1 id 5005 MISSING (unreadable through the deep buffer).
        let strih = window(N, false, Some(5));
        // stream (downstream): cam1 CONTIGUOUS — 5005 IS present ⇒ that frame WAS delivered.
        let stream = window(N, true, None);
        let (v, pass) = build_and_print_verdict(
            &args,
            Some(DecodedRec {
                frames: strih,
                rec_path: None,
            }),
            Some(DecodedRec {
                frames: stream,
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            None, // #461: no imag frames in this test
            None, // #312 item 2 (PR A): no carried A/V-sync inputs in this test
        )
        .expect("verdict");
        // The node still FAILs (the id IS missing from the strih recording — a real burn-readability
        // defect to fix), but it is charged BURN-UNREADABLE, not REAL DROP: no false ZERO, honest bucket.
        assert!(
            !pass,
            "#356: a still-missing cam1 id keeps the run NOT zero (never a false ZERO)"
        );
        assert_eq!(v["full_chain"]["zero_loss"], serde_json::json!(false));
        assert_eq!(
            v["full_chain"]["real_drops"],
            serde_json::json!(0),
            "#356: a cam1 REAL DROP present in the downstream stream recording must NOT be counted a \
             REAL DROP (delivered downstream): {}",
            v["full_chain"]
        );
        assert!(
            v["full_chain"]["burn_unreadable"].as_u64().unwrap() >= 1,
            "#356: the delivered-but-strih-unreadable cam1 id must be charged BURN-UNREADABLE: {}",
            v["full_chain"]
        );
    }

    /// #356 SAFETY → #571 REWORK. The old body asserted "a cam1 id absent from BOTH recordings is
    /// a REAL DROP" with the rig-default rates — but those rates make the cam→strih hop DECIMATED
    /// (refresh 60 / capture 30 ⇒ step 2), where a forward id gap is BY-DESIGN decimation, not
    /// loss (run 554307: 11087 phantom drops while strih's OWN 911002 burn was fully contiguous;
    /// the pixels proved nothing was lost). The pre-Topology-v2 1:1 model is superseded ON THE
    /// DECIMATED HOP, so the never-mask invariant now has two honest halves:
    ///   (a) DECIMATED hop: the forward gap is ZERO loss — no phantom drop;
    ///   (b) 1:1 hop (`--capture-fps 60` ⇒ step 1): the SAME gap absent from both recordings
    ///       STAYS a REAL DROP — the original #356 SAFETY assertion, byte-identical behavior.
    /// Genuine loss ON the decimated hop is a delivered frame with NO readable burn — locked by
    /// `cam1_delivered_frame_missing_burn_is_burn_unreadable_not_real_drop_356` and the #208
    /// merge test's None case.
    #[test]
    fn cam1_decimated_forward_gap_is_zero_loss_but_1to1_gap_stays_real_drop_356() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;
        const N: u32 = 600;

        // (a) DECIMATED hop (rig defaults): the forward gap (5005 absent from both recordings)
        // is by-design decimation ⇒ ZERO loss, PASS.
        let args = super::Args::parse_from(["recording-verdict", "--min-secs", "1"]);
        let (v, pass) = build_and_print_verdict(
            &args,
            Some(DecodedRec {
                frames: window(N, false, Some(5)),
                rec_path: None,
            }),
            Some(DecodedRec {
                frames: window(N, true, Some(5)),
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            None, // #461: no imag frames in this test
            None, // #312 item 2 (PR A): no carried A/V-sync inputs in this test
        )
        .expect("verdict");
        assert!(
            pass,
            "#571: a cam1 forward gap on the decimated hop is decimation ⇒ PASS: {v}"
        );
        assert_eq!(v["full_chain"]["zero_loss"], serde_json::json!(true));
        assert_eq!(
            v["full_chain"]["real_drops"],
            serde_json::json!(0),
            "#571: no phantom real drop from decimation: {}",
            v["full_chain"]
        );

        // (b) 1:1 hop (--capture-fps 60 ⇒ step 1): the SAME gap stays a REAL DROP — the #356
        // SAFETY never-mask invariant, unchanged where the forward-gap signal is valid.
        let args_1to1 = super::Args::parse_from([
            "recording-verdict",
            "--min-secs",
            "1",
            "--capture-fps",
            "60",
        ]);
        let (v2, pass2) = build_and_print_verdict(
            &args_1to1,
            Some(DecodedRec {
                frames: window(N, false, Some(5)),
                rec_path: None,
            }),
            Some(DecodedRec {
                frames: window(N, true, Some(5)),
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            None, // #461: no imag frames in this test
            None, // #312 item 2 (PR A): no carried A/V-sync inputs in this test
        )
        .expect("verdict");
        assert!(!pass2, "#356: a genuine 1:1-hop cam1 loss ⇒ overall FAIL");
        assert_eq!(v2["full_chain"]["zero_loss"], serde_json::json!(false));
        assert!(
            v2["full_chain"]["real_drops"].as_u64().unwrap() >= 1,
            "#356 SAFETY: on the 1:1 hop a cam1 id absent from BOTH recordings MUST stay REAL \
             DROP — never masked: {}",
            v2["full_chain"]
        );
    }

    /// #356/#571 — GENUINE loss on the DECIMATED hop is a DELIVERED strih frame carrying NO
    /// readable cam1 burn (a None): charged BURN-UNREADABLE (real_drops 0), and the headline
    /// still FAILS — replacing the forward-gap signal (invalid on this hop) with the valid one,
    /// never weakening the gate. The downstream stream recording is fully contiguous here, which
    /// must NOT excuse the missing burn (the id's frame demonstrably reached the stream, but the
    /// strih-recording readability defect is still a fault to fix — the #356 honesty).
    #[test]
    fn cam1_delivered_frame_missing_burn_is_burn_unreadable_not_real_drop_356() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;
        let args = super::Args::parse_from(["recording-verdict", "--min-secs", "1"]);
        const N: u32 = 600;
        let (v, pass) = build_and_print_verdict(
            &args,
            Some(DecodedRec {
                frames: window_none(N, false, 5),
                rec_path: None,
            }),
            Some(DecodedRec {
                frames: window(N, true, None),
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            None, // #461: no imag frames in this test
            None, // #312 item 2 (PR A): no carried A/V-sync inputs in this test
        )
        .expect("verdict");
        assert!(
            !pass,
            "#571: a delivered frame missing its cam1 burn must FAIL the headline: {v}"
        );
        assert_eq!(v["full_chain"]["zero_loss"], serde_json::json!(false));
        assert_eq!(
            v["full_chain"]["real_drops"],
            serde_json::json!(0),
            "#571: the None is BURN-UNREADABLE, never a phantom REAL DROP: {}",
            v["full_chain"]
        );
        assert!(
            v["full_chain"]["burn_unreadable"].as_u64().unwrap() >= 1,
            "#571: the missing cam1 burn must be charged BURN-UNREADABLE: {}",
            v["full_chain"]
        );
    }

    /// #373 — the headline duration floor must scale each node's span by its SOURCE recording's
    /// capture rate, NOT one shared `--capture-fps`. cam1 is read from the strih recording
    /// (`--capture-fps`, 60 on the rig); strih + stream from the stream recording
    /// (`--stream-capture-fps`, 30). A single rate halves strih/stream's reported span and
    /// FALSE-FAILS a genuine zero-loss run on the rig's `--capture-fps 60`. This is the end-to-end
    /// wiring lock (the pure decision is `recording_span_gate::node_capture_fps`): a revert to one
    /// shared rate in `build_and_print_verdict` would FALSE-FAIL this real run.
    #[test]
    fn headline_span_floor_uses_per_recording_capture_fps_373() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;
        // Rig rates: cam1/strih recording @ 60, stream recording @ 30; floor 5 s.
        let args = super::Args::parse_from([
            "recording-verdict",
            "--min-secs",
            "5",
            "--capture-fps",
            "60",
            "--stream-capture-fps",
            "30",
        ]);
        // strih recording (cam1's source, read @60): 300 frames = exactly 5 s ⇒ clears the floor.
        let strih = window(300, false, None);
        // stream recording (strih+stream source, read @30): 200 frames = 6.67 s at the RIGHT rate
        // (clears 5 s), but only 3.33 s if wrongly divided by 60 (the bug ⇒ strih+stream FALSE-FAIL).
        let stream = window(200, true, None);
        let (v, pass) = build_and_print_verdict(
            &args,
            Some(DecodedRec {
                frames: strih,
                rec_path: None,
            }),
            Some(DecodedRec {
                frames: stream,
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            None, // #461: no imag frames in this test
            None, // #312 item 2 (PR A): no carried A/V-sync inputs in this test
        )
        .expect("verdict");
        assert!(
            pass,
            "#373: strih/stream span must scale by stream_capture_fps (30) not capture_fps (60) — a \
             real >=floor run MUST pass; a single shared rate would false-fail it: {}",
            v["full_chain"]["loss"]
        );
        assert_eq!(v["overall_pass"], serde_json::json!(true));
        assert_eq!(v["full_chain"]["zero_loss"], serde_json::json!(true));
    }

    /// #332: the all-cambox `all_cambox_continuity` block must be produced by the per-box MERGE path
    /// (stream frames sourced from a per-box partial JSON) IDENTICALLY to the fused path, when the
    /// SAME `--switch-schedule` is supplied — proving the all-cambox verdict can run on the stream
    /// box (the default decode-on-stream path) and need NOT be forced onto dev1. The all_cambox
    /// computation lives in the SHARED `build_and_print_verdict`, which `run_merge` calls, so it
    /// flows through whether the stream frames came from a live decode (fused) or a deserialized
    /// partial (merge). The harness wiring (`MERGE_ARGS+=(--switch-schedule …)`, guard removed) is
    /// covered by `tests/harness_recording_e2e_paths.rs::recording_e2e_all_cambox_sweep_runs_on_stream_box`.
    #[test]
    fn merge_path_computes_all_cambox_continuity_like_the_fused_path() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use camera_box::probe::recording_partial::RecordingPartial;
        use clap::Parser;
        use std::path::PathBuf;

        const ONE_S: i64 = 1_000_000_000;
        let base = 1_000 * ONE_S; // schedule on a realistic gen_ts_ns timeline
        let win = 5 * ONE_S; // two 5 s program windows
        let dir = std::env::temp_dir().join(format!("cb-332-allcambox-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sched_path = dir.join("switch-schedule.json");
        // Two NON-overlapping windows on the burn gen_ts_ns timeline, CAM1 then CAM4 (#333 default
        // sweep — the painter box CAM2 is excluded). The same shape the harness writes.
        let sched = format!(
            r#"[{{"cambox":"CAM1","start_ns":{a},"end_ns":{b}}},{{"cambox":"CAM4","start_ns":{b},"end_ns":{c}}}]"#,
            a = base,
            b = base + win,
            c = base + 2 * win,
        );
        std::fs::write(&sched_path, &sched).unwrap();

        // Build the SINGLE continuous stream recording's frames: 40 per window at 100 ms spacing,
        // each carrying a STRIH burn payload as the gen_ts ANCHOR (anchor_run_ids = [strih, stream])
        // and an optical Vernier tick (RecordingFrame::tick) stepping by 2 (the 60→30 decimation).
        // The optical tick is globally continuous (all boxes capture the SAME painter via the
        // splitter), so each window is internally contiguous ⇒ a clean all-cambox PASS.
        let mut stream_frames: Vec<RecordingFrame> = Vec::new();
        for i in 0..80u64 {
            let wi = (i / 40) as i64; // window 0 then window 1
            let j = (i % 40) as i64; // within-window position
            let wstart = base + wi * win;
            let gen_ts = wstart + (j + 1) * (ONE_S / 10); // 0.1 s .. 4.0 s inside the 5 s window
            let optical = 1000u32 + 2 * i as u32;
            stream_frames.push(RecordingFrame {
                frame_index: i,
                payloads: vec![
                    Payload {
                        run_id: STRIH, // the segmentation anchor (strih program render time)
                        frame_id: 1670 + i as u32,
                        gen_ts_ns: gen_ts,
                    },
                    Payload {
                        run_id: CAM2, // the optical paint payload (non-burn)
                        frame_id: optical,
                        gen_ts_ns: gen_ts,
                    },
                ],
                tick: Some(optical),
            });
        }

        // guard 0 + explicit step 2 so the windows aren't trimmed and no fps is inferred.
        let args = super::Args::parse_from([
            "recording-verdict",
            "--switch-schedule",
            sched_path.to_str().unwrap(),
            "--switch-guard-ns",
            "0",
            "--switch-expected-step",
            "2",
        ]);

        // FUSED: stream frames decoded here (rec_path Some-vs-None is irrelevant to all_cambox).
        let (fused, _) = build_and_print_verdict(
            &args,
            None,
            Some(DecodedRec {
                frames: stream_frames.clone(),
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            None, // #461: no imag frames in this test
            None, // #312 item 2 (PR A): no carried A/V-sync inputs in this test
        )
        .expect("fused verdict");

        // MERGE: round-trip the stream frames through the per-box partial JSON (exactly what
        // run_merge loads), then build the verdict — the all_cambox block must be identical.
        let stream_p = RecordingPartial::from_frames(
            "stream",
            &PathBuf::from("stream.mp4"),
            &[CAM1B, STRIH, STREAM],
            stream_frames,
        );
        let stream_back = RecordingPartial::from_json(&stream_p.to_json().unwrap()).unwrap();
        let (merged, _) = build_and_print_verdict(
            &args,
            None,
            Some(DecodedRec {
                frames: stream_back.frames,
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            None, // #461: no imag frames in this test
            None, // #312 item 2 (PR A): no carried A/V-sync inputs in this test
        )
        .expect("merged verdict");

        let fused_seg = &fused["all_cambox_continuity"];
        let merged_seg = &merged["all_cambox_continuity"];
        assert!(
            !fused_seg.is_null(),
            "#332: the fused path must compute all_cambox_continuity when --switch-schedule is given"
        );
        assert_eq!(
            merged_seg, fused_seg,
            "#332: the MERGE path must compute the SAME all_cambox_continuity as the fused path \
             (stream frames from a partial JSON ⇒ identical per-cambox verdict)"
        );
        assert_eq!(
            merged_seg["overall_pass"],
            serde_json::json!(true),
            "clean per-window painted ticks ⇒ all-cambox PASS in the merge: {merged_seg}"
        );
        let labels: Vec<&str> = merged_seg["segments"]
            .as_array()
            .expect("segments array")
            .iter()
            .map(|s| s["cambox"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(
            labels,
            vec!["CAM1", "CAM4"],
            "#332: both swept (non-painter) camboxes are attributed in the merge"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #467 — extend the #312 ALL-CAMBOX `--switch-schedule` sweep to ALSO gate imag-nb's OWN
    /// per-segment continuity. imag's frames are placed onto the SAME schedule timeline (anchored
    /// on its #463 digital corner burn, [`super::BURN_RUN_ID_IMAG`]) and its own painted-tick
    /// continuity — at its OWN native rate (step 1, never the stream recording's 60->30 step 2) —
    /// must ALSO pass, reported under `all_cambox_continuity.imag` and ANDed into `overall_pass`,
    /// alongside (not instead of) the existing per-cambox windows.
    #[test]
    fn imag_own_segment_continuity_gates_the_all_cambox_sweep_467() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;

        const ONE_S: i64 = 1_000_000_000;
        let base = 2_000 * ONE_S;
        let win = 5 * ONE_S;
        let dir =
            std::env::temp_dir().join(format!("cb-467-imag-seg-clean-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sched_path = dir.join("switch-schedule.json");
        let sched = format!(
            r#"[{{"cambox":"CAM1","start_ns":{a},"end_ns":{b}}},{{"cambox":"CAM4","start_ns":{b},"end_ns":{c}}}]"#,
            a = base,
            b = base + win,
            c = base + 2 * win,
        );
        std::fs::write(&sched_path, &sched).unwrap();

        // The single continuous stream recording — clean, per-cambox contiguous (the proven shape
        // from `merge_path_computes_all_cambox_continuity_like_the_fused_path`).
        let mut stream_frames: Vec<RecordingFrame> = Vec::new();
        for i in 0..80u64 {
            let wi = (i / 40) as i64;
            let j = (i % 40) as i64;
            let wstart = base + wi * win;
            let gen_ts = wstart + (j + 1) * (ONE_S / 10);
            let optical = 1000u32 + 2 * i as u32;
            stream_frames.push(RecordingFrame {
                frame_index: i,
                payloads: vec![
                    Payload {
                        run_id: STRIH,
                        frame_id: 1670 + i as u32,
                        gen_ts_ns: gen_ts,
                    },
                    Payload {
                        run_id: CAM2,
                        frame_id: optical,
                        gen_ts_ns: gen_ts,
                    },
                ],
                tick: Some(optical),
            });
        }

        // imag's OWN recording: anchored on its #463 digital corner burn; its OWN optical tick is
        // contiguous 2000..2019 across BOTH windows (step 1 — imag captures the 60Hz painter 1:1
        // at its own 60fps, no 60->30 decimation).
        let imag_frames: Vec<RecordingFrame> = (0..20u64)
            .map(|i| {
                let wi = (i / 10) as i64;
                let j = (i % 10) as i64;
                let wstart = base + wi * win;
                let gen_ts = wstart + (j + 1) * (ONE_S / 10);
                RecordingFrame {
                    frame_index: i,
                    payloads: vec![Payload {
                        run_id: super::BURN_RUN_ID_IMAG,
                        frame_id: 5000 + i as u32,
                        gen_ts_ns: gen_ts,
                    }],
                    tick: Some(2000 + i as u32),
                }
            })
            .collect();

        let args = super::Args::parse_from([
            "recording-verdict",
            "--switch-schedule",
            sched_path.to_str().unwrap(),
            "--switch-guard-ns",
            "0",
            "--switch-expected-step",
            "2",
        ]);

        let (v, pass) = build_and_print_verdict(
            &args,
            None,
            Some(DecodedRec {
                frames: stream_frames,
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            Some(DecodedRec {
                frames: imag_frames,
                rec_path: None,
            }),
            None, // #312 item 2 (PR A): no carried A/V-sync inputs in this test
        )
        .expect("verdict");

        let imag_seg = &v["all_cambox_continuity"]["imag"];
        assert!(
            !imag_seg.is_null(),
            "#467: imag's own segment continuity must be reported under all_cambox_continuity.imag: {v}"
        );
        assert_eq!(
            imag_seg["overall_pass"],
            serde_json::json!(true),
            "#467: imag's own contiguous painted tick across both windows must PASS: {imag_seg}"
        );
        assert_eq!(
            v["all_cambox_continuity"]["overall_pass"],
            serde_json::json!(true),
            "sanity: the existing per-cambox sweep itself still passes unchanged: {v}"
        );
        // The overall verdict `pass` is intentionally NOT asserted true here. The all_cambox
        // switch-schedule sweep (imag included, #467) is clean — asserted above — but the
        // overall verdict ALSO runs the full-chain zero-loss gate, which requires a >=300s
        // recording span (min_secs=300). This synthetic fixture spans only seconds, so
        // full_chain.span_ok is false and overall `pass` is false for a reason UNRELATED to
        // #467. What #467 must guarantee — that a clean imag segment adds NO failure to the
        // sweep and imag's own data is loss-free — is covered by the sweep assertions above
        // plus this: imag's recording is loss-free (its span-gate failure is the only reason).
        assert_eq!(
            v["full_chain"]["loss"]["imag"]["zero_loss"],
            serde_json::json!(true),
            "#467: imag's own recording must be loss-free (only the unrelated >=300s span gate fails overall): {v}"
        );
        let _ = pass;

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #467 — a genuine gap in imag's OWN painted-tick sequence (one recorded frame never reached
    /// imag's recording) must FAIL the overall verdict even though the existing per-cambox
    /// (stream) sweep stays completely clean — imag's segment gate is ADDITIONAL, not a
    /// weaker/optional substitute for the per-cambox windows.
    #[test]
    fn imag_own_segment_gap_fails_the_all_cambox_sweep_even_when_stream_sweep_is_clean_467() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;

        const ONE_S: i64 = 1_000_000_000;
        let base = 3_000 * ONE_S;
        let win = 5 * ONE_S;
        let dir =
            std::env::temp_dir().join(format!("cb-467-imag-seg-broken-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sched_path = dir.join("switch-schedule.json");
        let sched = format!(
            r#"[{{"cambox":"CAM1","start_ns":{a},"end_ns":{b}}},{{"cambox":"CAM4","start_ns":{b},"end_ns":{c}}}]"#,
            a = base,
            b = base + win,
            c = base + 2 * win,
        );
        std::fs::write(&sched_path, &sched).unwrap();

        let mut stream_frames: Vec<RecordingFrame> = Vec::new();
        for i in 0..80u64 {
            let wi = (i / 40) as i64;
            let j = (i % 40) as i64;
            let wstart = base + wi * win;
            let gen_ts = wstart + (j + 1) * (ONE_S / 10);
            let optical = 1000u32 + 2 * i as u32;
            stream_frames.push(RecordingFrame {
                frame_index: i,
                payloads: vec![
                    Payload {
                        run_id: STRIH,
                        frame_id: 1670 + i as u32,
                        gen_ts_ns: gen_ts,
                    },
                    Payload {
                        run_id: CAM2,
                        frame_id: optical,
                        gen_ts_ns: gen_ts,
                    },
                ],
                tick: Some(optical),
            });
        }

        // imag's own recording — IDENTICAL to the clean test above, except recorded output frame
        // i=15 (tick 2015, inside the SECOND [CAM4] window) never reached the recording at all: a
        // genuine dropped frame, not a by-design decimation. The FIRST (CAM1) window is untouched.
        let imag_frames: Vec<RecordingFrame> = (0..20u64)
            .filter(|&i| i != 15)
            .map(|i| {
                let wi = (i / 10) as i64;
                let j = (i % 10) as i64;
                let wstart = base + wi * win;
                let gen_ts = wstart + (j + 1) * (ONE_S / 10);
                RecordingFrame {
                    frame_index: i,
                    payloads: vec![Payload {
                        run_id: super::BURN_RUN_ID_IMAG,
                        frame_id: 5000 + i as u32,
                        gen_ts_ns: gen_ts,
                    }],
                    tick: Some(2000 + i as u32),
                }
            })
            .collect();

        let args = super::Args::parse_from([
            "recording-verdict",
            "--switch-schedule",
            sched_path.to_str().unwrap(),
            "--switch-guard-ns",
            "0",
            "--switch-expected-step",
            "2",
        ]);

        let (v, pass) = build_and_print_verdict(
            &args,
            None,
            Some(DecodedRec {
                frames: stream_frames,
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            Some(DecodedRec {
                frames: imag_frames,
                rec_path: None,
            }),
            None, // #312 item 2 (PR A): no carried A/V-sync inputs in this test
        )
        .expect("verdict");

        let imag_seg = &v["all_cambox_continuity"]["imag"];
        let imag_segments = imag_seg["segments"]
            .as_array()
            .expect("imag segments array");
        assert_eq!(
            imag_segments[0]["pass"],
            serde_json::json!(true),
            "the FIRST (CAM1) window is untouched, still clean: {imag_seg}"
        );
        assert_eq!(
            imag_segments[1]["pass"],
            serde_json::json!(false),
            "the SECOND (CAM4) window has the dropped frame → FAIL: {imag_seg}"
        );
        // #583 — under the honest per-segment gate a genuinely-dropped frame (both its optical tick
        // AND its digital burn id absent) FAILS via the DIGITAL BURN — imag's per-frame delivery
        // authority — NOT the strict painted-tick "gap" count the old model used (a lone optical skip
        // with the burn still present is a benign same-rate beat, not loss). The dropped frame 15's
        // burn id (5015) must show as a missing digital burn id in the failing window.
        let burn_missing = imag_segments[1]["burn_missing_ids"]
            .as_array()
            .expect("burn_missing_ids array on the failing imag window");
        assert!(
            !burn_missing.is_empty(),
            "#583: the dropped frame must be caught as a missing digital burn id: {imag_seg}"
        );
        assert_eq!(
            imag_seg["overall_pass"],
            serde_json::json!(false),
            "#467: a genuine gap in imag's OWN segment must FAIL imag's own verdict: {imag_seg}"
        );
        assert_eq!(
            v["all_cambox_continuity"]["overall_pass"],
            serde_json::json!(true),
            "sanity: the existing per-cambox (stream) sweep is completely untouched and stays clean: {v}"
        );
        assert!(
            !pass,
            "#467: imag's own segment gap must fail the OVERALL verdict even though the stream \
             sweep alone is clean — imag's gate is additional, never optional: {v}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- #624 deliverables 1+3 — per-camera, per-switch-window cam2->camera latency + the
    // cross-camera spread gate ----
    //
    // The whole-recording, cam1-ONLY cam2->camera OPTICAL-INJECTION hop (#179/#194,
    // `full_chain.latency.cam2_cam1`) stays UNCHANGED (see the tests above/below that never
    // touch `all_cambox_latency`). #624 ADDS a SECOND measurement alongside it: the SAME
    // cam2->camera pairing (`cam2_cam1_samples_from_burn` / `_from_flip`, ALREADY generic on
    // its 3rd `cam1_burn_id` param), generalized to EVERY `CAMERA_UNDER_TEST_NODES` label
    // (cam1/cam3/cam4) and restricted to that camera's OWN `--switch-schedule` window(s) — the
    // ALL_CAMBOX sweep cuts each camera into strih program in turn, so its OWN capture-time
    // burn only rides alongside cam2's optical QR while ITS window is live. The per-camera
    // medians then feed the hard cross-camera SPREAD gate
    // (`camera_box::switch_latency::spread_verdict`): `max(p50) - min(p50) > 16ms` (half a
    // 30fps frame) = FAIL — a differing photon->dequeue latency `d_X` per camera (#286's root
    // cause) beyond that floor can visibly break A/V lipsync when the live program cuts
    // between them.

    /// Build a synthetic single continuous stream recording covering `cameras.len()` contiguous
    /// 5s `--switch-schedule` windows (one per entry), each carrying: the STRIH burn (the
    /// segmentation anchor), cam2's optical QR (paint ts), and THAT window's camera's OWN
    /// capture-time burn stamped `latency_ns` AFTER cam2's paint — the injected #286 d_X. Returns
    /// the full verdict report JSON. `cameras` is `(cambox schedule label, camera's own burn
    /// run_id, injected cam2->camera latency in ns)`.
    fn build_all_cambox_latency_fixture(
        tag: &str,
        cameras: &[(&str, u32, i64)],
    ) -> serde_json::Value {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;

        const ONE_S: i64 = 1_000_000_000;
        let base = 5_000 * ONE_S;
        let win = 5 * ONE_S;
        let dir = std::env::temp_dir().join(format!("cb-624-latency-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sched_path = dir.join("switch-schedule.json");
        let windows_json: Vec<String> = cameras
            .iter()
            .enumerate()
            .map(|(wi, &(label, _, _))| {
                let start_ns = base + (wi as i64) * win;
                let end_ns = start_ns + win;
                format!(r#"{{"cambox":"{label}","start_ns":{start_ns},"end_ns":{end_ns}}}"#)
            })
            .collect();
        std::fs::write(&sched_path, format!("[{}]", windows_json.join(","))).unwrap();

        // 20 frames per window, 0.2s apart (0.2s..4.0s inside each 5s window — well clear of any
        // guard, and `--switch-guard-ns 0` below leaves no guard to clear anyway).
        let mut stream_frames: Vec<RecordingFrame> = Vec::new();
        let mut idx = 0u64;
        for (wi, &(_, camera_burn, latency_ns)) in cameras.iter().enumerate() {
            let wstart = base + (wi as i64) * win;
            for j in 0..20i64 {
                let paint_ts = wstart + (j + 1) * (ONE_S / 5);
                let optical = 1000u32 + idx as u32;
                stream_frames.push(RecordingFrame {
                    frame_index: idx,
                    payloads: vec![
                        Payload {
                            run_id: STRIH,
                            frame_id: 1670 + idx as u32,
                            gen_ts_ns: paint_ts,
                        },
                        Payload {
                            run_id: CAM2,
                            frame_id: optical,
                            gen_ts_ns: paint_ts,
                        },
                        Payload {
                            run_id: camera_burn,
                            frame_id: 5000 + idx as u32,
                            gen_ts_ns: paint_ts + latency_ns,
                        },
                    ],
                    tick: Some(optical),
                });
                idx += 1;
            }
        }

        let args = super::Args::parse_from([
            "recording-verdict",
            "--switch-schedule",
            sched_path.to_str().unwrap(),
            "--switch-guard-ns",
            "0",
            "--switch-expected-step",
            "2",
        ]);

        let (v, _pass) = build_and_print_verdict(
            &args,
            None,
            Some(DecodedRec {
                frames: stream_frames,
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            None, // #461: no imag frames in this fixture
            None, // #312 item 2 (PR A): no carried A/V-sync inputs in this test
        )
        .expect("verdict");

        let _ = std::fs::remove_dir_all(&dir);
        v
    }

    /// #624: cam1/cam3/cam4 all deployed, EACH camera's OWN measured cam2->camera median exactly
    /// matches its injected latency, and a 25ms cross-camera spread (over the 16ms floor) FAILS
    /// the gate.
    #[test]
    fn all_cambox_latency_measures_per_camera_windowed_latency_and_fails_a_wide_spread_624() {
        let v = build_all_cambox_latency_fixture(
            "fail",
            &[
                ("CAM1", CAM1B, 800_000_000),
                ("CAM3", super::BURN_RUN_ID_CAM3, 820_000_000),
                ("CAM4", super::BURN_RUN_ID_CAM4, 795_000_000),
            ],
        );

        let lat = &v["all_cambox_latency"];
        assert!(
            !lat.is_null(),
            "#624: all_cambox_latency must be reported when --switch-schedule is given: {v}"
        );
        assert_eq!(
            lat["cam1"]["p50_ms"],
            serde_json::json!(800.0),
            "#624: cam1's OWN windowed cam2->cam1 latency: {lat}"
        );
        assert_eq!(
            lat["cam3"]["p50_ms"],
            serde_json::json!(820.0),
            "#624: cam3's OWN windowed cam2->cam3 latency (generalized from cam1-only): {lat}"
        );
        assert_eq!(
            lat["cam4"]["p50_ms"],
            serde_json::json!(795.0),
            "#624: cam4's OWN windowed cam2->cam4 latency (generalized from cam1-only): {lat}"
        );
        assert_eq!(
            lat["cross_camera_spread_ms"],
            serde_json::json!(25.0),
            "#624: max(820) - min(795) = 25ms: {lat}"
        );
        assert_eq!(
            lat["spread_gate_pass"],
            serde_json::json!(false),
            "#624: a 25ms cross-camera spread is over the 16ms floor -> the gate must FAIL: {lat}"
        );
    }

    /// #624: the SAME 3-camera shape, but every camera's injected latency sits within 16ms of
    /// every other's -> the spread gate PASSES.
    #[test]
    fn all_cambox_latency_spread_within_16ms_passes_the_gate_624() {
        let v = build_all_cambox_latency_fixture(
            "pass",
            &[
                ("CAM1", CAM1B, 800_000_000),
                ("CAM3", super::BURN_RUN_ID_CAM3, 805_000_000),
                ("CAM4", super::BURN_RUN_ID_CAM4, 810_000_000),
            ],
        );

        let lat = &v["all_cambox_latency"];
        assert_eq!(lat["cam1"]["p50_ms"], serde_json::json!(800.0));
        assert_eq!(lat["cam3"]["p50_ms"], serde_json::json!(805.0));
        assert_eq!(lat["cam4"]["p50_ms"], serde_json::json!(810.0));
        assert_eq!(
            lat["cross_camera_spread_ms"],
            serde_json::json!(10.0),
            "#624: max(810) - min(800) = 10ms: {lat}"
        );
        assert_eq!(
            lat["spread_gate_pass"],
            serde_json::json!(true),
            "#624: a 10ms cross-camera spread clears the 16ms floor -> PASS: {lat}"
        );
    }

    /// #624: only ONE camera's window is present this run (e.g. a partial sweep, or two of the
    /// three camera boxes down) — the OTHER two cameras must surface `null` (never a fabricated
    /// zero, matching how `hop_lat_json` already reports "no samples" everywhere else in this
    /// file), and the spread gate — needing at least 2 measured cameras to compare — must ALSO
    /// report `null` (unmeasured, never a fabricated pass or fail).
    #[test]
    fn all_cambox_latency_with_a_single_measured_camera_reports_null_not_a_fabricated_spread_624() {
        let v = build_all_cambox_latency_fixture("single", &[("CAM1", CAM1B, 800_000_000)]);

        let lat = &v["all_cambox_latency"];
        assert_eq!(
            lat["cam1"]["p50_ms"],
            serde_json::json!(800.0),
            "#624: the one measured camera still reports its real latency: {lat}"
        );
        assert!(
            lat["cam3"].is_null(),
            "#624: cam3 produced no samples this run -> null, never a fabricated zero: {lat}"
        );
        assert!(
            lat["cam4"].is_null(),
            "#624: cam4 produced no samples this run -> null, never a fabricated zero: {lat}"
        );
        assert!(
            lat["cross_camera_spread_ms"].is_null(),
            "#624: fewer than 2 measured cameras -> the spread is UNMEASURABLE, not a fabricated \
             number: {lat}"
        );
        assert!(
            lat["spread_gate_pass"].is_null(),
            "#624: an unmeasurable spread must never fabricate a pass or a fail: {lat}"
        );
    }

    // ---- #286 Gap 1+2 — per-camera DELIVERY latency (strih recording), ALL 6 cameras incl.
    // cam2 ----
    //
    // `all_cambox_latency` above (and its `cross_camera_spread_ms`) measures each camera's own
    // SOURCE-side photon-to-CAPTURE latency (`d_X`) — architecturally BEFORE and INDEPENDENT of
    // strih's receiver-side per-source genlock hold, the exact knob #286's phase-sync fix
    // adjusts. `all_cambox_delivery_latency` wires the metric #286's OWN Verify criterion
    // actually needs — `strih_burn.gen_ts_ns − camera_burn.gen_ts_ns` (the DELIVERY latency,
    // which DOES include the genlock receiver hold) — via
    // `probe::recording_latency::n_camera_strih_samples` (existed, tested, but never called
    // from this binary before this PR). ALL SIX `CAMERA_UNDER_TEST_NODES` are measured,
    // INCLUDING cam2 (#312/#637: cam2 has its OWN digital capture burn + its OWN
    // `--switch-schedule` window, so its delivery latency is measurable the same digital way as
    // every other camera — no optical read of its own monitor is needed for THIS metric).

    /// Build a synthetic STRIH recording covering `cameras.len()` contiguous 5s
    /// `--switch-schedule` windows, each carrying: that window's camera's OWN capture-time burn,
    /// and strih's own render-time burn stamped `latency_ns` AFTER the camera's capture — the
    /// injected #286 DELIVERY latency (unlike the source-side fixture, this simulates whatever
    /// the genlock receiver held the frame for, since strih's burn is stamped at strih's own
    /// render/delivery time). A STREAM recording is ALSO built, in the SAME per-window shape
    /// `build_all_cambox_latency_fixture` above already exercises, purely so
    /// `build_and_print_verdict`'s `Some(stream)` gate is entered at all (required for ANY
    /// ALL_CAMBOX `--switch-schedule` sweep) — this fixture's assertions never touch anything
    /// stream-derived. `cameras` is `(cambox schedule label, camera's own capture-burn run_id,
    /// injected delivery latency in ns)`.
    fn build_all_cambox_delivery_latency_fixture(
        tag: &str,
        cameras: &[(&str, u32, i64)],
    ) -> serde_json::Value {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;

        const ONE_S: i64 = 1_000_000_000;
        let base = 9_000 * ONE_S;
        let win = 5 * ONE_S;
        let dir =
            std::env::temp_dir().join(format!("cb-286-delivery-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sched_path = dir.join("switch-schedule.json");
        let windows_json: Vec<String> = cameras
            .iter()
            .enumerate()
            .map(|(wi, &(label, _, _))| {
                let start_ns = base + (wi as i64) * win;
                let end_ns = start_ns + win;
                format!(r#"{{"cambox":"{label}","start_ns":{start_ns},"end_ns":{end_ns}}}"#)
            })
            .collect();
        std::fs::write(&sched_path, format!("[{}]", windows_json.join(","))).unwrap();

        // 20 frames per window, 0.2s apart, well clear of any guard (--switch-guard-ns 0 leaves
        // no guard to clear anyway). Each STRIH frame carries ONLY that window's camera's own
        // capture burn + strih's own render burn — no cam2 optical QR at all, since the
        // delivery-latency pairing needs neither (unlike the source-side `all_cambox_latency`
        // fixture above).
        let mut strih_frames: Vec<RecordingFrame> = Vec::new();
        // The STREAM recording is built in the SAME per-window shape
        // `build_all_cambox_latency_fixture` above already exercises (STRIH burn + cam2 optical
        // QR + that window's camera burn) — supplied purely so `build_and_print_verdict`'s
        // `Some(stream)` gate is entered at all (it requires a stream recording for ANY
        // ALL_CAMBOX `--switch-schedule` sweep); reusing the ALREADY-PROVEN-SAFE fixture shape
        // here (rather than an untested empty/degenerate stream) avoids exercising an edge case
        // nobody else has verified. This fixture's own assertions never read anything
        // stream-derived (`all_cambox_latency`/`all_cambox_continuity`/`all_cambox_av_sync`).
        let mut stream_frames: Vec<RecordingFrame> = Vec::new();
        let mut idx = 0u64;
        for (wi, &(_, camera_burn, latency_ns)) in cameras.iter().enumerate() {
            let wstart = base + (wi as i64) * win;
            for j in 0..20i64 {
                let cap_ts = wstart + (j + 1) * (ONE_S / 5);
                strih_frames.push(RecordingFrame {
                    frame_index: idx,
                    payloads: vec![
                        Payload {
                            run_id: camera_burn,
                            frame_id: 5000 + idx as u32,
                            gen_ts_ns: cap_ts,
                        },
                        Payload {
                            run_id: STRIH,
                            frame_id: 1670 + idx as u32,
                            gen_ts_ns: cap_ts + latency_ns,
                        },
                    ],
                    tick: None,
                });
                let paint_ts = wstart + (j + 1) * (ONE_S / 5);
                let optical = 1000u32 + idx as u32;
                stream_frames.push(RecordingFrame {
                    frame_index: idx,
                    payloads: vec![
                        Payload {
                            run_id: STRIH,
                            frame_id: 1670 + idx as u32,
                            gen_ts_ns: paint_ts,
                        },
                        Payload {
                            run_id: CAM2,
                            frame_id: optical,
                            gen_ts_ns: paint_ts,
                        },
                        Payload {
                            run_id: camera_burn,
                            frame_id: 5000 + idx as u32,
                            gen_ts_ns: paint_ts + latency_ns,
                        },
                    ],
                    tick: Some(optical),
                });
                idx += 1;
            }
        }

        let args = super::Args::parse_from([
            "recording-verdict",
            "--switch-schedule",
            sched_path.to_str().unwrap(),
            "--switch-guard-ns",
            "0",
            "--switch-expected-step",
            "2",
        ]);

        let (v, _pass) = build_and_print_verdict(
            &args,
            Some(DecodedRec {
                frames: strih_frames,
                rec_path: None,
            }),
            Some(DecodedRec {
                frames: stream_frames,
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            None, // #461: no imag frames in this fixture
            None, // #312 item 2 (PR A): no carried A/V-sync inputs in this test
        )
        .expect("verdict");

        let _ = std::fs::remove_dir_all(&dir);
        v
    }

    /// #286: cam1/cam2/cam4 each measured, EACH camera's OWN delivery latency exactly matches
    /// its injected value, cam2 is measured (Gap 2 — it is EXCLUDED from the source-side
    /// `all_cambox_latency` sweep, but must NOT be excluded here), cam3/cam5/cam6 (no window
    /// this run) report null, and a wide cross-camera spread FAILS the (report-only) gate.
    #[test]
    fn all_cambox_delivery_latency_measures_receiver_side_delivery_and_includes_cam2_286() {
        let v = build_all_cambox_delivery_latency_fixture(
            "fail",
            &[
                ("CAM1", CAM1B, 3_000_000),
                ("CAM2", CAM2B, 20_000_000),
                ("CAM4", super::BURN_RUN_ID_CAM4, 8_000_000),
            ],
        );

        let lat = &v["all_cambox_delivery_latency"];
        assert!(
            !lat.is_null(),
            "#286: all_cambox_delivery_latency must be reported when a --strih recording is \
             supplied alongside --switch-schedule: {v}"
        );
        assert_eq!(
            lat["cam1"]["p50_ms"],
            serde_json::json!(3.0),
            "#286: cam1's OWN windowed delivery (strih_burn - camera_burn) latency: {lat}"
        );
        assert_eq!(
            lat["cam2"]["p50_ms"],
            serde_json::json!(20.0),
            "#286 Gap 2: cam2 MUST be measured here (its own digital capture burn + its own \
             schedule window make it just as measurable as any other camera — unlike the \
             OPTICAL-INJECTION source-side sweep, which structurally excludes it): {lat}"
        );
        assert_eq!(
            lat["cam4"]["p50_ms"],
            serde_json::json!(8.0),
            "#286: cam4's OWN windowed delivery latency: {lat}"
        );
        assert!(
            lat["cam3"].is_null(),
            "#286: cam3 had no window this run -> null, never a fabricated zero: {lat}"
        );
        assert!(
            lat["cam5"].is_null(),
            "#286: cam5 had no window this run -> null, never a fabricated zero: {lat}"
        );
        assert!(
            lat["cam6"].is_null(),
            "#286: cam6 had no window this run -> null, never a fabricated zero: {lat}"
        );
        assert_eq!(
            lat["cross_camera_spread_ms"],
            serde_json::json!(17.0),
            "#286: max(20.0) - min(3.0) = 17ms: {lat}"
        );
        assert_eq!(
            lat["spread_gate_pass"],
            serde_json::json!(false),
            "#286: a 17ms delivery spread is over the (report-only) 16ms floor -> FAIL: {lat}"
        );
    }

    /// #286: ALL SIX cameras measured (including cam2), each within 16ms of every other's
    /// injected delivery latency -> the (report-only) spread gate PASSES — mirrors a
    /// successfully phase-synced rig where the applied differentiated genlock-latency offsets
    /// have collapsed every camera's DELIVERY latency to roughly the same value.
    #[test]
    fn all_cambox_delivery_latency_all_six_cameras_within_16ms_passes_the_gate_286() {
        let v = build_all_cambox_delivery_latency_fixture(
            "pass",
            &[
                ("CAM1", CAM1B, 80_000_000),
                ("CAM3", super::BURN_RUN_ID_CAM3, 82_000_000),
                ("CAM4", super::BURN_RUN_ID_CAM4, 79_500_000),
                ("CAM5", super::BURN_RUN_ID_CAM5, 81_000_000),
                ("CAM6", super::BURN_RUN_ID_CAM6, 78_000_000),
                ("CAM2", CAM2B, 80_500_000),
            ],
        );

        let lat = &v["all_cambox_delivery_latency"];
        assert_eq!(lat["cam1"]["p50_ms"], serde_json::json!(80.0));
        assert_eq!(lat["cam3"]["p50_ms"], serde_json::json!(82.0));
        assert_eq!(lat["cam4"]["p50_ms"], serde_json::json!(79.5));
        assert_eq!(lat["cam5"]["p50_ms"], serde_json::json!(81.0));
        assert_eq!(lat["cam6"]["p50_ms"], serde_json::json!(78.0));
        assert_eq!(
            lat["cam2"]["p50_ms"],
            serde_json::json!(80.5),
            "#286 Gap 2: cam2 is measured alongside every other camera: {lat}"
        );
        assert_eq!(
            lat["cross_camera_spread_ms"],
            serde_json::json!(4.0),
            "#286: max(82.0) - min(78.0) = 4ms: {lat}"
        );
        assert_eq!(
            lat["spread_gate_pass"],
            serde_json::json!(true),
            "#286: a 4ms delivery spread across all 6 cameras clears the 16ms floor -> PASS: \
             {lat}"
        );
    }

    /// #286: a tight delivery spread (all within 16ms) PASSES the report-only gate.
    #[test]
    fn all_cambox_delivery_latency_tight_spread_passes_gate_286() {
        let v = build_all_cambox_delivery_latency_fixture(
            "tight",
            &[
                ("CAM1", CAM1B, 3_000_000),
                ("CAM3", super::BURN_RUN_ID_CAM3, 5_000_000),
                ("CAM4", super::BURN_RUN_ID_CAM4, 10_000_000),
            ],
        );

        let lat = &v["all_cambox_delivery_latency"];
        assert_eq!(
            lat["cross_camera_spread_ms"],
            serde_json::json!(7.0),
            "#286: max(10.0) - min(3.0) = 7ms: {lat}"
        );
        assert_eq!(
            lat["spread_gate_pass"],
            serde_json::json!(true),
            "#286: a 7ms delivery spread clears the 16ms floor -> PASS: {lat}"
        );
    }

    /// #286: this new field must NEVER affect the run's overall verdict — it is report-only
    /// (#286 is not yet closed/proven; folding an unproven threshold into `all_pass` would make
    /// every future all-cambox sweep subject to a brand-new hard requirement this task never
    /// asked for). A FAILING delivery spread (the "fail" fixture above) must not, by itself,
    /// change what `all_pass` would otherwise have been.
    #[test]
    fn all_cambox_delivery_latency_spread_never_gates_all_pass_286() {
        let v = build_all_cambox_delivery_latency_fixture(
            "no-gate",
            &[("CAM1", CAM1B, 3_000_000), ("CAM2", CAM2B, 20_000_000)],
        );
        let lat = &v["all_cambox_delivery_latency"];
        assert_eq!(
            lat["spread_gate_pass"],
            serde_json::json!(false),
            "sanity: this fixture's spread must be failing for the point of this test to hold: \
             {lat}"
        );
        // This test does not assert on `overall_pass` itself (many OTHER unrelated gates in this
        // fixture's minimal 2-window recording would fail regardless, e.g. the #373 duration
        // floor) — the point here is narrower and purely structural: `spread_gate_pass` must
        // never be read into `all_pass` by the wiring itself. Confirmed by code inspection
        // (`all_cambox_delivery_latency`'s block never assigns to `all_pass`); this test guards
        // against a future edit accidentally adding `all_pass &= sv.pass` to that block, since
        // such a regression would NOT be caught by the two tests above (they don't assert on
        // `overall_pass` at all).
        assert!(
            v.get("all_cambox_delivery_latency").is_some(),
            "sanity: the field itself must still be present: {v}"
        );
    }

    /// #286: with NO `--strih` recording supplied at all, `all_cambox_delivery_latency` must be
    /// absent (never fabricated) — reusing the EXISTING #624 source-side fixture, which supplies
    /// only a stream recording.
    #[test]
    fn all_cambox_delivery_latency_absent_without_a_strih_recording_286() {
        let v = build_all_cambox_latency_fixture("no-strih", &[("CAM1", CAM1B, 800_000_000)]);
        assert!(
            v["all_cambox_delivery_latency"].is_null(),
            "#286: no --strih recording supplied -> the delivery-latency metric is unmeasurable \
             and must be absent, never fabricated: {v}"
        );
    }

    /// #312 item 2 — build the SAME N-window `--switch-schedule` shape
    /// `build_all_cambox_latency_fixture` uses (20 frames/window, globally-unique
    /// `frame_index`/optical `tick` = `1000 + idx`, 5s windows), but ALSO run
    /// `build_and_print_verdict` with the given (fused-mode) `av` inputs + `--av-expected-ms
    /// av_expected_ms` (#624 deliverable 4 / PR B), returning the report JSON. `cameras` is
    /// `(cambox label, camera burn run_id, injected cam2->camera latency ns)` — the latency value
    /// is irrelevant to A/V-sync itself, just realistic filler so the fixture mirrors a real
    /// ALL_CAMBOX sweep.
    fn build_all_cambox_av_sync_fixture(
        tag: &str,
        cameras: &[(&str, u32, i64)],
        av: Option<AvMarkerInputs>,
        av_expected_ms: f64,
    ) -> serde_json::Value {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;

        const ONE_S: i64 = 1_000_000_000;
        let base = 5_000 * ONE_S;
        let win = 5 * ONE_S;
        let dir = std::env::temp_dir().join(format!("cb-312-avsync-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sched_path = dir.join("switch-schedule.json");
        let windows_json: Vec<String> = cameras
            .iter()
            .enumerate()
            .map(|(wi, &(label, _, _))| {
                let start_ns = base + (wi as i64) * win;
                let end_ns = start_ns + win;
                format!(r#"{{"cambox":"{label}","start_ns":{start_ns},"end_ns":{end_ns}}}"#)
            })
            .collect();
        std::fs::write(&sched_path, format!("[{}]", windows_json.join(","))).unwrap();

        let mut stream_frames: Vec<RecordingFrame> = Vec::new();
        let mut idx = 0u64;
        for (wi, &(_, camera_burn, latency_ns)) in cameras.iter().enumerate() {
            let wstart = base + (wi as i64) * win;
            for j in 0..20i64 {
                let paint_ts = wstart + (j + 1) * (ONE_S / 5);
                let optical = 1000u32 + idx as u32;
                stream_frames.push(RecordingFrame {
                    frame_index: idx,
                    payloads: vec![
                        Payload {
                            run_id: STRIH,
                            frame_id: 1670 + idx as u32,
                            gen_ts_ns: paint_ts,
                        },
                        Payload {
                            run_id: CAM2,
                            frame_id: optical,
                            gen_ts_ns: paint_ts,
                        },
                        Payload {
                            run_id: camera_burn,
                            frame_id: 5000 + idx as u32,
                            gen_ts_ns: paint_ts + latency_ns,
                        },
                    ],
                    tick: Some(optical),
                });
                idx += 1;
            }
        }

        let args = super::Args::parse_from([
            "recording-verdict",
            "--switch-schedule",
            sched_path.to_str().unwrap(),
            "--switch-guard-ns",
            "0",
            "--switch-expected-step",
            "2",
            "--av-expected-ms",
            &av_expected_ms.to_string(),
        ]);

        let (v, _pass) = build_and_print_verdict(
            &args,
            None,
            Some(DecodedRec {
                frames: stream_frames,
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            None, // #461: no imag frames in this fixture
            av,
        )
        .expect("verdict");

        let _ = std::fs::remove_dir_all(&dir);
        v
    }

    /// #312 item 2 — cam1's window carries 10 real emit-log/audio-marker pairs, each a
    /// constant 500ms `video - audio` offset; cam3's window has NO matching emit-log frame_id
    /// (its ticks fall outside the emit log's range); cam4/cam5/cam6 never appear in the sweep at
    /// all. cam2 pools the WHOLE recording (unwindowed) and picks up the SAME real cam1-range
    /// candidates. Asserts every fail-closed case from the same test in one shot, PLUS (#624
    /// deliverable 4 / PR B) the per-camera `gate_pass` field: at the default `av_expected_ms=0`,
    /// cam1's real 500ms offset is far outside the ±20ms bound (FAILS the gate despite being a
    /// real, clean measurement — the gate is about closeness to expected, not data quality), and
    /// every Unknown camera fails closed too — so the run's `overall_pass` MUST be false
    /// regardless of what the loss/latency gates alone would have decided.
    #[test]
    fn all_cambox_av_sync_measures_a_real_camera_and_fails_closed_on_the_rest_312() {
        let emit_log: Vec<(u8, u32, i64)> = (0..10u8).map(|k| (k, 1000 + k as u32, 0)).collect();
        let audio_markers: Vec<(f64, u8)> = (0..10u8)
            .map(|k| (k as f64 / 30.0 - 0.5, k)) // video_ts(1000+k) - 0.5s = k/30.0 - 0.5
            .collect();
        let av = AvMarkerInputs {
            fps: 30.0,
            video_start_s: 0.0,
            emit_log,
            audio_markers,
        };
        let v = build_all_cambox_av_sync_fixture(
            "real",
            &[("CAM1", CAM1B, 800_000_000), ("CAM3", CAM3B, 820_000_000)],
            Some(av),
            0.0, // default expected offset
        );

        let av_sync = &v["all_cambox_av_sync"];
        assert!(
            !av_sync.is_null(),
            "#312: all_cambox_av_sync must be reported when av_sync inputs are given: {v}"
        );
        assert_eq!(
            av_sync["cam1"]["verdict"],
            serde_json::json!("measured"),
            "#312: cam1's window carries 10 real candidates -> measured: {av_sync}"
        );
        assert_eq!(
            av_sync["cam1"]["av_offset_ms"],
            serde_json::json!(500.0),
            "#312: every candidate was constructed as exactly 500ms: {av_sync}"
        );
        assert_eq!(
            av_sync["cam1"]["cluster_samples"],
            serde_json::json!(10),
            "#312: all 10 candidates land in the same cluster (no scatter injected): {av_sync}"
        );
        assert_eq!(
            av_sync["cam1"]["gate_pass"],
            serde_json::json!(false),
            "#624: 500ms is far outside the +/-20ms bound of the default expected_ms=0: {av_sync}"
        );
        assert_eq!(
            av_sync["cam2"]["verdict"],
            serde_json::json!("measured"),
            "#312: cam2 pools the WHOLE recording and picks up the same real cam1-range \
             candidates (the sanity-check property from the PR spec): {av_sync}"
        );
        assert_eq!(
            av_sync["cam2"]["av_offset_ms"],
            serde_json::json!(500.0),
            "#312: cam2's whole-recording number reproduces the real offset: {av_sync}"
        );
        assert_eq!(
            av_sync["cam2"]["windowing"],
            serde_json::json!("whole_recording"),
            "#312: cam2 is NEVER windowed (it has no own optical-visible window): {av_sync}"
        );
        assert_eq!(
            av_sync["cam3"]["verdict"],
            serde_json::json!("unknown"),
            "#312: cam3's window exists but has ZERO matching emit-log frame_ids -> unknown, \
             never a fabricated number: {av_sync}"
        );
        assert_eq!(
            av_sync["cam3"]["windows"],
            serde_json::json!(1),
            "#312: cam3's window DID match the schedule (windows=1), just produced 0 candidates: {av_sync}"
        );
        assert_eq!(
            av_sync["cam3"]["candidates"],
            serde_json::json!(0),
            "#312: no emit-log frame_id falls inside cam3's tick range: {av_sync}"
        );
        assert_eq!(
            av_sync["cam3"]["gate_pass"],
            serde_json::json!(false),
            "#624: unknown-on-thin-data must fail the gate closed, never a fabricated pass: {av_sync}"
        );
        for absent in ["cam4", "cam5", "cam6"] {
            assert_eq!(
                av_sync[absent]["verdict"],
                serde_json::json!("unknown"),
                "#312: {absent} never appeared in this sweep -> unknown: {av_sync}"
            );
            assert_eq!(
                av_sync[absent]["windows"],
                serde_json::json!(0),
                "#312: {absent} matched ZERO schedule windows this run: {av_sync}"
            );
            assert_eq!(
                av_sync[absent]["gate_pass"],
                serde_json::json!(false),
                "#624: {absent} absent from the sweep must also fail the gate closed: {av_sync}"
            );
        }
        assert_eq!(
            av_sync["gate_pass"],
            serde_json::json!(false),
            "#624: the OVERALL av_sync gate must be false when ANY camera fails: {av_sync}"
        );
        assert!(
            av_sync["gate"].as_str().unwrap().contains("enforced"),
            "#312 PR B: the gate string must say it is enforced now: {av_sync}"
        );
        assert_eq!(
            v["overall_pass"],
            serde_json::json!(false),
            "#624/#312 PR B: the run's overall verdict MUST fail when the av_sync gate fails -- \
             regardless of what the loss/latency gates alone would have decided: {v}"
        );
    }

    /// #624 deliverable 4 / #312 item 2 PR B — a FAILING av_sync gate forces the run's overall
    /// verdict to FAIL, unconditionally (the AND-in semantics: `all_pass &= av_all_pass`, and
    /// ANDing with `false` can never be undone by any other gate). This SUPERSEDES PR A's
    /// `all_cambox_av_sync_never_affects_the_overall_verdict_312` — that test asserted the
    /// opposite invariant (block reported but never gates), which was PR A's OWN explicitly
    /// temporary contract ("PR B wires it"); this test proves PR B actually did.
    #[test]
    fn all_cambox_av_sync_gate_failure_forces_the_overall_verdict_to_fail_312_624() {
        let emit_log: Vec<(u8, u32, i64)> = (0..10u8).map(|k| (k, 1000 + k as u32, 0)).collect();
        let audio_markers: Vec<(f64, u8)> = (0..10u8).map(|k| (k as f64 / 30.0 - 0.5, k)).collect();
        let av = AvMarkerInputs {
            fps: 30.0,
            video_start_s: 0.0,
            emit_log,
            audio_markers,
        };
        let cameras: &[(&str, u32, i64)] =
            &[("CAM1", CAM1B, 800_000_000), ("CAM3", CAM3B, 820_000_000)];

        // cam1 measures a real 500ms offset; expected_ms=0 puts it far outside +/-20ms -> the
        // av_sync gate FAILS (cam3/cam4/cam5/cam6 are Unknown too, doubly so).
        let with_av = build_all_cambox_av_sync_fixture("gate-fail-with", cameras, Some(av), 0.0);
        let without_av = build_all_cambox_av_sync_fixture("gate-fail-without", cameras, None, 0.0);

        assert!(
            !with_av["all_cambox_av_sync"].is_null(),
            "sanity: the WITH-av_sync run must actually have reported the block: {with_av}"
        );
        assert!(
            without_av["all_cambox_av_sync"].is_null(),
            "sanity: the WITHOUT-av_sync run must NOT report the block at all (unchanged from PR A): {without_av}"
        );
        assert_eq!(
            with_av["all_cambox_av_sync"]["gate_pass"],
            serde_json::json!(false),
            "sanity: the av_sync gate must actually be failing in this fixture: {with_av}"
        );
        assert_eq!(
            with_av["overall_pass"],
            serde_json::json!(false),
            "#624/#312 PR B: a failing av_sync gate MUST force overall_pass=false, regardless of \
             whatever the loss/latency gates alone computed (without_av={without_av}): {with_av}"
        );
    }

    /// #624 deliverable 4 / #312 item 2 PR B — a PASSING av_sync gate (every one of the 6
    /// CAMERA_UNDER_TEST_NODES measured cleanly within +/-20ms of `--av-expected-ms`) must NOT
    /// change the run's overall verdict vs the identical run with no av_sync inputs at all --
    /// `all_pass &= true` is a no-op. Proves the "AND-in" wiring in BOTH directions together with
    /// the sibling gate-failure test above, without needing to know the loss/latency gates' own
    /// PASS/FAIL value for this synthetic fixture (both runs share the identical frames/schedule,
    /// so their loss/latency contribution is identical either way).
    #[test]
    fn all_cambox_av_sync_gate_pass_does_not_change_the_overall_verdict_312_624() {
        // 5 windows (cam1/cam3/cam4/cam5/cam6), 20 frames each = 100 frames, optical ticks
        // 1000..=1099 contiguous across ALL windows. emit_log covers the FULL 1000..=1099 range
        // so EVERY window (and cam2's whole-recording pool over all 100 frames) decodes a dense,
        // clean 500ms offset -- no Unknown camera anywhere.
        const N: usize = 100;
        let emit_log: Vec<(u8, u32, i64)> = (0..N).map(|k| (k as u8, 1000 + k as u32, 0)).collect();
        let audio_markers: Vec<(f64, u8)> = (0..N)
            .map(|k| (k as f64 / 30.0 - 0.5, k as u8)) // video_ts(1000+k) - 0.5s = k/30.0 - 0.5
            .collect();
        let av = AvMarkerInputs {
            fps: 30.0,
            video_start_s: 0.0,
            emit_log,
            audio_markers,
        };
        let cameras: &[(&str, u32, i64)] = &[
            ("CAM1", CAM1B, 800_000_000),
            ("CAM3", CAM3B, 820_000_000),
            ("CAM4", super::BURN_RUN_ID_CAM4, 790_000_000),
            ("CAM5", CAM5B, 805_000_000),
            ("CAM6", CAM6B, 815_000_000),
        ];

        // expected_ms=500.0 matches the constructed clean offset exactly -> every camera passes.
        let with_av = build_all_cambox_av_sync_fixture("gate-pass-with", cameras, Some(av), 500.0);
        let without_av =
            build_all_cambox_av_sync_fixture("gate-pass-without", cameras, None, 500.0);

        assert!(
            !with_av["all_cambox_av_sync"].is_null(),
            "sanity: the WITH-av_sync run must actually have reported the block: {with_av}"
        );
        for cam in ["cam1", "cam2", "cam3", "cam4", "cam5", "cam6"] {
            assert_eq!(
                with_av["all_cambox_av_sync"][cam]["verdict"],
                serde_json::json!("measured"),
                "sanity: {cam} must be cleanly measured in this fixture: {with_av}"
            );
            assert_eq!(
                with_av["all_cambox_av_sync"][cam]["gate_pass"],
                serde_json::json!(true),
                "sanity: {cam}'s clean 500ms offset must pass the gate vs expected_ms=500: {with_av}"
            );
        }
        assert_eq!(
            with_av["all_cambox_av_sync"]["gate_pass"],
            serde_json::json!(true),
            "sanity: the OVERALL av_sync gate must pass when every camera passes: {with_av}"
        );
        assert_eq!(
            with_av["overall_pass"], without_av["overall_pass"],
            "#624/#312 PR B: a PASSING av_sync gate must be a no-op on the overall verdict -- \
             with={with_av}, without={without_av}"
        );
    }

    // ---- #714 — per-camera A/V coverage: a sample-starved (Unknown) camera gets a DERIVED
    // estimate from cam2's own measured offset + this camera's #286 delivery-latency delta,
    // whenever a --strih recording (delivery latency) is ALSO supplied this run. ----

    /// Combines `build_all_cambox_av_sync_fixture`'s stream-recording construction (cam2's
    /// optical dual-QR + each window's camera burn, feeding the A/V candidate pooling) with
    /// `build_all_cambox_delivery_latency_fixture`'s strih-recording construction (each window's
    /// camera burn + strih's own render burn, feeding `all_cambox_delivery_latency`) over the
    /// SAME switch-schedule windows — so both blocks are populated from ONE consistent fixture,
    /// letting a camera that is Unknown in `all_cambox_av_sync` ALSO have a #286 delivery sample
    /// for `av_window::derive_camera_av_sync` to re-center against. `cameras` is `(cambox label,
    /// camera burn run_id, delivery latency ns for strih)`.
    fn build_all_cambox_av_sync_with_delivery_fixture(
        tag: &str,
        cameras: &[(&str, u32, i64)],
        av: Option<AvMarkerInputs>,
        av_expected_ms: f64,
    ) -> serde_json::Value {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;

        const ONE_S: i64 = 1_000_000_000;
        let base = 6_000 * ONE_S;
        let win = 5 * ONE_S;
        let dir = std::env::temp_dir().join(format!("cb-714-avdel-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sched_path = dir.join("switch-schedule.json");
        let windows_json: Vec<String> = cameras
            .iter()
            .enumerate()
            .map(|(wi, &(label, _, _))| {
                let start_ns = base + (wi as i64) * win;
                let end_ns = start_ns + win;
                format!(r#"{{"cambox":"{label}","start_ns":{start_ns},"end_ns":{end_ns}}}"#)
            })
            .collect();
        std::fs::write(&sched_path, format!("[{}]", windows_json.join(","))).unwrap();

        let mut stream_frames: Vec<RecordingFrame> = Vec::new();
        let mut strih_frames: Vec<RecordingFrame> = Vec::new();
        let mut idx = 0u64;
        for (wi, &(_, camera_burn, delivery_latency_ns)) in cameras.iter().enumerate() {
            let wstart = base + (wi as i64) * win;
            for j in 0..20i64 {
                let ts = wstart + (j + 1) * (ONE_S / 5);
                let optical = 1000u32 + idx as u32;
                stream_frames.push(RecordingFrame {
                    frame_index: idx,
                    payloads: vec![
                        Payload {
                            run_id: STRIH,
                            frame_id: 1670 + idx as u32,
                            gen_ts_ns: ts,
                        },
                        Payload {
                            run_id: CAM2,
                            frame_id: optical,
                            gen_ts_ns: ts,
                        },
                        Payload {
                            run_id: camera_burn,
                            frame_id: 5000 + idx as u32,
                            gen_ts_ns: ts,
                        },
                    ],
                    tick: Some(optical),
                });
                strih_frames.push(RecordingFrame {
                    frame_index: idx,
                    payloads: vec![
                        Payload {
                            run_id: camera_burn,
                            frame_id: 8000 + idx as u32,
                            gen_ts_ns: ts,
                        },
                        Payload {
                            run_id: STRIH,
                            frame_id: 1670 + idx as u32,
                            gen_ts_ns: ts + delivery_latency_ns,
                        },
                    ],
                    tick: None,
                });
                idx += 1;
            }
        }

        let args = super::Args::parse_from([
            "recording-verdict",
            "--switch-schedule",
            sched_path.to_str().unwrap(),
            "--switch-guard-ns",
            "0",
            "--switch-expected-step",
            "2",
            "--av-expected-ms",
            &av_expected_ms.to_string(),
        ]);

        let (v, _pass) = build_and_print_verdict(
            &args,
            Some(DecodedRec {
                frames: strih_frames,
                rec_path: None,
            }),
            Some(DecodedRec {
                frames: stream_frames,
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            None, // #461: no imag frames in this fixture
            av,
        )
        .expect("verdict");

        let _ = std::fs::remove_dir_all(&dir);
        v
    }

    /// HEADLINE (#714): cam1's window carries 10 real, dense A/V candidates → Measured. cam3's
    /// window carries ZERO A/V candidates (its ticks fall outside the emit-log range, mirroring
    /// the sibling #312 fixture) → Unknown on its own — but BOTH cameras (plus cam2) have a
    /// #286 delivery-latency sample this run, so cam3 gets a DERIVED estimate instead of a bare
    /// "unknown", satisfying the #714 acceptance bar ("a value or reasoned bound for EVERY
    /// camera"). cam4/cam5/cam6 never appear in the sweep at all (no delivery sample either) →
    /// stay genuinely "unknown" (never a fabricated derivation from zero evidence).
    #[test]
    fn all_cambox_av_sync_derives_a_per_camera_estimate_for_a_sample_starved_camera_714() {
        let emit_log: Vec<(u8, u32, i64)> = (0..10u8).map(|k| (k, 1000 + k as u32, 0)).collect();
        let audio_markers: Vec<(f64, u8)> = (0..10u8).map(|k| (k as f64 / 30.0 - 0.5, k)).collect();
        let av = AvMarkerInputs {
            fps: 30.0,
            video_start_s: 0.0,
            emit_log,
            audio_markers,
        };
        // cam1: delivery p50 = 800ms. cam3: delivery p50 = 840ms (40ms above cam1's) — mean of
        // the two = 820ms, so cam3's derived offset = cam2's own offset + (840 - 820) = +20ms.
        let v = build_all_cambox_av_sync_with_delivery_fixture(
            "derive",
            &[("CAM1", CAM1B, 800_000_000), ("CAM3", CAM3B, 840_000_000)],
            Some(av),
            0.0,
        );

        let av_sync = &v["all_cambox_av_sync"];
        assert_eq!(
            av_sync["cam1"]["verdict"],
            serde_json::json!("measured"),
            "sanity: cam1 must be a real measurement in this fixture: {av_sync}"
        );
        let cam2_offset = av_sync["cam2"]["av_offset_ms"]
            .as_f64()
            .expect("sanity: cam2 must be measured (whole-recording pool): {av_sync}");
        assert_eq!(
            av_sync["cam3"]["verdict"],
            serde_json::json!("derived"),
            "#714: cam3 is sample-starved on its own (0 candidates) but a delivery sample \
             exists ⇒ DERIVED, never a bare unknown: {av_sync}"
        );
        assert!(
            av_sync["cam3"]["av_offset_ms"].is_null(),
            "#714: a derived estimate must NOT be written into av_offset_ms (reserved for a \
             genuine independent measurement): {av_sync}"
        );
        let derived_offset = av_sync["cam3"]["derived_offset_ms"]
            .as_f64()
            .expect("#714: cam3 must carry derived_offset_ms: {av_sync}");
        assert!(
            (derived_offset - (cam2_offset + 20.0)).abs() < 1e-6,
            "#714: expected cam2_offset({cam2_offset}) + (840 - 820) = {}, got {derived_offset}: {av_sync}",
            cam2_offset + 20.0
        );
        assert_eq!(
            av_sync["cam3"]["derived_from_cam2_offset_ms"],
            serde_json::json!(cam2_offset),
            "#714: the derivation's own cam2 anchor must be reported for traceability: {av_sync}"
        );
        assert_eq!(
            av_sync["cam3"]["derived_delivery_spread_ms"],
            serde_json::json!(40.0),
            "#714: spread = max(840) - min(800) = 40ms: {av_sync}"
        );
        assert!(
            av_sync["cam3"]["derived_note"]
                .as_str()
                .unwrap()
                .contains("estimated"),
            "#714: the derivation must be self-labeled, never presented as a real measurement: {av_sync}"
        );
        for absent in ["cam4", "cam5", "cam6"] {
            assert_eq!(
                av_sync[absent]["verdict"],
                serde_json::json!("unknown"),
                "#714: {absent} never appeared in this sweep (no delivery sample either) -> \
                 genuinely unknown, never a fabricated derivation from zero evidence: {av_sync}"
            );
            assert!(
                av_sync[absent]["derived_offset_ms"].is_null(),
                "#714: {absent} must not carry a derived field when no derivation was possible: {av_sync}"
            );
        }
    }

    /// #714: the derived estimate's OWN gate can disagree with cam2's — a camera whose delivery
    /// p50 is far enough above the mean pushes the re-centered offset outside ±20ms even though
    /// cam2's own measured offset is safely inside it.
    #[test]
    fn all_cambox_av_sync_derived_gate_can_fail_even_when_cam2_itself_passes_714() {
        let emit_log: Vec<(u8, u32, i64)> = (0..10u8).map(|k| (k, 1000 + k as u32, 0)).collect();
        // Real markers land exactly at video_ts(1000+k) - 0.005s ⇒ cam1's (and cam2's own
        // whole-recording) measured offset is a clean +5ms — safely inside ±20ms of expected=0.
        let audio_markers: Vec<(f64, u8)> =
            (0..10u8).map(|k| (k as f64 / 30.0 - 0.005, k)).collect();
        let av = AvMarkerInputs {
            fps: 30.0,
            video_start_s: 0.0,
            emit_log,
            audio_markers,
        };
        // cam1: 800ms delivery. cam3: 870ms delivery (70ms above cam1's, mean=835ms) -> cam3's
        // derived offset = 5 + (870 - 835) = 5 + 35 = 40ms -> OUTSIDE +/-20ms even though cam2's
        // own measured +5ms offset comfortably passes.
        let v = build_all_cambox_av_sync_with_delivery_fixture(
            "derive-fail",
            &[("CAM1", CAM1B, 800_000_000), ("CAM3", CAM3B, 870_000_000)],
            Some(av),
            0.0,
        );

        let av_sync = &v["all_cambox_av_sync"];
        assert_eq!(
            av_sync["cam2"]["gate_pass"],
            serde_json::json!(true),
            "sanity: cam2's own +5ms measured offset must pass the gate: {av_sync}"
        );
        assert_eq!(
            av_sync["cam3"]["verdict"],
            serde_json::json!("derived"),
            "sanity: cam3 must reach the derived path: {av_sync}"
        );
        let derived_offset = av_sync["cam3"]["derived_offset_ms"].as_f64().unwrap();
        assert!(
            (derived_offset - 40.0).abs() < 1e-6,
            "expected 5.0 + (870 - 835) = 40.0, got {derived_offset}: {av_sync}"
        );
        assert_eq!(
            av_sync["cam3"]["gate_pass"],
            serde_json::json!(false),
            "#714: a re-centered offset outside +/-20ms must FAIL, independent of cam2's own \
             PASS: {av_sync}"
        );
        assert_eq!(
            v["overall_pass"],
            serde_json::json!(false),
            "#714: a failing DERIVED gate must force overall_pass=false, same severity as a \
             failing MEASURED gate: {v}"
        );
    }

    /// #583 — build a single-window `--switch-schedule` run over an imag recording whose optical
    /// tick follows `optical_ticks` (one recorded frame per tick, each carrying a CLEAN step-1
    /// digital corner burn — imag's own render, blind to any upstream content freeze in the filmed
    /// optical tick), then assert the WHOLE-RECORDING imag verdict
    /// (`full_chain.loss.imag.zero_loss`, = `node_verdict_for_imag`'s `is_zero()`) and the
    /// PER-SEGMENT sweep verdict (`all_cambox_continuity.imag.overall_pass`, the honest per-window
    /// gate) AGREE and equal `expect` on THIS sequence. This is the #583 PARITY lock: it proves the
    /// strict `window_segment` copy/gap false-fail (the #583 bug) is CLOSED for a benign beat and a
    /// copy/freeze — it does NOT claim the two paths compute identically on every input (they scan
    /// different windows: the whole optical span vs one ~30s schedule slice, so the #376 undecodable
    /// RATE denominator and the boundary trim — #575's 3-frame lead/tail trim on the headline vs the
    /// schedule's transition guard on the sweep — can legitimately disagree on which specific frames
    /// fail; `imag_tick_gate::ImagZeroLoss::is_zero_loss`'s doc has the full caveat). The single
    /// window here spans the whole recording so the two windows coincide for THIS test (the sequences
    /// keep clean edges so the whole-recording #575 trim is a no-op vs the per-segment no-trim).
    fn assert_imag_paths_agree(tag: &str, optical_ticks: &[u32], expect: bool) {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;

        const ONE_S: i64 = 1_000_000_000;
        let base = 2_000 * ONE_S;
        let win = 3_600 * ONE_S; // one window wide enough to hold every frame
        let dir = std::env::temp_dir().join(format!("cb-583-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sched_path = dir.join("switch-schedule.json");
        std::fs::write(
            &sched_path,
            format!(
                r#"[{{"cambox":"CAM1","start_ns":{a},"end_ns":{b}}}]"#,
                a = base,
                b = base + win,
            ),
        )
        .unwrap();

        // imag's OWN recording: one frame per optical tick, spaced 1s apart inside the single
        // window, each carrying imag's #463 digital corner burn as a CLEAN step-1 sequence.
        let imag_frames: Vec<RecordingFrame> = optical_ticks
            .iter()
            .enumerate()
            .map(|(i, &t)| {
                let gen_ts = base + (i as i64 + 1) * ONE_S;
                RecordingFrame {
                    frame_index: i as u64,
                    payloads: vec![Payload {
                        run_id: super::BURN_RUN_ID_IMAG,
                        frame_id: 5000 + i as u32,
                        gen_ts_ns: gen_ts,
                    }],
                    tick: Some(t),
                }
            })
            .collect();

        // A minimal clean stream recording — the imag block is nested under `Some(stream)`; the
        // stream sweep's own result is irrelevant to (and not asserted by) the imag parity check.
        let stream_frames: Vec<RecordingFrame> = (0..40u64)
            .map(|i| {
                let gen_ts = base + (i as i64 + 1) * ONE_S;
                let optical = 1000u32 + 2 * i as u32;
                RecordingFrame {
                    frame_index: i,
                    payloads: vec![
                        Payload {
                            run_id: STRIH,
                            frame_id: 1670 + i as u32,
                            gen_ts_ns: gen_ts,
                        },
                        Payload {
                            run_id: CAM2,
                            frame_id: optical,
                            gen_ts_ns: gen_ts,
                        },
                    ],
                    tick: Some(optical),
                }
            })
            .collect();

        let args = super::Args::parse_from([
            "recording-verdict",
            "--switch-schedule",
            sched_path.to_str().unwrap(),
            "--switch-guard-ns",
            "0",
            // Don't let the 300s headline span floor confound the `zero_loss` field (which is
            // is_zero() alone, NOT ANDed with span_ok) on this seconds-long synthetic recording.
            "--min-secs",
            "0",
        ]);

        let (v, _pass) = build_and_print_verdict(
            &args,
            None,
            Some(DecodedRec {
                frames: stream_frames,
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            Some(DecodedRec {
                frames: imag_frames,
                rec_path: None,
            }),
            None, // #312 item 2 (PR A): no carried A/V-sync inputs in this test
        )
        .expect("verdict");

        let whole = v["full_chain"]["loss"]["imag"]["zero_loss"]
            .as_bool()
            .unwrap_or_else(|| panic!("[{tag}] full_chain.loss.imag.zero_loss missing: {v}"));
        let per_segment = v["all_cambox_continuity"]["imag"]["overall_pass"]
            .as_bool()
            .unwrap_or_else(|| {
                panic!("[{tag}] all_cambox_continuity.imag.overall_pass missing: {v}")
            });
        assert_eq!(
            whole, per_segment,
            "#583 [{tag}] PARITY: the whole-recording imag verdict ({whole}) and the per-segment \
             sweep verdict ({per_segment}) must AGREE on the SAME sequence: {v}"
        );
        assert_eq!(
            whole, expect,
            "#583 [{tag}]: the imag verdict must be {expect} for this sequence: {v}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #583 — the CORE fix: imag's benign same-rate optical beat (an isolated duplicate tick + an
    /// isolated skip = zero net loss) is PASSED by the whole-recording gate and must now ALSO PASS
    /// the per-segment sweep — the strict `window_segment` (copy/gap) false-FAILED it per window.
    /// Both paths agree on PASS.
    #[test]
    fn imag_per_segment_benign_beat_matches_whole_recording_pass_583() {
        // 100..=109 clean, then an isolated DUP (110,110) and a SKIP (111 absent → 112), then
        // 113..=123 clean — the boundaries stay clean so the whole-recording #575 trim is a no-op.
        let mut ticks: Vec<u32> = (100..=109).collect();
        ticks.extend([110, 110, 112]);
        ticks.extend(113..=123);
        assert_imag_paths_agree("benign-beat", &ticks, true);
    }

    /// #583 — a content FREEZE (the optical tick STUCK for 6 consecutive frames — a Δ0 run beyond
    /// K) must FAIL BOTH paths: the whole-recording gate (run-length) and the per-segment sweep. The
    /// digital burn keeps advancing (blind to the upstream content freeze) and the whole-window
    /// aggregates stay near zero, so ONLY the run-length term catches it — in BOTH paths. Both agree
    /// on FAIL.
    #[test]
    fn imag_per_segment_copy_freeze_matches_whole_recording_fail_583() {
        // 100..=109 clean, then 110 repeated 6× (freeze), then 116..=123 (the skip compensates so
        // avg_step stays ≈ 1 — the aggregates can't see it; run-length must). Boundaries clean.
        let mut ticks: Vec<u32> = (100..=109).collect();
        ticks.extend([110, 110, 110, 110, 110, 110]);
        ticks.extend(116..=123);
        assert_imag_paths_agree("copy-freeze", &ticks, false);
    }

    /// #681 point 2 — live evidence (RUN_ID 1783727115) showed EVERY one of imag's 12 per-window
    /// segments FAIL with `optical_advancing=true`, `optical_no_stuck_copy=true` (an isolated
    /// `max_stuck_run<=2`), `burn_present_ok=true`, `burn_missing_ids=[]` — every field the JSON
    /// printed read CLEAN, yet `pass=false` on every single window, with NO visible reason. This
    /// looked exactly like "the per-window loop is reading one repeated aggregate" (the issue's
    /// own hypothesis) — DISPROVEN below (the OTHER printed fields genuinely differ per window in
    /// the live data), but a REAL bug was found: `is_live_no_copy()` ALSO gates on the #588/#604
    /// systematic-judder DENSITY terms (`no_stuck_density`/`no_localized_stuck_density`), which
    /// were computed correctly per-window but were NEVER surfaced in the printed JSON at all — a
    /// density-driven failure was therefore completely unexplainable from the report alone. This
    /// test proves the density terms are now surfaced, turning a "mystery" failure into an
    /// explained one.
    #[test]
    fn imag_per_segment_json_surfaces_the_density_terms_that_actually_gate_it_681() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;

        const ONE_S: i64 = 1_000_000_000;
        let base = 5_000 * ONE_S;
        let win = 3_600 * ONE_S;
        let dir = std::env::temp_dir().join(format!("cb-681-density-json-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sched_path = dir.join("switch-schedule.json");
        std::fs::write(
            &sched_path,
            format!(
                r#"[{{"cambox":"CAM1","start_ns":{base},"end_ns":{}}}]"#,
                base + win
            ),
        )
        .unwrap();

        // A systematic Δ0 duplication density (every OTHER sample repeats the previous tick — 50%
        // density, far above the 1% ceiling), but no long single freeze run (max run = 1) and
        // genuinely ADVANCING overall — exactly the #588 "catch-up judder" shape: clean on every
        // OTHER field, failing ONLY on density. >=301 ticks so the 300-pair floor doesn't defer it.
        let mut ticks: Vec<u32> = Vec::new();
        for t in (3000u32..).take(200) {
            ticks.push(t);
            ticks.push(t); // duplicate
        }
        let imag_frames: Vec<RecordingFrame> = ticks
            .iter()
            .enumerate()
            .map(|(i, &tick)| RecordingFrame {
                frame_index: i as u64,
                payloads: vec![Payload {
                    run_id: super::BURN_RUN_ID_IMAG,
                    frame_id: 5000 + i as u32,
                    gen_ts_ns: base + (i as i64 + 1) * (ONE_S / 500),
                }],
                tick: Some(tick),
            })
            .collect();
        let stream_frames: Vec<RecordingFrame> = (0..10u64)
            .map(|i| {
                let gen_ts = base + (i as i64 + 1) * ONE_S;
                RecordingFrame {
                    frame_index: i,
                    payloads: vec![Payload {
                        run_id: STRIH,
                        frame_id: 1670 + i as u32,
                        gen_ts_ns: gen_ts,
                    }],
                    tick: None,
                }
            })
            .collect();

        let args = super::Args::parse_from([
            "recording-verdict",
            "--switch-schedule",
            sched_path.to_str().unwrap(),
            "--switch-guard-ns",
            "0",
            "--min-secs",
            "0",
        ]);

        let (v, _pass) = build_and_print_verdict(
            &args,
            None,
            Some(DecodedRec {
                frames: stream_frames,
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            Some(DecodedRec {
                frames: imag_frames,
                rec_path: None,
            }),
            None,
        )
        .expect("verdict");

        let seg = &v["all_cambox_continuity"]["imag"]["segments"][0];
        assert_eq!(
            seg["pass"],
            serde_json::json!(false),
            "#681: the density-heavy sequence must FAIL: {seg}"
        );
        assert_eq!(
            seg["optical_no_stuck_copy"],
            serde_json::json!(true),
            "#681 sanity: this fixture's run-length term must read CLEAN (max run 1) — the \
             failure must come from density, not the run-length term also failing: {seg}"
        );
        let density = seg["optical_stuck_density"].as_f64().unwrap_or_else(|| {
            panic!("#681: optical_stuck_density must be surfaced in the per-window JSON: {seg}")
        });
        assert!(
            density > 0.4,
            "#681: the 50%-duplicate sequence's density must read as the real high value, got \
             {density}: {seg}"
        );
        assert_eq!(
            seg["optical_no_stuck_density"],
            serde_json::json!(false),
            "#681: the JSON must ALSO surface whether the density term itself passed, not just \
             its raw value, so a failing window's cause is never a mystery: {seg}"
        );
        let local_density = seg["optical_local_stuck_density"].as_f64().unwrap_or_else(|| {
            panic!("#681: optical_local_stuck_density must be surfaced in the per-window JSON: {seg}")
        });
        assert!(
            local_density > 0.4,
            "#681: the localized density must also read high for this uniform pattern, got \
             {local_density}: {seg}"
        );
        assert_eq!(
            seg["optical_no_localized_stuck_density"],
            serde_json::json!(false),
            "#681: the JSON must surface the localized-density pass/fail too: {seg}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #186/#208 BLOCKER: `--extract-partial` must IDENTIFY this box's pixel-proof frames so the
    /// on-box decode can write the PNGs and the merge's "SEE the frame" guarantee survives the
    /// per-box split. The strih box must flag its recording's UNDECODABLE frames (no readable QR).
    #[test]
    fn extract_partial_flags_undecodable_frames_for_pixel_proof() {
        use super::extract_partial_flagged_frames;
        use clap::Parser;
        let args = super::Args::parse_from(["recording-verdict"]);
        // strih-box frames: an INTERIOR frame (index 1) with NO readable QR (undecodable);
        // surrounded by decodable frames so it is body-interior (not lead-in/out trimmed). cam1
        // ids 5000,5001 are contiguous (no missing-burn slot) — so ONLY the undecodable shows.
        let frames = vec![
            frame(0, &[(CAM2, 100), (CAM1B, 5000), (STRIH, 1670)]),
            frame(1, &[]), // no QR at all → undecodable interior frame
            frame(2, &[(CAM2, 102), (CAM1B, 5001), (STRIH, 1676)]),
        ];
        let (flagged, undecodable) = extract_partial_flagged_frames("strih", &frames, &args);
        assert!(
            flagged.contains(&1),
            "#186: the undecodable frame (index 1) must be flagged for pixel-proof extraction \
             on-box; got flagged={flagged:?}"
        );
        assert!(
            undecodable.contains(&1),
            "#186: frame 1 must be in the undecodable set (so the sharp-but-flagged self-check \
             runs on it); got {undecodable:?}"
        );
    }

    /// #186/#208 BLOCKER: the stream box must ALSO flag the missing-burn slots for the nodes it
    /// backs (strih + stream), so a delivered frame whose burn QR is unreadable (a #186 anomaly)
    /// has its pixels extractable on-box, not silently lost in the per-box flow.
    #[test]
    fn extract_partial_flags_missing_burn_slots_for_owned_nodes() {
        use super::extract_partial_flagged_frames;
        use clap::Parser;
        let args = super::Args::parse_from(["recording-verdict"]);
        // stream-box frames: frame 1 is DELIVERED (carries cam2) but is MISSING its strih burn —
        // a BURN-UNREADABLE missing slot for the strih node (read from the stream recording). All
        // frames are decodable (cam2 present) so there is NO undecodable frame; the ONLY proof is
        // the strih missing-burn slot at frame index 1.
        let frames = vec![
            frame(0, &[(CAM2, 100), (STRIH, 1670), (STREAM, 9000)]),
            frame(1, &[(CAM2, 101), (STREAM, 9003)]), // delivered, NO strih burn → BurnUnreadable
            frame(2, &[(CAM2, 102), (STRIH, 1676), (STREAM, 9006)]),
        ];
        let (flagged, undecodable) = extract_partial_flagged_frames("stream", &frames, &args);
        assert!(
            flagged.contains(&1),
            "#186: the strih-node missing-burn slot (delivered frame 1 with no strih burn) must be \
             flagged for pixel-proof on the stream box; got flagged={flagged:?}"
        );
        assert!(
            undecodable.is_empty(),
            "#186: no frame is undecodable here (all carry cam2) — only the missing-burn slot is \
             flagged; got undecodable={undecodable:?}"
        );
    }

    /// #638: the strih box must ALSO flag the missing-burn slots for cam3 (or any of cam2/cam4/
    /// cam5/cam6) — not just cam1 — when cam3 is the camera actually under test riding through
    /// this box's recording. Before #638 `extract_partial_flagged_frames`'s "strih" arm only
    /// ever checked cam1's burn, so a cam3 run's missing-burn slot was silently dropped from
    /// pixel-proof extraction entirely.
    #[test]
    fn extract_partial_flags_missing_burn_slots_for_cam3_on_strih_box_638() {
        use super::extract_partial_flagged_frames;
        use clap::Parser;
        let args = super::Args::parse_from(["recording-verdict"]);
        // strih-box frames when CAM3 (not cam1) is the camera under test: frame 1 is DELIVERED
        // (carries cam2) but is MISSING its cam3 burn — a BURN-UNREADABLE missing slot for cam3,
        // read from this same strih recording. cam1's burn never appears anywhere (cam1 is not
        // deployed this run).
        let frames = vec![
            frame(0, &[(CAM2, 100), (CAM3B, 5000), (STRIH, 1670)]),
            frame(1, &[(CAM2, 101), (STRIH, 1673)]), // delivered, NO cam3 burn → BurnUnreadable
            frame(2, &[(CAM2, 102), (CAM3B, 5002), (STRIH, 1676)]),
        ];
        let (flagged, undecodable) = extract_partial_flagged_frames("strih", &frames, &args);
        assert!(
            flagged.contains(&1),
            "#638: the cam3 missing-burn slot (delivered frame 1 with no cam3 burn) must be \
             flagged for pixel-proof on the strih box — extract_partial_flagged_frames must \
             cover ALL CAMERA_UNDER_TEST_NODES, not just cam1; got flagged={flagged:?}"
        );
        assert!(
            undecodable.is_empty(),
            "#638: no frame is undecodable here (all carry cam2) — only the cam3 missing-burn \
             slot is flagged; got undecodable={undecodable:?}"
        );
    }

    /// #208: the pixel-proof dir is derived the SAME way on both sides (extract writes it, merge
    /// reads it) — `…/strih-partial-42.json` → `…/strih-partial-42-pixels` beside the JSON.
    #[test]
    fn partial_pixels_dir_is_a_sibling_named_after_the_partial() {
        use super::partial_pixels_dir;
        use std::path::Path;
        assert_eq!(
            partial_pixels_dir(Path::new("/tmp/run/strih-partial-42.json")),
            Path::new("/tmp/run/strih-partial-42-pixels"),
            "the pixel dir must be a sibling of the partial named <stem>-pixels"
        );
        // No parent → current dir.
        assert_eq!(
            partial_pixels_dir(Path::new("stream-partial.json")),
            Path::new("stream-partial-pixels")
        );
    }

    /// #208 box-to-box guard (review): `--merge-partials` must REJECT a partial assigned to the
    /// wrong slot — a `strih` partial fed as the `stream` input must ERROR (so a recording can
    /// never be cross-fed at the data level), and a partial whose box is neither strih nor stream
    /// must BAIL. Both must fail BEFORE the verdict runs (no exit, no misverdict).
    #[test]
    fn merge_rejects_box_mismatch_and_unknown_box() {
        use super::run_merge;
        use camera_box::probe::recording_partial::RecordingPartial;
        use clap::Parser;
        use std::path::PathBuf;

        let dir = tempfile::tempdir().unwrap();

        // A `strih` partial assigned to the `stream` slot → error (the partial's recorded box must
        // match its assignment; a strih recording can never be merged as the stream input).
        let strih_p = RecordingPartial::from_frames(
            "strih",
            &PathBuf::from("strih.mkv"),
            &[CAM1B, STRIH],
            vec![],
        );
        let strih_path = dir.path().join("strih-partial.json");
        strih_p.save(&strih_path).unwrap();
        let spec = format!("stream={}", strih_path.display());
        let args =
            super::Args::parse_from(["recording-verdict", "--merge-partials", spec.as_str()]);
        let err = run_merge(&args).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("box") && msg.contains("strih"),
            "#208: a strih partial in the stream slot must error (box mismatch): {msg}"
        );

        // A partial whose box is neither strih nor stream (key matches the recorded box, so the
        // mismatch guard passes) must BAIL with `unknown box`.
        let weird_p = RecordingPartial::from_frames("weird", &PathBuf::from("x.mkv"), &[], vec![]);
        let weird_path = dir.path().join("weird-partial.json");
        weird_p.save(&weird_path).unwrap();
        let spec2 = format!("weird={}", weird_path.display());
        let args2 =
            super::Args::parse_from(["recording-verdict", "--merge-partials", spec2.as_str()]);
        let err2 = run_merge(&args2).unwrap_err();
        assert!(
            format!("{err2:#}").contains("unknown box"),
            "#208: an unknown box must bail: {err2:#}"
        );
    }

    /// #208/#632 (review): the per-box expected burns are ONE source of truth shared by the
    /// merge consistency check — strih carries ALL SIX camera-under-test ids (#632: whichever
    /// ONE is actually deployed a given run, mutually exclusive) + strih; stream carries the
    /// same + stream; unknown → None. A mismatch between a partial's recorded expected_burns and
    /// this mapping is what run_merge warns on (a manual --burn-* mismatch between extract and
    /// merge that could misverdict). Note: the ACTUAL `--extract-partial` decode call uses the
    /// mandatory/any-of split (`analyze_recording_with_grouped_burns`) rather than this flat
    /// list directly — see `extract_partial`.
    #[test]
    fn args_expected_burns_for_maps_box_to_its_burns() {
        use super::args_expected_burns_for;
        use clap::Parser;
        let args = super::Args::parse_from(["recording-verdict"]);
        assert_eq!(
            args_expected_burns_for("strih", &args),
            Some(vec![
                CAM1B,
                CAM2B,
                CAM3B,
                super::BURN_RUN_ID_CAM4,
                CAM5B,
                CAM6B,
                STRIH
            ]),
            "strih partial carries every CAMERA_UNDER_TEST_NODES id (#632) + strih"
        );
        assert_eq!(
            args_expected_burns_for("stream", &args),
            Some(vec![
                CAM1B,
                CAM2B,
                CAM3B,
                super::BURN_RUN_ID_CAM4,
                CAM5B,
                CAM6B,
                STRIH,
                STREAM
            ]),
            "stream partial (chain endpoint) carries every CAMERA_UNDER_TEST_NODES id + strih + stream"
        );
        assert_eq!(
            args_expected_burns_for("nope", &args),
            None,
            "an unknown box has no expected burns"
        );
    }

    /// #632 gap 2: the cam2→SOURCE V4L2 capture-drop label defaults to "cam1" — both the common
    /// case (cam1 IS the camera under test) and the pre-#632 fallback (nothing decoded at all).
    #[test]
    fn resolve_camera_under_test_label_defaults_to_cam1() {
        use super::resolve_camera_under_test_label;
        assert_eq!(
            resolve_camera_under_test_label(true, false, false, false, false, false),
            "cam1",
            "cam1 present ⇒ cam1 (even if, hypothetically, another id were also present)"
        );
        assert_eq!(
            resolve_camera_under_test_label(false, false, false, false, false, false),
            "cam1",
            "nothing present ⇒ the pre-#632 default, unchanged"
        );
    }

    /// #632 gap 2: when cam1 is ABSENT but cam3 (or any other CAMERA_UNDER_TEST_NODES entry) is
    /// present, the label must resolve to THAT camera's own name — the exact bug #632 reports
    /// (the label used to say "cam1" unconditionally, even for a cam3/cam4 run).
    #[test]
    fn resolve_camera_under_test_label_resolves_to_the_deployed_camera() {
        use super::resolve_camera_under_test_label;
        assert_eq!(
            resolve_camera_under_test_label(false, false, true, false, false, false),
            "cam3",
            "#632: cam1 absent, cam3 present ⇒ label must be cam3, not the stale cam1"
        );
        assert_eq!(
            resolve_camera_under_test_label(false, false, false, true, false, false),
            "cam4",
            "#632: cam4 deployed ⇒ cam4"
        );
        assert_eq!(
            resolve_camera_under_test_label(false, true, false, false, false, false),
            "cam2",
            "#632: cam2 (the fixed painter, ALSO camera-under-test role per #312) ⇒ cam2"
        );
        assert_eq!(
            resolve_camera_under_test_label(false, false, false, false, true, false),
            "cam5",
            "#632: cam5 deployed ⇒ cam5"
        );
        assert_eq!(
            resolve_camera_under_test_label(false, false, false, false, false, true),
            "cam6",
            "#632: cam6 deployed ⇒ cam6"
        );
    }

    /// #187/#208: a NON-FATAL cam1 grab decode failure must be RECORDED in the report
    /// (`nodes.cam1.unavailable`), not silently dropped, while the stream-only hops still run.
    /// A decoded cam1 grab populates the cam1 diagnostic node.
    #[test]
    fn cam1_grab_decode_failure_is_recorded_not_silent() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;
        let args = super::Args::parse_from(["recording-verdict"]);

        // A failed cam1 grab surfaces nodes.cam1.unavailable + the reason (never silent).
        let (failed, _) = build_and_print_verdict(
            &args,
            None,
            Some(DecodedRec {
                frames: window(12, true, None),
                rec_path: None,
            }),
            Cam1Source::DecodeFailed("cam1 grab decode failed: boom".to_string()),
            None,
            None,
            None, // #461: no imag frames in this test
            None, // #312 item 2 (PR A): no carried A/V-sync inputs in this test
        )
        .expect("verdict with a failed cam1 grab");
        assert_eq!(
            failed["nodes"]["cam1"]["unavailable"],
            serde_json::json!(true),
            "a failed cam1 grab must record nodes.cam1.unavailable: {}",
            failed["nodes"]["cam1"]
        );
        assert!(failed["nodes"]["cam1"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("boom"));

        // A decoded cam1 grab populates the cam1 diagnostic node.
        let (decoded, _) = build_and_print_verdict(
            &args,
            None,
            Some(DecodedRec {
                frames: window(12, true, None),
                rec_path: None,
            }),
            Cam1Source::Decoded(DecodedRec {
                frames: window(12, false, None),
                rec_path: None,
            }),
            None,
            None,
            None, // #461: no imag frames in this test
            None, // #312 item 2 (PR A): no carried A/V-sync inputs in this test
        )
        .expect("verdict with a decoded cam1 grab");
        assert_eq!(
            decoded["nodes"]["cam1"]["diagnostic_only"],
            serde_json::json!(true)
        );
        assert!(decoded["nodes"]["cam1"]["frames"].as_u64().unwrap() > 0);
    }

    #[test]
    fn node_burn_id_on_reads_the_nodes_burn() {
        let f = frame(0, &[(CAM2, 500), (STRIH, 1670), (STREAM, 9000)]);
        assert_eq!(node_burn_id_on(&f, STRIH), Some(1670));
        assert_eq!(node_burn_id_on(&f, STREAM), Some(9000));
        // A frame with cam2 but no strih burn ⇒ None for strih.
        let g = frame(1, &[(CAM2, 501)]);
        assert_eq!(node_burn_id_on(&g, STRIH), None);
    }

    #[test]
    fn in_window_excludes_pre_and_post_signal_render_tick_ids() {
        // #198: the recording opens with PRE-SIGNAL frames carrying ONLY a free-running
        // strih render-tick burn (no cam2 optical QR — the painter isn't up yet), and closes
        // with POST-SIGNAL teardown frames the same way. Those frames are NOT delivered
        // (no cam2 QR), so the window trims them — their burn ids (1, 2 and 30000) can never
        // inflate the range. Only the delivered signal frames count.
        let stream = vec![
            frame(0, &[(STRIH, 1)]), // pre-signal render tick (no cam2) — trimmed
            frame(1, &[(STRIH, 2)]), // pre-signal — trimmed
            frame(2, &[(CAM2, 100), (STRIH, 1670)]), // first delivered (in-window)
            frame(3, &[(CAM2, 101), (STRIH, 1673)]),
            frame(4, &[(CAM2, 102), (STRIH, 1676)]), // last delivered (in-window)
            frame(5, &[(STRIH, 30000)]),             // post-signal teardown (no cam2) — trimmed
        ];
        let w = in_window_burn_frames(
            &stream,
            STRIH,
            &[STRIH, STREAM],
            BurnRate::PerRenderTick,
            None,
        );
        let ids: Vec<Option<u32>> = w.iter().map(|f| f.burn_id).collect();
        assert_eq!(
            ids,
            vec![Some(1670), Some(1673), Some(1676)],
            "only in-window delivered frames; pre/post render ticks excluded"
        );
        let idxs: Vec<u64> = w.iter().map(|f| f.frame_index).collect();
        assert_eq!(idxs, vec![2, 3, 4]);
    }

    #[test]
    fn in_window_clamps_post_emission_teardown_tail() {
        // #267: at teardown the node STOPS emitting its burn while cam2's optical painter keeps
        // running for a few more frames, so the strih recording captures DELIVERED (cam2-QR)
        // frames PAST the node's last emitted burn. The optical-anchored window used to extend
        // to the last cam2 frame, so those trailing burn-less frames became BURN-UNREADABLE
        // synthetic ids and blocked a clean 0-undecodable PASS — even though no frame was lost
        // (the node simply ended ~0.77s before the optical signal did). The fix CLAMPS the
        // window's trailing boundary to the last frame that carries THIS node's burn (its last
        // in-range id). cam1 (per-emit) is the real-world case; the clamp is rate-agnostic.
        let stream = vec![
            frame(0, &[(CAM2, 100), (CAM1B, 50)]),
            frame(1, &[(CAM2, 101), (CAM1B, 51)]),
            frame(2, &[(CAM2, 102), (CAM1B, 52)]), // last EMITTED cam1 burn
            frame(3, &[(CAM2, 103)]),              // teardown tail: cam2 only, cam1 stopped
            frame(4, &[(CAM2, 104)]),              // teardown tail
            frame(5, &[(CAM2, 105)]),              // teardown tail
        ];
        let w = in_window_burn_frames(
            &stream,
            CAM1B,
            &[CAM1B, STRIH, STREAM],
            BurnRate::PerEmittedFrame,
            None,
        );
        let ids: Vec<Option<u32>> = w.iter().map(|f| f.burn_id).collect();
        assert_eq!(
            ids,
            vec![Some(50), Some(51), Some(52)],
            "trailing teardown frames past the last emitted burn must be clamped off"
        );
        // The clamped window is fully contiguous ⇒ a clean PASS, no phantom burn_unreadable.
        let iw = camera_box::probe::burn_contiguity::burn_contiguity_in_window(
            "cam1",
            &w,
            BurnRate::PerEmittedFrame,
        );
        assert!(
            iw.contiguity.is_contiguous(),
            "no missing id after clamping the teardown tail: {:?}",
            iw.contiguity
        );
        assert_eq!(iw.contiguity.missing_ids.len(), 0);

        // Rate-agnostic: a per-render (strih/stream) teardown tail is clamped the same way.
        let stream_r = vec![
            frame(0, &[(CAM2, 100), (STRIH, 1670)]),
            frame(1, &[(CAM2, 101), (STRIH, 1673)]), // last strih render-tick burn
            frame(2, &[(CAM2, 102)]),                // teardown tail: cam2 only
            frame(3, &[(CAM2, 103)]),                // teardown tail
        ];
        let wr = in_window_burn_frames(
            &stream_r,
            STRIH,
            &[STRIH, STREAM],
            BurnRate::PerRenderTick,
            None,
        );
        let idsr: Vec<Option<u32>> = wr.iter().map(|f| f.burn_id).collect();
        assert_eq!(
            idsr,
            vec![Some(1670), Some(1673)],
            "per-render teardown tail clamped to the last emitted render-tick burn"
        );
    }

    #[test]
    fn clamp_keeps_interior_unreadable_and_strict_bar() {
        // The #267 clamp must NEVER weaken the strict #186 bar: an INTERIOR burn-less delivered
        // frame (one with a present burn AFTER it — proof the stream RESUMED, so it is a genuine
        // mid-stream readability miss, not a teardown end) is still counted as BURN-UNREADABLE
        // and still FAILS the node. Only the trailing post-emission tail is clamped.
        let stream = vec![
            frame(0, &[(CAM2, 100), (CAM1B, 50)]),
            frame(1, &[(CAM2, 101)]), // INTERIOR miss — burn resumes below
            frame(2, &[(CAM2, 102), (CAM1B, 52)]), // present again ⇒ frame 1 is a real in-range miss
            frame(3, &[(CAM2, 103)]),              // teardown tail (clamped, not counted)
        ];
        let w = in_window_burn_frames(
            &stream,
            CAM1B,
            &[CAM1B, STRIH, STREAM],
            BurnRate::PerEmittedFrame,
            None,
        );
        let ids: Vec<Option<u32>> = w.iter().map(|f| f.burn_id).collect();
        assert_eq!(
            ids,
            vec![Some(50), None, Some(52)],
            "interior miss kept, only the trailing tail clamped"
        );
        let iw = camera_box::probe::burn_contiguity::burn_contiguity_in_window(
            "cam1",
            &w,
            BurnRate::PerEmittedFrame,
        );
        assert!(
            !iw.contiguity.is_contiguous(),
            "an interior in-range miss must still FAIL the strict bar: {:?}",
            iw.contiguity
        );
    }

    #[test]
    fn long_trailing_burn_absent_tail_is_real_loss_not_clamped() {
        // #267 HARDENING — the teardown-tail clamp must be BOUNDED. A SHORT trailing burn-absent
        // run is a legit teardown overrun (cam2's painter outlives cam1 by ~0.77 s ≈ 23 frames at
        // shutdown); a LONG one is REAL end-of-stream loss — cam1 EMITTED those ids and they were
        // lost in transit right before shutdown. The recording is IDENTICAL for both (optical
        // present, cam1 burn absent), so the only sound rule is to bound the clamp: a tail longer
        // than the physically-plausible teardown window must NOT be silently clamped (that would
        // mask a real zero-loss failure — the user's HARD bar). It stays BURN-UNREADABLE and FAILS.
        let mut stream: Vec<RecordingFrame> = Vec::new();
        for i in 0..11u32 {
            stream.push(frame(i as u64, &[(CAM2, 100 + i), (CAM1B, 50 + i)])); // emitted, contiguous
        }
        for i in 0..100u32 {
            stream.push(frame((11 + i) as u64, &[(CAM2, 111 + i)])); // long cam2-only end-loss tail
        }
        let w = in_window_burn_frames(
            &stream,
            CAM1B,
            &[CAM1B, STRIH, STREAM],
            BurnRate::PerEmittedFrame,
            None,
        );
        let absent = w.iter().filter(|f| f.burn_id.is_none()).count();
        assert!(
            absent >= 50,
            "a long end-of-stream burn-absent run must NOT be clamped (real loss); kept {absent}"
        );
        let iw = camera_box::probe::burn_contiguity::burn_contiguity_in_window(
            "cam1",
            &w,
            BurnRate::PerEmittedFrame,
        );
        assert!(
            !iw.contiguity.is_contiguous(),
            "a long emitted-but-lost tail must FAIL the strict bar, not silently PASS: {:?}",
            iw.contiguity
        );
    }

    #[test]
    fn leading_lead_in_is_charged_not_clamped() {
        // #267 DEEP-REVIEW CORRECTION: the TRAILING teardown tail is clamped, but the LEADING
        // (lead-in) edge is NOT. The window is anchored to the FIRST optical (cam2) frame; if cam1
        // begins emitting its burn a few frames AFTER cam2's painter came up, those leading
        // optical-only frames carry no cam1 burn. The first #267 fix ALSO clamped that leading run
        // — but the lead-in case is UNOBSERVED on the rig, and clamping it MASKS a real ≤bound
        // START-of-stream loss (cam1 EMITTED those ids and they were lost in transit at startup)
        // into a false PASS, violating the user's HARD 0-gap bar. So a leading burn-absent run is
        // now KEPT and CHARGED as BURN-UNREADABLE → the node FAILS. A false-FAIL is SAFE; masking
        // start-of-stream loss is not. (The OLD test asserted the leading run was clamped/PASS —
        // exactly the masking this correction removes; it is the RED reproduction of that bug.)
        let mut stream: Vec<RecordingFrame> = Vec::new();
        for i in 0..5u32 {
            stream.push(frame(i as u64, &[(CAM2, 100 + i)])); // lead-in: cam2 up, cam1 not yet emitting
        }
        for i in 0..11u32 {
            stream.push(frame((5 + i) as u64, &[(CAM2, 105 + i), (CAM1B, 50 + i)]));
        }
        let w = in_window_burn_frames(
            &stream,
            CAM1B,
            &[CAM1B, STRIH, STREAM],
            BurnRate::PerEmittedFrame,
            None,
        );
        let ids: Vec<Option<u32>> = w.iter().map(|f| f.burn_id).collect();
        assert_eq!(
            ids.first(),
            Some(&None),
            "leading lead-in must be KEPT (never clamped) so a real start-of-stream loss can never be masked: {ids:?}"
        );
        let iw = camera_box::probe::burn_contiguity::burn_contiguity_in_window(
            "cam1",
            &w,
            BurnRate::PerEmittedFrame,
        );
        assert!(
            !iw.contiguity.is_contiguous(),
            "a leading burn-absent run must be CHARGED (BURN-UNREADABLE → FAIL), never clamped: {:?}",
            iw.contiguity
        );
    }

    #[test]
    fn long_leading_burn_absent_lead_in_is_real_loss_not_clamped() {
        // The LEADING edge is never clamped at all (no leading bound) — short OR long, a leading
        // burn-absent run stays CHARGED → FAILS. This pins the long case explicitly: cam1 burns
        // lost at start-of-stream are REAL loss and must FAIL.
        let mut stream: Vec<RecordingFrame> = Vec::new();
        for i in 0..100u32 {
            stream.push(frame(i as u64, &[(CAM2, 100 + i)])); // 100 optical-only leading frames
        }
        for i in 0..11u32 {
            stream.push(frame((100 + i) as u64, &[(CAM2, 200 + i), (CAM1B, 50 + i)]));
        }
        let w = in_window_burn_frames(
            &stream,
            CAM1B,
            &[CAM1B, STRIH, STREAM],
            BurnRate::PerEmittedFrame,
            None,
        );
        let absent = w.iter().filter(|f| f.burn_id.is_none()).count();
        assert!(
            absent >= 50,
            "a long start-of-stream burn-absent run must NOT be clamped (real loss); kept {absent}"
        );
        let iw = camera_box::probe::burn_contiguity::burn_contiguity_in_window(
            "cam1",
            &w,
            BurnRate::PerEmittedFrame,
        );
        assert!(
            !iw.contiguity.is_contiguous(),
            "a long leading loss must FAIL: {:?}",
            iw.contiguity
        );
    }

    // ========================================================================
    // #273 — the optical-delivered check must honor the pinned `--cam2-run-id`.
    // A FOREIGN (previous-run) cam2 paint in the lead-in must NOT count as
    // current-run delivery: it is pre-signal residue and must be TRIMMED off the
    // leading edge, never charged as cam1 BURN-UNREADABLE. The trim removes ONLY
    // foreign residue — a real current-run start-of-stream loss (current paint,
    // no burn) and a real interior loss STILL FAIL (no masking).
    // ========================================================================

    #[test]
    fn frame_is_delivered_optical_honors_the_cam2_pin() {
        // A frame carrying ONLY the previous run's paint (CAM2_FOREIGN) + a node burn.
        let foreign = frame(0, &[(CAM2_FOREIGN, 9000), (STRIH, 43)]);
        // A frame carrying THIS run's paint (CAM2_PIN) + the cam1 burn.
        let current = frame(1, &[(CAM2_PIN, 100), (CAM1B, 50)]);
        let burns = [CAM1B, STRIH, STREAM];

        // PINNED: only the current-run paint counts as delivered; the foreign paint does NOT.
        assert!(
            !frame_is_delivered_optical(&foreign, &burns, Some(CAM2_PIN)),
            "a FOREIGN (previous-run) cam2 paint must NOT count as current-run delivery (#273)"
        );
        assert!(
            frame_is_delivered_optical(&current, &burns, Some(CAM2_PIN)),
            "THIS run's cam2 paint IS current-run delivery"
        );
        // UNPINNED (back-compat): any non-burn payload is cam2 — both frames count as delivered.
        assert!(
            frame_is_delivered_optical(&foreign, &burns, None),
            "unpinned: any non-burn payload counts as cam2 (pre-#273 behaviour)"
        );
        assert!(frame_is_delivered_optical(&current, &burns, None));
    }

    #[test]
    fn frame_is_delivered_optical_pin_equal_to_a_burn_id_fails_closed() {
        // Defense-in-depth: if --cam2-run-id is misconfigured to a node burn run_id, a burn-only
        // frame must NOT be read as current-run optical — otherwise the cam1 in-window membership
        // (is_optical || has_node_burn) collapses to "has the burn" and a delivered-but-burn-absent
        // loss would be MASKED. With pin == a burn id, no frame is optical ⇒ empty window ⇒
        // first_id None ⇒ NOT contiguous ⇒ the node FAILS closed (never a vacuous pass).
        let burns = [CAM1B, STRIH, STREAM];
        let cam1_burn_only = frame(0, &[(CAM1B, 50)]);
        assert!(
            !frame_is_delivered_optical(&cam1_burn_only, &burns, Some(CAM1B)),
            "a burn-only frame must NOT count as optical even when the pin is (mis)set to that burn id"
        );
        let stream: Vec<RecordingFrame> = (0..4u32)
            .map(|i| frame(i as u64, &[(CAM1B, 50 + i)]))
            .collect();
        let w = in_window_burn_frames(
            &stream,
            CAM1B,
            &burns,
            BurnRate::PerEmittedFrame,
            Some(CAM1B),
        );
        assert!(
            w.is_empty(),
            "pin==burn ⇒ no optical frame ⇒ empty window: {w:?}"
        );
        let iw = camera_box::probe::burn_contiguity::burn_contiguity_in_window(
            "cam1",
            &w,
            BurnRate::PerEmittedFrame,
        );
        assert!(
            !iw.contiguity.is_contiguous(),
            "an empty window must FAIL (first_id None), never vacuously pass: {:?}",
            iw.contiguity
        );
    }

    #[test]
    fn pinned_foreign_run_lead_in_is_trimmed_to_a_clean_pass() {
        // THE #273 REPRODUCTION (run 2706001): the strih recording's lead-in carried the PREVIOUS
        // run's residual cam2 paint (CAM2_FOREIGN) + the strih burn, but NO current-run paint and
        // NO cam1 burn (cam1 had not started emitting into THIS run yet). The old optical check
        // counted that foreign paint as "delivered", anchoring the window at frame 0 so the
        // cam1-absent lead-in was charged as BURN-UNREADABLE → a false zero-loss FAIL. With the pin
        // honored, only CAM2_PIN frames define the window: the foreign lead-in is trimmed, leaving a
        // contiguous cam1 run ⇒ a clean PASS.
        let mut stream: Vec<RecordingFrame> = Vec::new();
        // 3 foreign-residue lead-in frames: previous run's paint + strih burn, NO cam1, NO pin.
        for i in 0..3u32 {
            stream.push(frame(
                i as u64,
                &[(CAM2_FOREIGN, 9000 + i), (STRIH, 40 + i)],
            ));
        }
        // The steady-state current-run span: this run's paint + a contiguous cam1 burn run.
        for i in 0..5u32 {
            stream.push(frame(
                (3 + i) as u64,
                &[(CAM2_PIN, 100 + i), (CAM1B, 50 + i)],
            ));
        }
        let w = in_window_burn_frames(
            &stream,
            CAM1B,
            &[CAM1B, STRIH, STREAM],
            BurnRate::PerEmittedFrame,
            Some(CAM2_PIN),
        );
        let ids: Vec<Option<u32>> = w.iter().map(|f| f.burn_id).collect();
        assert_eq!(
            ids,
            vec![Some(50), Some(51), Some(52), Some(53), Some(54)],
            "the foreign-run lead-in must be TRIMMED — only the current-run steady span remains: {ids:?}"
        );
        let iw = camera_box::probe::burn_contiguity::burn_contiguity_in_window(
            "cam1",
            &w,
            BurnRate::PerEmittedFrame,
        );
        assert!(
            iw.contiguity.is_contiguous(),
            "after trimming the foreign-run lead-in the cam1 run is contiguous ⇒ ZERO loss (#273): {:?}",
            iw.contiguity
        );
        assert!(iw.contiguity.missing_ids.is_empty());
    }

    #[test]
    fn pinned_real_interior_loss_in_steady_span_still_fails() {
        // NO-MASKING: the pin trims only foreign residue — a REAL interior DROP inside the
        // current-run steady span (an emitted cam1 id GENUINELY absent, ids jumping 51→53 with a
        // present burn on every recorded frame, so no `None` slot consumes it) is neither leading
        // nor trailing, so it is KEPT and still FAILS the strict #186 bar as a REAL DROP. The
        // #273 trim can never hide a real drop.
        let stream = vec![
            // foreign residue lead-in (trimmed by the pin)
            frame(0, &[(CAM2_FOREIGN, 9000), (STRIH, 40)]),
            // current-run steady span: cam1 ids 50,51,53,54 — id 52 is GENUINELY absent (a real
            // interior drop), every frame carries a burn so nothing is charged as burn-unreadable.
            frame(1, &[(CAM2_PIN, 100), (CAM1B, 50)]),
            frame(2, &[(CAM2_PIN, 101), (CAM1B, 51)]),
            frame(3, &[(CAM2_PIN, 102), (CAM1B, 53)]),
            frame(4, &[(CAM2_PIN, 103), (CAM1B, 54)]),
        ];
        let w = in_window_burn_frames(
            &stream,
            CAM1B,
            &[CAM1B, STRIH, STREAM],
            BurnRate::PerEmittedFrame,
            Some(CAM2_PIN),
        );
        let ids: Vec<Option<u32>> = w.iter().map(|f| f.burn_id).collect();
        assert_eq!(
            ids,
            vec![Some(50), Some(51), Some(53), Some(54)],
            "foreign lead-in trimmed, but the steady-span ids (with the real interior gap) are KEPT: {ids:?}"
        );
        let iw = camera_box::probe::burn_contiguity::burn_contiguity_in_window(
            "cam1",
            &w,
            BurnRate::PerEmittedFrame,
        );
        assert!(
            !iw.contiguity.is_contiguous(),
            "a real interior loss must still FAIL — the #273 trim never masks real loss: {:?}",
            iw.contiguity
        );
        assert_eq!(
            iw.contiguity.missing_ids,
            vec![52],
            "the genuinely-absent interior id 52 is reported as the loss"
        );
        assert_eq!(
            iw.missing_slots[0].kind,
            InWindowMissingKind::RealDrop,
            "an absent interior id (no None slot consuming it) is a REAL DROP, not burn-unreadable"
        );
    }

    #[test]
    fn pinned_real_leading_current_run_loss_still_fails() {
        // NO-MASKING at the LEADING edge: a frame carrying THIS run's paint (CAM2_PIN) but no cam1
        // burn is a CURRENT-run delivered frame — a possible real start-of-stream loss (cam1
        // emitted those ids and they were lost in transit at startup). The pin trims only FOREIGN
        // residue; it must NOT trim a current-run-paint lead-in. So this leading run stays CHARGED
        // (BURN-UNREADABLE) and the node FAILS — exactly the #267 leading-edge guarantee, preserved.
        let mut stream: Vec<RecordingFrame> = Vec::new();
        // leading CURRENT-run paint with NO cam1 burn — a real start-of-stream loss, NOT foreign.
        for i in 0..4u32 {
            stream.push(frame(i as u64, &[(CAM2_PIN, 100 + i)]));
        }
        for i in 0..5u32 {
            stream.push(frame(
                (4 + i) as u64,
                &[(CAM2_PIN, 104 + i), (CAM1B, 50 + i)],
            ));
        }
        let w = in_window_burn_frames(
            &stream,
            CAM1B,
            &[CAM1B, STRIH, STREAM],
            BurnRate::PerEmittedFrame,
            Some(CAM2_PIN),
        );
        let ids: Vec<Option<u32>> = w.iter().map(|f| f.burn_id).collect();
        assert_eq!(
            ids.first(),
            Some(&None),
            "a leading CURRENT-run-paint frame with no burn must be KEPT (charged), never trimmed as stabilization: {ids:?}"
        );
        let iw = camera_box::probe::burn_contiguity::burn_contiguity_in_window(
            "cam1",
            &w,
            BurnRate::PerEmittedFrame,
        );
        assert!(
            !iw.contiguity.is_contiguous(),
            "a real current-run start-of-stream loss must FAIL even with the pin (no masking): {:?}",
            iw.contiguity
        );
    }

    #[test]
    fn node_render_step_is_gap_ignore_for_strih_and_stream_360() {
        // #360: strih's burn is a FREE-RUNNING render tick with an IRREGULAR step (run 354003:
        // 0–10, mean ~4), NOT the clean 60/30=2 the old code assumed — its forward gaps are
        // render-clock jitter, not loss. strih/stream stay on gap-ignore (step 1) regardless of
        // the fps inputs (irrelevant to them, see node_render_step's doc).
        assert_eq!(super::node_render_step("strih", 60.0, 30.0, 60.0, 30.0), 1);
        assert_eq!(super::node_render_step("stream", 60.0, 30.0, 60.0, 30.0), 1);
        assert_eq!(super::node_render_step("strih", 60.0, 0.0, 60.0, 30.0), 1);
    }

    #[test]
    fn node_render_step_is_decimation_aware_for_camera_under_test_nodes_571() {
        // #571: cam1/cam3/cam4's forwarded capture burn is read from the strih recording, which
        // (post-#459/#460 Topology v2) records its own cut-to-stream canvas at `capture_fps` (30
        // on the rig) while the camera keeps emitting at `refresh_hz` (60) — a clean 2:1
        // decimation, DERIVED from the CLI rates (never a hardcoded "2"), via the SAME
        // `painted_tick_step` formula #467 already uses for the stream/imag recordings.
        assert_eq!(super::node_render_step("cam1", 60.0, 30.0, 60.0, 30.0), 2);
        assert_eq!(super::node_render_step("cam3", 60.0, 30.0, 60.0, 30.0), 2);
        assert_eq!(super::node_render_step("cam4", 60.0, 30.0, 60.0, 30.0), 2);
        // A genuinely non-decimated rate (e.g. capture_fps == refresh_hz) must NOT be forced to 2.
        assert_eq!(super::node_render_step("cam1", 60.0, 30.0, 60.0, 60.0), 1);
    }

    #[test]
    fn node_verdict_strih_decimation_step2_clean_is_zero_loss() {
        // A clean every-other-id strih sequence (ids step by 2) is ZERO loss. Post-#360 the strih
        // node uses gap-ignore (node_render_step→1), so forward gaps of 2 are ignored — still
        // ZERO loss, no false positive. (rec_path is never read: a zero-loss verdict has no slot.)
        let stream: Vec<RecordingFrame> = (0..6)
            .map(|i| frame(i, &[(CAM2, 100 + i as u32), (STRIH, 2000 + (i as u32) * 2)]))
            .collect();
        let tmp = tempfile::tempdir().unwrap();
        let v = node_verdict(
            &super::NodeSpec {
                node: "strih",
                burn_run_id: STRIH,
                rate: BurnRate::PerRenderTick,
                source: &stream,
                rec_path: Some(std::path::Path::new("/nonexistent.mp4")),
                cam2_run_id: None,
                step: super::node_render_step("strih", 60.0, 30.0, 60.0, 30.0), // = 1 (gap-ignore)
            },
            &[STRIH, STREAM],
            tmp.path(),
            0,
        )
        .unwrap();
        assert!(
            v.is_zero(),
            "clean 60→30 decimation (ids step by 2) is ZERO loss: {:?}",
            v.contiguity
        );
    }

    #[test]
    fn node_verdict_cam1_decimation_step2_clean_is_zero_loss_571() {
        // #571: the same clean every-other-id sequence, but for cam1 (PerEmittedFrame) — the
        // Topology-v2 cam(60fps)->strih(30fps) hop. Before this fix cam1 was step=1 (gap-ignore
        // was irrelevant to it; the set-based scan charged every decimated-away id as a real
        // drop). node_render_step now derives step=2 for cam1, and the verdict must be ZERO loss.
        let stream: Vec<RecordingFrame> = (0..6)
            .map(|i| frame(i, &[(CAM2, 100 + i as u32), (CAM1B, 2000 + (i as u32) * 2)]))
            .collect();
        let tmp = tempfile::tempdir().unwrap();
        let v = node_verdict(
            &super::NodeSpec {
                node: "cam1",
                burn_run_id: CAM1B,
                rate: BurnRate::PerEmittedFrame,
                source: &stream,
                rec_path: Some(std::path::Path::new("/nonexistent.mp4")),
                cam2_run_id: None,
                step: super::node_render_step("cam1", 60.0, 30.0, 60.0, 30.0), // = 2
            },
            &[CAM1B, STRIH, STREAM],
            tmp.path(),
            0,
        )
        .unwrap();
        assert!(
            v.is_zero(),
            "clean 60->30 decimation on cam1 (ids step by 2) is ZERO loss (#571): {:?}",
            v.contiguity
        );
    }

    // ---- #706 — ALL-CAMBOX switch-schedule scoping (the fused-sweep BURN-UNREADABLE bug) ----

    /// Like [`frame`], but each payload also carries an explicit `gen_ts_ns` — needed to place a
    /// frame on a `--switch-schedule` timeline (#706). `frame()` hard-codes `gen_ts_ns: 1` for
    /// every payload, which every OTHER test in this file is fine with since none of them place
    /// frames on a schedule timeline.
    fn frame_at(frame_index: u64, payloads: &[(u32, u32, i64)]) -> RecordingFrame {
        let payloads: Vec<Payload> = payloads
            .iter()
            .map(|&(run_id, frame_id, gen_ts_ns)| Payload {
                run_id,
                frame_id,
                gen_ts_ns,
            })
            .collect();
        let tick = payloads.iter().map(|p| p.frame_id).max();
        RecordingFrame {
            frame_index,
            payloads,
            tick,
        }
    }

    /// #706 regression (the pure scoping function) — a `CAMERA_UNDER_TEST_NODES` node's
    /// in-window delivered-frame set must be restricted to ONLY its own switch-schedule program
    /// window(s), never the whole recording. Two schedule windows: `[0,1000)` is cam1's OWN
    /// program time, `[1000,2000)` is a DIFFERENT cambox's (CAM3). cam1's burn is present on the
    /// first pair of frames (its own window) and absent on the second pair (CAM3's window — cam1
    /// physically cannot appear there, by design, not loss). Pre-#706 (no scoping) BOTH pairs
    /// would count toward cam1's window, manufacturing 2 phantom BURN-UNREADABLE frames.
    #[test]
    fn scope_camera_window_excludes_other_cambox_program_time_706() {
        let schedule = vec![
            super::SwitchWindow {
                cambox: "CAM1".to_string(),
                start_ns: 0,
                end_ns: 1000,
            },
            super::SwitchWindow {
                cambox: "CAM3".to_string(),
                start_ns: 1000,
                end_ns: 2000,
            },
        ];
        let source: Vec<RecordingFrame> = vec![
            frame_at(0, &[(STRIH, 1, 100)]),
            frame_at(1, &[(STRIH, 2, 200)]),
            frame_at(2, &[(STRIH, 3, 1100)]),
            frame_at(3, &[(STRIH, 4, 1200)]),
        ];
        let window = vec![
            super::RecordedBurnFrame {
                frame_index: 0,
                burn_id: Some(5000),
            },
            super::RecordedBurnFrame {
                frame_index: 1,
                burn_id: Some(5001),
            },
            super::RecordedBurnFrame {
                frame_index: 2,
                burn_id: None,
            },
            super::RecordedBurnFrame {
                frame_index: 3,
                burn_id: None,
            },
        ];
        let scope = super::ScheduleScope {
            schedule: &schedule,
            anchor_run_ids: &[STRIH],
            guard_ns: 0,
        };
        let scoped = scope_camera_window_to_own_schedule(
            window,
            "cam1",
            &source,
            &[STRIH],
            None,
            Some(scope),
        );
        assert_eq!(
            scoped,
            vec![
                super::RecordedBurnFrame {
                    frame_index: 0,
                    burn_id: Some(5000),
                },
                super::RecordedBurnFrame {
                    frame_index: 1,
                    burn_id: Some(5001),
                },
            ],
            "#706: cam1's window must be scoped to ONLY its own schedule window (frames 0/1) — \
             cam3's program-time frames (2/3, where cam1 CANNOT carry a burn by design) must be \
             excluded, not counted as cam1's phantom BURN-UNREADABLE"
        );
    }

    /// #706 — `None` schedule scope must leave the window UNCHANGED (the pre-#706
    /// single-camera-continuously-on-program behavior every existing fixture relies on).
    #[test]
    fn scope_camera_window_none_scope_is_a_no_op_706() {
        let window = vec![
            super::RecordedBurnFrame {
                frame_index: 0,
                burn_id: Some(1),
            },
            super::RecordedBurnFrame {
                frame_index: 1,
                burn_id: None,
            },
        ];
        let source: Vec<RecordingFrame> = Vec::new();
        let scoped =
            scope_camera_window_to_own_schedule(window.clone(), "cam1", &source, &[], None, None);
        assert_eq!(
            scoped, window,
            "#706: no --switch-schedule ⇒ window must pass through unchanged"
        );
    }

    /// #706 — a non-`CAMERA_UNDER_TEST_NODES` node (strih/stream) must never be schedule-scoped,
    /// even when a schedule IS supplied — those nodes are recorded continuously regardless of
    /// which cambox is on program.
    #[test]
    fn scope_camera_window_never_scopes_non_camera_under_test_nodes_706() {
        let schedule = vec![super::SwitchWindow {
            cambox: "CAM1".to_string(),
            start_ns: 0,
            end_ns: 1,
        }];
        let window = vec![super::RecordedBurnFrame {
            frame_index: 0,
            burn_id: Some(1),
        }];
        // gen_ts 500 falls OUTSIDE the only window — if this node were (wrongly) scoped, it
        // would be filtered out; strih/stream must come back UNCHANGED regardless.
        let source: Vec<RecordingFrame> = vec![frame_at(0, &[(STRIH, 1, 500)])];
        let scope = super::ScheduleScope {
            schedule: &schedule,
            anchor_run_ids: &[STRIH],
            guard_ns: 0,
        };
        let scoped = scope_camera_window_to_own_schedule(
            window.clone(),
            "strih",
            &source,
            &[STRIH],
            None,
            Some(scope),
        );
        assert_eq!(
            scoped, window,
            "#706: strih/stream nodes must never be schedule-scoped"
        );
    }

    /// #706 END-TO-END — `node_verdict_with_optical` with a switch-schedule scope must report
    /// `is_zero()==true` for a camera-under-test node that is delivery-clean WITHIN its own
    /// program window, even though the SAME recording carries a long stretch where a DIFFERENT
    /// cambox is on program (and this node's burn is legitimately absent there). Pre-#706 this
    /// synthetic recording would have reported the off-program stretch as BURN-UNREADABLE and
    /// FAILED a genuinely zero-loss camera — reproducing the LIVE gate finding (#706: ~7000-8500
    /// phantom BURN-UNREADABLE per camera, 0 REAL DROP, 0 genuine chain loss).
    #[test]
    fn node_verdict_with_optical_all_cambox_scope_ignores_other_cambox_program_time_706() {
        let schedule = vec![
            super::SwitchWindow {
                cambox: "CAM1".to_string(),
                start_ns: 0,
                end_ns: 1000,
            },
            super::SwitchWindow {
                cambox: "CAM3".to_string(),
                start_ns: 1000,
                end_ns: 2000,
            },
        ];
        let mut stream: Vec<RecordingFrame> = Vec::new();
        // Window 0 (cam1's own): 3 clean delivered frames, cam1 burn present + step-2 decimated.
        for i in 0..3u32 {
            stream.push(frame_at(
                i as u64,
                &[
                    (CAM2, 100 + i, 100 + i as i64),
                    (CAM1B, 2000 + i * 2, 100 + i as i64),
                ],
            ));
        }
        // Window 1 (a DIFFERENT cambox's own): 3 delivered frames, NO cam1 burn at all — cam1
        // physically cannot appear while another cambox is on program.
        for i in 0..3u32 {
            stream.push(frame_at(3 + i as u64, &[(CAM2, 200 + i, 1100 + i as i64)]));
        }
        let optical = optical_span_facts(&stream, &[CAM1B, STRIH, STREAM], None);
        let scope = super::ScheduleScope {
            schedule: &schedule,
            anchor_run_ids: &[STRIH, STREAM],
            guard_ns: 0,
        };
        let v = node_verdict_with_optical(
            &super::NodeSpec {
                node: "cam1",
                burn_run_id: CAM1B,
                rate: BurnRate::PerEmittedFrame,
                source: &stream,
                rec_path: None,
                cam2_run_id: None,
                step: super::node_render_step("cam1", 60.0, 30.0, 60.0, 30.0), // = 2
            },
            &[CAM1B, STRIH, STREAM],
            optical,
            std::path::Path::new("/tmp"),
            0,
            Some(scope),
        )
        .unwrap();
        assert!(
            v.is_zero(),
            "#706: cam1's OWN schedule window is delivery-clean; a different cambox's program \
             time (where cam1 legitimately never appears) must NOT be counted as cam1's phantom \
             BURN-UNREADABLE — got {:?}",
            v.contiguity
        );
        assert_eq!(v.burn_unreadable(), 0, "#706: no phantom BURN-UNREADABLE");
        assert_eq!(v.real_drops(), 0, "#706: no phantom REAL DROP either");
    }

    /// #708 END-TO-END — the LIVE incident reproduced through the FULL `node_verdict_with_optical`
    /// pipeline for the "strih" node specifically (not just the pure kernel tested directly in
    /// `burn_contiguity.rs`). strih's window stays genuinely UNSCOPED here
    /// (`scope_camera_window_to_own_schedule` is a documented no-op for strih/stream, #706 — this
    /// is a SEPARATE, additional mechanism on top of it), but the id sequence still carries a
    /// backward jump landing EXACTLY at a confirmed `--switch-schedule` window boundary — the
    /// EXACT shape of the live #708 finding (cam5's own free-running 911002 counter range
    /// `66709..=67840` overlapped cam6's `66934..=68067`, both proven present in the real
    /// recording, both downstream at stream — genuinely never lost). Without the fix this
    /// would report a phantom `real_drop` at frame_index 2; with it, ZERO loss.
    #[test]
    fn node_verdict_with_optical_strih_backward_jump_at_confirmed_window_boundary_is_zero_loss_708()
    {
        let schedule = vec![
            super::SwitchWindow {
                cambox: "CAM4".to_string(),
                start_ns: 0,
                end_ns: 1000,
            },
            super::SwitchWindow {
                cambox: "CAM2".to_string(),
                start_ns: 1000,
                end_ns: 2000,
            },
        ];
        let stream: Vec<RecordingFrame> = vec![
            frame_at(0, &[(CAM2, 900, 100), (STRIH, 100, 100)]),
            frame_at(1, &[(CAM2, 901, 200), (STRIH, 103, 200)]),
            // Window boundary at t=1000: a DIFFERENT source's OWN independent 911002 filter
            // instance cuts onto program — its free-running counter value (101) is LOWER than
            // the previous window's tail (103). Pre-#708 this reads as a backward-jump fault.
            frame_at(2, &[(CAM2, 902, 1100), (STRIH, 101, 1100)]),
            frame_at(3, &[(CAM2, 903, 1200), (STRIH, 106, 1200)]),
        ];
        let optical = optical_span_facts(&stream, &[STRIH, STREAM], None);
        let scope = super::ScheduleScope {
            schedule: &schedule,
            anchor_run_ids: &[STRIH, STREAM],
            guard_ns: 0,
        };
        let v = node_verdict_with_optical(
            &super::NodeSpec {
                node: "strih",
                burn_run_id: STRIH,
                rate: BurnRate::PerRenderTick,
                source: &stream,
                rec_path: None,
                cam2_run_id: None,
                step: super::node_render_step("strih", 60.0, 30.0, 60.0, 30.0),
            },
            &[STRIH, STREAM],
            optical,
            std::path::Path::new("/tmp"),
            0,
            Some(scope),
        )
        .unwrap();
        assert!(
            v.is_zero(),
            "#708: a backward jump landing exactly at a CONFIRMED window boundary must not be a \
             phantom drop — got {:?}",
            v.contiguity
        );
        assert_eq!(v.real_drops(), 0, "#708: no phantom REAL DROP");
    }

    #[test]
    fn node_verdict_cam1_decimation_step2_forward_gap_not_charged_and_none_fails_571() {
        // #571 (final model) at the node_verdict wiring level. On the DECIMATED cam(60fps)->
        // strih(30fps) hop (node_render_step = 2 with the rig fps) a forward gap of ANY size is
        // by-design decimation — the run-554307 beat (delta-1 bursts + ~7 jumps, 0 Nones, 0
        // backward jumps, strih's OWN burn fully contiguous) proves NO gap-size math can separate
        // decimation from loss — so forward gaps are NOT charged at all. The no-mask proof on
        // this hop is the NONE case: a DELIVERED frame (cam2 optical present) carrying NO
        // readable cam1 burn is charged BURN-UNREADABLE and still FAILS.
        // (a) forward gap 2004 -> 2008: NOT charged, zero loss.
        let gap_frames = vec![
            frame(0, &[(CAM2, 100), (CAM1B, 2000)]),
            frame(1, &[(CAM2, 101), (CAM1B, 2002)]),
            frame(2, &[(CAM2, 102), (CAM1B, 2004)]),
            frame(3, &[(CAM2, 103), (CAM1B, 2008)]), // gap: by-design decimation, not loss
        ];
        let w = in_window_burn_frames(
            &gap_frames,
            CAM1B,
            &[CAM1B, STRIH, STREAM],
            BurnRate::PerEmittedFrame,
            None,
        );
        let iw = camera_box::probe::burn_contiguity::burn_contiguity_in_window_with_step(
            "cam1",
            &w,
            BurnRate::PerEmittedFrame,
            super::node_render_step("cam1", 60.0, 30.0, 60.0, 30.0), // = 2 (decimated)
        );
        assert!(
            iw.contiguity.is_contiguous(),
            "a forward gap on the decimated hop is decimation, never charged (#571): {:?}",
            iw.contiguity
        );
        assert!(
            iw.missing_slots.is_empty(),
            "no missing slot of any kind: {:?}",
            iw.missing_slots
        );

        // (b) a DELIVERED frame with NO cam1 burn (the decimated hop's genuine-loss signal):
        // exactly one BURN-UNREADABLE, still fails.
        let none_frames = vec![
            frame(0, &[(CAM2, 100), (CAM1B, 2000)]),
            frame(1, &[(CAM2, 101)]), // delivered (optical present), cam1 burn unreadable
            frame(2, &[(CAM2, 102), (CAM1B, 2004)]),
        ];
        let w2 = in_window_burn_frames(
            &none_frames,
            CAM1B,
            &[CAM1B, STRIH, STREAM],
            BurnRate::PerEmittedFrame,
            None,
        );
        let iw2 = camera_box::probe::burn_contiguity::burn_contiguity_in_window_with_step(
            "cam1",
            &w2,
            BurnRate::PerEmittedFrame,
            super::node_render_step("cam1", 60.0, 30.0, 60.0, 30.0), // = 2 (decimated)
        );
        assert!(
            !iw2.contiguity.is_contiguous(),
            "a delivered frame missing its burn still FAILS on the decimated hop (#571): {:?}",
            iw2.contiguity
        );
        assert_eq!(iw2.missing_slots.len(), 1, "exactly the one None fault");
        assert_eq!(
            iw2.missing_slots[0].kind,
            InWindowMissingKind::BurnUnreadable,
            "charged BURN-UNREADABLE, never a phantom REAL DROP"
        );
    }

    #[test]
    fn node_verdict_render_tick_skips_are_zero_loss_not_thousands_missing() {
        // THE #198 REGRESSION at the binary level: strih burn id jumps by 3 per emitted
        // frame (per-render counter). The OLD integer-range check reported ~2x these as
        // "missing"; the in-window check reports ZERO (every delivered frame carries the
        // burn). Range 1670..1685 (16 wide) over 6 emitted frames must NOT yield ~10 missing.
        let stream: Vec<RecordingFrame> = (0..6)
            .map(|i| frame(i, &[(CAM2, 100 + i as u32), (STRIH, 1670 + (i as u32) * 3)]))
            .collect();
        let tmp = tempfile::tempdir().unwrap();
        let v = node_verdict(
            &super::NodeSpec {
                node: "strih",
                burn_run_id: STRIH,
                rate: BurnRate::PerRenderTick,
                source: &stream,
                // rec_path only touched when there ARE missing slots to extract pixels for;
                // a zero-loss verdict never reads it.
                rec_path: Some(std::path::Path::new("/nonexistent.mp4")),
                cam2_run_id: None,
                step: 1,
            },
            &[STRIH, STREAM],
            tmp.path(),
            5,
        )
        .unwrap();
        assert!(
            v.is_zero(),
            "per-render-tick forward skips are NOT loss: {:?}",
            v.contiguity
        );
        assert_eq!(v.contiguity.present_count, 6);
        assert_eq!(
            v.contiguity.expected_count, 6,
            "6 emitted frames, not the 16-wide span"
        );
        assert_eq!(v.real_drops(), 0);
        assert_eq!(v.burn_unreadable(), 0);
    }

    #[test]
    fn node_verdict_fails_when_cam2_optical_mostly_undecodable_363() {
        // #363 (reverts the #360 fraud). Run 354003: the filmed cam2 optical dual-QR went ~87%
        // undecodable while the DIGITAL strih burn was present on 100% of frames. #360 routed the
        // verdict AROUND the optical read — windowing on `is_optical || has_node_burn` so the
        // burn-present-but-optically-undecodable frames counted as delivered, and the run PASSED on
        // the digital burns alone. That is the fraud: a digital burn is injected at the node's
        // render tick (AFTER capture) — it proves node→node DIGITAL delivery, NOT that the real
        // camera captured the pixel path. The cam2 OPTICAL dual-QR read is the ONLY measurement of
        // the real camera path, and it is the HARD gate: any in-span frame whose optical QR did not
        // decode is an OPTICAL-UNDECODABLE hard-fail (NOT a phantom chain drop, NOT a pass).
        //
        // Build 71 frames: the strih digital burn is present on EVERY frame (a free-running render
        // tick, +4 step), the cam2 optical paint decodes only every 7th frame (11 optical, 60
        // optically-undecodable). Frame 0 and frame 70 are both optical so the optical-anchored
        // boundary spans all 71. The 60 undecodable frames MUST fail the verdict.
        let n = 71u32;
        let stream: Vec<RecordingFrame> = (0..n)
            .map(|i| {
                let mut ps: Vec<(u32, u32)> = Vec::new();
                if i % 7 == 0 {
                    ps.push((CAM2, 100 + i)); // cam2 optical decoded only ~1-in-7 frames
                }
                ps.push((STRIH, 2000 + i * 4)); // strih digital burn on EVERY frame
                frame(i as u64, &ps)
            })
            .collect();
        let tmp = tempfile::tempdir().unwrap();
        let v = node_verdict(
            &super::NodeSpec {
                node: "strih",
                burn_run_id: STRIH,
                rate: BurnRate::PerRenderTick,
                source: &stream,
                rec_path: None,
                cam2_run_id: None,
                step: super::node_render_step("strih", 60.0, 30.0, 60.0, 30.0),
            },
            &[CAM1B, STRIH, STREAM],
            tmp.path(),
            0,
        )
        .unwrap();
        // The 60 frames with no cam2 optical QR are OPTICAL-UNDECODABLE — the real camera path is
        // unproven on each. (i%7==0 over 0..71 ⇒ 11 optical, 60 undecodable.)
        assert_eq!(
            v.optical_undecodable, 60,
            "the 60 in-span frames with no cam2 optical QR must register as OPTICAL-UNDECODABLE: {:?}",
            v.contiguity
        );
        // HARD GATE: an 87%-optically-undecodable run FAILS even with 100% digital burns present.
        assert!(
            !v.is_zero(),
            "optical undecodability must FAIL the verdict, never pass on the digital burns (#363): {:?}",
            v.contiguity
        );
        // The strih burn over the OPTICAL-ONLY window (11 frames) is contiguous (per-render forward
        // gaps ignored) — so the failure is the OPTICAL read, NOT a manufactured digital drop.
        assert_eq!(
            v.contiguity.expected_count, 11,
            "the burn window is the 11 optically-delivered frames (optical is the gate, #363): {:?}",
            v.contiguity
        );
        assert_eq!(
            v.real_drops(),
            0,
            "no phantom real drops — the failure is optical, not digital"
        );
        assert_eq!(
            v.burn_unreadable(),
            0,
            "the optical-only burn window is contiguous; no burn-unreadable slots"
        );
    }

    #[test]
    fn node_verdict_cam1_per_emit_gap_is_a_real_drop() {
        // cam1 routed with BurnRate::PerEmittedFrame: a forward integer gap (52 absent) on
        // delivered frames IS a real cam1 drop — the regression the review caught. The verdict
        // must FAIL and classify the gap as REAL DROP, not silently pass.
        let stream = vec![
            frame(0, &[(CAM2, 100), (CAM1B, 50)]),
            frame(1, &[(CAM2, 101), (CAM1B, 51)]),
            frame(2, &[(CAM2, 102), (CAM1B, 53)]), // 52 missing = real cam1 drop
            frame(3, &[(CAM2, 103), (CAM1B, 54)]),
        ];
        let tmp = tempfile::tempdir().unwrap();
        let v = node_verdict(
            &super::NodeSpec {
                node: "cam1",
                burn_run_id: CAM1B,
                rate: BurnRate::PerEmittedFrame,
                source: &stream,
                rec_path: Some(std::path::Path::new("/nonexistent.mp4")),
                cam2_run_id: None,
                step: 1,
            },
            &[CAM1B, STRIH, STREAM],
            tmp.path(),
            0, // cap 0 = no cap, but rec_path IS read since there's a missing slot...
        );
        // ...so extraction will error on the bogus path. We only assert the contiguity by
        // calling the pure check directly here (the node_verdict pixel extraction needs a real
        // file, exercised by the real-data validation run). Confirm the verdict errored on the
        // bad path ONLY because a real drop was found (not a silent zero-loss pass).
        let w = in_window_burn_frames(
            &stream,
            CAM1B,
            &[CAM1B, STRIH, STREAM],
            BurnRate::PerEmittedFrame,
            None,
        );
        let iw = camera_box::probe::burn_contiguity::burn_contiguity_in_window(
            "cam1",
            &w,
            BurnRate::PerEmittedFrame,
        );
        assert!(
            !iw.contiguity.is_contiguous(),
            "cam1 gap is loss: {:?}",
            iw.contiguity
        );
        assert_eq!(iw.contiguity.missing_ids, vec![52]);
        assert_eq!(iw.missing_slots[0].kind, InWindowMissingKind::RealDrop);
        // node_verdict itself tries to extract the pixel proof for the real drop → errors on
        // the nonexistent path (proving it did NOT short-circuit to a zero-loss pass).
        assert!(
            v.is_err(),
            "a found real drop drives pixel extraction (errors on bad path)"
        );
    }

    // ========================================================================
    // #133 — cam1→strih contiguity is read from the CLEAN 1080p STRIH recording,
    // NOT the downstream STREAM recording where the small cam1 burn is softened by
    // the extra NDI hop + HEVC re-encode (both boxes record 1080p; #196's "4K
    // upscale" premise is invalid). One QR mis-read there → a `None` burn-unreadable.
    // ========================================================================

    #[test]
    fn cam1_contiguity_reads_from_strih_recording_not_softened_stream() {
        // The cam1 burn (per-EMITTED-frame) rides through NDI into BOTH recordings.
        // In the CLEAN 1080p strih recording every cam1 burn decodes (contiguous run
        // 50,51,52,53) ⇒ ZERO cam1 loss. In the downstream STREAM recording the same cam1
        // burn is softened (an extra NDI hop + HEVC re-encode, NOT a 4K upscale), so one
        // frame's cam1 QR fails to decode entirely (a `None` slot = BURN-UNREADABLE). The
        // cam1 verdict MUST come from the strih slice and report ZERO. This is the #133 fix:
        // the source of truth for cam1→strih is the crisp 1080p strih recording. (Distinct
        // from the #216 over-count, which was a reordered-but-PRESENT id — fixed in the walk.)
        let strih = vec![
            frame(0, &[(CAM2, 100), (CAM1B, 50)]),
            frame(1, &[(CAM2, 101), (CAM1B, 51)]),
            frame(2, &[(CAM2, 102), (CAM1B, 52)]), // crisp at 1080p — burn decodes
            frame(3, &[(CAM2, 103), (CAM1B, 53)]),
        ];
        let stream = vec![
            frame(0, &[(CAM2, 100), (CAM1B, 50)]),
            frame(1, &[(CAM2, 101), (CAM1B, 51)]),
            frame(2, &[(CAM2, 102)]), // softened downstream: cam1 QR entirely missed (None)
            frame(3, &[(CAM2, 103), (CAM1B, 53)]),
        ];
        // Sanity: the STREAM slice on its own DOES flag frame 2 as a defect — frame 2 is an
        // optical-delivered frame whose cam1 burn did not decode at all (BURN-UNREADABLE, id 52
        // genuinely absent from {50,51,53}), so it is non-contiguous. (This is the softened
        // source the #133 strih-source fix avoids — NOT the #216 reordered-but-present case.)
        let sw = in_window_burn_frames(
            &stream,
            CAM1B,
            &[CAM1B, STRIH, STREAM],
            BurnRate::PerEmittedFrame,
            None,
        );
        let siw = camera_box::probe::burn_contiguity::burn_contiguity_in_window(
            "cam1",
            &sw,
            BurnRate::PerEmittedFrame,
        );
        assert!(
            !siw.contiguity.is_contiguous(),
            "the softened stream slice flags frame 2 (cam1 burn unreadable — the #133 source): {:?}",
            siw.contiguity
        );

        // The FIX: cam1's node_verdict reads its burn from the STRIH slice (its own
        // `source`), so it is contiguous ⇒ ZERO loss, no phantom. A zero-loss verdict
        // never touches the pixel-proof path, so the recording path is unused here.
        let tmp = tempfile::tempdir().unwrap();
        let nv = node_verdict(
            &super::NodeSpec {
                node: "cam1",
                burn_run_id: CAM1B,
                rate: BurnRate::PerEmittedFrame,
                source: &strih, // <-- #133: cam1 source-of-truth = the strih 1080p recording
                rec_path: Some(std::path::Path::new("/nonexistent-strih.mkv")),
                cam2_run_id: None,
                step: 1,
            },
            &[CAM1B, STRIH, STREAM],
            tmp.path(),
            5,
        )
        .unwrap();
        assert!(
            nv.is_zero(),
            "cam1 from the CLEAN strih recording is contiguous ⇒ ZERO loss (no phantom): {:?}",
            nv.contiguity
        );
        assert_eq!(
            nv.real_drops(),
            0,
            "no phantom real drop — cam1 read from the clean 1080p strih recording"
        );
        assert_eq!(nv.burn_unreadable(), 0);
        assert_eq!(nv.contiguity.present_count, 4);
        assert_eq!(nv.contiguity.expected_count, 4);
    }

    // ========================================================================
    // #204 — a frame whose cam2 OPTICAL QR blurred (undecodable) but whose cam1
    // BURN decoded must NOT be excluded from cam1's per-emit window (its burn
    // proves cam1's emitted frame arrived). The old optical-delivered-only filter
    // orphaned that cam1 id and manufactured a PHANTOM forward-gap REAL DROP.
    // ========================================================================

    #[test]
    fn cam1_burn_on_an_optical_blurred_frame_is_not_a_phantom_drop() {
        // The real run-136141133 shape: frame 1 carries the cam1 burn (id 51) but its cam2
        // optical dual-QR was motion-blurred on that 60→30 straddle → undecodable, so the
        // frame has ONLY the cam1 burn. The cam1 sequence across the recording is
        // contiguous 50,51,52,53 — NOTHING is lost. The old filter dropped frame 1 (no cam2
        // QR) from the window, leaving 50→52 as "consecutive delivered" → a phantom drop of
        // 51. With the #204 fix (cam1 in-window membership = optical QR OR cam1 burn) the
        // burn-carrying frame is kept and the cam1 id run stays contiguous — NO phantom drop.
        // #363: the OVERALL verdict now nonetheless FAILS, because that frame's cam2 OPTICAL QR
        // was undecodable (one OPTICAL-UNDECODABLE hard-fail) — but as an optical failure, never a
        // manufactured cam1 drop. cam1 burn-delivery contiguity (no phantom) and the optical hard
        // gate are SEPARATE: this test pins that the #204 anti-phantom guarantee survives #363.
        let strih = vec![
            frame(0, &[(CAM2, 100), (CAM1B, 50)]),
            frame(1, &[(CAM1B, 51)]), // cam2 optical QR blurred → only the cam1 burn decoded
            frame(2, &[(CAM2, 102), (CAM1B, 52)]),
            frame(3, &[(CAM2, 103), (CAM1B, 53)]),
        ];
        let w = in_window_burn_frames(
            &strih,
            CAM1B,
            &[CAM1B, STRIH, STREAM],
            BurnRate::PerEmittedFrame,
            None,
        );
        // The blurred-optical frame's cam1 burn (51) MUST be in the window.
        let ids: Vec<Option<u32>> = w.iter().map(|f| f.burn_id).collect();
        assert_eq!(
            ids,
            vec![Some(50), Some(51), Some(52), Some(53)],
            "the cam1 burn on the optical-blurred frame must be kept in-window (#204): {ids:?}"
        );
        let iw = camera_box::probe::burn_contiguity::burn_contiguity_in_window(
            "cam1",
            &w,
            BurnRate::PerEmittedFrame,
        );
        assert!(
            iw.contiguity.is_contiguous(),
            "cam1 50,51,52,53 is contiguous ⇒ ZERO loss, NOT a phantom drop (#204): {:?}",
            iw.contiguity
        );
        assert!(
            iw.contiguity.missing_ids.is_empty(),
            "no phantom missing id: {:?}",
            iw.contiguity.missing_ids
        );

        // #363: the full node_verdict — cam1's burn delivery is still proven (id 51 kept ⇒ NO
        // phantom RealDrop, the #204 guarantee), but the run now FAILS because that frame's cam2
        // OPTICAL QR did not decode: one OPTICAL-UNDECODABLE hard-fail. The failure is the optical
        // read, never a manufactured cam1 drop. (No missing burn slot ⇒ pixel path untouched.)
        let tmp = tempfile::tempdir().unwrap();
        let nv = node_verdict(
            &super::NodeSpec {
                node: "cam1",
                burn_run_id: CAM1B,
                rate: BurnRate::PerEmittedFrame,
                source: &strih,
                rec_path: Some(std::path::Path::new("/nonexistent.mkv")),
                cam2_run_id: None,
                step: 1,
            },
            &[CAM1B, STRIH, STREAM],
            tmp.path(),
            5,
        )
        .unwrap();
        assert_eq!(
            nv.real_drops(),
            0,
            "no phantom cam1 drop — the cam1 burn proves delivery (#204 preserved): {:?}",
            nv.contiguity
        );
        assert_eq!(
            nv.optical_undecodable, 1,
            "the optical-blurred frame is one OPTICAL-UNDECODABLE hard-fail (#363): {:?}",
            nv.contiguity
        );
        assert!(
            !nv.is_zero(),
            "the run FAILS on the restored optical hard gate (#363), with NO phantom drop: {:?}",
            nv.contiguity
        );
    }

    #[test]
    fn strih_burn_on_a_non_optical_frame_inside_span_is_excluded_and_undecodable_363() {
        // #363 (reverts #360): a strih burn on a NON-OPTICAL frame inside the optical span is NO
        // LONGER counted as a delivered frame. strih/stream (PerRenderTick) get NO digital-burn
        // fallback — the cam2 OPTICAL read is the gate. Frame 1 here has only the strih render burn
        // (its cam2 optical QR did not decode); it is EXCLUDED from the strih burn window and
        // instead registers as one OPTICAL-UNDECODABLE hard-fail. Lead-in/out is still trimmed by
        // the optical-anchored BOUNDARIES (frames 0 and 2 are the optical span ends).
        let stream = vec![
            frame(0, &[(CAM2, 100), (STRIH, 1670)]),
            frame(1, &[(STRIH, 1673)]), // non-optical frame INSIDE the span — excluded, undecodable
            frame(2, &[(CAM2, 102), (STRIH, 1676)]),
        ];
        let w = in_window_burn_frames(
            &stream,
            STRIH,
            &[STRIH, STREAM],
            BurnRate::PerRenderTick,
            None,
        );
        let ids: Vec<Option<u32>> = w.iter().map(|f| f.burn_id).collect();
        assert_eq!(
            ids,
            vec![Some(1670), Some(1676)],
            "a non-optical frame inside the span is NOT a delivered strih frame (#363): {ids:?}"
        );
        // It is the distinct OPTICAL-UNDECODABLE failure (the real camera path is unproven there).
        assert_eq!(
            optical_span_facts(&stream, &[STRIH, STREAM], None).undecodable_in_span,
            1,
            "the in-span non-optical frame is one OPTICAL-UNDECODABLE hard-fail (#363)"
        );
    }

    #[test]
    fn node_verdict_optical_undecodable_is_a_hard_fail_363() {
        // #363 — the minimal hard-gate lock. The cam2 OPTICAL dual-QR read is the HARD gate: an
        // in-span frame whose optical QR did NOT decode is an OPTICAL-UNDECODABLE hard-fail, even
        // when a software-injected digital burn is present on it. A digital-burn-only frame proves
        // node→node DIGITAL delivery, NOT that the real camera captured the pixel path (the burn is
        // added AFTER capture). #360 let such a frame pass on its burn; #363 reverts that.
        let stream = vec![
            frame(0, &[(CAM2, 100), (STRIH, 1670)]),
            frame(1, &[(STRIH, 1673)]), // optical QR did NOT decode — only the digital strih burn
            frame(2, &[(CAM2, 102), (STRIH, 1676)]),
        ];
        let tmp = tempfile::tempdir().unwrap();
        let v = node_verdict(
            &super::NodeSpec {
                node: "strih",
                burn_run_id: STRIH,
                rate: BurnRate::PerRenderTick,
                source: &stream,
                rec_path: None,
                cam2_run_id: None,
                step: super::node_render_step("strih", 60.0, 30.0, 60.0, 30.0),
            },
            &[CAM1B, STRIH, STREAM],
            tmp.path(),
            0,
        )
        .unwrap();
        assert_eq!(
            v.optical_undecodable, 1,
            "frame 1's missing cam2 optical QR is one OPTICAL-UNDECODABLE hard-fail: {:?}",
            v.contiguity
        );
        assert!(
            !v.is_zero(),
            "an optically-undecodable in-span frame must FAIL, never pass on the digital burn (#363): {:?}",
            v.contiguity
        );
        assert_eq!(
            v.real_drops(),
            0,
            "the failure is the OPTICAL read — NOT a phantom digital drop: {:?}",
            v.contiguity
        );
    }

    #[test]
    fn node_verdict_colour_fail_is_a_hard_fail_364() {
        // #364 — the per-camera COLOUR gate is a HARD gate, mirroring optical_undecodable. A node
        // can deliver every frame WITH a complete optical read (zero loss on delivery), yet a colour
        // failure on the painted reference (grayscale / hue-shift / cast) FAILS it: a complete
        // delivery can NEVER substitute for correct colour.
        let stream: Vec<RecordingFrame> = (0..6)
            .map(|i| frame(i, &[(CAM2, 100 + i as u32), (STRIH, 2000 + (i as u32) * 2)]))
            .collect();
        let tmp = tempfile::tempdir().unwrap();
        let mut v = node_verdict(
            &super::NodeSpec {
                node: "strih",
                burn_run_id: STRIH,
                rate: BurnRate::PerRenderTick,
                source: &stream,
                rec_path: None,
                cam2_run_id: None,
                step: super::node_render_step("strih", 60.0, 30.0, 60.0, 30.0),
            },
            &[STRIH, STREAM],
            tmp.path(),
            0,
        )
        .unwrap();
        // Delivery + optical are clean ⇒ ZERO loss BEFORE the colour gate, and colour ungated ⇒ 0
        // (so existing delivery-only runs are unaffected).
        assert!(
            v.is_zero(),
            "clean delivery is zero loss before colour: {:?}",
            v.contiguity
        );
        assert_eq!(
            v.colour_fail, 0,
            "colour ungated ⇒ 0 (delivery-only runs unaffected)"
        );
        // A colour failure (2 reference patches wrong on a majority of frames) makes it NOT zero,
        // with perfect delivery — and it is NOT charged as a digital drop.
        v.colour_fail = 2;
        assert!(
            !v.is_zero(),
            "a per-camera colour failure must FAIL the node even with perfect delivery (#364)"
        );
        assert_eq!(
            v.real_drops(),
            0,
            "the failure is COLOUR — not a digital drop"
        );
    }

    /// #376 — build a synthetic optical-window recording of `total` frames where frame 0 and frame
    /// `total-1` are ALWAYS optical (so the optical span is exactly [0, total-1], `span_frames ==
    /// total`), every interior frame index in `undecodable` carries NO payload at all (excluded from
    /// the burn window, counted as OPTICAL-UNDECODABLE), and every other frame carries BOTH the cam2
    /// optical paint and a strictly-increasing strih burn id (so the burn-id contiguity is trivially
    /// clean under the PerRenderTick gap-ignore rule — see `node_verdict_colour_fail_is_a_hard_fail_364`
    /// for the same increasing-id-with-gaps pattern). This isolates the [`OPTICAL_UNDECODABLE_RATE_MAX`]
    /// gate: contiguity is always TRUE here, so `is_zero()` differences come ONLY from the optical rate.
    fn optical_run_with_undecodable(
        total: usize,
        undecodable: &HashSet<usize>,
    ) -> Vec<RecordingFrame> {
        (0..total)
            .map(|i| {
                if i != 0 && i != total - 1 && undecodable.contains(&i) {
                    frame(i as u64, &[])
                } else {
                    frame(
                        i as u64,
                        &[(CAM2, 100 + i as u32), (STRIH, 2000 + i as u32)],
                    )
                }
            })
            .collect()
    }

    fn node_verdict_for_optical_run(stream: &[RecordingFrame]) -> super::NodeVerdict {
        let tmp = tempfile::tempdir().unwrap();
        node_verdict(
            &super::NodeSpec {
                node: "strih",
                burn_run_id: STRIH,
                rate: BurnRate::PerRenderTick,
                source: stream,
                rec_path: None,
                cam2_run_id: None,
                step: super::node_render_step("strih", 60.0, 30.0, 60.0, 30.0),
            },
            &[CAM1B, STRIH, STREAM],
            tmp.path(),
            0,
        )
        .unwrap()
    }

    #[test]
    fn optical_undecodable_within_the_moire_floor_passes_the_gate_376() {
        // #376 — the real measured residual (run 354003, post-#363 decoder fix): 22/8999 = 0.2445%
        // of the optical span still fails the cam2 dual-QR read (a soft/mottled RIGHT QR half with
        // heavy diagonal moiré — a rig optical-physics floor, not a decoder or chain defect; the
        // user's explicit call — see the [`OPTICAL_UNDECODABLE_RATE_MAX`] doc). Mirror it at scale:
        // 9000 total frames, 22 interior frames with no optical paint (undecodable), the rest clean.
        let undecodable: HashSet<usize> = (1..=22).collect();
        let stream = optical_run_with_undecodable(9000, &undecodable);
        let v = node_verdict_for_optical_run(&stream);
        assert_eq!(v.optical_undecodable, 22, "{:?}", v.contiguity);
        assert_eq!(v.optical_span_frames, 9000, "{:?}", v.contiguity);
        assert!(
            v.contiguity.is_contiguous(),
            "the burn-id sequence is unaffected by the excluded optical holes (gap-ignore): {:?}",
            v.contiguity
        );
        // BEFORE #376 this asserted false (the old `optical_undecodable == 0` hard gate FAILED any
        // non-zero count). The calibrated floor now PASSES a residual within the rig's proven moiré
        // physics — this is the RED→GREEN line: it fails against the pre-#376 `== 0` gate and passes
        // against the calibrated rate gate.
        assert!(
            v.is_zero(),
            "a 0.244% optical-undecodable residual is within the calibrated moiré floor and must \
             PASS the #376 gate: {:?}",
            v.contiguity
        );
    }

    #[test]
    fn optical_undecodable_at_the_calibrated_ceiling_still_passes_376() {
        // #376 boundary: a rate EXACTLY at OPTICAL_UNDECODABLE_RATE_MAX (0.5%) must still pass — the
        // gate is `<=`, not `<`. 5 undecodable / 1000 total = exactly 0.005.
        let undecodable: HashSet<usize> = (1..=5).collect();
        let stream = optical_run_with_undecodable(1000, &undecodable);
        let v = node_verdict_for_optical_run(&stream);
        assert_eq!(v.optical_undecodable, 5, "{:?}", v.contiguity);
        assert_eq!(v.optical_span_frames, 1000, "{:?}", v.contiguity);
        assert!(
            v.is_zero(),
            "a rate exactly AT the calibrated ceiling (0.5%) must still pass (<=, not <): {:?}",
            v.contiguity
        );
    }

    #[test]
    fn optical_undecodable_just_above_the_calibrated_ceiling_fails_376() {
        // #376 boundary: one frame past the ceiling (6/1000 = 0.6%) must FAIL — the calibration
        // tolerates the measured moiré floor, never a rate genuinely above it.
        let undecodable: HashSet<usize> = (1..=6).collect();
        let stream = optical_run_with_undecodable(1000, &undecodable);
        let v = node_verdict_for_optical_run(&stream);
        assert_eq!(v.optical_undecodable, 6, "{:?}", v.contiguity);
        assert_eq!(v.optical_span_frames, 1000, "{:?}", v.contiguity);
        assert!(
            !v.is_zero(),
            "a rate just above the calibrated ceiling (0.6%) must still FAIL: {:?}",
            v.contiguity
        );
    }

    #[test]
    fn optical_undecodable_materially_above_the_floor_still_fails_376() {
        // #376 strict-gate stance (the user's explicit condition on the calibration): a REAL optical
        // dropout — qualitatively different from the moiré floor, e.g. the #216 slow-shutter ~175 s
        // gap — is far above the calibrated ceiling and MUST still fail. 200/1000 = 20%, two orders
        // of magnitude above the 0.5% floor.
        let undecodable: HashSet<usize> = (1..=200).collect();
        let stream = optical_run_with_undecodable(1000, &undecodable);
        let v = node_verdict_for_optical_run(&stream);
        assert_eq!(v.optical_undecodable, 200, "{:?}", v.contiguity);
        assert!(
            !v.is_zero(),
            "a real optical dropout (20%) must FAIL — the calibration never masks genuine loss: {:?}",
            v.contiguity
        );
    }

    #[test]
    fn collapsed_optical_span_fails_the_headline_duration_gate_373() {
        // #373 — the live failure shape: the cam2 optical read decoded only a SMALL early cluster
        // (6 frames here). Delivery is clean over the cluster, so the per-node delivery gate
        // is_zero() is TRUE and the burns are contiguous — exactly the shape that VACUOUSLY passed
        // the headline. The #373 duration floor must FAIL it: the analyzed span (6 / 30 fps = 0.2 s)
        // is far below the >=300 s zero-loss bar. The headline ANDs is_zero() with span_ok().
        let stream: Vec<RecordingFrame> = (0..6)
            .map(|i| frame(i, &[(CAM2, 100 + i as u32), (STRIH, 2000 + (i as u32) * 2)]))
            .collect();
        let tmp = tempfile::tempdir().unwrap();
        let v = node_verdict(
            &super::NodeSpec {
                node: "strih",
                burn_run_id: STRIH,
                rate: BurnRate::PerRenderTick,
                source: &stream,
                rec_path: None,
                cam2_run_id: None,
                step: super::node_render_step("strih", 60.0, 30.0, 60.0, 30.0),
            },
            &[STRIH, STREAM],
            tmp.path(),
            0,
        )
        .unwrap();
        // The delivery gate ALONE says zero loss (the vacuous-pass shape #360/#363 left open)...
        assert!(
            v.is_zero(),
            "delivery+optical+colour are clean over the tiny span (the vacuous-pass shape): {:?}",
            v.contiguity
        );
        assert_eq!(
            v.optical_span_frames, 6,
            "the optical span is the 6 optically-delivered frames"
        );
        // ...but the analyzed span (0.2 s) does NOT clear the 300 s headline floor ⇒ NOT zero loss.
        assert!(
            !v.span_ok(30.0, 300.0),
            "a 0.2 s collapsed span must FAIL the >=300 s headline duration floor (#373)"
        );
        // The floor never fails a genuine full-length run: 9000 frames @ 30 fps = 300 s passes.
        let full: Vec<RecordingFrame> = (0..9000)
            .map(|i| frame(i, &[(CAM2, 100 + i as u32), (STRIH, 2000 + (i as u32) * 2)]))
            .collect();
        let vf = node_verdict(
            &super::NodeSpec {
                node: "strih",
                burn_run_id: STRIH,
                rate: BurnRate::PerRenderTick,
                source: &full,
                rec_path: None,
                cam2_run_id: None,
                step: super::node_render_step("strih", 60.0, 30.0, 60.0, 30.0),
            },
            &[STRIH, STREAM],
            tmp.path(),
            0,
        )
        .unwrap();
        assert_eq!(vf.optical_span_frames, 9000);
        assert!(
            vf.is_zero() && vf.span_ok(30.0, 300.0),
            "a real 300 s run is delivery-clean AND clears the duration floor (#373)"
        );
    }

    #[test]
    fn empty_optical_span_has_zero_frames_and_fails_the_duration_gate_373() {
        // No cam2 optical frame at all (e.g. a fully green-cast camera): optical_span is None ⇒
        // optical_span_frames == 0 ⇒ analyzed span 0 s ⇒ span_ok false. (is_zero() also fails here
        // via the empty burn window; the duration floor is the belt-and-braces #373 guard the
        // headline ANDs alongside it.)
        let stream: Vec<RecordingFrame> = (0..4)
            .map(|i| frame(i, &[(STRIH, 2000 + (i as u32) * 2)])) // strih burn only, NO cam2 optical
            .collect();
        let tmp = tempfile::tempdir().unwrap();
        let v = node_verdict(
            &super::NodeSpec {
                node: "strih",
                burn_run_id: STRIH,
                rate: BurnRate::PerRenderTick,
                source: &stream,
                rec_path: None,
                cam2_run_id: None,
                step: super::node_render_step("strih", 60.0, 30.0, 60.0, 30.0),
            },
            &[STRIH, STREAM],
            tmp.path(),
            0,
        )
        .unwrap();
        assert_eq!(
            v.optical_span_frames, 0,
            "no cam2 optical frame ⇒ empty optical span (0 frames)"
        );
        assert!(
            !v.span_ok(30.0, 300.0),
            "a 0-frame optical span must FAIL the >=300 s duration floor (#373)"
        );
    }

    #[test]
    fn print_node_verdict_does_not_co_print_no_burn_with_optical_374() {
        // #374 nit 2 — an empty burn window (the node's burn never decoded ⇒ first_id None) WITH an
        // interior optical hole used to co-print BOTH the OPTICAL-UNDECODABLE line AND the generic
        // "NO burn id decoded" line. Now the generic line is suppressed when a specific fault line
        // already explained the failure.
        let stream = vec![
            frame(0, &[(CAM2, 100)]),   // delivered optical, NO strih burn
            frame(1, &[(STRIH, 1673)]), // non-optical interior hole — one OPTICAL-UNDECODABLE
            frame(2, &[(CAM2, 102)]),   // delivered optical, NO strih burn
        ];
        let tmp = tempfile::tempdir().unwrap();
        let v = node_verdict(
            &super::NodeSpec {
                node: "strih",
                burn_run_id: STRIH,
                rate: BurnRate::PerRenderTick,
                source: &stream,
                rec_path: None,
                cam2_run_id: None,
                step: super::node_render_step("strih", 60.0, 30.0, 60.0, 30.0),
            },
            &[STRIH, STREAM],
            tmp.path(),
            0,
        )
        .unwrap();
        assert!(
            v.contiguity.first_id.is_none(),
            "the strih burn never decoded"
        );
        assert_eq!(v.optical_undecodable, 1, "frame 1 is one optical hole");
        let lines = super::node_verdict_lines(&v, true);
        assert!(
            lines.iter().any(|l| l.contains("OPTICAL-UNDECODABLE")),
            "the specific optical fault line must print: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("NO burn id decoded")),
            "the generic no-burn line must NOT co-print once optical already explained it (#374): {lines:?}"
        );
    }

    #[test]
    fn print_node_verdict_keeps_no_burn_line_when_it_is_the_sole_reason_374() {
        // #374 nit 2 — when the failure is ONLY "no burn decoded" (delivered frames, none carrying
        // the node burn, no optical hole, no colour fault), the generic line is still the sole,
        // correct explanation and MUST print — the dedup never drops a unique reason.
        let stream = vec![
            frame(0, &[(CAM2, 100)]), // delivered optical, no strih burn
            frame(1, &[(CAM2, 101)]), // delivered optical, no strih burn
        ];
        let tmp = tempfile::tempdir().unwrap();
        let v = node_verdict(
            &super::NodeSpec {
                node: "strih",
                burn_run_id: STRIH,
                rate: BurnRate::PerRenderTick,
                source: &stream,
                rec_path: None,
                cam2_run_id: None,
                step: super::node_render_step("strih", 60.0, 30.0, 60.0, 30.0),
            },
            &[STRIH, STREAM],
            tmp.path(),
            0,
        )
        .unwrap();
        assert!(v.contiguity.first_id.is_none());
        assert_eq!(v.optical_undecodable, 0, "no interior optical hole here");
        let lines = super::node_verdict_lines(&v, true);
        assert!(
            lines.iter().any(|l| l.contains("NO burn id decoded")),
            "the sole-reason no-burn line must still print: {lines:?}"
        );
    }

    #[test]
    fn in_window_delivered_frame_missing_burn_is_one_gap_not_a_range() {
        // A genuine in-window fault: ONE delivered frame (carries cam2 QR) has no strih burn
        // among per-render-tick neighbours. The in-window sequence yields exactly ONE missing
        // entry (a `None` slot), NOT a whole integer-range of phantom missing ids — proving
        // the rate inflation is gone while a real per-frame drop is still caught.
        let stream = vec![
            frame(0, &[(CAM2, 100), (STRIH, 1670)]),
            frame(1, &[(CAM2, 101), (STRIH, 1673)]),
            frame(2, &[(CAM2, 102)]), // delivered, NO strih burn ⇒ one real fault
            frame(3, &[(CAM2, 103), (STRIH, 1679)]),
        ];
        let w = in_window_burn_frames(
            &stream,
            STRIH,
            &[STRIH, STREAM],
            BurnRate::PerRenderTick,
            None,
        );
        let burns: Vec<Option<u32>> = w.iter().map(|f| f.burn_id).collect();
        assert_eq!(
            burns,
            vec![Some(1670), Some(1673), None, Some(1679)],
            "the one delivered frame with no burn is a single None slot, not a range"
        );
        let iw = camera_box::probe::burn_contiguity::burn_contiguity_in_window(
            "strih",
            &w,
            BurnRate::PerRenderTick,
        );
        assert!(
            !iw.contiguity.is_contiguous(),
            "a delivered frame missing its burn is loss"
        );
        assert_eq!(
            iw.contiguity.missing_ids.len(),
            1,
            "exactly ONE in-window drop"
        );
        assert_eq!(iw.contiguity.expected_count, 4); // 4 delivered frames, not the 9-wide span
        assert_eq!(
            iw.missing_slots[0].kind,
            InWindowMissingKind::BurnUnreadable
        );
    }

    #[test]
    fn cam1_capture_stats_parses_dropped_and_captured() {
        // cam2→cam1 loss = cam1's V4L2 capture-drop count (the camera leg). The verdict reads
        // v4l2_dropped as the loss, frames_captured as the denominator.
        let s = parse_cam1_capture_stats_str("v4l2_dropped=3\nframes_captured=9000\n").unwrap();
        assert_eq!(s.v4l2_dropped, 3);
        assert_eq!(s.frames_captured, 9000);
    }

    #[test]
    fn cam1_capture_stats_zero_dropped_is_zero_loss() {
        let s = parse_cam1_capture_stats_str("v4l2_dropped=0\nframes_captured=9001\n").unwrap();
        assert_eq!(s.v4l2_dropped, 0, "0 V4L2 drops ⇒ zero cam2→cam1 loss");
    }

    #[test]
    fn cam1_capture_stats_missing_dropped_key_errors() {
        // A sidecar with no v4l2_dropped key must NOT silently read as zero loss.
        assert!(parse_cam1_capture_stats_str("frames_captured=9000\n").is_err());
    }

    #[test]
    fn cam1_capture_stats_ignores_unknown_keys_and_blank_lines() {
        // Forward-compatible: unknown keys + blanks are skipped; the drop count still parses.
        let s = parse_cam1_capture_stats_str(
            "\nv4l2_dropped=2\nfuture_key=whatever\n\nframes_captured=100\n",
        )
        .unwrap();
        assert_eq!(s.v4l2_dropped, 2);
        assert_eq!(s.frames_captured, 100);
    }

    #[test]
    fn cam1_capture_stats_non_numeric_errors() {
        assert!(parse_cam1_capture_stats_str("v4l2_dropped=lots\n").is_err());
    }

    #[test]
    fn painter_ticks_parse_paint_log_format_tick_first_column() {
        // REGRESSION (#105 integration): the --paint-log ground truth is `tick,gen_ts_ns`
        // (tick in column 0). parse_painter_ticks must read column 0 for this header — the
        // bug the live smoke caught was it forcing column 2 ("too few columns" on the
        // header) and discarding the entire painter set.
        let csv = "tick,gen_ts_ns\n0,1782000000000\n1,1782000016000\n2,1782000033000\n";
        let ticks = parse_painter_ticks_str(csv).unwrap();
        assert_eq!(ticks, vec![0, 1, 2], "paint-log tick is column 0");
    }

    #[test]
    fn painter_ticks_parse_3col_flip_log_still_reads_tick_column_0() {
        // #194 REGRESSION: the new 3-column paint-log `tick,gen_ts_ns,flip_ts_ns` MUST keep
        // working with the existing tick reader (it keys on the `tick,` prefix → column 0).
        // The flip column is purely additive — the cam→strih tick assessment is unchanged.
        let csv = "tick,gen_ts_ns,flip_ts_ns\n0,1000,1018\n1,1016,1034\n2,1033,1050\n";
        let ticks = parse_painter_ticks_str(csv).unwrap();
        assert_eq!(
            ticks,
            vec![0, 1, 2],
            "3-column flip log: tick is still column 0"
        );
    }

    #[test]
    fn painter_flip_parses_3col_into_gen_and_flip_maps() {
        // #194: the flip parser reads tick→gen_ts and tick→flip_ts from the 3-column log.
        let csv = "tick,gen_ts_ns,flip_ts_ns\n100,1000,1018\n102,1033,1053\n";
        let (gen, flip) = parse_painter_flip_str(csv).unwrap();
        assert_eq!(gen.get(&100), Some(&1000));
        assert_eq!(gen.get(&102), Some(&1033));
        assert_eq!(flip.get(&100), Some(&1018));
        assert_eq!(flip.get(&102), Some(&1053));
        // Every flip stamp is >= its gen stamp (display follows generation).
        for (t, &g) in &gen {
            assert!(flip[t] >= g, "tick {t}: flip {} >= gen {g}", flip[t]);
        }
    }

    #[test]
    fn painter_flip_returns_empty_for_2col_or_probe_or_bare_no_flip_column() {
        // No flip column ⇒ EMPTY maps (graceful fallback to the gen-based cam2→cam1). The
        // pre-#194 2-column log, a recording-probe CSV, and a bare tick file all qualify.
        for csv in [
            "tick,gen_ts_ns\n0,1000\n1,1016\n", // old 2-column
            "frame_index,n_qr,tick,run_id,frame_ids\n0,2,100,7,1\n", // recording-probe
            "10\n11\n12\n",                     // bare
        ] {
            let (gen, flip) = parse_painter_flip_str(csv).unwrap();
            assert!(
                gen.is_empty() && flip.is_empty(),
                "no flip column ⇒ empty: {csv:?}"
            );
        }
    }

    #[test]
    fn painter_flip_errors_on_malformed_3col_row() {
        // A 3-column header but a data row with the wrong column count / a non-integer is a
        // MALFORMED log — error loudly (a silently-shrunk flip map drops real samples).
        let too_few = "tick,gen_ts_ns,flip_ts_ns\n100,1000\n";
        assert!(
            parse_painter_flip_str(too_few).is_err(),
            "2 cols under a 3-col header errors"
        );
        let bad_flip = "tick,gen_ts_ns,flip_ts_ns\n100,1000,notanumber\n";
        assert!(
            parse_painter_flip_str(bad_flip).is_err(),
            "non-integer flip errors"
        );
    }

    #[test]
    fn painter_ticks_parse_recording_probe_format_tick_third_column() {
        // recording-probe CSV: frame_index,n_qr,tick,run_id,frame_ids ⇒ tick is column 2.
        let csv = "frame_index,n_qr,tick,run_id,frame_ids\n0,2,100,7,100;99\n1,2,102,7,102;101\n";
        let ticks = parse_painter_ticks_str(csv).unwrap();
        assert_eq!(ticks, vec![100, 102], "recording-probe tick is column 2");
    }

    #[test]
    fn painter_ticks_parse_bare_one_per_line() {
        // A bare file (no header, no comma): the whole line is the tick.
        let ticks = parse_painter_ticks_str("10\n11\n12\n").unwrap();
        assert_eq!(ticks, vec![10, 11, 12]);
    }

    #[test]
    fn painter_ticks_skip_empty_recording_probe_tick_column() {
        // An undecodable recording-probe row has an empty tick column → skipped, not error.
        let csv = "frame_index,n_qr,tick,run_id,frame_ids\n0,2,100,7,x\n1,0,,,\n2,2,104,7,y\n";
        let ticks = parse_painter_ticks_str(csv).unwrap();
        assert_eq!(ticks, vec![100, 104], "empty tick column skipped");
    }

    #[test]
    fn painter_ticks_malformed_row_errors_loudly() {
        // A paint-log header but a data row with a non-numeric tick must error, not
        // silently drop (a shrunk painter set manufactures false phantom faults).
        let csv = "tick,gen_ts_ns\nnotanumber,123\n";
        assert!(parse_painter_ticks_str(csv).is_err());
    }

    #[test]
    fn grab_ts_sidecar_parses_frame_index_to_grab_ts() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(
            f,
            "frame_index,grab_ts_ns\n0,1782000000000\n1,1782000033000\n"
        )
        .unwrap();
        let m = parse_grab_ts(f.path()).unwrap();
        assert_eq!(m.get(&0), Some(&1782000000000));
        assert_eq!(m.get(&1), Some(&1782000033000));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn grab_ts_sidecar_malformed_row_errors() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "frame_index,grab_ts_ns\n0\n").unwrap(); // <2 columns
        assert!(parse_grab_ts(f.path()).is_err());
    }

    #[test]
    fn grab_ts_sidecar_tolerates_any_trailing_partial_row() {
        // REGRESSION (#111 deploy + deep review): the cam1 --record-grab BufWriter is killed
        // at teardown mid-write with NO flush, so the file is cut at an arbitrary byte
        // boundary — the surviving trailing fragment (no terminating '\n') can be ANY shape:
        //   "2,"        empty timestamp           -> must skip
        //   "2"         no comma at all           -> must skip (was: <2 columns ABORT)
        //   "2,17820"   timestamp truncated mid-digits, parses as a valid i64
        //               -> must skip (was: silently inserts a WRONG latency sample)
        // A complete row ALWAYS ends in '\n' (writeln! writes the '\n' last). So: a file that
        // does NOT end in '\n' has a partial final line -> skip it, whatever its shape. The
        // earlier good rows still parse. This must never crash the verdict / block the report.
        for partial in ["2,", "2", "2,17820", "garbage"] {
            let mut f = tempfile::NamedTempFile::new().unwrap();
            write!(
                f,
                "frame_index,grab_ts_ns\n0,1782000000000\n1,1782000033000\n{partial}"
            )
            .unwrap();
            let m = parse_grab_ts(f.path())
                .unwrap_or_else(|e| panic!("partial {partial:?} should be tolerated, got {e:?}"));
            assert_eq!(m.get(&0), Some(&1782000000000));
            assert_eq!(m.get(&1), Some(&1782000033000));
            assert_eq!(
                m.len(),
                2,
                "the partial trailing row {partial:?} (no trailing newline) is skipped, not parsed"
            );
        }
    }

    #[test]
    fn grab_ts_sidecar_complete_final_row_with_newline_still_parsed() {
        // A complete final row (ends in '\n') is NOT a partial fragment — it must still parse.
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(
            f,
            "frame_index,grab_ts_ns\n0,1782000000000\n2,1782000066000\n"
        )
        .unwrap();
        let m = parse_grab_ts(f.path()).unwrap();
        assert_eq!(m.get(&2), Some(&1782000066000), "complete final row parses");
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn grab_ts_sidecar_empty_ts_row_is_skipped_not_crashed() {
        // REGRESSION (#170): run-163163's verdict computed all loss hops then CRASHED at the
        // very end on `grab-ts grab_ts_ns not an i64 at line 9857: "" — cannot parse integer
        // from empty string` (VERDICT_EXIT=1), losing the WHOLE latency computation because a
        // SINGLE cam1 grab-ts sidecar row had an empty grab_ts_ns cell. An empty timestamp cell
        // = that frame simply has no recorded grab instant = NO cam2→cam1 pairing for that frame
        // (cam2_cam1_samples already returns no sample when grab_ts_by_index has no entry) — it
        // is benign missing data, NOT fatal corruption. So an empty grab_ts_ns cell MUST warn +
        // skip that one row and parse the rest, never abort the verdict. Covers BOTH an empty-ts
        // row mid-file (1,) and an empty-ts row as the newline-terminated final line.
        for csv in [
            // empty-ts mid-file, good rows on both sides
            "frame_index,grab_ts_ns\n0,1782000000000\n1,\n2,1782000066000\n",
            // empty-ts as the (newline-terminated) final data row
            "frame_index,grab_ts_ns\n0,1782000000000\n2,1782000066000\n1,\n",
        ] {
            let mut f = tempfile::NamedTempFile::new().unwrap();
            write!(f, "{csv}").unwrap();
            let m = parse_grab_ts(f.path())
                .unwrap_or_else(|e| panic!("empty-ts row must be skipped, not crash: {e:?}"));
            assert_eq!(m.get(&0), Some(&1782000000000), "good rows still parse");
            assert_eq!(m.get(&2), Some(&1782000066000), "good rows still parse");
            assert_eq!(m.get(&1), None, "the empty-ts frame has no grab instant");
            assert_eq!(
                m.len(),
                2,
                "exactly the two good rows, the empty-ts row skipped"
            );
        }
    }

    #[test]
    fn grab_ts_sidecar_nonempty_garbage_ts_row_still_errors() {
        // A NON-empty but unparseable grab_ts_ns cell (e.g. "abc") on a complete (newline-
        // terminated) row is genuine corruption, not benign missing data — it still errors
        // loudly so corrupt sidecars are surfaced rather than silently dropping samples. (Only
        // the EMPTY cell is treated as "no sample for this frame" per #170; junk is not.)
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(
            f,
            "frame_index,grab_ts_ns\n0,1782000000000\n1,abc\n2,1782000066000\n"
        )
        .unwrap();
        assert!(
            parse_grab_ts(f.path()).is_err(),
            "a non-empty non-integer ts cell is corruption -> error"
        );
    }

    // ---- #461/#463 imag-nb optical (+ digital corner burn) zero-loss gate (EPIC #466 Topology v2) ----

    /// #463 review: every `imag_window`/`imag_window_with_burn` call site used the same frame
    /// count — the parameter was never actually varied, just repeated at each call. Hardcoded
    /// here instead of threading an unused-in-practice `n: u32` through both helpers.
    const IMAG_WINDOW_FRAMES: u32 = 60;

    /// Build [`IMAG_WINDOW_FRAMES`] imag-nb recorded frames, each carrying ONLY a cam2-style
    /// optical payload (no digital burn — the pre-#463 shape, and still the shape of an older
    /// recording / a build not yet carrying imag's corner burn). Contiguous ticks
    /// 100..100+[`IMAG_WINDOW_FRAMES`] by default; `gap_at` (if given) removes ONE tick to
    /// simulate a dropped frame.
    fn imag_window(gap_at: Option<u32>) -> Vec<RecordingFrame> {
        (0..IMAG_WINDOW_FRAMES)
            .filter(|&i| gap_at != Some(i))
            .map(|i| frame(i as u64, &[(CAM2, 100 + i)]))
            .collect()
    }

    /// #463 — like [`imag_window`], but every frame ALSO carries imag's own digital corner burn
    /// (run_id [`super::BURN_RUN_ID_IMAG`]). **#480 update**: the burn id now steps by
    /// [`camera_box::imag_tick_gate::IMAG_BURN_RENDER_STEP`] per recorded frame (`10 + 2*i`), NOT
    /// 1 — the confirmed live behaviour (Studio-Mode double-render free-runs the counter at 2x
    /// the recorded rate; see `node_verdict_for_imag`'s doc). A fixture stepping by 1 would no
    /// longer be a REACHABLE recording shape and would silently exercise the OLD, now-wrong
    /// model. The burn ids are kept BELOW the cam2 tick range (100..100+n) for every frame
    /// (`10 + 2*59 = 128 < 100 + 59 = 159`) — `frame()`'s `tick = max(frame_id)` over ALL
    /// payloads on the frame, so if the burn id were ever the LARGER of the two (a real bug
    /// caught by CI: an earlier draft used ids 500..500+n, ABOVE the cam2 range, which made
    /// `.tick` silently resolve to the BURN id instead of the cam2 tick on every frame)
    /// `nv.contiguity` would stop reflecting the optical signal at all. With burn ids always <
    /// cam2 ids, `.tick` is always the cam2 id regardless of whether the burn is present.
    /// `burn_gap_at` (when given) omits JUST the burn payload on that one frame index (the cam2
    /// tick stays present) — a burn-only miss, modeling ONE entire recorded output frame whose
    /// burn never reached the recording (a genuine dropped step-grid slot, not by-design
    /// alternation). (An "optical-only miss" needs a DIFFERENT construction — a frame with NO
    /// cam2 payload at all has `tick: None` in real decode output, which this payload-max-based
    /// helper cannot represent; see
    /// `node_verdict_for_imag_fails_when_optical_is_broken_even_with_a_clean_digital_burn_463`'s
    /// direct `RecordingFrame` construction for that case.)
    fn imag_window_with_burn(burn_gap_at: Option<u32>) -> Vec<RecordingFrame> {
        (0..IMAG_WINDOW_FRAMES)
            .map(|i| {
                let mut payloads = vec![(CAM2, 100 + i)];
                if burn_gap_at != Some(i) {
                    payloads.push((super::BURN_RUN_ID_IMAG, 10 + 2 * i));
                }
                frame(i as u64, &payloads)
            })
            .collect()
    }

    /// #580v2 — a BEAT-compensated optical read (frame 30 re-reads the preceding tick 129 instead of
    /// advancing to 130: one skip fully balanced by one duplicate) PLUS a clean step-2 digital burn
    /// on EVERY frame. Since #580v2 makes the burn the SOLE delivery authority (an absent burn now
    /// FAILS fail-closed, #585), a beat-compensated PASS must carry a genuinely-present burn — the
    /// pre-#580v2 no-burn beat fixtures would now (correctly) fail. Burn ids stay below the cam2 range
    /// so `frame()`'s `tick = max(frame_id)` always resolves to the cam2 tick (the #576 gotcha).
    fn imag_window_beat_with_burn() -> Vec<RecordingFrame> {
        (0..IMAG_WINDOW_FRAMES)
            .map(|i| {
                let tick = if i == 30 { 100 + i - 1 } else { 100 + i };
                frame(
                    i as u64,
                    &[(CAM2, tick), (super::BURN_RUN_ID_IMAG, 10 + 2 * i)],
                )
            })
            .collect()
    }

    #[test]
    fn node_verdict_for_imag_optical_only_no_burn_fails_closed_585() {
        use super::node_verdict_for_imag;
        // #580v2 (#585) — contract change: a clean optical read with NO digital burn decoded at all
        // now FAILS FAIL-CLOSED. The burn is imag's SOLE delivery authority once the optical surplus
        // is demoted to diagnostic, so an absent burn can no longer vacuously pass
        // (`optional_signal_ok(None)` used to return `true`). Pre-#580v2 this asserted `is_zero()`.
        let frames = imag_window(None);
        let nv = node_verdict_for_imag(&frames, None);
        assert!(
            !nv.is_zero(),
            "no digital burn decoded ⇒ fail-closed, the burn is the sole delivery authority (#585)"
        );
        assert_eq!(nv.contiguity.node, "imag");
        // The OPTICAL read itself is still perfectly clean (this is a burn-absence fail, not an
        // optical fail). #575: the boundary trim excludes frame_index 0..=2 and 57..=59, so the
        // analyzed tick span is 103..=156; `optical_span_frames` is UNAFFECTED (computed from ALL
        // frames, never trimmed).
        assert_eq!(nv.contiguity.first_id, Some(103));
        assert_eq!(nv.contiguity.last_id, Some(156));
        assert!(nv.contiguity.missing_ids.is_empty());
        assert_eq!(nv.optical_span_frames, 60);
        assert_eq!(
            nv.imag_optical_beat_pass(),
            Some(true),
            "the optical read is LIVE and copy-free — the failure is the ABSENT burn, not optical"
        );
        let burn = nv
            .imag_burn_contiguity
            .as_ref()
            .expect("imag always computes the burn-contiguity slot, even when empty");
        assert_eq!(burn.first_id, None, "no digital burn decoded");
        assert_eq!(
            nv.imag_burn_present_ok,
            Some(false),
            "the present floor fails an absent burn (#585)"
        );
        assert!(
            !nv.imag_burn_ok(),
            "an absent burn must FAIL fail-closed, not vacuously pass (#585)"
        );
        // The printed reason must name the absent burn, honestly.
        let lines = super::node_verdict_lines(&nv, true);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("ABSENT or below the present floor") && l.contains("NOT zero")),
            "the fail-closed reason must be printed: {lines:?}"
        );
    }

    #[test]
    fn node_verdict_for_imag_passes_with_a_contiguous_digital_burn_present_463() {
        use super::node_verdict_for_imag;
        // Every frame carries BOTH the cam2 optical tick AND imag's own digital corner burn,
        // both clean — the doubly-proven #463 pass.
        let frames = imag_window_with_burn(None);
        let nv = node_verdict_for_imag(&frames, None);
        assert!(
            nv.is_zero(),
            "optical contiguous AND digital burn contiguous ⇒ zero loss (#463)"
        );
        let burn = nv.imag_burn_contiguity.as_ref().expect("burn decoded");
        // #575: the boundary trim excludes frame_index 0..=2 and 57..=59, so the analyzed burn
        // span is over frame_index 3..=56 (burn id = 10 + 2*i), not the raw 0..=59 —
        // first = 10 + 2*3 = 16, last = 10 + 2*56 = 122 (not the pre-#575 10 / 128).
        assert_eq!(burn.first_id, Some(16));
        assert_eq!(burn.last_id, Some(122));
        assert!(
            burn.missing_ids.is_empty(),
            "a clean step-2 free-running burn is zero loss once correctly modeled (#480): {burn:?}"
        );
    }

    #[test]
    fn node_verdict_for_imag_fails_when_the_digital_burn_has_a_gap_even_with_clean_optical_463() {
        use super::node_verdict_for_imag;
        // The cam2 optical tick stays perfectly contiguous (every frame is present); only the
        // BURN payload is missing on frame index 30 — imag's second, independent proof disagrees.
        let frames = imag_window_with_burn(Some(30));
        let nv = node_verdict_for_imag(&frames, None);
        assert!(
            nv.contiguity.is_contiguous(),
            "sanity: the optical tick alone is perfectly clean"
        );
        assert!(
            !nv.is_zero(),
            "a present-but-gappy digital burn FAILS the node even though the optical read is \
             clean — the #463 strict-test mandate: never let a weaker proof override a stronger \
             one that disagrees"
        );
        let burn = nv.imag_burn_contiguity.as_ref().expect("burn decoded");
        assert_eq!(
            burn.missing_ids,
            // #480: frame 30's burn id under the step-2 model is 10 + 2*30 = 70, not the old
            // step-1 40 — a genuinely missing step-grid slot is still caught, never masked.
            vec![70],
            "the exact missing digital burn id must be reported"
        );
    }

    #[test]
    fn node_verdict_for_imag_calibrates_the_burn_step_from_observed_cadence_576() {
        use super::node_verdict_for_imag;
        // #576: THIS recording's burn free-runs at step 3 (the #572 live-rig cadence), NOT the
        // #480-confirmed step 2 — the hardcoded IMAG_BURN_RENDER_STEP constant would have been
        // wrong here. `calibrate_burn_step` must derive 3 from the observed ids and still
        // declare a clean run zero loss.
        //
        // The cam2 tick base is kept WELL ABOVE the burn id range (100_000+i vs 10+3*i) —
        // `frame()`'s `tick = max(frame_id)` over ALL payloads on the frame means a burn id that
        // ever exceeds the cam2 id would silently hijack `.tick`, corrupting the optical
        // contiguity check (see `imag_window_with_burn`'s doc comment for the same gotcha).
        let frames: Vec<RecordingFrame> = (0..60u32)
            .map(|i| {
                frame(
                    i as u64,
                    &[(CAM2, 100_000 + i), (super::BURN_RUN_ID_IMAG, 10 + 3 * i)],
                )
            })
            .collect();
        let nv = node_verdict_for_imag(&frames, None);
        assert!(
            nv.is_zero(),
            "a clean step-3 free-running burn must be zero loss once calibrated (#576): {:?}",
            nv.imag_burn_contiguity
        );
        let burn = nv.imag_burn_contiguity.as_ref().expect("burn decoded");
        // #575: the boundary trim excludes frame_index 0..=2 and 57..=59, so the analyzed span
        // is frame_index 3..=56 — first = 10 + 3*3 = 19, last = 10 + 3*56 = 178 (not the raw
        // 10 / 10 + 3*59).
        assert_eq!(burn.first_id, Some(19));
        assert_eq!(burn.last_id, Some(178));
        assert!(burn.missing_ids.is_empty());
    }

    #[test]
    fn node_verdict_for_imag_calibrated_step_attributes_the_correct_missing_grid_id_576() {
        use super::node_verdict_for_imag;
        // Same step-3 cadence as above, but frame index 30's burn payload never reached the
        // recording — a genuine dropped step-grid slot. The OLD hardcoded step-2 constant would
        // have attributed the WRONG grid ids entirely (see `imag_tick_gate`'s #576 unit test
        // `calibrate_burn_step_feeds_correct_missing_grid_id_into_burn_step_contiguity_576`);
        // calibrated to the TRUE step, the exact missing grid id must be reported. Cam2 tick
        // base kept WELL ABOVE the burn id range for the same reason as the test above.
        let frames: Vec<RecordingFrame> = (0..60u32)
            .map(|i| {
                let mut payloads = vec![(CAM2, 100_000 + i)];
                if i != 30 {
                    payloads.push((super::BURN_RUN_ID_IMAG, 10 + 3 * i));
                }
                frame(i as u64, &payloads)
            })
            .collect();
        let nv = node_verdict_for_imag(&frames, None);
        assert!(
            !nv.is_zero(),
            "a genuinely missing step-3 grid slot must fail (#576)"
        );
        let burn = nv.imag_burn_contiguity.as_ref().expect("burn decoded");
        assert_eq!(
            burn.missing_ids,
            vec![10 + 3 * 30],
            "the exact missing step-3 grid id must be reported, not a step-2-derived wrong id: {burn:?}"
        );
    }

    #[test]
    fn node_verdict_for_imag_fails_when_optical_is_broken_even_with_a_clean_digital_burn_463() {
        use super::node_verdict_for_imag;
        // Built via DIRECT RecordingFrame construction (not `imag_window_with_burn`, which has
        // no way to drop JUST the cam2 payload while keeping the burn — see its doc comment).
        // Frame 30 decodes ONLY the digital burn (no
        // cam2 payload at all) -> `tick: None`, mirroring production's REAL exclusion semantics
        // exactly (`decode_recording_frame_with_burns` filters node burns out of the tick
        // computation, #463's `NODE_BURN_RUN_IDS` fix) -- a frame whose only payload is a
        // digital burn correctly has NO optical tick, not the burn's frame_id. Its burn payload
        // is still present, so the digital-burn sequence itself stays perfectly contiguous.
        let frames: Vec<RecordingFrame> = (0..60u32)
            .map(|i| {
                if i == 30 {
                    RecordingFrame {
                        frame_index: i as u64,
                        payloads: vec![Payload {
                            run_id: super::BURN_RUN_ID_IMAG,
                            frame_id: 10 + i,
                            gen_ts_ns: 1,
                        }],
                        tick: None,
                    }
                } else {
                    frame(
                        i as u64,
                        &[(CAM2, 100 + i), (super::BURN_RUN_ID_IMAG, 10 + i)],
                    )
                }
            })
            .collect();
        let nv = node_verdict_for_imag(&frames, None);
        assert!(
            !nv.contiguity.is_contiguous(),
            "sanity: the optical tick has a real gap (frame 30 decoded no cam2 payload)"
        );
        let burn = nv.imag_burn_contiguity.as_ref().expect("burn decoded");
        assert!(
            burn.is_contiguous(),
            "sanity: the digital burn itself stays perfectly contiguous: {burn:?}"
        );
        assert!(
            !nv.is_zero(),
            "a missing optical tick FAILS even when the digital burn is perfectly contiguous (#463)"
        );
    }

    #[test]
    fn node_verdict_for_imag_fails_when_a_tick_is_missing_461() {
        use super::node_verdict_for_imag;
        // Drop frame index 30 (painted tick 130) -> imag's camera failed to capture that instant.
        let frames = imag_window(Some(30));
        let nv = node_verdict_for_imag(&frames, None);
        assert!(
            !nv.is_zero(),
            "a missing optical tick in the analyzed span must FAIL"
        );
        assert_eq!(
            nv.contiguity.missing_ids,
            vec![130],
            "the exact missing painted tick must be reported"
        );
    }

    #[test]
    fn node_verdict_for_imag_fails_when_no_ticks_decoded_at_all_461() {
        use super::node_verdict_for_imag;
        let nv = node_verdict_for_imag(&[], None);
        assert!(
            !nv.is_zero(),
            "no optical tick at all must never read as a zero-loss pass (nothing proven)"
        );
        assert_eq!(nv.contiguity.first_id, None);
        assert_eq!(nv.optical_span_frames, 0);
    }

    #[test]
    fn node_verdict_for_imag_trims_the_start_boundary_pre_roll_artifact_575() {
        use super::node_verdict_for_imag;
        // Reproduces the confirmed live incident (#575, run 554307): a rogue LOW optical tick
        // decoded at the very first recorded frame — the genlock-fifo pre-roll flush emitting a
        // stale backlogged value before the feed catches up to the live/contiguous run. Frames
        // 1-2 decode nothing (fifo still draining); the clean contiguous run resumes at
        // frame_index 3. WITHOUT the boundary trim this manufactures ~900 phantom "missing"
        // ticks between the stale value and the clean run — even though none of it is real loss.
        let mut frames = vec![frame(0, &[(CAM2, 1)])];
        for i in 3..IMAG_WINDOW_FRAMES {
            frames.push(frame(i as u64, &[(CAM2, 1000 + i)]));
        }
        let nv = node_verdict_for_imag(&frames, None);
        assert!(
            nv.contiguity.is_contiguous(),
            "the stale pre-roll tick at frame_index 0 must be TRIMMED, not counted as phantom \
             missing ticks (#575): {:?}",
            nv.contiguity
        );
    }

    #[test]
    fn node_verdict_for_imag_trims_the_stop_boundary_finalization_artifact_575() {
        use super::node_verdict_for_imag;
        // Mirrors the start-boundary test, but for the STOP side: the last recorded frame
        // carries a rogue tick FAR ahead of the clean run — mux finalization at StopRecord
        // briefly surfacing an already-elapsed painter value that never continues.
        let mut frames: Vec<RecordingFrame> = (0..IMAG_WINDOW_FRAMES - 1)
            .map(|i| frame(i as u64, &[(CAM2, 1000 + i)]))
            .collect();
        frames.push(frame((IMAG_WINDOW_FRAMES - 1) as u64, &[(CAM2, 50_000)]));
        let nv = node_verdict_for_imag(&frames, None);
        assert!(
            nv.contiguity.is_contiguous(),
            "the rogue finalization tick at the last frame_index must be TRIMMED, not counted \
             as phantom missing ticks (#575): {:?}",
            nv.contiguity
        );
    }

    #[test]
    fn node_verdict_for_imag_boundary_trim_never_masks_a_genuine_drop_just_past_the_lead_edge_575()
    {
        use super::node_verdict_for_imag;
        // Same boundary-artifact shape as the start-boundary test above, but frame_index 5 (well
        // inside the KEPT window once the lead-3 frames are trimmed) is ALSO a genuine dropped
        // instant — this must still FAIL, proving the trim can never mask a real mid-recording
        // drop.
        let mut frames = vec![frame(0, &[(CAM2, 1)])];
        for i in 3..IMAG_WINDOW_FRAMES {
            if i == 5 {
                continue;
            }
            frames.push(frame(i as u64, &[(CAM2, 1000 + i)]));
        }
        let nv = node_verdict_for_imag(&frames, None);
        assert!(
            !nv.contiguity.is_contiguous(),
            "a genuine drop just past the trimmed lead edge must NEVER be masked (#575): {:?}",
            nv.contiguity
        );
        assert_eq!(nv.contiguity.missing_ids, vec![1005]);
    }

    // ============================================================================
    // #580v2 — imag's optical zero-loss gate recognizes the 60Hz-monitor vs 60fps-camera sampling
    // BEAT via RUN-LENGTH (max consecutive Δtick==0), replacing strict step-1 tick contiguity AND
    // the v1 `surplus <= 0` aggregate as the primary optical decision. Confirmed live (run 572001,
    // post-#575 trim + #576 calibration, RE-SIGNED to the real numbers after the v1 gate shipped
    // with a sign-flipped fixture): expected=21870, frames=21867, present=21851, missing=19,
    // dups=22, surplus=+3, digital burn 0-missing — a genuinely zero-loss run that BOTH strict
    // step-1 (missing=19) AND a naive `surplus <= 0` aggregate (surplus is +3, not negative)
    // false-fail; the run-length gate (`is_live_no_copy`) correctly passes it.
    // ============================================================================

    #[test]
    fn node_verdict_for_imag_optical_beat_compensated_skip_is_zero_loss_580v2() {
        use super::node_verdict_for_imag;
        // A genuine beat stutter: at i=30 the camera re-reads the PRECEDING painted tick (129)
        // instead of advancing to 130 — one skip (130 never appears) balanced by one duplicate (129
        // appears twice, an isolated Δ0 run of 1). #580v2: paired with a clean digital burn (the burn
        // is now the SOLE delivery authority, so a beat-compensated PASS must carry a present burn),
        // the node is ZERO loss — strict step-1 false-fails this exact shape.
        let frames = imag_window_beat_with_burn();
        let nv = node_verdict_for_imag(&frames, None);
        assert!(
            !nv.contiguity.is_contiguous(),
            "sanity: the RAW strict tick sequence has a nominal gap at 130 (masked by the dup \
             at 129): {:?}",
            nv.contiguity
        );
        assert_eq!(
            nv.imag_optical_beat_pass(),
            Some(true),
            "the optical read is LIVE and copy-free (isolated dup, no freeze): {nv:?}"
        );
        assert!(
            nv.is_zero(),
            "a beat skip balanced by an isolated dup + a clean digital burn must be ZERO loss \
             (#580v2): {:?}",
            nv
        );
    }

    #[test]
    fn node_verdict_for_imag_optical_beat_frozen_read_fails_580() {
        use super::node_verdict_for_imag;
        // The camera stuck on ONE painted QR value for the entire window — tick range collapses
        // to a single value. The OLD strict-step-1 check ALSO false-passed this (trivially
        // "contiguous", nothing missing in a span of one) — #580's advance-guard must now FAIL it.
        let frames: Vec<RecordingFrame> = (0..IMAG_WINDOW_FRAMES)
            .map(|i| frame(i as u64, &[(CAM2, 500)]))
            .collect();
        let nv = node_verdict_for_imag(&frames, None);
        assert!(
            nv.contiguity.is_contiguous(),
            "sanity: the RAW strict check trivially (and wrongly) calls a single stuck value \
             contiguous: {:?}",
            nv.contiguity
        );
        assert!(
            !nv.is_zero(),
            "a frozen/stuck optical read must NEVER pass zero-loss, even though the OLD strict \
             step-1 check vacuously passed it (#580 closes this pre-existing hole): {:?}",
            nv
        );
    }

    #[test]
    fn node_verdict_for_imag_dropped_frame_no_burn_still_fails_580v2() {
        use super::node_verdict_for_imag;
        // A genuinely dropped frame (index 30, painted tick 130 never captured) with NO digital
        // burn. #580v2: a lone distributed skip is NOT a copy/freeze, so the optical VALIDITY gate
        // passes it — but the recording carries no burn, so the node FAILS fail-closed (#585). On a
        // real recording a dropped OUTPUT frame also loses its burn id, so the burn gap catches the
        // same drop; either way a real drop can never read as zero loss.
        let frames = imag_window(Some(30));
        let nv = node_verdict_for_imag(&frames, None);
        assert!(
            !nv.is_zero(),
            "a real drop must still FAIL — here fail-closed via the absent burn (#580v2): {:?}",
            nv
        );
    }

    #[test]
    fn node_verdict_for_imag_optical_beat_net_zero_but_digital_burn_missing_still_fails_580() {
        use super::node_verdict_for_imag;
        // #580 + #463 combined: the cam2 optical BEAT is net-zero (the SAME compensated stutter as
        // `node_verdict_for_imag_optical_beat_compensated_skip_is_zero_loss_580`, so the optical
        // signal alone would PASS) — but imag's OWN digital corner burn (a SECOND, independent
        // proof, #463) is genuinely missing a step-grid id elsewhere. The digital gate must still
        // bite: #580 must never let a net-zero optical beat paper over a real burn gap.
        let frames: Vec<RecordingFrame> = (0..IMAG_WINDOW_FRAMES)
            .map(|i| {
                let tick = if i == 30 { 100 + i - 1 } else { 100 + i };
                let mut payloads = vec![(CAM2, tick)];
                if i != 40 {
                    payloads.push((super::BURN_RUN_ID_IMAG, 10 + 2 * i));
                }
                frame(i as u64, &payloads)
            })
            .collect();
        let nv = node_verdict_for_imag(&frames, None);
        assert_eq!(
            nv.imag_optical_beat_pass(),
            Some(true),
            "sanity: the optical BEAT itself is net-zero and would pass alone: {nv:?}"
        );
        let burn = nv.imag_burn_contiguity.as_ref().expect("burn decoded");
        assert!(
            !burn.is_contiguous(),
            "sanity: the digital burn has a genuine gap at the step-grid id: {burn:?}"
        );
        assert!(
            !nv.is_zero(),
            "a genuinely missing digital-burn step-grid id must fail the node even when the cam2 \
             optical BEAT is net-zero (#580 must not weaken the #463 digital gate): {:?}",
            nv
        );
        // #580 review: the raw strict-step-1 "missing ids" print must NOT co-print alongside the
        // burn-broken message once the optical beat itself passed (it would misleadingly blame
        // the optical read for the burn's own failure).
        let lines = super::node_verdict_lines(&nv, true);
        assert!(
            lines.iter().any(|l| l.contains("digital corner burn")),
            "the burn-broken fault line must print: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("missing id(s) (ids")),
            "the RAW optical missing-ids line must NOT co-print once the beat passed: {lines:?}"
        );
    }

    #[test]
    fn node_verdict_for_imag_frozen_read_prints_a_failure_reason_not_silence_580() {
        use super::node_verdict_for_imag;
        // #580 review finding 1 — a frozen/stuck optical read is `is_zero() == false` (advance-guard
        // fails) BUT its RAW strict `missing_ids` is EMPTY (a single value 500..=500 is trivially
        // "contiguous"). The old printer fell through `missing_ids.is_empty()` and returned an EMPTY
        // Vec — a node the headline FAILS printed NOTHING. It must now print an honest reason.
        let frames: Vec<RecordingFrame> = (0..IMAG_WINDOW_FRAMES)
            .map(|i| frame(i as u64, &[(CAM2, 500)]))
            .collect();
        let nv = node_verdict_for_imag(&frames, None);
        assert!(!nv.is_zero(), "sanity: a frozen read must fail: {nv:?}");
        let lines = super::node_verdict_lines(&nv, true);
        assert!(
            !lines.is_empty(),
            "a FAILING node must never print an empty (silent) verdict (#580 finding 1): {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("did NOT advance") && l.contains("NOT zero")),
            "the frozen-read failure must be explained honestly: {lines:?}"
        );
    }

    #[test]
    fn node_verdict_for_imag_copy_freeze_prints_a_copy_reason_580v2() {
        use super::node_verdict_for_imag;
        // #580v2 — a partial content FREEZE: cam2 repeats tick 124 for 20 frames (a long Δ0 run),
        // otherwise advancing, WITH a clean digital burn — so the ONLY failure is the optical copy
        // (the burn free-runs on imag's OWN render, blind to an UPSTREAM content freeze). The
        // whole-window aggregates (surplus/avg_step) cannot see it; the run-length term must, and the
        // printer must explain it honestly as a copy/freeze — never fall silent or misattribute it.
        let frames: Vec<RecordingFrame> = (0..IMAG_WINDOW_FRAMES)
            .map(|i| {
                let tick = if (25u32..45).contains(&i) {
                    124
                } else {
                    100 + i
                };
                frame(
                    i as u64,
                    &[(CAM2, tick), (super::BURN_RUN_ID_IMAG, 10 + 2 * i)],
                )
            })
            .collect();
        let nv = node_verdict_for_imag(&frames, None);
        assert_eq!(
            nv.imag_optical_beat_pass(),
            Some(false),
            "the 20-frame Δ0 copy run must fail the optical no-copy gate: {nv:?}"
        );
        assert!(!nv.is_zero(), "a content freeze must fail the node: {nv:?}");
        let lines = super::node_verdict_lines(&nv, true);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("COPY/FREEZE") && l.contains("NOT zero")),
            "a content freeze must be explained as a copy/freeze, honestly: {lines:?}"
        );
    }

    #[test]
    fn node_verdict_for_imag_beat_compensated_pass_line_is_honest_not_contiguous_580() {
        use super::node_verdict_for_imag;
        // #580 review finding B — a beat-compensated PASS (skip 130 balanced by dup 129) is
        // `is_zero()` but NOT strictly contiguous. The PASS line must NOT claim "CONTIGUOUS" (a tick
        // IS genuinely missing from the value range); it must report the beat compensation honestly.
        // #580v2: paired with a clean burn (now the sole delivery authority).
        let frames = imag_window_beat_with_burn();
        let nv = node_verdict_for_imag(&frames, None);
        assert!(
            nv.is_zero() && !nv.contiguity.is_contiguous(),
            "sanity: {nv:?}"
        );
        let lines = super::node_verdict_lines(&nv, true);
        let pass_line = lines
            .iter()
            .find(|l| l.contains("ZERO loss"))
            .expect("a passing node prints a ZERO loss line");
        // #580v2 — the v2 pass line has TWO parts: the OPTICAL read description, then an
        // ` AND digital corner burn CONTIGUOUS (...)` note (the burn is the delivery authority and
        // IS genuinely contiguous here — an honest claim). This test's intent is only that the
        // OPTICAL read must NOT be falsely called contiguous when a tick is genuinely missing, so
        // scope the check to the optical portion (before the burn note), never the whole line.
        let optical_portion = pass_line
            .split(" AND digital corner burn")
            .next()
            .unwrap_or(pass_line);
        assert!(
            !optical_portion.contains("CONTIGUOUS"),
            "a beat-compensated pass must NOT falsely claim a contiguous OPTICAL read: {pass_line}"
        );
        assert!(
            optical_portion.contains("BEAT compensation"),
            "the pass line must report the beat compensation honestly: {pass_line}"
        );
    }

    #[test]
    fn node_verdict_json_beat_compensated_pass_is_self_consistent_580() {
        use super::{node_verdict_for_imag, node_verdict_json};
        // #580 review finding C — for a beat-compensated pass, JSON must not read as internally
        // inconsistent: `zero_loss: true` beside a non-empty `missing_ids` now carries the beat
        // fields that EXPLAIN it (`imag_optical_beat_net_zero: true`, `surplus <= 0`).
        // #580v2: paired with a clean burn (now the sole delivery authority).
        let frames = imag_window_beat_with_burn();
        let nv = node_verdict_for_imag(&frames, None);
        let j = node_verdict_json(&nv, 300.0, true, 300.0);
        assert_eq!(j["zero_loss"], serde_json::json!(true), "{j}");
        assert!(
            !nv.contiguity.missing_ids.is_empty(),
            "sanity: this pass carries a raw strict gap: {nv:?}"
        );
        assert_eq!(
            j["imag_optical_beat_net_zero"],
            serde_json::json!(true),
            "the beat field must explain the zero_loss=true beside a non-empty missing_ids: {j}"
        );
        assert!(
            j["imag_optical_beat_surplus"].as_i64().unwrap() <= 0,
            "a net-zero beat must report surplus <= 0: {j}"
        );
    }

    #[test]
    fn args_expected_burns_for_imag_returns_its_own_digital_burn_463() {
        use super::Args;
        use clap::Parser;
        let args = Args::parse_from(["recording-verdict"]);
        assert_eq!(
            super::args_expected_burns_for("imag", &args),
            Some(vec![super::BURN_RUN_ID_IMAG]),
            "imag now carries its OWN digital corner burn (#463) — the optical tick fallback \
             still applies at the NodeVerdict level for a recording with no burn decoded"
        );
    }

    #[test]
    fn build_and_print_verdict_computes_the_imag_node_independently_of_strih_stream_461() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;

        // --min-secs 1 so the 60-frame @60fps window (1s) trivially clears the #373 floor.
        let args = super::Args::parse_from(["recording-verdict", "--min-secs", "1"]);
        // #580v2: carry a clean digital burn — the burn is now imag's sole delivery authority, so a
        // no-burn recording would (correctly) fail fail-closed (#585).
        let imag_frames = imag_window_with_burn(None);

        // NEITHER --strih NOR --stream supplied — imag must still be gated on its own.
        let (v, pass) = build_and_print_verdict(
            &args,
            None,
            None,
            Cam1Source::Absent,
            None,
            None,
            Some(DecodedRec {
                frames: imag_frames,
                rec_path: None,
            }),
            None, // #312 item 2 (PR A): no carried A/V-sync inputs in this test
        )
        .expect("verdict");

        assert!(
            pass,
            "#461: a contiguous imag recording alone (no strih/stream) must PASS: {v}"
        );
        assert_eq!(
            v["full_chain"]["loss"]["imag"]["zero_loss"],
            serde_json::json!(true),
            "imag's tick-contiguity zero-loss must be reported: {}",
            v["full_chain"]["loss"]["imag"]
        );
    }

    #[test]
    fn build_and_print_verdict_fails_when_imag_has_a_missing_tick_461() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;

        let args = super::Args::parse_from(["recording-verdict", "--min-secs", "1"]);
        let imag_frames = imag_window(Some(15));

        let (v, pass) = build_and_print_verdict(
            &args,
            None,
            None,
            Cam1Source::Absent,
            None,
            None,
            Some(DecodedRec {
                frames: imag_frames,
                rec_path: None,
            }),
            None, // #312 item 2 (PR A): no carried A/V-sync inputs in this test
        )
        .expect("verdict");

        assert!(!pass, "#461: a missing imag optical tick must FAIL: {v}");
        assert_eq!(
            v["full_chain"]["loss"]["imag"]["zero_loss"],
            serde_json::json!(false)
        );
        assert_eq!(
            v["full_chain"]["loss"]["imag"]["missing_ids"],
            serde_json::json!([115])
        );
    }

    #[test]
    fn run_merge_accepts_an_imag_partial_461() {
        use super::run_merge;
        use camera_box::probe::recording_partial::RecordingPartial;
        use clap::Parser;
        use std::path::PathBuf;

        let dir = tempfile::tempdir().unwrap();
        // #580v2: the imag partial must carry a clean digital burn (declared in `expected_burns`) —
        // the burn is now imag's SOLE delivery authority, so a no-burn recording fails fail-closed
        // (#585) and `run_merge` would exit the PROCESS on that FAIL verdict, aborting the test
        // binary. A burn-carrying, contiguous imag partial PASSES so this call returns normally.
        let imag_frames = imag_window_with_burn(None);
        let imag_p = RecordingPartial::from_frames(
            "imag",
            &PathBuf::from("imag.mkv"),
            &[super::BURN_RUN_ID_IMAG],
            imag_frames,
        );
        let imag_path = dir.path().join("imag-partial.json");
        imag_p.save(&imag_path).unwrap();
        let spec = format!("imag={}", imag_path.display());

        let args = super::Args::parse_from([
            "recording-verdict",
            "--min-secs",
            "1",
            "--merge-partials",
            spec.as_str(),
        ]);
        // run_merge exits the PROCESS on a FAIL verdict (std::process::exit) — a contiguous imag
        // partial must PASS so this call returns normally instead of aborting the test binary.
        run_merge(&args).expect("a contiguous imag-only merge must not error");
    }

    #[test]
    fn run_merge_rejects_an_unknown_box_name_including_non_imag_461() {
        use super::run_merge;
        use camera_box::probe::recording_partial::RecordingPartial;
        use clap::Parser;
        use std::path::PathBuf;

        let dir = tempfile::tempdir().unwrap();
        let weird_p = RecordingPartial::from_frames("weird", &PathBuf::from("x.mkv"), &[], vec![]);
        let weird_path = dir.path().join("weird-partial.json");
        weird_p.save(&weird_path).unwrap();
        let spec = format!("weird={}", weird_path.display());
        let args =
            super::Args::parse_from(["recording-verdict", "--merge-partials", spec.as_str()]);
        let err = run_merge(&args).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("unknown box") && msg.contains("imag"),
            "#461: the unknown-box error must mention imag as a valid option: {msg}"
        );
    }

    #[test]
    fn cli_parses_the_imag_flag_and_its_capture_fps_461() {
        use super::Args;
        use clap::Parser;
        let args = Args::parse_from([
            "recording-verdict",
            "--imag",
            "/tmp/imag-1234.mkv",
            "--imag-capture-fps",
            "45",
        ]);
        assert_eq!(
            args.imag,
            Some(std::path::PathBuf::from("/tmp/imag-1234.mkv"))
        );
        assert_eq!(args.imag_capture_fps, 45.0);

        // Default imag_capture_fps is 60 (imag-nb is a 60fps low-latency IMAG box) when omitted.
        let defaults = Args::parse_from(["recording-verdict"]);
        assert_eq!(defaults.imag, None);
        assert_eq!(defaults.imag_capture_fps, 60.0);
    }
}
