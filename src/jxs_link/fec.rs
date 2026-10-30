//! Issue 1387 item 3 (owner 29.10.2026, comment 6509906102): forward error correction with NO
//! added latency -- one XOR parity datagram per group of up to [`FEC_GROUP`] datagrams of a slice.
//!
//! # The parity datagram
//!
//! The 16-byte slice header (parent module doc) with the magic [`FEC_MAGIC`] (`'F'`) instead of
//! `'J'`. Its `offset` is the byte offset of the group's FIRST datagram in the slice, `slice_len`
//! the slice's length, every other field that of the slice. Its payload is the XOR of the group's
//! data payloads, each zero-padded to the longest of them. An old receiver drops it as malformed
//! (bad magic) and loses nothing else; an old sender sends none.
//!
//! The group of a datagram follows from its own header: datagram `i = offset / SEGMENT_DATA_LEN`
//! of the slice is in group `i / FEC_GROUP`. The group's datagram count and each payload length
//! follow from `slice_len`, so a datagram the group misses can be rebuilt WHOLE, header and all.
//!
//! # No added latency
//!
//! The sender sends each group's parity right after the slice's datagrams (the same send turn, so
//! the pace schedule counts it). The receiver rebuilds a group's ONE missing datagram the moment
//! the rest of the group and its parity are in, and hands it to the assembler like any received
//! datagram, before the assembler would give the frame up (it waits for the NEXT frame). Two lost
//! datagrams in one group stay lost (concealed as before): an isolated loss is repaired, a burst
//! is not. Overhead: 1 datagram per 12 -- 9.1 % at 4 bpp 1080p (11 datagrams per slice), 9.5 % at
//! 8 bpp (21), 9.4 % at 12 (32).

use std::collections::{HashMap, VecDeque};

use super::{PacketHeader, DATAGRAM_LEN, HEADER_LEN, SEGMENT_DATA_LEN};

/// The parity datagram's magic byte (`'F'`), never a slice's `0x4A` or a report's `0x52`.
pub const FEC_MAGIC: u8 = 0x46;
/// Data datagrams covered by one parity datagram.
pub const FEC_GROUP: usize = 12;
/// Timecodes the receiver keeps groups for (the assembler holds two frames).
const KEEP_FRAMES: usize = 3;
/// A timecode this far behind every kept one is a clock step back: start over (the assembler's
/// `RESYNC_BACKWARD_100NS`).
const BACKWARD_STEP_100NS: u64 = super::assembler::RESYNC_BACKWARD_100NS;
const _: () = assert!(FEC_GROUP <= 16, "the received mask is a u16");

/// Datagrams of a slice of `slice_len` bytes (an empty slice is one header-only datagram).
pub fn datagrams_of(slice_len: usize) -> usize {
    slice_len.div_ceil(SEGMENT_DATA_LEN).max(1)
}

/// The parity datagrams of ONE slice whose datagrams `data` holds (as built by
/// `build_slice_datagrams`, every datagram [`DATAGRAM_LEN`] long but the last) into `out`; returns
/// how many. Every parity is [`DATAGRAM_LEN`] long but possibly the last, so `out` goes out as one
/// GSO send.
pub fn build_parity(data: &[u8], out: &mut Vec<u8>) -> usize {
    out.clear();
    let datagrams: Vec<&[u8]> = data.chunks(DATAGRAM_LEN).collect();
    let mut n = 0;
    for group in datagrams.chunks(FEC_GROUP) {
        let Some(first) = group.first() else { continue };
        if first.len() < HEADER_LEN {
            continue;
        }
        let len = group
            .iter()
            .map(|d| d.len().saturating_sub(HEADER_LEN))
            .max()
            .unwrap_or(0);
        let at = out.len();
        out.extend_from_slice(&first[..HEADER_LEN]);
        out[at] = FEC_MAGIC;
        out.resize(at + HEADER_LEN + len, 0);
        for d in group {
            let payload = &d[HEADER_LEN.min(d.len())..];
            for (o, b) in out[at + HEADER_LEN..].iter_mut().zip(payload) {
                *o ^= b;
            }
        }
        n += 1;
    }
    n
}

/// A datagram with the parity magic that is not a valid parity datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadParity;

#[derive(Debug)]
struct Group {
    header: PacketHeader,
    xor: Vec<u8>,
    have: u16,
    expected: usize,
    parity: bool,
    done: bool,
}

/// The receiver's repair state (module doc). Fed every data datagram ([`Self::on_data`]) and
/// every parity datagram ([`Self::on_parity`]); each returns a rebuilt datagram when its arrival
/// completed a group with exactly one datagram missing.
#[derive(Debug, Default)]
pub struct FecRepair {
    groups: HashMap<(u64, u8, u16), Group>,
    frames: VecDeque<u64>,
    /// Datagrams rebuilt since the receiver opened.
    pub repaired: u64,
}

impl FecRepair {
    /// Whether groups of frame `tc` may be kept: a frame older than every one kept (a late
    /// datagram of a frame already forgotten) never evicts a current one.
    fn note_frame(&mut self, tc: u64) -> bool {
        if self.frames.contains(&tc) {
            return true;
        }
        if let Some(oldest) = self.frames.iter().copied().min() {
            if tc + BACKWARD_STEP_100NS < oldest {
                // the wall stepped back (the nightly date step): the kept frames are of the old
                // timeline, forget them all (the assembler's own clock-step rule)
                self.frames.clear();
                self.groups.clear();
            } else if self.frames.len() == KEEP_FRAMES && self.frames.iter().all(|f| *f > tc) {
                return false;
            }
        }
        self.frames.push_back(tc);
        while self.frames.len() > KEEP_FRAMES {
            if let Some(old) = self.frames.iter().copied().min() {
                self.frames.retain(|f| *f != old);
                self.groups.retain(|k, _| k.0 != old);
            }
        }
        true
    }

    fn group(&mut self, h: &PacketHeader) -> Option<&mut Group> {
        let slice_len = h.slice_len as usize;
        let index = h.offset as usize / SEGMENT_DATA_LEN;
        let total = datagrams_of(slice_len);
        if index >= total {
            return None;
        }
        let g = index / FEC_GROUP;
        let expected = (total - g * FEC_GROUP).min(FEC_GROUP);
        if !self.note_frame(h.timecode_100ns) {
            return None;
        }
        let key = (h.timecode_100ns, h.slice_index, g as u16);
        let group = self.groups.entry(key).or_insert_with(|| Group {
            header: PacketHeader {
                offset: (g * FEC_GROUP * SEGMENT_DATA_LEN) as u16,
                ..*h
            },
            xor: vec![0; SEGMENT_DATA_LEN],
            have: 0,
            expected,
            parity: false,
            done: false,
        });
        (group.header.slice_len == h.slice_len
            && group.header.slice_count == h.slice_count
            && group.header.camera_id == h.camera_id)
            .then_some(group)
    }

    /// One received data datagram (its decoded header + payload). `out` = the rebuilt datagram
    /// when this one completed a repair.
    pub fn on_data(&mut self, h: &PacketHeader, payload: &[u8], out: &mut Vec<u8>) -> bool {
        let pos = (h.offset as usize / SEGMENT_DATA_LEN) % FEC_GROUP;
        let Some(g) = self.group(h) else {
            return false;
        };
        if g.have & (1 << pos) != 0 || g.done {
            return false;
        }
        g.have |= 1 << pos;
        for (x, b) in g.xor.iter_mut().zip(payload) {
            *x ^= b;
        }
        self.try_repair(h, out)
    }

    /// One parity datagram (whole, header included). `Err` = not a valid parity datagram.
    pub fn on_parity(&mut self, datagram: &[u8], out: &mut Vec<u8>) -> Result<bool, BadParity> {
        if datagram.len() < HEADER_LEN || datagram[0] != FEC_MAGIC {
            return Err(BadParity);
        }
        let mut copy = [0u8; HEADER_LEN];
        copy.copy_from_slice(&datagram[..HEADER_LEN]);
        copy[0] = super::MAGIC;
        let h = PacketHeader::decode(&copy).map_err(|_| BadParity)?;
        let index = h.offset as usize / SEGMENT_DATA_LEN;
        if !(h.offset as usize).is_multiple_of(SEGMENT_DATA_LEN) || !index.is_multiple_of(FEC_GROUP)
        {
            return Err(BadParity);
        }
        let payload = &datagram[HEADER_LEN..];
        if payload.len() > SEGMENT_DATA_LEN {
            return Err(BadParity);
        }
        let Some(g) = self.group(&h) else {
            return Err(BadParity);
        };
        if g.parity || g.done {
            return Ok(false);
        }
        g.parity = true;
        for (x, b) in g.xor.iter_mut().zip(payload) {
            *x ^= b;
        }
        Ok(self.try_repair(&h, out))
    }

    fn try_repair(&mut self, h: &PacketHeader, out: &mut Vec<u8>) -> bool {
        let index = h.offset as usize / SEGMENT_DATA_LEN;
        let gi = index / FEC_GROUP;
        let key = (h.timecode_100ns, h.slice_index, gi as u16);
        let Some(g) = self.groups.get_mut(&key) else {
            return false;
        };
        let have = g.have.count_ones() as usize;
        if g.done || !g.parity || have + 1 != g.expected {
            if have == g.expected {
                g.done = true;
            }
            return false;
        }
        let pos = (0..g.expected)
            .find(|p| g.have & (1 << p) == 0)
            .expect("one missing");
        let offset = (gi * FEC_GROUP + pos) * SEGMENT_DATA_LEN;
        let slice_len = g.header.slice_len as usize;
        let len = slice_len.saturating_sub(offset).min(SEGMENT_DATA_LEN);
        let rebuilt = PacketHeader {
            offset: offset as u16,
            ..g.header
        };
        let mut hdr = [0u8; HEADER_LEN];
        rebuilt.encode(&mut hdr);
        out.clear();
        out.extend_from_slice(&hdr);
        out.extend_from_slice(&g.xor[..len]);
        g.done = true;
        self.repaired += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jxs_link::build_slice_datagrams;

    fn slice(len: usize, seed: u32) -> Vec<u8> {
        (0..len as u32)
            .map(|i| (i.wrapping_mul(31) ^ seed) as u8)
            .collect()
    }

    fn base(tc: u64, index: u8) -> PacketHeader {
        PacketHeader {
            camera_id: 100,
            timecode_100ns: tc,
            slice_index: index,
            slice_count: 69,
            offset: 0,
            slice_len: 0,
        }
    }

    fn datagrams(buf: &[u8]) -> Vec<Vec<u8>> {
        buf.chunks(DATAGRAM_LEN).map(<[u8]>::to_vec).collect()
    }

    #[test]
    fn one_parity_per_twelve_datagrams_about_a_tenth_more() {
        let mut data = Vec::new();
        let mut par = Vec::new();
        for (len, groups) in [
            (0, 1),
            (110, 1),
            (1456, 1),
            (15_073, 1),
            (30_147, 2),
            (46_000, 3),
        ] {
            build_slice_datagrams(base(1, 3), &slice(len, 7), &mut data).unwrap();
            assert_eq!(build_parity(&data, &mut par), groups, "{len} bytes");
            let ps = datagrams(&par);
            assert_eq!(ps.len(), groups);
            for (g, p) in ps.iter().enumerate() {
                assert_eq!(p[0], FEC_MAGIC);
                let mut h = [0u8; HEADER_LEN];
                h.copy_from_slice(&p[..HEADER_LEN]);
                h[0] = crate::jxs_link::MAGIC;
                let h = PacketHeader::decode(&h).unwrap();
                assert_eq!(h.offset as usize, g * FEC_GROUP * SEGMENT_DATA_LEN);
                assert_eq!(h.slice_len as usize, len);
                assert!(p.len() <= DATAGRAM_LEN);
            }
            // every parity but the last is a full datagram: one GSO send
            assert!(ps[..ps.len() - 1].iter().all(|p| p.len() == DATAGRAM_LEN));
        }
        // the overhead at 4 / 8 / 12 bpp 1080p (15 / 30 / 45 KB slices)
        for (len, max_pct) in [(15_073usize, 9.1), (30_147, 9.6), (45_220, 9.4)] {
            let n = datagrams_of(len);
            let pct = 100.0 * n.div_ceil(FEC_GROUP) as f64 / n as f64;
            assert!(pct <= max_pct, "{len}: {pct:.1} %");
        }
    }

    /// Lose each datagram of a slice in turn (and in either order of parity vs the rest): the
    /// repair rebuilds it byte for byte, header included.
    #[test]
    fn any_single_lost_datagram_of_a_group_is_rebuilt_exactly() {
        let mut data = Vec::new();
        let mut par = Vec::new();
        for len in [0usize, 110, 1455, 1456, 1457, 17_500, 30_147] {
            let s = slice(len, len as u32);
            build_slice_datagrams(base(7, 5), &s, &mut data).unwrap();
            build_parity(&data, &mut par);
            let ds = datagrams(&data);
            let ps = datagrams(&par);
            for lost in 0..ds.len() {
                for parity_first in [false, true] {
                    let mut f = FecRepair::default();
                    let mut out = Vec::new();
                    let mut rebuilt = None;
                    let group = lost / FEC_GROUP;
                    if parity_first && f.on_parity(&ps[group], &mut out).unwrap() {
                        rebuilt = Some(out.clone());
                    }
                    for (i, d) in ds.iter().enumerate() {
                        if i == lost {
                            continue;
                        }
                        let h = PacketHeader::decode(d).unwrap();
                        if f.on_data(&h, &d[HEADER_LEN..], &mut out) {
                            rebuilt = Some(out.clone());
                        }
                    }
                    if !parity_first {
                        for p in &ps {
                            if f.on_parity(p, &mut out).unwrap() {
                                rebuilt = Some(out.clone());
                            }
                        }
                    }
                    assert_eq!(
                        rebuilt.as_deref(),
                        Some(ds[lost].as_slice()),
                        "slice {len}, lost {lost}, parity first {parity_first}"
                    );
                    assert_eq!(f.repaired, 1);
                }
            }
        }
    }

    #[test]
    fn two_losses_in_a_group_or_a_whole_slice_received_repair_nothing() {
        let mut data = Vec::new();
        let mut par = Vec::new();
        build_slice_datagrams(base(9, 2), &slice(17_500, 1), &mut data).unwrap();
        build_parity(&data, &mut par);
        let ds = datagrams(&data);
        let ps = datagrams(&par);
        let mut f = FecRepair::default();
        let mut out = Vec::new();
        for (i, d) in ds.iter().enumerate() {
            if i == 3 || i == 4 {
                continue;
            }
            let h = PacketHeader::decode(d).unwrap();
            assert!(!f.on_data(&h, &d[HEADER_LEN..], &mut out));
        }
        assert!(
            !f.on_parity(&ps[0], &mut out).unwrap(),
            "two missing: no repair"
        );
        // nothing lost: the parity rebuilds nothing; a duplicate datagram never corrupts the XOR
        let mut f = FecRepair::default();
        for d in &ds {
            let h = PacketHeader::decode(d).unwrap();
            assert!(!f.on_data(&h, &d[HEADER_LEN..], &mut out));
            assert!(!f.on_data(&h, &d[HEADER_LEN..], &mut out));
        }
        for p in &ps {
            assert!(!f.on_parity(p, &mut out).unwrap());
        }
        assert_eq!(f.repaired, 0);
    }

    #[test]
    fn a_bad_parity_is_refused_and_old_frames_are_forgotten() {
        let mut f = FecRepair::default();
        let mut out = Vec::new();
        let mut data = Vec::new();
        let mut par = Vec::new();
        build_slice_datagrams(base(11, 4), &slice(3000, 2), &mut data).unwrap();
        build_parity(&data, &mut par);
        let mut bad = par.clone();
        bad[12] = 1; // an offset inside a group
        assert!(f.on_parity(&bad, &mut out).is_err());
        assert!(
            f.on_parity(&data[..DATAGRAM_LEN], &mut out).is_err(),
            "a data datagram"
        );
        let mut long = par.clone();
        long.resize(HEADER_LEN + SEGMENT_DATA_LEN + 1, 0);
        assert!(f.on_parity(&long, &mut out).is_err());
        // only the newest frames are kept
        for tc in 0..10u64 {
            build_slice_datagrams(base(100 + tc, 1), &slice(3000, 2), &mut data).unwrap();
            let d = &data[..DATAGRAM_LEN];
            let h = PacketHeader::decode(d).unwrap();
            f.on_data(&h, &d[HEADER_LEN..], &mut out);
        }
        assert!(f.groups.len() <= KEEP_FRAMES);
    }

    /// Review round 2: after the wall steps back (the nightly date step), the FEC repairs again at
    /// once instead of refusing every frame until the clock passes the kept ones.
    #[test]
    fn a_clock_step_back_starts_the_repair_over() {
        let mut f = FecRepair::default();
        let mut out = Vec::new();
        let mut data = Vec::new();
        let mut par = Vec::new();
        for tc in [10_000_000u64, 10_333_333, 10_666_666] {
            build_slice_datagrams(base(tc, 1), &slice(3000, 2), &mut data).unwrap();
            let d = &data[..DATAGRAM_LEN];
            let h = PacketHeader::decode(d).unwrap();
            f.on_data(&h, &d[HEADER_LEN..], &mut out);
        }
        // 200 ms back: a datagram of the new timeline, then its parity -- repaired
        let tc = 8_000_000u64;
        build_slice_datagrams(base(tc, 1), &slice(3000, 2), &mut data).unwrap();
        build_parity(&data, &mut par);
        let ds = datagrams(&data);
        assert!(ds.len() >= 3);
        for (i, d) in ds.iter().enumerate().filter(|(i, _)| *i != 1) {
            let h = PacketHeader::decode(d).unwrap();
            assert!(!f.on_data(&h, &d[HEADER_LEN..], &mut out), "datagram {i}");
        }
        assert!(
            f.on_parity(&datagrams(&par)[0], &mut out).unwrap(),
            "the lost 2nd is rebuilt"
        );
        assert_eq!(out, ds[1]);
    }
}
