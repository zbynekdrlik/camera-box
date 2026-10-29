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
//!
//! **The receiver report (the automatic bpp's back channel):** once a second the probe sends ONE
//! 32-byte report (`camera_box::jxs_link`, "The receiver report") back to the address the taken
//! camera's slices came from: its lost slices, its late frames (decoded more than `--late-ms`
//! after their timecode, default 100), its decode-time p99 (last datagram in -> decoded) and
//! `--max-bpp` (0 = no limit). A sender in auto mode climbs only on these reports.
//! `--no-report` sends none (the sender then holds its bpp). The status line counts the reports
//! sent (`reports=`).

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use camera_box::jxs_link;
    use camera_box::jxs_recv::{JxsReceiver, ReceiverConfig};
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
        /// The highest bpp this receiver takes (0 = no limit), sent in every report.
        #[arg(long, default_value_t = 0)]
        max_bpp: u32,
        /// A frame decoded more than this long after its timecode counts as late in the report.
        #[arg(long, default_value_t = 100.0)]
        late_ms: f64,
        /// Send no receiver report (the sender's automatic bpp then holds).
        #[arg(long)]
        no_report: bool,
    }

    let args = Args::parse();
    anyhow::ensure!(
        args.max_bpp <= u8::MAX as u32,
        "--max-bpp {} does not fit the report's byte",
        args.max_bpp
    );
    // Issue 1387 slice 2: the receive loop is the library's `JxsReceiver`, the same code the
    // strih OBS source runs through jxs/recv-ffi.
    let mut cfg = ReceiverConfig::new(args.listen);
    cfg.camera = args.camera;
    cfg.max_bpp = args.max_bpp as u8;
    cfg.late_ms = args.late_ms;
    cfg.send_reports = !args.no_report;
    cfg.window = Duration::from_secs_f64(args.window_s.max(0.001));
    let mut rx = JxsReceiver::bind(cfg)?;
    let sock = rx.socket();
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

    let mut decoded_total: u64 = 0;
    let mut dumped: u32 = 0;
    let mut rgb: Vec<u8> = Vec::new();
    let mut announced = args.camera;
    let mut header_changes_seen = 0u64;
    let started = Instant::now();
    loop {
        if let Some(f) = rx.poll(Duration::from_millis(200))? {
            decoded_total += 1;
            if dumped < args.dump_png && decoded_total > args.dump_skip {
                let p = &f.planes;
                let (w, h) = (p.width() as usize, p.height() as usize);
                rgb.resize(w * h * 3, 0);
                jxs_link::planar422_to_rgb(p.y(), p.u(), p.v(), w, h, &mut rgb);
                let png = jxs_link::png::encode_rgb8(&rgb, p.width(), p.height());
                let path = args
                    .dump_dir
                    .join(format!("jxs-cam{}-{}.png", f.camera_id, f.timecode_100ns));
                match std::fs::write(&path, png) {
                    Ok(()) => println!("jxs recv: wrote {}", path.display()),
                    Err(e) => eprintln!("jxs recv: {}: {e}", path.display()),
                }
                dumped += 1;
            }
        }
        if announced.is_none() {
            if let Some(c) = rx.camera() {
                announced = Some(c);
                println!("jxs recv: taking camera id {c} (issue 1387)");
            }
        }
        if let Some(line) = rx.take_window_line() {
            println!("{line}");
            if let Some(e) = rx.take_last_error() {
                println!("jxs recv: last error: {e} (issue 1387)");
            }
            let changes = rx.totals().header_changes;
            if changes > header_changes_seen {
                println!(
                    "jxs recv: decoder reopened at {} new picture header(s) (a sender bpp change), {} in total (issue 1387)",
                    changes - header_changes_seen,
                    changes
                );
                header_changes_seen = changes;
            }
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
