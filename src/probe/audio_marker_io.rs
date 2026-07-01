//! Probe+Linux glue (#188): a dedicated thread that plays the A/V-sync chirp on cam2 USB audio
//! at a fixed cadence, logging (frame_id, wall_ts_ns) per emitted marker. OFF the capture core.
#![cfg(target_os = "linux")]

use crate::av_sync::{generate_chirp, ChirpParams};
use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};
use anyhow::{Context, Result};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

pub struct AudioMarkerEmitter {
    handle: JoinHandle<()>,
    log: Arc<Mutex<Vec<(u32, i64)>>>,
}

impl AudioMarkerEmitter {
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        device: String,
        sample_rate: u32,
        chirp: ChirpParams,
        current_id: Arc<AtomicU32>,
        refresh: Arc<AtomicU64>,
        stop: Arc<AtomicBool>,
        start: Instant,
        wall_clock: bool,
        cadence_ticks: u64,
    ) -> Result<AudioMarkerEmitter> {
        let log = Arc::new(Mutex::new(Vec::new()));
        let log_thread = log.clone();
        let handle = std::thread::spawn(move || {
            crate::affinity::pin_off_capture_core("audio-marker");
            if let Err(e) = run_emit(
                &device,
                sample_rate,
                &chirp,
                &current_id,
                &refresh,
                &stop,
                start,
                wall_clock,
                cadence_ticks,
                &log_thread,
            ) {
                eprintln!("[audio-marker] emit thread error: {e:#}");
            }
        });
        Ok(AudioMarkerEmitter { handle, log })
    }

    pub fn join(self) -> Vec<(u32, i64)> {
        let _ = self.handle.join();
        Arc::try_unwrap(self.log)
            .map(|m| m.into_inner().unwrap())
            .unwrap_or_default()
    }
}

#[allow(clippy::too_many_arguments)]
fn run_emit(
    device: &str,
    sample_rate: u32,
    chirp: &ChirpParams,
    current_id: &AtomicU32,
    refresh: &AtomicU64,
    stop: &AtomicBool,
    start: Instant,
    wall_clock: bool,
    cadence_ticks: u64,
    log: &Mutex<Vec<(u32, i64)>>,
) -> Result<()> {
    let pcm = open_playback(device, sample_rate)?;
    // pre-render the chirp once, as stereo i16
    let mono = generate_chirp(sample_rate, chirp.dur_ms, chirp.f0_hz, chirp.f1_hz);
    let mut stereo: Vec<i16> = Vec::with_capacity(mono.len() * 2);
    for s in &mono {
        let v = (s * 30_000.0) as i16; // headroom below i16::MAX
        stereo.push(v);
        stereo.push(v);
    }
    let mut last_fired = 0u64;
    while !stop.load(Ordering::Relaxed) {
        let tick = refresh.load(Ordering::Relaxed);
        if crate::av_sync::should_emit_marker(tick, cadence_ticks) && tick != last_fired {
            last_fired = tick;
            let fid = current_id.load(Ordering::Relaxed);
            let ts = crate::probe::clock_ns(start, wall_clock);
            log.lock().unwrap().push((fid, ts));
            let io = pcm.io_i16()?;
            if let Err(e) = io.writei(&stereo) {
                let _ = pcm.recover(e.errno(), true);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    Ok(())
}

fn open_playback(device: &str, sample_rate: u32) -> Result<PCM> {
    let pcm = PCM::new(device, Direction::Playback, false)
        .with_context(|| format!("open ALSA playback {device}"))?;
    {
        let hwp = HwParams::any(&pcm)?;
        hwp.set_channels(2)?;
        hwp.set_rate(sample_rate, ValueOr::Nearest)?;
        hwp.set_format(Format::s16())?;
        hwp.set_access(Access::RWInterleaved)?;
        hwp.set_period_size(256i64, ValueOr::Nearest)?;
        hwp.set_buffer_size(256i64 * 4)?;
        pcm.hw_params(&hwp)?;
    }
    {
        let swp = pcm.sw_params_current()?;
        swp.set_start_threshold(256i64)?;
        swp.set_avail_min(256i64)?;
        pcm.sw_params(&swp)?;
    }
    Ok(pcm)
}
