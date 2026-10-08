//! Recorded-file → rqrr QR analysis (offline, #106 / part of #105 Step 1).
//!
//! The loss/delivery verdict for the zero-loss proof (#105 acceptance #1) must be
//! counted ONLY from the **recorded OBS program-output file** — never an NDI tap
//! and never the lz4 spool (both add their own sampling artifacts that contaminate
//! the measurement). This module is that recorded-file path:
//!
//! ```text
//!   recording.mkv/.mp4
//!        │  ffprobe → native width×height
//!        ▼
//!   ffmpeg -i <file> -f rawvideo -pix_fmt gray pipe:1   (gray8/luma, native res)
//!        │  one width*height luma buffer per frame
//!        ▼
//!   decode_qr_luma_all_robust  (the #202 rqrr decoder in src/probe/qr.rs — full-frame
//!        │                       pass + bottom-band tile recovery for the small node burns)
//!        │  every CRC-valid Payload in the frame (dual-QR halves + recovered node burns)
//!        ▼
//!   RecordingFrame { frame_index, payloads, tick }   → CSV / stdout for #107
//! ```
//!
//! opencv was NEVER in this repo; the prior "flaky opencv" was an external tool.
//! Decoding a sharp QR is trivial for rqrr — this path simply feeds the recorded
//! frames into the already-unit-tested rqrr decoder, no opencv anywhere.
//!
//! The per-frame decode (`decode_recording_frame`) is pure and unit-tested. The
//! ffmpeg/ffprobe spawning glue (`analyze_recording`, `probe_dimensions`,
//! `read_frames`) is an external-process boundary — excluded from coverage like
//! the other hardware/process glue (`multi_reader`, `reader`, `run`).

use crate::probe::payload::Payload;
use crate::probe::qr::{
    decode_qr_luma_all_fast_then_robust, decode_qr_luma_all_fast_then_robust_grouped_optical,
    decode_qr_luma_all_robust,
};
use anyhow::{Context, Result};
use image::GrayImage;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// Upper bound on decode worker threads. The rqrr per-frame decode is the
/// CPU-bound bottleneck; beyond ~8 threads the ffmpeg I/O producer (one
/// sequential gray8 pipe) becomes the limit and more workers just contend.
const MAX_DECODE_WORKERS: usize = 8;

/// #187: estimated PEAK resident bytes a single decode worker holds for ONE frame,
/// per pixel of the native frame. A worker concurrently holds the source gray8 luma
/// (1 B/px), the `decode_qr_luma_all` working copy, and rqrr's `PreparedImage` (a
/// binarized `Box<[u8]>` ~1 B/px) plus its row-average + capstone-search scratch — so
/// the live peak is several × the raw frame. Measured on the dev1 box: a single
/// stream-only 3840×2160 decode at 4 workers peaks at ≈215 MB RSS (~46 MB/worker of
/// decode-attributable memory beyond baseline ≈ 5.5 B/px). 6 B/px is the conservative
/// upper estimate (source + clone + prepared pixels + scratch + margin) so the worker
/// cap stays SAFE rather than optimistic. On a 3840×2160 frame this is ≈50 MB/worker;
/// 8 workers ≈ 400 MB — added to the rest of a full multi-recording run on the 7.5 GB
/// dev box, the unbounded `min(cpus,8)` pool was the peak that got OOM-killed (EXIT=137).
const DECODE_PEAK_BYTES_PER_PIXEL: usize = 6;

/// #187: fraction of *available* memory the decode worker pool may consume at peak.
/// Half leaves headroom for ffmpeg, the OS page cache feeding the pipe, the small
/// per-frame `RecordingFrame` results vector, and the rest of the process — so the
/// pool can never be the cause of an OOM kill. Conservative on purpose: a slightly
/// smaller pool that COMPLETES beats a faster one that gets SIGKILLed mid-decode.
const MEM_BUDGET_FRACTION: f64 = 0.5;

/// Cap `cpu_workers` so the parallel decode's PEAK memory fits an available-memory
/// budget (#187). Each worker's per-frame peak ≈ [`DECODE_PEAK_BYTES_PER_PIXEL`] ×
/// frame area; the pool may use up to [`MEM_BUDGET_FRACTION`] of `avail_mem_bytes`.
/// Returns `min(cpu_workers, budget / per_worker)`, clamped to ≥ 1 so the pool always
/// makes forward progress (a 0-worker pool would hang). A degenerate 0-pixel frame or
/// zero per-worker cost is treated as "no memory pressure" → keep all CPU workers.
///
/// PURE (no I/O, no env, no syscalls) so the bound is unit-testable; the runtime entry
/// [`decode_workers`] reads the live available memory and frame dims and calls this.
/// This is the deterministic fix for the #187 OOM: on a small box (or a huge frame) the
/// worker count drops automatically to keep the decode within RAM, instead of the prior
/// unconditional `min(cpus, 8)` that blew past free memory and was OOM-killed.
fn workers_within_mem_budget(
    cpu_workers: usize,
    width: u32,
    height: u32,
    avail_mem_bytes: u64,
) -> usize {
    let per_worker = DECODE_PEAK_BYTES_PER_PIXEL as u64 * (width as u64) * (height as u64);
    if per_worker == 0 {
        return cpu_workers.max(1); // degenerate frame → no memory pressure
    }
    let budget = (avail_mem_bytes as f64 * MEM_BUDGET_FRACTION) as u64;
    let mem_cap = (budget / per_worker).max(1) as usize;
    cpu_workers.min(mem_cap).max(1)
}

/// Best-effort live available memory in bytes (Linux `/proc/meminfo` `MemAvailable`).
/// Returns `None` when the field can't be read/parsed (non-Linux, restricted env), in
/// which case [`decode_workers`] skips the memory bound (CPU bound only) — the prior
/// behavior, so this never makes the pool LARGER than before, only smaller when RAM is
/// genuinely tight (#187).
fn available_mem_bytes() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemAvailable:") {
            // Format: "MemAvailable:    3070272 kB"
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb.saturating_mul(1024));
        }
    }
    None
}

/// Default cap on how many flagged frames get a pixel-proof PNG written. The
/// verdict only needs a handful of visual examples per category; extracting a
/// PNG for thousands of flagged frames was a large slice of the #166 runtime
/// (re-stream + per-frame PNG encode) and is never needed to read the verdict.
/// The count of frames *dropped* by the cap is logged so nothing is hidden.
pub const DEFAULT_MAX_PIXEL_PROOF: usize = 30;

/// One analyzed frame of a recording, in file (capture) order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingFrame {
    /// 0-based index of this frame within the recording.
    pub frame_index: u64,
    /// Every CRC-valid QR payload found in the frame (both dual-QR halves, decoded
    /// in one rqrr pass). Empty when no readable QR is present in the frame.
    pub payloads: Vec<Payload>,
    /// Effective Vernier tick = the highest `frame_id` among the decoded OPTICAL (cam2
    /// dual-QR) payloads — left QR carries the latest even tick, right the latest odd, the
    /// freshest sharp region wins (matches `decode_capture_dual`'s `max_by_key(frame_id)` and
    /// `recording_latency::split_payloads`). The node BURNS (cam1/strih/stream/imag, run_ids
    /// [`NODE_BURN_RUN_IDS`]) are EXCLUDED: their per-node counters are independent of the
    /// optical tick and can exceed it, so a recovered burn must not hijack the Vernier tick
    /// (the #202 robust decode now recovers burns on most frames — without this exclusion the
    /// max would routinely be a burn's id, corrupting the tick-based diagnostics). `None` when
    /// no optical QR decoded.
    ///
    /// **#463 GOTCHA (caught by CI, not locally — this list gates the ACTUAL production tick,
    /// not just a test fixture):** when imag's own digital corner burn (`BURN_RUN_ID_IMAG`) was
    /// added, it was NOT initially added here — its burn payload's `frame_id` then legitimately
    /// competed with the cam2 optical tick in this `max()`, silently corrupting `imag`'s
    /// contiguity check whenever the burn's frame_id happened to exceed cam2's on a frame. ANY
    /// future node-burn run_id MUST be added here too, or its `.tick` silently corrupts.
    ///
    /// **#312 (found in code review, before merge): this list was NEVER extended for cam3/cam4
    /// (#624) and this PR's own new cam2/cam5/cam6 burns — exactly the #463 gotcha above,
    /// recurring.** All six camera-under-test burn ids are now included so none of their
    /// frame_ids can ever hijack the cam2 optical Vernier tick that
    /// `segment_frames_from_recording`'s ALL-CAMBOX per-segment continuity (this PR's own
    /// deliverable) anchors on.
    pub tick: Option<u32>,
}

/// The node-burn run_ids (every camera-under-test's own capture burn, plus strih/stream/imag) —
/// the digitally-generated marks our own code burns into the feed
/// (`recording_latency::BURN_RUN_ID_*`). They are NOT the cam2 optical Vernier tick and are
/// excluded from [`RecordingFrame::tick`]. Mirrored here as a small const so `tick` selection
/// doesn't pull the whole `RunIds` machinery into the per-frame hot path.
///
/// **#463 — this list is for the TICK EXCLUSION filter ONLY, never for a decode's `#207`
/// fast-path GATE** (see [`GENERIC_DIAGNOSTIC_BURN_IDS`] for that). The two purposes look
/// similar but must NOT share one list: excluding a burn from the Vernier tick is
/// correctness-critical and must include EVERY node burn that could ever appear (imag
/// included), but REQUIRING every one of them before the fast path can fire is a strictly
/// PER-RECORDING question — imag's own corner burn is emitted ONLY on imag-nb's own OBS
/// program output, so it can NEVER appear on a strih/stream/cam1-grab recording. Folding it
/// into the generic diagnostic tools' "MAXIMALLY-ROBUST" gate made `all_burns_present`
/// permanently false for every non-imag recording (a real ~10× decode slowdown for
/// `forensic-dump`/`recording-probe`/the A/V-sync tool with ZERO accuracy benefit, since
/// those tools never decode imag's own recording through this generic path) — caught in
/// review, not by a test (the existing suite has no timing assertion for this gate).
///
/// **issue 1196:** the list ALSO carries `recording_latency::AUX_TICK_RUN_ID` — not a burn but
/// the PAINTED aux Vernier tick pair (bottom burn-gap QRs,
/// `gen_ts_ns = 0`). Same exclusion rationale as the #463 gotcha above, sharper: on a torn or
/// band-corrupted frame the aux `frame_id`s carry a DIFFERENT paint generation than the primary
/// pair, so letting them feed `tick` would silently shift undecodable/continuity/cadence metrics
/// the strict gates are calibrated on. Only the report-only tear surface (`crate::tear_detect`
/// v2) reads them, by run_id, explicitly. Appended LAST so index-based test uses (`N[0]` = cam1)
/// stay stable.
///
/// **issue 1404:** the measurement clip's painted dual-QR (`recording_latency::MEASUREMENT_CLIP_RUN_ID`)
/// rides into the strih/stream recordings during a CG segment; its `frame_id` is the clip's own
/// tick, so it is tick-excluded like the aux pair. Appended last for the same reason.
pub const NODE_BURN_RUN_IDS: [u32; 14] = [
    crate::probe::recording_latency::BURN_RUN_ID_CAM1,
    crate::probe::recording_latency::BURN_RUN_ID_CAM2,
    crate::probe::recording_latency::BURN_RUN_ID_CAM3,
    crate::probe::recording_latency::BURN_RUN_ID_CAM4,
    crate::probe::recording_latency::BURN_RUN_ID_CAM5,
    crate::probe::recording_latency::BURN_RUN_ID_CAM6,
    crate::probe::recording_latency::BURN_RUN_ID_CAM7,
    crate::probe::recording_latency::BURN_RUN_ID_STRIH,
    crate::probe::recording_latency::BURN_RUN_ID_STREAM,
    crate::probe::recording_latency::BURN_RUN_ID_IMAG,
    // #1301: the SongPlayer-origin (911014) + cg-OBS-hop (911015) CG-chain burns can ride into a
    // strih/stream recording during a CG_CHAIN run, so they MUST be tick-excluded like every other
    // node burn (the #463/#312 gotcha) — a stray CG frame_id must never hijack the cam2 Vernier tick.
    crate::probe::recording_latency::BURN_RUN_ID_SONGPLAYER,
    crate::probe::recording_latency::BURN_RUN_ID_CG,
    crate::probe::recording_latency::AUX_TICK_RUN_ID,
    crate::probe::recording_latency::MEASUREMENT_CLIP_RUN_ID,
];

/// The node-burn run_ids the GENERIC diagnostic tools ([`decode_recording_frame`] /
/// [`analyze_recording`] — `forensic-dump`, `recording-probe`; the `--av-sync` painter path uses
/// the same set through `crate::av_sync_decode_plan::PAINTER_PATH_NODE_BURNS`, issue 1404)
/// require before the #207 fast path may skip the robust tiles. Deliberately NOT
/// [`NODE_BURN_RUN_IDS`] (#463): these generic, box-agnostic tools only ever decode a
/// strih/stream/cam1-grab recording (never imag's own recording, which has its own dedicated
/// `--imag` path in `recording-verdict`), and none of those recordings can EVER carry imag's
/// corner burn — requiring it here would make the fast path permanently unreachable for a
/// real recording, forcing the ~10× robust tiles on every frame for no accuracy gain.
const GENERIC_DIAGNOSTIC_BURN_IDS: [u32; 3] = [
    crate::probe::recording_latency::BURN_RUN_ID_CAM1,
    crate::probe::recording_latency::BURN_RUN_ID_STRIH,
    crate::probe::recording_latency::BURN_RUN_ID_STREAM,
];

/// Decode one native-resolution luma frame into a `RecordingFrame`, requiring the full
/// [`GENERIC_DIAGNOSTIC_BURN_IDS`] set before the fast path may skip the robust tiles.
///
/// This is the MAXIMALLY-ROBUST default FOR THE RECORDINGS THIS GENERIC PATH ACTUALLY SEES
/// (strih / stream / cam1-grab — never imag's own recording, #463): requiring all three node
/// burns means the #207 fast gate only fires on a frame that already carries cam1 + strih +
/// stream, so any recording missing a burn (e.g. a strih recording, which never carries the
/// stream burn) always runs the full robust recovery — exactly what the diagnostic tools
/// (forensic-dump, recording-probe) want. The verdict (the perf-critical 30-min 4K path)
/// instead decodes with the burns that recording is KNOWN to carry, so the fast path fires on its
/// ~99 %+ clean frames. See [`decode_recording_frame_with_burns`].
///
/// issue 1367: this per-frame helper (and [`decode_recording_frame_with_burns`]) runs the flat
/// decode with the node-burn echo gate OFF. Every recording analysis (`analyze_recording*`, the
/// parallel pool) runs the GROUPED decode instead, which drops a node burn read outside its own
/// slot.
pub fn decode_recording_frame(frame_index: u64, luma: GrayImage) -> RecordingFrame {
    decode_recording_frame_with_burns(frame_index, luma, &GENERIC_DIAGNOSTIC_BURN_IDS)
}

/// Decode one native-resolution luma frame into a `RecordingFrame`, with the #207 fast gate
/// keyed on `expected_node_burns` — the node burns THIS recording is known to carry.
///
/// PURE (no I/O, no ffmpeg): feeds the luma image into the rqrr decoder via the #207
/// FAST-then-ROBUST decode ([`decode_qr_luma_all_fast_then_robust`]): the cheap plain
/// full-frame pass FIRST, and the #202 tiled+upscaled retry that recovers the small node
/// burns rqrr's single `detect_grids` pass intermittently misses ONLY when one of
/// `expected_node_burns` is absent from the plain pass.
///
/// Passing the burns the recording ACTUALLY carries is what unlocks the speedup on every
/// recording: a strih recording (cam1 + strih, never stream) passes `[cam1, strih]`, so a
/// clean frame carrying both takes the fast path instead of forever running tiles to chase a
/// stream burn that was never recorded. On a clean recording the plain pass already reads
/// every expected burn on ~99 %+ of frames, so the ~10×-cost tiles almost never run — a 30-min
/// verdict drops from ~50 min to ~5 — while the rare hard frame still gets the full robust
/// recovery, preserving the #186 0-miss guarantee exactly. This decode runs in the parallel
/// #166/#187 worker pool, never on the latency-sensitive live tap.
/// `frame_index` is the caller-supplied position in the recording.
pub fn decode_recording_frame_with_burns(
    frame_index: u64,
    luma: GrayImage,
    expected_node_burns: &[u32],
) -> RecordingFrame {
    let payloads = decode_qr_luma_all_fast_then_robust(luma, expected_node_burns);
    // The Vernier tick is the freshest OPTICAL (cam2) half — node burns are excluded so a
    // recovered burn (now common, #202) never hijacks the max (see RecordingFrame::tick).
    let tick = payloads
        .iter()
        .filter(|p| !NODE_BURN_RUN_IDS.contains(&p.run_id))
        .map(|p| p.frame_id)
        .max();
    RecordingFrame {
        frame_index,
        payloads,
        tick,
    }
}

/// #632 gap 1 — [`decode_recording_frame_with_burns`] with the #207 fast gate split into TWO
/// independent groups (see [`decode_qr_luma_all_fast_then_robust_grouped_optical`]): `mandatory_burns`
/// must ALL decode (e.g. the strih/stream render burns), and `any_of_burns` needs only ONE
/// member decoded (e.g. whichever ONE of cam1/cam2/cam3/cam4/cam5/cam6 is the camera physically
/// under test THIS run — they are mutually exclusive, so requiring ALL six in one flat
/// mandatory list, the way [`decode_recording_frame_with_burns`] would if simply handed all
/// six ids, would be permanently unsatisfiable). This is
/// what lets a cam3/cam4/cam5/cam6-deployed recording take the #207 FAST path the way a
/// cam1-deployed one always could, without weakening the #186 0-miss guarantee for whichever
/// camera actually is under test (see the qr.rs doc for the full reasoning).
pub fn decode_recording_frame_with_grouped_burns(
    frame_index: u64,
    luma: GrayImage,
    mandatory_burns: &[u32],
    any_of_burns: &[u32],
) -> RecordingFrame {
    decode_recording_frame_with_grouped_burns_optical(
        frame_index,
        luma,
        mandatory_burns,
        any_of_burns,
        None,
    )
}

/// #707 — [`decode_recording_frame_with_grouped_burns`] with the #207 gate's third (optical)
/// completeness dimension: `min_distinct_optical = Some((cam2_run_id, 2))` requires the plain
/// pass to already carry BOTH dual-QR Vernier halves before skipping the #202 robust tiled
/// retry — see [`crate::probe::qr::decode_qr_luma_all_fast_then_robust_grouped_pathed_optical`]
/// for the full reasoning. `None` is byte-for-byte identical to
/// [`decode_recording_frame_with_grouped_burns`] (every pre-#707 caller, unaffected).
pub fn decode_recording_frame_with_grouped_burns_optical(
    frame_index: u64,
    luma: GrayImage,
    mandatory_burns: &[u32],
    any_of_burns: &[u32],
    min_distinct_optical: Option<(u32, usize)>,
) -> RecordingFrame {
    let payloads = decode_qr_luma_all_fast_then_robust_grouped_optical(
        luma,
        mandatory_burns,
        any_of_burns,
        min_distinct_optical,
    );
    // Same tick derivation as decode_recording_frame_with_burns (node burns excluded).
    let tick = payloads
        .iter()
        .filter(|p| !NODE_BURN_RUN_IDS.contains(&p.run_id))
        .map(|p| p.frame_id)
        .max();
    RecordingFrame {
        frame_index,
        payloads,
        tick,
    }
}

/// Native width×height of a recording's first video stream, via `ffprobe`.
fn probe_dimensions(path: &Path) -> Result<(u32, u32)> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0:s=x",
        ])
        .arg(path)
        .stderr(Stdio::piped())
        .output()
        .context("spawn ffprobe (install ffmpeg: apt install ffmpeg)")?;
    if !out.status.success() {
        anyhow::bail!(
            "ffprobe failed on {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let line = s
        .lines()
        .next()
        .with_context(|| format!("ffprobe returned no video stream for {}", path.display()))?
        .trim();
    let (w, h) = line
        .split_once('x')
        .with_context(|| format!("ffprobe dimensions not WxH: {line:?}"))?;
    let width: u32 = w
        .parse()
        .with_context(|| format!("ffprobe width not a number: {w:?}"))?;
    let height: u32 = h
        .parse()
        .with_context(|| format!("ffprobe height not a number: {h:?}"))?;
    anyhow::ensure!(
        width > 0 && height > 0,
        "ffprobe returned zero dimension ({width}x{height}) for {}",
        path.display()
    );
    tracing::info!(
        file = %path.display(), width, height,
        "recording native resolution probed"
    );
    Ok((width, height))
}

/// Stream every native-resolution gray8 (luma) frame out of `path` via ffmpeg,
/// invoking `on_frame(frame_index, luma)` for each. Frames are read by exact
/// byte count (`width * height`), so a truncated trailing frame is ignored.
///
/// `on_frame` returns `true` to keep reading and `false` to ABORT the read loop
/// early (the consumer can no longer accept frames — the parallel decode's
/// worker pool has died, or a head analysis, [`analyze_recording_head`], has
/// all its frames). On an early abort `read_frames` returns the count READ,
/// including the frame it stopped at; the caller distinguishes "all frames
/// read" (count == decoded) from "aborted" via its own bookkeeping. ffmpeg is
/// killed on abort so it does not linger writing to a closed pipe.
///
/// ffmpeg's stderr is INHERITED (not piped): on a 30-min / 54k-frame clip a piped
/// stderr we only drain after the stdout loop could fill its ~64 KB OS pipe buffer
/// while we block reading stdout — ffmpeg then blocks writing stderr and we block
/// reading stdout, a classic two-pipe deadlock. Inheriting routes any `-v error`
/// output straight to this process's stderr/logs with no buffer to fill. A
/// non-zero ffmpeg exit fails the call (the inherited error text is already in the
/// log).
fn read_frames(
    path: &Path,
    width: u32,
    height: u32,
    mut on_frame: impl FnMut(u64, GrayImage) -> bool,
) -> Result<u64> {
    let frame_bytes = (width as usize) * (height as usize);
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "gray", "pipe:1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("spawn ffmpeg (install ffmpeg: apt install ffmpeg)")?;

    let mut stdout = child
        .stdout
        .take()
        .context("ffmpeg stdout pipe unavailable")?;

    let mut buf = vec![0u8; frame_bytes];
    let mut frame_index: u64 = 0;
    let mut aborted = false;
    loop {
        match stdout.read_exact(&mut buf) {
            Ok(()) => {
                // Move `buf` into the GrayImage (no per-frame clone) and replace it
                // with a fresh buffer for the next read — on a 54k-frame clip this
                // avoids 54k redundant width*height copies.
                let owned = std::mem::replace(&mut buf, vec![0u8; frame_bytes]);
                let luma = GrayImage::from_raw(width, height, owned)
                    .context("luma buffer sized width*height")?;
                frame_index += 1;
                if !on_frame(frame_index - 1, luma) {
                    // Consumer can no longer accept frames — stop reading and kill
                    // ffmpeg so it doesn't block writing to the (soon-closed) pipe.
                    aborted = true;
                    let _ = child.kill();
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e).context("read ffmpeg rawvideo stdout"),
        }
    }

    // On an early abort we killed ffmpeg, so its exit status is a signal, not a
    // clean 0 — do not treat that as a decode failure (the abort is the real
    // cause, surfaced by the caller). Reap the child and return the partial count.
    // (ffmpeg may print a harmless "Error writing trailer"/SIGPIPE line to the
    // inherited stderr on kill — expected noise on the abort path, which every
    // head analysis takes, not a fault.)
    if aborted {
        let _ = child.wait();
        return Ok(frame_index);
    }

    let status = child.wait().context("wait for ffmpeg")?;
    if !status.success() {
        anyhow::bail!(
            "ffmpeg decode failed on {} (status {:?}); see ffmpeg stderr above",
            path.display(),
            status.code(),
        );
    }
    tracing::info!(
        file = %path.display(), frames = frame_index,
        "recording fully decoded via ffmpeg gray8 pipe"
    );
    Ok(frame_index)
}

/// Number of decode worker threads to use for a frame of `width`×`height`, capped by
/// BOTH the CPU count AND an available-memory budget (#187). The CPU cap is
/// `min(available_parallelism, MAX_DECODE_WORKERS)` (the #160/#166 sizing); the memory
/// cap ([`workers_within_mem_budget`]) then throttles it DOWN when free RAM cannot hold
/// `workers × per-frame peak` — the fix for the #187 OOM (the prior unconditional
/// `min(cpus, 8)` blew past free memory on a big 4K recording and was SIGKILLed).
///
/// `CAMERA_BOX_DECODE_WORKERS` overrides the auto CPU value (still clamped to
/// `1..=MAX_DECODE_WORKERS` AND still memory-bounded — an explicit pin must never let a
/// shared runner OOM) — useful to A/B the speedup (`=1` reproduces the single-threaded
/// loop). A non-numeric or zero value is ignored (falls back to auto). When available
/// memory can't be read (non-Linux / restricted), the memory bound is skipped (CPU
/// bound only — never larger than before).
fn decode_workers(width: u32, height: u32) -> usize {
    let cpu_workers = if let Ok(v) = std::env::var("CAMERA_BOX_DECODE_WORKERS") {
        v.trim()
            .parse::<usize>()
            .ok()
            .filter(|&n| n >= 1)
            .map(|n| n.clamp(1, MAX_DECODE_WORKERS))
            .unwrap_or_else(auto_cpu_workers)
    } else {
        auto_cpu_workers()
    };
    match available_mem_bytes() {
        Some(avail) => workers_within_mem_budget(cpu_workers, width, height, avail),
        None => cpu_workers, // memory unknown → CPU bound only (prior behavior)
    }
}

/// Auto CPU worker count = available parallelism clamped to `1..=MAX_DECODE_WORKERS`.
fn auto_cpu_workers() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, MAX_DECODE_WORKERS)
}

/// Decode an ffmpeg-produced gray8 frame stream into `RecordingFrame`s **across a
/// pool of CPU threads**, returning them in capture (`frame_index`) order.
///
/// `produce` is the I/O producer (e.g. [`read_frames`]): it calls the supplied
/// `emit(frame_index, luma)` once per native-resolution gray8 frame, in order,
/// and returns the total frame count. This function fans each raw luma frame out
/// to `workers` decode threads (the rqrr decode — `width*height` luma + detect —
/// is the ~60 ms/frame bottleneck and embarrassingly parallel: #160 proved this
/// for the spool path), then re-sorts the results by `frame_index` so the output
/// is byte-for-byte identical to the prior single-threaded loop.
///
/// A bounded job channel (`workers * 2`) caps the number of in-flight
/// undecoded frames so a 9000-frame / 7+ GB recording never materialises every
/// frame in RAM at once.
fn decode_stream_parallel(
    workers: usize,
    mandatory_burns: &[u32],
    any_of_burns: &[u32],
    min_distinct_optical: Option<(u32, usize)>,
    produce: impl FnOnce(&mut dyn FnMut(u64, GrayImage) -> bool) -> Result<u64>,
) -> Result<Vec<RecordingFrame>> {
    let (job_tx, job_rx) = std::sync::mpsc::sync_channel::<(u64, GrayImage)>(workers * 2);
    let job_rx = Arc::new(Mutex::new(job_rx));
    let (res_tx, res_rx) = std::sync::mpsc::channel::<RecordingFrame>();
    // Owned + shared so each worker thread can read the #207 fast-gate burn sets without
    // borrowing the caller's slices across the `'static` thread boundary. #632: split into two
    // independent groups (mandatory / any-of) — see `decode_recording_frame_with_grouped_burns`.
    let mandatory_burns: Arc<[u32]> = Arc::from(mandatory_burns);
    let any_of_burns: Arc<[u32]> = Arc::from(any_of_burns);

    let mut handles = Vec::with_capacity(workers);
    for _ in 0..workers {
        let job_rx = job_rx.clone();
        let res_tx = res_tx.clone();
        let mandatory_burns = mandatory_burns.clone();
        let any_of_burns = any_of_burns.clone();
        handles.push(std::thread::spawn(move || loop {
            // Lock only to pull the next job; release before the heavy decode so
            // workers don't serialise on the mutex (the #160 pattern).
            let job = {
                let rx = job_rx.lock().unwrap();
                rx.recv()
            };
            let Ok((idx, luma)) = job else { break };
            // `min_distinct_optical` is `Option<(u32, usize)>` (Copy) — captured by the `move`
            // closure once per worker thread, same as the #707 gate's other parameters.
            let f = decode_recording_frame_with_grouped_burns_optical(
                idx,
                luma,
                &mandatory_burns,
                &any_of_burns,
                min_distinct_optical,
            );
            tracing::debug!(
                frame = f.frame_index, decoded = f.payloads.len(), tick = ?f.tick,
                "recording frame analyzed"
            );
            // A closed result channel means the collector is gone — stop.
            if res_tx.send(f).is_err() {
                break;
            }
        }));
    }
    drop(res_tx); // only the workers' clones remain; res_rx ends when all exit
                  // CRITICAL (#166 review): drop THIS scope's Receiver Arc so the
                  // only remaining holders are the worker threads. Otherwise the
                  // producer's `job_tx.send()` could never observe `Err` when all
                  // workers die (the Arc strong-count would stay ≥1 here forever),
                  // and the bounded channel would fill and BLOCK the producer
                  // permanently — a hang instead of a loud error, the exact #166
                  // failure class. With this drop, all-workers-dead ⇒ Receiver
                  // dropped ⇒ `send` returns Err ⇒ the producer aborts the read
                  // loop ⇒ `handles.join()` surfaces the worker panic.
    drop(job_rx);

    // Collector: drain decoded frames as they complete (out of order) so workers
    // never block on a full result queue. Runs on its own thread concurrently
    // with the producer below.
    let collector = std::thread::spawn(move || {
        let mut out: Vec<RecordingFrame> = Vec::new();
        while let Ok(f) = res_rx.recv() {
            out.push(f);
        }
        out
    });

    // Producer: sequential ffmpeg I/O on THIS thread, feeding the worker pool. The
    // emit closure returns `false` when the job channel is closed (all workers have
    // died — see the drop above), which ABORTS the read loop so the producer can
    // reach `handles.join()` and surface the worker panic instead of blocking on a
    // full channel forever. A producer error still drops job_tx (closing the
    // workers) before we propagate it, so no thread leaks.
    let produce_result = produce(&mut |idx, luma| job_tx.send((idx, luma)).is_ok());
    drop(job_tx); // signal end-of-jobs → workers exit → collector finishes

    let mut worker_panicked = false;
    for h in handles {
        // A worker panic (e.g. a bug in decode) must fail the analysis loudly,
        // not silently drop frames. Join ALL handles first (so none leak) before
        // returning the error.
        if h.join().is_err() {
            worker_panicked = true;
        }
    }
    let mut frames = collector
        .join()
        .map_err(|_| anyhow::anyhow!("recording decode collector panicked"))?;
    if worker_panicked {
        anyhow::bail!("recording decode worker panicked");
    }

    // Surface a producer (ffmpeg/ffprobe) error after the threads are cleaned up.
    let total = produce_result?;

    // Re-establish capture order: results arrive in completion order across N
    // workers, so sort by frame_index to match the single-threaded output exactly.
    frames.sort_unstable_by_key(|f| f.frame_index);
    anyhow::ensure!(
        frames.len() as u64 == total,
        "parallel decode produced {} frames but the producer emitted {} \
         (a worker dropped frames / died mid-decode)",
        frames.len(),
        total
    );
    Ok(frames)
}

/// Analyze a recorded OBS program-output file end-to-end: probe its native
/// resolution, stream every frame as gray8 luma via ffmpeg, decode each with the
/// rqrr decoder **across all CPU cores**, and return one [`RecordingFrame`] per
/// frame in capture order.
///
/// This is the recorded-file analysis entrypoint (#106). It NEVER uses an NDI tap
/// or the lz4 spool — the loss/delivery verdict (#105 acceptance #1, computed
/// downstream in #107) must be derived only from the recorded file.
///
/// #166: the per-frame rqrr decode is parallelized over a worker pool (mirroring
/// the #160 spool-decode parallelization). A 7+ GB lossless gray8 cam1 grab that
/// previously took >1 h single-threaded now decodes in minutes; the output order
/// is identical to the prior serial loop (results are re-sorted by frame_index).
///
/// Uses the MAXIMALLY-ROBUST gate for the recordings this GENERIC path actually sees
/// (strih / stream / cam1-grab — never imag's own recording, #463): requires the full
/// [`GENERIC_DIAGNOSTIC_BURN_IDS`] set before the #207 fast path may skip the tiles — the
/// right default for the diagnostic dump tools. Deliberately NOT [`NODE_BURN_RUN_IDS`]: that
/// 4-element list includes imag's corner burn, which can never appear on a recording this
/// generic path decodes, so requiring it here would make the fast path permanently
/// unreachable (see the [`GENERIC_DIAGNOSTIC_BURN_IDS`] doc comment for the full ~10× slowdown
/// this caused when `analyze_recording` and `decode_recording_frame` disagreed on which list
/// to use — caught in review, #463). The verdict's perf-critical path calls
/// [`analyze_recording_with_burns`] with the burns the recording actually carries to unlock
/// the #207 speedup.
pub fn analyze_recording(path: &Path) -> Result<Vec<RecordingFrame>> {
    analyze_recording_with_burns(path, &GENERIC_DIAGNOSTIC_BURN_IDS)
}

/// #1088 (signal fix #1166) — stream every recorded frame out of `path` in recorded order and
/// compute each frame's CODEC-TOLERANT near-duplicate signal: the row-sampled mean-abs-luma-DIFFERENCE
/// ([`crate::dup_cadence::frame_row_sampled_mad`]) to the PREVIOUS recorded frame. Index `i` of the
/// returned vector is `RecordingFrame::frame_index` `i` (both `read_frames` and the parallel decode
/// index frames sequentially from 0): index 0 is `None` (no predecessor) and index `i>=1` is
/// `Some(MAD(frame i, frame i-1))`. A SEPARATE, luma-only ffmpeg pass that runs NO QR/burn decode,
/// so it is far cheaper than [`analyze_recording`]'s robust decode — run ONCE per verdict on the
/// offline worker for the duplication-masked 50→60 detector (issue 1088).
///
/// #1166: replaces the retired byte-exact `hash_recording_frames`. Byte-exact frame identity does
/// not survive the stream box's lossy `.mp4` encode (a genuine duplicate frame becomes byte-unique),
/// so a per-frame HASH observed almost none of the duplication (2/147 tick-proven copies, #1101).
/// The MAD-to-predecessor is codec-tolerant: a duplicate survives as a LOW-MAD pair. It must be
/// computed here (between consecutive FULL-resolution decoded frames) — a downscaled thumbnail loses
/// the copy/motion separation — and carried per frame; the dev1 merge, which has no recording, then
/// thresholds it per window ([`crate::dup_cadence::window_prev_mads`] / `measure_dup_cadence`).
///
/// Deliberately its OWN pass, NOT a new return value threaded through `analyze_recording_*`:
/// widening those functions' return type would churn all ~15 of their callers, every one
/// CI-first-compile (`required-features = ["probe"]`, no local compile path), for a report-only
/// metric — a poor risk trade. The extra offline luma decode is the cost of that isolation;
/// folding the diff into the main decode pass to save it is a report-only follow-up optimization
/// once the metric is calibrated.
pub fn frame_prev_diffs(path: &Path) -> Result<Vec<Option<f64>>> {
    let (width, height) = probe_dimensions(path)?;
    let mut diffs: Vec<Option<f64>> = Vec::new();
    let mut prev: Option<Vec<u8>> = None;
    read_frames(path, width, height, |_idx, luma| {
        let cur = luma.into_raw();
        match &prev {
            None => diffs.push(None),
            Some(p) => diffs.push(Some(crate::dup_cadence::frame_row_sampled_mad(
                p,
                &cur,
                width as usize,
                height as usize,
            ))),
        }
        prev = Some(cur);
        true
    })?;
    Ok(diffs)
}

/// [`analyze_recording`] with the #207 fast gate keyed on `expected_node_burns` — the node
/// burns THIS recording is known to carry. Passing the right set (e.g. `[cam1, strih]` for a
/// strih recording) lets the fast path fire on the ~99 %+ of clean frames, dropping a 30-min
/// verdict from ~50 min to ~5 while still running the full robust recovery on any frame that
/// misses an expected burn (the #186 0-miss guarantee). See [`decode_recording_frame_with_burns`].
pub fn analyze_recording_with_burns(
    path: &Path,
    expected_node_burns: &[u32],
) -> Result<Vec<RecordingFrame>> {
    // #632: mandatory-only (empty any-of group ⇒ vacuously satisfied) — IDENTICAL semantics to
    // the pre-#632 implementation, unchanged for every existing caller.
    analyze_recording_with_grouped_burns(path, expected_node_burns, &[])
}

/// #632 gap 1 — [`analyze_recording_with_burns`] with the #207 fast gate split into two
/// independent groups (see [`decode_recording_frame_with_grouped_burns`]): `mandatory_burns`
/// must ALL be found on a frame, `any_of_burns` needs only ONE member found (empty ⇒ vacuously
/// satisfied). Pass e.g. `mandatory = [strih]`, `any_of = [cam1, cam2, cam3, cam4, cam5, cam6]`
/// for a strih recording so the fast path fires regardless of WHICH camera is physically under
/// test this run (they are mutually exclusive — only the deployed one's burn ever appears).
pub fn analyze_recording_with_grouped_burns(
    path: &Path,
    mandatory_burns: &[u32],
    any_of_burns: &[u32],
) -> Result<Vec<RecordingFrame>> {
    analyze_recording_with_grouped_burns_optical(path, mandatory_burns, any_of_burns, None)
}

/// #707 — [`analyze_recording_with_grouped_burns`] with the #207 gate's third (optical)
/// completeness dimension threaded all the way through the parallel decode pool; see
/// [`decode_recording_frame_with_grouped_burns_optical`]. `None` is byte-for-byte identical to
/// [`analyze_recording_with_grouped_burns`] (every pre-#707 caller, unaffected).
pub fn analyze_recording_with_grouped_burns_optical(
    path: &Path,
    mandatory_burns: &[u32],
    any_of_burns: &[u32],
    min_distinct_optical: Option<(u32, usize)>,
) -> Result<Vec<RecordingFrame>> {
    analyze_frames(
        path,
        mandatory_burns,
        any_of_burns,
        min_distinct_optical,
        None,
    )
}

/// Issue 1404 — [`analyze_recording_with_grouped_burns_optical`] (no any-of group) over only the
/// FIRST `max_frames` frames of `path`, every frame when the recording is shorter: ffmpeg is
/// stopped once they are read. The `--av-sync` painter path reads its cheap head with it before
/// the full decode (`crate::av_sync_decode_plan`).
pub fn analyze_recording_head(
    path: &Path,
    max_frames: u64,
    mandatory_burns: &[u32],
    min_distinct_optical: Option<(u32, usize)>,
) -> Result<Vec<RecordingFrame>> {
    analyze_frames(
        path,
        mandatory_burns,
        &[],
        min_distinct_optical,
        Some(max_frames),
    )
}

/// The recording analysis behind [`analyze_recording_with_grouped_burns_optical`] (`max_frames =
/// None`, the whole recording) and [`analyze_recording_head`].
fn analyze_frames(
    path: &Path,
    mandatory_burns: &[u32],
    any_of_burns: &[u32],
    min_distinct_optical: Option<(u32, usize)>,
    max_frames: Option<u64>,
) -> Result<Vec<RecordingFrame>> {
    let (width, height) = probe_dimensions(path)?;
    let workers = decode_workers(width, height);
    // issue 1404: the counters are process-global and the --av-sync painter head adds its frames
    // to them, so the complete line also reports this analysis's own split
    let (fast_before, robust_before) = crate::probe::qr::decode_path_counts();
    tracing::info!(
        file = %path.display(), width, height, workers,
        mandatory_burns = ?mandatory_burns, any_of_burns = ?any_of_burns,
        min_distinct_optical = ?min_distinct_optical, max_frames = ?max_frames,
        avail_mb = available_mem_bytes().map(|b| b / 1_048_576),
        "recording analysis start (parallel decode, #187 memory-bounded worker pool, #207 fast-then-robust)"
    );
    // Emit a periodic frame-count HEARTBEAT at info level (every PROGRESS_EVERY
    // frames the producer reads). This is the liveness signal the harness stall
    // detector watches (scripts/verdict-monitor.sh): without it, a single large
    // recording decodes SILENTLY between its start/complete logs, so a long clip
    // could exceed the stall timeout and be falsely killed. The heartbeat keeps the
    // output growing during the decode so the detector sees real progress — the
    // deep fix, not a band-aid timeout bump (no-timeout-band-aids.md).
    const PROGRESS_EVERY: u64 = 1000;
    let frames = decode_stream_parallel(
        workers,
        mandatory_burns,
        any_of_burns,
        min_distinct_optical,
        |emit| {
            let read = read_frames(path, width, height, |idx, luma| {
                // issue 1404: a head analysis stops ffmpeg at the first frame past the head
                if max_frames.is_some_and(|max| idx >= max) {
                    return false;
                }
                if idx > 0 && idx % PROGRESS_EVERY == 0 {
                    tracing::info!(file = %path.display(), frames_read = idx, "recording decode progress");
                }
                emit(idx, luma)
            })?;
            // read_frames also counts the frame it stopped at, which the head never decoded
            Ok(max_frames.map_or(read, |max| read.min(max)))
        },
    )?;
    let decoded: usize = frames.iter().filter(|f| !f.payloads.is_empty()).count();
    // #207 — the fast/robust decode-path split, so the verdict log shows the speedup is real:
    // on a clean recording almost every frame should take the cheap plain-only fast path, with
    // the ~10×-cost tiled recovery firing only on the rare burn-missed frame. (Counters are
    // process-global, so the absolute numbers accumulate across all recordings analyzed in one
    // run — what matters is fast ≫ robust.)
    let (fast, robust) = crate::probe::qr::decode_path_counts();
    // issue 1370: how many expected node burns only the burn-isolated slot crops read. A large
    // number means the camera view pushed optical content into the #202 burn tiles.
    let burn_region_recoveries = crate::probe::burn_region_decode::burn_region_recovery_count();
    tracing::info!(
        file = %path.display(), total = frames.len(), with_qr = decoded, workers,
        fast_path_frames = fast, robust_fallback_frames = robust, burn_region_recoveries,
        this_analysis_fast = fast.saturating_sub(fast_before),
        this_analysis_robust = robust.saturating_sub(robust_before),
        "recording analysis complete (#207 decode-path split, cumulative this run)"
    );
    Ok(frames)
}

/// One extracted pixel-proof PNG of a frame the #107 verdict flagged.
#[derive(Debug, Clone)]
pub struct ExtractedFrame {
    /// The flagged camera-frame index (matches the verdict's named frame).
    pub frame_index: u64,
    /// Where the PNG was written.
    pub png_path: std::path::PathBuf,
    /// True when this frame was flagged UNDECODABLE yet rqrr finds a CRC-valid QR
    /// in the extracted pixels — i.e. a SHARP decodable QR was counted undecodable.
    /// That is a Step-1 / #106 DECODER BUG (the proof shows a readable code), not a
    /// chain loss, and the caller MUST surface it as a regression rather than a
    /// camera-delivery fault. `false` for a genuinely black/garbage frame (real
    /// loss) and for non-undecodable (copy/gap) frames.
    pub sharp_qr_but_flagged_undecodable: bool,
}

/// Decide which flagged frames get a pixel-proof PNG, capped at `max_extract`.
///
/// PURE (no I/O) so the cap policy is unit-testable: returns the **sorted** subset
/// of `flagged` (deduped) to extract — the first `max_extract` by frame index —
/// and how many were `dropped` by the cap. `max_extract == 0` means "no cap"
/// (extract all). Capping keeps the verdict fast (#166): thousands of PNGs were a
/// large slice of the runtime and the verdict needs only a handful of examples.
pub fn select_frames_to_extract(flagged: &[u64], max_extract: usize) -> (Vec<u64>, usize) {
    let mut want: Vec<u64> = flagged.to_vec();
    want.sort_unstable();
    want.dedup();
    if max_extract == 0 || want.len() <= max_extract {
        return (want, 0);
    }
    let dropped = want.len() - max_extract;
    want.truncate(max_extract);
    (want, dropped)
}

/// Extract pixel proof for the frames the #107 verdict flagged, capped at
/// `max_extract` PNGs (the first `max_extract` flagged frames by index; `0` =
/// no cap). Re-streams the recording (the per-frame decode is not retained in
/// memory at scale) and, for each selected `frame_index`, writes its
/// native-resolution luma as a PNG into `out_dir` (`frame-<index>.png`). For
/// frames in `undecodable` it also re-decodes the extracted pixels: a CRC-valid
/// NON-BURN (optical) QR there means a SHARP optical read was wrongly counted
/// undecodable — a decoder regression, flagged on the returned [`ExtractedFrame`].
///
/// #853: this self-check used to accept ANY CRC-valid QR (node burns included) as "sharp" —
/// guaranteed true on every fleet-wide undecodable frame purely from the always-crisp,
/// always-present node burns (proven on run 1867252327: all 5879 `tick == None` stream frames
/// carried exactly the node burns and ZERO optical payload), so it never actually proved anything
/// about the cam2 optical Vernier `undecodable` measures ([`RecordingFrame::tick`] excludes node
/// burns by design — see that field's own doc). [`crate::optical_payload_check::
/// has_non_burn_payload`] mirrors `tick`'s exact same burn-exclusion filter, so the self-check and
/// the real count can never again disagree about what "found something" means.
///
/// #166: the cap stops the extraction from writing thousands of PNGs (a big slice
/// of the runtime); the number of flagged frames *dropped* by the cap is logged so
/// nothing is silently hidden. The verdict's own counts (undecodable / copy / gap)
/// remain complete — only the *visual proof* is sampled.
///
/// This is the I/O glue (ffmpeg/image), excluded from coverage like the rest of the
/// recording process boundary; the verdict ENGINE that decides which frames are
/// flagged is the pure, unit-tested `recording_verdict` module, and the cap policy
/// is the pure, unit-tested [`select_frames_to_extract`].
pub fn extract_frames_png(
    path: &Path,
    flagged: &[u64],
    undecodable: &std::collections::HashSet<u64>,
    out_dir: &Path,
    max_extract: usize,
) -> Result<Vec<ExtractedFrame>> {
    use std::collections::HashSet;
    let (selected, dropped) = select_frames_to_extract(flagged, max_extract);
    let want: HashSet<u64> = selected.iter().copied().collect();
    if want.is_empty() {
        tracing::info!("extract_frames_png: no flagged frames — nothing to extract");
        return Ok(Vec::new());
    }
    if dropped > 0 {
        tracing::warn!(
            extracting = want.len(),
            cap = max_extract,
            dropped,
            flagged_total = flagged.len(),
            "pixel-proof PNG extraction CAPPED — extracting the first {} of {} flagged frames \
             ({} not extracted; the verdict counts remain complete, only the visual proof is sampled)",
            want.len(),
            flagged.len(),
            dropped
        );
    }
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("create PNG output dir {}", out_dir.display()))?;

    let (width, height) = probe_dimensions(path)?;
    let mut extracted: Vec<ExtractedFrame> = Vec::new();
    let mut io_err: Option<anyhow::Error> = None;
    // The highest wanted index — once we pass it there is nothing left to extract,
    // so we abort the read loop early (returning false) rather than streaming the
    // whole recording. With the cap (#166) `want` is small and usually clustered
    // near the start, so this avoids re-decoding the entire file for the PNGs.
    let last_wanted = want.iter().copied().max().unwrap_or(0);
    read_frames(path, width, height, |idx, luma| {
        if io_err.is_some() {
            return false; // a prior save failed — stop reading, surface it below
        }
        if want.contains(&idx) {
            let png_path = out_dir.join(format!("frame-{idx}.png"));
            // Re-decode BEFORE moving the luma into the PNG save, only for frames the
            // verdict called undecodable: a CRC-valid NON-BURN payload here = sharp-but-
            // flagged = decoder bug. Use the SAME robust decode the verdict used (#202) so
            // the self-check matches the decoder actually in effect — not the weaker plain
            // pass. #853: filtered to non-burn ids — see this function's own doc for why
            // "any QR decoded" (node burns included) is not evidence of anything.
            let sharp_qr_but_flagged_undecodable = if undecodable.contains(&idx) {
                let recheck = decode_qr_luma_all_robust(luma.clone());
                crate::optical_payload_check::has_non_burn_payload(
                    recheck.iter().map(|p| p.run_id),
                    &NODE_BURN_RUN_IDS,
                )
            } else {
                false
            };
            if let Err(e) = luma.save_with_format(&png_path, image::ImageFormat::Png) {
                io_err = Some(anyhow::Error::new(e).context(format!(
                    "save pixel-proof PNG {} for flagged frame {idx}",
                    png_path.display()
                )));
                return false;
            }
            tracing::warn!(
                frame = idx,
                png = %png_path.display(),
                sharp_qr_but_flagged_undecodable,
                "extracted pixel proof for flagged frame"
            );
            extracted.push(ExtractedFrame {
                frame_index: idx,
                png_path,
                sharp_qr_but_flagged_undecodable,
            });
        }
        // Keep reading until we have passed the last frame we care about.
        idx < last_wanted
    })?;
    if let Some(e) = io_err {
        return Err(e);
    }
    tracing::info!(
        extracted = extracted.len(),
        requested = want.len(),
        flagged_total = flagged.len(),
        capped_dropped = dropped,
        out_dir = %out_dir.display(),
        "pixel-proof extraction complete"
    );
    Ok(extracted)
}

#[cfg(test)]
#[path = "recording_tests.rs"]
mod tests;
