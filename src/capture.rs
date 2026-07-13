use crate::capture_rate_health::GrabberModel;
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
    /// #286 — this frame's real V4L2 CAPTURE instant, in `CLOCK_MONOTONIC` 100ns units
    /// (converted from the driver's `v4l::buffer::Metadata.timestamp` via
    /// [`crate::genlock_stamp::v4l_timestamp_to_monotonic_100ns`]). The genlock emit path
    /// maps this into the `CLOCK_REALTIME` domain
    /// ([`crate::genlock_stamp::capture_realtime_100ns`]) and stamps the emitted NDI
    /// timecode from IT, not the arrival/send wall-clock — so each grabber card's
    /// photon->dequeue latency `d_X` no longer leaks into the genlock stamp. `0` where no
    /// real V4L2 metadata is available (e.g. [`VideoCapture::frame_info`]'s static getter,
    /// or test fixtures) — callers that don't feed a real capture timestamp never see this
    /// field used for genlock stamping.
    pub capture_monotonic_100ns: i64,
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

/// #696 — a delivered V4L2 buffer's CONTENT health, derived PURELY from its metadata (no
/// I/O). Live incident (2026-07-11, cam3): a 10-minute E2E run showed a ~5m20s contiguous
/// span where 52% of cam3's optical frames were undecodable (a "speckled/mottled noise
/// texture" replacing the QR modules, pixel-proven) — while the V4L2 `sequence` counter
/// stayed PERFECTLY contiguous (0 dropped frames) and the app's own capture-rate health
/// stayed green the whole time. [`sequence_gap`]-based drop-detection is therefore BLIND to
/// this failure class entirely: the frames were delivered on schedule, just with corrupted
/// PIXEL CONTENT — a self-heal or gate keyed only on sequence/rate would see a fully healthy
/// stream (this is exactly the blind spot #696 identified: "frame-count-based checks would
/// miss it entirely"). The kernel/driver DOES expose two signals for a buffer whose content
/// it already suspects is bad:
///   - `Flags::ERROR` — "Buffer is ready, but the data contained within is corrupted" (the
///     V4L2 core/driver's own corruption flag; uvcvideo sets this when its own payload
///     assembly detects a broken frame, e.g. after a URB completes with a non-zero status
///     such as the `-71`/EPROTO seen in cam3's kernel log shortly before the corrupted span).
///   - `bytesused` short of the format's expected byte count — for an UNCOMPRESSED format
///     (this appliance always captures raw YUYV, never MJPEG) the driver should always
///     deliver a full `stride*height` buffer; fewer bytes means the frame was truncated
///     (torn) mid-capture and the tail of the buffer holds stale/garbage data from a
///     previous frame.
///
/// Returns `Some(reason)` when either signal fires (the caller drops the frame rather than
/// forwarding known-corrupted content to NDI/genlock), else `None`. This does NOT explain
/// *why* the USB link glitches — that root cause remains open (#696) — it only gives the
/// pipeline a way to DETECT and not propagate a frame the driver itself already flagged.
pub fn frame_integrity_issue(
    flags: v4l::buffer::Flags,
    bytesused: u32,
    expected_bytes: u32,
) -> Option<String> {
    if flags.contains(v4l::buffer::Flags::ERROR) {
        return Some(
            "V4L2_BUF_FLAG_ERROR set (driver flagged this buffer's data as corrupted, see #696)"
                .to_string(),
        );
    }
    if bytesused < expected_bytes {
        return Some(format!(
            "short buffer: {bytesused} bytes captured (expected {expected_bytes} for this \
             format) — frame truncated mid-capture, see #696"
        ));
    }
    None
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
/// `pub` (not `pub(crate)`): the `camera-box` binary is a SEPARATE crate that
/// reads this as `camera_box::capture::CHROMA_SAMPLE_FRAMES`, so it must be part
/// of the library's public surface.
pub const CHROMA_SAMPLE_FRAMES: u32 = 60;

/// Compute mean |U−128| and mean |V−128| over a subsampled YUYV422 frame.
///
/// YUYV422 macropixel layout: `Y0 U Y1 V` (4 bytes, 2 pixels). The U byte is
/// at offset 1 and the V byte at offset 3 within each macropixel. Neutral grey
/// encodes U=V=128; a chromatic source pushes U and V away from 128.
///
/// Honors `stride` (bytes per row) so a device that pads its rows is sampled on
/// REAL pixel data only — the V4L2 mmap buffer length is `stride * height`, not
/// `width * 2 * height`, and a padded device (`stride > width * 2`) would
/// otherwise have its padding bytes sampled as bogus chroma. (Same reason
/// [`yuyv_to_gray8`] takes a stride.) Within each row it samples every
/// [`CHROMA_SAMPLE_STRIDE`] macropixels to keep per-call cost small; at
/// 1920×1080 stride=3840 that is ~15 samples/row × 1080 rows ≈ 16 k samples —
/// negligible even at the 1 Hz periodic log rate.
///
/// `width`/`height` are pixel dimensions; `stride` is the row pitch in bytes
/// (use `width * 2` for a tightly packed frame). A sample whose bytes fall
/// outside the buffer is skipped (defensive against a short final buffer).
///
/// Returns `(mean |U−128|, mean |V−128|)` in `[0.0, 128.0]`. For a grayscale
/// source both values are close to 0; a colour source produces values clearly
/// above [`CHROMA_COLOR_THRESHOLD`]. Returns `(0.0, 0.0)` when no in-bounds
/// sample exists (empty/undersized buffer or zero dimensions).
pub fn mean_chroma(frame: &[u8], width: usize, height: usize, stride: usize) -> (f32, f32) {
    let macropixels_per_row = width / 2; // YUYV packs 2 pixels per 4-byte macropixel
    if macropixels_per_row == 0 || height == 0 {
        return (0.0, 0.0);
    }
    let mut u_sum: u64 = 0;
    let mut v_sum: u64 = 0;
    let mut count: u64 = 0;
    for y in 0..height {
        let row_start = y * stride;
        let mut mp = 0usize;
        while mp < macropixels_per_row {
            // Macropixel `mp` of row `y`: Y0 U Y1 V — U at +1, V at +3.
            let idx = row_start + mp * 4;
            if idx + 3 < frame.len() {
                let u = frame[idx + 1] as i16 - 128;
                let v = frame[idx + 3] as i16 - 128;
                u_sum += u.unsigned_abs() as u64;
                v_sum += v.unsigned_abs() as u64;
                count += 1;
            }
            mp += CHROMA_SAMPLE_STRIDE;
        }
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

/// The V4L2-queried min/max/default range for one integer picture control
/// (`VIDIOC_QUERY_EXT_CTRL`), so a [`ControlTarget::RangeScaled`] target can be
/// resolved against the ACTUAL device range instead of a literal calibrated for
/// one specific card (#456: a 0-255 card's neutral is ~128, not the ShadowCast
/// literal 50 that darkened cam5's image).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlRange {
    /// Minimum value, inclusive.
    pub minimum: i64,
    /// Maximum value, inclusive.
    pub maximum: i64,
    /// The driver's own reported default (the manufacturer's neutral setting).
    pub default_value: i64,
}

/// How to resolve a [`CaptureControl`]'s value at apply time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlTarget {
    /// Apply this V4L2 value verbatim, bypassing range-scaling entirely. Used
    /// for explicit numeric `CAMERA_BOX_CAPTURE_CONTROLS=name=value` operator
    /// overrides — the operator already picked an absolute value for THIS
    /// specific card, so it must never be reinterpreted (#456 requirement: env
    /// overrides bypass range-scaling).
    Literal(i64),
    /// Resolve to `reference_pct` percent of the device's OWN queried
    /// `[minimum,maximum]` range (`VIDIOC_QUERY_EXT_CTRL`), where `reference_pct`
    /// was calibrated against the ShadowCast card's native 0-100 range (#456).
    /// `50` lands on the range's midpoint, which is ALSO each known card's own
    /// manufacturer default (50 on ShadowCast's 0-100 range, ~128 on cam5's
    /// 0-255 range) — so the certified COLOUR set's "neutral" (reference_pct=50)
    /// now lands on the correct neutral on ANY card, and the SHARP set's tuned
    /// values (75/0) scale the same way. Falls back to applying `reference_pct`
    /// as a literal if the device's range can't be queried (e.g. a driver
    /// without `VIDIOC_QUERY_EXT_CTRL` support) — a possibly-wrong value beats
    /// silently skipping the control, and matches the pre-#456 behaviour.
    RangeScaled { reference_pct: i64 },
}

/// One certified V4L2 capture control (`id` + how to resolve its value) to
/// apply at device open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureControl {
    pub id: u32,
    pub target: ControlTarget,
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
            target: ControlTarget::RangeScaled { reference_pct: 75 },
        },
        CaptureControl {
            id: V4L2_CID_SATURATION,
            target: ControlTarget::RangeScaled { reference_pct: 0 },
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
            Ok(value) => out.push(CaptureControl {
                id,
                target: ControlTarget::Literal(value),
            }),
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
            target: ControlTarget::RangeScaled { reference_pct: 50 },
        },
        CaptureControl {
            id: V4L2_CID_CONTRAST,
            target: ControlTarget::RangeScaled { reference_pct: 50 },
        },
    ]
}

/// The CERTIFIED Elgato 4K S corrective V4L2 capture control (#729 follow-up,
/// 2026-07-13): a documented-proven-need exception to the zero-touch policy above,
/// for the ONE thing on this card that IS correctable — the purple/violet tint's
/// visible MAGNITUDE — via a partial saturation reduction.
///
/// Background: live diagnosis (2026-07-12, cam1+cam6, both Elgato 4K S) proved the
/// tint is a genuine ISP/AWB characteristic of this card model — it reproduces
/// identically at every control's factory default, in both raw YUYV and the card's
/// own onboard MJPG encoder, at two resolutions, and its chroma is strongly
/// luma-correlated (`corr(Y,V) = -0.96`), which NO hue rotation can null (a
/// rotation preserves the error vector's magnitude; it can only move it between the
/// U and V channels — confirmed empirically below). So a full, lossless fix is not
/// achievable with the 4 controls this card exposes (`v4l2-ctl -d /dev/video1
/// --list-ctrls`: brightness/contrast/saturation/hue, all 0-255, default 128 — no
/// white-balance/gain/gamma/backlight control exists on this model to correct the
/// cast selectively).
///
/// What IS achievable: `saturation=0` fully suppresses the tint (proven in the
/// original diagnosis), and a live sweep on cam6 (2026-07-13, real low-light scene
/// content, hue/contrast/brightness held at their card defaults) showed the chroma
/// metric scales LINEARLY with the saturation setting:
///
/// ```text
/// saturation  128(100%)  96(75%)  64(50%)  48(37.5%)  32(25%)  24(18.75%)  16   8    0
/// u_dev       33.2       25.1     16.4     12.5       8.2      6.3         4.2  2.0  0.0
/// v_dev       42.4       31.7     20.8     15.6       10.5     7.8         5.2  2.7  0.0
/// ```
///
/// `saturation=32` (out of the card's 0-255 range, ≈12.5% of range / 25% of the
/// card's own default 128) lands closest to the healthy target (`u_dev≈7,
/// v_dev≈10.7`, cam5's ShadowCast baseline) — confirmed on BOTH affected units
/// (cam6: u_dev=8.2 v_dev=10.5; cam1: u_dev=8.3 v_dev=10.5, near-identical, matching
/// the original finding that the two cards behave identically) — and visually
/// collapses the vivid magenta/purple cast down to a much duller, close-to-neutral
/// dark tone (pixel-proof screenshots on #729).
///
/// Contrast and brightness were also swept (held at default while varying the
/// other): brightness has almost no effect on chroma (33.1→31.3 across the FULL
/// 0-255 range); contrast=0 measurably reduces chroma too, but only by flattening
/// ALL image contrast to near-uniform gray — useless as broadcast video. Neither is
/// a viable alternative to desaturating.
///
/// **The tradeoff is real and is NOT free**: because the tint and any genuine scene
/// colour share the exact same saturation gain (this is what makes `saturation=0`
/// able to fully suppress the tint in the first place), this correction ALSO mutes
/// real colour content on these 2 units to the same ~25% fraction. There is no
/// partial-saturation value that removes proportionally MORE of the defect than of
/// real colour — the two are inseparable through this control. Live validation of
/// this specific tradeoff against genuinely colourful (not just low-light/dark)
/// content was NOT possible this session (the cam2 test-pattern painter that would
/// normally provide bright reference colour was unreachable — see the cam2 disk
/// GOTCHA — and the live scene at diagnosis time was a dark, low-colour room), but
/// the linear scaling relationship measured above is clean enough (near-perfect
/// proportionality across the whole sweep) to generalize with confidence: real
/// colour on these 2 cameras will read at roughly 25% of its normal saturation.
/// This function stays its own named entry — never folded into
/// [`color_production_controls`] — precisely so a future decision to drop, retune,
/// or replace it (e.g. once real bright content can be checked, or if the physical
/// units are swapped for a different model) is a one-line change, same as any other
/// documented per-model policy in [`documented_controls_for_model`].
pub fn elgato_4k_s_corrective_controls() -> Vec<CaptureControl> {
    vec![CaptureControl {
        id: V4L2_CID_SATURATION,
        target: ControlTarget::RangeScaled { reference_pct: 12 },
    }]
}

/// Choose the V4L2 capture controls to enforce at device open.
///
/// - `env_spec = Some(spec)` — an explicit `CAMERA_BOX_CAPTURE_CONTROLS` override;
///   parse it ([`parse_capture_controls`]). Used by ad-hoc rig tweaks; an empty /
///   whitespace spec yields no controls (deliberate "touch nothing" escape hatch).
///   The certified SHARP set ([`certified_cam1_controls`]) stays available ON DEMAND
///   via `CAMERA_BOX_CAPTURE_CONTROLS=certified`. An explicit override ALWAYS wins,
///   regardless of `model` — an operator who typed a literal spec means it.
/// - `env_spec = None` — **#729 zero-touch by default**: camera-box does NOT write
///   any colour control unless `model` has a specifically documented, proven need.
///   That's [`GrabberModel::ShadowCast2`] (#296: a stray `saturation=0` left by a
///   prior grab can persist ON THE DEVICE across restarts and brick production to
///   grayscale — enforcing the certified COLOUR set at every open is how that
///   self-heals) and, as of 2026-07-13, [`GrabberModel::Elgato4kS`] (a proven,
///   documented hardware/ISP tint on this specific card model — see
///   [`elgato_4k_s_corrective_controls`] for the empirical tuning that established
///   the correction and its real colour-fidelity tradeoff). Every other model —
///   [`GrabberModel::NzxtSignalHd60`] and [`GrabberModel::Unknown`] — gets NO
///   controls written at all: plug-and-play, factory defaults, no ceremony.
///
/// #729 supersedes the PRE-existing "production always gets the colour set" design
/// (below, kept for history): that design forced the SAME certified colour set onto
/// EVERY model unconditionally, including cards with no documented need for it. Live
/// diagnosis (2026-07-12, cam1+cam6 purple/violet tint) proved this is actively
/// counter-productive for the Elgato 4K S — the tint reproduces IDENTICALLY with
/// every control already sitting at the card's own factory default, in BOTH raw
/// YUYV and the card's own onboard MJPG encoder, so forcing a colour set onto it
/// buys nothing and risks smearing a value calibrated for a DIFFERENT model onto it
/// after a physical card swap (`GrabberModel` is resolved by
/// `capture_rate_health::resolve_grabber_model`, which prefers the RUNTIME-detected
/// card over the static hostname convention for exactly this reason).
///
/// #296 (history): the old no-override branch used to return NO controls at all, so
/// a stray `saturation=0` left by a prior grab persisted and the live ShadowCast
/// cameras went grayscale — the certified COLOUR set was introduced so ShadowCast
/// self-heals colour on every open. That need is preserved here, scoped to
/// ShadowCast 2 only instead of applied blindly to every model.
///
/// #338/#312 (history): the grab path is NOT auto-given the SHARP set
/// (`certified_cam1_controls`, `saturation=0`/`contrast=75`) — that set was meant to
/// aid QR decode but HURT it (run 312005: a ShadowCast box with the sharp set read
/// the painter QR ~50% undecodable, while a control-less card read the SAME monitor
/// clean). Grab uses the SAME model-gated policy as production; the sharp set stays
/// available on demand via `CAMERA_BOX_CAPTURE_CONTROLS=certified`.
/// `_record_grab` is retained for call-site clarity but does not affect selection.
pub fn select_capture_controls(
    model: GrabberModel,
    env_spec: Option<&str>,
    _record_grab: bool,
) -> Vec<CaptureControl> {
    match env_spec {
        Some(spec) => parse_capture_controls(spec),
        None => documented_controls_for_model(model),
    }
}

/// #729 — the model→controls policy table itself. `ShadowCast2` has a documented, proven
/// need (#296's grab-time grayscale-brick risk); `Elgato4kS` has a SECOND, separately
/// documented, proven need as of 2026-07-13 (the corrective partial-saturation set —
/// [`elgato_4k_s_corrective_controls`]); every other model is zero-touch. Kept as its own
/// function so the policy is a single, obviously-auditable place — adding a NEW
/// documented-need model later is a one-line change here, not a scattered edit.
fn documented_controls_for_model(model: GrabberModel) -> Vec<CaptureControl> {
    match model {
        GrabberModel::ShadowCast2 => color_production_controls(),
        GrabberModel::Elgato4kS => elgato_4k_s_corrective_controls(),
        GrabberModel::NzxtSignalHd60 | GrabberModel::Unknown => Vec::new(),
    }
}

/// #728/#729 — best-effort runtime grabber-model detection: open `device_path` just long
/// enough to read its `VIDIOC_QUERYCAP` `card` string (the SAME non-exclusive, non-streaming
/// open pattern `config::find_capture_device` already uses), then drop it. Returns `None` on
/// ANY failure (device busy, missing, doesn't support querying) — this is a best-effort
/// enrichment, never a hard requirement; the caller always has the hostname-convention
/// fallback via `capture_rate_health::resolve_grabber_model`.
pub fn query_card_name(device_path: &str) -> Option<String> {
    let device = Device::with_path(device_path).ok()?;
    let caps = device.query_caps().ok()?;
    Some(caps.card)
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
    /// Error type surfaced when a get/set/query fails (only ever logged — never fatal).
    type Err: std::fmt::Display;
    /// Set integer control `id` to `value`.
    fn set_ctrl(&self, id: u32, value: i64) -> std::result::Result<(), Self::Err>;
    /// Read integer control `id` back.
    fn get_ctrl(&self, id: u32) -> std::result::Result<i64, Self::Err>;
    /// Query control `id`'s min/max/default range (`VIDIOC_QUERY_EXT_CTRL`), so a
    /// [`ControlTarget::RangeScaled`] target can be resolved against the ACTUAL
    /// device range rather than a literal calibrated for one specific card
    /// (#456).
    fn query_range(&self, id: u32) -> std::result::Result<ControlRange, Self::Err>;
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

    fn query_range(&self, id: u32) -> std::result::Result<ControlRange, Self::Err> {
        // `Device::query_controls()` enumerates every control the driver reports
        // (VIDIOC_QUERY_EXT_CTRL with V4L2_CTRL_FLAG_NEXT_CTRL) — there is no
        // narrower single-id query in the v4l crate's public API, so this
        // re-enumerates the full control list PER CALL (#456 deep-review note).
        // Deliberately NOT cached: it runs at most twice per device open (the
        // certified sets are always 2 controls) — never per-frame — so the O(n)
        // enumeration cost per call is negligible in absolute terms, and adding a
        // caching layer around the external `v4l::Device` type (which this crate
        // does not own, so it can't gain a cache field) would be a bigger
        // architectural change than this bounded, open-time-only call pattern
        // warrants. Revisit if the certified control sets ever grow beyond a
        // couple of controls.
        let controls = self.query_controls()?;
        controls
            .into_iter()
            .find(|desc| desc.id == id)
            .map(|desc| ControlRange {
                minimum: desc.minimum,
                maximum: desc.maximum,
                default_value: desc.default,
            })
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("control id={id:#010x} not reported by VIDIOC_QUERY_EXT_CTRL"),
                )
            })
    }
}

/// Resolve a [`CaptureControl`]'s [`ControlTarget`] to the literal V4L2 value to
/// apply (#456). A [`ControlTarget::Literal`] never queries anything — explicit
/// operator overrides always bypass range-scaling. A
/// [`ControlTarget::RangeScaled`] queries the device's own `[minimum,maximum]`
/// range and scales the reference percentage onto it; if the query fails (a
/// driver without `VIDIOC_QUERY_EXT_CTRL` support), the reference percentage is
/// applied as a literal instead — a possibly-wrong value beats silently
/// skipping the control, and matches the pre-#456 behaviour.
fn resolve_control_target<IO: ControlIo>(io: &IO, id: u32, target: ControlTarget) -> i64 {
    match target {
        ControlTarget::Literal(v) => v,
        ControlTarget::RangeScaled { reference_pct } => match io.query_range(id) {
            Ok(range) => scale_to_range(reference_pct, range),
            Err(e) => {
                tracing::warn!(
                    "capture control id={:#010x} range query failed: {} \
                     (falling back to reference value {} applied literally)",
                    id,
                    e,
                    reference_pct
                );
                reference_pct
            }
        },
    }
}

/// Scale a `reference_pct` (0-100, calibrated against the ShadowCast card's
/// native 0-100 range) onto a device's ACTUAL queried `[minimum,maximum]`
/// range. `reference_pct=50` — the certified COLOUR set's neutral — IS, by
/// definition, the manufacturer's own default (#456 follow-up: prefer the
/// queried `default_value` directly for the neutral case, rather than only its
/// numeric midpoint — both known cards happen to have a default equal to their
/// midpoint (50 on ShadowCast's 0-100 range, 128 on cam5's 0-255 range), but a
/// future card whose default ISN'T its midpoint must still resolve to its own
/// default, not a value the manufacturer never calls "neutral"). Any other
/// `reference_pct` (the tuned SHARP set's 75/0) has no such correspondence and
/// always uses proportional scaling (e.g. 75% -> ~191 on a 0-255 range,
/// matching the ShadowCast literal 75 on a 0-100 range).
fn scale_to_range(reference_pct: i64, range: ControlRange) -> i64 {
    if reference_pct == 50
        && range.default_value >= range.minimum
        && range.default_value <= range.maximum
    {
        return range.default_value;
    }
    let span = range.maximum - range.minimum;
    if span <= 0 {
        // Degenerate/unreliable range query -- the reference percentage is the
        // best remaining guess (matches the pre-#456 literal behaviour). Logged
        // (unlike a normal successful scale) since a broken range query is worth
        // the operator's attention.
        tracing::warn!(
            "capture control range query returned a degenerate range \
             (minimum={} >= maximum={}) -- falling back to reference value {} \
             applied literally",
            range.minimum,
            range.maximum,
            reference_pct
        );
        return reference_pct;
    }
    let fraction = reference_pct as f64 / 100.0;
    let scaled = (range.minimum as f64 + fraction * span as f64).round() as i64;
    scaled.clamp(range.minimum, range.maximum)
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
        let value = resolve_control_target(io, c.id, c.target);
        match io.set_ctrl(c.id, value) {
            Ok(()) => match io.get_ctrl(c.id) {
                Ok(got) if got == value => {
                    report.applied += 1;
                    tracing::info!(
                        "capture control id={:#010x} set to {} (verified)",
                        c.id,
                        value
                    );
                }
                Ok(got) => {
                    report.adjusted += 1;
                    tracing::warn!(
                        "capture control id={:#010x} requested {} but device reports {} \
                         (driver clamped/ignored)",
                        c.id,
                        value,
                        got
                    );
                }
                Err(e) => {
                    report.adjusted += 1;
                    tracing::warn!(
                        "capture control id={:#010x} set to {} but read-back failed: {}",
                        c.id,
                        value,
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
                    value,
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
    /// #696: cumulative count of delivered buffers [`process_frame`](Self::process_frame)
    /// flagged as content-corrupted ([`frame_integrity_issue`]) and DROPPED (never passed to
    /// the caller's callback) over this stream's life. Distinct from `dropped_captures`
    /// (frames the DEVICE never delivered at all) — these frames WERE delivered, on schedule,
    /// with a good `sequence`, but their pixel content was flagged as bad.
    corrupted_frames: u64,
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
            corrupted_frames: 0,
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

    /// #696: cumulative count of delivered buffers dropped for failing
    /// [`frame_integrity_issue`] (V4L2_BUF_FLAG_ERROR or a short/truncated buffer) — content
    /// corruption the DEVICE's own `sequence` counter never signals (see the doc on
    /// [`frame_integrity_issue`]). Surfaced in the periodic streaming report (`main.rs`),
    /// same as `dropped_captures`, so this failure class is finally visible without needing
    /// a full E2E optical decode to notice it.
    pub fn corrupted_frames(&self) -> u64 {
        self.corrupted_frames
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

        // #696 — drop (never forward to NDI/genlock) a buffer the driver/format itself
        // already flags as corrupted, BEFORE any further processing. Still counted into
        // `record_sequence` (the device DID deliver a buffer on schedule; only its CONTENT
        // was bad) so the sequence/drop accounting stays coherent — this is a distinct
        // failure class from a device-side drop (see `frame_integrity_issue`'s doc).
        let expected_bytes = self.stride * self.height;
        if let Some(reason) =
            frame_integrity_issue(metadata.flags, metadata.bytesused, expected_bytes)
        {
            self.corrupted_frames += 1;
            tracing::warn!(
                "capture device delivered a CORRUPTED buffer (v4l2 sequence {}): {} \
                 (total corrupted {}) — frame dropped, not sent",
                seq,
                reason,
                self.corrupted_frames
            );
            self.record_sequence(seq);
            return Ok(());
        }

        // #286 — the driver's real CAPTURE instant (V4L2 default clock domain is
        // CLOCK_MONOTONIC; see the module-level doc on `FrameInfo::capture_monotonic_100ns`),
        // converted to monotonic 100ns units for the genlock emit path.
        let capture_monotonic_100ns = crate::genlock_stamp::v4l_timestamp_to_monotonic_100ns(
            metadata.timestamp.sec,
            metadata.timestamp.usec,
        );

        let info = FrameInfo {
            width: self.width,
            height: self.height,
            fourcc: self.fourcc,
            stride: self.stride,
            capture_monotonic_100ns,
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
            // No real V4L2 metadata at this static getter (no frame was just dequeued) — see
            // the doc on `FrameInfo::capture_monotonic_100ns`.
            capture_monotonic_100ns: 0,
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
            capture_monotonic_100ns: 0,
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
            capture_monotonic_100ns: 0,
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

    // ---- #696: frame_integrity_issue — detect a corrupted/torn V4L2 buffer -----------------

    #[test]
    fn frame_integrity_issue_none_on_a_healthy_full_size_buffer() {
        let flags = v4l::buffer::Flags::from(0);
        assert_eq!(frame_integrity_issue(flags, 3840 * 1080, 3840 * 1080), None);
    }

    #[test]
    fn frame_integrity_issue_fires_on_v4l2_buf_flag_error() {
        let flags = v4l::buffer::Flags::ERROR;
        let reason = frame_integrity_issue(flags, 3840 * 1080, 3840 * 1080)
            .expect("#696: ERROR flag must be detected even with a full-size buffer");
        assert!(
            reason.contains("V4L2_BUF_FLAG_ERROR"),
            "reason should name the flag: {reason}"
        );
    }

    #[test]
    fn frame_integrity_issue_fires_on_a_short_buffer_even_without_the_error_flag() {
        // A torn/truncated frame: the driver delivered fewer bytes than the format requires,
        // but did NOT set Flags::ERROR — must still be caught (never rely on the flag alone).
        let flags = v4l::buffer::Flags::from(0);
        let expected = 3840 * 1080;
        let reason = frame_integrity_issue(flags, expected - 1, expected)
            .expect("#696: a short buffer must be detected even with no ERROR flag");
        assert!(
            reason.contains("short buffer"),
            "reason should explain the size mismatch: {reason}"
        );
    }

    #[test]
    fn frame_integrity_issue_tolerates_a_buffer_larger_than_expected() {
        // Some drivers report bytesused with alignment padding above the strict minimum —
        // never flag an OVER-size buffer as corrupted, only a SHORT one.
        let flags = v4l::buffer::Flags::from(0);
        let expected = 3840 * 1080;
        assert_eq!(frame_integrity_issue(flags, expected + 16, expected), None);
    }

    #[test]
    fn frame_integrity_issue_error_flag_message_cites_696() {
        let reason = frame_integrity_issue(v4l::buffer::Flags::ERROR, 100, 100).unwrap();
        assert!(reason.contains("#696"));
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
        // #156: the certified sharp-grab set is exactly contrast=75 + saturation=0
        // (as a PERCENTAGE of the device's queried range — #456: RangeScaled, not
        // a literal, so it scales correctly on a card whose range isn't 0-100).
        let c = certified_cam1_controls();
        assert_eq!(
            c,
            vec![
                CaptureControl {
                    id: V4L2_CID_CONTRAST,
                    target: ControlTarget::RangeScaled { reference_pct: 75 },
                },
                CaptureControl {
                    id: V4L2_CID_SATURATION,
                    target: ControlTarget::RangeScaled { reference_pct: 0 },
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
        // Explicit numeric operator overrides are LITERAL — never range-scaled
        // (#456 requirement 4: the operator already picked the exact device value).
        let c = parse_capture_controls("contrast=75,saturation=0");
        assert_eq!(
            c,
            vec![
                CaptureControl {
                    id: V4L2_CID_CONTRAST,
                    target: ControlTarget::Literal(75),
                },
                CaptureControl {
                    id: V4L2_CID_SATURATION,
                    target: ControlTarget::Literal(0),
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
                    target: ControlTarget::Literal(50),
                },
                CaptureControl {
                    id: V4L2_CID_CONTRAST,
                    target: ControlTarget::Literal(50),
                },
                CaptureControl {
                    id: V4L2_CID_HUE,
                    target: ControlTarget::Literal(0),
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
                target: ControlTarget::Literal(75),
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
                    target: ControlTarget::RangeScaled { reference_pct: 50 },
                },
                CaptureControl {
                    id: V4L2_CID_CONTRAST,
                    target: ControlTarget::RangeScaled { reference_pct: 50 },
                },
            ]
        );
        assert!(
            !c.iter().any(|x| x.id == V4L2_CID_HUE),
            "colour set must NOT force hue (the #338 pink-tint regression) — got {c:?}"
        );
    }

    // #729 — these three tests were REWRITTEN (not just extended) to reflect the zero-touch-
    // by-default redesign: the OLD design forced the certified colour set onto EVERY model
    // unconditionally; live diagnosis (2026-07-12, cam1+cam6 Elgato purple/violet tint,
    // reproduces at factory-default controls) proved that's actively wrong for models with no
    // documented need. Genuinely-wrong-test justification per tdd-workflow.md: the old
    // assertions encoded the now-superseded "always force colour" design as if it were a
    // permanent regression guard; #296's real, still-valid need (ShadowCast 2's grab-time
    // grayscale-brick risk) is preserved below, now correctly SCOPED to ShadowCast2 only.

    #[test]
    fn shadowcast2_gets_the_certified_colour_set_296() {
        // #296 REGRESSION GUARD (preserved, now model-scoped): ShadowCast 2 (no
        // CAMERA_BOX_CAPTURE_CONTROLS override, no --record-grab) MUST still enforce the
        // certified COLOUR set at capture open, so a stray saturation=0 left by a prior
        // QR-test grab can NEVER persist as grayscale on a live restart (the church-event
        // regression this model is uniquely exposed to).
        let c = select_capture_controls(GrabberModel::ShadowCast2, None, false);
        assert!(
            c.contains(&CaptureControl {
                id: V4L2_CID_SATURATION,
                target: ControlTarget::RangeScaled { reference_pct: 50 },
            }),
            "ShadowCast 2 must restore saturation=50% (colour) — got {c:?}"
        );
        assert!(
            c.contains(&CaptureControl {
                id: V4L2_CID_CONTRAST,
                target: ControlTarget::RangeScaled { reference_pct: 50 },
            }),
            "ShadowCast 2 must restore contrast=50% — got {c:?}"
        );
        // #338: must NOT force hue (hue=0 is a pink tint on the ShadowCast card).
        assert!(
            !c.iter().any(|x| x.id == V4L2_CID_HUE),
            "must NOT force hue (the #338 pink-tint regression) — got {c:?}"
        );
        assert_eq!(
            c,
            color_production_controls(),
            "ShadowCast 2's documented-need path must be exactly the certified colour set"
        );
    }

    #[test]
    fn nzxt_and_unknown_are_zero_touch_by_default_729() {
        // #729: no documented need for the certified colour set on these models.
        // Plug-and-play means camera-box writes NOTHING here.
        for model in [GrabberModel::NzxtSignalHd60, GrabberModel::Unknown] {
            assert!(
                select_capture_controls(model, None, false).is_empty(),
                "{model:?} must be zero-touch (no CAMERA_BOX_CAPTURE_CONTROLS override) — #729"
            );
        }
    }

    #[test]
    fn elgato_4k_s_gets_the_corrective_saturation_set_729_followup() {
        // #729 follow-up (2026-07-13): the Elgato 4K S has a SECOND documented, proven
        // need — a partial-saturation correction for its own hardware/ISP purple/violet
        // tint (empirically tuned live on cam1+cam6, both reading u_dev≈8.2-8.3/v_dev≈10.5
        // at this setting, matching the healthy ≈7/10.7 target). FAILS on the unfixed
        // code (which still routes Elgato4kS to zero-touch / Vec::new()).
        let c = select_capture_controls(GrabberModel::Elgato4kS, None, false);
        assert_eq!(
            c,
            elgato_4k_s_corrective_controls(),
            "Elgato 4K S must apply its documented corrective saturation set — got {c:?}"
        );
        assert!(
            c.contains(&CaptureControl {
                id: V4L2_CID_SATURATION,
                target: ControlTarget::RangeScaled { reference_pct: 12 },
            }),
            "Elgato 4K S must reduce saturation to the certified corrective level — got {c:?}"
        );
        // Never the FULL desaturation of the sharp set — that would kill all colour, not
        // just the tint (saturation=0 was already proven to fully suppress the tint, but
        // this documented set targets the healthy chroma band, not zero).
        assert!(
            !c.iter().any(|x| x.id == V4L2_CID_SATURATION
                && x.target == ControlTarget::RangeScaled { reference_pct: 0 }),
            "must NOT be the full-desaturation sharp value — got {c:?}"
        );
        // Contrast/brightness/hue are left untouched — the sweep proved they don't help
        // (hue only rotates the error between channels; contrast=0 just flattens the
        // whole image) and touching them risks a NEW, undocumented drift.
        assert!(
            !c.iter().any(|x| x.id == V4L2_CID_HUE),
            "must NOT touch hue — got {c:?}"
        );
        assert!(
            !c.iter().any(|x| x.id == V4L2_CID_CONTRAST),
            "must NOT touch contrast — got {c:?}"
        );
    }

    #[test]
    fn select_capture_controls_grab_matches_production_not_sharp_312() {
        // #312: --record-grab (no env override) selects the SAME model-gated policy as
        // production, NOT the #156 sharp set. The sharp set HURT the optical decode (run
        // 312005: a ShadowCast box with the sharp set read the painter QR ~50% undecodable,
        // while a control-less card read the SAME monitor clean). The sharp set stays
        // available on demand via CAMERA_BOX_CAPTURE_CONTROLS=certified (asserted
        // separately).
        assert_eq!(
            select_capture_controls(GrabberModel::ShadowCast2, None, true),
            color_production_controls()
        );
        assert_ne!(
            select_capture_controls(GrabberModel::ShadowCast2, None, true),
            certified_cam1_controls(),
            "grab must NOT auto-apply the desaturating sharp set"
        );
        // #729 follow-up: grab on the Elgato 4K S gets the SAME corrective set as
        // production (the certified partial-saturation correction), not the sharp set,
        // and not zero-touch either.
        assert_eq!(
            select_capture_controls(GrabberModel::Elgato4kS, None, true),
            elgato_4k_s_corrective_controls(),
            "grab on the Elgato 4K S must apply the SAME documented corrective set as production — #729"
        );
        assert_ne!(
            select_capture_controls(GrabberModel::Elgato4kS, None, true),
            certified_cam1_controls(),
            "grab on the Elgato 4K S must NOT auto-apply the desaturating sharp set either"
        );
    }

    #[test]
    fn select_capture_controls_env_override_wins_over_every_model() {
        // An explicit CAMERA_BOX_CAPTURE_CONTROLS override is honoured regardless of model
        // or record_grab — an operator who typed a literal spec means it, even on a
        // zero-touch model.
        let parsed = parse_capture_controls("contrast=75,saturation=0");
        for model in [
            GrabberModel::ShadowCast2,
            GrabberModel::Elgato4kS,
            GrabberModel::NzxtSignalHd60,
            GrabberModel::Unknown,
        ] {
            assert_eq!(
                select_capture_controls(model, Some("contrast=75,saturation=0"), false),
                parsed
            );
            assert_eq!(
                select_capture_controls(model, Some("contrast=75,saturation=0"), true),
                parsed
            );
        }
    }

    #[test]
    fn select_capture_controls_explicit_empty_override_touches_nothing() {
        // An explicit empty/whitespace override is the deliberate "touch nothing" escape
        // hatch — must NOT silently fall back to the colour set, even for ShadowCast2.
        assert!(select_capture_controls(GrabberModel::ShadowCast2, Some(""), false).is_empty());
        assert!(select_capture_controls(GrabberModel::ShadowCast2, Some("   "), false).is_empty());
    }

    /// Fake [`ControlIo`] device: supports only the listed control ids; any other id
    /// errors on set AND get — modelling a card (the NZXT CAM4 grab card) that
    /// exposes NO v4l2 picture controls. Optionally carries a queried
    /// [`ControlRange`] per id (`with_range`), modelling a real device's
    /// `VIDIOC_QUERY_EXT_CTRL` response for the #456 range-aware resolution
    /// tests; an id with no attached range models a driver whose range query
    /// fails/is unsupported.
    struct FakeDevice {
        supported: std::collections::HashSet<u32>,
        values: std::cell::RefCell<std::collections::HashMap<u32, i64>>,
        ranges: std::cell::RefCell<std::collections::HashMap<u32, ControlRange>>,
    }

    impl FakeDevice {
        fn supporting(ids: &[u32]) -> Self {
            Self {
                supported: ids.iter().copied().collect(),
                values: std::cell::RefCell::new(std::collections::HashMap::new()),
                ranges: std::cell::RefCell::new(std::collections::HashMap::new()),
            }
        }

        /// Attach a queried [`ControlRange`] for `id`, as if `VIDIOC_QUERY_EXT_CTRL`
        /// reported it (#456).
        fn with_range(self, id: u32, range: ControlRange) -> Self {
            self.ranges.borrow_mut().insert(id, range);
            self
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

        fn query_range(&self, id: u32) -> std::result::Result<ControlRange, String> {
            self.ranges
                .borrow()
                .get(&id)
                .copied()
                .ok_or_else(|| format!("no queried range for control id={id:#010x}"))
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
            fn query_range(&self, _id: u32) -> std::result::Result<ControlRange, String> {
                Err("no range query support".to_string())
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

    // ── #456 range-aware control resolution ─────────────────────────────────

    #[test]
    fn range_scaled_neutral_stays_50_on_a_shadowcast_0_100_card_456() {
        // A ShadowCast-like card's queried range is 0-100 (default 50) — the
        // certified COLOUR set's neutral (reference_pct=50) must resolve to
        // exactly 50 here, unchanged from the pre-#456 literal behaviour on
        // THIS specific card.
        let shadowcast = FakeDevice::supporting(&[V4L2_CID_SATURATION, V4L2_CID_CONTRAST])
            .with_range(
                V4L2_CID_SATURATION,
                ControlRange {
                    minimum: 0,
                    maximum: 100,
                    default_value: 50,
                },
            )
            .with_range(
                V4L2_CID_CONTRAST,
                ControlRange {
                    minimum: 0,
                    maximum: 100,
                    default_value: 50,
                },
            );
        let report = apply_controls_with(&shadowcast, &color_production_controls());
        assert_eq!(report.applied, 2);
        assert_eq!(
            *shadowcast
                .values
                .borrow()
                .get(&V4L2_CID_SATURATION)
                .unwrap(),
            50
        );
        assert_eq!(
            *shadowcast.values.borrow().get(&V4L2_CID_CONTRAST).unwrap(),
            50
        );
    }

    #[test]
    fn range_scaled_neutral_resolves_to_range_midpoint_on_cam5_style_0_255_card_456() {
        // #456 RED: cam5's grab card queries a 0-255 range (default 128). The
        // certified COLOUR set's literal 50 (calibrated for the ShadowCast card's
        // 0-100 range) previously landed at ~20% on a 0-255 card = dark/washed-out.
        // Range-aware resolution must scale reference_pct=50 to THIS card's OWN
        // midpoint (~128), not the ShadowCast literal 50. FAILS on the unfixed
        // code (which applies reference_pct as a bare literal, landing on 50).
        let cam5 = FakeDevice::supporting(&[V4L2_CID_SATURATION, V4L2_CID_CONTRAST])
            .with_range(
                V4L2_CID_SATURATION,
                ControlRange {
                    minimum: 0,
                    maximum: 255,
                    default_value: 128,
                },
            )
            .with_range(
                V4L2_CID_CONTRAST,
                ControlRange {
                    minimum: 0,
                    maximum: 255,
                    default_value: 128,
                },
            );
        let report = apply_controls_with(&cam5, &color_production_controls());
        assert_eq!(report.applied, 2);
        let sat = *cam5.values.borrow().get(&V4L2_CID_SATURATION).unwrap();
        let con = *cam5.values.borrow().get(&V4L2_CID_CONTRAST).unwrap();
        assert!(
            (120..=136).contains(&sat),
            "saturation should resolve near this card's own midpoint (~128), got {sat}"
        );
        assert!(
            (120..=136).contains(&con),
            "contrast should resolve near this card's own midpoint (~128), got {con}"
        );
        assert_ne!(
            sat, 50,
            "must NOT apply the ShadowCast literal 50 on a 0-255 card (the #456 dark-image bug)"
        );
        assert_ne!(con, 50, "same for contrast — the #456 dark-image bug");
    }

    #[test]
    fn range_scaled_sharp_set_scales_proportionally_on_a_0_255_card_456() {
        // certified_cam1_controls() (contrast=75%, saturation=0%) must scale the
        // SAME way as the colour set on a differently-ranged card (issue #456:
        // "The SHARP set should scale the same way").
        let cam5 = FakeDevice::supporting(&[V4L2_CID_SATURATION, V4L2_CID_CONTRAST])
            .with_range(
                V4L2_CID_CONTRAST,
                ControlRange {
                    minimum: 0,
                    maximum: 255,
                    default_value: 128,
                },
            )
            .with_range(
                V4L2_CID_SATURATION,
                ControlRange {
                    minimum: 0,
                    maximum: 255,
                    default_value: 128,
                },
            );
        let report = apply_controls_with(&cam5, &certified_cam1_controls());
        assert_eq!(report.applied, 2);
        let con = *cam5.values.borrow().get(&V4L2_CID_CONTRAST).unwrap();
        let sat = *cam5.values.borrow().get(&V4L2_CID_SATURATION).unwrap();
        assert_eq!(
            sat, 0,
            "0% reference maps to the card's own minimum on any range"
        );
        assert!(
            (185..=196).contains(&con),
            "75% reference should scale to ~191 on a 0-255 range, got {con}"
        );
    }

    #[test]
    fn range_scaled_falls_back_to_literal_when_device_range_query_fails_456() {
        // A card that supports the controls but whose range query fails/is
        // unsupported (no `.with_range(...)` data attached) must still apply
        // gracefully — falling back to the reference_pct as a literal value,
        // exactly the pre-#456 ShadowCast-literal behaviour, never a hard failure.
        let legacy = FakeDevice::supporting(&[V4L2_CID_SATURATION, V4L2_CID_CONTRAST]);
        let report = apply_controls_with(&legacy, &color_production_controls());
        assert_eq!(
            report.applied, 2,
            "a range-query failure must still let the control apply (literal fallback)"
        );
        assert_eq!(
            *legacy.values.borrow().get(&V4L2_CID_SATURATION).unwrap(),
            50
        );
    }

    #[test]
    fn range_scaled_neutral_prefers_queried_default_over_numeric_midpoint_456() {
        // #456 follow-up (deep-review finding): the certified COLOUR set's neutral
        // (reference_pct=50) is BY DEFINITION the manufacturer's own default. Both
        // known cards (ShadowCast 50/100, cam5 128/255) happen to have a default
        // that equals their numeric midpoint, so pure proportional scaling and
        // "prefer the queried default" agree for them. A card whose default is NOT
        // its numeric midpoint must still resolve to its OWN default, not a
        // midpoint the manufacturer never calls "neutral". FAILS before this fix
        // (proportional midpoint = 0 + 0.5*200 = 100), PASSES after (queried
        // default = 140 is used directly).
        let weird_card = FakeDevice::supporting(&[V4L2_CID_SATURATION, V4L2_CID_CONTRAST])
            .with_range(
                V4L2_CID_SATURATION,
                ControlRange {
                    minimum: 0,
                    maximum: 200,
                    default_value: 140,
                },
            )
            .with_range(
                V4L2_CID_CONTRAST,
                ControlRange {
                    minimum: 0,
                    maximum: 200,
                    default_value: 140,
                },
            );
        let report = apply_controls_with(&weird_card, &color_production_controls());
        assert_eq!(report.applied, 2);
        assert_eq!(
            *weird_card
                .values
                .borrow()
                .get(&V4L2_CID_SATURATION)
                .unwrap(),
            140,
            "must use the queried default (140), not the numeric midpoint (100)"
        );
        assert_eq!(
            *weird_card.values.borrow().get(&V4L2_CID_CONTRAST).unwrap(),
            140,
            "must use the queried default (140), not the numeric midpoint (100)"
        );
    }

    #[test]
    fn range_scaled_sharp_set_stays_75_and_0_on_a_shadowcast_0_100_card_456() {
        // #456 follow-up (deep-review finding: missing coverage): the SHARP set
        // (certified_cam1_controls, 75%/0%) resolved on a real ShadowCast-shaped
        // 0-100 range must reproduce the exact pre-#456 literal values (75/0) —
        // the regression concern already covered for the COLOUR set was untested
        // for the SHARP set.
        let shadowcast = FakeDevice::supporting(&[V4L2_CID_SATURATION, V4L2_CID_CONTRAST])
            .with_range(
                V4L2_CID_CONTRAST,
                ControlRange {
                    minimum: 0,
                    maximum: 100,
                    default_value: 50,
                },
            )
            .with_range(
                V4L2_CID_SATURATION,
                ControlRange {
                    minimum: 0,
                    maximum: 100,
                    default_value: 50,
                },
            );
        let report = apply_controls_with(&shadowcast, &certified_cam1_controls());
        assert_eq!(report.applied, 2);
        assert_eq!(
            *shadowcast.values.borrow().get(&V4L2_CID_CONTRAST).unwrap(),
            75
        );
        assert_eq!(
            *shadowcast
                .values
                .borrow()
                .get(&V4L2_CID_SATURATION)
                .unwrap(),
            0
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
        // certified_cam1_controls() with saturation=0). #729: scoped to ShadowCast2, the
        // model this incident actually happened on (CAM1) and the only model with a
        // documented colour-set need post-#729.
        let grab = select_capture_controls(GrabberModel::ShadowCast2, None, true);
        assert!(
            !grab.iter().any(|c| c.id == V4L2_CID_SATURATION
                && c.target == ControlTarget::RangeScaled { reference_pct: 0 }),
            "grab selection must NOT desaturate (saturation=0% hurts QR decode) — got {grab:?}"
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
        // stride = width * 2 (tightly packed, no row padding).
        let (u_dev, v_dev) = mean_chroma(&frame, 4, 1, 8);
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
        // mean |U-128| ≈ 112 (clearly above threshold); mean |V-128| ≈ 18 (above
        // threshold by a smaller margin). Both must exceed CHROMA_COLOR_THRESHOLD.
        // 256 pixels wide, 1 row: 128 macropixels = 512 bytes; stride = width * 2.
        let macro_pixel: [u8; 4] = [41, 240, 41, 110];
        let frame: Vec<u8> = macro_pixel.iter().copied().cycle().take(512).collect();
        let (u_dev, v_dev) = mean_chroma(&frame, 256, 1, 512);
        assert!(
            u_dev > CHROMA_COLOR_THRESHOLD,
            "blue field: mean |U-128| ({u_dev:.1}) must exceed CHROMA_COLOR_THRESHOLD ({CHROMA_COLOR_THRESHOLD})"
        );
        assert!(
            v_dev > CHROMA_COLOR_THRESHOLD,
            "blue field: mean |V-128| ({v_dev:.1}) must also exceed CHROMA_COLOR_THRESHOLD ({CHROMA_COLOR_THRESHOLD})"
        );
        assert!(
            is_color_frame(u_dev, v_dev),
            "colour YUYV must be classified as colour: u={u_dev:.1} v={v_dev:.1}"
        );
    }

    #[test]
    fn mean_chroma_honors_stride_padding_299() {
        // #299: a row-padded device (stride > width*2) must be sampled on REAL
        // pixel data only — never the padding bytes. 2px wide × 2 rows, stride=6
        // (4 packed bytes + 2 pad). Row 0 is neutral grey (U=V=128 → dev 0);
        // row 1 is strongly chromatic (U=V=255 → dev 127). With stride honored,
        // both rows' real macropixels are sampled → mean dev = (0 + 127)/2 = 63.5
        // and the frame classifies as colour. If stride were ignored (treated as
        // packed width*2=4), row 1's data would be missed and/or padding (0 →
        // dev 128) wrongly sampled — either way the result would differ.
        let row0 = [0u8, 128, 0u8, 128, 0, 0]; // Y0 U Y1 V pad pad — neutral
        let row1 = [0u8, 255, 0u8, 255, 0, 0]; // Y0 U Y1 V pad pad — saturated
        let mut data = Vec::new();
        data.extend_from_slice(&row0);
        data.extend_from_slice(&row1);
        let (u_dev, v_dev) = mean_chroma(&data, 2, 2, 6);
        assert!(
            (u_dev - 63.5).abs() < 0.01,
            "stride-padded: mean |U-128| must be 63.5 (rows 0+1 sampled, padding skipped), got {u_dev}"
        );
        assert!(
            (v_dev - 63.5).abs() < 0.01,
            "stride-padded: mean |V-128| must be 63.5, got {v_dev}"
        );
        assert!(
            is_color_frame(u_dev, v_dev),
            "stride-padded chromatic frame must classify as colour: u={u_dev} v={v_dev}"
        );
    }

    #[test]
    fn mean_chroma_empty_or_zero_dims_is_zero_299() {
        // #299: defensive — empty buffer or zero dimensions yields (0,0), no panic.
        assert_eq!(mean_chroma(&[], 0, 0, 0), (0.0, 0.0));
        assert_eq!(mean_chroma(&[1, 2, 3, 4], 0, 1, 0), (0.0, 0.0));
        // Undersized buffer (no in-bounds macropixel) also yields (0,0).
        assert_eq!(mean_chroma(&[1, 2], 2, 1, 4), (0.0, 0.0));
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
