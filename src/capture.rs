use anyhow::{Context, Result};
use v4l::buffer::Type;
use v4l::control::{Control, Value};
use v4l::io::mmap::Stream;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;
use v4l::{Device, FourCC};

/// Video frame metadata (data passed separately as zero-copy reference)
#[derive(Clone, Copy)]
pub struct FrameInfo {
    pub width: u32,
    pub height: u32,
    pub fourcc: FourCC,
    pub stride: u32,
}

/// Video frame data with metadata (for compatibility, still used for owned data)
pub struct Frame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub fourcc: FourCC,
    pub stride: u32,
}

/// Frame rate as numerator/denominator
#[derive(Debug, Clone, Copy)]
pub struct FrameRate {
    pub numerator: u32,
    pub denominator: u32,
}

impl Default for FrameRate {
    fn default() -> Self {
        // Default to 30000/1001 (29.97 fps) if detection fails
        Self {
            numerator: 30000,
            denominator: 1001,
        }
    }
}

/// Derive a frame rate (fps) from a V4L2 capture interval.
///
/// V4L2 expresses the capture interval as a PERIOD — seconds per frame
/// (`numerator/denominator` s). Frames-per-second is the reciprocal, so a
/// `1/60` interval is 60 fps and a `1001/60000` interval is 59.94 fps. A zero
/// numerator or denominator means the device reported no usable interval and
/// falls back to the NTSC-safe default. Deriving the rate from the negotiated
/// interval (instead of hard-coding it) keeps the NDI-advertised rate and the
/// genlock pacing honest about what the capture device actually delivers.
pub fn frame_rate_from_interval(interval_numerator: u32, interval_denominator: u32) -> FrameRate {
    if interval_numerator == 0 || interval_denominator == 0 {
        return FrameRate::default();
    }
    // fps = 1 / period = denominator / numerator
    FrameRate {
        numerator: interval_denominator,
        denominator: interval_numerator,
    }
}

/// The v4l2 capture-interval denominator (frames/sec) to request. Post-#11 the rig runs a
/// true-60 fps chain end-to-end, so the default is the 60 fps native ShadowCast mode (unset /
/// 0 / invalid keeps it). `CAMERA_BOX_CAPTURE_FPS=30` still lets a box negotiate native 30 for a
/// legacy 30 fps path (the EMIT rate is the separate `CAMERA_BOX_GENLOCK_FPS` decimation knob).
pub fn requested_capture_denominator(override_fps: Option<u32>) -> u32 {
    override_fps.filter(|&f| f > 0).unwrap_or(60)
}

/// Number of frames the CAPTURE DEVICE silently dropped between two consecutive
/// delivered buffers, from their V4L2 `sequence` numbers (the kernel increments
/// `sequence` once per CAPTURED frame, skipping the value of any frame the driver
/// could not deliver). Consecutive frames (`cur == prev + 1`) ⇒ 0. A real forward
/// jump ⇒ the skipped count. `cur == prev` (a duplicate/no-advance) ⇒ 0.
///
/// #130: a BACKWARD sequence (`cur < prev` — a stream reset, frame reorder, or
/// 60→30 decimation re-numbering) must report 0, NOT a giant count. The previous
/// `cur.wrapping_sub(prev).saturating_sub(1)` returned ~`u32::MAX` for `cur < prev`
/// (e.g. `sequence_gap(12, 10)` = 4294967293), which accumulated into the garbage
/// `k*2^32 + 1` counter observed live on cam2 (34359738369 = 8*2^32 + 1).
///
/// The forward wrapping distance discriminates the two cases:
/// - a genuine FORWARD advance (incl. the legitimate `u32` wrap, e.g. `MAX → 0`)
///   has a SMALL wrapping distance → count it.
/// - a BACKWARD step wraps the "long way round" — its forward distance lands in the
///   upper half of the range → it is not a real forward advance → 0 drops.
pub fn sequence_gap(prev: u32, cur: u32) -> u32 {
    let forward = cur.wrapping_sub(prev);
    // Reinterpret the wrapping forward distance as a signed delta: a real forward
    // advance is a small POSITIVE delta; a backward step (reset/reorder/decimation)
    // is a NEGATIVE delta (forward distance in the upper half → high bit set). The
    // i32 cast splits at the exact symmetric u32 midpoint (2^31). Non-positive delta
    // (backward or duplicate) ⇒ 0 drops; otherwise the gap is delta - 1. The
    // legitimate forward u32 wrap (e.g. MAX→0, delta=+1) stays positive and counts.
    if (forward as i32) <= 0 {
        0
    } else {
        forward - 1
    }
}

/// Serialize cam1's V4L2 capture-drop statistics into the cam1-capture-stats SIDECAR the
/// recording-verdict reads as the cam2→cam1 LOSS (per the trustworthy-measurement rework).
///
/// cam2→cam1 is the camera leg: cam2's monitor → cam1's camera lens → cam1's V4L2 capture.
/// Loss on THIS leg is exactly the capture device dropping frames — the kernel's `sequence`
/// gap ([`sequence_gap`], cumulative in [`VideoCapture::dropped_captures`]). It is NOT a
/// painter-tick optical compare (which is confounded by the 60→30 genlock decimation and
/// flags present readable frames as lost — the false-positive source). The camera-box writes
/// this one-line-per-key sidecar on shutdown of a burn/test run; the verdict reads it.
///
/// Format (plain `key=value`, NO serde_json so the appliance binary stays
/// serde_json-free):
///
/// ```text
/// v4l2_dropped=<cumulative frames the capture device dropped>
/// frames_captured=<delivered buffers counted>
/// ```
///
/// PURE (no I/O) so it is unit-testable without a live V4L2 device; the caller writes the
/// returned string to the sidecar path.
pub fn serialize_capture_stats(v4l2_dropped: u64, frames_captured: u64) -> String {
    format!("v4l2_dropped={v4l2_dropped}\nframes_captured={frames_captured}\n")
}

/// Extract the gray8 (luma) plane from a packed YUYV (YUY2) capture buffer.
///
/// YUYV packs `Y0 U0 Y1 V0` per 4 bytes = 2 pixels: the luma is every EVEN byte
/// (`Y` at 0,2,4,…), chroma the odd bytes. This returns one luma byte per pixel
/// (`width*height` bytes), honoring `stride` (bytes per row; YUYV stride = `2*width`
/// for a tightly packed frame but a device may pad). This is the cam1 GRAB-RECORD
/// extraction (#105 node 2): a prod-clean, dependency-free luma plane the
/// `--record-grab` mode streams to dev1 for ffv1 encoding + QR decode. It deliberately
/// drops chroma — the QR the camera filmed is fully readable from luma, and gray8
/// halves the wire bytes vs full YUYV (so the grab-stream perturbs the measured
/// pipeline less). Rows/cols beyond the buffer are skipped (defensive against a short
/// final buffer); a too-small input yields a zero-padded plane rather than panicking.
pub fn yuyv_to_gray8(data: &[u8], width: u32, height: u32, stride: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let stride = stride as usize;
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        let row = y * stride;
        for x in 0..w {
            // luma byte for pixel x is at row + 2*x (even byte in the YUYV pair).
            let idx = row + 2 * x;
            if idx < data.len() {
                out[y * w + x] = data[idx];
            }
        }
    }
    out
}

// ── #299 colour-capture metric ────────────────────────────────────────────────

/// Minimum mean chroma deviation (|U−128| or |V−128|) to classify a YUYV frame
/// as colour. Values at or below this threshold indicate a monochrome/grayscale
/// source (both channels stay close to the neutral 128 point).
pub const CHROMA_COLOR_THRESHOLD: f32 = 2.0;

/// Macropixel sampling stride for [`mean_chroma`]: sample every Nth macropixel.
/// At N=64 and 1920×1080 (≈518 k macropixels) ≈8 k samples per call — negligible
/// cost even at the 1 Hz periodic log rate.
const CHROMA_SAMPLE_STRIDE: usize = 64;

/// How many captured frames to skip between chroma samples in the capture loop.
/// At 60 fps capture this yields one sample per second, giving the periodic
/// 5-second report window five fresh measurements to choose the most recent from.
pub const CHROMA_SAMPLE_FRAMES: u32 = 60;

/// Compute mean |U−128| and mean |V−128| over a subsampled YUYV422 frame.
///
/// YUYV422 macropixel layout: `Y0 U Y1 V` (4 bytes, 2 pixels). The U byte is
/// at offset 1 and the V byte at offset 3 within each macropixel. Neutral grey
/// encodes U=V=128; a chromatic source pushes U and V away from 128.
///
/// Samples every [`CHROMA_SAMPLE_STRIDE`] macropixels to keep per-call cost
/// small. `width` and `height` are the frame dimensions in pixels; the buffer
/// must contain at least `width * height * 2` bytes (YUYV422 is 2 bytes/pixel).
///
/// Returns `(mean |U−128|, mean |V−128|)` in `[0.0, 128.0]`. For a grayscale
/// source both values are close to 0; a colour source produces values clearly
/// above [`CHROMA_COLOR_THRESHOLD`]. Returns `(0.0, 0.0)` for an empty or
/// undersized buffer.
pub fn mean_chroma(frame: &[u8], width: usize, height: usize) -> (f32, f32) {
    // YUYV422: 2 bytes per pixel → frame_bytes = width * height * 2.
    let frame_bytes = width.saturating_mul(height).saturating_mul(2);
    if frame_bytes == 0 || frame.len() < frame_bytes {
        return (0.0, 0.0);
    }
    let step = CHROMA_SAMPLE_STRIDE.saturating_mul(4); // bytes per stride (4 bytes/macropixel)
    let mut u_sum: u64 = 0;
    let mut v_sum: u64 = 0;
    let mut count: u64 = 0;
    let mut i = 0usize;
    while i + 3 < frame_bytes {
        // Macropixel: Y0 U Y1 V — U at offset 1, V at offset 3.
        let u = frame[i + 1] as i16 - 128;
        let v = frame[i + 3] as i16 - 128;
        u_sum += u.unsigned_abs() as u64;
        v_sum += v.unsigned_abs() as u64;
        count += 1;
        i += step;
    }
    if count == 0 {
        return (0.0, 0.0);
    }
    (u_sum as f32 / count as f32, v_sum as f32 / count as f32)
}

/// Classify chroma deviations as colour or grayscale.
///
/// Returns `true` when either the U or V mean deviation (from [`mean_chroma`])
/// exceeds [`CHROMA_COLOR_THRESHOLD`], indicating discernible colour information
/// in the captured frame.
pub fn is_color_frame(u_dev: f32, v_dev: f32) -> bool {
    u_dev > CHROMA_COLOR_THRESHOLD || v_dev > CHROMA_COLOR_THRESHOLD
}

/// V4L2 control id for picture CONTRAST (`V4L2_CID_CONTRAST`).
pub const V4L2_CID_CONTRAST: u32 = 0x0098_0901;
/// V4L2 control id for picture SATURATION (`V4L2_CID_SATURATION`).
pub const V4L2_CID_SATURATION: u32 = 0x0098_0902;
/// V4L2 control id for picture HUE (`V4L2_CID_HUE`, `V4L2_CID_BASE+3`).
///
/// #338: hue is NEVER force-set on the grab/production path. `0` is NOT a neutral
/// value — on the ShadowCast capture card the V4L2 hue control is `min=0 max=100
/// default=50`, so forcing `0` is a MAX shift that tints the picture pink/magenta.
/// The only way hue is touched is an explicit operator `CAMERA_BOX_CAPTURE_CONTROLS`
/// override (`parse_capture_controls`); the certified colour set leaves it alone so
/// each card keeps its own neutral default.
pub const V4L2_CID_HUE: u32 = 0x0098_0903;

/// One certified V4L2 capture control (`id`, `value`) to apply at device open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureControl {
    pub id: u32,
    pub value: i64,
}

/// The CERTIFIED cam1 capture controls for a sharp, decodable optical grab
/// (#156 durable fix): `saturation=0` (kill the chroma fringing that softens the
/// black/white QR edges) + `contrast=75` (a hard luma separation so the filmed QR
/// stays bimodal). These were proven on the live rig — a grab with them decodes
/// ~100%; without them a camera-box restart silently reverts the device to its
/// soft defaults (`contrast=50`/`saturation=50`) and decode collapses. Applying
/// them IN the binary means a restart can never drop quality.
pub fn certified_cam1_controls() -> Vec<CaptureControl> {
    vec![
        CaptureControl {
            id: V4L2_CID_CONTRAST,
            value: 75,
        },
        CaptureControl {
            id: V4L2_CID_SATURATION,
            value: 0,
        },
    ]
}

/// Parse a `CAMERA_BOX_CAPTURE_CONTROLS` env value into capture controls.
///
/// Format: comma-separated `name=value` pairs, where `name` is `contrast`,
/// `saturation`, or `hue` — e.g. `"saturation=50,contrast=50,hue=0"`. The override
/// covers every control the production colour set ([`color_production_controls`])
/// enforces, so an operator can fully retune the picture (incl. resetting hue). The
/// special value `"certified"` (case-insensitive) expands to
/// [`certified_cam1_controls`]. Unknown names and malformed pairs are skipped with
/// a logged warning (a bad env must NOT crash capture). An empty / whitespace-only
/// string yields no controls (the explicit "touch nothing" override).
pub fn parse_capture_controls(spec: &str) -> Vec<CaptureControl> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Vec::new();
    }
    if spec.eq_ignore_ascii_case("certified") {
        return certified_cam1_controls();
    }
    let mut out = Vec::new();
    for pair in spec.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let Some((name, val)) = pair.split_once('=') else {
            tracing::warn!("CAMERA_BOX_CAPTURE_CONTROLS: skipping malformed pair {pair:?}");
            continue;
        };
        let id = match name.trim().to_ascii_lowercase().as_str() {
            "contrast" => V4L2_CID_CONTRAST,
            "saturation" => V4L2_CID_SATURATION,
            "hue" => V4L2_CID_HUE,
            other => {
                tracing::warn!("CAMERA_BOX_CAPTURE_CONTROLS: skipping unknown control {other:?}");
                continue;
            }
        };
        match val.trim().parse::<i64>() {
            Ok(value) => out.push(CaptureControl { id, value }),
            Err(_) => {
                tracing::warn!("CAMERA_BOX_CAPTURE_CONTROLS: {name:?} value {val:?} not an integer")
            }
        }
    }
    out
}

/// The CERTIFIED COLOUR production V4L2 capture controls, ENFORCED at every device
/// open so the production stream is ALWAYS normal colour, regardless of any stray
/// control a prior QR-test grab — or a previous process — left on the card (#296).
///
/// Why this exists: [`certified_cam1_controls`] (#156) deliberately sets
/// `saturation=0` (+ `contrast=75`) to sharpen the filmed QR for decode. That
/// control PERSISTS on the ShadowCast card after the grab process exits — and the
/// production path previously applied NO controls (`Vec::new()`), so once any grab /
/// QR-test had run, the card stayed at `saturation=0` and EVERY camera went
/// grayscale on live air (the church-event regression). Enforcing a known COLOUR
/// set on every open is the self-healing counterpart of the #150/#257 genlock
/// lockdown: colour is restored on every open, no matter what ran before.
///
/// Values: `saturation=50` — the ShadowCast factory colour level, proven on the
/// live rig to produce normal colour (channel_diff ≈ 35); `contrast=50` — the
/// factory default. Both equal the device defaults, so this set restores the
/// known-good colour without imposing any non-default picture shift.
///
/// #338: hue is deliberately NOT force-set. Forcing `hue=0` ("neutral") tinted the
/// live camera PINK — on the ShadowCast card hue is `min=0 max=100 default=50`, so
/// `0` is a MAX shift, not neutral. The grab path must never disturb hue, and
/// hardcoding any one hue value tints every card whose neutral isn't that value. Hue
/// is only ever touched via an explicit `CAMERA_BOX_CAPTURE_CONTROLS` override.
///
/// A card that lacks any of these controls (e.g. the NZXT CAM4 grab card, which
/// exposes NO v4l2 picture controls) logs a warning and PROCEEDS — see
/// [`apply_controls_with`]; a missing control is never fatal.
pub fn color_production_controls() -> Vec<CaptureControl> {
    vec![
        CaptureControl {
            id: V4L2_CID_SATURATION,
            value: 50,
        },
        CaptureControl {
            id: V4L2_CID_CONTRAST,
            value: 50,
        },
    ]
}

/// Choose the V4L2 capture controls to enforce at device open.
///
/// - `env_spec = Some(spec)` — an explicit `CAMERA_BOX_CAPTURE_CONTROLS` override;
///   parse it ([`parse_capture_controls`]). Used by ad-hoc rig tweaks; an empty /
///   whitespace spec yields no controls (deliberate "touch nothing" escape hatch).
///   The certified SHARP set ([`certified_cam1_controls`]) stays available ON DEMAND
///   via `CAMERA_BOX_CAPTURE_CONTROLS=certified`.
/// - `env_spec = None` — both PRODUCTION and a grab / QR-test run get the certified
///   COLOUR set ([`color_production_controls`]: device-default `saturation=50`,
///   `contrast=50`, hue untouched).
///
/// #296: this no-override branch previously returned NO controls, so a stray
/// `saturation=0` left by a prior grab persisted and the live cameras went
/// grayscale. The colour set now self-heals colour on every open.
///
/// #338/#312: the grab path is NO LONGER auto-given the SHARP set
/// ([`certified_cam1_controls`], `saturation=0`/`contrast=75`). That set was meant
/// to aid QR decode but HURT it (run 312005: a ShadowCast box with the sharp set
/// read the painter QR ~50% undecodable, while the NZXT card on device defaults read
/// the SAME monitor clean). The optical decode worked fine on device defaults before
/// these controls were added, so grab now selects the device-default colour set too.
/// `_record_grab` is retained for call-site clarity but no longer affects selection.
pub fn select_capture_controls(env_spec: Option<&str>, _record_grab: bool) -> Vec<CaptureControl> {
    match env_spec {
        Some(spec) => parse_capture_controls(spec),
        None => color_production_controls(),
    }
}

/// Outcome tally of applying a set of [`CaptureControl`]s, for logging + tests.
/// NONE of these outcomes abort capture — a device that rejects or clamps a control
/// (e.g. the NZXT CAM4 card, which exposes no picture controls) is tolerated, so a
/// box with a control-less grab card still streams (#296 — must not regress CAM4).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ControlReport {
    /// Set, and read back EXACTLY as requested.
    pub applied: usize,
    /// Set succeeded but the device reported a DIFFERENT value (driver clamped), or
    /// the read-back itself failed (can't verify) — soft, non-fatal.
    pub adjusted: usize,
    /// The device REJECTED the set — the control is not supported (the NZXT case).
    /// Logged loudly; capture proceeds.
    pub failed: usize,
}

/// Abstraction over a V4L2 device's integer picture-control get/set, so the
/// apply-controls POLICY (warn-and-continue, NEVER fatal) is unit-testable without a
/// real `/dev/video*` node. Implemented for the live [`Device`]; tests inject a fake
/// device (incl. one that supports NO controls — the NZXT CAM4 card).
pub trait ControlIo {
    /// Error type surfaced when a get/set fails (only ever logged — never fatal).
    type Err: std::fmt::Display;
    /// Set integer control `id` to `value`.
    fn set_ctrl(&self, id: u32, value: i64) -> std::result::Result<(), Self::Err>;
    /// Read integer control `id` back.
    fn get_ctrl(&self, id: u32) -> std::result::Result<i64, Self::Err>;
}

impl ControlIo for Device {
    type Err = std::io::Error;

    fn set_ctrl(&self, id: u32, value: i64) -> std::result::Result<(), Self::Err> {
        self.set_control(Control {
            id,
            value: Value::Integer(value),
        })
    }

    fn get_ctrl(&self, id: u32) -> std::result::Result<i64, Self::Err> {
        match self.control(id)?.value {
            Value::Integer(got) => Ok(got),
            other => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("control id={id:#010x} read back non-integer {other:?}"),
            )),
        }
    }
}

/// Apply `controls` through any [`ControlIo`], verifying each by read-back, and
/// return a [`ControlReport`]. A control that fails to SET (driver rejects it — the
/// NZXT CAM4 card has no picture controls) or reads back DIFFERENT is logged loudly
/// (`warn`) but NEVER aborts: a soft-but-running stream beats no stream, and the
/// warning surfaces the drift to the operator. This function CANNOT return an error —
/// that "never fatal" guarantee is what the NZXT regression test pins (#296). Empty
/// `controls` is a no-op (the explicit "touch nothing" override).
pub fn apply_controls_with<IO: ControlIo>(io: &IO, controls: &[CaptureControl]) -> ControlReport {
    let mut report = ControlReport::default();
    for c in controls {
        match io.set_ctrl(c.id, c.value) {
            Ok(()) => match io.get_ctrl(c.id) {
                Ok(got) if got == c.value => {
                    report.applied += 1;
                    tracing::info!(
                        "capture control id={:#010x} set to {} (verified)",
                        c.id,
                        c.value
                    );
                }
                Ok(got) => {
                    report.adjusted += 1;
                    tracing::warn!(
                        "capture control id={:#010x} requested {} but device reports {} \
                         (driver clamped/ignored)",
                        c.id,
                        c.value,
                        got
                    );
                }
                Err(e) => {
                    report.adjusted += 1;
                    tracing::warn!(
                        "capture control id={:#010x} set to {} but read-back failed: {}",
                        c.id,
                        c.value,
                        e
                    );
                }
            },
            Err(e) => {
                report.failed += 1;
                tracing::warn!(
                    "capture control id={:#010x} -> {} FAILED to apply: {} \
                     (capture continues; device may lack this control)",
                    c.id,
                    c.value,
                    e
                );
            }
        }
    }
    report
}

/// V4L2 video capture wrapper
pub struct VideoCapture {
    stream: Stream<'static>,
    width: u32,
    height: u32,
    fourcc: FourCC,
    stride: u32,
    frame_rate: FrameRate,
    /// V4L2 `sequence` of the last delivered buffer, for capture-drop detection
    /// ([`sequence_gap`]). `None` until the first frame.
    last_sequence: Option<u32>,
    /// Cumulative count of frames the capture device dropped over this stream's life.
    dropped_captures: u64,
    /// Count of delivered buffers (frames the device actually captured) over this
    /// stream's life. Paired with `dropped_captures` it is the cam2→cam1 LOSS denominator
    /// the verdict reports (`serialize_capture_stats`).
    frames_captured: u64,
}

impl VideoCapture {
    /// Open capture device and start streaming at 1920x1080 @ 60fps.
    ///
    /// Applies no V4L2 picture controls — production behaviour. Use
    /// [`open_with_controls`](Self::open_with_controls) to enforce the certified
    /// sharp-grab controls (#156).
    pub fn open(device_path: &str) -> Result<Self> {
        Self::open_with_controls(device_path, &[])
    }

    /// Open capture device, applying the given V4L2 picture `controls` BEFORE the
    /// stream starts (so they take effect for every captured frame), then stream at
    /// 1920x1080. Used by the cam1 grab-record path with [`certified_cam1_controls`]
    /// so a camera-box restart can never silently revert the device to its soft
    /// defaults and collapse QR decode (#156 durable fix). A control that fails to
    /// apply is logged loudly but does NOT abort open (capture still proceeds — the
    /// operator sees the warning).
    pub fn open_with_controls(device_path: &str, controls: &[CaptureControl]) -> Result<Self> {
        tracing::info!("Opening capture device: {}", device_path);

        let device = Device::with_path(device_path)
            .with_context(|| format!("Failed to open device: {}", device_path))?;

        // Query device capabilities
        let caps = device.query_caps()?;
        tracing::info!("Device: {} ({})", caps.card, caps.driver);

        // Get current format as starting point
        let mut format = Capture::format(&device)?;

        // Set 1920x1080 YUYV (best for NDI conversion)
        format.width = 1920;
        format.height = 1080;
        format.fourcc = FourCC::new(b"YUYV");

        let final_format =
            Capture::set_format(&device, &format).context("Failed to set 1920x1080 YUYV format")?;

        tracing::info!(
            "Capture format: {}x{} {} (stride: {})",
            final_format.width,
            final_format.height,
            final_format.fourcc,
            final_format.stride
        );

        let width = final_format.width;
        let height = final_format.height;
        let fourcc = final_format.fourcc;
        let stride = final_format.stride;

        // Request frame rate for the genlock/NDI pipeline (#11 quality bar). The
        // frame rate is derived from the rate the driver actually negotiates,
        // not hard-coded — so NDI metadata and genlock pacing stay honest about
        // what the capture device delivers.
        let frame_rate = match Capture::params(&device) {
            Ok(mut params) => {
                params.interval.numerator = 1;
                let req = requested_capture_denominator(
                    std::env::var("CAMERA_BOX_CAPTURE_FPS")
                        .ok()
                        .and_then(|s| s.parse().ok()),
                );
                params.interval.denominator = req;
                let negotiated = Capture::set_params(&device, &params).unwrap_or(params);
                frame_rate_from_interval(
                    negotiated.interval.numerator,
                    negotiated.interval.denominator,
                )
            }
            Err(_) => frame_rate_from_interval(1, 60),
        };
        tracing::info!(
            "Frame rate: {:.3} fps ({}/{})",
            frame_rate.numerator as f64 / frame_rate.denominator as f64,
            frame_rate.numerator,
            frame_rate.denominator
        );

        // Apply certified picture controls (saturation/contrast) AFTER set_format and
        // set_params, just before streaming (#156 regression fix). MANY UVC capture
        // devices — the rig's ShadowCast 2 included — RESET their picture controls to
        // factory defaults (contrast=50, saturation=50) on VIDIOC_S_FMT / VIDIOC_S_PARM.
        // Applying the controls BEFORE set_format (as the code previously did) let the
        // format-set clobber them right back to the soft defaults, so the grab decoded a
        // mushy QR and ~40% of frames went undecodable (run-163163) even though the
        // controls were "applied". Setting them here, after the format/rate negotiation
        // and verified by read-back, guarantees the device actually streams with the
        // certified sharp/bimodal controls. A failure warns but never aborts capture.
        Self::apply_controls(&device, controls);

        // Create memory-mapped stream with enough buffers to avoid frame drops
        // 4 buffers to handle processing time variance
        let stream = Stream::with_buffers(&device, Type::VideoCapture, 4)
            .context("Failed to create capture stream")?;

        // Leak the device to get 'static lifetime (it lives for program duration)
        let stream = unsafe { std::mem::transmute::<Stream<'_>, Stream<'static>>(stream) };

        Ok(Self {
            stream,
            width,
            height,
            fourcc,
            stride,
            frame_rate,
            last_sequence: None,
            dropped_captures: 0,
            frames_captured: 0,
        })
    }

    /// Apply each [`CaptureControl`] to an open V4L2 `device`, verifying by read-back.
    /// Delegates to the device-agnostic [`apply_controls_with`] (which is unit-tested
    /// against a fake control-less device for the NZXT CAM4 case). A control that
    /// fails to SET or reads back DIFFERENT is logged loudly but NEVER aborts capture
    /// — a soft-but-running stream beats no stream. Empty `controls` is a no-op.
    fn apply_controls(device: &Device, controls: &[CaptureControl]) {
        let report = apply_controls_with(device, controls);
        if report.adjusted > 0 || report.failed > 0 {
            tracing::warn!(
                "capture controls applied with {} clamped/unverified and {} unsupported \
                 (device may lack picture controls — capture continues)",
                report.adjusted,
                report.failed
            );
        }
    }

    /// Record a delivered buffer's V4L2 `sequence`, accounting for any frames the
    /// capture device dropped since the previous buffer ([`sequence_gap`]). Logs
    /// each gap with the surrounding sequence numbers and keeps a running total.
    fn record_sequence(&mut self, seq: u32) {
        if let Some(prev) = self.last_sequence {
            let gap = sequence_gap(prev, seq);
            if gap > 0 {
                self.dropped_captures += gap as u64;
                tracing::warn!(
                    "capture device dropped {} frame(s): v4l2 sequence {} -> {} (total dropped {})",
                    gap,
                    prev,
                    seq,
                    self.dropped_captures
                );
            }
        }
        self.last_sequence = Some(seq);
        self.frames_captured += 1;
    }

    /// Total frames the capture device has dropped over this stream's life
    /// (cumulative [`sequence_gap`]). Capture-card loss, not pipeline loss.
    /// Surfaced in the periodic streaming report (`main.rs`).
    pub fn dropped_captures(&self) -> u64 {
        self.dropped_captures
    }

    /// Total DELIVERED buffers (frames the device actually captured) over this stream's life.
    /// This is the COUNT OF DELIVERED frames, NOT the full denominator — the total frames the
    /// device should have produced is `frames_captured + dropped_captures`. Reported alongside
    /// [`dropped_captures`](Self::dropped_captures) in the cam1-capture-stats sidecar; the
    /// verdict's cam2→cam1 loss gate is the drop COUNT (`v4l2_dropped == 0`), so this value is
    /// context, not part of the pass/fail.
    pub fn frames_captured(&self) -> u64 {
        self.frames_captured
    }

    /// Write cam1's V4L2 capture-drop statistics to `path` (the cam1-capture-stats sidecar
    /// the recording-verdict reads as the cam2→cam1 LOSS). Called on shutdown of a burn/test
    /// run. The format is [`serialize_capture_stats`]. Errors are returned (the caller logs
    /// + continues — a missing sidecar simply means the verdict can't report cam2→cam1 loss).
    pub fn write_capture_stats(&self, path: &str) -> Result<()> {
        std::fs::write(
            path,
            serialize_capture_stats(self.dropped_captures, self.frames_captured),
        )
        .with_context(|| format!("write cam1 capture-stats sidecar {path}"))?;
        Ok(())
    }

    /// Capture next frame (blocking) - COPIES DATA
    #[allow(dead_code)]
    pub fn next_frame(&mut self) -> Result<Frame> {
        let (buffer, metadata) = self.stream.next()?;
        let seq = metadata.sequence;

        // Copy frame data (zero-copy would require unsafe lifetime tricks)
        let data = buffer.to_vec();
        // `buffer`/`metadata` borrow self.stream; record AFTER that borrow ends.
        self.record_sequence(seq);

        Ok(Frame {
            data,
            width: self.width,
            height: self.height,
            fourcc: self.fourcc,
            stride: self.stride,
        })
    }

    /// Process next frame with zero-copy callback (FAST PATH)
    /// The callback receives a direct reference to the mmap buffer - no copying!
    /// Buffer is automatically requeued after callback returns.
    #[inline]
    pub fn process_frame<F>(&mut self, mut callback: F) -> Result<()>
    where
        F: FnMut(&[u8], FrameInfo),
    {
        let (buffer, metadata) = self.stream.next()?;
        let seq = metadata.sequence;

        let info = FrameInfo {
            width: self.width,
            height: self.height,
            fourcc: self.fourcc,
            stride: self.stride,
        };

        // Zero-copy: pass buffer slice directly to callback
        #[allow(clippy::needless_borrow)]
        callback(&buffer, info);

        // `buffer`/`metadata` borrow self.stream; record AFTER that borrow ends
        // (after the callback) so the capture-drop accounting can take &mut self.
        self.record_sequence(seq);

        // Buffer automatically requeued when it goes out of scope
        Ok(())
    }

    /// Get frame info without capturing
    #[allow(dead_code)]
    pub fn frame_info(&self) -> FrameInfo {
        FrameInfo {
            width: self.width,
            height: self.height,
            fourcc: self.fourcc,
            stride: self.stride,
        }
    }

    /// Get frame dimensions
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Get pixel format
    #[allow(dead_code)]
    pub fn fourcc(&self) -> FourCC {
        self.fourcc
    }

    /// Get frame rate
    pub fn frame_rate(&self) -> FrameRate {
        self.frame_rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_rate_from_interval_60fps() {
        // 1080p60: V4L2 interval 1/60 s/frame -> 60 fps.
        let r = frame_rate_from_interval(1, 60);
        assert_eq!(r.numerator, 60);
        assert_eq!(r.denominator, 1);
        let fps = r.numerator as f64 / r.denominator as f64;
        assert!((fps - 60.0).abs() < 1e-9, "expected 60 fps, got {fps}");
    }

    #[test]
    fn frame_rate_from_interval_30fps() {
        // Legacy 30 fps interval still derives correctly.
        let r = frame_rate_from_interval(1, 30);
        assert_eq!(r.numerator, 30);
        assert_eq!(r.denominator, 1);
    }

    #[test]
    fn frame_rate_from_interval_5994fps() {
        // NTSC 59.94: interval 1001/60000 s/frame.
        let r = frame_rate_from_interval(1001, 60000);
        let fps = r.numerator as f64 / r.denominator as f64;
        assert!((fps - 59.94).abs() < 0.01, "expected 59.94 fps, got {fps}");
    }

    #[test]
    fn frame_rate_from_interval_invalid_falls_back() {
        // A zero numerator/denominator is not a usable interval -> default.
        assert_eq!(frame_rate_from_interval(0, 0).numerator, 30000);
        assert_eq!(frame_rate_from_interval(1, 0).denominator, 1001);
        assert_eq!(frame_rate_from_interval(0, 60).numerator, 30000);
    }

    #[test]
    fn test_frame_rate_default() {
        let rate = FrameRate::default();
        assert_eq!(rate.numerator, 30000);
        assert_eq!(rate.denominator, 1001);
        // 30000/1001 = ~29.97 fps
        let fps = rate.numerator as f64 / rate.denominator as f64;
        assert!((fps - 29.97).abs() < 0.01);
    }

    #[test]
    fn test_frame_rate_as_f64() {
        let rate = FrameRate {
            numerator: 60,
            denominator: 1,
        };
        let fps = rate.numerator as f64 / rate.denominator as f64;
        assert!((fps - 60.0).abs() < 0.001);

        let rate_ntsc = FrameRate {
            numerator: 60000,
            denominator: 1001,
        };
        let fps_ntsc = rate_ntsc.numerator as f64 / rate_ntsc.denominator as f64;
        assert!((fps_ntsc - 59.94).abs() < 0.01);
    }

    #[test]
    fn test_frame_rate_clone() {
        let rate = FrameRate {
            numerator: 24,
            denominator: 1,
        };
        let cloned = rate;
        assert_eq!(rate.numerator, cloned.numerator);
        assert_eq!(rate.denominator, cloned.denominator);
    }

    #[test]
    fn test_frame_info_clone_copy() {
        let info = FrameInfo {
            width: 1920,
            height: 1080,
            fourcc: FourCC::new(b"YUYV"),
            stride: 3840,
        };
        // Test Copy trait
        let copied = info;
        assert_eq!(info.width, copied.width);
        assert_eq!(info.height, copied.height);
        assert_eq!(info.stride, copied.stride);
    }

    #[test]
    fn test_frame_info_fields() {
        let info = FrameInfo {
            width: 1280,
            height: 720,
            fourcc: FourCC::new(b"MJPG"),
            stride: 2560,
        };
        assert_eq!(info.width, 1280);
        assert_eq!(info.height, 720);
        assert_eq!(info.stride, 2560);
    }

    #[test]
    fn test_frame_construction() {
        let frame = Frame {
            data: vec![0u8; 1920 * 1080 * 2],
            width: 1920,
            height: 1080,
            fourcc: FourCC::new(b"YUYV"),
            stride: 3840,
        };
        assert_eq!(frame.width, 1920);
        assert_eq!(frame.height, 1080);
        assert_eq!(frame.stride, 3840);
        assert_eq!(frame.data.len(), 1920 * 1080 * 2);
    }

    #[test]
    fn test_fourcc_formatting() {
        let fourcc = FourCC::new(b"YUYV");
        let display = format!("{}", fourcc);
        assert!(display.contains('Y') || display.len() == 4);
    }

    #[test]
    fn test_frame_rate_debug() {
        let rate = FrameRate {
            numerator: 30,
            denominator: 1,
        };
        let debug = format!("{:?}", rate);
        assert!(debug.contains("FrameRate"));
        assert!(debug.contains("30"));
    }

    #[test]
    fn sequence_gap_consecutive_is_zero() {
        assert_eq!(sequence_gap(10, 11), 0);
        assert_eq!(sequence_gap(0, 1), 0);
    }

    #[test]
    fn sequence_gap_counts_skipped_frames() {
        assert_eq!(sequence_gap(10, 12), 1); // frame 11 dropped
        assert_eq!(sequence_gap(10, 15), 4); // frames 11..14 dropped
    }

    #[test]
    fn sequence_gap_handles_u32_wrap() {
        // wraparound at u32::MAX -> 0 with no intervening drop is consecutive.
        assert_eq!(sequence_gap(u32::MAX, 0), 0);
        assert_eq!(sequence_gap(u32::MAX - 1, 1), 2); // MAX and 0 dropped
    }

    #[test]
    fn sequence_gap_same_or_no_advance_is_zero() {
        // A duplicate/no-advance (never expected) must not report a giant gap.
        assert_eq!(sequence_gap(10, 10), 0);
    }

    #[test]
    fn sequence_gap_backward_sequence_is_zero() {
        // #130: a BACKWARD v4l2 sequence (cur < prev: a stream reset, frame reorder,
        // or 60→30 decimation re-numbering) must NOT be counted as a giant drop. The
        // old `cur.wrapping_sub(prev).saturating_sub(1)` yielded ~u32::MAX here, which
        // (cast to u64 and accumulated) produced the garbage `k*2^32 + 1` counter live
        // on cam2 (e.g. 34359738369 = 8*2^32 + 1). A backward step is 0 drops.
        assert_eq!(
            sequence_gap(12, 10),
            0,
            "cur<prev by 2 (backward) => 0 drops"
        );
        assert_eq!(
            sequence_gap(10, 9),
            0,
            "cur<prev by 1 (backward) => 0 drops"
        );
        assert_eq!(
            sequence_gap(1000, 1),
            0,
            "a large backward jump (reset) => 0 drops, NOT ~u32::MAX"
        );
    }

    #[test]
    fn sequence_gap_legit_forward_wrap_still_counts() {
        // The LEGITIMATE forward u32 wrap (prev near u32::MAX, cur small) must STILL be
        // counted — it is a genuine forward advance across the wrap boundary, NOT a
        // backward step. A real forward wrap has a SMALL wrapping distance; a backward
        // step has a HUGE wrapping distance (> half the u32 range). That is the
        // discriminator the fix must preserve.
        assert_eq!(sequence_gap(u32::MAX, 0), 0, "consecutive across wrap => 0");
        assert_eq!(
            sequence_gap(u32::MAX - 1, 1),
            2,
            "MAX and 0 dropped across the wrap => 2 (forward wrap still counts)"
        );
        assert_eq!(
            sequence_gap(u32::MAX, 3),
            3,
            "forward wrap with 3 intervening (0,1,2 dropped) => 3"
        );
    }

    #[test]
    fn sequence_gap_splits_at_symmetric_midpoint() {
        // The forward/backward split is the exact symmetric u32 midpoint (2^31):
        // a forward distance of 2^31-1 (delta=+2147483647) is still "forward" and
        // counts; 2^31 and above is "backward" (the long way round) => 0. No bogus
        // ~2-billion drop count at the boundary.
        assert_eq!(
            sequence_gap(0, (1u32 << 31) - 1),
            (1u32 << 31) - 2,
            "delta = +2^31-1 (max positive i32) is forward => counts"
        );
        assert_eq!(
            sequence_gap(0, 1u32 << 31),
            0,
            "delta = 2^31 (i32::MIN, the midpoint) is backward => 0"
        );
        assert_eq!(
            sequence_gap(0, u32::MAX),
            0,
            "delta = -1 (one step backward) => 0, never ~u32::MAX"
        );
    }

    #[test]
    fn capture_stats_sidecar_carries_v4l2_dropped_and_frames_captured() {
        // The cam2→cam1 LOSS sidecar: the verdict reads v4l2_dropped as the cam2→cam1 loss
        // (capture-card drops), NOT a painter-tick optical compare. frames_captured is the
        // denominator. Plain key=value (the appliance binary stays serde_json-free).
        let s = serialize_capture_stats(3, 9000);
        assert_eq!(s, "v4l2_dropped=3\nframes_captured=9000\n");
    }

    #[test]
    fn capture_stats_zero_drops_is_zero_loss() {
        // Zero V4L2 drops ⇒ ZERO cam2→cam1 loss (every captured frame was delivered).
        let s = serialize_capture_stats(0, 9001);
        assert_eq!(s, "v4l2_dropped=0\nframes_captured=9001\n");
    }

    #[test]
    fn yuyv_to_gray8_picks_even_luma_bytes() {
        // 2x1 YUYV: Y0=10 U0=200 Y1=20 V0=201 → gray8 = [10, 20] (the even bytes).
        let yuyv = [10u8, 200, 20, 201];
        let gray = yuyv_to_gray8(&yuyv, 2, 1, 4);
        assert_eq!(gray, vec![10, 20], "luma is the even (Y) bytes only");
    }

    #[test]
    fn yuyv_to_gray8_honors_stride_padding() {
        // 2x2 with a padded stride of 6 bytes/row (4 luma+chroma + 2 pad). Row 0:
        // Y=1,Y=2; row 1: Y=3,Y=4. The 2 pad bytes per row must be skipped.
        let row0 = [1u8, 0, 2, 0, 0, 0]; // Y0=1 U Y1=2 V pad pad
        let row1 = [3u8, 0, 4, 0, 0, 0];
        let mut data = Vec::new();
        data.extend_from_slice(&row0);
        data.extend_from_slice(&row1);
        let gray = yuyv_to_gray8(&data, 2, 2, 6);
        assert_eq!(
            gray,
            vec![1, 2, 3, 4],
            "stride padding skipped, luma row-major"
        );
    }

    #[test]
    fn yuyv_to_gray8_short_buffer_zero_pads_no_panic() {
        // A truncated final buffer must not panic; missing pixels are 0.
        let yuyv = [9u8, 0]; // only 1 luma byte available for a 2x1 request
        let gray = yuyv_to_gray8(&yuyv, 2, 1, 4);
        assert_eq!(gray, vec![9, 0], "missing pixel zero-padded, no panic");
    }

    #[test]
    fn yuyv_to_gray8_output_is_one_byte_per_pixel() {
        let yuyv = vec![128u8; 4 * 4]; // 4 pixels (2x2) packed YUYV
        let gray = yuyv_to_gray8(&yuyv, 2, 2, 4);
        assert_eq!(gray.len(), 4, "one luma byte per pixel = width*height");
        assert!(gray.iter().all(|&b| b == 128));
    }

    #[test]
    fn capture_denominator_defaults_to_60_and_honors_override() {
        assert_eq!(requested_capture_denominator(None), 60);
        assert_eq!(requested_capture_denominator(Some(0)), 60); // 0 is invalid -> default
        assert_eq!(requested_capture_denominator(Some(30)), 30);
        assert_eq!(requested_capture_denominator(Some(60)), 60);
    }

    #[test]
    fn certified_controls_are_contrast75_saturation0() {
        // #156: the certified sharp-grab set is exactly contrast=75 + saturation=0.
        let c = certified_cam1_controls();
        assert_eq!(
            c,
            vec![
                CaptureControl {
                    id: V4L2_CID_CONTRAST,
                    value: 75
                },
                CaptureControl {
                    id: V4L2_CID_SATURATION,
                    value: 0
                },
            ]
        );
    }

    #[test]
    fn parse_capture_controls_certified_keyword_expands_to_the_set() {
        assert_eq!(
            parse_capture_controls("certified"),
            certified_cam1_controls()
        );
        assert_eq!(
            parse_capture_controls("CERTIFIED"),
            certified_cam1_controls()
        );
    }

    #[test]
    fn parse_capture_controls_explicit_pairs() {
        let c = parse_capture_controls("contrast=75,saturation=0");
        assert_eq!(
            c,
            vec![
                CaptureControl {
                    id: V4L2_CID_CONTRAST,
                    value: 75
                },
                CaptureControl {
                    id: V4L2_CID_SATURATION,
                    value: 0
                },
            ]
        );
    }

    #[test]
    fn parse_capture_controls_empty_is_no_controls() {
        // Production default: unset / whitespace-only env touches no controls.
        assert!(parse_capture_controls("").is_empty());
        assert!(parse_capture_controls("   ").is_empty());
    }

    #[test]
    fn parse_capture_controls_supports_hue_for_full_color_override() {
        // The env override must cover every control the production colour set enforces
        // (saturation/contrast/HUE), so an operator can fully retune the picture,
        // including resetting hue — symmetric with `color_production_controls`.
        let c = parse_capture_controls("saturation=50,contrast=50,hue=0");
        assert_eq!(
            c,
            vec![
                CaptureControl {
                    id: V4L2_CID_SATURATION,
                    value: 50
                },
                CaptureControl {
                    id: V4L2_CID_CONTRAST,
                    value: 50
                },
                CaptureControl {
                    id: V4L2_CID_HUE,
                    value: 0
                },
            ]
        );
    }

    #[test]
    fn parse_capture_controls_skips_unknown_and_malformed_but_keeps_valid() {
        // A bad env must NOT crash capture: unknown names + malformed pairs are
        // dropped, valid ones retained.
        let c = parse_capture_controls("brightness=10,contrast=75,notapair,saturation=x");
        assert_eq!(
            c,
            vec![CaptureControl {
                id: V4L2_CID_CONTRAST,
                value: 75
            }],
            "only the valid contrast pair survives"
        );
    }

    #[test]
    fn color_production_controls_is_saturation50_contrast50_no_hue_338() {
        // #338: the certified COLOUR production set is exactly saturation=50,
        // contrast=50 (both = device defaults) and NOTHING ELSE. The old assertion
        // also required `hue=0` — that encoded the #338 regression: hue=0 is a MAX
        // shift on the ShadowCast card (default 50) = a pink tint, so hue must NOT be
        // force-set. saturation=50 is the ShadowCast factory colour level proven on
        // the live rig (channel_diff≈35).
        let c = color_production_controls();
        assert_eq!(
            c,
            vec![
                CaptureControl {
                    id: V4L2_CID_SATURATION,
                    value: 50
                },
                CaptureControl {
                    id: V4L2_CID_CONTRAST,
                    value: 50
                },
            ]
        );
        assert!(
            !c.iter().any(|x| x.id == V4L2_CID_HUE),
            "colour set must NOT force hue (the #338 pink-tint regression) — got {c:?}"
        );
    }

    #[test]
    fn production_path_enforces_color_controls() {
        // #296 REGRESSION GUARD: production (no CAMERA_BOX_CAPTURE_CONTROLS override,
        // no --record-grab) MUST enforce the certified COLOUR set at capture open, so a
        // stray saturation=0 left by a prior QR-test grab can NEVER persist as
        // grayscale on a live restart (the church-event regression). BEFORE the fix
        // this branch returned NO controls (Vec::new()), so this test FAILS on the
        // unfixed code and PASSES once production selects color_production_controls().
        let c = select_capture_controls(None, false);
        assert!(
            c.contains(&CaptureControl {
                id: V4L2_CID_SATURATION,
                value: 50
            }),
            "production must restore saturation=50 (colour) — got {c:?}"
        );
        assert!(
            c.contains(&CaptureControl {
                id: V4L2_CID_CONTRAST,
                value: 50
            }),
            "production must restore contrast=50 — got {c:?}"
        );
        // #338: production must NOT force hue (the old assertion required hue=0, which
        // is a pink tint on the ShadowCast card — that encoded the regression).
        assert!(
            !c.iter().any(|x| x.id == V4L2_CID_HUE),
            "production must NOT force hue (the #338 pink-tint regression) — got {c:?}"
        );
        assert_eq!(
            c,
            color_production_controls(),
            "production path must be exactly the certified colour set"
        );
    }

    #[test]
    fn select_capture_controls_grab_uses_color_set_not_sharp_312() {
        // #312: --record-grab (no env override) now selects the device-default COLOUR
        // set, NOT the #156 sharp set. The old assertion required the sharp set
        // (saturation=0/contrast=75) — that encoded the #312 regression: the sharp
        // set HURT the optical decode (ShadowCast ~50% undecodable vs the NZXT card
        // reading the same monitor clean on device defaults). Grab now matches
        // production. The sharp set stays available on demand via
        // CAMERA_BOX_CAPTURE_CONTROLS=certified (asserted separately).
        assert_eq!(
            select_capture_controls(None, true),
            color_production_controls()
        );
        assert_ne!(
            select_capture_controls(None, true),
            certified_cam1_controls(),
            "grab must NOT auto-apply the desaturating sharp set"
        );
    }

    #[test]
    fn select_capture_controls_env_override_wins_over_both() {
        // An explicit CAMERA_BOX_CAPTURE_CONTROLS override is honoured even in
        // production (record_grab=false) and even during a grab (record_grab=true).
        let parsed = parse_capture_controls("contrast=75,saturation=0");
        assert_eq!(
            select_capture_controls(Some("contrast=75,saturation=0"), false),
            parsed
        );
        assert_eq!(
            select_capture_controls(Some("contrast=75,saturation=0"), true),
            parsed
        );
    }

    #[test]
    fn select_capture_controls_explicit_empty_override_touches_nothing() {
        // An explicit empty/whitespace override is the deliberate "touch nothing"
        // escape hatch — it must NOT silently fall back to the colour set.
        assert!(select_capture_controls(Some(""), false).is_empty());
        assert!(select_capture_controls(Some("   "), false).is_empty());
    }

    /// Fake [`ControlIo`] device: supports only the listed control ids; any other id
    /// errors on set AND get — modelling a card (the NZXT CAM4 grab card) that
    /// exposes NO v4l2 picture controls.
    struct FakeDevice {
        supported: std::collections::HashSet<u32>,
        values: std::cell::RefCell<std::collections::HashMap<u32, i64>>,
    }

    impl FakeDevice {
        fn supporting(ids: &[u32]) -> Self {
            Self {
                supported: ids.iter().copied().collect(),
                values: std::cell::RefCell::new(std::collections::HashMap::new()),
            }
        }
    }

    impl ControlIo for FakeDevice {
        type Err = String;

        fn set_ctrl(&self, id: u32, value: i64) -> std::result::Result<(), String> {
            if self.supported.contains(&id) {
                self.values.borrow_mut().insert(id, value);
                Ok(())
            } else {
                Err(format!(
                    "control id={id:#010x} not supported by this device"
                ))
            }
        }

        fn get_ctrl(&self, id: u32) -> std::result::Result<i64, String> {
            self.values
                .borrow()
                .get(&id)
                .copied()
                .ok_or_else(|| format!("control id={id:#010x} not supported by this device"))
        }
    }

    #[test]
    fn apply_controls_tolerates_device_missing_a_control() {
        // #296 NZXT CAM4 GUARD: the NZXT Signal HD60 grab card exposes NO v4l2 picture
        // controls. Enforcing the colour set on it must log a warning and PROCEED —
        // NEVER become fatal — so CAM4 still streams. apply_controls_with returns a
        // tally (it cannot return an error); a control-less device yields all-failed
        // with capture continuing.
        let nzxt = FakeDevice::supporting(&[]); // supports nothing — the NZXT case
        let report = apply_controls_with(&nzxt, &color_production_controls());
        assert_eq!(
            report.failed, 2,
            "every unsupported colour control (saturation+contrast) is a non-fatal failure"
        );
        assert_eq!(
            report.applied, 0,
            "nothing could be applied on a control-less card"
        );
        assert_eq!(report.adjusted, 0);
        // Reaching here at all proves the apply NEVER aborted — the graceful guarantee.
    }

    #[test]
    fn apply_controls_applies_supported_controls_on_shadowcast_like_device() {
        // A ShadowCast-like card (CAM1/2/3) supports saturation+contrast+hue. The
        // certified colour set drives only saturation+contrast (#338: hue is left
        // untouched), so both apply and verify, restoring colour without a hue shift.
        let shadowcast =
            FakeDevice::supporting(&[V4L2_CID_SATURATION, V4L2_CID_CONTRAST, V4L2_CID_HUE]);
        let report = apply_controls_with(&shadowcast, &color_production_controls());
        assert_eq!(report.applied, 2, "both colour controls verified");
        assert_eq!(report.failed, 0);
        assert_eq!(report.adjusted, 0);
        assert_eq!(
            *shadowcast
                .values
                .borrow()
                .get(&V4L2_CID_SATURATION)
                .unwrap(),
            50,
            "saturation driven to the colour level"
        );
    }

    #[test]
    fn apply_controls_partial_support_skips_only_the_missing_control() {
        // A card that supports saturation but NOT contrast (a plausible intermediate
        // device): the supported control applies, the missing one is a non-fatal
        // failure, capture proceeds. (#338: the colour set is saturation+contrast —
        // hue is no longer in the set, so contrast stands in as the missing control.)
        let partial = FakeDevice::supporting(&[V4L2_CID_SATURATION]);
        let report = apply_controls_with(&partial, &color_production_controls());
        assert_eq!(report.applied, 1, "saturation applied");
        assert_eq!(
            report.failed, 1,
            "contrast unsupported -> one non-fatal failure"
        );
    }

    #[test]
    fn apply_controls_reports_clamp_when_readback_differs() {
        // Driver clamp: a device that accepts the set but reports a different value
        // back is 'adjusted' (soft/unverified), never fatal. Model it with a device
        // that stores a fixed clamped value regardless of the request.
        struct ClampDevice;
        impl ControlIo for ClampDevice {
            type Err = String;
            fn set_ctrl(&self, _id: u32, _value: i64) -> std::result::Result<(), String> {
                Ok(())
            }
            fn get_ctrl(&self, _id: u32) -> std::result::Result<i64, String> {
                Ok(999) // clamped to a value we never requested
            }
        }
        let report = apply_controls_with(&ClampDevice, &color_production_controls());
        assert_eq!(report.applied, 0);
        assert_eq!(
            report.adjusted, 2,
            "both colour controls read back clamped -> adjusted"
        );
        assert_eq!(report.failed, 0);
    }

    #[test]
    fn apply_controls_empty_is_noop() {
        let nzxt = FakeDevice::supporting(&[]);
        let report = apply_controls_with(&nzxt, &[]);
        assert_eq!(
            report,
            ControlReport::default(),
            "no controls -> empty report"
        );
    }

    #[test]
    fn production_controls_must_not_force_hue_338() {
        // #338 REGRESSION GUARD: color_production_controls() must NOT force HUE.
        // Production previously forced hue=0 (doc-claimed "neutral"), but the
        // ShadowCast capture card's V4L2 hue is min=0 max=100 default=50 — so hue=0
        // is a MAX shift = a pink/magenta tint on the live camera (the #338 symptom:
        // one cam pink, the NZXT cam — which exposes no controls — clean). The colour
        // set must touch only saturation+contrast (both = device default 50) and
        // leave hue untouched. FAILS on the unfixed code (which includes hue=0).
        assert!(
            !color_production_controls()
                .iter()
                .any(|c| c.id == V4L2_CID_HUE),
            "production colour set must NOT force hue (hue=0 is a pink tint on ShadowCast) — got {:?}",
            color_production_controls()
        );
    }

    #[test]
    fn grab_selection_must_not_desaturate_312() {
        // #312 REGRESSION GUARD: the grab / QR-decode selection (no env override)
        // must NOT desaturate the capture. The #156 sharp set (saturation=0,
        // contrast=75) was auto-applied to grab runs but HURT the optical decode (run
        // 312005: CAM1 ShadowCast w/ sharp set ~50% undecodable; CAM4 on device
        // defaults read the SAME monitor clean). The grab path must select the
        // device-default colour set instead. FAILS on the unfixed code (which selects
        // certified_cam1_controls() with saturation=0).
        let grab = select_capture_controls(None, true);
        assert!(
            !grab
                .iter()
                .any(|c| c.id == V4L2_CID_SATURATION && c.value == 0),
            "grab selection must NOT desaturate (saturation=0 hurts QR decode) — got {grab:?}"
        );
        assert_eq!(
            grab,
            color_production_controls(),
            "grab must select the device-default colour set, same as production"
        );
    }

    // ── #299 chroma metric tests ─────────────────────────────────────────────

    #[test]
    fn mean_chroma_grayscale_yuyv_is_near_zero_299() {
        // #299 RED: synthetic YUYV frame with all U=128, V=128 (neutral chroma =
        // grayscale). mean_chroma must return values close to 0 for both channels.
        // Width=4 pixels (2 macropixels), height=1.
        // Macropixel layout: Y0 U Y1 V — both U and V are 128 here.
        let frame: Vec<u8> = [0u8, 128, 0u8, 128] // macropixel 1
            .iter()
            .chain([0u8, 128, 0u8, 128].iter()) // macropixel 2
            .copied()
            .collect();
        let (u_dev, v_dev) = mean_chroma(&frame, 4, 1);
        assert!(
            u_dev < 0.5,
            "grayscale YUYV: mean |U-128| must be near 0, got {u_dev}"
        );
        assert!(
            v_dev < 0.5,
            "grayscale YUYV: mean |V-128| must be near 0, got {v_dev}"
        );
        // Also verify the classifier agrees
        assert!(
            !is_color_frame(u_dev, v_dev),
            "grayscale frame must not be classified as colour: u={u_dev} v={v_dev}"
        );
    }

    #[test]
    fn mean_chroma_colour_yuyv_exceeds_threshold_299() {
        // #299 RED: synthetic YUYV frame simulating a saturated blue field.
        // Approximate YUV for blue: Y≈41, U≈240, V≈110.
        // mean |U-128| ≈ 112, mean |V-128| ≈ 18 — both >> CHROMA_COLOR_THRESHOLD.
        // 256 pixels wide, 1 row: 128 macropixels = 512 bytes.
        let macro_pixel: [u8; 4] = [41, 240, 41, 110];
        let frame: Vec<u8> = macro_pixel.iter().copied().cycle().take(512).collect();
        let (u_dev, v_dev) = mean_chroma(&frame, 256, 1);
        assert!(
            u_dev > CHROMA_COLOR_THRESHOLD,
            "blue field: mean |U-128| ({u_dev:.1}) must exceed CHROMA_COLOR_THRESHOLD ({CHROMA_COLOR_THRESHOLD})"
        );
        assert!(
            is_color_frame(u_dev, v_dev),
            "colour YUYV must be classified as colour: u={u_dev:.1} v={v_dev:.1}"
        );
    }

    #[test]
    fn is_color_frame_threshold_boundary_299() {
        // #299 RED: verify the threshold boundary behaves correctly.
        // At or below threshold → grayscale; above → colour.
        assert!(
            !is_color_frame(CHROMA_COLOR_THRESHOLD, 0.0),
            "exactly at threshold must be grayscale (exclusive bound)"
        );
        assert!(
            !is_color_frame(0.0, CHROMA_COLOR_THRESHOLD),
            "exactly at threshold (V) must be grayscale"
        );
        assert!(
            is_color_frame(CHROMA_COLOR_THRESHOLD + 0.01, 0.0),
            "just above threshold (U) must be colour"
        );
        assert!(
            is_color_frame(0.0, CHROMA_COLOR_THRESHOLD + 0.01),
            "just above threshold (V) must be colour"
        );
    }
}
