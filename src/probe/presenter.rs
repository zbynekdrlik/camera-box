//! Presenter abstraction — the painter writes QR frames through a `Presenter`,
//! which is either the DRM/KMS page-flip path ([`crate::probe::kms::KmsPresenter`],
//! tear-free + vblank-locked 1:1, #79) or the fbdev fallback
//! ([`crate::probe::fb::VsyncFb`], single-buffer vsync-gated write, #68).
//!
//! Selecting the presenter at runtime keeps the harness working everywhere it
//! did before: KMS needs DRM master (which detaches the live console) and a
//! `/dev/dri/card*` it can drive; where that is unavailable the painter falls
//! back to fbdev with no behaviour change.

use crate::probe::fb::VsyncFb;
use anyhow::Result;

// #464: `PresenterKind` (+ its `parse`) and the pure Auto-fallback decision
// `resolve_presenter_kind` now live at the crate root (`crate::presenter_kind`) — a Tier-0
// module with no probe deps, so `scripts/rig-mode.sh`'s presenter-aware liveness gate has one
// documented, unit-tested answer to "will this run ever touch /dev/fb0?" — re-exported here so
// every existing `crate::probe::presenter::PresenterKind` reference keeps compiling unchanged.
pub use crate::presenter_kind::{resolve_presenter_kind, PresenterKind, ResolvedPresenter};

/// A frame presenter: writes a full BGRA frame to the HDMI output, tear-free.
///
/// Implementors:
/// - [`crate::probe::kms::KmsPresenter`] — DRM page-flip, blocks on the vblank
///   flip-complete event (so [`paces_on_present`](Presenter::paces_on_present)
///   is `true`: the painter must NOT sleep — pacing is the hardware vblank).
/// - [`VsyncFb`] — fbdev single-buffer vsync-gated write (the painter still
///   sleep-paces at `--paint-fps`, so `paces_on_present` is `false`).
pub trait Presenter: Send {
    /// (width, height) of the presented frame.
    fn dimensions(&self) -> (u32, u32);

    /// Present a full BGRA frame (`width*height*4` bytes), tear-free.
    fn present(&mut self, bgra: &[u8]) -> Result<()>;

    /// Whether [`present`](Presenter::present) blocks until the next vblank and
    /// thus paces the painter itself (one new id per vblank). `true` for the KMS
    /// page-flip presenter (1:1, 60 fps); `false` for fbdev, where the painter
    /// must sleep-pace at `--paint-fps`.
    fn paces_on_present(&self) -> bool;

    /// Whether this presenter delivers the tear-free, 1:1, phase-locked output
    /// (#79): the KMS page-flip presenter running at exactly the capture refresh
    /// (60.000 Hz). `false` for fbdev (the sub-capture vsync-gated path, which
    /// still has the ~2.2% torn-QR blind spot) AND for a KMS run that had to fall
    /// back to a non-60 Hz mode — so a run is only ever reported as 1:1 when the
    /// hardware lock genuinely holds.
    fn phase_locked(&self) -> bool {
        false
    }
}

impl Presenter for VsyncFb {
    fn dimensions(&self) -> (u32, u32) {
        VsyncFb::dimensions(self)
    }
    fn present(&mut self, bgra: &[u8]) -> Result<()> {
        VsyncFb::present(self, bgra)
    }
    fn paces_on_present(&self) -> bool {
        // fbdev does NOT pace the painter — the painter sleep-paces at --paint-fps.
        false
    }
    // phase_locked defaults to false: the fbdev path is the sub-capture,
    // tear-prone fallback, never the 1:1 lock.
}

/// Open a presenter per `kind` for the given `fb_device` (fbdev path) and
/// `drm_device` (DRM card path). The KMS path drives the HDMI mode matching the
/// painter's `canvas_w`×`canvas_h` (it cannot scan out a different size than the
/// painter renders — driving a larger mode the painter can't fill is the live
/// cam2 bug this guards) AND running at `mode_refresh_mhz` (default 60_000 = the
/// capture rate; #1179 overrides it, e.g. 100_000 for the 2560×1080@100
/// experiment — the mode-SELECTION target only, never the phase-lock reference).
/// `Auto` tries KMS then falls back to fbdev, logging which path it landed on.
#[cfg(target_os = "linux")]
pub fn open_presenter(
    kind: PresenterKind,
    fb_device: &str,
    drm_device: &str,
    canvas_w: u32,
    canvas_h: u32,
    mode_refresh_mhz: u32,
) -> Result<Box<dyn Presenter>> {
    use crate::probe::kms::KmsPresenter;
    match kind {
        PresenterKind::Fbdev => Ok(Box::new(VsyncFb::open(fb_device)?)),
        PresenterKind::Kms => Ok(Box::new(KmsPresenter::open(
            drm_device,
            fb_device,
            canvas_w,
            canvas_h,
            mode_refresh_mhz,
        )?)),
        PresenterKind::Auto => {
            // #464: the actual KMS-open ATTEMPT stays here (it's the I/O); which presenter is
            // ACTUALLY in play — and whether it will ever touch /dev/fb0 — is decided by the
            // single pure `resolve_presenter_kind`, the same fn scripts/rig-mode.sh's
            // presenter-aware liveness gate documents its expectations against. Matched as a
            // (decision, result) pair rather than `.expect()`/`.expect_err()` so this compiles
            // without requiring `KmsPresenter`/its error to be `Debug`.
            //
            // #660: `fb_device` is passed through even on the KMS path so its `Drop` can blank
            // that device before releasing DRM master — see `crate::fb_blank`. KMS itself still
            // never reads/writes it during normal operation.
            let mut kms_result =
                KmsPresenter::open(drm_device, fb_device, canvas_w, canvas_h, mode_refresh_mhz);
            let mut opened_device = drm_device.to_string();
            // #854: `/dev/dri/cardN` numbering is NOT a stable ABI — a kernel/driver update or a
            // reboot can renumber the i915 KMS device on a single-GPU box with no other change at
            // all (confirmed live on cam2: card1 on 2026-07-04, card0 by 2026-07-28, no card1 node
            // at all). Before giving up on the tear-free double-buffered KMS path and quietly
            // falling back to the imperfect single-buffered fbdev presenter, try every OTHER
            // `/dev/dri/cardN` present — `order_drm_card_candidates` (Tier-0 tested) decides which
            // and in what order; only the directory read + the actual open attempts are I/O here.
            if kms_result.is_err() {
                if let Ok(read_dir) = std::fs::read_dir("/dev/dri") {
                    let entries: Vec<String> = read_dir
                        .filter_map(|e| e.ok())
                        .map(|e| e.path().to_string_lossy().into_owned())
                        .collect();
                    for candidate in crate::presenter_kind::order_drm_card_candidates(&entries) {
                        if candidate == opened_device {
                            continue; // already tried above
                        }
                        match KmsPresenter::open(
                            &candidate,
                            fb_device,
                            canvas_w,
                            canvas_h,
                            mode_refresh_mhz,
                        ) {
                            Ok(p) => {
                                tracing::warn!(
                                    "presenter: configured DRM device {} unavailable, found a \
                                     working KMS device at {} instead -- /dev/dri card \
                                     numbering has likely shifted (#854); update --drm-device \
                                     to stop relying on this fallback",
                                    opened_device,
                                    candidate
                                );
                                kms_result = Ok(p);
                                opened_device = candidate;
                                break;
                            }
                            Err(_) => continue,
                        }
                    }
                }
            }
            let resolved = resolve_presenter_kind(kind, kms_result.is_ok());
            match (resolved.kind, kms_result) {
                (PresenterKind::Kms, Ok(p)) => {
                    tracing::info!("presenter: using DRM/KMS page-flip ({})", opened_device);
                    Ok(Box::new(p))
                }
                (PresenterKind::Fbdev, Err(e)) => {
                    tracing::warn!(
                        "presenter: DRM/KMS unavailable ({:#}), falling back to fbdev ({})",
                        e,
                        fb_device
                    );
                    Ok(Box::new(VsyncFb::open(fb_device)?))
                }
                // resolve_presenter_kind(Auto, kms_open_ok) always mirrors kms_result.is_ok()
                // (locked by its own unit tests in src/presenter_kind.rs) — unreachable by
                // construction.
                _ => unreachable!(
                    "resolve_presenter_kind({:?}, kms_open_ok) desynced from the actual KMS \
                     open result",
                    kind
                ),
            }
        }
    }
}

/// Non-Linux builds have no DRM/KMS — only the fbdev presenter exists.
#[cfg(not(target_os = "linux"))]
pub fn open_presenter(
    kind: PresenterKind,
    fb_device: &str,
    _drm_device: &str,
    _canvas_w: u32,
    _canvas_h: u32,
    _mode_refresh_mhz: u32,
) -> Result<Box<dyn Presenter>> {
    match kind {
        PresenterKind::Kms => anyhow::bail!("DRM/KMS presenter is Linux-only"),
        _ => Ok(Box::new(VsyncFb::open(fb_device)?)),
    }
}

// #464: PresenterKind::parse + resolve_presenter_kind's unit tests moved to
// src/presenter_kind.rs (Tier-0, default features) — see that module's tests for
// parse_known_kinds / parse_rejects_unknown / the Auto-fallback resolution tests.
