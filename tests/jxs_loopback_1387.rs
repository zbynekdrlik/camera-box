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
use svt_jpegxs_sys::{Encoder, EncoderSettings, Yuv422Planes};

const W: usize = 640;
const H: usize = 128;

/// A smooth synthetic picture (gradients + a soft circle), shifted by `phase`.
fn planar(phase: usize) -> Yuv422Planes {
    let mut p = Yuv422Planes::new(W as u32, H as u32);
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

fn to_yuyv(p: &Yuv422Planes) -> Vec<u8> {
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
        emit_fps: 60,
    });

    let mut assembler = FrameAssembler::new();
    let got: RefCell<Option<(FrameReport, Yuv422Planes)>> = RefCell::new(None);
    let mut sink = DecodeSink::new(|r: &FrameReport, p: Option<&Yuv422Planes>| {
        *got.borrow_mut() = Some((*r, p.expect("decoded").clone()));
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while got.borrow().is_none() && Instant::now() < deadline {
        rx.recv_batch(|d, at| {
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
    let pictures: Vec<Yuv422Planes> = (0..N).map(|k| planar(k * 13)).collect();
    let base: i64 = 17_618_456_000_000_000;
    let got: RefCell<Vec<(u64, Yuv422Planes)>> = RefCell::new(Vec::new());
    let mut assembler = FrameAssembler::new();
    let mut sink = DecodeSink::new(|r: &FrameReport, p: Option<&Yuv422Planes>| {
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
                emit_fps: 60,
            });
        }
        let until = Instant::now() + Duration::from_millis(150);
        while Instant::now() < until {
            rx.recv_batch(|d, at| {
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
fn encode(enc: &mut Encoder, p: &Yuv422Planes) -> Vec<Vec<u8>> {
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

    let mut out: Vec<(FrameReport, Yuv422Planes)> = Vec::new();
    {
        let mut a = FrameAssembler::new();
        let mut sink = DecodeSink::new(|r: &FrameReport, p: Option<&Yuv422Planes>| {
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
    let rows = |p: &Yuv422Planes, r0: usize, r1: usize| p.y()[r0 * W..r1 * W].to_vec();
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
fn tall(phase: u8) -> Yuv422Planes {
    let mut p = Yuv422Planes::new(320, 1080);
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
        let mut sink = DecodeSink::new(|_: &FrameReport, p: Option<&Yuv422Planes>| {
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
        let mut sink = DecodeSink::new(|r: &FrameReport, p: Option<&Yuv422Planes>| {
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
