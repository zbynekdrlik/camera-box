//! #454 — content-assertion guard: the new-cam-box runbook skill + `verify-device.sh` acceptance
//! gate must exist, cover the documented checks, and be wired into the project's playbook router.
//!
//! Style mirrors `tests/setup_device_provisioner_hardening.rs` / `tests/appliance_boot_hardening.rs`:
//! read the REAL files and assert on the REAL contract (a textual pin proves the surface EXISTS
//! and covers the right ground; `tests/verify_device_pure_functions.rs` proves the pure decision
//! functions WORK). RED before `scripts/verify-device.sh` / `.claude/skills/provision/SKILL.md`
//! exist; GREEN after.

use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn on_noncomment_line(body: &str, needle: &str) -> bool {
    body.lines()
        .any(|l| l.contains(needle) && !l.trim_start().starts_with('#'))
}

const VERIFY_SCRIPT: &str = "scripts/verify-device.sh";
const SKILL: &str = ".claude/skills/provision/SKILL.md";

// ---------------------------------------------------------------------------------------------
// scripts/verify-device.sh -- must exist, be executable-looking, and reuse existing signals
// ---------------------------------------------------------------------------------------------

#[test]
fn verify_device_script_exists_and_is_a_bash_script() {
    let body = read(VERIFY_SCRIPT);
    assert!(
        body.starts_with("#!/usr/bin/env bash") || body.starts_with("#!/bin/bash"),
        "{VERIFY_SCRIPT} must start with a bash shebang"
    );
    assert!(
        on_noncomment_line(&body, "set -euo pipefail"),
        "{VERIFY_SCRIPT} must use `set -euo pipefail` (script-failure-policy)"
    );
}

#[test]
fn verify_device_sources_camera_set_for_name_resolution() {
    let body = read(VERIFY_SCRIPT);
    assert!(
        on_noncomment_line(&body, "camera-set.sh"),
        "{VERIFY_SCRIPT} must source scripts/camera-set.sh -- the single source of truth for \
         NAME -> IP / CAMERA_GENLOCK_FPS (#454/#24/#451)"
    );
    assert!(
        on_noncomment_line(&body, "camera_resolve"),
        "{VERIFY_SCRIPT} must call camera_resolve() to resolve the NAME argument (#454)"
    );
}

#[test]
fn verify_device_reuses_ndi_alive_and_clock_offset_guard_instead_of_reinventing() {
    let body = read(VERIFY_SCRIPT);
    assert!(
        on_noncomment_line(&body, "lib/ndi-alive.sh"),
        "{VERIFY_SCRIPT} must source scripts/lib/ndi-alive.sh and reuse emit_ok_grep_pattern() / \
         fatal_grep_pattern() instead of reinventing the NDI-alive signal (#454, reuse mandate)"
    );
    assert!(
        on_noncomment_line(&body, "clock-offset-guard.sh"),
        "{VERIFY_SCRIPT} must source scripts/clock-offset-guard.sh and reuse its dantesync PTP \
         lock + offset parsers instead of reinventing them (#454, reuse mandate)"
    );
    assert!(
        body.contains("emit_ok_grep_pattern"),
        "{VERIFY_SCRIPT} must call emit_ok_grep_pattern() (shared with deploy-fleet.sh / \
         upgrade-fleet-ndi.sh)"
    );
    assert!(
        body.contains("ptp_locked_from_journal") || body.contains("dantesync_locked_ok"),
        "{VERIFY_SCRIPT} must use clock-offset-guard.sh's ptp_locked_from_journal (directly or via \
         a thin wrapper) for the dantesync-locked check (#454)"
    );
}

#[test]
fn verify_device_has_a_source_guard_for_pure_functions() {
    let body = read(VERIFY_SCRIPT);
    assert!(
        on_noncomment_line(&body, r#"[ "${BASH_SOURCE[0]}" != "${0}" ]"#),
        "{VERIFY_SCRIPT} must guard its live SSH flow behind a `BASH_SOURCE != $0` check so the \
         pure functions can be sourced + unit-tested offline (#454, same convention as \
         setup-device.sh / clock-offset-guard.sh)"
    );
}

/// Every one of the ticket's 9 acceptance checks (a)-(i) must have a textual footprint in the
/// script -- this is the coverage pin; `tests/verify_device_pure_functions.rs` proves each one's
/// LOGIC is correct.
#[test]
fn verify_device_covers_every_acceptance_check() {
    let body = read(VERIFY_SCRIPT);
    let required = [
        ("--version", "(a) camera-box --version check"),
        ("is-active", "(b) systemctl is-active camera-box check"),
        ("journalctl", "(c)/(d)/(i) journalctl-based checks"),
        ("genlock.conf", "(e) genlock.conf drop-in check"),
        ("cpu-affinity.conf", "(f) cpu-affinity.conf drop-in check"),
        ("libndi.so.6", "(g) libndi.so.6 symlink-chain check"),
        ("avahi-browse", "(h) avahi mDNS NDI discovery check"),
        ("chroma", "(i) capture chroma colour/grayscale check"),
    ];
    for (needle, label) in required {
        assert!(
            body.contains(needle),
            "{VERIFY_SCRIPT} must cover {label} -- expected to find '{needle}' somewhere in the \
             script (#454)"
        );
    }
}

#[test]
fn verify_device_exits_nonzero_on_any_failed_check() {
    let body = read(VERIFY_SCRIPT);
    assert!(
        body.contains("exit 1") || body.contains("exit \"$FAILS\""),
        "{VERIFY_SCRIPT} must exit non-zero when any check fails (test-strictness: no silent \
         pass) (#454)"
    );
}

// ---------------------------------------------------------------------------------------------
// .claude/skills/provision/SKILL.md -- the canonical new-cam-box runbook
// ---------------------------------------------------------------------------------------------

#[test]
fn provision_skill_exists_with_frontmatter() {
    let body = read(SKILL);
    assert!(
        body.starts_with("---\nname: provision"),
        "{SKILL} must start with a `name: provision` frontmatter block (repo skill convention)"
    );
}

#[test]
fn provision_skill_documents_the_full_four_phase_flow() {
    let body = read(SKILL);
    for needle in [
        "create-usb-linux.sh",
        "--target-disk",
        "--yes",
        "setup-device.sh",
        "verify-device.sh",
    ] {
        assert!(
            body.contains(needle),
            "{SKILL} must document `{needle}` as part of the canonical new-cam-box runbook (#454)"
        );
    }
}

#[test]
fn provision_skill_references_the_tracking_issues() {
    let body = read(SKILL);
    for issue in ["#448", "#450", "#451", "#454"] {
        assert!(
            body.contains(issue),
            "{SKILL} should reference {issue} (the provisioning epic this runbook documents)"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// CLAUDE.md playbook router -- must point to the new skill
// ---------------------------------------------------------------------------------------------

#[test]
fn claude_md_router_points_to_the_provision_skill() {
    let body = read("CLAUDE.md");
    assert!(
        on_noncomment_line(&body, ".claude/skills/provision"),
        "CLAUDE.md's ## Playbook router must gain a line pointing to .claude/skills/provision \
         (project-playbook-maintenance.md) (#454)"
    );
}
