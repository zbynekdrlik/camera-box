//! Reader thread: receive NDI, decode QR, record observed frames.

use crate::ndi::NdiReceiver;
use crate::probe::analyzer::Observed;
use crate::probe::luma::{bgra_to_luma, crop_center, uyvy_to_luma};
use crate::probe::qr::decode_qr_luma;
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
        let full = match &frame.fourcc.to_le_bytes() {
            b"BGRA" | b"BGRX" => bgra_to_luma(&frame.data, frame.width, frame.height, frame.stride),
            _ => uyvy_to_luma(&frame.data, frame.width, frame.height, frame.stride),
        };
        let img = crop_center(&full, params.decode_crop, params.decode_crop);
        if let Some(p) = decode_qr_luma(img) {
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
