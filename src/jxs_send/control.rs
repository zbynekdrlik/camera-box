//! Issue 1387: the packet thread's control of the link (the auto bpp's side on `jxs-send`; the
//! parent module doc). Built only with the `jxs` feature.

use super::{jxs_link, BppMode, JxsSocket, PaceControl, SenderOptions, Stats};
use crate::jxs_auto_bpp::{AutoBpp, AutoBppConfig, Decision, Second};
use crate::jxs_link::SendWindow;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The packet thread's side of the bpp (issue 1387 auto bpp): which bpp the frames on the wire
/// carry, the one-second observations, the receiver reports, and (in [`BppMode::Auto`]) the
/// controller whose decisions the feeder applies through `want`.
pub(super) struct BppControl {
    ctl: Option<AutoBpp>,
    want: Arc<AtomicU32>,
    /// The bpp of the session whose frames are being sent.
    session_bpp: u32,
    /// A new session arrived: its first frame tells the controller where it starts.
    apply_pending: bool,
    origin: Instant,
    last_tick: Instant,
    sec_encode_ms: Vec<f64>,
    sec_errors: u64,
    last_report_at: Option<Instant>,
    /// Receiver reports read since the last status line.
    reports: u64,
    /// The automatic pace (issue 1387): fed the receiver's link from every report.
    pub(super) pace: PaceControl,
}

impl BppControl {
    pub(super) fn new(
        opts: SenderOptions,
        want: Arc<AtomicU32>,
        own_iface: Option<String>,
    ) -> Self {
        let mode = opts.bpp;
        let ctl = match mode {
            BppMode::Fixed(_) => None,
            BppMode::Auto { min, max } => Some(AutoBpp::new(
                opts.auto_config
                    .unwrap_or_else(|| AutoBppConfig::new(min, max)),
            )),
        };
        let now = Instant::now();
        Self {
            ctl,
            session_bpp: mode.start(),
            want,
            apply_pending: true,
            origin: now,
            last_tick: now,
            sec_encode_ms: Vec::with_capacity(64),
            sec_errors: 0,
            last_report_at: None,
            reports: 0,
            pace: PaceControl::new(opts.pace_mbps, own_iface),
        }
    }

    /// The bpp of the session whose frames are being sent.
    pub(super) fn session_bpp(&self) -> u32 {
        self.session_bpp
    }

    pub(super) fn on_session(&mut self, bpp: u32) {
        self.session_bpp = bpp;
        self.apply_pending = true;
    }

    /// The first frame of a session is about to be sent.
    pub(super) fn on_frame(&mut self, timecode_100ns: i64) {
        if std::mem::take(&mut self.apply_pending) {
            if let Some(c) = self.ctl.as_mut() {
                c.applied(self.session_bpp, timecode_100ns.max(0) as u64);
            }
        }
    }

    pub(super) fn on_frame_sent(&mut self, encode_ms: f64) {
        self.sec_encode_ms.push(encode_ms);
    }

    pub(super) fn on_frame_error(&mut self) {
        self.sec_errors += 1;
    }

    pub(super) fn on_refused(&mut self, bpp: u32, penalize: bool) {
        let Some(c) = self.ctl.as_mut() else { return };
        // only a step the controller waits on is answered (a geometry refusal at the bpp it
        // already runs is the feeder's own ERROR line, nothing to do here)
        if c.target() != bpp || c.current() == bpp {
            return;
        }
        if penalize {
            c.refused(bpp);
        } else {
            c.cancelled(bpp);
        }
        self.want.store(c.target(), Ordering::Relaxed);
        tracing::warn!(
            "jxs send: no session at {bpp} bpp -- staying at {} (issue 1387)",
            c.current()
        );
    }

    /// Every loop turn: read the receiver's reports; once a second, tick the controller.
    pub(super) fn poll(&mut self, socket: &JxsSocket, camera_id: u8, stats: &Stats) {
        if let Some(r) = socket.take_report(camera_id) {
            let now = Instant::now();
            self.last_report_at = Some(now);
            self.reports += 1;
            self.pace.on_report(&r);
            if let Some(c) = self.ctl.as_mut() {
                c.note_report(r, now.duration_since(self.origin).as_secs_f64());
            }
        }
        if self.last_tick.elapsed() < Duration::from_millis(jxs_link::REPORT_PERIOD_MS) {
            return;
        }
        self.last_tick = Instant::now();
        self.pace.refresh_own_link();
        let dropped = stats.dropped_tick.swap(0, Ordering::Relaxed);
        let (_, p99, _) = crate::jxs_bench::p50_p99_max(&mut self.sec_encode_ms);
        let second = Second {
            frames: self.sec_encode_ms.len() as u32,
            encode_p99_ms: p99,
            dropped_busy: dropped,
            encode_errors: std::mem::take(&mut self.sec_errors),
        };
        self.sec_encode_ms.clear();
        let Some(c) = self.ctl.as_mut() else { return };
        let now_s = self.origin.elapsed().as_secs_f64();
        if let Decision::Step { from, to, reason } = c.tick(&second, now_s) {
            self.want.store(to, Ordering::Relaxed);
            tracing::info!(
                "jxs send: bpp {from} -> {to} ({reason}; second: frames={} encode_p99_ms={:.2} dropped_busy={} encode_errors={}) (issue 1387)",
                second.frames,
                second.encode_p99_ms,
                second.dropped_busy,
                second.encode_errors
            );
        }
    }

    /// The status line's bpp fields (and the window's report count, restarted).
    pub(super) fn fill(&mut self, w: &mut SendWindow) {
        w.reports = std::mem::take(&mut self.reports);
        w.bpp = self.session_bpp;
        w.auto = self.ctl.is_some();
        w.bpp_reason = self.ctl.as_ref().and_then(|c| c.last_reason());
        w.report_age_s = self.last_report_at.map(|t| t.elapsed().as_secs_f64());
        self.pace.fill(w);
    }
}
