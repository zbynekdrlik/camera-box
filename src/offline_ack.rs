//! #855 — parse the operator's offline ack (`CAMBOX_OFFLINE_ACK` / `rig-fleet.txt`,
//! `scripts/lib/cambox-offline-ack.sh`) on the RUST side, so `recording-verdict`'s per-camera
//! A/V-sync gate (`all_cambox_av_sync`) can report an acked-offline box as EXCLUDED instead of
//! judging it UNKNOWN/FAIL on the zero samples it was never going to produce.
//!
//! ## Why this lives at the crate root (default features), not in `probe`
//!
//! `src/bin/recording-verdict.rs` and everything under `src/probe/` are
//! `#[cfg(feature = "probe")]`-gated, which the project's Tier-0 local-build policy BANS
//! compiling/testing locally (see CLAUDE.md's Local Build Policy). This module is the PURE
//! parsing seam — the same pattern as `src/reannounce.rs` / `src/colour_scale.rs` — so it unit
//! tests on default features (Tier-0 testable), while `recording-verdict.rs` stays thin glue
//! that just calls [`parse`] and looks a camera name up in the returned map.
//!
//! ## Format
//!
//! Exactly the shell-side `CAMBOX_OFFLINE_ACK` format
//! (`scripts/lib/cambox-offline-ack.sh::cambox_offline_ack_reason`): a comma-separated list of
//! `box:reason` pairs, e.g. `"cam5:powered-off-2026-07-27,cam6:powered-off-2026-07-27"`. A bare
//! box name with no `:reason` part is accepted with reason `"unspecified"` — mirrors the shell
//! function's own fallback exactly, so the SAME config string threads through unchanged whether
//! it is consumed by the shell preflight or this Rust parser.

use std::collections::HashMap;

/// Parse a `CAMBOX_OFFLINE_ACK`-format string into a box-name -> reason map. Empty/blank input
/// (and any blank entries within it) produce an empty map for those entries — never an error;
/// an absent ack is simply "nothing acked", exactly like the shell side.
///
/// Case-sensitive EXACT box-name match downstream (this function just builds the map — the
/// caller does a plain `HashMap::get`, never a substring match), matching
/// `cambox_offline_ack_reason`'s own documented contract ("cam7" must never match "cam70").
pub fn parse(_ack: &str) -> HashMap<String, String> {
    // #855 RED: not implemented yet -- every test below must fail (panic), not silently pass.
    todo!("#855: offline_ack::parse not implemented yet")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_is_empty_map_855() {
        assert!(parse("").is_empty(), "no ack configured -> nothing acked");
    }

    #[test]
    fn single_box_reason_pair_855() {
        let m = parse("cam7:vmount-battery-discharged-2026-07-14");
        assert_eq!(
            m.get("cam7").map(String::as_str),
            Some("vmount-battery-discharged-2026-07-14")
        );
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn multiple_box_reason_pairs_855() {
        let m = parse("cam5:powered-off-2026-07-27,cam6:powered-off-2026-07-27,cam7:powered-off-2026-07-27");
        assert_eq!(m.len(), 3);
        for cam in ["cam5", "cam6", "cam7"] {
            assert_eq!(m.get(cam).map(String::as_str), Some("powered-off-2026-07-27"));
        }
    }

    #[test]
    fn bare_box_name_with_no_reason_gets_unspecified_855() {
        let m = parse("cam7");
        assert_eq!(m.get("cam7").map(String::as_str), Some("unspecified"));
    }

    #[test]
    fn a_box_not_named_in_the_ack_is_not_in_the_map_855() {
        let m = parse("cam5:powered-off-2026-07-27");
        assert!(
            !m.contains_key("cam4"),
            "cam4 was never named in the ack -> must not appear in the map, \
             so a caller's HashMap::get must never confuse it with an acked box: {m:?}"
        );
    }

    #[test]
    fn exact_match_never_a_substring_match_855() {
        // #758's own documented contract, restated here: "cam7" must never match "cam70" or a
        // "camera7" typo. HashMap::get is already exact-match by construction, but this pins the
        // parsed KEY itself is the box name verbatim, not a prefix/pattern.
        let m = parse("cam7:reason");
        assert!(!m.contains_key("cam70"));
        assert!(!m.contains_key("camera7"));
        assert!(m.contains_key("cam7"));
    }

    #[test]
    fn whitespace_around_entries_and_pairs_is_trimmed_855() {
        let m = parse(" cam5 : powered-off , cam6:reason2 ");
        assert_eq!(m.get("cam5").map(String::as_str), Some("powered-off"));
        assert_eq!(m.get("cam6").map(String::as_str), Some("reason2"));
    }
}
