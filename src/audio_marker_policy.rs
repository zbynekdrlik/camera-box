//! #984 — the QPSK A/V-sync audio marker's default-enable policy AND the pure ALSA
//! device-resolution decision, extracted out of `frame-probe.rs`'s CLI wiring so both are
//! unit-testable Tier-0 (default features, no probe deps).
//!
//! ## Why this lives at the crate root (default features), not in `probe`
//!
//! `src/bin/frame-probe.rs` has `required-features = ["probe"]` in `Cargo.toml` — default-feature
//! `cargo check`/`clippy`/`test` never even attempt to compile it, so a change confined there has
//! ZERO local verification path until CI runs (see the project CLAUDE.md's Local Build Policy
//! section). This module is the PURE decision seam — the same pattern as `src/colour_scale.rs` /
//! `src/reannounce.rs` / `src/motion_sweep.rs`: no I/O, no probe deps, so it unit-tests Tier-0.
//! `frame-probe.rs` becomes a thin caller of these functions.
//!
//! ## The bug this closes (issue 984)
//!
//! `frame-probe --paint-only`'s QPSK marker used to be OPT-IN (`--audio-marker`, default false) —
//! only a caller that passed the flag explicitly (`scripts/rig-mode.sh`'s TEST-mode launch) got
//! audio. `systemd/cam2-painter.service` (the PERMANENT cam2 painter, issue 863) never passes it,
//! so the permanent unit painted QR forever and emitted no marker, ever — the rig looked alive
//! (QR on screen) but was silently dead audio-wise. [`audio_marker_default_enabled`] makes the
//! marker default ON whenever `--paint-only` is set — the SAME `.unwrap_or(paint_only)` shape
//! `colour_scale`/`motion_sweep` already use (issue 367 / issue 751 precedent, also the exact
//! precedent `scripts/setup-device.sh:148`'s comment cites) — so a launcher that adds no flags at
//! all still gets audio; an explicit `--audio-marker[=bool]` always overrides.
//!
//! The device string itself must never be a dead hardcoded pin either (issue 725 already fixed
//! this class for the explicit TEST-mode path: any HDMI renegotiation — a reboot, a cable reseat —
//! can move which `DEV=N` the physical monitor lands on). [`parse_aplay_hdmi_devices`] /
//! [`resolve_marker_device_from_aplay`] are a line-for-line Rust mirror of
//! `scripts/lib/marker-device-resolve.sh`'s `_marker_device_parse_entries` /
//! `marker_device_resolve_from_aplay` — the SAME "the bracketed monitor name differs from its own
//! `HDMI N` slot label" decision — so the bash TEST-mode resolution path and this new Rust
//! default-resolution path make the IDENTICAL decision from the SAME `aplay -l` text.

/// The pinned last-resort fallback ALSA device — used only when NO HDMI playback device in a live
/// `aplay -l` listing carries a genuine (non-generic) monitor name. Matches
/// `scripts/rig-mode.sh`'s `AUDIO_MARKER_DEVICE` default and
/// `scripts/lib/marker-device-resolve.sh`'s identical fallback.
pub const FALLBACK_MARKER_DEVICE: &str = "hw:CARD=PCH,DEV=3";

/// #984: `--paint-only` must emit the QPSK marker BY DEFAULT (hard-locked the way genlock was
/// locked in issue 257) — an explicit `--audio-marker[=bool]` on the CLI always overrides this.
/// Mirrors the existing `colour_scale`/`motion_sweep` `.unwrap_or(paint_only)` default-enable
/// shape in `src/bin/frame-probe.rs`. A run that is not `--paint-only` (the Phase-1 loopback /
/// synth-ndi paths) never has real hardware audio to emit, so it stays off regardless.
pub fn audio_marker_default_enabled(paint_only: bool) -> bool {
    // #984 RED: stub reproducing TODAY's real bug (opt-in only, never default-on) --
    // replaced with the real `paint_only` policy in the GREEN commit.
    let _ = paint_only;
    false
}

/// One HDMI playback device parsed out of a live `aplay -l` transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AplayHdmiDevice {
    pub card: String,
    pub dev: String,
    /// True when the bracketed monitor name differs from the device's own generic `HDMI N`
    /// slot label — i.e. a real, negotiated monitor is attached (mirrors the bash
    /// `has_monitor` computation in `scripts/lib/marker-device-resolve.sh`).
    pub has_monitor: bool,
    pub monitor_name: String,
}

/// Parse one `aplay -l` line of the shape
/// `card 0: PCH [HDA Intel PCH], device 3: HDMI 0 [BenQ GL2480]` into its
/// [`AplayHdmiDevice`]. Returns `None` for any line that does not match this exact HDMI-device
/// shape (a non-HDMI card, a USB audio card, a header/blank line, ...) — mirrors the bash
/// regex `^card\ [0-9]+:\ ([A-Za-z0-9_]+)\ \[.*\],\ device\ ([0-9]+):\ (HDMI\ [0-9]+)\ \[(.*)\]$`
/// field-for-field (no `regex` crate dependency — plain, checked string slicing).
fn parse_aplay_hdmi_line(line: &str) -> Option<AplayHdmiDevice> {
    // #984 RED: stub reproducing TODAY's real bug (no live device-resolution logic exists at
    // all) -- replaced with the real parser in the GREEN commit.
    let _ = line;
    None
}

/// Parse a full `aplay -l` transcript into every HDMI playback device it lists — pure text
/// parsing, no I/O. Non-HDMI lines (other cards, headers, blank lines) are silently skipped, in
/// listing order (matches `_marker_device_parse_entries`'s line-by-line behaviour).
pub fn parse_aplay_hdmi_devices(aplay_text: &str) -> Vec<AplayHdmiDevice> {
    aplay_text
        .lines()
        .filter_map(parse_aplay_hdmi_line)
        .collect()
}

/// Resolve the QPSK marker's ALSA device from a live `aplay -l` transcript: the FIRST HDMI
/// playback device (in `aplay -l`'s own listing order — deterministic, never ambiguous) that
/// carries a genuine (non-generic) monitor name, as `"hw:CARD=<name>,DEV=<n>"`. Returns `None`
/// when no device in the transcript carries a real monitor — the caller MUST treat that as a
/// resolution failure and fall back to [`FALLBACK_MARKER_DEVICE`] (never silently pick a dead
/// pin). Mirrors `marker_device_resolve_from_aplay` in
/// `scripts/lib/marker-device-resolve.sh` exactly.
pub fn resolve_marker_device_from_aplay(aplay_text: &str) -> Option<String> {
    parse_aplay_hdmi_devices(aplay_text)
        .into_iter()
        .find(|d| d.has_monitor)
        .map(|d| format!("hw:CARD={},DEV={}", d.card, d.dev))
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- audio_marker_default_enabled ---------------------------------------------------------

    #[test]
    fn paint_only_true_defaults_the_marker_on() {
        assert!(audio_marker_default_enabled(true));
    }

    #[test]
    fn paint_only_false_defaults_the_marker_off() {
        assert!(!audio_marker_default_enabled(false));
    }

    // --- parse_aplay_hdmi_devices / resolve_marker_device_from_aplay -------------------------

    // The exact live example from scripts/lib/marker-device-resolve.sh's own header comment:
    // a genuine BenQ monitor negotiated on card PCH device 3, and a second HDMI device with no
    // monitor attached (its bracketed name equals its own slot label).
    const APLAY_L_WITH_MONITOR: &str = "\
**** List of PLAYBACK Hardware Devices ****
card 0: PCH [HDA Intel PCH], device 3: HDMI 0 [BenQ GL2480]
card 0: PCH [HDA Intel PCH], device 7: HDMI 1 [HDMI 1]
card 1: CARD [USB Audio], device 0: USB Audio [USB Audio]
";

    const APLAY_L_NO_MONITOR: &str = "\
**** List of PLAYBACK Hardware Devices ****
card 0: PCH [HDA Intel PCH], device 3: HDMI 0 [HDMI 0]
card 0: PCH [HDA Intel PCH], device 7: HDMI 1 [HDMI 1]
";

    const APLAY_L_EMPTY: &str = "";

    #[test]
    fn parses_the_genuine_monitor_device_with_has_monitor_true() {
        let devices = parse_aplay_hdmi_devices(APLAY_L_WITH_MONITOR);
        assert_eq!(
            devices,
            vec![
                AplayHdmiDevice {
                    card: "PCH".to_string(),
                    dev: "3".to_string(),
                    has_monitor: true,
                    monitor_name: "BenQ GL2480".to_string(),
                },
                AplayHdmiDevice {
                    card: "PCH".to_string(),
                    dev: "7".to_string(),
                    has_monitor: false,
                    monitor_name: "HDMI 1".to_string(),
                },
            ]
        );
    }

    #[test]
    fn skips_non_hdmi_lines_entirely() {
        // The USB Audio card line above must never appear in the parsed HDMI set.
        let devices = parse_aplay_hdmi_devices(APLAY_L_WITH_MONITOR);
        assert!(devices.iter().all(|d| d.card != "CARD"));
    }

    #[test]
    fn resolves_the_first_genuine_monitor_device() {
        assert_eq!(
            resolve_marker_device_from_aplay(APLAY_L_WITH_MONITOR),
            Some("hw:CARD=PCH,DEV=3".to_string())
        );
    }

    #[test]
    fn resolution_fails_when_no_device_carries_a_genuine_monitor() {
        assert_eq!(resolve_marker_device_from_aplay(APLAY_L_NO_MONITOR), None);
    }

    #[test]
    fn resolution_fails_on_an_empty_transcript() {
        assert_eq!(resolve_marker_device_from_aplay(APLAY_L_EMPTY), None);
    }

    #[test]
    fn fallback_device_matches_the_pinned_rig_mode_default() {
        assert_eq!(FALLBACK_MARKER_DEVICE, "hw:CARD=PCH,DEV=3");
    }
}
