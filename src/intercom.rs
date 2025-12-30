//! VBAN Intercom - bidirectional audio over network
//!
//! Receives VBAN audio stream and plays through speakers.
//! Captures microphone audio and sends via VBAN.

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleRate, StreamConfig};
use std::collections::VecDeque;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
// mpsc removed - now sending directly from audio callback
use std::sync::Arc;
use std::sync::Mutex;

use crate::vban::{VbanCodec, VbanHeader, MAX_VBAN_PACKET_SIZE, VBAN_HEADER_SIZE, VBAN_PORT};

/// Intercom configuration
#[derive(Debug, Clone)]
pub struct IntercomConfig {
    /// VBAN stream name to receive/send
    pub stream_name: String,
    /// Target host for sending VBAN
    pub target_host: String,
    /// Sample rate (default: 48000)
    pub sample_rate: u32,
    /// Number of channels (default: 2 for stereo)
    pub channels: u8,
}

impl Default for IntercomConfig {
    fn default() -> Self {
        Self {
            stream_name: "cam1".to_string(),
            target_host: "strih.lan".to_string(),
            sample_rate: 48000,
            channels: 2,
        }
    }
}

/// Audio buffer for thread-safe sample exchange
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
        // Drop old samples if buffer is getting full
        while self.samples.len() + data.len() > self.capacity {
            self.samples.pop_front();
        }
        self.samples.extend(data.iter().copied());
    }

    fn pop_samples(&mut self, count: usize) -> Vec<i16> {
        let available = count.min(self.samples.len());
        self.samples.drain(..available).collect()
    }

    #[allow(dead_code)]
    fn len(&self) -> usize {
        self.samples.len()
    }
}

/// Find audio device by name pattern (case-insensitive partial match)
fn find_audio_device(
    host: &cpal::Host,
    name_pattern: Option<&str>,
    is_input: bool,
) -> Result<cpal::Device> {
    let devices: Vec<_> = if is_input {
        host.input_devices()?.collect()
    } else {
        host.output_devices()?.collect()
    };

    if let Some(pattern) = name_pattern {
        let pattern_lower = pattern.to_lowercase();
        for device in &devices {
            if let Ok(name) = device.name() {
                if name.to_lowercase().contains(&pattern_lower) {
                    tracing::info!(
                        "Found {} device: {}",
                        if is_input { "input" } else { "output" },
                        name
                    );
                    return Ok(device.clone());
                }
            }
        }
    }

    // Fall back to default device
    let device = if is_input {
        host.default_input_device()
    } else {
        host.default_output_device()
    };

    device.ok_or_else(|| {
        anyhow!(
            "No {} audio device found",
            if is_input { "input" } else { "output" }
        )
    })
}

/// Get supported channel count for a device at the given sample rate
fn get_supported_channels(device: &cpal::Device, is_input: bool, sample_rate: u32) -> Option<u16> {
    use cpal::SupportedStreamConfigRange;

    let configs: Vec<SupportedStreamConfigRange> = if is_input {
        device.supported_input_configs().ok()?.collect()
    } else {
        device.supported_output_configs().ok()?.collect()
    };

    // Find a config that supports our sample rate
    for config in configs {
        let min_rate = config.min_sample_rate().0;
        let max_rate = config.max_sample_rate().0;
        if sample_rate >= min_rate && sample_rate <= max_rate {
            // Return the channel count from this config
            return Some(config.channels());
        }
    }

    // Fallback: try to get default config
    let default_config = if is_input {
        device.default_input_config().ok()
    } else {
        device.default_output_config().ok()
    };

    default_config.map(|c| c.channels())
}

/// Run the VBAN receiver (network -> speakers)
fn run_receiver(
    config: &IntercomConfig,
    playback_buffer: Arc<Mutex<AudioBuffer>>,
    running: Arc<AtomicBool>,
    frames_received: Arc<AtomicU64>,
) -> Result<()> {
    let socket = UdpSocket::bind(format!("0.0.0.0:{}", VBAN_PORT))
        .context("Failed to bind VBAN receiver socket")?;
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

                // Decode header
                let header = match VbanHeader::decode(&packet_buf[..len]) {
                    Ok(h) => h,
                    Err(_) => continue,
                };

                // Filter by stream name
                if header.stream_name_str() != config.stream_name {
                    continue;
                }

                // Extract audio data
                let audio_data = &packet_buf[VBAN_HEADER_SIZE..len];
                let codec = header.codec;

                // Convert to i16 samples based on codec
                let samples: Vec<i16> = match codec {
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
                    _ => {
                        // Assume PCM16 for unknown codecs
                        audio_data
                            .chunks_exact(2)
                            .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
                            .collect()
                    }
                };

                // Add to playback buffer
                if let Ok(mut buf) = playback_buffer.lock() {
                    buf.push_samples(&samples);
                }

                frames_received.fetch_add(1, Ordering::Relaxed);
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Timeout, continue
            }
            Err(e) => {
                tracing::warn!("VBAN receive error: {}", e);
            }
        }
    }

    Ok(())
}


/// Run the intercom system
pub fn run_intercom(config: IntercomConfig, running: Arc<AtomicBool>) -> Result<()> {
    tracing::info!(
        "Starting VBAN intercom: stream={}, target={}, {}Hz, {} channels",
        config.stream_name,
        config.target_host,
        config.sample_rate,
        config.channels
    );

    // Initialize audio host
    let host = cpal::default_host();
    tracing::info!("Audio host: {}", host.id().name());

    // Find audio devices
    let input_device = find_audio_device(&host, None, true)?;
    let output_device = find_audio_device(&host, None, false)?;

    tracing::info!("Input device: {}", input_device.name().unwrap_or_default());
    tracing::info!(
        "Output device: {}",
        output_device.name().unwrap_or_default()
    );

    // Query device capabilities and use appropriate channel counts
    // Input: use device capability (typically mono for USB mics)
    let input_channels = get_supported_channels(&input_device, true, config.sample_rate)
        .unwrap_or(1);
    // Output: force stereo (2 channels) since VBAN streams are typically stereo
    // and most output devices support it (ALSA plug may report wrong value)
    let output_channels = 2u16;

    tracing::info!(
        "Audio config: input {} ch, output {} ch @ {}Hz",
        input_channels,
        output_channels,
        config.sample_rate
    );

    // Configure audio streams with device-specific channel counts
    let input_config = StreamConfig {
        channels: input_channels,
        sample_rate: SampleRate(config.sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    let output_config = StreamConfig {
        channels: output_channels,
        sample_rate: SampleRate(config.sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    // Playback buffer for receiving VBAN
    let playback_buffer_capacity = config.sample_rate as usize * output_channels as usize / 2;
    let playback_buffer = Arc::new(Mutex::new(AudioBuffer::new(playback_buffer_capacity)));

    // Statistics
    let frames_received = Arc::new(AtomicU64::new(0));
    let frames_sent = Arc::new(AtomicU64::new(0));
    let samples_captured = Arc::new(AtomicU64::new(0));

    // VBAN sender socket and state (for direct sending from callback)
    let vban_socket = UdpSocket::bind("0.0.0.0:0").context("Failed to create VBAN sender socket")?;
    let target_addr = format!("{}:{}", config.target_host, VBAN_PORT);
    vban_socket.connect(&target_addr)?;
    let vban_socket = Arc::new(vban_socket);

    tracing::info!(
        "VBAN sender targeting {}, stream: {}, mono->stereo",
        target_addr,
        config.stream_name
    );

    // Create output stream (VBAN -> speakers)
    let playback_buf_clone = Arc::clone(&playback_buffer);
    let output_stream = output_device.build_output_stream(
        &output_config,
        move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
            if let Ok(mut buf) = playback_buf_clone.lock() {
                let samples = buf.pop_samples(data.len());
                for (i, sample) in data.iter_mut().enumerate() {
                    *sample = samples.get(i).copied().unwrap_or(0);
                }
            } else {
                // Fill with silence on lock failure
                data.fill(0);
            }
        },
        move |err| {
            tracing::error!("Output stream error: {}", err);
        },
        None,
    )?;

    // Create input stream (microphone -> VBAN directly)
    let samples_captured_clone = Arc::clone(&samples_captured);
    let frames_sent_clone = Arc::clone(&frames_sent);
    let vban_socket_clone = Arc::clone(&vban_socket);
    let frame_counter = Arc::new(AtomicU64::new(0));
    let frame_counter_clone = Arc::clone(&frame_counter);

    let input_stream = input_device.build_input_stream(
        &input_config,
        move |data: &[i16], _: &cpal::InputCallbackInfo| {
            samples_captured_clone.fetch_add(data.len() as u64, Ordering::Relaxed);

            // Split data into chunks for smaller VBAN packets (~128 samples each)
            const CHUNK_SIZE: usize = 128;

            for chunk in data.chunks(CHUNK_SIZE) {
                // Convert mono to stereo
                let stereo_data: Vec<i16> = chunk.iter().flat_map(|&s| [s, s]).collect();

                // Create VBAN packet
                let samples_per_frame = chunk.len();
                let mut packet = vec![0u8; VBAN_HEADER_SIZE + stereo_data.len() * 2];

                // VBAN header
                packet[0..4].copy_from_slice(b"VBAN");
                packet[4] = 3; // Sample rate index for 48000Hz
                packet[5] = (samples_per_frame.saturating_sub(1) & 0xFF) as u8;
                packet[6] = 1; // 2 channels - 1
                packet[7] = 0x01; // PCM16

                // Stream name
                let name = b"cam1";
                packet[8..8 + name.len()].copy_from_slice(name);

                // Frame counter
                let fc = frame_counter_clone.fetch_add(1, Ordering::Relaxed) as u32;
                packet[24..28].copy_from_slice(&fc.to_le_bytes());

                // Audio data (stereo PCM16 LE)
                for (i, &sample) in stereo_data.iter().enumerate() {
                    let bytes = sample.to_le_bytes();
                    packet[VBAN_HEADER_SIZE + i * 2] = bytes[0];
                    packet[VBAN_HEADER_SIZE + i * 2 + 1] = bytes[1];
                }

                // Send packet
                let _ = vban_socket_clone.send(&packet);
                frames_sent_clone.fetch_add(1, Ordering::Relaxed);
            }
        },
        move |err| {
            tracing::error!("Input stream error: {}", err);
        },
        None,
    )?;

    // Start audio streams
    output_stream.play()?;
    input_stream.play()?;
    tracing::info!("Audio streams started");

    // Start VBAN receiver thread
    let recv_config = config.clone();
    let recv_buf = Arc::clone(&playback_buffer);
    let recv_running = Arc::clone(&running);
    let recv_frames = Arc::clone(&frames_received);
    let receiver_thread = std::thread::spawn(move || {
        if let Err(e) = run_receiver(&recv_config, recv_buf, recv_running, recv_frames) {
            tracing::error!("VBAN receiver error: {}", e);
        }
    });

    // Note: VBAN sending is now done directly from the input stream callback

    // Stats reporting loop
    let mut last_received = 0u64;
    let mut last_sent = 0u64;
    let report_interval = std::time::Duration::from_secs(10);
    let mut last_report = std::time::Instant::now();

    while running.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(100));

        if last_report.elapsed() >= report_interval {
            let received = frames_received.load(Ordering::Relaxed);
            let sent = frames_sent.load(Ordering::Relaxed);

            let recv_rate = (received - last_received) as f64 / report_interval.as_secs_f64();
            let send_rate = (sent - last_sent) as f64 / report_interval.as_secs_f64();

            let captured = samples_captured.load(Ordering::Relaxed);
            let capture_rate = captured as f64 / report_interval.as_secs_f64();

            tracing::info!(
                "Intercom: recv {:.1} pkt/s, send {:.1} pkt/s, capture {:.0} samp/s",
                recv_rate,
                send_rate,
                capture_rate
            );

            samples_captured.store(0, Ordering::Relaxed);

            last_received = received;
            last_sent = sent;
            last_report = std::time::Instant::now();
        }
    }

    // Wait for receiver thread to finish
    let _ = receiver_thread.join();

    tracing::info!("VBAN intercom stopped");
    Ok(())
}
