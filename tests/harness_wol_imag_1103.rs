//! #1103 -- Wake-on-LAN enablement for imag-nb (the imag counterpart of issue 1053's strih/stream
//! WoL). imag-nb is normally an always-on projection box; after an event it can be powered down /
//! taken away (the issue-1013 scenario), and today it cannot be woken remotely. This locks the
//! four deliverables the same way the rest of this repo's harness suite does:
//!
//! 1. scripts/wol-targets.txt -- the imag-nb row (ip + the live-read USB-NIC MAC), STRUCTURAL.
//! 2. scripts/wake-box.sh     -- table-driven, so `wake-box.sh imag-nb --dry-run` resolves the imag
//!    MAC end-to-end with NO sender code change (no second sender), BEHAVIORAL dry-run.
//! 3. scripts/setup-imag.sh   -- step 1 arms WoL durably via NetworkManager
//!    (`802-3-ethernet.wake-on-lan magic`) on the SAME $CON it already modifies for the static IP,
//!    STRUCTURAL.
//! 4. scripts/verify-imag.sh  -- the pure `imag_wol_enabled_ok` acceptance check (0 iff the
//!    persisted NM value is exactly `magic`) wired in as check (x) BEFORE check (o)'s OBS restart
//!    (#884 ordering), BEHAVIORAL (sourced) + STRUCTURAL.
//!
//! RED before the enablement lands (no imag row, `wake-box.sh imag-nb` fails, no WoL provisioning,
//! no `imag_wol_enabled_ok`); GREEN after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

// ---------------------------------------------------------------------------------------------
// 1. scripts/wol-targets.txt -- the checked-in imag-nb row (live-read MAC)
// ---------------------------------------------------------------------------------------------

/// The imag-nb notebook was RETURNED to the owner on 16.9.2026 (issue 1316): its WoL row is kept
/// ONLY as a retirement comment (the historical ip + MAC stay readable for the day the IMAG role
/// returns on a new notebook, whose MAC will differ) -- an ACTIVE `imag-nb` row must be ABSENT, so
/// nothing can wake a box that no longer exists on the wire.
#[test]
fn wol_targets_table_keeps_imag_nb_only_as_a_retired_comment() {
    let s = read("scripts/wol-targets.txt");
    let active: Vec<&str> = s
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter(|l| l.split_whitespace().next() == Some("imag-nb"))
        .collect();
    assert!(
        active.is_empty(),
        "issue 1316: the retired imag-nb must have NO active wol-targets.txt row: {active:?}"
    );
    assert!(
        s.contains("imag-nb RETIRED") && s.contains("1316"),
        "issue 1316: the retirement comment (imag-nb RETIRED ... issue 1316) must stay in the table"
    );
    // the historical MAC survives as a comment for the re-provisioning day
    assert!(
        s.contains("6C:1F:F7:66:15:4B"),
        "#1103: keep the last live-read imag-nb MAC readable in the retirement comment"
    );
}

// ---------------------------------------------------------------------------------------------
// 2. scripts/wake-box.sh -- table-driven end-to-end: a retired box FAILS LOUD, never a packet
// ---------------------------------------------------------------------------------------------

#[test]
fn wake_box_dry_run_refuses_the_retired_imag_nb_loudly() {
    let out = Command::new("bash")
        .current_dir(manifest_dir())
        .args(["scripts/wake-box.sh", "imag-nb", "--dry-run"])
        .output()
        .expect("run wake-box.sh");
    let so = String::from_utf8_lossy(&out.stdout);
    let se = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "issue 1316: `wake-box.sh imag-nb --dry-run` must FAIL once the row is retired:\n{so}"
    );
    assert!(
        se.contains("unknown box imag-nb"),
        "issue 1316: the refusal must name the box (wol_table_lookup: unknown box imag-nb):\n{se}"
    );
    assert!(
        !so.contains("mac=") && !so.contains("DRY-RUN"),
        "issue 1316: no MAC may resolve and no send path may be reached for a retired box:\n{so}"
    );
}

/// The table-driven sender itself is unchanged (#1103 contract) -- proven on a LIVE row.
#[test]
fn wake_box_dry_run_still_resolves_a_live_row_from_the_table() {
    let out = Command::new("bash")
        .current_dir(manifest_dir())
        .args(["scripts/wake-box.sh", "strih", "--dry-run"])
        .output()
        .expect("run wake-box.sh");
    let so = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "#1103: `wake-box.sh strih --dry-run` must succeed for a live table row: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        so.contains("mac=") && so.contains("102 bytes"),
        "#1103: MAC resolved from the table + the 102-byte magic packet reported:\n{so}"
    );
    assert!(
        so.contains("10.77.9.255:9") && so.contains("255.255.255.255:9"),
        "#1103: dry-run must target the subnet + limited broadcast on port 9:\n{so}"
    );
    assert!(
        so.contains("DRY-RUN: no packet sent"),
        "#1103: dry-run must send nothing:\n{so}"
    );
}

// ---------------------------------------------------------------------------------------------
// 3. scripts/setup-imag.sh -- WoL armed durably via NetworkManager in step 1
// ---------------------------------------------------------------------------------------------

#[test]
fn setup_imag_arms_wol_via_nm_on_the_static_ip_connection() {
    let s = read("scripts/setup-imag.sh");
    // NM re-applies this on every connection-up (every boot) -> survives reboot; and it is set on
    // the SAME $CON step 1 already modifies for the static IP, keeping ONE source of truth.
    assert!(
        s.contains(r#"nmcli con mod "$CON" 802-3-ethernet.wake-on-lan magic"#),
        "#1103: setup-imag.sh must arm WoL via NM on the step-1 $CON \
         (`nmcli con mod \"$CON\" 802-3-ethernet.wake-on-lan magic ...`)"
    );
    // it belongs INSIDE step 1 (static IP / NIC discovery), before the #486 network-perf step -- so
    // the single step-1 `nmcli con up "$CON"` applies both the IP and the WoL setting.
    let wol = s
        .find("802-3-ethernet.wake-on-lan magic")
        .expect("#1103: the WoL nmcli line must exist");
    let net = s
        .find("Network performance tuning (#486)")
        .expect("the #486 network-performance step must exist");
    assert!(
        wol < net,
        "#1103: WoL arming must land inside step 1 (before the #486 network-performance step)"
    );
}

// ---------------------------------------------------------------------------------------------
// 4. scripts/verify-imag.sh -- pure imag_wol_enabled_ok check + (x) wiring before (o)
// ---------------------------------------------------------------------------------------------

/// Source verify-imag.sh (its BASH_SOURCE != $0 guard skips the live SSH/WS flow) and run `body`
/// against its pure functions; returns whether the command succeeded. Pure -- no network.
fn run_verify_sourced(body: &str) -> bool {
    let script = manifest_dir().join("scripts/verify-imag.sh");
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", script)
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness")
        .status
        .success()
}

#[test]
fn imag_wol_enabled_ok_accepts_magic_rejects_anything_else() {
    // magic (the persisted `nmcli -g 802-3-ethernet.wake-on-lan` value) is the ONLY pass.
    assert!(
        run_verify_sourced("imag_wol_enabled_ok magic"),
        "#1103: exactly `magic` must pass"
    );
    assert!(
        run_verify_sourced("imag_wol_enabled_ok '  magic  '"),
        "#1103: surrounding whitespace must be tolerated"
    );
    // `default`/`none`/empty = WoL not provisioned; `g` is the ethtool runtime word, not the NM
    // value; `magic secureon` = password-protected wake our passwordless sender cannot trigger.
    for bad in ["default", "none", "", "0", "g", "magic secureon"] {
        assert!(
            !run_verify_sourced(&format!("imag_wol_enabled_ok '{bad}'")),
            "#1103: {bad:?} must FAIL (WoL not armed as exactly `magic`)"
        );
    }
}

#[test]
fn verify_imag_wol_check_is_wired_before_the_o_restart() {
    let s = read("scripts/verify-imag.sh");
    assert!(
        s.contains("imag_wol_enabled_ok"),
        "#1103: verify-imag.sh must define + call imag_wol_enabled_ok"
    );
    // the durable source of truth is the persisted NM value, read sudo-lessly via nmcli -g.
    assert!(
        s.contains("802-3-ethernet.wake-on-lan"),
        "#1103: verify-imag.sh must read the persisted NM WoL value (nmcli -g 802-3-ethernet.wake-on-lan)"
    );
    // #884 ordering: a WoL read reflects boot-time state, so the check must sit BEFORE check (o),
    // whose OBS restart replaces the tracked process (and would move any later check off boot state).
    let x = s
        .find("# (x)")
        .expect("#1103: the WoL acceptance check labelled (x) must exist");
    let o = s
        .find("# (o) both projectors")
        .expect("check (o) must exist");
    assert!(
        x < o,
        "#1103: the (x) WoL check must sit BEFORE check (o)'s OBS restart (#884 ordering)"
    );
}

// ---------------------------------------------------------------------------------------------
// 5. scripts/wake-box.sh -- its OWN usage/help documents imag-nb as a target
// ---------------------------------------------------------------------------------------------

#[test]
fn wake_box_usage_lists_imag_nb_as_a_target() {
    // #1103 added imag-nb as a wake target (the wol-targets.txt row + table-driven send). The
    // sender's OWN usage/help must document it too, not just the original strih | stream, so a
    // `wake-box.sh --help` actually lists the imag box a recovery run would target.
    let s = read("scripts/wake-box.sh");
    assert!(
        s.contains("strih | stream | imag-nb"),
        "#1103: wake-box.sh usage/help must list imag-nb alongside strih | stream"
    );
}
