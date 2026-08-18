//! #791 (imag reprovision parity) — the full max-performance persistence trio must be PROVISIONED
//! by `scripts/setup-imag.sh`, not hand-placed.
//!
//! Live audit 2026-08-18 (10.77.9.182): the incumbent's performance persistence lives in a separate
//! `imag-maxperf.service` (issue 756) → `/usr/local/sbin/imag-maxperf.sh` (governor + EPP +
//! intel_pstate no_turbo=0 + platform_profile=performance + powerprofilesctl + usbcore autosuspend +
//! PCI runtime-PM off) PLUS a hotplug-persistent udev rule `99-imag-maxperf-pm.rules`. None of it
//! was ever tracked in the repo (`grep -rn imag-maxperf scripts/ tests/ systemd/` == NONE) — it was
//! hand-placed (issue 756) and never ported to the generator, the exact "hand-placed hidden by a
//! hand patch" class #791 exists to close (same shape as imag-obs-start.sh #840, NVIDIA tuning #841,
//! remoteos-mcp #858). setup-imag.sh step 4 (`cpu-performance.service` + rc.local) persists ONLY the
//! governor + per-device USB/NET `power/control=on`; EPP/turbo/platform_profile/PCI-PM/hotplug-udev
//! were absent entirely — the exact EPP-persistence gap the 2026-07-18 audit demanded be folded in.
//!
//! These are static text guards over setup-imag.sh + verify-imag.sh (Tier-0, no box). RED before the
//! step-26 provisioning + the verify (y) check land; GREEN after.

use std::path::PathBuf;

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

const SETUP: &str = "scripts/setup-imag.sh";
const VERIFY: &str = "scripts/verify-imag.sh";

/// The systemd oneshot unit must be generated verbatim to match the live box's topology (the
/// mandate is "identical with today's imag"): ExecStart the sbin script, RemainAfterExit, ordered
/// after power-profiles-daemon (powerprofilesctl needs the daemon up).
#[test]
fn setup_imag_provisions_the_maxperf_service_791() {
    let body = read(SETUP);
    assert!(
        body.contains("cat > /etc/systemd/system/imag-maxperf.service"),
        "{SETUP} must WRITE /etc/systemd/system/imag-maxperf.service (issue 756 perf persistence, #791 parity)"
    );
    assert!(
        body.contains("ExecStart=/usr/local/sbin/imag-maxperf.sh"),
        "{SETUP}: imag-maxperf.service must ExecStart /usr/local/sbin/imag-maxperf.sh (matches the live box)"
    );
    assert!(
        body.contains("RemainAfterExit=yes"),
        "{SETUP}: imag-maxperf.service is a oneshot enforcement unit — RemainAfterExit=yes"
    );
    assert!(
        body.contains("power-profiles-daemon.service"),
        "{SETUP}: imag-maxperf.service must order After= power-profiles-daemon.service (powerprofilesctl needs the daemon)"
    );
}

/// The enforcement script itself must be generated with every performance knob the live script sets:
/// governor, EPP, intel_pstate no_turbo=0, platform_profile, powerprofilesctl, usbcore autosuspend,
/// PCI runtime-PM. Each guarded by `[ -f ]`/`command -v` so it stays hardware-agnostic (#816).
#[test]
fn setup_imag_provisions_the_maxperf_script_791() {
    let body = read(SETUP);
    assert!(
        body.contains("cat > /usr/local/sbin/imag-maxperf.sh"),
        "{SETUP} must WRITE /usr/local/sbin/imag-maxperf.sh (the boot enforcement script, #791)"
    );
    for needle in [
        "scaling_governor",
        "energy_performance_preference",
        "intel_pstate/no_turbo",
        "platform_profile",
        "powerprofilesctl set performance",
        "usbcore/parameters/autosuspend",
        "/sys/bus/pci/devices/*/power/control",
    ] {
        assert!(
            body.contains(needle),
            "{SETUP}: imag-maxperf.sh must set the {needle} performance knob (2026-07-18 audit: EPP/turbo/profile/PM persistence)"
        );
    }
}

/// The hotplug-persistent udev rule must be provisioned too — a device that re-enumerates (USB
/// re-plug, PCI hotplug) must have its runtime PM forced back off, else the boot-time one-shot
/// write is lost. `ATTR{power/control}="on"` == runtime PM OFF (kernel semantics).
#[test]
fn setup_imag_provisions_the_maxperf_udev_rule_791() {
    let body = read(SETUP);
    assert!(
        body.contains("99-imag-maxperf-pm.rules"),
        "{SETUP} must WRITE /etc/udev/rules.d/99-imag-maxperf-pm.rules (hotplug runtime-PM persistence, #791)"
    );
    assert!(
        body.contains(r#"ATTR{power/control}="on""#),
        "{SETUP}: the maxperf udev rule must force runtime PM OFF (power/control=on) on device add"
    );
}

/// The unit must actually be enabled + started at provisioning time — a written-but-disabled unit
/// is the #840 "provisioned the file, never wired it" trap.
#[test]
fn setup_imag_enables_the_maxperf_service_791() {
    let body = read(SETUP);
    assert!(
        body.contains("systemctl enable --now imag-maxperf.service"),
        "{SETUP} must `systemctl enable --now imag-maxperf.service` (a written-but-disabled unit is the #840 trap)"
    );
}

/// btop is a MENU dependency: setup-imag.sh's GENERATED ~/.config/openbox/menu.xml (step 16, #785)
/// must carry a "Systémový monitor" item whose `<command>` runs `x-terminal-emulator -e btop`, and
/// btop must be installed. The live box has both (btop 1.3.0 + the menu item) — the issue body's own
/// acceptance lists "Systémový monitor (btop) + Terminál"; the pre-#791 generated menu had neither,
/// so a fresh box lost the operator's system monitor. The assertion anchors on the generated
/// `<command>` XML form (NOT a bare "-e btop" that a nearby COMMENT would also match — the #791
/// review caught that tautology).
#[test]
fn setup_imag_installs_btop_791() {
    let body = read(SETUP);
    // The GENERATED menu item's <command> must exist (the real provisioned entry, not a comment).
    assert!(
        body.contains("<command>x-terminal-emulator -e btop</command>"),
        "{SETUP}: the GENERATED openbox menu.xml (step 16) must carry the Systémový monitor <command>x-terminal-emulator -e btop</command> (#791 parity, issue body acceptance)"
    );
    // ...and a plain Terminál item, matching the live box + the issue body acceptance.
    assert!(
        body.contains("<command>x-terminal-emulator</command>"),
        "{SETUP}: the GENERATED menu.xml must also carry a Terminál <command>x-terminal-emulator</command> (#791 parity)"
    );
    let install_idx = body
        .find("apt-get install -y openbox lightdm")
        .expect("step-15 kiosk package install present");
    // btop must appear on that SAME install line (before its terminating newline).
    let line_end = body[install_idx..]
        .find('\n')
        .map(|o| install_idx + o)
        .unwrap_or(body.len());
    assert!(
        body[install_idx..line_end].contains("btop"),
        "{SETUP}: btop must be installed alongside openbox/lightdm/feh/wmctrl (step 15) — the menu's Systémový monitor depends on it (#791)"
    );
}

/// verify-imag.sh must GATE the max-perf persistence: assert the service + script + udev rule
/// exist, and assert the runtime STATE reads performance via the pure `imag_maxperf_state_ok`
/// function. A reprovision that silently lost EPP/turbo persistence must FAIL the gate, not pass.
#[test]
fn verify_imag_gates_the_maxperf_persistence_791() {
    let body = read(VERIFY);
    assert!(
        body.contains("imag-maxperf.service"),
        "{VERIFY} must assert imag-maxperf.service (the #791 perf-persistence gate)"
    );
    assert!(
        body.contains("imag_maxperf_state_ok"),
        "{VERIFY} must call the pure imag_maxperf_state_ok to assert the runtime performance STATE (#791)"
    );
    assert!(
        body.contains("99-imag-maxperf-pm.rules"),
        "{VERIFY} must assert the maxperf udev rule presence (#791)"
    );
}
