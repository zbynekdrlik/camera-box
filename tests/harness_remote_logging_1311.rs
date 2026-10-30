//! camera-box #1311 (Finding 1 step 2) -- get kernel + journal messages OFF the cambox in REAL TIME.
//!
//! Two dead boot sticks in 24 h (cam1 13.9., cam2 14.9.) went half-dead (#1309): the root fs USB
//! stick dropped off the bus while RAM-resident daemons kept running. `/var/log` is a tmpfs and
//! `rsyslog` is PURGED (#762), so the ONLY durable log is the #1309 on-STICK persistent-journal
//! partition -- which dies WITH the stick. The next death must be diagnosable, so messages have to
//! leave the box before the fs is needed. `scripts/lib/remote-logging.sh` is the single source of
//! truth: netconsole (kernel printk over UDP, survives the death instant) + systemd-journal-upload
//! (the rich journal), both to the box's OWN venue strih (issue 1394: the sink is resolved from the
//! venue fact files, never dev1), wired into setup-device.sh ([remote-logging] + STEP 16 pkg) and
//! verify-device.sh ((ak) acceptance check). The receivers live on the strih box (setup-strih 16k).
//!
//! These tests (a) source the REAL lib for its pure MAC parser / content generators / fail-closed
//! verdict, and (b) static-anchor the consumers -- the same convention as
//! `tests/harness_dscp_nft_ds52.rs` / `tests/harness_mgmt_liveness_1309.rs`. Tier-0: no kernel / root
//! / network needed; the GREEN fixture is the exact live-rendered gather output.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    let p = manifest_dir().join("scripts/lib/remote-logging.sh");
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

// A fully-healthy gathered state (the exact KEY=VALUE shape remote_log_gather_remote_snippet emits).
const GREEN_BLOCK: &str = "NC_SVC_ENABLED=enabled\nNC_SVC_ACTIVE=active\nNC_SCRIPT_X=yes\nNC_ENABLED=1\nNC_REMOTE_IP=10.77.9.202\nNC_REMOTE_PORT=514\nJU_SVC_ENABLED=enabled\nJU_URL=http://10.77.9.202:19532\nJU_STATE_SAVE=--save-state=/run/systemd/journal-upload/state\nCONSOLE_LOGLEVEL=7\nKMSG_REDIRECT=12";

// The box's venue strih sink the GREEN block points at (issue 1394: strih-snv, cam2/cam3/cam5).
const SINK: &str = "10.77.9.202";

/// Source the lib and run `remote_log_verdict "$ARG" "$SINK"` (both via env, never interpolated).
fn verdict(block: &str) -> String {
    let harness = "set -uo pipefail\n. \"$SCRIPT\"\nremote_log_verdict \"$ARG\" \"$SINK\"";
    let out = Command::new("bash")
        .arg("-c")
        .arg(harness)
        .env("SCRIPT", lib_script())
        .env("ARG", block)
        .env("SINK", SINK)
        .output()
        .expect("failed to run bash harness");
    assert!(out.status.success(), "verdict harness exited non-zero");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// -------------------------------------------------------------------------------------------
// pure MAC parser
// -------------------------------------------------------------------------------------------

#[test]
fn mac_parser_extracts_a_resolved_lladdr_lowercased() {
    assert_eq!(
        call_over_arg(
            "remote_log_mac_from_neigh",
            "10.77.9.200 dev enp2s0 lladdr AA:BB:CC:DD:EE:F0 REACHABLE"
        )
        .trim(),
        "aa:bb:cc:dd:ee:f0"
    );
    assert_eq!(
        call_over_arg(
            "remote_log_mac_from_neigh",
            "10.77.9.200 dev enp2s0 lladdr 11:22:33:44:55:66 STALE"
        )
        .trim(),
        "11:22:33:44:55:66"
    );
}

#[test]
fn mac_parser_returns_empty_for_unresolved_or_absent() {
    // FAILED / INCOMPLETE == not yet resolved -> empty so the boot oneshot keeps retrying, never
    // writing a bogus MAC.
    assert_eq!(
        call_over_arg("remote_log_mac_from_neigh", "10.77.9.200 dev enp2s0 FAILED").trim(),
        ""
    );
    assert_eq!(
        call_over_arg(
            "remote_log_mac_from_neigh",
            "10.77.9.200 dev enp2s0  INCOMPLETE"
        )
        .trim(),
        ""
    );
    assert_eq!(call_over_arg("remote_log_mac_from_neigh", "").trim(), "");
}

// -------------------------------------------------------------------------------------------
// content generators
// -------------------------------------------------------------------------------------------

#[test]
fn netconsole_setup_script_embeds_the_mac_parser_and_arms_the_target() {
    let s = run_sourced("remote_log_netconsole_setup_script_content 10.77.9.202");
    // ONE source of truth: the pure parser is embedded via `declare -f` (renders `name ()`).
    assert!(
        s.contains("remote_log_mac_from_neigh ()"),
        "setup script must embed the mac parser via declare -f: {s}"
    );
    assert!(
        s.contains("ip neigh show"),
        "setup script must resolve the sink's MAC from the neigh cache: {s}"
    );
    assert!(
        s.contains("echo \"$SINK_IP\" > \"$CFG/remote_ip\"")
            && s.contains("echo 1 > \"$CFG/enabled\""),
        "setup script must configure + enable the dynamic configfs target: {s}"
    );
    // set -uo pipefail (never set -e -- the configfs writes are deliberately best-effort || true).
    assert!(
        s.contains("set -uo pipefail") && !s.contains("set -euo pipefail"),
        "setup script must be set -uo (not -e): {s}"
    );
    // #1311 review F1: a failed arm must make the oneshot exit non-zero so `systemctl is-active` is
    // truthful (under set -uo a failed configfs write does not abort on its own).
    assert!(
        s.contains("arm FAILED") && s.contains("cat \"$CFG/enabled\""),
        "setup script must verify the target actually enabled and exit non-zero otherwise (F1): {s}"
    );
}

#[test]
fn netconsole_service_is_a_boot_enabled_oneshot_after_network_online() {
    let u = run_sourced("remote_log_netconsole_service_unit_content");
    assert!(u.contains("Type=oneshot"), "must be a oneshot: {u}");
    assert!(
        u.contains("RemainAfterExit=yes"),
        "must RemainAfterExit: {u}"
    );
    assert!(
        u.contains("ExecStart=/usr/local/sbin/cambox-netconsole-setup.sh"),
        "must run the setup script: {u}"
    );
    assert!(
        u.contains("After=network-online.target") && u.contains("Wants=network-online.target"),
        "must order after network-online: {u}"
    );
    assert!(
        u.contains("WantedBy=multi-user.target"),
        "must be boot-enabled: {u}"
    );
}

#[test]
fn journal_upload_conf_points_at_the_venue_strih_sink() {
    let c = run_sourced("remote_log_journal_upload_conf_content 10.77.9.202");
    assert!(c.contains("[Upload]"), "must be an [Upload] section: {c}");
    assert!(
        c.contains("URL=http://10.77.9.202:19532"),
        "must point at the given sink's journal-remote: {c}"
    );
}

#[test]
fn journal_upload_dropin_redirects_the_cursor_to_run_for_the_ro_root() {
    let d = run_sourced("remote_log_journal_upload_dropin_content");
    // Clears the stock ExecStart then re-states it with a /run save-state (the default /var/lib is
    // on the read-only root).
    assert!(
        d.contains("ExecStart=\n") || d.contains("ExecStart=\r\n"),
        "must clear the stock ExecStart first: {d}"
    );
    assert!(
        d.contains("--save-state=/run/systemd/journal-upload/state"),
        "must redirect the cursor to a /run tmpfs path: {d}"
    );
    assert!(
        d.contains("RuntimeDirectory=systemd/journal-upload"),
        "must create the /run cursor dir with the service user's ownership: {d}"
    );
    // #1311 review F2: clear the stock unit's StateDirectory (under /var/lib, on the ro root) so the
    // unit does not fail trying to mkdir there regardless of the --save-state override.
    assert!(
        d.contains("StateDirectory="),
        "must clear the stock StateDirectory= so the ro-root /var/lib mkdir cannot fail the unit (F2): {d}"
    );
}

// -------------------------------------------------------------------------------------------
// fail-closed verdict
// -------------------------------------------------------------------------------------------

#[test]
fn verdict_ok_only_when_every_facet_is_healthy() {
    assert_eq!(verdict(GREEN_BLOCK).trim(), "ok");
}

#[test]
fn verdict_fails_closed_on_missing_or_wrong_facets() {
    // empty -> fail-closed (never read as "safely logging")
    let v = verdict("");
    assert!(v.contains("FAIL:"), "empty must fail-closed: {v}");

    // netconsole target absent
    let no_target = GREEN_BLOCK.replace("NC_ENABLED=1", "NC_ENABLED=__NO_TARGET__");
    let v = verdict(&no_target);
    assert!(
        v.contains("no netconsole configfs target"),
        "must fail when the target is absent: {v}"
    );

    // wrong remote_ip
    let wrong_ip = GREEN_BLOCK.replace("NC_REMOTE_IP=10.77.9.202", "NC_REMOTE_IP=10.77.9.201");
    let v = verdict(&wrong_ip);
    assert!(
        v.contains("!= the venue strih sink"),
        "must fail on a wrong remote_ip: {v}"
    );

    // netconsole service not enabled
    let nc_off = GREEN_BLOCK.replace("NC_SVC_ENABLED=enabled", "NC_SVC_ENABLED=disabled");
    let v = verdict(&nc_off);
    assert!(
        v.contains("not enabled"),
        "must fail when netconsole is not enabled: {v}"
    );

    // journal-upload cursor not redirected to /run (would be unwritable on ro root)
    let bad_state = GREEN_BLOCK.replace(
        "--save-state=/run/systemd/journal-upload/state",
        "--save-state=/var/lib/systemd/journal-upload/state",
    );
    let v = verdict(&bad_state);
    assert!(
        v.contains("not redirected to /run"),
        "must fail when the cursor is not on /run: {v}"
    );

    // journal-upload URL wrong
    let bad_url = GREEN_BLOCK.replace(
        "JU_URL=http://10.77.9.202:19532",
        "JU_URL=http://10.77.9.202:9999",
    );
    let v = verdict(&bad_url);
    assert!(
        v.contains("journal-upload URL"),
        "must fail on a wrong URL: {v}"
    );
}

#[test]
fn verdict_does_not_gate_journal_upload_active_state() {
    // journal-upload's ACTIVE state depends on the venue strih's receiver (setup-strih 16k); a
    // cambox is correctly provisioned even before the sink exists. The GREEN block carries no
    // JU_SVC_ACTIVE line at all, and it must still be "ok".
    assert!(
        !GREEN_BLOCK.contains("JU_SVC_ACTIVE"),
        "the healthy fixture must not need a journal-upload active line"
    );
    assert_eq!(verdict(GREEN_BLOCK).trim(), "ok");
}

// -------------------------------------------------------------------------------------------
// static anchors: the consumers all wire the SAME source of truth
// -------------------------------------------------------------------------------------------

#[test]
fn setup_device_installs_and_enables_both_transports() {
    let s = read("scripts/setup-device.sh");
    assert!(
        s.contains(". \"$HERE/lib/remote-logging.sh\""),
        "setup-device.sh must source the lib"
    );
    assert!(
        s.contains("[remote-logging]"),
        "setup-device.sh must have the [remote-logging] install sub-step"
    );
    assert!(
        s.contains("remote_log_netconsole_setup_script_content \"$REMOTE_LOG_SINK_IP\" > \"$REMOTE_LOG_NC_SCRIPT_PATH\""),
        "setup-device.sh must write the netconsole setup script"
    );
    assert!(
        s.contains("remote_log_journal_upload_conf_content \"$REMOTE_LOG_SINK_IP\" > \"$REMOTE_LOG_JU_CONF_PATH\""),
        "setup-device.sh must write the journal-upload conf"
    );
    assert!(
        s.contains("systemctl enable \"$REMOTE_LOG_NC_SERVICE_NAME\""),
        "setup-device.sh must enable the netconsole oneshot (enable-only)"
    );
    // STEP 16 must install the package that ships systemd-journal-upload.
    assert!(
        s.contains("systemd-journal-remote"),
        "setup-device.sh STEP 16 must apt-install systemd-journal-remote"
    );
}

/// Issue 1394: the base image has no box identity, so it cannot know the box's venue log sink;
/// setup-device (always run next, with the box name) writes + enables both transports.
#[test]
fn create_usb_bakes_no_box_specific_logging_file_1394() {
    let s = read("scripts/create-usb-linux.sh");
    let code: String = s
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    for word in [
        "remote_log_",
        "REMOTE_LOG_",
        "systemctl enable cambox-netconsole",
        "systemctl enable systemd-journal-upload",
    ] {
        assert!(
            !code.contains(word),
            "create-usb-linux.sh must not carry `{word}`"
        );
    }
    assert!(
        code.contains("systemd-journal-remote"),
        "create-usb-linux.sh chroot still apt-installs systemd-journal-remote"
    );
}

/// cam1 M.2 install 24.9.2026: the base image writes /etc/systemd/journal-upload.conf on the host
/// side BEFORE the chroot `apt-get install ... systemd-journal-remote`. That package ships the same
/// conffile, so dpkg asked "keep or replace?", read EOF on stdin, failed the configure step and the
/// whole install aborted ("end of file on stdin at conffile prompt"). The chroot setup script must
/// keep the pre-baked conffiles (--force-confdef + --force-confold) before its FIRST apt-get install.
#[test]
fn create_usb_chroot_keeps_prebaked_conffiles_before_the_first_apt_install() {
    let s = read("scripts/create-usb-linux.sh");
    let setup = s
        .split("cat > \"$MOUNT_ROOT/tmp/setup.sh\" << 'SETUP_EOF'")
        .nth(1)
        .expect("create-usb-linux.sh must write the chroot setup script");
    let first_install = setup
        .find("apt-get install")
        .expect("the chroot setup script must apt-get install packages");
    let head = &setup[..first_install];
    assert!(
        head.contains("--force-confold") && head.contains("--force-confdef"),
        "the chroot setup script must configure dpkg --force-confdef/--force-confold BEFORE its \
         first apt-get install, or the pre-baked journal-upload.conf aborts the install at a \
         conffile prompt"
    );
}

#[test]
fn verify_device_has_the_ak_check_before_q() {
    let s = read("scripts/verify-device.sh");
    assert!(
        s.contains("remote_log_gather_remote_snippet") && s.contains("remote_log_verdict"),
        "verify-device.sh (ak) must use the shared gather snippet + verdict"
    );
    // (ak) must come BEFORE (q) -- the (q)-last invariant (check_q_is_wired).
    let ak = s
        .find("# (ak) off-box remote logging effective")
        .expect("verify-device.sh must have the (ak) check block");
    let q = s
        .rfind("# (q) .bak cruft drift")
        .expect("verify-device.sh must still have the (q) block");
    assert!(
        ak < q,
        "the (ak) check must be inserted BEFORE the (q) block"
    );
}
