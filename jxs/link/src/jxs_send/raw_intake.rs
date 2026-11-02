//! Issue 1387 slice 3: the hand-off's RAW planar intake (the strih OBS program output through
//! `jxs/send-ffi`) and its published status window, beside the cambox's YUYV `offer`
//! (`handoff.rs`). Built only with the `jxs` feature.

use super::handoff::{JxsHandoff, Ready};
use super::{jxs_link, Offered, RawFrame, RawLayout};
use crate::jxs_link::SendWindow;
use std::sync::atomic::Ordering;
use std::time::Instant;
use svt_jpegxs_sys::{ChromaFormat, Picture};

/// The encoder's chroma sampling for a raw layout (NV12 is 4:2:0 split on the copy).
fn chroma_of(layout: RawLayout) -> ChromaFormat {
    match layout {
        RawLayout::I420 | RawLayout::Nv12 => ChromaFormat::Yuv420,
        RawLayout::I422 => ChromaFormat::Yuv422,
        RawLayout::I444 => ChromaFormat::Yuv444,
    }
}

impl JxsHandoff {
    /// Hand one raw planar picture over (issue 1387 slice 3: the strih OBS program output): the
    /// caller's thread copies it straight into a free picture of its chroma sampling (NV12's
    /// interleaved chroma split on the way) and queues it. Never blocks: with every picture in
    /// use the frame is [`Offered::Busy`] (`dropped_busy`). A geometry or plane that does not fit
    /// is [`Offered::Refused`] (`encode_errors`). The planes are not read after it returns.
    pub fn offer_raw(&self, frame: &RawFrame<'_>) -> Offered {
        if !jxs_link::on_jxs_grid(frame.timecode_100ns, frame.emit_fps) {
            return Offered::OffGrid;
        }
        self.stats
            .note_handoff(frame.timecode_100ns, 0, crate::jxs_recv::realtime_100ns());
        let (w, h, layout) = (frame.width, frame.height, frame.layout);
        if !layout.fits(w, h) {
            self.stats.convert_errors.fetch_add(1, Ordering::Relaxed);
            return Offered::Refused;
        }
        let Ok(mut pic) = self.free_rx.try_recv() else {
            self.stats.note_dropped();
            return Offered::Busy;
        };
        let started = Instant::now();
        let chroma = chroma_of(layout);
        let p = pic.planes();
        if (p.width(), p.height(), p.chroma()) != (w, h, chroma) {
            pic = Picture::with_chroma(w, h, chroma);
        }
        let (cw, ch) = layout.chroma_dims(w, h);
        let (w, h, cw, ch) = (w as usize, h as usize, cw as usize, ch as usize);
        let [py, pu, pv] = frame.planes;
        let [sy, su, sv] = frame.strides;
        let ok = {
            let (y, u, v) = pic.planes_mut().planes_mut();
            jxs_link::copy_plane(py, sy, w, h, y)
                && match layout {
                    RawLayout::Nv12 => jxs_link::split_interleaved(pu, su, cw, ch, u, v),
                    _ => {
                        jxs_link::copy_plane(pu, su, cw, ch, u)
                            && jxs_link::copy_plane(pv, sv, cw, ch, v)
                    }
                }
        };
        let handed_off = Instant::now();
        self.stats
            .note_convert(handed_off.duration_since(started).as_nanos() as u64);
        if !ok {
            self.stats.convert_errors.fetch_add(1, Ordering::Relaxed);
            let _ = self.free_tx.try_send(pic);
            return Offered::Refused;
        }
        self.queue(Ready {
            pic,
            timecode_100ns: frame.timecode_100ns,
            handed_off,
        })
    }

    /// The last finished status window with its sequence number, once per window (`None` = no
    /// new one since the last call). The cambox logs the same window itself; this is for a
    /// caller with its own log (issue 1387 slice 3).
    pub fn take_window(&self) -> Option<(u64, SendWindow)> {
        let (seq, w) = *self.published.lock().unwrap_or_else(|e| e.into_inner());
        let last = self.taken_window.swap(seq, Ordering::Relaxed);
        (seq != 0 && seq != last).then_some((seq, w))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 60 fps emit stamp of slot `k` in second 1_761_730_000 (29.10.2026).
    fn tc(k: i64) -> i64 {
        1_761_730_000 * 10_000_000 + k * 10_000_000 / 60
    }

    /// Issue 1387 slice 3: a raw NV12 picture (the OBS canvas) is copied into a 4:2:0 picture
    /// (its chroma de-interleaved, its row padding dropped); a third one while both pictures are
    /// queued is Busy, a frame off the 30 fps grid of a 60 fps caller never goes, a geometry the
    /// layout cannot take is Refused before a picture is taken.
    #[test]
    fn a_raw_nv12_picture_is_copied_into_a_420_picture_and_queued() {
        let (h, ready) = JxsHandoff::unstarted();
        let (w, ht, stride) = (8u32, 4u32, 10usize);
        let y: Vec<u8> = (0..stride * 4).map(|i| i as u8).collect();
        let uv: Vec<u8> = (0..stride * 2).map(|i| 100 + i as u8).collect();
        let raw = |timecode_100ns, emit_fps| RawFrame {
            layout: RawLayout::Nv12,
            width: w,
            height: ht,
            planes: [&y, &uv, &[]],
            strides: [stride, stride, 0],
            timecode_100ns,
            emit_fps,
        };
        assert_eq!(h.offer_raw(&raw(tc(1), 60)), Offered::OffGrid);
        assert_eq!(h.offer_raw(&raw(tc(0), 30)), Offered::Taken);
        assert_eq!(
            h.offer_raw(&raw(tc(1), 30)),
            Offered::Taken,
            "30 fps: every frame"
        );
        assert_eq!(h.offer_raw(&raw(tc(2), 30)), Offered::Busy);
        assert_eq!(h.dropped_busy(), 1);
        let a = ready.try_recv().unwrap();
        let p = a.pic.planes();
        assert_eq!(p.chroma(), svt_jpegxs_sys::ChromaFormat::Yuv420);
        assert_eq!(&p.y()[..8], &y[..8]);
        assert_eq!(&p.y()[8..16], &y[10..18], "the row padding is dropped");
        assert_eq!(p.u(), &[100, 102, 104, 106, 110, 112, 114, 116]);
        assert_eq!(p.v(), &[101, 103, 105, 107, 111, 113, 115, 117]);
        h.free_tx.try_send(a.pic).unwrap();
        let odd = RawFrame {
            height: 3,
            ..raw(tc(4), 30)
        };
        assert_eq!(h.offer_raw(&odd), Offered::Refused);
        let short = RawFrame {
            strides: [stride, 7, 0],
            ..raw(tc(4), 30)
        };
        assert_eq!(
            h.offer_raw(&short),
            Offered::Refused,
            "a UV row shorter than 8 bytes"
        );
        assert_eq!(h.stats.convert_errors.load(Ordering::Relaxed), 2);
        assert_eq!(
            h.offer_raw(&raw(tc(4), 30)),
            Offered::Taken,
            "the picture came back"
        );
    }
}
