//! #1299 — vendored-source anchor guards for the fleet-visible genlock-lock-json: emission.
//!
//! The #1298 statusbar widget gained a machine-readable `genlock-lock-json:` line (the versioned
//! JSON the #1299 bundle-state facet + dev1 watchdog read). These guards assert the new symbols +
//! log string are PRESENT so a future `git subtree pull` can't silently revert them, and that the
//! new marker stays mutually non-substring with every existing `genlock-*` / `*-audit:` family
//! (the `.claude/rules/jitter-audit-parser.md` rule) — INCLUDING the #1298 `genlock-lock:` line it
//! sits beside. They are the Linux-CI twin of the 3-copy pwsh source-anchor gates in both
//! `windows-genlock{,-fast}.yml` (`.claude/rules/obs-titlebar-build-id.md`) — keep all three in
//! lock-step.
//!
//! Std-only + path via a runtime env lookup (not the `env!` macro) so it runs both under cargo and
//! standalone (`rustc --test tests/genlock_lock_json_guards.rs` from the repo root).

use std::path::PathBuf;

fn vendor_file(rel: &str) -> String {
    let base = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let path = PathBuf::from(base).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Collapse all runs of whitespace to a single space — the Rust twin of the pwsh gates'
/// `-replace '\s+', ' '`, so the same pinned substring works in both.
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

const STATUSBAR_CPP: &str = "vendor/obs-studio/frontend/widgets/OBSBasicStatusBar.cpp";
const STATUSBAR_HPP: &str = "vendor/obs-studio/frontend/widgets/OBSBasicStatusBar.hpp";

fn assert_has(file: &str, needle: &str) {
    let src = squish(&vendor_file(file));
    assert!(
        src.contains(needle),
        "{file}: #1299 anchor `{needle}` is GONE — the fleet-visible genlock-lock-json: emission \
         was partially reverted (e.g. a subtree pull). Re-apply it and keep the \
         windows-genlock{{,-fast}}.yml pwsh gates in lock-step."
    );
}

#[test]
fn genlock_lock_json_emission_present() {
    // the versioned machine-readable line the dev1 fleet watchdog + bundle-state read
    assert_has(STATUSBAR_CPP, "genlock-lock-json: %s (#1299)");
    // the pure builder + escaper (no obs_data dependency -> Facet B lift-compilable)
    assert_has(STATUSBAR_CPP, "std::string genlock_build_lock_json(");
    assert_has(
        STATUSBAR_CPP,
        "void genlock_json_append_escaped(std::string &out, const char *s)",
    );
    // the structured per-input record the builder walks
    assert_has(STATUSBAR_CPP, "struct GenlockInputRow {");
    // the heartbeat constant + its own change-tracking state (separate from genlockLastLogged*)
    assert_has(
        STATUSBAR_CPP,
        "static constexpr int GENLOCK_JSON_HEARTBEAT_TICKS = 30;",
    );
    assert_has(STATUSBAR_HPP, "int genlockJsonHeartbeatTicks = 0;");
    assert_has(STATUSBAR_HPP, "int genlockJsonLastState = -1;");
}

#[test]
fn genlock_lock_recent_event_offender_present_1299_part3() {
    // #1299 Part 3: the builder emits the recent_event_inputs attribution (top offender name+count)
    // and the widget feeds recent_event from the CONNECTED-only, PHASE-only aggregate via the pure
    // genlock_input_phase_events rule (drops underruns + absent-sender rebind churn). A subtree pull
    // that reverts either silently re-opens the chronic recent_event false-page.
    assert_has(
        STATUSBAR_CPP,
        "genlock_json_append_escaped(j, recent_event_input_name);",
    );
    assert_has(
        STATUSBAR_CPP,
        "genlock_input_phase_events(r.connected ? 1 : 0, r.relocks, r.late_holds,",
    );
    // the enriched human reason (reason=recent_event:<name>) built from the offender name
    assert_has(
        STATUSBAR_CPP,
        "reason == GENLOCK_LOCK_REASON_RECENT_EVENT && !recent_event_input_name.empty()",
    );
    // the pure rule + its C mirror anchor (kept in lock-step by tests/genlock_lock_state_parity.rs)
    assert_has(
        "vendor/obs-studio/frontend/widgets/GenlockLockState.hpp",
        "static inline uint64_t genlock_input_phase_events(int connected, uint64_t relocks,",
    );
}

#[test]
fn genlock_lock_json_marker_is_mutually_non_substring() {
    // The new OBS-log family `genlock-lock-json:` must be mutually non-substring with every
    // existing marker (jitter-audit-parser.md) — ESPECIALLY the #1298 `genlock-lock:` line it sits
    // beside, so a parser keyed on one never matches the other.
    const NEW: &str = "genlock-lock-json:";
    let existing = [
        "genlock-lock:",
        "genlock-fifo audit '",
        "genlock-ndi-output audit '",
        "genlock-ndi-filter audit '",
        "genlock-relock",
        "genlock-acquire-bracket '%s':",
        "multiview-audit:",
        "program-render-audit:",
        "recv-timing #797 '",
        "asrc: source '",
    ];
    for m in existing {
        assert!(
            !NEW.contains(m) && !m.contains(NEW),
            "#1299: the new `genlock-lock-json:` marker collides (substring) with existing marker \
             `{m}` — pick a marker mutually non-substring with every genlock-* / *-audit: family."
        );
    }
}
