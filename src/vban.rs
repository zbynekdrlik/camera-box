//! VBAN protocol implementation
//!
//! VBAN (VB-Audio Network) is a simple UDP-based audio streaming protocol.
//! Default port: 6980

use anyhow::{anyhow, Result};

/// VBAN magic header bytes
pub const VBAN_MAGIC: &[u8; 4] = b"VBAN";

/// Default VBAN UDP port
pub const VBAN_PORT: u16 = 6980;

/// VBAN header size in bytes
pub const VBAN_HEADER_SIZE: usize = 28;

/// Maximum stream name length (including null terminator)
pub const VBAN_STREAM_NAME_SIZE: usize = 16;

/// VBAN protocol types
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code)]
pub enum VbanProtocol {
    Audio = 0x00,
    Serial = 0x20,
    Text = 0x40,
    Service = 0x60,
}

/// VBAN sample rates (index -> Hz)
pub const SAMPLE_RATES: &[u32] = &[
    6000, 12000, 24000, 48000, 96000, 192000, 384000, // 0-6
    8000, 16000, 32000, 64000, 128000, 256000, 512000, // 7-13
    11025, 22050, 44100, 88200, 176400, 352800, // 14-19
];

/// VBAN audio codec formats
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code)]
pub enum VbanCodec {
    Pcm8 = 0x00,
    Pcm16 = 0x01,
    Pcm24 = 0x02,
    Pcm32 = 0x03,
    Float32 = 0x04,
    Float64 = 0x05,
}

#[allow(dead_code)]
impl VbanCodec {
    /// Get bytes per sample for this codec
    pub fn bytes_per_sample(&self) -> usize {
        match self {
            VbanCodec::Pcm8 => 1,
            VbanCodec::Pcm16 => 2,
            VbanCodec::Pcm24 => 3,
            VbanCodec::Pcm32 => 4,
            VbanCodec::Float32 => 4,
            VbanCodec::Float64 => 8,
        }
    }
}

/// VBAN packet header
#[derive(Debug, Clone)]
pub struct VbanHeader {
    /// Sample rate index (0-19)
    pub sample_rate_index: u8,
    /// Number of samples per frame (1-256, stored as n-1)
    pub samples_per_frame: u8,
    /// Number of channels (1-256, stored as n-1)
    pub channels: u8,
    /// Data format/codec
    pub codec: u8,
    /// Stream name (up to 16 bytes, null-terminated)
    pub stream_name: [u8; VBAN_STREAM_NAME_SIZE],
    /// Frame counter
    pub frame_counter: u32,
}

impl VbanHeader {
    /// Create a new VBAN header
    pub fn new(
        stream_name: &str,
        sample_rate: u32,
        channels: u8,
        codec: VbanCodec,
    ) -> Result<Self> {
        let sample_rate_index = sample_rate_to_index(sample_rate)
            .ok_or_else(|| anyhow!("Unsupported sample rate: {}", sample_rate))?;

        let mut name_bytes = [0u8; VBAN_STREAM_NAME_SIZE];
        let name_len = stream_name.len().min(VBAN_STREAM_NAME_SIZE - 1);
        name_bytes[..name_len].copy_from_slice(&stream_name.as_bytes()[..name_len]);

        Ok(Self {
            sample_rate_index,
            samples_per_frame: 0, // Will be set per packet
            channels: channels.saturating_sub(1),
            codec: codec as u8,
            stream_name: name_bytes,
            frame_counter: 0,
        })
    }

    /// Get the actual sample rate in Hz
    #[allow(dead_code)]
    pub fn sample_rate(&self) -> u32 {
        let idx = (self.sample_rate_index & 0x1F) as usize;
        if idx < SAMPLE_RATES.len() {
            SAMPLE_RATES[idx]
        } else {
            48000 // Default fallback
        }
    }

    /// Get the actual number of channels
    #[allow(dead_code)]
    pub fn num_channels(&self) -> u8 {
        self.channels.saturating_add(1)
    }

    /// Get the actual samples per frame
    #[allow(dead_code)]
    pub fn num_samples(&self) -> usize {
        (self.samples_per_frame as usize).saturating_add(1)
    }

    /// Encode header to bytes
    pub fn encode(&self, samples_per_frame: usize) -> [u8; VBAN_HEADER_SIZE] {
        let mut buf = [0u8; VBAN_HEADER_SIZE];

        // Magic "VBAN"
        buf[0..4].copy_from_slice(VBAN_MAGIC);

        // Sample rate index (lower 5 bits) + protocol (upper 3 bits)
        buf[4] = self.sample_rate_index & 0x1F; // Audio protocol = 0x00

        // Samples per frame - 1
        buf[5] = (samples_per_frame.saturating_sub(1) & 0xFF) as u8;

        // Channels - 1
        buf[6] = self.channels;

        // Codec/format
        buf[7] = self.codec;

        // Stream name
        buf[8..24].copy_from_slice(&self.stream_name);

        // Frame counter (little-endian)
        buf[24..28].copy_from_slice(&self.frame_counter.to_le_bytes());

        buf
    }

    /// Decode header from bytes
    pub fn decode(data: &[u8]) -> Result<Self> {
        if data.len() < VBAN_HEADER_SIZE {
            return Err(anyhow!("VBAN packet too short: {} bytes", data.len()));
        }

        // Check magic
        if &data[0..4] != VBAN_MAGIC {
            return Err(anyhow!("Invalid VBAN magic"));
        }

        // Check protocol type (upper 3 bits of byte 4)
        let protocol = data[4] & 0xE0;
        if protocol != VbanProtocol::Audio as u8 {
            return Err(anyhow!("Not a VBAN audio packet"));
        }

        let mut stream_name = [0u8; VBAN_STREAM_NAME_SIZE];
        stream_name.copy_from_slice(&data[8..24]);

        Ok(Self {
            sample_rate_index: data[4] & 0x1F,
            samples_per_frame: data[5],
            channels: data[6],
            codec: data[7],
            stream_name,
            frame_counter: u32::from_le_bytes([data[24], data[25], data[26], data[27]]),
        })
    }

    /// Get stream name as string
    pub fn stream_name_str(&self) -> &str {
        let end = self
            .stream_name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(VBAN_STREAM_NAME_SIZE);
        std::str::from_utf8(&self.stream_name[..end]).unwrap_or("")
    }
}

/// Convert sample rate to VBAN index
pub fn sample_rate_to_index(rate: u32) -> Option<u8> {
    SAMPLE_RATES
        .iter()
        .position(|&r| r == rate)
        .map(|i| i as u8)
}

/// Maximum VBAN packet size (header + 256 samples * 8 channels * 4 bytes)
pub const MAX_VBAN_PACKET_SIZE: usize = VBAN_HEADER_SIZE + 256 * 8 * 4;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header_encode_decode() {
        let header = VbanHeader::new("test", 48000, 2, VbanCodec::Pcm16).unwrap();
        let encoded = header.encode(256);
        let decoded = VbanHeader::decode(&encoded).unwrap();

        assert_eq!(decoded.sample_rate(), 48000);
        assert_eq!(decoded.num_channels(), 2);
        assert_eq!(decoded.stream_name_str(), "test");
    }

    #[test]
    fn test_sample_rate_index() {
        assert_eq!(sample_rate_to_index(48000), Some(3));
        assert_eq!(sample_rate_to_index(44100), Some(16));
        assert_eq!(sample_rate_to_index(12345), None);
    }
}
