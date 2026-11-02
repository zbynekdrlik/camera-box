//! Issue 1387 slice 3: a JPEG XS link started by a caller that is not the cambox capture loop
//! (the strih OBS program output through `jxs/send-ffi`): the id, the cores and the egress mark
//! are given, the same sender threads run.

#[cfg(feature = "jxs")]
use super::{bpp_mode_text, handoff, slice_threads_for, JxsSocket, FRAMES_IN_FLIGHT};
use super::{jxs_link, JxsHandoff, JxsSender, SenderOptions};

/// A link started by a caller that is not the cambox capture loop (issue 1387 slice 3: the strih
/// OBS program output through `jxs/send-ffi`): the id, the cores and the egress mark are given,
/// not derived from the cambox's hostname and capture split.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkConfig {
    /// `host:port` (IPv4).
    pub target: String,
    /// The wire's camera id (a stream id outside the CAM range, e.g. 100, for the program).
    pub camera_id: u8,
    /// The encoder's cores (empty = every online core).
    pub cores: Vec<usize>,
    pub opts: SenderOptions,
    /// Set [`super::JXS_SOCKET_MARK`] (a cambox's egress pacing exemption; off elsewhere).
    pub egress_mark: bool,
}

impl JxsSender {
    /// Start a link for [`LinkConfig`]; `Err` names why it could not (no thread exists then).
    pub fn start_link(cfg: LinkConfig) -> Result<JxsHandoff, String> {
        let target = match jxs_link::parse_target(Some(&cfg.target)) {
            Ok(Some(t)) => t,
            Ok(None) => return Err("no target".to_string()),
            Err(e) => return Err(e),
        };
        start_link_worker(target, cfg)
    }
}

#[cfg(not(feature = "jxs"))]
fn start_link_worker(_target: String, _cfg: LinkConfig) -> Result<JxsHandoff, String> {
    Err("camera-box was built without the `jxs` feature".to_string())
}

#[cfg(feature = "jxs")]
fn start_link_worker(target: String, cfg: LinkConfig) -> Result<JxsHandoff, String> {
    let socket =
        JxsSocket::connect_with(&target, cfg.egress_mark).map_err(|e| format!("{target}: {e}"))?;
    let cores = if cfg.cores.is_empty() {
        crate::affinity::online_cores()
    } else {
        cfg.cores
    };
    tracing::info!(
        "jxs send: link ON to {} (stream id {}, UDP GSO {}, egress mark {}, send buffer {} bytes) -- JPEG XS {} at up to {} fps, latency profile, {} frames in flight, encoder on cores {cores:?} with {} slice threads, send {} (issue 1387)",
        socket
            .peer()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| target.clone()),
        cfg.camera_id,
        if socket.gso() { "on" } else { "OFF: one send per datagram" },
        socket.egress_text(),
        socket.send_buffer(),
        bpp_mode_text(cfg.opts.bpp),
        jxs_link::JXS_FPS,
        FRAMES_IN_FLIGHT,
        slice_threads_for(&cores),
        super::pace_text(cfg.opts.pace_mbps),
    );
    handoff::spawn(socket, cfg.camera_id, cores, cfg.opts)
        .ok_or_else(|| "the sender threads could not be spawned".to_string())
}
