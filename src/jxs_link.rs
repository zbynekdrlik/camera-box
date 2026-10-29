//! Issue 1387 -- the camera-box JPEG XS link: the wire format and the pure pieces both ends share.
//!
//! The owner's transport decision (issue comment 6489239017): our own light UDP protocol, no RTP,
//! no retransmission, no jitter buffer. Each JPEG XS slice goes on the wire the moment the encoder
//! produces it, as one `sendmsg` with UDP GSO (`UDP_SEGMENT`), to ONE configured receiver. A lost
//! slice is concealed from the previous frame at the receiver and counted.
//!
//! # The datagram (THE one place the layout is defined)
//!
//! Every UDP datagram is a 16-byte header followed by up to [`SEGMENT_DATA_LEN`] bytes of one
//! packet of the JPEG XS codestream. Little-endian:
//!
//! | bytes | field | meaning |
//! |---|---|---|
//! | 0 | magic | `0x4A` (`'J'`) |
//! | 1 | version | [`VERSION`] (1) |
//! | 2 | camera id | the box's camera number (`CAM<N>` hostname), 0 if unknown |
//! | 3..10 | timecode | the frame's genlock timecode, 100 ns since the Unix epoch, 56 bits (to 2198) |
//! | 10 | slice index | 0 = the picture header packet, 1.. = the slices, in coding order |
//! | 11 | slice count | packets in this frame (header + slices) |
//! | 12..14 | byte offset | where this datagram's bytes sit inside its slice |
//! | 14..16 | slice length | the whole slice's length in bytes |
//!
//! The timecode is the SAME value the emit gate stamps on the NDI frame (the floor-boundary
//! genlock timecode, `docs/genlock-sender-contract.md`), so the receiver can pair a frame with its
//! NDI twin and measure capture-to-decoded latency on the dantesync-disciplined wall clock.
//!
//! A datagram is at most [`DATAGRAM_LEN`] (1472) bytes: a 1500-byte Ethernet MTU minus the IPv4
//! and UDP headers, so no datagram is ever IP-fragmented. With GSO the kernel (or the NIC) cuts a
//! whole slice into those datagrams, so the sender builds the slice as consecutive
//! `[header][chunk]` pairs and hands it over in one call (see [`build_slice_datagrams`]).
//!
//! Every packet of a frame (header and slices) is a "slice" on the wire; the receiver feeds them
//! to the decoder in index order.
//!
//! # The receiver report (the back channel for the automatic bpp, `crate::jxs_auto_bpp`)
//!
//! Once per second ([`REPORT_PERIOD_MS`]) the receiver sends ONE 32-byte datagram back to the
//! address the slices came from (the sender's connected socket, so nothing else reaches it).
//! Little-endian:
//!
//! | bytes | field | meaning |
//! |---|---|---|
//! | 0 | magic | `0x52` (`'R'`), never a slice datagram's `0x4A` |
//! | 1 | version | [`REPORT_VERSION`] (1) |
//! | 2 | camera id | the camera the report is about |
//! | 3 | max bpp | the highest bpp the receiver takes, 0 = no limit |
//! | 4..8 | seq | +1 per report |
//! | 8..12 | frames | frames finished in the period (decoded or concealed) |
//! | 12..16 | lost slices | slices concealed or missing in the period |
//! | 16..20 | late frames | frames decoded later than the receiver's deadline after their timecode |
//! | 20..24 | decode p99 | last datagram in -> frame decoded, microseconds (`u32::MAX` = none) |
//! | 24..32 | last timecode | the newest frame timecode of the period (100 ns, 0 = no frame) |
//!
//! The last timecode tells the sender WHICH bpp the report describes: a report whose newest frame
//! predates the sender's last bpp change is about the old bpp and steps nothing down.

pub mod assembler;
pub mod pace;
pub mod planar;
pub mod png;
pub mod recv_stats;
pub mod report;

pub use assembler::{FrameAssembler, FrameReport, PushOutcome, SliceSink};
pub use planar::{copy_plane, planar422_to_rgb, psnr, split_interleaved, yuyv_to_planar422};
pub use recv_stats::RecvWindow;
pub use report::{
    ReceiverReport, ReportError, ReportWindow, NO_DECODE_TIME, REPORT_LEN, REPORT_PERIOD_MS,
    REPORT_VERSION,
};

/// Datagram magic byte (`'J'`).
pub const MAGIC: u8 = 0x4A;
/// Wire format version.
pub const VERSION: u8 = 1;
/// Header length in bytes.
pub const HEADER_LEN: usize = 16;
/// The largest datagram: a 1500-byte MTU minus 20 (IPv4) and 8 (UDP).
pub const DATAGRAM_LEN: usize = 1472;
/// Codestream bytes per full datagram.
pub const SEGMENT_DATA_LEN: usize = DATAGRAM_LEN - HEADER_LEN;
/// The timecode field is 56 bits wide.
pub const MAX_TIMECODE_100NS: u64 = (1u64 << 56) - 1;
// 7e9 s after 1970 (the year 2191) still fits.
const _: () = assert!(MAX_TIMECODE_100NS > 7_000_000_000 * 10_000_000);
/// The largest slice the 16-bit length field carries.
pub const MAX_SLICE_LEN: usize = u16::MAX as usize;
/// Datagrams per slice are tracked in a 64-bit mask (a 65535-byte slice needs 46).
pub const MAX_SEGMENTS_PER_SLICE: usize = MAX_SLICE_LEN.div_ceil(SEGMENT_DATA_LEN);
const _: () = assert!(MAX_SEGMENTS_PER_SLICE <= 64);
/// Datagrams handed to the kernel in one GSO send. Linux accepts up to 64 segments (`UDP_MAX_SEGMENTS`
/// before 6.9) and a 65507-byte payload; 40 x 1472 = 58880 stays under both.
pub const GSO_SEGMENTS_PER_SEND: usize = 40;
const _: () = assert!(GSO_SEGMENTS_PER_SEND * DATAGRAM_LEN <= 65507);
/// The encoder's slice height in luma lines (the SVT-JPEG-XS default, set explicitly).
pub const SLICE_HEIGHT_LINES: u32 = 16;

/// The JPEG XS link's frame rate: the strih canvas (owner decision 29.10.2026, issue 1387). NDI
/// stays at the emit rate (60).
pub const JXS_FPS: u64 = 30;
/// The emit rate assumed when genlock is off (no emit grid): the cameras' capture rate.
pub const JXS_ASSUMED_EMIT_FPS: u64 = 60;

/// Whether the frame stamped `timecode_100ns` goes over JPEG XS: the frame on the [`JXS_FPS`]
/// grid, i.e. the EVEN slot of the per-second emit grid (`crate::genlock_grid`), the frame the
/// strih N==2 conveyor presents at the production pin. Decided from the genlock timecode alone,
/// never a frame counter: a dropped capture or a second boundary cannot shift the parity.
/// `emit_fps` is the NDI emit rate (0 = genlock off: [`JXS_ASSUMED_EMIT_FPS`]); an emit rate at
/// or below [`JXS_FPS`] sends every frame. The supported emit rates above it are whole multiples
/// of 30 (the rig emits 60): any other rate keeps only the stamps that happen to fall on the
/// 30 fps grid (50 fps: every 5th, 10 fps).
pub fn on_jxs_grid(timecode_100ns: i64, emit_fps: u32) -> bool {
    use crate::genlock_grid::{per_second_floor, UNITS_100NS_PER_SECOND as U};
    if timecode_100ns < 0 {
        return false;
    }
    let emit = match emit_fps as u64 {
        0 => JXS_ASSUMED_EMIT_FPS,
        f => f,
    };
    if emit <= JXS_FPS {
        return true;
    }
    // the emit-grid stamp of this frame, then: is it a point of the 30 fps grid too? An even
    // 60 fps slot 2j is floor(2j * 1 s / 60) = floor(j * 1 s / 30), the same integer; an odd
    // one lies strictly between two 30 fps points.
    let stamp = per_second_floor(timecode_100ns as u64, emit, U);
    per_second_floor(stamp, JXS_FPS, U) == stamp
}

/// One datagram's header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketHeader {
    pub camera_id: u8,
    pub timecode_100ns: u64,
    pub slice_index: u8,
    pub slice_count: u8,
    pub offset: u16,
    pub slice_len: u16,
}

/// Why a datagram was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderError {
    TooShort,
    BadMagic,
    BadVersion(u8),
    /// slice index not below slice count, or a zero count
    BadIndex,
}

impl PacketHeader {
    /// Write the 16 header bytes.
    pub fn encode(&self, out: &mut [u8; HEADER_LEN]) {
        out[0] = MAGIC;
        out[1] = VERSION;
        out[2] = self.camera_id;
        let tc = (self.timecode_100ns & MAX_TIMECODE_100NS).to_le_bytes();
        out[3..10].copy_from_slice(&tc[..7]);
        out[10] = self.slice_index;
        out[11] = self.slice_count;
        out[12..14].copy_from_slice(&self.offset.to_le_bytes());
        out[14..16].copy_from_slice(&self.slice_len.to_le_bytes());
    }

    /// Read the header from the front of a datagram.
    pub fn decode(datagram: &[u8]) -> Result<Self, HeaderError> {
        if datagram.len() < HEADER_LEN {
            return Err(HeaderError::TooShort);
        }
        if datagram[0] != MAGIC {
            return Err(HeaderError::BadMagic);
        }
        if datagram[1] != VERSION {
            return Err(HeaderError::BadVersion(datagram[1]));
        }
        let mut tc = [0u8; 8];
        tc[..7].copy_from_slice(&datagram[3..10]);
        let h = Self {
            camera_id: datagram[2],
            timecode_100ns: u64::from_le_bytes(tc),
            slice_index: datagram[10],
            slice_count: datagram[11],
            offset: u16::from_le_bytes([datagram[12], datagram[13]]),
            slice_len: u16::from_le_bytes([datagram[14], datagram[15]]),
        };
        if h.slice_count == 0 || h.slice_index >= h.slice_count {
            return Err(HeaderError::BadIndex);
        }
        Ok(h)
    }
}

/// Packets per frame for a picture of `height` luma lines: the header plus one per slice.
pub fn packets_per_frame(height: u32) -> u32 {
    1 + height.div_ceil(SLICE_HEIGHT_LINES)
}

/// Why a slice cannot go on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceError {
    /// The slice is longer than the 16-bit length field ([`MAX_SLICE_LEN`]).
    TooLong(usize),
    /// The frame has more packets than the 8-bit count field.
    TooManySlices(u32),
}

/// Build one slice as consecutive datagrams (`[header][chunk]`, every datagram [`DATAGRAM_LEN`]
/// bytes except the last) into `out` (cleared, its capacity reused: no allocation once warm).
/// Returns the number of datagrams. `base` carries the camera id, timecode, index and count; its
/// offset and length are filled per datagram.
pub fn build_slice_datagrams(
    base: PacketHeader,
    slice: &[u8],
    out: &mut Vec<u8>,
) -> Result<usize, SliceError> {
    if slice.len() > MAX_SLICE_LEN {
        return Err(SliceError::TooLong(slice.len()));
    }
    out.clear();
    let mut hdr = [0u8; HEADER_LEN];
    let mut n = 0;
    // An empty packet still goes out as one header-only datagram, so the receiver sees the index.
    let chunks = slice.chunks(SEGMENT_DATA_LEN);
    let empty = slice.is_empty();
    let mut push = |offset: usize, chunk: &[u8]| {
        let h = PacketHeader {
            offset: offset as u16,
            slice_len: slice.len() as u16,
            ..base
        };
        h.encode(&mut hdr);
        out.extend_from_slice(&hdr);
        out.extend_from_slice(chunk);
    };
    if empty {
        push(0, &[]);
        n = 1;
    } else {
        for (i, chunk) in chunks.enumerate() {
            push(i * SEGMENT_DATA_LEN, chunk);
            n += 1;
        }
    }
    Ok(n)
}

/// The byte ranges of `buf` (built by [`build_slice_datagrams`]) for successive GSO sends of at
/// most [`GSO_SEGMENTS_PER_SEND`] datagrams each. Each range starts on a datagram boundary, so the
/// kernel's `gso_size` cut lines up with the headers.
/// The line-rate case of [`pace::send_ranges`] (one cut, one implementation).
pub fn gso_send_ranges(buf_len: usize) -> impl Iterator<Item = std::ops::Range<usize>> {
    pace::send_ranges(buf_len, GSO_SEGMENTS_PER_SEND)
}

/// Split one received buffer into datagrams. With UDP GRO the kernel hands several datagrams of
/// the same flow as one buffer, every one `gso_size` bytes except the last; without it
/// (`gso_size == None`) the buffer is one datagram.
pub fn split_gro(buf: &[u8], gso_size: Option<usize>) -> impl Iterator<Item = &[u8]> {
    let size = match gso_size {
        Some(s) if s > 0 => s,
        _ => buf.len().max(1),
    };
    buf.chunks(size)
}

/// The camera id for the header: the `CAM<N>` number of the hostname, or 0.
pub fn camera_id_from_hostname(hostname: &str) -> u8 {
    crate::send_stagger::camera_number_from_hostname(hostname)
        .and_then(|n| u8::try_from(n).ok())
        .unwrap_or(0)
}

/// Parse `CAMERA_BOX_JXS_TARGET`. Unset or blank = the link is OFF (`Ok(None)`): nothing is
/// created, nothing is sent. Otherwise `host:port` with a non-zero port.
pub fn parse_target(value: Option<&str>) -> Result<Option<String>, String> {
    let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    let Some((host, port)) = v.rsplit_once(':') else {
        return Err(format!("CAMERA_BOX_JXS_TARGET='{v}' is not host:port"));
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        return Err(format!("CAMERA_BOX_JXS_TARGET='{v}' has no host"));
    }
    match port.parse::<u16>() {
        Ok(p) if p != 0 => Ok(Some(v.to_string())),
        _ => Err(format!("CAMERA_BOX_JXS_TARGET='{v}' has no valid port")),
    }
}

/// The sender's per-window counters (reset every status line).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SendWindow {
    pub frames: u64,
    pub slices: u64,
    pub bytes: u64,
    pub dropped_busy: u64,
    pub send_errors: u64,
    /// Frames not encoded or not sent whole (refused geometry, conversion, codec error, a packet
    /// the wire format cannot carry).
    pub encode_errors: u64,
    /// The most frames the encoder held at once in this window, and how many it may hold.
    pub in_flight_max: u64,
    pub in_flight_cap: u64,
    /// YUYV -> planar conversions on the capture thread (the hand-off's own cost).
    pub convert_count: u64,
    pub convert_ms_sum: f64,
    pub convert_ms_max: f64,
    /// Hand-off (the converted frame is ready) -> its first packet sent.
    pub first_slice_ms_sum: f64,
    pub first_slice_ms_max: f64,
    /// Picture handed to the encoder -> its last packet sent.
    pub encode_ms_sum: f64,
    pub encode_ms_max: f64,
    /// The window's length in seconds (the `fps=` divisor) and the encoder's bits per pixel.
    pub window_s: f64,
    pub bpp: u32,
    /// The automatic bpp is on (issue 1387; off = `CAMERA_BOX_JXS_BPP` pins it).
    pub auto: bool,
    /// Why the bpp last changed (`None` = it never changed).
    pub bpp_reason: Option<crate::jxs_auto_bpp::StepReason>,
    /// Seconds since the last receiver report (`None` = never one).
    pub report_age_s: Option<f64>,
    /// Receiver reports read in this window (one a second from a live receiver; issue 1387
    /// slice 3).
    pub reports: u64,
    /// The frame's genlock timecode -> the wall clock when the capture thread hands it to the
    /// JPEG XS thread ([`HandoffAge`]).
    pub handoff_age: HandoffAge,
    /// The frame's capture instant -> the same hand-off. `handoff_age - capture_age` = where the
    /// capture sits inside its emit slot (the timecode is the slot boundary at or before it).
    pub capture_age: HandoffAge,
    /// The send pacing in Mb/s of wire (issue 1387, `pace`), 0 = line rate.
    pub pace_mbps: u32,
    /// Per whole frame: its first send -> its last send returned (the frame's time on the wire,
    /// paced or not; at line rate it follows the encoder's slice span); the average divides by
    /// `wire_frames` (a frame with no send has no wire time).
    pub wire_frames: u64,
    pub wire_ms_sum: f64,
    pub wire_ms_max: f64,
}

/// The age of an instant (a frame's genlock timecode, or its capture instant) at the hand-off to
/// the JPEG XS thread: the wall clock (`CLOCK_REALTIME`, the dantesync-disciplined clock both are
/// on) minus it, in 100 ns. Positive = it lies in the past. A diagnostic of the cambox side of
/// the link, before any JPEG XS work. The timecode is the emit-grid boundary AT OR BEFORE the
/// stamp instant (`genlock_stamp::genlock_emit_timecode_100ns`): the raw capture instant on the
/// raw path, the phase tracker's slot middle while it drives the stamps (`phase_lock=stamp`). So
/// the timecode's age = (raw capture - its stamped slot start: the capture's phase in its slot, a
/// little outside 0..interval near an edge under the tracker's hysteresis) + the RAW capture's
/// age (capture -> hand-off: V4L2 queue residence, the emit gate).
pub fn handoff_age_100ns(timecode_100ns: i64, now_100ns: i64) -> i64 {
    now_100ns.saturating_sub(timecode_100ns)
}

/// avg / min / max of the hand-off ages of one status window (allocation-free).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HandoffAge {
    pub count: u64,
    pub sum_100ns: i64,
    pub min_100ns: i64,
    pub max_100ns: i64,
}

impl HandoffAge {
    pub fn note(&mut self, age_100ns: i64) {
        if self.count == 0 {
            self.min_100ns = age_100ns;
            self.max_100ns = age_100ns;
        } else {
            self.min_100ns = self.min_100ns.min(age_100ns);
            self.max_100ns = self.max_100ns.max(age_100ns);
        }
        self.count += 1;
        self.sum_100ns = self.sum_100ns.saturating_add(age_100ns);
    }

    /// `(avg, min, max)` in ms, `None` with no sample.
    pub fn avg_min_max_ms(&self) -> Option<(f64, f64, f64)> {
        (self.count > 0).then(|| {
            (
                self.sum_100ns as f64 / self.count as f64 / 1e4,
                self.min_100ns as f64 / 1e4,
                self.max_100ns as f64 / 1e4,
            )
        })
    }

    /// The status line value: `avg/min/max` with 2 decimals, `-/-/-` with no sample.
    pub fn text(&self) -> String {
        self.avg_min_max_ms()
            .map_or("-/-/-".to_string(), |(a, lo, hi)| {
                format!("{a:.2}/{lo:.2}/{hi:.2}")
            })
    }
}

fn avg(sum: f64, n: u64) -> f64 {
    if n > 0 {
        sum / n as f64
    } else {
        0.0
    }
}

impl SendWindow {
    pub fn note_frame(&mut self, slices: u64, bytes: u64, first_slice_ms: f64, encode_ms: f64) {
        self.frames += 1;
        self.slices += slices;
        self.bytes += bytes;
        self.first_slice_ms_sum += first_slice_ms;
        self.first_slice_ms_max = self.first_slice_ms_max.max(first_slice_ms);
        self.encode_ms_sum += encode_ms;
        self.encode_ms_max = self.encode_ms_max.max(encode_ms);
    }

    /// One whole frame's time on the wire (first send -> last send returned).
    pub fn note_wire(&mut self, wire_ms: f64) {
        self.wire_frames += 1;
        self.wire_ms_sum += wire_ms;
        self.wire_ms_max = self.wire_ms_max.max(wire_ms);
    }

    /// The 5 s status line: `jxs send: fps= bpp= auto= bpp_reason= frames= slices= bytes=
    /// dropped_busy= in_flight=max/cap convert_ms_avg/max= handoff_age_ms_avg/min/max=
    /// capture_age_ms_avg/min/max= first_slice_ms_avg/max=
    /// encode_ms_avg/max= send_errors= encode_errors= report_age_s= reports= pace_mbps=
    /// wire_ms_avg/max= (issue 1387)`. `bytes`
    /// counts the codestream, not the headers; `bpp_reason` / `report_age_s` / the two ages read `-` when
    /// there is none.
    pub fn status_line(&self) -> String {
        format!(
            "jxs send: fps={:.1} bpp={} auto={} bpp_reason={} frames={} slices={} bytes={} dropped_busy={} in_flight={}/{} convert_ms_avg/max={:.2}/{:.2} handoff_age_ms_avg/min/max={} capture_age_ms_avg/min/max={} first_slice_ms_avg/max={:.2}/{:.2} encode_ms_avg/max={:.2}/{:.2} send_errors={} encode_errors={} report_age_s={} reports={} pace_mbps={} wire_ms_avg/max={:.2}/{:.2} (issue 1387)",
            if self.window_s > 0.0 {
                self.frames as f64 / self.window_s
            } else {
                0.0
            },
            self.bpp,
            if self.auto { "on" } else { "off" },
            self.bpp_reason.map_or("-".to_string(), |r| r.to_string()),
            self.frames,
            self.slices,
            self.bytes,
            self.dropped_busy,
            self.in_flight_max,
            self.in_flight_cap,
            avg(self.convert_ms_sum, self.convert_count),
            self.convert_ms_max,
            self.handoff_age.text(),
            self.capture_age.text(),
            avg(self.first_slice_ms_sum, self.frames),
            self.first_slice_ms_max,
            avg(self.encode_ms_sum, self.frames),
            self.encode_ms_max,
            self.send_errors,
            self.encode_errors,
            self.report_age_s
                .map_or("-".to_string(), |a| format!("{a:.1}")),
            self.reports,
            self.pace_mbps,
            avg(self.wire_ms_sum, self.frames),
            self.wire_ms_max,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdr() -> PacketHeader {
        PacketHeader {
            camera_id: 5,
            timecode_100ns: 17_618_456_123_456_789,
            slice_index: 3,
            slice_count: 69,
            offset: 2912,
            slice_len: 15_123,
        }
    }

    #[test]
    fn header_round_trips_byte_exactly() {
        let h = hdr();
        let mut b = [0u8; HEADER_LEN];
        h.encode(&mut b);
        assert_eq!(b[0], 0x4A);
        assert_eq!(b[1], 1);
        assert_eq!(b[2], 5);
        assert_eq!(&b[12..14], &2912u16.to_le_bytes());
        assert_eq!(&b[14..16], &15_123u16.to_le_bytes());
        assert_eq!(PacketHeader::decode(&b), Ok(h));
    }

    #[test]
    fn header_carries_every_56_bit_timecode_and_a_2026_one() {
        for tc in [0, 1, 17_618_456_123_456_789, MAX_TIMECODE_100NS] {
            let mut b = [0u8; HEADER_LEN];
            PacketHeader {
                timecode_100ns: tc,
                ..hdr()
            }
            .encode(&mut b);
            assert_eq!(PacketHeader::decode(&b).unwrap().timecode_100ns, tc);
        }
    }

    #[test]
    fn header_refuses_garbage() {
        let mut b = [0u8; HEADER_LEN];
        hdr().encode(&mut b);
        assert_eq!(PacketHeader::decode(&b[..15]), Err(HeaderError::TooShort));
        let mut m = b;
        m[0] = 0x80;
        assert_eq!(PacketHeader::decode(&m), Err(HeaderError::BadMagic));
        let mut v = b;
        v[1] = 2;
        assert_eq!(PacketHeader::decode(&v), Err(HeaderError::BadVersion(2)));
        let mut i = b;
        i[10] = 69;
        assert_eq!(PacketHeader::decode(&i), Err(HeaderError::BadIndex));
        let mut z = b;
        z[11] = 0;
        z[10] = 0;
        assert_eq!(PacketHeader::decode(&z), Err(HeaderError::BadIndex));
    }

    #[test]
    fn a_1080p_frame_is_a_header_plus_68_slices() {
        assert_eq!(packets_per_frame(1080), 69);
        assert_eq!(packets_per_frame(64), 5);
        assert_eq!(packets_per_frame(1), 2);
    }

    #[test]
    fn a_slice_becomes_full_datagrams_and_a_short_tail() {
        let slice: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
        let mut out = Vec::new();
        let base = PacketHeader {
            offset: 0,
            slice_len: 0,
            ..hdr()
        };
        let n = build_slice_datagrams(base, &slice, &mut out).unwrap();
        assert_eq!(n, 3);
        assert_eq!(out.len(), 3 * HEADER_LEN + 3000);
        let dgrams: Vec<&[u8]> = split_gro(&out, Some(DATAGRAM_LEN)).collect();
        assert_eq!(dgrams.len(), 3);
        let mut rebuilt = vec![0u8; 3000];
        for d in &dgrams {
            let h = PacketHeader::decode(d).unwrap();
            assert_eq!(h.slice_len, 3000);
            assert_eq!(h.slice_index, 3);
            let o = h.offset as usize;
            rebuilt[o..o + d.len() - HEADER_LEN].copy_from_slice(&d[HEADER_LEN..]);
        }
        assert_eq!(rebuilt, slice);
        // the capacity is reused for the next slice
        let cap = out.capacity();
        build_slice_datagrams(base, &slice[..100], &mut out).unwrap();
        assert_eq!(out.capacity(), cap);
        assert_eq!(out.len(), HEADER_LEN + 100);
    }

    #[test]
    fn an_empty_packet_is_one_header_only_datagram_and_an_oversized_slice_is_refused() {
        let mut out = Vec::new();
        assert_eq!(build_slice_datagrams(hdr(), &[], &mut out), Ok(1));
        assert_eq!(out.len(), HEADER_LEN);
        let big = vec![0u8; MAX_SLICE_LEN + 1];
        assert_eq!(
            build_slice_datagrams(hdr(), &big, &mut out),
            Err(SliceError::TooLong(MAX_SLICE_LEN + 1))
        );
    }

    #[test]
    fn gso_sends_start_on_datagram_boundaries_and_cover_the_buffer() {
        let len = 100 * DATAGRAM_LEN + 77;
        let ranges: Vec<_> = gso_send_ranges(len).collect();
        assert_eq!(ranges.len(), 3);
        assert_eq!(ranges[0], 0..40 * DATAGRAM_LEN);
        assert_eq!(ranges[2].end, len);
        for r in &ranges {
            assert_eq!(r.start % DATAGRAM_LEN, 0);
        }
        assert_eq!(gso_send_ranges(0).count(), 0);
    }

    #[test]
    fn a_buffer_without_gro_is_one_datagram() {
        let b = [1u8; 300];
        assert_eq!(split_gro(&b, None).count(), 1);
        assert_eq!(split_gro(&b, Some(0)).count(), 1);
        assert_eq!(split_gro(&b, Some(100)).count(), 3);
    }

    #[test]
    fn the_target_is_off_unless_set() {
        assert_eq!(parse_target(None), Ok(None));
        assert_eq!(parse_target(Some("")), Ok(None));
        assert_eq!(parse_target(Some("   ")), Ok(None));
        assert_eq!(
            parse_target(Some("strih.lan:7000")),
            Ok(Some("strih.lan:7000".to_string()))
        );
        assert_eq!(
            parse_target(Some(" 10.77.9.202:7001 ")),
            Ok(Some("10.77.9.202:7001".to_string()))
        );
        assert!(parse_target(Some("strih.lan")).is_err());
        assert!(parse_target(Some("strih.lan:0")).is_err());
        assert!(parse_target(Some(":7000")).is_err());
        assert!(parse_target(Some("h:70000")).is_err());
    }

    #[test]
    fn the_camera_id_comes_from_the_hostname() {
        assert_eq!(camera_id_from_hostname("CAM5"), 5);
        assert_eq!(camera_id_from_hostname("dev1"), 0);
    }

    /// A 60 fps emit stamp on the per-second grid (`floor_boundary_100ns`), slot `k` of second
    /// `sec`.
    fn stamp60(sec: i64, k: i64) -> i64 {
        sec * 10_000_000 + k * 10_000_000 / 60
    }

    #[test]
    fn jpeg_xs_takes_the_even_slots_of_the_per_second_grid() {
        let sec = 1_761_730_000; // 29.10.2026
        let picked: Vec<i64> = (0..60)
            .filter(|&k| on_jxs_grid(stamp60(sec, k), 60))
            .collect();
        assert_eq!(
            picked,
            (0..60).step_by(2).collect::<Vec<_>>(),
            "the even slots, 30 a second"
        );
        // the strih 30 fps grid points ARE the even 60 fps stamps
        for j in 0..30 {
            assert_eq!(stamp60(sec, 2 * j), sec * 10_000_000 + j * 10_000_000 / 30);
        }
    }

    #[test]
    fn the_parity_holds_across_a_second_boundary_and_a_dropped_capture() {
        let sec = 1_761_730_000;
        // slots 56..59 of one second, 0..3 of the next: 56, 58, 0, 2 go (never two in a row,
        // never a gap of 4 at the boundary)
        let run: Vec<(i64, i64)> = (56..60)
            .map(|k| (sec, k))
            .chain((0..4).map(|k| (sec + 1, k)))
            .collect();
        let picked: Vec<(i64, i64)> = run
            .iter()
            .copied()
            .filter(|&(s, k)| on_jxs_grid(stamp60(s, k), 60))
            .collect();
        assert_eq!(picked, [(sec, 56), (sec, 58), (sec + 1, 0), (sec + 1, 2)]);
        // a dropped capture (slot 58 never emitted) costs that one JPEG XS frame only: the next
        // picked frame is still slot 0 of the next second, not the odd slot 59
        let without_58: Vec<(i64, i64)> = run
            .iter()
            .copied()
            .filter(|&(s, k)| !(s == sec && k == 58))
            .filter(|&(s, k)| on_jxs_grid(stamp60(s, k), 60))
            .collect();
        assert_eq!(without_58, [(sec, 56), (sec + 1, 0), (sec + 1, 2)]);
        // and an odd stamp never goes, whatever came before it
        assert!(!on_jxs_grid(stamp60(sec, 59), 60));
    }

    #[test]
    fn a_50_fps_emit_keeps_only_its_stamps_on_the_30_fps_grid() {
        let sec: i64 = 1_761_730_000;
        let kept = (0..50)
            .filter(|&k| on_jxs_grid(sec * 10_000_000 + k * 10_000_000 / 50, 50))
            .count();
        assert_eq!(
            kept, 10,
            "a rate that is no multiple of 30 runs the link at 10 fps"
        );
        // a 120 fps emit keeps every 4th stamp: 30 a second
        let kept = (0..120)
            .filter(|&k| on_jxs_grid(sec * 10_000_000 + k * 10_000_000 / 120, 120))
            .count();
        assert_eq!(kept, 30);
    }

    #[test]
    fn a_30_fps_emit_or_genlock_off_follows_the_emit_rate() {
        let sec = 1_761_730_000;
        // a 30 fps emit: every frame is on the 30 fps grid
        assert!((0..30).all(|j| on_jxs_grid(sec * 10_000_000 + j * 10_000_000 / 30, 30)));
        // genlock off (emit 0): a raw capture instant is floored on the assumed 60 fps grid
        assert!(on_jxs_grid(stamp60(sec, 4) + 1234, 0));
        assert!(!on_jxs_grid(stamp60(sec, 5) + 1234, 0));
        assert!(!on_jxs_grid(-1, 60), "a negative timecode is never sent");
    }

    #[test]
    fn the_status_line_names_every_counter() {
        let mut w = SendWindow::default();
        w.note_frame(69, 1_000_000, 0.4, 14.0);
        w.note_frame(69, 1_000_000, 0.6, 16.0);
        w.dropped_busy = 3;
        w.in_flight_max = 2;
        w.in_flight_cap = 2;
        w.convert_count = 4;
        w.convert_ms_sum = 2.0;
        w.convert_ms_max = 0.75;
        w.window_s = 0.5;
        w.bpp = 6;
        w.auto = true;
        w.bpp_reason = Some(crate::jxs_auto_bpp::StepReason::Clean { needed_s: 10 });
        w.report_age_s = Some(0.43);
        w.reports = 5;
        w.handoff_age.note(210_000);
        w.handoff_age.note(290_000);
        w.pace_mbps = 900;
        w.note_wire(19.5);
        w.note_wire(20.1);
        assert_eq!(
            w.status_line(),
            "jxs send: fps=4.0 bpp=6 auto=on bpp_reason=clean-10s frames=2 slices=138 bytes=2000000 dropped_busy=3 in_flight=2/2 convert_ms_avg/max=0.50/0.75 handoff_age_ms_avg/min/max=25.00/21.00/29.00 capture_age_ms_avg/min/max=-/-/- first_slice_ms_avg/max=0.50/0.60 encode_ms_avg/max=15.00/16.00 send_errors=0 encode_errors=0 report_age_s=0.4 reports=5 pace_mbps=900 wire_ms_avg/max=19.80/20.10 (issue 1387)"
        );
        let empty = SendWindow::default().status_line();
        assert!(empty.contains("convert_ms_avg/max=0.00/0.00"), "{empty}");
        assert!(
            empty.contains("first_slice_ms_avg/max=0.00/0.00"),
            "{empty}"
        );
        // no key contains another: a grep for one never matches another
        assert!(empty.contains("fps=0.0 bpp=0 "), "{empty}");
        let keys = [
            "fps=",
            "bpp=",
            "frames=",
            "slices=",
            "bytes=",
            "dropped_busy=",
            "in_flight=",
            "convert_ms_avg/max=",
            "first_slice_ms_avg/max=",
            "encode_ms_avg/max=",
            "send_errors=",
            "encode_errors=",
            "auto=",
            "bpp_reason=",
            "report_age_s=",
            "reports=",
            "handoff_age_ms_avg/min/max=",
            "capture_age_ms_avg/min/max=",
            "pace_mbps=",
            "wire_ms_avg/max=",
        ];
        assert!(
            empty.contains(" pace_mbps=0 wire_ms_avg/max=0.00/0.00 "),
            "line rate reads 0: {empty}"
        );
        assert!(
            empty.contains(" auto=off bpp_reason=- ")
                && empty.contains(" report_age_s=- ")
                && empty.contains(" handoff_age_ms_avg/min/max=-/-/- "),
            "{empty}"
        );
        for a in keys {
            assert_eq!(empty.matches(a).count(), 1, "{a}");
            for b in keys {
                assert!(a == b || !b.contains(a), "{a} in {b}");
            }
        }
    }

    #[test]
    fn the_handoff_age_is_wall_clock_minus_timecode_with_avg_min_max() {
        let tc = 17_617_300_000_000_000i64;
        assert_eq!(handoff_age_100ns(tc, tc + 214_000), 214_000, "21.4 ms old");
        assert_eq!(
            handoff_age_100ns(tc, tc - 5_000),
            -5_000,
            "a stamp ahead reads negative"
        );
        assert_eq!(handoff_age_100ns(i64::MIN, i64::MAX), i64::MAX, "saturates");
        let mut a = HandoffAge::default();
        assert_eq!(a.avg_min_max_ms(), None);
        assert_eq!(a.text(), "-/-/-");
        // the cam5 sawtooth: 21 ms climbing to 39 ms, then a reset
        for age in [210_000, 245_000, 280_000, 390_000, 215_000] {
            a.note(age);
        }
        assert_eq!(a.count, 5);
        assert_eq!(a.avg_min_max_ms(), Some((26.8, 21.0, 39.0)));
        assert_eq!(a.text(), "26.80/21.00/39.00");
        // a single (negative) sample is its own min and max
        let mut b = HandoffAge::default();
        b.note(-10_000);
        assert_eq!(b.avg_min_max_ms(), Some((-1.0, -1.0, -1.0)));
    }
}
