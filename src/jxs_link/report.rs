//! Issue 1387: the receiver's back-channel report (layout in the parent module doc, "The receiver
//! report"), and the per-second window that fills it on the receive side.

/// Report magic byte (`'R'`).
pub const REPORT_MAGIC: u8 = 0x52;
/// Report format version.
pub const REPORT_VERSION: u8 = 1;
/// A report is exactly this long.
pub const REPORT_LEN: usize = 32;
/// `decode_p99_us` when the window decoded no frame.
pub const NO_DECODE_TIME: u32 = u32::MAX;
/// How often the receiver reports.
pub const REPORT_PERIOD_MS: u64 = 1000;

/// One receiver report: what the receiver saw of ONE camera in the last report period.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceiverReport {
    pub camera_id: u8,
    /// The highest bpp the receiver takes (0 = no limit).
    pub max_bpp: u32,
    /// Increments by one per report (a sender acts on one report once).
    pub seq: u32,
    /// Frames finished in the period (decoded or concealed).
    pub frames: u32,
    pub lost_slices: u32,
    /// Frames decoded later than the receiver's deadline after their timecode.
    pub late_frames: u32,
    /// p99 of last datagram in -> frame decoded, microseconds ([`NO_DECODE_TIME`] = none).
    pub decode_p99_us: u32,
    /// The newest frame timecode the period covers (0 = no frame).
    pub last_timecode_100ns: u64,
}

/// Why a datagram is not a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportError {
    WrongLength(usize),
    BadMagic,
    BadVersion(u8),
    /// `max_bpp` does not fit the byte.
    BadBpp,
}

impl ReceiverReport {
    pub fn encode(&self) -> Result<[u8; REPORT_LEN], ReportError> {
        let _ = self;
        Err(ReportError::BadBpp) // RED stub (issue 1387)
    }

    pub fn decode(b: &[u8]) -> Result<Self, ReportError> {
        Err(ReportError::WrongLength(b.len())) // RED stub (issue 1387)
    }
}

/// The receive side's report window (one camera): fed per finished frame, drained once per
/// [`REPORT_PERIOD_MS`] into a [`ReceiverReport`].
#[derive(Debug, Default)]
pub struct ReportWindow {
    seq: u32,
    frames: u32,
    lost_slices: u32,
    late_frames: u32,
    last_tc: u64,
    decode_us: Vec<u32>,
}

impl ReportWindow {
    /// One finished frame: its lost slices, its timecode, whether it was decoded later than the
    /// receiver's deadline, and its decode time (`None` = not decoded).
    pub fn note_frame(
        &mut self,
        lost: u32,
        timecode_100ns: u64,
        late: bool,
        decode_us: Option<u32>,
    ) {
        self.frames = self.frames.saturating_add(1);
        self.lost_slices = self.lost_slices.saturating_add(lost);
        self.late_frames = self.late_frames.saturating_add(late as u32);
        self.last_tc = self.last_tc.max(timecode_100ns);
        if let Some(us) = decode_us {
            self.decode_us.push(us);
        }
    }

    /// Slices lost outside a finished frame (a frame ended by a resync) count too.
    pub fn note_lost(&mut self, lost: u32) {
        self.lost_slices = self.lost_slices.saturating_add(lost);
    }

    /// The report for the period just ended, then a fresh period (the sequence number moves on).
    pub fn take(&mut self, camera_id: u8, max_bpp: u32) -> ReceiverReport {
        self.decode_us.sort_unstable();
        let p99 = if self.decode_us.is_empty() {
            NO_DECODE_TIME
        } else {
            let n = self.decode_us.len();
            self.decode_us[(((n as f64) * 0.99).ceil() as usize).clamp(1, n) - 1]
        };
        let r = ReceiverReport {
            camera_id,
            max_bpp,
            seq: self.seq,
            frames: self.frames,
            lost_slices: self.lost_slices,
            late_frames: self.late_frames,
            decode_p99_us: p99,
            last_timecode_100ns: self.last_tc,
        };
        self.seq = self.seq.wrapping_add(1);
        self.frames = 0;
        self.lost_slices = 0;
        self.late_frames = 0;
        self.last_tc = 0;
        self.decode_us.clear();
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ReceiverReport {
        ReceiverReport {
            camera_id: 5,
            max_bpp: 8,
            seq: 0x0102_0304,
            frames: 30,
            lost_slices: 2,
            late_frames: 1,
            decode_p99_us: 4_321,
            last_timecode_100ns: 17_617_300_123_456_789,
        }
    }

    #[test]
    fn a_report_round_trips_byte_exactly_little_endian() {
        let r = sample();
        let b = r.encode().unwrap();
        assert_eq!(b.len(), REPORT_LEN);
        assert_eq!(&b[..8], &[0x52, 1, 5, 8, 0x04, 0x03, 0x02, 0x01]);
        assert_eq!(&b[8..12], &30u32.to_le_bytes());
        assert_eq!(&b[24..32], &17_617_300_123_456_789u64.to_le_bytes());
        assert_eq!(ReceiverReport::decode(&b), Ok(r));
    }

    #[test]
    fn anything_else_is_refused_never_misread() {
        let b = sample().encode().unwrap();
        assert_eq!(
            ReceiverReport::decode(&b[..31]),
            Err(ReportError::WrongLength(31))
        );
        let mut x = b;
        x[0] = 0x4A; // a JPEG XS slice datagram's magic
        assert_eq!(ReceiverReport::decode(&x), Err(ReportError::BadMagic));
        let mut x = b;
        x[1] = 2;
        assert_eq!(ReceiverReport::decode(&x), Err(ReportError::BadVersion(2)));
        let big = ReceiverReport {
            max_bpp: 300,
            ..sample()
        };
        assert_eq!(big.encode(), Err(ReportError::BadBpp));
        // a slice datagram (16-byte header + data) is never a report
        let mut slice = vec![0x4Au8, 1, 5];
        slice.resize(1472, 0);
        assert!(ReceiverReport::decode(&slice).is_err());
    }

    #[test]
    fn the_window_sums_a_period_then_starts_over_with_the_next_sequence_number() {
        let mut w = ReportWindow::default();
        for i in 0..100u32 {
            w.note_frame(
                if i == 7 { 3 } else { 0 },
                1_000 + i as u64,
                i == 50,
                Some(1_000 + i * 10),
            );
        }
        w.note_lost(2);
        let r = w.take(4, 0);
        assert_eq!(
            (r.camera_id, r.seq, r.frames, r.lost_slices, r.late_frames),
            (4, 0, 100, 5, 1)
        );
        assert_eq!(r.decode_p99_us, 1_980, "nearest-rank p99 of 1000..1990");
        assert_eq!(r.last_timecode_100ns, 1_099);
        let empty = w.take(4, 6);
        assert_eq!(
            (empty.seq, empty.frames, empty.lost_slices, empty.max_bpp),
            (1, 0, 0, 6)
        );
        assert_eq!(empty.decode_p99_us, NO_DECODE_TIME);
        assert_eq!(empty.last_timecode_100ns, 0);
        // a frame that was not decoded has no decode time but still counts
        w.note_frame(1, 5, false, None);
        assert_eq!(w.take(4, 0).decode_p99_us, NO_DECODE_TIME);
    }
}
