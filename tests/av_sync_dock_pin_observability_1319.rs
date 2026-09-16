//! #1319 Part 2 — dock BIAS observability in `vendor/av-sync-dock/src/sync-test-output.cpp`.
//!
//! The overnight A/V BAND false alarm (issue 1319) was caused by the dock estimator carrying a
//! non-constant bias vs the recording-based verdict (a wrong-cluster QPSK-marker lag pick, most
//! likely after a genlock pin change). The dock C++ compiles ONLY via the `windows-genlock*.yml`
//! pwsh gate (no local compile path here), so this is a SOURCE-presence guard (same convention as
//! `av_sync_dock_qr_patch_guard.rs`): it reads the vendored C++ as text and fails loudly if the two
//! observability additions are ever dropped by a refactor / `git subtree pull`. Both are ALSO
//! mirrored by a pwsh presence check in BOTH windows-genlock workflows (the two-language double
//! coverage `.claude/rules/av-sync-dock-anchor-refactor-safety.md` mandates for a source anchor).

use std::path::PathBuf;

const DOCK_OUTPUT: &str = "vendor/av-sync-dock/src/sync-test-output.cpp";

fn vendor_file(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Collapse every run of ASCII whitespace to a single space so the assertions survive reformatting
/// (mirrors the pwsh `-replace '\s+', ' '` the workflow gate uses on the same source).
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn updated_locked_line_appends_lag_idx_and_cands() {
    let src = squish(&vendor_file(DOCK_OUTPUT));
    // The LOCKED/UPDATED cluster line must carry the chosen cluster's lag bucket + candidate-pool
    // size at the END, so a wrong-cluster pick (matched << cands, lag_idx jumps) is visible.
    assert!(
        src.contains("lag_idx=%ld cands=%zu"),
        "issue 1319: the UPDATED/LOCKED offset= line must append lag_idx=%ld cands=%zu"
    );
    // cands must be the total candidate pool the densest window was chosen from.
    assert!(
        src.contains("st->cb_offset_cluster.samples.size()"),
        "issue 1319: cands must be the cluster's total candidate-pool size"
    );
    // matched= (chosen-window count) and mad= (scatter) must still precede the new fields.
    let line = src
        .split("source=cluster matched=%zu mad=%.1fms")
        .nth(1)
        .expect("issue 1319: the matched=/mad= tokens must still precede the new fields");
    assert!(
        line.trim_start().starts_with("lag_idx=%ld cands=%zu"),
        "issue 1319: lag_idx=/cands= must be APPENDED after mad= (existing tokens byte-identical)"
    );
}

#[test]
fn pin_change_is_observed() {
    let src = squish(&vendor_file(DOCK_OUTPUT));
    // The pin-change marker + its backing state field must both exist.
    assert!(
        src.contains("av-sync-dock: pin-change observed %d -> %d"),
        "issue 1319: a `pin-change observed <old> -> <new>` marker must be emitted on a pin change"
    );
    assert!(
        src.contains("int32_t cb_last_seen_pin_ms = -1;"),
        "issue 1319: the -1-sentinel cb_last_seen_pin_ms state field must back the pin-change marker"
    );
    // The marker must be gated on a real change (>= 0 sentinel + inequality), never fire on every push.
    assert!(
        src.contains("st->cb_last_seen_pin_ms >= 0 && current_ms != st->cb_last_seen_pin_ms"),
        "issue 1319: the pin-change marker must be gated on the sentinel + an actual change"
    );
}
