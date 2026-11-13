//! Issue 1501: the cameraman preview's source switch and the local-camera frame tap.
//!
//! After every camera-box start the HDMI preview shows the strih NDI feed. A double press of the
//! power button flips [`PreviewSwitch`] to the local camera and back. While it says local, the
//! capture thread offers every second emitted frame to [`PreviewTap`]: one row memcpy into a free
//! slot under `try_lock`, a busy slot is a counted skip, never a wait. The display thread takes the
//! newest frame. While the switch says strih, `offer` returns after one atomic load.
//! Pure std, no I/O, so it unit-tests on default features.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Which picture the cameraman preview shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewSource {
    /// The strih NDI feed (`STRIH-SNV (interkom)`), the default after every start.
    Strih,
    /// The camera this box grabs.
    Local,
}

/// The shared switch: the button thread flips it, the capture and display threads read it.
#[derive(Debug, Default)]
pub struct PreviewSwitch {
    local: AtomicBool,
}

impl PreviewSwitch {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn source(&self) -> PreviewSource {
        if self.local.load(Ordering::Relaxed) {
            PreviewSource::Local
        } else {
            PreviewSource::Strih
        }
    }

    /// Flip to the other source; returns the new one.
    pub fn toggle(&self) -> PreviewSource {
        if self.local.fetch_xor(true, Ordering::Relaxed) {
            PreviewSource::Strih
        } else {
            PreviewSource::Local
        }
    }
}

/// One packed frame (no row padding) as the capture delivered it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TapFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// The FourCC as the display reads it (`u32::from_le_bytes(*b"YUYV")`).
    pub fourcc: u32,
    /// Increases with every frame put into the tap.
    pub seq: u64,
}

/// What a capture-thread offer did (for the counters and the tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfferResult {
    /// The switch says strih: nothing copied.
    Inactive,
    /// The odd frame of the 60 -> 30 fps thinning: nothing copied.
    Thinned,
    /// The display holds the slot right now: skipped, counted.
    Busy,
    /// The frame does not fit `stride * height` or has a zero size: skipped, counted.
    Malformed,
    /// Copied into the slot.
    Stored,
}

/// The latest-frame mailbox between the capture thread and the display thread.
#[derive(Debug)]
pub struct PreviewTap {
    switch: Arc<PreviewSwitch>,
    slot: Mutex<TapFrame>,
    offers: AtomicU64,
    stored: AtomicU64,
    busy: AtomicU64,
    malformed: AtomicU64,
}

impl PreviewTap {
    pub fn new(switch: Arc<PreviewSwitch>) -> Arc<Self> {
        Arc::new(Self {
            switch,
            slot: Mutex::new(TapFrame::default()),
            offers: AtomicU64::new(0),
            stored: AtomicU64::new(0),
            busy: AtomicU64::new(0),
            malformed: AtomicU64::new(0),
        })
    }

    pub fn switch(&self) -> &Arc<PreviewSwitch> {
        &self.switch
    }

    /// Capture thread: offer one emitted frame. Never blocks.
    pub fn offer(
        &self,
        data: &[u8],
        width: u32,
        height: u32,
        stride: u32,
        fourcc: u32,
    ) -> OfferResult {
        if self.switch.source() != PreviewSource::Local {
            return OfferResult::Inactive;
        }
        let n = self.offers.fetch_add(1, Ordering::Relaxed);
        if n % 2 == 1 {
            return OfferResult::Thinned;
        }
        let row = (width as usize) * 2;
        let stride = stride as usize;
        if width == 0 || height == 0 || stride < row || data.len() < stride * height as usize {
            self.malformed.fetch_add(1, Ordering::Relaxed);
            return OfferResult::Malformed;
        }
        let Ok(mut slot) = self.slot.try_lock() else {
            self.busy.fetch_add(1, Ordering::Relaxed);
            return OfferResult::Busy;
        };
        slot.data.resize(row * height as usize, 0);
        if stride == row {
            slot.data.copy_from_slice(&data[..row * height as usize]);
        } else {
            for (y, dst) in slot.data.chunks_exact_mut(row).enumerate() {
                dst.copy_from_slice(&data[y * stride..y * stride + row]);
            }
        }
        slot.width = width;
        slot.height = height;
        slot.fourcc = fourcc;
        slot.seq = self.stored.fetch_add(1, Ordering::Relaxed) + 1;
        OfferResult::Stored
    }

    /// Display thread: copy the newest frame into `out` when it is newer than `out.seq`.
    /// Returns whether `out` changed.
    pub fn take_newer(&self, out: &mut TapFrame) -> bool {
        let slot = match self.slot.lock() {
            Ok(s) => s,
            Err(p) => p.into_inner(),
        };
        if slot.seq == 0 || slot.seq == out.seq {
            return false;
        }
        out.data.clear();
        out.data.extend_from_slice(&slot.data);
        out.width = slot.width;
        out.height = slot.height;
        out.fourcc = slot.fourcc;
        out.seq = slot.seq;
        true
    }

    /// `(stored, busy, malformed)` since the start.
    pub fn counters(&self) -> (u64, u64, u64) {
        (
            self.stored.load(Ordering::Relaxed),
            self.busy.load(Ordering::Relaxed),
            self.malformed.load(Ordering::Relaxed),
        )
    }
}

/// YUYV (Y0 U Y1 V) to UYVY (U Y0 V Y1), byte pairs swapped; the display renders UYVY. A trailing
/// odd byte is dropped.
pub fn yuyv_to_uyvy(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len() & !1);
    for pair in src.as_chunks::<2>().0 {
        out.push(pair[1]);
        out.push(pair[0]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const YUYV: u32 = u32::from_le_bytes(*b"YUYV");

    /// Owner 13.11.2026: switch the preview by a command, not the button. One request = one flip, taken
    /// once; a real SIGUSR1 after the install is a request (the handler only stores the flag).
    #[test]
    fn a_preview_toggle_request_is_taken_once_and_sigusr1_makes_one_1501() {
        let _ = take_preview_toggle_request();
        assert!(!take_preview_toggle_request());
        request_preview_toggle();
        assert!(take_preview_toggle_request());
        assert!(!take_preview_toggle_request(), "one request flips the preview once");
        install_preview_toggle_signal();
        install_preview_toggle_signal(); // idempotent
        #[cfg(target_os = "linux")]
        {
            // SAFETY: raise is a plain POSIX call; the installed handler only stores an atomic flag.
            assert_eq!(unsafe { libc::raise(libc::SIGUSR1) }, 0);
            assert!(take_preview_toggle_request(), "SIGUSR1 is a preview toggle request");
        }
    }

    fn frame(width: u32, height: u32, stride: u32, fill: u8) -> Vec<u8> {
        let mut v = vec![0xEE; (stride * height) as usize];
        for y in 0..height {
            for x in 0..width * 2 {
                v[(y * stride + x) as usize] = fill.wrapping_add((y * 7 + x) as u8);
            }
        }
        v
    }

    #[test]
    fn yuyv_becomes_uyvy_pair_by_pair() {
        // Y0=10 U=20 Y1=30 V=40 -> U Y0 V Y1
        assert_eq!(
            yuyv_to_uyvy(&[10, 20, 30, 40, 11, 21]),
            vec![20, 10, 40, 30, 21, 11]
        );
        assert_eq!(yuyv_to_uyvy(&[1, 2, 3]), vec![2, 1]);
        assert!(yuyv_to_uyvy(&[]).is_empty());
    }

    #[test]
    fn the_switch_starts_on_strih_and_toggles_back_and_forth() {
        let s = PreviewSwitch::new();
        assert_eq!(s.source(), PreviewSource::Strih);
        assert_eq!(s.toggle(), PreviewSource::Local);
        assert_eq!(s.source(), PreviewSource::Local);
        assert_eq!(s.toggle(), PreviewSource::Strih);
        assert_eq!(s.source(), PreviewSource::Strih);
    }

    #[test]
    fn nothing_is_copied_while_the_switch_says_strih() {
        let tap = PreviewTap::new(PreviewSwitch::new());
        let f = frame(4, 2, 8, 1);
        assert_eq!(tap.offer(&f, 4, 2, 8, YUYV), OfferResult::Inactive);
        let mut out = TapFrame::default();
        assert!(!tap.take_newer(&mut out));
        assert_eq!(tap.counters(), (0, 0, 0));
    }

    #[test]
    fn every_second_frame_is_stored_and_the_display_gets_the_newest() {
        let s = PreviewSwitch::new();
        s.toggle();
        let tap = PreviewTap::new(s);
        let a = frame(4, 2, 8, 1);
        let b = frame(4, 2, 8, 50);
        let c = frame(4, 2, 8, 99);
        assert_eq!(tap.offer(&a, 4, 2, 8, YUYV), OfferResult::Stored);
        assert_eq!(tap.offer(&b, 4, 2, 8, YUYV), OfferResult::Thinned);
        assert_eq!(tap.offer(&c, 4, 2, 8, YUYV), OfferResult::Stored);
        let mut out = TapFrame::default();
        assert!(tap.take_newer(&mut out));
        assert_eq!(out.data, c);
        assert_eq!(
            (out.width, out.height, out.fourcc, out.seq),
            (4, 2, YUYV, 2)
        );
        assert!(!tap.take_newer(&mut out), "nothing newer");
    }

    #[test]
    fn row_padding_is_dropped() {
        let s = PreviewSwitch::new();
        s.toggle();
        let tap = PreviewTap::new(s);
        let f = frame(3, 2, 10, 5);
        assert_eq!(tap.offer(&f, 3, 2, 10, YUYV), OfferResult::Stored);
        let mut out = TapFrame::default();
        tap.take_newer(&mut out);
        assert_eq!(out.data.len(), 12);
        assert_eq!(&out.data[..6], &f[..6]);
        assert_eq!(&out.data[6..], &f[10..16]);
        assert!(!out.data.contains(&0xEE));
    }

    #[test]
    fn a_held_slot_is_a_counted_skip_never_a_wait() {
        let s = PreviewSwitch::new();
        s.toggle();
        let tap = PreviewTap::new(s);
        let f = frame(4, 2, 8, 1);
        let guard = tap.slot.lock().unwrap();
        assert_eq!(tap.offer(&f, 4, 2, 8, YUYV), OfferResult::Busy);
        drop(guard);
        assert_eq!(tap.counters(), (0, 1, 0));
    }

    #[test]
    fn a_short_or_empty_frame_is_refused_and_counted() {
        let s = PreviewSwitch::new();
        s.toggle();
        let tap = PreviewTap::new(s);
        assert_eq!(tap.offer(&[0; 10], 4, 2, 8, YUYV), OfferResult::Malformed);
        tap.offer(&[0; 16], 4, 2, 8, YUYV); // thinned slot
        assert_eq!(tap.offer(&[0; 16], 4, 2, 6, YUYV), OfferResult::Malformed);
        tap.offer(&[0; 16], 4, 2, 8, YUYV);
        assert_eq!(tap.offer(&[], 0, 0, 0, YUYV), OfferResult::Malformed);
        assert_eq!(tap.counters(), (0, 0, 3));
    }

    #[test]
    fn switching_back_to_strih_stops_the_copies() {
        let s = PreviewSwitch::new();
        s.toggle();
        let tap = PreviewTap::new(Arc::clone(&s));
        let f = frame(4, 2, 8, 1);
        assert_eq!(tap.offer(&f, 4, 2, 8, YUYV), OfferResult::Stored);
        s.toggle();
        assert_eq!(tap.offer(&f, 4, 2, 8, YUYV), OfferResult::Inactive);
        assert_eq!(tap.counters().0, 1);
    }
}
