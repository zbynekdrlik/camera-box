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

/// How the painter should obtain a presenter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenterKind {
    /// Try KMS first; fall back to fbdev if DRM master can't be taken (#79).
    Auto,
    /// Force the DRM/KMS page-flip presenter (error if it can't be opened).
    Kms,
    /// Force the fbdev `/dev/fb0` presenter (the #68 path).
    Fbdev,
}

impl PresenterKind {
    /// Parse the `--presenter` CLI value. Unknown values error so a typo does
    /// not silently fall back to a different presenter than the operator asked.
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "auto" => Ok(PresenterKind::Auto),
            "kms" => Ok(PresenterKind::Kms),
            "fbdev" => Ok(PresenterKind::Fbdev),
            other => anyhow::bail!("unknown --presenter '{}' (use auto|kms|fbdev)", other),
        }
    }
}

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
/// cam2 bug this guards). `Auto` tries KMS then falls back to fbdev, logging
/// which path it landed on.
#[cfg(target_os = "linux")]
pub fn open_presenter(
    kind: PresenterKind,
    fb_device: &str,
    drm_device: &str,
    canvas_w: u32,
    canvas_h: u32,
) -> Result<Box<dyn Presenter>> {
    use crate::probe::kms::KmsPresenter;
    match kind {
        PresenterKind::Fbdev => Ok(Box::new(VsyncFb::open(fb_device)?)),
        PresenterKind::Kms => Ok(Box::new(KmsPresenter::open(
            drm_device, canvas_w, canvas_h,
        )?)),
        PresenterKind::Auto => match KmsPresenter::open(drm_device, canvas_w, canvas_h) {
            Ok(p) => {
                tracing::info!("presenter: using DRM/KMS page-flip ({})", drm_device);
                Ok(Box::new(p))
            }
            Err(e) => {
                tracing::warn!(
                    "presenter: DRM/KMS unavailable ({:#}), falling back to fbdev ({})",
                    e,
                    fb_device
                );
                Ok(Box::new(VsyncFb::open(fb_device)?))
            }
        },
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
) -> Result<Box<dyn Presenter>> {
    match kind {
        PresenterKind::Kms => anyhow::bail!("DRM/KMS presenter is Linux-only"),
        _ => Ok(Box::new(VsyncFb::open(fb_device)?)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_known_kinds() {
        assert_eq!(PresenterKind::parse("auto").unwrap(), PresenterKind::Auto);
        assert_eq!(PresenterKind::parse("kms").unwrap(), PresenterKind::Kms);
        assert_eq!(PresenterKind::parse("fbdev").unwrap(), PresenterKind::Fbdev);
    }

    #[test]
    fn parse_rejects_unknown() {
        let err = PresenterKind::parse("vsync").unwrap_err();
        assert!(err.to_string().contains("unknown --presenter"), "{err}");
    }
}
