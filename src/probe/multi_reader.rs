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

            // Compress the raw pixel data with lz4 (prepend_size variant so
            // decode_spool can decompress without a separate length field).
            let compressed = lz4_flex::compress_prepend_size(&frame.data);
            let clen = compressed.len() as u32;

            // Record: recv_ts_ns(i64 LE) + node_emit_tc_ns(i64 LE) +
            //         fourcc(u32 LE) + width(u32 LE) + height(u32 LE) +
            //         stride(u32 LE) + clen(u32 LE) + <clen bytes>
            writer.write_all(&recv_ts_ns.to_le_bytes())?;
            writer.write_all(&node_emit_tc_ns.to_le_bytes())?;
            writer.write_all(&frame.fourcc.to_le_bytes())?;
            writer.write_all(&frame.width.to_le_bytes())?;
            writer.write_all(&frame.height.to_le_bytes())?;
            writer.write_all(&frame.stride.to_le_bytes())?;
            writer.write_all(&clen.to_le_bytes())?;
            writer.write_all(&compressed)?;
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
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut out = Vec::new();

    let mut header = [0u8; 8 + 8 + 4 + 4 + 4 + 4 + 4]; // 36 bytes

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

        let decoded = if dual {
            decode_capture_dual(fourcc, &data, width, height, stride, decode_crop)
        } else {
            decode_capture(fourcc, &data, width, height, stride, decode_crop)
        };

        if let Some(p) = decoded {
            if p.run_id == run_id {
                out.push(Observed {
                    frame_id: p.frame_id,
                    gen_ts_ns: p.gen_ts_ns,
                    recv_ts_ns,
                    node_emit_tc_ns,
                });
            }
        }
    }

    Ok(out)
}
