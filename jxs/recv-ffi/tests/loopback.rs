//! Issue 1387 slice 2 -- the C ABI end to end through the real encoder: frames sent from a UDP
//! socket the way the cambox sender does come out of `cbjxs_next_frame` decoded, with the sender's
//! timecode, and the 32-byte receiver report reaches that socket (the cambox auto bpp's input).
//! The error and NULL paths of every call are pinned too.
#![cfg(target_os = "linux")]

use camera_box::jxs_link::{
    build_slice_datagrams, psnr, split_gro, PacketHeader, ReceiverReport, DATAGRAM_LEN, REPORT_LEN,
};
use jxs_recv_ffi::*;
use std::net::UdpSocket;
use std::ptr;
use std::time::{Duration, Instant};
use svt_jpegxs_sys::{Encoder, EncoderSettings, Yuv422Planes};

const W: u32 = 640;
const H: u32 = 128;

fn picture(phase: u32) -> Yuv422Planes {
    let mut p = Yuv422Planes::new(W, H);
    let (y, u, v) = p.planes_mut();
    for (i, px) in y.iter_mut().enumerate() {
        *px = (40 + ((i as u32 % W + phase) % W) * 150 / W) as u8;
    }
    for (i, px) in u.iter_mut().enumerate() {
        *px = (100 + (i as u32 / (W / 2)) % 30) as u8;
    }
    v.fill(140);
    p
}

fn send_frame(enc: &mut Encoder, tx: &UdpSocket, port: u16, timecode: u64, p: &Yuv422Planes) {
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
            camera_id: 5,
            timecode_100ns: timecode,
            slice_index: i as u8,
            slice_count: count,
            offset: 0,
            slice_len: 0,
        };
        let mut buf = Vec::new();
        build_slice_datagrams(base, pk, &mut buf).unwrap();
        for d in split_gro(&buf, Some(DATAGRAM_LEN)) {
            tx.send_to(d, ("127.0.0.1", port)).unwrap();
        }
    }
}

fn open(camera: u8, max_bpp: u8) -> *mut CbjxsReceiver {
    let mut err = [0 as std::ffi::c_char; 128];
    let rx = unsafe { cbjxs_open(0, camera, max_bpp, err.as_mut_ptr(), err.len()) };
    assert!(!rx.is_null(), "open failed");
    rx
}

#[test]
fn a_frame_sent_like_the_cambox_comes_out_of_the_c_abi_with_its_timecode_and_the_report_goes_back()
{
    assert_eq!(cbjxs_abi_version(), CBJXS_ABI_VERSION);
    let rx = open(5, 7);
    let port = unsafe { cbjxs_local_port(rx) };
    assert_ne!(port, 0);
    let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
    tx.set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let mut enc = Encoder::open(EncoderSettings::issue_1387(W, H)).unwrap();

    let base: u64 = 17_618_456_000_000_000;
    let mut frames: Vec<(i64, u8, u32, f64, f64)> = Vec::new();
    let mut report: Option<ReceiverReport> = None;
    let mut sent = 0u64;
    let deadline = Instant::now() + Duration::from_secs(15);
    while (frames.len() < 3 || report.is_none()) && Instant::now() < deadline {
        if sent < 3 {
            send_frame(
                &mut enc,
                &tx,
                port,
                base + sent * 333_333,
                &picture(sent as u32),
            );
            sent += 1;
        }
        let mut f = CbjxsFrame::default();
        let r = unsafe { cbjxs_next_frame(rx, 300, &mut f) };
        assert!(r == 0 || r == 1, "next_frame {r}");
        if r == 1 {
            assert_eq!((f.width, f.height), (W, H));
            assert_eq!((f.y_stride, f.uv_stride), (W, W / 2));
            let n = (W * H) as usize;
            let (y, u) = unsafe {
                (
                    std::slice::from_raw_parts(f.y, n),
                    std::slice::from_raw_parts(f.u, n / 2),
                )
            };
            let k = ((f.timecode_100ns as u64 - base) / 333_333) as u32;
            let original = picture(k);
            frames.push((
                f.timecode_100ns,
                f.camera_id,
                f.lost_slices,
                psnr(original.y(), y),
                psnr(original.u(), u),
            ));
            assert!(f.decoded_at_100ns >= f.last_arrival_100ns);
            assert!(f.last_arrival_100ns >= f.first_arrival_100ns);
        }
        let mut b = [0u8; 64];
        if let Ok(n) = tx.recv(&mut b) {
            if n == REPORT_LEN {
                report = ReceiverReport::decode(&b[..n]).ok();
            }
        }
    }
    assert_eq!(frames.len(), 3, "{frames:?}");
    for (i, (tc, cam, lost, py, pu)) in frames.iter().enumerate() {
        assert_eq!(*tc, (base + i as u64 * 333_333) as i64, "timecode carried");
        assert_eq!((*cam, *lost), (5, 0));
        assert!(*py >= 40.0 && *pu >= 40.0, "PSNR y {py:.2} u {pu:.2}");
    }
    let r = report.expect("the receiver report reached the sender's socket");
    assert_eq!((r.camera_id, r.max_bpp), (5, 7));

    let mut s = CbjxsStats::default();
    assert_eq!(unsafe { cbjxs_get_stats(rx, &mut s) }, 0);
    assert_eq!((s.frames, s.undecodable_frames, s.camera_id), (3, 0, 5));
    assert!(s.reports_sent >= 1 && s.bytes > 0, "{s:?}");
    let mut e = [0 as std::ffi::c_char; 32];
    assert_eq!(unsafe { cbjxs_take_error(rx, e.as_mut_ptr(), e.len()) }, 0);
    unsafe { cbjxs_close(rx) };
}

#[test]
fn an_empty_receiver_times_out_with_zero_and_never_fills_the_frame() {
    let rx = open(0, 0);
    let mut f = CbjxsFrame {
        width: 99,
        ..CbjxsFrame::default()
    };
    let t0 = Instant::now();
    assert_eq!(unsafe { cbjxs_next_frame(rx, 80, &mut f) }, 0);
    assert!(t0.elapsed() >= Duration::from_millis(60));
    assert_eq!(f.width, 99, "the frame is untouched without a frame");
    let mut s = CbjxsStats::default();
    assert_eq!(unsafe { cbjxs_get_stats(rx, &mut s) }, 0);
    assert_eq!((s.frames, s.camera_id), (0, 0));
    unsafe { cbjxs_close(rx) };
}

#[test]
fn a_port_already_in_use_fails_open_with_the_reason() {
    let taken = UdpSocket::bind("0.0.0.0:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let mut err = [0 as std::ffi::c_char; 128];
    let rx = unsafe { cbjxs_open(port, 0, 0, err.as_mut_ptr(), err.len()) };
    assert!(rx.is_null());
    let msg = unsafe { std::ffi::CStr::from_ptr(err.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    assert!(msg.contains(&port.to_string()), "{msg}");
    // a 1-byte buffer still gets its NUL, and a NULL buffer is fine
    let mut one = [7 as std::ffi::c_char; 1];
    assert!(unsafe { cbjxs_open(port, 0, 0, one.as_mut_ptr(), 1) }.is_null());
    assert_eq!(one[0], 0);
    assert!(unsafe { cbjxs_open(port, 0, 0, ptr::null_mut(), 64) }.is_null());
}

#[test]
fn every_call_accepts_null() {
    unsafe {
        assert_eq!(cbjxs_local_port(ptr::null()), 0);
        let mut f = CbjxsFrame::default();
        assert_eq!(cbjxs_next_frame(ptr::null_mut(), 0, &mut f), -2);
        let rx = open(0, 0);
        assert_eq!(cbjxs_next_frame(rx, 0, ptr::null_mut()), -2);
        let mut s = CbjxsStats::default();
        assert_eq!(cbjxs_get_stats(ptr::null(), &mut s), -1);
        assert_eq!(cbjxs_get_stats(rx, ptr::null_mut()), -1);
        assert_eq!(cbjxs_take_error(ptr::null_mut(), ptr::null_mut(), 0), 0);
        assert_eq!(cbjxs_take_error(rx, ptr::null_mut(), 8), 0);
        cbjxs_close(rx);
        cbjxs_close(ptr::null_mut());
    }
}

#[test]
fn a_zero_timeout_poll_still_receives_and_a_new_max_bpp_reaches_the_report_without_a_reopen() {
    let rx = open(5, 0);
    let port = unsafe { cbjxs_local_port(rx) };
    assert!(unsafe { cbjxs_recv_buffer_bytes(rx) } > 0);
    let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
    tx.set_nonblocking(true).unwrap();
    let mut enc = Encoder::open(EncoderSettings::issue_1387(W, H)).unwrap();
    send_frame(&mut enc, &tx, port, 17_618_456_000_000_000, &picture(0));
    // a non-blocking caller: timeout 0, never a sleep of its own beyond 1 ms
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut got = false;
    while !got && Instant::now() < deadline {
        let mut f = CbjxsFrame::default();
        got = unsafe { cbjxs_next_frame(rx, 0, &mut f) } == 1;
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(got, "a zero-timeout poll decoded the frame");

    assert_eq!(unsafe { cbjxs_set_max_bpp(rx, 6) }, 0);
    assert_eq!(unsafe { cbjxs_set_max_bpp(ptr::null_mut(), 6) }, -1);
    let mut cap = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    while cap != Some(6) && Instant::now() < deadline {
        let mut f = CbjxsFrame::default();
        assert!(unsafe { cbjxs_next_frame(rx, 100, &mut f) } >= 0);
        let mut b = [0u8; 64];
        while let Ok(n) = tx.recv(&mut b) {
            if let Ok(r) = ReceiverReport::decode(&b[..n]) {
                cap = Some(r.max_bpp);
            }
        }
    }
    assert_eq!(cap, Some(6), "the next report carries the new cap");
    assert_eq!(unsafe { cbjxs_local_port(rx) }, port, "the same socket");
    unsafe { cbjxs_close(rx) };
}
