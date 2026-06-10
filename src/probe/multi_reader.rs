//! Multi-source NDI reader: one `NdiReceiver` + thread per tap. All taps share
//! one `Instant` start, so every `recv_ts_ns` is on dev1's single monotonic
//! clock — which makes the differ's per-hop latency (Δ recv_ts) a valid
//! single-clock measurement. Hardware glue: excluded from coverage/mutants.

use crate::ndi::NdiReceiver;
use crate::probe::analyzer::Observed;
use crate::probe::qr::decode_capture;
use anyhow::Result;
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
    let result = TapResult {
        name: spec.name.clone(),
        observed: observed.clone(),
        captured: captured.clone(),
    };
    let handle = std::thread::spawn(move || tap_loop(spec, start, stop, observed, captured));
    (handle, result)
}

fn tap_loop(
    spec: TapSpec,
    start: Instant,
    stop: Arc<AtomicBool>,
    observed: Arc<Mutex<Vec<Observed>>>,
    captured: Arc<AtomicU64>,
) -> Result<()> {
    let mut rx = NdiReceiver::connect(&spec.source, spec.connect_timeout_secs)?;
    while !stop.load(Ordering::Relaxed) {
        let frame = match rx.capture_frame(100)? {
            Some(f) => f,
            None => continue,
        };
        let recv_ts_ns = start.elapsed().as_nanos() as i64;
        // Count every frame that physically arrived BEFORE attempting QR decode,
        // so a torn-QR frame still increments `captured`. This is what separates
        // hop frame-loss from tap decode-failure.
        captured.fetch_add(1, Ordering::Relaxed);
        if let Some(p) = decode_capture(
            frame.fourcc,
            &frame.data,
            frame.width,
            frame.height,
            frame.stride,
            spec.decode_crop,
        ) {
            if p.run_id == spec.run_id {
                observed.lock().unwrap().push(Observed {
                    frame_id: p.frame_id,
                    gen_ts_ns: p.gen_ts_ns,
                    recv_ts_ns,
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
