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
use camera_box::frozen_leg::SegmentLeg;
use camera_box::offline_ack;
use camera_box::probe::av_sync_recording::{decode_av_marker_inputs, AvMarkerInputs};
use camera_box::probe::burn_contiguity::{
    burn_contiguity_in_window_with_step, burn_contiguity_in_window_with_step_and_schedule,
    BurnRate, InWindowMissingKind, NodeContiguity, RecordedBurnFrame,
};
use camera_box::probe::recording::{
    analyze_recording_with_burns, analyze_recording_with_grouped_burns_optical, extract_frames_png,
    select_frames_to_extract, RecordingFrame, DEFAULT_MAX_PIXEL_PROOF,
};
use camera_box::probe::recording_latency::{
    burn_ids_in, burn_ids_with_frame_index_in, cam2_cam1_samples, cam2_cam1_samples_from_burn,
    cam2_cam1_samples_from_flip, cam_strih_samples, chain_hop_samples_from_stream, hop_latency,
    n_camera_strih_samples, painter_internal_gen_to_flip, per_frame_latency_csv_rows,
    strih_stream_samples, strih_stream_samples_from_stream, write_latency_csv, HopLatency,
    LatencySample, RunIds, BURN_RUN_ID_CAM1, BURN_RUN_ID_CAM2, BURN_RUN_ID_CAM3, BURN_RUN_ID_CAM4,
    BURN_RUN_ID_CAM5, BURN_RUN_ID_CAM6, BURN_RUN_ID_CAM7, BURN_RUN_ID_IMAG, BURN_RUN_ID_STREAM,
    BURN_RUN_ID_STRIH,
};
use camera_box::probe::recording_partial::RecordingPartial;
use camera_box::probe::recording_segments::{
    load_switch_schedule, place_frame_in_window, raw_window_index, segment_continuity,
    SegmentFrame, SwitchWindow, WindowPlacement, DEFAULT_TRANSITION_GUARD_NS,
};
use camera_box::probe::recording_verdict::{
    cam_strih_assessment, verdict, FrameTick, RecordingVerdict, VerdictConfig,
};
use camera_box::qpsk_marker::{av_offset_candidates_deduped, DEDUPE_SAME_FID_WINDOW_S};
use camera_box::self_heal_attribution::{attribute_self_heal, SelfHealResetEvent};
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
    /// #755: cam7's capture-burn run_id (fleet growth 6→7, #753). See `--burn-cam3-run-id`.
    #[arg(long, default_value_t = BURN_RUN_ID_CAM7)]
    burn_cam7_run_id: u32,
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
    /// #1035 — the HARD absolute cam→strih p99 latency bound (ms) for the MAIN E2E. The umbrella
    /// issue 406 standing requirement is "zero-loss + BOUNDED-latency"; before this, the recording
    /// verdict computed `latency.cam_strih` but never gated on it. Bounds the production
    /// camera→strih ABSOLUTE latency (cam2 paint gen_ts → strih program) — NOT cam→stream, which
    /// is ~1s BY DESIGN (the intentional genlock hold aligning program video to the late-mastered
    /// audio; never bound that). DEFAULT-ON (hard-locked, not a forgettable flag) at the
    /// calibrated `e2e_latency_gate::CAM_STRIH_P99_LATENCY_MAX_MS` (400 ms: 1.66x the worst of 20
    /// green runs' p99). Fires only when a strih recording is present (cam1-only optical mode =
    /// N/A). Whether it folds into `overall_pass` is the one-line-restorable
    /// `e2e_latency_gate::gates_overall_pass()` seam (LIVE today). Set higher to relax / lower to
    /// tighten.
    #[arg(long, default_value_t = camera_box::e2e_latency_gate::CAM_STRIH_P99_LATENCY_MAX_MS)]
    max_cam_strih_p99_latency_ms: f64,
    /// #1036 — the calibrated per-window bound on the "15fps-like" presentation-judder
    /// signature (`presentation_cadence::paired_fraction`: a held frame immediately followed by a
    /// compensating double-step jump — the class issue 726 measures but that never gated). Bounds
    /// the WORST `paired_fraction` across every cadence-bearing cambox window. DEFAULT-ON
    /// (hard-locked, not a forgettable flag) at the calibrated
    /// `presentation_cadence::PAIRED_FRACTION_JUDDER_MAX` (0.05: 10.6x the worst of 210 windows
    /// from 21 green runs, ~19x below the pathology). Whether it folds into `overall_pass` is the
    /// one-line-restorable `presentation_cadence::gates_overall_pass()` seam (LIVE today). Set
    /// higher to relax / lower to tighten.
    #[arg(long, default_value_t = camera_box::presentation_cadence::PAIRED_FRACTION_JUDDER_MAX)]
    max_cadence_paired_fraction: f64,
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
    /// #1143: OBS's own record-session render stats for the imag recording, as a compact JSON object
    /// (`{"drawn_frames","attempted_frames","lagged_frames","lagged_pct","max_render_ms"}` — exactly
    /// what `scripts/imag_record_encoder.parse_obs_record_stats` emits). The harness captures it from
    /// the imag OBS log stop-stats around the record window and passes it to `--extract-partial imag`;
    /// it is carried in the partial (`record_render`) and surfaced REPORT-ONLY under
    /// `full_chain.loss.imag` so a stale x264 encoder's observer effect is attributed to the RECORDER.
    /// Absent ⇒ no record_render carried (unchanged behaviour). Ignored for strih/stream extracts.
    #[arg(long)]
    record_render_stats: Option<String>,
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
    /// issue 930 lipsync cross-validation: the SyncNet-aggregated offset (ms, video - audio) for
    /// the PAIRED lipsync-test-mode recording of the SAME rig state, from `scripts/av_sync_measure.py`
    /// and `scripts/av_sync_calibrate.py --calibrate` (its `mean_offset_ms`). Optional -- when
    /// given together with `--av-sync`, adds a `lipsync_cross_check` object to the printed JSON
    /// comparing it against this recording's own QR/QPSK offset (`camera_box::lipsync_cross_check`).
    /// Report-only TODAY (issue 1032: `gates_overall_pass` is derived-`false` while
    /// `RECORDED_CLEAN_PAIRED_RUNS` < `REQUIRED_CLEAN_PAIRED_RUNS`, so it does not affect this CLI's
    /// exit code); once the supervisor records N>=5 clean paired runs, a Disagree verdict makes
    /// `--av-sync` exit non-zero (the JSON is still printed first).
    /// `allow_negative_numbers`: video-earlier-than-audio is half the real outcome space and must
    /// parse as a bare `-N` value (clap 4 otherwise reads a leading `-` as a new flag).
    #[arg(long, allow_negative_numbers = true)]
    syncnet_offset_ms: Option<f64>,
    /// #624 deliverable 4 / #312 item 2 PR B: the expected MEASURED A/V offset (ms) the per-camera
    /// gate centres on — each camera's `av_offset_ms` must land within ±AV_OFFSET_GATE_TOLERANCE_MS
    /// of this value.
    ///
    /// #1178: the DEFAULT is the calibrated fixed rig video-leg
    /// (`av_window::RIG_VIDEO_LEG_OFFSET_MS`) — RE-DERIVED 2026-08-29 to 0.0: the −92 calibration
    /// briefly derived from verdict 845554984 turned out to be a stale-painter artifact (issue 1138
    /// class, an un-pinned cam2 frame-probe painter emitting the QPSK marker without its own
    /// emit-delay compensation); with the marker delay now compensated AT SOURCE a correctly
    /// aligned rig MEASURES ~0, so this default is 0 again. A mode that PHYSICALLY compensates a
    /// leg (MEASUREMENT_EQ / issue 1003, whose stream-hold rebalance lands the measured offset at
    /// ~0) passes its own explicit `--av-expected-ms 0`, which is numerically the same today but
    /// stays an explicit override so a future non-zero recalibration (a rig-verified video-chain
    /// change) never silently double-counts; an operator dialing a nonzero source offset overrides
    /// it the same way.
    #[arg(long, allow_negative_numbers = true, default_value_t = camera_box::av_window::RIG_VIDEO_LEG_OFFSET_MS)]
    av_expected_ms: f64,
    /// #855: operator-acknowledged offline boxes, threaded from the shell-side
    /// `CAMBOX_OFFLINE_ACK` / `rig-fleet.txt` ack (`scripts/lib/cambox-offline-ack.sh`) across
    /// the shell -> Rust boundary. Same "box:reason,box:reason" format the shell side already
    /// canonicalizes (a bare box name gets reason "unspecified") -- see `offline_ack::parse`.
    /// Currently consumed ONLY by the ALL-CAMBOX per-camera A/V-sync gate (#624/#312's
    /// `all_cambox_av_sync`): a box named here is reported EXCLUDED there instead of judged
    /// UNKNOWN/FAIL on zero samples it was never going to produce. A box NOT named here keeps
    /// the existing fail-closed default unchanged (#836: never widen the tolerance, never
    /// downgrade the gate -- this only fixes WHO gets judged).
    #[arg(long, default_value = "")]
    offline_ack_cams: String,
    /// #1142 — does THIS run's contract REQUIRE a verified imag leg? When set, a silently-skipped
    /// imag leg (`imag_leg_verified=false` and not operator-offline-acked) or a failing imag
    /// PRESENCE term (span floor / undecodable moiré floor / colour) REDs `overall_pass` (the owner
    /// honesty mandate). DEFAULT `false` so the many in-process/unit verdict tests that build a
    /// verdict WITHOUT an imag partial (isolated strih/stream/cam scenarios) are unaffected — only
    /// the production full-chain merge declares the imag leg part of its contract. `recording-e2e.sh`
    /// passes it on the ALL_CAMBOX `[8/8d]` merge (never the strih+stream-only zero-loss-restart
    /// merge). The imag PER-FRAME CONTENT terms stay report-only regardless (issue 1130 observer
    /// effect); this flag only governs the BLOCKING presence/verification terms.
    #[arg(long, default_value_t = false)]
    require_imag_leg: bool,
    /// #895: a `capture_rate_selfheal` (#663) USB-reset event detected by the harness during THIS
    /// recording window (`scripts/lib/self-heal-attribution.sh`'s mid-recording scan, wired at
    /// `recording-e2e.sh`'s `[7b/8]`), so the `frozen_leg` classifier never misreports the
    /// resulting stale/duplicate frames as a camera fault. Repeat per event:
    /// `--self-heal-reset cam1:1785439475449374588 --self-heal-reset cam1:1785439600100000000`.
    /// A malformed token is silently dropped (`SelfHealResetEvent::parse`) rather than erroring —
    /// the harness's own scan output is the only producer, and a partially-unparseable token must
    /// never abort an otherwise-valid verdict run.
    #[arg(long, value_name = "CAMBOX:EPOCH_NS")]
    self_heal_reset: Vec<String>,
    /// issue 946 / issue 910: a kind-tagged run-integrity RESTART event detected by the harness
    /// during THIS recording window (`scripts/lib/self-heal-attribution.sh`'s unified
    /// recognised-event scan, `[7b/8]`, read from BOTH journald AND each camera's burn-instance
    /// log). Repeat per event: `--restart-event KIND:CAMBOX:EPOCH_NS` where KIND is
    /// `self_heal_reset` | `capture_wedge` (issue 945, exit 79) | `emit_freeze` (issue 944, exit
    /// 81). Threaded through the SAME `attribute_self_heal` correlation as `--self-heal-reset`
    /// (which stays as the untagged self-heal-only alias), so a wedge/emit-freeze restart mid-
    /// recording is re-attributed away from `frozen_leg` too. A malformed token is silently
    /// dropped (`SelfHealResetEvent::parse`) — never aborts an otherwise-valid verdict run.
    #[arg(long, value_name = "KIND:CAMBOX:EPOCH_NS")]
    restart_event: Vec<String>,
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

/// #904 introduced an owner-directed, deliberate relaxation of the absolute-zero `real_drops`
/// bar on the camera-under-test ("endpoint") nodes ONLY (`burn_unreadable`, colour, the optical
/// span floor, frozen-camera and delivery-latency terms were ALL untouched — see
/// [`NodeVerdict::is_zero_within_allowance`]), to absorb one-off single-frame artifacts (e.g.
/// #903's 30us program-switch boundary case) while the fix for that root cause was still
/// outstanding.
///
/// **#905 item 1 reverted this DEFAULT back to 0** (2026-08-13): #903's fix merged and 6/6
/// sampled recent full-cycle runs showed `real_drops=0` on every node with the allowance NEVER
/// consumed — the tolerance was no longer buying anything, so the strict zero bar is restored.
/// The allowance MECHANISM itself is untouched and still available via
/// `CAMERA_BOX_REAL_DROPS_ALLOWANCE` (see [`real_drops_allowance`]) for a future genuinely new
/// artifact class — only the silent DEFAULT moved back to strict. NEVER raise this constant
/// again without a fresh measured incident and its own re-tighten ticket, same discipline as
/// [`OPTICAL_UNDECODABLE_RATE_MAX`].
///
/// **Issue 1169 RE-WIDENED this DEFAULT to 1** (owner, 2026-08-22) — the SECOND SEAM of the
/// zero-loss singleton work (sibling of the per-segment `<=1/<=1` bar). The first full verdict of
/// the series (859647390) failed `full_chain.zero_loss` on exactly `real_drops:1` over 314.7
/// analyzed seconds: a single per-frame delivery SINGLETON (the issue-1167 v3 paced-trickle
/// absorption + a FIFO stale_replay in the same event; `burn_unreadable` stays 0 — a genuine
/// delivery singleton, not a burn-readability defect). Per the owner's 2026-07-31 strict-test
/// revision ("jedna stratená snímka nie je problém"), the band re-widens to the LOUD `<=1`
/// singleton: a single drop PASSES within the allowance and is reported LOUDLY (never a silent
/// green), while `>=2` of anything still FAILS and `burn_unreadable` stays an unconditional hard
/// fail. This is the exact `gate-allowance-restore-red-green.md` shape, inverted. **Issue 1169
/// stays OPEN as the RE-TIGHTEN trail** — a one-constant flip back to 0 (proven dormant by
/// `re_tightening_the_1169_allowance_to_zero_restores_the_strict_bar`), landed once a
/// zero-singleton green run holds (e.g. after the issue-1168 floor reduction and/or the cam1-card
/// swap). NEVER widen this band further without a fresh measured incident and its own trail.
const REAL_DROPS_ALLOWANCE_DEFAULT: u32 = 1;

/// #904 — env-overridable read of the per-node `real_drops` allowance (mirrors the
/// `CAMERA_BOX_DECODE_WORKERS` idiom in `src/probe/recording.rs`: a non-numeric or absent value
/// silently falls back to [`REAL_DROPS_ALLOWANCE_DEFAULT`], never panics).
fn real_drops_allowance() -> u32 {
    std::env::var("CAMERA_BOX_REAL_DROPS_ALLOWANCE")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(REAL_DROPS_ALLOWANCE_DEFAULT)
}

/// **Issue 1169 THIRD SEAM (owner, 2026-08-22)** — the LOUD singleton allowance for the raw
/// cam-leg V4L2 capture-drop counter (`full_chain.loss.cam2_*.zero_loss`, the LAST binding
/// `all_pass &= …` red). It is the sibling of the two prior seams: the per-segment `<=1/<=1`
/// singleton bar (`window_gate::segment_singleton_allowance_*`) and the per-node real-drops
/// singleton (`REAL_DROPS_ALLOWANCE_DEFAULT`, the delivery-hop counter above). A `v4l2_dropped`
/// count WITHIN this band is an UPSTREAM camera-leg buffer drop (the kernel `sequence` gap
/// `capture.rs` tracks) that the merged issue-1167 v2–v5 paced-trickle + FIFO emit-fill absorbs by
/// design — so a strict-zero bar on the RAW counter double-reds what the presented layer already
/// compensated. The first full verdict of the series showed exactly `v4l2_dropped:2` over
/// `frames_captured:35961` (0.0056%) while `full_chain.zero_loss` + `all_cambox_continuity` were
/// already green. Per the owner's 2026-07-31 strict-test revision ("jedna stratená snímka nie je
/// problém"), a `v4l2_dropped <= CAMLEG_V4L2_DROP_ALLOWANCE_DEFAULT` count PASSES within the
/// allowance and is reported LOUDLY (never a silent green — a `note` + `camleg_singleton_band_consumed`
/// on the node JSON), while `> CAMLEG_V4L2_DROP_ALLOWANCE_DEFAULT` still FAILS unchanged. The
/// default of 2 is justified from the live data: healthy cam2/cam3 routinely log 0–2 capture-dropped
/// per ~10-min run window. This is the exact `gate-allowance-restore-red-green.md` shape, inverted,
/// THIRD instance. **Issue 1169 stays OPEN as the RE-TIGHTEN trail** — a one-constant flip back to 0
/// (proven dormant by `re_tightening_the_camleg_v4l2_band_to_zero_restores_the_strict_bar`), landed
/// once a zero-singleton green run holds (e.g. after the issue-1168 floor reduction and/or the
/// cam1-card swap). NEVER widen this band further without a fresh measured incident and its own trail.
const CAMLEG_V4L2_DROP_ALLOWANCE_DEFAULT: u32 = 2;

/// #1169 — env-overridable read of the cam-leg V4L2 capture-drop allowance (mirrors the
/// [`real_drops_allowance`] idiom above: a non-numeric or absent value silently falls back to
/// [`CAMLEG_V4L2_DROP_ALLOWANCE_DEFAULT`], never panics).
fn camleg_v4l2_drop_allowance() -> u32 {
    std::env::var("CAMERA_BOX_CAMLEG_V4L2_DROP_ALLOWANCE")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(CAMLEG_V4L2_DROP_ALLOWANCE_DEFAULT)
}

/// #1169 — the PURE cam-leg V4L2 capture-drop band decision (Tier-0 unit-testable; the whole
/// `recording-verdict` bin is probe-gated with no local compile path, so keeping this a standalone
/// scalar fn is what lets a rustc-replica prove the boundary). Returns `(within_band,
/// band_consumed)`:
/// - `within_band` — `v4l2_dropped <= allowance`; drives the node's `zero_loss` and the `all_pass`
///   fold (a within-band count PASSES `overall_pass`).
/// - `band_consumed` — `within_band && v4l2_dropped > 0`; a NON-zero drop count that only cleared
///   the gate because of the band (the LOUD note case, distinct from a clean strict-zero pass).
fn camleg_capture_band(v4l2_dropped: u64, allowance: u32) -> (bool, bool) {
    let within_band = v4l2_dropped <= allowance as u64;
    let band_consumed = within_band && v4l2_dropped > 0;
    (within_band, band_consumed)
}

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
const CAMERA_UNDER_TEST_NODES: [&str; 7] = ["cam1", "cam2", "cam3", "cam4", "cam5", "cam6", "cam7"];

/// #312 — the SUBSET of [`CAMERA_UNDER_TEST_NODES`] that physically films cam2's painted
/// monitor via the HDMI-splitter optical loopback (a real lens + capture card pointed at cam2's
/// screen). Used ONLY by the `#624` per-camera cam2→camera OPTICAL-INJECTION latency loop
/// (`camera_burn.gen_ts_ns − cam2_optical.gen_ts_ns`) — cam2 is EXCLUDED here because cam2 IS
/// the painter: there is no second camera-vs-monitor optical hop to measure when the "camera
/// under test" is cam2 itself (that would degenerate into measuring cam2 against its own
/// framebuffer paint, not a real optical-injection latency). cam2 still gets its own DIGITAL
/// contiguity/loss proof via [`CAMERA_UNDER_TEST_NODES`] above — only this narrower optical
/// latency measurement excludes it.
const OPTICAL_INJECTION_NODES: [&str; 6] = ["cam1", "cam3", "cam4", "cam5", "cam6", "cam7"];

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
    /// #904 — the SAME headline decision as [`Self::is_zero`], except the non-imag PRIMARY
    /// signal ([`Self::optical_ok`]'s strict-contiguity fallback) also accepts up to `allowance`
    /// `real_drops()` on this node, PROVIDED `burn_unreadable() == 0` — an unreadable burn stays
    /// an unconditional hard fail (the ticket's own "does NOT touch burn_unreadable" line), only
    /// a small number of genuinely-missing frames is tolerated. `allowance == 0` reproduces
    /// [`Self::is_zero`] EXACTLY (this is the RED→GREEN default-preserves-behavior line — a node
    /// with any real drop still fails at allowance 0, same as today). imag is UNAFFECTED either
    /// way: [`Self::optical_ok`]'s `imag_optical_beat_pass()` branch is used verbatim, the
    /// allowance only ever reaches the non-imag `contiguity`-based fallback.
    fn is_zero_within_allowance(&self, allowance: u32) -> bool {
        self.optical_ok_within_allowance(allowance)
            && self.optical_undecodable_ok()
            && self.colour_fail == 0
            && self.imag_burn_ok()
    }
    /// #904 — [`Self::optical_ok`] generalized with a `real_drops()` allowance on the non-imag
    /// fallback (imag's `imag_optical_beat_pass()` branch is untouched, same as
    /// [`Self::optical_ok`]). A node with NO burn at all (`first_id.is_none()`) still never
    /// passes — nothing was proven, allowance or not.
    fn optical_ok_within_allowance(&self, allowance: u32) -> bool {
        self.imag_optical_beat_pass().unwrap_or_else(|| {
            self.contiguity.first_id.is_some()
                && self.burn_unreadable() == 0
                && self.real_drops() <= allowance as usize
        })
    }
    /// #904 — true iff this node PASSES [`Self::is_zero_within_allowance`] with `allowance` ONLY
    /// because it carries `1..=allowance` real drops (i.e. it would have FAILED at allowance 0,
    /// [`Self::is_zero`]). Drives the LOUD "this pass consumed slack" reporting — a run must never
    /// silently look identical to a genuine zero-loss pass when it only cleared the gate because
    /// of #904's relaxation.
    fn consumed_real_drops_allowance(&self, allowance: u32) -> bool {
        self.is_zero_within_allowance(allowance) && !self.is_zero()
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

/// #708/#741 — for EACH entry of `window`, compute which `--switch-schedule` window index (if
/// any) it belongs to, for the SOLE purpose of feeding
/// [`burn_contiguity_in_window_with_step_and_schedule`]'s `crossed_window_boundary` check (this
/// function's only caller/consumer — see that function's `window_of` parameter).
///
/// #741 fix (live-investigated, 2026-07-15): this used to reuse [`place_frame_in_window`] — the
/// GUARD-filtered placement content-attribution uses everywhere else — but that made the #708
/// exception structurally unable to fire for the exact case it exists to except. A genuine
/// program switch changes the active render source within roughly one render tick (~30ms) of
/// the schedule boundary; the settle-time guard band (`scope.guard_ns`, ~1s) is far wider than
/// that, so the LAST frame before a real cut and the FIRST frame after it are — by construction
/// — both inside the guard band on their own side, both reading back `Guard` (mapped to `None`
/// below the old way). `crossed_window_boundary` only ever fires on `(Some(pw), Some(cw))`, so it
/// silently stayed `false` on every real boundary crossing, misclassifying the expected
/// per-source free-running-counter discontinuity as a `RealDrop` (confirmed live: both fresh
/// investigation runs' every single flagged "real_drop" landed within 1–32ms of an actual
/// `--switch-schedule` boundary — well inside the guard band, never a genuine mid-window gap).
///
/// This now uses [`raw_window_index`] — the SAME schedule-interval lookup `place_frame_in_window`
/// performs internally, just WITHOUT its settle-time guard layered on top. Content attribution
/// elsewhere ([`scope_camera_window_to_own_schedule`], the #312 sweep) is completely unaffected —
/// they still call `place_frame_in_window` directly and keep the guard, which is correct for
/// THEIR purpose (deciding which cambox's tally a frame counts toward). Only the boundary-
/// crossing QUESTION ("did the schedule's active window change between these two frames") needs
/// the guard-free answer, because the guard's settle-time margin is exactly what was hiding the
/// boundary from it.
///
/// `None` at a position ⇒ the frame's own gen_ts anchor is missing, or it fell genuinely outside
/// every scheduled window — [`burn_contiguity_in_window_with_step_and_schedule`] treats an
/// unknown window on EITHER side of a comparison as "assume the SAME window" (never silently
/// suppresses a real anomaly), so returning `None` here is always the conservative, safe choice.
///
/// #903 — the SECOND element of the returned tuple is the companion `near_boundary_of` signal:
/// for each entry of `window`, is that frame's own `gen_ts_ns` within
/// [`camera_box::window_boundary_tolerance::DEFAULT_BOUNDARY_TOLERANCE_NS`] of ANY schedule
/// boundary instant (a window's `start_ns` or `end_ns`)? `raw_window_index`'s exact `>=`/`<`
/// interval test has no tolerance at all, and the boundary instant (dev1's clock) and `gen_ts_ns`
/// (the painter's clock) are only ever guaranteed to agree within the `#326` gate's 200ms — so a
/// genuine crossing can still read back as the SAME window on both sides when clock disagreement
/// puts a frame on the "wrong" side of the exact boundary by less than that (confirmed live: run
/// 30637408198's one wrongly-charged jump landed its frame 30 microseconds on the old side).
/// `burn_contiguity_in_window_with_step_and_schedule`'s `near_boundary_of` parameter uses this as
/// an ADDITIONAL confirmation on top of (never a replacement for) the exact window-index check —
/// never fires when either side's window is unknown, exactly like the exact check.
fn attribute_window_indices(
    window: &[RecordedBurnFrame],
    source: &[RecordingFrame],
    all_burn_run_ids: &[u32],
    cam2_run_id: Option<u32>,
    scope: ScheduleScope<'_>,
) -> (Vec<Option<usize>>, Vec<bool>) {
    let gen_ts_by_index: HashMap<u64, i64> = source
        .iter()
        .filter_map(|f| {
            frame_gen_ts_anchor(f, scope.anchor_run_ids, all_burn_run_ids, cam2_run_id)
                .map(|ts| (f.frame_index, ts))
        })
        .collect();
    // #903 — the flat list of every schedule window's start/end instant, the boundary set
    // `near_any_boundary` measures distance against.
    let boundaries: Vec<i64> = scope
        .schedule
        .iter()
        .flat_map(|w| [w.start_ns, w.end_ns])
        .collect();
    let window_of = window
        .iter()
        .map(|rbf| {
            gen_ts_by_index
                .get(&rbf.frame_index)
                .and_then(|&gen_ts| raw_window_index(gen_ts, scope.schedule))
        })
        .collect();
    let near_boundary_of = window
        .iter()
        .map(|rbf| {
            gen_ts_by_index
                .get(&rbf.frame_index)
                .is_some_and(|&gen_ts| {
                    camera_box::window_boundary_tolerance::near_any_boundary(
                        gen_ts,
                        &boundaries,
                        camera_box::window_boundary_tolerance::DEFAULT_BOUNDARY_TOLERANCE_NS,
                    )
                })
        })
        .collect();
    (window_of, near_boundary_of)
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
    let window_attribution: Option<(Vec<Option<usize>>, Vec<bool>)> = if node == "strih" {
        schedule_scope.map(|scope| {
            attribute_window_indices(&window, source, all_burn_run_ids, spec.cam2_run_id, scope)
        })
    } else {
        None
    };
    let window_of: Option<Vec<Option<usize>>> = window_attribution.as_ref().map(|(w, _)| w.clone());
    // #903 — the companion near-boundary tolerant signal (see `attribute_window_indices`'s doc).
    let near_boundary_of: Option<Vec<bool>> = window_attribution.as_ref().map(|(_, nb)| nb.clone());
    let in_window = burn_contiguity_in_window_with_step_and_schedule(
        node,
        &window,
        spec.rate,
        spec.step,
        window_of.as_deref(),
        near_boundary_of.as_deref(),
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
fn node_verdict_lines(v: &NodeVerdict, span_ok: bool, allowance: u32) -> Vec<String> {
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
    if v.is_zero_within_allowance(allowance) && span_ok {
        // #904 — LOUD, never silent: this specific pass only cleared the gate because of the
        // real_drops allowance (it would have FAILED at allowance 0 — see
        // `consumed_real_drops_allowance`). Printed FIRST, before the "ZERO loss" line below, so
        // it can never be missed scrolling past — a pass that consumed slack must be visibly
        // distinguishable from a genuine zero-loss pass (issue 1169 is the re-tighten trail).
        if v.consumed_real_drops_allowance(allowance) {
            lines.push(format!(
                "  [{}] ZERO loss WITHIN ALLOWANCE — real-drops singleton allowance consumed: {} \
                 — issue 1169 re-tighten trail (#904/#1169 per-node allowance of {allowance}; \
                 burn_unreadable stays 0).",
                c.node,
                v.real_drops()
            ));
        }
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
            // #904 — a non-imag node (imag_optical_beat is always None there) that only passed
            // via the real_drops allowance is NOT strictly contiguous — say so honestly instead
            // of the blanket "CONTIGUOUS" claim (the same no-overstatement discipline the imag
            // beat-compensation branch above already follows).
            None if v.consumed_real_drops_allowance(allowance) => format!(
                "burn-id sequence has {} real drop(s) WITHIN the #904 allowance (not strictly \
                 contiguous) AND cam2 optical read complete",
                v.real_drops()
            ),
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

/// Print the ONE trustworthy binary verdict for a node, human-readable, no jargon. `allowance` is
/// the #904 per-node `real_drops` allowance (0 reproduces the pre-#904 strict behavior exactly —
/// see [`NodeVerdict::is_zero_within_allowance`]).
fn print_node_verdict(v: &NodeVerdict, span_ok: bool, allowance: u32) {
    for line in node_verdict_lines(v, span_ok, allowance) {
        println!("{line}");
    }
}

/// JSON for one node's trustworthy verdict. `analyzed_secs` / `span_ok` / `min_secs` are the #373
/// headline duration gate (the analyzed optical span and whether it cleared the floor), so the
/// report explains a FAIL caused by a collapsed/partial optical read — not just a bare
/// `overall_pass: false`. `zero_loss` here is the per-node DELIVERY gate, #904-allowance-aware
/// (`is_zero_within_allowance`, `allowance == 0` ⇒ byte-identical to the pre-#904 `is_zero()`);
/// the headline `overall_pass` ANDs it with `span_ok`.
fn node_verdict_json(
    v: &NodeVerdict,
    analyzed_secs: f64,
    span_ok: bool,
    min_secs: f64,
    allowance: u32,
) -> serde_json::Value {
    serde_json::json!({
        "node": v.contiguity.node,
        "zero_loss": v.is_zero_within_allowance(allowance),
        "first_id": v.contiguity.first_id,
        "last_id": v.contiguity.last_id,
        "present_count": v.contiguity.present_count,
        "expected_count": v.contiguity.expected_count,
        "missing_ids": v.contiguity.missing_ids,
        "real_drops": v.real_drops(),
        "burn_unreadable": v.burn_unreadable(),
        // #904 — LOUD, always present (not just when consumed): the allowance THIS node was
        // judged against, and whether this specific pass only cleared the gate because of it.
        // `real_drops_allowance` is 0 for imag (untouched by #904; see its call site) and the
        // configured value (env `CAMERA_BOX_REAL_DROPS_ALLOWANCE`, default
        // `REAL_DROPS_ALLOWANCE_DEFAULT`) for every camera-under-test node.
        "real_drops_allowance": allowance,
        "consumed_real_drops_allowance": v.consumed_real_drops_allowance(allowance),
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

/// issue 930 — the lipsync cross-check for one `--av-sync` recording, extracted as its OWN
/// decode-free function so a fixture test can exercise the wiring without a real recording
/// decode (the rest of `run_av_sync` needs real files/ffprobe and has no such path — see
/// CLAUDE.md's "No bypass exists for `src/bin/recording-verdict.rs`"). Returns `None` when the
/// caller never supplied `--syncnet-offset-ms` (the pre-930 behavior: no `lipsync_cross_check`
/// key at all), `Some` otherwise — `camera_box::lipsync_cross_check::evaluate` itself decides
/// Agree/Disagree/Unknown.
fn lipsync_cross_check_for(
    qr_qpsk_offset_ms: f64,
    syncnet_offset_ms: Option<f64>,
) -> Option<camera_box::lipsync_cross_check::LipsyncCrossCheck> {
    let syncnet_offset_ms = syncnet_offset_ms?;
    Some(camera_box::lipsync_cross_check::evaluate(
        Some(syncnet_offset_ms),
        Some(qr_qpsk_offset_ms),
        camera_box::lipsync_cross_check::LIPSYNC_CROSS_CHECK_TOLERANCE_MS,
    ))
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
    let mut json = serde_json::json!({
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
    // issue 930/1032 — lipsync cross-validation: only added when the caller supplied the paired
    // lipsync-test-mode run's SyncNet offset (never printed on a plain --av-sync call, so every
    // pre-930 invocation's JSON shape is byte-for-byte unchanged).
    let mut lipsync_folds_to_failure = false;
    if let Some(cross_check) =
        lipsync_cross_check_for(report.offset.offset_ms, args.syncnet_offset_ms)
    {
        let gate_pass =
            camera_box::lipsync_cross_check::lipsync_cross_check_gate_pass(cross_check.verdict);
        // issue 1032: fold-earned check against the recorded evidence count. DORMANT today —
        // RECORDED_CLEAN_PAIRED_RUNS (0) < REQUIRED_CLEAN_PAIRED_RUNS (5), so this is always
        // `false` and the run still exits 0; it goes live the instant the supervisor bumps the
        // recorded count to 5 (one-constant flip in src/lipsync_cross_check.rs).
        lipsync_folds_to_failure = camera_box::lipsync_cross_check::folds_to_failure(
            cross_check.verdict,
            camera_box::lipsync_cross_check::RECORDED_CLEAN_PAIRED_RUNS,
        );
        json["lipsync_cross_check"] = serde_json::json!({
            "syncnet_offset_ms": cross_check.syncnet_offset_ms,
            "qr_qpsk_offset_ms": cross_check.qr_qpsk_offset_ms,
            "delta_ms": cross_check.delta_ms,
            "tolerance_ms": cross_check.tolerance_ms,
            "verdict": format!("{:?}", cross_check.verdict),
            "gate_pass": gate_pass,
            "gates_overall_pass": camera_box::lipsync_cross_check::gates_overall_pass(),
            "required_clean_paired_runs": camera_box::lipsync_cross_check::REQUIRED_CLEAN_PAIRED_RUNS,
            "recorded_clean_paired_runs": camera_box::lipsync_cross_check::RECORDED_CLEAN_PAIRED_RUNS,
            "folds_to_failure": lipsync_folds_to_failure,
        });
        tracing::info!(
            syncnet_offset_ms = ?cross_check.syncnet_offset_ms,
            qr_qpsk_offset_ms = report.offset.offset_ms,
            delta_ms = ?cross_check.delta_ms,
            verdict = ?cross_check.verdict,
            gate_pass,
            gates_overall_pass = camera_box::lipsync_cross_check::gates_overall_pass(),
            recorded_clean_paired_runs =
                camera_box::lipsync_cross_check::RECORDED_CLEAN_PAIRED_RUNS,
            required_clean_paired_runs =
                camera_box::lipsync_cross_check::REQUIRED_CLEAN_PAIRED_RUNS,
            folds_to_failure = lipsync_folds_to_failure,
            "issue 930/1032 lipsync cross-check"
        );
    }
    println!("{}", serde_json::to_string_pretty(&json)?);
    tracing::info!(
        av_offset_ms = report.offset.offset_ms,
        mad_ms = report.offset.mad_ms,
        matched = report.offset.matched,
        audio_markers = report.audio_markers,
        video_ticks = report.video_ticks,
        "A/V-sync offset measured (video − audio; >0 = video lags audio)"
    );
    // issue 1032 — fold the lipsync cross-check into this --av-sync run's exit code. The JSON above
    // is ALWAYS printed first (the operator sees both offsets + the delta) before the run fails.
    // DORMANT today: `folds_to_failure` is always `false` while RECORDED_CLEAN_PAIRED_RUNS (0) <
    // REQUIRED_CLEAN_PAIRED_RUNS (5), so exit behavior is byte-identical to before; it goes LIVE
    // the moment the supervisor records N>=5 clean paired runs by bumping that one constant.
    if lipsync_folds_to_failure {
        anyhow::bail!(
            "lipsync cross-check DISAGREE: SyncNet vs QR/QPSK differ beyond the {}ms tolerance \
             (see the printed lipsync_cross_check JSON) — the fold is live ({} of {} clean paired \
             runs recorded)",
            camera_box::lipsync_cross_check::LIPSYNC_CROSS_CHECK_TOLERANCE_MS,
            camera_box::lipsync_cross_check::RECORDED_CLEAN_PAIRED_RUNS,
            camera_box::lipsync_cross_check::REQUIRED_CLEAN_PAIRED_RUNS,
        );
    }
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
/// #1112 back-compat wrapper — preserves the original 8-arg signature so every existing (test +
/// fused-`main`) call site is byte-for-byte unchanged. The fused path carries no per-frame
/// near-duplicate diffs (the #1088 dup-cadence block recomputes them from the LOCAL `stream_rec`
/// there), so it delegates with `None`; only the #208 merge path (`run_merge`, which has no
/// recording on dev1) calls [`build_and_print_verdict_with_stream_diffs`] with the vector carried in
/// the stream partial (`RecordingPartial::frame_prev_diffs`).
#[allow(clippy::too_many_arguments)]
fn build_and_print_verdict(
    args: &Args,
    strih: Option<DecodedRec>,
    stream: Option<DecodedRec>,
    cam1: Cam1Source,
    strih_colour: Option<camera_box::colour_verify::NodeColourSummary>,
    stream_colour: Option<camera_box::colour_verify::NodeColourSummary>,
    imag: Option<DecodedRec>,
    stream_av_sync: Option<AvMarkerInputs>,
) -> Result<(serde_json::Value, bool)> {
    build_and_print_verdict_with_stream_diffs(
        args,
        strih,
        stream,
        cam1,
        strih_colour,
        stream_colour,
        imag,
        stream_av_sync,
        None,
        None, // issue 1118: the fused/test path never degrades an imag partial (no schema skip)
        None, // #1143: the fused/test path carries no OBS record-render stats
    )
}

/// [`build_and_print_verdict`] + the STREAM recording's per-frame near-duplicate MAD-to-predecessor
/// vector carried from the #208 merge. `stream_frame_prev_diffs` is `Some` ONLY in the merge path on
/// an all-cambox stream extract (see `run_merge` / `RecordingPartial::frame_prev_diffs`); `None` on
/// the fused path (there the #1088 dup-cadence block recomputes it from the local `stream_rec`) and
/// on any run without the carried vector. Consumed ONLY by the #1088 dup-cadence block below — every
/// other term is unaffected by this argument.
#[allow(clippy::too_many_arguments)]
fn build_and_print_verdict_with_stream_diffs(
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
    // #1112/#1166 — the STREAM recording's per-frame near-duplicate MAD-to-predecessor vector
    // carried from the merge (`Some` only in the #208 merge path on an all-cambox stream extract;
    // see above). Fed to the #1088 dup-cadence block so it emits in the production
    // `VERDICT_ON_STREAM=1` merge gate.
    stream_frame_prev_diffs: Option<Vec<Option<f64>>>,
    // issue 1118 — `Some(reason)` when `run_merge` DROPPED a schema-mismatched imag partial (the
    // report-only degrade path); surfaced at `full_chain.imag_leg_skip_reason` beside
    // `imag_leg_verified` so a degraded run is mineable, never silent. `None` on every normal run
    // and on the fused/test path.
    imag_skip_reason: Option<String>,
    // #1143 — OBS's own record-session render stats for the imag recording, carried from the imag
    // partial's `record_render` (Some only when `run_merge` merged an imag partial the harness had
    // extracted with `--record-render-stats`). Surfaced REPORT-ONLY under `full_chain.loss.imag`
    // (drawn/attempted/lagged% + max in-record ms) so a stale x264 encoder's observer effect is
    // attributed to the recorder, never to the delivery chain. `None` on every run without it.
    imag_record_render: Option<camera_box::record_render_stats::RecordRenderStats>,
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

        // #1035 — the absolute cam→strih p99 latency BOUND (umbrella issue 406 bounded-latency).
        // The strih recording is present here, so the gate is APPLICABLE: a `None` p99 now means
        // "strih present but zero paired cam→strih samples", which FAILS (test-strictness — a gate
        // that could not measure must not report green). The bound is default-on (hard-locked);
        // whether it folds into `overall_pass` is the one-line-restorable `gates_overall_pass()`
        // seam (LIVE today). cam→stream's ~1s genlock hold is deliberately NOT bounded.
        let cam_strih_p99 = cam_strih_lat.as_ref().map(|l| l.stats.p99_ms);
        // Pass the measured MIN too: a negative min (recv before gen) is a DanteSync desync, so
        // the whole measurement is untrustworthy and the gate FAILS rather than passing on a
        // small/negative p99 (mirrors differ::absolute_latency_gate_pass's backstop).
        let cam_strih_min = cam_strih_lat.as_ref().map(|l| l.stats.min_ms);
        let bound = args.max_cam_strih_p99_latency_ms;
        let gate_pass = camera_box::e2e_latency_gate::cam_strih_latency_gate_pass(
            cam_strih_p99,
            cam_strih_min,
            Some(bound),
        );
        let gates_overall = camera_box::e2e_latency_gate::gates_overall_pass();
        report["latency"]["cam_strih_gate"] = serde_json::json!({
            "bound_p99_ms": bound,
            "p99_ms": cam_strih_p99,
            "min_ms": cam_strih_min,
            "pass": gate_pass,
            "gates_overall_pass": gates_overall,
            "note": "#1035 absolute cam->strih p99 latency bound (issue 406 bounded-latency). \
                     cam->stream is ~1s by design (genlock hold) and is NOT bounded. Relax/tighten \
                     via --max-cam-strih-p99-latency-ms; report-only via e2e_latency_gate::\
                     gates_overall_pass.",
        });
        // Fold: a FAIL only fails the run while the seam gates overall_pass (LIVE today).
        all_pass &= gate_pass || !gates_overall;
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
        let cam7_ids = burn_ids_in(cam1_source, args.burn_cam7_run_id);
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
            !cam7_ids.is_empty(),
        );
        let strih_ids_seq = burn_ids_in(stream_frames, args.burn_strih_run_id);
        let stream_ids_seq = burn_ids_in(stream_frames, args.burn_stream_run_id);
        let any_burn = !cam1_ids.is_empty()
            || !cam2_ids.is_empty()
            || !cam3_ids.is_empty()
            || !cam4_ids.is_empty()
            || !cam5_ids.is_empty()
            || !cam6_ids.is_empty()
            || !cam7_ids.is_empty()
            || !strih_ids_seq.is_empty()
            || !stream_ids_seq.is_empty();
        if any_burn {
            println!();
            println!(
                "=== #174 FULL-CHAIN per-hop verdict (camera-under-test from the {cam1_source_label}; strih/stream from the stream recording) ==="
            );
            println!(
                "  burn ids: cam1={} cam2={} cam3={} cam4={} cam5={} cam6={} cam7={} (from {cam1_source_label}) strih={} stream={} (stream recording)",
                cam1_ids.len(),
                cam2_ids.len(),
                cam3_ids.len(),
                cam4_ids.len(),
                cam5_ids.len(),
                cam6_ids.len(),
                cam7_ids.len(),
                strih_ids_seq.len(),
                stream_ids_seq.len()
            );
            report["full_chain"]["burn_ids_present"] = serde_json::json!({
                "cam1": cam1_ids.len(), "cam2": cam2_ids.len(), "cam3": cam3_ids.len(),
                "cam4": cam4_ids.len(), "cam5": cam5_ids.len(), "cam6": cam6_ids.len(),
                "cam7": cam7_ids.len(),
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
                || !cam6_ids.is_empty()
                || !cam7_ids.is_empty();
            if strih_data.is_some() && !camera_under_test_measured {
                eprintln!(
                    "WARNING: --strih supplied but NO camera-under-test burn found in the strih \
                     recording (checked cam1={}, cam2={}, cam3={}, cam4={}, cam5={}, cam6={}, \
                     cam7={}) — the camera→strih hop is UNMEASURED this run (burn OFF or not \
                     reaching strih). A ZERO-loss headline below covers strih/stream ONLY.",
                    args.burn_cam1_run_id,
                    args.burn_cam2_run_id,
                    args.burn_cam3_run_id,
                    args.burn_cam4_run_id,
                    args.burn_cam5_run_id,
                    args.burn_cam6_run_id,
                    args.burn_cam7_run_id
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
                args.burn_cam7_run_id,
                args.burn_strih_run_id,
                args.burn_stream_run_id,
            ];
            println!();
            println!(
                "=== #186 ZERO-LOSS VERDICT — per-node burn-id contiguity (the ONE trustworthy check) ==="
            );
            let mut node_verdicts: Vec<NodeVerdict> = Vec::new();
            // #904 — read ONCE, reused by every camera-under-test node's headline decision below
            // AND its per-node JSON (never re-read per node — a mid-run env change must not shift
            // the bar between nodes of the SAME run).
            let real_drops_allowance = real_drops_allowance();
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
                    // #755 — cam7 (fleet growth 6→7, #753), see the cam3 comment above.
                    NodeSpec {
                        node: "cam7",
                        burn_run_id: args.burn_cam7_run_id,
                        rate: BurnRate::PerEmittedFrame,
                        source: cam1_source,
                        rec_path: cam1_rec_path,
                        cam2_run_id: cam2_pin,
                        step: node_render_step(
                            "cam7",
                            args.strih_emit_fps,
                            args.stream_capture_fps,
                            args.refresh_hz,
                            args.capture_fps,
                        ),
                    },
                    !cam7_ids.is_empty(),
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
                print_node_verdict(&nv, span_ok, real_drops_allowance);
                if !span_ok {
                    println!(
                        "  [{}] NOT zero — analyzed optical span {:.1}s < {:.1}s floor: the cam2 \
                         dual-QR read COLLAPSED to {} frame(s); a contiguous burn window over so few \
                         frames proves nothing (#373).",
                        spec.node, span_secs, cfg.min_secs, nv.optical_span_frames
                    );
                }
                all_pass &= nv.is_zero_within_allowance(real_drops_allowance) && span_ok;
                report["full_chain"]["loss"][spec.node] =
                    node_verdict_json(&nv, span_secs, span_ok, cfg.min_secs, real_drops_allowance);
                // #870 — per-hop burn-id UNIQUENESS / MAX-HOLD assertion. `burn_contiguity` is
                // presence-only (a `BTreeSet`), so a hop that REPEATS frames (the identical
                // rendered image delivered on many consecutive recorded frames) leaves it clean.
                // Measured on the STREAM recording — the single surface carrying every node burn,
                // and where a freeze/repeat manifests to the viewer — from the SAME recorded-order
                // `(frame_index, id)` extractor contiguity uses (no new decode); the frame index
                // lets a recorded gap (an undecodable burn in between) break a hold rather than
                // inflate it. NOTE the source recording differs from the loss source for a
                // camera-under-test node: cam1/cam3/… loss is read from the CLEAN strih recording
                // (`cam1_source`), but `hold` is deliberately the stream recording (the delivered,
                // viewer-facing surface) even under the same `full_chain.loss.<node>.hold` key.
                // The pure decision core is `camera_box::burn_hold`; it is LIVE today
                // (`burn_hold::gates_overall_pass() == true`) — the calibrated max-hold bound folds
                // into `overall_pass`, so a hop that re-delivers one burn id past the bound FAILS
                // the run. Flipped LIVE (#870) after its green-run distribution accumulated (worst
                // green max_hold 2, bound 4, incl. cam1/issue-909); the fold below is a one-line
                // revert to report-only if a future rig change ever trips it.
                // #575/#870: trim the recording START/STOP boundary (genlock pre-roll flush +
                // mux-finalization tail-drain holding the final frame — confirmed live, run
                // 554307) off the hold input BEFORE the max-hold walk, anchored on the STREAM
                // recording's OWN frame-index bounds — the SAME position-trim the imag leg applies
                // (`node_verdict_for_imag`). Without it a boundary-artifact freeze of the final
                // frame (a KNOWN non-loss class, #575) could falsely trip the LIVE bound.
                let stream_first_idx = stream_frames.first().map(|f| f.frame_index).unwrap_or(0);
                let stream_last_idx = stream_frames.last().map(|f| f.frame_index).unwrap_or(0);
                let hold = camera_box::burn_hold::burn_hold_distribution(
                    spec.node,
                    &camera_box::recording_boundary_trim::trim_boundary_pairs(
                        &burn_ids_with_frame_index_in(stream_frames, spec.burn_run_id),
                        stream_first_idx,
                        stream_last_idx,
                        camera_box::recording_boundary_trim::BOUNDARY_TRIM_LEAD_FRAMES,
                        camera_box::recording_boundary_trim::BOUNDARY_TRIM_TAIL_FRAMES,
                    ),
                );
                let hold_within = hold.within_bound(camera_box::burn_hold::MAX_HOLD_FRAMES);
                report["full_chain"]["loss"][spec.node]["hold"] = serde_json::json!({
                    "max_hold_frames": hold.max_hold_frames,
                    "max_hold_id": hold.max_hold_id,
                    "bound": camera_box::burn_hold::MAX_HOLD_FRAMES,
                    "within_bound": hold_within,
                    "duplicate_pairs": hold.duplicate_pairs,
                    "adjacent_pairs": hold.adjacent_pairs,
                    "duplicate_pair_fraction": hold.duplicate_pair_fraction(),
                    "total_burn_frames": hold.total_burn_frames,
                    "distinct_ids": hold.distinct_ids,
                    "histogram": hold.histogram,
                    // Scoped per-term gate flag (#870) — name the flag after the specific term, not
                    // the whole object (optical-undecodable-floor-report-only.md).
                    "gates_overall_pass": camera_box::burn_hold::gates_overall_pass(),
                });
                if !hold_within {
                    println!(
                        "  [{}] #870 REPEAT/max-hold: burn id {} held for {} consecutive recorded \
                         frames (> bound {}); {:.1}% of adjacent pairs byte-identical ({}/{}). \
                         LIVE (#870) — this FAILS the run.",
                        spec.node,
                        hold.max_hold_id
                            .map(|id| id.to_string())
                            .unwrap_or_else(|| "<none>".to_string()),
                        hold.max_hold_frames,
                        camera_box::burn_hold::MAX_HOLD_FRAMES,
                        hold.duplicate_pair_fraction() * 100.0,
                        hold.duplicate_pairs,
                        hold.adjacent_pairs,
                    );
                }
                // LIVE fold (#870) — `gates_overall_pass()` is `true`, so an over-bound hold clears
                // `all_pass` and FAILS the run. Flip the seam back to `false` for a one-line revert
                // to report-only.
                all_pass &= hold_within || !camera_box::burn_hold::gates_overall_pass();
                node_verdicts.push(nv);
            }
            // The single binary headline, in plain words.
            let total_real: usize = node_verdicts.iter().map(NodeVerdict::real_drops).sum();
            let total_burn_unreadable: usize =
                node_verdicts.iter().map(NodeVerdict::burn_unreadable).sum();
            // #904 — which camera-under-test node(s), if any, only passed because of the
            // real_drops allowance — the LOUD run-level signal (#905 tracks re-tightening).
            let allowance_consumed_nodes: Vec<String> = node_verdicts
                .iter()
                .filter(|nv| nv.consumed_real_drops_allowance(real_drops_allowance))
                .map(|nv| nv.contiguity.node.clone())
                .collect();
            // #373 — the headline is ZERO loss only when every node is delivery-clean (#904:
            // within its real_drops allowance) AND its analyzed optical span cleared the duration
            // floor (no vacuous pass over a collapsed read).
            let all_zero = node_verdicts.iter().all(|nv| {
                nv.is_zero_within_allowance(real_drops_allowance)
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
            if all_zero && allowance_consumed_nodes.is_empty() {
                println!(
                    "  >>> ZERO loss: all burn-id sequences CONTIGUOUS (no missing id on any node)."
                );
            } else if all_zero {
                // #904/#1169 — LOUD: a genuine pass, but NOT a strict zero-loss pass — never let
                // this look identical to the clean branch above. Issue 1169 (owner, 2026-08-22)
                // re-widened the default to the <=1 singleton band and is the re-tighten trail.
                println!(
                    "  >>> ⚠ #1169 REAL-DROPS SINGLETON ALLOWANCE: real-drops singleton allowance \
                     consumed: {total_real} — issue 1169 re-tighten trail. Within the per-node \
                     allowance of {real_drops_allowance} on: {} — 0 BURN-UNREADABLE, everything \
                     else at the usual strict bar; 2+ of anything still FAILS.",
                    allowance_consumed_nodes.join(", ")
                );
            } else {
                println!(
                    "  >>> NOT zero: {total_real} REAL DROP + {total_burn_unreadable} BURN-UNREADABLE \
                     (each id classified above with its pixel slot; fix every burn-unreadable burn)."
                );
            }
            report["full_chain"]["zero_loss"] = serde_json::Value::Bool(all_zero);
            // #904 — always present (not just when consumed) so a JSON consumer can see the bar
            // this run was judged against, and which node(s) (if any) needed it.
            report["full_chain"]["real_drops_allowance"] = serde_json::json!(real_drops_allowance);
            report["full_chain"]["real_drops_allowance_consumed_nodes"] =
                serde_json::json!(allowance_consumed_nodes);
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
        let allowance = camleg_v4l2_drop_allowance();
        let capture_zero = stats.v4l2_dropped == 0;
        // #1169 THIRD SEAM — a LOUD singleton band on the RAW cam-leg V4L2 capture-drop counter.
        // A count WITHIN `allowance` is an UPSTREAM camera-leg buffer drop the issue-1167 emit-fill
        // absorbs by design, so it PASSES with a loud note instead of double-redding what the
        // presented layer already compensated; `> allowance` still FAILS unchanged.
        let (within_band, band_consumed) = camleg_capture_band(stats.v4l2_dropped, allowance);
        let note = band_consumed.then(|| {
            format!(
                "cam-leg V4L2 singleton band consumed: {}/{} — absorbed by the issue-1167 emit \
                 fill; issue 1169 re-tighten trail",
                stats.v4l2_dropped, allowance
            )
        });
        if capture_zero {
            println!(
                "  [cam2→{camera_under_test_label}] ZERO loss — {camera_under_test_label} V4L2 \
                 capture dropped 0 frames ({} captured).",
                stats.frames_captured
            );
        } else if band_consumed {
            // Denominator is the TOTAL the device should have produced = delivered + dropped.
            // LOUD: a PASS, but NOT a strict zero — never let it look like the clean branch above.
            let total = stats.frames_captured.saturating_add(stats.v4l2_dropped);
            println!(
                "  >>> ⚠ #1169 CAM-LEG V4L2 SINGLETON BAND: [cam2→{camera_under_test_label}] \
                 {camera_under_test_label} V4L2 capture dropped {} of {} frames ({} delivered) — \
                 WITHIN the singleton band ({allowance}); absorbed by the issue-1167 emit fill; \
                 issue 1169 re-tighten trail.",
                stats.v4l2_dropped, total, stats.frames_captured
            );
        } else {
            // Denominator is the TOTAL the device should have produced = delivered + dropped
            // (frames_captured counts only delivered buffers, not the lost ones).
            let total = stats.frames_captured.saturating_add(stats.v4l2_dropped);
            println!(
                "  [cam2→{camera_under_test_label}] NOT zero — {camera_under_test_label} V4L2 \
                 capture dropped {} of {} frames ({} delivered; REAL capture-card drops on the \
                 camera leg — OVER the issue-1169 singleton band of {allowance}).",
                stats.v4l2_dropped, total, stats.frames_captured
            );
        }
        all_pass &= within_band;
        let mut node = serde_json::json!({
            "zero_loss": within_band,
            "capture_zero": capture_zero,
            "v4l2_dropped": stats.v4l2_dropped,
            "frames_captured": stats.frames_captured,
            "camleg_v4l2_drop_allowance": allowance,
            "camleg_singleton_band_consumed": band_consumed,
            "source": format!(
                "{camera_under_test_label} V4L2 sequence-gap capture-drop (camera leg) — not a painter-tick compare"
            ),
        });
        if let Some(note) = note {
            node["note"] = serde_json::Value::String(note);
        }
        report["full_chain"]["loss"][format!("cam2_{camera_under_test_label}")] = node;
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
        // #904 — imag is UNTOUCHED by the real_drops allowance (allowance=0 here reproduces the
        // pre-#904 strict `is_zero()` exactly; imag's own gate is the beat-aware optical verdict,
        // not the non-imag contiguity fallback the allowance applies to — see
        // `NodeVerdict::optical_ok_within_allowance`).
        print_node_verdict(&nv, span_ok, 0);
        if !span_ok {
            println!(
                "  [imag] NOT zero — analyzed optical span {:.1}s < {:.1}s floor: the cam2 \
                 dual-QR read COLLAPSED to {} frame(s); a contiguous tick window over so few \
                 frames proves nothing (#373).",
                span_secs, cfg.min_secs, nv.optical_span_frames
            );
        }
        // issue 798 (path A) -> #1142 STRICT flip — SPLIT the imag leg into a BLOCKING
        // presence/verification term and a REPORT-ONLY per-frame content term. Issue 1130 comment
        // 5347311707 proved the imag ~19.5% repeated ticks are an OBSERVER EFFECT: the E2E x264
        // software encode starves the imag iGPU (PL1 30W clamp) past the 16.7 ms graphics budget so
        // OBS repeats whole RENDERS — only during the record window ("churn, not loss", avg_step
        // 1.006). Both the optical BEAT and the digital-BURN Δ0 are confounded by it, so gating the
        // per-frame terms now would false-red every run on the recorder's own load, not the chain.
        // - PRESENCE/VERIFICATION (BLOCKING now): the cam2 optical undecodable moiré floor (#376 —
        //   a repeated decodable frame is still decodable, so this rate is NOT inflated by the
        //   repeats) and the analyzed-span floor (#373). Not confounded by frame-repeat. The
        //   `colour_fail == 0` term is included to make this composite EXACTLY `is_zero()`'s
        //   decomposition, but it is STRUCTURALLY always-true for imag (`node_verdict_for_imag`
        //   hardcodes `colour_fail: 0`; imag carries no sampled colour), so it can never itself red
        //   — do NOT advertise imag colour as an active gate.
        // - PER-FRAME CONTENT (REPORT-ONLY, pending the issue 1143 imag encoder fix): the
        //   optical-beat freeze/stuck verdict (`optical_ok`) + the digital-burn contiguity
        //   (`imag_burn_ok`). Surfaced but never reds a run.
        let imag_presence_ok = nv.optical_undecodable_ok() && nv.colour_fail == 0 && span_ok;
        let imag_content_ok = nv.optical_ok() && nv.imag_burn_ok();
        // #1142 — the imag PRESENCE terms gate overall_pass ONLY when this run's contract REQUIRES a
        // verified imag leg (`--require-imag-leg`, set by the production full-chain merge). An
        // isolated/unit verdict run that happens to carry an imag partial surfaces it but does not
        // gate on it (so the many imag-less/isolated verdict tests stay green).
        if args.require_imag_leg {
            all_pass &= camera_box::imag_leg_gate::folds_into_overall_pass(imag_presence_ok);
        }
        // The PER-FRAME CONTENT fold is report-only (a no-op) regardless of the flag.
        all_pass &= camera_box::imag_leg_gate::content_folds_into_overall_pass(imag_content_ok);
        // The honest FULL imag verdict (presence AND content), surfaced as `imag_leg_pass`.
        let imag_leg_ok = nv.is_zero() && span_ok;
        let mut imag_json = node_verdict_json(&nv, span_secs, span_ok, cfg.min_secs, 0);
        // #575 — note the boundary trim honestly: the exact lead/tail frame counts excluded from
        // imag's optical tick + digital burn contiguity checks before this verdict was computed.
        imag_json["boundary_trim_lead_frames"] =
            serde_json::json!(camera_box::recording_boundary_trim::BOUNDARY_TRIM_LEAD_FRAMES);
        imag_json["boundary_trim_tail_frames"] =
            serde_json::json!(camera_box::recording_boundary_trim::BOUNDARY_TRIM_TAIL_FRAMES);
        // issue 798 -> #1142 — scoped split flags (name the TERM, not the whole object, per
        // optical-undecodable-floor-report-only.md). `imag_leg_pass` is this run's FULL imag verdict
        // (optical beat + digital burn contiguity + undecodable + colour, ANDed with span_ok);
        // `imag_presence_pass` (BLOCKING when --require-imag-leg) and `imag_content_pass`
        // (REPORT-ONLY) are the #1142 split. `gates_overall_pass` is `true` (presence seam LIVE);
        // `content_gates_overall_pass` is `false` (per-frame content report-only, issue 1130).
        imag_json["imag_leg_pass"] = serde_json::json!(imag_leg_ok);
        // #1142 — scoped split: presence/verification BLOCKS, per-frame content report-only.
        imag_json["imag_presence_pass"] = serde_json::json!(imag_presence_ok);
        imag_json["imag_content_pass"] = serde_json::json!(imag_content_ok);
        imag_json["gates_overall_pass"] =
            serde_json::json!(camera_box::imag_leg_gate::gates_overall_pass());
        imag_json["content_gates_overall_pass"] =
            serde_json::json!(camera_box::imag_leg_gate::content_gates_overall_pass());
        imag_json["report_only_note"] = serde_json::json!(
            "issue 798 path A -> #1142: the imag PRESENCE/VERIFICATION terms (analyzed-span floor \
             #373 + cam2 undecodable moiré floor #376) now BLOCK overall_pass (only when \
             --require-imag-leg); the \
             PER-FRAME CONTENT terms (imag_content_pass: digital-burn contiguity + optical beat) \
             stay REPORT-ONLY pending the issue 1143 imag encoder fix (issue 1130 x264 record-load \
             observer effect — content_gates_overall_pass==false)."
        );
        // #1143 — surface OBS's own record-session render stats REPORT-ONLY (never gates). A high
        // `record_render_lagged_pct` means the RECORDER itself juddered the render (the x264
        // observer effect, #1130) — so a stuck/copy reading on this run is attributable to the
        // recording load, not the delivery chain. ~18.4% under x264, ~0% under the VAAPI-tex fix.
        // `max_render_ms` (#1143 Task 4) is the render budget measured DURING the active recording.
        if let Some(rr) = &imag_record_render {
            imag_json["record_render_lagged_pct"] = serde_json::json!(rr.lagged_pct);
            imag_json["record_render_lagged_frames"] = serde_json::json!(rr.lagged_frames);
            imag_json["record_render_attempted_frames"] = serde_json::json!(rr.attempted_frames);
            imag_json["record_render_drawn_frames"] = serde_json::json!(rr.drawn_frames);
            imag_json["record_render_max_render_ms"] = serde_json::json!(rr.max_render_ms);
        }
        report["full_chain"]["loss"]["imag"] = imag_json;
    }
    // issue 798 -> #1142 — make a silently-skipped imag leg VISIBLE (the "ONE full test, no
    // partials" doctrine) AND now RED it. `imag_leg_verified` records whether an imag partial
    // actually reached this merge (0/76 status quo). A green run that never merged an imag partial
    // is a HIDDEN partial; #1142 makes it BLOCKING (owner honesty mandate 2026-08-19) — a run that
    // silently skipped imag, or dropped it via the issue 1118 schema-degrade (which sets
    // imag_leg_verified=false, so a degraded run now REDs, not silently passes), reds overall_pass.
    let imag_leg_verified = imag_frames_opt.is_some();
    report["full_chain"]["imag_leg_verified"] = serde_json::json!(imag_leg_verified);
    // #1142 — the ONE sanctioned skip is an operator-acknowledged offline imag (#1013): when imag
    // is in the CAMBOX_OFFLINE_ACK set (--offline-ack-cams), an absent leg is EXPECTED and must not
    // red. `verified_leg_ok(verified, offline_acked)` folds through the LIVE presence seam.
    let imag_offline_acked =
        camera_box::offline_ack::parse(&args.offline_ack_cams).contains_key("imag");
    // #1142 — the verified fold gates ONLY when this run's contract REQUIRES the imag leg
    // (`--require-imag-leg`, set by the production full-chain merge). Without it (the many
    // isolated/unit verdict runs with no imag partial), a missing imag leg is surfaced but never
    // reds. WITH it, a silently-skipped imag leg REDs unless imag is operator-offline-acked (#1013).
    let imag_leg_verified_gates =
        args.require_imag_leg && camera_box::imag_leg_gate::gates_overall_pass();
    if args.require_imag_leg {
        all_pass &= camera_box::imag_leg_gate::folds_into_overall_pass(
            camera_box::imag_leg_gate::verified_leg_ok(imag_leg_verified, imag_offline_acked),
        );
    }
    report["full_chain"]["imag_leg_required"] = serde_json::json!(args.require_imag_leg);
    report["full_chain"]["imag_leg_verified_offline_acked"] = serde_json::json!(imag_offline_acked);
    report["full_chain"]["imag_leg_verified_gates_overall_pass"] =
        serde_json::json!(imag_leg_verified_gates);
    if !imag_leg_verified {
        println!(
            "  [imag] leg NOT verified this run — no imag partial merged (--merge-partials imag=... \
             absent; imag StopRecord / reachability / decode failed at recording-e2e.sh [8/8c], a \
             strih/stream-only merge, or the issue 1118 schema-degrade dropped it). A full-chain \
             E2E that silently skips the imag leg is a hidden partial (issue 798). {}",
            if imag_offline_acked {
                "imag is operator-offline-acked (#1013) — the ONE sanctioned skip: this does NOT \
                 red overall_pass."
            } else {
                "#1142: this now REDs overall_pass (imag_leg_verified is BLOCKING) unless imag is \
                 operator-offline-acked."
            }
        );
    }
    // issue 1118 -> #1142 — when the imag partial was DROPPED because its schema mismatched this
    // build (a stale on-imag emitter after a PARTIAL_SCHEMA_VERSION bump), record WHY beside the
    // bare `imag_leg_verified=false`, so a degraded run is mineable rather than looking like a plain
    // "imag never ran". The DEGRADE is unchanged (the merge still computes the verdict from the
    // remaining strih+stream partials instead of hard-dying) — but #1142 makes the resulting
    // `imag_leg_verified=false` BLOCKING (unless imag is operator-offline-acked, #1013): a
    // schema-degraded imag leg now REDs overall_pass instead of silently passing (owner mandate:
    // "schema degrade smie ostať degrade, ale musí RED-ovať, nie ticho prejsť").
    if let Some(reason) = &imag_skip_reason {
        report["full_chain"]["imag_leg_skip_reason"] = serde_json::json!(reason);
        println!(
            "  [imag] leg DEGRADED this run (issue 1118): {reason} \
             Verdict computed from the remaining (strih+stream) partials; #1142: the resulting \
             imag_leg_verified=false REDs overall_pass unless imag is operator-offline-acked."
        );
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
                    args.burn_cam7_run_id,
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
                    // Issue 889 (2026-07-30 user decision on issue 883) visibility requirement 1,
                    // extended by issue 915 (2026-08-01 user decision) and the 2026-08-05 RE-GATE
                    // (ticket 889 comment 5196190653): a loud WARN naming EVERY reason a window's
                    // STRICT verdict (`s.pass`) is false — whether or not that reason still gates
                    // `overall_pass`. Since the re-gate, `copies`/`gaps` are ONLY rescued WITHIN
                    // the per-window tolerance (`seg.copies_gaps_tolerance`) — OVER it,
                    // the window fails `relaxed_pass` again, so the `else` branch below is no
                    // longer only `frame_count == 0` and must name the real reason(s).
                    if !s.pass {
                        if s.relaxed_pass {
                            let floor_ok = camera_box::optical_floor::window_within_floor(
                                s.undecodable,
                                s.frames,
                            );
                            if !floor_ok {
                                println!(
                                    "      ⚠ #915 REPORT-ONLY: undecodable={} exceeds the \
                                     issue-881 per-window floor, but does NOT gate overall_pass \
                                     while issue 909 (cam1 grabber) + issue 881 (120Hz monitor) \
                                     are unresolved (see issue #915).",
                                    s.undecodable
                                );
                            }
                            if s.copies != 0 || s.gaps != 0 {
                                // #1132 (owner mandate 2026-08-19): the copies/gaps tolerance
                                // rescue is DISARMED, so a within-tolerance nonzero copies/gaps
                                // window (relaxed_pass == true) now GATES overall_pass. The old
                                // "does NOT gate" wording would be a LIE here (the exact masking
                                // the owner removed) -- key the message off the live seam so it can
                                // never claim "does NOT gate" for a window that actually fails.
                                if camera_box::window_gate::copies_gaps_tolerance_gates_overall_pass(
                                ) {
                                    println!(
                                        "      ⚠ #889 WITHIN TOLERANCE: copies={} gaps={} fails the \
                                         pre-889 strict rule, but stays within the per-window \
                                         singleton tolerance ({}) and does NOT gate overall_pass \
                                         (see issue #889 for the decision record).",
                                        s.copies, s.gaps, seg.copies_gaps_tolerance
                                    );
                                } else if camera_box::window_gate::segment_singleton_allowance_consumed(
                                    s.copies, s.gaps,
                                ) {
                                    // #1169 (owner, 2026-08-22): a <=1/<=1 SINGLETON is ABSORBED
                                    // into overall_pass (the designed issue-1167 paced-trickle +
                                    // FIFO stale_replay residual) -- LOUDLY (strict pass stays
                                    // false above), never masked. >=2 of either hits the
                                    // STRICT-ESCALATE branch below and still fails.
                                    println!(
                                        "      ⚠ #1169 SINGLETON ALLOWANCE: copies={} gaps={} \
                                         (<= {}/{} each) -- ABSORBED into overall_pass as the \
                                         designed issue-1167 paced-trickle + FIFO stale_replay \
                                         residual; the STRICT verdict still FAILS (report-only, \
                                         visible) and 2+ of either still fails. Re-tighten trail: \
                                         issue 1169.",
                                        s.copies,
                                        s.gaps,
                                        camera_box::window_gate::SEGMENT_SINGLETON_COPIES_ALLOWANCE,
                                        camera_box::window_gate::SEGMENT_SINGLETON_GAPS_ALLOWANCE
                                    );
                                } else {
                                    println!(
                                        "      ⚠ #1132 STRICT-ESCALATE: copies={} gaps={} -- the \
                                         relaxed copies/gaps rescue is DISARMED (owner mandate) and \
                                         this is over the issue-1169 singleton allowance; this \
                                         window FAILS overall_pass and must be escalated, never \
                                         masked (see issue #1132). relaxed_pass reports the old \
                                         tolerant verdict above for observability only.",
                                        s.copies, s.gaps
                                    );
                                }
                            }
                        } else {
                            // `relaxed_pass` fails too — name every real reason via the pure,
                            // Tier-0-testable seam `camera_box::window_gate::
                            // relaxed_failure_reasons` (issue-889 re-gate deep-review findings
                            // 1+2, fixed through this seam rather than re-deriving the conditions
                            // inline again): a frame_count==0 window is EmptyWindow (never
                            // misread as an exceeded floor — the old inline logic here made that
                            // exact mistake, since `optical_floor::window_within_floor`'s
                            // defensive frame_count==0 clause always reads `false`), an
                            // over-tolerance copies/gaps failure is OverCopiesGapsTolerance, and
                            // an over-floor undecodable count is worded as actually gating
                            // (FloorExceededGating) ONLY when `optical_floor::gates_overall_pass()`
                            // is genuinely `true` — never unconditionally, since that flag is
                            // hardcoded `false` today (see issue #915 for the restore path on
                            // issue #905). A window can carry more than one reason at once (e.g.
                            // over-tolerance copies/gaps AND a merely-report-only over-floor
                            // undecodable count) — every applicable reason prints.
                            let reasons = camera_box::window_gate::relaxed_failure_reasons(
                                s.frames,
                                s.undecodable,
                                s.copies,
                                s.gaps,
                            );
                            for reason in &reasons {
                                match reason {
                                    camera_box::window_gate::RelaxedFailureReason::EmptyWindow => {
                                        println!(
                                            "      ⚠ this window fails the RELAXED verdict (not \
                                             issue 889's, issue 915's, nor the re-gate's doing — \
                                             frame_count==0) — it still fails overall_pass."
                                        );
                                    }
                                    camera_box::window_gate::RelaxedFailureReason::OverCopiesGapsTolerance => {
                                        println!(
                                            "      ⚠ #889 RE-GATE FAIL: copies={} gaps={} \
                                             exceeds the per-window singleton tolerance ({}) — \
                                             this window FAILS overall_pass (see issue #889 for \
                                             the decision record).",
                                            s.copies, s.gaps, seg.copies_gaps_tolerance
                                        );
                                    }
                                    camera_box::window_gate::RelaxedFailureReason::FloorExceededGating => {
                                        println!(
                                            "      ⚠ undecodable={} exceeds the issue-881 \
                                             per-window floor and currently gates overall_pass \
                                             (see issue #915 for the restore-path state).",
                                            s.undecodable
                                        );
                                    }
                                    camera_box::window_gate::RelaxedFailureReason::FloorWithinReportOnly => {
                                        println!(
                                            "      ⚠ #915 REPORT-ONLY: undecodable={} exceeds \
                                             the issue-881 per-window floor, but does NOT gate \
                                             overall_pass while issue 909 (cam1 grabber) + issue \
                                             881 (120Hz monitor) are unresolved (see issue #915).",
                                            s.undecodable
                                        );
                                    }
                                }
                            }
                            if reasons.is_empty() {
                                // Should not happen for a window that genuinely failed
                                // `relaxed_pass` (`window_gate::decide` and `relaxed_failure_
                                // reasons` derive the same conditions) — but a window that DOES
                                // fail overall_pass must never print silence.
                                println!(
                                    "      ⚠ this window fails the RELAXED verdict for an \
                                     unrecognized reason (frames={} undecodable={} copies={} \
                                     gaps={}) — it still fails overall_pass.",
                                    s.frames, s.undecodable, s.copies, s.gaps
                                );
                            }
                        }
                    }
                    // #726: presentation-cadence EVENNESS — this per-window PRINT is report-only;
                    // #1036 gates the RUN-level worst `paired_fraction` (the cadence_judder_gate
                    // term below; see src/presentation_cadence.rs). `None` on any window with no
                    // painted tick (every non-cam2 window in a sweep).
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
                        // #726 MISCALIBRATION FIX: the auto-calibrated (data-derived) reading +
                        // raw delta histogram, printed alongside the caller-supplied-step line
                        // above so a self-consistency mismatch is visible directly in the report
                        // (a zero-copies/zero-gaps window should show derived_uniform close to
                        // sample_deltas even when the line above's `uniform` is near 0 — see
                        // src/presentation_cadence.rs).
                        println!(
                            "      cadence(derived): step={} uniform={}/{} ({:.3}) histogram={:?}",
                            pc.derived_expected_step,
                            pc.derived_uniform_steps,
                            pc.sample_deltas,
                            pc.derived_uniform_fraction,
                            pc.delta_histogram
                        );
                    }
                }
                // Issue 889 visibility requirement 3 — this summary line prints UNCONDITIONALLY,
                // whether or not any window failed, so silence is never mistaken for strictness.
                // Requirement 4 — hardcoded, one-line-deletable, no env knob (see the field's own
                // doc in `crate::probe::recording_segments::SegmentedContinuity` for the restore
                // path — issue 883 item 4 + two consecutive clean strict runs).
                //
                // 2026-08-05 RE-GATE (ticket 889 comment 5196190653): `windows_failed_report_only`
                // (the STRICT absolute-zero count) no longer describes what actually gates
                // `overall_pass` — that is now `windows_over_copies_gaps_tolerance` (the
                // per-window tolerance, `seg.copies_gaps_tolerance`). Name the tolerance
                // explicitly so a reader never mistakes "no longer gates" (no longer true) for
                // the current behavior.
                println!(
                    "  ⚠ #889 STRICT-ZERO (report-only): {}/{} cambox window(s) would FAIL the \
                     pre-889 absolute-zero copies/gaps rule (windows_failed_report_only) — this \
                     count stays visible but does NOT by itself gate overall_pass.",
                    seg.windows_failed_report_only,
                    seg.segments.len()
                );
                println!(
                    "  ⚠ #889 RE-GATE (per-window tolerance={}): {}/{} cambox window(s) exceed the \
                     per-window copies/gaps tolerance (windows_over_copies_gaps_tolerance) — a \
                     SUBSET of what now gates under #1132 (see the #1132 line below and issue #889 \
                     for the decision record). NOTE: this is the <=3 relaxed tolerance, NOT the \
                     issue-1169 <=1 SINGLETON allowance (a separate, tighter band; see its own line).",
                    seg.copies_gaps_tolerance,
                    seg.windows_over_copies_gaps_tolerance,
                    seg.segments.len()
                );
                // #1132 (owner mandate 2026-08-19): the copies/gaps tolerance (`<=3`) rescue is
                // DISARMED. #1169 (owner, 2026-08-22) then RE-INTRODUCED a strictly-tighter
                // `<=1/<=1` SINGLETON allowance: a single copy/gap is ABSORBED (the designed
                // issue-1167 paced-trickle + FIFO stale_replay residual) while `>=2` of either
                // still FAILS. Split the count so both the loud absorption AND the loud escalation
                // are visible, never masked. When either seam is re-armed/re-tightened these lines
                // shift accordingly (the #889 tolerance line above governs when re-armed).
                if !camera_box::window_gate::copies_gaps_tolerance_gates_overall_pass() {
                    let windows_over_singleton = seg
                        .segments
                        .iter()
                        .filter(|s| {
                            s.frames > 0
                                && (s.copies != 0 || s.gaps != 0)
                                && !camera_box::window_gate::segment_singleton_allowance_consumed(
                                    s.copies, s.gaps,
                                )
                        })
                        .count();
                    if seg.windows_singleton_allowance_consumed > 0 {
                        println!(
                            "  ⚠ #1169 SINGLETON ALLOWANCE: {}/{} cambox window(s) had a <= {}/{} \
                             copies/gaps singleton ABSORBED into overall_pass (the designed \
                             issue-1167 paced-trickle + FIFO stale_replay residual; each carries a \
                             per-segment note, strict pass stays false/visible). Re-tighten trail: \
                             issue 1169.",
                            seg.windows_singleton_allowance_consumed,
                            seg.segments.len(),
                            camera_box::window_gate::SEGMENT_SINGLETON_COPIES_ALLOWANCE,
                            camera_box::window_gate::SEGMENT_SINGLETON_GAPS_ALLOWANCE
                        );
                    }
                    println!(
                        "  ⚠ #1132 STRICT: copies/gaps rescue DISARMED — {}/{} cambox window(s) \
                         carry copies/gaps OVER the issue-1169 singleton allowance and FAIL \
                         overall_pass (escalate, never mask). See issue #1132 / issue #1169.",
                        windows_over_singleton,
                        seg.segments.len()
                    );
                }
                // Issue 915 (2026-08-01 user decision) visibility requirement, mirrors #889
                // requirement 3 — prints UNCONDITIONALLY whether or not the run-wide floor was
                // exceeded, so silence is never mistaken for strictness. Hardcoded,
                // one-line-deletable — see `camera_box::optical_floor::gates_overall_pass` for
                // the restore path on issue 905.
                println!(
                    "  ⚠ #915 REPORT-ONLY: run-wide undecodable={} (floor {}, within_floor={}) \
                     -- no longer gates overall_pass while issue 909 (cam1 grabber) + issue 881 \
                     (120Hz monitor) are unresolved (see issue #915 for the decision record and \
                     issue #905 for the restore path).",
                    seg.total_undecodable,
                    camera_box::optical_floor::RUN_UNDECODABLE_FLOOR,
                    seg.run_wide_undecodable_within_floor
                );
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
                    // Issue 915 (2026-08-01 user decision): an unambiguous machine-readable flag
                    // scoped to the optical undecodable floor specifically (NOT a blanket
                    // "gates_overall_pass" on the whole object — frame_count/schedule-non-empty
                    // still gate this object's `overall_pass`, only the floor term stopped).
                    // Mirrors the field name/shape issue 861/914 already established for their
                    // fully-decoupled terms.
                    obj.insert(
                        "undecodable_floor_gates_overall_pass".to_string(),
                        serde_json::json!(camera_box::optical_floor::gates_overall_pass()),
                    );
                    obj.insert(
                        "undecodable_floor_gate".to_string(),
                        serde_json::json!(
                            "report-only -- the issue-881 optical undecodable floor (per-window \
                             + run-wide) does NOT gate overall_pass, pending issue 909 (cam1 \
                             grabber) + issue 881 (120Hz monitor) (see issue #915 for the \
                             decision record and issue #905 for the restore path)"
                        ),
                    );
                    // Finding 5 of the issue-889 re-gate deep review — a self-describing prose
                    // gate key, mirroring `undecodable_floor_gate`'s idiom immediately above but
                    // SCOPED by name to `copies`/`gaps` specifically (issue-915 lesson: never a
                    // blanket "gate" key on the whole object). #1132 (2026-08-19): the tolerance
                    // rescue is DISARMED, so the prose now describes the STRICT policy (any nonzero
                    // copies/gaps gates) while it is disarmed, and reverts to the #889 tolerance
                    // wording if the seam is re-armed -- the JSON self-describes either way.
                    let copies_gaps_tol_gates =
                        camera_box::window_gate::copies_gaps_tolerance_gates_overall_pass();
                    obj.insert(
                        "copies_gaps_gate".to_string(),
                        serde_json::json!(if copies_gaps_tol_gates {
                            format!(
                                "gates overall_pass above the per-window singleton tolerance ({}) -- \
                                 see issue #889 for the decision record",
                                seg.copies_gaps_tolerance
                            )
                        } else {
                            "#1132: the copies/gaps tolerance rescue is DISARMED -- overall_pass \
                             gates on ANY nonzero copies/gaps (strict copies==0 && gaps==0); the \
                             tolerance is dormant/reported-only. See issue #1132."
                                .to_string()
                        }),
                    );
                    // #1132 (owner mandate 2026-08-19): the machine-readable companion to the prose
                    // above, mirroring `undecodable_floor_gates_overall_pass`. `false` = the rescue
                    // is disarmed, so overall_pass folds strict copies==0 && gaps==0; flip the seam
                    // (`window_gate::copies_gaps_tolerance_gates_overall_pass`) to re-arm it.
                    obj.insert(
                        "copies_gaps_tolerance_gates_overall_pass".to_string(),
                        serde_json::json!(copies_gaps_tol_gates),
                    );
                    // #1169 (owner, 2026-08-22): the SINGLETON allowance seam, self-describing in
                    // the JSON exactly like the copies_gaps keys above -- a DISTINCT, strictly
                    // tighter (<=1/<=1) band than the disarmed <=3 tolerance. `armed=true` = a
                    // <=1/<=1 singleton is absorbed into overall_pass (loudly, strict stays false);
                    // >=2 of either still fails. Re-tighten to absolute zero = flip the arm to false.
                    let singleton_armed =
                        camera_box::window_gate::segment_singleton_allowance_gates_overall_pass();
                    obj.insert(
                        "segment_singleton_allowance_gates_overall_pass".to_string(),
                        serde_json::json!(singleton_armed),
                    );
                    obj.insert(
                        "segment_singleton_copies_allowance".to_string(),
                        serde_json::json!(
                            camera_box::window_gate::SEGMENT_SINGLETON_COPIES_ALLOWANCE
                        ),
                    );
                    obj.insert(
                        "segment_singleton_gaps_allowance".to_string(),
                        serde_json::json!(
                            camera_box::window_gate::SEGMENT_SINGLETON_GAPS_ALLOWANCE
                        ),
                    );
                    obj.insert(
                        "segment_singleton_gate".to_string(),
                        serde_json::json!(if singleton_armed {
                            format!(
                                "#1169: a <= {}/{} copies/gaps SINGLETON is absorbed into \
                                 overall_pass (the designed issue-1167 paced-trickle + FIFO \
                                 stale_replay residual), loudly (strict pass stays false, a \
                                 per-segment singleton_allowance_note fires, \
                                 windows_singleton_allowance_consumed counts it); 2+ of either \
                                 still fails. Re-tighten to absolute zero = flip \
                                 segment_singleton_allowance_gates_overall_pass to false.",
                                camera_box::window_gate::SEGMENT_SINGLETON_COPIES_ALLOWANCE,
                                camera_box::window_gate::SEGMENT_SINGLETON_GAPS_ALLOWANCE
                            )
                        } else {
                            "#1169 singleton allowance DISARMED -- overall_pass folds strict \
                             copies==0 && gaps==0."
                                .to_string()
                        }),
                    );
                    // #1132 review finding (2026-08-19): the EXACT count of windows carrying any
                    // nonzero copies/gaps on a non-empty window -- serialized for mineability
                    // parity. #1169 (2026-08-22): this is now a SUPERSET of the windows that GATE
                    // overall_pass -- it also counts the <=1/<=1 SINGLETON windows now ABSORBED (see
                    // `windows_singleton_allowance_consumed`, already serialized on the object). The
                    // windows that actually gate = this count MINUS windows_singleton_allowance_consumed.
                    // `windows_over_copies_gaps_tolerance` remains a SUBSET (misses within-<=3
                    // nonzero windows), `windows_failed_report_only` a SUPERSET (adds floor-only
                    // strict failures). Same value the #1132 STRICT stdout summary derives from.
                    obj.insert(
                        "windows_with_copies_or_gaps".to_string(),
                        serde_json::json!(seg
                            .segments
                            .iter()
                            .filter(|s| s.frames > 0 && (s.copies != 0 || s.gaps != 0))
                            .count()),
                    );
                }
                report["all_cambox_continuity"] = seg_json;
                all_pass &= seg.overall_pass;

                // #781 — REPORT-ONLY projection-tap scanout-TEAR surface. cam2's USB grabber
                // captures imag-nb's HDMI output, so this all-cambox sweep already records the
                // physical projection path (imag render -> DRM scanout -> HDMI -> grabber). A
                // captured frame whose cam2-optical dual-QR Vernier payloads span MORE than the
                // by-design even/odd adjacency carried >= 2 paint generations = a scanout tear.
                // Computed from the SAME per-frame payloads + window attribution the strict sweep
                // uses (`frame_gen_ts_anchor` + `place_frame_in_window`) -- no partial schema change
                // (the payloads are already carried) and no on-box work. Pure logic lives in
                // `camera_box::tear_detect` (Tier-0). REPORT-ONLY: `gates_overall_pass()` is `false`.
                // The primary-only signal was PROVEN-BLIND on the single-vertical-band dual-QR
                // content (a horizontal scanout tear corrupts both QR halves at the same height
                // -> undecodable, never two clean generations); issue 1196's v2 therefore folds in
                // the bottom AUX tick pair (AUX_TICK_RUN_ID) and takes the span over the UNION, so
                // a seam BETWEEN the bands yields a clean generation in each. The computed
                // `viability` distinguishes "no tears" from "signal blind", and the new
                // aux_decode_fraction / primary_dark_aux_alive_fraction fields gate the future
                // promotion honestly. NEVER gates and can NEVER newly fail a passing verdict.
                {
                    // issue 1196 (v2): per frame, the PRIMARY dual-QR ids (non-reserved run_ids —
                    // the aux run_id sits IN NODE_BURN_RUN_IDS, so this filter excludes it
                    // automatically) plus the AUX bottom tick pair's ids, extracted BY the
                    // reserved AUX_TICK_RUN_ID. The tear span is computed over their union, which
                    // is what makes a seam BETWEEN the two painted bands detectable.
                    let mut tear_by_window: Vec<Vec<(Vec<u32>, Vec<u32>)>> =
                        vec![Vec::new(); schedule.len()];
                    for f in stream_frames {
                        if let Some(gen_ts) =
                            frame_gen_ts_anchor(f, &anchor_run_ids, &all_burns, cam2_pin)
                        {
                            if let WindowPlacement::In(wi) =
                                place_frame_in_window(gen_ts, schedule, args.switch_guard_ns)
                            {
                                let primary: Vec<u32> = f
                                    .payloads
                                    .iter()
                                    .filter(|p| {
                                        !camera_box::probe::recording::NODE_BURN_RUN_IDS
                                            .contains(&p.run_id)
                                    })
                                    .map(|p| p.frame_id)
                                    .collect();
                                let aux: Vec<u32> = f
                                    .payloads
                                    .iter()
                                    .filter(|p| {
                                        p.run_id
                                            == camera_box::probe::recording_latency::AUX_TICK_RUN_ID
                                    })
                                    .map(|p| p.frame_id)
                                    .collect();
                                tear_by_window[wi].push((primary, aux));
                            }
                        }
                    }
                    let tear_stats: Vec<camera_box::tear_detect::TearStats> = tear_by_window
                        .iter()
                        .map(|w| camera_box::tear_detect::window_tear_stats(w))
                        .collect();
                    let windows_json: Vec<serde_json::Value> = schedule
                        .iter()
                        .zip(&tear_stats)
                        .map(|(w, stats)| {
                            let mut v =
                                serde_json::to_value(stats).unwrap_or(serde_json::Value::Null);
                            if let Some(obj) = v.as_object_mut() {
                                obj.insert(
                                    "cambox".to_string(),
                                    serde_json::json!(w.cambox.clone()),
                                );
                                obj.insert(
                                    "tear_gate_pass".to_string(),
                                    serde_json::json!(camera_box::tear_detect::tear_gate_pass(
                                        stats
                                    )),
                                );
                            }
                            v
                        })
                        .collect();
                    let total_tears: u32 = tear_stats.iter().map(|s| s.tear_frames).sum();
                    let any_observed = tear_stats.iter().any(|s| {
                        s.viability == camera_box::tear_detect::TearSignalViability::Observed
                    });
                    // issue 1196: run-level aux coverage (frame-weighted mean of the per-window
                    // aux_decode_fraction) — the "did the small aux marks survive the chain?"
                    // one-liner; 0.000 on pre-aux recordings.
                    let tear_total_frames: u32 = tear_stats.iter().map(|s| s.total_frames).sum();
                    let aux_coverage = if tear_total_frames > 0 {
                        tear_stats
                            .iter()
                            .map(|s| s.aux_decode_fraction * s.total_frames as f64)
                            .sum::<f64>()
                            / tear_total_frames as f64
                    } else {
                        0.0
                    };
                    println!(
                        "  #781 projection-tap tear surface (REPORT-ONLY): {} torn frame(s) across \
                         {} window(s); signal viability {}; aux tick-pair coverage {:.3}",
                        total_tears,
                        schedule.len(),
                        if any_observed {
                            "OBSERVED"
                        } else {
                            "UNPROVEN (no union-span tear seen on this content)"
                        },
                        aux_coverage
                    );
                    report["all_cambox_continuity"]["tear"] = serde_json::json!({
                        "gates_overall_pass": camera_box::tear_detect::gates_overall_pass(),
                        "vernier_max_spread": camera_box::tear_detect::VERNIER_MAX_SPREAD,
                        "tear_gate": "report-only -- the tear span is the UNION of the primary \
                            dual-QR ids and the issue-1196 bottom aux tick pair's ids (span > \
                            vernier_max_spread = >= 2 paint generations captured); the aux pair \
                            gives the vertical redundancy the single-band primary content lacks \
                            (a seam through the primary band alone reads undecodable, never two \
                            clean generations -- see issue 781). Ships report-only with a computed \
                            signal_viability plus aux_decode_fraction (aux chain-survival \
                            coverage) and primary_dark_aux_alive_fraction (band-localized \
                            corruption discriminator); flip gates_overall_pass to true only once \
                            the signal is Observed on a known-torn run + a bound AND an \
                            aux-coverage floor are calibrated from the mined real-frame fixture.",
                        "windows": windows_json,
                    });
                    // Report-only fold (no-op while gates_overall_pass()==false): one-line LIVE flip.
                    all_pass &= camera_box::tear_detect::run_tear_gate_pass(&tear_stats)
                        || !camera_box::tear_detect::gates_overall_pass();
                }

                // #859 — REPORT-ONLY painter-pacing attribution. From the cam2 painter's own
                // `tick,gen_ts_ns,flip_ts_ns` ground truth (already supplied via `--painter`),
                // decide whether the residual DUPLICATE (`copies`) is the painter's OWN stall (a
                // missed DRM-vsync deadline / a repeated painted tick) or DOWNSTREAM of the
                // page-flip (monitor/camera/splitter optical beat, or the strih/stream genlock
                // FIFO limit cycle). The pure logic lives in `camera_box::painter_pacing` (Tier-0
                // tested); this is the thin probe-side surface. NEVER gates, changes NO threshold,
                // and can NEVER newly fail a passing verdict — a read error emits an `unavailable`
                // block, and absent `--painter` emits nothing at all.
                if let Some(painter_path) = &args.painter {
                    match std::fs::read_to_string(painter_path) {
                        Ok(text) => {
                            let pacing = camera_box::painter_pacing::analyze_csv(&text);
                            let total_copies: u32 = seg.segments.iter().map(|s| s.copies).sum();
                            let mut pv =
                                serde_json::to_value(&pacing).unwrap_or(serde_json::Value::Null);
                            if let Some(obj) = pv.as_object_mut() {
                                obj.insert(
                                    "total_copies".to_string(),
                                    serde_json::json!(total_copies),
                                );
                                obj.insert(
                                    "attribution".to_string(),
                                    serde_json::json!(pacing.duplicate_attribution(total_copies)),
                                );
                            }
                            report["all_cambox_continuity"]["painter_pacing"] = pv;
                        }
                        Err(e) => {
                            report["all_cambox_continuity"]["painter_pacing"] = serde_json::json!({
                                "unavailable": true,
                                "reason": format!("read painter CSV {}: {e}", painter_path.display()),
                            });
                        }
                    }
                }

                // #1036 — the calibrated paired-JUDDER gate (issue 406 zero-loss; the "15fps-like"
                // cadence class issue 726 measures but that never gated). Bound the WORST
                // per-window `paired_fraction` across every cadence-bearing cambox window (a single
                // per-window RATE — the pathology saturates every affected window, so no run-wide
                // second term is needed, unlike the count-based optical floor). `None` worst = no
                // cadence window at all (mass optical-decode failure, already hard-failed by
                // copies/gaps/undecodable) = not applicable, passes. Whether it folds into
                // `overall_pass` is the one-line-restorable `gates_overall_pass()` seam (LIVE).
                let worst_cadence_paired_fraction: Option<f64> = seg
                    .segments
                    .iter()
                    .filter_map(|s| s.presentation_cadence.as_ref().map(|pc| pc.paired_fraction))
                    .fold(None::<f64>, |acc, pf| Some(acc.map_or(pf, |m| m.max(pf))));
                let cadence_bound = args.max_cadence_paired_fraction;
                let cadence_gate_pass = camera_box::presentation_cadence::cadence_judder_gate_pass(
                    worst_cadence_paired_fraction,
                    Some(cadence_bound),
                );
                let cadence_gates_overall = camera_box::presentation_cadence::gates_overall_pass();
                report["all_cambox_continuity"]["cadence_judder_gate"] = serde_json::json!({
                    "bound_paired_fraction": cadence_bound,
                    "worst_paired_fraction": worst_cadence_paired_fraction,
                    "pass": cadence_gate_pass,
                    "gates_overall_pass": cadence_gates_overall,
                    "note": "#1036 calibrated 15fps-judder bound (issue 726 metric, issue 406 \
                             zero-loss). Worst per-window presentation_cadence.paired_fraction \
                             across cambox windows; None = no cadence window (not applicable, \
                             passes). Relax/tighten via --max-cadence-paired-fraction; report-only \
                             via presentation_cadence::gates_overall_pass.",
                });
                println!(
 "  #1036 CADENCE-JUDDER gate: worst paired_fraction={} (bound {}, pass={}, gates_overall_pass={})",
                    worst_cadence_paired_fraction
                        .map(|p| format!("{p:.5}"))
                        .unwrap_or_else(|| "n/a".to_string()),
                    cadence_bound,
                    cadence_gate_pass,
                    cadence_gates_overall,
                );
                // Fold: a FAIL only fails the run while the seam gates overall_pass (LIVE today).
                all_pass &= cadence_gate_pass || !cadence_gates_overall;

                // #1142 — the NEW cadence-UNIFORMITY floor gate (owner mandate 2026-08-19): a broad
                // companion to the paired-judder gate above. Bound the WORST (minimum) per-window
                // `derived_uniform_fraction` (the self-consistent mode-based field, #726 fix — NOT
                // the raw `uniform_fraction`, which false-reds a clean off-expected-step window; on
                // the real rig the two are equal) across every cadence-bearing cambox window — a
                // smooth 60→30 downsample reads ~1.0; the 60→30 + FIFO limit-cycle churn drops it to
                // ~0.67-0.78 on today's rig (issue 1130). A per-window RATE (like the judder gate) so
                // a single per-window-MIN term is honest (no run-wide second term). `None` worst = no
                // cadence window (mass decode failure, already hard-failed by copies/gaps/undecodable)
                // = not applicable, passes. LIVE via `presentation_cadence::uniformity_gates_overall_pass`
                // — the 0.95 floor REDs the current sick rig BY DESIGN.
                let worst_cadence_uniform_fraction: Option<f64> = seg
                    .segments
                    .iter()
                    .filter_map(|s| {
                        s.presentation_cadence
                            .as_ref()
                            .map(|pc| pc.uniform_fraction)
                    })
                    .fold(None::<f64>, |acc, uf| Some(acc.map_or(uf, |m| m.min(uf))));
                // Diagnostic-only: the self-consistent (mode-derived) reading, surfaced so a future
                // switch away from the raw field (if it ever false-reds a clean-but-jittery run) is
                // a one-field change. NOT gated — see src/presentation_cadence.rs UNIFORM_FRACTION_MIN.
                let worst_cadence_derived_uniform_fraction: Option<f64> = seg
                    .segments
                    .iter()
                    .filter_map(|s| {
                        s.presentation_cadence
                            .as_ref()
                            .map(|pc| pc.derived_uniform_fraction)
                    })
                    .fold(None::<f64>, |acc, uf| Some(acc.map_or(uf, |m| m.min(uf))));
                let uniformity_floor = camera_box::presentation_cadence::UNIFORM_FRACTION_MIN;
                let uniformity_gate_pass =
                    camera_box::presentation_cadence::cadence_uniformity_gate_pass(
                        worst_cadence_derived_uniform_fraction,
                        Some(uniformity_floor),
                    );
                let uniformity_gates_overall =
                    camera_box::presentation_cadence::uniformity_gates_overall_pass();
                report["all_cambox_continuity"]["cadence_uniformity_gate"] = serde_json::json!({
                    "min_uniform_fraction": uniformity_floor,
                    "worst_uniform_fraction": worst_cadence_derived_uniform_fraction,
                    "worst_raw_uniform_fraction": worst_cadence_uniform_fraction,
                    "pass": uniformity_gate_pass,
                    "gates_overall_pass": uniformity_gates_overall,
                    "note": "#1142 cadence-uniformity FLOOR (owner mandate). Worst per-window \
                             presentation_cadence.derived_uniform_fraction (the self-consistent \
                             mode-based field, #726 fix) across cambox windows must be >= \
                             min_uniform_fraction (0.95); a smooth 60->30 chain reads ~1.0, the \
                             current rig ~0.67-0.78 (issue 1130 60->30 + FIFO churn) so this REDs \
                             the sick rig by design. worst_raw_uniform_fraction is the raw reading, \
                             DIAGNOSTIC only (it false-reds a clean off-expected-step window; \
                             derived does not). None = no cadence window (not applicable, passes). \
                             LIVE via presentation_cadence::uniformity_gates_overall_pass.",
                });
                println!(
 "  #1142 CADENCE-UNIFORMITY gate: worst derived_uniform_fraction={} (raw={}, floor {}, pass={}, gates_overall_pass={})",
                    worst_cadence_derived_uniform_fraction
                        .map(|p| format!("{p:.5}"))
                        .unwrap_or_else(|| "n/a".to_string()),
                    worst_cadence_uniform_fraction
                        .map(|p| format!("{p:.5}"))
                        .unwrap_or_else(|| "n/a".to_string()),
                    uniformity_floor,
                    uniformity_gate_pass,
                    uniformity_gates_overall,
                );
                // Fold: a FAIL only fails the run while the seam gates overall_pass (LIVE today).
                all_pass &= uniformity_gate_pass || !uniformity_gates_overall;

                // #1088/#1112 — REPORT-ONLY duplication-masked 50→60 dup-rate seam (the #794 hard
                // layer). The cadence watchdog (#794) reads strih's genlock-fifo `received=` rate
                // and is STRUCTURALLY BLIND to a grabber that upconverts a 50fps source to 60 by
                // frame DUPLICATION: it delivers a padded genuine 60 NDI frames/s, so `received=`
                // reads a clean 60. The surviving signal is per-frame CONTENT identity — a
                // row-sampled content hash per recorded frame, sliced into the SAME cambox windows
                // the sweep above uses, fed into the pure `camera_box::dup_cadence` classifier.
                //
                // #1112/#1166 — the near-duplicate signal needs the recording PIXELS, which in the
                // production `VERDICT_ON_STREAM=1` merge are NOT on dev1. So the STREAM box computes
                // the per-frame MAD-to-predecessor during `--extract-partial stream` (recording
                // local) and CARRIES it in the partial (`stream_frame_prev_diffs`); the fused
                // `--stream` path still has the recording here and recomputes it from `stream_rec`.
                // Either way the SAME windowing + classifier runs — this closes the #1101 finding
                // that the surface was structurally unreachable in the merge gate (0/81 verdicts
                // carried it). Report-only / calibration-first: `gates_overall_pass()` is false (no
                // calibrated bound yet — #1166 owns the LIVE flip), so the fold below is a no-op.
                // Each skip REASON is logged HERE during source resolution (accurately — a diff
                // FAILURE prints its own {e}, a genuine no-source prints the no-carry/no-recording
                // line), so the consuming match's None arm is a no-op and never double-logs a
                // contradictory second line (issue-1112 review). Matched by VALUE, so the carried
                // vector is MOVED, not cloned (`stream_frame_prev_diffs` is not read again).
                let dup_diff_source: Option<Vec<Option<f64>>> = match stream_frame_prev_diffs {
                    // Merge path (production gate): the stream box already diffed its LOCAL recording
                    // during extract and carried the vector — the ONLY way the pixel-derived signal
                    // reaches the dev1 merge, which has no recording.
                    Some(carried) => Some(carried),
                    // Fused path (legacy `--stream`): the recording is on this host, recompute it
                    // exactly as before #1112. A diff failure is NON-FATAL for a report-only surface.
                    None => match stream_rec.as_deref() {
                        Some(rec_path) => {
                            match camera_box::probe::recording::frame_prev_diffs(rec_path) {
                                Ok(d) => Some(d),
                                Err(e) => {
                                    println!(
                                        "  #1088 DUP-CADENCE: skipped — could not diff stream recording: {e}"
                                    );
                                    None
                                }
                            }
                        }
                        // Genuine no-source: no carry from the box AND no local recording (the
                        // routine pre-#1112 merge case). Accurate reason, unlike a diff failure.
                        None => {
                            println!(
                                "  #1088 DUP-CADENCE: skipped — no stream frame diffs (no carry \
                                 from the stream box, no local recording to diff)"
                            );
                            None
                        }
                    },
                };
                // The skip reason for a None dup_diff_source was already logged accurately during
                // source resolution above, so the None arm needs no body — if let, per clippy.
                if let Some(frame_prev_mads) = dup_diff_source {
                    let (dup_windows, dup_no_anchor) = partition_frames_by_window(
                        stream_frames,
                        &anchor_run_ids,
                        &all_burns,
                        cam2_pin,
                        schedule,
                        args.switch_guard_ns,
                    );
                    let mut dcs: Vec<Option<camera_box::dup_cadence::DupCadence>> =
                        Vec::with_capacity(dup_windows.len());
                    // #1101 — signal-viability cross-check, built in the SAME pass (no second
                    // iteration over dup_windows). Each window's tick-copies (STRICT-adjacent tick
                    // repeats — a subset of the canonical `copies`, which additionally bridges an
                    // undecodable gap) are cross-checked against the content near-duplicates over the
                    // SAME consecutive-frame basis, so an all-zero duplicate_fraction can't be
                    // mistaken for a promotable green. #1166 replaced the byte-exact content hash
                    // (blind on the lossy tap) with the codec-tolerant near-duplicate MAD signal, so
                    // this cross-check now reads Viable on a healthy run instead of Blind.
                    let mut copy_obs: Vec<camera_box::dup_cadence::CopyObservation> =
                        Vec::with_capacity(dup_windows.len());
                    let mut worst_raw_fraction: Option<f64> = None;
                    let mut masked_windows: usize = 0;
                    for win_frames in &dup_windows {
                        // #1112/#1166 — slice the (carried or locally-recomputed) per-frame
                        // MAD-to-predecessor vector into THIS window's near-duplicate sequence, by
                        // frame_index + recording-adjacency (the pure Tier-0 helper — index-alignment
                        // and the window-boundary gating are the fragile parts and are unit-tested
                        // there). The SAME `seq` feeds BOTH the classifier and the #1101 viability
                        // cross-check below, so their near-dup positions match exactly (resolving the
                        // #1101 review's content/duplicate sequence-mismatch).
                        let win_idxs: Vec<u64> = win_frames.iter().map(|f| f.frame_index).collect();
                        let seq =
                            camera_box::dup_cadence::window_prev_mads(&win_idxs, &frame_prev_mads);
                        let dc = camera_box::dup_cadence::measure_dup_cadence(&seq);
                        if let Some(ref d) = dc {
                            worst_raw_fraction = Some(
                                worst_raw_fraction
                                    .map_or(d.duplicate_fraction, |m| m.max(d.duplicate_fraction)),
                            );
                            if d.duplication_masked {
                                masked_windows += 1;
                            }
                        }
                        dcs.push(dc);
                        // #1101 — parallel per-frame (tick, near-dup MAD) slices for THIS window: tick
                        // from RecordingFrame::tick, the near-dup MADs from the SAME `seq` above (so
                        // its near-dup positions are identical to the classifier's). A None tick/MAD
                        // never forms a copy/dup (a decode gap must not manufacture a false duplicate).
                        let win_ticks: Vec<Option<u64>> =
                            win_frames.iter().map(|f| f.tick.map(u64::from)).collect();
                        copy_obs.push(camera_box::dup_cadence::copy_observation(&win_ticks, &seq));
                    }
                    // The GATE keys on the DISCRIMINATED signal (worst fraction among
                    // windows classified `duplication_masked`), never the raw worst —
                    // a freeze/glitch has a high raw fraction but is coverage/regularity
                    // vetoed (frozen_leg's domain), so gating on raw would double-jeopardy
                    // it (issue 1088 review finding).
                    let worst_masked_fraction =
                        camera_box::dup_cadence::worst_masked_duplicate_fraction(&dcs);
                    // #1101/#1166 — fold the per-window signal-viability cross-check (built in the loop
                    // above) into the run-level verdict: does the content near-duplicate signal
                    // actually OBSERVE the duplication the Vernier-tick copies prove is present? The
                    // retired byte-exact hash missed nearly every copy on the lossy stream recording
                    // (2/147 measured → Blind); #1166's codec-tolerant MAD-to-predecessor signal
                    // observes ~81% of the tick-proven copies on the retained real lossy frames, so
                    // this now reads Viable. `signal_promotable` still gates the LIVE flip on ≥2
                    // consecutive REAL runs reading viable + a recalibrated bound (#1166).
                    let obs_agg = camera_box::dup_cadence::aggregate_copy_observations(&copy_obs);
                    let signal_viability = camera_box::dup_cadence::signal_viability(&obs_agg);
                    let signal_promotable =
                        camera_box::dup_cadence::signal_promotable(signal_viability);
                    let dup_window_json: Vec<serde_json::Value> = dcs
                        .iter()
                        .enumerate()
                        .map(|(wi, dc)| {
                            serde_json::json!({
                                "cambox": schedule[wi].cambox.clone(),
                                "dup_cadence": dc,
                            })
                        })
                        .collect();
                    let dup_bound = camera_box::dup_cadence::DUP_RATE_PULLDOWN_MIN;
                    let dup_gate_pass = camera_box::dup_cadence::dup_cadence_gate_pass(
                        worst_masked_fraction,
                        Some(dup_bound),
                    );
                    let dup_gates_overall = camera_box::dup_cadence::gates_overall_pass();
                    report["all_cambox_continuity"]["duplication_masked_cadence"] = serde_json::json!({
                        "windows": dup_window_json,
                        "masked_windows": masked_windows,
                        "worst_masked_duplicate_fraction": worst_masked_fraction,
                        "worst_raw_duplicate_fraction": worst_raw_fraction,
                        "bound_duplicate_fraction": dup_bound,
                        "pass": dup_gate_pass,
                        "gates_overall_pass": dup_gates_overall,
                        "frames_no_anchor": dup_no_anchor,
                        "signal_viability": signal_viability,
                        "signal_promotable": signal_promotable,
                        "tick_proven_copies": obs_agg.tick_copies,
                        "copies_observed_by_content": obs_agg.copies_observed_by_content,
                        "content_near_dup_pairs": obs_agg.content_near_dup_pairs,
                        "copy_observation_rate": obs_agg.copy_observation_rate,
                        "note": "#1088 duplication-masked 50->60 detector: \
                                 per-cambox-window codec-tolerant near-duplicate rate \
                                 (row-sampled mean-abs-luma-diff to the recording \
                                 predecessor <= NEAR_DUP_MAD_MAX; #1166 replaced the \
                                 byte-exact content hash that was blind on the lossy \
                                 .mp4). The #794 hard layer the received= rate tap is \
                                 blind to. Per-window duplication_masked flags a \
                                 sustained + regular + window-spanning pulldown. The \
                                 GATE keys on worst_masked_duplicate_fraction (worst raw \
                                 fraction among MASKED windows), NOT \
                                 worst_raw_duplicate_fraction (a freeze/glitch has a \
                                 high raw fraction but is coverage/regularity vetoed → \
                                 excluded, no double-jeopardy with frozen_leg). \
                                 REPORT-ONLY / calibration-first via \
                                 dup_cadence::gates_overall_pass (false). #1101 \
                                 signal_viability cross-checks the content near-duplicates \
                                 against tick_proven_copies (repeated Vernier tick = a \
                                 byte-duplicate frame): blind = copies present but the \
                                 signal observed <50% of them; viable = >=50% observed \
                                 (the #1166 fix); signal_promotable (viable on >=2 real \
                                 runs + a recalibrated bound) is the LIVE-flip precondition.",
                    });
                    println!(
"  #1088 DUP-CADENCE (report-only): masked_windows={} worst_masked={} worst_raw={} (bound {}, pass={}, gates_overall_pass={})",
                                masked_windows,
                                worst_masked_fraction
                                    .map(|p| format!("{p:.5}"))
                                    .unwrap_or_else(|| "n/a".to_string()),
                                worst_raw_fraction
                                    .map(|p| format!("{p:.5}"))
                                    .unwrap_or_else(|| "n/a".to_string()),
                                dup_bound,
                                dup_gate_pass,
                                dup_gates_overall,
                            );
                    println!(
"  #1101 DUP-CADENCE SIGNAL: viability={:?} promotable={} (tick_proven_copies={} observed_by_content={} rate={})",
                                signal_viability,
                                signal_promotable,
                                obs_agg.tick_copies,
                                obs_agg.copies_observed_by_content,
                                obs_agg
                                    .copy_observation_rate
                                    .map(|r| format!("{r:.4}"))
                                    .unwrap_or_else(|| "n/a".to_string()),
                            );
                    // Fold: a FAIL only fails the run while the seam gates overall_pass
                    // (report-only today, so this is a no-op).
                    all_pass &= dup_gate_pass || !dup_gates_overall;
                }

                // #768 — REPORT-ONLY cold-cut onset seam. The all-cambox sweep cuts program to each
                // cambox after it has sat program-hidden ~60s (the active 3-box CAM1/CAM2/CAM3 cycle
                // at 30s segments hides each 2x30s between windows). The per-segment aggregate
                // copies/gaps average a single wake-up gap over the whole ~30s window, and the 1s
                // transition guard discards the onset entirely — so nothing measured the cut
                // TRANSITION, the blind spot that let issue 767 (a genlocked receiver that never
                // rebinds after its sender restarts) survive every gate. This reads the raw decoded
                // frames in the first ONSET_WINDOW_NS after each switch (bypassing the guard via
                // `seg_frames`) and reports, per cold cut (cambox hidden >= COLD_HIDDEN_SECS), the
                // wake-up latency + onset (un)decodable counts. Report-only (calibration-first): the
                // onset was never serialized before, so no bound is calibratable yet —
                // `gates_overall_pass()` is false and this fold is a no-op. The pure logic lives in
                // `camera_box::cold_cut` (Tier-0 tested); this is the thin probe-side consumer.
                let cold_windows: Vec<camera_box::cold_cut::ColdCutWindow> = schedule
                    .iter()
                    .map(|w| {
                        let onset_frames: Vec<camera_box::cold_cut::OnsetFrame> = seg_frames
                            .iter()
                            .filter(|f| {
                                camera_box::cold_cut::in_onset_window(f.gen_ts_ns, w.start_ns)
                            })
                            .map(|f| camera_box::cold_cut::OnsetFrame {
                                gen_ts_ns: f.gen_ts_ns,
                                decodable: f.tick.is_some(),
                            })
                            .collect();
                        // #1086 part-4: total delivered frames across the WHOLE window
                        // [start_ns, end_ns) (not just the onset) for the sustained-receive-fps
                        // health check. Reads seg_frames directly (bypasses the segmenter guard),
                        // same as the onset read above.
                        let window_frames = seg_frames
                            .iter()
                            .filter(|f| f.gen_ts_ns >= w.start_ns && f.gen_ts_ns < w.end_ns)
                            .count() as u32;
                        camera_box::cold_cut::ColdCutWindow {
                            cambox: w.cambox.clone(),
                            start_ns: w.start_ns,
                            end_ns: w.end_ns,
                            onset_frames,
                            window_frames,
                        }
                    })
                    .collect();
                let cold_report = camera_box::cold_cut::build_report(&cold_windows);
                let cold_gate_pass = camera_box::cold_cut::cold_cut_gate_pass(&cold_report);
                let cold_gates_overall = camera_box::cold_cut::gates_overall_pass();
                let mut cold_json =
                    serde_json::to_value(&cold_report).unwrap_or(serde_json::Value::Null);
                if let Some(obj) = cold_json.as_object_mut() {
                    obj.insert("pass".to_string(), serde_json::json!(cold_gate_pass));
                    obj.insert(
                        "gates_overall_pass".to_string(),
                        serde_json::json!(cold_gates_overall),
                    );
                    obj.insert(
                        "gate".to_string(),
                        serde_json::json!(
                            "#768/#1086 report-only -- the cold-cut onset (first 1s after a switch \
                             to a cambox hidden >= 60s) is NOT gated pending a warm baseline + a \
                             deliberate keepalive-bypass cold cut (COLD_CUT_BYPASS_CAM, #1086); \
                             measures wake-up latency + onset undecodable + (phase-2) sustained \
                             receive-fps health + the issue-793 startup-segfault vs genuine \
                             cold-cut-miss attribution so a future run can calibrate a LIVE bound"
                        ),
                    );
                }
                report["all_cambox_continuity"]["cold_cut_onset"] = cold_json;
                println!(
                    "  #768/#1086 COLD-CUT onset: {} cold transition(s) (hidden >= {}s), worst wakeup {}, receive_degraded={}, possible_segfault_miss={}, genuine_cold_cut_miss={} (report-only, gates_overall_pass={})",
                    cold_report.cold_transitions_found,
                    cold_report.cold_hidden_secs,
                    cold_report
                        .worst_wakeup_latency_ns
                        .map(|w| format!("{w} ns"))
                        .unwrap_or_else(|| "n/a".to_string()),
                    cold_report.any_receive_degraded,
                    cold_report.any_miss_possibly_segfault,
                    cold_report.any_genuine_cold_cut_miss,
                    cold_gates_overall,
                );
                // Fold: report-only, so this is a no-op while gates_overall_pass() is false.
                all_pass &= cold_gate_pass || !cold_gates_overall;

                // #758 item 4 — the frozen-leg classifier: distinguishes a SUSTAINED camera
                // freeze (hard-fail) from isolated stale-replay frames (informational-only,
                // never gates) using the SAME per-window copies/frames/duration `seg.segments`
                // already carries. Separate from `seg.overall_pass` above (which ALSO fails on
                // undecodable/gaps, unrelated to freeze/replay) — this is specifically the
                // #758-motivated distinction run 1299588287's own forensics conflated (cam2/3/4/6's
                // 1-3 replayed frames misread as "freezes"; only cam7 was genuinely frozen).
                let leg_segments: Vec<SegmentLeg> = seg
                    .segments
                    .iter()
                    .map(|s| SegmentLeg {
                        cambox: &s.cambox,
                        copies: s.copies,
                        frames: s.frames,
                        start_ns: s.start_ns,
                        end_ns: s.end_ns,
                    })
                    .collect();
                // #895 — correlate the harness's --self-heal-reset events (recording-e2e.sh's
                // [7b/8] mid-recording scan, scripts/lib/self-heal-attribution.sh) against these
                // SAME windows BEFORE emitting frozen_leg, so a capture_rate_selfheal (#663)
                // USB-reset firing during the recording is attributed to self_heal_reset, never
                // misreported as a camera fault. A malformed token is silently dropped — the
                // harness's own scan is the only producer.
                // issue 946 / issue 910: merge the legacy untagged `--self-heal-reset` tokens
                // (default to the self_heal_reset kind) with the kind-tagged `--restart-event`
                // tokens (capture-wedge / emit-freeze too) into ONE event list. `parse` accepts
                // both shapes, so a mixed invocation just works.
                let self_heal_events: Vec<SelfHealResetEvent> = args
                    .self_heal_reset
                    .iter()
                    .chain(args.restart_event.iter())
                    .filter_map(|t| SelfHealResetEvent::parse(t))
                    .collect();
                let leg_report = attribute_self_heal(&leg_segments, &self_heal_events);
                for f in &leg_report.frozen {
                    println!("  {}", f.message());
                }
                for s in &leg_report.stale_replay {
                    println!("  {}", s.message());
                }
                for sh in &leg_report.self_heal {
                    println!("  {}", sh.message());
                }
                for ev in &leg_report.unattributed_events {
                    println!(
                        "  {}: {} at {} (epoch ns) -- no correlating classified window, still counts as a run-integrity event (#895/#946)",
                        ev.kind.label(),
                        ev.cambox,
                        ev.at_ns
                    );
                }
                // #914 (2026-08-01, user decision -- mirrors issue 889's report-only pattern and
                // issue 861's caller-only decoupling): frozen_leg/self_heal_reset no longer gate
                // `overall_pass` while cam1's ShadowCast 2 grabber defect (issue 909) remains
                // physically unresolved -- restore path on issue 905 (flip
                // `SelfHealAttributionReport::overall_pass_contribution` back to
                // `!any_frozen() && !any_self_heal()` once cam1 is physically replaced and a
                // stable week passes with no self-heal escalations). `gates_overall_pass` below
                // mirrors the exact field name/shape `all_cambox_av_sync` already established for
                // issue 861 -- an unambiguous machine-readable flag alongside the still-fully-
                // computed frozen/self-heal findings.
                let frozen_self_heal_gate_note = "report-only -- does NOT gate overall_pass, \
                     pending cam1 hardware fix (see issue #914 for the decision record and issue \
                     #905 for the restore path)";
                report["frozen_leg"] = serde_json::json!({
                    "frozen": leg_report.frozen.iter().map(|f| serde_json::json!({
                        "cambox": f.cambox,
                        "since_ns": f.since_ns,
                        "copies": f.copies,
                        "approx_stale_secs": f.approx_stale_secs,
                        "density": f.density,
                        "message": f.message(),
                    })).collect::<Vec<_>>(),
                    "stale_replay": leg_report.stale_replay.iter().map(|s| serde_json::json!({
                        "cambox": s.cambox,
                        "copies": s.copies,
                        "message": s.message(),
                    })).collect::<Vec<_>>(),
                });
                // The JSON key `self_heal_reset` is kept for back-compat (issue-895/914 consumers +
                // tests key on it); as of issue 946 each entry ALSO carries a `kind`
                // (self_heal_reset | capture_wedge | emit_freeze) so a reader can tell which
                // run-integrity restart explained the window.
                report["self_heal_reset"] = serde_json::json!({
                    "attributed": leg_report.self_heal.iter().map(|sh| serde_json::json!({
                        "kind": sh.kind.label(),
                        "cambox": sh.cambox,
                        "since_ns": sh.since_ns,
                        "reset_at_ns": sh.reset_at_ns,
                        "copies": sh.copies,
                        "approx_stale_secs": sh.approx_stale_secs,
                        "density": sh.density,
                        "message": sh.message(),
                    })).collect::<Vec<_>>(),
                    "unattributed_events": leg_report.unattributed_events.iter().map(|ev| serde_json::json!({
                        "kind": ev.kind.label(),
                        "cambox": ev.cambox,
                        "at_ns": ev.at_ns,
                    })).collect::<Vec<_>>(),
                });
                if let Some(obj) = report["frozen_leg"].as_object_mut() {
                    obj.insert("gates_overall_pass".to_string(), serde_json::json!(false));
                    obj.insert(
                        "gate".to_string(),
                        serde_json::json!(frozen_self_heal_gate_note),
                    );
                }
                if let Some(obj) = report["self_heal_reset"].as_object_mut() {
                    obj.insert("gates_overall_pass".to_string(), serde_json::json!(false));
                    obj.insert(
                        "gate".to_string(),
                        serde_json::json!(frozen_self_heal_gate_note),
                    );
                }
                // #914 visibility requirement (mirrors issue 889 requirement 3): prints
                // UNCONDITIONALLY, whether or not anything fired, so silence is never mistaken
                // for strictness.
                println!(
                    "  >>> #914 REPORT-ONLY: frozen_leg={} self_heal_reset={} \
                     unattributed_events={} -- {frozen_self_heal_gate_note}",
                    leg_report.frozen.len(),
                    leg_report.self_heal.len(),
                    leg_report.unattributed_events.len()
                );
                all_pass &= leg_report.overall_pass_contribution();

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
                        // issue 798 (path A) -> #1142 — this per-segment imag continuity is a
                        // PER-FRAME CONTENT term (the same class as the whole-recording node's
                        // burn/beat), so it folds through the REPORT-ONLY CONTENT seam
                        // (content_gates_overall_pass). Issue 1130 proved the imag per-frame
                        // repetition is an x264 record-load OBSERVER EFFECT, so this continuity is
                        // confounded during the recording and must NOT red a run yet (pending the
                        // issue 1143 encoder fix). Scoped flag names the imag TERM only — the
                        // per-cambox (stream) sweep's own `overall_pass` fold is UNTOUCHED and
                        // stays blocking.
                        "gates_overall_pass": camera_box::imag_leg_gate::content_gates_overall_pass(),
                        "report_only_note": "issue 798 path A -> #1142: imag per-segment continuity \
                                             is a PER-FRAME CONTENT term, surfaced but REPORT-ONLY \
                                             (does not gate overall_pass) pending the issue 1143 \
                                             imag encoder fix (issue 1130 x264 record-load observer \
                                             effect). Presence/verification is separately BLOCKING.",
                    });
                    // issue 798 -> #1142 — REPORT-ONLY per-frame CONTENT fold: a no-op while
                    // `imag_leg_gate::content_gates_overall_pass()` is `false`. The per-segment imag
                    // continuity is confounded by the issue 1130 observer effect, so it stays
                    // report-only pending the issue 1143 encoder fix. The imag PRESENCE/VERIFICATION
                    // terms (whole-recording node) are separately BLOCKING via gates_overall_pass.
                    all_pass &= camera_box::imag_leg_gate::content_folds_into_overall_pass(
                        imag_overall_pass,
                    );
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
                    args.burn_cam7_run_id,
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
                        "cam7" => args.burn_cam7_run_id,
                        _ => unreachable!(
                            "OPTICAL_INJECTION_NODES is exactly cam1/cam3/cam4/cam5/cam6/cam7"
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
                // #1033 -> #1142: `spread_gate_pass` FOLDS into `all_pass` through the
                // `delivery_spread_gate` seam, now BLOCKING (`gates_overall_pass()==true`, owner
                // mandate 2026-08-19) — a wide delivery spread REDs the run at the shared
                // SPREAD_THRESHOLD_MS bound (the SOURCE side already blocked). The "green" runs it
                // used to pass were falsely green (the phase lottery hid a real spread failure).
                // Its purpose today is still to
                // let a re-verification run SEE whether the applied differentiated offsets
                // collapsed the delivery-time spread — now via the standard flip-ready seam.
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
                        args.burn_cam7_run_id,
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
                                 spread={:.2}ms (threshold {:.1}ms) → {} ({}, see #286/#1033)",
                                sv.max_p50_ms,
                                sv.min_p50_ms,
                                sv.spread_ms,
                                camera_box::switch_latency::SPREAD_THRESHOLD_MS,
                                if sv.pass { "PASS" } else { "FAIL" },
                                if camera_box::delivery_spread_gate::gates_overall_pass() {
                                    "LIVE — folds into all_pass"
                                } else {
                                    "report-only — does NOT gate all_pass"
                                }
                            );
                            delivery_json.insert(
                                "cross_camera_spread_ms".to_string(),
                                serde_json::json!(sv.spread_ms),
                            );
                            delivery_json
                                .insert("spread_gate_pass".to_string(), serde_json::json!(sv.pass));
                            // #1033 -> #1142 — fold the delivery cross-camera spread into
                            // `overall_pass` through the seam, now BLOCKING (owner mandate
                            // 2026-08-19): a wide spread (> SPREAD_THRESHOLD_MS) REDs the run. The
                            // "green" runs this used to pass were FALSELY green (the phase lottery,
                            // 3.97 vs 85 ms, hid a real delivery-spread failure); #1142 stops that.
                            // The SOURCE-side spread already blocks at the same bound; this makes the
                            // DELIVERY side block too. Mirrors the source-side sweep's own fold.
                            delivery_json.insert(
                                "gates_overall_pass".to_string(),
                                serde_json::json!(
                                    camera_box::delivery_spread_gate::gates_overall_pass()
                                ),
                            );
                            all_pass &=
                                camera_box::delivery_spread_gate::folds_into_overall_pass(sv.pass);
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
                // cameras) without gating `all_pass`. PR B (this) wires the tolerance-bounded cross-window
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
                    // #855: operator-acknowledged offline boxes (CAMBOX_OFFLINE_ACK / rig-fleet.txt,
                    // carried here via --offline-ack-cams) are EXCLUDED from this gate below,
                    // never judged UNKNOWN/FAIL on samples they were never going to produce. A box
                    // NOT in this map keeps the existing fail-closed default (#836).
                    let offline_ack_map = offline_ack::parse(&args.offline_ack_cams);
                    let mut av_json = serde_json::Map::new();
                    // #624 deliverable 4 / #312 item 2 PR B: every camera under test must PASS
                    // the A/V-offset gate (±AV_OFFSET_GATE_TOLERANCE_MS) for the run's overall
                    // verdict to pass — folded into `all_pass` below, alongside
                    // all_cambox_continuity + all_cambox_latency.
                    let mut av_all_pass = true;
                    // #1178: per-camera residuals (measured/effective − expected calibrated
                    // video-leg) collected for the report-only cross-camera residual summary.
                    let mut av_residuals: Vec<f64> = Vec::new();
                    // #855/#861 fail-closed floor: how many cameras were actually JUDGED (not
                    // ack-excluded). An ack list covering EVERY camera would otherwise leave the
                    // AND-fold vacuously true — the one lever that could silently disable the
                    // re-armed BLOCKING gate with zero measurements behind it.
                    let mut av_judged_cameras: usize = 0;
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
                        // #855: an operator-acknowledged offline box is reported EXCLUDED here,
                        // never judged — it never gets a chance to fail the gate on zero samples
                        // that were never going to exist. Skips derivation/gate-pass entirely and
                        // is NEVER folded into `av_all_pass` (below), so it can neither pass nor
                        // fail the gate, only be visibly skipped. A box NOT in offline_ack_map
                        // falls through to the unchanged fail-closed path below.
                        if let Some(reason) = offline_ack_map.get(camera) {
                            println!(
                                "  {camera}: EXCLUDED — operator-acknowledged offline ({reason}) \
                                 [gate SKIPPED, #855]"
                            );
                            av_json.insert(
                                camera.to_string(),
                                serde_json::json!({
                                    "node": camera,
                                    "excluded": true,
                                    "exclude_reason": reason,
                                    "verdict": "excluded",
                                    "gate_pass": serde_json::Value::Null,
                                    "windows": cam_sync.windows,
                                    "candidates": cam_sync.candidates,
                                    "cluster_samples": cam_sync.cluster_samples,
                                    "av_offset_ms": serde_json::Value::Null,
                                    "mad_ms": serde_json::Value::Null,
                                    "effective_offset_ms": serde_json::Value::Null,
                                }),
                            );
                            continue;
                        }
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
                        av_judged_cameras += 1;
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
                        // #1178 (review finding): compute the effective offset ONCE so the JSON
                        // `effective_offset_ms` and the residual below can never diverge.
                        let cam_effective_offset =
                            av_window::effective_offset_ms(cam_sync, derived.as_ref());
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
                            // #714/#689 — the ONE computable per-camera A/V offset the gate/report
                            // read (measured value for cam2, sound derived value for a starved
                            // camera, null only for a genuine unknown) — so the raw verdict is
                            // never a bare `av_offset_ms=null` for a camera we DO have a number for
                            // (the "silent cam2-only" the #714 one-full-test mandate forbids). The
                            // `verdict` label above still says which kind of value this is.
                            "effective_offset_ms": cam_effective_offset,
                        });
                        // #1178 report-only: this camera's RESIDUAL A/V offset — its
                        // measured/effective offset with the expected calibrated video-leg removed
                        // (~0 for an aligned camera). Collected for the cross-camera summary below.
                        if let Some(eff) = cam_effective_offset {
                            let residual = av_window::residual_offset_ms(eff, args.av_expected_ms);
                            cam_json["residual_offset_ms"] = serde_json::json!(residual);
                            av_residuals.push(residual);
                        }
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
                    // #855/#861 fail-closed floor: zero judged cameras (every camera in the
                    // sweep ack-excluded) is an UNMEASURED gate, never a pass — the ack list
                    // must not be able to silently disable the re-armed BLOCKING gate.
                    if av_judged_cameras == 0 {
                        av_all_pass = false;
                        println!(
                            "  >>> #624 A/V-offset gate: ZERO cameras judged (every camera in \
                             the sweep is operator-ack-excluded) — failing closed; an ack list \
                             must never silently disable the re-armed gate (#855/#861)"
                        );
                    }
                    av_json.insert(
                        "judged_cameras".to_string(),
                        serde_json::json!(av_judged_cameras),
                    );
                    // #861 (2026-08-06, re-armed after ASRC #803 proved stable — see
                    // av_window::gates_overall_pass()'s own doc comment for the decision record):
                    // was TEMPORARILY report-only since 2026-07-29 (user decision on #856) while
                    // program audio drifted ~160ms/hour against video (epic #800, foreign clock
                    // domain) — a constant video-delay offset could not hold ±20ms until
                    // per-source ASRC landed. That precondition is now met, so this term folds
                    // into `all_pass` again, mirroring the issue-914/915 `gates_overall_pass()`
                    // seam exactly (applied in reverse: re-blocking, not relaxing).
                    // #1178: whether expected_ms is the calibrated fixed video-leg default or an
                    // explicit override (MEASUREMENT_EQ / issue 1003, or an operator-dialed value).
                    let av_expected_is_calibrated_default =
                        (args.av_expected_ms - av_window::RIG_VIDEO_LEG_OFFSET_MS).abs() < 1e-9;
                    let av_gate_blocking = av_window::gates_overall_pass();
                    println!(
                        "  >>> #624 deliverable 4 A/V-offset gate: expected={:.1}ms tolerance=±{:.1}ms → {} \
                         ({})",
                        args.av_expected_ms,
                        av_window::AV_OFFSET_GATE_TOLERANCE_MS,
                        if av_all_pass { "PASS" } else { "FAIL" },
                        if av_gate_blocking {
                            "BLOCKING — gates overall_pass again, re-armed after ASRC (#803), see #861"
                        } else {
                            "report-only — does NOT gate overall_pass, pending ASRC, see #861"
                        }
                    );
                    println!(
                        "  >>> #1178 rig_video_leg_offset_ms={:.1}ms (calibrated fixed video-leg: monitor lag + sensor→HDMI + grabber); expected_ms={:.1}ms {}",
                        av_window::RIG_VIDEO_LEG_OFFSET_MS,
                        args.av_expected_ms,
                        if av_expected_is_calibrated_default {
                            "= calibrated video-leg default (subtracted before the ±tolerance band)"
                        } else {
                            "= explicit override (physical compensation / operator-dialed; calibration replaced)"
                        }
                    );
                    av_json.insert(
                        "expected_ms".to_string(),
                        serde_json::json!(args.av_expected_ms),
                    );
                    av_json.insert(
                        "gate_tolerance_ms".to_string(),
                        serde_json::json!(av_window::AV_OFFSET_GATE_TOLERANCE_MS),
                    );
                    // #1178: the NAMED, surfaced fixed video-leg calibration (never a silent
                    // shift) + whether the current expected_ms is that calibrated default or an
                    // explicit override (e.g. MEASUREMENT_EQ / issue 1003, or an operator-dialed
                    // value).
                    av_json.insert(
                        "rig_video_leg_offset_ms".to_string(),
                        serde_json::json!(av_window::RIG_VIDEO_LEG_OFFSET_MS),
                    );
                    av_json.insert(
                        "expected_ms_is_calibrated_default".to_string(),
                        serde_json::json!(av_expected_is_calibrated_default),
                    );
                    // #1178 report-only: cross-camera residual (measured − expected) median +
                    // spread — surfaces whatever cross-run instability REMAINS after the fixed
                    // video-leg is removed (issue 952 / issue 1004) WITHOUT masking a global drift
                    // (the BLOCKING gate uses the fixed constant, never this per-run median).
                    let av_residual_summary = av_window::residual_summary(&av_residuals);
                    av_json.insert(
                        "residual_median_ms".to_string(),
                        serde_json::json!(av_residual_summary.median_ms),
                    );
                    av_json.insert(
                        "residual_spread_ms".to_string(),
                        serde_json::json!(av_residual_summary.spread_ms),
                    );
                    av_json.insert("gate_pass".to_string(), serde_json::json!(av_all_pass));
                    // #861: unambiguous machine-readable flag alongside `gate_pass` — whether this
                    // term's measured PASS/FAIL COUNTS toward `overall_pass` (see `gate_pass`
                    // above for the measured value itself).
                    av_json.insert(
                        "gates_overall_pass".to_string(),
                        serde_json::json!(av_gate_blocking),
                    );
                    av_json.insert(
                        "gate".to_string(),
                        serde_json::json!(if av_gate_blocking {
                            format!(
                                "BLOCKING — every camera under test must be within ±{:.0}ms of \
                                 expected_ms (#624 deliverable 4 / #312 item 2 PR B); re-armed \
                                 after ASRC (#803) proved stable, see #861",
                                av_window::AV_OFFSET_GATE_TOLERANCE_MS
                            )
                        } else {
                            format!(
                                "report-only — does NOT gate overall_pass, pending ASRC (#861); \
                                 every camera under test is still measured against ±{:.0}ms of \
                                 expected_ms (#624 deliverable 4 / #312 item 2 PR B)",
                                av_window::AV_OFFSET_GATE_TOLERANCE_MS
                            )
                        }),
                    );
                    // #748: silent-vs-undecoded discriminator. When EVERY judged (not
                    // operator-ack-excluded) camera produced zero candidates, `candidates == 0`
                    // alone cannot say WHY — a genuinely silent mbc chain (mute / Dante misroute)
                    // vs audio present but the QPSK marker never clustered. The whole-recording
                    // preamble-onset count (`av.audio_preamble_screens_passed`, measured from the
                    // ACTUAL recorded audio) separates them via the pure `classify_av_audio_state`.
                    // `_section_av_sync` reads `av_audio_silent` to blame the right link (null/absent
                    // keeps the safe, loud "check the mbc mute" default).
                    let mut av_all_zero = true;
                    let mut av_judged = 0usize;
                    for &camera in CAMERA_UNDER_TEST_NODES.iter() {
                        if offline_ack_map.contains_key(camera) {
                            continue;
                        }
                        if let Some(cs) = cam_syncs.get(camera) {
                            av_judged += 1;
                            if cs.candidates != 0 {
                                av_all_zero = false;
                            }
                        }
                    }
                    let av_audio_state = av_window::classify_av_audio_state(
                        av_judged,
                        av_all_zero,
                        av.audio_preamble_screens_passed,
                    );
                    av_json.insert(
                        "av_audio_silent".to_string(),
                        match av_audio_state.av_audio_silent_flag() {
                            Some(silent) => serde_json::json!(silent),
                            None => serde_json::Value::Null,
                        },
                    );
                    av_json.insert(
                        "av_audio_preamble_screens".to_string(),
                        serde_json::json!(av.audio_preamble_screens_passed),
                    );
                    report["all_cambox_av_sync"] = serde_json::Value::Object(av_json);
                    // #861: folds into `all_pass` again — a no-op when the gate PASSES (av_all_pass
                    // == true) or when `gates_overall_pass()` reverts to report-only in the future
                    // (`!av_gate_blocking` short-circuits the OR to true); otherwise a FAILING
                    // av_sync gate now forces `all_pass = false`, same severity as the loss/latency
                    // gates. Zero-loss (all_cambox_continuity / burn-id contiguity, `seg.overall_pass`
                    // above) remains STRICT and is completely unaffected by this either way.
                    all_pass &= av_all_pass || !av_gate_blocking;
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
    cam7_present: bool,
) -> &'static str {
    if !cam1_present {
        for (node, present) in [
            ("cam2", cam2_present),
            ("cam3", cam3_present),
            ("cam4", cam4_present),
            ("cam5", cam5_present),
            ("cam6", cam6_present),
            ("cam7", cam7_present),
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
        "cam7" => args.burn_cam7_run_id,
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

    // #707 EVENT-FORENSICS: when the stream box's extract is given the SAME `--switch-schedule`
    // the merge step uses, ALSO flag the ±2-frame neighbourhood of every located residual
    // copy/gap event (`crate::residual_events::residual_events`, via the #312 sweep's own
    // `segment_continuity`) — extending the #186 pixel-proof machinery so a forensics dossier
    // gets real pixels for the event, not just the JSON. Only the stream box carries the
    // continuous recording the sweep is windowed over (the strih box's own recording is a
    // DIFFERENT, per-camera partial view — see `segment_frames_from_recording`'s own doc). A
    // missing/unparsable schedule degrades gracefully (WARN, no residual-event flagging) — the
    // existing undecodable + missing-burn flagging above is unaffected either way.
    if box_name == "stream" {
        if let Some(schedule_path) = &args.switch_schedule {
            match load_switch_schedule(schedule_path) {
                Ok(schedule) => {
                    let expected_step = if args.switch_expected_step > 0 {
                        args.switch_expected_step
                    } else {
                        camera_box::recording_span_gate::painted_tick_step(
                            args.refresh_hz,
                            args.stream_capture_fps,
                        )
                    };
                    let anchor_run_ids = [args.burn_strih_run_id, args.burn_stream_run_id];
                    let (seg_frames, _no_anchor) = segment_frames_from_recording(
                        frames,
                        &anchor_run_ids,
                        &all_burns,
                        cam2_pin,
                    );
                    let seg = segment_continuity(
                        &seg_frames,
                        &schedule,
                        args.switch_guard_ns,
                        expected_step,
                    );
                    const RESIDUAL_EVENT_NEIGHBOUR_FRAMES: u64 = 2;
                    for ev in &seg.residual_events {
                        let lo = ev
                            .frame_index
                            .saturating_sub(RESIDUAL_EVENT_NEIGHBOUR_FRAMES);
                        let hi = ev.frame_index + RESIDUAL_EVENT_NEIGHBOUR_FRAMES;
                        flagged.extend(lo..=hi);
                    }
                    if !seg.residual_events.is_empty() {
                        println!(
                            "#707 event-forensics [stream]: {} residual copy/gap event(s) across \
                             {} switch-schedule window(s) → their ±{RESIDUAL_EVENT_NEIGHBOUR_FRAMES}-frame \
                             neighbourhoods added to the pixel-proof flag set.",
                            seg.residual_events.len(),
                            seg.segments.len(),
                        );
                    }
                }
                Err(e) => {
                    eprintln!(
                        "WARNING: #707 event-forensics: could not load --switch-schedule {} \
                         ({e:#}) — residual copy/gap events will NOT get pixel proof on this \
                         extract (the existing undecodable/missing-burn flagging is unaffected).",
                        schedule_path.display()
                    );
                }
            }
        }
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
    // #1112 — when the STREAM box extracts for an all-cambox run (`--switch-schedule` is present,
    // which the production `VERDICT_ON_STREAM=1` gate ALWAYS pushes to the stream box — so this is
    // default-on, never a new forgettable flag), row-sample every recorded frame's content hash ON
    // this box (the recording is LOCAL here) and CARRY it in the partial. The dev1 merge — which
    // has no recording — then slices it per cambox window to feed the #1088 dup-cadence surface;
    // without this carry that surface is structurally unreachable in the merge gate (#1101 finding:
    // 0/81 verdicts). Gated on box+schedule so a delivery-only / non-all-cambox extract pays
    // nothing. A hash failure is NON-FATAL: the surface is report-only, so a hiccup must never fail
    // the extract — log and carry `None` (the merge then simply skips the surface, as before).
    // NOTE: a SEPARATE luma-only ffmpeg pass for now (`frame_prev_diffs`); folding it into the
    // existing burns/ticks decode to avoid the second pass is a report-only follow-up optimization.
    let frame_prev_diffs: Option<Vec<Option<f64>>> = if box_name == "stream"
        && args.switch_schedule.is_some()
    {
        match camera_box::probe::recording::frame_prev_diffs(rec_path) {
            Ok(d) => {
                println!(
                    "#1112/#1166 dup-cadence [stream]: row-sampled near-duplicate MADs for {} frames \
                     carried in the partial (feeds the #1088 duplication-masked 50->60 surface in the \
                     dev1 merge).",
                    d.len()
                );
                Some(d)
            }
            Err(e) => {
                eprintln!(
                    "WARNING: #1112/#1166 dup-cadence [stream]: could not diff the stream recording \
                     ({e}) — carrying no frame diffs; the report-only dup-cadence surface will be \
                     skipped in the merge (never fails the run)."
                );
                None
            }
        }
    } else {
        None
    };
    // #1143 — when the harness passed --record-render-stats for the imag extract, parse OBS's own
    // record-session render stats (captured from the imag OBS log stop-stats around the record
    // window) and carry them in the partial. Gated on the imag box (the observer-effect source, the
    // only box whose recording overloaded a software encoder). A parse failure ERRORS loudly: the
    // harness only passes a value it already parsed from the log, so a shape mismatch here is a real
    // extract/merge coupling bug, not a benign skip.
    let record_render = if box_name == "imag" {
        match &args.record_render_stats {
            Some(json) => {
                let stats: camera_box::record_render_stats::RecordRenderStats =
                    serde_json::from_str(json)
                        .with_context(|| format!("parse --record-render-stats JSON {json:?}"))?;
                Some(stats)
            }
            None => None,
        }
    } else {
        None
    };
    let partial = RecordingPartial::from_frames(box_name, rec_path, &expected_burns, frames)
        .with_colour(colour)
        .with_av_sync(av_sync)
        .with_frame_prev_diffs(frame_prev_diffs)
        .with_record_render(record_render);
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
    // #1112/#1166 — the stream partial's carried per-frame near-duplicate MAD-to-predecessor vector
    // (Some only when the stream box extracted on an all-cambox run). Only the STREAM recording feeds
    // the #1088 dup-cadence surface, mirroring `stream_av_sync` above; the dev1 merge has no
    // recording, so this carry is the ONLY way the pixel-derived signal reaches the merge verdict.
    let mut stream_frame_prev_diffs: Option<Vec<Option<f64>>> = None;
    // Each box's partial path, so after the verdict we can point the operator at the #186 pixel
    // proofs that box wrote during `--extract-partial` and the harness pulled back beside it.
    let mut box_paths: Vec<(String, PathBuf)> = Vec::new();
    // issue 1118 — the reason the imag leg was DROPPED this run (a schema-mismatched report-only
    // partial), surfaced at `full_chain.imag_leg_skip_reason` so a degraded run is mineable, not
    // silent. `None` on a normal run.
    let mut imag_skip_reason: Option<String> = None;
    // #1143 — OBS's own record-session render stats carried from the imag partial's `record_render`
    // (Some only when the imag box was extracted with `--record-render-stats`). Surfaced report-only.
    let mut imag_record_render: Option<camera_box::record_render_stats::RecordRenderStats> = None;
    for spec in &args.merge_partials {
        let (box_name, path) = spec
            .split_once('=')
            .with_context(|| format!("--merge-partials expects BOX=JSON, got {spec:?}"))?;
        // issue 1118 -> #1142 — a schema-mismatched imag partial must DEGRADE (drop it, keep
        // merging strih+stream), never abort the whole merge with no verdict JSON (the fatal
        // `load(path)?` before). The DEGRADE is unchanged; only its CONSEQUENCE changed: the dropped
        // partial sets `imag_leg_verified=false`, which #1142 makes BLOCKING, so the run now
        // produces a RED verdict (honest — a stale imag emitter IS a defect) instead of silently
        // passing. Degrading (RED verdict) still beats aborting (no verdict at all). The decision is
        // the PURE crate-root `partial_schema_gate` seam (Tier-0-tested): it degrades ONLY a clean
        // schema mismatch on the imag box; strih/stream and every non-schema failure stay fatal.
        // Why a MISSING / unreadable imag partial staying Fatal is safe here (not a contradiction
        // with "imag is report-only"): a genuinely-skipped imag leg never reaches this load at all
        // — recording-e2e.sh `[8/8d]` only appends `--merge-partials imag=<path>` when
        // `[ -f "$IMAG_PARTIAL" ]` (harness_imag_topology anchors that guard), so the ONLY way an
        // imag spec arrives is that the file EXISTS. A present-but-schema-stale file is exactly the
        // degrade case; a present-but-corrupt/unreadable file is a genuine defect worth the Fatal.
        let partial = match RecordingPartial::load(Path::new(path)) {
            Ok(p) => p,
            Err(e) => {
                let found = std::fs::read_to_string(path)
                    .ok()
                    .and_then(|s| camera_box::partial_schema_gate::peek_schema_version(&s));
                match camera_box::partial_schema_gate::classify_load_failure(
                    box_name,
                    found,
                    camera_box::probe::recording_partial::PARTIAL_SCHEMA_VERSION,
                ) {
                    camera_box::partial_schema_gate::PartialLoadDisposition::Degrade { reason } => {
                        eprintln!(
                            "WARNING: --merge-partials {box_name}={path}: {reason} Load error: {e:#}"
                        );
                        imag_skip_reason = Some(reason);
                        continue;
                    }
                    camera_box::partial_schema_gate::PartialLoadDisposition::Fatal => {
                        return Err(e).with_context(|| format!("load partial {path}"));
                    }
                }
            }
        };
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
        // #377/#312/#1112 — take the carried colour summary + A/V-sync inputs + near-duplicate diffs
        // before `frames` moves into the DecodedRec.
        let colour = partial.colour;
        let av_sync = partial.av_sync;
        let frame_prev_diffs = partial.frame_prev_diffs;
        let record_render = partial.record_render; // #1143 — carried before `frames` moves below
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
                stream_frame_prev_diffs = frame_prev_diffs;
            }
            // #461: imag carries no burns, so there is no colour to carry either in this ticket.
            "imag" => {
                imag = Some(rec);
                imag_record_render = record_render; // #1143 report-only OBS record-render stats
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
    let (_report, all_pass) = build_and_print_verdict_with_stream_diffs(
        args,
        strih,
        stream,
        Cam1Source::Absent,
        strih_colour,
        stream_colour,
        imag,
        stream_av_sync, // #312 item 2 (PR A): carried from the stream partial's --av-marker-log extract
        stream_frame_prev_diffs, // #1112/#1166: carried from the stream partial's all-cambox extract
        imag_skip_reason, // issue 1118: Some when a schema-mismatched imag partial was dropped (degrade)
        imag_record_render, // #1143: carried from the imag partial's --record-render-stats extract
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
        for cam in ["cam1", "cam3", "cam4", "cam5", "cam6", "cam7"] {
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
            super::BURN_RUN_ID_CAM7,
        ];
        assert!(
            !other_reserved.contains(&args.burn_cam3_run_id),
            "--burn-cam3-run-id defaults to {}, which collides with another reserved burn \
             run_id {other_reserved:?} (#24) — BURN_RUN_ID_IMAG=911003 is imag-nb's OWN digital \
             corner burn (#463); reserve cam3 a FRESH, unique run_id instead of reusing it",
            args.burn_cam3_run_id
        );
    }

    /// #312/#755 — cam2/cam5/cam6/cam7's default capture-burn run_ids must ALSO be unique among
    /// every other reserved id (mirrors the #24 cam3 regression test above — the same class of
    /// latent collision bug is exactly what reserving a FRESH id per new camera-under-test guards
    /// against). All TEN reserved ids must be pairwise distinct.
    #[test]
    fn all_ten_reserved_burn_run_ids_are_pairwise_distinct_755() {
        use clap::Parser;
        use std::collections::HashSet;

        let args = super::Args::parse_from(["recording-verdict"]);
        let ids: [(&str, u32); 10] = [
            ("cam1", super::BURN_RUN_ID_CAM1),
            ("cam2", args.burn_cam2_run_id),
            ("cam3", super::BURN_RUN_ID_CAM3),
            ("cam4", super::BURN_RUN_ID_CAM4),
            ("cam5", args.burn_cam5_run_id),
            ("cam6", args.burn_cam6_run_id),
            ("cam7", args.burn_cam7_run_id),
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

    /// #904 — like [`window`], but injects a genuine cam1 forward gap at EVERY position in
    /// `gaps_at` (each one a SEPARATE missing id, never merged — positions must be spaced apart).
    /// Used to build fixtures with a KNOWN, controllable count of REAL DROP ids (rather than
    /// `window`'s single optional gap) to exercise the #904 per-node real_drops allowance.
    fn window_multi_gap(n: u32, with_stream: bool, gaps_at: &[u32]) -> Vec<RecordingFrame> {
        (0..n)
            .map(|i| {
                let mut ps: Vec<(u32, u32)> = vec![(CAM2, 100 + i)];
                let skipped_before = gaps_at.iter().filter(|&&g| i >= g).count() as u32;
                ps.push((CAM1B, 5000 + i + skipped_before));
                ps.push((STRIH, 1670 + 3 * i));
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

    /// #1142 — `--require-imag-leg` gates a MISSING imag leg (the honesty flip), with the offline-ack
    /// exemption. Reuses the SAME clean cam2 scenario as `cam2_digital_burn_..._312` (contiguous
    /// strih+stream, imag=None, overall PASS by default) so the A/B isolates the flag: without it the
    /// missing imag leg does not red; with it a non-acked missing leg REDs; with it + an operator
    /// offline-ack (#1013) it is the ONE sanctioned skip and passes again.
    #[test]
    fn require_imag_leg_flag_gates_a_missing_imag_leg_1142() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;

        let mk = || {
            (
                Some(DecodedRec {
                    frames: window_cam2(60, false, None),
                    rec_path: None,
                }),
                Some(DecodedRec {
                    frames: window_cam2(60, true, None),
                    rec_path: None,
                }),
            )
        };

        // Flag OFF (default): a missing imag leg is surfaced but does NOT red the run.
        let args_off = super::Args::parse_from(["recording-verdict", "--min-secs", "1"]);
        let (s, st) = mk();
        let (v_off, pass_off) =
            build_and_print_verdict(&args_off, s, st, Cam1Source::Absent, None, None, None, None)
                .expect("verdict");
        assert!(
            pass_off,
            "#1142: without --require-imag-leg a missing imag leg does NOT red: {v_off}"
        );
        assert_eq!(
            v_off["full_chain"]["imag_leg_required"],
            serde_json::json!(false)
        );
        assert_eq!(
            v_off["full_chain"]["imag_leg_verified"],
            serde_json::json!(false)
        );
        assert_eq!(
            v_off["full_chain"]["imag_leg_verified_gates_overall_pass"],
            serde_json::json!(false),
            "flag off ⇒ the verified term does not gate this run: {v_off}"
        );

        // Flag ON, imag NOT acked: the SAME clean run now REDs — a silently-skipped imag leg is a
        // hidden partial the full-chain contract forbids (#798 honesty mandate).
        let args_on =
            super::Args::parse_from(["recording-verdict", "--min-secs", "1", "--require-imag-leg"]);
        let (s, st) = mk();
        let (v_on, pass_on) =
            build_and_print_verdict(&args_on, s, st, Cam1Source::Absent, None, None, None, None)
                .expect("verdict");
        assert!(
            !pass_on,
            "#1142: --require-imag-leg + a missing (non-acked) imag leg must RED: {v_on}"
        );
        assert_eq!(
            v_on["full_chain"]["imag_leg_required"],
            serde_json::json!(true)
        );
        assert_eq!(
            v_on["full_chain"]["imag_leg_verified_gates_overall_pass"],
            serde_json::json!(true),
            "flag on + seam live ⇒ the verified term gates: {v_on}"
        );

        // Flag ON + imag operator-offline-acked (#1013): the ONE sanctioned skip — back to PASS.
        let args_ack = super::Args::parse_from([
            "recording-verdict",
            "--min-secs",
            "1",
            "--require-imag-leg",
            "--offline-ack-cams",
            "imag:notebook-away",
        ]);
        let (s, st) = mk();
        let (v_ack, pass_ack) =
            build_and_print_verdict(&args_ack, s, st, Cam1Source::Absent, None, None, None, None)
                .expect("verdict");
        assert!(
            pass_ack,
            "#1013: an operator-offline-acked imag is the ONE sanctioned skip — no red: {v_ack}"
        );
        assert_eq!(
            v_ack["full_chain"]["imag_leg_verified_offline_acked"],
            serde_json::json!(true)
        );
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
    const CAM7B: u32 = super::BURN_RUN_ID_CAM7; // #755 cam7's OWN per-EMIT capture burn run_id (911012)

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

    /// #755 — a window of N delivered frames carrying cam7's OWN digital capture-burn in every
    /// frame (mirrors [`window_cam5_and_cam6`] for the 7th camera, #753).
    fn window_cam7(n: u32, with_stream: bool) -> Vec<RecordingFrame> {
        (0..n)
            .map(|i| {
                let mut ps: Vec<(u32, u32)> = vec![(CAM2, 100 + i), (CAM7B, 9500 + i)];
                ps.push((STRIH, 1670 + 3 * i));
                if with_stream {
                    ps.push((STREAM, 12000 + 3 * i));
                }
                frame(i as u64, &ps)
            })
            .collect()
    }

    /// #755 — extends the #186 per-node digital-burn contiguity check to CAM7 (fleet growth 6→7,
    /// #753). A contiguous cam7 burn end-to-end ⇒ the fused verdict reports node "cam7" ZERO loss,
    /// exactly like cam5/cam6 — locks that the new `NodeSpec` tuple, the `--burn-cam7-run-id`
    /// plumbing, and the `CAMERA_UNDER_TEST_NODES` membership all wired correctly (a missing site
    /// would leave cam7 unmeasured / absent from the report, the "NO SAMPLES" mode #286 fixed).
    #[test]
    fn cam7_digital_burn_extends_the_186_contiguity_check_755() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;

        let args = super::Args::parse_from(["recording-verdict", "--min-secs", "1"]);
        let strih_frames = window_cam7(60, false);
        let stream_frames = window_cam7(60, true);

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

        assert!(pass, "#755: contiguous cam7 burn ⇒ overall PASS: {v}");
        let loss = &v["full_chain"]["loss"];
        assert_eq!(
            loss["cam7"]["zero_loss"],
            serde_json::json!(true),
            "#755: cam7 must be verdicted ZERO loss when its OWN burn is contiguous: {loss}"
        );
        for absent in ["cam1", "cam3", "cam4", "cam5", "cam6"] {
            assert!(
                loss.get(absent).is_none(),
                "#755: {absent} never emitted this run ⇒ must NOT appear in the loss report: {loss}"
            );
        }
        assert_eq!(
            v["full_chain"]["burn_ids_present"]["cam7"],
            serde_json::json!(60),
            "#755: all 60 cam7 burn ids decoded: {}",
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
        //
        // #904 had briefly widened this to require 3 well-separated gaps (one past its default
        // allowance of 2) so this SAFETY test kept proving loss BEYOND that allowance. #905 item
        // 1 reverted `REAL_DROPS_ALLOWANCE_DEFAULT` back to 0 (a single gap sufficed then); issue
        // 1169 (owner, 2026-08-22) RE-WIDENED the default to the <=1 SINGLETON band, so this
        // SAFETY test now injects TWO well-separated gaps (one PAST the singleton band) to keep
        // proving the never-mask invariant BEYOND the allowance — the same shape #904 used for
        // its allowance of 2, re-scaled to the singleton band 1. The allowance mechanism itself
        // stays overridable via `CAMERA_BOX_REAL_DROPS_ALLOWANCE`.
        let args_1to1 = super::Args::parse_from([
            "recording-verdict",
            "--min-secs",
            "1",
            "--capture-fps",
            "60",
        ]);
        // TWO well-separated gaps ⇒ 2 REAL DROP ids, one PAST the issue-1169 singleton band (1).
        let safety_gaps = [5, 300];
        let (v2, pass2) = build_and_print_verdict(
            &args_1to1,
            Some(DecodedRec {
                frames: window_multi_gap(N, false, &safety_gaps),
                rec_path: None,
            }),
            Some(DecodedRec {
                frames: window_multi_gap(N, true, &safety_gaps),
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
            "#356/#1169: genuine 1:1-hop cam1 loss BEYOND the singleton band ⇒ overall FAIL"
        );
        assert_eq!(v2["full_chain"]["zero_loss"], serde_json::json!(false));
        assert_eq!(
            v2["full_chain"]["real_drops"],
            serde_json::json!(2),
            "#356/#1169 SAFETY: on the 1:1 hop, cam1 ids absent from BOTH recordings MUST stay \
             REAL DROP — 2 (one past the <=1 singleton band) are never masked: {}",
            v2["full_chain"]
        );
    }

    // ---- #904/#905/#1169 — the small, explicit, LOUD real_drops allowance MECHANISM, and the
    // #1169-re-widened <=1 SINGLETON default (issue 905 item 1's zero bar, re-widened by 1169) ----

    /// #1169 (owner, 2026-08-22) — SECOND SEAM: RE-WIDENS `REAL_DROPS_ALLOWANCE_DEFAULT` back to
    /// 1 for the sanctioned per-frame delivery SINGLETON (issue-1167 v3 paced-trickle absorption
    /// plus a FIFO stale_replay in the same event; burn_unreadable stays 0). Owner's 2026-07-31
    /// strict-test revision: "jedna stratená snímka nie je problém." With the DEFAULT now 1 (no
    /// env override), a SINGLE genuine 1:1-hop cam1 real drop PASSES the headline WITHIN the
    /// allowance and is reported LOUDLY (never a silent green) — and 2 drops still FAIL, so the
    /// band is a strict singleton, never an open door. Inverts the issue-905 zero-bar test per
    /// `gate-allowance-restore-red-green.md`; the one-constant re-tighten back to 0 is proven
    /// dormant by `re_tightening_the_1169_allowance_to_zero_restores_the_strict_bar`.
    #[test]
    fn single_real_drop_passes_loudly_within_the_1169_singleton_allowance() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;
        const N: u32 = 600;
        let args = super::Args::parse_from([
            "recording-verdict",
            "--min-secs",
            "1",
            "--capture-fps",
            "60",
        ]);
        // exactly ONE genuine 1:1-hop cam1 real drop ⇒ the sanctioned singleton.
        let one = [100];
        let (v, pass) = build_and_print_verdict(
            &args,
            Some(DecodedRec {
                frames: window_multi_gap(N, false, &one),
                rec_path: None,
            }),
            Some(DecodedRec {
                frames: window_multi_gap(N, true, &one),
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
            "#1169: a single real drop must PASS within the re-widened singleton allowance: {v}"
        );
        assert_eq!(v["full_chain"]["zero_loss"], serde_json::json!(true));
        assert_eq!(v["full_chain"]["real_drops"], serde_json::json!(1));
        assert_eq!(
            v["full_chain"]["real_drops_allowance"],
            serde_json::json!(super::REAL_DROPS_ALLOWANCE_DEFAULT),
            "#1169: the DEFAULT allowance (no env override) must read back as the re-widened band: {}",
            v["full_chain"]
        );
        assert_eq!(
            super::REAL_DROPS_ALLOWANCE_DEFAULT,
            1,
            "#1169: the compiled DEFAULT must be the singleton band 1 (the re-tighten trail on \
             issue 1169 flips this one constant back to 0)"
        );
        // LOUD, never silent: the run-level consumed signal NAMES the node that only cleared the
        // gate on slack — a singleton pass must never look identical to a genuine zero-loss pass.
        let consumed_nodes = v["full_chain"]["real_drops_allowance_consumed_nodes"]
            .as_array()
            .expect("real_drops_allowance_consumed_nodes must be a JSON array");
        assert_eq!(
            consumed_nodes,
            &vec![serde_json::json!("cam1")],
            "#1169: the singleton pass must LOUDLY name the node that consumed the allowance: {}",
            v["full_chain"]
        );
        let cam1_loss = &v["full_chain"]["loss"]["cam1"];
        assert_eq!(cam1_loss["zero_loss"], serde_json::json!(true));
        assert_eq!(
            cam1_loss["consumed_real_drops_allowance"],
            serde_json::json!(true),
            "#1169: the singleton node must carry the LOUD consumed flag: {cam1_loss}"
        );

        // TWO drops STILL FAIL — the band is a strict <=1 singleton, never an open door.
        let two = [100, 300];
        let (v2, pass2) = build_and_print_verdict(
            &args,
            Some(DecodedRec {
                frames: window_multi_gap(N, false, &two),
                rec_path: None,
            }),
            Some(DecodedRec {
                frames: window_multi_gap(N, true, &two),
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            None,
            None,
        )
        .expect("verdict");
        assert!(
            !pass2,
            "#1169: TWO real drops must STILL FAIL — the allowance is a <=1 singleton band: {v2}"
        );
        assert_eq!(v2["full_chain"]["zero_loss"], serde_json::json!(false));
        assert_eq!(v2["full_chain"]["real_drops"], serde_json::json!(2));
    }

    /// #1169 — DORMANT re-tighten proof: flipping the ONE constant back to 0 (this ticket's
    /// re-tighten trail, closed only by a zero-singleton green run) restores the STRICT bar.
    /// Proven at the pure-method level with an EXPLICIT allowance of 0 (what
    /// `REAL_DROPS_ALLOWANCE_DEFAULT = 0` yields), independent of the compiled default and of
    /// process env — so the strict path stays regression-tested while the DEFAULT is the
    /// re-widened 1. Mirrors the issue-905 restore discipline
    /// (`gate-allowance-restore-red-green.md`), inverted: the mechanism is dormant, never deleted.
    #[test]
    fn re_tightening_the_1169_allowance_to_zero_restores_the_strict_bar() {
        let one_drop = window_multi_gap(200, false, &[100]);
        let tmp = tempfile::tempdir().unwrap();
        let v = node_verdict(
            &super::NodeSpec {
                node: "cam1",
                burn_run_id: CAM1B,
                rate: BurnRate::PerEmittedFrame,
                source: &one_drop,
                rec_path: None,
                cam2_run_id: None,
                step: 1,
            },
            &[CAM1B, STRIH, STREAM],
            tmp.path(),
            0,
        )
        .unwrap();
        assert_eq!(v.real_drops(), 1);
        // At the re-widened DEFAULT band (1) the SAME singleton passes on slack, LOUDLY ...
        assert!(
            v.is_zero_within_allowance(1),
            "#1169: the singleton passes at the re-widened band: {v:?}"
        );
        assert!(v.consumed_real_drops_allowance(1));
        // ... and re-tightening the ONE constant back to 0 restores the strict zero-drop bar:
        assert!(
            !v.is_zero_within_allowance(0),
            "#1169: re-tightening to 0 restores the strict bar for the same singleton: {v:?}"
        );
        assert!(!v.consumed_real_drops_allowance(0));
    }

    /// #905/#1169 — the #904 allowance MECHANISM itself is untouched by item 1's revert AND by
    /// issue 1169's re-widen of the DEFAULT (only the default VALUE has ever moved: 2 -> 0 -> 1).
    /// An EXPLICIT nonzero allowance ABOVE the default (e.g. a future incident
    /// re-widening it via `CAMERA_BOX_REAL_DROPS_ALLOWANCE`) must still tolerate real drops
    /// within it, and the pass MUST still be visibly distinguishable from a genuine zero-loss
    /// pass: `real_drops_allowance` + `consumed_real_drops_allowance` on the per-node JSON, LOUD
    /// as #904 originally required. Proven at the `NodeVerdict`/`node_verdict_json` level (both
    /// take `allowance` as an explicit parameter, independent of process env) so this test never
    /// needs to mutate global env state.
    #[test]
    fn real_drops_within_an_explicit_nonzero_allowance_still_passes_and_is_reported_loudly_905() {
        const EXPLICIT_ALLOWANCE: u32 = 2;
        // Two well-separated genuine gaps ⇒ exactly 2 REAL DROP ids on the 1:1 hop.
        let two_drops = window_multi_gap(600, false, &[100, 300]);
        let tmp = tempfile::tempdir().unwrap();
        let v = node_verdict(
            &super::NodeSpec {
                node: "cam1",
                burn_run_id: CAM1B,
                rate: BurnRate::PerEmittedFrame,
                source: &two_drops,
                rec_path: None,
                cam2_run_id: None,
                step: 1,
            },
            &[CAM1B, STRIH, STREAM],
            tmp.path(),
            0,
        )
        .unwrap();
        assert_eq!(v.real_drops(), 2);
        assert!(
            v.is_zero_within_allowance(EXPLICIT_ALLOWANCE),
            "#905: an explicit nonzero allowance must still tolerate drops within it: {v:?}"
        );
        assert!(
            v.consumed_real_drops_allowance(EXPLICIT_ALLOWANCE),
            "#905: and the pass must be marked as having consumed slack: {v:?}"
        );
        let json = super::node_verdict_json(&v, 20.0, true, 1.0, EXPLICIT_ALLOWANCE);
        assert_eq!(json["zero_loss"], serde_json::json!(true));
        assert_eq!(json["real_drops"], serde_json::json!(2));
        assert_eq!(
            json["real_drops_allowance"],
            serde_json::json!(EXPLICIT_ALLOWANCE)
        );
        assert_eq!(
            json["consumed_real_drops_allowance"],
            serde_json::json!(true),
            "#905/#1169: the per-node JSON must LOUDLY carry the consumed signal at an EXPLICIT \
             allowance above the compiled default: {json}"
        );

        // At an EXPLICIT allowance of 0 (what the #1169 re-tighten flip restores), the SAME two
        // drops must FAIL.
        assert!(
            !v.is_zero_within_allowance(0),
            "#905/#1169: at an explicit allowance of 0, the same 2 drops must FAIL: {v:?}"
        );
        assert!(!v.consumed_real_drops_allowance(0));
    }

    /// #904 — `is_zero_within_allowance(0)` must be BYTE-IDENTICAL to the pre-#904 `is_zero()` on
    /// both a clean node AND a node carrying a real drop — the documented "default keeps current
    /// behavior" requirement, proven at the pure-method level (independent of whichever numeric
    /// default `REAL_DROPS_ALLOWANCE_DEFAULT` happens to be).
    #[test]
    fn allowance_zero_matches_pre_904_is_zero_exactly_904() {
        let clean = window_multi_gap(200, false, &[]);
        let tmp = tempfile::tempdir().unwrap();
        let v_clean = node_verdict(
            &super::NodeSpec {
                node: "cam1",
                burn_run_id: CAM1B,
                rate: BurnRate::PerEmittedFrame,
                source: &clean,
                rec_path: None,
                cam2_run_id: None,
                step: 1,
            },
            &[CAM1B, STRIH, STREAM],
            tmp.path(),
            0,
        )
        .unwrap();
        assert!(v_clean.is_zero());
        assert_eq!(v_clean.is_zero_within_allowance(0), v_clean.is_zero());
        assert!(!v_clean.consumed_real_drops_allowance(0));

        let one_drop = window_multi_gap(200, false, &[100]);
        let v_drop = node_verdict(
            &super::NodeSpec {
                node: "cam1",
                burn_run_id: CAM1B,
                rate: BurnRate::PerEmittedFrame,
                source: &one_drop,
                rec_path: None,
                cam2_run_id: None,
                step: 1,
            },
            &[CAM1B, STRIH, STREAM],
            tmp.path(),
            0,
        )
        .unwrap();
        assert_eq!(v_drop.real_drops(), 1);
        assert!(
            !v_drop.is_zero(),
            "a genuine real drop must fail is_zero(): {v_drop:?}"
        );
        assert_eq!(
            v_drop.is_zero_within_allowance(0),
            v_drop.is_zero(),
            "#904: allowance 0 must reproduce is_zero() exactly, even with a real drop present"
        );
        assert!(!v_drop.consumed_real_drops_allowance(0));
    }

    /// #904 — `burn_unreadable` stays an UNCONDITIONAL hard fail, regardless of how large the
    /// real_drops allowance is (the ticket's own "does NOT touch burn_unreadable" line). A frame
    /// that WAS delivered (cam2's optical marker present) but carries no readable cam1 burn must
    /// still fail even against a huge allowance.
    #[test]
    fn burn_unreadable_is_never_excused_by_any_real_drops_allowance_904() {
        let frames = window_none(200, false, 100);
        let tmp = tempfile::tempdir().unwrap();
        let v = node_verdict(
            &super::NodeSpec {
                node: "cam1",
                burn_run_id: CAM1B,
                rate: BurnRate::PerEmittedFrame,
                source: &frames,
                rec_path: None,
                cam2_run_id: None,
                step: 1,
            },
            &[CAM1B, STRIH, STREAM],
            tmp.path(),
            0,
        )
        .unwrap();
        assert_eq!(
            v.real_drops(),
            0,
            "the missing cam1 burn is BURN-UNREADABLE, not a real drop"
        );
        assert!(v.burn_unreadable() >= 1, "{v:?}");
        assert!(
            !v.is_zero_within_allowance(1000),
            "#904: burn_unreadable must stay a hard fail even against a huge allowance: {v:?}"
        );
        assert!(!v.consumed_real_drops_allowance(1000));
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

    /// Issue 889 re-gate (2026-08-05 ROZHODNUTÉ, ticket 889 comment 5196190653) end-to-end: a
    /// SINGLE stale/frozen painted-tick copy in the single all-cambox window (undecodable=0) sits
    /// WITHIN the per-window tolerance (`copies<=WINDOW_COPIES_GAPS_TOLERANCE`, recalibrated
    /// 1 → 2 → 3 on 2026-08-06, ticket 889 comments 5198131539 / 5200533407) — it must still be
    /// COMPUTED and printed in the verdict JSON and must still fail that window's STRICT `pass`.
    /// (Issue 1132, 2026-08-19) made a bare copy ALSO fail `all_cambox_continuity.overall_pass` —
    /// the `<=3` tolerance rescue was disarmed. Issue 1169 (owner, 2026-08-22) then absorbed a
    /// `<=1/<=1` SINGLETON back in through its OWN tighter seam, never a re-arm of the `<=3`
    /// rescue. **SUPERSEDED by issue 1220 (owner mandate, 2026-08-29): the `<=3` tolerance channel
    /// IS re-armed** (see `camera_box::window_gate::copies_gaps_tolerance_gates_overall_pass` for
    /// the full decision record) — a single copy is absorbed through THAT channel now, and the
    /// issue-1169 singleton band is dormant (superseded by `decide()`'s `if`/`else if`
    /// precedence, never deleted). `windows_failed_report_only` must still report it
    /// (strict-zero visibility is unaffected by any of the three seams). Renamed from
    /// `..._copy_alone_is_report_only_end_to_end_889` — the old name implied copies never gate at
    /// all, which stopped being true once the re-gate landed; a single copy still passes because
    /// 1 <= the tolerance, not because the term is inert.
    #[test]
    fn all_cambox_continuity_single_copy_within_tolerance_passes_overall_889_regate() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;

        const ONE_S: i64 = 1_000_000_000;
        let base = 1_000 * ONE_S;
        let win = 5 * ONE_S;
        let sched = format!(
            r#"[{{"cambox":"CAM1","start_ns":{a},"end_ns":{b}}}]"#,
            a = base,
            b = base + win
        );
        let dir = std::env::temp_dir().join(format!("cb-889-copy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sched_path = dir.join("switch-schedule.json");
        std::fs::write(&sched_path, &sched).unwrap();

        let mut stream_frames: Vec<RecordingFrame> = Vec::new();
        for i in 0..20u64 {
            let gen_ts = base + (i as i64 + 1) * (ONE_S / 10);
            let optical = 1000u32 + 2 * i as u32; // clean step-2 sequence, undecodable=0 throughout
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
        // Insert ONE extra frame right after index 9, repeating index 9's tick verbatim (a
        // stale/frozen copy) WITHOUT removing/overwriting any real tick value -- the distinct
        // present-tick sequence stays fully contiguous (no incidental gap), isolating `copies`
        // from `gaps` so this test proves copies alone is report-only.
        let dup_gen_ts = base + 10 * (ONE_S / 10) + (ONE_S / 100); // strictly between i=9 and i=10
        stream_frames.insert(
            10,
            RecordingFrame {
                frame_index: 9_000,
                payloads: vec![
                    Payload {
                        run_id: STRIH,
                        frame_id: 9_670,
                        gen_ts_ns: dup_gen_ts,
                    },
                    Payload {
                        run_id: CAM2,
                        frame_id: 1018, // == index 9's optical tick (1000 + 2*9)
                        gen_ts_ns: dup_gen_ts,
                    },
                ],
                tick: Some(1018),
            },
        );

        let args = super::Args::parse_from([
            "recording-verdict",
            "--switch-schedule",
            sched_path.to_str().unwrap(),
            "--switch-guard-ns",
            "0",
            "--switch-expected-step",
            "2",
        ]);
        let (v, _) = build_and_print_verdict(
            &args,
            None,
            Some(DecodedRec {
                frames: stream_frames,
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            None,
            None,
        )
        .expect("verdict");

        let seg = &v["all_cambox_continuity"];
        assert_eq!(
            seg["segments"][0]["copies"],
            serde_json::json!(1),
            "889: the copy is still COMPUTED and printed: {seg}"
        );
        assert_eq!(
            seg["segments"][0]["pass"],
            serde_json::json!(false),
            "889: the STRICT per-window verdict still fails on the copy: {seg}"
        );
        assert_eq!(
            seg["overall_pass"],
            serde_json::json!(true),
            "1220: a single copy is now ABSORBED into overall_pass via the RE-ARMED issue-1220 \
             <=3 tolerance channel (owner mandate, 2026-08-29) -- the issue-1169 <=1/<=1 \
             singleton band is dormant (superseded by precedence), not what did the absorbing: {seg}"
        );
        assert_eq!(
            seg["windows_singleton_allowance_consumed"],
            serde_json::json!(0),
            "1220: the singleton mechanism never fires while the tolerance channel is armed: {seg}"
        );
        assert!(
            seg["segments"][0]["singleton_allowance_note"].is_null(),
            "1220: no singleton note -- the tolerance channel absorbed this, not the singleton: {seg}"
        );
        assert_eq!(
            seg["windows_failed_report_only"],
            serde_json::json!(1),
            "889: the verdict JSON must carry the machine-readable report-only count: {seg}"
        );
        assert_eq!(
            seg["windows_over_copies_gaps_tolerance"],
            serde_json::json!(0),
            "889 re-gate: a single copy stays within tolerance -- no window is OVER tolerance: {seg}"
        );
        assert_eq!(
            seg["copies_gaps_tolerance"],
            serde_json::json!(camera_box::window_gate::WINDOW_COPIES_GAPS_TOLERANCE),
            "889 re-gate: the tolerance value must be echoed in the JSON (tracks the \
             window_gate constant so the walk-down never breaks this echo check): {seg}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Issue 889 re-gate (2026-08-05 ROZHODNUTÉ, recalibrated 1 → 2 → 3 on 2026-08-06, ticket 889
    /// comments 5198131539 / 5200533407) differential proof: `overall_pass` must swing from PASS
    /// to FAIL exactly at the tolerance boundary, and must NOT swing at all when copies/gaps stay
    /// AT the tolerance (even combined). Uses the SAME differential-fixture technique issue 914's
    /// `frozen_leg_and_self_heal_reset_no_longer_gate_the_overall_verdict_914` established: build
    /// otherwise-IDENTICAL fixtures varying only the defect under test, and diff `overall_pass`
    /// against a clean baseline, rather than asserting an absolute value (many other unrelated
    /// gates also fold into `overall_pass`). Renamed from
    /// `..._singleton_tolerance_boundary_...` — "singleton" (implying exactly one) stopped being
    /// an accurate description once the tolerance moved past 1; the fixtures below build AT the
    /// tolerance and tolerance+1 through the const instead of hardcoded literals, so this test
    /// tracks whatever the tolerance is calibrated to across every recalibration.
    ///
    /// **This invariant was briefly NOT what governed `overall_pass` between #1132 (2026-08-19,
    /// disarmed the tolerance rescue for the blocking verdict) and #1220 (owner mandate,
    /// 2026-08-29, re-armed it)** — fixture (a) below tracked that disarmed reality in between.
    /// #1220 restores this doc's original framing exactly.
    #[test]
    fn copies_gaps_tolerance_boundary_gates_overall_pass_889_regate() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;

        const ONE_S: i64 = 1_000_000_000;
        let base = 1_000 * ONE_S;
        let win = 5 * ONE_S;
        let sched = format!(
            r#"[{{"cambox":"CAM1","start_ns":{a},"end_ns":{b}}}]"#,
            a = base,
            b = base + win
        );

        // `dup_count`: consecutive stale/frozen copies of index 9's tick inserted right after it
        // (drives `copies`, never touches `gaps` -- the duplicate value is already present in the
        // sorted-distinct set `painted_tick_gaps` consumes). `gap_extra_steps` (k): the LAST
        // frame's optical tick is pushed `k` extra steps ahead of its normal value, opening exactly
        // `k` missing distinct values in the sorted tick range (drives `gaps`, never touches
        // `copies` -- every tick value, including the modified one, stays distinct). Both knobs are
        // independent by construction, so a fixture can isolate either term or combine them.
        fn build_fixture(
            tag: &str,
            base: i64,
            sched: &str,
            dup_count: u32,
            gap_extra_steps: u32,
        ) -> serde_json::Value {
            const ONE_S: i64 = 1_000_000_000;
            let dir =
                std::env::temp_dir().join(format!("cb-889-regate-{tag}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let sched_path = dir.join("switch-schedule.json");
            std::fs::write(&sched_path, sched).unwrap();

            let mut stream_frames: Vec<RecordingFrame> = Vec::new();
            for i in 0..20u64 {
                let gen_ts = base + (i as i64 + 1) * (ONE_S / 10);
                // Clean step-2 sequence 1000..1038, EXCEPT the last frame (i==19) is pushed
                // `gap_extra_steps` extra steps ahead when requested -- opening that many missing
                // distinct values in the sorted tick range without touching any other frame.
                let optical = if i == 19 {
                    1000u32 + 2 * i as u32 + 2 * gap_extra_steps
                } else {
                    1000u32 + 2 * i as u32
                };
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
            // Insert `dup_count` consecutive extra frames right after index 9, each repeating
            // index 9's tick verbatim (1018) -- a stale/frozen copy run. The distinct present-tick
            // sequence stays fully contiguous (the duplicate value is already present), isolating
            // `copies` from `gaps`.
            for k in 0..dup_count as u64 {
                let dup_gen_ts = base + 9 * (ONE_S / 10) + (k as i64 + 1) * (ONE_S / 1000);
                stream_frames.insert(
                    10 + k as usize,
                    RecordingFrame {
                        frame_index: 9_000 + k,
                        payloads: vec![
                            Payload {
                                run_id: STRIH,
                                frame_id: 9_670 + k as u32,
                                gen_ts_ns: dup_gen_ts,
                            },
                            Payload {
                                run_id: CAM2,
                                frame_id: 1018,
                                gen_ts_ns: dup_gen_ts,
                            },
                        ],
                        tick: Some(1018),
                    },
                );
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
            let (v, _) = build_and_print_verdict(
                &args,
                None,
                Some(DecodedRec {
                    frames: stream_frames,
                    rec_path: None,
                }),
                Cam1Source::Absent,
                None,
                None,
                None,
                None,
            )
            .expect("verdict");
            let _ = std::fs::remove_dir_all(&dir);
            v
        }

        let clean = build_fixture("clean", base, &sched, 0, 0);
        assert_eq!(
            clean["all_cambox_continuity"]["overall_pass"],
            serde_json::json!(true),
            "sanity: the clean fixture must pass: {clean}"
        );

        // Fixture knobs are `u32`; the const is `u32` too, so these read straight from it instead
        // of hardcoding the pre-recalibration boundary (1/2) -- this test self-adjusts with any
        // future recalibration of `WINDOW_COPIES_GAPS_TOLERANCE`.
        let tolerance = camera_box::window_gate::WINDOW_COPIES_GAPS_TOLERANCE;

        // (a) BOTH terms AT the tolerance simultaneously -- must NOT swing overall_pass.
        let at_tolerance = build_fixture("at-tolerance", base, &sched, tolerance, tolerance);
        let at_seg = &at_tolerance["all_cambox_continuity"];
        assert_eq!(
            at_seg["segments"][0]["copies"],
            serde_json::json!(tolerance),
            "{at_seg}"
        );
        assert_eq!(
            at_seg["segments"][0]["gaps"],
            serde_json::json!(tolerance),
            "{at_seg}"
        );
        assert_eq!(
            at_seg["overall_pass"],
            serde_json::json!(true),
            "1220: copies AND gaps AT the re-armed tolerance must NOT swing overall_pass -- the \
             tolerance channel is armed again (owner mandate, 2026-08-29): \
             clean={clean}, at_tolerance={at_tolerance}"
        );
        assert_eq!(
            at_seg["windows_over_copies_gaps_tolerance"],
            serde_json::json!(0),
            "{at_seg}"
        );

        // (b) copies alone OVER tolerance -- must swing overall_pass to FAIL.
        let copies_over = build_fixture("copies-over", base, &sched, tolerance + 1, 0);
        let copies_seg = &copies_over["all_cambox_continuity"];
        assert_eq!(
            copies_seg["segments"][0]["copies"],
            serde_json::json!(tolerance + 1),
            "{copies_seg}"
        );
        assert_eq!(
            copies_seg["segments"][0]["gaps"],
            serde_json::json!(0),
            "{copies_seg}"
        );
        assert_ne!(
            clean["all_cambox_continuity"]["overall_pass"], copies_seg["overall_pass"],
            "889 re-gate: copies over the tolerance -- overall_pass must swing to FAIL: \
             clean={clean}, copies_over={copies_over}"
        );
        assert_eq!(
            copies_seg["overall_pass"],
            serde_json::json!(false),
            "{copies_seg}"
        );
        assert_eq!(
            copies_seg["windows_over_copies_gaps_tolerance"],
            serde_json::json!(1),
            "{copies_seg}"
        );

        // (c) gaps alone OVER tolerance -- must swing overall_pass to FAIL.
        let gaps_over = build_fixture("gaps-over", base, &sched, 0, tolerance + 1);
        let gaps_seg = &gaps_over["all_cambox_continuity"];
        assert_eq!(
            gaps_seg["segments"][0]["copies"],
            serde_json::json!(0),
            "{gaps_seg}"
        );
        assert_eq!(
            gaps_seg["segments"][0]["gaps"],
            serde_json::json!(tolerance + 1),
            "{gaps_seg}"
        );
        assert_ne!(
            clean["all_cambox_continuity"]["overall_pass"], gaps_seg["overall_pass"],
            "889 re-gate: gaps over the tolerance -- overall_pass must swing to FAIL: \
             clean={clean}, gaps_over={gaps_over}"
        );
        assert_eq!(
            gaps_seg["overall_pass"],
            serde_json::json!(false),
            "{gaps_seg}"
        );
        assert_eq!(
            gaps_seg["windows_over_copies_gaps_tolerance"],
            serde_json::json!(1),
            "{gaps_seg}"
        );
    }

    /// Issue 915 (2026-08-01, user decision) end-to-end: a single all-cambox window whose
    /// undecodable count exceeds the issue-881 per-window floor (5 > 4) must still be COMPUTED
    /// and printed in the verdict JSON, must still fail that window's STRICT `pass`, but must NO
    /// LONGER fail `all_cambox_continuity.overall_pass` -- and the JSON must carry the new
    /// `undecodable_floor_gates_overall_pass` machine-readable flag.
    #[test]
    fn all_cambox_continuity_undecodable_over_floor_is_report_only_end_to_end_915() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;

        const ONE_S: i64 = 1_000_000_000;
        let base = 1_000 * ONE_S;
        let win = 5 * ONE_S;
        let sched = format!(
            r#"[{{"cambox":"CAM1","start_ns":{a},"end_ns":{b}}}]"#,
            a = base,
            b = base + win
        );
        let dir = std::env::temp_dir().join(format!("cb-915-undecodable-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sched_path = dir.join("switch-schedule.json");
        std::fs::write(&sched_path, &sched).unwrap();

        let mut stream_frames: Vec<RecordingFrame> = Vec::new();
        for i in 0..20u64 {
            let gen_ts = base + (i as i64 + 1) * (ONE_S / 10);
            // 5 undecodable frames (i in 5..10) inside an otherwise clean step-2 sequence -- the
            // whole-window net-span gap this creates (1008 -> 1020) is exactly credited by the 5
            // undecodable slots (painted_tick_gaps, issue 625), so copies=0 and gaps=0 stay
            // clean and this fixture isolates the floor term alone. 5 exceeds the per-window
            // floor (4) but stays within the run-wide floor (8).
            let mut payloads = vec![Payload {
                run_id: STRIH,
                frame_id: 1670 + i as u32,
                gen_ts_ns: gen_ts,
            }];
            let tick = if (5..10).contains(&i) {
                None
            } else {
                let optical = 1000u32 + 2 * i as u32;
                payloads.push(Payload {
                    run_id: CAM2,
                    frame_id: optical,
                    gen_ts_ns: gen_ts,
                });
                Some(optical)
            };
            stream_frames.push(RecordingFrame {
                frame_index: i,
                payloads,
                tick,
            });
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
        let (v, _) = build_and_print_verdict(
            &args,
            None,
            Some(DecodedRec {
                frames: stream_frames,
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            None,
            None,
        )
        .expect("verdict");

        let seg = &v["all_cambox_continuity"];
        assert_eq!(
            seg["segments"][0]["undecodable"],
            serde_json::json!(5),
            "915: the over-floor undecodable count is still COMPUTED and printed: {seg}"
        );
        assert_eq!(
            seg["segments"][0]["copies"],
            serde_json::json!(0),
            "915: isolates the floor term -- no copies: {seg}"
        );
        assert_eq!(
            seg["segments"][0]["gaps"],
            serde_json::json!(0),
            "915: isolates the floor term -- the net-span gap is fully credited by undecodable: {seg}"
        );
        assert_eq!(
            seg["segments"][0]["pass"],
            serde_json::json!(false),
            "915: the STRICT per-window verdict still fails on the over-floor undecodable count: {seg}"
        );
        assert_eq!(
            seg["overall_pass"],
            serde_json::json!(true),
            "915: an over-floor undecodable count alone no longer fails overall_pass: {seg}"
        );
        assert_eq!(
            seg["total_undecodable"],
            serde_json::json!(5),
            "915: the run-wide sum is still COMPUTED and reported: {seg}"
        );
        assert_eq!(
            seg["run_wide_undecodable_within_floor"],
            serde_json::json!(true),
            "915: 5 stays within the run-wide floor (8) -- isolates the per-window term: {seg}"
        );
        assert_eq!(
            seg["undecodable_floor_gates_overall_pass"],
            serde_json::json!(false),
            "915: the verdict JSON must carry the machine-readable report-only flag: {seg}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Issue 914 (2026-08-01, user decision -- mirrors issue 889's report-only shape and issue
    /// 861's caller-only decoupling): the `frozen_leg` (issue 758) and `self_heal_reset`
    /// CLASSIFIERS themselves are report-only annotations layered on TOP of the underlying
    /// per-window `copies`/`gaps`/`undecodable` data -- their own `gates_overall_pass` JSON
    /// fields must read `false`, always. **2026-08-05 RE-GATE (ticket 889 comment 5196190653):**
    /// this fixture's "genuinely HARD-FROZEN" window is built from `copies=5` (density-based,
    /// see the `frozen` block below) -- FAR over the per-window tolerance (recalibrated 1 → 2 → 3
    /// on 2026-08-06, ticket 889 comments 5198131539 / 5200533407:
    /// `crate::window_gate::WINDOW_COPIES_GAPS_TOLERANCE`), so the UNDERLYING data now
    /// correctly fails `overall_pass` again via the re-gate, independent of whatever
    /// `frozen_leg`/`self_heal_reset` report. Renamed from `..._no_longer_gate_the_overall_
    /// verdict_914` -- that claim is no longer true for a window this badly frozen; the
    /// re-gate is SUPPOSED to catch exactly this class of regression. What issue 914 actually
    /// established (the classifiers' OWN report-only status) is unchanged and still asserted.
    #[test]
    fn frozen_leg_classifier_stays_report_only_but_its_copies_now_gate_overall_pass_889_regate() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;

        const ONE_S: i64 = 1_000_000_000;
        let base = 1_000 * ONE_S;
        let win = 5 * ONE_S;
        let sched = format!(
            r#"[{{"cambox":"CAM1","start_ns":{a},"end_ns":{b}}}]"#,
            a = base,
            b = base + win
        );

        fn build_fixture(
            tag: &str,
            base: i64,
            sched: &str,
            frozen: bool,
            self_heal_reset: Option<&str>,
        ) -> serde_json::Value {
            build_fixture_with_min_secs(tag, base, sched, frozen, self_heal_reset, None)
        }

        // Finding 3 of the issue-889 re-gate deep review: the plain `build_fixture` above always
        // uses the default `--min-secs` (300), so its fixtures' TOP-LEVEL `overall_pass` is
        // permanently confounded by the unrelated full_chain 300s span floor (this synthetic
        // recording spans ~2s) -- true regardless of self_heal_reset/frozen_leg's own gating, per
        // the comment at the `clean`/`with_events` assertions below. `min_secs` lets a caller
        // clear that confound (mirrors the `--min-secs 1` idiom already used throughout this
        // file's test suite) so a TOP-LEVEL `overall_pass` differential can actually isolate a
        // SPECIFIC term's contribution instead of always reading false regardless of it.
        fn build_fixture_with_min_secs(
            tag: &str,
            base: i64,
            sched: &str,
            frozen: bool,
            self_heal_reset: Option<&str>,
            min_secs: Option<&str>,
        ) -> serde_json::Value {
            const ONE_S: i64 = 1_000_000_000;
            let dir = std::env::temp_dir().join(format!("cb-914-{tag}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let sched_path = dir.join("switch-schedule.json");
            std::fs::write(&sched_path, sched).unwrap();

            let mut stream_frames: Vec<RecordingFrame> = Vec::new();
            for i in 0..20u64 {
                let gen_ts = base + (i as i64 + 1) * (ONE_S / 10);
                let optical = 1000u32 + 2 * i as u32; // clean step-2 sequence, undecodable=0
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
            if frozen {
                // 5 consecutive duplicates of index 9's tick (1018), inserted right after index
                // 9 -- `copies=5` against `frames=25` -> density 0.20, ABOVE
                // `frozen_leg::FROZEN_DENSITY_THRESHOLD` (0.10) -> genuinely HARD-FROZEN (not
                // merely stale_replay, whose isolated-copy allowance is also 5 -- this proves
                // DENSITY, not just count, is what trips it here). The real present-tick
                // sequence around the duplicates stays perfectly contiguous (step 2), isolating
                // this fixture's ONLY defect to the copies/frozen classification.
                for k in 0..5u64 {
                    let dup_gen_ts = base + 9 * (ONE_S / 10) + (k as i64 + 1) * (ONE_S / 1000);
                    stream_frames.insert(
                        10 + k as usize,
                        RecordingFrame {
                            frame_index: 9_000 + k,
                            payloads: vec![
                                Payload {
                                    run_id: STRIH,
                                    frame_id: 9_670 + k as u32,
                                    gen_ts_ns: dup_gen_ts,
                                },
                                Payload {
                                    run_id: CAM2,
                                    frame_id: 1018, // == index 9's optical tick
                                    gen_ts_ns: dup_gen_ts,
                                },
                            ],
                            tick: Some(1018),
                        },
                    );
                }
            }

            let mut argv = vec![
                "recording-verdict".to_string(),
                "--switch-schedule".to_string(),
                sched_path.to_str().unwrap().to_string(),
                "--switch-guard-ns".to_string(),
                "0".to_string(),
                "--switch-expected-step".to_string(),
                "2".to_string(),
            ];
            if let Some(tok) = self_heal_reset {
                argv.push("--self-heal-reset".to_string());
                argv.push(tok.to_string());
            }
            if let Some(ms) = min_secs {
                argv.push("--min-secs".to_string());
                argv.push(ms.to_string());
            }
            let args = super::Args::parse_from(argv);
            let (v, _) = build_and_print_verdict(
                &args,
                None,
                Some(DecodedRec {
                    frames: stream_frames,
                    rec_path: None,
                }),
                Cam1Source::Absent,
                None,
                None,
                None,
                None,
            )
            .expect("verdict");
            let _ = std::fs::remove_dir_all(&dir);
            v
        }

        let clean = build_fixture("clean", base, &sched, false, None);
        // "CAM_UNRELATED" never appears in any schedule window in this fixture, so this event
        // can never correlate to a classified window -- guaranteed `unattributed_events`,
        // independent of the frozen fixture on CAM1.
        let with_events = build_fixture(
            "frozen",
            base,
            &sched,
            true,
            Some("CAM_UNRELATED:1000000000000"),
        );

        assert_eq!(
            clean["frozen_leg"]["frozen"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0),
            0,
            "sanity: the clean fixture must have no frozen windows: {clean}"
        );
        assert_eq!(
            with_events["frozen_leg"]["frozen"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0),
            1,
            "sanity: this fixture must genuinely classify one HARD-FROZEN window: {with_events}"
        );
        assert_eq!(
            with_events["self_heal_reset"]["unattributed_events"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0),
            1,
            "sanity: the self-heal event on an unrelated cambox must stay unattributed: {with_events}"
        );
        assert_eq!(
            with_events["frozen_leg"]["gates_overall_pass"],
            serde_json::json!(false),
            "914: the JSON must say plainly this term does not gate overall_pass: {with_events}"
        );
        assert_eq!(
            with_events["self_heal_reset"]["gates_overall_pass"],
            serde_json::json!(false),
            "914: same for self_heal_reset: {with_events}"
        );
        // 889 re-gate: the fixture's own copies=5 (against frames=25, density 0.20 -- the exact
        // shape the frozen_leg classifier needs to prove DENSITY, not just count) is FAR over the
        // tolerance (recalibrated 1 → 2 → 3 on 2026-08-06), so this window (and therefore
        // `all_cambox_continuity.overall_pass` specifically) now correctly FAILS again -- this is
        // the re-gate doing its job, not frozen_leg/self_heal_reset (which stay report-only, per
        // the two assertions immediately above). Scoped to `all_cambox_continuity.overall_pass`,
        // NOT the top-level `overall_pass` -- this short synthetic fixture (a few seconds) always
        // fails the UNRELATED `full_chain` 300s duration floor regardless of copies/gaps, so the
        // top-level field is not a valid proxy for what this test is isolating (the same reason
        // the ORIGINAL 914 test used a differential rather than an absolute assertion).
        assert_eq!(
            clean["all_cambox_continuity"]["overall_pass"],
            serde_json::json!(true),
            "sanity: the clean fixture's all_cambox_continuity has no defects at all: {clean}"
        );
        assert_eq!(
            with_events["all_cambox_continuity"]["overall_pass"],
            serde_json::json!(false),
            "889 re-gate: copies=5 far exceeds the tolerance -- \
             all_cambox_continuity.overall_pass must FAIL again, even though \
             frozen_leg/self_heal_reset themselves stay report-only: {with_events}"
        );
        assert_eq!(
            with_events["all_cambox_continuity"]["windows_over_copies_gaps_tolerance"],
            serde_json::json!(1),
            "889 re-gate: exactly the frozen window exceeds the tolerance: {with_events}"
        );

        // Finding 3 of the issue-889 re-gate deep review: everything above proves frozen_leg/
        // self_heal_reset's OWN `gates_overall_pass` JSON field reads `false` (a hardcoded
        // literal, see the `obj.insert("gates_overall_pass", ...)` call sites in
        // `build_and_print_verdict` -- disconnected from `SelfHealAttributionReport::
        // overall_pass_contribution()`, the function that ACTUALLY decides whether these terms
        // fold into `all_pass`) -- but nothing above would catch a regression in that function
        // itself, because `with_events`'s copies=5 defect ALREADY fails `all_cambox_continuity.
        // overall_pass` on its own (the re-gate), confounding any differential at that level; the
        // TOP-LEVEL `overall_pass` differential the ORIGINAL (pre-re-gate) #914 test used is
        // ALSO confounded, but by something else entirely (the >=300s `full_chain` span floor,
        // which fails unconditionally on this ~2s synthetic recording regardless of self-heal).
        //
        // The frozen term is no longer isolable on its own (a genuinely HARD-FROZEN window needs
        // `copies` far past the tolerance, which by itself already fails
        // `all_cambox_continuity.overall_pass`). `self_heal_reset` IS still isolable: build a
        // fixture IDENTICAL to a clean baseline except for ONE unattributed self-heal event, with
        // the confounding span floor cleared via `--min-secs 0` (the fixture's `strih` node
        // analyzed span, 20 ids at the default 30fps `--stream-capture-fps`, is only ~0.667s --
        // even `--min-secs 1` still fails it; `--min-secs 0` removes the floor entirely, which is
        // all this differential needs: it is isolating self_heal_reset's own contribution, not
        // proving anything about the span floor itself) so the top-level `overall_pass`
        // differential actually isolates `self_heal_reset`'s own decoupling instead of always
        // reading `false` regardless of it.
        let clean_isolated =
            build_fixture_with_min_secs("clean-isolated", base, &sched, false, None, Some("0"));
        let self_heal_only = build_fixture_with_min_secs(
            "self-heal-only",
            base,
            &sched,
            false,
            Some("CAM_UNRELATED:1000000000000"),
            Some("0"),
        );
        assert_eq!(
            self_heal_only["frozen_leg"]["frozen"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0),
            0,
            "sanity: this isolated fixture carries NO frozen defect -- its ONLY difference from \
             clean_isolated is the self-heal event: {self_heal_only}"
        );
        assert_eq!(
            self_heal_only["self_heal_reset"]["unattributed_events"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0),
            1,
            "sanity: the isolated fixture carries exactly one unattributed self-heal event: \
             {self_heal_only}"
        );
        // Sanity: with the span-floor confound cleared, the clean baseline's TOP-LEVEL
        // `overall_pass` genuinely reads `true` -- proving the differential below is not
        // tautologically comparing two permanently-`false` values (which would prove nothing).
        assert_eq!(
            clean_isolated["overall_pass"],
            serde_json::json!(true),
            "sanity: with --min-secs 1 clearing the span-floor confound, the isolated clean \
             fixture's top-level overall_pass must genuinely read true: {clean_isolated}"
        );
        assert_eq!(
            clean_isolated["overall_pass"], self_heal_only["overall_pass"],
            "914 regression guard: an unattributed self-heal event ALONE (no frozen defect, no \
             copies/gaps defect) must be a no-op on the TOP-LEVEL overall_pass (report-only, \
             pending cam1 hardware fix issue 909 -- restore path issue 905). Unlike the \
             `with_events` fixture above (confounded by its own copies=5 re-gate failure), this \
             differential genuinely isolates self_heal_reset's contribution: \
             clean_isolated={clean_isolated}, self_heal_only={self_heal_only}"
        );
    }

    /// #467 — extend the #312 ALL-CAMBOX `--switch-schedule` sweep to ALSO gate imag-nb's OWN
    /// per-segment continuity. imag's frames are placed onto the SAME schedule timeline (anchored
    /// on its #463 digital corner burn, [`super::BURN_RUN_ID_IMAG`]) and its own painted-tick
    /// continuity — at its OWN native rate (step 1, never the stream recording's 60->30 step 2) —
    /// must ALSO be computed + surfaced under `all_cambox_continuity.imag`, alongside (not instead
    /// of) the existing per-cambox windows. (issue 798 path A: this imag term now folds
    /// REPORT-ONLY into `overall_pass` — a clean imag segment, as here, still reports
    /// `overall_pass: true`; the report-only fold only changes whether a FAILING imag segment reds
    /// the run, which the sibling `imag_own_segment_gap_is_surfaced_report_only_798` covers.)
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
    fn imag_own_segment_gap_is_surfaced_report_only_798() {
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
            "#467: a genuine gap in imag's OWN segment must still be DETECTED — imag's own verdict FAILs: {imag_seg}"
        );
        // issue 798 (path A) -> #1142: the imag per-segment continuity is a PER-FRAME CONTENT term,
        // so it now folds through the REPORT-ONLY CONTENT seam (content_gates_overall_pass) — its
        // FAIL is surfaced (above) with a LOUD scoped flag, but does NOT gate the OVERALL verdict
        // (confounded by the issue 1130 observer effect, pending the issue 1143 encoder fix). The
        // imag PRESENCE/VERIFICATION terms are separately BLOCKING. The per-cambox (stream) sweep's
        // own fold is UNTOUCHED and stays blocking (asserted clean below).
        assert_eq!(
            imag_seg["gates_overall_pass"],
            serde_json::json!(false),
            "#1142: imag's per-segment continuity is a REPORT-ONLY content term — it does NOT gate overall_pass: {imag_seg}"
        );
        assert_eq!(
            v["all_cambox_continuity"]["overall_pass"],
            serde_json::json!(true),
            "sanity: the existing per-cambox (stream) sweep is completely untouched and stays clean: {v}"
        );
        // `pass` is intentionally not asserted: the report-only FOLD semantics (a failing imag
        // segment no longer reds overall_pass) are proven in the Tier-0 `tests/imag_leg_gate.rs`,
        // and this synthetic fixture's overall verdict also runs the unrelated #373 span gate.
        let _ = pass;

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
    // (`camera_box::switch_latency::spread_verdict`): `max(p50) - min(p50) >
    // SPREAD_THRESHOLD_MS` (24ms since issue 1120; was a 16ms half-frame) = FAIL — a differing
    // photon->dequeue latency `d_X` per camera (#286's root cause) beyond that bound can visibly
    // break A/V lipsync when the live program cuts between them.

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
    /// matches its injected latency, and a genuinely-broken 55ms cross-camera spread (well over
    /// the recalibrated 24ms bound — issue 1120 — and over a full 33.3ms program frame) FAILS
    /// the gate.
    #[test]
    fn all_cambox_latency_measures_per_camera_windowed_latency_and_fails_a_wide_spread_624() {
        let v = build_all_cambox_latency_fixture(
            "fail",
            &[
                ("CAM1", CAM1B, 800_000_000),
                ("CAM3", super::BURN_RUN_ID_CAM3, 850_000_000),
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
            serde_json::json!(850.0),
            "#624: cam3's OWN windowed cam2->cam3 latency (generalized from cam1-only): {lat}"
        );
        assert_eq!(
            lat["cam4"]["p50_ms"],
            serde_json::json!(795.0),
            "#624: cam4's OWN windowed cam2->cam4 latency (generalized from cam1-only): {lat}"
        );
        assert_eq!(
            lat["cross_camera_spread_ms"],
            serde_json::json!(55.0),
            "#624: max(850) - min(795) = 55ms: {lat}"
        );
        assert_eq!(
            lat["spread_gate_pass"],
            serde_json::json!(false),
            "#1120: a 55ms cross-camera spread is well over the 24ms bound -> the gate must FAIL: \
             {lat}"
        );
    }

    /// #624: the SAME 3-camera shape, but every camera's injected latency sits within the spread
    /// bound of every other's -> the spread gate PASSES.
    #[test]
    fn all_cambox_latency_spread_within_bound_passes_the_gate_624() {
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
            "#624: a 10ms cross-camera spread clears the 24ms bound -> PASS: {lat}"
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
                ("CAM2", CAM2B, 30_000_000),
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
            serde_json::json!(30.0),
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
            serde_json::json!(27.0),
            "#286: max(30.0) - min(3.0) = 27ms: {lat}"
        );
        assert_eq!(
            lat["spread_gate_pass"],
            serde_json::json!(false),
            "#1120: a 27ms delivery spread is over the (now BLOCKING, #1142) 24ms bound -> FAIL: {lat}"
        );
    }

    /// #286: ALL SIX cameras measured (including cam2), each within the spread bound of every
    /// other's injected delivery latency -> the (blocking since #1142) spread gate PASSES — mirrors a
    /// successfully phase-synced rig where the applied differentiated genlock-latency offsets
    /// have collapsed every camera's DELIVERY latency to roughly the same value.
    #[test]
    fn all_cambox_delivery_latency_all_six_cameras_within_bound_passes_the_gate_286() {
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
            "#286: a 4ms delivery spread across all 6 cameras clears the 24ms bound -> PASS: \
             {lat}"
        );
    }

    /// #286: a tight delivery spread (all within the spread bound) PASSES the gate (blocking since #1142).
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
            "#286: a 7ms delivery spread clears the 24ms bound -> PASS: {lat}"
        );
    }

    /// #1033 -> #1142: the delivery cross-camera spread FOLDS into `overall_pass` through the
    /// `delivery_spread_gate` seam, now BLOCKING (`gates_overall_pass()==true`, owner mandate
    /// 2026-08-19). It shipped report-only under issue 1033 (the fleet was not tight-green), but the
    /// "green" runs it passed were FALSELY green — the phase lottery (3.97 vs 85 ms) hid a real
    /// delivery-spread failure — so #1142 flips it LIVE. Pinned BOTH directions via the pure fold at
    /// the wiring, plus the surfaced `gates_overall_pass` JSON field. (The Tier-0
    /// `tests/delivery_spread_gate.rs` pins the seam's pure contract; this pins it AT the wiring.)
    #[test]
    fn all_cambox_delivery_latency_spread_folds_blocking_since_1142() {
        // #1142 — the seam is BLOCKING: a wide delivery spread now REDs a run.
        assert!(
            camera_box::delivery_spread_gate::gates_overall_pass(),
            "#1142: the delivery-spread seam must be BLOCKING (gates_overall_pass()==true)"
        );

        let v = build_all_cambox_delivery_latency_fixture(
            "seam",
            &[("CAM1", CAM1B, 3_000_000), ("CAM2", CAM2B, 30_000_000)],
        );
        let lat = &v["all_cambox_delivery_latency"];
        assert_eq!(
            lat["spread_gate_pass"],
            serde_json::json!(false),
            "sanity: this fixture's 27ms spread must FAIL the 24ms bound for the point of this \
             test to hold: {lat}"
        );
        // #1142 — the block now surfaces the LIVE seam state so the report consumer + a miner can
        // tell blocking from report-only.
        assert_eq!(
            lat["gates_overall_pass"],
            serde_json::json!(true),
            "#1142: the delivery block must surface the BLOCKING seam state: {lat}"
        );

        // Re-pin the seam's BLOCKING contract at the call site, on the EXACT function the wiring
        // above invokes (`folds_into_overall_pass`): for this fixture's failing `sv.pass == false`,
        // the blocking seam now folds to FAIL (a wide spread REDs the run). This does not assert on
        // the fixture's own `overall_pass` (the minimal 2-window recording reds for unrelated
        // reasons too, e.g. the #373 duration floor) — the pure seam contract is verified here, with
        // `tests/delivery_spread_gate.rs` covering it Tier-0 as well.
        assert!(
            !camera_box::delivery_spread_gate::folds_into_overall_pass(false),
            "#1142 blocking: a FAILING delivery spread must fold to FAIL while the seam is LIVE"
        );
        assert!(
            camera_box::delivery_spread_gate::folds_into_overall_pass(true),
            "a TIGHT delivery spread passes"
        );
        // The report-only fold direction stays pinned (a hypothetical revert): fold(_, false) passes.
        assert!(
            camera_box::delivery_spread_gate::fold(false, false),
            "report-only direction: a wide spread would not red if the seam were reverted"
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
        build_all_cambox_av_sync_fixture_with_ack(tag, cameras, av, av_expected_ms, "", None)
    }

    /// #855 — same as [`build_all_cambox_av_sync_fixture`], plus a `--offline-ack-cams` value
    /// (the raw `CAMBOX_OFFLINE_ACK`-format string) threaded through to the CLI args, so a test
    /// can prove an acked-offline camera is reported EXCLUDED rather than judged. `#861` adds an
    /// optional `--cam1-capture-stats` sidecar path — a real, TOP-LEVEL, unconditional zero-loss
    /// signal (`cam2_present -> capture-drop`, `all_pass &= capture_zero`), completely independent
    /// of the switch-schedule/AV plumbing this fixture otherwise builds — so a test can inject a
    /// genuine zero-loss defect that must still fail `overall_pass` regardless of the (now
    /// report-only) A/V-offset term.
    fn build_all_cambox_av_sync_fixture_with_ack(
        tag: &str,
        cameras: &[(&str, u32, i64)],
        av: Option<AvMarkerInputs>,
        av_expected_ms: f64,
        offline_ack_cams: &str,
        cam1_capture_stats: Option<&std::path::Path>,
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

        let mut argv: Vec<String> = vec![
            "recording-verdict".to_string(),
            "--switch-schedule".to_string(),
            sched_path.to_str().unwrap().to_string(),
            "--switch-guard-ns".to_string(),
            "0".to_string(),
            "--switch-expected-step".to_string(),
            "2".to_string(),
            "--av-expected-ms".to_string(),
            av_expected_ms.to_string(),
            "--offline-ack-cams".to_string(),
            offline_ack_cams.to_string(),
            // Review finding on PR #1002 (issue 861 vacuity controls): without this, every
            // fixture's sub-300s synthetic recording fails `span_ok` and forces
            // `overall_pass=false` REGARDLESS of the gate under test -- no fixture-based
            // overall_pass assertion in this file had any bite. Same convention the
            // Args-based unit tests in this file already use (theirs pass 1; these
            // single-window fixtures analyze only ~0.7s, so 0 is the honest floor here).
            "--min-secs".to_string(),
            "0".to_string(),
        ];
        if let Some(stats_path) = cam1_capture_stats {
            argv.push("--cam1-capture-stats".to_string());
            argv.push(stats_path.to_str().unwrap().to_string());
        }
        let args = super::Args::parse_from(argv);

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
    /// cam1's real 500ms offset is far outside the tolerance bound (FAILS the gate despite being a
    /// real, clean measurement — the gate is about closeness to expected, not data quality), and
    /// every Unknown camera fails closed too. **#861 (2026-08-06): this term is BLOCKING again**
    /// (re-armed after ASRC #803 proved stable) — it measures/fails closed exactly as before, and
    /// now ALSO decides `overall_pass` (see
    /// `all_cambox_av_sync_gate_failure_forces_the_overall_verdict_to_fail_861_rearmed` for that
    /// proof).
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
            audio_preamble_screens_passed: audio_markers.len() as u64,
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
            "#624: 500ms is far outside the tolerance bound of the default expected_ms=0: {av_sync}"
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
        // #861 (2026-08-06, re-armed after ASRC #803 proved stable): the gate is BLOCKING again —
        // the string must say so, and an explicit machine-readable flag says it DOES decide
        // `overall_pass` (see
        // `all_cambox_av_sync_gate_failure_forces_the_overall_verdict_to_fail_861_rearmed` below
        // for the actual overall_pass proof).
        assert!(
            !av_sync["gate"].as_str().unwrap().contains("report-only"),
            "#861: the gate string must NOT say report-only anymore (re-armed): {av_sync}"
        );
        assert_eq!(
            av_sync["gates_overall_pass"],
            serde_json::json!(true),
            "#861: the JSON must be unambiguous that this term decides overall_pass again: {av_sync}"
        );
    }

    /// #861 (2026-08-06, re-armed after ASRC #803 proved stable) — a FAILING av_sync gate must
    /// FORCE the run's overall verdict to fail again: `all_pass &= av_all_pass ||
    /// !av_window::gates_overall_pass()` is back in the caller. This INVERTS
    /// `all_cambox_av_sync_gate_failure_no_longer_forces_the_overall_verdict_to_fail_861` (the
    /// 2026-07-29 report-only proof this test replaces) and restores the ORIGINAL pre-#861
    /// invariant `all_cambox_av_sync_gate_failure_forces_the_overall_verdict_to_fail_312_624` once
    /// asserted (git a30c8f53a). A silent revert of `av_window::gates_overall_pass()` back to
    /// report-only would make this test FAIL loudly (see that function's own regression test,
    /// `gates_overall_pass_is_blocking_again_861`, for the companion guard at the pure-decision
    /// layer).
    ///
    /// VACUITY GUARD (review finding on this PR): the fixture's deliveries MUST keep the
    /// cross-camera spread within `switch_latency::SPREAD_THRESHOLD_MS` (an earlier 800/820ms
    /// pair had spread 20, over the 16 ms bound in force when this guard was written — issue 1120
    /// later recalibrated the bound to 24 ms — so `all_pass &= sv.pass` already forced
    /// `overall_pass=false` BEFORE the A/V fold ran, which would have left this test green even
    /// with the fold deleted). The CURRENT 800/810 pair (spread 10 ms, well within the bound)
    /// avoids that: the `without_av` control proves every OTHER gate passes on this exact fixture,
    /// so the failing A/V fold is the ONLY thing that can (and must) flip the with-av verdict.
    #[test]
    fn all_cambox_av_sync_gate_failure_forces_the_overall_verdict_to_fail_861_rearmed() {
        let emit_log: Vec<(u8, u32, i64)> = (0..10u8).map(|k| (k, 1000 + k as u32, 0)).collect();
        let audio_markers: Vec<(f64, u8)> = (0..10u8).map(|k| (k as f64 / 30.0 - 0.5, k)).collect();
        let av = AvMarkerInputs {
            fps: 30.0,
            video_start_s: 0.0,
            emit_log,
            audio_preamble_screens_passed: audio_markers.len() as u64,
            audio_markers,
        };
        // Spread = 10ms <= SPREAD_THRESHOLD_MS (24ms) -> the cross-camera spread gate PASSES; see
        // the vacuity guard in the doc comment above.
        let cameras: &[(&str, u32, i64)] =
            &[("CAM1", CAM1B, 800_000_000), ("CAM3", CAM3B, 810_000_000)];

        // cam1 measures a real 500ms offset; expected_ms=0 puts it far outside the tolerance ->
        // the av_sync gate FAILS (cam3/cam4/cam5/cam6 are Unknown too, doubly so).
        let with_av =
            build_all_cambox_av_sync_fixture("gate-fail-with-861-rearmed", cameras, Some(av), 0.0);
        let without_av =
            build_all_cambox_av_sync_fixture("gate-fail-without-861-rearmed", cameras, None, 0.0);

        assert_eq!(
            without_av["overall_pass"],
            serde_json::json!(true),
            "vacuity control: with NO A/V inputs this exact fixture must pass every other gate \
             (loss, latency, spread) — otherwise the with-av assertion below proves nothing \
             about the A/V fold: {without_av}"
        );
        assert!(
            !with_av["all_cambox_av_sync"].is_null(),
            "sanity: the WITH-av_sync run must actually have reported the block: {with_av}"
        );
        assert_eq!(
            with_av["all_cambox_av_sync"]["gate_pass"],
            serde_json::json!(false),
            "sanity: the av_sync gate must actually be failing in this fixture: {with_av}"
        );
        assert_eq!(
            with_av["all_cambox_av_sync"]["gates_overall_pass"],
            serde_json::json!(true),
            "#861: the JSON must say plainly that this failing term DOES gate again: {with_av}"
        );
        assert_eq!(
            with_av["overall_pass"],
            serde_json::json!(false),
            "#861: a failing av_sync gate MUST force overall_pass=false again, regardless of \
             whatever the loss/latency gates alone computed: {with_av}"
        );
    }

    /// #861 — zero-loss enforcement is completely UNAFFECTED by the A/V-offset term's own
    /// report-only-vs-blocking state (whichever it currently is): a real zero-loss defect (a
    /// cam2→SOURCE V4L2 capture-drop, `--cam1-capture-stats`) still forces `overall_pass=false`,
    /// unconditionally. This gate (`all_pass &= within_band` in `recording-verdict.rs`, where
    /// `within_band = camleg_capture_band(v4l2_dropped, allowance)`) is TOP-LEVEL — parsed and
    /// applied whenever `--cam1-capture-stats` is supplied, with NO dependency on
    /// `--switch-schedule` / `--stream` / the A/V-sync plumbing at all. The A/V inputs here are the
    /// same clean 500ms fixture used by the sibling PASS test (does not matter either way — the
    /// point is the capture-drop alone). `v4l2_dropped=7` is deliberately WAY OVER the #1169
    /// `CAMLEG_V4L2_DROP_ALLOWANCE_DEFAULT` (=2) singleton band, so it stays a hard FAIL (the band
    /// only absorbs a `<=2` singleton — see the `..._absorbs_two_drops_...` sibling below).
    #[test]
    fn zero_loss_capture_drop_still_fails_overall_pass_regardless_of_av_gate_861() {
        let emit_log: Vec<(u8, u32, i64)> = (0..10u8).map(|k| (k, 1000 + k as u32, 0)).collect();
        let audio_markers: Vec<(f64, u8)> = (0..10u8)
            .map(|k| (k as f64 / 30.0 - 0.5, k)) // video_ts(1000+k) - 0.5s = k/30.0 - 0.5
            .collect();
        let av = AvMarkerInputs {
            fps: 30.0,
            video_start_s: 0.0,
            emit_log,
            audio_preamble_screens_passed: audio_markers.len() as u64,
            audio_markers,
        };
        let cameras: &[(&str, u32, i64)] = &[("CAM1", CAM1B, 800_000_000)];

        let dir = std::env::temp_dir().join(format!("cb-861-capture-drop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let stats_path = dir.join("cam1-capture-stats.txt");
        std::fs::write(&stats_path, "v4l2_dropped=7\nframes_captured=1000\n").unwrap();

        // av_expected_ms=500.0 matches the constructed clean 500ms offset -- the A/V term PASSES
        // cleanly here (unlike the sibling FAIL test above); the only defect in this run is the
        // injected capture-drop.
        let v = build_all_cambox_av_sync_fixture_with_ack(
            "zero-loss-capture-drop-861",
            cameras,
            Some(av),
            500.0,
            "",
            Some(&stats_path),
        );
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(
            v["full_chain"]["loss"]["cam2_cam1"]["zero_loss"],
            serde_json::json!(false),
            "sanity: the injected capture-drop must actually be non-zero in this fixture: {v}"
        );
        assert_eq!(
            v["overall_pass"],
            serde_json::json!(false),
            "#861: a real zero-loss defect (camera-leg V4L2 capture drop) must still force \
             overall_pass=false -- completely unaffected by the A/V-offset term's own \
             report-only-vs-blocking state: {v}"
        );
    }

    /// #1169 THIRD SEAM (owner, 2026-08-22) — the cam-leg V4L2 capture-drop counter
    /// (`full_chain.loss.cam2_*`, the last binding `all_pass &= …` red) gets the SAME loud
    /// singleton band the two prior seams gave the presented + burn-delivery layers. A
    /// `v4l2_dropped` count WITHIN `CAMLEG_V4L2_DROP_ALLOWANCE_DEFAULT` (=2) is an UPSTREAM
    /// camera-leg buffer drop the issue-1167 emit-fill absorbs by design (the first full verdict
    /// of the series showed exactly `v4l2_dropped:2` over `frames_captured:35961` = 0.0056%, while
    /// `full_chain.zero_loss` + `all_cambox_continuity.overall_pass` were already green) — so it
    /// must PASS `overall_pass` with `zero_loss=true` + a LOUD `note`, NEVER a silent green. This
    /// is the exact `gate-allowance-restore-red-green.md` shape, third instance; issue 1169 stays
    /// OPEN as the re-tighten trail (the DEFAULT flips back to 0 once a zero-singleton green run
    /// holds).
    ///
    /// FIXTURE (issue 1169 CI fix): the `--cam1-capture-stats` gate is TOP-LEVEL, but an
    /// `overall_pass=true` assertion needs EVERY other fold term green too — and the av-sync
    /// fixture (`build_all_cambox_av_sync_fixture_with_ack`) can never deliver that with ONE
    /// scheduled camera: its BLOCKING `all_cambox_av_sync` gate fails closed on the
    /// absent-unacked cam3..cam7 (issue 855 / issue 861), so the old shape read
    /// `overall_pass=false` for a reason UNRELATED to the band under test. Mirrors the
    /// otherwise-green `single_real_drop_passes_loudly_within_the_1169_singleton_allowance`
    /// sibling's fixture instead (clean `window` frames, no schedule, no A/V inputs) — the
    /// injected capture-drop sidecar is the ONLY non-zero signal, so the camleg band is the
    /// ONLY term that can flip the verdict.
    #[test]
    fn camleg_v4l2_singleton_band_absorbs_two_drops_into_overall_pass_1169() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;
        const N: u32 = 600;

        let dir = std::env::temp_dir().join(format!("cb-1169-camleg-band-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let stats_path = dir.join("cam1-capture-stats.txt");
        // exactly TWO cam-leg V4L2 capture drops — the sanctioned singleton band (<=2).
        std::fs::write(&stats_path, "v4l2_dropped=2\nframes_captured=35961\n").unwrap();

        let args = super::Args::parse_from([
            "recording-verdict",
            "--min-secs",
            "1",
            "--capture-fps",
            "60",
            "--cam1-capture-stats",
            stats_path.to_str().unwrap(),
        ]);
        // Fully clean recordings (ZERO real drops, contiguous burns, span green at min-secs 1):
        // every other fold term passes, exactly like the real_drops singleton sibling.
        let (v, pass) = build_and_print_verdict(
            &args,
            Some(DecodedRec {
                frames: window(N, false, None),
                rec_path: None,
            }),
            Some(DecodedRec {
                frames: window(N, true, None),
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            None, // no imag frames in this test
            None, // no carried A/V-sync inputs in this test
        )
        .expect("verdict");
        let _ = std::fs::remove_dir_all(&dir);

        // The band ABSORBS the 2 drops: the node reads zero_loss=true (within the band) ...
        assert_eq!(
            v["full_chain"]["loss"]["cam2_cam1"]["zero_loss"],
            serde_json::json!(true),
            "#1169: 2 V4L2 capture drops are WITHIN the singleton band ⇒ zero_loss stays true: {v}"
        );
        // ... yet it is LOUD, never a silent green: the band was CONSUMED + carries the note ...
        assert_eq!(
            v["full_chain"]["loss"]["cam2_cam1"]["camleg_singleton_band_consumed"],
            serde_json::json!(true),
            "#1169: a within-band NON-zero drop count must be marked as CONSUMED (loud): {v}"
        );
        assert_eq!(
            v["full_chain"]["loss"]["cam2_cam1"]["v4l2_dropped"],
            serde_json::json!(2),
            "#1169: the raw v4l2_dropped count stays honestly reported: {v}"
        );
        let note = v["full_chain"]["loss"]["cam2_cam1"]["note"]
            .as_str()
            .unwrap_or("");
        assert!(
            note.contains("cam-leg V4L2 singleton band consumed"),
            "#1169: the consumed band must carry the loud named note: {v}"
        );
        // ... and the whole run PASSES (this was the LAST binding red) — both the returned
        // all_pass fold and the emitted JSON headline agree.
        assert!(
            pass,
            "issue 1169: 2 capture-leg drops within the band must NOT fail the all_pass fold: {v}"
        );
        assert_eq!(
            v["overall_pass"],
            serde_json::json!(true),
            "#1169: 2 capture-leg drops within the band must NOT fail overall_pass: {v}"
        );
    }

    /// #1169 THIRD SEAM — the PURE band decision boundary + the compiled DEFAULT. Proven at the
    /// `camleg_capture_band` level (an explicit allowance, independent of process env) so the
    /// boundary stays regression-tested without mutating global env state. `<= allowance` is
    /// within-band; a within-band NON-zero count is `band_consumed` (the loud note); a strict zero
    /// is within-band but NOT consumed.
    #[test]
    fn camleg_capture_band_boundary_and_default_1169() {
        use super::{camleg_capture_band, CAMLEG_V4L2_DROP_ALLOWANCE_DEFAULT};
        // strict zero: within band, but NOT consumed (a clean pass, no loud note).
        assert_eq!(camleg_capture_band(0, 2), (true, false));
        // 1 and 2 drops: within the band, CONSUMED (loud note).
        assert_eq!(camleg_capture_band(1, 2), (true, true));
        assert_eq!(camleg_capture_band(2, 2), (true, true));
        // 3 drops: OVER the band ⇒ not within, not consumed ⇒ a hard fail.
        assert_eq!(camleg_capture_band(3, 2), (false, false));
        assert_eq!(
            CAMLEG_V4L2_DROP_ALLOWANCE_DEFAULT, 2,
            "#1169: the compiled DEFAULT must be the singleton band 2 (the re-tighten trail on \
             issue 1169 flips this one constant back to 0)"
        );
    }

    /// #1169 — DORMANT re-tighten proof: flipping the ONE constant back to 0 (this ticket's
    /// re-tighten trail, closed only by a zero-singleton green run) restores the STRICT bar.
    /// Proven at the pure-fn level with an EXPLICIT allowance of 0 (what
    /// `CAMLEG_V4L2_DROP_ALLOWANCE_DEFAULT = 0` yields), independent of the compiled default and of
    /// process env. Mirrors the `re_tightening_the_1169_allowance_to_zero_restores_the_strict_bar`
    /// sibling for the real-drops seam — the mechanism stays dormant, never deleted.
    #[test]
    fn re_tightening_the_camleg_v4l2_band_to_zero_restores_the_strict_bar() {
        use super::camleg_capture_band;
        // At the DEFAULT band (2) the 2-drop singleton passes on slack, LOUDLY (consumed) ...
        assert_eq!(camleg_capture_band(2, 2), (true, true));
        // ... and re-tightening the ONE constant back to 0 restores the strict zero-drop bar:
        assert_eq!(
            camleg_capture_band(2, 0),
            (false, false),
            "#1169: re-tightening to 0 restores the strict bar for the same 2-drop count"
        );
        // a genuine zero still passes cleanly at the strict bar (within-band, not consumed).
        assert_eq!(camleg_capture_band(0, 0), (true, false));
    }

    /// #1169 THIRD SEAM — the OVER-band end-to-end fold: 3 cam-leg V4L2 capture drops are OVER the
    /// `<=2` singleton band, so the node stays `zero_loss=false` and the whole run FAILS
    /// `overall_pass` (the band never becomes an open door). Same otherwise-green clean-`window`
    /// fixture as the `..._absorbs_two_drops_...` sibling above (issue 1169 CI fix — the old
    /// av-sync fixture left the blocking av-sync gate red on absent-unacked cameras, so the
    /// asserted `overall_pass=false` was overdetermined and proved nothing about the band): here
    /// every OTHER term is green, so the over-band drop count is what fails the run.
    #[test]
    fn camleg_v4l2_three_drops_over_band_still_fails_overall_pass_1169() {
        use super::{build_and_print_verdict, Cam1Source, DecodedRec};
        use clap::Parser;
        const N: u32 = 600;

        let dir = std::env::temp_dir().join(format!("cb-1169-camleg-over-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let stats_path = dir.join("cam1-capture-stats.txt");
        // THREE cam-leg V4L2 capture drops — one past the singleton band (>2).
        std::fs::write(&stats_path, "v4l2_dropped=3\nframes_captured=35961\n").unwrap();

        let args = super::Args::parse_from([
            "recording-verdict",
            "--min-secs",
            "1",
            "--capture-fps",
            "60",
            "--cam1-capture-stats",
            stats_path.to_str().unwrap(),
        ]);
        let (v, pass) = build_and_print_verdict(
            &args,
            Some(DecodedRec {
                frames: window(N, false, None),
                rec_path: None,
            }),
            Some(DecodedRec {
                frames: window(N, true, None),
                rec_path: None,
            }),
            Cam1Source::Absent,
            None,
            None,
            None, // no imag frames in this test
            None, // no carried A/V-sync inputs in this test
        )
        .expect("verdict");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            !pass,
            "issue 1169: 3 capture-leg drops (over the band) must fail the all_pass fold: {v}"
        );

        assert_eq!(
            v["full_chain"]["loss"]["cam2_cam1"]["zero_loss"],
            serde_json::json!(false),
            "#1169: 3 V4L2 capture drops are OVER the singleton band ⇒ zero_loss stays false: {v}"
        );
        assert_eq!(
            v["full_chain"]["loss"]["cam2_cam1"]["camleg_singleton_band_consumed"],
            serde_json::json!(false),
            "#1169: an OVER-band count is a genuine fail, never a 'consumed' band: {v}"
        );
        assert_eq!(
            v["overall_pass"],
            serde_json::json!(false),
            "#1169: 3 capture-leg drops (over the band) must FAIL overall_pass: {v}"
        );
    }

    /// #624 deliverable 4 / #312 item 2 PR B (still true post-#861) — a PASSING av_sync gate
    /// (every one of the 6 CAMERA_UNDER_TEST_NODES measured cleanly within the tolerance of
    /// `--av-expected-ms`) must NOT change the run's overall verdict vs the identical run with no
    /// av_sync inputs at all — `all_pass &= (true || ...)` is a no-op REGARDLESS of whether the
    /// term is report-only or blocking (#861, re-armed 2026-08-06): a PASSING gate never changes
    /// the fold either way. Proves the wiring is a true AND-in (never accidentally forcing a PASS
    /// fixture to fail) together with the sibling gate-FAILURE test above, without needing to know
    /// the loss/latency gates' own PASS/FAIL value for this synthetic fixture (both runs share the
    /// identical frames/schedule, so their loss/latency contribution is identical either way).
    #[test]
    fn all_cambox_av_sync_gate_pass_does_not_change_the_overall_verdict_312_624() {
        // 6 windows (cam1/cam3/cam4/cam5/cam6/cam7 — #755), 20 frames each = 120 frames, optical
        // ticks 1000..=1119 contiguous across ALL windows. emit_log covers the FULL 1000..=1119
        // range so EVERY window (and cam2's whole-recording pool over all 120 frames) decodes a
        // dense, clean 500ms offset -- no Unknown camera anywhere.
        const N: usize = 120;
        let emit_log: Vec<(u8, u32, i64)> = (0..N).map(|k| (k as u8, 1000 + k as u32, 0)).collect();
        let audio_markers: Vec<(f64, u8)> = (0..N)
            .map(|k| (k as f64 / 30.0 - 0.5, k as u8)) // video_ts(1000+k) - 0.5s = k/30.0 - 0.5
            .collect();
        let av = AvMarkerInputs {
            fps: 30.0,
            video_start_s: 0.0,
            emit_log,
            audio_preamble_screens_passed: audio_markers.len() as u64,
            audio_markers,
        };
        // Spread = 815-800 = 15ms <= SPREAD_THRESHOLD_MS (24ms) — the earlier 790..820 fixture
        // (spread 30, still over the 24ms bound) made BOTH runs fail on the spread gate, so the
        // with==without equality below was
        // `false == false` and could not detect a wiring bug that wrongly ANDs a PASSING gate
        // (review finding on this PR). With every gate passing, `without_av` is genuinely true
        // and the equality has bite.
        let cameras: &[(&str, u32, i64)] = &[
            ("CAM1", CAM1B, 800_000_000),
            ("CAM3", CAM3B, 812_000_000),
            ("CAM4", super::BURN_RUN_ID_CAM4, 803_000_000),
            ("CAM5", CAM5B, 805_000_000),
            ("CAM6", CAM6B, 815_000_000),
            ("CAM7", CAM7B, 810_000_000),
        ];

        // expected_ms=500.0 matches the constructed clean offset exactly -> every camera passes.
        let with_av = build_all_cambox_av_sync_fixture("gate-pass-with", cameras, Some(av), 500.0);
        let without_av =
            build_all_cambox_av_sync_fixture("gate-pass-without", cameras, None, 500.0);

        assert_eq!(
            without_av["overall_pass"],
            serde_json::json!(true),
            "vacuity control: with NO A/V inputs this fixture must pass every gate outright — \
             otherwise the with==without equality below is false==false and proves nothing: \
             {without_av}"
        );

        assert!(
            !with_av["all_cambox_av_sync"].is_null(),
            "sanity: the WITH-av_sync run must actually have reported the block: {with_av}"
        );
        for cam in ["cam1", "cam2", "cam3", "cam4", "cam5", "cam6", "cam7"] {
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

    /// #855 — an operator-acknowledged offline box (CAMBOX_OFFLINE_ACK / rig-fleet.txt, threaded
    /// via `--offline-ack-cams`) must be reported EXCLUDED, never judged: cam5/cam6/cam7 never
    /// appear in this fixture's switch schedule at all (same "absent from the sweep" shape the
    /// #312/#624 test above already covers), but are ACKED here -- so instead of the previous
    /// fail-closed "unknown, gate_pass=false", each must come back `verdict:"excluded"`,
    /// `excluded:true`, its `exclude_reason` carried verbatim, and `gate_pass` NULL (never judged
    /// pass or fail). cam4 is deliberately left OUT of the ack -- it is ALSO absent from the
    /// schedule, and must keep the UNCHANGED fail-closed behaviour (#836: this fix changes WHO
    /// gets judged, never HOW harshly).
    #[test]
    fn all_cambox_av_sync_offline_acked_camera_is_excluded_not_judged_855() {
        let emit_log: Vec<(u8, u32, i64)> = (0..10u8).map(|k| (k, 1000 + k as u32, 0)).collect();
        let audio_markers: Vec<(f64, u8)> = (0..10u8)
            .map(|k| (k as f64 / 30.0 - 0.5, k)) // video_ts(1000+k) - 0.5s = k/30.0 - 0.5
            .collect();
        let av = AvMarkerInputs {
            fps: 30.0,
            video_start_s: 0.0,
            emit_log,
            audio_preamble_screens_passed: audio_markers.len() as u64,
            audio_markers,
        };
        let cameras: &[(&str, u32, i64)] =
            &[("CAM1", CAM1B, 800_000_000), ("CAM3", CAM3B, 820_000_000)];
        let v = build_all_cambox_av_sync_fixture_with_ack(
            "offline-ack-855",
            cameras,
            Some(av),
            0.0,
            "cam5:powered-off-2026-07-27,cam6:powered-off-2026-07-27,cam7:powered-off-2026-07-27",
            None,
        );
        let av_sync = &v["all_cambox_av_sync"];
        for cam in ["cam5", "cam6", "cam7"] {
            assert_eq!(
                av_sync[cam]["verdict"],
                serde_json::json!("excluded"),
                "#855: {cam} is acked offline -> excluded, never judged: {av_sync}"
            );
            assert_eq!(
                av_sync[cam]["excluded"],
                serde_json::json!(true),
                "#855: {cam}'s excluded flag must be set: {av_sync}"
            );
            assert_eq!(
                av_sync[cam]["exclude_reason"],
                serde_json::json!("powered-off-2026-07-27"),
                "#855: {cam}'s ack reason must be carried verbatim into the JSON: {av_sync}"
            );
            assert!(
                av_sync[cam]["gate_pass"].is_null(),
                "#855: an excluded camera's gate_pass must be null (never judged pass or fail), \
                 not fabricated true/false: {av_sync}"
            );
        }
        assert_eq!(
            av_sync["cam4"]["verdict"],
            serde_json::json!("unknown"),
            "#855: cam4 is NOT acked -- absent-from-sweep must keep failing closed unchanged: {av_sync}"
        );
        assert_eq!(
            av_sync["cam4"]["gate_pass"],
            serde_json::json!(false),
            "#855/#836: an unacked absent camera must still fail the gate -- excluding requires \
             an EXPLICIT ack, never a bare lack of data: {av_sync}"
        );
    }

    /// #855 acceptance — every currently-unscheduled camera acked offline must NEVER drag the
    /// OVERALL `all_cambox_av_sync.gate_pass` down, even though NONE of them appear in the
    /// switch schedule at all (only CAM1 is scheduled here; cam3/cam4/cam5/cam6/cam7 are all
    /// acked). Contrasted against the identical fixture with NO acks, which fails closed exactly
    /// like the pre-#855 behaviour -- proving this is a real behavioural change, not just a
    /// per-camera cosmetic label.
    #[test]
    fn all_cambox_av_sync_gate_pass_true_when_every_unscheduled_camera_is_acked_855() {
        let emit_log: Vec<(u8, u32, i64)> = (0..10u8).map(|k| (k, 1000 + k as u32, 0)).collect();
        let audio_markers: Vec<(f64, u8)> = (0..10u8)
            .map(|k| (k as f64 / 30.0 - 0.5, k)) // video_ts(1000+k) - 0.5s = k/30.0 - 0.5
            .collect();
        let av = AvMarkerInputs {
            fps: 30.0,
            video_start_s: 0.0,
            emit_log,
            audio_preamble_screens_passed: audio_markers.len() as u64,
            audio_markers,
        };
        let cameras: &[(&str, u32, i64)] = &[("CAM1", CAM1B, 800_000_000)];
        // 500ms is the constructed clean offset (video_ts(1000+k) - audio_ts = 0.5s, #312's
        // established convention) -- av_expected_ms=500.0 makes cam1's (and cam2's) real
        // measurement PASS, isolating this test to the exclusion behaviour of the other 5.
        let acked = build_all_cambox_av_sync_fixture_with_ack(
            "unscheduled-acked-855",
            cameras,
            Some(av.clone()),
            500.0,
            "cam3:reason,cam4:reason,cam5:reason,cam6:reason,cam7:reason",
            None,
        );
        let unacked = build_all_cambox_av_sync_fixture_with_ack(
            "unscheduled-unacked-855",
            cameras,
            Some(av),
            500.0,
            "",
            None,
        );

        assert_eq!(
            acked["all_cambox_av_sync"]["gate_pass"],
            serde_json::json!(true),
            "#855: every unscheduled camera was acked -> only cam1+cam2 are judged, both pass \
             (measured 500ms == expected_ms 500) -> the overall gate must be true: \
             {acked}"
        );
        assert_eq!(
            unacked["all_cambox_av_sync"]["gate_pass"],
            serde_json::json!(false),
            "sanity: the IDENTICAL fixture with NO acks reproduces the pre-#855 fail-closed bug \
             (cam3..cam7 absent+unacked -> unknown -> gate FAILS) -- proving the ack is what \
             changed the outcome, not an unrelated fixture difference: {unacked}"
        );
    }

    /// #855/#861 — the ack list must never be able to silently DISABLE the re-armed BLOCKING
    /// gate: with EVERY camera in the sweep operator-ack-excluded, zero cameras are judged, the
    /// AND-fold is vacuously true, and (pre this guard) a run that measured NOTHING reported
    /// `gate_pass: true`. rig-fleet.txt already acks 5 of 7 cameras, so the ack file is a live
    /// single lever — UNMEASURED must fail closed, and the JSON carries the judged count.
    #[test]
    fn all_cambox_av_sync_fails_closed_when_every_camera_is_ack_excluded_861() {
        let emit_log: Vec<(u8, u32, i64)> = (0..10u8).map(|k| (k, 1000 + k as u32, 0)).collect();
        let audio_markers: Vec<(f64, u8)> = (0..10u8).map(|k| (k as f64 / 30.0, k)).collect();
        let av = AvMarkerInputs {
            fps: 30.0,
            video_start_s: 0.0,
            emit_log,
            audio_preamble_screens_passed: audio_markers.len() as u64,
            audio_markers,
        };
        let cameras: &[(&str, u32, i64)] = &[("CAM1", CAM1B, 800_000_000)];
        let v = build_all_cambox_av_sync_fixture_with_ack(
            "all-acked-fails-closed-861",
            cameras,
            Some(av),
            0.0,
            "cam1:test-ack,cam2:test-ack,cam3:test-ack,cam4:test-ack,cam5:test-ack,\
             cam6:test-ack,cam7:test-ack",
            None,
        );
        let av_sync = &v["all_cambox_av_sync"];
        assert_eq!(
            av_sync["judged_cameras"],
            serde_json::json!(0),
            "#861: every camera acked -> zero judged, and the count must be reported: {av_sync}"
        );
        assert_eq!(
            av_sync["gate_pass"],
            serde_json::json!(false),
            "#861: an all-acked (zero-measurement) A/V gate must FAIL closed, never report a \
             vacuous pass: {av_sync}"
        );
        assert_eq!(
            v["overall_pass"],
            serde_json::json!(false),
            "#861: the vacuous-pass guard must reach the overall verdict while the gate is \
             blocking: {v}"
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
            // Same span_ok vacuity guard as build_all_cambox_av_sync_fixture_with_ack (review
            // finding on PR #1002): a sub-300s synthetic recording must not force
            // overall_pass=false past the gate under test.
            "--min-secs",
            "0",
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
            audio_preamble_screens_passed: audio_markers.len() as u64,
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
    /// p50 is far enough above the mean pushes the re-centered offset outside the tolerance even
    /// though cam2's own measured offset is safely inside it.
    #[test]
    fn all_cambox_av_sync_derived_gate_can_fail_even_when_cam2_itself_passes_714() {
        let emit_log: Vec<(u8, u32, i64)> = (0..10u8).map(|k| (k, 1000 + k as u32, 0)).collect();
        // Real markers land exactly at video_ts(1000+k) - 0.005s ⇒ cam1's (and cam2's own
        // whole-recording) measured offset is a clean +5ms — safely inside the tolerance of
        // expected=0.
        let audio_markers: Vec<(f64, u8)> =
            (0..10u8).map(|k| (k as f64 / 30.0 - 0.005, k)).collect();
        let av = AvMarkerInputs {
            fps: 30.0,
            video_start_s: 0.0,
            emit_log,
            audio_preamble_screens_passed: audio_markers.len() as u64,
            audio_markers,
        };
        // cam1: 800ms delivery. cam3: 2×(tolerance+10ms) above cam1's, so with the two-camera
        // mean sitting halfway, cam3's re-centered delta = tolerance+10 and its derived offset =
        // 5 + tolerance + 10 -> OUTSIDE the bound even though cam2's own measured +5ms offset
        // comfortably passes. Tolerance-relative (not a literal delivery pair) so this test pins
        // the SPEC at any bound — it survived the 20→90ms interim change of the issue-861 re-arm
        // (episode quantization, re-tighten tracked on issue 1003) untouched in intent.
        let delta_over_ms = super::av_window::AV_OFFSET_GATE_TOLERANCE_MS + 10.0;
        let cam3_delivery_ns = 800_000_000 + (2.0 * delta_over_ms * 1_000_000.0) as i64;
        let v = build_all_cambox_av_sync_with_delivery_fixture(
            "derive-fail",
            &[
                ("CAM1", CAM1B, 800_000_000),
                ("CAM3", CAM3B, cam3_delivery_ns),
            ],
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
            (derived_offset - (5.0 + delta_over_ms)).abs() < 1e-6,
            "expected 5.0 + (tolerance + 10), got {derived_offset}: {av_sync}"
        );
        assert_eq!(
            av_sync["cam3"]["gate_pass"],
            serde_json::json!(false),
            "#714: a re-centered offset outside the tolerance must FAIL, independent of cam2's \
             own PASS: {av_sync}"
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
                CAM7B,
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
                CAM7B,
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
            resolve_camera_under_test_label(true, false, false, false, false, false, false),
            "cam1",
            "cam1 present ⇒ cam1 (even if, hypothetically, another id were also present)"
        );
        assert_eq!(
            resolve_camera_under_test_label(false, false, false, false, false, false, false),
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
            resolve_camera_under_test_label(false, false, true, false, false, false, false),
            "cam3",
            "#632: cam1 absent, cam3 present ⇒ label must be cam3, not the stale cam1"
        );
        assert_eq!(
            resolve_camera_under_test_label(false, false, false, true, false, false, false),
            "cam4",
            "#632: cam4 deployed ⇒ cam4"
        );
        assert_eq!(
            resolve_camera_under_test_label(false, true, false, false, false, false, false),
            "cam2",
            "#632: cam2 (the fixed painter, ALSO camera-under-test role per #312) ⇒ cam2"
        );
        assert_eq!(
            resolve_camera_under_test_label(false, false, false, false, true, false, false),
            "cam5",
            "#632: cam5 deployed ⇒ cam5"
        );
        assert_eq!(
            resolve_camera_under_test_label(false, false, false, false, false, true, false),
            "cam6",
            "#632: cam6 deployed ⇒ cam6"
        );
        assert_eq!(
            resolve_camera_under_test_label(false, false, false, false, false, false, true),
            "cam7",
            "#755: cam7 deployed ⇒ cam7"
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
    fn node_verdict_with_optical_strih_backward_jump_within_the_real_transition_guard_is_zero_loss_741(
    ) {
        // #741: the #708 test above uses `guard_ns: 0`, which never exercises the ACTUAL bug --
        // #708's exception is derived from `attribute_window_indices`'s window_of, which used to
        // route through the GUARD-filtered `place_frame_in_window`. In PRODUCTION `guard_ns` is
        // `DEFAULT_TRANSITION_GUARD_NS` (1s), and a genuine program switch changes the active
        // render source within roughly one render tick (~30ms) of the boundary -- so the frames
        // straddling a REAL cut are, by construction, ALWAYS inside that much-wider 1s guard band
        // on their own side. Live-investigated (2026-07-15, #741): both fresh CI runs' every
        // single flagged "real_drop" landed within 1-32ms of an actual switch-schedule boundary
        // -- reproduced here with a jump 30ms before the boundary and 13ms after it, matching the
        // measured magnitudes exactly.
        let guard_ns = super::DEFAULT_TRANSITION_GUARD_NS;
        let boundary_ns: i64 = 1_000_000_000; // 1s
        let schedule = vec![
            super::SwitchWindow {
                cambox: "CAM4".to_string(),
                start_ns: 0,
                end_ns: boundary_ns,
            },
            super::SwitchWindow {
                cambox: "CAM2".to_string(),
                start_ns: boundary_ns,
                end_ns: boundary_ns + 1_000_000_000,
            },
        ];
        let before_ns = boundary_ns - 30_000_000; // 30ms before the cut
        let after_ns = boundary_ns + 13_000_000; // 13ms after the cut
        let stream: Vec<RecordingFrame> = vec![
            frame_at(
                0,
                &[
                    (CAM2, 900, before_ns - 100_000_000),
                    (STRIH, 100, before_ns - 100_000_000),
                ],
            ),
            // Last frame of window0, well inside the 1s guard on the "before" side.
            frame_at(1, &[(CAM2, 901, before_ns), (STRIH, 103, before_ns)]),
            // First frame of window1, well inside the 1s guard on the "after" side. A DIFFERENT
            // source's own independent 911002 filter instance cuts onto program -- its
            // free-running counter value (101) is LOWER than the previous window's tail (103).
            frame_at(2, &[(CAM2, 902, after_ns), (STRIH, 101, after_ns)]),
            frame_at(
                3,
                &[
                    (CAM2, 903, after_ns + 100_000_000),
                    (STRIH, 106, after_ns + 100_000_000),
                ],
            ),
        ];
        let optical = optical_span_facts(&stream, &[STRIH, STREAM], None);
        let scope = super::ScheduleScope {
            schedule: &schedule,
            anchor_run_ids: &[STRIH, STREAM],
            guard_ns,
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
            "#741: a backward jump landing within the REAL (1s) transition guard of a CONFIRMED \
             window boundary must not be a phantom drop -- the guard-filtered window_of used to \
             blind attribute_window_indices to exactly this case (both sides always land inside \
             the guard on a genuine cut) -- got {:?}",
            v.contiguity
        );
        assert_eq!(v.real_drops(), 0, "#741: no phantom REAL DROP");
    }

    #[test]
    fn node_verdict_with_optical_strih_backward_jump_30us_before_boundary_is_zero_loss_903() {
        // #903 -- THE bug #741 did not cover. The live diagnosis (run 30637408198): the
        // backward-jumping frame's own gen_ts landed 30 MICROSECONDS on the OLD side of its
        // boundary, so BOTH the previous present frame AND the jump frame itself resolve to the
        // SAME (old) window via `raw_window_index` (an exact `>=`/`<` interval test) -- the #708
        // exact check alone reads "no crossing" and charges it. Unlike the #741 fixture (whose
        // jump frame already lands on the CORRECT side of the boundary, just deep inside the 1s
        // guard band), this fixture's jump frame stays on the WRONG side of the exact boundary --
        // only the #903 near-boundary tolerant signal can confirm this one.
        let guard_ns = super::DEFAULT_TRANSITION_GUARD_NS;
        let boundary_ns: i64 = 1_000_000_000; // 1s
        let schedule = vec![
            super::SwitchWindow {
                cambox: "CAM4".to_string(),
                start_ns: 0,
                end_ns: boundary_ns,
            },
            super::SwitchWindow {
                cambox: "CAM2".to_string(),
                start_ns: boundary_ns,
                end_ns: boundary_ns + 1_000_000_000,
            },
        ];
        // The previous present frame sits well inside window0, far from the boundary (not near).
        let prev_ns = boundary_ns - 100_000_000; // 100ms before
                                                 // The backward-jumping frame: this run's real offset, 30 MICROSECONDS before the boundary
                                                 // -- still strictly < boundary_ns, so raw_window_index reads window0 here too (SAME as
                                                 // prev_ns's window), even though the switch has genuinely already happened.
        let jump_ns = boundary_ns - 30_000; // 0.030ms = 30_000ns before
                                            // The next frame, well inside window1 (the new counter continuing forward).
        let next_ns = boundary_ns + 100_000_000;
        let stream: Vec<RecordingFrame> = vec![
            frame_at(
                0,
                &[
                    (CAM2, 900, prev_ns - 100_000_000),
                    (STRIH, 100, prev_ns - 100_000_000),
                ],
            ),
            // Well inside window0, far from the boundary -- window_of == Some(0), not near.
            frame_at(1, &[(CAM2, 901, prev_ns), (STRIH, 103, prev_ns)]),
            // The backward jump: A DIFFERENT source's own independent 911002 filter instance cut
            // onto program -- its free-running counter value (101) is LOWER than the previous
            // window's tail (103) -- but its gen_ts is only 30us before the boundary, so
            // window_of ALSO reads Some(0) here (the SAME window as the previous frame).
            frame_at(2, &[(CAM2, 902, jump_ns), (STRIH, 101, jump_ns)]),
            frame_at(3, &[(CAM2, 903, next_ns), (STRIH, 106, next_ns)]),
        ];
        let optical = optical_span_facts(&stream, &[STRIH, STREAM], None);
        let scope = super::ScheduleScope {
            schedule: &schedule,
            anchor_run_ids: &[STRIH, STREAM],
            guard_ns,
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
            "#903: a backward jump landing only 30us before a CONFIRMED window boundary must not \
             be a phantom drop -- the exact window-index test alone cannot see it, only the \
             near-boundary tolerant signal can -- got {:?}",
            v.contiguity
        );
        assert_eq!(v.real_drops(), 0, "#903: no phantom REAL DROP");
    }

    #[test]
    fn node_verdict_with_optical_strih_backward_jump_far_from_any_boundary_still_charged_903() {
        // #903's hard constraint: a genuine fault occurring far from every schedule boundary must
        // still FAIL -- the near-boundary tolerance must never degrade into "ignore backward
        // jumps". Same id sequence as the #708/#741 fixtures, but BOTH present frames straddling
        // the backward jump sit deep inside window0, nowhere near boundary_ns.
        let guard_ns = super::DEFAULT_TRANSITION_GUARD_NS;
        let boundary_ns: i64 = 1_000_000_000; // 1s
        let schedule = vec![
            super::SwitchWindow {
                cambox: "CAM4".to_string(),
                start_ns: 0,
                end_ns: boundary_ns,
            },
            super::SwitchWindow {
                cambox: "CAM2".to_string(),
                start_ns: boundary_ns,
                end_ns: boundary_ns + 1_000_000_000,
            },
        ];
        // Deep inside window0 -- 500ms from the boundary, far past the 200ms tolerance.
        let mid_ns = boundary_ns / 2;
        let stream: Vec<RecordingFrame> = vec![
            frame_at(
                0,
                &[
                    (CAM2, 900, mid_ns - 20_000_000),
                    (STRIH, 100, mid_ns - 20_000_000),
                ],
            ),
            frame_at(1, &[(CAM2, 901, mid_ns), (STRIH, 103, mid_ns)]),
            // Genuine in-window backward jump -- a real fault, far from any boundary.
            frame_at(
                2,
                &[
                    (CAM2, 902, mid_ns + 20_000_000),
                    (STRIH, 101, mid_ns + 20_000_000),
                ],
            ),
            frame_at(
                3,
                &[
                    (CAM2, 903, mid_ns + 40_000_000),
                    (STRIH, 106, mid_ns + 40_000_000),
                ],
            ),
        ];
        let optical = optical_span_facts(&stream, &[STRIH, STREAM], None);
        let scope = super::ScheduleScope {
            schedule: &schedule,
            anchor_run_ids: &[STRIH, STREAM],
            guard_ns,
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
            !v.is_zero(),
            "#903: a backward jump far from every boundary must still be a REAL fault -- the \
             tolerance must never mask a genuine drop -- got {:?}",
            v.contiguity
        );
        assert_eq!(
            v.real_drops(),
            1,
            "#903: exactly one genuine REAL DROP, still charged"
        );
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
        let lines = super::node_verdict_lines(&v, true, 0);
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
        let lines = super::node_verdict_lines(&v, true, 0);
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
        let lines = super::node_verdict_lines(&nv, true, 0);
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
        let lines = super::node_verdict_lines(&nv, true, 0);
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
        let lines = super::node_verdict_lines(&nv, true, 0);
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
        let lines = super::node_verdict_lines(&nv, true, 0);
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
        let lines = super::node_verdict_lines(&nv, true, 0);
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
        let j = node_verdict_json(&nv, 300.0, true, 300.0, 0);
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
    fn build_and_print_verdict_surfaces_imag_missing_tick_report_only_798() {
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

        // issue 798 (path A) -> #1142: the imag leg is now SPLIT — presence/verification BLOCKS,
        // per-frame content is REPORT-ONLY. A missing imag optical tick is a PER-FRAME CONTENT
        // failure (the optical-beat term), so it is still detected + surfaced (zero_loss false, the
        // missing id, imag_leg_pass false, imag_content_pass false, content_gates_overall_pass
        // false) but does NOT itself red overall_pass — it is confounded by the issue 1130 x264
        // record-load observer effect (pending the issue 1143 encoder fix). The presence seam is
        // separately BLOCKING (gates_overall_pass true). `pass` is intentionally not asserted: this
        // synthetic imag-only fixture also runs the unrelated #373 span gate (59 frames ~ 1s,
        // borderline min_secs), which feeds the now-BLOCKING presence term — so the overall verdict
        // depends on span, not on this content failure, and must not be a flaky assertion here. The
        // fold semantics (content fails report-only, presence fails blocking) are proven in the
        // Tier-0 `tests/imag_leg_gate.rs`.
        assert_eq!(
            v["full_chain"]["loss"]["imag"]["zero_loss"],
            serde_json::json!(false)
        );
        assert_eq!(
            v["full_chain"]["loss"]["imag"]["missing_ids"],
            serde_json::json!([115])
        );
        assert_eq!(
            v["full_chain"]["loss"]["imag"]["imag_leg_pass"],
            serde_json::json!(false),
            "#798: the imag leg's own FULL verdict is FAIL, surfaced here: {v}"
        );
        assert_eq!(
            v["full_chain"]["loss"]["imag"]["imag_content_pass"],
            serde_json::json!(false),
            "#1142: a missing optical tick is a per-frame CONTENT failure, surfaced here: {v}"
        );
        assert_eq!(
            v["full_chain"]["loss"]["imag"]["gates_overall_pass"],
            serde_json::json!(true),
            "#1142: the imag PRESENCE/VERIFICATION seam is BLOCKING (gates_overall_pass true): {v}"
        );
        assert_eq!(
            v["full_chain"]["loss"]["imag"]["content_gates_overall_pass"],
            serde_json::json!(false),
            "#1142: the imag PER-FRAME CONTENT seam is REPORT-ONLY (does not gate overall_pass): {v}"
        );
        assert_eq!(
            v["full_chain"]["imag_leg_verified"],
            serde_json::json!(true),
            "#798: an imag partial reached this merge, so the run verified the imag leg: {v}"
        );
        let _ = pass;
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

    // --- issue 930: lipsync cross-check wiring on --av-sync -----------------------------------

    #[test]
    fn cli_parses_syncnet_offset_ms_930() {
        use super::Args;
        use clap::Parser;
        let args = Args::parse_from([
            "recording-verdict",
            "--av-sync",
            "/tmp/stream-REC.mp4",
            "--av-marker-log",
            "/tmp/markers.csv",
            "--syncnet-offset-ms",
            "37.5",
        ]);
        assert_eq!(args.syncnet_offset_ms, Some(37.5));

        // Omitted (the pre-930 default, and every existing --av-sync caller): None, so
        // `lipsync_cross_check_for` short-circuits and no JSON key is ever added.
        let defaults = Args::parse_from(["recording-verdict"]);
        assert_eq!(defaults.syncnet_offset_ms, None);

        // Negative offset (video earlier than audio) must parse in bare space-separated form --
        // without `allow_negative_numbers`, clap 4 reads a leading `-` as a new flag and errors.
        let negative = Args::parse_from([
            "recording-verdict",
            "--av-sync",
            "/tmp/stream-REC.mp4",
            "--av-marker-log",
            "/tmp/markers.csv",
            "--syncnet-offset-ms",
            "-37.5",
        ]);
        assert_eq!(negative.syncnet_offset_ms, Some(-37.5));
    }

    #[test]
    fn lipsync_cross_check_for_is_none_without_syncnet_offset_930() {
        assert_eq!(super::lipsync_cross_check_for(12.0, None), None);
    }

    #[test]
    fn lipsync_cross_check_for_agrees_within_tolerance_930() {
        // 12.0 (QR/QPSK) vs 40.0 (SyncNet) -> delta 28.0, within the 50ms tolerance.
        let cc = super::lipsync_cross_check_for(12.0, Some(40.0)).expect("Some when syncnet given");
        assert_eq!(cc.qr_qpsk_offset_ms, Some(12.0));
        assert_eq!(cc.syncnet_offset_ms, Some(40.0));
        assert_eq!(cc.delta_ms, Some(28.0));
        assert_eq!(
            cc.verdict,
            camera_box::lipsync_cross_check::LipsyncCrossCheckVerdict::Agree
        );
        assert!(
            !camera_box::lipsync_cross_check::gates_overall_pass(),
            "930: report-only from day one"
        );
    }

    #[test]
    fn lipsync_cross_check_for_disagrees_beyond_tolerance_930() {
        // 0.0 (QR/QPSK) vs 500.0 (SyncNet) -> delta 500.0, far beyond the 50ms tolerance.
        let cc = super::lipsync_cross_check_for(0.0, Some(500.0)).expect("Some when syncnet given");
        assert_eq!(cc.delta_ms, Some(500.0));
        assert_eq!(
            cc.verdict,
            camera_box::lipsync_cross_check::LipsyncCrossCheckVerdict::Disagree
        );
    }
}
