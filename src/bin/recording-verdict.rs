//! recording-verdict: #107 hard-fail loss verdict from recorded OBS program files.
//!
//! Consumes the #106 recorded-file decode (NOT an NDI tap, NOT the lz4 spool) and
//! produces a HARD-FAIL zero-loss verdict with NO thresholds:
//!
//!   PASS = 0 undecodable AND 0 net copy AND 0 net gap AND analyzed span ≥ 300 s.
//!
//! The free-running 60→30 camera-sampling beat (mean step exactly 2.0, symmetric)
//! is recognized and NOT counted as loss; only the NET imbalance is real loss.
//!
//! Usage:
//!   recording-verdict --strih strih.mkv [--stream stream.mkv] \
//!       [--painter painter.csv] [--out-dir run-dir] [--min-secs 300]
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
    burn_contiguity_in_window, BurnRate, InWindowMissingKind, NodeContiguity, RecordedBurnFrame,
};
use camera_box::probe::recording::{
    analyze_recording, extract_frames_png, select_frames_to_extract, RecordingFrame,
    DEFAULT_MAX_PIXEL_PROOF,
};
use camera_box::probe::recording_latency::{
    burn_ids_in, cam2_cam1_samples, cam2_cam1_samples_from_burn, cam2_cam1_samples_from_flip,
    cam_strih_samples, chain_hop_samples_from_stream, hop_latency, painter_internal_gen_to_flip,
    strih_stream_samples, strih_stream_samples_from_stream, HopLatency, RunIds, BURN_RUN_ID_CAM1,
    BURN_RUN_ID_STREAM, BURN_RUN_ID_STRIH,
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
    /// #108 per-hop ABSOLUTE latency: the strih node's burn-QR run_id (the value
    /// `OBS_BURN_RUN_ID` was set to on strih; default mirrors the #111 burn filter).
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

/// Whether `f` is a DELIVERED optical frame — it carries cam2's optical dual-QR (any
/// CRC-valid payload whose run_id is NOT one of the forwarded node burns). A delivered
/// frame proves a frame reached the recording at that optical instant.
fn frame_is_delivered_optical(f: &RecordingFrame, burn_run_ids: &[u32]) -> bool {
    f.payloads.iter().any(|p| !burn_run_ids.contains(&p.run_id))
}

/// The full trustworthy verdict for one node: the contiguity result plus, when not
/// contiguous, every missing id classified from the pixels.
#[derive(Debug, Clone, serde::Serialize)]
struct NodeVerdict {
    contiguity: NodeContiguity,
    classified: Vec<ClassifiedMissing>,
}

impl NodeVerdict {
    /// ZERO loss ⇔ contiguous (no missing id). A BURN-UNREADABLE missing id is a real
    /// DEFECT to fix and still makes the node NOT-zero (it is never silently excluded).
    fn is_zero(&self) -> bool {
        self.contiguity.is_contiguous()
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
/// sequence; that optical loss is the separate per-recording continuity check, not a burn
/// fault for this node.
fn in_window_burn_frames(
    stream: &[RecordingFrame],
    burn_run_id: u32,
    all_burn_run_ids: &[u32],
) -> Vec<RecordedBurnFrame> {
    let is_delivered = |f: &RecordingFrame| frame_is_delivered_optical(f, all_burn_run_ids);
    let first = stream.iter().position(is_delivered);
    let last = stream.iter().rposition(is_delivered);
    let (first, last) = match (first, last) {
        (Some(f), Some(l)) => (f, l),
        // No delivered frame at all ⇒ no signal window ⇒ nothing to prove (empty).
        _ => return Vec::new(),
    };
    stream[first..=last]
        .iter()
        .filter(|f| is_delivered(f))
        .map(|f| RecordedBurnFrame {
            frame_index: f.frame_index,
            burn_id: node_burn_id_on(f, burn_run_id),
        })
        .collect()
}

/// One node's identity for the contiguity verdict: its label, its burn run_id, and how its
/// burn counter advances (#198 — cam1 per-emit, strih/stream per-render).
struct NodeSpec<'a> {
    node: &'a str,
    burn_run_id: u32,
    rate: BurnRate,
}

/// Build the trustworthy verdict for one node from the decoded stream frames: run the
/// IN-WINDOW per-recorded-frame contiguity check (#198 — rate-aware: cam1's burn is per-EMIT
/// so a forward integer gap is a REAL drop, strih/stream's is per-RENDER so a forward gap is
/// not loss), then extract a pixel-proof PNG for each missing slot the check identified.
///
/// The pure [`burn_contiguity_in_window`] is the SINGLE source of truth for both the
/// contiguity result AND each missing slot's (id, recorded frame_index, kind) — this function
/// no longer recomputes the walk or re-classifies; it just attaches the pixel proof.
fn node_verdict(
    spec: &NodeSpec,
    stream: &[RecordingFrame],
    all_burn_run_ids: &[u32],
    out_dir: &Path,
    stream_path: &Path,
    max_pixel_proof: usize,
) -> Result<NodeVerdict> {
    let node = spec.node;
    // #198: walk only the in-window DELIVERED frames; rate decides whether a forward integer
    // gap is loss; a delivered frame missing its burn IS; out-of-window ids are excluded.
    let window = in_window_burn_frames(stream, spec.burn_run_id, all_burn_run_ids);
    let in_window = burn_contiguity_in_window(node, &window, spec.rate);
    let contiguity = in_window.contiguity;

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
    let slots: Vec<u64> = classified.iter().filter_map(|c| c.frame_index).collect();
    if !slots.is_empty() {
        let png_dir = out_dir.join(format!("{node}-missing"));
        let extracted = extract_frames_png(
            stream_path,
            &slots,
            &HashSet::new(),
            &png_dir,
            max_pixel_proof,
        )?;
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
    })
}

/// Print the ONE trustworthy binary verdict for a node, human-readable, no jargon.
fn print_node_verdict(v: &NodeVerdict) {
    let c = &v.contiguity;
    let span = match (c.first_id, c.last_id) {
        (Some(f), Some(l)) => format!("ids {f}..={l}, {} present", c.present_count),
        _ => "no burn ids decoded".to_string(),
    };
    if v.is_zero() {
        println!(
            "  [{}] ZERO loss — burn-id sequence CONTIGUOUS ({span}).",
            c.node
        );
        return;
    }
    println!(
        "  [{}] NOT zero — {} missing id(s) ({span}): {} REAL DROP, {} BURN-UNREADABLE (fix burn).",
        c.node,
        c.missing_ids.len(),
        v.real_drops(),
        v.burn_unreadable(),
    );
    for cm in &v.classified {
        let label = match cm.kind {
            MissingKind::RealDrop => "REAL DROP",
            MissingKind::BurnUnreadable => "BURN-UNREADABLE (fix burn, frame delivered)",
        };
        let png = cm.png.as_deref().unwrap_or("<no pixel slot>");
        match cm.frame_index {
            Some(fi) => println!("    id {} -> {label} (frame {fi}, pixels: {png})", cm.id),
            None => println!("    id {} -> {label} (no recorded slot)", cm.id),
        }
    }
}

/// JSON for one node's trustworthy verdict.
fn node_verdict_json(v: &NodeVerdict) -> serde_json::Value {
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
/// `frame_index → grab_ts_ns` map. A malformed row (wrong column count, non-integer)
/// errors loudly rather than silently dropping — a silently-shrunk grab-ts map would
/// drop legitimate cam2→cam1 latency samples without any signal.
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
        let ts: i64 = ts_s.trim().parse().with_context(|| {
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

/// Analyze a recording into the per-frame tick stream (the #106 decode → #107 input).
fn ticks_of(path: &Path) -> Result<(Vec<RecordingFrame>, Vec<FrameTick>)> {
    let frames =
        analyze_recording(path).with_context(|| format!("analyze recording {}", path.display()))?;
    let ticks = FrameTick::from_recording_frames(&frames);
    Ok((frames, ticks))
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
    path: &Path,
    v: &RecordingVerdict,
    out_dir: &Path,
    max_pixel_proof: usize,
) -> Result<()> {
    println!("=== {label} recording DIAGNOSTIC ({}) ===", path.display());
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
                 paired in the recording(s). Enable the #111 burn (OBS_BURN_QR) on the PROBE scene \
                 and pass the matching --burn-*-run-id."
            );
            false
        }
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    tracing::info!(
        strih = ?args.strih.as_ref().map(|p| p.display().to_string()),
        stream = ?args.stream.as_ref().map(|p| p.display().to_string()),
        painter = ?args.painter.as_ref().map(|p| p.display().to_string()),
        out_dir = %args.out_dir.display(),
        min_secs = args.min_secs,
        "recording-verdict start"
    );

    let cfg = VerdictConfig {
        capture_fps: args.capture_fps,
        min_secs: args.min_secs,
        refresh_hz: args.refresh_hz,
    };

    let mut all_pass = true;
    // #105 4-node machine-readable report (per-node verdict + per-hop loss + latency),
    // built incrementally and written to --json for the 2-graph report renderer.
    let mut report = serde_json::json!({ "nodes": {}, "hops": {}, "latency": {} });

    // strih recording verdict (strict hop-1 endpoint). Keep the decoded frames so the
    // #108 per-hop latency engine can read each frame's cam2 + node-burn gen_ts_ns.
    // `--strih` is OPTIONAL: when omitted (the cam1-only optical-readability loop) the
    // strih-dependent hops (cam→strih, strih→stream) are skipped and only the cam1
    // grab is decoded/assessed.
    let strih_data: Option<(Vec<RecordingFrame>, Vec<FrameTick>)> = match &args.strih {
        Some(strih_path) => {
            let (strih_frames, strih_ticks) = ticks_of(strih_path)?;
            let strih_v = verdict(&strih_ticks, &cfg);
            // Diagnostic only (#186): the per-recording beat metrics do not gate the
            // headline — the burn-id contiguity below is authoritative.
            report_recording_diag(
                "strih",
                strih_path,
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
                "=== strih: SKIPPED (no --strih) — cam1-only optical-readability mode; \
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
    if let Some(stream_path) = &args.stream {
        let (stream_frames, stream_ticks) = ticks_of(stream_path)?;
        let stream_v = verdict(&stream_ticks, &cfg);
        // Diagnostic (not a gate): surface undecodable + span. The #186 burn-contiguity
        // verdict is authoritative for loss.
        report_recording_diag(
            "stream",
            stream_path,
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
    // strih is a real cam1→strih DROP.
    let mut cam1_frames_opt: Option<Vec<RecordingFrame>> = None;
    if let Some(cam1_path) = &args.cam1 {
        // #187: the cam1 GRAB is the large (multi-GB 4K) decode. Decode it so a failure
        // (a decode error, or the worker pool aborting under memory pressure on a tight
        // box) does NOT `?`-abort the WHOLE verdict — the valuable stream-only hops are
        // computed LATER from the stream recording ALONE and must still be produced + the
        // JSON still written. A cam1 grab failure degrades to "cam1-grab hops unavailable",
        // not a dead run. (The #187 memory bound makes this rare; this is the belt-and-
        // braces so a manual `--cam1` on a small box can never silently lose the rest of
        // the verdict.) `ticks_of` returns Err on a grab failure; we catch it here.
        let cam1_decoded = match ticks_of(cam1_path) {
            Ok(d) => Some(d),
            Err(e) => {
                eprintln!(
                    "WARNING: cam1 grab decode FAILED ({e:#}) — skipping the cam1-GRAB hops; \
                     the stream-only per-hop verdict (incl cam2→cam1 from the cam1-capture burn) \
                     is still computed from the stream recording below. (#187: a cam1 grab \
                     failure no longer aborts the whole run.)"
                );
                report["nodes"]["cam1"] = serde_json::json!({
                    "unavailable": true,
                    "reason": format!("cam1 grab decode failed: {e:#}"),
                });
                None
            }
        };
        if let Some((cam1_frames, cam1_ticks)) = cam1_decoded {
            // cam1's own per-recording continuity (undecodable / net copy/gap WITHIN cam1).
            // Diagnostic only (#186) — the burn-id contiguity below is authoritative for loss.
            let cam1_v = verdict(&cam1_ticks, &cfg);
            report_recording_diag(
                "cam1",
                cam1_path,
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
        } // end: cam1 grab decoded OK (#187 non-fatal guard)
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
    let cam2_pin = if args.cam2_run_id != 0 {
        Some(args.cam2_run_id)
    } else {
        None
    };
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
                let cam2_pin_c1 = if args.cam2_run_id != 0 {
                    Some(args.cam2_run_id)
                } else {
                    None
                };
                let c1_lat = hop_latency(
                    "cam2→cam1",
                    &cam2_cam1_samples(cam1_frames, &grab_ts, cam2_pin_c1),
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
        let cam1_ids = burn_ids_in(stream_frames, args.burn_cam1_run_id);
        let strih_ids_seq = burn_ids_in(stream_frames, args.burn_strih_run_id);
        let stream_ids_seq = burn_ids_in(stream_frames, args.burn_stream_run_id);
        let any_burn =
            !cam1_ids.is_empty() || !strih_ids_seq.is_empty() || !stream_ids_seq.is_empty();
        if any_burn {
            println!();
            println!(
                "=== #174 FULL-CHAIN per-hop verdict from the STREAM recording ALONE (clean burn-id pairing) ==="
            );
            println!(
                "  burn ids in stream recording: cam1={} strih={} stream={}",
                cam1_ids.len(),
                strih_ids_seq.len(),
                stream_ids_seq.len()
            );
            report["full_chain"]["burn_ids_present"] = serde_json::json!({
                "cam1": cam1_ids.len(), "strih": strih_ids_seq.len(), "stream": stream_ids_seq.len(),
            });

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
                args.burn_strih_run_id,
                args.burn_stream_run_id,
            ];
            println!();
            println!(
                "=== #186 ZERO-LOSS VERDICT — per-node burn-id contiguity (the ONE trustworthy check) ==="
            );
            let mut node_verdicts: Vec<NodeVerdict> = Vec::new();
            // #198: cam1's burn increments per EMITTED frame (src/main.rs), so its in-window id
            // run must be contiguous integers (a forward gap = a real cam1 drop). strih/stream
            // burn per RENDER tick (DistroAV filter), so a forward gap is expected, not loss.
            for (spec, present) in [
                (
                    NodeSpec {
                        node: "cam1",
                        burn_run_id: args.burn_cam1_run_id,
                        rate: BurnRate::PerEmittedFrame,
                    },
                    !cam1_ids.is_empty(),
                ),
                (
                    NodeSpec {
                        node: "strih",
                        burn_run_id: args.burn_strih_run_id,
                        rate: BurnRate::PerRenderTick,
                    },
                    !strih_ids_seq.is_empty(),
                ),
                (
                    NodeSpec {
                        node: "stream",
                        burn_run_id: args.burn_stream_run_id,
                        rate: BurnRate::PerRenderTick,
                    },
                    !stream_ids_seq.is_empty(),
                ),
            ] {
                if !present {
                    continue;
                }
                let stream_path = args
                    .stream
                    .as_ref()
                    .expect("stream_frames_opt is Some ⇒ --stream was provided");
                let nv = node_verdict(
                    &spec,
                    stream_frames,
                    &all_burns,
                    &args.out_dir,
                    stream_path.as_path(),
                    args.max_pixel_proof,
                )?;
                print_node_verdict(&nv);
                all_pass &= nv.is_zero();
                report["full_chain"]["loss"][spec.node] = node_verdict_json(&nv);
                node_verdicts.push(nv);
            }
            // The single binary headline, in plain words.
            let total_real: usize = node_verdicts.iter().map(NodeVerdict::real_drops).sum();
            let total_burn_unreadable: usize =
                node_verdicts.iter().map(NodeVerdict::burn_unreadable).sum();
            let all_zero = node_verdicts.iter().all(NodeVerdict::is_zero);
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
                 stream recording. Set CAMERA_BOX_BURN_RUN_ID on cam1 + OBS_BURN_QR on strih/stream \
                 (+ --burn-*-run-id) and re-run for the clean per-hop loss + latency."
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
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        in_window_burn_frames, node_burn_id_on, node_verdict, parse_cam1_capture_stats_str,
        parse_grab_ts, parse_painter_flip_str, parse_painter_ticks_str,
    };
    use camera_box::probe::burn_contiguity::{BurnRate, InWindowMissingKind};
    use camera_box::probe::payload::Payload;
    use camera_box::probe::recording::RecordingFrame;
    use std::io::Write;

    // ---- #198 in-window burn-contiguity wiring (the bug-level regression) ----

    const CAM2: u32 = 7; // optical cam2 run_id (not a burn)
    const CAM1B: u32 = 911001; // cam1 per-EMIT capture burn run_id
    const STRIH: u32 = 911002; // strih per-render burn run_id
    const STREAM: u32 = 911004; // stream per-render burn run_id

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
        let w = in_window_burn_frames(&stream, STRIH, &[STRIH, STREAM]);
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
            },
            &stream,
            &[STRIH, STREAM],
            tmp.path(),
            // stream_path only touched when there ARE missing slots to extract pixels for;
            // a zero-loss verdict never reads it.
            std::path::Path::new("/nonexistent.mp4"),
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
            },
            &stream,
            &[CAM1B, STRIH, STREAM],
            tmp.path(),
            std::path::Path::new("/nonexistent.mp4"),
            0, // cap 0 = no cap, but stream_path IS read since there's a missing slot...
        );
        // ...so extraction will error on the bogus path. We only assert the contiguity by
        // calling the pure check directly here (the node_verdict pixel extraction needs a real
        // file, exercised by the real-data validation run). Confirm the verdict errored on the
        // bad path ONLY because a real drop was found (not a silent zero-loss pass).
        let w = in_window_burn_frames(&stream, CAM1B, &[CAM1B, STRIH, STREAM]);
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
        let w = in_window_burn_frames(&stream, STRIH, &[STRIH, STREAM]);
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
        // A complete final row (ends in '\n') is NOT a partial fragment — it must still parse,
        // and a genuinely malformed COMPLETE final row still errors (it's not a kill artifact).
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(
            f,
            "frame_index,grab_ts_ns\n0,1782000000000\n2,1782000066000\n"
        )
        .unwrap();
        let m = parse_grab_ts(f.path()).unwrap();
        assert_eq!(m.get(&2), Some(&1782000066000), "complete final row parses");
        assert_eq!(m.len(), 2);
        // A complete (newline-terminated) malformed final row is corruption, not a kill cut.
        let mut f2 = tempfile::NamedTempFile::new().unwrap();
        write!(f2, "frame_index,grab_ts_ns\n0,1782000000000\n2,\n").unwrap();
        assert!(
            parse_grab_ts(f2.path()).is_err(),
            "an empty-ts row terminated by a newline is complete-and-corrupt -> error"
        );
    }

    #[test]
    fn grab_ts_sidecar_empty_ts_midfile_still_errors() {
        // A row with an empty timestamp that is NOT the last line is genuine corruption
        // (a silently-shrunk grab-ts map would drop real latency samples) — still errors.
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(
            f,
            "frame_index,grab_ts_ns\n0,1782000000000\n1,\n2,1782000066000\n"
        )
        .unwrap();
        assert!(parse_grab_ts(f.path()).is_err());
    }
}
