//! dantesync issue 52 (camera-box provisioning half) -- DSCP-mark the Linux NTP CLIENT's outgoing
//! requests via an nftables OUTPUT-mangle rule installed by provisioning.
//!
//! dantesync's Linux NTP client (`rsntp::SntpClient`) creates its UDP socket internally, so the
//! process owns no handle to `setsockopt(IP_TOS)` -- it can EF-mark the master's REPLIES
//! (`src/dscp.rs`) but NOT its own client REQUESTS on Linux. The venue MikroTik CRS switches honour
//! DSCP in hardware (TRUST-L3), so the request direction must be marked too. `scripts/lib/dscp-nft.sh`
//! is the single source of truth: a dedicated `table ip dantesync_dscp` OUTPUT-mangle rule (DSCP EF)
//! applied at boot by a `dantesync-dscp.service` oneshot, wired into all three provisioning scripts
//! (setup-device.sh STEP 16 pkg + STEP 17c install, verify-device.sh's (ae) acceptance check,
//! create-usb-linux.sh's base-image mirror).
//!
//! These tests (a) source the REAL lib for its pure content-generator / parser / verdict functions,
//! and (b) static-anchor the three consumers -- the same convention as
//! `tests/harness_udev_camera_box_894.rs` / `tests/verify_device_pure_functions.rs`. Tier-0: no
//! `nft`/root needed -- the GREEN fixture is the EXACT live-rendered `nft list` output.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    let p = manifest_dir().join("scripts/lib/dscp-nft.sh");
    assert!(p.exists(), "{} not found", p.display());
    p
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source the real lib and run `body`, returning stdout. Asserts the harness itself exited 0.
fn run_sourced(body: &str) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib_script())
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "sourced harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Source the lib, call a 0/1-returning predicate with its argument passed via env var (never
/// interpolated into the bash -c script text), return its exit code as a bool.
fn predicate(func: &str, arg: &str) -> bool {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{func} \"$ARG\"");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib_script())
        .env("ARG", arg)
        .output()
        .expect("failed to run bash harness");
    out.status.success()
}

/// Source the lib, run `func "$ARG"`, return trimmed stdout (arg via env, never interpolated).
fn call_over_arg(func: &str, arg: &str) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{func} \"$ARG\"");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib_script())
        .env("ARG", arg)
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "call_over_arg {func} exited non-zero: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// The EXACT live-rendered `nft list table ip dantesync_dscp` output (captured on Ubuntu 24.04,
// nft v1.0.9). This is the GREEN fixture the parser must accept.
const REAL_NFT_RENDER: &str = "table ip dantesync_dscp {\n\tchain output {\n\t\ttype filter hook output priority mangle; policy accept;\n\t\tudp dport 123 ip dscp set ef\n\t}\n}\n";

fn flattened(s: &str) -> String {
    s.replace('\n', "|")
}

// -------------------------------------------------------------------------------------------
// content generators
// -------------------------------------------------------------------------------------------

#[test]
fn ruleset_content_is_a_dedicated_mangle_table_never_a_flush() {
    let rs = run_sourced("dscp_nft_ruleset_content");
    assert!(
        rs.contains("table ip dantesync_dscp {"),
        "must define the dedicated table: {rs}"
    );
    assert!(
        rs.contains("delete table ip dantesync_dscp"),
        "must carry the idempotent table/delete-table replace pair: {rs}"
    );
    assert!(
        rs.contains("type filter hook output priority mangle; policy accept;"),
        "output chain must be an OUTPUT-mangle chain with policy accept (never drops): {rs}"
    );
    assert!(
        rs.contains("udp dport 123 ip dscp set ef"),
        "must mark udp dport 123 with DSCP EF: {rs}"
    );
    // Must carry no actual `flush ruleset` DIRECTIVE -- a comment merely explaining the choice
    // ("NEVER `flush ruleset`") is fine, so match a non-comment flush command, not the substring.
    assert!(
        !rs.lines().any(|l| {
            let t = l.trim_start();
            !t.starts_with('#') && t.starts_with("flush ")
        }),
        "must NEVER flush the whole ruleset -- a dedicated table coexists with any firewall: {rs}"
    );
}

#[test]
fn service_unit_is_a_boot_enabled_oneshot_applying_the_ruleset() {
    let un = run_sourced("dscp_nft_service_unit_content");
    assert!(un.contains("Type=oneshot"), "must be a oneshot: {un}");
    assert!(
        un.contains("RemainAfterExit=yes"),
        "RemainAfterExit=yes so `systemctl is-active` reads active after apply: {un}"
    );
    assert!(
        un.contains("ExecStart=/usr/sbin/nft -f /etc/nftables.d/dantesync-dscp.nft"),
        "ExecStart must apply the ruleset file the lib writes: {un}"
    );
    assert!(
        un.contains("WantedBy=multi-user.target"),
        "must be pulled in at boot (reboot survival via enable): {un}"
    );
    // Lock the early-boot firewall-slot ordering so a future edit can't silently drop it.
    assert!(
        un.contains("Wants=network-pre.target") && un.contains("Before=network-pre.target"),
        "must order before network-pre.target so the rule is in place from the first packet: {un}"
    );
}

// -------------------------------------------------------------------------------------------
// dscp_nft_rule_present -- RED -> GREEN over the exact live render
// -------------------------------------------------------------------------------------------

#[test]
fn rule_present_accepts_the_exact_live_render_raw_and_flattened() {
    assert!(
        predicate("dscp_nft_rule_present", REAL_NFT_RENDER),
        "raw live render must be accepted"
    );
    assert!(
        predicate("dscp_nft_rule_present", &flattened(REAL_NFT_RENDER)),
        "the `|`-flattened gather form must be accepted too"
    );
}

#[test]
fn rule_present_rejects_absent_wrong_table_and_wrong_port() {
    assert!(
        !predicate("dscp_nft_rule_present", ""),
        "empty ruleset must be rejected"
    );
    assert!(
        !predicate("dscp_nft_rule_present", "__NFT_ABSENT__"),
        "the nft-not-installed sentinel must be rejected"
    );
    assert!(
        !predicate(
            "dscp_nft_rule_present",
            "table ip filter { chain output { udp dport 123 ip dscp set ef } }"
        ),
        "a udp/123 EF rule in a DIFFERENT table must not satisfy the check"
    );
    assert!(
        !predicate(
            "dscp_nft_rule_present",
            "table ip dantesync_dscp { chain output { udp dport 53 ip dscp set ef } }"
        ),
        "our table marking the WRONG port (53) must not satisfy the check"
    );
}

// -------------------------------------------------------------------------------------------
// dscp_nft_verdict -- fail-closed on each facet
// -------------------------------------------------------------------------------------------

#[test]
fn verdict_ok_only_when_rule_live_and_service_enabled_active() {
    let block = format!(
        "NFT_TABLE={}\nDSCP_SVC_ACTIVE=active\nDSCP_SVC_ENABLED=enabled",
        flattened(REAL_NFT_RENDER)
    );
    assert_eq!(call_over_arg("dscp_nft_verdict", &block).trim(), "ok");
}

#[test]
fn verdict_fails_when_nftables_absent() {
    let block = "NFT_TABLE=__NFT_ABSENT__\nDSCP_SVC_ACTIVE=inactive\nDSCP_SVC_ENABLED=disabled";
    let v = call_over_arg("dscp_nft_verdict", block);
    assert!(v.contains("FAIL:"), "must fail: {v}");
    assert!(
        v.contains("nftables"),
        "must name the missing nftables package: {v}"
    );
}

#[test]
fn verdict_fails_when_service_not_active() {
    let block = format!(
        "NFT_TABLE={}\nDSCP_SVC_ACTIVE=inactive\nDSCP_SVC_ENABLED=enabled",
        flattened(REAL_NFT_RENDER)
    );
    let v = call_over_arg("dscp_nft_verdict", &block);
    assert!(
        v.contains("FAIL:") && v.contains("not active"),
        "must fail on inactive oneshot: {v}"
    );
}

#[test]
fn verdict_fails_when_service_not_enabled() {
    let block = format!(
        "NFT_TABLE={}\nDSCP_SVC_ACTIVE=active\nDSCP_SVC_ENABLED=disabled",
        flattened(REAL_NFT_RENDER)
    );
    let v = call_over_arg("dscp_nft_verdict", &block);
    assert!(
        v.contains("FAIL:") && v.contains("not enabled"),
        "must fail when the oneshot will not survive a reboot: {v}"
    );
}

#[test]
fn gather_snippet_reads_the_table_and_the_service_state() {
    let g = run_sourced("dscp_nft_gather_remote_snippet");
    assert!(
        g.contains("nft list table ip dantesync_dscp"),
        "must read the dedicated table: {g}"
    );
    assert!(
        g.contains("__NFT_ABSENT__"),
        "must emit the nft-absent sentinel: {g}"
    );
    assert!(
        g.contains("systemctl is-active dantesync-dscp"),
        "must read the oneshot active state: {g}"
    );
    assert!(
        g.contains("systemctl is-enabled dantesync-dscp"),
        "must read the oneshot enabled state: {g}"
    );
}

// -------------------------------------------------------------------------------------------
// static anchors -- all three provisioning scripts consume the lib (dual/triple bake)
// -------------------------------------------------------------------------------------------

#[test]
fn setup_device_sources_lib_installs_nftables_and_has_step_17c() {
    let s = read("scripts/setup-device.sh");
    assert!(
        s.contains(". \"$HERE/lib/dscp-nft.sh\""),
        "setup-device.sh must source the lib"
    );
    assert!(
        s.contains("ca-certificates psmisc nftables"),
        "STEP 16 must install the nftables package (provides nft for STEP 17c)"
    );
    assert!(s.contains("[17c]"), "must have the STEP 17c banner");
    assert!(
        s.contains("dscp_nft_ruleset_content > \"$DSCP_NFT_RULESET_PATH\""),
        "STEP 17c must write the ruleset file"
    );
    assert!(
        s.contains("dscp_nft_service_unit_content > \"$DSCP_NFT_SERVICE_PATH\""),
        "STEP 17c must write the oneshot unit"
    );
    assert!(
        s.contains("systemctl enable \"$DSCP_NFT_SERVICE_NAME\""),
        "STEP 17c must enable the oneshot (enable-only, effective next boot)"
    );
    // STEP 17c must live in the rw window -- BEFORE STEP 18's ro-root flip.
    let step17c = s.find("[17c]").expect("17c");
    let step18 = s.find("STEP 18: Configure read-only").expect("STEP 18");
    assert!(
        step17c < step18,
        "STEP 17c must run before the STEP 18 ro-root flip (rw window)"
    );
}

#[test]
fn verify_device_sources_lib_and_has_ae_check_before_q() {
    let s = read("scripts/verify-device.sh");
    assert!(
        s.contains(". \"$HERE/lib/dscp-nft.sh\""),
        "verify-device.sh must source the lib"
    );
    let ae = s
        .find("# (ae) NTP-client DSCP marking:")
        .expect("(ae) check block must be present in the live flow");
    // (q) must stay the intentionally-LAST check -- the (ae) exec block precedes it.
    let q = s
        .rfind("# (q) .bak cruft drift")
        .expect("(q) check block must be present");
    assert!(
        ae < q,
        "the (ae) check must precede (q) so (q) remains the last check (ae={ae} q={q})"
    );
    assert!(
        s.contains("dscp_nft_gather_remote_snippet") && s.contains("dscp_nft_verdict"),
        "the (ae) check must consume the lib's gather + verdict functions"
    );
}

#[test]
fn create_usb_sources_lib_bakes_files_and_enables_in_chroot() {
    let s = read("scripts/create-usb-linux.sh");
    assert!(
        s.contains(". \"$SCRIPT_DIR/lib/dscp-nft.sh\""),
        "create-usb-linux.sh must source the lib"
    );
    assert!(
        s.contains("dscp_nft_ruleset_content > \"$MOUNT_ROOT$DSCP_NFT_RULESET_PATH\""),
        "must bake the ruleset file into the base image host-side"
    );
    assert!(
        s.contains("dscp_nft_service_unit_content > \"$MOUNT_ROOT$DSCP_NFT_SERVICE_PATH\""),
        "must bake the oneshot unit into the base image host-side"
    );
    assert!(
        s.contains("    nftables \\"),
        "the chroot apt install must add the nftables package"
    );
    assert!(
        s.contains("systemctl enable dantesync-dscp"),
        "the chroot must enable the dantesync-dscp oneshot"
    );
}

// -------------------------------------------------------------------------------------------
// review 5B4 hardening: numeric-DSCP acceptance + wrong-class rejection + set -e composition
// -------------------------------------------------------------------------------------------

#[test]
fn rule_present_matches_numeric_dscp_and_rejects_wrong_class() {
    // nft renders EF as the keyword `ef` today, but the parser also accepts the numeric forms so a
    // future render variation (0x2e / 46) never false-negatives.
    let hex =
        "table ip dantesync_dscp {\n\tchain output {\n\t\tudp dport 123 ip dscp set 0x2e\n\t}\n}\n";
    let dec =
        "table ip dantesync_dscp {\n\tchain output {\n\t\tudp dport 123 ip dscp set 46\n\t}\n}\n";
    assert!(
        predicate("dscp_nft_rule_present", hex),
        "0x2e (EF) render must be accepted"
    );
    assert!(
        predicate("dscp_nft_rule_present", dec),
        "46 (EF) render must be accepted"
    );
    // Our table marking the WRONG DSCP class must not satisfy the check.
    let wrong_class =
        "table ip dantesync_dscp {\n\tchain output {\n\t\tudp dport 123 ip dscp set cs0\n\t}\n}\n";
    assert!(
        !predicate("dscp_nft_rule_present", wrong_class),
        "a non-EF DSCP class (cs0) must be rejected"
    );
}

#[test]
fn verdict_composes_without_aborting_under_set_e() {
    // verify-device.sh runs with `set -euo pipefail`; dscp_nft_verdict must return 0 on every path
    // (incl. the multi-FAIL path) so a no-match grep inside it never aborts the caller -- the
    // #1133-class regression the repo just learned. Source under the SAME strict options here.
    let script = lib_script();
    let cases = [
        (
            format!(
                "NFT_TABLE={}\nDSCP_SVC_ACTIVE=active\nDSCP_SVC_ENABLED=enabled",
                flattened(REAL_NFT_RENDER)
            ),
            true,
        ),
        (
            "NFT_TABLE=__NFT_ABSENT__\nDSCP_SVC_ACTIVE=inactive\nDSCP_SVC_ENABLED=disabled"
                .to_string(),
            false,
        ),
    ];
    for (block, expect_ok) in cases {
        let out = Command::new("bash")
            .arg("-c")
            .arg("set -euo pipefail\n. \"$SCRIPT\"\nv=\"$(dscp_nft_verdict \"$ARG\")\"\nprintf '%s' \"$v\"")
            .env("SCRIPT", &script)
            .env("ARG", &block)
            .output()
            .expect("failed to run bash harness");
        assert!(
            out.status.success(),
            "dscp_nft_verdict must not abort under set -euo pipefail: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v = String::from_utf8_lossy(&out.stdout);
        if expect_ok {
            assert_eq!(v.trim(), "ok");
        } else {
            assert!(
                v.contains("FAIL:"),
                "expected FAIL reasons under set -e: {v}"
            );
        }
    }
}
