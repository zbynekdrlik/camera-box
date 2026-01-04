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
    pub sidetone_gain: f32,
    /// Microphone gain for outbound VBAN stream (default: 8.0 = +18dB)
    pub mic_gain: f32,
    /// Headphone gain for incoming VBAN stream (default: 10.0)
    pub headphone_gain: f32,
    /// Enable peak limiter on microphone output
    pub limiter_enabled: bool,
    /// Limiter threshold as fraction of max (0.5 = -6dB)
    pub limiter_threshold: f32,
}

impl Default for IntercomConfig {
    fn default() -> Self {
        Self {
            stream_name: "cam1".to_string(),
            target_host: "strih.lan".to_string(),
            sample_rate: SAMPLE_RATE,
            channels: 2,
            sidetone_gain: 30.0,
            mic_gain: 8.0,
            headphone_gain: 10.0,
            limiter_enabled: true,
            limiter_threshold: 0.15,
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
// Peak Limiter (for microphone output to network)
// =============================================================================

/// Look-ahead peak limiter to prevent audio spikes from reaching the network.
/// Uses a small delay buffer to detect peaks before they hit the output,
/// allowing preemptive gain reduction for transparent limiting.
pub struct PeakLimiter {
    /// Threshold as fraction of max (0.5 = -6dB)
    threshold: f32,
    /// Attack coefficient (how fast gain reduces)
    attack_coeff: f32,
    /// Release coefficient (how fast gain recovers)
    release_coeff: f32,
    /// Current envelope follower value (the gain to apply)
    envelope: f32,
    /// Look-ahead buffer (delays audio to allow preemptive limiting)
    lookahead_buffer: VecDeque<i16>,
    /// Look-ahead samples (0.5ms at 48kHz = 24 samples)
    lookahead_samples: usize,
}

impl PeakLimiter {
    /// Create a new peak limiter.
    ///
    /// # Arguments
    /// * `threshold` - Threshold as fraction of max (0.5 = -6dB)
    /// * `sample_rate` - Audio sample rate in Hz
    pub fn new(threshold: f32, sample_rate: u32) -> Self {
        // Attack: 0.1ms = very fast to catch transients
        let attack_time = 0.0001;
        // Release: 50ms = smooth recovery
        let release_time = 0.050;
        // Look-ahead: 0.5ms = catches spikes before output
        let lookahead_time = 0.0005;

        let attack_coeff = (-1.0 / (attack_time * sample_rate as f32)).exp();
        let release_coeff = (-1.0 / (release_time * sample_rate as f32)).exp();
        let lookahead_samples = (lookahead_time * sample_rate as f32) as usize;

        Self {
            threshold: threshold.clamp(0.01, 1.0),
            attack_coeff,
            release_coeff,
            envelope: 1.0,
            lookahead_buffer: VecDeque::with_capacity(lookahead_samples + 1),
            lookahead_samples,
        }
    }

    /// Process a single sample through the limiter.
    /// Returns the limited sample.
    pub fn process(&mut self, input: i16) -> i16 {
        // Push input to look-ahead buffer
        self.lookahead_buffer.push_back(input);

        // If buffer not full yet, output silence (startup transient)
        if self.lookahead_buffer.len() <= self.lookahead_samples {
            return 0;
        }

        // Get the delayed sample (this is what we'll output)
        let delayed = self.lookahead_buffer.pop_front().unwrap_or(0);

        // Find peak in look-ahead window
        let peak = self
            .lookahead_buffer
            .iter()
            .map(|&s| (s as f32).abs() / 32768.0)
            .fold(0.0f32, f32::max);

        // Calculate target gain (reduce if over threshold)
        let target_gain = if peak > self.threshold {
            self.threshold / peak
        } else {
            1.0
        };

        // Apply envelope follower with attack/release
        let coeff = if target_gain < self.envelope {
            self.attack_coeff // Fast attack for transients
        } else {
            self.release_coeff // Slow release for smooth recovery
        };
        self.envelope = self.envelope * coeff + target_gain * (1.0 - coeff);

        // Apply gain to delayed sample
        let limited = (delayed as f32 * self.envelope).clamp(-32768.0, 32767.0) as i16;

        // HARD CLIPPER: Safety net for instantaneous spikes that envelope can't catch
        // This ensures NO sample ever exceeds the threshold, even on plug/unplug transients
        let hard_clip_max = (self.threshold * 32767.0) as i16;
        limited.clamp(-hard_clip_max, hard_clip_max)
    }

    /// Process a buffer of samples in-place.
    pub fn process_buffer(&mut self, buffer: &mut [i16]) {
        for sample in buffer.iter_mut() {
            *sample = self.process(*sample);
        }
    }

    /// Reset the limiter state (call when audio stream restarts).
    #[allow(dead_code)]
    pub fn reset(&mut self) {
        self.envelope = 1.0;
        self.lookahead_buffer.clear();
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

// =============================================================================
// Testable Audio Buffer (public for testing)
// =============================================================================

/// Ring buffer for audio samples (exposed for testing)
#[derive(Debug)]
pub struct TestableAudioBuffer {
    samples: VecDeque<i16>,
    capacity: usize,
}

impl TestableAudioBuffer {
    /// Create a new audio buffer with the given capacity
    pub fn new(capacity: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Push samples into the buffer, dropping oldest if at capacity
    pub fn push_samples(&mut self, data: &[i16]) {
        while self.samples.len() + data.len() > self.capacity {
            self.samples.pop_front();
        }
        self.samples.extend(data.iter().copied());
    }

    /// Pop up to `count` samples from the buffer
    pub fn pop_samples(&mut self, count: usize) -> Vec<i16> {
        let available = count.min(self.samples.len());
        self.samples.drain(..available).collect()
    }

    /// Get current number of samples in buffer
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Check if buffer is empty
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Get buffer capacity
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

fn run_intercom_inner(config: &IntercomConfig, running: Arc<AtomicBool>) -> Result<()> {
    // Open ALSA devices with retry
    let capture = loop {
        match open_alsa_capture() {
            Ok(c) => break c,
            Err(e) => {
                if !running.load(Ordering::Relaxed) {
                    return Ok(());
                }
                tracing::warn!("Waiting for audio capture device: {} - retrying...", e);
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
    };

    let playback = loop {
        match open_alsa_playback() {
            Ok(p) => break p,
            Err(e) => {
                if !running.load(Ordering::Relaxed) {
                    return Ok(());
                }
                tracing::warn!("Waiting for audio playback device: {} - retrying...", e);
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
    };

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
    let sidetone_gain = config.sidetone_gain;
    let headphone_gain = config.headphone_gain;
    let mic_gain = config.mic_gain;

    // Peak limiter for microphone output (prevents spikes from plug/unplug)
    let mut limiter = if config.limiter_enabled {
        Some(PeakLimiter::new(config.limiter_threshold, SAMPLE_RATE))
    } else {
        None
    };
    tracing::info!(
        "Audio gains: mic={:.1}x, headphone={:.1}x, sidetone={:.1}x, limiter={}",
        mic_gain,
        headphone_gain,
        sidetone_gain,
        if config.limiter_enabled {
            format!("on (threshold={:.0}%)", config.limiter_threshold * 100.0)
        } else {
            "off".to_string()
        }
    );

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

    // Capture watchdog - detect if capture stops producing samples
    let mut last_capture_samples = 0u64;
    let mut capture_stall_count = 0u32;

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
            Ok(frames) if frames > 0 => {
                samples_captured.fetch_add(frames as u64, Ordering::Relaxed);
                capture_stall_count = 0; // Reset stall counter on successful capture

                if !is_muted {
                    // Add RAW samples to sidetone buffer (no gain/limiter for minimum latency)
                    for &sample in &capture_buf[..frames] {
                        if sidetone_buf.len() < 512 {
                            sidetone_buf.push_back(sample);
                        }
                    }

                    // Apply mic gain and limiter for VBAN output (separate from sidetone)
                    // Pre-clip: catch ALSA garbage from plug/unplug BEFORE gain amplification
                    // Any sample near max likely indicates a transient glitch
                    const PRE_CLIP_THRESHOLD: i16 = 30000; // ~91% of max
                    let mut vban_samples: Vec<i16> = capture_buf[..frames]
                        .iter()
                        .map(|&s| {
                            // Pre-clip extreme values before applying gain
                            let clipped = s.clamp(-PRE_CLIP_THRESHOLD, PRE_CLIP_THRESHOLD);
                            (clipped as f32 * mic_gain).clamp(-32768.0, 32767.0) as i16
                        })
                        .collect();

                    // Apply limiter if enabled (prevents spikes from plug/unplug)
                    if let Some(ref mut lim) = limiter {
                        lim.process_buffer(&mut vban_samples);
                    }

                    // Send VBAN packets
                    const CHUNK_SIZE: usize = 128;
                    for chunk in vban_samples.chunks(CHUNK_SIZE) {
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
            Ok(_) => {
                // Zero frames - capture might be stalled
                capture_stall_count += 1;
            }
            Err(e) => {
                capture_stall_count += 1;
                if !recover_alsa(&capture, e.errno()) {
                    return Err(anyhow!("ALSA capture error: {}", e));
                }
            }
        }

        // Quick stall detection: if 500+ consecutive iterations without capture
        // (about 2.5 seconds at 5ms/iteration), force restart
        if capture_stall_count > 500 {
            tracing::warn!(
                "Capture device unresponsive ({} consecutive failures), forcing restart...",
                capture_stall_count
            );
            return Err(anyhow!("Capture device unresponsive"));
        }

        // === PLAYBACK ===
        // Mix VBAN + sidetone
        let vban_samples = if let Ok(mut buf) = playback_buffer.lock() {
            buf.pop_samples(playback_buf.len())
        } else {
            vec![]
        };

        for (i, sample) in playback_buf.iter_mut().enumerate() {
            let vban = (vban_samples.get(i).copied().unwrap_or(0) as f32 * headphone_gain) as i32;
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

        // Stats and watchdog
        if last_report.elapsed() >= report_interval {
            let received = frames_received.load(Ordering::Relaxed);
            let sent = frames_sent.load(Ordering::Relaxed);
            let recv_rate = (received - last_received) as f64 / report_interval.as_secs_f64();
            let send_rate = (sent - last_sent) as f64 / report_interval.as_secs_f64();
            let captured = samples_captured.load(Ordering::Relaxed);
            let capture_rate =
                (captured - last_capture_samples) as f64 / report_interval.as_secs_f64();

            tracing::info!(
                "Intercom: recv {:.1} pkt/s, send {:.1} pkt/s, capture {:.0} samp/s",
                recv_rate,
                send_rate,
                capture_rate
            );

            // Watchdog: if no samples captured in this period, something is wrong
            if captured == last_capture_samples && capture_rate < 1000.0 {
                tracing::warn!(
                    "Capture stalled! No samples in {}s (stall_count={}), forcing restart...",
                    report_interval.as_secs(),
                    capture_stall_count
                );
                return Err(anyhow!("Capture device stalled - forcing restart"));
            }

            last_capture_samples = captured;
            last_received = received;
            last_sent = sent;
            last_report = std::time::Instant::now();
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_intercom_config_default() {
        let config = IntercomConfig::default();
        assert_eq!(config.stream_name, "cam1");
        assert_eq!(config.target_host, "strih.lan");
        assert_eq!(config.sample_rate, 48000);
        assert_eq!(config.channels, 2);
        assert!((config.sidetone_gain - 30.0).abs() < 0.001);
        assert!((config.mic_gain - 8.0).abs() < 0.001);
        assert!((config.headphone_gain - 10.0).abs() < 0.001);
        assert!(config.limiter_enabled);
        assert!((config.limiter_threshold - 0.15).abs() < 0.001);
    }

    #[test]
    fn test_intercom_config_clone() {
        let config = IntercomConfig {
            stream_name: "test".to_string(),
            target_host: "host.lan".to_string(),
            sample_rate: 44100,
            channels: 1,
            sidetone_gain: 15.0,
            mic_gain: 2.0,
            headphone_gain: 8.0,
            limiter_enabled: false,
            limiter_threshold: 0.8,
        };
        let cloned = config.clone();
        assert_eq!(config.stream_name, cloned.stream_name);
        assert_eq!(config.target_host, cloned.target_host);
        assert_eq!(config.sample_rate, cloned.sample_rate);
        assert_eq!(config.channels, cloned.channels);
        assert!((config.mic_gain - cloned.mic_gain).abs() < 0.001);
        assert!((config.headphone_gain - cloned.headphone_gain).abs() < 0.001);
        assert_eq!(config.limiter_enabled, cloned.limiter_enabled);
        assert!((config.limiter_threshold - cloned.limiter_threshold).abs() < 0.001);
    }

    #[test]
    fn test_audio_buffer_new() {
        let buf = TestableAudioBuffer::new(100);
        assert_eq!(buf.capacity(), 100);
        assert_eq!(buf.len(), 0);
        assert!(buf.is_empty());
    }

    #[test]
    fn test_audio_buffer_push_within_capacity() {
        let mut buf = TestableAudioBuffer::new(10);
        buf.push_samples(&[1, 2, 3, 4, 5]);
        assert_eq!(buf.len(), 5);
        assert!(!buf.is_empty());
    }

    #[test]
    fn test_audio_buffer_push_overflow() {
        let mut buf = TestableAudioBuffer::new(5);
        buf.push_samples(&[1, 2, 3, 4, 5]);
        assert_eq!(buf.len(), 5);

        // Push more - should drop oldest
        buf.push_samples(&[6, 7, 8]);
        assert_eq!(buf.len(), 5); // Still at capacity

        // First three should be gone, remaining should be 4, 5, 6, 7, 8
        let samples = buf.pop_samples(5);
        assert_eq!(samples, vec![4, 5, 6, 7, 8]);
    }

    #[test]
    fn test_audio_buffer_pop_exact() {
        let mut buf = TestableAudioBuffer::new(10);
        buf.push_samples(&[1, 2, 3, 4, 5]);

        let samples = buf.pop_samples(3);
        assert_eq!(samples, vec![1, 2, 3]);
        assert_eq!(buf.len(), 2);
    }

    #[test]
    fn test_audio_buffer_pop_more_than_available() {
        let mut buf = TestableAudioBuffer::new(10);
        buf.push_samples(&[1, 2, 3]);

        let samples = buf.pop_samples(10);
        assert_eq!(samples, vec![1, 2, 3]);
        assert!(buf.is_empty());
    }

    #[test]
    fn test_audio_buffer_pop_empty() {
        let mut buf = TestableAudioBuffer::new(10);
        let samples = buf.pop_samples(5);
        assert!(samples.is_empty());
    }

    #[test]
    fn test_audio_buffer_fifo_order() {
        let mut buf = TestableAudioBuffer::new(100);
        buf.push_samples(&[1, 2, 3]);
        buf.push_samples(&[4, 5, 6]);

        let samples = buf.pop_samples(6);
        assert_eq!(samples, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn test_audio_buffer_interleaved_push_pop() {
        let mut buf = TestableAudioBuffer::new(100);

        buf.push_samples(&[1, 2]);
        let s1 = buf.pop_samples(1);
        assert_eq!(s1, vec![1]);

        buf.push_samples(&[3, 4]);
        let s2 = buf.pop_samples(3);
        assert_eq!(s2, vec![2, 3, 4]);
    }

    #[test]
    fn test_audio_buffer_large_capacity() {
        let mut buf = TestableAudioBuffer::new(48000); // 1 second at 48kHz
        let samples: Vec<i16> = (0..48000).map(|i| (i % 1000) as i16).collect();
        buf.push_samples(&samples);
        assert_eq!(buf.len(), 48000);
    }

    #[test]
    fn test_alsa_constants() {
        assert_eq!(SAMPLE_RATE, 48000);
        assert_eq!(PERIOD_SIZE, 256);
        assert_eq!(BUFFER_PERIODS, 4);
        assert_eq!(ALSA_DEVICE, "hw:CARD=HID,DEV=0");
    }

    #[test]
    fn test_config_debug() {
        let config = IntercomConfig::default();
        let debug = format!("{:?}", config);
        assert!(debug.contains("IntercomConfig"));
        assert!(debug.contains("cam1"));
    }

    // =============================================================================
    // PeakLimiter Tests
    // =============================================================================

    #[test]
    fn test_limiter_new() {
        let limiter = PeakLimiter::new(0.5, 48000);
        assert!((limiter.threshold - 0.5).abs() < 0.001);
        assert!((limiter.envelope - 1.0).abs() < 0.001);
        assert_eq!(limiter.lookahead_samples, 24); // 0.5ms at 48kHz
    }

    #[test]
    fn test_limiter_threshold_clamping() {
        // Threshold should be clamped to valid range
        let limiter_low = PeakLimiter::new(0.001, 48000);
        assert!((limiter_low.threshold - 0.01).abs() < 0.001);

        let limiter_high = PeakLimiter::new(2.0, 48000);
        assert!((limiter_high.threshold - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_limiter_passes_quiet_signal() {
        let mut limiter = PeakLimiter::new(0.5, 48000); // -6dB threshold

        // Fill look-ahead buffer with the quiet signal itself
        let input: i16 = 1000; // About -30dB, well below threshold
        for _ in 0..50 {
            limiter.process(input);
        }

        // After look-ahead is filled, quiet signal should pass through nearly unchanged
        let output = limiter.process(input);

        // Envelope should be ~1.0 for quiet signal, so output ≈ input
        assert!(
            (output as i32 - input as i32).abs() < 100,
            "Quiet signal should pass through nearly unchanged: {} vs {}",
            output,
            input
        );
    }

    #[test]
    fn test_limiter_reduces_loud_signal() {
        let mut limiter = PeakLimiter::new(0.5, 48000); // -6dB threshold = 16384

        // Fill look-ahead buffer
        for _ in 0..50 {
            limiter.process(0);
        }

        // Process a loud spike (above threshold)
        let spike_value: i16 = 30000; // Well above 16384 threshold
        for _ in 0..100 {
            limiter.process(spike_value);
        }

        // After processing, the output should be reduced
        let output = limiter.process(spike_value);
        assert!(
            (output.abs() as i32) < (spike_value.abs() as i32),
            "Loud signal should be reduced: output {} should be < input {}",
            output,
            spike_value
        );
    }

    #[test]
    fn test_limiter_startup_silence() {
        let mut limiter = PeakLimiter::new(0.5, 48000);

        // During look-ahead fill, output should be 0 (silence)
        for _ in 0..24 {
            // 24 samples = look-ahead time
            let output = limiter.process(10000);
            assert_eq!(output, 0, "Should output silence during look-ahead fill");
        }

        // After look-ahead is filled, we should get actual output
        let output = limiter.process(10000);
        assert_ne!(output, 0, "Should output audio after look-ahead is filled");
    }

    #[test]
    fn test_limiter_process_buffer() {
        let mut limiter = PeakLimiter::new(0.5, 48000);

        // Fill look-ahead
        let mut warmup = vec![0i16; 50];
        limiter.process_buffer(&mut warmup);

        // Create a buffer with some samples
        let mut buffer: Vec<i16> = vec![1000, 2000, 3000, 4000, 5000];
        let original = buffer.clone();

        limiter.process_buffer(&mut buffer);

        // Buffer should be modified
        assert_eq!(buffer.len(), original.len());
    }

    #[test]
    fn test_limiter_reset() {
        let mut limiter = PeakLimiter::new(0.5, 48000);

        // Process some samples
        for _ in 0..100 {
            limiter.process(20000);
        }

        // Envelope should be reduced after processing loud signal
        assert!(limiter.envelope < 1.0);

        // Reset should restore envelope to 1.0
        limiter.reset();
        assert!((limiter.envelope - 1.0).abs() < 0.001);
        assert!(limiter.lookahead_buffer.is_empty());
    }

    #[test]
    fn test_limiter_different_sample_rates() {
        // At 48kHz, 0.5ms = 24 samples
        let limiter_48k = PeakLimiter::new(0.5, 48000);
        assert_eq!(limiter_48k.lookahead_samples, 24);

        // At 44100Hz, 0.5ms = ~22 samples
        let limiter_44k = PeakLimiter::new(0.5, 44100);
        assert_eq!(limiter_44k.lookahead_samples, 22);

        // At 96kHz, 0.5ms = 48 samples
        let limiter_96k = PeakLimiter::new(0.5, 96000);
        assert_eq!(limiter_96k.lookahead_samples, 48);
    }

    #[test]
    fn test_limiter_prevents_clipping() {
        let mut limiter = PeakLimiter::new(0.5, 48000);

        // Fill look-ahead with extreme values
        for _ in 0..50 {
            limiter.process(32767);
        }

        // Process more extreme values and collect outputs
        let outputs: Vec<i16> = (0..100).map(|_| limiter.process(32767)).collect();

        // Outputs should be reduced (limited) below max
        // At 0.5 threshold, output should be around 16384 or less
        let max_output = outputs.iter().map(|&s| s.abs()).max().unwrap_or(0);
        assert!(
            max_output < 32767,
            "Limiter should reduce output below max: got {}",
            max_output
        );
    }

    #[test]
    fn test_limiter_hard_clip_at_threshold() {
        // Test that hard clipper caps output at exactly threshold * 32767
        let threshold = 0.15;
        let mut limiter = PeakLimiter::new(threshold, 48000);

        // Fill look-ahead buffer
        for _ in 0..50 {
            limiter.process(32767);
        }

        // Process extreme values
        let outputs: Vec<i16> = (0..100).map(|_| limiter.process(32767)).collect();

        // Hard clip max should be threshold * 32767 = 0.15 * 32767 = 4915
        let hard_clip_max = (threshold * 32767.0) as i16;
        let max_output = outputs.iter().map(|&s| s.abs()).max().unwrap_or(0);

        assert!(
            max_output <= hard_clip_max,
            "Hard clipper should cap at {}, got {}",
            hard_clip_max,
            max_output
        );
    }

    #[test]
    fn test_limiter_default_threshold_is_aggressive() {
        // Regression test: default threshold should be 0.15 (15%) to prevent loud spikes
        let config = IntercomConfig::default();
        assert!(
            (config.limiter_threshold - 0.15).abs() < 0.001,
            "Default limiter threshold should be 0.15 for aggressive spike prevention, got {}",
            config.limiter_threshold
        );
    }

    #[test]
    fn test_limiter_with_low_threshold() {
        // Test that a low threshold (0.15) effectively limits loud signals
        let mut limiter = PeakLimiter::new(0.15, 48000);

        // Fill look-ahead
        for _ in 0..30 {
            limiter.process(0);
        }

        // Send a spike at max level (simulating plug/unplug transient)
        let spike_outputs: Vec<i16> = (0..50).map(|_| limiter.process(32767)).collect();

        // With 0.15 threshold, max output should be ~4915
        let max_spike = spike_outputs.iter().map(|&s| s.abs()).max().unwrap_or(0);
        assert!(
            max_spike <= 4916, // 0.15 * 32767 rounded up
            "Low threshold should aggressively limit spikes: got {}",
            max_spike
        );
    }
}
