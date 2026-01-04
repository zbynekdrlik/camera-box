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

    /// Sidetone gain multiplier (0.0 = off, default: 30.0)
    #[serde(default = "default_sidetone_gain")]
    pub sidetone_gain: f32,

    /// Microphone gain for outbound VBAN stream (default: 12.0 = +22dB)
    #[serde(default = "default_mic_gain")]
    pub mic_gain: f32,

    /// Headphone gain for incoming VBAN stream (default: 15.0)
    #[serde(default = "default_headphone_gain")]
    pub headphone_gain: f32,

    /// Enable peak limiter on microphone output (default: true)
    #[serde(default = "default_limiter_enabled")]
    pub limiter_enabled: bool,

    /// Limiter threshold as fraction of max (0.5 = -6dB, default: 0.5)
    #[serde(default = "default_limiter_threshold")]
    pub limiter_threshold: f32,
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

fn default_sidetone_gain() -> f32 {
    30.0 // Direct gain multiplier for sidetone
}

fn default_mic_gain() -> f32 {
    12.0 // +22dB boost for outbound mic
}

fn default_headphone_gain() -> f32 {
    15.0 // Headphone volume from strih.lan
}

fn default_limiter_enabled() -> bool {
    true // Limiter on by default to prevent spikes
}

fn default_limiter_threshold() -> f32 {
    0.15 // -16dB ceiling - aggressive to prevent plug/unplug spikes
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_config_default_values() {
        let config = Config::default();
        assert_eq!(config.hostname, "camera-box");
        assert_eq!(config.ndi_name, "usb");
        assert_eq!(config.device, "auto");
        assert!(config.display.is_none());
        assert!(config.intercom.is_none());
    }

    #[test]
    fn test_config_load_nonexistent_returns_default() {
        let result = Config::load("/nonexistent/path/to/config.toml");
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.hostname, "camera-box");
        assert_eq!(config.ndi_name, "usb");
    }

    #[test]
    fn test_config_load_valid_toml() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
hostname = "CAM1"
ndi_name = "camera"
device = "/dev/video0"

[display]
source = "STRIH-SNV"
fb_device = "/dev/fb1"

[intercom]
stream = "cam1"
target = "192.168.1.100"
sample_rate = 44100
channels = 1
sidetone_gain = 15.0
"#
        )
        .unwrap();

        let config = Config::load(file.path()).unwrap();
        assert_eq!(config.hostname, "CAM1");
        assert_eq!(config.ndi_name, "camera");
        assert_eq!(config.device, "/dev/video0");

        let display = config.display.unwrap();
        assert_eq!(display.source, "STRIH-SNV");
        assert_eq!(display.fb_device, "/dev/fb1");

        let intercom = config.intercom.unwrap();
        assert_eq!(intercom.stream, "cam1");
        assert_eq!(intercom.target, "192.168.1.100");
        assert_eq!(intercom.sample_rate, 44100);
        assert_eq!(intercom.channels, 1);
        assert!((intercom.sidetone_gain - 15.0).abs() < 0.001);
    }

    #[test]
    fn test_config_load_partial_toml_uses_defaults() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
hostname = "CAM2"
"#
        )
        .unwrap();

        let config = Config::load(file.path()).unwrap();
        assert_eq!(config.hostname, "CAM2");
        // These should be defaults
        assert_eq!(config.ndi_name, "usb");
        assert_eq!(config.device, "auto");
        assert!(config.display.is_none());
        assert!(config.intercom.is_none());
    }

    #[test]
    fn test_config_load_invalid_toml_error() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "this is not valid toml {{{{").unwrap();

        let result = Config::load(file.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_device_path_explicit() {
        let config = Config {
            device: "/dev/video2".to_string(),
            ..Default::default()
        };
        let path = config.device_path().unwrap();
        assert_eq!(path, "/dev/video2");
    }

    #[test]
    fn test_intercom_config_defaults() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
[intercom]
stream = "test"
"#
        )
        .unwrap();

        let config = Config::load(file.path()).unwrap();
        let intercom = config.intercom.unwrap();
        assert_eq!(intercom.stream, "test");
        // These should be defaults
        assert_eq!(intercom.target, "strih.lan");
        assert_eq!(intercom.sample_rate, 48000);
        assert_eq!(intercom.channels, 2);
        assert!((intercom.sidetone_gain - 30.0).abs() < 0.001);
        assert!((intercom.mic_gain - 12.0).abs() < 0.001);
        assert!((intercom.headphone_gain - 15.0).abs() < 0.001);
        assert!(intercom.limiter_enabled);
        assert!((intercom.limiter_threshold - 0.15).abs() < 0.001);
    }

    #[test]
    fn test_display_config_defaults() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
[display]
source = "NDI Source"
"#
        )
        .unwrap();

        let config = Config::load(file.path()).unwrap();
        let display = config.display.unwrap();
        assert_eq!(display.source, "NDI Source");
        assert_eq!(display.fb_device, "/dev/fb0"); // Default
    }

    #[test]
    fn test_default_function_values() {
        assert_eq!(default_hostname(), "camera-box");
        assert_eq!(default_ndi_name(), "usb");
        assert_eq!(default_device(), "auto");
        assert_eq!(default_fb_device(), "/dev/fb0");
        assert_eq!(default_intercom_stream(), "cam1");
        assert_eq!(default_intercom_target(), "strih.lan");
        assert_eq!(default_intercom_sample_rate(), 48000);
        assert_eq!(default_intercom_channels(), 2);
        assert!((default_sidetone_gain() - 30.0).abs() < 0.001);
        assert!((default_mic_gain() - 12.0).abs() < 0.001);
        assert!((default_headphone_gain() - 15.0).abs() < 0.001);
        assert!(default_limiter_enabled());
        assert!((default_limiter_threshold() - 0.15).abs() < 0.001);
    }

    #[test]
    fn test_config_empty_file_uses_defaults() {
        let file = NamedTempFile::new().unwrap();
        // Empty file - should parse as empty TOML and use all defaults
        let config = Config::load(file.path()).unwrap();
        assert_eq!(config.hostname, "camera-box");
        assert_eq!(config.ndi_name, "usb");
        assert_eq!(config.device, "auto");
    }

    #[test]
    fn test_display_config_clone() {
        let display = DisplayConfig {
            source: "test".to_string(),
            fb_device: "/dev/fb0".to_string(),
        };
        let cloned = display.clone();
        assert_eq!(display.source, cloned.source);
        assert_eq!(display.fb_device, cloned.fb_device);
    }

    #[test]
    fn test_intercom_config_clone() {
        let intercom = IntercomConfig {
            stream: "test".to_string(),
            target: "host.lan".to_string(),
            sample_rate: 48000,
            channels: 2,
            sidetone_gain: 15.0,
            mic_gain: 12.0,
            headphone_gain: 15.0,
            limiter_enabled: true,
            limiter_threshold: 0.5,
        };
        let cloned = intercom.clone();
        assert_eq!(intercom.stream, cloned.stream);
        assert_eq!(intercom.target, cloned.target);
        assert_eq!(intercom.sample_rate, cloned.sample_rate);
        assert_eq!(intercom.channels, cloned.channels);
        assert!((intercom.sidetone_gain - cloned.sidetone_gain).abs() < 0.001);
        assert!((intercom.mic_gain - cloned.mic_gain).abs() < 0.001);
        assert!((intercom.headphone_gain - cloned.headphone_gain).abs() < 0.001);
        assert_eq!(intercom.limiter_enabled, cloned.limiter_enabled);
        assert!((intercom.limiter_threshold - cloned.limiter_threshold).abs() < 0.001);
    }
}
