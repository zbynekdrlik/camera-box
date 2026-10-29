//! Issue 1387 slice 1b: a PIPELINED encode session (stub: the RED commit).

use crate::{EncoderSettings, JxsError, Yuv422Planes};

/// The most pictures a session holds at once.
pub const MAX_IN_FLIGHT: usize = 8;

const SESSION_CODE: i32 = i32::MIN + 1;

/// One picture of a pipelined session.
#[derive(Debug)]
#[allow(dead_code)]
pub struct Picture {
    planes: Yuv422Planes,
    bitstream: Vec<u8>,
}

impl Picture {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            planes: Yuv422Planes::new(width, height),
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

pub struct EncoderInput;
pub struct EncoderOutput;

impl JxsError {
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

pub fn open_pipelined(_s: EncoderSettings) -> Result<(EncoderInput, EncoderOutput), JxsError> {
    Err(session_error("not implemented"))
}

impl EncoderInput {
    pub fn width(&self) -> u32 {
        0
    }

    pub fn height(&self) -> u32 {
        0
    }

    pub fn bytes_per_frame(&self) -> usize {
        0
    }

    pub fn submit(&self, pic: Picture) -> Result<(), (Picture, JxsError)> {
        Err((pic, session_error("not implemented")))
    }
}

impl EncoderOutput {
    pub fn width(&self) -> u32 {
        0
    }

    pub fn height(&self) -> u32 {
        0
    }

    pub fn in_flight(&self) -> usize {
        0
    }

    pub fn recv_frame(
        &self,
        _on_packet: impl FnMut(u32, &[u8], bool),
    ) -> Result<(Picture, u32), JxsError> {
        Err(session_error("not implemented"))
    }

    pub fn abandon(&self) -> Vec<Picture> {
        Vec::new()
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
