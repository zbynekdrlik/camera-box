use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    } else if config.device == "auto" {
        // #828 — auto-detect with a slow, clearly-logged IN-PROCESS retry instead of bailing.
        // A box whose USB grabber is absent (removed / dead / not yet fitted) settles into a
        // quiet retry — one clear "no capture device — check the grabber" line per cycle — and
        // auto-recovers within one interval when a grabber (re-)appears, rather than exiting into
        // a ~3 s `Restart=always` storm (cam4 incident: NRestarts=27719). `RestartSec=3` stays
        // fast for a genuine mid-stream transient crash; the systemd StartLimit is only a
        // belt-and-braces cap on any OTHER runaway.
        camera_box::no_device::wait_for_capture_device(
            camera_box::config::find_capture_device_opt,
            std::thread::sleep,
        )
    } else {
        // An explicitly-configured device path is used as-is (existing behaviour).
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
    let effective_emit_fps = send_rate.numerator as f64 / send_rate.denominator.max(1) as f64;
    // Review finding (#792): a sub-60 emit path breaks the blend premise itself (pairs are no
    // longer adjacent ~16.7ms exposures) AND would halve the output under a 30/1 label — hard
    // OPT-OUT, not warn-and-proceed. 59.0 so a 60000/1001 capture is not spuriously disabled.
    let mut publish_30p_tee: Option<camera_box::publish_30p::Tee> = if publish_30p_cfg.enabled
        && effective_emit_fps < 59.0
    {
        tracing::warn!(
            "#792 publish-30p requires a ~60fps emit path (effective {:.1} fps) — feature DISABLED, 60p unaffected",
            effective_emit_fps
        );
        None
    } else if publish_30p_cfg.enabled {
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
                            "#792 publish-30p ACTIVE: streaming as '{}' (blend={}, channel depth {})",
                            name_30p,
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

    // #945 — capture-wedge self-watchdog. `VideoCapture::process_frame`'s blocking V4L2 dequeue
    // can wedge (never return) on a USB completion-handler fault, freezing the ENTIRE capture
    // loop below at that one call site — including every existing observability check
    // (capture_stall, capture_rate_health/selfheal, the 5s stats tick), all of which only run
    // once process_frame() returns. So the watchdog MUST live on a genuinely separate thread that
    // can never itself be blocked by the same dequeue: it polls a monotonic heartbeat the capture
    // loop updates immediately after every process_frame() call returns (Ok or Err — see
    // `capture_wedge`'s module doc for why that is content-agnostic and never fires on a genuine
    // no-signal condition), and forces a restart the moment that heartbeat has gone stale for
    // `capture_wedge::CAPTURE_WEDGE_THRESHOLD_S`.
    let wedge_heartbeat_ns = Arc::new(AtomicU64::new(0));
    let wedge_watchdog_epoch = std::time::Instant::now();
    // #944 — emit-liveness heartbeat, stamped by the capture loop ONLY when a good frame is
    // actually emitted (below). 0 = "no frame emitted yet" (disarmed): the emit-freeze check is
    // skipped until the first emit, so boot/NDI warmup never false-fires (a never-emits-from-boot
    // box is #945's + the #747 frozen-camera preflight's domain, not this watchdog's). Polled by
    // the SAME watchdog thread, AFTER the #945 wedge check, so a true dequeue wedge is diagnosed
    // by #945 and only a capture-alive-but-emit-dead freeze trips this. See `emit_freeze`.
    let emit_heartbeat_ns = Arc::new(AtomicU64::new(0));
    {
        let heartbeat = Arc::clone(&wedge_heartbeat_ns);
        let emit_heartbeat = Arc::clone(&emit_heartbeat_ns);
        let running_watchdog = Arc::clone(&running);
        std::thread::Builder::new()
            .name("capture-wedge-watchdog".into())
            .spawn(move || {
                // Poll well inside the wedge threshold so the watchdog itself can never add more
                // than one poll interval of detection latency.
                let poll_interval = std::time::Duration::from_secs(5);
                while running_watchdog.load(Ordering::Relaxed) {
                    std::thread::sleep(poll_interval);
                    if !running_watchdog.load(Ordering::Relaxed) {
                        break; // normal shutdown in progress — never misreport as a wedge
                    }
                    let now_ns = wedge_watchdog_epoch.elapsed().as_nanos() as u64;
                    let last_progress_ns = heartbeat.load(Ordering::Relaxed);
                    let seconds_since_last_progress =
                        now_ns.saturating_sub(last_progress_ns) as f64 / 1_000_000_000.0;
                    if camera_box::capture_wedge::evaluate_wedge(
                        seconds_since_last_progress,
                        camera_box::capture_wedge::CAPTURE_WEDGE_THRESHOLD_S,
                    ) == camera_box::capture_wedge::WedgeVerdict::Wedged
                    {
                        tracing::error!(
                            "{}",
                            camera_box::capture_wedge::capture_wedge_message(
                                seconds_since_last_progress,
                                camera_box::capture_wedge::CAPTURE_WEDGE_THRESHOLD_S,
                            )
                        );
                        // The capture loop is provably dead (its own blocking dequeue never
                        // returned) — a graceful in-process shutdown of ITS state is not
                        // reachable from here, so exit immediately. systemd's Restart=always
                        // (camera-box.service, unchanged) brings the process back up against a
                        // fresh device open.
                        std::process::exit(camera_box::capture_wedge::CAPTURE_WEDGE_EXIT_CODE);
                    }

                    // #944 — emit-freeze check. Reached only when #945 said the capture thread is
                    // NOT wedged (its dequeue is returning). `seconds_since_last_progress` above is
                    // the capture-return staleness; combined with the emit heartbeat's staleness it
                    // discriminates a frozen OUTPUT (dequeue returning, no good frame emitted — e.g.
                    // a corrupted-buffer stream) from a thread wedge (#945's domain). Disarmed until
                    // the first emit (heartbeat == 0).
                    let last_emit_ns = emit_heartbeat.load(Ordering::Relaxed);
                    if last_emit_ns != 0 {
                        let seconds_since_last_emit =
                            now_ns.saturating_sub(last_emit_ns) as f64 / 1_000_000_000.0;
                        if camera_box::emit_freeze::evaluate_emit_freeze(
                            seconds_since_last_emit,
                            seconds_since_last_progress,
                            camera_box::emit_freeze::EMIT_FREEZE_THRESHOLD_S,
                            camera_box::emit_freeze::CAPTURE_FRESH_BOUND_S,
                        ) == camera_box::emit_freeze::EmitFreezeVerdict::Frozen
                        {
                            tracing::error!(
                                "{}",
                                camera_box::emit_freeze::emit_freeze_message(
                                    seconds_since_last_emit,
                                    seconds_since_last_progress,
                                    camera_box::emit_freeze::EMIT_FREEZE_THRESHOLD_S,
                                )
                            );
                            // The capture thread is alive but the NDI output is frozen — a graceful
                            // in-process teardown of the sender is not cleanly reachable from here
                            // (it may be owned by the burn thread), so exit immediately. systemd's
                            // Restart=always tears the sender down (source goes gone, not frozen)
                            // and re-opens the device — the same recovery shape as #945.
                            std::process::exit(camera_box::emit_freeze::EMIT_FREEZE_EXIT_CODE);
                        }
                    }
                }
            })
            .expect("failed to spawn #945 capture-wedge watchdog thread");
    }

    // Spawn capture loop in blocking task - minimal overhead for lowest latency
    let running_capture = Arc::clone(&running);
    // #944 — the capture loop stamps this on every actual emit; `wedge_watchdog_epoch` (Copy)
    // moves into the closure so both the stamp here and the watchdog poll share one epoch.
    let emit_heartbeat_capture = Arc::clone(&emit_heartbeat_ns);
    let capture_handle = tokio::task::spawn_blocking(move || {
        // Apply real-time optimizations BEFORE entering the capture loop
        apply_realtime_optimizations();

        let mut frame_count: u64 = 0; // frames captured this report window
        let mut emit_count: u64 = 0; // frames actually sent to NDI this window
        let mut last_report = std::time::Instant::now();
        // #1200 — capture-side byte-identical dupe fraction for the latch-halving detector. Counted
        // per 5s report window (reusing the SAME #889 content_hash, no change to the decimation
        // gate) and fed to the tracker in the report block; reset with frame_count. prev_capture_hash
        // PERSISTS across windows (a window-boundary frame can be a dupe of the previous window's
        // last captured frame).
        let mut prev_capture_hash: Option<u64> = None;
        let mut window_dupe_captures: u64 = 0;
        let mut window_total_captures: u64 = 0;
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
        // #1079 — the per-frame spatial-roughness metric sampled alongside `last_chroma`,
        // so the `capture chroma:` line can carry a `rough=` term the dev1 splitter-port
        // watchdog reads to catch the Elgato purple-noise no-signal mode (colourful but
        // unstructured) that the colour/grayscale label alone misses. `None` until the
        // first sample lands (in lockstep with `last_chroma`).
        let mut last_roughness: Option<f32> = None;
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
        // counter/tolerance for a deviation that stays comfortably inside the wide jitter-band
        // tolerance above (#685's correct-for-short-bursts widening, which #717 leaves
        // unchanged). The SAME streak is checked against TWO thresholds below (#971): confirmed
        // at 60s (`SUSTAINED_WARN_WINDOWS`) it is informational-only; once it also reaches the
        // much longer `CHRONIC_SUSTAINED_WARN_WINDOWS` (15 min) bar it trips self-heal too. One
        // shared counter (not two) is deliberate — both checks are fed the exact SAME
        // `sustained_deviant` flag from the exact same starting state, so a second counter would
        // always equal this one; checking one streak against two thresholds is simpler and can
        // never silently drift out of sync with itself. See `capture_rate_health::
        // sustained_tolerance_pct_for_model`'s doc and `capture_rate_selfheal::
        // should_trigger_selfheal`'s doc for the full root-cause -> approach writeup.
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

        // #1128 — fast-capture grabber STUCK detector (ShadowCast ~62.5 fps + persistent
        // corrupted). Keys on the COMBINED signature (over-rate AND persistent corrupted, both
        // sustained), which the existing #656/#971 bands miss: the jitter band is deliberately
        // wide for ShadowCast (never fires at 62.5) and the chronic band waits 15 min, while the
        // corrupted-frame counter feeds no decision at all. The corrupted band is the
        // discriminator so a benign over-rate wobble (0 corrupted, absorbed by the decimation
        // gate) never fires. On a STUCK verdict the `#1128 grabber STUCK` marker is ALWAYS logged
        // (the dev1 alert watchdog greps it); the actual USB re-auth reuses the SAME #663
        // rate-limited path but is gated OFF by default — set CAMERA_BOX_GRABBER_STUCK_SELFHEAL=1
        // to enable live re-auth (a deliberate opt-in, so enabling it on the rig is a supervised
        // step). Fed one sample per 5 s report window below.
        let mut grabber_stuck_tracker = camera_box::grabber_stuck::GrabberStuckTracker::new();
        let grabber_stuck_selfheal_enabled = std::env::var("CAMERA_BOX_GRABBER_STUCK_SELFHEAL")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        // #1193 — sustained OVER-RATE detector (the 3rd self-heal trigger). Keys on the COMBINED
        // signature over-rate (a majority of the cap-1s buckets >= 61) AND dupe-victim shed churn,
        // both sustained ~5 min — the state whose manual USB re-auth cure decays in ~2h, which the
        // #656/#971 bands (wide jitter tolerance / decoupled sustained) and the #1128 STUCK band
        // (requires corrupted frames) all miss. The churn band is the discriminator (a benign
        // over-rate wobble sheds 0). On OverRate the `#1193 grabber OVER-RATE` marker is ALWAYS
        // logged (report-only); the actual USB re-auth reuses the SAME shared self-heal throttle
        // path but is gated OFF by default — set CAMERA_BOX_GRABBER_OVERRATE_SELFHEAL=1 to enable
        // live re-auth (a deliberate opt-in, canary-armed on cam2 as a supervised step) — AND a
        // 30-min per-trigger cooldown floor guards it beyond the shared throttle. Fed one sample
        // per 5 s report window below.
        let mut over_rate_tracker = camera_box::capture_overrate::CaptureOverRateTracker::new();
        let over_rate_selfheal_enabled = std::env::var("CAMERA_BOX_GRABBER_OVERRATE_SELFHEAL")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        // #1200 — cam3 ShadowCast LATCH-HALVING detector: the 4th self-heal trigger. Keys on the
        // capture-side byte-identical dupe FRACTION (healthy 30fps-into-60fps ~0.5, latch-halved
        // ~0.75), the blind spot the #1193 over-rate (0 over-rate, 0 shed churn) and #1128 STUCK
        // (0 corrupted) triggers both miss. On Halved the `#1200 grabber LATCH-HALVING` marker is
        // ALWAYS logged (report-only, no I/O); the actual USB re-auth reuses the SAME shared
        // self-heal throttle path but is gated OFF by default — set CAMERA_BOX_GRABBER_HALVING_SELFHEAL=1
        // to enable it (a deliberate opt-in; the re-auth cure is UNPROVEN for latch-halving, so the
        // marker's detection value is the primary deliverable) — AND a 30-min per-trigger cooldown
        // floor guards it beyond the shared throttle. Fed one sample per 5 s report window below.
        let mut latch_halving_tracker =
            camera_box::capture_latch_halving::CaptureLatchHalvingTracker::new();
        let latch_halving_selfheal_enabled = std::env::var("CAMERA_BOX_GRABBER_HALVING_SELFHEAL")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

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
        // (#889) dupe-preferring decimation: owns the boundary + content-dupe-preference
        // bookkeeping (replaces the bare `next_boundary_ns` local) so a fast/over-rate
        // grabber's own internal-buffer repeat is preferentially shed over the genuine unique
        // tick next to it. See `camera_box::dupe_decimation`'s module doc for the full mechanism.
        let mut decimation_gate = camera_box::dupe_decimation::DecimationGate::new();

        while running_capture.load(Ordering::Relaxed) {
            // #707 B1 — snapshot the emit counter before the frame closure so the per-second ring
            // can attribute exactly this frame's emit (0 or 1) after it returns.
            let emit_before = emit_count;
            // #944 — did THIS iteration actually DISPATCH a good frame to NDI (a confirmed
            // production send, or a queued burn job)? This is the emit-liveness signal, distinct
            // from `emit_count` (which increments even on a production send Err, for rate stats):
            // a persistently-failing send is itself a silent-frozen mode, so it must NOT advance
            // the heartbeat. Reset per iteration; set inside the closure only on a confirmed
            // dispatch.
            let mut frame_dispatched = false;
            // #1167 — snapshot the corrupted-buffer counter before process_frame. A corrupted
            // buffer (V4L2_BUF_FLAG_ERROR / short) is DROPPED inside process_frame BEFORE the
            // callback (capture.rs), so it never reaches decimation_gate.poll below: at an over-rate
            // that removes a would-be-emitted GOOD frame from the stream, and the over-rate shed
            // machinery then SKIPS its 60 fps slot (a strih FIFO hold -> the cam1 align sawtooth).
            // A delta after the call means this iteration dropped one; we register a bounded make-up
            // so the gate reclaims exactly that slot with the nearest good frame on its next shed.
            let corrupted_before = capture.corrupted_frames();
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
                    // #1079 — sample the spatial-roughness term on the SAME frame, so
                    // `rough=` and `u_dev/v_dev` are always in lockstep on the log line.
                    last_roughness = Some(camera_box::capture::luma_roughness(
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
                    // (#889) dupe-preferring decimation: pacing still decides WHEN a captured
                    // frame must be shed (`genlock_pacing::genlock_emit_gate`, unchanged); the content hash
                    // now decides WHICH captured frame is the victim — prefer shedding a
                    // grabber-repeat dupe over the unique tick captured right next to it. See
                    // `dupe_decimation`'s module doc for the full root-cause -> fix writeup.
                    let prev_boundary_ns = decimation_gate.next_boundary_ns();
                    // (#1145 round 3) one pass yields BOTH the exact content_hash (byte-identical
                    // buffer-repeat dupes, e.g. CAM1) AND a luma lattice for the noise-tolerant
                    // compare a marginal jittery over-rate card needs — its surplus is a noisy
                    // optical RE-SAMPLE of the same painted frame (sensor noise), which the exact
                    // hash misses, so it would be emitted as a "unique" (a held painted-id) forcing
                    // a compensating shed (a skipped painted-id) = the Δ1/Δ3 cadence churn.
                    let (content_hash, content_luma) = camera_box::dupe_decimation::dupe_content_sig(
                        data,
                        info.width as usize,
                        info.height as usize,
                        info.stride as usize,
                    );
                    // #1200 — count the capture-side byte-identical dupe fraction for the
                    // latch-halving detector (reusing the SAME content_hash just computed; this
                    // does NOT change any decimation decision below). A window at ~0.75 dupe
                    // fraction = each unique frame captured ~4x (15 unique/s in a 60fps stream);
                    // healthy 30fps-into-60fps is ~0.5. Accumulated per 5s report window, drained +
                    // reset there. prev_capture_hash persists across windows on purpose.
                    if prev_capture_hash == Some(content_hash) {
                        window_dupe_captures = window_dupe_captures.saturating_add(1);
                    }
                    prev_capture_hash = Some(content_hash);
                    window_total_captures = window_total_captures.saturating_add(1);
                    // #1131 — did THIS frame come from a NON-EMPTY V4L2 queue (the driver already
                    // had it buffered, i.e. its blocking dequeue returned in well under one capture
                    // interval)? A buffered frame PROVES a real captured frame exists to fill the
                    // next un-emitted boundary, so the gate catches up one interval instead of
                    // grid-resyncing past it (the #1131 multi-slot-skip judder on a sick/wobbly
                    // grabber, whose 0-capture-dropped signature confirms the frames exist). A
                    // frame from an empty queue (the loop genuinely waited — a device/clock gap)
                    // keeps the pre-existing #131 forward-resync. Same `dequeue_duration_ms` signal
                    // the #707 capture-stall WARN reads, thresholded the other way.
                    let queue_had_frame = if configured_capture_fps > 0.0 {
                        camera_box::capture_stall::frame_from_nonempty_queue(
                            info.dequeue_duration_ms,
                            1000.0 / configured_capture_fps,
                        )
                    } else {
                        false
                    };
                    // #1145 v2 — the MONOTONIC clocks the queue-depth drain needs: `now_mono` is
                    // read once here, `capture_mono` is the V4L2 buffer's own CLOCK_MONOTONIC
                    // capture instant (`FrameInfo::capture_monotonic_100ns`, 100ns units; 0 = no
                    // real measurement -> the drain self-disables for this frame). Their difference
                    // is this frame's queue residence, and consecutive capture instants feed the
                    // capture-takt EMA (both monotonic, immune to the DanteSync realtime steps
                    // `wall_clock_ns()` grids the emit boundary to).
                    let now_mono_ns = monotonic_clock_ns();
                    let capture_mono_ns = (info.capture_monotonic_100ns.max(0) as u64) * 100;
                    // (#1145 round 3) stage this frame's luma lattice for poll's noise-tolerant dupe
                    // compare (armed only under sustained over-rate, never two consecutive frames).
                    // Called immediately before poll, mirroring the hash for this same frame.
                    decimation_gate.note_frame_luma(content_luma);
                    let emit = decimation_gate.poll(
                        wall_clock_ns(),
                        out_interval_ns,
                        content_hash,
                        queue_had_frame,
                        now_mono_ns,
                        capture_mono_ns,
                    );
                    let next_boundary_ns = decimation_gate.next_boundary_ns();
                    // #707 — a clock discontinuity (DanteSync NTP/PTP step, or a stalled poll)
                    // can leap the gate's boundary past one or more intervals that are then
                    // NEVER emitted — the missing direct evidence for whether a clock step is
                    // what's behind a #666/#707-class transient emit-rate deficit. See
                    // `genlock_pacing::boundary_skip_count`'s own doc comment.
                    // (#1145 v2.1) DEDUCT the fast-drain's INTENTIONAL extra boundary advance: a
                    // FastDrain retires an already-stale boundary (advances +2), which
                    // `boundary_skip_count` would otherwise report as ONE un-emitted-content SKIP —
                    // the sick-leg / clock-step signature `leg-health-guard.sh` hard-fails on. An
                    // intentional drain is NOT that, so it must contribute 0 to the #707 diagnostic.
                    let skipped = camera_box::genlock_pacing::boundary_skip_count(
                        prev_boundary_ns,
                        next_boundary_ns,
                        out_interval_ns,
                    )
                    .saturating_sub(decimation_gate.last_poll_intentional_extra_advance());
                    if skipped > 0 {
                        // #752 — do NOT log per skip (that was the ~10/s storm). Accumulate; the
                        // 5s Streaming report below drains ONE aggregated WARN with the count.
                        emit_skip_log.record(skipped);
                    }
                    if !emit {
                        return; // decimated -- either blind pacing or a preferred dupe shed
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
                // (#1167 v4) ONE send path (burn or production) for BOTH the current frame and the
                // empty-queue starvation repeats below, parameterized by the genlock emit timecode.
                // Factored so a repeat and the current frame can never drift in how they send. Every
                // capture of `emit_one` is disjoint from the per-poll `decimation_gate` borrow above.
                let mut emit_one = |emit_timecode_100ns: i64| {
                    // #275b ASYNC BURN PATH (test mode: probe + CAMERA_BOX_BURN_RUN_ID).
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
                            // #286 BUG SITE #1 FIX — the emitted frame's genlock NDI timecode keys on
                            // the real CAPTURE instant (`capture_realtime_100ns`), not an arrival-based
                            // boundary. `gen_ts_ns` stays arrival (`emit_wall_ns`) unchanged — it feeds
                            // the #624/#625 latency measurement. (#1167 v4) `emit_timecode_100ns` is
                            // the per-frame boundary timecode (the current frame, or a repeat's own
                            // earlier boundary), so every send lands in its own downstream FIFO slot.
                            gen_ts_ns: emit_wall_ns,
                            emit_timecode_100ns,
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
                            // #944 — a queued burn job is the strongest emit-liveness signal available
                            // on this thread (the burn thread performs the actual NDI send asynchronously).
                            Ok(()) => {
                                emit_count += 1;
                                frame_dispatched = true;
                            }
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

                    // PRODUCTION / non-burn path: zero-copy direct send. Under the probe build the
                    // sender lives in `capture_sender`; it moves to the burn thread ONLY when the burn
                    // is active, and then the handoff above handles every frame and returns — so this
                    // path is reached only when the burn is inactive (`capture_sender` = Some).
                    #[cfg(feature = "probe")]
                    let sender = capture_sender
                        .as_mut()
                        .expect("capture_sender is present whenever the burn is inactive");
                    // #286 BUG SITE #2 FIX — pass the CAPTURE-based genlock timecode through so
                    // send_frame_zero_copy stamps the real capture instant (or the repeat's own
                    // boundary), never re-deriving an arrival-based boundary at send time.
                    match sender.send_frame_zero_copy(data, info, emit_timecode_100ns) {
                        // #944 — only a CONFIRMED send proves the NDI output is live; a send that errors
                        // is itself a silent-frozen mode (nothing reaches NDI while every health signal
                        // stays green), so it must NOT advance the emit-liveness heartbeat.
                        Ok(()) => frame_dispatched = true,
                        Err(e) => tracing::error!("Failed to send frame: {}", e),
                    }
                    emit_count += 1; // reached only when the frame passed the gate
                    tee_grab(data);
                    // #792 — tee the emitted frame to the optional 30p publisher LAST (one bounded
                    // memcpy + try_send, drop-on-full: never blocks this 60p hot path). Production
                    // path only — the probe burn path above returns before reaching here.
                    if let Some(t) = publish_30p_tee.as_mut() {
                        t.tee(data, info, emit_timecode_100ns);
                    }
                };

                // The current frame's CAPTURE-based genlock timecode (the base boundary).
                let capture_timecode_100ns = camera_box::genlock_stamp::genlock_emit_timecode_100ns(
                    capture_realtime_100ns,
                    // `emit_wall_ns` is nanoseconds; this parameter is 100ns units.
                    emit_wall_ns / 100,
                    send_fps as i64,
                );
                // (#1167 v4) An UNDER-rate dip left `starvation_repeats` empty-queue 60fps boundaries
                // unfilled (poll reported them, capped + gated on a measured sustained under-rate). Fill
                // each by re-emitting the CURRENT good frame (it passed process_frame's corruption
                // check — never corrupted content), EARLIEST slot first with its own boundary timecode,
                // THEN emit the current frame. `0` in every healthy / over-rate window, so the loop is a
                // no-op and the emit is byte-identical to the pre-v4 single send there.
                let starvation_repeats = decimation_gate.last_poll_starvation_repeats();
                for j in (1..=starvation_repeats).rev() {
                    emit_one(camera_box::genlock_pacing::starvation_repeat_timecode_100ns(
                        capture_timecode_100ns,
                        j,
                        send_fps as i64,
                    ));
                }
                emit_one(capture_timecode_100ns);
            });

            // #945 — heartbeat: `capture.process_frame(...)` above just RETURNED (Ok or Err,
            // checked below) — proof the blocking V4L2 dequeue is not wedged. Updated
            // unconditionally on EITHER outcome, and unconditional on whether a frame was
            // actually emitted/interesting inside the closure above, so a genuine no-signal
            // condition (the device still delivering blank frames or a fast repeating error)
            // never itself starves the heartbeat — see `capture_wedge`'s module doc.
            wedge_heartbeat_ns.store(
                wedge_watchdog_epoch.elapsed().as_nanos() as u64,
                Ordering::Relaxed,
            );

            // #1167 — a corrupted buffer was dropped this iteration (before the emit gate, see the
            // snapshot above). Only meaningful while genlock decimation is active (out_interval_ns
            // > 0 — the gate is polled); register one make-up so the gate fills the slot the dropped
            // frame vacated with the nearest good frame instead of letting the over-rate absorption
            // skip it. Bounded inside the gate, so this never over-emits.
            if out_interval_ns > 0 && capture.corrupted_frames() > corrupted_before {
                decimation_gate.note_corrupted_frame();
            }

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
                    // #944 — stamp the emit-liveness heartbeat when a good frame was actually
                    // DISPATCHED to NDI this iteration (`frame_dispatched`: a confirmed production
                    // send, or a queued burn job — NOT merely a gate-passed frame whose send
                    // errored). A corrupted buffer returns Ok without dispatching, and a
                    // persistently-failing send never dispatches either, so this never advances on
                    // any frozen-output stream — exactly the signal #945's return-based heartbeat
                    // cannot see. Shared #945 watchdog epoch so the poll can subtract it.
                    if frame_dispatched {
                        emit_heartbeat_capture.store(
                            wedge_watchdog_epoch.elapsed().as_nanos() as u64,
                            Ordering::Relaxed,
                        );
                    }
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
                        // #1193 — the dupe-victim shed count this window, hoisted here so the
                        // over-rate self-heal trigger below (outside the `out_interval_ns > 0`
                        // block) can read it. It is drained from `take_shed_counts()` exactly once
                        // (inside that block); a non-genlock box (out_interval_ns == 0) never drains
                        // it, so it stays 0 → the over-rate trigger's churn band never confirms,
                        // which is correct (no decimation gate → no dupe-victim sheds).
                        let mut window_dupe_shed: u64 = 0;
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

                            // (#889) periodic mechanism-visibility log — proves on a live box
                            // that dupe-preferring decimation is actually shedding grabber
                            // dupes (vs the pre-existing blind pacing drop), on the SAME 5s
                            // cadence as the #752 emit-gate-skip summary below. Printed every
                            // window (never suppressed on 0/0 — a healthy card legitimately
                            // shows 0/0, which is the self-neutralizing behavior by design, not
                            // the mechanism being off).
                            let (
                                dupe_shed,
                                blind_shed,
                                dupe_emitted,
                                retired,
                                drained,
                                fast_drained,
                            ) = decimation_gate.take_shed_counts();
                            // #1193 — capture the dupe-victim shed count for the over-rate trigger
                            // below (this is the ONLY drain of this counter per window).
                            window_dupe_shed = dupe_shed;
                            // (#1167 v4) the starvation last-frame-repeat count is drained SEPARATELY
                            // (the 6-tuple above is byte-frozen) and appended to the summary segment.
                            let starvation_repeats = decimation_gate.take_starvation_repeats();
                            tracing::info!(
                                "{}",
                                camera_box::dupe_decimation::dupe_shed_summary(
                                    dupe_shed,
                                    blind_shed,
                                    dupe_emitted,
                                    retired,
                                    drained,
                                    fast_drained,
                                    starvation_repeats,
                                    5
                                )
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

                        // #944 — surface the age of the last EMITTED good frame on this same 5s
                        // cadence so a frozen output is visible in journald directly, without
                        // diffing successive `Streaming:` timestamps. Computed from the same u64
                        // heartbeat + shared #945 watchdog epoch the emit-freeze watchdog polls.
                        let last_emit_hb_ns = emit_heartbeat_capture.load(Ordering::Relaxed);
                        if last_emit_hb_ns == 0 {
                            tracing::info!("#944 last-emit-age: n/a (no frame emitted yet)");
                        } else {
                            let now_hb_ns = wedge_watchdog_epoch.elapsed().as_nanos() as u64;
                            let last_emit_age_s =
                                now_hb_ns.saturating_sub(last_emit_hb_ns) as f64 / 1_000_000_000.0;
                            tracing::info!(
                                "#944 last-emit-age: {:.1}s (emit-freeze watchdog restarts at {:.0}s of no emit while capture stays alive)",
                                last_emit_age_s,
                                camera_box::emit_freeze::EMIT_FREEZE_THRESHOLD_S,
                            );
                        }

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

                        // #971 — CHRONIC-band check: the SAME streak (`consecutive_sustained_
                        // breaches`) as the 60s sustained-confirm above, just held to the much
                        // longer `CHRONIC_SUSTAINED_WARN_WINDOWS` (15 min) bar before it counts.
                        // See `capture_rate_selfheal::should_trigger_selfheal`'s doc for the full
                        // root-cause -> approach writeup.
                        let sustained_chronic = camera_box::capture_rate_health::should_warn(
                            consecutive_sustained_breaches,
                            camera_box::capture_rate_health::CHRONIC_SUSTAINED_WARN_WINDOWS,
                        );

                        // #909/#971 — the SUSTAINED band alone (a chronic-looking over-rate that
                        // stays inside the wide jitter envelope, e.g. cam1's 62-64fps) is EXPECTED
                        // grabber behavior for a SHORT while — the genlock decimation gate above
                        // absorbs any capture over-rate into exact NDI output BY DESIGN. Log it
                        // informational-only while it's merely sustained, not yet chronic (guarded
                        // `!sustained_chronic` so this line never claims "no USB reset triggered"
                        // in the same window one actually does — see the #971 CHRONIC block
                        // below). Only when the JITTER band ALSO confirms (genuinely beyond
                        // #685's widened per-model tolerance), OR the sustained deviation has
                        // become genuinely CHRONIC, does self-heal actually act.
                        if sustained_confirmed && !jitter_confirmed && !sustained_chronic {
                            tracing::info!(
                                "#717 capture-delivery-rate SUSTAINED band confirmed (informational at THIS tier — a USB reset AUTO-ESCALATES once this sustained deviation becomes chronic, see #971): {:.2} fps captured vs {:.2} fps configured/negotiated (>{:.1}% deviation sustained for {} consecutive report windows, ~{}s) — inside {}'s wide {:.1}% jitter-tolerant envelope; the genlock decimation gate absorbs this over-rate into exact NDI output by design, so NO USB reset is triggered yet (see #971: escalates to a reset if this persists to {}s)",
                                cap_fps,
                                configured_capture_fps,
                                sustained_rate_tolerance_pct,
                                consecutive_sustained_breaches,
                                consecutive_sustained_breaches as u64 * 5,
                                grabber_model,
                                capture_rate_tolerance_pct,
                                camera_box::capture_rate_health::CHRONIC_SUSTAINED_WARN_WINDOWS
                                    as u64
                                    * 5
                            );
                        }

                        if camera_box::capture_rate_selfheal::should_trigger_selfheal(
                            jitter_confirmed,
                            sustained_chronic,
                        ) {
                            if jitter_confirmed {
                                // #971 review finding: when BOTH bands confirm the same window,
                                // say so explicitly — otherwise the log reads as a plain jitter
                                // event and hides that the sustained band has ALSO gone chronic
                                // (harmless either way, since one reset covers both, but worth
                                // naming for anyone reading the journal later).
                                let chronic_note = if sustained_chronic {
                                    " (the sustained band is ALSO chronic this window, see #971 \
                                       — the same reset covers both)"
                                } else {
                                    ""
                                };
                                tracing::warn!(
                                    "#656 capture-delivery-rate DEFECTIVE: {:.2} fps captured vs {:.2} fps configured/negotiated (>{:.1}% deviation sustained for {} consecutive report windows, ~{}s, {} tolerance) — USB-reset the capture device (see #656, #685){}",
                                    cap_fps,
                                    configured_capture_fps,
                                    capture_rate_tolerance_pct,
                                    consecutive_rate_breaches,
                                    consecutive_rate_breaches as u64 * 5,
                                    grabber_model,
                                    chronic_note
                                );
                            } else {
                                tracing::warn!(
                                    "#971 capture-delivery-rate CHRONIC sustained-band DEFECTIVE: {:.2} fps captured vs {:.2} fps configured/negotiated (>{:.1}% deviation held for {} consecutive report windows, ~{}s — beyond the {}s chronic bar) — USB-reset the capture device (see #971, #909, #717)",
                                    cap_fps,
                                    configured_capture_fps,
                                    sustained_rate_tolerance_pct,
                                    consecutive_sustained_breaches,
                                    consecutive_sustained_breaches as u64 * 5,
                                    camera_box::capture_rate_health::CHRONIC_SUSTAINED_WARN_WINDOWS
                                        as u64
                                        * 5
                                );
                            }

                            // #663/#1149 — self-heal via the shared `attempt_self_heal` helper: the
                            // #656 fix (a manual USB reset) is only TEMPORARY — the same defect
                            // recurred within hours, three times in one day (live incident).
                            // Rate-limited + escalating automatic USB reset (see the
                            // capture_rate_selfheal module doc for the full design). This
                            // load→decide→save→reset→exit sequence is shared byte-for-byte with the
                            // #1128 grabber-STUCK trigger below via the ONE #1149 helper.
                            let now_epoch_s = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0);
                            if let Some(code) = camera_box::capture_rate_selfheal::attempt_self_heal(
                                &device_path_owned,
                                grabber_model,
                                now_epoch_s,
                                std::path::Path::new(camera_box::capture_rate_selfheal::STATE_PATH),
                                &camera_box::capture_rate_selfheal::CAPTURE_RATE_SELF_HEAL_MESSAGES,
                                camera_box::capture_rate_selfheal::perform_usb_reset,
                            ) {
                                running_capture.store(false, Ordering::Relaxed);
                                pending_self_heal_exit_code = Some(code);
                            }
                        }

                        // #1128 — feed this 5 s window into the grabber-STUCK detector (over-rate
                        // AND persistent corrupted). Runs every window regardless of the #656/#971
                        // bands above. On STUCK: ALWAYS log the `#1128 grabber STUCK` marker
                        // (report-only, no I/O — the dev1 alert watchdog greps it); the actual USB
                        // re-auth is gated OFF by default and, when enabled, reuses the SAME #663
                        // rate-limited state so the two triggers share the 600 s throttle +
                        // escalation. Guarded by `pending_self_heal_exit_code.is_none()` so it
                        // never double-resets in a window the #971 chronic band already fired.
                        if let camera_box::grabber_stuck::GrabberStuckVerdict::Stuck {
                            captured_fps,
                            corrupted_delta,
                            windows,
                        } = grabber_stuck_tracker.observe(cap_fps, corrupted)
                        {
                            tracing::warn!(
                                "{}",
                                camera_box::grabber_stuck::stuck_warn_message(
                                    &device_path_owned,
                                    captured_fps,
                                    corrupted_delta,
                                    windows,
                                )
                            );
                            if grabber_stuck_selfheal_enabled
                                && pending_self_heal_exit_code.is_none()
                            {
                                let now_epoch_s = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs())
                                    .unwrap_or(0);
                                if let Some(code) =
                                    camera_box::capture_rate_selfheal::attempt_self_heal(
                                        &device_path_owned,
                                        grabber_model,
                                        now_epoch_s,
                                        std::path::Path::new(
                                            camera_box::capture_rate_selfheal::STATE_PATH,
                                        ),
                                        &camera_box::capture_rate_selfheal::GRABBER_STUCK_SELF_HEAL_MESSAGES,
                                        camera_box::capture_rate_selfheal::perform_usb_reset,
                                    )
                                {
                                    running_capture.store(false, Ordering::Relaxed);
                                    pending_self_heal_exit_code = Some(code);
                                }
                            }
                        }

                        // #1193 — feed this 5 s window into the sustained OVER-RATE detector: a
                        // majority of the per-second capture buckets at/above the over-rate floor
                        // AND dupe-victim shed churn (the drained `window_dupe_shed`), both held for
                        // ~5 min. This is the cam2 ShadowCast state whose manual USB re-auth cure
                        // decays in ~2h and which the #656/#971 + #1128 triggers all miss. On
                        // OverRate the `#1193 grabber OVER-RATE` marker is ALWAYS logged
                        // (report-only, no I/O — a future dev1 watchdog would grep it); the actual
                        // USB re-auth reuses the SAME shared self-heal throttle path, is gated OFF
                        // by default (CAMERA_BOX_GRABBER_OVERRATE_SELFHEAL), guarded by
                        // `pending_self_heal_exit_code.is_none()` (never double-reset a window
                        // another band already fired), AND additionally by a 30-min per-trigger
                        // cooldown floor checked against the SHARED state file — stricter than the
                        // 10-min shared throttle, so the other two triggers stay untouched. Since
                        // #1201 the whole gated sequence is the shared
                        // capture_rate_selfheal::attempt_floored_self_heal wrapper.
                        if let camera_box::capture_overrate::CaptureOverRateVerdict::OverRate {
                            captured_max_bucket,
                            dupe_shed,
                            windows,
                        } = over_rate_tracker
                            .observe(&emit_ring.capture_buckets(), window_dupe_shed)
                        {
                            tracing::warn!(
                                "{}",
                                camera_box::capture_overrate::over_rate_warn_message(
                                    &device_path_owned,
                                    captured_max_bucket,
                                    dupe_shed,
                                    windows,
                                )
                            );
                            if let Some(code) =
                                camera_box::capture_rate_selfheal::attempt_floored_self_heal(
                                    over_rate_selfheal_enabled,
                                    pending_self_heal_exit_code.is_none(),
                                    camera_box::capture_overrate::OVERRATE_MIN_HEAL_INTERVAL_S,
                                    &device_path_owned,
                                    grabber_model,
                                    std::path::Path::new(
                                        camera_box::capture_rate_selfheal::STATE_PATH,
                                    ),
                                    &camera_box::capture_rate_selfheal::OVER_RATE_SELF_HEAL_MESSAGES,
                                    camera_box::capture_rate_selfheal::perform_usb_reset,
                                )
                            {
                                running_capture.store(false, Ordering::Relaxed);
                                pending_self_heal_exit_code = Some(code);
                            }
                        }

                        // #1200 — feed this 5 s window's capture-side byte-identical dupe fraction
                        // into the latch-halving detector: healthy 30fps-into-60fps is ~0.5,
                        // latch-halved is ~0.75 (each unique frame captured ~4x at a correct 60fps
                        // pace, 15 unique/s). The #1193 over-rate (0 over-rate, 0 shed) and #1128
                        // STUCK (0 corrupted) triggers both miss it. On Halved the `#1200 grabber
                        // LATCH-HALVING` marker is ALWAYS logged (report-only, no I/O — a future
                        // dev1 watchdog would grep it); the actual USB re-auth reuses the SAME
                        // shared self-heal throttle path, is gated OFF by default
                        // (CAMERA_BOX_GRABBER_HALVING_SELFHEAL), guarded by
                        // `pending_self_heal_exit_code.is_none()` (never double-reset a window
                        // another band already fired), AND additionally by a 30-min per-trigger
                        // cooldown floor checked against the SHARED state file — since #1201 the
                        // whole gated sequence is the shared
                        // capture_rate_selfheal::attempt_floored_self_heal wrapper. The re-auth
                        // cure is UNPROVEN for this state (it did NOT cure cam3 on 2026-08-25), so
                        // the marker's detection value is the real deliverable.
                        if let camera_box::capture_latch_halving::CaptureLatchHalvingVerdict::Halved {
                            dupe_fraction,
                            dupe_captures,
                            total_captures,
                            windows,
                        } = latch_halving_tracker
                            .observe(window_dupe_captures, window_total_captures)
                        {
                            tracing::warn!(
                                "{}",
                                camera_box::capture_latch_halving::latch_halving_warn_message(
                                    &device_path_owned,
                                    dupe_fraction,
                                    dupe_captures,
                                    total_captures,
                                    windows,
                                )
                            );
                            if let Some(code) =
                                camera_box::capture_rate_selfheal::attempt_floored_self_heal(
                                    latch_halving_selfheal_enabled,
                                    pending_self_heal_exit_code.is_none(),
                                    camera_box::capture_latch_halving::HALVING_MIN_HEAL_INTERVAL_S,
                                    &device_path_owned,
                                    grabber_model,
                                    std::path::Path::new(
                                        camera_box::capture_rate_selfheal::STATE_PATH,
                                    ),
                                    &camera_box::capture_rate_selfheal::LATCH_HALVING_SELF_HEAL_MESSAGES,
                                    camera_box::capture_rate_selfheal::perform_usb_reset,
                                )
                            {
                                running_capture.store(false, Ordering::Relaxed);
                                pending_self_heal_exit_code = Some(code);
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
                            // #1079 — report-only spatial-roughness term BEFORE the `-> label`,
                            // so both existing consumers (splitter-health.sh, verify-device.sh)
                            // keep matching the `-> colour|grayscale` tail unchanged while the
                            // dev1 watchdog can now read `rough=` to catch the Elgato purple-noise
                            // no-signal mode (colourful, structureless) the label alone misses.
                            let rough = last_roughness.unwrap_or(0.0);
                            tracing::info!(
                                "capture chroma: u_dev={:.1} v_dev={:.1} rough={:.1} -> {}",
                                u_dev,
                                v_dev,
                                rough,
                                colour_label
                            );
                        }

                        frame_count = 0;
                        emit_count = 0;
                        // #1200 — reset the per-window dupe counters. prev_capture_hash is NOT reset
                        // (it persists across windows so a window-boundary frame can still be seen
                        // as a dupe of the previous window's last capture).
                        window_dupe_captures = 0;
                        window_total_captures = 0;
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
