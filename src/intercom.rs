//! VBAN Intercom - bidirectional audio over network with direct ALSA
//!
//! Uses direct ALSA API for lowest latency audio I/O.
//! Receives VBAN audio stream and plays through USB headset.
//! Captures microphone audio and sends via VBAN.
//! Provides low-latency sidetone (mic monitoring in headphones).

use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};
use anyhow::{anyhow, Context, Result};
use evdev::{Device, Key};
use std::collections::VecDeque;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

use crate::vban::{VbanCodec, VbanHeader, MAX_VBAN_PACKET_SIZE, VBAN_HEADER_SIZE, VBAN_PORT};

// ALSA configuration - optimized for low latency
const ALSA_DEVICE: &str = "hw:CARD=HID,DEV=0";
const SAMPLE_RATE: u32 = 48000;
const PERIOD_SIZE: u32 = 256; // ~5.3ms at 48kHz - low latency
const BUFFER_PERIODS: u32 = 4; // 4 periods = ~21ms total buffer

// =============================================================================
// Power Button Mute Toggle
// =============================================================================

fn find_power_buttons() -> Vec<(String, i32)> {
    let mut devices = Vec::new();
    for i in 0..10 {
        let path = format!("/dev/input/event{}", i);
        if let Ok(device) = Device::open(&path) {
            if let Some(keys) = device.supported_keys() {
                if keys.contains(Key::KEY_POWER) {
                    let name = device.name().unwrap_or("unknown").to_string();
                    tracing::info!("Found power button: {} ({})", name, path);
                    use std::os::unix::io::AsRawFd;
                    let fd = device.as_raw_fd();
                    let dup_fd = unsafe { libc::dup(fd) };
                    if dup_fd >= 0 {
                        devices.push((path, dup_fd));
                    }
                }
            }
        }
    }
    devices
}

fn run_power_button_monitor(muted: Arc<AtomicBool>, running: Arc<AtomicBool>) {
    let devices = find_power_buttons();
    if devices.is_empty() {
        tracing::warn!("No power button found - mute toggle disabled");
        return;
    }

    for (_path, fd) in &devices {
        unsafe {
            let flags = libc::fcntl(*fd, libc::F_GETFL);
            libc::fcntl(*fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }

    tracing::info!(
        "Power button mute toggle enabled ({} devices)",
        devices.len()
    );
    let mut event_buf = [0u8; 24];

    while running.load(Ordering::Relaxed) {
        for (path, fd) in &devices {
            let n = unsafe {
                libc::read(
                    *fd,
                    event_buf.as_mut_ptr() as *mut libc::c_void,
                    event_buf.len(),
                )
            };
            if n == 24 {
                let event_type = u16::from_ne_bytes([event_buf[16], event_buf[17]]);
                let event_code = u16::from_ne_bytes([event_buf[18], event_buf[19]]);
                let event_value = i32::from_ne_bytes([
                    event_buf[20],
                    event_buf[21],
                    event_buf[22],
                    event_buf[23],
                ]);
                if event_type == 1 && event_code == 116 && event_value == 1 {
                    let was_muted = muted.fetch_xor(true, Ordering::Relaxed);
                    let now_muted = !was_muted;
                    tracing::info!(
                        "🎤 Microphone {} (via {})",
                        if now_muted { "MUTED" } else { "UNMUTED" },
                        path
                    );
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    for (_path, fd) in devices {
        unsafe { libc::close(fd) };
    }
}

// =============================================================================
// Configuration
// =============================================================================

#[derive(Debug, Clone)]
pub struct IntercomConfig {
    pub stream_name: String,
    pub target_host: String,
    #[allow(dead_code)] // Config API, uses SAMPLE_RATE constant internally
    pub sample_rate: u32,
    #[allow(dead_code)] // Config API, uses fixed mono/stereo internally
    pub channels: u8,
    pub sidetone_volume: f32,
}

impl Default for IntercomConfig {
    fn default() -> Self {
        Self {
            stream_name: "cam1".to_string(),
            target_host: "strih.lan".to_string(),
            sample_rate: SAMPLE_RATE,
            channels: 2,
            sidetone_volume: 1.0,
        }
    }
}

// =============================================================================
// Audio Buffer
// =============================================================================

struct AudioBuffer {
    samples: VecDeque<i16>,
    capacity: usize,
}

impl AudioBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn push_samples(&mut self, data: &[i16]) {
        while self.samples.len() + data.len() > self.capacity {
            self.samples.pop_front();
        }
        self.samples.extend(data.iter().copied());
    }

    fn pop_samples(&mut self, count: usize) -> Vec<i16> {
        let available = count.min(self.samples.len());
        self.samples.drain(..available).collect()
    }
}

// =============================================================================
// Direct ALSA Audio
// =============================================================================

fn open_alsa_capture() -> Result<PCM> {
    let pcm = PCM::new(ALSA_DEVICE, Direction::Capture, false)
        .context("Failed to open ALSA capture device")?;

    {
        let hwp = HwParams::any(&pcm)?;
        hwp.set_channels(1)?; // Mono microphone
        hwp.set_rate(SAMPLE_RATE, ValueOr::Nearest)?;
        hwp.set_format(Format::s16())?;
        hwp.set_access(Access::RWInterleaved)?;
        hwp.set_period_size(PERIOD_SIZE as i64, ValueOr::Nearest)?;
        hwp.set_buffer_size((PERIOD_SIZE * BUFFER_PERIODS) as i64)?;
        pcm.hw_params(&hwp)?;
    }

    {
        let swp = pcm.sw_params_current()?;
        swp.set_start_threshold(1)?;
        swp.set_avail_min(PERIOD_SIZE as i64)?;
        pcm.sw_params(&swp)?;
    }

    tracing::info!(
        "ALSA capture: hw:CARD=HID, {}Hz mono, period={} frames",
        SAMPLE_RATE,
        PERIOD_SIZE
    );
    Ok(pcm)
}

fn open_alsa_playback() -> Result<PCM> {
    let pcm = PCM::new(ALSA_DEVICE, Direction::Playback, false)
        .context("Failed to open ALSA playback device")?;

    {
        let hwp = HwParams::any(&pcm)?;
        hwp.set_channels(2)?; // Stereo output
        hwp.set_rate(SAMPLE_RATE, ValueOr::Nearest)?;
        hwp.set_format(Format::s16())?;
        hwp.set_access(Access::RWInterleaved)?;
        hwp.set_period_size(PERIOD_SIZE as i64, ValueOr::Nearest)?;
        hwp.set_buffer_size((PERIOD_SIZE * BUFFER_PERIODS) as i64)?;
        pcm.hw_params(&hwp)?;
    }

    {
        let swp = pcm.sw_params_current()?;
        swp.set_start_threshold(PERIOD_SIZE as i64)?;
        swp.set_avail_min(PERIOD_SIZE as i64)?;
        pcm.sw_params(&swp)?;
    }

    tracing::info!(
        "ALSA playback: hw:CARD=HID, {}Hz stereo, period={} frames",
        SAMPLE_RATE,
        PERIOD_SIZE
    );
    Ok(pcm)
}

fn recover_alsa(pcm: &PCM, err: i32) -> bool {
    match pcm.recover(err, true) {
        Ok(_) => true,
        Err(e) => {
            tracing::error!("ALSA recovery failed: {}", e);
            false
        }
    }
}

// =============================================================================
// VBAN Receiver
// =============================================================================

fn run_receiver(
    config: &IntercomConfig,
    playback_buffer: Arc<Mutex<AudioBuffer>>,
    running: Arc<AtomicBool>,
    frames_received: Arc<AtomicU64>,
) -> Result<()> {
    let socket = UdpSocket::bind(format!("0.0.0.0:{}", VBAN_PORT))?;
    socket
        .set_read_timeout(Some(std::time::Duration::from_millis(100)))
        .ok();

    tracing::info!(
        "VBAN receiver listening on port {}, stream: {}",
        VBAN_PORT,
        config.stream_name
    );
    let mut packet_buf = [0u8; MAX_VBAN_PACKET_SIZE];

    while running.load(Ordering::Relaxed) {
        match socket.recv_from(&mut packet_buf) {
            Ok((len, _addr)) => {
                if len < VBAN_HEADER_SIZE {
                    continue;
                }
                let header = match VbanHeader::decode(&packet_buf[..len]) {
                    Ok(h) => h,
                    Err(_) => continue,
                };
                if header.stream_name_str() != config.stream_name {
                    continue;
                }

                let audio_data = &packet_buf[VBAN_HEADER_SIZE..len];
                let samples: Vec<i16> = match header.codec {
                    c if c == VbanCodec::Pcm16 as u8 => audio_data
                        .chunks_exact(2)
                        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
                        .collect(),
                    c if c == VbanCodec::Float32 as u8 => audio_data
                        .chunks_exact(4)
                        .map(|chunk| {
                            let f = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                            (f * 32767.0).clamp(-32768.0, 32767.0) as i16
                        })
                        .collect(),
                    _ => audio_data
                        .chunks_exact(2)
                        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
                        .collect(),
                };

                if let Ok(mut buf) = playback_buffer.lock() {
                    buf.push_samples(&samples);
                }
                frames_received.fetch_add(1, Ordering::Relaxed);
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => tracing::warn!("VBAN receive error: {}", e),
        }
    }
    Ok(())
}

// =============================================================================
// Main Intercom Loop
// =============================================================================

fn apply_intercom_priority() {
    unsafe {
        libc::nice(10);
        let mut cpuset: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_SET(1, &mut cpuset);
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &cpuset);
    }
}

pub fn run_intercom(config: IntercomConfig, running: Arc<AtomicBool>) -> Result<()> {
    apply_intercom_priority();

    while running.load(Ordering::Relaxed) {
        tracing::info!(
            "Starting VBAN intercom with direct ALSA: stream={}, target={}",
            config.stream_name,
            config.target_host
        );

        match run_intercom_inner(&config, Arc::clone(&running)) {
            Ok(()) => {
                tracing::info!("Intercom stopped normally");
                break;
            }
            Err(e) => {
                tracing::error!("Intercom error: {} - restarting in 2 seconds", e);
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
    }
    Ok(())
}

fn run_intercom_inner(config: &IntercomConfig, running: Arc<AtomicBool>) -> Result<()> {
    // Open ALSA devices
    let capture = open_alsa_capture()?;
    let playback = open_alsa_playback()?;

    // Mute state
    let muted = Arc::new(AtomicBool::new(true));
    tracing::info!("🎤 Microphone starts MUTED - press power button to unmute");

    // Start power button monitor
    let muted_btn = Arc::clone(&muted);
    let running_btn = Arc::clone(&running);
    std::thread::spawn(move || run_power_button_monitor(muted_btn, running_btn));

    // VBAN sender
    let vban_socket = UdpSocket::bind("0.0.0.0:0")?;
    let target_addr = format!("{}:{}", config.target_host, VBAN_PORT);
    vban_socket.connect(&target_addr)?;
    tracing::info!(
        "VBAN sender targeting {}, stream: {}",
        target_addr,
        config.stream_name
    );

    // Playback buffer for VBAN receive
    let playback_buffer = Arc::new(Mutex::new(AudioBuffer::new(SAMPLE_RATE as usize)));

    // Stats
    let frames_received = Arc::new(AtomicU64::new(0));
    let frames_sent = Arc::new(AtomicU64::new(0));
    let samples_captured = Arc::new(AtomicU64::new(0));

    // Start VBAN receiver thread
    let recv_config = config.clone();
    let recv_buf = Arc::clone(&playback_buffer);
    let recv_running = Arc::clone(&running);
    let recv_frames = Arc::clone(&frames_received);
    std::thread::spawn(move || {
        if let Err(e) = run_receiver(&recv_config, recv_buf, recv_running, recv_frames) {
            tracing::error!("VBAN receiver error: {}", e);
        }
    });

    // Audio gains
    let sidetone_gain = 20.0_f32 * config.sidetone_volume;
    let vban_gain = 4.0_f32;

    // VBAN packet state
    let mut frame_counter: u32 = 0;
    let stream_name_bytes: [u8; 16] = {
        let mut buf = [0u8; 16];
        let name = config.stream_name.as_bytes();
        let len = name.len().min(16);
        buf[..len].copy_from_slice(&name[..len]);
        buf
    };

    // Buffers
    let mut capture_buf = vec![0i16; PERIOD_SIZE as usize];
    let mut playback_buf = vec![0i16; (PERIOD_SIZE * 2) as usize]; // Stereo
    let mut sidetone_buf = VecDeque::<i16>::with_capacity(1024);

    // Stats timing
    let mut last_report = std::time::Instant::now();
    let report_interval = std::time::Duration::from_secs(10);
    let mut last_received = 0u64;
    let mut last_sent = 0u64;

    tracing::info!(
        "Audio streams started with direct ALSA, period={}frames (~{:.1}ms)",
        PERIOD_SIZE,
        PERIOD_SIZE as f32 / SAMPLE_RATE as f32 * 1000.0
    );

    while running.load(Ordering::Relaxed) {
        let is_muted = muted.load(Ordering::Relaxed);

        // === CAPTURE ===
        let io_cap = capture.io_i16()?;
        match io_cap.readi(&mut capture_buf) {
            Ok(frames) => {
                samples_captured.fetch_add(frames as u64, Ordering::Relaxed);

                if !is_muted {
                    // Add to sidetone buffer (mono -> stereo later)
                    for &sample in &capture_buf[..frames] {
                        if sidetone_buf.len() < 512 {
                            sidetone_buf.push_back(sample);
                        }
                    }

                    // Send VBAN packets
                    const CHUNK_SIZE: usize = 128;
                    for chunk in capture_buf[..frames].chunks(CHUNK_SIZE) {
                        let stereo_data: Vec<i16> = chunk.iter().flat_map(|&s| [s, s]).collect();
                        let samples_per_frame = chunk.len();
                        let mut packet = vec![0u8; VBAN_HEADER_SIZE + stereo_data.len() * 2];

                        packet[0..4].copy_from_slice(b"VBAN");
                        packet[4] = 3; // 48kHz
                        packet[5] = (samples_per_frame.saturating_sub(1) & 0xFF) as u8;
                        packet[6] = 1; // 2 channels - 1
                        packet[7] = 0x01; // PCM16
                        packet[8..24].copy_from_slice(&stream_name_bytes);
                        packet[24..28].copy_from_slice(&frame_counter.to_le_bytes());

                        for (i, &sample) in stereo_data.iter().enumerate() {
                            let bytes = sample.to_le_bytes();
                            packet[VBAN_HEADER_SIZE + i * 2] = bytes[0];
                            packet[VBAN_HEADER_SIZE + i * 2 + 1] = bytes[1];
                        }

                        let _ = vban_socket.send(&packet);
                        frame_counter = frame_counter.wrapping_add(1);
                        frames_sent.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            Err(e) => {
                if !recover_alsa(&capture, e.errno()) {
                    return Err(anyhow!("ALSA capture error: {}", e));
                }
            }
        }

        // === PLAYBACK ===
        // Mix VBAN + sidetone
        let vban_samples = if let Ok(mut buf) = playback_buffer.lock() {
            buf.pop_samples(playback_buf.len())
        } else {
            vec![]
        };

        for (i, sample) in playback_buf.iter_mut().enumerate() {
            let vban = (vban_samples.get(i).copied().unwrap_or(0) as f32 * vban_gain) as i32;
            let sidetone = if is_muted {
                0
            } else {
                // Get mono sample and duplicate for stereo
                let mono = if i % 2 == 0 {
                    sidetone_buf.pop_front().unwrap_or(0)
                } else {
                    sidetone_buf.front().copied().unwrap_or(0)
                };
                (mono as f32 * sidetone_gain) as i32
            };
            *sample = (vban + sidetone).clamp(-32768, 32767) as i16;
        }

        // Write to ALSA
        let io_play = playback.io_i16()?;
        match io_play.writei(&playback_buf) {
            Ok(_) => {}
            Err(e) => {
                if !recover_alsa(&playback, e.errno()) {
                    return Err(anyhow!("ALSA playback error: {}", e));
                }
            }
        }

        // Stats
        if last_report.elapsed() >= report_interval {
            let received = frames_received.load(Ordering::Relaxed);
            let sent = frames_sent.load(Ordering::Relaxed);
            let recv_rate = (received - last_received) as f64 / report_interval.as_secs_f64();
            let send_rate = (sent - last_sent) as f64 / report_interval.as_secs_f64();
            let captured = samples_captured.swap(0, Ordering::Relaxed);
            let capture_rate = captured as f64 / report_interval.as_secs_f64();

            tracing::info!(
                "Intercom: recv {:.1} pkt/s, send {:.1} pkt/s, capture {:.0} samp/s",
                recv_rate,
                send_rate,
                capture_rate
            );

            last_received = received;
            last_sent = sent;
            last_report = std::time::Instant::now();
        }
    }

    Ok(())
}
