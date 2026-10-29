//! Issue 1387 -- the JPEG XS link's receive/measure tool (slice 1: no OBS change yet).
//!
//! Listens on one UDP port for a cambox's JPEG XS link (`CAMERA_BOX_JXS_TARGET` on the cambox
//! points here), reassembles slice by slice, decodes slice-wise, conceals a lost slice from the
//! previous frame (counted), and prints one line per window:
//!
//! `jxs recv: fps= frames= concealed= lost_slices= undecodable= datagrams= mbps=
//!  latency_ms_p50/p99/max= first_arrival_ms_p50/max= spread_ms_p50/max= dup= stale= malformed=
//!  resyncs= (issue 1387)`
//!
//! - `latency_ms`: the sender's genlock timecode (the frame's capture instant on the grid) to the
//!   frame decoded here, both on the dantesync-disciplined wall clock.
//! - `first_arrival_ms`: the timecode to the frame's first datagram on this host's wire (kernel
//!   receive timestamp), `spread_ms`: the frame's first to last datagram.
//!
//! Usage: `jxs-recv-probe [--listen 0.0.0.0:7000] [--camera N] [--seconds 0] [--window-s 5]
//!         [--dump-png N --dump-dir DIR [--dump-skip S]]`
//!
//! `--dump-png` writes N decoded frames (after skipping S) as `<dir>/jxs-<camera>-<timecode>.png`
//! (BT.709 limited range -> RGB), for the pixel comparison with the same frame over NDI.

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use camera_box::jxs_link::{self, FrameAssembler, PacketHeader, PushOutcome, RecvWindow};
    use camera_box::jxs_recv::{realtime_100ns, DecodeSink, JxsRecvSocket};
    use clap::Parser;
    use std::time::{Duration, Instant};

    #[derive(Parser)]
    #[command(about = "Issue 1387: receive + measure a cambox JPEG XS link")]
    struct Args {
        #[arg(long, default_value = "0.0.0.0:7000")]
        listen: std::net::SocketAddr,
        /// Stop after this many seconds (0 = run until killed).
        #[arg(long, default_value_t = 0)]
        seconds: u64,
        #[arg(long, default_value_t = 5.0)]
        window_s: f64,
        /// Take only this camera id (the sender's CAM<N>); default = the first one heard.
        #[arg(long)]
        camera: Option<u8>,
        /// Write this many decoded frames as PNG.
        #[arg(long, default_value_t = 0)]
        dump_png: u32,
        #[arg(long, default_value = ".")]
        dump_dir: std::path::PathBuf,
        /// Decoded frames to skip before the first PNG.
        #[arg(long, default_value_t = 0)]
        dump_skip: u64,
    }

    let args = Args::parse();
    let mut sock = JxsRecvSocket::bind(args.listen)?;
    sock.set_timeout(Some(Duration::from_millis(200)))?;
    let (gro, ts) = sock.features();
    println!(
        "jxs recv: listening on {} (GRO {}, kernel timestamps {}, receive buffer {} bytes) (issue 1387)",
        sock.local_addr()?,
        if gro { "on" } else { "OFF" },
        if ts { "on" } else { "OFF" },
        sock.recv_buffer()
    );
    if args.dump_png > 0 {
        std::fs::create_dir_all(&args.dump_dir)?;
    }

    let mut window = RecvWindow::default();
    let mut assembler = FrameAssembler::new();
    let mut decoded_total: u64 = 0;
    let mut dumped: u32 = 0;
    let mut rgb: Vec<u8> = Vec::new();
    let dump_dir = args.dump_dir.clone();
    let (dump_png, dump_skip) = (args.dump_png, args.dump_skip);
    // Filled by the decoder sink, drained by the loop (both on this thread).
    let frame_events: std::cell::RefCell<Vec<(jxs_link::FrameReport, i64, bool)>> =
        std::cell::RefCell::new(Vec::with_capacity(16));
    let mut sink = DecodeSink::new(|report, planes| {
        let decoded_at = realtime_100ns();
        frame_events
            .borrow_mut()
            .push((*report, decoded_at, planes.is_some()));
        if let Some(p) = planes {
            decoded_total += 1;
            if dumped < dump_png && decoded_total > dump_skip {
                let (w, h) = (p.width() as usize, p.height() as usize);
                rgb.resize(w * h * 3, 0);
                jxs_link::planar422_to_rgb(p.y(), p.u(), p.v(), w, h, &mut rgb);
                let png = jxs_link::png::encode_rgb8(&rgb, p.width(), p.height());
                let path = dump_dir.join(format!(
                    "jxs-cam{}-{}.png",
                    report.camera_id, report.timecode
                ));
                match std::fs::write(&path, png) {
                    Ok(()) => println!("jxs recv: wrote {}", path.display()),
                    Err(e) => eprintln!("jxs recv: {}: {e}", path.display()),
                }
                dumped += 1;
            }
        }
    });

    // One assembler serves one sender: every camera stamps the same genlock grid.
    let mut camera = args.camera;
    let started = Instant::now();
    let mut window_start = Instant::now();
    loop {
        sock.recv_batch(|d, arrival| {
            window.datagrams += 1;
            let Ok(h) = PacketHeader::decode(d) else {
                window.malformed += 1;
                return;
            };
            match camera {
                None => {
                    camera = Some(h.camera_id);
                    println!("jxs recv: taking camera id {} (issue 1387)", h.camera_id);
                }
                Some(c) if c != h.camera_id => {
                    window.foreign += 1;
                    return;
                }
                Some(_) => {}
            }
            window.bytes += (d.len() - jxs_link::HEADER_LEN) as u64;
            match assembler.push(&h, &d[jxs_link::HEADER_LEN..], arrival, &mut sink) {
                PushOutcome::Accepted => {}
                PushOutcome::Duplicate => window.duplicate += 1,
                PushOutcome::Stale => window.stale += 1,
                PushOutcome::Malformed => window.malformed += 1,
                PushOutcome::Resync => window.resyncs += 1,
            }
        })?;
        for (r, decoded_at, ok) in frame_events.borrow_mut().drain(..) {
            if ok {
                let ms = |t: i64| t as f64 / 10_000.0;
                let tc = r.timecode as i64;
                window.note_frame(
                    ms(decoded_at - tc),
                    ms(r.first_arrival - tc),
                    ms(r.last_arrival - r.first_arrival),
                    r.lost_slices,
                );
            } else {
                window.undecodable_frames += 1;
                window.lost_slices += r.lost_slices as u64;
            }
        }
        let el = window_start.elapsed().as_secs_f64();
        if el >= args.window_s {
            println!("{}", window.status_line(el));
            if let Some(e) = sink.last_error.take() {
                println!("jxs recv: last decoder error: {e} (issue 1387)");
            }
            window.reset();
            window_start = Instant::now();
        }
        if args.seconds > 0 && started.elapsed().as_secs() >= args.seconds {
            return Ok(());
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("jxs-recv-probe runs on Linux only (issue 1387)");
    std::process::exit(2);
}
