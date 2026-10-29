//! Issue 1387 slice 3 -- the strih program link end to end through BOTH C ABIs: 4:2:0 pictures
//! (NV12 as the OBS canvas hands them, and planar I420) go into `cbjxs_send_frame` (the real
//! sender: copy, the real SVT-JPEG-XS encoder, UDP GSO on loopback) and come out of
//! `cbjxs_next_frame` (the slice-2 receiver) decoded as 4:2:0, with their timecode, 0 lost, close
//! to the original; the receiver's report reaches the sender (its auto bpp's input) and the
//! sender's status window counts it.
#![cfg(target_os = "linux")]

use camera_box::jxs_link::psnr;
use jxs_recv_ffi::jxs_send_ffi::*;
use jxs_recv_ffi::*;
use std::ffi::{c_char, CString};
use std::time::{Duration, Instant};

const W: u32 = 1920;
const H: u32 = 1080;

/// A 4:2:0 test picture: a luma ramp moving with `phase`, smooth distinct U and V fields. Planes
/// are padded (`stride` > row) like an OBS frame can be.
struct Pic420 {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

fn pic420(phase: u32) -> Pic420 {
    let (w, h, cw, ch) = (W as usize, H as usize, W as usize / 2, H as usize / 2);
    let y = (0..w * h)
        .map(|i| (32 + ((i % w + phase as usize * 7) % w) * 180 / w) as u8)
        .collect();
    let u = (0..cw * ch)
        .map(|i| (80 + (i / cw) * 60 / ch) as u8)
        .collect();
    let v = (0..cw * ch)
        .map(|i| (100 + (i % cw) * 50 / cw) as u8)
        .collect();
    Pic420 { y, u, v }
}

/// The picture laid out as NV12 with padded rows (Y stride 2048, UV stride 2048).
fn nv12(p: &Pic420) -> (Vec<u8>, Vec<u8>) {
    let (w, h, cw, ch, stride) = (
        W as usize,
        H as usize,
        W as usize / 2,
        H as usize / 2,
        2048usize,
    );
    let mut y = vec![0xEEu8; stride * h];
    for r in 0..h {
        y[r * stride..r * stride + w].copy_from_slice(&p.y[r * w..(r + 1) * w]);
    }
    let mut uv = vec![0xEEu8; stride * ch];
    for r in 0..ch {
        for c in 0..cw {
            uv[r * stride + 2 * c] = p.u[r * cw + c];
            uv[r * stride + 2 * c + 1] = p.v[r * cw + c];
        }
    }
    (y, uv)
}

fn open_sender(port: u16) -> *mut CbjxsSender {
    let target = CString::new(format!("127.0.0.1:{port}")).unwrap();
    let cfg = CbjxsSendConfig {
        target: target.as_ptr(),
        cores: std::ptr::null(),
        emit_fps: 30,
        stream_id: 100,
        bpp_min: 0,
        bpp_max: 0,
        reserved: 0,
    };
    let mut err = [0 as c_char; 160];
    let tx = unsafe { cbjxs_send_open(&cfg, err.as_mut_ptr(), err.len()) };
    assert!(
        !tx.is_null(),
        "open: {}",
        unsafe { std::ffi::CStr::from_ptr(err.as_ptr()) }.to_string_lossy()
    );
    tx
}

#[test]
fn program_pictures_in_420_cross_both_c_abis_with_their_timecode_and_nothing_lost() {
    assert_eq!(cbjxs_send_abi_version(), CBJXS_SEND_ABI_VERSION);
    let mut err = [0 as c_char; 128];
    // the program stream's id, 100: outside the CAM range
    let rx = unsafe { cbjxs_open(0, 100, 0, err.as_mut_ptr(), err.len()) };
    assert!(!rx.is_null());
    let port = unsafe { cbjxs_local_port(rx) };
    let tx = open_sender(port);

    // a 30 fps grid in second 1_761_730_000 (29.10.2026)
    let base: i64 = 1_761_730_000 * 10_000_000;
    let tc = |k: i64| base + k * 10_000_000 / 30;
    const N: i64 = 12;
    let originals: Vec<Pic420> = (0..N as u32).map(pic420).collect();
    let mut got: Vec<(i64, u32, u8, f64, f64, f64)> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut k = 0i64;
    let mut next_send = Instant::now();
    while got.len() < N as usize && Instant::now() < deadline {
        if k < N && Instant::now() >= next_send {
            let p = &originals[k as usize];
            // even frames NV12 (the OBS canvas), odd frames planar I420
            let (y, uv) = nv12(p);
            let pic = if k % 2 == 0 {
                CbjxsSendPicture {
                    format: CBJXS_SEND_NV12,
                    width: W,
                    height: H,
                    reserved: 0,
                    planes: [y.as_ptr(), uv.as_ptr(), std::ptr::null()],
                    strides: [2048, 2048, 0],
                    reserved2: 0,
                    timecode_100ns: tc(k),
                }
            } else {
                CbjxsSendPicture {
                    format: CBJXS_SEND_I420,
                    width: W,
                    height: H,
                    reserved: 0,
                    planes: [p.y.as_ptr(), p.u.as_ptr(), p.v.as_ptr()],
                    strides: [W, W / 2, W / 2],
                    reserved2: 0,
                    timecode_100ns: tc(k),
                }
            };
            let r = unsafe { cbjxs_send_frame(tx, &pic) };
            if r == 1 {
                k += 1;
                // the planes were copied: the caller's buffers may go at once
                drop((y, uv));
                next_send = Instant::now() + Duration::from_millis(40);
            } else {
                assert_eq!(r, 0, "frame {k}: only a busy hand-off may refuse");
                next_send = Instant::now() + Duration::from_millis(5);
            }
        }
        let mut f = CbjxsFrame::default();
        let r = unsafe { cbjxs_next_frame(rx, 20, &mut f) };
        assert!(r == 0 || r == 1, "next_frame {r}");
        if r == 1 {
            assert_eq!((f.width, f.height), (W, H));
            assert_eq!(f.chroma, CBJXS_CHROMA_420, "the program arrives as 4:2:0");
            assert_eq!((f.y_stride, f.uv_stride), (W, W / 2));
            // the slot of the stamp (rounded: a 30 fps slot is 333_333.3 x 100 ns)
            let i = (((f.timecode_100ns - base) * 30 + 5_000_000) / 10_000_000) as usize;
            let o = &originals[i];
            let (n, c) = ((W * H) as usize, (W * H / 4) as usize);
            let (y, u, v) = unsafe {
                (
                    std::slice::from_raw_parts(f.y, n),
                    std::slice::from_raw_parts(f.u, c),
                    std::slice::from_raw_parts(f.v, c),
                )
            };
            got.push((
                f.timecode_100ns,
                f.lost_slices,
                f.camera_id,
                psnr(&o.y, y),
                psnr(&o.u, u),
                psnr(&o.v, v),
            ));
        }
    }
    assert_eq!(got.len(), N as usize, "every picture arrived: {got:?}");
    for (i, (t, lost, cam, py, pu, pv)) in got.iter().enumerate() {
        assert_eq!(*t, tc(i as i64), "the timecode is carried, in order");
        assert_eq!(
            (*lost, *cam),
            (0, 100),
            "0 lost slices, the program's stream id"
        );
        assert!(
            *py >= 40.0 && *pu >= 40.0 && *pv >= 40.0,
            "frame {i}: PSNR y {py:.1} u {pu:.1} v {pv:.1}"
        );
    }
    let mut s = CbjxsStats::default();
    assert_eq!(unsafe { cbjxs_get_stats(rx, &mut s) }, 0);
    assert_eq!(
        (s.frames, s.lost_slices, s.undecodable_frames),
        (N as u64, 0, 0)
    );

    // the receiver reports once a second; the sender's next status window counts them
    let mut st = CbjxsSendStats::default();
    let wait = Instant::now() + Duration::from_secs(12);
    let mut reports = 0;
    let mut sent = 0;
    while Instant::now() < wait && reports == 0 {
        let mut f = CbjxsFrame::default();
        unsafe { cbjxs_next_frame(rx, 100, &mut f) };
        if unsafe { cbjxs_send_get_stats(tx, &mut st) } == 1 {
            reports += st.reports;
            sent += st.frames;
            assert_eq!(st.auto_bpp, 1);
            assert_eq!((st.dropped_busy, st.encode_errors), (0, 0), "{st:?}");
        }
    }
    assert!(
        reports > 0,
        "the receiver's report reached the sender: {st:?}"
    );
    assert!(sent > 0, "the window counted the frames: {st:?}");
    assert_eq!(
        unsafe { cbjxs_send_get_stats(tx, &mut st) },
        0,
        "each window once"
    );
    unsafe {
        cbjxs_send_close(tx);
        cbjxs_close(rx);
    }
}

#[test]
fn a_picture_the_format_cannot_take_is_refused_and_named() {
    let tx = open_sender(9);
    let buf = vec![0u8; 64 * 64];
    let mut pic = CbjxsSendPicture {
        format: CBJXS_SEND_I420,
        width: 64,
        height: 63,
        reserved: 0,
        planes: [buf.as_ptr(); 3],
        strides: [64, 32, 32],
        reserved2: 0,
        timecode_100ns: 0,
    };
    assert_eq!(unsafe { cbjxs_send_frame(tx, &pic) }, -1);
    let mut e = [0 as c_char; 128];
    let n = unsafe { cbjxs_send_take_error(tx, e.as_mut_ptr(), e.len()) };
    let msg = unsafe { std::ffi::CStr::from_ptr(e.as_ptr()) }.to_string_lossy();
    assert!(n > 0 && msg.contains("does not fit I420"), "{msg}");
    assert_eq!(
        unsafe { cbjxs_send_take_error(tx, e.as_mut_ptr(), e.len()) },
        0
    );
    pic.height = 64;
    pic.format = 77;
    assert_eq!(unsafe { cbjxs_send_frame(tx, &pic) }, -1);
    assert_eq!(unsafe { cbjxs_send_frame(std::ptr::null_mut(), &pic) }, -2);
    assert_eq!(unsafe { cbjxs_send_frame(tx, std::ptr::null()) }, -2);
    unsafe { cbjxs_send_close(tx) };
    unsafe { cbjxs_send_close(std::ptr::null_mut()) };
}
