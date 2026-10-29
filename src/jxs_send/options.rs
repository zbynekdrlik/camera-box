//! Issue 1387: how the JPEG XS sender picks its bpp (fixed or automatic) and whether its
//! encoder runs SCHED_FIFO; pure parsing of the environment, every platform.

/// The environment variable that sets the encoder's bits per pixel (owner decision 29.10.2026).
pub const BPP_ENV: &str = "CAMERA_BOX_JXS_BPP";
/// The default bits per pixel (comment 6489868243: ~250 Mb/s at 30 fps).
pub const DEFAULT_BPP: u32 = 4;
/// The highest bpp the wire format carries: a 16-line 1080p slice at 12 bpp is ~46 KB, under the
/// 64 KB slice length field.
pub const MAX_BPP: u32 = 12;
// the automatic bpp tracks a back-off penalty for every level a sender can run
const _: () = assert!((MAX_BPP as usize) < crate::jxs_auto_bpp::LEVELS);

/// The encoder's bits per pixel from [`BPP_ENV`]: unset or blank = [`DEFAULT_BPP`]; a whole
/// number 1..=[`MAX_BPP`] is taken; anything else is refused (`Err`, the caller logs it and keeps
/// the default).
pub fn parse_bpp(value: Option<&str>) -> Result<u32, String> {
    let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(DEFAULT_BPP);
    };
    match v.parse::<u32>() {
        Ok(b) if (1..=MAX_BPP).contains(&b) => Ok(b),
        _ => Err(format!(
            "{BPP_ENV}='{v}' is not a whole number 1..={MAX_BPP}"
        )),
    }
}

/// The automatic bpp's floor and ceiling (issue 1387 auto bpp). Used only when [`BPP_ENV`] is
/// NOT set: a set `CAMERA_BOX_JXS_BPP` pins a fixed bpp and turns the automatic one off.
pub const BPP_MIN_ENV: &str = "CAMERA_BOX_JXS_BPP_MIN";
pub const BPP_MAX_ENV: &str = "CAMERA_BOX_JXS_BPP_MAX";
/// The automatic bpp's defaults: start (and floor) at the measured 4, climb to 8 (~500 Mb/s at
/// 30 fps, the owner's budget on the 1 GbE cambox link).
pub const DEFAULT_AUTO_MIN_BPP: u32 = 4;
pub const DEFAULT_AUTO_MAX_BPP: u32 = 8;
/// Opt-in SCHED_FIFO priority for the encoder threads (unset = SCHED_OTHER, like every other
/// auxiliary thread). Must stay below the capture's [`crate::affinity::CAPTURE_FIFO_PRIORITY`].
pub const FIFO_ENV: &str = "CAMERA_BOX_JXS_FIFO_PRIO";

/// How the sender picks its bpp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BppMode {
    /// `CAMERA_BOX_JXS_BPP` is set: this bpp, always.
    Fixed(u32),
    /// The automatic controller (`crate::jxs_auto_bpp`) between these bounds, starting at `min`.
    Auto { min: u32, max: u32 },
}

impl BppMode {
    /// The bpp the first session opens at.
    pub fn start(&self) -> u32 {
        match *self {
            Self::Fixed(b) => b,
            Self::Auto { min, .. } => min,
        }
    }
}

/// The bpp mode from [`BPP_ENV`] / [`BPP_MIN_ENV`] / [`BPP_MAX_ENV`], plus a warning per value it
/// refused (the caller logs them). `CAMERA_BOX_JXS_BPP` alone pins a fixed bpp (a refused value
/// keeps [`DEFAULT_BPP`], fixed, as before); otherwise the automatic mode between the floor and
/// the ceiling (each refused value keeps its default; a floor above the ceiling keeps both
/// defaults).
pub fn parse_bpp_mode(
    bpp: Option<&str>,
    min: Option<&str>,
    max: Option<&str>,
) -> (BppMode, Vec<String>) {
    fn set(v: Option<&str>) -> Option<&str> {
        v.map(str::trim).filter(|v| !v.is_empty())
    }
    let mut warnings = Vec::new();
    if set(bpp).is_some() {
        let b = parse_bpp(bpp).unwrap_or_else(|e| {
            warnings.push(format!("{e} -- using {DEFAULT_BPP} bpp"));
            DEFAULT_BPP
        });
        if set(min).is_some() || set(max).is_some() {
            warnings.push(format!(
                "{BPP_ENV} pins a fixed bpp: {BPP_MIN_ENV} / {BPP_MAX_ENV} are ignored (the automatic bpp is off)"
            ));
        }
        return (BppMode::Fixed(b), warnings);
    }
    let mut bound = |name: &str, v: Option<&str>, default: u32| match set(v) {
        None => default,
        Some(_) => parse_bpp(v).unwrap_or_else(|e| {
            warnings.push(format!(
                "{} -- using {default} as {name}",
                e.replace(BPP_ENV, name)
            ));
            default
        }),
    };
    let lo = bound(BPP_MIN_ENV, min, DEFAULT_AUTO_MIN_BPP);
    let hi = bound(BPP_MAX_ENV, max, DEFAULT_AUTO_MAX_BPP);
    if lo > hi {
        warnings.push(format!(
            "{BPP_MIN_ENV}={lo} is above {BPP_MAX_ENV}={hi} -- using {DEFAULT_AUTO_MIN_BPP}..={DEFAULT_AUTO_MAX_BPP}"
        ));
        return (
            BppMode::Auto {
                min: DEFAULT_AUTO_MIN_BPP,
                max: DEFAULT_AUTO_MAX_BPP,
            },
            warnings,
        );
    }
    (BppMode::Auto { min: lo, max: hi }, warnings)
}

/// The encoder's SCHED_FIFO priority from [`FIFO_ENV`]: unset / blank / `0` / `off` = `None`
/// (SCHED_OTHER); a whole number 1 .. capture priority - 1 is taken; anything else is refused.
pub fn parse_fifo_prio(value: Option<&str>) -> Result<Option<i32>, String> {
    let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    if v == "0" || v.eq_ignore_ascii_case("off") {
        return Ok(None);
    }
    let top = crate::affinity::CAPTURE_FIFO_PRIORITY - 1;
    match v.parse::<i32>() {
        Ok(p) if (1..=top).contains(&p) => Ok(Some(p)),
        _ => Err(format!(
            "{FIFO_ENV}='{v}' is not 1..={top} (below the capture's SCHED_FIFO {})",
            crate::affinity::CAPTURE_FIFO_PRIORITY
        )),
    }
}

/// The cambox's send pacing override (issue 1387): unset / blank / `0` = AUTOMATIC, the default
/// (`jxs_link::pace_auto`: paced to 90 % of the receiver's link when it is slower than this one,
/// else line rate -- a 1 GbE cambox into the strih's 10 GbE stays at line rate); else a fixed rate
/// in Mb/s of Ethernet wire, `jxs_link::pace::PACE_MBPS_MIN..=PACE_MBPS_MAX`.
pub const PACE_ENV: &str = "CAMERA_BOX_JXS_PACE_MBPS";

/// The send pacing from [`PACE_ENV`]: unset / blank = 0 (automatic); a whole number 0 or
/// `PACE_MBPS_MIN..=PACE_MBPS_MAX` is taken; anything else is refused (the caller logs it and
/// keeps the automatic pace).
pub fn parse_pace_mbps(value: Option<&str>) -> Result<u32, String> {
    let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(0);
    };
    v.parse::<u32>()
        .map_err(|_| format!("{PACE_ENV}='{v}' is not a whole number"))
        .and_then(|m| {
            crate::jxs_link::pace::check_pace_mbps(m).map_err(|e| format!("{PACE_ENV}: {e}"))
        })
}

/// The "link ON" line's pacing words.
pub fn pace_text(mbps: u32) -> String {
    if mbps == 0 {
        "line rate (no pacing)".to_string()
    } else {
        format!(
            "paced at {mbps} Mb/s of wire, at most {} datagrams per send",
            crate::jxs_link::pace::PACE_BURST_SEGMENTS
        )
    }
}

/// What the sender runs with besides its target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SenderOptions {
    pub bpp: BppMode,
    /// The encoder threads' SCHED_FIFO priority (`None` = SCHED_OTHER).
    pub fifo: Option<i32>,
    /// The send pacing override in Mb/s of Ethernet wire, 0 = automatic
    /// (`crate::jxs_link::pace_auto`).
    pub pace_mbps: u32,
    /// The automatic bpp's tuning (`None` = `AutoBppConfig::new(min, max)`, the sender's
    /// setting; tests shorten the clean run).
    pub auto_config: Option<crate::jxs_auto_bpp::AutoBppConfig>,
}

impl SenderOptions {
    pub fn new(bpp: BppMode, fifo: Option<i32>) -> Self {
        Self {
            bpp,
            fifo,
            pace_mbps: 0,
            auto_config: None,
        }
    }

    /// The same options sending at `pace_mbps` (0 = automatic).
    pub fn with_pace(self, pace_mbps: u32) -> Self {
        Self { pace_mbps, ..self }
    }
}

/// The "link ON" line's bpp words.
pub fn bpp_mode_text(m: BppMode) -> String {
    match m {
        BppMode::Fixed(b) => format!("{b} bpp (fixed by {BPP_ENV}, auto off)"),
        BppMode::Auto { min, max } => format!(
            "auto bpp {min}..={max} (starts at {min}, climbs only on the receiver's reports)"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bpp_is_fixed_when_pinned_else_automatic_between_the_bounds() {
        let auto = |lo, hi| BppMode::Auto { min: lo, max: hi };
        assert_eq!(parse_bpp_mode(None, None, None), (auto(4, 8), vec![]));
        assert_eq!(parse_bpp_mode(Some(" "), None, None).0, auto(4, 8));
        assert_eq!(
            parse_bpp_mode(None, Some("5"), Some("10")),
            (auto(5, 10), vec![])
        );
        assert_eq!(parse_bpp_mode(None, Some("6"), Some("6")).0, auto(6, 6));
        // BPP alone pins it: auto off
        assert_eq!(
            parse_bpp_mode(Some("6"), None, None),
            (BppMode::Fixed(6), vec![])
        );
        let (m, w) = parse_bpp_mode(Some("6"), Some("4"), None);
        assert_eq!(m, BppMode::Fixed(6));
        assert!(w[0].contains("ignored"), "{w:?}");
        // a refused pinned value keeps the old default, still fixed
        let (m, w) = parse_bpp_mode(Some("x"), None, None);
        assert_eq!(m, BppMode::Fixed(DEFAULT_BPP));
        assert_eq!(w.len(), 1);
        // a refused bound keeps its default and names its own variable
        let (m, w) = parse_bpp_mode(None, Some("0"), Some("99"));
        assert_eq!(m, auto(4, 8));
        assert!(
            w[0].contains(BPP_MIN_ENV) && w[1].contains(BPP_MAX_ENV),
            "{w:?}"
        );
        // a floor above the ceiling keeps both defaults
        let (m, w) = parse_bpp_mode(None, Some("9"), Some("5"));
        assert_eq!(m, auto(4, 8));
        assert!(w[0].contains("above"), "{w:?}");
        assert_eq!(auto(5, 9).start(), 5);
        assert_eq!(BppMode::Fixed(7).start(), 7);
        assert!(bpp_mode_text(auto(4, 8)).contains("auto bpp 4..=8"));
        assert!(bpp_mode_text(BppMode::Fixed(6)).contains("6 bpp (fixed"));
    }

    #[test]
    fn the_cambox_pace_is_automatic_unless_set_to_a_bounded_rate() {
        assert_eq!(parse_pace_mbps(None), Ok(0));
        assert_eq!(parse_pace_mbps(Some(" ")), Ok(0));
        assert_eq!(parse_pace_mbps(Some("0")), Ok(0));
        assert_eq!(parse_pace_mbps(Some(" 900\n")), Ok(900));
        assert_eq!(parse_pace_mbps(Some("100")), Ok(100));
        for bad in ["99", "100001", "-5", "x", "9.5"] {
            let e = parse_pace_mbps(Some(bad)).unwrap_err();
            assert!(e.contains(PACE_ENV), "{bad}: {e}");
        }
        let opts = SenderOptions::new(BppMode::Fixed(4), None);
        assert_eq!(opts.pace_mbps, 0, "automatic unless asked");
        assert_eq!(opts.with_pace(900).pace_mbps, 900);
        assert_eq!(opts.with_pace(900).bpp, BppMode::Fixed(4));
        assert!(pace_text(0).contains("paced automatically (90% of the receiver's link"));
        assert!(
            pace_text(900).contains("paced at 900 Mb/s"),
            "{}",
            pace_text(900)
        );
    }

    #[test]
    fn the_encoder_fifo_is_opt_in_and_stays_below_the_capture() {
        assert_eq!(parse_fifo_prio(None), Ok(None));
        assert_eq!(parse_fifo_prio(Some("")), Ok(None));
        assert_eq!(parse_fifo_prio(Some("0")), Ok(None));
        assert_eq!(parse_fifo_prio(Some("OFF")), Ok(None));
        assert_eq!(parse_fifo_prio(Some(" 50\n")), Ok(Some(50)));
        assert_eq!(parse_fifo_prio(Some("89")), Ok(Some(89)));
        for bad in ["90", "99", "-1", "x", "5.5"] {
            let e = parse_fifo_prio(Some(bad)).unwrap_err();
            assert!(e.contains(FIFO_ENV), "{bad}: {e}");
        }
        // (JXS_ENCODER_FIFO_PRIORITY < CAPTURE_FIFO_PRIORITY is a const assert in affinity.rs)
        assert_eq!(
            parse_fifo_prio(Some(
                &crate::affinity::JXS_ENCODER_FIFO_PRIORITY.to_string()
            )),
            Ok(Some(crate::affinity::JXS_ENCODER_FIFO_PRIORITY))
        );
    }
}
