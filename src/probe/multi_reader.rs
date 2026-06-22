//! Multi-source NDI reader: one `NdiReceiver` + thread per tap. All taps share
//! one `Instant` start, so every `recv_ts_ns` is on dev1's single monotonic
//! clock — which makes the differ's per-hop latency (Δ recv_ts) a valid
//! single-clock measurement. Hardware glue: excluded from coverage/mutants.

use crate::ndi::NdiReceiver;
use crate::probe::analyzer::Observed;
use crate::probe::clock_ns;
use crate::probe::qr::{decode_capture, decode_capture_dual};
use anyhow::Result;
use std::io::{BufReader, BufWriter, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

/// One tap: a named NDI source to subscribe to, filtered to `run_id`.
pub struct TapSpec {
    pub name: String,
    pub source: String,
    pub run_id: u32,
    pub connect_timeout_secs: u32,
    /// Side of the centered square the QR decode is restricted to (ROI speed fix).
    pub decode_crop: u32,
    /// Clock domain for `recv_ts_ns`. `false` (default) ⇒ dev1's shared monotonic
    /// `Instant` — correct for per-hop RELATIVE latency (recv−recv between two
    /// taps on this one machine). `true` ⇒ CLOCK_REALTIME epoch ns — required for
    /// ABSOLUTE end-to-end latency (recv(endpoint) − gen(source)), which is only
    /// sound when the painter's `gen_ts` and this `recv_ts` share the
    /// DanteSync-disciplined wall clock (#7 / #8, strih = master). Per-hop
    /// relative latency stays valid either way (both taps use the same domain).
    pub wall_clock: bool,
    /// When true, use `decode_capture_dual` (picks the highest frame_id CRC-valid
    /// half from a side-by-side dual-QR frame). Matches the painter's `dual_qr`
    /// flag on the camera's frame-probe run.
    pub dual: bool,
    /// When `Some(path)`, capture every frame to disk with NO live QR decoding and
    /// NO `observed` updates during the run; decode_spool is called AFTER the taps
    /// join so the NDI receiver is never stalled by QR decode latency. When `None`
    /// the existing live-decode path is used (behaviour unchanged).
    pub spool: Option<String>,
}

/// A tap's accumulating buffer, readable by the differ after the run.
pub struct TapResult {
    pub name: String,
    pub observed: Arc<Mutex<Vec<Observed>>>,
    /// Every NDI frame this tap pulled off the wire, decoded or not. `captured`
    /// minus the run_id-matching decoded count is the tap's decode-miss floor —
    /// frames that physically ARRIVED but did not yield a matching-run_id QR:
    /// torn by NDI compression/resample, or (≈0 in a single-run probe) a QR from
    /// a different run_id. Without this, a torn frame is indistinguishable from a
    /// frame the hop genuinely dropped, so the differ's `dropped_ids` would
    /// over-report loss. Capture-count parity across a hop proves the hop
    /// delivered every frame even when some ids fail to decode at the tap.
    pub captured: Arc<AtomicU64>,
    /// Set once `NdiReceiver::connect` returns for this tap. Until it is set the
    /// tap is still discovering/connecting to its NDI source (up to
    /// `connect_timeout_secs`) and has had NO chance to capture, so its
    /// `captured == 0` is "not connected yet", NOT "dead output". The #81
    /// liveness pre-check waits for every tap to be `connected` before it starts
    /// the capture window, so a healthy-but-slow-to-discover tap can never be
    /// mis-flagged as a dead downstream output / GPU device-removed.
    pub connected: Arc<AtomicBool>,
}

/// Spawn a reader thread for one tap. Returns its join handle plus a handle to
/// its observed buffer. The thread runs until `stop` is set.
pub fn spawn_tap(
    spec: TapSpec,
    start: Instant,
    stop: Arc<AtomicBool>,
) -> (JoinHandle<Result<()>>, TapResult) {
    let observed: Arc<Mutex<Vec<Observed>>> = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::new(AtomicU64::new(0));
    let connected = Arc::new(AtomicBool::new(false));
    let result = TapResult {
        name: spec.name.clone(),
        observed: observed.clone(),
        captured: captured.clone(),
        connected: connected.clone(),
    };
    let handle =
        std::thread::spawn(move || tap_loop(spec, start, stop, observed, captured, connected));
    (handle, result)
}

fn tap_loop(
    spec: TapSpec,
    start: Instant,
    stop: Arc<AtomicBool>,
    observed: Arc<Mutex<Vec<Observed>>>,
    captured: Arc<AtomicU64>,
    connected: Arc<AtomicBool>,
) -> Result<()> {
    let mut rx = NdiReceiver::connect(&spec.source, spec.connect_timeout_secs)?;
    // The NDI source was found and the receiver is up: from here the tap is
    // capturing, so a subsequent `captured == 0` is a dead output, not a tap
    // that simply hadn't connected yet (#81 liveness pre-check gate).
    connected.store(true, Ordering::Relaxed);

    // Spool-mode: open the spool file once, write raw lz4-compressed frames to it,
    // skip QR decoding entirely so the NDI receiver is never stalled.
    if let Some(ref path) = spec.spool {
        let file = std::fs::File::create(path)?;
        let mut writer = BufWriter::new(file);

        while !stop.load(Ordering::Relaxed) {
            let frame = match rx.capture_frame(100)? {
                Some(f) => f,
                None => continue,
            };
            let recv_ts_ns = clock_ns(start, spec.wall_clock);
            let node_emit_tc_ns = frame.timecode_100ns.saturating_mul(100);
            // Count BEFORE compression so captured is always the raw-arrival count.
            captured.fetch_add(1, Ordering::Relaxed);

            write_spool_record(
                &mut writer,
                recv_ts_ns,
                node_emit_tc_ns,
                frame.fourcc,
                frame.width,
                frame.height,
                frame.stride,
                &frame.data,
            )?;
        }

        writer.flush()?;
        let total = captured.load(Ordering::Relaxed);
        tracing::info!(
            tap = %spec.name, source = %spec.source,
            captured = total,
            "tap finished (spool mode — decode deferred)"
        );
        return Ok(());
    }

    // Live-decode path (unchanged behaviour when spool is None).
    while !stop.load(Ordering::Relaxed) {
        let frame = match rx.capture_frame(100)? {
            Some(f) => f,
            None => continue,
        };
        let recv_ts_ns = clock_ns(start, spec.wall_clock);
        // Per-node EMIT time stamped by THIS tap's source (NDI timecode, 100ns
        // units since epoch) -> ns. 0 stays 0 (source did not stamp a usable
        // timecode). saturating_mul guards the SDK sentinel (INT64_MAX) if it
        // ever surfaces on a received frame.
        let node_emit_tc_ns = frame.timecode_100ns.saturating_mul(100);
        // Count every frame that physically arrived BEFORE attempting QR decode,
        // so a torn-QR frame still increments `captured`. This is what separates
        // hop frame-loss from tap decode-failure.
        captured.fetch_add(1, Ordering::Relaxed);
        let decoded = if spec.dual {
            decode_capture_dual(
                frame.fourcc,
                &frame.data,
                frame.width,
                frame.height,
                frame.stride,
                spec.decode_crop,
            )
        } else {
            decode_capture(
                frame.fourcc,
                &frame.data,
                frame.width,
                frame.height,
                frame.stride,
                spec.decode_crop,
            )
        };
        if let Some(p) = decoded {
            if p.run_id == spec.run_id {
                observed.lock().unwrap().push(Observed {
                    frame_id: p.frame_id,
                    gen_ts_ns: p.gen_ts_ns,
                    recv_ts_ns,
                    node_emit_tc_ns,
                });
            }
        }
    }
    let total = captured.load(Ordering::Relaxed);
    let decoded = observed.lock().unwrap().len();
    tracing::info!(
        tap = %spec.name, source = %spec.source,
        captured = total, decoded = decoded,
        decode_failed = total.saturating_sub(decoded as u64),
        "tap finished"
    );
    Ok(())
}

/// Serialize ONE spooled frame record to `writer` in the spool wire format,
/// lz4-compressing the pixel data. The single source of truth for the on-disk
/// layout that [`decode_spool`] reads back — so the round-trip (store→load→decode)
/// is exercised by ONE format definition, testable without an NDI receiver (#160).
///
/// Record (all little-endian):
/// `recv_ts_ns(i64) node_emit_tc_ns(i64) fourcc(u32) width(u32) height(u32)
///  stride(u32) clen(u32) <clen bytes of lz4 compress_prepend_size(data)>`.
///
/// `fourcc`/`width`/`height`/`stride` are stored PER frame and honored on decode,
/// so a tap whose NDI source emits BGRX/BGRA (the OBS DistroAV output) round-trips
/// exactly as a UYVY camera tap does — no single assumed format.
#[allow(clippy::too_many_arguments)]
pub fn write_spool_record<W: Write>(
    writer: &mut W,
    recv_ts_ns: i64,
    node_emit_tc_ns: i64,
    fourcc: u32,
    width: u32,
    height: u32,
    stride: u32,
    data: &[u8],
) -> std::io::Result<()> {
    // Compress the raw pixel data with lz4 (prepend_size variant so decode_spool
    // can decompress without a separate uncompressed-length field).
    let compressed = lz4_flex::compress_prepend_size(data);
    let clen = compressed.len() as u32;

    writer.write_all(&recv_ts_ns.to_le_bytes())?;
    writer.write_all(&node_emit_tc_ns.to_le_bytes())?;
    writer.write_all(&fourcc.to_le_bytes())?;
    writer.write_all(&width.to_le_bytes())?;
    writer.write_all(&height.to_le_bytes())?;
    writer.write_all(&stride.to_le_bytes())?;
    writer.write_all(&clen.to_le_bytes())?;
    writer.write_all(&compressed)?;
    Ok(())
}

/// Decode a spool file written by `tap_loop` in spool mode. Reads every record,
/// decompresses the pixel data with lz4, decodes the QR payload, and returns
/// all observations whose `run_id` matches the caller's `run_id`.
///
/// Call AFTER all tap threads have joined (i.e. the spool file is complete) and
/// BEFORE the snapshot/trim step so the differ sees a fully-populated
/// `observed` vector.
pub fn decode_spool(
    path: &str,
    run_id: u32,
    dual: bool,
    decode_crop: u32,
) -> anyhow::Result<Vec<Observed>> {
    // #160: ONE I/O thread (this function) reads+lz4-decompresses records
    // sequentially off the single spool file and hands each raw frame to a pool of
    // `workers` decode threads over a BOUNDED channel. The QR decode (full-frame
    // luma + rqrr) is the ~0.06 s/frame bottleneck and is embarrassingly parallel;
    // the prior single-threaded loop took ~7 min PER tap (≈21 min for the 3-tap
    // 300 s proof), long enough that a constrained run could read the result before
    // the OBS-output taps had finished decoding and mistake "still decoding" for
    // "decode=0" (the #160 symptom). With N workers it is ~7 min / N. The bounded
    // channel caps in-flight decompressed frames (each ~8 MB) so a 9000-frame spool
    // never materialises all frames in RAM at once.
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 8);

    // Job = one decompressed frame ready to decode. Result carries the decode
    // outcome plus the tally inputs so the summary stays accurate.
    struct Job {
        recv_ts_ns: i64,
        node_emit_tc_ns: i64,
        fourcc: u32,
        width: u32,
        height: u32,
        stride: u32,
        data: Vec<u8>,
    }
    enum DecodeOutcome {
        Matched(Observed),
        RunIdMismatch,
        DecodeFailed,
    }

    let (job_tx, job_rx) = std::sync::mpsc::sync_channel::<Job>(workers * 2);
    let job_rx = Arc::new(Mutex::new(job_rx));
    let (res_tx, res_rx) = std::sync::mpsc::channel::<DecodeOutcome>();

    let mut worker_handles = Vec::with_capacity(workers);
    for _ in 0..workers {
        let job_rx = job_rx.clone();
        let res_tx = res_tx.clone();
        worker_handles.push(std::thread::spawn(move || loop {
            // Lock only to pull the next job; release before the heavy decode so
            // workers don't serialise on the mutex.
            let job = {
                let rx = job_rx.lock().unwrap();
                rx.recv()
            };
            let Ok(job) = job else { break };
            let decoded = if dual {
                decode_capture_dual(
                    job.fourcc,
                    &job.data,
                    job.width,
                    job.height,
                    job.stride,
                    decode_crop,
                )
            } else {
                decode_capture(
                    job.fourcc,
                    &job.data,
                    job.width,
                    job.height,
                    job.stride,
                    decode_crop,
                )
            };
            let outcome = match decoded {
                Some(p) if p.run_id == run_id => DecodeOutcome::Matched(Observed {
                    frame_id: p.frame_id,
                    gen_ts_ns: p.gen_ts_ns,
                    recv_ts_ns: job.recv_ts_ns,
                    node_emit_tc_ns: job.node_emit_tc_ns,
                }),
                Some(_) => DecodeOutcome::RunIdMismatch,
                None => DecodeOutcome::DecodeFailed,
            };
            // A closed result channel means the collector is gone (shouldn't
            // happen before all jobs drain); stop rather than spin.
            if res_tx.send(outcome).is_err() {
                break;
            }
        }));
    }
    drop(res_tx); // only the workers' clones remain; res_rx ends when all exit

    // Collector thread: drains decode results into the output vector and tallies.
    // Runs concurrently with the reader so workers never block on a full result
    // queue. Returns the ordered-by-completion observations (the caller sorts by
    // frame_id downstream; order here is irrelevant to the differ).
    let collector = std::thread::spawn(move || {
        let mut out: Vec<Observed> = Vec::new();
        let mut decoded_ok: u64 = 0;
        let mut run_id_mismatch: u64 = 0;
        let mut decode_failed: u64 = 0;
        while let Ok(o) = res_rx.recv() {
            match o {
                DecodeOutcome::Matched(obs) => {
                    decoded_ok += 1;
                    out.push(obs);
                }
                DecodeOutcome::RunIdMismatch => run_id_mismatch += 1,
                DecodeOutcome::DecodeFailed => decode_failed += 1,
            }
        }
        (out, decoded_ok, run_id_mismatch, decode_failed)
    });

    // Reader: sequential I/O + lz4 decompress, feeding the worker pool.
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut header = [0u8; 8 + 8 + 4 + 4 + 4 + 4 + 4]; // 36 bytes

    // Diagnostics (#160): the spool offline-decode silently returned 0 for the
    // OBS-output taps while the SAME chain decoded 100% live. Log the per-record
    // format/dims and a decode tally so a future 0-decode spool is debuggable from
    // the run output alone, not a redeploy. Cheap: a handful of logged records +
    // one summary line per tap.
    let mut records: u64 = 0;
    let mut fmt_seen: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();

    let read_result: anyhow::Result<()> = (|| {
        loop {
            // Try to read the fixed header; a clean EOF at a record boundary is the
            // normal end-of-file condition.
            match reader.read_exact(&mut header) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }

            let recv_ts_ns = i64::from_le_bytes(header[0..8].try_into().unwrap());
            let node_emit_tc_ns = i64::from_le_bytes(header[8..16].try_into().unwrap());
            let fourcc = u32::from_le_bytes(header[16..20].try_into().unwrap());
            let width = u32::from_le_bytes(header[20..24].try_into().unwrap());
            let height = u32::from_le_bytes(header[24..28].try_into().unwrap());
            let stride = u32::from_le_bytes(header[28..32].try_into().unwrap());
            let clen = u32::from_le_bytes(header[32..36].try_into().unwrap()) as usize;

            let mut buf = vec![0u8; clen];
            reader.read_exact(&mut buf)?;

            let data = lz4_flex::decompress_size_prepended(&buf)?;

            // Record what we actually read off disk, for the first few records and a
            // running per-format tally. fourcc as its 4 ASCII bytes (the NDI FourCC).
            let fourcc_str: String = fourcc
                .to_le_bytes()
                .iter()
                .map(|&b| if b.is_ascii_graphic() { b as char } else { '?' })
                .collect();
            if records < 3 {
                tracing::info!(
                    spool = %path,
                    record = records,
                    fourcc = %fourcc_str,
                    fourcc_hex = format_args!("0x{fourcc:08x}"),
                    width,
                    height,
                    stride,
                    data_len = data.len(),
                    expected_uyvy = (stride as usize) * (height as usize),
                    "#160 spool record"
                );
            }
            *fmt_seen
                .entry(format!("{fourcc_str}/{width}x{height}/s{stride}"))
                .or_insert(0) += 1;
            records += 1;

            // Hand off to the worker pool. A send error means every worker exited
            // early (decode panic) — stop reading and surface it via the join below.
            if job_tx
                .send(Job {
                    recv_ts_ns,
                    node_emit_tc_ns,
                    fourcc,
                    width,
                    height,
                    stride,
                    data,
                })
                .is_err()
            {
                break;
            }
        }
        Ok(())
    })();

    // Close the job channel so workers drain and exit, then join everyone.
    drop(job_tx);
    for h in worker_handles {
        // A worker panic (e.g. malformed frame) must surface, not hang the run.
        h.join()
            .map_err(|_| anyhow::anyhow!("spool decode worker panicked"))?;
    }
    let (out, decoded_ok, run_id_mismatch, decode_failed) = collector
        .join()
        .map_err(|_| anyhow::anyhow!("spool decode collector panicked"))?;
    // Propagate any reader error only AFTER draining workers, so a partial spool
    // still reports what it decoded in the summary below before erroring out.
    read_result?;

    tracing::info!(
        spool = %path,
        records,
        decoded_ok,
        run_id_mismatch,
        decode_failed,
        workers,
        formats = ?fmt_seen,
        "#160 spool decode summary"
    );

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::payload::Payload;
    use crate::probe::qr::{render_qr_bgra, render_qr_dual_bgra};
    use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};

    /// One frame to spool in a test: its recv_ts plus its raw pixel bytes + format.
    struct TestFrame<'a> {
        recv_ts_ns: i64,
        fourcc: u32,
        data: &'a [u8],
        width: u32,
        height: u32,
        stride: u32,
    }

    /// Build a spool file in a temp dir from raw frame bytes, then `decode_spool`
    /// it back. Returns the decoded observations. Exercises the EXACT on-disk
    /// round-trip (`write_spool_record` → `decode_spool`) with no NDI receiver.
    fn roundtrip(frames: &[TestFrame], run_id: u32, dual: bool, decode_crop: u32) -> Vec<Observed> {
        // Unique per-call dir so parallel tests never share a spool file.
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "spool160_test_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, AtomicOrdering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tap.bin");
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut w = BufWriter::new(file);
            for (i, f) in frames.iter().enumerate() {
                write_spool_record(
                    &mut w,
                    f.recv_ts_ns,
                    1000 + i as i64, // node_emit_tc_ns — arbitrary, preserved through round-trip
                    f.fourcc,
                    f.width,
                    f.height,
                    f.stride,
                    f.data,
                )
                .unwrap();
            }
            w.flush().unwrap();
        }
        let out = decode_spool(path.to_str().unwrap(), run_id, dual, decode_crop).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        out
    }

    /// #160 core: a NON-UYVY (BGRX, the OBS DistroAV output format) single-QR frame
    /// must round-trip store→load→decode and yield its payload. This is the
    /// regression the issue is about — the spool path must honor the per-frame
    /// format, not assume the camera's UYVY.
    #[test]
    fn spool_roundtrips_bgrx_single_qr_frame() {
        let p = Payload {
            run_id: 42,
            frame_id: 12345,
            gen_ts_ns: 9_000_000,
        };
        let (w, h, qs) = (1920u32, 1080u32, 700u32);
        let bgra = render_qr_bgra(&p, w, h, qs); // BGRA byte layout == BGRX for luma
        let fourcc = u32::from_le_bytes(*b"BGRX");
        let decode_crop = (qs + 120).min(1080);
        let out = roundtrip(
            &[TestFrame {
                recv_ts_ns: 111,
                fourcc,
                data: &bgra,
                width: w,
                height: h,
                stride: w * 4,
            }],
            p.run_id,
            false,
            decode_crop,
        );
        assert_eq!(out.len(), 1, "BGRX frame must decode from spool");
        assert_eq!(out[0].frame_id, p.frame_id);
        assert_eq!(out[0].gen_ts_ns, p.gen_ts_ns);
        assert_eq!(
            out[0].recv_ts_ns, 111,
            "recv_ts must survive the round-trip"
        );
        assert_eq!(
            out[0].node_emit_tc_ns, 1000,
            "emit_tc must survive the round-trip"
        );
    }

    /// #160: a NON-UYVY (BGRX) DUAL-QR frame (the dual-QR Vernier path used by the
    /// real cam→strih→stream proof) must round-trip and decode the highest-frame_id
    /// half — exactly the path that returned 0 for the OBS-output taps in the issue.
    #[test]
    fn spool_roundtrips_bgrx_dual_qr_frame() {
        let left = Payload {
            run_id: 7,
            frame_id: 200,
            gen_ts_ns: 1,
        };
        let right = Payload {
            run_id: 7,
            frame_id: 201,
            gen_ts_ns: 2,
        };
        let (w, h, qs) = (1920u32, 1080u32, 520u32);
        let bgra = render_qr_dual_bgra(&left, &right, w, h, qs);
        let fourcc = u32::from_le_bytes(*b"BGRX");
        let decode_crop = (qs + 120).min(1080);
        let out = roundtrip(
            &[TestFrame {
                recv_ts_ns: 222,
                fourcc,
                data: &bgra,
                width: w,
                height: h,
                stride: w * 4,
            }],
            7,
            true,
            decode_crop,
        );
        assert_eq!(out.len(), 1, "BGRX dual-QR frame must decode from spool");
        // dual decode reconciles to the highest frame_id half.
        assert_eq!(out[0].frame_id, 201);
    }

    /// #160: a mixed-format spool (a UYVY camera frame AND a BGRX OBS-output frame
    /// in the SAME file) must decode BOTH — proving the decode honors each frame's
    /// stored format rather than assuming one. This is the exact failure mode named
    /// in the issue: cam (UYVY) decoded while strih/stream (OBS output) did not.
    #[test]
    fn spool_decodes_mixed_uyvy_and_bgrx_frames() {
        let p_uyvy = Payload {
            run_id: 9,
            frame_id: 1,
            gen_ts_ns: 10,
        };
        let p_bgrx = Payload {
            run_id: 9,
            frame_id: 2,
            gen_ts_ns: 20,
        };
        let (w, h, qs) = (1920u32, 1080u32, 700u32);
        let decode_crop = (qs + 120).min(1080);

        // BGRX frame straight from the renderer.
        let bgrx = render_qr_bgra(&p_bgrx, w, h, qs);

        // UYVY frame: render BGRA then pack to UYVY (luma in the odd byte) so a real
        // 2-byte/px camera frame is exercised, not a re-labelled BGRA buffer.
        let bgra = render_qr_bgra(&p_uyvy, w, h, qs);
        let mut uyvy = vec![0u8; (w * h * 2) as usize];
        for i in 0..(w * h) as usize {
            let lum = bgra[i * 4]; // B==G==R on the gray QR canvas, so any channel is luma
            uyvy[i * 2] = 128; // U/V chroma (neutral)
            uyvy[i * 2 + 1] = lum; // Y
        }

        let out = roundtrip(
            &[
                TestFrame {
                    recv_ts_ns: 1,
                    fourcc: u32::from_le_bytes(*b"UYVY"),
                    data: &uyvy,
                    width: w,
                    height: h,
                    stride: w * 2,
                },
                TestFrame {
                    recv_ts_ns: 2,
                    fourcc: u32::from_le_bytes(*b"BGRX"),
                    data: &bgrx,
                    width: w,
                    height: h,
                    stride: w * 4,
                },
            ],
            9,
            false,
            decode_crop,
        );
        let mut ids: Vec<u32> = out.iter().map(|o| o.frame_id).collect();
        ids.sort_unstable();
        assert_eq!(
            ids,
            vec![1, 2],
            "both UYVY and BGRX frames must decode from one spool"
        );
    }

    /// A run_id that does not match is excluded (the differ must not see foreign
    /// runs). Guards the run_id filter in the parallel decode path.
    #[test]
    fn spool_excludes_foreign_run_id() {
        let p = Payload {
            run_id: 1,
            frame_id: 5,
            gen_ts_ns: 1,
        };
        let (w, h, qs) = (1920u32, 1080u32, 700u32);
        let bgra = render_qr_bgra(&p, w, h, qs);
        let fourcc = u32::from_le_bytes(*b"BGRX");
        let decode_crop = (qs + 120).min(1080);
        // Spool carries run_id=1 frames, but we decode asking for run_id=2.
        let out = roundtrip(
            &[TestFrame {
                recv_ts_ns: 1,
                fourcc,
                data: &bgra,
                width: w,
                height: h,
                stride: w * 4,
            }],
            2,
            false,
            decode_crop,
        );
        assert!(
            out.is_empty(),
            "frames from a different run_id must be excluded"
        );
    }
}
