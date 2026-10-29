//! Issue 1387 -- the JPEG XS ENCODER cost bench (no network, no capture).
//!
//! Runs the sender's own pipelined SVT-JPEG-XS session (two pictures in flight, latency profile,
//! decomp_v 2, one slice-pack thread per core) on the given cores and prints one line per bpp:
//!
//! `jxs bench: bpp= frames= fps= dropped_busy= encode_ms_p50/p99/max= open_ms= close_ms=
//!  bytes_per_frame= rc= signs= sigf= vpred= quant= pattern= cores= fifo= (issue 1387)`
//!
//! - `encode_ms`: submit -> the frame's LAST packet out of the encoder (no send, no socket: the
//!   pure codec cost, which the live `jxs send` line's `encode_ms` also includes the send in).
//! - `open_ms` / `close_ms`: what a session (re)open and close cost (the auto-bpp re-init).
//! - `--fps 30` paces the submits like the live sender (a frame with both pictures busy is
//!   `dropped_busy`); `--fps 0` submits as fast as a picture is free (saturation).
//!
//! It touches no device and needs no root (only `--fifo` needs CAP_SYS_NICE). On a live cambox
//! it runs read-only from /dev/shm next to camera-box, never instead of it:
//! `taskset -c 0-1 /dev/shm/jxs-encode-bench --cores 0-1 --bpp 4,6,8 --frames 300`.
//!
//! Usage: `jxs-encode-bench [--width 1920] [--height 1080] [--bpp 4,6,8] [--frames 300]
//!         [--fps 30] [--cores 0-1] [--slice-threads N] [--pattern noise|smooth]
//!         [--yuv422p FILE] [--rc N] [--signs N] [--sigf N] [--vpred N] [--quant N]
//!         [--fifo PRIO]`

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use clap::Parser;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    use svt_jpegxs_sys::{open_pipelined, EncoderSettings, EncoderTuning, Picture};

    #[derive(Parser)]
    #[command(about = "Issue 1387: SVT-JPEG-XS encode cost per bpp, the sender's own session")]
    struct Args {
        #[arg(long, default_value_t = 1920)]
        width: u32,
        #[arg(long, default_value_t = 1080)]
        height: u32,
        /// Comma list of whole bpp values.
        #[arg(long, default_value = "4,6,8")]
        bpp: String,
        #[arg(long, default_value_t = 300)]
        frames: u32,
        /// Submit pace (0 = as fast as a picture is free).
        #[arg(long, default_value_t = 30.0)]
        fps: f64,
        /// cpulist the encoder threads run on (the bench pins itself there first).
        #[arg(long, default_value = "0-1")]
        cores: String,
        /// Slice-pack threads (default: one per core, at most 4, like the sender).
        #[arg(long)]
        slice_threads: Option<u32>,
        /// Synthetic content: `noise` (sensor-like detail, the worst case) or `smooth`.
        #[arg(long, default_value = "noise")]
        pattern: String,
        /// Use this planar 8-bit 4:2:2 file (ffmpeg -pix_fmt yuv422p -f rawvideo) instead.
        #[arg(long)]
        yuv422p: Option<std::path::PathBuf>,
        #[arg(long)]
        rc: Option<u32>,
        #[arg(long)]
        signs: Option<u32>,
        #[arg(long)]
        sigf: Option<u32>,
        #[arg(long)]
        vpred: Option<u32>,
        #[arg(long)]
        quant: Option<u32>,
        /// Run the bench thread (and the encoder threads it starts) SCHED_FIFO at this priority.
        #[arg(long)]
        fifo: Option<i32>,
    }

    let a = Args::parse();
    let cores = camera_box::affinity::parse_cpulist(&a.cores);
    anyhow::ensure!(!cores.is_empty(), "--cores '{}' is not a cpulist", a.cores);
    anyhow::ensure!(
        camera_box::affinity::pin_current_thread_to(&cores),
        "could not pin to {cores:?}"
    );
    if let Some(prio) = a.fifo {
        anyhow::ensure!(
            camera_box::affinity::set_current_thread_fifo_inherited(prio),
            "SCHED_FIFO {prio} refused (needs CAP_SYS_NICE)"
        );
    }
    let threads = a
        .slice_threads
        .unwrap_or_else(|| camera_box::jxs_send::slice_threads_for(&cores));
    let bpps: Vec<u32> = a
        .bpp
        .split(',')
        .map(|s| s.trim().parse::<u32>())
        .collect::<Result<_, _>>()?;
    let (w, h) = (a.width, a.height);

    // The content: one source frame, copied into every picture once.
    let mut src = Picture::new(w, h);
    {
        let (y, u, v) = src.planes_mut().planes_mut();
        if let Some(path) = &a.yuv422p {
            let raw = std::fs::read(path)?;
            let (ny, nc) = (y.len(), u.len());
            anyhow::ensure!(
                raw.len() >= ny + 2 * nc,
                "{} holds {} bytes, a {w}x{h} yuv422p frame needs {}",
                path.display(),
                raw.len(),
                ny + 2 * nc
            );
            y.copy_from_slice(&raw[..ny]);
            u.copy_from_slice(&raw[ny..ny + nc]);
            v.copy_from_slice(&raw[ny + nc..ny + 2 * nc]);
        } else {
            camera_box::jxs_bench::fill_pattern(&a.pattern, w as usize, h as usize, y, u, v)
                .map_err(anyhow::Error::msg)?;
        }
    }
    let pattern = a
        .yuv422p
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or(a.pattern.clone());
    let tuning = EncoderTuning {
        rate_control_mode: a.rc,
        coding_signs_handling: a.signs,
        coding_significance: a.sigf,
        coding_vertical_prediction: a.vpred,
        quantization: a.quant,
    };

    for bpp in bpps {
        let mut s = EncoderSettings::issue_1387(w, h).with_slice_threads(threads);
        s.bpp_numerator = bpp;
        s.tuning = tuning;
        let t_open = Instant::now();
        let (input, output) = open_pipelined(s).map_err(|e| anyhow::anyhow!("{e}"))?;
        let open_ms = t_open.elapsed().as_secs_f64() * 1e3;
        let bytes_per_frame = input.bytes_per_frame();

        // two pictures, like the sender; the receiver thread hands them back
        let (free_tx, free_rx) = mpsc::sync_channel::<Picture>(2);
        for _ in 0..2 {
            let mut p = Picture::new(w, h);
            {
                let (y, u, v) = p.planes_mut().planes_mut();
                y.copy_from_slice(src.planes().y());
                u.copy_from_slice(src.planes().u());
                v.copy_from_slice(src.planes().v());
            }
            free_tx.send(p)?;
        }
        let (sub_tx, sub_rx) = mpsc::channel::<Instant>();
        let recv_cores = cores.clone();
        let receiver = std::thread::spawn(move || {
            camera_box::affinity::pin_current_thread_to(&recv_cores);
            let mut times = Vec::new();
            while let Ok(submitted) = sub_rx.recv() {
                match output.recv_frame(|_, _, _| {}) {
                    Ok((pic, _)) => {
                        times.push(submitted.elapsed().as_secs_f64() * 1e3);
                        let _ = free_tx.send(pic);
                    }
                    Err(e) => {
                        eprintln!("jxs bench: {e}");
                        break;
                    }
                }
            }
            (times, output)
        });

        let period = if a.fps > 0.0 {
            Some(Duration::from_secs_f64(1.0 / a.fps))
        } else {
            None
        };
        let start = Instant::now();
        let mut dropped = 0u64;
        for i in 0..a.frames {
            let pic = if let Some(p) = period {
                let due = start + p * i;
                if let Some(wait) = due.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
                match free_rx.try_recv() {
                    Ok(pic) => pic,
                    Err(_) => {
                        dropped += 1;
                        continue;
                    }
                }
            } else {
                free_rx.recv()?
            };
            let t = Instant::now();
            if let Err((_, e)) = input.submit(pic) {
                anyhow::bail!("submit: {e}");
            }
            sub_tx.send(t)?;
        }
        drop(sub_tx);
        let (mut times, output) = receiver
            .join()
            .map_err(|_| anyhow::anyhow!("receiver panicked"))?;
        let elapsed = start.elapsed().as_secs_f64();
        let t_close = Instant::now();
        drop(input);
        drop(output);
        let close_ms = t_close.elapsed().as_secs_f64() * 1e3;
        let (p50, p99, max) = camera_box::jxs_bench::p50_p99_max(&mut times);
        println!(
            "jxs bench: bpp={bpp} frames={} fps={:.1} dropped_busy={dropped} encode_ms_p50/p99/max={p50:.2}/{p99:.2}/{max:.2} open_ms={open_ms:.1} close_ms={close_ms:.1} bytes_per_frame={bytes_per_frame} rc={} signs={} sigf={} vpred={} quant={} pattern={pattern} cores={cores:?} slice_threads={threads} fifo={} (issue 1387)",
            times.len(),
            times.len() as f64 / elapsed,
            knob(a.rc),
            knob(a.signs),
            knob(a.sigf),
            knob(a.vpred),
            knob(a.quant),
            a.fifo.map(|p| p.to_string()).unwrap_or_else(|| "off".into()),
        );
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn knob(v: Option<u32>) -> String {
    v.map(|v| v.to_string()).unwrap_or_else(|| "default".into())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("jxs-encode-bench runs on Linux only (issue 1387)");
}
