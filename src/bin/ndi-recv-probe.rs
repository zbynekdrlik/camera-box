//! #797 — standalone NDI receive-rate probe (production-safe: pure receiver process, no
//! framebuffer, no service, no OBS interaction). Discriminates whether the venue "50 of 60fps"
//! delivery cap lives in the libndi↔Linux-host pair (this probe ALSO gets ~50) or in the
//! DistroAV/OBS receive integration (this probe gets 60 while OBS gets 50).
//!
//! Usage: ndi-recv-probe "CAM7 (usb)" [secs]   (default 30 s; prints a rate line every 5 s)
//!
//! Uses the SAME battle-tested recv FFI as the cambox interkom display (ndi::NdiReceiver:
//! finder → recv_create_v3 at HIGHEST bandwidth → recv_capture_v3), so a rate difference vs
//! OBS cannot be blamed on exotic receiver settings.

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let mut args = std::env::args().skip(1);
    let source = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: ndi-recv-probe \"<NDI source name>\" [secs]"))?;
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(30);

    let hwaccel = std::env::args().any(|a| a == "--hwaccel");
    let mut rx = camera_box::ndi::NdiReceiver::connect(&source, 15)?;
    if hwaccel {
        rx.send_metadata("<ndi_video_codec type=\"hardware\"/>")?;
        println!("hw-accel metadata SENT (DistroAV-identical)");
    }
    println!("connected to '{source}' — measuring {secs}s...");

    let t0 = std::time::Instant::now();
    let mut window_start = std::time::Instant::now();
    let mut window_frames: u64 = 0;
    let mut total_frames: u64 = 0;
    let mut first_frame_at: Option<f64> = None;
    while t0.elapsed().as_secs() < secs {
        if rx.capture_frame(100)?.is_some() {
            window_frames += 1;
            total_frames += 1;
            if first_frame_at.is_none() {
                first_frame_at = Some(t0.elapsed().as_secs_f64());
            }
        }
        let w = window_start.elapsed().as_secs_f64();
        if w >= 5.0 {
            println!("rate: {:.1} fps ({window_frames} frames / {w:.2}s)", window_frames as f64 / w);
            window_frames = 0;
            window_start = std::time::Instant::now();
        }
    }
    let active = t0.elapsed().as_secs_f64() - first_frame_at.unwrap_or(0.0);
    println!(
        "TOTAL: {total_frames} frames in {active:.2}s active = {:.2} fps",
        total_frames as f64 / active.max(0.001)
    );
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("linux only");
}
