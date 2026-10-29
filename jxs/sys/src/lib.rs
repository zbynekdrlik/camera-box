//! Issue 1387: the vendored SVT-JPEG-XS codec behind a thin C shim (`csrc/jxs_shim.c`), and a
//! small safe wrapper the camera-box sender and the receive probe use.
//!
//! The Rust side never re-declares an SVT struct: the shim includes the real headers and passes
//! only scalars and plain pointers, so a library update cannot silently change a layout here.
//!
//! The encoder runs ONE frame at a time ([`Encoder::encode_frame`] returns only after the frame's
//! last packet), which is the camera-box sender's "one frame in flight" rule (a busy encoder drops
//! the next frame, it never queues it). Every buffer is allocated once at open and reused.

use std::os::raw::c_int;

/// Luma lines per slice. The camera-box wire format derives a frame's packet count from it
/// (`crate::jxs_link::packets_per_frame` in camera-box: the header + one packet per slice).
pub const SLICE_HEIGHT_LINES: u32 = 16;

/// `SvtJxsErrorNone`.
const OK: i32 = 0;

#[repr(C)]
struct EncSettings {
    width: u32,
    height: u32,
    bpp_numerator: u32,
    bpp_denominator: u32,
    ndecomp_v: u32,
    threads_num: u32,
    slice_height: u32,
    cpu_profile: u8,
    slice_packets: u8,
}

#[repr(C)]
struct CEnc {
    _private: [u8; 0],
}

#[repr(C)]
struct CDec {
    _private: [u8; 0],
}

extern "C" {
    fn cbjxs_enc_open(s: *const EncSettings, out: *mut *mut CEnc, bytes_per_frame: *mut u32)
        -> i32;
    fn cbjxs_enc_send(
        e: *mut CEnc,
        y: *const u8,
        u: *const u8,
        v: *const u8,
        stride_y: u32,
        stride_c: u32,
        bitstream: *mut u8,
        bitstream_size: u32,
    ) -> i32;
    fn cbjxs_enc_get_packet(
        e: *mut CEnc,
        data: *mut *const u8,
        len: *mut u32,
        last: *mut u8,
    ) -> i32;
    fn cbjxs_enc_close(e: *mut CEnc);
    fn cbjxs_dec_open(
        codestream: *const u8,
        len: usize,
        out: *mut *mut CDec,
        width: *mut u32,
        height: *mut u32,
        format: *mut u32,
        bit_depth: *mut u32,
    ) -> i32;
    #[allow(clippy::too_many_arguments)]
    fn cbjxs_dec_send_packet(
        d: *mut CDec,
        packet: *const u8,
        len: u32,
        y: *mut u8,
        u: *mut u8,
        v: *mut u8,
        stride_y: u32,
        stride_c: u32,
        size_y: u32,
        size_c: u32,
    ) -> i32;
    fn cbjxs_dec_get_frame(d: *mut CDec) -> i32;
    fn cbjxs_dec_close(d: *mut CDec);
    fn cbjxs_format_yuv422() -> u32;
}

/// An SVT-JPEG-XS error code (`SvtJxsErrorType_t`), printed in hex like the library's header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JxsError {
    pub code: i32,
    pub during: &'static str,
}

impl std::fmt::Display for JxsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SVT-JPEG-XS {} failed: error 0x{:08x}",
            self.during, self.code as u32
        )
    }
}

impl std::error::Error for JxsError {}

fn check(code: i32, during: &'static str) -> Result<(), JxsError> {
    if code == OK {
        Ok(())
    } else {
        Err(JxsError { code, during })
    }
}

/// One planar 8-bit YUV 4:2:2 picture: Y is `width x height`, U and V `width/2 x height`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Yuv422Planes {
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

impl Yuv422Planes {
    /// Zero-filled planes. `width` must be even (4:2:2).
    pub fn new(width: u32, height: u32) -> Self {
        let (w, h) = (width as usize, height as usize);
        Self {
            width,
            height,
            y: vec![0; w * h],
            u: vec![0; w / 2 * h],
            v: vec![0; w / 2 * h],
        }
    }

    /// The chroma plane width in samples.
    pub fn chroma_width(&self) -> u32 {
        self.width / 2
    }
}

/// Encoder settings. [`EncoderSettings::issue_1387`] is the measured cambox setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderSettings {
    pub width: u32,
    pub height: u32,
    pub bpp_numerator: u32,
    pub bpp_denominator: u32,
    pub ndecomp_v: u32,
    pub threads_num: u32,
    /// `true` = the library's low-latency profile (cpu_profile 0).
    pub latency_profile: bool,
}

impl EncoderSettings {
    /// Issue 1387 comments 6489539657 / 6489868243: latency profile, decomp_v 2, 4 bpp. On the
    /// N100 the first slice leaves 0.3 ms after the frame is handed over and the frame is decoded
    /// 15.6-15.75 ms after it, ~500 Mb/s at 1080p60.
    pub fn issue_1387(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            bpp_numerator: 4,
            bpp_denominator: 1,
            ndecomp_v: 2,
            threads_num: 0,
            latency_profile: true,
        }
    }
}

/// A slice-packetized encoder that owns its input planes and its bitstream buffer.
pub struct Encoder {
    raw: *mut CEnc,
    planes: Yuv422Planes,
    bitstream: Vec<u8>,
}

// The handle is used from one thread at a time (`&mut self`); the library's own worker threads
// never touch the Rust side.
unsafe impl Send for Encoder {}

impl Encoder {
    /// Open the encoder (it starts its worker threads on the calling thread's CPU set).
    pub fn open(s: EncoderSettings) -> Result<Self, JxsError> {
        if s.width == 0 || s.height == 0 || !s.width.is_multiple_of(2) {
            return Err(JxsError {
                code: i32::MIN,
                during: "open (width must be even and non-zero)",
            });
        }
        let c = EncSettings {
            width: s.width,
            height: s.height,
            bpp_numerator: s.bpp_numerator,
            bpp_denominator: s.bpp_denominator,
            ndecomp_v: s.ndecomp_v,
            threads_num: s.threads_num,
            slice_height: SLICE_HEIGHT_LINES,
            cpu_profile: if s.latency_profile { 0 } else { 1 },
            slice_packets: 1,
        };
        let mut raw = std::ptr::null_mut();
        let mut bytes_per_frame = 0u32;
        // SAFETY: `c` and both out-pointers are valid for the call; the shim only reads `c`.
        check(
            unsafe { cbjxs_enc_open(&c, &mut raw, &mut bytes_per_frame) },
            "encoder open",
        )?;
        Ok(Self {
            raw,
            planes: Yuv422Planes::new(s.width, s.height),
            bitstream: vec![0; bytes_per_frame as usize],
        })
    }

    /// The bitstream budget of one frame in bytes (the codec's fixed bit budget).
    pub fn bytes_per_frame(&self) -> usize {
        self.bitstream.len()
    }

    /// The input planes, to be filled before [`Self::encode_frame`].
    pub fn planes_mut(&mut self) -> &mut Yuv422Planes {
        &mut self.planes
    }

    /// Encode the current planes. `on_packet(index, bytes, is_last)` is called for every packet
    /// the moment the encoder produces it: index 0 is the picture header, then one per slice.
    /// Returns the number of packets. Allocates nothing.
    pub fn encode_frame(
        &mut self,
        mut on_packet: impl FnMut(u32, &[u8], bool),
    ) -> Result<u32, JxsError> {
        let p = &self.planes;
        // SAFETY: the planes and the bitstream buffer are owned by `self`, sized at open for this
        // geometry, and stay untouched until this function returns, which is after the frame's
        // last packet (the library's release point for both buffers).
        check(
            unsafe {
                cbjxs_enc_send(
                    self.raw,
                    p.y.as_ptr(),
                    p.u.as_ptr(),
                    p.v.as_ptr(),
                    p.width,
                    p.chroma_width(),
                    self.bitstream.as_mut_ptr(),
                    self.bitstream.len() as u32,
                )
            },
            "send picture",
        )?;
        let mut index = 0u32;
        loop {
            let mut data: *const u8 = std::ptr::null();
            let mut len = 0u32;
            let mut last = 0u8;
            // SAFETY: valid out-pointers; the call blocks until a packet of this frame is out.
            check(
                unsafe { cbjxs_enc_get_packet(self.raw, &mut data, &mut len, &mut last) },
                "get packet",
            )?;
            let bytes: &[u8] = if data.is_null() || len == 0 {
                &[]
            } else {
                // SAFETY: the library returns a pointer into `self.bitstream` with `len` valid
                // bytes, valid until the frame's last packet (after this loop).
                unsafe { std::slice::from_raw_parts(data, len as usize) }
            };
            on_packet(index, bytes, last != 0);
            index += 1;
            if last != 0 {
                return Ok(index);
            }
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: `raw` came from cbjxs_enc_open and is closed exactly once.
        unsafe { cbjxs_enc_close(self.raw) }
    }
}

/// A packet-mode decoder that owns its output planes.
pub struct Decoder {
    raw: *mut CDec,
    planes: Yuv422Planes,
}

unsafe impl Send for Decoder {}

impl Decoder {
    /// Open from one whole frame's codestream (every packet concatenated): the library's init
    /// parses the full frame (the header packet alone is refused as too short). The frame is
    /// only parsed, not decoded: feed it again packet by packet to decode it.
    pub fn open(header: &[u8]) -> Result<Self, JxsError> {
        let mut raw = std::ptr::null_mut();
        let (mut w, mut h, mut fmt, mut depth) = (0u32, 0u32, 0u32, 0u32);
        // SAFETY: `header` is valid for its length; the out-pointers are valid.
        check(
            unsafe {
                cbjxs_dec_open(
                    header.as_ptr(),
                    header.len(),
                    &mut raw,
                    &mut w,
                    &mut h,
                    &mut fmt,
                    &mut depth,
                )
            },
            "decoder open",
        )?;
        // SAFETY: a plain getter.
        let yuv422 = unsafe { cbjxs_format_yuv422() };
        if fmt != yuv422 || depth != 8 || !w.is_multiple_of(2) {
            // SAFETY: `raw` was just opened and is closed once.
            unsafe { cbjxs_dec_close(raw) };
            return Err(JxsError {
                code: c_int::MIN,
                during: "decoder open (stream is not 8-bit planar 4:2:2)",
            });
        }
        Ok(Self {
            raw,
            planes: Yuv422Planes::new(w, h),
        })
    }

    pub fn width(&self) -> u32 {
        self.planes.width
    }

    pub fn height(&self) -> u32 {
        self.planes.height
    }

    /// Feed one packet (header or slice, in order) of the current frame.
    pub fn send_packet(&mut self, packet: &[u8]) -> Result<(), JxsError> {
        let p = &mut self.planes;
        let (sy, sc) = (p.y.len() as u32, p.u.len() as u32);
        let (wy, wc) = (p.width, p.width / 2);
        // SAFETY: the packet is valid for its length; the planes are owned by `self`, sized for
        // the decoded geometry, and only written by the decoder between this call and
        // `finish_frame`.
        check(
            unsafe {
                cbjxs_dec_send_packet(
                    self.raw,
                    packet.as_ptr(),
                    packet.len() as u32,
                    p.y.as_mut_ptr(),
                    p.u.as_mut_ptr(),
                    p.v.as_mut_ptr(),
                    wy,
                    wc,
                    sy,
                    sc,
                )
            },
            "send packet",
        )
    }

    /// Block until the frame whose packets were all fed is decoded, and return it.
    /// Never call it before every packet of the frame was fed: the library would wait forever.
    pub fn finish_frame(&mut self) -> Result<&Yuv422Planes, JxsError> {
        // SAFETY: a valid handle.
        check(unsafe { cbjxs_dec_get_frame(self.raw) }, "get frame")?;
        Ok(&self.planes)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: `raw` came from cbjxs_dec_open and is closed exactly once.
        unsafe { cbjxs_dec_close(self.raw) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(p: &mut Yuv422Planes) {
        let w = p.width as usize;
        for (i, b) in p.y.iter_mut().enumerate() {
            *b = ((i % w) * 255 / w) as u8;
        }
        for (i, b) in p.u.iter_mut().enumerate() {
            *b = (64 + (i / (w / 2)) % 128) as u8;
        }
        for b in p.v.iter_mut() {
            *b = 160;
        }
    }

    #[test]
    fn a_frame_encodes_to_a_header_and_slices_and_decodes_back() {
        let (w, h) = (256u32, 64u32);
        let mut enc = Encoder::open(EncoderSettings::issue_1387(w, h)).expect("open");
        gradient(enc.planes_mut());
        let original = enc.planes_mut().clone();
        let mut packets: Vec<Vec<u8>> = Vec::new();
        let n = enc
            .encode_frame(|i, b, _| {
                assert_eq!(i as usize, packets.len());
                packets.push(b.to_vec());
            })
            .expect("encode");
        // 64 lines at the default 16-line slice height: the header + 4 slices.
        assert_eq!(n, 5, "packet count");
        assert!(packets.iter().all(|p| !p.is_empty()));
        let total: usize = packets.iter().map(Vec::len).sum();
        assert!(total <= enc.bytes_per_frame(), "{total} > budget");

        // The header packet alone is too short for the library's init: it reads a whole frame.
        assert!(Decoder::open(&packets[0]).is_err());
        let whole: Vec<u8> = packets.concat();
        let mut dec = Decoder::open(&whole).expect("decoder open");
        assert_eq!((dec.width(), dec.height()), (w, h));
        for p in &packets {
            dec.send_packet(p).expect("send packet");
        }
        let out = dec.finish_frame().expect("decode").clone();
        let max_err = original
            .y
            .iter()
            .zip(&out.y)
            .map(|(a, b)| (*a as i32 - *b as i32).abs())
            .max()
            .unwrap();
        assert!(max_err <= 8, "luma max error {max_err}");
    }

    #[test]
    fn an_odd_width_is_refused_before_the_library_sees_it() {
        assert!(Encoder::open(EncoderSettings::issue_1387(255, 64)).is_err());
    }
}
