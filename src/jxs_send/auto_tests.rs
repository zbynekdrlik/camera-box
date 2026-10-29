//! Issue 1387: the automatic bpp's socket round trip and its end-to-end run through the real
//! encoder (split out of the parent module's tests to keep it under its size budget).

use super::*;

/// The status line's `handoff_age_ms` is the frame timecode's age on the wall clock when the
/// capture thread hands the frame over, for every grid frame (a busy-dropped one too), and the
/// window starts over after each status line.
#[cfg(feature = "jxs")]
#[test]
fn the_handoff_age_is_measured_on_the_wall_clock_at_the_hand_off() {
    let (h, _ready) = JxsHandoff::unstarted();
    let frame = vec![1u8; 64 * 8 * 2];
    let now = crate::jxs_recv::realtime_100ns();
    // on the 30 fps grid (emit 30), 20 ms and 40 ms old
    for age_ms in [20i64, 40, 30] {
        h.offer(&JxsFrame {
            data: &frame,
            width: 64,
            height: 8,
            stride: 128,
            yuyv: true,
            timecode_100ns: now - age_ms * 10_000,
            emit_fps: 30,
        });
    }
    assert_eq!(h.dropped_busy(), 1, "the third frame found no free picture");
    let mut w = jxs_link::SendWindow::default();
    h.stats.drain_into(&mut w);
    let (avg, lo, hi) = w.handoff_age.avg_min_max_ms().expect("three samples");
    assert_eq!(
        w.handoff_age.count, 3,
        "the busy-dropped frame is measured too"
    );
    // the offers ran within a few ms of `now`
    assert!((20.0..25.0).contains(&lo), "{lo}");
    assert!((40.0..45.0).contains(&hi), "{hi}");
    assert!((30.0..35.0).contains(&avg), "{avg}");
    assert!(w.status_line().contains(" handoff_age_ms_avg/min/max="));
    let mut again = jxs_link::SendWindow::default();
    h.stats.drain_into(&mut again);
    assert_eq!(again.handoff_age.count, 0, "a fresh window");
}

/// The receiver answers to the address the slices came from, from its listening socket; the
/// sender's connected socket takes it, ignores other cameras and garbage, and never blocks.
#[test]
fn a_receiver_report_reaches_the_sender_through_its_connected_socket() {
    let mut rx = crate::jxs_recv::JxsRecvSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    rx.set_timeout(Some(std::time::Duration::from_millis(500)))
        .unwrap();
    let port = rx.local_addr().unwrap().port();
    let tx = JxsSocket::connect(&format!("127.0.0.1:{port}")).unwrap();
    assert_eq!(
        tx.take_report(5),
        None,
        "nothing waiting: no block, no report"
    );
    tx.sock.send(&[0x4A; 20]).unwrap();
    let mut from = None;
    rx.recv_batch_from(|_, _, f| from = f).unwrap();
    let from = from.expect("the kernel names the source");
    assert_eq!(from, tx.sock.local_addr().unwrap());
    let report = |cam, seq| jxs_link::ReceiverReport {
        camera_id: cam,
        max_bpp: 0,
        seq,
        frames: 30,
        lost_slices: 0,
        late_frames: 0,
        decode_p99_us: 2_000,
        last_timecode_100ns: 42,
    };
    rx.send_report(from, &report(5, 1)).unwrap();
    rx.send_report(from, &report(9, 2)).unwrap(); // another camera
    rx.send_report(from, &report(5, 3)).unwrap();
    // the reports are on loopback already; read until the newest is in (bounded)
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut got = None;
    while got.is_none() && std::time::Instant::now() < deadline {
        got = tx.take_report(5);
    }
    assert_eq!(got, Some(report(5, 3)), "the newest one of this camera");
    assert_eq!(tx.take_report(5), None, "drained");
    // a stranger cannot reach the connected socket
    let stranger = UdpSocket::bind("127.0.0.1:0").unwrap();
    stranger
        .send_to(&report(5, 4).encode().unwrap(), from)
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(tx.take_report(5), None);
}

/// The whole auto-bpp loop through the real encoder: a receiver that reports clean seconds
/// makes the sender climb from 4 to 5 bpp, a receiver loss brings it back to 4; every frame
/// handed over arrives whole, so no bpp change costs a frame.
#[cfg(feature = "jxs")]
#[test]
fn the_auto_bpp_climbs_on_clean_reports_and_falls_on_a_loss_without_losing_a_frame() {
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};
    let mut rx = crate::jxs_recv::JxsRecvSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    rx.set_timeout(Some(Duration::from_millis(10))).unwrap();
    let port = rx.local_addr().unwrap().port();
    let (mut h, ready_rx) = JxsHandoff::unstarted();
    let socket = JxsSocket::connect(&format!("127.0.0.1:{port}")).unwrap();
    let mut cfg = crate::jxs_auto_bpp::AutoBppConfig::new(4, 5);
    cfg.up_after_s = 1;
    // a debug build: the encode-time rule is not what this test drives
    cfg.up_headroom = 100.0;
    cfg.down_fraction = 100.0;
    let opts = SenderOptions {
        auto_config: Some(cfg),
        ..SenderOptions::new(BppMode::Auto { min: 4, max: 5 }, None)
    };
    assert!(h.start_threads(ready_rx, socket, 5, vec![0, 1], opts));
    let (w, ht) = (256u32, 64u32);
    let frame: Vec<u8> = (0..w * ht * 2).map(|i| (i * 7 % 251) as u8).collect();
    let packets = jxs_link::packets_per_frame(ht);
    // per frame timecode: (datagrams, codestream bytes)
    let mut frames: BTreeMap<u64, (u32, usize)> = BTreeMap::new();
    let mut handed = Vec::new();
    let start = Instant::now();
    let mut next_frame = start;
    let mut last_report = start;
    let mut seq = 0u32;
    let mut lossy_from: Option<Instant> = None;
    let mut sender: Option<std::net::SocketAddr> = None;
    let t0 = 1_761_730_000i64 * 10_000_000;
    let mut k = 0i64;
    while start.elapsed() < Duration::from_secs(8) {
        if Instant::now() >= next_frame {
            let tc = t0 + k * 10_000_000 / 30;
            h.offer(&JxsFrame {
                data: &frame,
                width: w,
                height: ht,
                stride: w * 2,
                yuyv: true,
                timecode_100ns: tc,
                emit_fps: 30,
            });
            handed.push(tc as u64);
            k += 1;
            next_frame += Duration::from_millis(33);
        }
        rx.recv_batch_from(|d, _, from| {
            let hd = jxs_link::PacketHeader::decode(d).unwrap();
            sender = from;
            let e = frames.entry(hd.timecode_100ns).or_insert((0, 0));
            e.0 += 1;
            e.1 += d.len() - jxs_link::HEADER_LEN;
        })
        .unwrap();
        // the first time a frame of 5 bpp is seen, start reporting a loss
        let at5 = frames.values().any(|&(_, b)| b > 9_000);
        if at5 && lossy_from.is_none() {
            lossy_from = Some(Instant::now() + Duration::from_millis(1500));
        }
        if last_report.elapsed() >= Duration::from_millis(200) {
            if let Some(to) = sender {
                let lost = lossy_from.is_some_and(|t| Instant::now() >= t);
                let r = jxs_link::ReceiverReport {
                    camera_id: 5,
                    max_bpp: 0,
                    seq,
                    frames: 6,
                    lost_slices: lost as u32,
                    late_frames: 0,
                    decode_p99_us: 500,
                    last_timecode_100ns: frames.keys().next_back().copied().unwrap_or(0),
                };
                rx.send_report(to, &r).unwrap();
                seq += 1;
            }
            last_report = Instant::now();
        }
    }
    drop(h);
    // drain what is still on the wire
    for _ in 0..20 {
        rx.recv_batch_from(|d, _, _| {
            let hd = jxs_link::PacketHeader::decode(d).unwrap();
            let e = frames.entry(hd.timecode_100ns).or_insert((0, 0));
            e.0 += 1;
            e.1 += d.len() - jxs_link::HEADER_LEN;
        })
        .unwrap();
    }
    // 4 bpp of 256x64 = 8192 bytes, 5 bpp = 10240 (constant bit rate)
    let sizes: Vec<usize> = frames.values().map(|&(_, b)| b).collect();
    let first5 = sizes
        .iter()
        .position(|&b| b > 9_000)
        .expect("climbed to 5 bpp");
    assert!(sizes[..first5].iter().all(|&b| b < 9_000), "4 bpp first");
    let back4 = sizes[first5..]
        .iter()
        .position(|&b| b < 9_000)
        .expect("fell back to 4 bpp on the loss");
    assert!(back4 > 0);
    // every frame handed over arrived, whole (no frame lost at either swap)
    assert_eq!(
        frames.keys().copied().collect::<Vec<_>>(),
        handed,
        "every frame on the wire"
    );
    let datagrams_ok = frames.values().all(|&(n, _)| n >= packets);
    assert!(datagrams_ok, "every frame whole");
}
