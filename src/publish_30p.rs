//! #792 — optional SECONDARY 30fps NDI stream: 2-frame temporal blend of the emitted 60fps
//! pairs, published alongside the primary stream (e.g. "CAM1 (usb)" + "CAM1 (30p)").
//!
//! WHY a blend and not decimation: strih's 30fps canvas already decimates the 60fps feed
//! today (takes every 2nd frame → sharp but juddery pans). This stream exists to offer the
//! OTHER motion rendition: averaging each (2k, 2k+1) frame pair reconstructs a continuous
//! ~1/30s-adjacent exposure — the classic 180°-shutter-equivalent motion blur at 30fps
//! (what a Teranex "FRC Aperture" knob does). On a static scene the average of two
//! near-identical frames is the same image with ~sqrt(2) LESS sensor noise; the only
//! failure mode is double-image ghosting when the SOURCE shutter is short — hence the
//! per-box blend knob (retreat toward decimation without a fleet rebuild).
//!
//! ZERO-IMPACT CONTRACT (the reason this module exists as its own worker):
//! - The 60p capture/emit hot path does ONE bounded memcpy + ONE `try_send` per emitted
//!   frame ([`Tee::tee`]). The channel is bounded ([`CHANNEL_DEPTH`]); a full channel DROPS
//!   the frame (counted, never blocks). This is the deliberate OPPOSITE of the burn ring's
//!   back-pressuring submit (`probe::genlock::BurnRing`) — a burn id must never be lost,
//!   whereas the 30p stream must never be able to stall the broadcast feed.
//! - Everything heavy (pairing, blend, YUYV→UYVY convert, SpeedHQ encode inside the NDI
//!   send) runs on the "publish-30p" worker thread, pinned OFF the isolated capture core
//!   ([`crate::affinity::pin_off_capture_core`]).
//! - Buffers recycle through a bounded return channel (no steady-state allocation).
//!
//! Config (systemd drop-in / env, mirrors `CAMERA_BOX_GENLOCK_FPS`'s pattern):
//! - `CAMERA_BOX_PUBLISH_30P=1` — enable (unset/0 ⇒ bit-identical legacy behavior).
//! - `CAMERA_BOX_PUBLISH_30P_BLEND=0.0..0.5` — MAX weight of the pair's SECOND frame.
//!   0.5 = full 50/50 average (default), 0.0 = pure decimation (keep first frame),
//!   between = weighted average (less ghosting, less smoothness).
//! - `CAMERA_BOX_PUBLISH_30P_MODE=adaptive|fixed` — default `adaptive`: MOTION-ADAPTIVE
//!   blending. Per-macropixel luma difference ramps the blend weight from the configured
//!   max (static areas: full average = smooth + ~sqrt(2) noise reduction) down to ZERO on
//!   moving edges (no double-image ghosting even with a short outdoor-sunlight shutter).
//!   This makes the stream self-tuning — the operator never has to touch the knob; the
//!   knob + `fixed` mode remain as an explicit override/fallback. Same tier as broadcast
//!   "motion adaptive" standards conversion (vs the naive fixed blend below it and the
//!   GPU-class motion-compensated tier above it).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;

use crate::capture::FrameInfo;

/// Bounded tee-channel depth (frames in flight toward the worker). Small on purpose:
/// the worker consumes 60/s and outputs 30/s; depth 4 absorbs scheduling jitter while
/// bounding the 30p stream's added latency to a few source frames.
pub const CHANNEL_DEPTH: usize = 4;

/// Recycled-buffer pool depth: channel depth + the worker's in-flight pair + slack.
pub const POOL_CAP: usize = CHANNEL_DEPTH + 2;

/// Default blend weight of the pair's second frame (full 50/50 average).
pub const BLEND_DEFAULT: f32 = 0.5;

/// The blend knob's ceiling: 0.5 is the symmetric average; weights above it would just
/// mirror the same renditions with the pair's roles swapped (and bias the stamp).
pub const BLEND_MAX: f32 = 0.5;

/// Worker stats log cadence, in OUTPUT frames (~10 s at 30fps).
pub const STATS_LOG_EVERY_OUTPUTS: u64 = 300;

/// Motion-adaptive ramp: luma differences at or below this are treated as sensor noise /
/// static content → full configured blend. 8-bit LSB.
pub const ADAPT_LUMA_T_LO: u8 = 8;

/// Motion-adaptive ramp: luma differences at or above this are a moving edge → blend
/// weight ZERO (keep the first frame sharp, no ghost). Linear ramp between the two.
pub const ADAPT_LUMA_T_HI: u8 = 32;

/// Parsed `CAMERA_BOX_PUBLISH_30P*` configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    pub enabled: bool,
    /// MAX weight of the pair's SECOND frame, clamped to `[0.0, BLEND_MAX]`.
    pub blend: f32,
    /// Motion-adaptive blending (default). `false` = fixed whole-frame weight.
    pub adaptive: bool,
}

/// Pure parse of the enable flag ("1"/"true"/"yes", case-insensitive, trimmed).
pub fn parse_enabled(v: Option<&str>) -> bool {
    matches!(
        v.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

/// Pure parse of the blend knob. Returns the resolved weight plus an optional
/// human-readable warning (invalid / out-of-range input → default/clamped).
pub fn parse_blend(v: Option<&str>) -> (f32, Option<String>) {
    let Some(raw) = v.map(str::trim).filter(|s| !s.is_empty()) else {
        return (BLEND_DEFAULT, None);
    };
    match raw.parse::<f32>() {
        Ok(b) if b.is_finite() => {
            if (0.0..=BLEND_MAX).contains(&b) {
                (b, None)
            } else {
                let clamped = b.clamp(0.0, BLEND_MAX);
                (
                    clamped,
                    Some(format!(
                        "CAMERA_BOX_PUBLISH_30P_BLEND={raw} out of range [0.0, {BLEND_MAX}] — clamped to {clamped}"
                    )),
                )
            }
        }
        _ => (
            BLEND_DEFAULT,
            Some(format!(
                "CAMERA_BOX_PUBLISH_30P_BLEND={raw} is not a number — using default {BLEND_DEFAULT}"
            )),
        ),
    }
}

/// Pure parse of the mode knob: `adaptive` (default, incl. unset) or `fixed`. Anything
/// else falls back to adaptive with a warning.
pub fn parse_mode(v: Option<&str>) -> (bool, Option<String>) {
    match v.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        None | Some("") | Some("adaptive") => (true, None),
        Some("fixed") => (false, None),
        Some(other) => (
            true,
            Some(format!(
                "CAMERA_BOX_PUBLISH_30P_MODE={other} unknown (adaptive|fixed) — using adaptive"
            )),
        ),
    }
}

impl Config {
    /// Pure core (review finding: the env-NAME→field glue must be testable): resolve the
    /// config through an injected lookup — `CAMERA_BOX_PUBLISH_30P`, `_BLEND`, `_MODE`.
    /// Unset ⇒ disabled. Parse warnings are logged here (once, at startup).
    pub fn from_env_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let enabled = parse_enabled(get("CAMERA_BOX_PUBLISH_30P").as_deref());
        let (blend, warn) = parse_blend(get("CAMERA_BOX_PUBLISH_30P_BLEND").as_deref());
        if let Some(w) = warn {
            tracing::warn!("#792 {}", w);
        }
        let (adaptive, warn) = parse_mode(get("CAMERA_BOX_PUBLISH_30P_MODE").as_deref());
        if let Some(w) = warn {
            tracing::warn!("#792 {}", w);
        }
        Config {
            enabled,
            blend,
            adaptive,
        }
    }

    /// Read the real process env. Thin delegate — all logic lives in [`Self::from_env_lookup`].
    pub fn from_process_env() -> Self {
        Self::from_env_lookup(|k| std::env::var(k).ok())
    }
}

/// Derive the secondary stream's NDI name from the primary's. NDI displays sources as
/// `MACHINE (stream_name)`, and the fleet's primary stream name is "usb" (→ "CAM1 (usb)"),
/// so the secondary is named "30p" (→ "CAM1 (30p)"). A non-default primary name keeps its
/// identity with a " 30p" suffix so two instances can never collide.
pub fn derive_30p_name(primary: &str) -> String {
    if primary == "usb" {
        "30p".to_string()
    } else {
        format!("{primary} 30p")
    }
}

/// Blend weight as Q8 fixed point (0..=128): `round(blend * 256)` clamped so 0.5 → 128.
pub fn blend_weight_q8(blend: f32) -> u16 {
    let b = blend.clamp(0.0, BLEND_MAX);
    ((b * 256.0).round() as u16).min(128)
}

/// Weighted temporal blend of two same-layout byte frames, in place into `first`:
/// `first[i] = (first[i]*(256-wb) + second[i]*wb + 128) >> 8`.
///
/// `wb_q8 == 128` is the EXACT rounding average `(a+b+1)>>1` (the 50/50 default);
/// `wb_q8 == 0` is a no-op (pure decimation keeps `first`). Byte-wise blending is exact
/// for interleaved 8-bit formats (YUYV/UYVY): both frames share the identical layout, so
/// every byte position holds the same component. Trailing bytes beyond the shorter slice
/// are left untouched (defensive; same-dims frames are always equal length).
pub fn blend_into(first: &mut [u8], second: &[u8], wb_q8: u16) {
    if wb_q8 == 0 {
        return;
    }
    let wb = u32::from(wb_q8.min(128));
    let wa = 256 - wb;
    for (a, &b) in first.iter_mut().zip(second.iter()) {
        *a = ((u32::from(*a) * wa + u32::from(b) * wb + 128) >> 8) as u8;
    }
}

/// Byte offset of the FIRST luma sample within a 4-byte macropixel group for the
/// byte-blendable interleaved formats: YUYV = `Y0 U Y1 V` → 0, UYVY = `U Y0 V Y1` → 1.
/// `None` = format is not blendable at the byte level.
pub fn luma_offset_for(fourcc: &str) -> Option<usize> {
    match fourcc {
        "YUYV" => Some(0),
        "UYVY" => Some(1),
        _ => None,
    }
}

/// Motion-adaptive weight ramp: luma difference `d` at/below `t_lo` → full `wmax_q8`
/// (static content), at/above `t_hi` → 0 (moving edge), linear in between.
pub fn adaptive_weight_q8(d: u8, wmax_q8: u16, t_lo: u8, t_hi: u8) -> u16 {
    if d <= t_lo {
        return wmax_q8;
    }
    if d >= t_hi || t_hi <= t_lo {
        return 0;
    }
    let span = u32::from(t_hi - t_lo);
    let above = u32::from(d - t_lo);
    (u32::from(wmax_q8) * (span - above) / span) as u16
}

/// MOTION-ADAPTIVE temporal blend, in place into `first`. Per 4-byte macropixel
/// (2 pixels sharing chroma): the group's motion measure is the LARGER of its two luma
/// differences; the blend weight ramps from `wmax_q8` (static → full average, temporal
/// noise reduction) to 0 (moving edge → keep the first frame sharp, NO double-image
/// ghost). Chroma follows its group's luma decision so no colour fringing can appear on
/// moving edges. A trailing partial group (never occurs on real even-width frames) is
/// left as the first frame.
pub fn blend_adaptive_into(
    first: &mut [u8],
    second: &[u8],
    luma_offset: usize,
    wmax_q8: u16,
    t_lo: u8,
    t_hi: u8,
) {
    if wmax_q8 == 0 {
        return;
    }
    debug_assert!(luma_offset < 2);
    let n = first.len().min(second.len()) / 4 * 4;
    for (ga, gb) in first[..n]
        .chunks_exact_mut(4)
        .zip(second[..n].chunks_exact(4))
    {
        let d0 = ga[luma_offset].abs_diff(gb[luma_offset]);
        let d1 = ga[luma_offset + 2].abs_diff(gb[luma_offset + 2]);
        let wb = u32::from(adaptive_weight_q8(d0.max(d1), wmax_q8, t_lo, t_hi));
        if wb == 0 {
            continue;
        }
        let wa = 256 - wb;
        for (a, &b) in ga.iter_mut().zip(gb.iter()) {
            *a = ((u32::from(*a) * wa + u32::from(b) * wb + 128) >> 8) as u8;
        }
    }
}

/// One teed frame crossing to the worker.
pub struct Frame {
    pub buf: Vec<u8>,
    /// Monotonic tee sequence (increments per EMITTED 60p frame, including teed frames
    /// later dropped on a full channel — gaps are how the pairer detects drops).
    pub seq: u64,
    /// The 60p frame's genlock emit timecode (100ns) — a pair's output is stamped with
    /// its FIRST frame's timecode so 30p ticks stay wall-clock-boundary aligned.
    pub timecode_100ns: i64,
    pub info: FrameInfo,
}

/// The pairing decision for one arriving frame. `Solo` frames pass through UNBLENDED —
/// never blend non-adjacent frames (that ghosts worse than decimation).
#[derive(Debug, PartialEq)]
pub enum PairAction<T> {
    /// First (even) member held; wait for its odd partner.
    Hold,
    /// A complete adjacent (2k, 2k+1) pair — blend `second` into `first`, emit `first`.
    Blend { first: T, second: T },
    /// A pair member whose partner was dropped — emit as-is.
    Solo(T),
    /// Two distinct pair slots each lost a member (both partners dropped in between) —
    /// emit both as-is, `earlier` first.
    SoloBoth { earlier: T, later: T },
}

/// Parity-anchored pair assembler: even seq starts a pair slot (2k, 2k+1); the matching
/// odd completes it. Any gap degrades the affected slot(s) to unblended passthrough.
#[derive(Default)]
pub struct Pairer<T> {
    pending: Option<(u64, T)>,
}

impl<T> Pairer<T> {
    pub fn new() -> Self {
        Pairer { pending: None }
    }

    pub fn push(&mut self, seq: u64, item: T) -> PairAction<T> {
        if seq.is_multiple_of(2) {
            // Even: starts a new pair slot. A still-pending even means its odd was dropped.
            match self.pending.take() {
                Some((_, stale)) => {
                    self.pending = Some((seq, item));
                    PairAction::Solo(stale)
                }
                None => {
                    self.pending = Some((seq, item));
                    PairAction::Hold
                }
            }
        } else {
            match self.pending.take() {
                // The adjacent partner: a complete pair.
                Some((pseq, first)) if pseq + 1 == seq => PairAction::Blend {
                    first,
                    second: item,
                },
                // A pending even from an OLDER slot (its odd dropped) AND this odd's own
                // even dropped: two broken slots, both pass through.
                Some((_, stale)) => PairAction::SoloBoth {
                    earlier: stale,
                    later: item,
                },
                // This odd's even was dropped.
                None => PairAction::Solo(item),
            }
        }
    }
}

/// The capture-thread side of the tee. One bounded memcpy + one `try_send` per emitted
/// frame; NEVER blocks (full channel = counted drop, buffer kept as the next spare).
pub struct Tee {
    tx: SyncSender<Frame>,
    pool_rx: Receiver<Vec<u8>>,
    spare: Option<Vec<u8>>,
    seq: u64,
    drops: Arc<AtomicU64>,
}

impl Tee {
    pub fn new(tx: SyncSender<Frame>, pool_rx: Receiver<Vec<u8>>, drops: Arc<AtomicU64>) -> Self {
        Tee {
            tx,
            pool_rx,
            spare: None,
            seq: 0,
            drops,
        }
    }

    pub fn drop_count(&self) -> u64 {
        self.drops.load(Ordering::Relaxed)
    }

    /// Copy the emitted frame into a recycled buffer and hand it to the worker.
    /// Bounded and non-blocking by construction.
    pub fn tee(&mut self, data: &[u8], info: FrameInfo, timecode_100ns: i64) {
        let mut buf = self
            .spare
            .take()
            .or_else(|| self.pool_rx.try_recv().ok())
            .unwrap_or_default();
        buf.clear();
        buf.extend_from_slice(data);
        let frame = Frame {
            buf,
            seq: self.seq,
            timecode_100ns,
            info,
        };
        self.seq += 1;
        match self.tx.try_send(frame) {
            Ok(()) => {}
            // Full channel: the 30p worker is behind — drop THIS frame (the pairer sees
            // the seq gap and degrades that slot to passthrough). Keep the buffer as the
            // next spare so a sustained-drop phase still allocates nothing.
            Err(TrySendError::Full(f)) | Err(TrySendError::Disconnected(f)) => {
                self.spare = Some(f.buf);
                self.drops.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Worker loop, generic over the send so it is unit-testable without an NDI runtime.
/// Consumes teed frames, pairs them, blends complete pairs, emits at half rate, and
/// recycles buffers back through `pool_tx`.
pub fn run_worker_loop(
    rx: Receiver<Frame>,
    pool_tx: SyncSender<Vec<u8>>,
    wb_q8: u16,
    adaptive: bool,
    drops: Arc<AtomicU64>,
    mut send: impl FnMut(&Frame),
) {
    let mut pairer: Pairer<Frame> = Pairer::new();
    let mut outputs: u64 = 0;
    let mut blended: u64 = 0;
    let mut solo: u64 = 0;
    let mut warned_unblendable = false;
    let recycle = |pool_tx: &SyncSender<Vec<u8>>, buf: Vec<u8>| {
        // Full pool = buffer is simply freed; the tee falls back to one fresh alloc.
        let _ = pool_tx.try_send(buf);
    };
    while let Ok(frame) = rx.recv() {
        match pairer.push(frame.seq, frame) {
            PairAction::Hold => {}
            PairAction::Blend { mut first, second } => {
                let fourcc = first.info.fourcc.str().unwrap_or("");
                if let Some(luma_offset) = luma_offset_for(fourcc) {
                    if adaptive {
                        blend_adaptive_into(
                            &mut first.buf,
                            &second.buf,
                            luma_offset,
                            wb_q8,
                            ADAPT_LUMA_T_LO,
                            ADAPT_LUMA_T_HI,
                        );
                    } else {
                        blend_into(&mut first.buf, &second.buf, wb_q8);
                    }
                    blended += 1;
                } else {
                    // Non-interleaved-8bit format (MJPG etc): byte blending is invalid —
                    // degrade to decimation (emit the pair's first frame untouched).
                    if !warned_unblendable {
                        warned_unblendable = true;
                        tracing::warn!(
                            "#792 publish-30p: fourcc {} is not byte-blendable — emitting decimated (first-of-pair) frames",
                            if fourcc.is_empty() { "?" } else { fourcc }
                        );
                    }
                    solo += 1;
                }
                send(&first);
                outputs += 1;
                recycle(&pool_tx, first.buf);
                recycle(&pool_tx, second.buf);
            }
            PairAction::Solo(f) => {
                solo += 1;
                send(&f);
                outputs += 1;
                recycle(&pool_tx, f.buf);
            }
            PairAction::SoloBoth { earlier, later } => {
                solo += 2;
                send(&earlier);
                send(&later);
                outputs += 2;
                recycle(&pool_tx, earlier.buf);
                recycle(&pool_tx, later.buf);
            }
        }
        if outputs > 0 && outputs.is_multiple_of(STATS_LOG_EVERY_OUTPUTS) {
            tracing::info!(
                "#792 publish-30p: {} outputs ({} blended, {} solo), {} tee drops",
                outputs,
                blended,
                solo,
                drops.load(Ordering::Relaxed)
            );
        }
    }
    tracing::info!(
        "#792 publish-30p worker exiting (tee closed): {} outputs ({} blended, {} solo), {} tee drops",
        outputs,
        blended,
        solo,
        drops.load(Ordering::Relaxed)
    );
}

/// Spawn the "publish-30p" worker owning `sender` (a second, 30fps NDI sender with
/// external pacing already enabled) and return the capture-thread [`Tee`].
pub fn spawn(mut sender: crate::ndi::NdiSender, config: Config) -> std::io::Result<Tee> {
    let (tx, rx) = sync_channel::<Frame>(CHANNEL_DEPTH);
    let (pool_tx, pool_rx) = sync_channel::<Vec<u8>>(POOL_CAP);
    let drops = Arc::new(AtomicU64::new(0));
    let drops_worker = Arc::clone(&drops);
    let wb_q8 = blend_weight_q8(config.blend);
    let adaptive = config.adaptive;
    std::thread::Builder::new()
        .name("publish-30p".into())
        .spawn(move || {
            // OFF the isolated capture core, onto the general cores (painter/intercom side).
            crate::affinity::pin_off_capture_core("publish-30p");
            run_worker_loop(rx, pool_tx, wb_q8, adaptive, drops_worker, |f: &Frame| {
                if let Err(e) = sender.send_frame_data_with_timecode(
                    &f.buf,
                    f.info.width,
                    f.info.height,
                    f.info.fourcc,
                    f.info.stride,
                    f.timecode_100ns,
                ) {
                    tracing::error!("#792 publish-30p send failed: {}", e);
                }
                // #297 parity for the SECOND sender (review finding): the primary re-registers on
                // a network change via the capture loop's maybe_reannounce() poll — without the
                // same poll here, a boot race / link flap would leave "CAMn (30p)" undiscoverable
                // while "CAMn (usb)" recovers. Internally throttled to REANNOUNCE_POLL_INTERVAL,
                // so a per-output call (~30/s) is ~free; frames keep flowing from local capture
                // even while the network is down, so the poll fires exactly when it matters.
                if let Err(e) = sender.maybe_reannounce() {
                    tracing::warn!("#792 publish-30p re-announce check failed: {}", e);
                }
            });
        })?;
    Ok(Tee::new(tx, pool_rx, drops))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(fourcc: &[u8; 4]) -> FrameInfo {
        FrameInfo {
            width: 4,
            height: 1,
            fourcc: v4l::FourCC::new(fourcc),
            stride: 8,
            capture_monotonic_100ns: 0,
            dequeue_duration_ms: 0.0,
        }
    }

    fn frame(seq: u64, byte: u8) -> Frame {
        Frame {
            buf: vec![byte; 8],
            seq,
            timecode_100ns: 1000 + seq as i64,
            info: info(b"YUYV"),
        }
    }

    // -- config parsing --------------------------------------------------------------

    #[test]
    fn enabled_parses_truthy_values_only() {
        assert!(parse_enabled(Some("1")));
        assert!(parse_enabled(Some("true")));
        assert!(parse_enabled(Some(" YES ")));
        assert!(!parse_enabled(Some("0")));
        assert!(!parse_enabled(Some("false")));
        assert!(!parse_enabled(Some("")));
        assert!(!parse_enabled(None));
    }

    #[test]
    fn blend_defaults_clamps_and_warns() {
        assert_eq!(parse_blend(None), (BLEND_DEFAULT, None));
        assert_eq!(parse_blend(Some("0.5")), (0.5, None));
        assert_eq!(parse_blend(Some("0.0")), (0.0, None));
        assert_eq!(parse_blend(Some("0.25")), (0.25, None));
        let (b, warn) = parse_blend(Some("0.9"));
        assert_eq!(b, BLEND_MAX, "above the ceiling clamps to BLEND_MAX");
        assert!(warn.is_some());
        let (b, warn) = parse_blend(Some("-1"));
        assert_eq!(b, 0.0, "below zero clamps to 0.0 (pure decimation)");
        assert!(warn.is_some());
        let (b, warn) = parse_blend(Some("abc"));
        assert_eq!(b, BLEND_DEFAULT, "garbage falls back to the default");
        assert!(warn.is_some());
        let (b, warn) = parse_blend(Some("NaN"));
        assert_eq!(b, BLEND_DEFAULT, "NaN is rejected, never clamped");
        assert!(warn.is_some());
    }

    #[test]
    fn secondary_name_derivation() {
        assert_eq!(derive_30p_name("usb"), "30p", "fleet default → CAMn (30p)");
        assert_eq!(derive_30p_name("custom"), "custom 30p");
    }

    #[test]
    fn env_lookup_maps_each_var_name_to_its_field() {
        let lookup = |vars: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                vars.iter()
                    .find(|(name, _)| *name == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        // All unset ⇒ disabled, default blend, adaptive.
        let c = Config::from_env_lookup(lookup(&[]));
        assert_eq!(
            c,
            Config {
                enabled: false,
                blend: BLEND_DEFAULT,
                adaptive: true
            }
        );
        // Each real env NAME lands in its field (pins the glue, not just the parsers).
        let c = Config::from_env_lookup(lookup(&[("CAMERA_BOX_PUBLISH_30P", "1")]));
        assert!(c.enabled && c.adaptive && c.blend == BLEND_DEFAULT);
        let c = Config::from_env_lookup(lookup(&[("CAMERA_BOX_PUBLISH_30P_BLEND", "0.25")]));
        assert_eq!(c.blend, 0.25);
        assert!(!c.enabled);
        let c = Config::from_env_lookup(lookup(&[("CAMERA_BOX_PUBLISH_30P_MODE", "fixed")]));
        assert!(!c.adaptive);
        assert!(!c.enabled);
    }

    #[test]
    fn mode_defaults_to_adaptive() {
        assert_eq!(parse_mode(None), (true, None));
        assert_eq!(parse_mode(Some("adaptive")), (true, None));
        assert_eq!(parse_mode(Some(" FIXED ")), (false, None));
        let (adaptive, warn) = parse_mode(Some("garbage"));
        assert!(adaptive, "unknown mode falls back to adaptive");
        assert!(warn.is_some());
    }

    // -- motion-adaptive blend ---------------------------------------------------------

    #[test]
    fn adaptive_weight_ramp() {
        // Static (d <= t_lo): full weight. Moving edge (d >= t_hi): zero. Linear between.
        assert_eq!(adaptive_weight_q8(0, 128, 8, 32), 128);
        assert_eq!(adaptive_weight_q8(8, 128, 8, 32), 128);
        assert_eq!(adaptive_weight_q8(32, 128, 8, 32), 0);
        assert_eq!(adaptive_weight_q8(255, 128, 8, 32), 0);
        assert_eq!(
            adaptive_weight_q8(20, 128, 8, 32),
            64,
            "midpoint = half weight"
        );
        let w14 = adaptive_weight_q8(14, 128, 8, 32);
        let w26 = adaptive_weight_q8(26, 128, 8, 32);
        assert!(w14 > 64 && w26 < 64, "ramp is monotonic");
        assert_eq!(
            adaptive_weight_q8(9, 128, 8, 8),
            0,
            "degenerate ramp never divides by zero"
        );
    }

    #[test]
    fn adaptive_blend_full_averages_static_and_keeps_moving_edges_sharp() {
        // YUYV groups: [Y0 U Y1 V]. Group 1 static (luma diff 0, chroma noise only) →
        // full average incl chroma (temporal NR). Group 2 moving edge (luma diff 100)
        // → ALL 4 bytes keep the FIRST frame (no ghost, no colour fringe).
        let mut a = vec![100, 120, 100, 130, 50, 90, 50, 200];
        let b = vec![100, 124, 100, 134, 150, 10, 150, 100];
        blend_adaptive_into(&mut a, &b, 0, 128, ADAPT_LUMA_T_LO, ADAPT_LUMA_T_HI);
        assert_eq!(
            a,
            vec![100, 122, 100, 132, 50, 90, 50, 200],
            "static group averaged, moving group untouched"
        );
    }

    #[test]
    fn adaptive_blend_honours_uyvy_luma_offset() {
        // UYVY groups: [U Y0 V Y1] — luma at offsets 1 and 3. Moving lumas must shield
        // the group even when chroma bytes happen to match.
        let mut a = vec![120, 50, 130, 50];
        let b = vec![120, 150, 130, 150];
        blend_adaptive_into(&mut a, &b, 1, 128, ADAPT_LUMA_T_LO, ADAPT_LUMA_T_HI);
        assert_eq!(
            a,
            vec![120, 50, 130, 50],
            "moving UYVY group keeps the first frame"
        );
        let mut a = vec![120, 50, 130, 50];
        let b = vec![124, 52, 134, 52];
        blend_adaptive_into(&mut a, &b, 1, 128, ADAPT_LUMA_T_LO, ADAPT_LUMA_T_HI);
        assert_eq!(
            a,
            vec![122, 51, 132, 51],
            "static UYVY group fully averaged"
        );
    }

    #[test]
    fn adaptive_blend_zero_weight_is_noop_and_partial_tail_is_left() {
        let mut a = vec![10u8, 10, 10, 10, 99, 99];
        let b = vec![12u8, 12, 12, 12, 1, 1];
        blend_adaptive_into(&mut a, &b, 0, 0, ADAPT_LUMA_T_LO, ADAPT_LUMA_T_HI);
        assert_eq!(
            a,
            vec![10, 10, 10, 10, 99, 99],
            "wmax 0 = pure decimation no-op"
        );
        blend_adaptive_into(&mut a, &b, 0, 128, ADAPT_LUMA_T_LO, ADAPT_LUMA_T_HI);
        assert_eq!(
            a,
            vec![11, 11, 11, 11, 99, 99],
            "complete group blended; trailing partial group left as the first frame"
        );
    }

    // -- blend math ------------------------------------------------------------------

    #[test]
    fn blend_half_is_exact_rounding_average() {
        assert_eq!(blend_weight_q8(0.5), 128);
        let mut a = vec![0u8, 255, 10, 11, 100];
        let b = vec![0u8, 255, 11, 10, 101];
        blend_into(&mut a, &b, 128);
        // (a+b+1)>>1 per byte:
        assert_eq!(a, vec![0, 255, 11, 11, 101]);
    }

    #[test]
    fn blend_zero_is_pure_decimation_noop() {
        assert_eq!(blend_weight_q8(0.0), 0);
        let mut a = vec![1u8, 2, 3];
        blend_into(&mut a, &[200, 200, 200], 0);
        assert_eq!(a, vec![1, 2, 3]);
    }

    #[test]
    fn blend_weighted_biases_toward_first_frame() {
        // wb=64 (blend 0.25): out = (a*192 + b*64 + 128) >> 8
        assert_eq!(blend_weight_q8(0.25), 64);
        let mut a = vec![0u8];
        blend_into(&mut a, &[255], 64);
        assert_eq!(a, vec![64], "quarter-weight of the second frame");
        let mut a = vec![100u8];
        blend_into(&mut a, &[100], 64);
        assert_eq!(
            a,
            vec![100],
            "identical inputs are a fixed point at any weight"
        );
    }

    #[test]
    fn blend_tolerates_length_mismatch() {
        let mut a = vec![10u8, 10, 10];
        blend_into(&mut a, &[20u8], 128);
        assert_eq!(
            a,
            vec![15, 10, 10],
            "only the overlapping prefix is blended"
        );
    }

    // -- pairing ---------------------------------------------------------------------

    #[test]
    fn pairer_blends_adjacent_pairs() {
        let mut p: Pairer<u64> = Pairer::new();
        assert_eq!(p.push(0, 100), PairAction::Hold);
        assert_eq!(
            p.push(1, 101),
            PairAction::Blend {
                first: 100,
                second: 101
            }
        );
        assert_eq!(p.push(2, 102), PairAction::Hold);
        assert_eq!(
            p.push(3, 103),
            PairAction::Blend {
                first: 102,
                second: 103
            }
        );
    }

    #[test]
    fn pairer_degrades_dropped_odd_to_solo_passthrough() {
        let mut p: Pairer<u64> = Pairer::new();
        assert_eq!(p.push(0, 100), PairAction::Hold);
        // seq 1 dropped at the tee; the next even flushes the stale first frame solo.
        assert_eq!(p.push(2, 102), PairAction::Solo(100));
        assert_eq!(
            p.push(3, 103),
            PairAction::Blend {
                first: 102,
                second: 103
            }
        );
    }

    #[test]
    fn pairer_degrades_dropped_even_to_solo_passthrough() {
        let mut p: Pairer<u64> = Pairer::new();
        // seq 0 dropped; the orphan odd passes through solo.
        assert_eq!(p.push(1, 101), PairAction::Solo(101));
        assert_eq!(p.push(2, 102), PairAction::Hold);
    }

    #[test]
    fn pairer_flushes_two_broken_slots_as_solo_both() {
        let mut p: Pairer<u64> = Pairer::new();
        assert_eq!(p.push(0, 100), PairAction::Hold);
        // seq 1 and 2 both dropped: slot (0,1) lost its odd, slot (2,3) lost its even.
        assert_eq!(
            p.push(3, 103),
            PairAction::SoloBoth {
                earlier: 100,
                later: 103
            }
        );
    }

    // -- tee drop policy (the zero-impact contract) ------------------------------------

    #[test]
    fn tee_never_blocks_on_a_full_channel_and_counts_drops() {
        let (tx, rx) = sync_channel::<Frame>(1);
        let (_pool_tx, pool_rx) = sync_channel::<Vec<u8>>(POOL_CAP);
        let drops = Arc::new(AtomicU64::new(0));
        let mut tee = Tee::new(tx, pool_rx, Arc::clone(&drops));
        let data = [7u8; 8];
        tee.tee(&data, info(b"YUYV"), 111); // fills the depth-1 channel
        tee.tee(&data, info(b"YUYV"), 222); // full → must return immediately, counted
        tee.tee(&data, info(b"YUYV"), 333); // still full → counted
        assert_eq!(tee.drop_count(), 2);
        let f0 = rx.recv().expect("first frame delivered");
        assert_eq!(f0.seq, 0);
        assert_eq!(f0.timecode_100ns, 111);
        assert_eq!(f0.buf, vec![7u8; 8]);
        // seq advanced across the drops (the pairer relies on the gap).
        tee.tee(&data, info(b"YUYV"), 444);
        let f3 = rx.recv().expect("post-drop frame delivered");
        assert_eq!(f3.seq, 3, "dropped frames still consume sequence numbers");
    }

    #[test]
    fn tee_survives_a_disconnected_worker() {
        let (tx, rx) = sync_channel::<Frame>(1);
        let (_pool_tx, pool_rx) = sync_channel::<Vec<u8>>(POOL_CAP);
        let drops = Arc::new(AtomicU64::new(0));
        let mut tee = Tee::new(tx, pool_rx, Arc::clone(&drops));
        drop(rx); // worker died
        let data = [1u8; 8];
        tee.tee(&data, info(b"YUYV"), 111);
        tee.tee(&data, info(b"YUYV"), 222);
        assert_eq!(
            tee.drop_count(),
            2,
            "sends to a dead worker count as drops, never panic"
        );
    }

    #[test]
    fn tee_recycles_pool_buffers() {
        let (tx, rx) = sync_channel::<Frame>(CHANNEL_DEPTH);
        let (pool_tx, pool_rx) = sync_channel::<Vec<u8>>(POOL_CAP);
        let drops = Arc::new(AtomicU64::new(0));
        let mut tee = Tee::new(tx, pool_rx, drops);
        let mut recycled = Vec::with_capacity(64);
        recycled.push(9u8); // distinguishable capacity/content is cleared on reuse
        pool_tx.send(recycled).unwrap();
        tee.tee(&[5u8; 8], info(b"YUYV"), 1);
        let f = rx.recv().unwrap();
        assert_eq!(
            f.buf,
            vec![5u8; 8],
            "recycled buffer was cleared and refilled"
        );
        assert!(f.buf.capacity() >= 64, "the pooled allocation was reused");
    }

    // -- worker loop (fake send — no NDI runtime) --------------------------------------

    #[test]
    fn worker_blends_pairs_and_passes_gaps_through_solo() {
        let (tx, rx) = sync_channel::<Frame>(16);
        let (pool_tx, pool_rx) = sync_channel::<Vec<u8>>(POOL_CAP);
        let drops = Arc::new(AtomicU64::new(0));
        // Pair (0,1): 10 & 20 → avg 15. Gap at seq 3 (dropped): seq 2 flushes solo when
        // seq 4 arrives; pair (4,5) completes normally.
        tx.send(frame(0, 10)).unwrap();
        tx.send(frame(1, 20)).unwrap();
        tx.send(frame(2, 30)).unwrap();
        tx.send(frame(4, 40)).unwrap();
        tx.send(frame(5, 60)).unwrap();
        drop(tx);
        let mut sent: Vec<(u64, i64, Vec<u8>)> = Vec::new();
        run_worker_loop(rx, pool_tx, 128, false, drops, |f| {
            sent.push((f.seq, f.timecode_100ns, f.buf.clone()));
        });
        assert_eq!(sent.len(), 3, "5 inputs with one gap → 3 outputs");
        // Blended pair (0,1), stamped with the FIRST frame's timecode:
        assert_eq!(sent[0].0, 0);
        assert_eq!(sent[0].1, 1000);
        assert_eq!(sent[0].2, vec![15u8; 8]);
        // Solo passthrough of seq 2 — UNBLENDED:
        assert_eq!(sent[1].0, 2);
        assert_eq!(sent[1].2, vec![30u8; 8]);
        // Blended pair (4,5): avg(40,60)=50, first frame's timecode:
        assert_eq!(sent[2].0, 4);
        assert_eq!(sent[2].1, 1004);
        assert_eq!(sent[2].2, vec![50u8; 8]);
        // Buffers recycled back to the pool:
        assert!(
            pool_rx.try_recv().is_ok(),
            "worker returns buffers to the pool"
        );
    }

    #[test]
    fn worker_degrades_unblendable_fourcc_to_decimation() {
        let (tx, rx) = sync_channel::<Frame>(16);
        let (pool_tx, pool_rx) = sync_channel::<Vec<u8>>(POOL_CAP);
        let _ = &pool_rx;
        let drops = Arc::new(AtomicU64::new(0));
        let mut f0 = frame(0, 10);
        f0.info = info(b"MJPG");
        let mut f1 = frame(1, 20);
        f1.info = info(b"MJPG");
        tx.send(f0).unwrap();
        tx.send(f1).unwrap();
        drop(tx);
        let mut sent: Vec<Vec<u8>> = Vec::new();
        run_worker_loop(rx, pool_tx, 128, false, drops, |f| sent.push(f.buf.clone()));
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0],
            vec![10u8; 8],
            "MJPG pair emits the FIRST frame untouched"
        );
    }

    #[test]
    fn worker_adaptive_mode_dispatches_the_motion_adaptive_blend() {
        // Same data as the adaptive_blend unit test: group 1 static (chroma noise only),
        // group 2 a moving edge. adaptive=true must keep the moving group's FIRST-frame
        // bytes; a regression flipping the dispatch to the fixed blend would average them.
        let (tx, rx) = sync_channel::<Frame>(16);
        let (pool_tx, _pool_rx) = sync_channel::<Vec<u8>>(POOL_CAP);
        let drops = Arc::new(AtomicU64::new(0));
        let mut f0 = frame(0, 0);
        f0.buf = vec![100, 120, 100, 130, 50, 90, 50, 200];
        let mut f1 = frame(1, 0);
        f1.buf = vec![100, 124, 100, 134, 150, 10, 150, 100];
        tx.send(f0).unwrap();
        tx.send(f1).unwrap();
        drop(tx);
        let mut sent: Vec<Vec<u8>> = Vec::new();
        run_worker_loop(rx, pool_tx, 128, true, drops, |f| sent.push(f.buf.clone()));
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0],
            vec![100, 122, 100, 132, 50, 90, 50, 200],
            "adaptive dispatch: static group averaged, moving group keeps the first frame \
             (the fixed blend would have averaged both groups)"
        );
    }

    #[test]
    fn worker_flushes_double_drop_as_two_solo_outputs_in_order() {
        // seqs 1 and 2 dropped at the tee: slot (0,1) lost its odd, slot (2,3) lost its
        // even — SoloBoth must emit BOTH survivors, earlier first, unblended, each with
        // its own timecode.
        let (tx, rx) = sync_channel::<Frame>(16);
        let (pool_tx, pool_rx) = sync_channel::<Vec<u8>>(POOL_CAP);
        let drops = Arc::new(AtomicU64::new(0));
        tx.send(frame(0, 10)).unwrap();
        tx.send(frame(3, 40)).unwrap();
        drop(tx);
        let mut sent: Vec<(u64, i64, Vec<u8>)> = Vec::new();
        run_worker_loop(rx, pool_tx, 128, false, drops, |f| {
            sent.push((f.seq, f.timecode_100ns, f.buf.clone()));
        });
        assert_eq!(sent.len(), 2, "both broken slots' survivors are emitted");
        assert_eq!(
            sent[0],
            (0, 1000, vec![10u8; 8]),
            "earlier survivor first, unblended, its own timecode"
        );
        assert_eq!(
            sent[1],
            (3, 1003, vec![40u8; 8]),
            "later survivor second, unblended, its own timecode"
        );
        // Both buffers recycled back to the pool.
        assert!(pool_rx.try_recv().is_ok() && pool_rx.try_recv().is_ok());
    }

    #[test]
    fn worker_decimates_when_blend_weight_is_zero() {
        let (tx, rx) = sync_channel::<Frame>(16);
        let (pool_tx, _pool_rx) = sync_channel::<Vec<u8>>(POOL_CAP);
        let drops = Arc::new(AtomicU64::new(0));
        tx.send(frame(0, 10)).unwrap();
        tx.send(frame(1, 20)).unwrap();
        drop(tx);
        let mut sent: Vec<Vec<u8>> = Vec::new();
        run_worker_loop(rx, pool_tx, 0, false, drops, |f| sent.push(f.buf.clone()));
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0],
            vec![10u8; 8],
            "blend 0.0 emits the pair's first frame verbatim"
        );
    }
}
