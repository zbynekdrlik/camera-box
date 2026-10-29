//! Issue 1387 slice 2 -- `jxs_recv::JxsReceiver`, the one receive loop the probe and the strih
//! OBS source share, end to end over 127.0.0.1 with the real codec: frames come out decoded with
//! their timecode, the receiver report goes back to the address the slices came from, another
//! camera's datagrams are counted and ignored.
#![cfg(all(target_os = "linux", feature = "jxs"))]

use camera_box::jxs_link::{
    build_slice_datagrams, psnr, split_gro, PacketHeader, ReceiverReport, DATAGRAM_LEN, REPORT_LEN,
};
use camera_box::jxs_recv::{JxsReceiver, ReceiverConfig};
use std::net::UdpSocket;
use std::time::{Duration, Instant};
use svt_jpegxs_sys::{Encoder, EncoderSettings, YuvPlanes};

const W: u32 = 640;
const H: u32 = 128;

fn picture(phase: u32) -> YuvPlanes {
    let mut p = YuvPlanes::new(W, H);
    let (y, u, v) = p.planes_mut();
    for (i, px) in y.iter_mut().enumerate() {
        *px = (40 + ((i as u32 % W + phase) % W) * 150 / W) as u8;
    }
    u.fill(110);
    v.fill(140);
    p
}

/// Encode `p` and send it as camera `camera` with `timecode` from `tx` to `to`.
fn send_frame(
    enc: &mut Encoder,
    tx: &UdpSocket,
    to: std::net::SocketAddr,
    camera: u8,
    timecode: u64,
    p: &YuvPlanes,
) {
    let (y, u, v) = enc.planes_mut().planes_mut();
    y.copy_from_slice(p.y());
    u.copy_from_slice(p.u());
    v.copy_from_slice(p.v());
    let mut packets: Vec<Vec<u8>> = Vec::new();
    enc.encode_frame(|_, b, _| packets.push(b.to_vec()))
        .unwrap();
    let count = packets.len() as u8;
    for (i, pk) in packets.iter().enumerate() {
        let base = PacketHeader {
            camera_id: camera,
            timecode_100ns: timecode,
            slice_index: i as u8,
            slice_count: count,
            offset: 0,
            slice_len: 0,
        };
        let mut buf = Vec::new();
        build_slice_datagrams(base, pk, &mut buf).unwrap();
        for d in split_gro(&buf, Some(DATAGRAM_LEN)) {
            tx.send_to(d, to).unwrap();
        }
    }
}

fn receiver() -> JxsReceiver {
    let mut cfg = ReceiverConfig::new("127.0.0.1:0".parse().unwrap());
    cfg.max_bpp = 6;
    JxsReceiver::bind(cfg).unwrap()
}

#[test]
fn frames_come_out_decoded_with_their_timecode_and_the_report_goes_back_to_the_sender() {
    let mut rx = receiver();
    let to = rx.local_addr().unwrap();
    let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
    tx.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let mut enc = Encoder::open(EncoderSettings::issue_1387(W, H)).unwrap();

    let base: u64 = 17_618_456_000_000_000;
    let mut got = Vec::new();
    let mut report: Option<ReceiverReport> = None;
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut sent = 0u64;
    while (got.len() < 4 || report.is_none()) && Instant::now() < deadline {
        if sent < 4 {
            let p = picture(sent as u32 * 7);
            send_frame(&mut enc, &tx, to, 5, base + sent * 333_333, &p);
            sent += 1;
        }
        if let Some(f) = rx.poll(Duration::from_millis(300)).unwrap() {
            let original = picture(((f.timecode_100ns - base) / 333_333) as u32 * 7);
            got.push((
                f.timecode_100ns,
                f.camera_id,
                f.lost_slices,
                psnr(original.y(), f.planes.y()),
            ));
        }
        let mut b = [0u8; 64];
        if let Ok(n) = tx.recv(&mut b) {
            if n == REPORT_LEN {
                report = ReceiverReport::decode(&b[..n]).ok();
            }
        }
    }
    assert_eq!(got.len(), 4, "four frames decoded: {got:?}");
    for (i, (tc, cam, lost, py)) in got.iter().enumerate() {
        assert_eq!(*tc, base + i as u64 * 333_333, "in order, timecode carried");
        assert_eq!((*cam, *lost), (5, 0));
        assert!(*py >= 40.0, "luma PSNR {py:.2} dB");
    }
    let r = report.expect("a receiver report reached the sender's own socket");
    assert_eq!(r.camera_id, 5);
    assert_eq!(r.max_bpp, 6, "the configured cap travels in the report");
    let t = rx.totals();
    assert_eq!(t.frames, 4);
    assert_eq!(t.undecodable_frames, 0);
    assert!(t.reports_sent >= 1, "{t:?}");
    assert!(t.bytes > 0 && t.datagrams > 0);
    assert_eq!(rx.camera(), Some(5));
    assert_eq!(rx.sender(), Some(tx.local_addr().unwrap()));
}

#[test]
fn another_cameras_datagrams_are_counted_and_never_decoded() {
    let mut cfg = ReceiverConfig::new("127.0.0.1:0".parse().unwrap());
    cfg.camera = Some(3);
    let mut rx = JxsReceiver::bind(cfg).unwrap();
    let to = rx.local_addr().unwrap();
    let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut enc = Encoder::open(EncoderSettings::issue_1387(W, H)).unwrap();
    send_frame(&mut enc, &tx, to, 7, 17_618_456_000_000_000, &picture(0));
    let deadline = Instant::now() + Duration::from_secs(5);
    while rx.totals().foreign == 0 && Instant::now() < deadline {
        assert!(rx.poll(Duration::from_millis(100)).unwrap().is_none());
    }
    let t = rx.totals();
    assert!(t.foreign > 0, "{t:?}");
    assert_eq!((t.frames, t.bytes), (0, 0), "nothing of camera 7 was taken");
    assert_eq!(rx.camera(), Some(3));
}

#[test]
fn a_poll_with_nothing_to_receive_returns_none_within_its_timeout() {
    let mut rx = receiver();
    let t0 = Instant::now();
    assert!(rx.poll(Duration::from_millis(120)).unwrap().is_none());
    let took = t0.elapsed();
    assert!(
        took >= Duration::from_millis(100) && took < Duration::from_millis(1000),
        "{took:?}"
    );
}
