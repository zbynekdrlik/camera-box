//! #1117 — the dev1-side alert-watchdog wiring + owner-facing page-text guard.
//!
//! Root cause (issue 1117, live incident 2026-08-18T22:59:57): the optical-chain watchdog paged
//! `🚨 #860 optical-chain: cam2 painter DEAD ... Restore: scripts/rig-mode.sh test` — English
//! tech-dump, no owner-facing outcome, no "who acts" — DURING a live Full-path E2E run that had
//! legitimately stopped cam2-painter.service (and whose pass even measured optical=OK). The pure
//! decision fix lives in tests/harness_optical_chain_health_860.rs; THIS file pins the two dev1
//! surfaces' WIRING + PAGE TEXT so a future regression can't silently reintroduce either gap:
//!   1. the optical-chain watchdog consults the rig-busy (E2E-window) signal + passes it into the
//!      4-arg decision, and handles the new log-only verdicts;
//!   2. every owner-facing PAGE body across the swept dev1 alert-watchdogs is plain Slovak with
//!      explicit ownership (agent-recoverable => "Rieši Claude"; physical => a human step), and the
//!      uniform English tech-dump phrase "Confirmed over ... consecutive passes" is gone from them.
//!
//! Static-content assertions (read the script FILE), the same shape as the recording-e2e.sh anchor
//! tests — internal LOG lines may stay English, so these pin only the `notify --body` PAGE strings
//! and the wiring, never the log lines.

use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_script(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

// ---------------------------------------------------------------------------------------------
// 1) optical-chain watchdog wiring: it must reuse the #281 rig-active heartbeat as the rig-busy
//    signal, pass it into the 4-arg decision, and route the new log-only verdicts.
// ---------------------------------------------------------------------------------------------
#[test]
fn optical_chain_watchdog_reuses_rig_heartbeat_and_passes_rig_busy() {
    let s = read_script("scripts/optical-chain-alert-watchdog.sh");
    assert!(
        s.contains(". \"$HERE/lib/rig-heartbeat.sh\""),
        "watchdog must SOURCE the shared #281 rig-heartbeat lib (reuse, not a second busy detector)"
    );
    assert!(
        s.contains("rig_heartbeat_active"),
        "watchdog must consult rig_heartbeat_active as the E2E-window signal"
    );
    // The 4-arg decision call: rig_busy is the 4th positional arg.
    assert!(
        s.contains(
            "optical_chain_alert_condition \"$painter_expected\" \"$painter_alive\" \"$optical\" \"$rig_busy\""
        ),
        "watchdog must pass rig_busy as the 4th arg to the pure decision"
    );
    // The log-only verdicts must be handled (suppressed, not paged).
    assert!(
        s.contains("log-only:*)"),
        "watchdog main() must branch on log-only:* verdicts (suppress, never page)"
    );
}

// ---------------------------------------------------------------------------------------------
// 2) optical-chain PAGE text is Slovak + explicit ownership, and the old English is gone.
// ---------------------------------------------------------------------------------------------
#[test]
fn optical_chain_page_text_is_slovak_with_ownership() {
    let s = read_script("scripts/optical-chain-alert-watchdog.sh");
    assert!(
        s.contains("optická vetva cam2"),
        "page header must be the plain-Slovak 'optická vetva cam2'"
    );
    assert!(
        s.contains("Rieši Claude automaticky"),
        "an agent-recoverable page must tell the owner Claude handles it (ownership)"
    );
    // The exact English tech-dump the owner complained about must be gone from the page bodies.
    for banned in [
        "cam2 painter DEAD",
        "Restore: scripts/rig-mode.sh test",
        "process-alive is not QR-on-screen",
    ] {
        assert!(
            !s.contains(banned),
            "old English page tech-dump still present: {banned:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// 3) the SWEEP: every listed dev1 alert-watchdog's PAGE bodies are Slovak with explicit ownership,
//    and the uniform English "Confirmed over ... consecutive passes" phrase is gone.
// ---------------------------------------------------------------------------------------------

/// (script path, a Slovak marker that MUST appear in its page text after translation).
// #1117 review: every marker is a distinctive Slovak PAGE-BODY phrase absent from the
// pre-translation origin/dev file (verified `git show origin/dev:<f> | grep -cF <marker>` == 0),
// so each is a genuine translation guard -- a vacuous marker (a technical term like `imag` /
// `BundleStateServer` / `HDMI splitter` that already appears in comments/code) would pass even
// against the untranslated file and guard nothing.
const SWEEP: &[(&str, &str)] = &[
    ("scripts/cadence-alert-watchdog.sh", "kadencia kamery"),
    (
        // #1206 moved the recovery ✅ ping ("opäť sníma vo farbe") to the machine channel, so the
        // marker now points at a distinctive Slovak phrase in the surviving ALERT page body.
        "scripts/splitter-port-alert-watchdog.sh",
        "chyba je najskôr v HDMI splitter porte",
    ),
    ("scripts/mv-fps-alert-watchdog.sh", "Multiview fps"),
    ("scripts/network-reach-alert-watchdog.sh", "nedostupný box"),
    (
        "scripts/avsync-heartbeat-alert-watchdog.sh",
        "A/V-sync monitor",
    ),
    ("scripts/bundle-state-alert-watchdog.sh", "hoci box beží"),
    ("scripts/obs-liveness-watchdog.sh", "OBS zamrznuté"),
    ("scripts/imag-obs-alert-watchdog.sh", "imag OBS"),
    (
        "scripts/imag-power-envelope-alert-watchdog.sh",
        "napájací limit",
    ),
];

#[test]
fn swept_watchdogs_page_text_is_slovakized() {
    for (path, marker) in SWEEP {
        let s = read_script(path);
        assert!(
            s.contains(marker),
            "{path}: expected Slovak page marker {marker:?} not found (translation missing?)"
        );
    }
}

#[test]
fn swept_watchdogs_dropped_the_english_confirmed_over_phrase() {
    // "Confirmed over ${CONFIRM_THRESHOLD} consecutive passes" appeared ONLY in the page body of
    // each of these (verified: one occurrence per file, all in `notify --body`) — so its absence is
    // a clean regression guard against reverting a page body to English.
    for path in [
        "scripts/cadence-alert-watchdog.sh",
        "scripts/splitter-port-alert-watchdog.sh",
        "scripts/mv-fps-alert-watchdog.sh",
        "scripts/network-reach-alert-watchdog.sh",
        "scripts/bundle-state-alert-watchdog.sh",
        "scripts/obs-liveness-watchdog.sh",
    ] {
        let s = read_script(path);
        assert!(
            !s.contains("Confirmed over"),
            "{path}: the English 'Confirmed over ... consecutive passes' page phrase is back"
        );
    }
}

#[test]
fn agent_recoverable_watchdogs_state_claude_ownership() {
    // Each agent-recoverable page tells the owner Claude handles it, so they never wonder
    // "co mam akoze ja s tym robit". Physical-intervention pages (splitter-port, network-reach) are
    // deliberately EXCLUDED here — they carry an honest human step instead (asserted below).
    for path in [
        "scripts/mv-fps-alert-watchdog.sh",
        "scripts/avsync-heartbeat-alert-watchdog.sh",
        "scripts/bundle-state-alert-watchdog.sh",
        "scripts/obs-liveness-watchdog.sh",
        "scripts/imag-obs-alert-watchdog.sh",
        "scripts/imag-power-envelope-alert-watchdog.sh",
    ] {
        let s = read_script(path);
        assert!(
            s.contains("Rieši Claude"),
            "{path}: an agent-recoverable page must state Claude ownership"
        );
    }
}

#[test]
fn physical_intervention_watchdogs_state_a_human_step() {
    // A genuinely physical fault (cable/NIC/box off) must tell the owner a concrete physical step,
    // never a false "Claude rieši".
    for path in [
        "scripts/splitter-port-alert-watchdog.sh",
        "scripts/network-reach-alert-watchdog.sh",
    ] {
        let s = read_script(path);
        assert!(
            s.contains("fyzick"),
            "{path}: a physical fault page must name a human/physical step (fyzický zásah)"
        );
    }
}
