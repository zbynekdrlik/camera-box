//! Issue 1540 slice 2 -- the cambox side of the dcontroller video-monitoring contract (schema 1).
//! RED: the API shape only; the counters, the mapping and the parsers are not implemented yet.

use serde_json::{json, Value};

/// The contract's schema number; any field change moves it.
pub const SCHEMA: u32 = 1;

/// The port the endpoint listens on unless `CAMERA_BOX_VIDEO_STATS_PORT` says otherwise.
pub const DEFAULT_PORT: u16 = 0;

/// The env var naming the port; `0` turns the endpoint off.
pub const PORT_ENV: &str = "CAMERA_BOX_VIDEO_STATS_PORT";

/// The served path.
pub const PATH: &str = "/video.json";

/// Not implemented yet.
pub fn port_from_env(_value: Option<&str>) -> Result<Option<u16>, String> {
    Ok(None)
}

/// One finished capture-loop report window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CaptureWindow {
    pub captured: u64,
    pub sent: u64,
    pub capture_dropped_total: u64,
    pub corrupted_total: u64,
    pub starvation_repeats: u64,
    pub blind_shed: u64,
}

/// The counters of one finished JPEG XS send window.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct JxsWindow {
    pub frames: u64,
    pub slices: u64,
    pub bytes: u64,
    pub dropped_busy: u64,
    pub send_errors: u64,
    pub encode_errors: u64,
    pub slot_fills: u64,
    pub fec_sent: u64,
    pub fec_repaired: u64,
    pub bpp: u32,
    pub pace_mbps: u32,
    pub encode_ms_sum: f64,
}

/// The running totals (not implemented yet).
#[derive(Debug, Default)]
pub struct CamboxVideoCounters;

impl CamboxVideoCounters {
    pub fn note_capture_window(&self, _w: &CaptureWindow) {}
    pub fn note_jxs_connected(&self, _now_unix_ms: u64) {}
    pub fn note_jxs_window(&self, _seq: u64, _w: &JxsWindow) {}
    pub fn snapshot(&self) -> CounterSnapshot {
        CounterSnapshot::default()
    }
}

/// A plain copy of the counters at one read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CounterSnapshot {
    pub capture_frames: u64,
    pub capture_lost: u64,
    pub capture_dropped: u64,
    pub capture_corrupted: u64,
    pub emit_frames: u64,
    pub ndi_duplicated: u64,
    pub ndi_skipped: u64,
    pub jxs_connected_at_ms: Option<u64>,
    pub jxs_frames: u64,
    pub jxs_slices: u64,
    pub jxs_bytes: u64,
    pub jxs_dropped_busy: u64,
    pub jxs_send_errors: u64,
    pub jxs_encode_errors: u64,
    pub jxs_slot_fills: u64,
    pub jxs_fec_sent: u64,
    pub jxs_fec_repaired: u64,
    pub jxs_windows: u64,
    pub jxs_windows_unread: u64,
    pub jxs_bpp: Option<u64>,
    pub jxs_pace_mbps: Option<u64>,
    pub jxs_latency_us: Option<u64>,
}

/// What a node is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeMeta {
    pub node: String,
    pub ndi_name: String,
    pub jxs_target: Option<String>,
    pub run_start_unix_ms: u64,
    pub run_id: Option<String>,
    pub version: String,
}

/// Not implemented yet.
pub fn run_id(_boot_id: &str, _proc_self_stat: &str) -> Option<String> {
    None
}

/// Not implemented yet.
pub fn rfc3339_utc(_unix_s: u64) -> String {
    String::new()
}

/// Not implemented yet.
pub fn render_json(_meta: &NodeMeta, _s: &CounterSnapshot, _read_at_unix_ms: u64) -> Value {
    json!({})
}
