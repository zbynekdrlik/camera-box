//! #1362 — resolve the cameraman HDMI preview source from what is actually on the LAN.
//!
//! Every cambox previews the strih box's `interkom` NDI output on its HDMI monitor (the #528
//! fleet-wide default — no per-box config, no env knob). That source used to be one baked box
//! name, and `NdiReceiver::connect` finds sources by name, so the strih hardware swap (the Windows
//! strih PC -> the Linux notebook on 20.9.2026) turned every cameraman monitor black until a new
//! binary shipped — and any future strih box (e.g. a `STRIH-PP`) or rename would again. The
//! issue-1414 rename (the Linux strih's NDI prefix to `STRIH-SNV`) relies on exactly this fallback.
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
//!
//! Issue 1431 (joint event: one DNS switch for the whole cambox): BEFORE all of the above, the
//! `STRIH-<box> (interkom)` output whose NDI address is the IP `strih.lan` resolves to (re-resolved
//! on every connect) is taken on sight. So the router's `strih.lan` record moves the preview with
//! the intercom, the shading and the clock. A name that does not resolve, or an IP no strih
//! interkom output is published from, leaves the rules above unchanged.

/// The preferred preview source every cambox asks for: the SNV strih box's interkom output
/// (strih-snv, `<role>-<venue>` since issue 1414). Any other `STRIH-<box> (interkom)` is still found
/// by the fallback in [`resolve_preview_source`] -- the Poprad strih, or the SNV strih while it
/// still runs its pre-1414 hostname -- so this only has to name the usual strih.
pub const DEFAULT_PREVIEW_SOURCE: &str = "STRIH-SNV (interkom)";

use std::net::IpAddr;

/// NDI host prefix every strih box publishes under (`STRIH-SNV`, `STRIH-PP`, ...).
pub const STRIH_HOST_PREFIX: &str = "STRIH-";

/// NDI output suffix of the strih interkom/return output (the full NDI name is `HOST (output)`).
pub const INTERKOM_OUTPUT_SUFFIX: &str = " (interkom)";

/// The DNS name of the venue strih every cambox service follows (issue 1431): dantesync's NTP
/// server, the intercom's VBAN target, the shading relay's registration and this preview.
pub const STRIH_DNS_NAME: &str = "strih.lan";

/// One discovered NDI source: its name and the address the finder reports for it
/// (`p_url_address`, `ip:port`), `None` when the SDK gave none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredSource {
    pub name: String,
    pub address: Option<String>,
}

/// Outcome of resolving the preview source against the discovered NDI source list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewResolution {
    /// The strih interkom output published from the IP `strih.lan` resolves to (issue 1431).
    Followed { name: String, strih_ip: IpAddr },
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

/// The IP of an NDI source address (`ip:port`, `[v6]:port` or a bare IP), else `None`.
pub fn source_ip(_address: &str) -> Option<IpAddr> {
    None
}

/// Issue 1431: the strih interkom output published from `strih_ip` wins on sight; otherwise the
/// name rules of [`resolve_preview_source`] apply unchanged.
pub fn resolve_followed_preview_source(
    discovered: &[DiscoveredSource],
    preferred: &str,
    _strih_ip: Option<IpAddr>,
) -> PreviewResolution {
    let names: Vec<String> = discovered.iter().map(|s| s.name.clone()).collect();
    resolve_preview_source(&names, preferred)
}

/// The journal line for the `strih.lan` resolution, only when it changed since `last_logged`
/// (`None` = never logged). Issue 1431.
pub fn strih_lan_log_line(
    _last_logged: Option<&Option<IpAddr>>,
    _now: Option<IpAddr>,
) -> Option<String> {
    None
}

/// Decide which exact name (if any) to connect to NOW. `window_elapsed` is `true` on the final
/// pass of the find window: the preferred name is taken on sight, the single fallback only once
/// the window elapsed (a late mDNS announce of the preferred box must win), ambiguity and
/// nothing-found never pick.
pub fn pick_preview_source(resolution: &PreviewResolution, window_elapsed: bool) -> Option<String> {
    match resolution {
        PreviewResolution::Followed { name, .. } | PreviewResolution::Preferred(name) => {
            Some(name.clone())
        }
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
        PreviewResolution::Followed { name, .. }
        | PreviewResolution::Preferred(name)
        | PreviewResolution::Fallback(name) => {
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

    /// The SNV LAN after the issue-1414 rename: strih-snv's five OBS outputs plus camera
    /// senders (the 23.9.2026 `avahi-browse -rtp _ndi._tcp` read with the box renamed).
    fn live_lan_strih_snv() -> Vec<String> {
        names(&[
            "CAM1 (usb)",
            "CAM3 (usb)",
            "STRIH-SNV (2ME PGM)",
            "STRIH-SNV (2ME PVW)",
            "STRIH-SNV (Grading)",
            "STRIH-SNV (interkom)",
            "STRIH-SNV (MULTIVIEW)",
            "STREAM-SNV (PGM)",
        ])
    }

    /// The same LAN while the strih box still runs its pre-1414 hostname (the transition).
    fn lan_before_the_strih_rename() -> Vec<String> {
        names(&[
            "CAM1 (usb)",
            "STRIH-LX (2ME PGM)", // issue 1414 transition: the pre-rename hostname
            "STRIH-LX (interkom)", // issue 1414 transition: the pre-rename hostname
            "STREAM-SNV (PGM)",
        ])
    }

    #[test]
    fn default_preview_source_is_the_strih_snv_interkom_output_1414() {
        assert_eq!(DEFAULT_PREVIEW_SOURCE, "STRIH-SNV (interkom)");
        assert!(is_strih_interkom(DEFAULT_PREVIEW_SOURCE));
    }

    #[test]
    fn preferred_name_on_the_lan_is_used() {
        assert_eq!(
            resolve_preview_source(&live_lan_strih_snv(), DEFAULT_PREVIEW_SOURCE),
            PreviewResolution::Preferred("STRIH-SNV (interkom)".to_string())
        );
    }

    #[test]
    fn the_old_named_strih_is_still_picked_after_the_window_1414() {
        // A new cambox binary during the roll, the strih box not yet renamed: the preferred
        // STRIH-SNV name is absent, the ONE strih interkom output is the old-named one, and it is
        // taken on the final pass of the find window, never mid-window.
        let old = "STRIH-LX (interkom)"; // issue 1414 transition: the pre-rename hostname
        let r = resolve_preview_source(&lan_before_the_strih_rename(), DEFAULT_PREVIEW_SOURCE);
        assert_eq!(r, PreviewResolution::Fallback(old.to_string()));
        assert_eq!(pick_preview_source(&r, false), None);
        assert_eq!(pick_preview_source(&r, true), Some(old.to_string()));
    }

    #[test]
    fn an_old_binary_finds_the_renamed_strih_after_the_window_1414() {
        // An old cambox binary (preferred = the pre-rename name) on the renamed LAN: the ONE strih
        // interkom output is STRIH-SNV, taken after the window, so no monitor goes dark.
        let old_preferred = "STRIH-LX (interkom)"; // issue 1414 transition: an old binary
        let r = resolve_preview_source(&live_lan_strih_snv(), old_preferred);
        assert_eq!(
            r,
            PreviewResolution::Fallback("STRIH-SNV (interkom)".to_string())
        );
        assert_eq!(pick_preview_source(&r, false), None);
        assert_eq!(
            pick_preview_source(&r, true),
            Some("STRIH-SNV (interkom)".to_string())
        );
    }

    #[test]
    fn the_reported_bug_old_preferred_name_falls_back_to_the_single_strih_interkom() {
        // The issue-1362 fleet state: the box asks for a strih that is gone, the LAN has one
        // other strih -- the monitor must show its interkom output, not stay black.
        assert_eq!(
            resolve_preview_source(&live_lan_strih_snv(), "STRIH-GONE (interkom)"),
            PreviewResolution::Fallback("STRIH-SNV (interkom)".to_string())
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
            resolve_preview_source(&lan, "STRIH-GONE (interkom)"),
            PreviewResolution::Ambiguous(names(&["STRIH-PP (interkom)", "STRIH-SNV (interkom)"]))
        );
    }

    #[test]
    fn preferred_wins_over_other_strih_interkom_outputs() {
        let lan = names(&[
            "STRIH-PP (interkom)",
            "STRIH-SNV (interkom)",
            "STRIH-GONE (interkom)",
        ]);
        assert_eq!(
            resolve_preview_source(&lan, DEFAULT_PREVIEW_SOURCE),
            PreviewResolution::Preferred("STRIH-SNV (interkom)".to_string())
        );
    }

    #[test]
    fn a_duplicate_listing_of_one_source_is_not_ambiguity() {
        let lan = names(&["STRIH-SNV (interkom)", "STRIH-SNV (interkom)"]);
        assert_eq!(
            resolve_preview_source(&lan, "STRIH-GONE (interkom)"),
            PreviewResolution::Fallback("STRIH-SNV (interkom)".to_string())
        );
    }

    #[test]
    fn nothing_matching_is_not_found() {
        assert_eq!(
            resolve_preview_source(&[], DEFAULT_PREVIEW_SOURCE),
            PreviewResolution::NotFound
        );
        let lan = names(&["STRIH-SNV (2ME PGM)", "STRIH-SNV (MULTIVIEW)", "CAM1 (usb)"]);
        assert_eq!(
            resolve_preview_source(&lan, DEFAULT_PREVIEW_SOURCE),
            PreviewResolution::NotFound
        );
    }

    #[test]
    fn strih_interkom_matching_is_exact_on_prefix_suffix_and_case() {
        assert!(is_strih_interkom("STRIH-SNV (interkom)"));
        assert!(is_strih_interkom("STRIH-PP (interkom)"));
        assert!(is_strih_interkom("STRIH-GONE (interkom)"));
        // not a strih interkom output:
        assert!(!is_strih_interkom("STRIH-SNV (2ME PGM)"));
        assert!(!is_strih_interkom("STREAM-SNV (interkom)"));
        assert!(!is_strih_interkom("XSTRIH-SNV (interkom)"));
        assert!(!is_strih_interkom("strih-snv (interkom)"));
        assert!(!is_strih_interkom("STRIH-SNV (Interkom)"));
        assert!(!is_strih_interkom("STRIH-SNV (interkom) 2"));
        assert!(!is_strih_interkom("STRIH- (interkom)"));
        assert!(!is_strih_interkom("STRIH-SNV(interkom)"));
        assert!(!is_strih_interkom(""));
        // a parenthesised host part is not a strih box name:
        assert!(!is_strih_interkom("STRIH-A (x) (interkom)"));
        assert!(!is_strih_interkom("STRIH-A) (interkom)"));
    }

    #[test]
    fn a_decision_is_logged_once_per_change_and_only_when_final() {
        let preferred = PreviewResolution::Preferred("STRIH-SNV (interkom)".to_string());
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
        let r = PreviewResolution::Preferred("STRIH-SNV (interkom)".to_string());
        assert_eq!(
            pick_preview_source(&r, false),
            Some("STRIH-SNV (interkom)".to_string())
        );
        assert_eq!(
            pick_preview_source(&r, true),
            Some("STRIH-SNV (interkom)".to_string())
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
                &PreviewResolution::Fallback("STRIH-PP (interkom)".to_string()),
                p
            ),
            "NDI display: preview source resolved to 'STRIH-PP (interkom)' (#1362)"
        );
        assert_eq!(
            preview_log_line(&PreviewResolution::Preferred(p.to_string()), p),
            "NDI display: preview source resolved to 'STRIH-SNV (interkom)' (#1362)"
        );
        let amb = preview_log_line(
            &PreviewResolution::Ambiguous(names(&["STRIH-GONE (interkom)", "STRIH-PP (interkom)"])),
            p,
        );
        assert!(
            amb.contains("'STRIH-GONE (interkom)', 'STRIH-PP (interkom)'"),
            "{amb}"
        );
        assert!(amb.contains("waiting for 'STRIH-SNV (interkom)'"), "{amb}");
        let none = preview_log_line(&PreviewResolution::NotFound, p);
        assert!(
            none.contains("'STRIH-SNV (interkom)'") && none.contains("(#1362)"),
            "{none}"
        );
    }

    // --- issue 1431: the strih behind strih.lan -------------------------------------------

    fn src(name: &str, address: Option<&str>) -> DiscoveredSource {
        DiscoveredSource {
            name: name.to_string(),
            address: address.map(str::to_string),
        }
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    /// A joint event: both strih boxes on one network, strih-snv at .9.202, strih-pp at .8.202.
    fn joint_lan() -> Vec<DiscoveredSource> {
        vec![
            src("CAM1 (usb)", Some("10.77.9.61:5961")),
            src("STRIH-SNV (2ME PGM)", Some("10.77.9.202:5961")),
            src("STRIH-SNV (interkom)", Some("10.77.9.202:5963")),
            src("STRIH-PP (interkom)", Some("10.77.8.202:5962")),
            src("STRIH-PP (2ME PGM)", Some("10.77.8.202:5961")),
        ]
    }

    #[test]
    fn source_ip_reads_ip_port_v6_and_bare_addresses_1431() {
        assert_eq!(source_ip("10.77.9.202:5961"), Some(ip("10.77.9.202")));
        assert_eq!(source_ip("10.77.9.202"), Some(ip("10.77.9.202")));
        assert_eq!(source_ip("[fe80::1]:5961"), Some(ip("fe80::1")));
        assert_eq!(source_ip(" 10.77.8.202:5962 "), Some(ip("10.77.8.202")));
        assert_eq!(source_ip(""), None);
        assert_eq!(source_ip("strih-snv.lan:5961"), None);
        assert_eq!(source_ip("10.77.9:5961"), None);
    }

    #[test]
    fn the_strih_behind_strih_lan_wins_over_the_preferred_name_1431() {
        // strih.lan points at strih-pp: a SNV cambox follows it although STRIH-SNV is announced.
        let r = resolve_followed_preview_source(
            &joint_lan(),
            DEFAULT_PREVIEW_SOURCE,
            Some(ip("10.77.8.202")),
        );
        assert_eq!(
            r,
            PreviewResolution::Followed {
                name: "STRIH-PP (interkom)".to_string(),
                strih_ip: ip("10.77.8.202"),
            }
        );
        // taken on sight, mid-window too
        assert_eq!(
            pick_preview_source(&r, false),
            Some("STRIH-PP (interkom)".to_string())
        );
        // and strih.lan at strih-snv picks SNV
        assert_eq!(
            resolve_followed_preview_source(
                &joint_lan(),
                DEFAULT_PREVIEW_SOURCE,
                Some(ip("10.77.9.202"))
            ),
            PreviewResolution::Followed {
                name: "STRIH-SNV (interkom)".to_string(),
                strih_ip: ip("10.77.9.202"),
            }
        );
    }

    #[test]
    fn only_a_strih_interkom_output_at_that_ip_is_followed_1431() {
        // The PGM output and a camera at the strih IP never become the preview.
        let lan = vec![
            src("STRIH-PP (2ME PGM)", Some("10.77.8.202:5961")),
            src("CAM9 (usb)", Some("10.77.8.202:5964")),
            src("STRIH-SNV (interkom)", Some("10.77.9.202:5963")),
        ];
        assert_eq!(
            resolve_followed_preview_source(&lan, DEFAULT_PREVIEW_SOURCE, Some(ip("10.77.8.202"))),
            PreviewResolution::Preferred("STRIH-SNV (interkom)".to_string())
        );
    }

    #[test]
    fn unresolved_or_unmatched_strih_lan_keeps_todays_rules_1431() {
        let lan = joint_lan();
        let names: Vec<String> = lan.iter().map(|s| s.name.clone()).collect();
        let today = resolve_preview_source(&names, DEFAULT_PREVIEW_SOURCE);
        assert_eq!(
            resolve_followed_preview_source(&lan, DEFAULT_PREVIEW_SOURCE, None),
            today
        );
        assert_eq!(
            resolve_followed_preview_source(&lan, DEFAULT_PREVIEW_SOURCE, Some(ip("10.77.9.1"))),
            today
        );
        // a source with no address never matches
        let no_addr = vec![src("STRIH-PP (interkom)", None)];
        assert_eq!(
            resolve_followed_preview_source(
                &no_addr,
                DEFAULT_PREVIEW_SOURCE,
                Some(ip("10.77.8.202"))
            ),
            PreviewResolution::Fallback("STRIH-PP (interkom)".to_string())
        );
    }

    #[test]
    fn two_interkom_outputs_at_the_strih_ip_pick_the_first_by_name_1431() {
        let lan = vec![
            src("STRIH-PP2 (interkom)", Some("10.77.8.202:5964")),
            src("STRIH-PP (interkom)", Some("10.77.8.202:5962")),
        ];
        assert_eq!(
            resolve_followed_preview_source(&lan, DEFAULT_PREVIEW_SOURCE, Some(ip("10.77.8.202"))),
            PreviewResolution::Followed {
                name: "STRIH-PP (interkom)".to_string(),
                strih_ip: ip("10.77.8.202"),
            }
        );
    }

    #[test]
    fn a_followed_resolution_is_logged_with_the_strih_lan_ip_once_per_change_1431() {
        let a = PreviewResolution::Followed {
            name: "STRIH-PP (interkom)".to_string(),
            strih_ip: ip("10.77.8.202"),
        };
        assert_eq!(
            preview_log_line(&a, DEFAULT_PREVIEW_SOURCE),
            "NDI display: preview source resolved to 'STRIH-PP (interkom)', the strih behind \
             strih.lan (10.77.8.202) (#1431)"
        );
        assert_eq!(
            preview_log_level(None, &a, true, false),
            Some(PreviewLogLevel::Info)
        );
        assert_eq!(preview_log_level(Some(&a), &a, true, false), None);
        // the same name reached by the old preferred-name rule is a change, logged again
        let p = PreviewResolution::Preferred("STRIH-PP (interkom)".to_string());
        assert_eq!(
            preview_log_level(Some(&a), &p, true, false),
            Some(PreviewLogLevel::Info)
        );
    }

    #[test]
    fn the_strih_lan_resolution_is_logged_once_per_change_1431() {
        let snv = Some(ip("10.77.9.202"));
        let pp = Some(ip("10.77.8.202"));
        assert_eq!(
            strih_lan_log_line(None, snv).as_deref(),
            Some(
                "NDI display: strih.lan resolves to 10.77.9.202 -- the preview follows the strih \
                 interkom output published from it (#1431)"
            )
        );
        assert_eq!(strih_lan_log_line(Some(&snv), snv), None);
        assert_eq!(
            strih_lan_log_line(Some(&snv), pp).as_deref(),
            Some(
                "NDI display: strih.lan resolves to 10.77.8.202 -- the preview follows the strih \
                 interkom output published from it (#1431)"
            )
        );
        assert_eq!(
            strih_lan_log_line(Some(&pp), None).as_deref(),
            Some(
                "NDI display: strih.lan does not resolve -- the preview keeps the strih name \
                 rules (#1431)"
            )
        );
        assert_eq!(strih_lan_log_line(Some(&None), None), None);
    }
}
