//! Probe+Linux glue (#188): a dedicated thread that emits the QR-based (QPSK) A/V-sync audio marker
//! on the cam2 HDMI audio out (`hw:CARD=PCH,DEV=3`) alongside the painted dual-QR, logging
//! `(index, frame_id, emit_playout_ts_ns)` per marker. OFF the capture core.
//!
//! CONTINUOUS FEED — the reason this replaces `audio_marker_io` (the chirp): a bursty
//! write-then-idle emitter lets the ALSA ring DRAIN between markers, the PCM STOPS, and the next
//! write XRUN-freezes (observed on HDMI after the first chirp). Here the thread ALWAYS writes —
//! silence between markers, the marker samples when one is due — so the stream never underruns. The
//! encode is the pure norihiro-compatible QPSK from `crate::qpsk_marker`.
#![cfg(target_os = "linux")]

use crate::qpsk_marker::{marker_signal, to_stereo_i16, AudioParams};
use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};
use anyhow::{Context, Result};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

/// One logged marker: the QPSK `index` carried in the audio, the dual-QR `frame_id` displayed when
/// it played out, and the playout wall/proc timestamp (ns). recording-verdict pairs a decoded audio
/// index → this `frame_id` → the video time of that frame → the A/V offset.
pub type QpskMarkerLog = Vec<(u8, u32, i64)>;

pub struct QpskEmitter {
    handle: JoinHandle<()>,
    log: Arc<Mutex<QpskMarkerLog>>,
}

impl QpskEmitter {
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        device: String,
        params: AudioParams,
        current_id: Arc<AtomicU32>,
        refresh: Arc<AtomicU64>,
        stop: Arc<AtomicBool>,
        start: Instant,
        wall_clock: bool,
        cadence_ticks: u64,
    ) -> Result<QpskEmitter> {
        let log = Arc::new(Mutex::new(Vec::new()));
        let log_thread = log.clone();
        // Open before the thread so a bad device fails the run loudly (not silently in a thread).
        let pcm = open_playback(&device, params.sample_rate)?;
        let handle = std::thread::spawn(move || {
            crate::affinity::pin_off_capture_core("qpsk-marker");
            if let Err(e) = run_emit(
                pcm,
                &params,
                &current_id,
                &refresh,
                &stop,
                start,
                wall_clock,
                cadence_ticks,
                &log_thread,
            ) {
                eprintln!("[qpsk-marker] emit thread error: {e:#}");
            }
        });
        Ok(QpskEmitter { handle, log })
    }

    pub fn join(self) -> QpskMarkerLog {
        let _ = self.handle.join();
        Arc::try_unwrap(self.log)
            .map(|m| m.into_inner().unwrap())
            .unwrap_or_default()
    }
}

#[allow(clippy::too_many_arguments)]
fn run_emit(
    pcm: PCM,
    params: &AudioParams,
    current_id: &AtomicU32,
    refresh: &AtomicU64,
    stop: &AtomicBool,
    start: Instant,
    wall_clock: bool,
    cadence_ticks: u64,
    log: &Mutex<QpskMarkerLog>,
) -> Result<()> {
    let io = pcm.io_i16()?;
    let sr = params.sample_rate as i64;
    // Pre-render all 256 marker signals once (stereo i16). The emit loop just picks one — no
    // per-marker float work on the audio thread.
    let markers: Vec<Vec<i16>> = (0..=255u8)
        .map(|i| to_stereo_i16(&marker_signal(i, params), params.amplitude))
        .collect();
    // One period of silence keeps the ring fed between markers (continuous feed).
    let silence = vec![0i16; SILENCE_FRAMES * 2];

    let mut index: u8 = 0;
    let mut last_fired: u64 = 0;
    let cadence = cadence_ticks.max(1);
    while !stop.load(Ordering::Relaxed) {
        let tick = refresh.load(Ordering::Relaxed);
        // Fire once the painter has advanced a full cadence past the last marker (and is running).
        if tick >= last_fired + cadence && tick > 0 {
            last_fired = tick;
            // Playout time = now + the frames still queued ahead of ours (ALSA delay) → the real
            // DAC instant this marker sounds, not the enqueue instant. The continuous feed keeps
            // the ring NEARLY FULL (~150–170 ms at the 8192-frame ring), so the frame_id must be
            // compensated the same way: `playout_frame_id` converts the ALSA delay into painter
            // frames (~10 @ 60 fps) so the logged id is the dual-QR actually ON SCREEN when the
            // marker is heard. Logging the raw enqueue-instant id biased the measured A/V offset
            // by the whole ring delay (the −194.5 ms live number carried ~160 ms of it).
            let delay_frames = pcm.delay().unwrap_or(0);
            let ts = crate::probe::clock_ns(start, wall_clock) + delay_frames * 1_000_000_000 / sr;
            let fid = crate::qpsk_marker::playout_frame_id(
                current_id.load(Ordering::Relaxed),
                delay_frames,
                params.sample_rate,
                params.vr_num,
                params.vr_den,
            );
            log.lock().unwrap().push((index, fid, ts));
            write_all(&pcm, &io, &markers[index as usize])?;
            index = index.wrapping_add(1);
        } else {
            // Keep the stream alive & self-paced (blocking write) — never let the ring drain.
            write_all(&pcm, &io, &silence)?;
        }
    }
    Ok(())
}

/// Frames per silence write between markers — one period, so the loop wakes ~every period to check
/// the refresh tick while keeping the ring fed.
const SILENCE_FRAMES: usize = 1024;

/// Blocking interleaved write of the whole stereo buffer, recovering once from an underrun so a
/// transient hiccup never kills the stream.
fn write_all(pcm: &PCM, io: &alsa::pcm::IO<'_, i16>, stereo: &[i16]) -> Result<()> {
    match io.writei(stereo) {
        Ok(_) => Ok(()),
        Err(e) => {
            pcm.recover(e.errno(), true).ok();
            pcm.prepare().ok();
            io.writei(stereo)
                .map(|_| ())
                .context("ALSA writei after recover")
        }
    }
}

fn open_playback(device: &str, sample_rate: u32) -> Result<PCM> {
    let pcm = PCM::new(device, Direction::Playback, false)
        .with_context(|| format!("open ALSA playback {device}"))?;
    let buffer_frames: i64 = 8192;
    let period: i64 = 1024;
    {
        let hwp = HwParams::any(&pcm)?;
        hwp.set_channels(2)?;
        hwp.set_rate(sample_rate, ValueOr::Nearest)?;
        hwp.set_format(Format::s16())?;
        hwp.set_access(Access::RWInterleaved)?;
        hwp.set_period_size(period, ValueOr::Nearest)?;
        hwp.set_buffer_size_near(buffer_frames)?;
        pcm.hw_params(&hwp)?;
    }
    {
        let swp = pcm.sw_params_current()?;
        // Start once ~half the ring is queued, so the continuous silence feed starts smoothly and
        // stays fed (no start/stop between markers — that was the chirp XRUN).
        swp.set_start_threshold(period * 2)?;
        swp.set_avail_min(period)?;
        pcm.sw_params(&swp)?;
    }
    Ok(pcm)
}
