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

/// Extract the numeric suffix from a `/dev/dri/cardN` path or bare basename, for ordering DRM
/// device candidates deterministically. Returns `None` for anything that is not a bare `cardN`
/// node (render nodes like `renderD128`, the `by-path` symlink directory, or any other entry) so
/// callers can filter those out before trying to open them as a KMS display device.
pub fn drm_card_index(entry: &str) -> Option<u32> {
    entry.rsplit('/').next()?.strip_prefix("card")?.parse().ok()
}

/// Order a list of `/dev/dri` directory entries (bare basenames or full paths) into the sequence
/// of DRM card device candidates `open_presenter`'s Auto fallback should try, ascending by card
/// number, after the configured/default device already failed to open.
///
/// #854: `/dev/dri/cardN` numbering is NOT a stable ABI — it depends on module load order, which
/// a kernel/driver update or a reboot can change even on a single-GPU box with no other change at
/// all. cam2 enumerated its i915 KMS device as `card1` on 2026-07-04 (see the #464 doc comment
/// above — its quoted live log line proves KMS was genuinely working then) and as `card0` by
/// 2026-07-28, with NO `card1` node at all. Because `PresenterKind::Auto` treats a failed KMS
/// open as an ordinary, silent fallback to the imperfect single-buffered fbdev presenter, this
/// kind of renumbering degrades the tear-free double-buffered KMS guarantee permanently and
/// invisibly — no error, no crash, just a quietly worse presenter from that reboot onward.
/// Ascending order is deterministic (never raw `read_dir` order, which is unspecified) and tries
/// the lowest-numbered — conventionally primary — card first.
pub fn order_drm_card_candidates(entries: &[String]) -> Vec<String> {
    let mut candidates: Vec<(u32, String)> = entries
        .iter()
        .filter_map(|e| drm_card_index(e).map(|n| (n, e.clone())))
        .collect();
    candidates.sort_by_key(|(n, _)| *n);
    candidates.into_iter().map(|(_, path)| path).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #854 RED: `/dev/dri` card numbering is not a stable ABI — cam2 enumerated its i915 KMS
    /// device as `card1` on 2026-07-04 (the #464 doc comment above quotes a live log line
    /// proving it) and as `card0` by 2026-07-28, with NO `card1` node at all after a reboot +
    /// kernel update. `order_drm_card_candidates` does not exist yet — this is the pure decision
    /// `open_presenter`'s Auto fallback will use to try every OTHER `/dev/dri/cardN` before
    /// giving up on KMS, so a future renumbering can never again silently and permanently
    /// degrade a tear-free double-buffered KMS run to the imperfect single-buffered fbdev path.
    #[test]
    fn order_drm_card_candidates_filters_and_sorts_ascending() {
        let entries = vec![
            "/dev/dri/renderD128".to_string(),
            "/dev/dri/card1".to_string(),
            "/dev/dri/by-path".to_string(),
            "/dev/dri/card0".to_string(),
        ];
        assert_eq!(
            order_drm_card_candidates(&entries),
            vec!["/dev/dri/card0".to_string(), "/dev/dri/card1".to_string()],
            "#854: render nodes / by-path entries must be filtered out, and cardN entries \
             ordered ascending by number (deterministic — never raw read_dir order)"
        );
    }

    #[test]
    fn order_drm_card_candidates_empty_when_no_card_nodes() {
        let entries = vec!["/dev/dri/renderD128".to_string()];
        assert!(order_drm_card_candidates(&entries).is_empty());
    }

    #[test]
    fn order_drm_card_candidates_handles_bare_basenames() {
        // `open_presenter`'s discovery reads directory ENTRY NAMES (basenames), not full
        // paths, in one call site — the fn must work on either shape.
        let entries = vec![
            "card2".to_string(),
            "card0".to_string(),
            "renderD129".to_string(),
        ];
        assert_eq!(
            order_drm_card_candidates(&entries),
            vec!["card0".to_string(), "card2".to_string()]
        );
    }

    #[test]
    fn drm_card_index_parses_card_n_only() {
        assert_eq!(drm_card_index("/dev/dri/card1"), Some(1));
        assert_eq!(drm_card_index("card0"), Some(0));
        assert_eq!(drm_card_index("/dev/dri/renderD128"), None);
        assert_eq!(drm_card_index("/dev/dri/by-path"), None);
        assert_eq!(
            drm_card_index("card"),
            None,
            "no digits after 'card' -> None"
        );
        assert_eq!(drm_card_index("cardX"), None, "non-numeric suffix -> None");
    }

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
