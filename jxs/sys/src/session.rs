//! Issue 1387 slice 1b: a PIPELINED encode session, so a frame can be handed to the library while
//! the previous one's slices are still coming out.
//!
//! The library allows it: `send_picture` only posts into a 10-deep input FIFO, the final stage
//! re-orders the output by frame number, and `get_packet` reads one output FIFO. What it needs is
//! that every picture's planes and bitstream stay untouched until that picture's LAST packet.
//!
//! So a session is split in two halves that share one handle:
//! - [`EncoderInput::submit`] moves a [`Picture`] INTO the session (the C side now reads its
//!   planes and writes its bitstream);
//! - [`EncoderOutput::recv_frame`] reads the oldest picture's packets and, after its last one,
//!   moves the picture back OUT.
//!
//! The two halves may live on two threads (the camera-box sender's `jxs-feed` and `jxs-send`).
//! Only the output half calls `get_packet`, and only it may close the session early
//! ([`EncoderOutput::abandon`], after a codec error); a closed session refuses every further
//! submit and returns the pictures it still held.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::{
    cbjxs_enc_close, cbjxs_enc_get_packet, cbjxs_enc_open, cbjxs_enc_send, check, CEnc,
    EncSettings, EncoderSettings, JxsError, Yuv422Planes, SLICE_HEIGHT_LINES,
};

/// The most pictures a session holds at once (the library's input FIFO is 10 deep, so a submit
/// never blocks on it).
pub const MAX_IN_FLIGHT: usize = 8;

/// The error code of a session that is closed or holds no picture (never a library code).
const SESSION_CODE: i32 = i32::MIN + 1;

/// One picture of a pipelined session: its input planes and its bitstream buffer. Small to move
/// (the planes are boxed), so a refused submit hands it back cheaply.
#[derive(Debug)]
pub struct Picture {
    planes: Box<Yuv422Planes>,
    bitstream: Vec<u8>,
}

impl Picture {
    /// Zero-filled planes; the bitstream buffer is sized by the session at the first submit.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            planes: Box::new(Yuv422Planes::new(width, height)),
            bitstream: Vec::new(),
        }
    }

    pub fn planes(&self) -> &Yuv422Planes {
        &self.planes
    }

    pub fn planes_mut(&mut self) -> &mut Yuv422Planes {
        &mut self.planes
    }
}

struct State {
    /// Null once the session is closed.
    raw: *mut CEnc,
    /// Pictures the library holds, oldest first (the order `get_packet` answers in).
    in_flight: VecDeque<Picture>,
}

// SAFETY: the handle is only used under the session mutex (send, close) or by the one output
// half (get_packet), which the library allows concurrently with a send.
unsafe impl Send for State {}

struct Session {
    width: u32,
    height: u32,
    bytes_per_frame: usize,
    state: Mutex<State>,
}

impl Session {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let st = self.state.get_mut().unwrap_or_else(|e| e.into_inner());
        if !st.raw.is_null() {
            // SAFETY: opened by cbjxs_enc_open, closed exactly once (abandon nulls it).
            unsafe { cbjxs_enc_close(st.raw) };
            st.raw = std::ptr::null_mut();
        }
        // the pictures drop after the library's threads are gone
    }
}

/// The submitting half of a session.
pub struct EncoderInput {
    s: Arc<Session>,
}

/// The receiving half of a session.
pub struct EncoderOutput {
    s: Arc<Session>,
}

impl JxsError {
    /// The session was closed (by [`EncoderOutput::abandon`]) or held no picture to receive:
    /// not a library error.
    pub fn is_session_state(&self) -> bool {
        self.code == SESSION_CODE
    }
}

fn session_error(during: &'static str) -> JxsError {
    JxsError {
        code: SESSION_CODE,
        during,
    }
}

/// Open a pipelined session. The library's worker threads start on the calling thread's CPU set
/// (`threads_num` = 2 + the slice threads, [`EncoderSettings::with_slice_threads`]).
pub fn open_pipelined(s: EncoderSettings) -> Result<(EncoderInput, EncoderOutput), JxsError> {
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
    let s = Arc::new(Session {
        width: s.width,
        height: s.height,
        bytes_per_frame: bytes_per_frame as usize,
        state: Mutex::new(State {
            raw,
            in_flight: VecDeque::with_capacity(MAX_IN_FLIGHT),
        }),
    });
    Ok((EncoderInput { s: Arc::clone(&s) }, EncoderOutput { s }))
}

impl EncoderInput {
    pub fn width(&self) -> u32 {
        self.s.width
    }

    pub fn height(&self) -> u32 {
        self.s.height
    }

    /// The bitstream budget of one frame in bytes.
    pub fn bytes_per_frame(&self) -> usize {
        self.s.bytes_per_frame
    }

    /// Hand a picture to the library. On success the session owns it until
    /// [`EncoderOutput::recv_frame`] returns it after its last packet. On failure (another
    /// geometry, a closed session, [`MAX_IN_FLIGHT`] reached, a library error) it comes back
    /// untouched. Allocates only the first time a picture meets this session (its bitstream).
    pub fn submit(&self, mut pic: Picture) -> Result<(), (Picture, JxsError)> {
        if !pic.planes.matches(self.s.width, self.s.height) {
            return Err((
                pic,
                session_error("submit (the picture does not match the session's geometry)"),
            ));
        }
        if pic.bitstream.len() != self.s.bytes_per_frame {
            pic.bitstream = vec![0; self.s.bytes_per_frame];
        }
        let mut st = self.s.lock();
        if st.raw.is_null() {
            return Err((pic, session_error("submit (the session is closed)")));
        }
        if st.in_flight.len() >= MAX_IN_FLIGHT {
            return Err((pic, session_error("submit (too many pictures in flight)")));
        }
        let raw = st.raw;
        st.in_flight.push_back(pic);
        let p = st.in_flight.back_mut().expect("just pushed");
        let (w, cw) = (p.planes.width(), p.planes.chroma_width());
        // SAFETY: the planes (checked against the session geometry) and the bitstream (sized to
        // the session's budget) live on the heap of a picture the session now owns; nothing
        // touches them until recv_frame took the picture's last packet, which is the library's
        // release point for both. Moving the `Picture` value inside the queue never moves them.
        let code = unsafe {
            cbjxs_enc_send(
                raw,
                p.planes.y().as_ptr(),
                p.planes.u().as_ptr(),
                p.planes.v().as_ptr(),
                w,
                cw,
                p.bitstream.as_mut_ptr(),
                p.bitstream.len() as u32,
            )
        };
        match check(code, "send picture") {
            Ok(()) => Ok(()),
            Err(e) => {
                let pic = st.in_flight.pop_back().expect("pushed above");
                Err((pic, e))
            }
        }
    }
}

impl EncoderOutput {
    pub fn width(&self) -> u32 {
        self.s.width
    }

    pub fn height(&self) -> u32 {
        self.s.height
    }

    /// Pictures the session holds right now.
    pub fn in_flight(&self) -> usize {
        self.s.lock().in_flight.len()
    }

    /// Receive the oldest picture: `on_packet(index, bytes, is_last)` for every packet the
    /// moment the library produces it (index 0 = the picture header), then the picture comes
    /// back with the packet count. An error is a session state (closed, nothing in flight: no
    /// packet was read) or a library error (call [`Self::abandon`]).
    pub fn recv_frame(
        &self,
        mut on_packet: impl FnMut(u32, &[u8], bool),
    ) -> Result<(Picture, u32), JxsError> {
        let raw = {
            let st = self.s.lock();
            if st.raw.is_null() {
                return Err(session_error("receive (the session is closed)"));
            }
            if st.in_flight.is_empty() {
                return Err(session_error("receive (no picture in flight)"));
            }
            st.raw
        };
        let mut index = 0u32;
        loop {
            let mut data: *const u8 = std::ptr::null();
            let mut len = 0u32;
            let mut last = 0u8;
            // SAFETY: a valid handle (only this half closes it); the call blocks until a packet
            // of the oldest picture is out, and one is in flight.
            check(
                unsafe { cbjxs_enc_get_packet(raw, &mut data, &mut len, &mut last) },
                "get packet",
            )?;
            let bytes: &[u8] = if data.is_null() || len == 0 {
                &[]
            } else {
                // SAFETY: the library points into the oldest picture's bitstream (a heap buffer
                // the session owns and nobody else touches), valid until its last packet.
                unsafe { std::slice::from_raw_parts(data, len as usize) }
            };
            on_packet(index, bytes, last != 0);
            index += 1;
            if last != 0 {
                let pic = self
                    .s
                    .lock()
                    .in_flight
                    .pop_front()
                    .expect("a picture was in flight");
                return Ok((pic, index));
            }
        }
    }

    /// Close the session after a library error and take back every picture it held. Every later
    /// submit or receive answers a session-state error.
    pub fn abandon(&self) -> Vec<Picture> {
        let mut st = self.s.lock();
        if !st.raw.is_null() {
            // SAFETY: opened by cbjxs_enc_open; this half is the only get_packet caller and is
            // here, the input half waits on the lock: nothing else uses the handle.
            unsafe { cbjxs_enc_close(st.raw) };
            st.raw = std::ptr::null_mut();
        }
        st.in_flight.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Decoder, Fed};
    use std::time::Duration;

    const W: u32 = 256;
    const H: u32 = 64;

    fn settings() -> EncoderSettings {
        EncoderSettings::issue_1387(W, H).with_slice_threads(2)
    }

    fn picture(phase: usize) -> Picture {
        let mut p = Picture::new(W, H);
        let w = W as usize;
        let (y, u, v) = p.planes_mut().planes_mut();
        for (i, b) in y.iter_mut().enumerate() {
            *b = (((i * 3 + phase * 37) % w) * 255 / w) as u8;
        }
        for (i, b) in u.iter_mut().enumerate() {
            *b = (40 + (i / (w / 2) + phase * 11) % 160) as u8;
        }
        for (i, b) in v.iter_mut().enumerate() {
            *b = (200 - (i / (w / 2) + phase * 7) % 150) as u8;
        }
        p
    }

    fn decode(packets: &[Vec<u8>]) -> Yuv422Planes {
        let mut dec = Decoder::open(&packets.concat()).expect("decoder open");
        for (i, p) in packets.iter().enumerate() {
            let fed = dec.send_packet(p).expect("send packet");
            assert_eq!(
                fed,
                if i + 1 == packets.len() {
                    Fed::Taken
                } else {
                    Fed::NeedMore
                }
            );
        }
        dec.finish_frame(Duration::from_secs(5))
            .expect("decode")
            .clone()
    }

    fn max_err(a: &[u8], b: &[u8]) -> i32 {
        a.iter()
            .zip(b)
            .map(|(x, y)| (*x as i32 - *y as i32).abs())
            .max()
            .unwrap()
    }

    #[test]
    fn the_slice_thread_count_is_explicit() {
        assert_eq!(settings().threads_num, 4, "2 slice threads + init + final");
        assert_eq!(settings().slice_threads(), 2);
        assert_eq!(
            EncoderSettings::issue_1387(W, H)
                .with_slice_threads(0)
                .slice_threads(),
            1,
            "never zero slice threads"
        );
    }

    /// Two pictures are handed over before the first one's packets are read; each comes back,
    /// in order, untouched, and its packets decode to ITS content (not the other one's).
    #[test]
    fn two_pictures_in_flight_come_back_in_order_and_decode_to_their_own_content() {
        let (input, output) = open_pipelined(settings()).expect("open");
        let originals = [picture(1), picture(2)];
        for o in &originals {
            let p = Picture {
                planes: o.planes.clone(),
                bitstream: Vec::new(),
            };
            input.submit(p).map_err(|(_, e)| e).expect("submit");
        }
        assert_eq!(output.in_flight(), 2, "two frames in flight");
        for (k, original) in originals.iter().enumerate() {
            let mut packets: Vec<Vec<u8>> = Vec::new();
            let mut lasts = 0;
            let (back, n) = output
                .recv_frame(|i, b, last| {
                    assert_eq!(i as usize, packets.len());
                    packets.push(b.to_vec());
                    lasts += last as u32;
                })
                .expect("receive");
            assert_eq!(n, 5, "64 lines: the header + 4 slices");
            assert_eq!(lasts, 1, "one last packet per frame");
            assert_eq!(
                back.planes(),
                original.planes(),
                "the picture comes back as sent"
            );
            assert_eq!(back.bitstream.len(), input.bytes_per_frame());
            let out = decode(&packets);
            assert!(
                max_err(original.planes().y(), out.y()) <= 8,
                "frame {k} luma"
            );
            assert!(
                max_err(original.planes().u(), out.u()) <= 8,
                "frame {k} chroma"
            );
            let other = &originals[1 - k];
            assert!(
                max_err(other.planes().y(), out.y()) > 8,
                "frame {k} decodes to its own content, not the other frame's"
            );
        }
        assert_eq!(output.in_flight(), 0);
        assert!(
            output
                .recv_frame(|_, _, _| {})
                .unwrap_err()
                .is_session_state(),
            "nothing in flight: no blocking get_packet"
        );
    }

    #[test]
    fn a_picture_of_another_geometry_comes_back_untouched() {
        let (input, output) = open_pipelined(settings()).expect("open");
        let (back, e) = input.submit(Picture::new(W / 2, H)).unwrap_err();
        assert!(e.is_session_state());
        assert_eq!(back.planes().width(), W / 2);
        assert_eq!(output.in_flight(), 0);
    }

    #[test]
    fn an_abandoned_session_returns_its_pictures_and_refuses_more() {
        let (input, output) = open_pipelined(settings()).expect("open");
        input
            .submit(picture(1))
            .map_err(|(_, e)| e)
            .expect("submit");
        input
            .submit(picture(2))
            .map_err(|(_, e)| e)
            .expect("submit");
        let back = output.abandon();
        assert_eq!(back.len(), 2, "every held picture comes back");
        let (_, e) = input.submit(picture(3)).unwrap_err();
        assert!(e.is_session_state(), "closed");
        assert!(output
            .recv_frame(|_, _, _| {})
            .unwrap_err()
            .is_session_state());
        assert!(output.abandon().is_empty(), "a second abandon is a no-op");
    }

    #[test]
    fn the_session_refuses_more_than_its_in_flight_cap() {
        let (input, output) = open_pipelined(settings()).expect("open");
        for k in 0..MAX_IN_FLIGHT {
            input
                .submit(picture(k))
                .map_err(|(_, e)| e)
                .expect("submit");
        }
        let (_, e) = input.submit(picture(99)).unwrap_err();
        assert!(e.is_session_state());
        for _ in 0..MAX_IN_FLIGHT {
            output.recv_frame(|_, _, _| {}).expect("receive");
        }
    }

    /// The two halves on two threads: one submits a stream of pictures while the other reads
    /// packets, as the camera-box sender does.
    #[test]
    fn the_halves_run_on_two_threads() {
        let (input, output) = open_pipelined(settings()).expect("open");
        let (tx, rx) = std::sync::mpsc::sync_channel::<()>(4);
        let reader = std::thread::spawn(move || {
            let mut frames = 0;
            while rx.recv().is_ok() {
                let (_, n) = output.recv_frame(|_, _, _| {}).expect("receive");
                assert_eq!(n, 5);
                frames += 1;
            }
            frames
        });
        for k in 0..40 {
            loop {
                match input.submit(picture(k)) {
                    Ok(()) => break,
                    Err((_, e)) if e.is_session_state() => std::thread::yield_now(),
                    Err((_, e)) => panic!("{e}"),
                }
            }
            tx.send(()).unwrap();
        }
        drop(tx);
        assert_eq!(reader.join().unwrap(), 40);
    }
}
