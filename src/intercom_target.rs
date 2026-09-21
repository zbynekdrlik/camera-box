//! #1345 M1b — appliance-side intercom-target env override.
//!
//! The dev cambox root filesystem is READ-ONLY, so `/etc/camera-box/config.toml` cannot be edited to
//! repoint the VBAN intercom at the new Linux strih-lx hub. camera-box therefore honours a
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_wins_and_trims_and_notes_both_hosts() {
        let (target, note) = resolve_intercom_target(Some("  strih-lx.lan \n"), "strih.lan");
        assert_eq!(target, "strih-lx.lan");
        let note = note.expect("override note");
        assert!(note.contains(INTERCOM_TARGET_ENV));
        assert!(note.contains("strih-lx.lan"));
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
