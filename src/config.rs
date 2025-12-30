use anyhow::Result;
use serde::Deserialize;
use std::fs;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Config {
    /// Device hostname
    #[serde(default = "default_hostname")]
    pub hostname: String,

    /// NDI source name (appears as "NAME (hostname)" in NDI)
    #[serde(default = "default_ndi_name")]
    pub ndi_name: String,

    /// Video capture device path ("auto" for auto-detection)
    #[serde(default = "default_device")]
    pub device: String,

    /// NDI display configuration (optional)
    #[serde(default)]
    pub display: Option<DisplayConfig>,

    /// VBAN intercom configuration (optional)
    #[serde(default)]
    pub intercom: Option<IntercomConfig>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DisplayConfig {
    /// NDI source name to display (partial match)
    pub source: String,

    /// Framebuffer device (default: /dev/fb0)
    #[serde(default = "default_fb_device")]
    pub fb_device: String,
}

fn default_fb_device() -> String {
    "/dev/fb0".to_string()
}

#[derive(Debug, Deserialize, Clone)]
pub struct IntercomConfig {
    /// VBAN stream name (default: "cam1")
    #[serde(default = "default_intercom_stream")]
    pub stream: String,

    /// Target host for VBAN (default: "strih.lan")
    #[serde(default = "default_intercom_target")]
    pub target: String,

    /// Sample rate in Hz (default: 48000)
    #[serde(default = "default_intercom_sample_rate")]
    pub sample_rate: u32,

    /// Number of audio channels (default: 2)
    #[serde(default = "default_intercom_channels")]
    pub channels: u8,

    /// Sidetone volume (0.0 = off, 1.0 = full, default: 0.5)
    #[serde(default = "default_sidetone_volume")]
    pub sidetone_volume: f32,
}

fn default_intercom_stream() -> String {
    "cam1".to_string()
}

fn default_intercom_target() -> String {
    "strih.lan".to_string()
}

fn default_intercom_sample_rate() -> u32 {
    48000
}

fn default_intercom_channels() -> u8 {
    2
}

fn default_sidetone_volume() -> f32 {
    1.0 // 100% sidetone by default
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hostname: default_hostname(),
            ndi_name: default_ndi_name(),
            device: default_device(),
            display: None,
            intercom: None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct NetworkConfig {
    /// "dhcp" or "static"
    pub mode: String,
    /// Static IP address with CIDR (e.g., "192.168.1.100/24")
    pub address: Option<String>,
    /// Gateway IP
    pub gateway: Option<String>,
    /// DNS server
    pub dns: Option<String>,
}

fn default_hostname() -> String {
    "camera-box".to_string()
}

fn default_ndi_name() -> String {
    "usb".to_string()
}

fn default_device() -> String {
    "auto".to_string()
}

impl Config {
    /// Load configuration from file, or return defaults if file doesn't exist
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        if path.exists() {
            let content = fs::read_to_string(path)?;
            let config: Config = toml::from_str(&content)?;
            Ok(config)
        } else {
            Ok(Config::default())
        }
    }

    /// Get the video device path, resolving "auto" to first available device
    pub fn device_path(&self) -> Result<String> {
        if self.device == "auto" {
            find_capture_device()
        } else {
            Ok(self.device.clone())
        }
    }
}

/// Find first available V4L2 capture device
fn find_capture_device() -> Result<String> {
    use v4l::device::Device;

    for i in 0..10 {
        let path = format!("/dev/video{}", i);
        if let Ok(device) = Device::with_path(&path) {
            // Check if this device supports video capture
            let caps = device.query_caps()?;
            if caps
                .capabilities
                .contains(v4l::capability::Flags::VIDEO_CAPTURE)
            {
                tracing::info!("Auto-detected capture device: {}", path);
                return Ok(path);
            }
        }
    }
    anyhow::bail!("No video capture device found")
}
