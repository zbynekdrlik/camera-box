//! Reader thread: receive NDI, decode QR, record observed frames.

use crate::ndi::NdiReceiver;
use crate::probe::analyzer::Observed;
use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub struct ReadParams {
    pub run_id: u32,
    pub source: String,
    pub connect_timeout_secs: u32,
    /// Side length of the centered square the QR decode is restricted to.
    /// The painter centers the QR, so decoding only this ROI keeps per-frame
    /// decode fast enough to track the capture without backlog.
    pub decode_crop: u32,
}

/// Receive until `stop` is set. Records every decoded frame whose run_id matches.
pub fn run_reader(
    params: ReadParams,
    start: Instant,
    stop: Arc<AtomicBool>,
    observed: Arc<Mutex<Vec<Observed>>>,
) -> Result<()> {
    let mut rx = NdiReceiver::connect(&params.source, params.connect_timeout_secs)?;

    while !stop.load(Ordering::Relaxed) {
        let frame = match rx.capture_frame(100)? {
            Some(f) => f,
            None => continue,
        };
        let recv_ts_ns = start.elapsed().as_nanos() as i64;
        if let Some(p) = crate::probe::qr::decode_capture(
            frame.fourcc,
            &frame.data,
            frame.width,
            frame.height,
            frame.stride,
            params.decode_crop,
        ) {
            if p.run_id == params.run_id {
                observed.lock().unwrap().push(Observed {
                    frame_id: p.frame_id,
                    gen_ts_ns: p.gen_ts_ns,
                    recv_ts_ns,
                });
            }
        }
    }
    Ok(())
}
