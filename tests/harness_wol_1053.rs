//! #1053 -- Wake-on-LAN remote-recovery tooling for strih + stream (the recovery counterpart to
//! issue 1001's outage-detection watchdog). Locks the fix's pieces the same way the rest of this
//! repo's harness suite does: BEHAVIORAL where the logic is pure (scripts/lib/wol.sh -- MAC
//! normalization, the 102-byte magic packet, the box->ip->mac table lookup, all run by sourcing the
//! lib with no network), and STRUCTURAL where a live rig/ssh would otherwise be needed (the
//! wake-box.sh sender wiring, the enable-nic-wol.ps1 NIC helper contract, the target table).
//!
//! 1. scripts/lib/wol.sh          -- pure functions, tested behaviorally (RED without the lib, GREEN with it).
//! 2. scripts/wake-box.sh         -- dev1-side magic-packet sender (sources wol.sh; python3 SO_BROADCAST send).
//! 3. scripts/enable-nic-wol.ps1  -- Windows NIC WoL enable+verify helper (idempotent, fail-loud, dry-run).
//! 4. scripts/wol-targets.txt     -- the checked-in box->ip->mac table.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source scripts/lib/wol.sh and run `code`, returning (stdout, exit_ok). Pure -- no network.
fn run_wol(code: &str) -> (String, bool) {
    let out = Command::new("bash")
        .current_dir(manifest_dir())
        .arg("-c")
        .arg(format!(". scripts/lib/wol.sh; {code}"))
        .output()
        .expect("spawn bash");
    (
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        out.status.success(),
    )
}

// ---------------------------------------------------------------------------------------------
// 1. scripts/lib/wol.sh -- BEHAVIORAL (pure logic)
// ---------------------------------------------------------------------------------------------

#[test]
fn wol_lib_defines_the_three_pure_functions() {
    let s = read("scripts/lib/wol.sh");
    for f in [
        "wol_normalize_mac",
        "wol_magic_packet_hex",
        "wol_table_lookup",
    ] {
        assert!(
            s.contains(&format!("{f}()")),
            "#1053: scripts/lib/wol.sh must define {f}()"
        );
    }
    // source-only: no top-level side effects, and no `set -euo pipefail` leaking into the caller.
    assert!(
        !s.contains("\nset -euo pipefail"),
        "#1053: wol.sh is source-only, must not set strict mode"
    );
}

#[test]
fn wol_lib_is_sourceable_without_side_effects() {
    let (out, ok) = run_wol("echo SOURCED_OK");
    assert!(
        ok && out == "SOURCED_OK",
        "#1053: wol.sh must source cleanly with no side effects, got {out:?}"
    );
}

#[test]
fn normalize_mac_canonicalizes_every_accepted_form() {
    for form in [
        "5c-6a-80-f6-6c-f7",
        "5C:6A:80:F6:6C:F7",
        "5c6a80f66cf7",
        "5c6a.80f6.6cf7",
    ] {
        let (out, ok) = run_wol(&format!("wol_normalize_mac {form}"));
        assert!(ok, "#1053: {form} should normalize ok");
        assert_eq!(
            out, "5C:6A:80:F6:6C:F7",
            "#1053: {form} -> canonical uppercase colon form"
        );
    }
}

#[test]
fn normalize_mac_fails_loud_on_garbage() {
    for bad in [
        "zz:6a:80:f6:6c:f7",
        "5c:6a:80:f6:6c",
        "5c6a80f66cf7aa",
        "",
        "hello",
    ] {
        let (_out, ok) = run_wol(&format!("wol_normalize_mac {bad} 2>/dev/null"));
        assert!(
            !ok,
            "#1053: invalid MAC {bad:?} must FAIL (non-zero), never be coerced"
        );
    }
}

#[test]
fn magic_packet_is_6xff_then_16x_the_mac() {
    let (out, ok) = run_wol("wol_magic_packet_hex 5C:6A:80:F6:6C:F7");
    assert!(ok, "#1053: magic packet build should succeed");
    // 102 bytes = 204 hex chars: 6x 0xFF sync stream + 16x the 6-byte MAC.
    assert_eq!(
        out.len(),
        204,
        "#1053: magic packet must be 102 bytes (204 hex chars), got {}",
        out.len()
    );
    assert!(
        out.starts_with("ffffffffffff"),
        "#1053: must start with the 6-byte 0xFF sync stream"
    );
    let mac = "5c6a80f66cf7";
    let body = &out[12..];
    assert_eq!(body.len(), 192, "#1053: 16 MAC repetitions = 96 bytes");
    for i in 0..16 {
        assert_eq!(
            &body[i * 12..(i + 1) * 12],
            mac,
            "#1053: repetition {i} must equal the MAC"
        );
    }
}

#[test]
fn magic_packet_propagates_a_bad_mac_failure() {
    let (_out, ok) = run_wol("wol_magic_packet_hex not-a-mac 2>/dev/null");
    assert!(
        !ok,
        "#1053: an invalid MAC must fail the magic-packet build, never emit a bogus packet"
    );
}

#[test]
fn table_lookup_resolves_ip_mac_and_derived_broadcast() {
    let tbl = "# c\\nstrih 10.77.9.202 5C:6A:80:F6:6C:F7\\nstream 10.77.9.204 E8:9C:25:CE:B6:EA";
    let setup = format!("TBL=$(printf '%b' \"{tbl}\"); ");
    for (box_, field, want) in [
        ("strih", "ip", "10.77.9.202"),
        ("strih", "mac", "5C:6A:80:F6:6C:F7"),
        ("strih", "broadcast", "10.77.9.255"),
        ("stream", "broadcast", "10.77.9.255"),
    ] {
        let (out, ok) = run_wol(&format!("{setup}wol_table_lookup \"$TBL\" {box_} {field}"));
        assert!(ok, "#1053: lookup {box_}/{field} should succeed");
        assert_eq!(out, want, "#1053: {box_}/{field}");
    }
}

#[test]
fn table_lookup_fails_loud_on_unknown_box_or_field() {
    let setup = "TBL=$(printf '%b' 'strih 10.77.9.202 5C:6A:80:F6:6C:F7'); ";
    let (_o, ok1) = run_wol(&format!(
        "{setup}wol_table_lookup \"$TBL\" nosuch ip 2>/dev/null"
    ));
    assert!(!ok1, "#1053: unknown box must fail");
    let (_o, ok2) = run_wol(&format!(
        "{setup}wol_table_lookup \"$TBL\" strih nosuch 2>/dev/null"
    ));
    assert!(!ok2, "#1053: unknown field must fail");
}

// ---------------------------------------------------------------------------------------------
// 2. scripts/wake-box.sh -- STRUCTURAL + dry-run behavioral
// ---------------------------------------------------------------------------------------------

#[test]
fn wake_box_sources_the_pure_lib_and_sends_via_broadcast() {
    let s = read("scripts/wake-box.sh");
    assert!(
        s.contains("set -euo pipefail"),
        "#1053: wake-box.sh must set strict mode"
    );
    assert!(
        s.contains("lib/wol.sh"),
        "#1053: wake-box.sh must source the pure lib"
    );
    assert!(
        s.contains("wol_magic_packet_hex"),
        "#1053: must build the packet via the pure lib"
    );
    // the impure send must set SO_BROADCAST (bash /dev/udp cannot) -- no new system dep, python3 only.
    assert!(
        s.contains("SO_BROADCAST"),
        "#1053: the UDP send must set SO_BROADCAST for the magic packet"
    );
    assert!(
        !s.contains("wakeonlan") && !s.contains("etherwake"),
        "#1053: no new system dependency -- python3 SO_BROADCAST send only"
    );
    assert!(
        s.contains("--dry-run"),
        "#1053: wake-box.sh must offer --dry-run"
    );
}

#[test]
fn wake_box_dry_run_resolves_target_without_sending() {
    let out = Command::new("bash")
        .current_dir(manifest_dir())
        .args(["scripts/wake-box.sh", "strih", "--dry-run"])
        .output()
        .expect("run wake-box.sh");
    let so = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "#1053: dry-run must succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        so.contains("mac=5C:6A:80:F6:6C:F7"),
        "#1053: strih MAC resolved from the table:\n{so}"
    );
    assert!(
        so.contains("102 bytes"),
        "#1053: 102-byte packet reported:\n{so}"
    );
    assert!(
        so.contains("10.77.9.255:9") && so.contains("255.255.255.255:9"),
        "#1053: dry-run must target subnet + limited broadcast on port 9:\n{so}"
    );
    assert!(
        so.contains("DRY-RUN: no packet sent"),
        "#1053: dry-run must send nothing:\n{so}"
    );
}

#[test]
fn wake_box_fails_loud_on_unknown_box() {
    let out = Command::new("bash")
        .current_dir(manifest_dir())
        .args(["scripts/wake-box.sh", "nosuchbox", "--dry-run"])
        .output()
        .expect("run wake-box.sh");
    assert!(
        !out.status.success(),
        "#1053: unknown box must fail loud, not silently no-op"
    );
}

// ---------------------------------------------------------------------------------------------
// 3. scripts/enable-nic-wol.ps1 -- STRUCTURAL contract (runs on the Windows box; no local pwsh)
// ---------------------------------------------------------------------------------------------

#[test]
fn enable_nic_wol_ps1_contract() {
    let s = read("scripts/enable-nic-wol.ps1");
    // fail-loud
    assert!(
        s.contains("$ErrorActionPreference = 'Stop'")
            || s.contains("$ErrorActionPreference='Stop'"),
        "#1053: enable-nic-wol.ps1 must fail loud (ErrorActionPreference Stop)"
    );
    // the three modes: apply (default) + dry-run + verify-only
    assert!(s.contains("[switch]$DryRun"), "#1053: must offer -DryRun");
    assert!(
        s.contains("[switch]$VerifyOnly"),
        "#1053: must offer -VerifyOnly"
    );
    // the WoL-critical properties it enforces + verifies
    assert!(
        s.contains("*WakeOnMagicPacket"),
        "#1053: must enforce the magic-packet wake property"
    );
    assert!(
        s.contains("Set-NetAdapterAdvancedProperty"),
        "#1053: must set NIC advanced properties"
    );
    assert!(
        s.contains("Get-NetAdapterPowerManagement"),
        "#1053: must read the power-management state"
    );
    assert!(
        s.contains("wake_armed"),
        "#1053: must verify the NIC is armed to wake (powercfg)"
    );
    // idempotent: reads current value before deciding to change
    assert!(
        s.contains("Get-NetAdapterAdvancedProperty"),
        "#1053: idempotent -- read current before change"
    );
    // applying needs elevation (fail-loud when not admin)
    assert!(
        s.to_lowercase().contains("administrator"),
        "#1053: applying must require an elevated session"
    );
}

// ---------------------------------------------------------------------------------------------
// 4. scripts/wol-targets.txt -- the checked-in table (matches the live-read MACs)
// ---------------------------------------------------------------------------------------------

#[test]
fn wol_targets_table_has_both_boxes_with_live_macs() {
    let s = read("scripts/wol-targets.txt");
    // strih + stream rows, IPs per targets.md, MACs live-read from each box's active NIC 2026-08-17.
    assert!(
        s.contains("strih") && s.contains("10.77.9.202") && s.contains("5C:6A:80:F6:6C:F7"),
        "#1053: strih row (ip + live MAC)"
    );
    assert!(
        s.contains("stream") && s.contains("10.77.9.204") && s.contains("E8:9C:25:CE:B6:EA"),
        "#1053: stream row (ip + live MAC)"
    );
}
