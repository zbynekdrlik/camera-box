//! #1362 — resolve the cameraman HDMI preview source from what is actually on the LAN.
//!
//! Every cambox previews the strih box's `interkom` NDI output on its HDMI monitor (the #528
//! fleet-wide default — no per-box config, no env knob). That source used to be one baked box
//! name (`STRIH-SNV (interkom)`), and `NdiReceiver::connect` finds sources by name, so the strih
//! hardware swap (Windows STRIH-SNV -> Linux STRIH-LX on 20.9.2026) turned every cameraman monitor
//! black until a new binary shipped — and any future strih box (e.g. a `STRIH-PP`) would again.
//!
//! The preferred name is matched EXACTLY (an explicit `--display`/`[display]` value included —
//! the old receiver matched a substring); a non-exact strih value still reaches the single
//! `STRIH-<box> (interkom)` fallback, but only after the whole find window (30 s).
//!
//! The PURE decision lives here (no NDI, no I/O — Tier-0 unit-tested); the finder loop that feeds
//! it the discovered names and connects to the chosen exact name is `crate::ndi::NdiReceiver::
//! connect_with`, driven by `crate::ndi_display::run_display_loop`.
//!
//! Rules (the approved Approach 1 on the ticket):
//! * the preferred name (the baked default, or an explicit CLI/config source) wins whenever it is
//!   on the LAN, and it is connected to the moment it is seen;
//! * otherwise the ONE discovered `STRIH-<box> (interkom)` output is used — but only after the
//!   whole find window elapsed, so a slow mDNS announce of the preferred box is never pre-empted;
//! * two or more such outputs and no preferred one = ambiguity: pick NOTHING (keep waiting for
//!   the preferred name) and report the candidates — never alternate between strih boxes;
//! * nothing matching = keep retrying exactly as before.

/// The preferred preview source every cambox asks for: the CURRENT strih box's interkom output
/// (strih-lx since the M4 cut-over, 20.9.2026). Any other `STRIH-<box> (interkom)` is still found
/// by the fallback in [`resolve_preview_source`], so this only has to name the usual strih.
pub const DEFAULT_PREVIEW_SOURCE: &str = "STRIH-LX (interkom)";

/// NDI host prefix every strih box publishes under (`STRIH-SNV`, `STRIH-LX`, `STRIH-PP`, ...).
pub const STRIH_HOST_PREFIX: &str = "STRIH-";

/// NDI output suffix of the strih interkom/return output (the full NDI name is `HOST (output)`).
pub const INTERKOM_OUTPUT_SUFFIX: &str = " (interkom)";

/// Outcome of resolving the preview source against the discovered NDI source list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewResolution {
    /// The preferred name is on the LAN.
    Preferred(String),
    /// The preferred name is absent; exactly one `STRIH-<box> (interkom)` output is present.
    Fallback(String),
    /// The preferred name is absent and 2+ strih interkom outputs are present (sorted, unique).
    Ambiguous(Vec<String>),
    /// Neither the preferred name nor any strih interkom output is present.
    NotFound,
}

/// `true` when `name` is a strih interkom output: `STRIH-<box> (interkom)` with a non-empty box
/// part, case exactly as NDI publishes it.
pub fn is_strih_interkom(name: &str) -> bool {
    name.strip_prefix(STRIH_HOST_PREFIX)
        .and_then(|rest| rest.strip_suffix(INTERKOM_OUTPUT_SUFFIX))
        .is_some_and(|host| !host.is_empty() && !host.contains(['(', ')']))
}

/// Resolve the preview source from the currently discovered NDI source names.
pub fn resolve_preview_source(discovered: &[String], preferred: &str) -> PreviewResolution {
    if discovered.iter().any(|name| name == preferred) {
        return PreviewResolution::Preferred(preferred.to_string());
    }
    let mut candidates: Vec<String> = discovered
        .iter()
        .filter(|name| is_strih_interkom(name))
        .cloned()
        .collect();
    candidates.sort();
    candidates.dedup();
    match candidates.len() {
        0 => PreviewResolution::NotFound,
        1 => PreviewResolution::Fallback(candidates.remove(0)),
        _ => PreviewResolution::Ambiguous(candidates),
    }
}

/// Decide which exact name (if any) to connect to NOW. `window_elapsed` is `true` on the final
/// pass of the find window: the preferred name is taken on sight, the single fallback only once
/// the window elapsed (a late mDNS announce of the preferred box must win), ambiguity and
/// nothing-found never pick.
pub fn pick_preview_source(resolution: &PreviewResolution, window_elapsed: bool) -> Option<String> {
    match resolution {
        PreviewResolution::Preferred(name) => Some(name.clone()),
        PreviewResolution::Fallback(name) if window_elapsed => Some(name.clone()),
        PreviewResolution::Fallback(_)
        | PreviewResolution::Ambiguous(_)
        | PreviewResolution::NotFound => None,
    }
}

/// Journal level for a preview-source resolution line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewLogLevel {
    /// A source was picked (preferred or the single fallback).
    Info,
    /// The find window ended with nothing picked (ambiguous or not found).
    Warn,
}

/// Should the display loop log this resolution NOW, and at which level? Only a FINAL decision is
/// logged (a pick, or the end of the find window — never a mid-window 1 s finder pass), and only
/// when it differs from `last_logged` (which persists across reconnects), so an unchanged state is
/// never re-logged per retry.
pub fn preview_log_level(
    last_logged: Option<&PreviewResolution>,
    resolution: &PreviewResolution,
    picked: bool,
    window_elapsed: bool,
) -> Option<PreviewLogLevel> {
    if !(picked || window_elapsed) || last_logged == Some(resolution) {
        return None;
    }
    Some(if picked {
        PreviewLogLevel::Info
    } else {
        PreviewLogLevel::Warn
    })
}

/// The one journal line describing a resolution (logged once per change by the display loop).
pub fn preview_log_line(resolution: &PreviewResolution, preferred: &str) -> String {
    match resolution {
        PreviewResolution::Preferred(name) | PreviewResolution::Fallback(name) => {
            format!("NDI display: preview source resolved to '{name}' (#1362)")
        }
        PreviewResolution::Ambiguous(candidates) => {
            let list = candidates
                .iter()
                .map(|c| format!("'{c}'"))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "NDI display: preview source '{preferred}' not on the LAN and {} strih interkom \
                 outputs are ({list}) -- not picking one, waiting for '{preferred}' (#1362)",
                candidates.len()
            )
        }
        PreviewResolution::NotFound => format!(
            "NDI display: preview source '{preferred}' not on the LAN and no \
             STRIH-<box> (interkom) output found -- retrying (#1362)"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// The live LAN of 23.9.2026 (dev1 `avahi-browse -rtp _ndi._tcp`): strih-lx's five OBS
    /// outputs plus camera senders — no `STRIH-SNV (...)` any more.
    fn live_lan_2026_09_23() -> Vec<String> {
        names(&[
            "CAM1 (usb)",
            "CAM3 (usb)",
            "STRIH-LX (2ME PGM)",
            "STRIH-LX (2ME PVW)",
            "STRIH-LX (Grading)",
            "STRIH-LX (interkom)",
            "STRIH-LX (MULTIVIEW)",
            "STREAM-SNV (PGM)",
        ])
    }

    #[test]
    fn default_preview_source_is_the_current_strih_lx_interkom_output() {
        assert_eq!(DEFAULT_PREVIEW_SOURCE, "STRIH-LX (interkom)");
        assert!(is_strih_interkom(DEFAULT_PREVIEW_SOURCE));
    }

    #[test]
    fn preferred_name_on_the_lan_is_used() {
        assert_eq!(
            resolve_preview_source(&live_lan_2026_09_23(), "STRIH-LX (interkom)"),
            PreviewResolution::Preferred("STRIH-LX (interkom)".to_string())
        );
    }

    #[test]
    fn the_reported_bug_old_preferred_name_falls_back_to_the_single_strih_interkom() {
        // The exact fleet state of the ticket: the box asks for the dead Windows strih, the LAN
        // only has strih-lx — the monitor must show STRIH-LX (interkom), not stay black.
        assert_eq!(
            resolve_preview_source(&live_lan_2026_09_23(), "STRIH-SNV (interkom)"),
            PreviewResolution::Fallback("STRIH-LX (interkom)".to_string())
        );
    }

    #[test]
    fn a_future_strih_box_is_found_without_a_new_binary() {
        let lan = names(&["STRIH-PP (2ME PGM)", "STRIH-PP (interkom)", "CAM1 (usb)"]);
        assert_eq!(
            resolve_preview_source(&lan, DEFAULT_PREVIEW_SOURCE),
            PreviewResolution::Fallback("STRIH-PP (interkom)".to_string())
        );
    }

    #[test]
    fn two_strih_interkom_outputs_without_the_preferred_one_are_ambiguous_and_sorted() {
        let lan = names(&["STRIH-SNV (interkom)", "CAM1 (usb)", "STRIH-PP (interkom)"]);
        assert_eq!(
            resolve_preview_source(&lan, "STRIH-LX (interkom)"),
            PreviewResolution::Ambiguous(names(&["STRIH-PP (interkom)", "STRIH-SNV (interkom)"]))
        );
    }

    #[test]
    fn preferred_wins_over_other_strih_interkom_outputs() {
        let lan = names(&[
            "STRIH-PP (interkom)",
            "STRIH-LX (interkom)",
            "STRIH-SNV (interkom)",
        ]);
        assert_eq!(
            resolve_preview_source(&lan, "STRIH-LX (interkom)"),
            PreviewResolution::Preferred("STRIH-LX (interkom)".to_string())
        );
    }

    #[test]
    fn a_duplicate_listing_of_one_source_is_not_ambiguity() {
        let lan = names(&["STRIH-LX (interkom)", "STRIH-LX (interkom)"]);
        assert_eq!(
            resolve_preview_source(&lan, "STRIH-SNV (interkom)"),
            PreviewResolution::Fallback("STRIH-LX (interkom)".to_string())
        );
    }

    #[test]
    fn nothing_matching_is_not_found() {
        assert_eq!(
            resolve_preview_source(&[], DEFAULT_PREVIEW_SOURCE),
            PreviewResolution::NotFound
        );
        let lan = names(&["STRIH-LX (2ME PGM)", "STRIH-LX (MULTIVIEW)", "CAM1 (usb)"]);
        assert_eq!(
            resolve_preview_source(&lan, DEFAULT_PREVIEW_SOURCE),
            PreviewResolution::NotFound
        );
    }

    #[test]
    fn strih_interkom_matching_is_exact_on_prefix_suffix_and_case() {
        assert!(is_strih_interkom("STRIH-LX (interkom)"));
        assert!(is_strih_interkom("STRIH-SNV (interkom)"));
        assert!(is_strih_interkom("STRIH-PP (interkom)"));
        // not a strih interkom output:
        assert!(!is_strih_interkom("STRIH-LX (2ME PGM)"));
        assert!(!is_strih_interkom("STREAM-SNV (interkom)"));
        assert!(!is_strih_interkom("XSTRIH-LX (interkom)"));
        assert!(!is_strih_interkom("strih-lx (interkom)"));
        assert!(!is_strih_interkom("STRIH-LX (Interkom)"));
        assert!(!is_strih_interkom("STRIH-LX (interkom) 2"));
        assert!(!is_strih_interkom("STRIH- (interkom)"));
        assert!(!is_strih_interkom("STRIH-LX(interkom)"));
        assert!(!is_strih_interkom(""));
        // a parenthesised host part is not a strih box name:
        assert!(!is_strih_interkom("STRIH-A (x) (interkom)"));
        assert!(!is_strih_interkom("STRIH-A) (interkom)"));
    }

    #[test]
    fn a_decision_is_logged_once_per_change_and_only_when_final() {
        let preferred = PreviewResolution::Preferred("STRIH-LX (interkom)".to_string());
        let fallback = PreviewResolution::Fallback("STRIH-PP (interkom)".to_string());
        let none = PreviewResolution::NotFound;
        // first pick is logged at INFO, a repeat of it (next reconnect) is not
        assert_eq!(
            preview_log_level(None, &preferred, true, false),
            Some(PreviewLogLevel::Info)
        );
        assert_eq!(
            preview_log_level(Some(&preferred), &preferred, true, false),
            None
        );
        // a mid-window pass that did not pick is never logged (no per-second spam)
        assert_eq!(preview_log_level(None, &fallback, false, false), None);
        assert_eq!(preview_log_level(None, &none, false, false), None);
        // the end of the window without a pick is logged at WARN, once
        assert_eq!(
            preview_log_level(None, &none, false, true),
            Some(PreviewLogLevel::Warn)
        );
        assert_eq!(preview_log_level(Some(&none), &none, false, true), None);
        // a change (not-found -> preferred appeared) is logged again
        assert_eq!(
            preview_log_level(Some(&none), &preferred, true, false),
            Some(PreviewLogLevel::Info)
        );
        // a changed ambiguity set is a change too
        let amb_a =
            PreviewResolution::Ambiguous(names(&["STRIH-A (interkom)", "STRIH-B (interkom)"]));
        let amb_b =
            PreviewResolution::Ambiguous(names(&["STRIH-A (interkom)", "STRIH-C (interkom)"]));
        assert_eq!(
            preview_log_level(Some(&amb_a), &amb_b, false, true),
            Some(PreviewLogLevel::Warn)
        );
        assert_eq!(preview_log_level(Some(&amb_b), &amb_b, false, true), None);
    }

    #[test]
    fn preferred_connects_immediately_even_mid_window() {
        let r = PreviewResolution::Preferred("STRIH-LX (interkom)".to_string());
        assert_eq!(
            pick_preview_source(&r, false),
            Some("STRIH-LX (interkom)".to_string())
        );
        assert_eq!(
            pick_preview_source(&r, true),
            Some("STRIH-LX (interkom)".to_string())
        );
    }

    #[test]
    fn fallback_waits_for_the_whole_find_window() {
        // mDNS announces arrive in arbitrary order: a sibling strih must never pre-empt a
        // preferred box that simply has not been announced yet in the first second.
        let r = PreviewResolution::Fallback("STRIH-PP (interkom)".to_string());
        assert_eq!(pick_preview_source(&r, false), None);
        assert_eq!(
            pick_preview_source(&r, true),
            Some("STRIH-PP (interkom)".to_string())
        );
    }

    #[test]
    fn ambiguous_and_not_found_never_pick() {
        let amb =
            PreviewResolution::Ambiguous(names(&["STRIH-PP (interkom)", "STRIH-SNV (interkom)"]));
        assert_eq!(pick_preview_source(&amb, false), None);
        assert_eq!(pick_preview_source(&amb, true), None);
        assert_eq!(
            pick_preview_source(&PreviewResolution::NotFound, true),
            None
        );
    }

    #[test]
    fn log_lines_name_the_resolved_source_and_the_ambiguous_candidates() {
        let p = "STRIH-SNV (interkom)";
        assert_eq!(
            preview_log_line(
                &PreviewResolution::Fallback("STRIH-LX (interkom)".to_string()),
                p
            ),
            "NDI display: preview source resolved to 'STRIH-LX (interkom)' (#1362)"
        );
        assert_eq!(
            preview_log_line(&PreviewResolution::Preferred(p.to_string()), p),
            "NDI display: preview source resolved to 'STRIH-SNV (interkom)' (#1362)"
        );
        let amb = preview_log_line(
            &PreviewResolution::Ambiguous(names(&["STRIH-LX (interkom)", "STRIH-PP (interkom)"])),
            p,
        );
        assert!(
            amb.contains("'STRIH-LX (interkom)', 'STRIH-PP (interkom)'"),
            "{amb}"
        );
        assert!(amb.contains("waiting for 'STRIH-SNV (interkom)'"), "{amb}");
        let none = preview_log_line(&PreviewResolution::NotFound, p);
        assert!(
            none.contains("'STRIH-SNV (interkom)'") && none.contains("(#1362)"),
            "{none}"
        );
    }
}
