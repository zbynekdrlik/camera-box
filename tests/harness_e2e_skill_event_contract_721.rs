//! #721: the e2e supervisor EVENT checklist must document the machine-checkable EVENT-mode
//! CONTRACT that `rig-mode.sh event` now runs automatically — specifically the two additions
//! the ticket's acceptance names:
//!   - the fleet-wide `pgrep -f -- --paint-only` paint-process sweep (catches a RENAMED painter
//!     binary — the exact 2026-07-12 incident where a copy of frame-probe kept QR on air), and
//!   - the PIXEL assert (`qr_screenshot_check.py`: screenshot the camera scenes over OBS WS and
//!     QR-decode the actual rendered pixels — the decisive check the user caught by eye).
//!
//! Same content-assert precedent as `harness_frozen_camera_gate.rs::e2e_skill_documents_frozen_camera_gate`
//! (365): a stale supervisor doc reintroduces exactly the "someone has to remember what to check"
//! fragility this whole ticket exists to kill, so the doc is pinned by a test.

use std::fs;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// #721: the e2e EVENT checklist must document the pixel proof (screenshot QR-decode) step.
#[test]
fn e2e_skill_documents_event_mode_pixel_proof() {
    let skill = read(".claude/skills/e2e/SKILL.md");
    assert!(
        skill.contains("qr_screenshot_check"),
        ".claude/skills/e2e/SKILL.md EVENT-mode section must document the PIXEL proof \
         (scripts/qr_screenshot_check.py — screenshot the camera scenes over OBS WS and \
         QR-decode the actual rendered pixels). This is the decisive #721 check the user caught \
         by eye minutes before a broadcast; the supervisor checklist must name it."
    );
}

/// #721: the e2e EVENT checklist must document the fleet-wide `--paint-only` pattern sweep
/// (catches a RENAMED painter binary, never a bare process-name match).
#[test]
fn e2e_skill_documents_event_mode_fleet_paint_sweep() {
    let skill = read(".claude/skills/e2e/SKILL.md");
    assert!(
        skill.contains("pgrep -f -- --paint-only"),
        ".claude/skills/e2e/SKILL.md EVENT-mode section must document the fleet-wide \
         `pgrep -f -- --paint-only` paint-process sweep — the pattern-based check that catches a \
         RENAMED painter binary (the #721 incident), never a bare frame-probe name match. The \
         supervisor checklist must name it fleet-wide."
    );
}
