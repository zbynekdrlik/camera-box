//! cam1 GRAB-RECORD mode (#105 node 2) — record cam1's grabbed frames to a FILE
//! (via dev1) with a per-frame grab timestamp, so the recording-based 4-node verdict
//! can decode cam1's grab WITHOUT an NDI tap.
//!
//! ## Why streamed to dev1, not encoded on cam1
//!
//! cam1 (10.77.9.61) has NO ffmpeg and a 100 MB tmpfs `/tmp`; a raw gray8 1080p30
//! stream is ~62 MB/s (≈112 GB / 30 min) — it cannot be encoded or stored locally.
//! So `--record-grab tcp://dev1:PORT` streams the **gray8 luma plane** of each EMITTED
//! frame (the same genlock-gated frames that reach NDI — see the capture loop) to dev1
//! over TCP, where `ffmpeg -f rawvideo -pix_fmt gray … -c:v ffv1 cam1.mkv` encodes a
//! lossless file the existing `analyze_recording` decodes. gray8 (not full YUYV) halves
//! the wire bytes (so the grab-stream perturbs the measured pipeline less) and still
//! carries the filmed QR fully readably.
//!
//! ## Prod-clean + gated
//!
//! This mode is OFF unless `--record-grab` is given (a TEST flag — normal operation is
//! unaffected). It pulls in NO probe / rqrr / image deps: it only does a dependency-free
//! YUYV→gray8 luma extract ([`crate::capture::yuyv_to_gray8`]) and a socket/file write.
//! The QR decode happens later on dev1 from the recorded file.
//!
//! ## The grab-timestamp sidecar
//!
//! Alongside the video stream, each emitted frame's `(frame_index, grab_ts_ns)` is
//! appended to a local sidecar CSV (tiny — ~30 B/frame, ~3 MB / 30 min, fits cam1's
//! disk). `grab_ts_ns` is CLOCK_REALTIME (the DanteSync wall clock, the SAME domain the
//! cam2 painter stamps), so the recording-verdict computes the REAL cam2→cam1
//! optical+grab latency (`grab_ts − cam2 paint gen_ts`) with NO #111 burn. `frame_index`
//! counts EMITTED frames in order, matching ffmpeg's frame numbering on dev1.

use anyhow::{Context, Result};
use std::io::{BufWriter, Write};
use std::net::TcpStream;
use std::path::Path;

/// Parse the `--record-grab` destination. Only `tcp://HOST:PORT` is supported (the
/// validated path — local encode is impossible on cam1). Returns the `HOST:PORT`
/// authority for [`TcpStream::connect`]. A non-tcp or malformed value errors loudly.
pub fn parse_grab_dest(dest: &str) -> Result<String> {
    let rest = dest
        .strip_prefix("tcp://")
        .with_context(|| format!("--record-grab must be tcp://HOST:PORT (got {dest:?})"))?;
    // Must contain a host and a port.
    let (host, port) = rest
        .rsplit_once(':')
        .with_context(|| format!("--record-grab tcp dest needs HOST:PORT (got {dest:?})"))?;
    if host.is_empty() || port.is_empty() || port.parse::<u16>().is_err() {
        anyhow::bail!("--record-grab tcp dest needs a valid HOST and PORT (got {dest:?})");
    }
    Ok(rest.to_string())
}

/// A live cam1 grab recorder: a TCP stream of gray8 frames to dev1 + a local
/// grab-timestamp sidecar. Created once when `--record-grab` is set; `write_frame`
/// is called for each EMITTED frame from the capture loop.
pub struct GrabRecorder {
    video: BufWriter<TcpStream>,
    sidecar: BufWriter<std::fs::File>,
    width: u32,
    height: u32,
    stride: u32,
    /// 0-based count of frames written (= ffmpeg's frame index on dev1).
    frame_index: u64,
}

impl GrabRecorder {
    /// Connect the video stream to `dest` (`tcp://HOST:PORT`) and open the sidecar CSV
    /// at `sidecar_path`. The dev1-side `ffmpeg … -i tcp://0.0.0.0:PORT?listen` must be
    /// listening first (the harness starts it before launching cam1's camera-box).
    pub fn connect(
        dest: &str,
        sidecar_path: &Path,
        width: u32,
        height: u32,
        stride: u32,
    ) -> Result<Self> {
        let authority = parse_grab_dest(dest)?;
        let sock = TcpStream::connect(&authority)
            .with_context(|| format!("connect grab video stream to {authority}"))?;
        sock.set_nodelay(true).ok();
        let file = std::fs::File::create(sidecar_path)
            .with_context(|| format!("create grab-ts sidecar {}", sidecar_path.display()))?;
        let mut sidecar = BufWriter::new(file);
        writeln!(sidecar, "frame_index,grab_ts_ns").context("write sidecar header")?;
        tracing::info!(
            dest = %authority, sidecar = %sidecar_path.display(), width, height, stride,
            "cam1 grab-record connected (gray8 → dev1 ffv1)"
        );
        Ok(Self {
            video: BufWriter::new(sock),
            sidecar,
            width,
            height,
            stride,
            frame_index: 0,
        })
    }

    /// Tee one EMITTED capture frame: extract its gray8 luma, write it to the video
    /// stream, and append `(frame_index, grab_ts_ns)` to the sidecar. `grab_ts_ns` is
    /// the wall-clock grab instant (CLOCK_REALTIME) of THIS frame. Returns an error on
    /// a broken stream so the caller can stop recording (and keep streaming NDI).
    pub fn write_frame(&mut self, yuyv: &[u8], grab_ts_ns: i64) -> Result<()> {
        let gray = crate::capture::yuyv_to_gray8(yuyv, self.width, self.height, self.stride);
        self.video
            .write_all(&gray)
            .context("write gray8 frame to grab video stream")?;
        writeln!(self.sidecar, "{},{}", self.frame_index, grab_ts_ns)
            .context("append grab-ts sidecar row")?;
        self.frame_index += 1;
        Ok(())
    }

    /// Flush + close both writers (best-effort) — call on shutdown so the last frames
    /// and sidecar rows reach disk/dev1.
    pub fn finish(mut self) {
        let _ = self.video.flush();
        let _ = self.sidecar.flush();
        tracing::info!(frames = self.frame_index, "cam1 grab-record finished");
    }

    /// Frames written so far (for the periodic streaming report).
    pub fn frames_written(&self) -> u64 {
        self.frame_index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_grab_dest_accepts_tcp_host_port() {
        assert_eq!(
            parse_grab_dest("tcp://10.77.9.21:9099").unwrap(),
            "10.77.9.21:9099"
        );
        assert_eq!(parse_grab_dest("tcp://dev1:5000").unwrap(), "dev1:5000");
    }

    #[test]
    fn parse_grab_dest_rejects_non_tcp_or_malformed() {
        assert!(
            parse_grab_dest("/tmp/cam1.mkv").is_err(),
            "local path not supported"
        );
        assert!(parse_grab_dest("tcp://hostonly").is_err(), "needs a port");
        assert!(
            parse_grab_dest("tcp://host:notaport").is_err(),
            "port must be u16"
        );
        assert!(parse_grab_dest("tcp://:9099").is_err(), "needs a host");
        assert!(parse_grab_dest("tcp://host:").is_err(), "empty port");
    }
}
