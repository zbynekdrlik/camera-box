//! #464 — the pure Auto-fallback PRESENTER decision, extracted out of
//! `probe::presenter::open_presenter`'s hardware I/O so it compiles + unit-tests on DEFAULT
//! features (Tier-0) — the same crate-root pure-seam pattern as `src/reannounce.rs` /
//! `src/colour_scale.rs` (see their doc comments for why these live at the crate root instead of
//! inside the `probe` feature).
//!
//! ## The bug this exists to prevent recurring (#464, confirmed live on cam2 2026-07-04)
//!
//! `scripts/rig-mode.sh` launches the QR painter with the default `--presenter auto` (no
//! `--presenter` flag). On cam2's Intel i915 with a connected HDMI, `auto` acquires DRM master and
//! runs the KMS page-flip presenter ([`crate::probe::kms::KmsPresenter`], via
//! `probe::presenter::open_presenter`), which BY DESIGN never opens `/dev/fb0` — only the fbdev
//! fallback ([`crate::probe::fb::VsyncFb`]) does. The rig's liveness gate checked ONLY
//! `fuser -s /dev/fb0`, so a healthy, correctly-painting KMS run was reported as a FAILED painter
//! (live evidence: `presenter: using DRM/KMS page-flip (/dev/dri/card1)` +
//! `vblank-locked 1:1 60Hz` in the log, `/dev/dri/card1` held, `/dev/fb0` held by nobody, marker
//! CSV growing — genuinely painting, reported FAIL).
//!
//! `resolve_presenter_kind` is the SINGLE SOURCE OF TRUTH both `open_presenter` (this crate) and
//! `scripts/rig-mode.sh`'s presenter-aware liveness gate
//! (`scripts/lib/presenter-liveness-check.sh`) answer "will this run ever touch `/dev/fb0`?" from
//! — so the two can never drift apart again.

use anyhow::Result;

/// How the caller asked the painter to obtain a presenter — mirrors the CLI `--presenter` values
/// (`auto` | `kms` | `fbdev`). Moved here (out of `probe::presenter`, which is
/// `#[cfg(feature = "probe")]`) so it — and the pure decision below — compile on default features;
/// `probe::presenter` re-exports this type so every existing `probe::presenter::PresenterKind`
/// reference keeps compiling unchanged.
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
    /// Parse the `--presenter` CLI value. Unknown values error so a typo does not silently fall
    /// back to a different presenter than the operator asked.
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "auto" => Ok(PresenterKind::Auto),
            "kms" => Ok(PresenterKind::Kms),
            "fbdev" => Ok(PresenterKind::Fbdev),
            other => anyhow::bail!("unknown --presenter '{}' (use auto|kms|fbdev)", other),
        }
    }
}

/// The outcome of resolving a requested [`PresenterKind`] against whether the KMS presenter could
/// actually be opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedPresenter {
    /// The presenter that will actually run.
    pub kind: PresenterKind,
    /// Whether that presenter touches `/dev/fb0` — the exact fact #464's liveness gate got wrong
    /// by assuming it was always true.
    pub touches_fb0: bool,
}

/// Resolve `requested` against `kms_open_ok` (whether `KmsPresenter::open` succeeded) into the
/// presenter that will ACTUALLY run plus whether it touches `/dev/fb0`. Pure — no I/O, no device
/// access — so both `open_presenter` (fed the real KMS-open result) and a unit test (fed a
/// synthetic bool) drive it identically.
///
/// `Kms`/`Fbdev` are forced choices — `kms_open_ok` is irrelevant to them (a forced `Kms` that
/// fails to open is an error in `open_presenter`, never a fallback; a forced `Fbdev` never
/// attempts KMS at all). Only `Auto` actually branches on `kms_open_ok` (#79/#464): a successful
/// KMS open resolves to `Kms` (never touches `/dev/fb0` — the exact fact the #464 liveness gate
/// got wrong), a failed one falls back to `Fbdev` (the unchanged #68 path, which DOES touch it).
pub fn resolve_presenter_kind(requested: PresenterKind, kms_open_ok: bool) -> ResolvedPresenter {
    match requested {
        PresenterKind::Kms => ResolvedPresenter {
            kind: PresenterKind::Kms,
            touches_fb0: false,
        },
        PresenterKind::Fbdev => ResolvedPresenter {
            kind: PresenterKind::Fbdev,
            touches_fb0: true,
        },
        PresenterKind::Auto => {
            if kms_open_ok {
                ResolvedPresenter {
                    kind: PresenterKind::Kms,
                    touches_fb0: false,
                }
            } else {
                ResolvedPresenter {
                    kind: PresenterKind::Fbdev,
                    touches_fb0: true,
                }
            }
        }
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

    /// #464 HEADLINE: Auto + a successful KMS open must resolve to Kms, and — the exact fact the
    /// live bug got wrong — must NOT be reported as touching /dev/fb0 (KmsPresenter never opens
    /// it).
    #[test]
    fn auto_with_kms_open_ok_resolves_to_kms_not_touching_fb0() {
        let r = resolve_presenter_kind(PresenterKind::Auto, true);
        assert_eq!(
            r.kind,
            PresenterKind::Kms,
            "#464: Auto + KMS-open-ok must resolve to Kms"
        );
        assert!(
            !r.touches_fb0,
            "#464: the KMS presenter never opens /dev/fb0 — a gate that assumes it does is \
             exactly the #464 bug (painter alive but NOT writing /dev/fb0, falsely reported FAIL)"
        );
    }

    /// Auto + a FAILED KMS open must fall back to Fbdev, which DOES touch /dev/fb0 (the #68 path,
    /// unchanged behavior).
    #[test]
    fn auto_with_kms_open_failed_resolves_to_fbdev_touching_fb0() {
        let r = resolve_presenter_kind(PresenterKind::Auto, false);
        assert_eq!(
            r.kind,
            PresenterKind::Fbdev,
            "#464: Auto + KMS-open-failed must fall back to Fbdev"
        );
        assert!(
            r.touches_fb0,
            "#464: the fbdev fallback DOES touch /dev/fb0 — unchanged #68 behavior"
        );
    }

    /// A forced `--presenter kms` never touches fb0, regardless of the (irrelevant) kms_open_ok
    /// input — when Kms is forced, `open_presenter` errors out instead of falling back, so there
    /// is no fbdev path to consider.
    #[test]
    fn forced_kms_never_touches_fb0() {
        assert!(!resolve_presenter_kind(PresenterKind::Kms, true).touches_fb0);
        assert!(!resolve_presenter_kind(PresenterKind::Kms, false).touches_fb0);
    }

    /// A forced `--presenter fbdev` always touches fb0, regardless of kms_open_ok (KMS is never
    /// attempted on this path).
    #[test]
    fn forced_fbdev_always_touches_fb0() {
        assert!(resolve_presenter_kind(PresenterKind::Fbdev, true).touches_fb0);
        assert!(resolve_presenter_kind(PresenterKind::Fbdev, false).touches_fb0);
    }
}
