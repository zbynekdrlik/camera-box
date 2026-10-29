//! Issue 1387: reassembly of JPEG XS link datagrams into frames, slice by slice.
//!
//! No jitter buffer (owner decision 6489239017): a packet is handed on the moment every packet
//! before it in the same frame is complete, so the decoder works slice by slice while the frame
//! is still arriving. A datagram that never comes is not waited for: the frame is FINALIZED, and
//! its missing packets are handed on as lost (the receiver conceals them from the previous frame),
//! when
//! - the next frame is complete (everything of the older frame should have arrived first), or
//! - a datagram of a third frame arrives while two are open (the oldest is given up).
//!
//! So a lost slice costs at most about one frame of delay for that frame, never a stall.
//! Datagrams of a frame may arrive in any order (UDP over one LAN hop does not reorder in
//! practice, the assembler still does not rely on it).
//!
//! Allocation-free once warm: two frame slots, their slice buffers reused by capacity.

use super::{PacketHeader, SEGMENT_DATA_LEN};

/// A timecode this far BEHIND the last finished frame is a clock step back (a dantesync date
/// step, a sender restart on another clock), never a late datagram: start over.
pub const RESYNC_BACKWARD_100NS: u64 = 10_000_000;

/// What the receiver does with the reassembled stream.
pub trait SliceSink {
    /// One packet of frame `timecode`, in index order. `None` = lost: conceal it.
    fn packet(&mut self, timecode: u64, index: u8, count: u8, bytes: Option<&[u8]>);
    /// Every packet of the frame was handed on.
    fn frame_done(&mut self, report: &FrameReport);
}

/// One finished frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameReport {
    pub timecode: u64,
    pub camera_id: u8,
    pub slice_count: u8,
    pub lost_slices: u32,
    /// Receive time of the frame's first and last datagram (the caller's clock).
    pub first_arrival: i64,
    pub last_arrival: i64,
}

/// What became of one datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    Accepted,
    /// Already received.
    Duplicate,
    /// Belongs to a frame already finished or given up.
    Stale,
    /// Inconsistent with its own header or with its frame.
    Malformed,
    /// A clock step back: the state was reset and the datagram accepted into a fresh frame.
    Resync,
}

#[derive(Debug, Default)]
struct Slice {
    data: Vec<u8>,
    len: usize,
    mask: u64,
    need: u64,
    seen: bool,
}

impl Slice {
    fn complete(&self) -> bool {
        self.seen && self.mask == self.need
    }
}

#[derive(Debug, Default)]
struct Slot {
    active: bool,
    finalized: bool,
    timecode: u64,
    camera_id: u8,
    count: usize,
    next: usize,
    complete: usize,
    lost: u32,
    first_arrival: i64,
    last_arrival: i64,
    slices: Vec<Slice>,
}

impl Slot {
    fn claim(&mut self, h: &PacketHeader, arrival: i64) {
        self.active = true;
        self.finalized = false;
        self.timecode = h.timecode_100ns;
        self.camera_id = h.camera_id;
        self.count = h.slice_count as usize;
        self.next = 0;
        self.complete = 0;
        self.lost = 0;
        self.first_arrival = arrival;
        self.last_arrival = arrival;
        if self.slices.len() < self.count {
            self.slices.resize_with(self.count, Slice::default);
        }
        for s in &mut self.slices[..self.count] {
            s.seen = false;
            s.mask = 0;
            s.len = 0;
        }
    }

    fn all_complete(&self) -> bool {
        self.complete == self.count
    }
}

/// The receiver's reassembly state for ONE sender.
#[derive(Debug, Default)]
pub struct FrameAssembler {
    slots: [Slot; 2],
    last_done: Option<u64>,
}

fn segment_mask(len: usize) -> u64 {
    let n = len.div_ceil(SEGMENT_DATA_LEN).max(1);
    if n >= 64 {
        u64::MAX
    } else {
        (1u64 << n) - 1
    }
}

impl FrameAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget every open frame (a resync).
    pub fn reset(&mut self) {
        for s in &mut self.slots {
            s.active = false;
        }
        self.last_done = None;
    }

    /// Take one datagram's payload (the bytes after the header) received at `arrival` (any
    /// clock; it is only copied into [`FrameReport`]). Hands every packet that became
    /// deliverable to `sink`.
    pub fn push(
        &mut self,
        h: &PacketHeader,
        payload: &[u8],
        arrival: i64,
        sink: &mut impl SliceSink,
    ) -> PushOutcome {
        let offset = h.offset as usize;
        let slice_len = h.slice_len as usize;
        let valid = if slice_len == 0 {
            offset == 0 && payload.is_empty()
        } else {
            offset.is_multiple_of(SEGMENT_DATA_LEN)
                && offset < slice_len
                && payload.len() == SEGMENT_DATA_LEN.min(slice_len - offset)
        };
        if !valid {
            return PushOutcome::Malformed;
        }

        let mut outcome = PushOutcome::Accepted;
        if let Some(done) = self.last_done {
            if h.timecode_100ns <= done {
                if done - h.timecode_100ns > RESYNC_BACKWARD_100NS {
                    self.reset();
                    outcome = PushOutcome::Resync;
                } else {
                    return PushOutcome::Stale;
                }
            }
        }

        let slot_ix = match self.find(h.timecode_100ns) {
            Some(i) => i,
            None => match self.claim_for(h, arrival, sink) {
                Some(i) => i,
                None => return PushOutcome::Stale,
            },
        };
        let slot = &mut self.slots[slot_ix];
        if h.slice_count as usize != slot.count {
            return PushOutcome::Malformed;
        }
        let s = &mut slot.slices[h.slice_index as usize];
        if !s.seen {
            s.seen = true;
            s.len = slice_len;
            s.need = segment_mask(slice_len);
            s.mask = 0;
            s.data.clear();
            s.data.resize(slice_len, 0);
        } else if s.len != slice_len {
            return PushOutcome::Malformed;
        }
        let bit = 1u64 << (offset / SEGMENT_DATA_LEN);
        if s.mask & bit != 0 {
            return PushOutcome::Duplicate;
        }
        s.data[offset..offset + payload.len()].copy_from_slice(payload);
        s.mask |= bit;
        if s.mask == s.need {
            slot.complete += 1;
        }
        slot.last_arrival = arrival;
        self.deliver(sink);
        outcome
    }

    fn find(&self, timecode: u64) -> Option<usize> {
        self.slots
            .iter()
            .position(|s| s.active && s.timecode == timecode)
    }

    fn oldest(&self) -> Option<usize> {
        match (self.slots[0].active, self.slots[1].active) {
            (true, true) => Some(if self.slots[0].timecode <= self.slots[1].timecode {
                0
            } else {
                1
            }),
            (true, false) => Some(0),
            (false, true) => Some(1),
            (false, false) => None,
        }
    }

    /// A slot for a new frame; when both are open, the oldest is given up first (if the new
    /// frame is newer than it, else the datagram is stale).
    fn claim_for(
        &mut self,
        h: &PacketHeader,
        arrival: i64,
        sink: &mut impl SliceSink,
    ) -> Option<usize> {
        if let Some(free) = self.slots.iter().position(|s| !s.active) {
            self.slots[free].claim(h, arrival);
            return Some(free);
        }
        let old = self.oldest()?;
        if h.timecode_100ns < self.slots[old].timecode {
            return None;
        }
        self.slots[old].finalized = true;
        self.deliver(sink);
        let free = self.slots.iter().position(|s| !s.active)?;
        self.slots[free].claim(h, arrival);
        Some(free)
    }

    fn deliver(&mut self, sink: &mut impl SliceSink) {
        while let Some(old) = self.oldest() {
            let other = 1 - old;
            if self.slots[other].active && self.slots[other].all_complete() {
                // The newer frame is all here: what the older one still lacks is lost.
                self.slots[old].finalized = true;
            }
            let slot = &mut self.slots[old];
            while slot.next < slot.count {
                let i = slot.next;
                let s = &slot.slices[i];
                if s.complete() {
                    sink.packet(
                        slot.timecode,
                        i as u8,
                        slot.count as u8,
                        Some(&s.data[..s.len]),
                    );
                } else if slot.finalized {
                    slot.lost += 1;
                    sink.packet(slot.timecode, i as u8, slot.count as u8, None);
                } else {
                    break;
                }
                slot.next += 1;
            }
            if slot.next < slot.count {
                return;
            }
            let report = FrameReport {
                timecode: slot.timecode,
                camera_id: slot.camera_id,
                slice_count: slot.count as u8,
                lost_slices: slot.lost,
                first_arrival: slot.first_arrival,
                last_arrival: slot.last_arrival,
            };
            slot.active = false;
            self.last_done = Some(report.timecode);
            sink.frame_done(&report);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{build_slice_datagrams, split_gro, PacketHeader, DATAGRAM_LEN, HEADER_LEN};
    use super::*;

    #[derive(Default)]
    struct Rec {
        events: Vec<(u64, u8, Option<Vec<u8>>)>,
        done: Vec<FrameReport>,
    }

    impl SliceSink for Rec {
        fn packet(&mut self, tc: u64, index: u8, _count: u8, bytes: Option<&[u8]>) {
            self.events.push((tc, index, bytes.map(<[u8]>::to_vec)));
        }
        fn frame_done(&mut self, r: &FrameReport) {
            self.done.push(*r);
        }
    }

    /// The datagrams of one frame: packet i is `sizes[i]` bytes of value `i`.
    fn frame(tc: u64, sizes: &[usize]) -> Vec<Vec<Vec<u8>>> {
        let mut out = Vec::new();
        for (i, &n) in sizes.iter().enumerate() {
            let base = PacketHeader {
                camera_id: 3,
                timecode_100ns: tc,
                slice_index: i as u8,
                slice_count: sizes.len() as u8,
                offset: 0,
                slice_len: 0,
            };
            let data = vec![i as u8 + 1; n];
            let mut buf = Vec::new();
            build_slice_datagrams(base, &data, &mut buf).unwrap();
            out.push(
                split_gro(&buf, Some(DATAGRAM_LEN))
                    .map(<[u8]>::to_vec)
                    .collect(),
            );
        }
        out
    }

    fn push(a: &mut FrameAssembler, d: &[u8], at: i64, r: &mut Rec) -> PushOutcome {
        let h = PacketHeader::decode(d).unwrap();
        a.push(&h, &d[HEADER_LEN..], at, r)
    }

    #[test]
    fn an_in_order_frame_is_handed_on_slice_by_slice() {
        let f = frame(1000, &[50, 3000, 1456]);
        let (mut a, mut r) = (FrameAssembler::new(), Rec::default());
        for d in &f[0] {
            assert_eq!(push(&mut a, d, 1, &mut r), PushOutcome::Accepted);
        }
        // the header packet goes on before the slices arrive
        assert_eq!(r.events.len(), 1);
        assert_eq!(r.events[0], (1000, 0, Some(vec![1; 50])));
        push(&mut a, &f[1][0], 2, &mut r);
        push(&mut a, &f[1][1], 3, &mut r);
        assert_eq!(r.events.len(), 1, "slice 1 waits for its last datagram");
        push(&mut a, &f[1][2], 4, &mut r);
        assert_eq!(r.events[1], (1000, 1, Some(vec![2; 3000])));
        push(&mut a, &f[2][0], 5, &mut r);
        assert_eq!(r.events.len(), 3);
        assert_eq!(
            r.done,
            vec![FrameReport {
                timecode: 1000,
                camera_id: 3,
                slice_count: 3,
                lost_slices: 0,
                first_arrival: 1,
                last_arrival: 5,
            }]
        );
    }

    #[test]
    fn reordered_datagrams_are_handed_on_in_index_order() {
        let f = frame(1000, &[20, 3000, 3000]);
        let mut all: Vec<Vec<u8>> = f.into_iter().flatten().collect();
        all.reverse();
        let (mut a, mut r) = (FrameAssembler::new(), Rec::default());
        for d in &all {
            push(&mut a, d, 0, &mut r);
        }
        let idx: Vec<u8> = r.events.iter().map(|e| e.1).collect();
        assert_eq!(idx, vec![0, 1, 2]);
        assert!(r.events.iter().all(|e| e.2.is_some()));
        assert_eq!(r.done.len(), 1);
    }

    #[test]
    fn a_lost_slice_is_handed_on_as_lost_once_the_next_frame_is_complete() {
        let f1 = frame(1000, &[20, 100, 100, 100]);
        let f2 = frame(1167, &[20, 100, 100, 100]);
        let (mut a, mut r) = (FrameAssembler::new(), Rec::default());
        for (i, s) in f1.iter().enumerate() {
            if i != 2 {
                push(&mut a, &s[0], 0, &mut r);
            }
        }
        assert!(
            r.done.is_empty(),
            "no jitter wait, but no early give-up either"
        );
        assert_eq!(r.events.len(), 2);
        for s in f2.iter().take(3) {
            push(&mut a, &s[0], 0, &mut r);
        }
        assert!(r.done.is_empty(), "frame 2 still incomplete");
        push(&mut a, &f2[3][0], 0, &mut r);
        assert_eq!(r.done.len(), 2);
        assert_eq!(r.done[0].lost_slices, 1);
        assert_eq!(r.done[1].lost_slices, 0);
        assert_eq!(r.events[2], (1000, 2, None));
        assert_eq!(r.events[3], (1000, 3, Some(vec![4; 100])));
    }

    #[test]
    fn a_third_frame_gives_up_the_oldest() {
        let f1 = frame(1000, &[20, 100]);
        let f2 = frame(1167, &[20, 100]);
        let f3 = frame(1333, &[20, 100]);
        let (mut a, mut r) = (FrameAssembler::new(), Rec::default());
        push(&mut a, &f1[0][0], 0, &mut r);
        push(&mut a, &f2[0][0], 0, &mut r);
        push(&mut a, &f3[0][0], 0, &mut r);
        assert_eq!(r.done.len(), 1);
        assert_eq!(r.done[0].timecode, 1000);
        assert_eq!(r.done[0].lost_slices, 1);
        // frame 1167 is now the oldest open frame; its late slice still completes it
        push(&mut a, &f2[1][0], 0, &mut r);
        assert_eq!(r.done.len(), 2);
        assert_eq!(r.done[1].lost_slices, 0);
    }

    #[test]
    fn duplicates_stale_and_malformed_datagrams_change_nothing() {
        let f1 = frame(5000, &[20, 100]);
        let (mut a, mut r) = (FrameAssembler::new(), Rec::default());
        push(&mut a, &f1[0][0], 0, &mut r);
        assert_eq!(push(&mut a, &f1[0][0], 0, &mut r), PushOutcome::Duplicate);
        push(&mut a, &f1[1][0], 0, &mut r);
        assert_eq!(r.done.len(), 1);
        assert_eq!(push(&mut a, &f1[1][0], 0, &mut r), PushOutcome::Stale);
        // an older frame than both open ones
        let late = frame(4000, &[20, 100]);
        assert_eq!(push(&mut a, &late[0][0], 0, &mut r), PushOutcome::Stale);
        // a payload shorter than the header says
        let d = &frame(6000, &[20, 100])[1][0];
        let h = PacketHeader::decode(d).unwrap();
        assert_eq!(
            a.push(&h, &d[HEADER_LEN..HEADER_LEN + 50], 0, &mut r),
            PushOutcome::Malformed
        );
        // a slice count that disagrees with the open frame
        let d0 = &frame(7000, &[20, 100])[0][0];
        push(&mut a, d0, 0, &mut r);
        let d1 = &frame(7000, &[20, 100, 100])[1][0];
        assert_eq!(push(&mut a, d1, 0, &mut r), PushOutcome::Malformed);
        assert_eq!(r.done.len(), 1);
    }

    #[test]
    fn a_clock_step_back_starts_over() {
        let (mut a, mut r) = (FrameAssembler::new(), Rec::default());
        let f = frame(100_000_000, &[20]);
        push(&mut a, &f[0][0], 0, &mut r);
        assert_eq!(r.done.len(), 1);
        let back = frame(100_000_000 - RESYNC_BACKWARD_100NS - 1, &[20]);
        assert_eq!(push(&mut a, &back[0][0], 0, &mut r), PushOutcome::Resync);
        assert_eq!(r.done.len(), 2);
    }

    #[test]
    fn slots_reuse_their_buffers() {
        let (mut a, mut r) = (FrameAssembler::new(), Rec::default());
        for k in 0..4u64 {
            for s in frame(1000 + k * 167, &[20, 3000]) {
                for d in &s {
                    push(&mut a, d, 0, &mut r);
                }
            }
        }
        let caps: Vec<usize> = a.slots[0]
            .slices
            .iter()
            .map(|s| s.data.capacity())
            .collect();
        for s in frame(9000, &[20, 3000]) {
            for d in &s {
                push(&mut a, d, 0, &mut r);
            }
        }
        let after: Vec<usize> = a.slots[0]
            .slices
            .iter()
            .map(|s| s.data.capacity())
            .collect();
        assert_eq!(caps, after);
        assert_eq!(r.done.len(), 5);
    }
}
