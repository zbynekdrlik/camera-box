//! #901 — rig-mode.sh `test` mode needs a NON-BLOCKING tier for the measurement-audio-arrival
//! check: hard-FAIL only when the mbc chain is genuinely DEAD (device absent / near-digital-
//! silence), but only WARN+report when it is merely QUIET (present but degraded — the CURRENT
//! #976 ~26dB degradation, which must never wedge a rig-mode TEST restore).
//!
//! These are PURELY ADDITIVE functions in scripts/lib/audio-presence-preflight.sh — the existing
//! STRICT `audio_preflight_is_silent`/`audio_preflight_silent_message` (used by recording-e2e.sh's
//! real [4b2/8] gate) are untouched; see harness_audio_presence_preflight.rs for those.
//!
//! Reference levels (from the lib's own header comment): digital silence measures ~-91 dB; a live
//! QPSK marker measures ~-5 dB. -80 dB sits comfortably above the silence floor and comfortably
//! below any real signal, so it is the DEAD/QUIET boundary; -60 dB (the existing STRICT threshold)
//! stays the QUIET/AUDIBLE boundary.

use std::process::Command;

fn lib() -> String {
    format!(
        "{}/scripts/lib/audio-presence-preflight.sh",
        env!("CARGO_MANIFEST_DIR")
    )
}

fn run(snippet: &str) -> (bool, String) {
    let script = format!(". \"{}\"\n{}", lib(), snippet);
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run bash");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

#[test]
fn digital_silence_tiers_as_dead() {
    let (_ok, tier) = run("audio_preflight_tier -91.0");
    assert_eq!(tier, "dead", "true digital silence must tier as dead");
}

#[test]
fn a_healthy_marker_tiers_as_audible() {
    let (_ok, tier) = run("audio_preflight_tier -5.4");
    assert_eq!(tier, "audible");
}

#[test]
fn a_degraded_but_present_signal_tiers_as_quiet_not_dead() {
    // The current #976 live degradation (~26 dB below a healthy ~-34 dB level) lands well below
    // the existing -60 dB STRICT bar but nowhere near true silence -- must be "quiet", never "dead".
    let (_ok, tier) = run("audio_preflight_tier -65");
    assert_eq!(
        tier, "quiet",
        "a present-but-degraded level must tier as quiet, not dead"
    );
}

#[test]
fn boundary_values_are_inclusive_on_the_healthy_side() {
    // Exactly at -80 -> quiet (not dead); exactly at -60 -> audible (not quiet). Strict '<' on
    // both boundaries, mirroring audio_preflight_is_silent's own strict-< convention.
    let (_ok, at_dead_floor) = run("audio_preflight_tier -80");
    assert_eq!(at_dead_floor, "quiet");
    let (_ok, at_warn_floor) = run("audio_preflight_tier -60");
    assert_eq!(at_warn_floor, "audible");
    let (_ok, just_below_dead_floor) = run("audio_preflight_tier -80.1");
    assert_eq!(just_below_dead_floor, "dead");
    let (_ok, just_below_warn_floor) = run("audio_preflight_tier -60.1");
    assert_eq!(just_below_warn_floor, "quiet");
}

#[test]
fn thresholds_are_overridable() {
    let (_ok, tier) = run("audio_preflight_tier -50 -40 -30");
    assert_eq!(
        tier, "dead",
        "with dead floor -40, -50 must tier dead regardless of the default -80"
    );
}

#[test]
fn dead_message_names_the_chain_and_never_claims_merely_quiet() {
    let (_ok, m) = run("audio_preflight_dead_message -91.0");
    for needle in ["mbc", "Dante", "901"] {
        assert!(m.contains(needle), "dead message must mention {needle:?}: {m}");
    }
    assert!(
        m.to_lowercase().contains("dead") || m.to_lowercase().contains("silen"),
        "dead message must clearly say the chain is dead/silent: {m}"
    );
}

#[test]
fn quiet_message_notes_it_is_non_blocking_and_references_976() {
    let (_ok, m) = run("audio_preflight_quiet_message -65");
    assert!(
        m.contains("976"),
        "quiet message must reference the known #976 degradation: {m}"
    );
    assert!(
        m.to_lowercase().contains("warn") || m.to_lowercase().contains("proceed"),
        "quiet message must make clear this does not block: {m}"
    );
    assert!(m.contains("-65"), "quiet message must include the measured level: {m}");
}
