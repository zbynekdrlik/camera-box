use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::signal;
use tracing_subscriber::EnvFilter;

use camera_box::capture::VideoCapture;
use camera_box::config::{Config, DisplayConfig};
use camera_box::intercom;
use camera_box::ndi::NdiSender;
use camera_box::ndi_display::{self, NdiDisplayConfig};
#[cfg(feature = "probe")]
use camera_box::probe::genlock::SubmitError;

/// #275b — one emitted frame's async cam1-burn work, handed capture thread → burn thread over
/// the bounded ring ([`camera_box::probe::genlock::BurnRing`]). `buf` is an OWNED copy of the
/// captured frame (the zero-copy mmap is only valid in the capture callback); all identity
/// fields are stamped on the CAPTURE thread at the genlock emit-gate instant — the monotonic
/// burn `frame_id`, the emit-instant `gen_ts_ns`, and `emit_timecode_100ns` (the EMITTED frame's
/// genlock boundary timecode) — so the burn thread re-derives NONE of them. The burn thread
/// renders the QR into `buf` and NDI-sends it with the carried timecode.
#[cfg(feature = "probe")]
struct BurnJob {
    buf: Vec<u8>,
    info: camera_box::capture::FrameInfo,
    run_id: u32,
    frame_id: u32,
    gen_ts_ns: i64,
    emit_timecode_100ns: i64,
    /// #279 FIX 3 — render the QR into `buf` before sending? `true` for a YUYV frame (the normal
    /// burn); `false` for a non-YUYV frame the v4l2 driver substituted — sent UNBURNED so a format
    /// substitution can never kill the cam1 feed (the QR burner assumes the YUYV byte layout).
    render_qr: bool,
}

/// Wall-clock time in ns (CLOCK_REALTIME — the DanteSync-disciplined clock the
/// cluster genlock aligns to). Used by the genlock decimation gate.
fn wall_clock_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe {
        libc::clock_gettime(libc::CLOCK_REALTIME, &mut ts);
    }
    (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64)
}

/// #286 — monotonic clock time in ns (`CLOCK_MONOTONIC`, boot-relative). This is the same
/// clock domain the V4L2 UVC driver stamps on a dequeued capture buffer by default (the
/// `TIMESTAMP_MONOTONIC` flag), so sampling it back-to-back with [`wall_clock_ns`] gives the
/// monotonic->realtime offset the capture-based genlock stamp needs
/// ([`sample_mono_to_real_offset_100ns`]). Mirrors `wall_clock_ns`'s exact
/// `clock_gettime` pattern.
fn monotonic_clock_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
    }
    (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64)
}

/// #286 — sample `realtime_now - monotonic_now` in 100ns units
/// (`genlock_stamp::capture_realtime_100ns`'s `mono_to_real_offset_100ns` input). The two
/// `clock_gettime` calls are made back-to-back so the (sub-microsecond) gap between reading
/// each clock is negligible against the 100ns stamp granularity. Re-sampled periodically by
/// the capture loop (see [`camera_box::genlock_stamp::should_resample_mono_to_real_offset`])
/// so a realtime clock step/slew (e.g. DanteSync/NTP correction) cannot skew the stamp.
fn sample_mono_to_real_offset_100ns() -> i64 {
    let real_100ns = (wall_clock_ns() / 100) as i64;
    let mono_100ns = (monotonic_clock_ns() / 100) as i64;
    real_100ns - mono_100ns
}

/// #685 follow-up (live-discovered 2026-07-11, deploying to cam1): the box's OS-level
/// hostname, via the `gethostname(2)` syscall — the SAME live value the `hostname` shell
/// command reports. Used to resolve the grabber model (see the call site in
/// `run_capture_loop`). The deployed fleet's `config.toml` does NOT set the optional
/// `hostname` field (confirmed live: it silently defaulted to the generic `"camera-box"`
/// string on cam1, so `capture_rate_health::grabber_model_for_hostname` could never match
/// a real CAM1-6 name and permanently fell back to the strict 1% tolerance — the exact
/// false-positive #685 set out to eliminate; cam1 was still self-heal-resetting on its own
/// normal ~61fps wobble minutes after the #685 binary was deployed, until this follow-up).
/// The OS-level hostname IS set correctly per-box by `scripts/setup-device.sh` (confirmed
/// live: cam1 -> `"CAM1"`), so query it directly instead of trusting the unpopulated
/// app-config field. Returns an empty string on any syscall failure (never panics) — the
/// caller falls back to `GrabberModel::Unknown` (the safe strict tolerance), same as an
/// unrecognized hostname.
fn os_hostname() -> String {
    // HOST_NAME_MAX is 64 on Linux; 256 leaves generous headroom without any real cost.
    let mut buf = [0u8; 256];
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if rc != 0 {
        return String::new();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// Apply real-time optimizations to the current thread for lowest latency
/// Based on media-bridge's extreme low-latency settings
fn apply_realtime_optimizations() {
    // 1. Set real-time SCHED_FIFO scheduling with high priority
    apply_realtime_scheduling();

    // 2. Lock all memory to prevent page faults
    apply_memory_locking();

    // 3. Set CPU affinity (optional - pin to core 1)
    apply_cpu_affinity();
}

/// Set SCHED_FIFO real-time scheduling with priority 90
fn apply_realtime_scheduling() {
    unsafe {
        let param = libc::sched_param { sched_priority: 90 };
        let result = libc::sched_setscheduler(0, libc::SCHED_FIFO, &param);

        if result == 0 {
            tracing::info!("Real-time SCHED_FIFO priority 90 enabled");
        } else {
            tracing::warn!(
                "Could not set real-time priority (need CAP_SYS_NICE). \
                Run: sudo setcap 'cap_sys_nice,cap_ipc_lock+ep' /usr/local/bin/camera-box"
            );
        }
    }
}

/// Lock all memory to prevent page faults during capture
fn apply_memory_locking() {
    unsafe {
        // MCL_CURRENT: Lock all pages currently mapped
        // MCL_FUTURE: Lock all pages that will be mapped in the future
        let result = libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE);

        if result == 0 {
            tracing::info!("Memory locked (mlockall) - no page faults possible");
        } else {
            tracing::warn!(
                "Could not lock memory (need CAP_IPC_LOCK). \
                Run: sudo setcap 'cap_sys_nice,cap_ipc_lock+ep' /usr/local/bin/camera-box"
            );
        }
    }
}

/// Pin the capture + NDI-emit hot thread to the isolated core (#289) so the
/// SCHED_FIFO grab runs ALONE on the `isolcpus`-reserved core, immune to the box
/// load on the general cores (USB kworkers, rsyslogd, ssh, the QR painter, ...).
/// The isolated core is derived from `/sys` — never hardcoded; see [`camera_box::affinity`].
fn apply_cpu_affinity() {
    camera_box::affinity::pin_capture_thread();
}

/// #528: fleet-wide default HDMI cameraman-preview source. camboxes have no keyboard/mouse and
/// the preview monitor gets physically moved between cameras during an event, so a per-box
/// static source table (the earlier #556 design) cannot work — every box previews the SAME
/// source unconditionally, and the existing ~1s DRM-connector poll (`ndi_display`) already
/// delivers "plug in -> shows preview, unplug -> stops, move the monitor to another box -> that
/// box shows it" for free. This is the interkom/return/talkback monitor cam1/cam2 already
/// showed before #556 — NOT the cameraman Multiview camera grid (owner correction, 2026-07-08).
const DEFAULT_DISPLAY_SOURCE: &str = "STRIH-SNV (interkom)";

/// #528: E2E-harness opt-out. When this env var is set (to any value), the display thread does
/// not start at all, freeing `/dev/fb0` for the QR painter (`scripts/rig-mode.sh test`). This
/// replaces the old "run camera-box with no --display flag" toggle now that the preview is
/// unconditional (a bare ExecStart used to mean no display thread at all; it no longer does).
const NO_DISPLAY_ENV: &str = "CAMERA_BOX_NO_DISPLAY";

/// #528: resolve which NDI display config (if any) camera-box should run this launch, given the
/// CLI `--display`/`--fb-device` flags, the optional `config.toml [display]` section, and the
/// E2E harness's `CAMERA_BOX_NO_DISPLAY` opt-out (checked first — it must win over everything
/// else so `rig-mode.sh test` can reliably free `/dev/fb0` for the QR painter). Precedence below
/// that mirrors the pre-existing CLI-overrides-config rule. Pure + unit-tested so the "no
/// --display flag and no [display] section" case is provably covered (the exact fleet gap #528
/// reported: cam1 had neither, so this function used to return `None` — no preview at all).
fn resolve_display_config(
    cli_source: Option<&str>,
    cli_fb_device: &str,
    config_display: Option<&DisplayConfig>,
    no_display_opt_out: bool,
) -> Option<NdiDisplayConfig> {
    if no_display_opt_out {
        return None;
    }
    if let Some(source) = cli_source {
        return Some(NdiDisplayConfig {
            source_name: source.to_string(),
            fb_device: cli_fb_device.to_string(),
            find_timeout_secs: 30,
        });
    }
    if let Some(display) = config_display {
        return Some(NdiDisplayConfig {
            source_name: display.source.clone(),
            fb_device: display.fb_device.clone(),
            find_timeout_secs: 30,
        });
    }
    // #528: no CLI flag, no config.toml [display] section — every cambox still previews the
    // fleet-wide default (the exact case that used to return None, the reported bug).
    Some(NdiDisplayConfig {
        source_name: DEFAULT_DISPLAY_SOURCE.to_string(),
        fb_device: cli_fb_device.to_string(),
        find_timeout_secs: 30,
    })
}

/// Simple USB video capture to NDI streaming appliance
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to configuration file
    #[arg(short, long, default_value = "/etc/camera-box/config.toml")]
    config: PathBuf,

    /// Override video device path
    #[arg(short, long)]
    device: Option<String>,

    /// NDI source to display on HDMI (e.g., "STRIH-SNV (interkom)")
    #[arg(long = "display")]
    display_source: Option<String>,

    /// Framebuffer device for display output
    #[arg(long, default_value = "/dev/fb0")]
    fb_device: String,

    /// Enable debug logging
    #[arg(long)]
    debug: bool,

    /// Enable VBAN intercom (stream name, e.g., "cam1")
    #[arg(long = "intercom")]
    intercom_stream: Option<String>,

    /// VBAN intercom target host (default: strih.lan)
    #[arg(long, default_value = "strih.lan")]
    intercom_target: String,

    /// #105 node 2 — cam1 GRAB-RECORD (TEST mode, OFF by default). Stream the gray8
    /// luma of each EMITTED frame to `tcp://HOST:PORT` (dev1, which encodes ffv1) so
    /// the recording-based 4-node verdict can decode cam1's grab WITHOUT an NDI tap.
    /// Normal operation is unaffected when unset; pulls in no decode deps.
    #[arg(long)]
    record_grab: Option<String>,

    /// #105 node 2 — path for the cam1 grab-timestamp sidecar CSV
    /// (`frame_index,grab_ts_ns`, CLOCK_REALTIME) written alongside --record-grab.
    /// Required when --record-grab is set; scp it back to dev1 after the run.
    #[arg(long, default_value = "/tmp/cam1-grab-ts.csv")]
    record_grab_ts: String,

    /// #289 — internal helper: route the USB capture-controller IRQ(s) onto the
    /// isolated core (writes /proc/irq/<n>/smp_affinity, needs root) then exit.
    /// Invoked by the systemd unit's ExecStartPre — not for interactive use.
    #[arg(long = "setup-irq-affinity")]
    setup_irq_affinity: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging
    let filter = if args.debug {
        EnvFilter::new("camera_box=debug,grafton_ndi=debug")
    } else {
        EnvFilter::new("camera_box=info")
    };
    tracing_subscriber::fmt().with_env_filter(filter).init();

    // #289 — ExecStartPre helper: route the USB capture IRQ(s) onto the isolated
    // core, then exit (a oneshot before the main service process starts).
    if args.setup_irq_affinity {
        camera_box::affinity::setup_irq_affinity();
        return Ok(());
    }

    tracing::info!("camera-box starting...");

    // Load configuration
    let config = Config::load(&args.config)?;
    tracing::info!("Hostname: {}", config.hostname);

    // Determine device path
    let device_path = if let Some(ref device) = args.device {
        device.clone()
    } else {
        config.device_path()?
    };

    // Determine display source (CLI overrides config overrides the #528 fleet-wide default).
    // The E2E harness opts out entirely via CAMERA_BOX_NO_DISPLAY (rig-mode.sh test), freeing
    // /dev/fb0 for the QR painter.
    let no_display_opt_out = std::env::var_os(NO_DISPLAY_ENV).is_some();
    if no_display_opt_out {
        tracing::info!(
            "NDI display disabled via {} (E2E harness mode)",
            NO_DISPLAY_ENV
        );
    }
    let display_config = resolve_display_config(
        args.display_source.as_deref(),
        &args.fb_device,
        config.display.as_ref(),
        no_display_opt_out,
    );

    // Determine intercom config (CLI overrides config)
    let intercom_config = if let Some(ref stream) = args.intercom_stream {
        Some(intercom::IntercomConfig {
            stream_name: stream.clone(),
            target_host: args.intercom_target.clone(),
            sample_rate: 48000,
            channels: 2,
            sidetone_gain: 100.0,
            mic_gain: 12.0,       // +22dB boost for outbound mic
            headphone_gain: 15.0, // Headphone volume from network
            limiter_enabled: true,
            limiter_threshold: 0.5, // -6dB ceiling
        })
    } else {
        config.intercom.as_ref().map(|ic| intercom::IntercomConfig {
            stream_name: ic.stream.clone(),
            target_host: ic.target.clone(),
            sample_rate: ic.sample_rate,
            channels: ic.channels,
            sidetone_gain: ic.sidetone_gain,
            mic_gain: ic.mic_gain,
            headphone_gain: ic.headphone_gain,
            limiter_enabled: ic.limiter_enabled,
            limiter_threshold: ic.limiter_threshold,
        })
    };

    // Run the capture loop with optional display and intercom
    run_capture_loop(
        &device_path,
        &config.ndi_name,
        &config.hostname,
        display_config,
        intercom_config,
        args.record_grab.clone(),
        args.record_grab_ts.clone(),
    )
    .await
}

async fn run_capture_loop(
    device_path: &str,
    ndi_name: &str,
    // #685 — this box's hostname (CAM1-CAM6), used ONLY to resolve its grabber model for a
    // per-model capture-rate tolerance (see `capture_rate_health::grabber_model_for_hostname`).
    hostname: &str,
    display_config: Option<NdiDisplayConfig>,
    intercom_config: Option<intercom::IntercomConfig>,
    record_grab: Option<String>,
    record_grab_ts: String,
) -> Result<()> {
    // Shared flag for graceful shutdown
    let running = Arc::new(AtomicBool::new(true));

    // Start display thread if configured (LOW PRIORITY - different core)
    let display_handle = if let Some(config) = display_config {
        let running_clone = Arc::clone(&running);
        tracing::info!("Starting NDI display for source: {}", config.source_name);

        Some(std::thread::spawn(move || {
            // Apply low priority settings BEFORE doing anything
            ndi_display::apply_low_priority();

            if let Err(e) = ndi_display::run_display_loop(config, running_clone) {
                tracing::error!("NDI display error: {}", e);
            }
        }))
    } else {
        None
    };

    // Start intercom thread if configured
    let intercom_handle = if let Some(config) = intercom_config {
        let running_clone = Arc::clone(&running);
        tracing::info!(
            "Starting VBAN intercom: stream={}, target={}",
            config.stream_name,
            config.target_host
        );

        Some(std::thread::spawn(move || {
            if let Err(e) = intercom::run_intercom(config, running_clone) {
                tracing::error!("Intercom error: {}", e);
            }
        }))
    } else {
        None
    };

    // #685/#728/#729 — resolve this box's grabber model ONCE, up front, BEFORE deciding which
    // v4l2 controls to enforce (colour policy, #729) and BEFORE the capture-rate self-heal
    // envelope (#685/#663) — both consumers now share ONE detection instead of drifting apart.
    // Prefers the real OS-level hostname (`os_hostname()`) over the config-derived `hostname`
    // parameter — see that function's doc for why (the deployed fleet's config.toml doesn't set
    // it). `Copy`, so it moves into the `'static` spawn_blocking closure below for free.
    let resolved_hostname = {
        let os = os_hostname();
        if os.is_empty() {
            hostname.to_string()
        } else {
            os
        }
    };
    // #728 — best-effort RUNTIME detection: read the actual plugged-in card's V4L2 `card`
    // string. A physical card swap changes what's plugged in without changing the box's
    // hostname, so the static hostname->model table alone silently desyncs from reality the
    // moment a card moves (the 2026-07-12 cam1<->cam5 reshuffle). `query_card_name` is
    // best-effort (never fatal) — `resolve_grabber_model` falls back to the hostname
    // convention whenever detection is unavailable or unrecognized.
    let detected_card = camera_box::capture::query_card_name(device_path);
    let grabber_model = camera_box::capture_rate_health::resolve_grabber_model(
        &resolved_hostname,
        detected_card.as_deref(),
    );
    let hostname_model =
        camera_box::capture_rate_health::grabber_model_for_hostname(&resolved_hostname);
    if grabber_model != hostname_model {
        tracing::warn!(
            "grabber model MISMATCH: hostname '{}' convention says {}, but the plugged-in \
             card ({:?}) resolves to {} — using the RUNTIME-detected model (a physical card \
             swap changed what's on this box; #728/#729 never trust the stale hostname \
             mapping over live hardware)",
            resolved_hostname,
            hostname_model,
            detected_card,
            grabber_model
        );
    } else {
        tracing::info!(
            "grabber model: {} (hostname '{}', detected card: {:?})",
            grabber_model,
            resolved_hostname,
            detected_card
        );
    }

    // Open capture device at 1920x1080 @ 60fps.
    //
    // Select the V4L2 picture controls to ENFORCE at open (see `select_capture_controls`):
    // - `CAMERA_BOX_CAPTURE_CONTROLS` set -> that explicit override, regardless of model,
    // - else                              -> #729 zero-touch by default: NO controls at all
    //                                         unless `grabber_model` has a documented, proven
    //                                         need (ShadowCast 2's #296 grab-time grayscale-brick
    //                                         risk, or Elgato 4K S's #729-follow-up corrective
    //                                         saturation set for its own ISP tint). Plug-and-play
    //                                         for every other card — factory defaults, no
    //                                         ceremony.
    let env_spec = std::env::var("CAMERA_BOX_CAPTURE_CONTROLS").ok();
    let capture_controls: Vec<camera_box::capture::CaptureControl> =
        camera_box::capture::select_capture_controls(
            grabber_model,
            env_spec.as_deref(),
            record_grab.is_some(),
        );
    if capture_controls.is_empty() {
        tracing::info!(
            "no v4l2 capture controls to enforce at open (zero-touch: {} has no documented \
             need, or an explicit empty override) — #729",
            grabber_model
        );
    } else {
        tracing::info!(
            "enforcing {} certified v4l2 capture control(s) at open ({} documented need / \
             explicit override)",
            capture_controls.len(),
            grabber_model
        );
    }
    let mut capture = VideoCapture::open_with_controls(device_path, &capture_controls)?;
    let (width, height) = capture.dimensions();
    let frame_rate = capture.frame_rate();
    tracing::info!("Capturing at {}x{}", width, height);

    // #663 — owned copy of the resolved device path for the self-heal USB reset (the capture
    // loop below runs on a `'static` spawn_blocking closure, so the borrowed `device_path: &str`
    // parameter can't be captured directly).
    let device_path_owned = device_path.to_string();

    // Genlock #11: optionally emit NDI at a target broadcast rate, decimating the
    // faster capture onto DanteSync wall-clock boundaries, so a downstream
    // genlocked OBS (genlock_fifo) consumes exactly one frame per render tick =
    // zero loss. CAMERA_BOX_GENLOCK_FPS=30 makes a 60fps capture emit 30fps
    // wall-paced (matching a 1080p30 broadcast). Unset = send every captured
    // frame at the capture rate (legacy behavior).
    let genlock_fps: Option<u32> = std::env::var("CAMERA_BOX_GENLOCK_FPS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|&f| f > 0);
    let send_rate = match genlock_fps {
        Some(f) => {
            tracing::info!(
                "GENLOCK: emitting {} fps wall-paced (capture {}/{} fps)",
                f,
                frame_rate.numerator,
                frame_rate.denominator
            );
            camera_box::capture::FrameRate {
                numerator: f,
                denominator: 1,
            }
        }
        None => frame_rate,
    };

    // #174 cam1-capture render-time QR burn — TEST MODE ONLY. Gated behind
    // CAMERA_BOX_BURN_RUN_ID (mirrors the strih/stream DistroAV burn run_id env): when
    // UNSET the burn is OFF and the live NDI feed stays completely CLEAN (zero-copy send
    // untouched, no QR on the broadcast). When set, BEFORE NDIlib_send_send_video_v2 a
    // small QR carrying (run_id, per-emit frame_id, cam1-capture wall-clock gen_ts_ns) is
    // drawn into the EMITTED frame's bottom-center luma so the full chain cam1→strih→
    // stream pairs on a clean digital burn-id from the single stream recording. The burn
    // module is `probe`-feature-gated (kept out of the production binary); the e2e harness
    // deploys a probe-featured camera-box to cam1 for the test run.
    #[cfg(feature = "probe")]
    let burn_run_id: Option<u32> = std::env::var("CAMERA_BOX_BURN_RUN_ID")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|&id| id > 0);
    #[cfg(feature = "probe")]
    if let Some(id) = burn_run_id {
        tracing::info!(
            "#174 CAM1-CAPTURE BURN ACTIVE (TEST MODE): run_id={} — bottom-center QR burned into the emitted frame (production feed is OFF unless CAMERA_BOX_BURN_RUN_ID is set)",
            id
        );
        // #275b: the async burn stamps each emitted frame's NDI timecode at the genlock
        // emit-gate boundary. Without genlock there is no boundary to align to (the timecode
        // falls back to the raw wall clock), so the measurement is only well-defined under
        // genlock. The E2E harness always sets GENLOCK_FPS; warn loudly if it didn't.
        if genlock_fps.is_none() {
            tracing::warn!(
                "#275b cam1 burn is ACTIVE without genlock (GENLOCK_FPS unset) — emitted-frame timecodes will NOT be boundary-aligned; enable genlock for a well-defined measurement"
            );
        }
    }

    // Create NDI sender with configured name and the (genlock or capture) rate
    let mut sender = NdiSender::new(ndi_name, send_rate)?;
    if genlock_fps.is_some() {
        sender.set_external_pacing(true);
    }
    tracing::info!("NDI sender ready, streaming as '{}'", ndi_name);
    tracing::info!("ZERO-COPY mode: AVX2 SIMD + sync send for lowest latency");

    // #792 — optional SECONDARY 30fps NDI stream: 2-frame temporal blend of the emitted 60fps
    // pairs, published as "<machine> (30p)" alongside the primary. OFF unless
    // CAMERA_BOX_PUBLISH_30P=1 (unset ⇒ bit-identical legacy behavior). The tee into this path
    // is bounded + drop-on-full, so the 60p hot path can NEVER block on it; a sender-create
    // failure disables the feature loudly and leaves the 60p stream untouched.
    let publish_30p_cfg = camera_box::publish_30p::Config::from_process_env();
    let mut publish_30p_tee: Option<camera_box::publish_30p::Tee> = if publish_30p_cfg.enabled {
        let effective_fps = send_rate.numerator as f64 / send_rate.denominator.max(1) as f64;
        if effective_fps < 60.0 {
            tracing::warn!(
                "#792 publish-30p expects a 60fps emit path; effective send rate is {:.1} fps — pair blending would combine NON-adjacent frames",
                effective_fps
            );
        }
        let name_30p = camera_box::publish_30p::derive_30p_name(ndi_name);
        match NdiSender::new(
            &name_30p,
            camera_box::capture::FrameRate {
                numerator: 30,
                denominator: 1,
            },
        ) {
            Ok(mut s) => {
                // The worker consumes emitted pairs (external cadence) and stamps each output
                // with its pair's FIRST frame's genlock timecode — no internal pacing sleep.
                s.set_external_pacing(true);
                match camera_box::publish_30p::spawn(s, publish_30p_cfg) {
                    Ok(tee) => {
                        tracing::info!(
                            "#792 publish-30p ACTIVE: streaming as '{}' (mode={}, max blend={}, channel depth {})",
                            name_30p,
                            if publish_30p_cfg.adaptive {
                                "motion-adaptive"
                            } else {
                                "fixed"
                            },
                            publish_30p_cfg.blend,
                            camera_box::publish_30p::CHANNEL_DEPTH
                        );
                        Some(tee)
                    }
                    Err(e) => {
                        tracing::error!(
                            "#792 publish-30p worker spawn FAILED ({e}) — secondary stream disabled, 60p unaffected"
                        );
                        None
                    }
                }
            }
            Err(e) => {
                tracing::error!(
                    "#792 publish-30p sender create FAILED ({e}) — secondary stream disabled, 60p unaffected"
                );
                None
            }
        }
    } else {
        None
    };

    // #105 node 2 — cam1 grab recorder (TEST mode). Connect BEFORE the loop so the
    // dev1 ffmpeg listener is bound and the sidecar header is written; a connect
    // failure is fatal (the operator asked to record — don't silently NDI-only).
    let (grab_width, grab_height) = capture.dimensions();
    let grab_stride = capture.frame_info().stride;
    let mut grab_recorder = match &record_grab {
        Some(dest) => {
            let rec = camera_box::grab_record::GrabRecorder::connect(
                dest,
                std::path::Path::new(&record_grab_ts),
                grab_width,
                grab_height,
                grab_stride,
            )?;
            tracing::info!(
                "cam1 GRAB-RECORD active (#105 node 2): {} → {}",
                record_grab_ts,
                dest
            );
            Some(rec)
        }
        None => None,
    };

    // cam2→cam1 LOSS sidecar (TEST mode): when CAMERA_BOX_CAPTURE_STATS is set to a path,
    // the capture loop writes cam1's V4L2 capture-drop count there on shutdown. The
    // recording-verdict reads it as the cam2→cam1 loss (the camera leg — a dropped capture
    // = a lost frame), NOT a painter-tick optical compare. UNSET ⇒ no sidecar (production).
    let capture_stats_path: Option<String> = std::env::var("CAMERA_BOX_CAPTURE_STATS")
        .ok()
        .filter(|s| !s.is_empty());
    if let Some(p) = &capture_stats_path {
        tracing::info!(
            "cam2→cam1 LOSS sidecar active (TEST MODE): cam1 V4L2 capture-drop stats → {} on shutdown",
            p
        );
    }

    // Spawn capture loop in blocking task - minimal overhead for lowest latency
    let running_capture = Arc::clone(&running);
    let capture_handle = tokio::task::spawn_blocking(move || {
        // Apply real-time optimizations BEFORE entering the capture loop
        apply_realtime_optimizations();

        let mut frame_count: u64 = 0; // frames captured this report window
        let mut emit_count: u64 = 0; // frames actually sent to NDI this window
        let mut last_report = std::time::Instant::now();
        // #707 B1 — per-second emit/capture ring. A MONOTONIC epoch (never the wall clock — this
        // ticket is about DanteSync wall-clock seams) buckets emit/capture into 1-second slices so a
        // sub-5s emit pause (the #707 freeze) surfaces instead of averaging into the 5s report.
        let ring_epoch = std::time::Instant::now();
        let mut emit_ring = camera_box::emit_rate_ring::EmitRateRing::new(
            camera_box::emit_rate_ring::DEFAULT_RING_SECONDS,
        );
        // #752 — coalesce the per-gate-call emit-gate-skip WARN (previously ~10/s on a skipping
        // box → rsyslogd 37% + journald 15% CPU on the 3-core boxes, a starvation feedback loop)
        // into ONE aggregated WARN per 5s Streaming report. The per-skip detail is accumulated
        // here and drained on the report tick below.
        let mut emit_skip_log = camera_box::emit_skip_log::EmitGateSkipLog::new();

        // #299 — colour-capture metric: sample chroma once per CHROMA_SAMPLE_FRAMES
        // captured frames (≈1 Hz at 60 fps) and log alongside the 5-second fps report.
        // A grayscale log line ("grayscale (source likely monochrome)") is the automatic
        // regression signal that replaces the previous "look at it and wonder" method.
        // `None` until the first sample lands, so a cold-start report never logs a
        // false "grayscale" reading from an uninitialised (0,0).
        let mut last_chroma: Option<(f32, f32)> = None;
        let mut chroma_frame_ctr: u32 = 0;

        // #656 — capture-delivery-rate health: consecutive-breach counter for the pure
        // `capture_rate_health` decision, checked against the device's NEGOTIATED capture rate
        // (`frame_rate`, read once above at capture-open time) every ~5s report window below.
        let mut consecutive_rate_breaches: u32 = 0;
        let configured_capture_fps: f64 =
            frame_rate.numerator as f64 / frame_rate.denominator.max(1) as f64;
        // #685 — this box's model-specific capture-rate deviation tolerance (percent); see
        // `capture_rate_health::tolerance_pct_for_model`. Only the CAPTURE-side (#656/#663)
        // check below uses this — the EMIT-side (#666) health check just below stays on its own
        // model-agnostic tolerance, since the emit-side invariant (60.00 fps on the DanteSync
        // tick, zero loss) is unaffected by capture-side wobble and stays strict by design.
        let capture_rate_tolerance_pct =
            camera_box::capture_rate_health::tolerance_pct_for_model(grabber_model);
        // #717 — SUSTAINED-band capture-rate health: a SEPARATE, narrower consecutive-breach
        // counter/tolerance, so a genuinely chronic deviation (e.g. cam1's #674 63.9-64.0fps)
        // still trips self-heal even when it stays comfortably inside the wide jitter-band
        // tolerance above (#685's correct-for-short-bursts widening, which #717 leaves
        // unchanged). See `capture_rate_health::sustained_tolerance_pct_for_model`'s doc.
        let mut consecutive_sustained_breaches: u32 = 0;
        let sustained_rate_tolerance_pct =
            camera_box::capture_rate_health::sustained_tolerance_pct_for_model(grabber_model);
        // #666 — emit-vs-capture health: consecutive-breach counter for the SAME pure
        // `capture_rate_health` decision, but checked against the configured genlock SEND rate
        // (`send_fps`, below) instead of the negotiated capture rate — catches a defect in the
        // emit/genlock-gate path even while capture itself stays perfectly healthy.
        let mut consecutive_emit_breaches: u32 = 0;

        // #663 — self-heal: set (instead of calling `std::process::exit` immediately) once a USB
        // reset attempt has run, so the loop below stops via the NORMAL `running_capture` flag
        // and falls through to the EXISTING shutdown cleanup (burn ring drain, grab-recorder
        // flush, burn-thread join, capture-stats sidecar write) before the process actually
        // exits. A raw mid-loop `process::exit` would skip all of that — harmless in plain
        // production, but it would truncate an in-flight `--record-grab` E2E recording if a
        // self-heal fires mid-test (review finding, #663).
        let mut pending_self_heal_exit_code: Option<i32> = None;

        // #275b — async cam1 capture-burn pipeline. When the burn is active (probe +
        // CAMERA_BOX_BURN_RUN_ID), move the single NDI sender to a dedicated burn thread and hand
        // each emitted frame off over a bounded ring, so the heavy per-frame QR render no longer
        // runs on the emit loop (which capped cam1 at 30 fps). Otherwise the sender stays on the
        // capture thread (production / non-burn zero-copy path, unchanged). `capture_sender` holds
        // the sender so it can be `take`n into the burn thread. `burn_ids` is the monotonic
        // per-EMITTED-frame burn id, drawn once per emit on this thread to keep the burn id ↔
        // emitted-frame mapping strictly 1:1. `send_fps` is the genlock rate used for the
        // gate-instant emitted-frame timecode (0 ⇒ genlock off, a degenerate no-op) — used by
        // BOTH the probe burn path and the #286 production capture-based timecode below.
        let send_fps: u32 = genlock_fps.unwrap_or(0);
        // #286 — the periodically-resampled monotonic->realtime clock offset
        // (`genlock_stamp::capture_realtime_100ns`'s `mono_to_real_offset_100ns`), plus how
        // many captured frames have elapsed since the last sample. Sampled once here (before
        // the first frame) and re-sampled inside the loop per
        // `genlock_stamp::should_resample_mono_to_real_offset` so a realtime clock step/slew
        // (DanteSync/NTP correction) never permanently skews the capture-based genlock stamp.
        let mut mono_to_real_offset_100ns: i64 = sample_mono_to_real_offset_100ns();
        let mut frames_since_offset_sample: u64 = 0;
        #[cfg(feature = "probe")]
        let mut burn_ids = camera_box::probe::genlock::BurnFrameIdSource::default();
        #[cfg(feature = "probe")]
        let mut capture_sender: Option<NdiSender> = Some(sender);
        #[cfg(feature = "probe")]
        let mut burn_ring: Option<camera_box::probe::genlock::BurnRing<BurnJob>> = None;
        #[cfg(feature = "probe")]
        let mut burn_handle: Option<std::thread::JoinHandle<()>> = None;
        // #280 — bounded pool of reusable frame buffers for the async cam1-burn copy. The capture
        // thread takes a buffer here (reuse, or alloc only when empty), copies the emitted frame in,
        // and submits; the burn thread returns it after the NDI send. Replaces the per-frame ~4MB
        // `to_vec` allocation churn at up to 60 fps with recycled buffers (no order/mapping change).
        #[cfg(feature = "probe")]
        let burn_pool = Arc::new(camera_box::probe::genlock::BufferPool::new(
            camera_box::probe::genlock::BURN_POOL_CAP,
        ));
        #[cfg(feature = "probe")]
        if burn_run_id.is_some() {
            // #279 FIX 2 — hand the burn ring the capture loop's `running` flag so a full-ring
            // submit is interruptible on shutdown (never wedges the capture thread).
            let (ring, rx) =
                camera_box::probe::genlock::burn_ring::<BurnJob>(Arc::clone(&running_capture));
            // #280 — the burn thread's handle on the buffer pool: it returns each frame's buffer
            // here after the NDI send so the capture thread can refill it instead of reallocating.
            let burn_pool_consumer = Arc::clone(&burn_pool);
            let mut burn_sender = capture_sender
                .take()
                .expect("NDI sender is present before the burn thread takes it");
            // #279 FIX 4 — whether genlock paces the pipeline. ON: the capture thread already
            // gated each emit to a wall-clock boundary and stamped its timecode, so the burn
            // thread sends verbatim (no re-derivation/sleep → no queue jitter in the pacing).
            // OFF (manual burn run): nothing decimates, so the burn thread must restore the old
            // self-pacing send (wait to the sender-rate boundary + stamp that boundary timecode).
            let burn_external_pacing = genlock_fps.is_some();
            let handle = std::thread::Builder::new()
                .name("cam1-burn".into())
                .spawn(move || {
                    // #289 — in burn mode (probe/E2E) the NDI sender lives HERE, so this thread is
                    // the EMIT hot path. Pin it explicitly to the isolated core (issue point 1:
                    // capture + EMIT → isolated core) rather than relying on the affinity it
                    // inherits from the capture thread — same core, but explicit + robust to any
                    // future spawn-order change.
                    camera_box::affinity::pin_capture_thread();
                    // Burn thread: (optionally) render the QR into the copied frame + NDI-send it in
                    // receive (= emit) order. Ends when the capture loop drops the ring (shutdown).
                    camera_box::probe::genlock::run_burn_ring(rx, |mut job: BurnJob| {
                        // #279 FIX 3 — render the QR ONLY for a YUYV frame; a substituted non-YUYV
                        // frame is sent UNBURNED (never dropped, so the feed can't go dead).
                        if job.render_qr {
                            let payload = camera_box::probe::payload::Payload {
                                run_id: job.run_id,
                                frame_id: job.frame_id,
                                gen_ts_ns: job.gen_ts_ns,
                            };
                            camera_box::probe::qr::burn_qr_yuyv(
                                &mut job.buf,
                                job.info.width,
                                job.info.height,
                                job.info.stride,
                                &payload,
                                camera_box::probe::qr::CAM1_BURN_QR_PX,
                            );
                        }
                        let send_result = if burn_external_pacing {
                            // Genlock on: send with the gate-stamped emitted-frame timecode.
                            burn_sender.send_frame_data_with_timecode(
                                &job.buf,
                                job.info.width,
                                job.info.height,
                                job.info.fourcc,
                                job.info.stride,
                                job.emit_timecode_100ns,
                            )
                        } else {
                            // #279 FIX 4 — genlock off (manual burn): restore the old self-pacing
                            // send. send_frame_data waits to the sender-rate boundary and stamps
                            // that boundary timecode (external_pacing was left false on the sender),
                            // so a manual burn launch emits paced, boundary-aligned frames as before.
                            burn_sender.send_frame_data(
                                &job.buf,
                                job.info.width,
                                job.info.height,
                                job.info.fourcc,
                                job.info.stride,
                            )
                        };
                        if let Err(e) = send_result {
                            // #279 FIX 5 — full context on every error path: identify the frame
                            // (frame_id/run_id) and use Debug ({:?}) so the error chain is kept.
                            tracing::error!(
                                "#275b cam1-burn thread NDI send failed: frame_id={} run_id={} err={:?}",
                                job.frame_id,
                                job.run_id,
                                e
                            );
                        }
                        // #280 — the frame is sent; return its buffer (with its ~4MB capacity) to
                        // the pool so the capture thread refills it instead of allocating a new one.
                        burn_pool_consumer.put(job.buf);
                    });
                })
                .expect("spawn cam1-burn thread");
            burn_ring = Some(ring);
            burn_handle = Some(handle);
        }

        // Genlock decimation state: emit the first capture at/after each target
        // wall-clock boundary, skip the rest. interval_ns 0 => decimation off.
        let out_interval_ns: u64 = genlock_fps
            .map(|f| 1_000_000_000u64 / f as u64)
            .unwrap_or(0);
        let mut next_boundary_ns: u64 = 0;

        while running_capture.load(Ordering::Relaxed) {
            // #707 B1 — snapshot the emit counter before the frame closure so the per-second ring
            // can attribute exactly this frame's emit (0 or 1) after it returns.
            let emit_before = emit_count;
            // ZERO-COPY: Process frame directly from mmap buffer without copying
            let result = capture.process_frame(|data, info| {
                // #286 — periodically re-sample the monotonic->realtime clock offset. Counts
                // EVERY captured frame toward the cadence (regardless of emit/decimate
                // decisions below), so the offset stays fresh even during a long decimated
                // stretch — mirrors the chroma sample's "always count captured frames" note.
                frames_since_offset_sample += 1;
                if camera_box::genlock_stamp::should_resample_mono_to_real_offset(
                    frames_since_offset_sample,
                ) {
                    mono_to_real_offset_100ns = sample_mono_to_real_offset_100ns();
                    frames_since_offset_sample = 0;
                }

                // #299 — chroma sample: every CHROMA_SAMPLE_FRAMES captured frames
                // (regardless of emit/decimate decisions so we always sample the raw
                // device output). Stored in `last_chroma`; logged on the 5-second tick.
                chroma_frame_ctr = chroma_frame_ctr.wrapping_add(1);
                if chroma_frame_ctr.is_multiple_of(camera_box::capture::CHROMA_SAMPLE_FRAMES) {
                    last_chroma = Some(camera_box::capture::mean_chroma(
                        data,
                        info.width as usize,
                        info.height as usize,
                        info.stride as usize,
                    ));
                }

                // #707 — did THIS frame's blocking V4L2 dequeue itself stall? Checked on EVERY
                // captured frame (regardless of emit/decimate decisions below), mirroring the
                // offset-sample/chroma-sample blocks above. See `capture_stall`'s module doc: this
                // is the missing capture-side half of the `send_stall` observability pair — a
                // WARN here on the NEXT natural CAM1-class recurrence, at the same time as an
                // `all_cambox_delivery_latency` spike, confirms the V4L2/USB/driver layer as the
                // mechanism; silence here (as already confirmed for `send_stall` on a real 2026-
                // 07-14 recurrence) would point elsewhere (e.g. strih's own presentation cadence,
                // per #726).
                if configured_capture_fps > 0.0 {
                    let capture_frame_interval_ms = 1000.0 / configured_capture_fps;
                    if camera_box::capture_stall::is_capture_stall(
                        info.dequeue_duration_ms,
                        capture_frame_interval_ms,
                    ) {
                        tracing::warn!(
                            "{}",
                            camera_box::capture_stall::capture_stall_warning(
                                info.dequeue_duration_ms,
                                capture_frame_interval_ms,
                                configured_capture_fps,
                            )
                        );
                    }
                }

                if out_interval_ns > 0 {
                    // Genlock decimation: emit only the capture at/after each
                    // wall-clock boundary (pure logic in ndi::genlock_emit_gate).
                    let prev_boundary_ns = next_boundary_ns;
                    let (emit, next) = camera_box::ndi::genlock_emit_gate(
                        wall_clock_ns(),
                        next_boundary_ns,
                        out_interval_ns,
                    );
                    next_boundary_ns = next;
                    // #707 — a clock discontinuity (DanteSync NTP/PTP step, or a stalled poll)
                    // can leap the gate's boundary past one or more intervals that are then
                    // NEVER emitted — the missing direct evidence for whether a clock step is
                    // what's behind a #666/#707-class transient emit-rate deficit. See
                    // `ndi::boundary_skip_count`'s own doc comment.
                    let skipped = camera_box::ndi::boundary_skip_count(
                        prev_boundary_ns,
                        next_boundary_ns,
                        out_interval_ns,
                    );
                    if skipped > 0 {
                        // #752 — do NOT log per skip (that was the ~10/s storm). Accumulate; the
                        // 5s Streaming report below drains ONE aggregated WARN with the count.
                        emit_skip_log.record(skipped);
                    }
                    if !emit {
                        return; // between boundaries — decimate (don't send)
                    }
                }
                // #275b — ONE cam1 emit-instant wall-clock stamp (CLOCK_REALTIME, the DanteSync
                // clock), shared by the burned QR's gen_ts AND the grab-recording tee, so both
                // describe the SAME instant even when the async submit below back-pressures (the
                // grab ts must not drift later than the emit, which would inflate cam2→cam1 latency).
                let emit_wall_ns = wall_clock_ns() as i64;

                // #286 — this frame's real CAPTURE instant, mapped from the V4L2-stamped
                // CLOCK_MONOTONIC domain into CLOCK_REALTIME. THIS (not `emit_wall_ns`) is
                // the basis for the emitted NDI genlock timecode below, so a grabber card's
                // photon->dequeue latency can no longer leak into the stamp. `emit_wall_ns`
                // is retained unchanged for the burn's own `gen_ts_ns` + grab-record tee
                // (see genlock_stamp's module doc — those stay arrival-based on purpose).
                let capture_realtime_100ns = camera_box::genlock_stamp::capture_realtime_100ns(
                    info.capture_monotonic_100ns,
                    mono_to_real_offset_100ns,
                );

                // #105 node 2 — tee the EMITTED (original, unburned) frame to the cam1 grab
                // recording at the emit instant. A broken grab stream stops recording but NEVER
                // the NDI send (the measured pipeline must not be disturbed by a recorder fault).
                // One closure so the burn path and the production path can't drift.
                let mut tee_grab = |data: &[u8]| {
                    if let Some(rec) = grab_recorder.as_mut() {
                        if let Err(e) = rec.write_frame(data, emit_wall_ns) {
                            tracing::error!("grab-record write failed, stopping recorder: {}", e);
                            grab_recorder = None;
                        }
                    }
                };

                // #275b ASYNC BURN PATH (test mode: probe + CAMERA_BOX_BURN_RUN_ID). Stamp the
                // emitted frame's burn id + emit-instant gen_ts + gate-instant NDI timecode HERE
                // (the genlock-authoritative moment), copy the frame, and hand it to the burn
                // thread. The heavy QR render + NDI send run OFF this thread so the burn no longer
                // caps the emit rate; the bounded ring back-pressures (never drops) → the burn id
                // ↔ emitted-frame mapping stays strictly 1:1. When the burn is OFF (production /
                // non-burn), control falls through to the verbatim zero-copy send below.
                #[cfg(feature = "probe")]
                if let (Some(ring), Some(run_id)) = (burn_ring.as_ref(), burn_run_id) {
                    // #279 FIX 3 — the NDI sender lives on the burn thread, so EVERY emitted frame
                    // must route through the ring. The QR is rendered ONLY for YUYV (the burner
                    // assumes that layout); a v4l2 format substitution yields a non-YUYV frame that
                    // is sent UNBURNED (passthrough) — never dropped, so a substitution can't kill
                    // the cam1 feed (restores the pre-#275b `_ => data` graceful degradation).
                    let fourcc = info.fourcc.str().unwrap_or("");
                    let render_qr = camera_box::probe::genlock::burn_should_render_qr(fourcc);
                    if !render_qr {
                        tracing::warn!(
                            "#275b burn active but frame fourcc is {} (not YUYV) — emitting UNBURNED passthrough (cam1 should always be YUYV)",
                            if fourcc.is_empty() { "?" } else { fourcc }
                        );
                    }
                    let frame_id = burn_ids.next_id();
                    // #280 — copy the mmap frame into a RECYCLED pool buffer (reused, or allocated
                    // only when the free list is empty) instead of a fresh per-frame `to_vec`. The
                    // mmap is valid only inside this callback, so a copy is still required to cross
                    // the thread boundary — but a reused buffer keeps its ~4MB capacity so the
                    // copy does not reallocate. clear()+extend reuses that capacity in place.
                    let mut buf = burn_pool.take();
                    buf.clear();
                    buf.extend_from_slice(data);
                    let job = BurnJob {
                        buf,
                        info,
                        run_id,
                        frame_id,
                        // #286 BUG SITE #1 FIX — the emitted frame's genlock NDI timecode now
                        // keys on the real CAPTURE instant (`capture_realtime_100ns`), not the
                        // arrival-based `boundary_timecode_100ns(send_fps)` this used to call.
                        // `gen_ts_ns` stays arrival (`emit_wall_ns`) unchanged — it feeds the
                        // #624/#625 latency measurement, which is out of scope for this fix.
                        gen_ts_ns: emit_wall_ns,
                        emit_timecode_100ns: camera_box::genlock_stamp::genlock_emit_timecode_100ns(
                            capture_realtime_100ns,
                            // `emit_wall_ns` is nanoseconds; this parameter is 100ns units.
                            emit_wall_ns / 100,
                            send_fps as i64,
                        ),
                        render_qr,
                    };
                    // BLOCKING submit (back-pressures, never drops → 1:1 preserved). Count the
                    // emit ONLY on success: any Err means the frame was NOT sent, so it must not
                    // inflate the emitted-fps stat. #279 FIX 2 — a full-ring submit is
                    // interruptible on shutdown (ShutdownInterrupted), distinct from the burn
                    // thread being gone (Closed).
                    // On either Err the un-sent job (and its #280 pooled buffer) is dropped/freed
                    // — both are TERMINAL paths (shutdown signalled, or the burn thread is gone),
                    // never steady state, so not returning the buffer to the pool cannot leak.
                    match ring.submit(job) {
                        Ok(()) => emit_count += 1,
                        Err(SubmitError::ShutdownInterrupted(_)) => tracing::info!(
                            "#275b cam1-burn submit interrupted by shutdown — frame_id={} not sent",
                            frame_id
                        ),
                        Err(SubmitError::Closed(_)) => tracing::error!(
                            "#275b cam1-burn ring closed — burn thread gone (frame_id={} run_id={} not sent)",
                            frame_id, run_id
                        ),
                    }
                    tee_grab(data);
                    return; // handed to the burn thread; the sender lives there now
                }

                // PRODUCTION / non-burn path: zero-copy direct send (unchanged). Under the probe
                // build the sender lives in `capture_sender`; it moves to the burn thread ONLY when
                // the burn is active, and then the handoff above handles every frame and returns —
                // so this path is reached only when the burn is inactive (`capture_sender` = Some).
                #[cfg(feature = "probe")]
                let sender = capture_sender
                    .as_mut()
                    .expect("capture_sender is present whenever the burn is inactive");
                // #286 BUG SITE #2 FIX — pass the CAPTURE-based genlock timecode through to
                // send_frame_zero_copy so it stamps the real capture instant (under external
                // pacing) instead of re-deriving an arrival-based boundary at send time.
                let capture_timecode_100ns = camera_box::genlock_stamp::genlock_emit_timecode_100ns(
                    capture_realtime_100ns,
                    // `emit_wall_ns` is nanoseconds; this parameter is 100ns units.
                    emit_wall_ns / 100,
                    send_fps as i64,
                );
                if let Err(e) = sender.send_frame_zero_copy(data, info, capture_timecode_100ns) {
                    tracing::error!("Failed to send frame: {}", e);
                }
                emit_count += 1; // reached only when the frame passed the gate
                tee_grab(data);
                // #792 — tee the emitted frame to the optional 30p publisher LAST (one bounded
                // memcpy + try_send, drop-on-full: never blocks this 60p hot path). Production
                // path only — the probe burn path above returns before reaching here.
                if let Some(t) = publish_30p_tee.as_mut() {
                    t.tee(data, info, capture_timecode_100ns);
                }
            });

            match result {
                Ok(()) => {
                    frame_count += 1;

                    // #707 B1 — feed the per-second emit/capture ring (MONOTONIC clock) so a
                    // sub-5s emit pause surfaces instead of averaging into the 5s report. WARN the
                    // instant any completed 1-second bucket's emit dips below the send floor: this
                    // is the box-side prong of #707 B1's freeze discriminator — if it fires during
                    // a strih freeze the box emit path dipped; if the box stays clean (buckets ~60)
                    // while strih freezes, the loss is downstream (link / NDI SDK), read off the
                    // transport sampler instead.
                    let emitted_this = (emit_count - emit_before) as u32;
                    let now_mono_ns = ring_epoch.elapsed().as_nanos() as u64;
                    for bucket in emit_ring.observe(now_mono_ns, emitted_this, 1) {
                        if camera_box::emit_rate_ring::emit_bucket_below_floor(
                            bucket.emit,
                            send_fps as f64,
                            camera_box::emit_rate_ring::BUCKET_FLOOR_FRACTION,
                        ) {
                            tracing::warn!(
                                "#707 B1 emit-1s DIP: a 1-second window emitted {} frames vs a {} fps configured send rate (floor {:.0}) while capture stayed {} that second — a sub-5s emit pause on THIS box. Discriminator: during a strih freeze this WARN = the box emit path dipped; if the box stays clean while strih freezes, the loss is downstream (link/NDI SDK — read the transport sampler CSV). recent emit-1s: {:?} cap-1s: {:?}",
                                bucket.emit,
                                send_fps,
                                send_fps as f64 * camera_box::emit_rate_ring::BUCKET_FLOOR_FRACTION,
                                bucket.capture,
                                emit_ring.emit_buckets(),
                                emit_ring.capture_buckets(),
                            );
                        }
                    }

                    // Report fps every 5 seconds. Under genlock decimation the
                    // emit rate (frames actually sent) differs from the capture
                    // rate — log both so a decimation regression (e.g. emitting
                    // 0 or 60 instead of 30) is visible on-device.
                    let elapsed = last_report.elapsed();
                    if elapsed.as_secs() >= 5 {
                        let secs = elapsed.as_secs_f64();
                        let cap_fps = frame_count as f64 / secs;
                        let dropped = capture.dropped_captures();
                        // #696 — a cumulative count of buffers DELIVERED on schedule but
                        // dropped for content corruption (V4L2_BUF_FLAG_ERROR / a short
                        // buffer) — distinct from `dropped` (frames the device never
                        // delivered at all). Surfaced alongside capture-dropped so this
                        // failure class (previously invisible to any rate/sequence-based
                        // check) shows up in the routine 5s report.
                        let corrupted = capture.corrupted_frames();
                        if out_interval_ns > 0 {
                            let emit_fps = emit_count as f64 / secs;
                            tracing::info!(
                                "Streaming: {:.1} fps emitted / {:.1} fps captured ({} sent, {} captured, {} capture-dropped, {} corrupted)",
                                emit_fps,
                                cap_fps,
                                emit_count,
                                frame_count,
                                dropped,
                                corrupted
                            );

                            // #666 — emit-vs-capture health: WARN when the EMITTED fps has
                            // sustained a deviation from the box's configured genlock SEND rate
                            // (not the negotiated CAPTURE rate — capture can stay perfectly
                            // healthy while the emit/genlock-gate path alone degrades) for
                            // CAPTURE_RATE_WARN_WINDOWS consecutive report windows. Live finding
                            // (cam5/cam6, 2026-07-11): a transient ~20% emit deficit (captured
                            // rate unaffected, 0 capture-dropped) that self-recovered within
                            // ~5 minutes with no restart — this WARN is the automatic signal a
                            // future recurrence needs instead of relying on someone noticing live.
                            let emit_deviant = camera_box::capture_rate_health::is_rate_deviant(
                                emit_fps,
                                send_fps as f64,
                                camera_box::capture_rate_health::EMIT_RATE_TOLERANCE_PCT,
                            );
                            consecutive_emit_breaches =
                                camera_box::capture_rate_health::next_consecutive_breaches(
                                    consecutive_emit_breaches,
                                    emit_deviant,
                                );
                            if camera_box::capture_rate_health::should_warn(
                                consecutive_emit_breaches,
                                camera_box::capture_rate_health::CAPTURE_RATE_WARN_WINDOWS,
                            ) {
                                tracing::warn!(
                                    "#666 emit-delivery-rate DEFECTIVE: {:.2} fps emitted vs {:.2} fps configured send rate (captured stayed {:.2} fps, {} capture-dropped) — >{:.1}% deviation sustained for {} consecutive report windows, ~{}s (network/genlock-gate hiccup, not a capture-device defect — see #666)",
                                    emit_fps,
                                    send_fps as f64,
                                    cap_fps,
                                    dropped,
                                    camera_box::capture_rate_health::EMIT_RATE_TOLERANCE_PCT,
                                    consecutive_emit_breaches,
                                    consecutive_emit_breaches as u64 * 5
                                );
                            }
                        } else {
                            tracing::info!(
                                "Streaming: {:.1} fps ({} frames, {} capture-dropped, {} corrupted)",
                                cap_fps,
                                frame_count,
                                dropped,
                                corrupted
                            );
                        }
                        // #707 B1 — print the per-second emit/capture ring alongside the 5s
                        // average so a sub-5s emit pause (the #707 freeze) is visible in the log
                        // even when it averaged out of the fps line above. Buckets are oldest-first,
                        // one per completed 1-second window.
                        tracing::info!(
                            "#707 emit-1s: {:?} cap-1s: {:?} (1-second buckets, oldest first)",
                            emit_ring.emit_buckets(),
                            emit_ring.capture_buckets(),
                        );

                        // #752 — ONE aggregated emit-gate-skip WARN per report window (drains +
                        // resets the accumulator). A clean window logs nothing; a skipping window
                        // logs a single line with the event count + total boundaries skipped,
                        // instead of the ~10/s per-skip storm that starved the emit thread.
                        if let Some((skip_events, skip_total)) = emit_skip_log.take() {
                            tracing::warn!(
                                "{}",
                                camera_box::emit_skip_log::skip_summary_warning(
                                    skip_events,
                                    skip_total,
                                    5
                                )
                            );
                        }

                        // #656 — capture-delivery-rate health: WARN when the captured fps has
                        // sustained a >1% deviation from the device's negotiated capture rate
                        // for CAPTURE_RATE_WARN_WINDOWS consecutive report windows (a real
                        // capture-device rate defect, e.g. a USB dongle silently re-negotiating
                        // its rate) — never on a single momentary blip. This is the automatic
                        // regression signal that replaces after-the-fact tick-pattern
                        // archaeology on a recorded E2E run (the #656 root cause).
                        let rate_deviant = camera_box::capture_rate_health::is_rate_deviant(
                            cap_fps,
                            configured_capture_fps,
                            capture_rate_tolerance_pct,
                        );
                        consecutive_rate_breaches =
                            camera_box::capture_rate_health::next_consecutive_breaches(
                                consecutive_rate_breaches,
                                rate_deviant,
                            );
                        let jitter_confirmed = camera_box::capture_rate_health::should_warn(
                            consecutive_rate_breaches,
                            camera_box::capture_rate_health::CAPTURE_RATE_WARN_WINDOWS,
                        );

                        // #717 — SUSTAINED-band check: a SEPARATE, narrower tolerance
                        // (`sustained_rate_tolerance_pct`) + a longer required run
                        // (`SUSTAINED_WARN_WINDOWS`, 60s) so a genuinely chronic deviation (e.g.
                        // cam1's #674 chronic 63.9-64.0fps) still trips self-heal even while it
                        // stays comfortably inside the wide jitter floor above — #685's widening
                        // is still correct for genuinely short-lived quantization jitter (band
                        // (a)); this catches band (b), the sustained case #685 over-corrected
                        // away. See `capture_rate_health::sustained_tolerance_pct_for_model`'s doc.
                        let sustained_deviant = camera_box::capture_rate_health::is_rate_deviant(
                            cap_fps,
                            configured_capture_fps,
                            sustained_rate_tolerance_pct,
                        );
                        consecutive_sustained_breaches =
                            camera_box::capture_rate_health::next_consecutive_breaches(
                                consecutive_sustained_breaches,
                                sustained_deviant,
                            );
                        let sustained_confirmed = camera_box::capture_rate_health::should_warn(
                            consecutive_sustained_breaches,
                            camera_box::capture_rate_health::SUSTAINED_WARN_WINDOWS,
                        );

                        if camera_box::capture_rate_selfheal::should_trigger_selfheal(
                            jitter_confirmed,
                            sustained_confirmed,
                        ) {
                            if jitter_confirmed {
                                tracing::warn!(
                                    "#656 capture-delivery-rate DEFECTIVE: {:.2} fps captured vs {:.2} fps configured/negotiated (>{:.1}% deviation sustained for {} consecutive report windows, ~{}s, {} tolerance) — USB-reset the capture device (see #656, #685)",
                                    cap_fps,
                                    configured_capture_fps,
                                    capture_rate_tolerance_pct,
                                    consecutive_rate_breaches,
                                    consecutive_rate_breaches as u64 * 5,
                                    grabber_model
                                );
                            } else {
                                tracing::warn!(
                                    "#717 capture-delivery-rate SUSTAINED defect: {:.2} fps captured vs {:.2} fps configured/negotiated (>{:.1}% deviation sustained for {} consecutive report windows, ~{}s) — inside {}'s wide {:.1}% jitter-tolerant envelope but PERSISTENT beyond the #717 60s sustained bar, reset-fixable per #642/#674 evidence — USB-reset the capture device (see #717, #674, #685, #663)",
                                    cap_fps,
                                    configured_capture_fps,
                                    sustained_rate_tolerance_pct,
                                    consecutive_sustained_breaches,
                                    consecutive_sustained_breaches as u64 * 5,
                                    grabber_model,
                                    capture_rate_tolerance_pct
                                );
                            }

                            // #663 — self-heal: the #656 fix (a manual USB reset) is only
                            // TEMPORARY — the same defect recurred within hours, three times in
                            // one day (live incident). Rate-limited + escalating automatic USB
                            // reset (see capture_rate_selfheal module doc for the full design).
                            let now_epoch_s = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0);
                            let state_path =
                                std::path::Path::new(camera_box::capture_rate_selfheal::STATE_PATH);
                            let prev_selfheal_state =
                                camera_box::capture_rate_selfheal::load_state(state_path);
                            let (selfheal_decision, next_selfheal_state) =
                                camera_box::capture_rate_selfheal::decide_selfheal(
                                    prev_selfheal_state,
                                    true,
                                    now_epoch_s,
                                    camera_box::capture_rate_selfheal::DEFAULT_MIN_HEAL_INTERVAL_S,
                                    camera_box::capture_rate_selfheal::DEFAULT_RECURRENCE_WINDOW_S,
                                    camera_box::capture_rate_selfheal::DEFAULT_CRITICAL_ESCALATION_HEALS,
                                );
                            match selfheal_decision {
                                camera_box::capture_rate_selfheal::SelfHealDecision::Healthy => {
                                    // Unreachable: we only reach this block once should_warn (a
                                    // confirmed sustained deviation) is already true.
                                }
                                camera_box::capture_rate_selfheal::SelfHealDecision::Throttled {
                                    seconds_remaining,
                                } => {
                                    tracing::warn!(
                                        "#663 self-heal rate-limited: the last USB reset attempt was too recent — {}s remaining before another attempt is allowed",
                                        seconds_remaining
                                    );
                                }
                                camera_box::capture_rate_selfheal::SelfHealDecision::Heal {
                                    attempt_number,
                                    escalate_critical,
                                } => {
                                    if escalate_critical {
                                        tracing::error!(
                                            "{}",
                                            camera_box::capture_rate_selfheal::critical_escalation_message(
                                                &device_path_owned,
                                                attempt_number,
                                                grabber_model
                                            )
                                        );
                                    }
                                    if let Err(e) = camera_box::capture_rate_selfheal::save_state(
                                        state_path,
                                        &next_selfheal_state,
                                    ) {
                                        tracing::error!(
                                            "#663 self-heal: failed to persist self-heal state to {}: {} (rate-limit/escalation count may not survive this restart)",
                                            camera_box::capture_rate_selfheal::STATE_PATH,
                                            e
                                        );
                                    }
                                    match camera_box::capture_rate_selfheal::perform_usb_reset(
                                        &device_path_owned,
                                    ) {
                                        Ok(()) => {
                                            tracing::warn!(
                                                "#663 self-heal: USB reset attempt #{} succeeded — will exit (code {}) after graceful shutdown so systemd restarts camera-box against the re-enumerated device",
                                                attempt_number,
                                                camera_box::capture_rate_selfheal::SELF_HEAL_EXIT_CODE
                                            );
                                            running_capture.store(false, Ordering::Relaxed);
                                            pending_self_heal_exit_code = Some(
                                                camera_box::capture_rate_selfheal::SELF_HEAL_EXIT_CODE,
                                            );
                                        }
                                        Err(e) => {
                                            // Review finding (#663): a TOTAL reset failure (e.g.
                                            // the reauthorize retries above all failed) can leave
                                            // the capture device WORSE than the original rate
                                            // defect — possibly fully disconnected. That deserves
                                            // CRITICAL visibility, not a buried error line, and the
                                            // process should still exit so `systemctl status`
                                            // visibly reflects the failure and a fresh process gets
                                            // a clean shot at it next rate-limit window (the state
                                            // file already recorded this attempt, so the 600s
                                            // throttle applies regardless of exiting here).
                                            tracing::error!(
                                                "CRITICAL #663: self-heal USB reset attempt #{} FAILED: {:#} — the capture device may now be in a WORSE state than the original rate defect (possibly disconnected); exiting (code {}) after graceful shutdown so systemd retries with a fresh process",
                                                attempt_number,
                                                e,
                                                camera_box::capture_rate_selfheal::SELF_HEAL_RESET_FAILED_EXIT_CODE
                                            );
                                            running_capture.store(false, Ordering::Relaxed);
                                            pending_self_heal_exit_code = Some(
                                                camera_box::capture_rate_selfheal::SELF_HEAL_RESET_FAILED_EXIT_CODE,
                                            );
                                        }
                                    }
                                }
                            }
                        }

                        // #299 — log the most recent chroma sample alongside the fps report.
                        // A "grayscale" line here means the capture card is delivering
                        // monochrome frames — the automatic regression signal for colour-capture.
                        // Skipped until the first sample lands (no false cold-start reading).
                        if let Some((u_dev, v_dev)) = last_chroma {
                            let colour_label = if camera_box::capture::is_color_frame(u_dev, v_dev)
                            {
                                "colour"
                            } else {
                                "grayscale (source likely monochrome)"
                            };
                            tracing::info!(
                                "capture chroma: u_dev={:.1} v_dev={:.1} -> {}",
                                u_dev,
                                v_dev,
                                colour_label
                            );
                        }

                        frame_count = 0;
                        emit_count = 0;
                        last_report = std::time::Instant::now();
                    }

                    // #297 — re-announce the NDI sender if the host's usable network changed
                    // (boot race / link flap), so the OBS NDI finder rediscovers this box.
                    // Throttled internally to REANNOUNCE_POLL_INTERVAL; a stable network is a
                    // no-op (never re-creates the sender, so steady state is unaffected). The
                    // PRODUCTION (non-burn) sender is the one being discovered; in a burn/E2E
                    // run the sender lives on the burn thread and re-announce is not needed.
                    #[cfg(not(feature = "probe"))]
                    if let Err(e) = sender.maybe_reannounce() {
                        tracing::warn!("#297 NDI sender re-announce check failed: {}", e);
                    }
                    #[cfg(feature = "probe")]
                    if let Some(s) = capture_sender.as_mut() {
                        if let Err(e) = s.maybe_reannounce() {
                            tracing::warn!("#297 NDI sender re-announce check failed: {}", e);
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to capture frame: {}", e);
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
        // #275b — close the burn ring so the burn thread's recv loop ends and it drains the last
        // queued frames. Dropping the producer closes the channel.
        #[cfg(feature = "probe")]
        drop(burn_ring);

        // #279 FIX 2 — flush + close the grab recording BEFORE joining the burn thread. The grab
        // sink is independent of the burn thread, and the join can briefly block on the burn
        // thread's final NDI sends (a momentarily stalled strih OBS). Flushing grab first
        // guarantees the recording is complete and the sidecar rows reach dev1/disk even if the
        // burn-thread join lags — the truncated-grab half of the wedge this fix targets.
        if let Some(rec) = grab_recorder.take() {
            rec.finish();
        }

        // #275b — join the burn thread so the last queued frames are rendered + sent and the NDI
        // sender it owns is destroyed cleanly before shutdown continues.
        #[cfg(feature = "probe")]
        if let Some(h) = burn_handle.take() {
            if let Err(e) = h.join() {
                tracing::error!("#275b cam1-burn thread panicked during shutdown: {:?}", e);
            }
        }
        // #280 — pool audit: how many frame buffers were ever ALLOCATED (vs one `to_vec` per
        // emitted frame before this change) and how many sit idle now. A small allocation count
        // against a long run is the proof the pool recycled instead of churning per-frame heap.
        #[cfg(feature = "probe")]
        if burn_run_id.is_some() {
            tracing::info!(
                "#280 cam1-burn buffer pool: {} total buffers allocated, {} idle at shutdown (recycled across the run instead of per-frame to_vec)",
                burn_pool.allocations(),
                burn_pool.free_len()
            );
        }
        // cam2→cam1 LOSS sidecar: write cam1's final V4L2 capture-drop count so the verdict
        // reports the camera-leg loss (a non-fatal best effort — a write failure only means
        // the verdict can't report cam2→cam1 loss, it must not abort the shutdown).
        if let Some(path) = &capture_stats_path {
            match capture.write_capture_stats(path) {
                Ok(()) => tracing::info!(
                    "cam2→cam1 LOSS sidecar written: {} ({} V4L2 capture-drops over {} captured)",
                    path,
                    capture.dropped_captures(),
                    capture.frames_captured()
                ),
                Err(e) => tracing::error!("failed to write cam2→cam1 capture-stats sidecar: {e:#}"),
            }
        }

        // #663 — self-heal exit, AFTER all the shutdown cleanup above has run (burn ring drain,
        // grab-recorder flush, burn-thread join, capture-stats sidecar write). `main()`'s own
        // shutdown path only ever proceeds past `signal::ctrl_c().await` on an actual Ctrl+C —
        // this capture loop exiting on its own (self-heal) would otherwise leave `main()` awaiting
        // a signal that never comes, so the process must exit explicitly here to actually restart
        // via systemd's `Restart=always`.
        if let Some(code) = pending_self_heal_exit_code {
            tracing::warn!(
                "#663 self-heal: shutdown cleanup complete — exiting now (code {})",
                code
            );
            std::process::exit(code);
        }
    });

    // Wait for shutdown signal
    tracing::info!("Streaming started. Press Ctrl+C to stop.");
    signal::ctrl_c().await?;
    tracing::info!("Shutdown signal received");

    // Signal all threads to stop
    running.store(false, Ordering::Relaxed);

    // Wait for capture loop (with timeout)
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), capture_handle).await;

    // Wait for display thread if running
    if let Some(handle) = display_handle {
        let _ = handle.join();
    }

    // Wait for intercom thread if running
    if let Some(handle) = intercom_handle {
        let _ = handle.join();
    }

    tracing::info!("camera-box stopped");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// #685 follow-up: a live regression guard for the exact bug that let cam1 keep
    /// self-heal-resetting on its own normal wobble even AFTER the #685 binary was
    /// deployed — `os_hostname()` must actually reach the real `gethostname(2)` value
    /// (non-empty on any real machine, incl. this test runner), not silently degrade to
    /// the empty-string failure path every time.
    #[test]
    fn os_hostname_returns_a_real_non_empty_value() {
        let h = os_hostname();
        assert!(
            !h.is_empty(),
            "gethostname(2) must resolve to a real hostname on any machine that can run this test"
        );
    }

    #[test]
    fn test_args_parse_default() {
        // Test that default values are correct
        let args = Args::try_parse_from(["camera-box"]).unwrap();
        assert_eq!(args.config, PathBuf::from("/etc/camera-box/config.toml"));
        assert!(args.device.is_none());
        assert!(args.display_source.is_none());
        assert_eq!(args.fb_device, "/dev/fb0");
        assert!(!args.debug);
        assert!(args.intercom_stream.is_none());
        assert_eq!(args.intercom_target, "strih.lan");
        // #289 — the IRQ-affinity ExecStartPre helper flag is OFF by default
        // (a normal `camera-box` run never touches /proc/irq).
        assert!(!args.setup_irq_affinity);
    }

    #[test]
    fn test_args_parse_with_device() {
        let args = Args::try_parse_from(["camera-box", "--device", "/dev/video2"]).unwrap();
        assert_eq!(args.device, Some("/dev/video2".to_string()));
    }

    #[test]
    fn test_args_parse_with_config() {
        let args = Args::try_parse_from(["camera-box", "-c", "/custom/config.toml"]).unwrap();
        assert_eq!(args.config, PathBuf::from("/custom/config.toml"));
    }

    #[test]
    fn test_args_parse_with_display() {
        let args =
            Args::try_parse_from(["camera-box", "--display", "STRIH-SNV (interkom)"]).unwrap();
        assert_eq!(
            args.display_source,
            Some("STRIH-SNV (interkom)".to_string())
        );
    }

    #[test]
    fn test_args_parse_with_intercom() {
        let args = Args::try_parse_from([
            "camera-box",
            "--intercom",
            "cam1",
            "--intercom-target",
            "192.168.1.100",
        ])
        .unwrap();
        assert_eq!(args.intercom_stream, Some("cam1".to_string()));
        assert_eq!(args.intercom_target, "192.168.1.100");
    }

    #[test]
    fn test_args_parse_debug_flag() {
        let args = Args::try_parse_from(["camera-box", "--debug"]).unwrap();
        assert!(args.debug);
    }

    #[test]
    fn test_args_record_grab_off_by_default() {
        // #105 node 2: --record-grab is OFF unless given (normal operation unaffected).
        let args = Args::try_parse_from(["camera-box"]).unwrap();
        assert!(args.record_grab.is_none());
        assert_eq!(args.record_grab_ts, "/tmp/cam1-grab-ts.csv");
    }

    #[test]
    fn test_args_record_grab_tcp_dest_and_sidecar() {
        let args = Args::try_parse_from([
            "camera-box",
            "--record-grab",
            "tcp://10.77.9.21:9099",
            "--record-grab-ts",
            "/tmp/grab.csv",
        ])
        .unwrap();
        assert_eq!(args.record_grab.as_deref(), Some("tcp://10.77.9.21:9099"));
        assert_eq!(args.record_grab_ts, "/tmp/grab.csv");
    }

    #[test]
    fn test_args_parse_fb_device() {
        let args = Args::try_parse_from(["camera-box", "--fb-device", "/dev/fb1"]).unwrap();
        assert_eq!(args.fb_device, "/dev/fb1");
    }

    #[test]
    fn test_args_command_valid() {
        // Ensure the command can be built
        Args::command().debug_assert();
    }

    #[test]
    fn test_args_all_options() {
        let args = Args::try_parse_from([
            "camera-box",
            "-c",
            "/custom/config.toml",
            "-d",
            "/dev/video3",
            "--display",
            "NDI Source",
            "--fb-device",
            "/dev/fb1",
            "--debug",
            "--intercom",
            "cam2",
            "--intercom-target",
            "host.lan",
        ])
        .unwrap();

        assert_eq!(args.config, PathBuf::from("/custom/config.toml"));
        assert_eq!(args.device, Some("/dev/video3".to_string()));
        assert_eq!(args.display_source, Some("NDI Source".to_string()));
        assert_eq!(args.fb_device, "/dev/fb1");
        assert!(args.debug);
        assert_eq!(args.intercom_stream, Some("cam2".to_string()));
        assert_eq!(args.intercom_target, "host.lan");
    }

    // --- #528: resolve_display_config — the HDMI cameraman preview is UNCONDITIONAL ------------
    //
    // The reported bug: cam1 had no `--display` CLI flag and no `[display]` config.toml section,
    // so the display thread never started at all — no cameraman preview, at all, ever. The fix
    // makes every cambox preview `DEFAULT_DISPLAY_SOURCE` unless a CLI flag or config section
    // explicitly overrides it, and adds a single opt-out (`CAMERA_BOX_NO_DISPLAY`) for the E2E
    // harness's QR painter to reclaim /dev/fb0.

    #[test]
    fn resolve_display_config_defaults_to_the_fleet_wide_interkom_source_when_unconfigured() {
        // #528 headline: NO CLI flag, NO config.toml [display] section (cam1's exact live state
        // before this fix) must still resolve to a display config — never `None`.
        let cfg = resolve_display_config(None, "/dev/fb0", None, false).expect(
            "the HDMI cameraman preview must be unconditional — Some even with no CLI flag and \
             no [display] config section (#528: this was the exact cam1 bug)",
        );
        assert_eq!(cfg.source_name, DEFAULT_DISPLAY_SOURCE);
        assert_eq!(cfg.fb_device, "/dev/fb0");
        assert_eq!(cfg.find_timeout_secs, 30);
    }

    #[test]
    fn resolve_display_config_cli_flag_overrides_the_default() {
        let cfg = resolve_display_config(Some("Custom Source"), "/dev/fb1", None, false)
            .expect("Some when an explicit --display source is given");
        assert_eq!(cfg.source_name, "Custom Source");
        assert_eq!(cfg.fb_device, "/dev/fb1");
    }

    #[test]
    fn resolve_display_config_config_toml_overrides_the_default_when_no_cli_flag() {
        let display = DisplayConfig {
            source: "Config Source".to_string(),
            fb_device: "/dev/fb2".to_string(),
        };
        let cfg = resolve_display_config(None, "/dev/fb0", Some(&display), false)
            .expect("Some when a config.toml [display] section is given");
        assert_eq!(cfg.source_name, "Config Source");
        assert_eq!(cfg.fb_device, "/dev/fb2");
    }

    #[test]
    fn resolve_display_config_cli_flag_wins_over_config_toml() {
        // Pre-existing precedence rule (unchanged by #528): CLI overrides config.
        let display = DisplayConfig {
            source: "Config Source".to_string(),
            fb_device: "/dev/fb2".to_string(),
        };
        let cfg = resolve_display_config(Some("CLI Source"), "/dev/fb1", Some(&display), false)
            .expect("Some");
        assert_eq!(cfg.source_name, "CLI Source");
        assert_eq!(cfg.fb_device, "/dev/fb1");
    }

    #[test]
    fn resolve_display_config_no_display_opt_out_disables_display_entirely() {
        // #528: rig-mode.sh test sets CAMERA_BOX_NO_DISPLAY so the QR painter can reliably own
        // /dev/fb0 — this must win even when a CLI flag or config section is also present,
        // replacing the old "bare ExecStart == no display thread" toggle that #528 breaks.
        assert!(resolve_display_config(Some("Whatever"), "/dev/fb0", None, true).is_none());
        assert!(resolve_display_config(None, "/dev/fb0", None, true).is_none());
        let display = DisplayConfig {
            source: "Config Source".to_string(),
            fb_device: "/dev/fb2".to_string(),
        };
        assert!(resolve_display_config(None, "/dev/fb0", Some(&display), true).is_none());
    }
}
