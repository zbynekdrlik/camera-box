//! #89 — pure DXGI device-lost (GPU TDR / driver-internal-error) log-signature matcher.
//!
//! Extracted from `probe::obs_log_audit` (#81) to a CRATE-ROOT pure module (no `probe` feature
//! deps) so the default-feature watchdog/self-heal detection pipeline (`obs_watchdog`,
//! `obs_self_heal` — Tier-0, no `probe` feature) can share the EXACT SAME signature match that
//! `probe::obs_log_audit::audit_obs_log` already uses for the #81 harness-side dead-GPU
//! diagnosis, never a second independently-drifting copy of the DXGI codes (mirrors the
//! `reannounce.rs` / `colour_scale.rs` "pure seam at crate root" pattern documented in
//! CLAUDE.md's Local Build Policy section).
//!
//! The signature: when a GPU device is removed/reset (a TDR) or the driver hits an internal
//! error, OBS logs one or both of:
//!   `device_texture_create (D3D11): Failed to create 2D texture (887A0005)`
//!   `  Device Removed Reason: 887A0007`
//! 887A0005 = DXGI_ERROR_DEVICE_REMOVED, 887A0006 = DXGI_ERROR_DRIVER_INTERNAL_ERROR,
//! 887A0007 = DXGI_ERROR_DEVICE_RESET. Once it fires OBS cannot create textures, so the
//! compositor emits nothing and the NDI Main Output black-holes (#81's stream.lan incident: this
//! signature appeared 6071× and OBS never recovered without a full PC reboot).
//!
//! Pure `&str` scanning, no deps — unit-tests on default features (Tier-0).

/// The DXGI device-loss codes treated as a dead-GPU signature. Both forms appear in the wedged
/// stream log: the texture-create failure code (887A0005) and the device-removed reason code
/// (887A0007); 887A0006 (a driver-internal-error, distinct from a classic TDR hang) is included
/// too since either form black-holes the output — seeing any of them is a dead-GPU diagnosis.
pub const DXGI_DEVICE_LOST_CODES: &[&str] = &[
    "887A0005", // DXGI_ERROR_DEVICE_REMOVED
    "887A0006", // DXGI_ERROR_DRIVER_INTERNAL_ERROR
    "887A0007", // DXGI_ERROR_DEVICE_RESET
];

/// True when a log line carries a DXGI device-lost signature. Matches the OBS
/// texture-create-failure line, the device-removed-reason line, and any other line quoting one
/// of the device-lost codes — so a build that logs only one of the two forms is still caught.
pub fn line_is_device_lost(line: &str) -> bool {
    DXGI_DEVICE_LOST_CODES.iter().any(|c| line.contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texture_create_failure_line_matches() {
        assert!(line_is_device_lost(
            "03:33:39.533: device_texture_create (D3D11): Failed to create 2D texture (887A0005)"
        ));
    }

    #[test]
    fn device_removed_reason_line_matches() {
        assert!(line_is_device_lost(
            "03:33:39.533:   Device Removed Reason: 887A0007"
        ));
    }

    #[test]
    fn driver_internal_error_code_matches() {
        assert!(line_is_device_lost(
            "some line mentioning 887A0006 in passing"
        ));
    }

    #[test]
    fn clean_line_does_not_match() {
        assert!(!line_is_device_lost(
            "00:02:02.000: genlock: render tick ENABLED"
        ));
    }

    #[test]
    fn empty_line_does_not_match() {
        assert!(!line_is_device_lost(""));
    }

    #[test]
    fn all_three_codes_are_exactly_the_dxgi_device_lost_set() {
        // Pin the exact set — a change here is a deliberate, reviewed change to what counts as
        // a dead-GPU signature, never an accidental drift.
        assert_eq!(
            DXGI_DEVICE_LOST_CODES,
            &["887A0005", "887A0006", "887A0007"]
        );
    }
}
