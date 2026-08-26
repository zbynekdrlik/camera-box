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
//!
//! ## Self-recovery for the degraded marker (issue 1172)
//!
//! Issue 984's soft-degrade path (in `probe::run::run_paint_only`) opened the ALSA PCM ONCE and,
//! on a transient-busy failure, only LOGGED "still DEGRADED" forever — it never re-attempted the
//! open, so a marker degraded by a device that was momentarily held at painter start
//! (`hw:CARD=PCH,DEV=3` still releasing from a lipsync-test ffmpeg) stayed silent until a manual
//! `systemctl restart cam2-painter`. [`AudioMarkerRecovery`] is the PURE decision seam that closes
//! that: it tells the (probe-gated, linux-only) control loop WHEN to re-open, so the degraded
//! marker genuinely re-opens the device each retry cycle and self-recovers once it frees.

use std::time::Duration;

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
    paint_only
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
    let rest = line.strip_prefix("card ")?;
    let colon1 = rest.find(':')?;
    let (num1, rest) = rest.split_at(colon1);
    if num1.is_empty() || !num1.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let rest = rest.strip_prefix(':')?.strip_prefix(' ')?;

    let bracket1 = rest.find(" [")?;
    let cardname = &rest[..bracket1];
    if cardname.is_empty()
        || !cardname
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return None;
    }
    let rest = &rest[bracket1 + 2..];
    let close1 = rest.find(']')?;
    // description (unused) = &rest[..close1]
    let rest = &rest[close1 + 1..];
    let rest = rest.strip_prefix(", device ")?;

    let colon2 = rest.find(':')?;
    let (num2, rest) = rest.split_at(colon2);
    if num2.is_empty() || !num2.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let rest = rest.strip_prefix(':')?.strip_prefix(' ')?;

    let bracket2 = rest.find(" [")?;
    let slot = &rest[..bracket2];
    let slot_num = slot.strip_prefix("HDMI ")?;
    if slot_num.is_empty() || !slot_num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let rest = &rest[bracket2 + 2..];
    let monitor = rest.strip_suffix(']')?;

    let has_monitor = !monitor.is_empty() && monitor != slot;
    Some(AplayHdmiDevice {
        card: cardname.to_string(),
        dev: num2.to_string(),
        has_monitor,
        monitor_name: monitor.to_string(),
    })
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

/// #1172 — while the SOFT (issue 984) audio marker is degraded (its ALSA PCM could not be opened,
/// e.g. `hw:CARD=PCH,DEV=3` was transiently held by an ffmpeg/aplay lipsync test still releasing
/// it at painter start), the emit device is re-opened this often to test whether the holder has
/// let go. Matches the existing 5 s degraded-log cadence so a freed device resumes the marker
/// within ~one period: small enough to recover "within a few cycles" (issue 1172), large enough
/// that a still-busy device is never hammered.
pub const RECOVERY_RETRY_INTERVAL: Duration = Duration::from_secs(5);

/// #1172 — what the degraded control loop (`probe::run::run_paint_only`) should do for the audio
/// marker on a given poll tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerRecoveryStep {
    /// A live emitter is running — nothing to do (the caller still polls its `death_reason()`).
    Healthy,
    /// Degraded, but still within the retry interval — keep painting, take no action this tick.
    Waiting,
    /// Degraded and the retry interval has elapsed — attempt a fresh ALSA re-open NOW.
    AttemptReopen,
}

/// #1172 — pure self-recovery state machine for the degraded audio-marker path.
///
/// The impure caller (`probe::run::run_paint_only`, linux + probe-gated) owns the ALSA re-open
/// (`QpskEmitter::spawn`), the `Instant` retry clock, and the tracing; this type owns only the
/// *timing + degraded-transition decision*, so the whole recovery policy is Tier-0 testable on
/// default features even though the code that drives it is probe-gated (`src/probe/**`, CI-only
/// compile). Same PURE-decision seam the crate already uses for NDI re-announce
/// (`crate::reannounce::ReannounceState`).
///
/// The contract this type enforces (and that the shipped issue-984 degraded loop VIOLATED — it
/// only logged, never re-attempted the open, so a marker degraded by a transient ALSA-busy at
/// startup stayed silent until a manual `systemctl restart cam2-painter`):
/// - a degraded marker whose retry interval has elapsed asks the caller to RE-OPEN
///   ([`MarkerRecoveryStep::AttemptReopen`]) — the recovery the old loop never had;
/// - a SUCCESSFUL re-open clears the degraded state so a subsequent tick is `Healthy` (no more
///   retries, no spin, no needless re-open of a working device);
/// - a FAILED re-open LEAVES it degraded so the next interval retries again;
/// - a healthy marker is always `Healthy` and never retries.
#[derive(Debug, Clone)]
pub struct AudioMarkerRecovery {
    degraded: bool,
}

impl AudioMarkerRecovery {
    /// A marker whose PCM opened cleanly — a live emitter is running.
    pub fn healthy() -> Self {
        Self { degraded: false }
    }

    /// A marker whose startup open FAILED (soft degrade) — degraded from the first tick, so the
    /// retry loop takes over immediately.
    pub fn degraded() -> Self {
        Self { degraded: true }
    }

    /// True while no live emitter is running (the periodic `#984` "still DEGRADED" heartbeat and
    /// the retry path both key on this).
    pub fn is_degraded(&self) -> bool {
        self.degraded
    }

    /// Decide what to do on this poll tick. `since_last_attempt` is the wall time elapsed since
    /// the last ALSA open attempt (the startup spawn, or the most recent retry). A healthy marker
    /// is always [`MarkerRecoveryStep::Healthy`]; a degraded marker asks for a re-open once the interval
    /// has elapsed ([`MarkerRecoveryStep::AttemptReopen`]), otherwise [`MarkerRecoveryStep::Waiting`].
    pub fn step(&self, since_last_attempt: Duration) -> MarkerRecoveryStep {
        if !self.degraded {
            MarkerRecoveryStep::Healthy
        } else if since_last_attempt >= RECOVERY_RETRY_INTERVAL {
            MarkerRecoveryStep::AttemptReopen
        } else {
            MarkerRecoveryStep::Waiting
        }
    }

    /// Record the outcome of a re-open ATTEMPT: `true` (a fresh emitter is running) clears the
    /// degraded state; `false` (the device is still busy) LEAVES it degraded so the next interval
    /// retries again. The caller resets its retry clock whenever it makes an attempt, so a failed
    /// retry waits a full interval before the next one — never a hot spin on a busy device.
    pub fn record_reopen(&mut self, succeeded: bool) {
        if succeeded {
            self.degraded = false;
        }
    }

    /// Record that a RUNNING emitter died mid-run (a soft mid-run ALSA failure): drop to degraded
    /// so the retry loop re-opens a fresh emitter exactly as at a failed startup open.
    pub fn mark_degraded(&mut self) {
        self.degraded = true;
    }
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

    // --- #1172 AudioMarkerRecovery ------------------------------------------------------------

    #[test]
    fn healthy_marker_never_retries() {
        let r = AudioMarkerRecovery::healthy();
        assert!(!r.is_degraded());
        assert_eq!(r.step(Duration::from_secs(0)), MarkerRecoveryStep::Healthy);
        // Even long past the interval, a healthy marker never re-opens a working device.
        assert_eq!(
            r.step(Duration::from_secs(3600)),
            MarkerRecoveryStep::Healthy
        );
    }

    #[test]
    fn degraded_marker_waits_within_the_interval() {
        let r = AudioMarkerRecovery::degraded();
        assert!(r.is_degraded());
        assert_eq!(r.step(Duration::from_secs(0)), MarkerRecoveryStep::Waiting);
        assert_eq!(
            r.step(RECOVERY_RETRY_INTERVAL - Duration::from_millis(1)),
            MarkerRecoveryStep::Waiting
        );
    }

    #[test]
    fn degraded_marker_retries_after_the_interval() {
        // THE issue-1172 bug in pure form: the shipped loop NEVER re-attempts the open — it only
        // logs "still DEGRADED" forever. Once the retry interval elapses, a degraded marker MUST
        // ask the caller to re-open. RED against the stub (which returns Waiting forever).
        let r = AudioMarkerRecovery::degraded();
        assert_eq!(
            r.step(RECOVERY_RETRY_INTERVAL),
            MarkerRecoveryStep::AttemptReopen
        );
        assert_eq!(
            r.step(RECOVERY_RETRY_INTERVAL + Duration::from_secs(10)),
            MarkerRecoveryStep::AttemptReopen
        );
    }

    #[test]
    fn successful_reopen_clears_degraded_and_stops_retrying() {
        // After the device frees and a re-open succeeds, the marker is healthy again and a later
        // tick (even long past the interval) must NOT keep retrying — no spin, no needless
        // re-open of a working device.
        let mut r = AudioMarkerRecovery::degraded();
        assert_eq!(
            r.step(RECOVERY_RETRY_INTERVAL),
            MarkerRecoveryStep::AttemptReopen
        );
        r.record_reopen(true);
        assert!(!r.is_degraded());
        assert_eq!(
            r.step(RECOVERY_RETRY_INTERVAL * 10),
            MarkerRecoveryStep::Healthy
        );
    }

    #[test]
    fn failed_reopen_stays_degraded_and_retries_next_interval() {
        // A retry against a still-busy device fails; the marker stays degraded and the next
        // elapsed interval asks to re-open again (self-recovery keeps trying until it frees).
        let mut r = AudioMarkerRecovery::degraded();
        assert_eq!(
            r.step(RECOVERY_RETRY_INTERVAL),
            MarkerRecoveryStep::AttemptReopen
        );
        r.record_reopen(false);
        assert!(r.is_degraded());
        assert_eq!(
            r.step(RECOVERY_RETRY_INTERVAL),
            MarkerRecoveryStep::AttemptReopen
        );
    }

    #[test]
    fn mid_run_death_drops_a_healthy_marker_to_degraded_then_recovers() {
        // A soft mid-run ALSA death degrades a previously-healthy marker; the same retry path then
        // recovers it exactly as at a failed startup open.
        let mut r = AudioMarkerRecovery::healthy();
        assert_eq!(r.step(Duration::from_secs(1)), MarkerRecoveryStep::Healthy);
        r.mark_degraded();
        assert!(r.is_degraded());
        assert_eq!(r.step(Duration::ZERO), MarkerRecoveryStep::Waiting);
        assert_eq!(
            r.step(RECOVERY_RETRY_INTERVAL),
            MarkerRecoveryStep::AttemptReopen
        );
        r.record_reopen(true);
        assert_eq!(r.step(RECOVERY_RETRY_INTERVAL), MarkerRecoveryStep::Healthy);
    }

    #[test]
    fn retry_interval_matches_the_degraded_log_cadence() {
        // The retry cadence tracks the existing 5 s #984 degraded-log heartbeat so a freed device
        // resumes within ~one log period (issue 1172 "within a few cycles").
        assert_eq!(RECOVERY_RETRY_INTERVAL, Duration::from_secs(5));
    }
}
