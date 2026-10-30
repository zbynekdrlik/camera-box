//! Issue 1387 -- the JPEG XS link end to end through the real codec (needs the `jxs` feature,
//! which CI's lint/test/coverage jobs build with `--all-features`).
//!
//! 1. The camera-box sender path (`JxsSender` -> `JxsHandoff::offer`, the same call the capture
//!    loop makes) encodes a synthetic YUYV frame and sends it over 127.0.0.1 with GSO; the
//!    receive side (`JxsRecvSocket` recvmmsg + GRO -> `FrameAssembler` -> `DecodeSink`) decodes it
//!    back within a PSNR bound, with the timecode and camera id carried end to end.
//! 2. A lost slice is concealed from the previous frame and counted.
#![cfg(all(target_os = "linux", feature = "jxs"))]

use camera_box::jxs_link::{
    self, build_slice_datagrams, psnr, split_gro, FrameAssembler, FrameReport, PacketHeader,
    DATAGRAM_LEN, HEADER_LEN,
};
use camera_box::jxs_recv::{DecodeSink, JxsRecvSocket};
use camera_box::jxs_send::{JxsFrame, JxsSender};
use std::cell::RefCell;
use std::time::{Duration, Instant};
use svt_jpegxs_sys::{Encoder, EncoderSettings, YuvPlanes};

const W: usize = 640;
const H: usize = 128;

/// A smooth synthetic picture (gradients + a soft circle), shifted by `phase`.
fn planar(phase: usize) -> YuvPlanes {
    let mut p = YuvPlanes::new(W as u32, H as u32);
    let (py, pu, pv) = p.planes_mut();
    for y in 0..H {
        for x in 0..W {
            let dx = x as f64 - (W / 2 + phase) as f64;
            let dy = y as f64 - (H / 2) as f64;
            let circle = if dx * dx + dy * dy < 40.0 * 40.0 {
                60.0
            } else {
                0.0
            };
            py[y * W + x] = (40.0 + ((x + phase) % W) as f64 * 120.0 / W as f64 + circle) as u8;
        }
        for x in 0..W / 2 {
            pu[y * W / 2 + x] = (100 + (y * 40 / H) + phase % 20) as u8;
            pv[y * W / 2 + x] = (150 - (x * 30 / (W / 2))) as u8;
        }
    }
    p
}

fn to_yuyv(p: &YuvPlanes) -> Vec<u8> {
    let mut out = Vec::with_capacity(W * H * 2);
    for y in 0..H {
        for x in 0..W / 2 {
            out.push(p.y()[y * W + 2 * x]);
            out.push(p.u()[y * W / 2 + x]);
            out.push(p.y()[y * W + 2 * x + 1]);
            out.push(p.v()[y * W / 2 + x]);
        }
    }
    out
}

#[test]
fn the_wire_format_derives_the_packet_count_from_the_codec_slice_height() {
    assert_eq!(
        jxs_link::SLICE_HEIGHT_LINES,
        svt_jpegxs_sys::SLICE_HEIGHT_LINES
    );
}

#[test]
fn a_frame_crosses_loopback_and_decodes_back_within_the_psnr_bound() {
    let mut rx = JxsRecvSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    rx.set_timeout(Some(Duration::from_millis(100))).unwrap();
    let port = rx.local_addr().unwrap().port();
    let tx = JxsSender::start(Some(&format!("127.0.0.1:{port}")), "CAM5")
        .expect("the link starts when the target is set");

    let original = planar(0);
    let yuyv = to_yuyv(&original);
    let timecode: i64 = 17_618_456_000_000_000;
    tx.offer(&JxsFrame {
        data: &yuyv,
        width: W as u32,
        height: H as u32,
        stride: (W * 2) as u32,
        yuyv: true,
        timecode_100ns: timecode,
        capture_realtime_100ns: 0,
        emit_fps: 60,
    });

    let mut assembler = FrameAssembler::new();
    let got: RefCell<Option<(FrameReport, YuvPlanes)>> = RefCell::new(None);
    let mut sink = DecodeSink::new(|r: &FrameReport, p: Option<&YuvPlanes>| {
        *got.borrow_mut() = Some((*r, p.expect("decoded").clone()));
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while got.borrow().is_none() && Instant::now() < deadline {
        rx.recv_batch(|d, at| {
            // the FEC parity datagrams (issue 1387 item 3) are not slices
            if d[0] == camera_box::jxs_link::fec::FEC_MAGIC {
                return;
            }
            let h = PacketHeader::decode(d).expect("header");
            assembler.push(&h, &d[HEADER_LEN..], at, &mut sink);
        })
        .unwrap();
        assert!(
            sink.last_error.is_none(),
            "decoder error: {:?}",
            sink.last_error
        );
    }
    drop(sink);
    let (report, decoded) = got.into_inner().expect("a decoded frame within 10 s");
    assert_eq!(report.timecode, timecode as u64);
    assert_eq!(report.camera_id, 5);
    assert_eq!(report.lost_slices, 0);
    assert_eq!(
        report.slice_count as u32,
        jxs_link::packets_per_frame(H as u32)
    );
    let py = psnr(original.y(), decoded.y());
    let pu = psnr(original.u(), decoded.u());
    let pv = psnr(original.v(), decoded.v());
    assert!(py >= 40.0, "luma PSNR {py:.2} dB");
    assert!(pu >= 40.0 && pv >= 40.0, "chroma PSNR {pu:.2}/{pv:.2} dB");
    // the sender counts nothing as dropped for one frame
    assert_eq!(tx.dropped_busy(), 0);
}

/// Slice 1b: frames offered faster than one encode lasts are all taken (two in flight), every
/// one decodes to its own picture with its own timecode, in order, and the ones dropped are
/// counted, never lost silently.
#[test]
fn a_stream_of_frames_keeps_two_in_flight_and_every_frame_decodes_in_order() {
    let mut rx = JxsRecvSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    rx.set_timeout(Some(Duration::from_millis(100))).unwrap();
    let port = rx.local_addr().unwrap().port();
    let tx = JxsSender::start(Some(&format!("127.0.0.1:{port}")), "CAM5").expect("link on");
    const N: usize = 12;
    let pictures: Vec<YuvPlanes> = (0..N).map(|k| planar(k * 13)).collect();
    let base: i64 = 17_618_456_000_000_000;
    let got: RefCell<Vec<(u64, YuvPlanes)>> = RefCell::new(Vec::new());
    let mut assembler = FrameAssembler::new();
    let mut sink = DecodeSink::new(|r: &FrameReport, p: Option<&YuvPlanes>| {
        if let Some(p) = p {
            got.borrow_mut().push((r.timecode, p.clone()));
        }
    });
    // two offers back to back, then a pause: the second one must not be dropped
    for pair in 0..N / 2 {
        for k in [2 * pair, 2 * pair + 1] {
            let yuyv = to_yuyv(&pictures[k]);
            // the even slots of the 60 fps emit grid: the frames JPEG XS takes
            tx.offer(&JxsFrame {
                data: &yuyv,
                width: W as u32,
                height: H as u32,
                stride: (W * 2) as u32,
                yuyv: true,
                timecode_100ns: base + 2 * k as i64 * 10_000_000 / 60,
                capture_realtime_100ns: 0,
                emit_fps: 60,
            });
            // the odd slot after it never goes (and is no drop)
            tx.offer(&JxsFrame {
                data: &yuyv,
                width: W as u32,
                height: H as u32,
                stride: (W * 2) as u32,
                yuyv: true,
                timecode_100ns: base + (2 * k as i64 + 1) * 10_000_000 / 60,
                capture_realtime_100ns: 0,
                emit_fps: 60,
            });
        }
        let until = Instant::now() + Duration::from_millis(150);
        while Instant::now() < until {
            rx.recv_batch(|d, at| {
                // the FEC parity datagrams (issue 1387 item 3) are not slices
                if d[0] == camera_box::jxs_link::fec::FEC_MAGIC {
                    return;
                }
                let h = PacketHeader::decode(d).expect("header");
                assembler.push(&h, &d[HEADER_LEN..], at, &mut sink);
            })
            .unwrap();
        }
    }
    assert!(sink.last_error.is_none(), "{:?}", sink.last_error);
    drop(sink);
    assert_eq!(
        tx.dropped_busy(),
        0,
        "two frames back to back are both taken"
    );
    let got = got.into_inner();
    // the assembler finishes a frame when the next one completes: the last may still be open
    assert!(got.len() >= N - 1, "{} of {N} frames decoded", got.len());
    for (k, (tc, p)) in got.iter().enumerate() {
        assert_eq!(
            *tc,
            (base + 2 * k as i64 * 10_000_000 / 60) as u64,
            "frame {k} in order, only the even slots"
        );
        let py = psnr(pictures[k].y(), p.y());
        assert!(py >= 40.0, "frame {k} luma PSNR {py:.2} dB");
        if k > 0 {
            assert!(
                psnr(pictures[k - 1].y(), p.y()) < py,
                "frame {k} is its own picture"
            );
        }
    }
}

/// Encode one picture into its packets.
fn encode(enc: &mut Encoder, p: &YuvPlanes) -> Vec<Vec<u8>> {
    let (y, u, v) = enc.planes_mut().planes_mut();
    y.copy_from_slice(p.y());
    u.copy_from_slice(p.u());
    v.copy_from_slice(p.v());
    let mut packets = Vec::new();
    enc.encode_frame(|_, b, _| packets.push(b.to_vec()))
        .unwrap();
    packets
}

fn push_frame(
    a: &mut FrameAssembler,
    sink: &mut impl jxs_link::SliceSink,
    tc: u64,
    packets: &[Vec<u8>],
    drop_index: Option<usize>,
) {
    for (i, p) in packets.iter().enumerate() {
        if Some(i) == drop_index {
            continue;
        }
        let base = PacketHeader {
            camera_id: 1,
            timecode_100ns: tc,
            slice_index: i as u8,
            slice_count: packets.len() as u8,
            offset: 0,
            slice_len: 0,
        };
        let mut buf = Vec::new();
        build_slice_datagrams(base, p, &mut buf).unwrap();
        for d in split_gro(&buf, Some(DATAGRAM_LEN)) {
            let h = PacketHeader::decode(d).unwrap();
            a.push(&h, &d[HEADER_LEN..], 0, sink);
        }
    }
}

#[test]
fn a_lost_slice_is_concealed_from_the_previous_frame_and_counted() {
    let mut enc = Encoder::open(EncoderSettings::issue_1387(W as u32, H as u32)).unwrap();
    let pa = planar(0);
    let pb = planar(200);
    let ka = encode(&mut enc, &pa);
    let kb = encode(&mut enc, &pb);
    let kc = encode(&mut enc, &pa);
    let lost = 3; // packet 3 = slice 3 = luma rows 32..48

    let mut out: Vec<(FrameReport, YuvPlanes)> = Vec::new();
    {
        let mut a = FrameAssembler::new();
        let mut sink = DecodeSink::new(|r: &FrameReport, p: Option<&YuvPlanes>| {
            out.push((*r, p.expect("decoded").clone()));
        });
        push_frame(&mut a, &mut sink, 1000, &ka, None);
        push_frame(&mut a, &mut sink, 1167, &kb, Some(lost));
        // the next complete frame finalizes the one with the hole
        push_frame(&mut a, &mut sink, 1333, &kc, None);
        assert_eq!(sink.errors, 0, "{:?}", sink.last_error);
    }
    assert_eq!(out.len(), 3);
    assert_eq!(out[0].0.lost_slices, 0);
    assert_eq!(out[1].0.lost_slices, 1, "the hole is counted");
    assert_eq!(out[2].0.lost_slices, 0);

    let (a_dec, b_dec) = (&out[0].1, &out[1].1);
    let rows = |p: &YuvPlanes, r0: usize, r1: usize| p.y()[r0 * W..r1 * W].to_vec();
    let s0 = (lost - 1) * 16;
    // inside the concealed slice (away from its edges) the picture is frame A's
    let conceal_vs_a = psnr(&rows(b_dec, s0 + 4, s0 + 12), &rows(a_dec, s0 + 4, s0 + 12));
    let conceal_vs_b = psnr(&rows(b_dec, s0 + 4, s0 + 12), &rows(&pb, s0 + 4, s0 + 12));
    assert!(
        conceal_vs_a > 35.0 && conceal_vs_a > conceal_vs_b + 10.0,
        "concealed rows: vs A {conceal_vs_a:.1} dB, vs B {conceal_vs_b:.1} dB"
    );
    // the rest of frame B is frame B
    let far = psnr(&rows(b_dec, 80, H), &rows(&pb, 80, H));
    assert!(far >= 40.0, "unaffected rows {far:.1} dB");
    // and the frame after the hole decodes clean again
    assert!(psnr(out[2].1.y(), pa.y()) >= 40.0);
}

/// Run `f` on a thread and fail (instead of wedging the suite) if it does not finish in time.
fn within<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> T {
    let h = std::thread::spawn(f);
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !h.is_finished() {
        assert!(Instant::now() < deadline, "the receiver blocked");
        std::thread::sleep(Duration::from_millis(10));
    }
    h.join().expect("no panic")
}

/// A 1080-line picture: 69 packets, enough slices to exhaust the decoder's frame pools if a
/// failed frame were fed to its end.
fn tall(phase: u8) -> YuvPlanes {
    let mut p = YuvPlanes::new(320, 1080);
    let (y, u, v) = p.planes_mut();
    for (i, b) in y.iter_mut().enumerate() {
        *b = ((i % 320) as u8 / 2).wrapping_add(phase);
    }
    u.fill(120);
    v.fill(140);
    p
}

#[test]
fn a_garbled_slice_costs_that_frame_and_the_next_one_decodes() {
    let decoded: Vec<bool> = within(30, || {
        let mut enc = Encoder::open(EncoderSettings::issue_1387(320, 1080)).unwrap();
        let fa = encode(&mut enc, &tall(0));
        let mut fb = encode(&mut enc, &tall(40));
        let fc = encode(&mut enc, &tall(80));
        assert_eq!(fb.len(), 69);
        for b in fb[2].iter_mut().skip(2) {
            *b ^= 0xA5;
        }
        let mut seen = Vec::new();
        let mut a = FrameAssembler::new();
        let mut sink = DecodeSink::new(|_: &FrameReport, p: Option<&YuvPlanes>| {
            seen.push(p.is_some());
        });
        push_frame(&mut a, &mut sink, 1000, &fa, None);
        push_frame(&mut a, &mut sink, 1167, &fb, None);
        push_frame(&mut a, &mut sink, 1333, &fc, None);
        push_frame(&mut a, &mut sink, 1500, &fa, None);
        drop(sink);
        seen
    });
    assert_eq!(decoded.len(), 4, "every frame is reported");
    assert!(decoded[0], "the first frame decodes");
    // the garbled frame either fails (reported, decoder reopened) or the entropy decoder
    // tolerated it; either way the stream goes on:
    assert!(decoded[3], "a later clean frame decodes: {decoded:?}");
}

#[test]
fn a_clock_step_back_mid_frame_ends_that_frame_and_the_stream_goes_on() {
    let reports: Vec<(u64, u32, bool)> = within(30, || {
        let mut enc = Encoder::open(EncoderSettings::issue_1387(W as u32, H as u32)).unwrap();
        let fa = encode(&mut enc, &planar(0));
        let fb = encode(&mut enc, &planar(50));
        let mut out = Vec::new();
        let mut a = FrameAssembler::new();
        let mut sink = DecodeSink::new(|r: &FrameReport, p: Option<&YuvPlanes>| {
            out.push((r.timecode, r.lost_slices, p.is_some()));
        });
        let t0 = 100_000_000u64;
        push_frame(&mut a, &mut sink, t0, &fa, None);
        // half of the next frame, then the sender's clock steps back 200 ms
        push_frame_prefix(&mut a, &mut sink, t0 + 166_667, &fb, 3);
        push_frame(&mut a, &mut sink, t0 - 2_000_000, &fb, None);
        push_frame(&mut a, &mut sink, t0 - 2_000_000 + 166_667, &fa, None);
        drop(sink);
        out
    });
    assert_eq!(reports.len(), 4, "{reports:?}");
    assert_eq!(reports[1].0, 100_000_000 + 166_667);
    assert!(reports[1].1 > 0, "the cut frame reports its lost packets");
    assert!(
        reports[2].2 && reports[3].2,
        "the new epoch decodes: {reports:?}"
    );
}

/// Issue 1387 (live 29.10.2026, cam5 -> strih-snv): the auto bpp tried 4 -> 5 and the receiver
/// logged `undecodable` + "the frame ended before its last packet", so the sender stepped straight
/// back. A bpp change keeps the geometry and the packet count; only the codestream size moves.
/// Every frame across 4 -> 5 -> 8 -> 6 -> 4 must decode, with no loss counted.
#[test]
fn a_bpp_change_mid_stream_decodes_every_frame_with_no_loss() {
    let (seen, errors, last_error) = within(60, || {
        let mut frames: Vec<Vec<Vec<u8>>> = Vec::new();
        let mut phase = 0u8;
        for bpp in [4u32, 5, 8, 6, 4] {
            let mut s = EncoderSettings::issue_1387(320, 1080);
            s.bpp_numerator = bpp;
            let mut enc = Encoder::open(s).unwrap();
            for _ in 0..3 {
                frames.push(encode(&mut enc, &tall(phase)));
                phase = phase.wrapping_add(17);
            }
        }
        let mut seen: Vec<(u64, u32, bool)> = Vec::new();
        let (errors, last_error) = {
            let mut a = FrameAssembler::new();
            let mut sink = DecodeSink::new(|r: &FrameReport, p: Option<&YuvPlanes>| {
                seen.push((r.timecode, r.lost_slices, p.is_some()));
            });
            let t0 = 100_000_000u64;
            for (k, f) in frames.iter().enumerate() {
                push_frame(&mut a, &mut sink, t0 + k as u64 * 333_333, f, None);
            }
            // one more frame finalizes the last one of the run
            push_frame(&mut a, &mut sink, t0 + 15 * 333_333, &frames[0], None);
            (sink.errors, sink.last_error.clone())
        };
        (seen, errors, last_error)
    });
    assert!(seen.len() >= 15, "every frame is reported: {seen:?}");
    for (k, (tc, lost, decoded)) in seen.iter().take(15).enumerate() {
        assert_eq!(*lost, 0, "frame {k} (tc {tc}) counts no loss");
        assert!(
            *decoded,
            "frame {k} decodes: {seen:?}, last error {last_error:?}"
        );
    }
    assert_eq!(errors, 0, "undecodable == 0, last error {last_error:?}");
}

/// Issue 1387: across a bpp change every decoded picture is its OWN frame (never a stale one),
/// the decoder reopens exactly once per change and never in a steady stream, and a slice lost in
/// the second frame at the new bpp is concealed from the first one (the concealment source is the
/// new bpp's, the old bpp's slices are never replayed).
#[test]
fn after_a_bpp_change_every_picture_is_its_own_and_a_loss_is_concealed_from_the_new_bpp() {
    let (out, header_changes, errors) = within(60, || {
        let mut frames: Vec<(YuvPlanes, Vec<Vec<u8>>)> = Vec::new();
        let mut phase = 0u8;
        for (bpp, n) in [(4u32, 3), (8, 3)] {
            let mut s = EncoderSettings::issue_1387(320, 1080);
            s.bpp_numerator = bpp;
            let mut enc = Encoder::open(s).unwrap();
            for _ in 0..n {
                let p = tall(phase);
                let k = encode(&mut enc, &p);
                frames.push((p, k));
                phase = phase.wrapping_add(53);
            }
        }
        let mut out: Vec<(u32, Option<YuvPlanes>)> = Vec::new();
        let (header_changes, errors) = {
            let mut a = FrameAssembler::new();
            let mut sink = DecodeSink::new(|r: &FrameReport, p: Option<&YuvPlanes>| {
                out.push((r.lost_slices, p.cloned()));
            });
            let t0 = 100_000_000u64;
            for (k, (_, f)) in frames.iter().enumerate() {
                // the second frame at 8 bpp loses packet 5
                let drop = (k == 4).then_some(5);
                push_frame(&mut a, &mut sink, t0 + k as u64 * 333_333, f, drop);
            }
            push_frame(&mut a, &mut sink, t0 + 6 * 333_333, &frames[5].1, None);
            (sink.header_changes, sink.errors)
        };
        (out, header_changes, errors)
    });
    assert_eq!(errors, 0);
    assert_eq!(header_changes, 1, "one reopen for the one bpp change");
    assert!(out.len() >= 6);
    for (k, (lost, planes)) in out.iter().take(6).enumerate() {
        let planes = planes
            .as_ref()
            .unwrap_or_else(|| panic!("frame {k} decodes"));
        assert_eq!(*lost, u32::from(k == 4), "frame {k} loss count");
        let own = psnr(planes.y(), frames_src(k).y());
        let prev = psnr(planes.y(), frames_src(k.max(1) - 1).y());
        if k == 4 {
            // outside the concealed slice the picture is frame 4's
            let w = 320;
            let far = psnr(&planes.y()[200 * w..], &frames_src(4).y()[200 * w..]);
            assert!(far >= 35.0, "frame 4 away from the hole: {far:.1} dB");
        } else {
            assert!(own >= 35.0, "frame {k} is its own picture: {own:.1} dB");
            if k > 0 {
                assert!(
                    own > prev + 5.0,
                    "frame {k} is not stale: own {own:.1} prev {prev:.1}"
                );
            }
        }
    }
    // inside the hole (packet 5 = luma rows 64..80) frame 4 shows frame 3's decode, the new bpp's
    let w = 320;
    let rows = |p: &YuvPlanes| p.y()[68 * w..76 * w].to_vec();
    let (f3, f4) = (out[3].1.as_ref().unwrap(), out[4].1.as_ref().unwrap());
    let hole_vs_3 = psnr(&rows(f4), &rows(f3));
    let hole_vs_4 = psnr(&rows(f4), &rows(&frames_src(4)));
    assert!(
        hole_vs_3 > 35.0 && hole_vs_3 > hole_vs_4 + 5.0,
        "concealed rows: vs frame 3 {hole_vs_3:.1} dB, vs frame 4 {hole_vs_4:.1} dB"
    );
}

/// The source picture of frame `k` of the bpp-change test above (the same phase sequence).
fn frames_src(k: usize) -> YuvPlanes {
    tall((k as u8).wrapping_mul(53))
}

/// Encode `n` frames of `tall(phase0 + 53 j)` at `bpp`.
fn tall_frames_at(bpp: u32, phase0: u8, n: usize) -> Vec<(YuvPlanes, Vec<Vec<u8>>)> {
    let mut s = EncoderSettings::issue_1387(320, 1080);
    s.bpp_numerator = bpp;
    let mut enc = Encoder::open(s).unwrap();
    (0..n)
        .map(|j| {
            let p = tall(phase0.wrapping_add((j as u8).wrapping_mul(53)));
            let k = encode(&mut enc, &p);
            (p, k)
        })
        .collect()
}

/// Run `frames` (with an optional dropped packet per frame) through the receiver; returns each
/// frame's (lost slices, decoded picture), the undecodable count and the header changes.
fn receive(
    frames: Vec<(Vec<Vec<u8>>, Option<usize>)>,
) -> (Vec<(u32, Option<YuvPlanes>)>, u64, u64) {
    within(60, move || {
        let mut out: Vec<(u32, Option<YuvPlanes>)> = Vec::new();
        let (errors, changes) = {
            let mut a = FrameAssembler::new();
            let mut sink = DecodeSink::new(|r: &FrameReport, p: Option<&YuvPlanes>| {
                out.push((r.lost_slices, p.cloned()));
            });
            let t0 = 100_000_000u64;
            for (k, (f, drop)) in frames.iter().enumerate() {
                push_frame(&mut a, &mut sink, t0 + k as u64 * 333_333, f, *drop);
            }
            let last = &frames[frames.len() - 1].0;
            push_frame(
                &mut a,
                &mut sink,
                t0 + frames.len() as u64 * 333_333,
                last,
                None,
            );
            (sink.errors, sink.header_changes)
        };
        (out, errors, changes)
    })
}

/// Issue 1387 review: a slice lost in the FIRST frame at a new bpp has no concealment source
/// (the old bpp's slices have other sizes): that frame is reported undecodable, never decoded
/// from a mix of two bpps, and the next frame decodes as its own picture.
#[test]
fn a_loss_in_the_first_frame_at_a_new_bpp_is_never_concealed_from_the_old_bpp() {
    let at4 = tall_frames_at(4, 0, 2);
    let at5 = tall_frames_at(5, 111, 2);
    let src5 = at5[1].0.clone();
    let frames = vec![
        (at4[0].1.clone(), None),
        (at4[1].1.clone(), None),
        (at5[0].1.clone(), Some(7)),
        (at5[1].1.clone(), None),
    ];
    let (out, errors, changes) = receive(frames);
    assert!(out.len() >= 4, "every frame is reported");
    assert!(out[0].1.is_some() && out[1].1.is_some(), "4 bpp decodes");
    assert_eq!(out[2].0, 1, "the hole is counted");
    assert!(out[2].1.is_none(), "never decoded from a mix of two bpps");
    assert_eq!(errors, 1, "exactly the frame with the hole is undecodable");
    assert_eq!(changes, 1);
    let p3 = out[3].1.as_ref().expect("the next frame decodes");
    assert!(psnr(p3.y(), src5.y()) >= 35.0);
}

/// Issue 1387 review: a bpp change right after a frame the decoder gave up on (the decoder is
/// closed when the new header arrives) still forgets the old bpp's concealment source: a lost
/// slice in that first new-bpp frame makes it undecodable, never a decode from two bpps.
#[test]
fn a_bpp_change_after_a_failed_frame_never_conceals_from_the_old_bpp() {
    let at4 = tall_frames_at(4, 0, 2);
    let at5 = tall_frames_at(5, 111, 2);
    let src5 = at5[1].0.clone();
    let mut garbled = at4[1].1.clone();
    for b in garbled[2].iter_mut().skip(2) {
        *b ^= 0xA5;
    }
    let frames = vec![
        (at4[0].1.clone(), None),
        (garbled, None),
        (at5[0].1.clone(), Some(7)),
        (at5[1].1.clone(), None),
    ];
    let (out, _errors, changes) = receive(frames);
    assert!(out.len() >= 4, "every frame is reported");
    assert!(out[0].1.is_some());
    assert_eq!(out[2].0, 1, "the hole is counted");
    assert!(
        out[2].1.is_none(),
        "the first 5 bpp frame with a hole is never decoded from 4 bpp slices"
    );
    assert_eq!(changes, 1, "the change is seen with the decoder closed");
    let p3 = out[3].1.as_ref().expect("the next frame decodes");
    assert!(psnr(p3.y(), src5.y()) >= 35.0);
}

/// Push only the first `n` packets of a frame.
fn push_frame_prefix(
    a: &mut FrameAssembler,
    sink: &mut impl jxs_link::SliceSink,
    tc: u64,
    packets: &[Vec<u8>],
    n: usize,
) {
    let count = packets.len();
    for (i, p) in packets.iter().enumerate().take(n) {
        let base = PacketHeader {
            camera_id: 1,
            timecode_100ns: tc,
            slice_index: i as u8,
            slice_count: count as u8,
            offset: 0,
            slice_len: 0,
        };
        let mut buf = Vec::new();
        build_slice_datagrams(base, p, &mut buf).unwrap();
        for d in split_gro(&buf, Some(DATAGRAM_LEN)) {
            let h = PacketHeader::decode(d).unwrap();
            a.push(&h, &d[HEADER_LEN..], 0, sink);
        }
    }
}
