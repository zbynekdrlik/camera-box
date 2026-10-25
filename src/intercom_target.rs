//! #1345 M1b — appliance-side intercom-target env override.
//!
//! The dev cambox root filesystem is READ-ONLY, so `/etc/camera-box/config.toml` cannot be edited to
//! repoint the VBAN intercom at the new Linux strih-snv hub. camera-box therefore honours a
//! `CAMERA_BOX_INTERCOM_TARGET` env override (precedence env > CLI flag > config.toml), carried by a
//! `/run` systemd drop-in written by `scripts/lib/intercom-target-dropin.sh` (tmpfs, so a reboot
//! auto-reverts to the deployed Windows-strih target). Only the DEV cambox cam1 is repointed for the
//! M1 loopback test; cam2-7 stay on the Windows strih until the M4 cut-over.
//!
//! Pure std logic (no env read, no I/O) — the caller passes the raw env value in — so it is Tier-0
//! unit-testable (runs on the Linux `test` CI job, default features).

/// The environment variable that overrides the resolved (CLI/config) intercom target host.
pub const INTERCOM_TARGET_ENV: &str = "CAMERA_BOX_INTERCOM_TARGET";

/// Apply the [`INTERCOM_TARGET_ENV`] override to an already-resolved intercom target host.
///
/// `env_value` is the raw env var (e.g. `std::env::var(INTERCOM_TARGET_ENV).ok().as_deref()`), and
/// `resolved` is the host already chosen by CLI flag > config.toml. Returns the effective target
/// host, plus `Some(note)` (an `info!`-worthy line naming the env var and BOTH the new and the old
/// host) when the env actually overrode it. A `None` / empty / whitespace-only env value leaves
/// `resolved` untouched and yields no note.
pub fn resolve_intercom_target(
    env_value: Option<&str>,
    resolved: &str,
) -> (String, Option<String>) {
    match env_value {
        Some(raw) if !raw.trim().is_empty() => {
            let host = raw.trim();
            let note = format!(
                "intercom target override: {INTERCOM_TARGET_ENV}={host} (config/CLI said {resolved})"
            );
            (host.to_string(), Some(note))
        }
        _ => (resolved.to_string(), None),
    }
}

/// Issue 1482 (owner ROZHODNUTÉ 25.10.2026): in the SNV rig's DIRECT mode (cam2's HDMI straight into
/// the splitter, no camera) cam2's USB jack carries the QPSK measurement marker, so cam2 runs NO intercom
/// there. rig-mode TEST writes `CAMERA_BOX_INTERCOM=off` into its `/run` camera-box drop-in; EVENT
/// removes the drop-in and the intercom comes back from config.toml.
pub const INTERCOM_SWITCH_ENV: &str = "CAMERA_BOX_INTERCOM";

/// Issue 1482: true when the [`INTERCOM_SWITCH_ENV`] value turns the intercom OFF (`off`, any case,
/// surrounding whitespace ignored). Any other value, or none, leaves the configured intercom alone.
pub fn intercom_switched_off(env_value: Option<&str>) -> bool {
    matches!(env_value, Some(v) if v.trim().eq_ignore_ascii_case("off"))
}

#[cfg(test)]
mod switch_tests {
    use super::*;

    #[test]
    fn off_in_any_case_switches_the_intercom_off_1482() {
        for v in ["off", "OFF", " Off\n"] {
            assert!(intercom_switched_off(Some(v)), "{v:?}");
        }
    }

    #[test]
    fn anything_else_keeps_the_configured_intercom_1482() {
        for v in [
            None,
            Some(""),
            Some("on"),
            Some("0"),
            Some("offline"),
            Some("no"),
        ] {
            assert!(!intercom_switched_off(v), "{v:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_wins_and_trims_and_notes_both_hosts() {
        let (target, note) = resolve_intercom_target(Some("  strih-snv.lan \n"), "strih.lan");
        assert_eq!(target, "strih-snv.lan");
        let note = note.expect("override note");
        assert!(note.contains(INTERCOM_TARGET_ENV));
        assert!(note.contains("strih-snv.lan"));
        assert!(note.contains("strih.lan"));
    }

    #[test]
    fn none_and_blank_keep_resolved_without_a_note() {
        assert_eq!(
            resolve_intercom_target(None, "strih.lan"),
            ("strih.lan".to_string(), None)
        );
        assert_eq!(
            resolve_intercom_target(Some(""), "strih.lan"),
            ("strih.lan".to_string(), None)
        );
        assert_eq!(
            resolve_intercom_target(Some("   \t"), "strih.lan"),
            ("strih.lan".to_string(), None)
        );
    }
}
