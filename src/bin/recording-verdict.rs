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
use camera_box::probe::burn_contiguity::{
    burn_contiguity_in_window_with_step, BurnRate, InWindowMissingKind, NodeContiguity,
    RecordedBurnFrame,
};
use camera_box::probe::recording::{
    analyze_recording_with_burns, extract_frames_png, select_frames_to_extract, RecordingFrame,
    DEFAULT_MAX_PIXEL_PROOF,
};
use camera_box::probe::recording_latency::{
    burn_ids_in, cam2_cam1_samples, cam2_cam1_samples_from_burn, cam2_cam1_samples_from_flip,
    cam_strih_samples, chain_hop_samples_from_stream, hop_latency, painter_internal_gen_to_flip,
    per_frame_latency_csv_rows, strih_stream_samples, strih_stream_samples_from_stream,
    write_latency_csv, HopLatency, RunIds, BURN_RUN_ID_CAM1, BURN_RUN_ID_CAM3, BURN_RUN_ID_CAM4,
    BURN_RUN_ID_STREAM, BURN_RUN_ID_STRIH,
};
use camera_box::probe::recording_partial::RecordingPartial;
use camera_box::probe::recording_segments::{
    load_switch_schedule, segment_continuity, SegmentFrame, DEFAULT_TRANSITION_GUARD_NS,
};
use camera_box::probe::recording_verdict::{
    cam_strih_assessment, verdict, FrameTick, RecordingVerdict, VerdictConfig,
};
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
    /// #461 imag-nb OBS-program recording (EPIC #466 Topology v2, the new 60fps low-latency
    /// IMAG box). imag has NO digital node-burn yet (911003 is reserved for it, #463) — its
    /// zero-loss proof is instead the cam2 OPTICAL tick's own first..=last contiguity (no
    /// 60→30 beat: imag captures the 60Hz painter 1:1 at 60fps). Independent of --strih/--stream;
    /// may be supplied alone or alongside them.
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
    /// absent, cam3 is silently skipped exactly like cam1 is today when its burn is off. Default
    /// mirrors cam3's reserved id.
    #[arg(long, default_value_t = BURN_RUN_ID_CAM3)]
    burn_cam3_run_id: u32,
    /// #24: cam4's capture-burn run_id. See `--burn-cam3-run-id`.
    #[arg(long, default_value_t = BURN_RUN_ID_CAM4)]
    burn_cam4_run_id: u32,
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
    /// are rejected. Default 60.
    #[arg(long, default_value_t = 60.0)]
    av_cluster_tol_ms: f64,
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

/// #24 — the node labels that occupy the "camera under test" role: whichever ONE of the four
/// physical source cameras is deployed with `CAMERA_BOX_BURN_RUN_ID` set this run (mutually
/// exclusive — a real run only ever has ONE producing a non-empty id set). Both the clean-source
/// selection (#133, `cam1_source`/`cam1_rec_path`) and the #356 cross-recording reconciliation
/// apply identically to any of them; strih/stream never do.
const CAMERA_UNDER_TEST_NODES: [&str; 3] = ["cam1", "cam3", "cam4"];

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
}

impl NodeVerdict {
    /// ZERO loss ⇔ the burn-id sequence is contiguous (no missing id — a BURN-UNREADABLE missing
    /// id is a real DEFECT and still makes the node NOT-zero, never silently excluded) AND the
    /// cam2 OPTICAL read is complete across the span WITHIN the calibrated moiré floor (#376:
    /// [`Self::optical_undecodable_ok`]). The optical read is still the HARD gate — a run where
    /// the filmed dual-QR went undecodable at a rate ABOVE [`OPTICAL_UNDECODABLE_RATE_MAX`] FAILS
    /// even if every node's digital burn is present (reverts the #360 burn-only weakening); only
    /// the rig's PROVEN optical-physics floor (#376) is tolerated, never a genuine read failure.
    fn is_zero(&self) -> bool {
        self.contiguity.is_contiguous() && self.optical_undecodable_ok() && self.colour_fail == 0
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
    /// #11 mixed 60/30 → #360 REVISED: the by-design per-recorded-frame burn-id step for a
    /// [`BurnRate::PerRenderTick`] node — the DECIMATION factor used by the step>=2 excess-gap
    /// charging in [`burn_contiguity_in_window_with_step`]. That charging is RETAINED for a
    /// genuinely-clean-decimation hop, but NO current node feeds it `>= 2` (see [`node_render_step`]):
    /// strih's burn turned out to be a FREE-RUNNING render tick with an IRREGULAR step (not a clean
    /// 2), so it uses gap-ignore (`1`) like the stream burn. Ignored for cam1 (PerEmittedFrame,
    /// set-based). See [`node_render_step`].
    step: i64,
}

/// #11 → #360: the per-recorded-frame burn-id step for a [`BurnRate::PerRenderTick`] node. The
/// decimation-aware excess-gap charging in [`burn_contiguity_in_window_with_step`] (a forward gap
/// `> step` charges the excess as a real drop) is CORRECT only when the node's burn steps by a
/// CLEAN integer per recorded frame. The rig data refutes that for strih:
///
/// - **strih** is a FREE-RUNNING DistroAV render-tick, NOT a per-output-frame counter. Read from the
///   30fps stream recording its per-frame step is IRREGULAR (run 354003: 0–10, mean ~4 — NOT the
///   assumed `round(60/30) = 2`), so a forward gap is render-clock jitter, not a lost frame: EVERY
///   strih gap > 8 on 354003 coincided with a CLEAN stream-burn step (the stream burn never gapped
///   ⇒ zero stream-output loss). The old strih=2 charging therefore manufactured ~17 300 phantom
///   REAL DROPs. So strih now uses gap-ignore (`1`): a delivered frame MISSING its strih burn is
///   still BURN-UNREADABLE (FAILS), and real loss is caught by the stream burn (per-output-frame)
///   plus cam1 (per-emitted). A strih→stream NDI content-hold loss shows as a SMALL strih step (a
///   held frame), never the large gap the old code charged — that detection belongs to the
///   per-frame continuity reconciliation (#356), not this free-running-tick gap math.
/// - **stream** is emitted AND recorded by the same stream OBS ⇒ `1` (no decimation).
/// - **cam1** is PerEmittedFrame (set-based) ⇒ its step is never consulted; returns `1` harmlessly.
///
/// `strih_emit_fps` / `stream_capture_fps` stay on the CLI (and are read here) for provenance and
/// the separate OPTICAL diagnostic step; they no longer drive the strih loss step.
fn node_render_step(node: &str, strih_emit_fps: f64, stream_capture_fps: f64) -> i64 {
    // Read the rig-pinned fps for provenance; no current node is a clean integer decimation, so
    // every node uses gap-ignore (see the docstring above for why strih is NOT a clean step-2).
    let _ = (node, strih_emit_fps, stream_capture_fps);
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
        let gen_ts = anchor_run_ids
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
            });
        match gen_ts {
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
fn node_verdict_with_optical(
    spec: &NodeSpec,
    all_burn_run_ids: &[u32],
    optical: OpticalSpanFacts,
    out_dir: &Path,
    max_pixel_proof: usize,
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
    let in_window = burn_contiguity_in_window_with_step(node, &window, spec.rate, spec.step);
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
    })
}

/// #461 — build imag-nb's verdict from its OWN recording (EPIC #466 Topology v2). imag has no
/// digital node-burn yet (911003 is reserved for a later ticket, #463), so its zero-loss proof
/// is the cam2 OPTICAL tick's own first..=last contiguity instead: imag captures the 60Hz
/// painter 1:1 at 60fps with NO 60→30 beat, so a missing tick VALUE in the analyzed span means
/// imag's camera failed to capture that instant — the digital-burn equivalent of a candidate
/// dropped frame, applied to the optical tick.
///
/// Sibling of [`node_verdict_with_optical`] but structurally simpler: imag has no [`NodeSpec`]
/// (no `burn_run_id`), no pixel-proof extraction (out of scope for this ticket — the frame
/// indices ARE known via [`RecordingFrame::frame_index`], a future ticket can wire it the same
/// way [`node_verdict_with_optical`] does), and no colour gate (not wired for imag yet). The
/// tick-contiguity ARITHMETIC itself is the Tier-0 pure [`camera_box::imag_tick_gate::
/// tick_contiguity`] — this function is the thin probe-gated glue that extracts
/// [`RecordingFrame::tick`] and converts the result into the SAME [`NodeContiguity`] /
/// [`NodeVerdict`] shape every other node uses, so `is_zero()` / `print_node_verdict` /
/// `node_verdict_json` all work UNCHANGED for a node with no burn.
fn node_verdict_for_imag(frames: &[RecordingFrame], cam2_run_id: Option<u32>) -> NodeVerdict {
    // imag carries NO node burn at all, so every CRC-valid non-burn payload is cam2's optical
    // paint — mirrors `frame_is_delivered_optical`'s "no burns to exclude" with an empty set.
    let optical = optical_span_facts(frames, &[], cam2_run_id);
    let ticks: Vec<u32> = frames.iter().filter_map(|f| f.tick).collect();
    let tc = camera_box::imag_tick_gate::tick_contiguity(&ticks);
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
        lines.push(format!(
            "  [{}] ZERO loss — burn-id sequence CONTIGUOUS ({span}) AND cam2 optical read complete.",
            c.node
        ));
        return lines;
    }
    // #374 nit 2 — whether a SPECIFIC fault line (colour / optical) already explained the failure.
    let mut explained = false;
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
    // (strih: cam1+strih; stream: all three; cam1 grab: cam1 only) for the #207 fast gate.
    let strih = decode_for(
        args.strih.as_deref(),
        &[args.burn_cam1_run_id, args.burn_strih_run_id],
    )?;
    let stream = decode_for(
        args.stream.as_deref(),
        &[
            args.burn_cam1_run_id,
            args.burn_strih_run_id,
            args.burn_stream_run_id,
        ],
    )?;
    // #461: the imag recording carries NO node burn (911003 is reserved for a later ticket,
    // #463) — its zero-loss proof is the cam2 optical tick's own contiguity instead.
    let imag = decode_for(args.imag.as_deref(), &[])?;
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

    // Fused path: no carried colour — `build_node_colour_fail` samples each node's recording directly.
    let (_report, all_pass) =
        build_and_print_verdict(&args, strih, stream, cam1, None, None, imag)?;
    if !all_pass {
        std::process::exit(1);
    }
    Ok(())
}

/// Build the full-chain verdict + print it + write the `--json` report, returning the report
/// JSON and the binary PASS. Operates on ALREADY-DECODED frames so the fused path (live decode)
/// and #208 merge path (deserialized per-box partials) share IDENTICAL logic — the merged
/// verdict is therefore equivalent to the fused output (same fields, same PASS semantics). The
/// ONLY recording-dependent step is pixel-proof PNG extraction, skipped when a `DecodedRec` has
/// no `rec_path` (merge mode); the contiguity/PASS gate is pure and unaffected.
#[allow(clippy::too_many_arguments)]
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
        // #24 — cam3/cam4 occupy the SAME "camera under test" role as cam1 (mutually exclusive in
        // any real run: only the ONE camera actually deployed with CAMERA_BOX_BURN_RUN_ID set
        // produces a non-empty id set here), so they read from the SAME clean source
        // (`cam1_source`, #133) with their OWN reserved burn run_id.
        let cam3_ids = burn_ids_in(cam1_source, args.burn_cam3_run_id);
        let cam4_ids = burn_ids_in(cam1_source, args.burn_cam4_run_id);
        let strih_ids_seq = burn_ids_in(stream_frames, args.burn_strih_run_id);
        let stream_ids_seq = burn_ids_in(stream_frames, args.burn_stream_run_id);
        let any_burn = !cam1_ids.is_empty()
            || !cam3_ids.is_empty()
            || !cam4_ids.is_empty()
            || !strih_ids_seq.is_empty()
            || !stream_ids_seq.is_empty();
        if any_burn {
            println!();
            println!(
                "=== #174 FULL-CHAIN per-hop verdict (camera-under-test from the {cam1_source_label}; strih/stream from the stream recording) ==="
            );
            println!(
                "  burn ids: cam1={} cam3={} cam4={} (from {cam1_source_label}) strih={} stream={} (stream recording)",
                cam1_ids.len(),
                cam3_ids.len(),
                cam4_ids.len(),
                strih_ids_seq.len(),
                stream_ids_seq.len()
            );
            report["full_chain"]["burn_ids_present"] = serde_json::json!({
                "cam1": cam1_ids.len(), "cam3": cam3_ids.len(), "cam4": cam4_ids.len(),
                "strih": strih_ids_seq.len(), "stream": stream_ids_seq.len(),
            });
            report["full_chain"]["cam1_source"] = serde_json::json!(cam1_source_label);
            // #133 (review, #24 generalized): if --strih was supplied (so the camera-under-test's
            // source IS the strih recording) but NONE of cam1/cam3/cam4 carried a burn there, the
            // camera leg is silently SKIPPED below and an all-zero headline could stand WITHOUT the
            // camera having been measured. The capture burn (CAMERA_BOX_BURN_RUN_ID on whichever
            // camera is under test) rides into strih's program, so its absence in a --strih run
            // means the burn was OFF or never reached strih — loudly WARN so a "ZERO loss" headline
            // is never read as a camera→strih proof when the camera was unmeasured. (No hard fail:
            // a deliberate burn-off / strih+stream-only diagnostic run is still valid.)
            let camera_under_test_measured =
                !cam1_ids.is_empty() || !cam3_ids.is_empty() || !cam4_ids.is_empty();
            if strih_data.is_some() && !camera_under_test_measured {
                eprintln!(
                    "WARNING: --strih supplied but NO camera-under-test burn found in the strih \
                     recording (checked cam1={}, cam3={}, cam4={}) — the camera→strih hop is \
                     UNMEASURED this run (burn OFF or not reaching strih). A ZERO-loss headline \
                     below covers strih/stream ONLY.",
                    args.burn_cam1_run_id, args.burn_cam3_run_id, args.burn_cam4_run_id
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
                args.burn_cam3_run_id,
                args.burn_cam4_run_id,
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
                        // cam1 is set-based (PerEmittedFrame) — step is never consulted.
                        step: node_render_step(
                            "cam1",
                            args.strih_emit_fps,
                            args.stream_capture_fps,
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
                        ),
                    },
                    !cam4_ids.is_empty(),
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
                        // #11: the 60fps strih burn read from the 30fps stream recording ⇒ step 2
                        // (decimation-aware — a gap > 2 charges the excess as a real drop).
                        step: node_render_step(
                            "strih",
                            args.strih_emit_fps,
                            args.stream_capture_fps,
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

    // cam2→cam1 LOSS = cam1's V4L2 CAPTURE-DROP count (the camera leg: cam2 monitor → cam1
    // lens → cam1 V4L2 capture). A dropped capture = a lost frame on that leg — the kernel
    // `sequence` gap the camera-box tracks (capture.rs), NOT a painter-tick optical compare
    // (which the 60→30 genlock decimation confounds, flagging present readable frames as
    // lost). The burn-id contiguity above covers the DIGITAL chain from cam1's EMITTED frame
    // onward (cam1 burn increments per emit, after the genlock gate), so it cannot see a
    // capture drop UPSTREAM of the burn — this sidecar is that separate signal.
    //
    // Run at TOP LEVEL (not nested under the full-chain burn block): the cam2→cam1 loss
    // depends ONLY on --cam1-capture-stats, so a supplied gate flag is ALWAYS parsed + gated
    // and a missing/malformed file ALWAYS errors — even when --stream is absent or the stream
    // carried no burns (otherwise a supplied capture-drop sidecar showing real drops could be
    // silently ignored while OVERALL printed ZERO loss).
    if let Some(stats_path) = &args.cam1_capture_stats {
        let stats = parse_cam1_capture_stats(stats_path)?;
        let cam1_zero = stats.v4l2_dropped == 0;
        if cam1_zero {
            println!(
                "  [cam2→cam1] ZERO loss — cam1 V4L2 capture dropped 0 frames ({} captured).",
                stats.frames_captured
            );
        } else {
            // Denominator is the TOTAL the device should have produced = delivered + dropped
            // (frames_captured counts only delivered buffers, not the lost ones).
            let total = stats.frames_captured.saturating_add(stats.v4l2_dropped);
            println!(
                "  [cam2→cam1] NOT zero — cam1 V4L2 capture dropped {} of {} frames \
                 ({} delivered; REAL capture-card drops on the camera leg).",
                stats.v4l2_dropped, total, stats.frames_captured
            );
        }
        all_pass &= cam1_zero;
        report["full_chain"]["loss"]["cam2_cam1"] = serde_json::json!({
            "zero_loss": cam1_zero,
            "v4l2_dropped": stats.v4l2_dropped,
            "frames_captured": stats.frames_captured,
            "source": "cam1 V4L2 sequence-gap capture-drop (camera leg) — not a painter-tick compare",
        });
    }

    // #461 — imag-nb (EPIC #466 Topology v2) has no digital node-burn yet (911003 is reserved
    // for a later ticket, #463); its zero-loss proof is the cam2 OPTICAL tick's own first..=last
    // contiguity instead (imag captures the 60Hz painter 1:1 at 60fps — no 60→30 beat). Run at
    // TOP LEVEL (like the cam2→cam1 capture-stats gate above): --imag is INDEPENDENT of
    // --strih/--stream and must be gated whether or not either is supplied.
    //
    // NOTE: the #312 ALL-CAMBOX --switch-schedule sweep below does NOT yet cover imag frames —
    // extending it needs its own anchor mode (imag has no burn to anchor a schedule window on,
    // unlike the strih/stream burns `segment_frames_from_recording` uses today) and is tracked
    // as a follow-up, not blocking the standard (non-sweep) zero-loss verdict this ticket adds.
    if let Some(d) = imag {
        let imag_frames = d.frames;
        let nv = node_verdict_for_imag(&imag_frames, args.cam2_pin());
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
        report["full_chain"]["loss"]["imag"] =
            node_verdict_json(&nv, span_secs, span_ok, cfg.min_secs);
    }

    // #312 Phase-1 — ALL-CAMBOX per-segment continuity (the all-active splitter proof). When a
    // switch schedule is supplied, partition the SINGLE continuous stream recording into the per-
    // cambox program windows (by burn gen_ts_ns, minus the transition guard on each boundary) and
    // verify the painted-tick continuity PER cambox. Gates the headline alongside the per-node burn
    // verdict so a single cambox dropping in ITS ~30s window fails the run.
    if let Some(schedule_path) = &args.switch_schedule {
        match &stream_frames_opt {
            Some(stream_frames) => {
                let schedule = load_switch_schedule(schedule_path)?;
                // The painted tick's by-design step in the stream recording = the decimation of the
                // 60Hz painter at the recording rate (refresh_hz / stream_capture_fps = 2). Derived
                // from the configured fps when --switch-expected-step is 0, else the explicit value.
                let expected_step = if args.switch_expected_step > 0 {
                    args.switch_expected_step
                } else if args.stream_capture_fps > 0.0 {
                    (args.refresh_hz / args.stream_capture_fps).round().max(1.0) as i64
                } else {
                    1
                };
                let anchor_run_ids = [args.burn_strih_run_id, args.burn_stream_run_id];
                let all_burns = [
                    args.burn_cam1_run_id,
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
                    segment_continuity(&seg_frames, &schedule, args.switch_guard_ns, expected_step);
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
///       - strih box → cam1 (its burn is read from the clean 1080p strih recording, #133),
///       - stream box → strih + stream (their burns are read from the stream recording).
/// `undecodable` is the undecodable subset (so `extract_frames_png` runs its sharp-but-flagged
/// self-check on those frames). PURE (no I/O) so the selection is unit-testable; the PNG write is
/// the thin `extract_frames_png` glue in [`extract_partial`].
fn extract_partial_flagged_frames(
    box_name: &str,
    frames: &[RecordingFrame],
    args: &Args,
) -> (Vec<u64>, HashSet<u64>) {
    let all_burns = [
        args.burn_cam1_run_id,
        args.burn_strih_run_id,
        args.burn_stream_run_id,
    ];
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
    // backs cam1 (its burn is crispest in the clean 1080p strih recording); the stream box backs
    // strih + stream (their own burns are co-located with cam2's optical QR only in the stream
    // recording). These are the SAME (node, source) pairings `build_and_print_verdict` uses, so the
    // missing slots — and thus the extracted PNG frame indices — match what the merge would flag.
    // #198: cam1's burn is per-EMITTED-frame (a forward gap is a real drop); strih/stream burn
    // per-RENDER-tick (a forward gap is not loss, but a delivered frame missing its burn is).
    // #360: the same step the merge verdict uses (node_render_step → gap-ignore for all current
    // nodes, since strih's free-running render tick is not a clean decimation), so the on-box
    // pixel-proof flagging matches what the merge flags.
    let strih_step = node_render_step("strih", args.strih_emit_fps, args.stream_capture_fps);
    let owned: &[(&str, u32, BurnRate, i64)] = match box_name {
        "strih" => &[("cam1", args.burn_cam1_run_id, BurnRate::PerEmittedFrame, 1)],
        "stream" => &[
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
        _ => &[],
    };
    // #273: thread the cam2 pin so the on-box pixel-proof flagging anchors the optical window to
    // THIS run's paint exactly as the merge verdict does (a foreign-run lead-in is not flagged as
    // delivered). `None` for an unpinned extract (e.g. the strih box runs without --cam2-run-id).
    let cam2_pin = args.cam2_pin();
    for &(node, burn_run_id, rate, step) in owned {
        let window = in_window_burn_frames(frames, burn_run_id, &all_burns, rate, cam2_pin);
        let iw = burn_contiguity_in_window_with_step(node, &window, rate, step);
        flagged.extend(iw.missing_slots.iter().map(|s| s.frame_index));
    }

    flagged.sort_unstable();
    flagged.dedup();
    (flagged, undecodable)
}

/// The node-burn run_ids a per-box partial is expected to carry, derived from the box name + the
/// `--burn-*-run-id` args: the strih recording carries cam1 (forwarded) + strih; the stream
/// recording (the chain endpoint) carries all three; the imag recording carries NONE (#461 —
/// imag-nb has no digital burn yet, its zero-loss proof is the cam2 optical tick's own
/// contiguity). `None` for an unknown box. SINGLE source of truth for BOTH `--extract-partial`
/// (what it decodes for) and the `--merge-partials` consistency check — so a manual
/// `--burn-*-run-id` mismatch between extract and merge cannot silently misverdict (run_merge
/// warns when a loaded partial's `expected_burns` disagree with this).
fn args_expected_burns_for(box_name: &str, args: &Args) -> Option<Vec<u32>> {
    match box_name {
        "strih" => Some(vec![args.burn_cam1_run_id, args.burn_strih_run_id]),
        "stream" => Some(vec![
            args.burn_cam1_run_id,
            args.burn_strih_run_id,
            args.burn_stream_run_id,
        ]),
        "imag" => Some(vec![]),
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
        // #461: the imag recording carries NO burns — its expected_burns is Some(vec![]) above.
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
    let frames = analyze_recording_with_burns(rec_path, &expected_burns)
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
    let partial = RecordingPartial::from_frames(box_name, rec_path, &expected_burns, frames)
        .with_colour(colour);
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
    // #461: imag has no burn slot to reconcile against `--burn-*-run-id` (its expected_burns is
    // always the empty set), so it needs no colour-carry either (colour-gate is not wired for
    // imag in this ticket).
    let mut imag: Option<DecodedRec> = None;
    // #377 — the per-recording colour summaries carried in each partial (Some only when the box
    // extracted with --colour-gate). Threaded into the verdict so the colour gate works through the
    // split decode path (the gate is fused/on-host — the recording is only on the box).
    let mut strih_colour: Option<camera_box::colour_verify::NodeColourSummary> = None;
    let mut stream_colour: Option<camera_box::colour_verify::NodeColourSummary> = None;
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
        // #377 — take the carried colour summary before `frames` moves into the DecodedRec.
        let colour = partial.colour;
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
        frame_is_delivered_optical, in_window_burn_frames, node_burn_id_on,
        node_verdict_with_optical, optical_span_facts, parse_cam1_capture_stats_str, parse_grab_ts,
        parse_painter_flip_str, parse_painter_ticks_str,
    };
    use camera_box::probe::burn_contiguity::{BurnRate, InWindowMissingKind};
    use camera_box::probe::payload::Payload;
    use camera_box::probe::recording::RecordingFrame;
    use std::collections::HashSet;
    use std::io::Write;

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
        node_verdict_with_optical(spec, all_burn_run_ids, optical, out_dir, max_pixel_proof)
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

    // ---- #24 — extend the #186 per-node digital-burn contiguity check to cam3/cam4 ----

    const CAM3B: u32 = 911003; // #24 cam3 per-EMIT capture burn run_id

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

    /// #24 — a REAL gap in cam3's digital burn sequence (absent from BOTH the strih and stream
    /// recordings, so it can never be reconciled as delivered-downstream) is classified a REAL
    /// DROP and FAILS the headline, exactly like a cam1 gap does today — the #186 gate is a
    /// genuine HARD gate for cam3, not a vacuous always-pass.
    #[test]
    fn cam3_digital_burn_gap_is_a_real_drop_and_fails_24() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;

        let args = super::Args::parse_from(["recording-verdict", "--min-secs", "1"]);
        let strih_frames = window_cam3(60, false, Some(30));
        let stream_frames = window_cam3(60, true, Some(30));

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
        )
        .expect("verdict");

        assert!(!pass, "#24: a cam3 burn gap must FAIL the headline: {v}");
        assert_eq!(
            v["full_chain"]["loss"]["cam3"]["zero_loss"],
            serde_json::json!(false),
            "#24: cam3 must be NOT zero when its burn sequence has a gap: {}",
            v["full_chain"]["loss"]["cam3"]
        );
        assert!(
            v["full_chain"]["loss"]["cam3"]["real_drops"]
                .as_u64()
                .unwrap_or(0)
                >= 1,
            "#24: the gap (absent from both recordings) must be classified a REAL DROP: {}",
            v["full_chain"]["loss"]["cam3"]
        );
    }

    /// #24 — the #356 cross-recording reconciliation (previously cam1-only) generalizes to cam3:
    /// a cam3 id classified REAL DROP from the (clean, upstream) strih recording but PROVEN
    /// delivered in the downstream stream recording is re-classified BURN-UNREADABLE, not REAL
    /// DROP — exactly as cam1 already does. Locks that generalizing the reconciliation condition
    /// did not silently drop this behaviour for a non-cam1 camera-under-test node.
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

        let args = super::Args::parse_from(["recording-verdict", "--min-secs", "1"]);
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
    /// clean ZERO-loss run AND a run with a real cam1 drop. This is the equivalence the per-box
    /// decode-in-place flow rests on: no recording is copied box-to-box, yet the verdict is
    /// identical.
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

        // REAL cam1 DROP: cam1's contiguity source is the STRIH recording (#133); a forward gap
        // (id 5005 missing from frame 5 on) is a real cam1 drop ⇒ NOT zero ⇒ FAIL. Merge agrees.
        // Same full-length span so the FAIL is the cam1 drop, not the #373 duration floor.
        // #356: the gap must ALSO be absent from the DOWNSTREAM stream recording, or the new
        // cross-recording reconciliation (correctly) reads the id as delivered-downstream and
        // re-classifies it BURN-UNREADABLE — which is not a REAL DROP. To keep this a GENUINE real
        // drop (absent from BOTH recordings, the case the reconciliation must never mask), inject
        // the SAME gap into the stream frames too. (The dedicated #356 reconciliation RED/GREEN +
        // SAFETY tests lock the downgrade and the never-mask invariant.)
        let (drop, drop_pass) = run_both(
            window(FULL_SPAN_FRAMES, false, Some(5)),
            window(FULL_SPAN_FRAMES, true, Some(5)),
        );
        assert!(!drop_pass, "#208: a real cam1 drop ⇒ overall FAIL");
        assert_eq!(drop["full_chain"]["zero_loss"], serde_json::json!(false));
        assert!(
            drop["full_chain"]["real_drops"].as_u64().unwrap() >= 1,
            "#208: the missing cam1 id must be classified as a REAL DROP: {}",
            drop["full_chain"]["real_drops"]
        );
    }

    /// #356 — cross-recording reconciliation. A cam1 id that is a REAL DROP in the (clean, upstream)
    /// strih recording but IS decoded in the DOWNSTREAM stream recording was delivered (the frame
    /// reached the stream) — the small cam1 burn was merely UNREADABLE in the strih recording at the
    /// high-latency 60→30 hop. It must be classified BURN-UNREADABLE, NOT REAL DROP, so the merge
    /// headline stops over-counting (the #356 residual cam1 over-count). Runs through the SHARED
    /// `build_and_print_verdict` (fused == merge), so the merge production flow gets it identically.
    #[test]
    fn cam1_real_drop_present_downstream_is_burn_unreadable_not_real_drop_356() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;
        // --min-secs 1 so the small contiguous window trivially clears the #373 span floor.
        let args = super::Args::parse_from(["recording-verdict", "--min-secs", "1"]);
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

    /// #356 SAFETY (the #1 invariant): a cam1 id ABSENT from BOTH the strih AND the downstream stream
    /// recording is a GENUINE chain loss and MUST stay REAL DROP — the reconciliation must NEVER mask
    /// it (no false ZERO). This test FAILS if the fix is too aggressive (downgrades an unproven id).
    #[test]
    fn cam1_real_drop_absent_from_both_stays_real_drop_356() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;
        let args = super::Args::parse_from(["recording-verdict", "--min-secs", "1"]);
        const N: u32 = 600;
        // cam1 id 5005 missing from BOTH the strih AND the stream recording ⇒ genuine loss.
        let strih = window(N, false, Some(5));
        let stream = window(N, true, Some(5));
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
        )
        .expect("verdict");
        assert!(!pass, "#356: a genuine cam1 loss ⇒ overall FAIL");
        assert_eq!(v["full_chain"]["zero_loss"], serde_json::json!(false));
        assert!(
            v["full_chain"]["real_drops"].as_u64().unwrap() >= 1,
            "#356 SAFETY: a cam1 id absent from BOTH recordings MUST stay REAL DROP — never masked: {}",
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

    /// #208 (review): the per-box expected burns are ONE source of truth shared by extract and the
    /// merge consistency check — strih carries cam1+strih, stream carries all three, unknown → None.
    /// A mismatch between a partial's recorded expected_burns and this mapping is what run_merge
    /// warns on (a manual --burn-* mismatch between extract and merge that could misverdict).
    #[test]
    fn args_expected_burns_for_maps_box_to_its_burns() {
        use super::args_expected_burns_for;
        use clap::Parser;
        let args = super::Args::parse_from(["recording-verdict"]);
        assert_eq!(
            args_expected_burns_for("strih", &args),
            Some(vec![CAM1B, STRIH]),
            "strih partial carries cam1 (forwarded) + strih"
        );
        assert_eq!(
            args_expected_burns_for("stream", &args),
            Some(vec![CAM1B, STRIH, STREAM]),
            "stream partial (chain endpoint) carries all three burns"
        );
        assert_eq!(
            args_expected_burns_for("nope", &args),
            None,
            "an unknown box has no expected burns"
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
    fn node_render_step_is_gap_ignore_for_all_nodes_360() {
        // #360: strih's burn is a FREE-RUNNING render tick with an IRREGULAR step (run 354003:
        // 0–10, mean ~4), NOT the clean 60/30=2 the old code assumed — its forward gaps are
        // render-clock jitter, not loss. So every node now uses gap-ignore (step 1); the
        // step>=2 excess-gap charging in burn_contiguity stays as a tested capability for a
        // genuinely-clean-decimation hop, but no current node feeds it. (Inputs ignored.)
        assert_eq!(super::node_render_step("strih", 60.0, 30.0), 1);
        assert_eq!(super::node_render_step("stream", 60.0, 30.0), 1);
        assert_eq!(super::node_render_step("cam1", 60.0, 30.0), 1);
        assert_eq!(super::node_render_step("strih", 60.0, 0.0), 1);
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
                step: super::node_render_step("strih", 60.0, 30.0), // = 2
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
                step: super::node_render_step("strih", 60.0, 30.0),
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
                step: super::node_render_step("strih", 60.0, 30.0),
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
                step: super::node_render_step("strih", 60.0, 30.0),
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
                step: super::node_render_step("strih", 60.0, 30.0),
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
                step: super::node_render_step("strih", 60.0, 30.0),
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
                step: super::node_render_step("strih", 60.0, 30.0),
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
                step: super::node_render_step("strih", 60.0, 30.0),
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
                step: super::node_render_step("strih", 60.0, 30.0),
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
                step: super::node_render_step("strih", 60.0, 30.0),
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

    // ---- #461 imag-nb burn-less optical zero-loss gate (EPIC #466 Topology v2) ----

    /// Build N imag-nb recorded frames, each carrying ONLY a cam2-style optical payload (no
    /// burn — imag has none, 911003 is reserved for a later ticket, #463). Contiguous ticks
    /// 100..100+n by default; `gap_at` (if given) removes ONE tick to simulate a dropped frame.
    fn imag_window(n: u32, gap_at: Option<u32>) -> Vec<RecordingFrame> {
        (0..n)
            .filter(|&i| gap_at != Some(i))
            .map(|i| frame(i as u64, &[(CAM2, 100 + i)]))
            .collect()
    }

    #[test]
    fn node_verdict_for_imag_reports_zero_loss_when_ticks_are_contiguous_461() {
        use super::node_verdict_for_imag;
        let frames = imag_window(60, None);
        let nv = node_verdict_for_imag(&frames, None);
        assert!(
            nv.is_zero(),
            "60 contiguous optical ticks with no gap must be zero loss"
        );
        assert_eq!(nv.contiguity.node, "imag");
        assert_eq!(nv.contiguity.first_id, Some(100));
        assert_eq!(nv.contiguity.last_id, Some(159));
        assert!(nv.contiguity.missing_ids.is_empty());
        assert_eq!(nv.optical_span_frames, 60);
        assert_eq!(nv.colour_fail, 0, "colour gate is not wired for imag yet");
    }

    #[test]
    fn node_verdict_for_imag_fails_when_a_tick_is_missing_461() {
        use super::node_verdict_for_imag;
        // Drop frame index 30 (painted tick 130) -> imag's camera failed to capture that instant.
        let frames = imag_window(60, Some(30));
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
    fn args_expected_burns_for_imag_returns_the_empty_burn_set_461() {
        use super::Args;
        use clap::Parser;
        let args = Args::parse_from(["recording-verdict"]);
        assert_eq!(
            super::args_expected_burns_for("imag", &args),
            Some(vec![]),
            "imag has no digital node-burn yet (911003 is reserved for a later ticket, #463)"
        );
    }

    #[test]
    fn build_and_print_verdict_computes_the_imag_node_independently_of_strih_stream_461() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;

        // --min-secs 1 so the 60-frame @60fps window (1s) trivially clears the #373 floor.
        let args = super::Args::parse_from(["recording-verdict", "--min-secs", "1"]);
        let imag_frames = imag_window(60, None);

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
        let imag_frames = imag_window(60, Some(15));

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
        let imag_frames = imag_window(60, None);
        let imag_p =
            RecordingPartial::from_frames("imag", &PathBuf::from("imag.mkv"), &[], imag_frames);
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
