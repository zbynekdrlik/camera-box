use anyhow::Result;
use serde::Deserialize;
use std::fs;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Config {
    /// Device hostname (NDI source will always be "usb")
    #[serde(default = "default_hostname")]
    pub hostname: String,

    /// Video capture device path ("auto" for auto-detection)
    #[serde(default = "default_device")]
    pub device: String,

    /// Network configuration (optional, for future use)
    #[allow(dead_code)]
    pub network: Option<NetworkConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hostname: default_hostname(),
            device: default_device(),
            network: None,
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
            if caps.capabilities.contains(v4l::capability::Flags::VIDEO_CAPTURE) {
                tracing::info!("Auto-detected capture device: {}", path);
                return Ok(path);
            }
        }
    }
    anyhow::bail!("No video capture device found")
}
