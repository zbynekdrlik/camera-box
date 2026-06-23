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
//!   decode_qr_luma_all  (the EXISTING rqrr decoder in src/probe/qr.rs)
//!        │  every CRC-valid Payload in the frame (both dual-QR halves in ONE pass)
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
use crate::probe::qr::decode_qr_luma_all;
use anyhow::{Context, Result};
use image::GrayImage;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// Upper bound on decode worker threads. The rqrr per-frame decode is the
/// CPU-bound bottleneck; beyond ~8 threads the ffmpeg I/O producer (one
/// sequential gray8 pipe) becomes the limit and more workers just contend.
const MAX_DECODE_WORKERS: usize = 8;

/// Default cap on how many flagged frames get a pixel-proof PNG written. The
/// verdict only needs a handful of visual examples per category; extracting a
/// PNG for thousands of flagged frames was a large slice of the #166 runtime
/// (re-stream + per-frame PNG encode) and is never needed to read the verdict.
/// The count of frames *dropped* by the cap is logged so nothing is hidden.
pub const DEFAULT_MAX_PIXEL_PROOF: usize = 30;

/// One analyzed frame of a recording, in file (capture) order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingFrame {
    /// 0-based index of this frame within the recording.
    pub frame_index: u64,
    /// Every CRC-valid QR payload found in the frame (both dual-QR halves, decoded
    /// in one rqrr pass). Empty when no readable QR is present in the frame.
    pub payloads: Vec<Payload>,
    /// Effective Vernier tick = the highest `frame_id` among the decoded payloads
    /// (left QR carries the latest even tick, right the latest odd; the freshest
    /// sharp region wins — matches `decode_capture_dual`'s `max_by_key(frame_id)`).
    /// `None` when nothing decoded.
    pub tick: Option<u32>,
}

/// Decode one native-resolution luma frame into a `RecordingFrame`.
///
/// PURE (no I/O, no ffmpeg): feeds the luma image straight into the existing rqrr
/// decoder, which finds BOTH side-by-side dual-QR codes in one prepare+detect pass.
/// `frame_index` is the caller-supplied position in the recording.
pub fn decode_recording_frame(frame_index: u64, luma: GrayImage) -> RecordingFrame {
    let payloads = decode_qr_luma_all(luma);
    let tick = payloads.iter().map(|p| p.frame_id).max();
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
/// early (the consumer can no longer accept frames — e.g. the parallel decode's
/// worker pool has died). On an early abort `read_frames` returns the count
/// emitted so far; the caller distinguishes "all frames read" (count == decoded)
/// from "aborted" via its own bookkeeping. ffmpeg is killed on abort so it does
/// not linger writing to a closed pipe.
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

/// Number of decode worker threads to use (= available CPUs, clamped to
/// `1..=MAX_DECODE_WORKERS`). Mirrors the #160 spool-decode pool sizing.
///
/// `CAMERA_BOX_DECODE_WORKERS` overrides the auto value (still clamped to the
/// `1..=MAX_DECODE_WORKERS` range) — useful to pin a count on a shared runner or
/// to A/B the speedup (`=1` reproduces the prior single-threaded loop). A
/// non-numeric or zero value is ignored (falls back to auto).
fn decode_workers() -> usize {
    if let Ok(v) = std::env::var("CAMERA_BOX_DECODE_WORKERS") {
        if let Ok(n) = v.trim().parse::<usize>() {
            if n >= 1 {
                return n.clamp(1, MAX_DECODE_WORKERS);
            }
        }
    }
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
    produce: impl FnOnce(&mut dyn FnMut(u64, GrayImage) -> bool) -> Result<u64>,
) -> Result<Vec<RecordingFrame>> {
    let (job_tx, job_rx) = std::sync::mpsc::sync_channel::<(u64, GrayImage)>(workers * 2);
    let job_rx = Arc::new(Mutex::new(job_rx));
    let (res_tx, res_rx) = std::sync::mpsc::channel::<RecordingFrame>();

    let mut handles = Vec::with_capacity(workers);
    for _ in 0..workers {
        let job_rx = job_rx.clone();
        let res_tx = res_tx.clone();
        handles.push(std::thread::spawn(move || loop {
            // Lock only to pull the next job; release before the heavy decode so
            // workers don't serialise on the mutex (the #160 pattern).
            let job = {
                let rx = job_rx.lock().unwrap();
                rx.recv()
            };
            let Ok((idx, luma)) = job else { break };
            let f = decode_recording_frame(idx, luma);
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
pub fn analyze_recording(path: &Path) -> Result<Vec<RecordingFrame>> {
    let (width, height) = probe_dimensions(path)?;
    let workers = decode_workers();
    tracing::info!(
        file = %path.display(), width, height, workers,
        "recording analysis start (parallel decode)"
    );
    // Emit a periodic frame-count HEARTBEAT at info level (every PROGRESS_EVERY
    // frames the producer reads). This is the liveness signal the harness stall
    // detector watches (scripts/verdict-monitor.sh): without it, a single large
    // recording decodes SILENTLY between its start/complete logs, so a long clip
    // could exceed the stall timeout and be falsely killed. The heartbeat keeps the
    // output growing during the decode so the detector sees real progress — the
    // deep fix, not a band-aid timeout bump (no-timeout-band-aids.md).
    const PROGRESS_EVERY: u64 = 1000;
    let frames = decode_stream_parallel(workers, |emit| {
        read_frames(path, width, height, |idx, luma| {
            if idx > 0 && idx % PROGRESS_EVERY == 0 {
                tracing::info!(file = %path.display(), frames_read = idx, "recording decode progress");
            }
            emit(idx, luma)
        })
    })?;
    let decoded: usize = frames.iter().filter(|f| !f.payloads.is_empty()).count();
    tracing::info!(
        file = %path.display(), total = frames.len(), with_qr = decoded, workers,
        "recording analysis complete"
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
/// QR there means a SHARP code was wrongly counted undecodable — a decoder
/// regression, flagged on the returned [`ExtractedFrame`].
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
            // verdict called undecodable: a CRC-valid payload here = sharp-but-flagged
            // = decoder bug.
            let sharp_qr_but_flagged_undecodable = if undecodable.contains(&idx) {
                !decode_qr_luma_all(luma.clone()).is_empty()
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
mod tests {
    use super::*;
    use crate::probe::luma::bgra_to_luma;
    use crate::probe::qr::render_qr_dual_bgra;

    fn dual_qr_luma(left_id: u32, right_id: u32) -> GrayImage {
        let (cw, ch, qs) = (960u32, 540u32, 260u32);
        let l = Payload {
            run_id: 6519,
            frame_id: left_id,
            gen_ts_ns: 1,
        };
        let r = Payload {
            run_id: 6519,
            frame_id: right_id,
            gen_ts_ns: 2,
        };
        let bgra = render_qr_dual_bgra(&l, &r, cw, ch, qs);
        bgra_to_luma(&bgra, cw, ch, cw * 4)
    }

    #[test]
    fn decode_recording_frame_returns_both_dual_qrs() {
        // A strih-6519-type frame (two sharp QRs) decodes to BOTH — the exact case
        // opencv silently returned 0/1 on.
        let f = decode_recording_frame(0, dual_qr_luma(6518, 6519));
        assert_eq!(f.frame_index, 0);
        assert_eq!(f.payloads.len(), 2, "both QRs decode: {:?}", f.payloads);
        let ids: Vec<u32> = f.payloads.iter().map(|p| p.frame_id).collect();
        assert!(ids.contains(&6518) && ids.contains(&6519), "ids {ids:?}");
    }

    #[test]
    fn tick_is_max_frame_id() {
        // Effective Vernier tick = max(left, right).
        let f = decode_recording_frame(7, dual_qr_luma(200, 201));
        assert_eq!(f.frame_index, 7);
        assert_eq!(f.tick, Some(201));
    }

    #[test]
    fn blank_frame_decodes_to_zero_and_none_tick() {
        let blank = GrayImage::from_raw(640, 480, vec![255u8; 640 * 480]).unwrap();
        let f = decode_recording_frame(3, blank);
        assert!(f.payloads.is_empty(), "no QR in a blank frame");
        assert_eq!(f.tick, None);
    }

    #[test]
    fn deterministic_same_frame_same_result() {
        // STRICT acceptance: same frame → same result.
        let a = decode_recording_frame(0, dual_qr_luma(10, 11));
        let b = decode_recording_frame(0, dual_qr_luma(10, 11));
        assert_eq!(a, b);
    }

    // ---- #166: parallel decode (order-preserving across the worker pool) ----

    #[test]
    fn parallel_decode_preserves_capture_order_and_decodes_every_frame() {
        // The parallel decoder fans frames across N workers that complete OUT of
        // order, then re-sorts by frame_index. With >1 worker, a sequence of real
        // dual-QR frames must come back in EXACT capture order with EVERY frame
        // decoded — proving the reorder logic, not just that it ran. (The single
        // -threaded loop trivially preserved order; this is the regression guard
        // that the parallelization did not corrupt it.)
        let n: u64 = 60;
        let frames = decode_stream_parallel(4, |emit| {
            for i in 0..n {
                // left = even tick 2i, right = odd tick 2i+1 → tick = 2i+1, unique
                // per frame so an order bug is detectable.
                emit(i, dual_qr_luma(2 * i as u32, 2 * i as u32 + 1));
            }
            Ok(n)
        })
        .expect("parallel decode");

        assert_eq!(frames.len() as u64, n, "every frame returned");
        for (i, f) in frames.iter().enumerate() {
            assert_eq!(f.frame_index, i as u64, "frames in capture order at {i}");
            assert_eq!(
                f.tick,
                Some(2 * i as u32 + 1),
                "frame {i} decoded its own ticks"
            );
            assert_eq!(f.payloads.len(), 2, "both QRs decoded for frame {i}");
        }
    }

    #[test]
    fn parallel_decode_matches_single_threaded_result_exactly() {
        // Byte-for-byte equivalence with the serial reference for the SAME input —
        // the parallelization must not change the decode result, only its speed.
        let n: u64 = 24;
        let make = |i: u64| dual_qr_luma(7 * i as u32, 7 * i as u32 + 1);

        let serial: Vec<RecordingFrame> =
            (0..n).map(|i| decode_recording_frame(i, make(i))).collect();

        // workers=1 and workers=4 must both equal the serial reference.
        for w in [1usize, 4] {
            let par = decode_stream_parallel(w, |emit| {
                for i in 0..n {
                    emit(i, make(i));
                }
                Ok(n)
            })
            .unwrap();
            assert_eq!(par, serial, "parallel (workers={w}) == single-threaded");
        }
    }

    #[test]
    fn parallel_decode_propagates_producer_error() {
        // A producer (ffmpeg/ffprobe) error must surface as an Err, not a silent
        // short read — otherwise a truncated decode could be read as "0 loss".
        let r = decode_stream_parallel(4, |emit| {
            emit(0, dual_qr_luma(0, 1));
            anyhow::bail!("simulated ffmpeg failure");
        });
        assert!(r.is_err(), "producer error propagates");
    }

    // ---- #166: pixel-proof PNG cap (pure policy) ----

    #[test]
    fn select_frames_caps_to_first_n_and_reports_dropped() {
        let flagged: Vec<u64> = (0..100).collect();
        let (sel, dropped) = select_frames_to_extract(&flagged, 30);
        assert_eq!(sel.len(), 30, "capped to N");
        assert_eq!(sel, (0..30).collect::<Vec<u64>>(), "first N by index");
        assert_eq!(dropped, 70, "dropped count = total - N");
    }

    #[test]
    fn select_frames_no_cap_when_under_limit_or_zero() {
        let flagged = vec![5u64, 1, 9, 3];
        // Under the limit: all kept (sorted, deduped), 0 dropped.
        let (sel, dropped) = select_frames_to_extract(&flagged, 30);
        assert_eq!(sel, vec![1, 3, 5, 9]);
        assert_eq!(dropped, 0);
        // max=0 means "no cap": all kept even when large.
        let big: Vec<u64> = (0..500).collect();
        let (sel0, dropped0) = select_frames_to_extract(&big, 0);
        assert_eq!(sel0.len(), 500);
        assert_eq!(dropped0, 0);
    }

    #[test]
    fn select_frames_dedups_before_capping() {
        // Duplicate flagged indices must not consume cap slots twice.
        let flagged = vec![1u64, 1, 2, 2, 3, 3, 4, 4];
        let (sel, dropped) = select_frames_to_extract(&flagged, 3);
        assert_eq!(sel, vec![1, 2, 3]);
        assert_eq!(dropped, 1, "4 unique - 3 cap = 1 dropped");
    }
}
