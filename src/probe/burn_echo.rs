//! issue 1367 — the node-burn ECHO gate of the camera-chain recording decode.
//!
//! A node burn (a camera capture burn or an OBS render burn) is a crisp overlay at ONE known place
//! on the recorded frame: its slot in [`crate::burn_regions`]. A camera that films a monitor showing
//! OBS captures smaller, still decodable copies of those burns. On run 386740541 cam2 filmed the
//! strih-lx HDMI multiview, and the Preview, Program and camera cells each held node burns, cam2's
//! own among them. rqrr reads them like any QR. Two things followed:
//! - the echoes added stale ids to cam2's contiguity (copies/gaps on both CAM2 windows);
//! - on frame 1521 an echo of strih's burn and one of cam3's burn satisfied the fast-path gate, so
//!   the real burns were never read.
//!
//! The gate: every decode pass keeps the centre of each rqrr grid ([`LocatedPayload`], in frame
//! pixels), and [`split_node_burn_echoes`] lets a slotted run_id through only when that centre lies
//! in its own slot plus the pad ([`crate::burn_regions::node_burn_in_own_slot`]). Anything else of
//! that run_id is an echo and never merges. The optical dual-QR, the aux marks and every other
//! unslotted payload pass untouched.
//!
//! The camera-chain decode (`qr::decode_qr_luma_all_fast_then_robust_grouped_pathed_optical`, the
//! strih and stream recordings) applies it after every pass: the full frame, the #754 top band, the
//! #202 tiles and the issue-1370 slot crops. The single-group decode (imag, cg, the cam1 grab, the
//! generic diagnostic tools) runs with [`NodeBurnGate::Off`], byte-identical to before.
//!
//! The rejected echoes are counted per process ([`burn_echo_rejection_count`]): distinct echo
//! payloads per frame, summed. The extract carries the count in its partial and the verdict reports
//! it (`burn_echoes_rejected`, report-only).

use crate::burn_regions::node_burn_in_own_slot;
use crate::probe::payload::Payload;
use crate::probe::qr::{merge_payloads, DecodePath};
use std::sync::atomic::{AtomicU64, Ordering};

/// One recording frame's decode (`qr::decode_qr_luma_all_fast_then_robust_gated`): the payloads
/// that count, the [`DecodePath`] it took, and the node-burn echoes the gate rejected (distinct, in
/// first-read order; always empty under [`NodeBurnGate::Off`]).
#[derive(Clone, Debug, PartialEq)]
pub struct FrameDecode {
    pub payloads: Vec<Payload>,
    pub path: DecodePath,
    pub echoes: Vec<Payload>,
}

/// Whether a decode lets a slotted node burn through only from its own slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeBurnGate {
    /// The camera-chain decode: a slotted node burn counts only inside its own slot + pad.
    OwnSlot,
    /// Every CRC-valid payload counts wherever it was read (the pre-issue-1367 behaviour).
    Off,
}

/// One decoded payload plus the centre of its rqrr grid (the mean of the four corners), in pixels
/// of the image it was read from. [`LocatedPayload::in_frame`] maps a crop's read back to the frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocatedPayload {
    pub payload: Payload,
    pub cx: f64,
    pub cy: f64,
}

impl LocatedPayload {
    /// The same read in frame pixels, for an image cropped at `(x0, y0)` and then resized so that
    /// one decoded pixel spans `scale_x` x `scale_y` crop pixels (1.0 when not resized).
    pub fn in_frame(self, x0: u32, y0: u32, scale_x: f64, scale_y: f64) -> Self {
        LocatedPayload {
            payload: self.payload,
            cx: f64::from(x0) + self.cx * scale_x,
            cy: f64::from(y0) + self.cy * scale_y,
        }
    }
}

/// Map every read of one crop back to frame pixels (see [`LocatedPayload::in_frame`]).
pub fn reads_in_frame(
    reads: Vec<LocatedPayload>,
    x0: u32,
    y0: u32,
    scale_x: f64,
    scale_y: f64,
) -> Vec<LocatedPayload> {
    reads
        .into_iter()
        .map(|r| r.in_frame(x0, y0, scale_x, scale_y))
        .collect()
}

/// The payloads of `reads`, in order, positions dropped.
pub fn payloads(reads: Vec<LocatedPayload>) -> Vec<Payload> {
    reads.into_iter().map(|r| r.payload).collect()
}

/// Merge `add` into `into`, keeping each distinct `(run_id, frame_id)` once and the FIRST read's
/// position: the located twin of `qr::merge_payloads`, with the same keep-first rule.
pub fn merge_located(into: &mut Vec<LocatedPayload>, add: Vec<LocatedPayload>) {
    for r in add {
        let p = r.payload;
        if !into
            .iter()
            .any(|q| q.payload.run_id == p.run_id && q.payload.frame_id == p.frame_id)
        {
            into.push(r);
        }
    }
}

/// Split the reads of one pass (frame pixels, on a `frame_w`x`frame_h` frame) into the payloads
/// that count and the node-burn echoes, both in read order. Under [`NodeBurnGate::Off`] every read
/// counts and there are no echoes.
pub fn split_node_burn_echoes(
    reads: Vec<LocatedPayload>,
    frame_w: u32,
    frame_h: u32,
    gate: NodeBurnGate,
) -> (Vec<Payload>, Vec<Payload>) {
    if gate == NodeBurnGate::Off {
        return (payloads(reads), Vec::new());
    }
    let mut accepted = Vec::with_capacity(reads.len());
    let mut echoes = Vec::new();
    for r in reads {
        if node_burn_in_own_slot(r.payload.run_id, r.cx, r.cy, frame_w, frame_h) {
            accepted.push(r.payload);
        } else {
            echoes.push(r.payload);
        }
    }
    (accepted, echoes)
}

/// Gate one later pass's reads (frame pixels) and merge them: what counts into `out`, the echoes
/// into `echoes`, each keeping every distinct `(run_id, frame_id)` once (`qr::merge_payloads`).
pub(crate) fn admit_reads(
    out: &mut Vec<Payload>,
    echoes: &mut Vec<Payload>,
    reads: Vec<LocatedPayload>,
    frame_w: u32,
    frame_h: u32,
    gate: NodeBurnGate,
) {
    let (accepted, rejected) = split_node_burn_echoes(reads, frame_w, frame_h, gate);
    merge_payloads(out, accepted);
    merge_payloads(echoes, rejected);
}

/// Process-wide count of node-burn echoes the camera-chain decode rejected (distinct payloads per
/// frame, summed). The extract reads it once at the end; tests assert on the per-call echoes
/// instead, since a global counter races concurrent tests.
static BURN_ECHOES_REJECTED: AtomicU64 = AtomicU64::new(0);

/// Node-burn echoes rejected since process start (see the module doc).
pub fn burn_echo_rejection_count() -> u64 {
    BURN_ECHOES_REJECTED.load(Ordering::Relaxed)
}

/// Add one frame's distinct rejected echoes to the process-wide count and log them.
pub(crate) fn record_frame_echoes(echoes: &[Payload]) {
    if echoes.is_empty() {
        return;
    }
    BURN_ECHOES_REJECTED.fetch_add(echoes.len() as u64, Ordering::Relaxed);
    tracing::debug!(
        echoes = ?echoes
            .iter()
            .map(|p| (p.run_id, p.frame_id))
            .collect::<Vec<_>>(),
        "issue 1367: node burn(s) read outside their own slot rejected as optical echoes"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(run_id: u32, frame_id: u32) -> Payload {
        Payload {
            run_id,
            frame_id,
            gen_ts_ns: i64::from(frame_id),
        }
    }

    fn at(payload: Payload, cx: f64, cy: f64) -> LocatedPayload {
        LocatedPayload { payload, cx, cy }
    }

    #[test]
    fn a_crop_read_maps_back_through_offset_and_scale_1367() {
        // A #202 tile: cropped at (0, 594), upscaled from 800x486 to 1280x777 (1/1.6 each way).
        let r = at(p(911_002, 1), 300.0, 480.0).in_frame(0, 594, 800.0 / 1280.0, 486.0 / 777.0);
        assert!((r.cx - 187.5).abs() < 1e-9, "{r:?}");
        assert!(
            (r.cy - (594.0 + 480.0 * 486.0 / 777.0)).abs() < 1e-9,
            "{r:?}"
        );
        // An unscaled crop only shifts.
        let r = at(p(911_002, 1), 10.0, 20.0).in_frame(792, 728, 1.0, 1.0);
        assert_eq!((r.cx, r.cy), (802.0, 748.0));
        assert_eq!(r.payload, p(911_002, 1));
    }

    #[test]
    fn the_gate_keeps_in_slot_burns_and_unslotted_payloads_in_read_order_1367() {
        // Frame 1521's top-band reads (1920x1080) plus its real in-slot burns.
        let reads = vec![
            at(p(911_008, 17043), 481.0, 454.0),
            at(p(386_740_541, 35275), 1328.0, 99.0),
            at(p(911_013, 35284), 631.0, 423.0),
            at(p(911_002, 16602), 1061.0, 442.0),
            at(p(911_009, 39230), 963.0, 916.0),
            at(p(911_002, 16607), 194.0, 893.0),
        ];
        let (accepted, echoes) =
            split_node_burn_echoes(reads.clone(), 1920, 1080, NodeBurnGate::OwnSlot);
        assert_eq!(
            accepted,
            vec![
                p(386_740_541, 35275),
                p(911_013, 35284),
                p(911_009, 39230),
                p(911_002, 16607)
            ]
        );
        assert_eq!(echoes, vec![p(911_008, 17043), p(911_002, 16602)]);
        // Off: everything counts, nothing is an echo — the pre-issue-1367 behaviour.
        let (all, none) = split_node_burn_echoes(reads.clone(), 1920, 1080, NodeBurnGate::Off);
        assert_eq!(all, payloads(reads));
        assert!(none.is_empty());
    }

    #[test]
    fn merge_located_keeps_the_first_read_of_each_identity_1367() {
        let mut into = vec![at(p(911_009, 51769), 963.0, 916.0)];
        merge_located(
            &mut into,
            vec![
                at(p(911_009, 51769), 1.0, 1.0),
                at(p(911_009, 51761), 1442.0, 453.0),
                at(p(911_009, 51761), 2.0, 2.0),
            ],
        );
        assert_eq!(
            into,
            vec![
                at(p(911_009, 51769), 963.0, 916.0),
                at(p(911_009, 51761), 1442.0, 453.0)
            ]
        );
    }
}
