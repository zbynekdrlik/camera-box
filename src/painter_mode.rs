//! Explicit painter display-mode override (#1179 — the 2560x1080@100 experiment from issue 881,
//! the LG 34U511A-B native max without a 120 Hz monitor swap).
//!
//! PURE, crate-root, no probe deps — so its RED→GREEN is Tier-0-verifiable (this repo's probe
//! modules only compile under `--features probe`, i.e. CI-only). The probe-gated painter/presenter/
//! kms glue consumes the resolved values; this module owns only the parsing, the geometry scaling,
//! and the "override-or-defaults" resolution.
//!
//! ## Why an override, and what it touches
//!
//! The painter renders a fixed canvas and `KmsPresenter` selects the HDMI mode that matches that
//! canvas AND runs at exactly the 60.000 Hz capture rate (`probe::kms::pick_mode` +
//! `TARGET_REFRESH_MHZ`). To drive 2560x1080@100 instead, TWO things must change: the CANVAS
//! (already a `canvas_w`/`canvas_h` parameter) and the mode-SELECTION refresh handed to
//! `pick_mode` (the genuinely-hardcoded value). This module resolves both from one `WxH@RR` string.
//!
//! The mode-SELECTION refresh is DISTINCT from the capture phase-lock reference: 100 Hz is not an
//! integer multiple of the 60 fps capture, so no tear-free 1:1 lock exists at 100 Hz — the presenter
//! keeps checking phase-lockability against the fixed 60.000 Hz capture rate, so a 100 Hz run is
//! honestly reported as NOT phase-locked. This module never touches that check; it only supplies the
//! selection target + the scaled canvas geometry.

/// The painter's historical fixed canvas width — the baseline the dual-QR geometry was tuned at
/// (QR px 700 on a 1920-wide canvas). QR scaling references THIS constant so a wider override keeps
/// the QR the same physical fraction of the panel (a smaller physical QR would confound the #1179
/// undecodable-floor measurement).
pub const BASELINE_CANVAS_W: u32 = 1920;

/// A parsed `WxH@RR` display-mode override.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeOverride {
    pub width: u32,
    pub height: u32,
    /// Vertical refresh in milli-Hz (100_000 == 100.000 Hz), matching
    /// `probe::kms::ModeCandidate::refresh_mhz` so it feeds `pick_mode` directly.
    pub refresh_mhz: u32,
}

/// The painter canvas parameters resolved from an OPTIONAL override plus the CLI defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedCanvas {
    pub canvas_w: u32,
    pub canvas_h: u32,
    pub qr_size: u32,
    /// The mode-SELECTION refresh target handed to `pick_mode` (NOT the phase-lock reference).
    pub mode_refresh_mhz: u32,
}

/// Parse a `WxH@RR` display-mode string, e.g. `2560x1080@100` or `1920x1080@59.94`.
///
/// `RR` is Hz and may be fractional (converted to milli-Hz, rounded to nearest — so `100` →
/// `100_000`, `59.94` → `59_940`). Every component must be strictly positive. Returns a
/// human-readable `Err(String)` for any malformed input (so the CLI can surface it verbatim).
pub fn parse_display_mode(s: &str) -> Result<ModeOverride, String> {
    let s = s.trim();
    let (dims, rr) = s
        .split_once('@')
        .ok_or_else(|| format!("display mode '{s}' is missing '@<refresh>' (want WxH@RR)"))?;
    let (w_str, h_str) = dims
        .split_once('x')
        .ok_or_else(|| format!("display mode '{s}' is missing 'x' between width and height"))?;

    let width: u32 = w_str
        .trim()
        .parse()
        .map_err(|_| format!("display mode '{s}': width '{w_str}' is not an integer"))?;
    let height: u32 = h_str
        .trim()
        .parse()
        .map_err(|_| format!("display mode '{s}': height '{h_str}' is not an integer"))?;
    let refresh_hz: f64 = rr
        .trim()
        .parse()
        .map_err(|_| format!("display mode '{s}': refresh '{rr}' is not a number"))?;

    if width == 0 || height == 0 {
        return Err(format!("display mode '{s}': width and height must be > 0"));
    }
    if !(refresh_hz.is_finite() && refresh_hz > 0.0) {
        return Err(format!(
            "display mode '{s}': refresh must be a positive number of Hz"
        ));
    }

    let refresh_mhz = (refresh_hz * 1000.0).round() as u32;
    if refresh_mhz == 0 {
        return Err(format!("display mode '{s}': refresh rounds to 0 mHz"));
    }
    Ok(ModeOverride {
        width,
        height,
        refresh_mhz,
    })
}

/// Scale a QR pixel size proportionally with canvas width, relative to `base_canvas_w`, rounded to
/// nearest. Keeps the QR the same physical fraction of the panel when the canvas widens (700 px on
/// a 1920-wide canvas → 933 px on a 2560-wide canvas). Returns `base_qr` unchanged when
/// `new_canvas_w == base_canvas_w`, and (defensively) when `base_canvas_w == 0`.
pub fn scaled_qr_size(base_qr: u32, base_canvas_w: u32, new_canvas_w: u32) -> u32 {
    if base_canvas_w == 0 || new_canvas_w == base_canvas_w {
        return base_qr;
    }
    let num = base_qr as u64 * new_canvas_w as u64 + (base_canvas_w as u64 / 2);
    (num / base_canvas_w as u64) as u32
}

/// Resolve the painter canvas parameters from an OPTIONAL mode override plus the CLI defaults.
///
/// With NO override this returns the defaults VERBATIM — i.e. behaviour is byte-identical to before
/// #1179 (this is the guarantee `resolve_canvas_none_returns_defaults_verbatim` pins). With an
/// override, the canvas follows the mode, the QR scales proportionally from [`BASELINE_CANVAS_W`],
/// and `mode_refresh_mhz` becomes the override's refresh (fed to `pick_mode`). `default_qr_size` is
/// the baseline QR at the 1920-wide canvas; `default_refresh_mhz` is the capture-rate selection
/// target used when no override is given.
pub fn resolve_canvas(
    over: Option<ModeOverride>,
    default_canvas_w: u32,
    default_canvas_h: u32,
    default_qr_size: u32,
    default_refresh_mhz: u32,
) -> ResolvedCanvas {
    match over {
        None => ResolvedCanvas {
            canvas_w: default_canvas_w,
            canvas_h: default_canvas_h,
            qr_size: default_qr_size,
            mode_refresh_mhz: default_refresh_mhz,
        },
        Some(m) => ResolvedCanvas {
            canvas_w: m.width,
            canvas_h: m.height,
            qr_size: scaled_qr_size(default_qr_size, BASELINE_CANVAS_W, m.width),
            mode_refresh_mhz: m.refresh_mhz,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_integer_refresh() {
        assert_eq!(
            parse_display_mode("2560x1080@100"),
            Ok(ModeOverride {
                width: 2560,
                height: 1080,
                refresh_mhz: 100_000,
            })
        );
    }

    #[test]
    fn parse_fractional_refresh_to_millihz() {
        // 59.94 Hz -> 59_940 mHz (the 59.94-vs-60.00 distinction pick_mode preserves).
        assert_eq!(
            parse_display_mode("1920x1080@59.94"),
            Ok(ModeOverride {
                width: 1920,
                height: 1080,
                refresh_mhz: 59_940,
            })
        );
    }

    #[test]
    fn parse_tolerates_surrounding_whitespace() {
        assert_eq!(
            parse_display_mode("  2560x1080@100  "),
            Ok(ModeOverride {
                width: 2560,
                height: 1080,
                refresh_mhz: 100_000,
            })
        );
    }

    #[test]
    fn parse_rejects_malformed() {
        for bad in [
            "",
            "2560x1080",      // no @refresh
            "2560@100",       // no x
            "2560x1080@",     // empty refresh
            "axb@100",        // non-integer dims
            "2560x1080@abc",  // non-numeric refresh
            "0x1080@100",     // zero width
            "2560x0@100",     // zero height
            "2560x1080@0",    // zero refresh
            "2560x1080@-100", // negative refresh
        ] {
            assert!(
                parse_display_mode(bad).is_err(),
                "expected '{bad}' to be rejected"
            );
        }
    }

    #[test]
    fn scaled_qr_size_widens_proportionally() {
        // The ticket's exact figure: 700 px on a 1920 canvas -> ~933 on a 2560 canvas.
        assert_eq!(scaled_qr_size(700, 1920, 2560), 933);
    }

    #[test]
    fn scaled_qr_size_identity_when_width_unchanged() {
        assert_eq!(scaled_qr_size(700, 1920, 1920), 700);
    }

    #[test]
    fn scaled_qr_size_guards_zero_base() {
        assert_eq!(scaled_qr_size(700, 0, 2560), 700);
    }

    #[test]
    fn resolve_canvas_none_returns_defaults_verbatim() {
        // The byte-identical-default guarantee: no --display-mode ⇒ every field is the CLI default
        // unchanged, so the painter selects exactly the mode it selects today.
        assert_eq!(
            resolve_canvas(None, 1920, 1080, 700, 60_000),
            ResolvedCanvas {
                canvas_w: 1920,
                canvas_h: 1080,
                qr_size: 700,
                mode_refresh_mhz: 60_000,
            }
        );
    }

    #[test]
    fn resolve_canvas_applies_override_and_scales_qr() {
        let over = parse_display_mode("2560x1080@100").unwrap();
        assert_eq!(
            resolve_canvas(Some(over), 1920, 1080, 700, 60_000),
            ResolvedCanvas {
                canvas_w: 2560,
                canvas_h: 1080,
                qr_size: 933,
                mode_refresh_mhz: 100_000,
            }
        );
    }

    #[test]
    fn resolve_canvas_scales_qr_from_a_nondefault_base() {
        // If --qr-size sets the 1920-baseline QR, the override scales THAT base.
        let over = parse_display_mode("2560x1080@100").unwrap();
        let r = resolve_canvas(Some(over), 1920, 1080, 800, 60_000);
        assert_eq!(r.qr_size, scaled_qr_size(800, BASELINE_CANVAS_W, 2560));
        assert_eq!(r.canvas_w, 2560);
        assert_eq!(r.mode_refresh_mhz, 100_000);
    }
}
