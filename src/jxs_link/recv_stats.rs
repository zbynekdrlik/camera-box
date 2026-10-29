//! Issue 1387: the receive probe's per-window statistics (pure; buffers reused per window).

/// Sorted-sample percentile (nearest rank). `None` for no samples.
pub fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    Some(sorted[rank.clamp(1, sorted.len()) - 1])
}

/// One receive window (the probe prints one line per window and resets it).
#[derive(Debug, Default)]
pub struct RecvWindow {
    pub frames: u64,
    pub concealed_frames: u64,
    pub lost_slices: u64,
    pub undecodable_frames: u64,
    pub datagrams: u64,
    pub bytes: u64,
    pub duplicate: u64,
    pub stale: u64,
    pub malformed: u64,
    pub resyncs: u64,
    /// Sender timecode -> frame decoded here, ms (both on the dantesync wall clock).
    latency_ms: Vec<f64>,
    /// Sender timecode -> first datagram on the wire here, ms.
    first_arrival_ms: Vec<f64>,
    /// First -> last datagram of a frame, ms (the wire-side arrival spread).
    spread_ms: Vec<f64>,
}

impl RecvWindow {
    pub fn note_frame(
        &mut self,
        latency_ms: f64,
        first_arrival_ms: f64,
        spread_ms: f64,
        lost: u32,
    ) {
        self.frames += 1;
        self.lost_slices += lost as u64;
        if lost > 0 {
            self.concealed_frames += 1;
        }
        self.latency_ms.push(latency_ms);
        self.first_arrival_ms.push(first_arrival_ms);
        self.spread_ms.push(spread_ms);
    }

    /// Clear for the next window, keeping the sample buffers' capacity.
    pub fn reset(&mut self) {
        let (mut a, mut b, mut c) = (
            std::mem::take(&mut self.latency_ms),
            std::mem::take(&mut self.first_arrival_ms),
            std::mem::take(&mut self.spread_ms),
        );
        a.clear();
        b.clear();
        c.clear();
        *self = Self {
            latency_ms: a,
            first_arrival_ms: b,
            spread_ms: c,
            ..Self::default()
        };
    }

    /// `jxs recv: fps= frames= concealed= lost_slices= ... (issue 1387)` for a window of
    /// `window_s` seconds. Sorts the sample buffers in place.
    pub fn status_line(&mut self, window_s: f64) -> String {
        let fmt = |v: Option<f64>| v.map_or("na".to_string(), |x| format!("{x:.2}"));
        let cmp = |a: &f64, b: &f64| a.total_cmp(b);
        self.latency_ms.sort_by(cmp);
        self.first_arrival_ms.sort_by(cmp);
        self.spread_ms.sort_by(cmp);
        let fps = if window_s > 0.0 {
            self.frames as f64 / window_s
        } else {
            0.0
        };
        let mbps = if window_s > 0.0 {
            self.bytes as f64 * 8.0 / window_s / 1e6
        } else {
            0.0
        };
        format!(
            "jxs recv: fps={fps:.2} frames={} concealed={} lost_slices={} undecodable={} datagrams={} mbps={mbps:.1} \
             latency_ms_p50/p99/max={}/{}/{} first_arrival_ms_p50/max={}/{} spread_ms_p50/max={}/{} \
             dup={} stale={} malformed={} resyncs={} (issue 1387)",
            self.frames,
            self.concealed_frames,
            self.lost_slices,
            self.undecodable_frames,
            self.datagrams,
            fmt(percentile(&self.latency_ms, 50.0)),
            fmt(percentile(&self.latency_ms, 99.0)),
            fmt(self.latency_ms.last().copied()),
            fmt(percentile(&self.first_arrival_ms, 50.0)),
            fmt(self.first_arrival_ms.last().copied()),
            fmt(percentile(&self.spread_ms, 50.0)),
            fmt(self.spread_ms.last().copied()),
            self.duplicate,
            self.stale,
            self.malformed,
            self.resyncs,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_percentiles() {
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(percentile(&v, 50.0), Some(50.0));
        assert_eq!(percentile(&v, 99.0), Some(99.0));
        assert_eq!(percentile(&v, 100.0), Some(100.0));
        assert_eq!(percentile(&[7.0], 1.0), Some(7.0));
        assert_eq!(percentile(&[], 50.0), None);
    }

    #[test]
    fn the_status_line_reports_the_window_and_reset_keeps_capacity() {
        let mut w = RecvWindow::default();
        for i in 0..60 {
            w.note_frame(15.0 + i as f64 * 0.01, 1.0, 0.5, if i == 3 { 2 } else { 0 });
        }
        w.bytes = 60 * 1_000_000;
        w.datagrams = 60 * 700;
        let line = w.status_line(1.0);
        assert!(
            line.starts_with("jxs recv: fps=60.00 frames=60 concealed=1 lost_slices=2 "),
            "{line}"
        );
        assert!(line.contains("mbps=480.0"), "{line}");
        assert!(
            line.contains("latency_ms_p50/p99/max=15.29/15.59/15.59"),
            "{line}"
        );
        assert!(line.ends_with("(issue 1387)"));
        let cap = w.latency_ms.capacity();
        w.reset();
        assert_eq!(w.frames, 0);
        assert_eq!(w.latency_ms.capacity(), cap);
        assert!(w
            .status_line(1.0)
            .contains("latency_ms_p50/p99/max=na/na/na"));
    }
}
