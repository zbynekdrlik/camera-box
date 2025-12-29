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

/// Run the VBAN sender (microphone -> network)
fn run_sender(
    config: &IntercomConfig,
    capture_buffer: Arc<Mutex<AudioBuffer>>,
    running: Arc<AtomicBool>,
    frames_sent: Arc<AtomicU64>,
) -> Result<()> {
    let socket = UdpSocket::bind("0.0.0.0:0").context("Failed to create VBAN sender socket")?;

    let target_addr = format!("{}:{}", config.target_host, VBAN_PORT);
    tracing::info!(
        "VBAN sender targeting {}, stream: {}",
        target_addr,
        config.stream_name
    );

    let mut header = VbanHeader::new(
        &config.stream_name,
        config.sample_rate,
        config.channels,
        VbanCodec::Pcm16,
    )?;

    // Samples per VBAN packet (typically 256)
    let samples_per_packet = 256;
    let samples_needed = samples_per_packet * config.channels as usize;

    // Calculate packet interval based on sample rate
    let packet_interval = std::time::Duration::from_micros(
        (samples_per_packet as u64 * 1_000_000) / config.sample_rate as u64,
    );

    let mut packet_buf = vec![0u8; VBAN_HEADER_SIZE + samples_needed * 2];
    let mut last_send = std::time::Instant::now();

    while running.load(Ordering::Relaxed) {
        // Wait for packet interval
        let elapsed = last_send.elapsed();
        if elapsed < packet_interval {
            std::thread::sleep(packet_interval - elapsed);
        }
        last_send = std::time::Instant::now();

        // Get samples from capture buffer
        let samples = if let Ok(mut buf) = capture_buffer.lock() {
            buf.pop_samples(samples_needed)
        } else {
            continue;
        };

        if samples.is_empty() {
            continue;
        }

        // Pad with silence if not enough samples
        let mut padded_samples = samples;
        while padded_samples.len() < samples_needed {
            padded_samples.push(0);
        }

        // Encode header
        let header_bytes = header.encode(samples_per_packet);
        packet_buf[..VBAN_HEADER_SIZE].copy_from_slice(&header_bytes);

        // Encode audio data as PCM16 little-endian
        for (i, &sample) in padded_samples.iter().enumerate() {
            let bytes = sample.to_le_bytes();
            packet_buf[VBAN_HEADER_SIZE + i * 2] = bytes[0];
            packet_buf[VBAN_HEADER_SIZE + i * 2 + 1] = bytes[1];
        }

        let packet_len = VBAN_HEADER_SIZE + padded_samples.len() * 2;

        // Send packet
        if let Err(e) = socket.send_to(&packet_buf[..packet_len], &target_addr) {
            tracing::warn!("VBAN send error: {}", e);
        } else {
            header.frame_counter = header.frame_counter.wrapping_add(1);
            frames_sent.fetch_add(1, Ordering::Relaxed);
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

    // Configure audio streams
    let stream_config = StreamConfig {
        channels: config.channels as u16,
        sample_rate: SampleRate(config.sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    // Audio buffers (about 500ms of audio)
    let buffer_capacity = config.sample_rate as usize * config.channels as usize / 2;
    let playback_buffer = Arc::new(Mutex::new(AudioBuffer::new(buffer_capacity)));
    let capture_buffer = Arc::new(Mutex::new(AudioBuffer::new(buffer_capacity)));

    // Statistics
    let frames_received = Arc::new(AtomicU64::new(0));
    let frames_sent = Arc::new(AtomicU64::new(0));

    // Create output stream (VBAN -> speakers)
    let playback_buf_clone = Arc::clone(&playback_buffer);
    let output_stream = output_device.build_output_stream(
        &stream_config,
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

    // Create input stream (microphone -> VBAN)
    let capture_buf_clone = Arc::clone(&capture_buffer);
    let input_stream = input_device.build_input_stream(
        &stream_config,
        move |data: &[i16], _: &cpal::InputCallbackInfo| {
            if let Ok(mut buf) = capture_buf_clone.lock() {
                buf.push_samples(data);
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

    // Start VBAN sender thread
    let send_config = config.clone();
    let send_buf = Arc::clone(&capture_buffer);
    let send_running = Arc::clone(&running);
    let send_frames = Arc::clone(&frames_sent);
    let sender_thread = std::thread::spawn(move || {
        if let Err(e) = run_sender(&send_config, send_buf, send_running, send_frames) {
            tracing::error!("VBAN sender error: {}", e);
        }
    });

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

            tracing::info!(
                "Intercom: recv {:.1} pkt/s, send {:.1} pkt/s",
                recv_rate,
                send_rate
            );

            last_received = received;
            last_sent = sent;
            last_report = std::time::Instant::now();
        }
    }

    // Wait for threads to finish
    let _ = receiver_thread.join();
    let _ = sender_thread.join();

    tracing::info!("VBAN intercom stopped");
    Ok(())
}
