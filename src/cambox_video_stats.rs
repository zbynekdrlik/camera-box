//! Issue 1540 slice 2 -- the cambox side of the dcontroller video-monitoring contract (schema 1):
//! cumulative per-node + per-connection counters, served read-only on `:8896/video.json` by
//! [`crate::cambox_video_http`].
//!
//! The capture loop already counts everything, but only per 5-s report window, and resets it:
//! `Streaming: N sent, M captured` (`frame_count` / `emit_count`), the decimation gate's
//! starvation repeats + blind sheds, and the JPEG XS sender's per-window `SendWindow` (the
//! `jxs send: ...` line). This module keeps the RUNNING TOTALS:
//!
//! - the capture thread folds each finished window in ([`CamboxVideoCounters::note_capture_window`],
//!   [`CamboxVideoCounters::note_jxs_window`]): a handful of Relaxed `fetch_add`s once per 5 s,
//!   never per frame, never a lock, so the realtime path cannot wait on a reader;
//! - the HTTP thread reads a [`CounterSnapshot`] and maps it with the pure [`render_json`].
//!
//! A snapshot reads its atomics one by one, so a read that races a window fold may see one window
//! of one counter and not yet of another. The consumer takes deltas every 5-10 s; the next read is
//! consistent again. Counters are cumulative since `run_start` (the process start), so a
//! camera-box restart shows as a new `run_start` / `run_id` and counters from zero.
//!
//! Field names follow the draft on issue 1540 (comment 7034656554); the decisions where it was
//! open are recorded in the slice-2 design comment (7126682640). Burn (probe/E2E) mode moves the
//! JPEG XS link to the burn thread, where the capture loop cannot read its windows: the `jxs`
//! record then stays `connected: false` (probe builds only).

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};

/// The contract's schema number; any field change moves it.
pub const SCHEMA: u32 = 1;

/// The port the endpoint listens on unless `CAMERA_BOX_VIDEO_STATS_PORT` says otherwise.
pub const DEFAULT_PORT: u16 = 8896;

/// The env var naming the port; `0` turns the endpoint off.
pub const PORT_ENV: &str = "CAMERA_BOX_VIDEO_STATS_PORT";

/// The served path.
pub const PATH: &str = "/video.json";

/// The port from the env value: unset or empty = [`DEFAULT_PORT`], `0` = off (`None`), a number =
/// that port, anything else = `Err` with the text to log (the caller then keeps the default).
pub fn port_from_env(value: Option<&str>) -> Result<Option<u16>, String> {
    match value.map(str::trim) {
        None | Some("") => Ok(Some(DEFAULT_PORT)),
        Some(v) => match v.parse::<u16>() {
            Ok(0) => Ok(None),
            Ok(p) => Ok(Some(p)),
            Err(_) => Err(format!(
                "{PORT_ENV}={v:?} is not a port number -- serving on {DEFAULT_PORT} (issue 1540)"
            )),
        },
    }
}

/// One finished capture-loop report window (the values the `Streaming:` line prints).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CaptureWindow {
    /// Frames captured in the window (`captured`).
    pub captured: u64,
    /// Frames sent to NDI in the window (`sent`, starvation repeats included).
    pub sent: u64,
    /// The device's capture-dropped count, CUMULATIVE since the capture opened.
    pub capture_dropped_total: u64,
    /// The device's corrupted-buffer count, CUMULATIVE since the capture opened.
    pub corrupted_total: u64,
    /// Starvation last-frame repeats emitted in the window (a repeated frame on the output).
    pub starvation_repeats: u64,
    /// Blind-pacing sheds in the window (a unique captured frame not emitted).
    pub blind_shed: u64,
}

/// The counters of one finished JPEG XS send window, taken from the vstack `SendWindow`.
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
    /// Picture handed to the encoder -> its last packet sent, summed over the window's frames.
    pub encode_ms_sum: f64,
}

impl JxsWindow {
    /// The fields this contract reads from one vstack send window.
    pub fn from_send_window(w: &crate::jxs_link::SendWindow) -> Self {
        Self {
            frames: w.frames,
            slices: w.slices,
            bytes: w.bytes,
            dropped_busy: w.dropped_busy,
            send_errors: w.send_errors,
            encode_errors: w.encode_errors,
            slot_fills: w.slot_fills,
            fec_sent: w.fec_sent,
            fec_repaired: w.fec_repaired,
            bpp: w.bpp,
            pace_mbps: w.pace_mbps,
            encode_ms_sum: w.encode_ms_sum,
        }
    }

    /// The window's mean encode-to-last-send delay in microseconds, `None` without a frame.
    pub fn latency_us(&self) -> Option<u64> {
        (self.frames > 0).then(|| (self.encode_ms_sum * 1000.0 / self.frames as f64).round() as u64)
    }
}

/// "No value" in the atomics that hold an optional number.
const NONE: u64 = u64::MAX;

/// The running totals, shared between the capture thread (writer) and the HTTP thread (reader).
#[derive(Debug)]
pub struct CamboxVideoCounters {
    capture_frames: AtomicU64,
    capture_lost: AtomicU64,
    capture_dropped: AtomicU64,
    capture_corrupted: AtomicU64,
    emit_frames: AtomicU64,
    ndi_duplicated: AtomicU64,
    ndi_skipped: AtomicU64,
    jxs_connected_at_ms: AtomicU64,
    jxs_frames: AtomicU64,
    jxs_slices: AtomicU64,
    jxs_bytes: AtomicU64,
    jxs_dropped_busy: AtomicU64,
    jxs_send_errors: AtomicU64,
    jxs_encode_errors: AtomicU64,
    jxs_slot_fills: AtomicU64,
    jxs_fec_sent: AtomicU64,
    jxs_fec_repaired: AtomicU64,
    jxs_windows: AtomicU64,
    jxs_windows_unread: AtomicU64,
    jxs_last_seq: AtomicU64,
    jxs_bpp: AtomicU64,
    jxs_pace_mbps: AtomicU64,
    jxs_latency_us: AtomicU64,
}

impl Default for CamboxVideoCounters {
    fn default() -> Self {
        let z = || AtomicU64::new(0);
        Self {
            capture_frames: z(),
            capture_lost: z(),
            capture_dropped: z(),
            capture_corrupted: z(),
            emit_frames: z(),
            ndi_duplicated: z(),
            ndi_skipped: z(),
            jxs_connected_at_ms: AtomicU64::new(NONE),
            jxs_frames: z(),
            jxs_slices: z(),
            jxs_bytes: z(),
            jxs_dropped_busy: z(),
            jxs_send_errors: z(),
            jxs_encode_errors: z(),
            jxs_slot_fills: z(),
            jxs_fec_sent: z(),
            jxs_fec_repaired: z(),
            jxs_windows: z(),
            jxs_windows_unread: z(),
            jxs_last_seq: z(),
            jxs_bpp: AtomicU64::new(NONE),
            jxs_pace_mbps: AtomicU64::new(NONE),
            jxs_latency_us: AtomicU64::new(NONE),
        }
    }
}

fn add(a: &AtomicU64, v: u64) {
    a.fetch_add(v, Ordering::Relaxed);
}

fn get(a: &AtomicU64) -> u64 {
    a.load(Ordering::Relaxed)
}

fn opt(a: &AtomicU64) -> Option<u64> {
    Some(get(a)).filter(|v| *v != NONE)
}

impl CamboxVideoCounters {
    /// Fold one finished capture report window in. `capture_lost` adds the window's frame-loss
    /// term `max(0, sent - captured)` (the leg-health definition: an over-rate window is no loss).
    pub fn note_capture_window(&self, w: &CaptureWindow) {
        add(&self.capture_frames, w.captured);
        add(&self.emit_frames, w.sent);
        add(&self.capture_lost, w.sent.saturating_sub(w.captured));
        add(&self.ndi_duplicated, w.starvation_repeats);
        add(&self.ndi_skipped, w.blind_shed);
        // the device counters are cumulative already: the newest value
        self.capture_dropped
            .store(w.capture_dropped_total, Ordering::Relaxed);
        self.capture_corrupted
            .store(w.corrupted_total, Ordering::Relaxed);
    }

    /// The JPEG XS link socket is up (its counters start here). Only the first call counts.
    pub fn note_jxs_connected(&self, now_unix_ms: u64) {
        let _ = self.jxs_connected_at_ms.compare_exchange(
            NONE,
            now_unix_ms.min(NONE - 1),
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }

    /// Fold the JPEG XS window published as sequence `seq` (vstack `take_window`, once per
    /// window). A sequence that jumps past the next one means a window was published and replaced
    /// before it was read: each such window is counted in `windows_unread`, never guessed.
    pub fn note_jxs_window(&self, seq: u64, w: &JxsWindow) {
        let last = self.jxs_last_seq.swap(seq, Ordering::Relaxed);
        if last > 0 && seq > last + 1 {
            add(&self.jxs_windows_unread, seq - last - 1);
        }
        add(&self.jxs_windows, 1);
        add(&self.jxs_frames, w.frames);
        add(&self.jxs_slices, w.slices);
        add(&self.jxs_bytes, w.bytes);
        add(&self.jxs_dropped_busy, w.dropped_busy);
        add(&self.jxs_send_errors, w.send_errors);
        add(&self.jxs_encode_errors, w.encode_errors);
        add(&self.jxs_slot_fills, w.slot_fills);
        add(&self.jxs_fec_sent, w.fec_sent);
        add(&self.jxs_fec_repaired, w.fec_repaired);
        self.jxs_bpp.store(u64::from(w.bpp), Ordering::Relaxed);
        self.jxs_pace_mbps
            .store(u64::from(w.pace_mbps), Ordering::Relaxed);
        if let Some(us) = w.latency_us() {
            self.jxs_latency_us.store(us, Ordering::Relaxed);
        }
    }

    /// The current totals.
    pub fn snapshot(&self) -> CounterSnapshot {
        CounterSnapshot {
            capture_frames: get(&self.capture_frames),
            capture_lost: get(&self.capture_lost),
            capture_dropped: get(&self.capture_dropped),
            capture_corrupted: get(&self.capture_corrupted),
            emit_frames: get(&self.emit_frames),
            ndi_duplicated: get(&self.ndi_duplicated),
            ndi_skipped: get(&self.ndi_skipped),
            jxs_connected_at_ms: opt(&self.jxs_connected_at_ms),
            jxs_frames: get(&self.jxs_frames),
            jxs_slices: get(&self.jxs_slices),
            jxs_bytes: get(&self.jxs_bytes),
            jxs_dropped_busy: get(&self.jxs_dropped_busy),
            jxs_send_errors: get(&self.jxs_send_errors),
            jxs_encode_errors: get(&self.jxs_encode_errors),
            jxs_slot_fills: get(&self.jxs_slot_fills),
            jxs_fec_sent: get(&self.jxs_fec_sent),
            jxs_fec_repaired: get(&self.jxs_fec_repaired),
            jxs_windows: get(&self.jxs_windows),
            jxs_windows_unread: get(&self.jxs_windows_unread),
            jxs_bpp: opt(&self.jxs_bpp),
            jxs_pace_mbps: opt(&self.jxs_pace_mbps),
            jxs_latency_us: opt(&self.jxs_latency_us),
        }
    }
}

/// A plain copy of [`CamboxVideoCounters`] at one read.
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

/// What a node is: fixed for the process lifetime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeMeta {
    /// The node name in the discovery list (the lower-case hostname, `cam2`).
    pub node: String,
    /// The NDI source name this box sends (`CAM2 (usb)`).
    pub ndi_name: String,
    /// `CAMERA_BOX_JXS_TARGET` (`host:port`), `None` = the JPEG XS link is off.
    pub jxs_target: Option<String>,
    /// The process start, unix milliseconds.
    pub run_start_unix_ms: u64,
    /// `<boot id>:<process start ticks>` ([`run_id`]), `None` when unreadable.
    pub run_id: Option<String>,
    /// The camera-box version.
    pub version: String,
}

/// `<boot id>:<process start ticks>` from `/proc/sys/kernel/random/boot_id` and
/// `/proc/self/stat` (field 22, `starttime`), the same run identity the strih browser keeper and
/// the OBS handle facet use. `None` when either is malformed.
pub fn run_id(boot_id: &str, proc_self_stat: &str) -> Option<String> {
    let boot = boot_id.trim();
    if boot.is_empty() {
        return None;
    }
    // the command name (field 2) may hold spaces and parentheses: fields after the LAST ')'
    // start at field 3 (state), so starttime (22) is the 20th of them
    let rest = &proc_self_stat[proc_self_stat.rfind(')')? + 1..];
    let start: u64 = rest.split_whitespace().nth(19)?.parse().ok()?;
    Some(format!("{boot}:{start}"))
}

/// Unix seconds as RFC 3339 UTC (`2026-11-20T09:30:05Z`), the contract's timestamp form.
pub fn rfc3339_utc(unix_s: u64) -> String {
    let days = (unix_s / 86_400) as i64;
    let secs = unix_s % 86_400;
    // civil-from-days (H. Hinnant), days since 1970-01-01
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// The wall clock, unix milliseconds (0 before 1970, never a panic).
pub fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// This process's [`run_id`] read from `/proc` (`None` off Linux or when unreadable).
pub fn read_run_id() -> Option<String> {
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    run_id(&boot, &stat)
}

fn ts(unix_ms: u64) -> String {
    rfc3339_utc(unix_ms / 1000)
}

/// The schema-1 document for one read at `read_at_unix_ms`.
pub fn render_json(meta: &NodeMeta, s: &CounterSnapshot, read_at_unix_ms: u64) -> Value {
    let run_start = ts(meta.run_start_unix_ms);
    let mut connections = vec![json!({
        "id": format!("ndi:{}->{}", meta.node, meta.ndi_name),
        "source": meta.node,
        "sink": Value::Null,
        "sink_input": Value::Null,
        "stream_name": meta.ndi_name,
        "kind": "ndi",
        "direction": "out",
        "latency_ms": Value::Null,
        "frames_sent": s.emit_frames,
        "frames_duplicated": s.ndi_duplicated,
        "frames_skipped": s.ndi_skipped,
        "counters_since": run_start,
    })];
    if let Some(target) = &meta.jxs_target {
        connections.push(json!({
            "id": format!("jxs:{}->{}", meta.node, target),
            "source": meta.node,
            "sink": target,
            "sink_input": Value::Null,
            "kind": "jxs",
            "direction": "out",
            "connected": s.jxs_connected_at_ms.is_some(),
            "latency_ms": s.jxs_latency_us.map(|us| us as f64 / 1000.0),
            "frames_sent": s.jxs_frames,
            "frames_dropped_busy": s.jxs_dropped_busy,
            "slices_sent": s.jxs_slices,
            "bytes_sent": s.jxs_bytes,
            "send_errors": s.jxs_send_errors,
            "encode_errors": s.jxs_encode_errors,
            "slot_fills": s.jxs_slot_fills,
            "fec_sent": s.jxs_fec_sent,
            "fec_repaired": s.jxs_fec_repaired,
            "bpp": s.jxs_bpp,
            "pace_mbps": s.jxs_pace_mbps,
            "windows": s.jxs_windows,
            "windows_unread": s.jxs_windows_unread,
            "counters_since": s.jxs_connected_at_ms.map(ts),
        }));
    }
    json!({
        "schema": SCHEMA,
        "node": meta.node,
        "kind": "cambox",
        "version": meta.version,
        "read_at": ts(read_at_unix_ms),
        "run_start": run_start,
        "run_id": meta.run_id,
        "counters_since": run_start,
        "capture_frames": s.capture_frames,
        "capture_lost": s.capture_lost,
        "capture_dropped": s.capture_dropped,
        "capture_corrupted": s.capture_corrupted,
        "emit_frames": s.emit_frames,
        "send_dropped_busy": s.jxs_dropped_busy,
        "connections": connections,
    })
}
