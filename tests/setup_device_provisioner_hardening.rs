//! #450 (rescoped 2026-07-05) — `setup-device.sh` becomes a name-resolved, fail-loud one-shot
//! provisioner. #455/#457 already landed the peripheral plumbing (genlock/cpu-affinity drop-ins,
//! CI-artifact binary, NDI auto-fetch, dantesync pin, password env-ization); THIS ticket covers
//! the remaining, still-unaddressed core:
//!
//!   1. Name-resolved single-arg invocation — source `scripts/camera-set.sh` and resolve
//!      `DEVICE_NAME -> IP / VBAN stream / genlock FPS`, dropping the free-text 3-positional-arg
//!      form (`setup-device.sh CAM5` alone must work).
//!   2. Canonical PLAIN `ExecStart` — no baked `--display "STRIH-SNV (interkom)"` STRING LITERAL,
//!      and no per-box variance at all: `ExecStart=/usr/local/bin/camera-box`, unconditionally, on
//!      EVERY box. #562 (2026-07-07) had briefly made cam2 an exception via a table-driven
//!      `execstart_display_flag()`; #528's 2026-07-08 design pivot retired that whole per-box
//!      mechanism (the owner rejected static per-box preview config — camboxes have no
//!      keyboard/mouse and the monitor moves between cameras during an event) in favor of an
//!      UNCONDITIONAL, fleet-wide default baked into the binary itself
//!      (`DEFAULT_DISPLAY_SOURCE` in `src/main.rs`). ExecStart is back to being the exact
//!      canonical PLAIN line, on every box, with zero exceptions.
//!   3. Fail-loud posture (script-failure-policy) — `set -euo pipefail`; hard-exit non-zero on
//!      binary/NDI/ALSA/dantesync install failure instead of warn-and-continue.
//!   4. Hard-fail if `/usr/lib/ndi/libndi.so.6` is still missing at the end (was: print "ACTION
//!      REQUIRED" and exit 0).
//!   5. Idempotency guard on the STEP 18 fstab ro/tmpfs rewrite — an unconditional `cp` was
//!      clobbering `/etc/fstab.bak` (the true pre-provisioning original) on every re-run.
//!   6. genlock.conf FPS sourced from the per-cam `CAMERA_GENLOCK_FPS` table (#451), not a
//!      hardcoded `60` (covered by `tests/provisioning_realtime_isolation.rs`).
//!   7. Never print "[19/19] Setup Complete!" on a half-configured box — exit non-zero instead.
//!
//! Style mirrors the repo's other script guards (`appliance_boot_hardening.rs`,
//! `setup_device_fleet_binary_ndi.rs`, `provisioning_realtime_isolation.rs`): read the REAL
//! provisioning script and assert on the REAL contract. RED before this change (3-positional-arg
//! usage, baked --display, warn-and-continue failure paths, unconditional fstab.bak clobber,
//! exit-0-on-missing-NDI); GREEN after.

use std::path::PathBuf;

const SCRIPT: &str = "scripts/setup-device.sh";
const USB_SCRIPT: &str = "scripts/create-usb-linux.sh";

fn read_script() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SCRIPT);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn read_usb_script() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(USB_SCRIPT);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// True if `needle` appears on a line that is NOT a `#` comment. Mirrors the `on_noncomment_line`
/// helper in `appliance_boot_hardening.rs` / `setup_device_fleet_binary_ndi.rs`.
fn on_noncomment_line(body: &str, needle: &str) -> bool {
    body.lines()
        .any(|l| l.contains(needle) && !l.trim_start().starts_with('#'))
}

/// Index of the first NON-comment line containing `needle`.
fn first_noncomment_idx(body: &str, needle: &str) -> Option<usize> {
    body.lines()
        .position(|l| l.contains(needle) && !l.trim_start().starts_with('#'))
}

/// Index of the first NON-comment line whose TRIMMED content is EXACTLY `needle` -- distinguishes
/// a bare function CALL (e.g. `restore_root_mode`) from its DEFINITION line (`restore_root_mode() {`),
/// since `first_noncomment_idx` would otherwise match the (textually-earlier) definition.
fn first_noncomment_exact_idx(body: &str, needle: &str) -> Option<usize> {
    body.lines()
        .position(|l| l.trim() == needle && !l.trim_start().starts_with('#'))
}

// ---------------------------------------------------------------------------------------------
// 1. Name-resolved single-arg invocation
// ---------------------------------------------------------------------------------------------

#[test]
fn setup_device_sources_camera_set_and_resolves_by_name() {
    let body = read_script();
    assert!(
        on_noncomment_line(&body, "camera-set.sh"),
        "setup-device.sh must source scripts/camera-set.sh -- the single source of truth for the \
         cam1-6 fleet map (#450/#24)"
    );
    assert!(
        on_noncomment_line(&body, "camera_resolve"),
        "setup-device.sh must call camera_resolve() to resolve DEVICE_NAME -> IP/stream/genlock-fps \
         (#450)"
    );
}

#[test]
fn setup_device_no_longer_takes_three_positional_args() {
    let body = read_script();
    assert!(
        !body.contains("DEVICE_NAME DEVICE_IP VBAN_STREAM"),
        "setup-device.sh must no longer document/require the old 3-positional-arg form \
         (DEVICE_NAME DEVICE_IP VBAN_STREAM) -- DEVICE_NAME alone must resolve everything (#450)"
    );
    assert!(
        !on_noncomment_line(&body, r#"DEVICE_IP="${2:-}"#),
        "setup-device.sh must not read DEVICE_IP from a free-text positional arg any more -- it \
         must be DERIVED from camera_resolve() (#450)"
    );
    assert!(
        !on_noncomment_line(&body, r#"VBAN_STREAM="${3:-}"#),
        "setup-device.sh must not read VBAN_STREAM from a free-text positional arg any more -- it \
         must be DERIVED from camera_resolve() (#450)"
    );
    assert!(
        on_noncomment_line(&body, "Usage: $0 [--binary <url|path>] DEVICE_NAME"),
        "setup-device.sh's usage message must reflect the single-arg form (#450)"
    );
}

// ---------------------------------------------------------------------------------------------
// 2. Canonical PLAIN ExecStart
// ---------------------------------------------------------------------------------------------

#[test]
fn setup_device_execstart_is_canonical_plain_unconditionally() {
    // #528 design pivot (2026-07-08): the #562 per-box ExecStart-mechanism table
    // (CAMERA_DISPLAY_EXECSTART_SOURCE / execstart_display_flag()) that used to let cam2 bake a
    // --display flag into ExecStart is GONE -- the owner rejected the whole per-box-config
    // approach (camboxes have no keyboard/mouse; the preview monitor moves between cameras during
    // an event). The HDMI cameraman preview is now UNCONDITIONAL and fleet-wide, baked into the
    // binary's own default (DEFAULT_DISPLAY_SOURCE in src/main.rs) -- ExecStart is the exact
    // canonical PLAIN line on EVERY box, no exceptions, no per-cam variance, no table lookup.
    let body = read_script();
    assert!(
        !body.contains(r#"--display "STRIH-SNV"#),
        "setup-device.sh must never hardcode a literal --display flag as a string -- the preview \
         lives entirely in the binary's own unconditional default (#450/#528)"
    );
    // NON-comment lines only: an explanatory "this used to call X" comment legitimately mentions
    // the retired names for history — only an actual definition/call site is a real regression.
    assert!(
        !on_noncomment_line(&body, "execstart_display_flag")
            && !on_noncomment_line(&body, "CAMERA_DISPLAY_EXECSTART_SOURCE"),
        "#528: setup-device.sh must no longer define/call execstart_display_flag or reference the \
         retired CAMERA_DISPLAY_EXECSTART_SOURCE table -- ExecStart is unconditionally plain"
    );
    assert!(
        on_noncomment_line(&body, "ExecStart=/usr/local/bin/camera-box") && {
            let line = body
                .lines()
                .find(|l| {
                    l.trim_start()
                        .starts_with("ExecStart=/usr/local/bin/camera-box")
                        && !l.trim_start().starts_with('#')
                })
                .expect("the ExecStart line must be present");
            line.trim() == "ExecStart=/usr/local/bin/camera-box"
        },
        "setup-device.sh's STEP 7 ExecStart line must be EXACTLY \
         `ExecStart=/usr/local/bin/camera-box` -- unconditionally, on every box (#528)"
    );
}

// ---------------------------------------------------------------------------------------------
// 3. Fail-loud posture (script-failure-policy)
// ---------------------------------------------------------------------------------------------

#[test]
fn setup_device_uses_set_euo_pipefail() {
    let body = read_script();
    assert!(
        on_noncomment_line(&body, "set -euo pipefail"),
        "setup-device.sh must use `set -euo pipefail` (script-failure-policy) -- a bare `set -e` \
         does not fail loud on a pipeline whose non-final stage fails (#450)"
    );
    // Only the script's OWN top-of-file `set` line matters here -- the STEP 15 kernel
    // postinst.d hook heredoc legitimately embeds its own separate `/bin/sh` script with its own
    // `set -e` (a different, POSIX-sh script written to disk, not this script's shell options).
    let header: String = body.lines().take(20).collect::<Vec<_>>().join("\n");
    assert!(
        !header.lines().any(|l| l.trim() == "set -e"),
        "setup-device.sh's own top-of-file bare `set -e` must be replaced by `set -euo pipefail` \
         (#450)"
    );
}

#[test]
fn setup_device_defines_a_fail_helper() {
    let body = read_script();
    assert!(
        on_noncomment_line(&body, "fail()"),
        "setup-device.sh must define a fail() helper that prints and exits non-zero, so every \
         install step can fail loud consistently (#450, script-failure-policy)"
    );
}

#[test]
fn setup_device_binary_install_fails_loud_never_warns() {
    let body = read_script();
    // The old warn-and-continue text must be gone.
    assert!(
        !body.contains("Please install manually to /usr/local/bin/camera-box"),
        "setup-device.sh must no longer warn-and-continue when the --binary URL download fails \
         (#450)"
    );
    for needle in [
        "could not download binary from",
        "no successful CI run found on branch",
        "gh run download failed for run",
        "gh CLI unavailable or GH_TOKEN unset",
    ] {
        assert!(
            body.contains(needle),
            "setup-device.sh STEP 3 must fail loud (via fail()) with a message containing \
             `{needle}` instead of warning and continuing (#450)"
        );
    }
}

#[test]
fn setup_device_ndi_fetch_fails_loud_never_warns() {
    let body = read_script();
    for needle in [
        "NDI fetch from $NDI_PEER produced no file",
        "could not fetch NDI library from fleet peer",
    ] {
        assert!(
            on_noncomment_line(&body, needle),
            "setup-device.sh STEP 4 must fail loud (via fail()) with a message containing \
             `{needle}` instead of warning and continuing (#450)"
        );
    }
}

#[test]
fn setup_device_alsa_detection_fails_loud_never_defaults_to_card_1() {
    let body = read_script();
    assert!(
        !on_noncomment_line(&body, "USB_CARD=1"),
        "setup-device.sh must not silently default to ALSA card 1 when auto-detection fails -- a \
         wrong hardcoded card would silently misconfigure the intercom (#450)"
    );
    // #782 replaced STEP 5's card-NUMBER auto-detection with a by-NAME config (sysdefault:CARD=HID),
    // which is enumeration-proof (a baked card NUMBER dangles on re-enumeration). The #450 fail-loud
    // INTENT is preserved -- STEP 5 still hard-fails (never silently misconfigures) when the intercom
    // headset is absent -- only the mechanism/message changed: it now confirms the HID card exists by
    // NAME instead of auto-detecting a number.
    assert!(
        body.contains("no ALSA card named 'HID'"),
        "setup-device.sh STEP 5 must fail loud when the HID intercom headset card is absent \
         (#450 fail-loud intent, #782 by-NAME migration)"
    );
}

#[test]
fn setup_device_dantesync_install_fails_loud_never_non_critical() {
    let body = read_script();
    assert!(
        !body.to_lowercase().contains("non-critical"),
        "setup-device.sh must no longer treat a dantesync install failure as \"non-critical\" -- \
         dantesync disciplines the cluster clock genlock depends on (#8/#450)"
    );
    for needle in [
        "could not get dantesync release URL",
        "failed to download dantesync from",
    ] {
        assert!(
            body.contains(needle),
            "setup-device.sh STEP 17 must fail loud (via fail()) with a message containing \
             `{needle}` instead of a non-critical warning (#450)"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// 4 + 7. Hard-fail on a half-configured box; never print "Setup Complete!" first
// ---------------------------------------------------------------------------------------------

#[test]
fn setup_device_never_prints_action_required_and_exits_zero() {
    let body = read_script();
    assert!(
        !body.contains("ACTION REQUIRED"),
        "setup-device.sh must no longer print \"ACTION REQUIRED\" and exit 0 when the NDI library \
         is still missing at the end -- it must hard-fail instead (#450)"
    );
}

#[test]
fn setup_device_exits_nonzero_on_half_configured_box_before_reporting_complete() {
    let body = read_script();
    assert!(
        on_noncomment_line(&body, "/usr/local/bin/camera-box") && body.contains("MISSING"),
        "setup-device.sh STEP 19 must check for the camera-box binary and accumulate a MISSING \
         reason before deciding whether to report success (#450)"
    );
    assert!(
        on_noncomment_line(&body, "/usr/lib/ndi/libndi.so.6") && body.contains("MISSING"),
        "setup-device.sh STEP 19 must check for the NDI library and accumulate a MISSING reason \
         before deciding whether to report success (#450)"
    );
    assert!(
        body.contains("half-configured box"),
        "setup-device.sh STEP 19 must fail loud (via fail()) naming the box half-configured when \
         the binary or NDI library is still missing (#450)"
    );

    // ORDER: the missing-artifact gate must run BEFORE the "Setup Complete!" banner is printed --
    // otherwise a half-configured box could still see a false-positive success banner before the
    // process later exits non-zero (or, worse, never gets checked at all).
    let missing_check_idx = first_noncomment_idx(&body, "half-configured box")
        .expect("half-configured box fail call present");
    let complete_idx =
        first_noncomment_idx(&body, "Setup Complete!").expect("Setup Complete! banner present");
    assert!(
        missing_check_idx < complete_idx,
        "the half-configured-box check must run BEFORE the \"Setup Complete!\" banner is printed \
         (#450) -- found the check at line {missing_check_idx}, the banner at line {complete_idx}"
    );
}

// ---------------------------------------------------------------------------------------------
// 5. Idempotency guard on the STEP 18 fstab backup
// ---------------------------------------------------------------------------------------------

#[test]
fn setup_device_fstab_backup_is_idempotent() {
    let body = read_script();
    let guard_idx = first_noncomment_idx(&body, "[ ! -f /etc/fstab.bak ]")
        .expect("an idempotency guard on /etc/fstab.bak must be present (#450)");
    let cp_idx = first_noncomment_idx(&body, "cp /etc/fstab /etc/fstab.bak")
        .expect("the fstab backup cp command must still be present (#450)");
    assert!(
        guard_idx < cp_idx,
        "the `[ ! -f /etc/fstab.bak ]` guard must run BEFORE `cp /etc/fstab /etc/fstab.bak`, or a \
         re-run clobbers the true pre-provisioning original with the already-rewritten (ro+tmpfs) \
         fstab (#450). guard at line {guard_idx}, cp at line {cp_idx}"
    );
}

// ---------------------------------------------------------------------------------------------
// 8. #591 — dantesync is the SOLE clock authority: PURGE every competing timesync daemon
// ---------------------------------------------------------------------------------------------
// cam5/cam6 (N150) shipped with systemd-timesyncd active ALONGSIDE dantesync -> a real 5.28-second
// clock desync ([NTP] offset:-5280959us), invisible to weeks of "passing" verification. A minimalist
// cambox/imag appliance runs ONLY dantesync; provisioning must PURGE the competing daemon (masking is
// a band-aid — the package must be gone), then mask it as a backstop. Belt-and-suspenders across
// systemd-timesyncd / chrony / ntp / ntpsec / openntpd.

#[test]
fn setup_device_purges_every_competing_timesync_daemon() {
    let body = read_script();
    // The full competing-daemon set the provisioner iterates (unique to the #591 purge block).
    assert!(
        body.contains("systemd-timesyncd chrony ntp ntpsec openntpd"),
        "setup-device.sh must handle the full competing-timesync-daemon set \
         `systemd-timesyncd chrony ntp ntpsec openntpd` (#591) -- dantesync is the sole clock \
         authority; a 2nd timesync daemon caused the cam5/6 5.28s desync"
    );
    // A PURGE (not just a mask): the package must be REMOVED from a minimalist appliance.
    assert!(
        on_noncomment_line(&body, r#"apt-get purge -y "$_ts""#),
        "setup-device.sh must `apt-get purge` each competing timesync daemon (#591) -- masking \
         alone leaves the package installed, which the verify gate (r) now hard-fails"
    );
    // A MASK backstop so a re-install cannot silently re-activate it.
    assert!(
        on_noncomment_line(&body, r#"systemctl mask "$_ts""#),
        "setup-device.sh must also MASK each competing timesync daemon as a backstop (#591)"
    );
}

#[test]
fn setup_device_timesync_purge_runs_before_installing_dantesync() {
    // Order: purge competing daemons BEFORE installing dantesync (the sole authority) — reads as
    // "remove every other clock, then install ours". Both are in the rw window (the ro conversion
    // is STEP 18, after both). Anchor on the dantesync DOWNLOAD (the actual install action), not
    // the STEP 17 banner echo which also contains the words "Installing dantesync".
    let body = read_script();
    let purge_idx = first_noncomment_idx(&body, r#"apt-get purge -y "$_ts""#)
        .expect("the #591 competing-timesync purge must be present");
    let dantesync_idx = first_noncomment_idx(&body, "-o /usr/local/bin/dantesync")
        .expect("the dantesync download (install action) must be present");
    assert!(
        purge_idx < dantesync_idx,
        "the competing-timesync purge (line {purge_idx}) must run before the dantesync install \
         (line {dantesync_idx}) (#591)"
    );
}

#[test]
fn create_usb_purges_timesyncd_from_the_base_image() {
    // #591: the base debootstrap image ships systemd-timesyncd. Purge it in the chroot so a
    // freshly-imaged box never ships a 2nd timesync daemon (dantesync is installed later by
    // setup-device.sh as the sole authority).
    let body = read_usb_script();
    assert!(
        body.contains("systemd-timesyncd"),
        "create-usb-linux.sh must purge/mask systemd-timesyncd in the chroot (#591)"
    );
    assert!(
        body.contains("apt-get purge") && body.contains("timesyncd"),
        "create-usb-linux.sh must `apt-get purge` systemd-timesyncd from the base image so a \
         freshly-imaged box never ships a 2nd timesync daemon (#591)"
    );
}

// ---------------------------------------------------------------------------------------------
// #597 — linuxptp (ptp4l/phc2sys) is a 2nd class of competing timesync authority: a rogue PTP
// daemon would fight dantesync's OWN PTP servo directly on this PTP rig. Unlike the #591 NTP
// daemons (where dpkg package name == systemd unit name, so one `for _ts in ...` loop suffices),
// linuxptp's dpkg PACKAGE is "linuxptp" but its systemd UNITS are "ptp4l" and "phc2sys" -- so the
// purge needs its own stanza with the package-vs-unit split modelled correctly (there is no
// "ptp4l" or "phc2sys" apt package to purge; there is no "linuxptp" systemd unit to mask).
// ---------------------------------------------------------------------------------------------

#[test]
fn setup_device_purges_linuxptp_ptp4l_phc2sys() {
    let body = read_script();
    assert!(
        body.contains("ptp4l") && body.contains("phc2sys"),
        "setup-device.sh must name both linuxptp units (ptp4l, phc2sys) in its #597 purge stanza \
         -- a rogue PTP daemon fights dantesync's own PTP servo directly"
    );
    assert!(
        on_noncomment_line(&body, "apt-get purge -y linuxptp"),
        "setup-device.sh must `apt-get purge` the linuxptp PACKAGE (#597) -- its dpkg name \
         differs from its unit names (ptp4l/phc2sys), so it cannot reuse the #591 `for _ts in ...` \
         loop and needs a purge line of its own. dantesync is a standalone binary \
         (/usr/local/bin/dantesync) with no dependency on the linuxptp package"
    );
    assert!(
        on_noncomment_line(&body, r#"systemctl mask "$_u""#),
        "setup-device.sh must MASK each linuxptp unit as a backstop, mirroring the #591 \
         `systemctl mask \"$_ts\"` pattern for the NTP daemons (#597)"
    );
}

#[test]
fn setup_device_linuxptp_purge_runs_before_installing_dantesync() {
    // Same ordering guarantee as the #591 NTP-daemon purge: remove every competing clock BEFORE
    // installing dantesync as the sole authority.
    let body = read_script();
    let purge_idx = first_noncomment_idx(&body, "apt-get purge -y linuxptp")
        .expect("the #597 linuxptp purge must be present");
    let dantesync_idx = first_noncomment_idx(&body, "-o /usr/local/bin/dantesync")
        .expect("the dantesync download (install action) must be present");
    assert!(
        purge_idx < dantesync_idx,
        "the linuxptp purge (line {purge_idx}) must run before the dantesync install (line \
         {dantesync_idx}) (#597)"
    );
}

#[test]
fn create_usb_purges_linuxptp() {
    // Mirrors create_usb_purges_timesyncd_from_the_base_image, but for linuxptp (#597): a freshly
    // imaged box must never ship a 2nd PTP authority any more than a 2nd NTP one.
    let body = read_usb_script();
    assert!(
        body.contains("ptp4l") && body.contains("phc2sys") && body.contains("linuxptp"),
        "create-usb-linux.sh must purge/mask linuxptp (ptp4l/phc2sys) in the chroot (#597), \
         mirroring the #591 NTP-daemon purge"
    );
    assert!(
        on_noncomment_line(&body, "apt-get purge -y linuxptp"),
        "create-usb-linux.sh must `apt-get purge` the linuxptp package from the base image (#597)"
    );
}

// ---------------------------------------------------------------------------------------------
// #599 — STEP 15-18 (fwupd purge, package install, timesync/linuxptp purge, fstab rewrite) all
// need a writable root. On an IN-PLACE RE-RUN against an already-booted ro appliance, none of them
// get it: the apt-get/dpkg calls in STEP 15-17 fail and are swallowed by `|| true` guards (silent
// no-op), and STEP 18's fstab rewrite would hard-abort. The provisioner must remount rw BEFORE
// STEP 15 and remount back to ro AFTER STEP 18, so an in-place re-provision actually applies
// instead of silently doing nothing while still reporting success.
// ---------------------------------------------------------------------------------------------

#[test]
fn setup_device_defines_root_rw_ro_helpers() {
    let body = read_script();
    for needle in [
        "root_mount_is_readonly()",
        "ensure_root_writable()",
        "restore_root_mode()",
    ] {
        assert!(
            on_noncomment_line(&body, needle),
            "setup-device.sh must define `{needle}` (#599) -- an in-place re-run against an \
             already-booted ro appliance must remount rw before STEP 15-18's apt/dpkg/systemctl/\
             fstab writes, then remount back to ro afterward, instead of silently no-op'ing"
        );
    }
}

#[test]
fn setup_device_calls_ensure_root_writable_before_the_fwupd_purge() {
    let body = read_script();
    let call_idx = first_noncomment_exact_idx(&body, "ensure_root_writable").expect(
        "ensure_root_writable must be CALLED (a bare invocation, not just defined) before STEP 15 \
         (#599)",
    );
    let fwupd_purge_idx = first_noncomment_idx(&body, "apt-get purge -y fwupd")
        .expect("the STEP 15 fwupd purge must be present");
    assert!(
        call_idx < fwupd_purge_idx,
        "the ensure_root_writable call (line {call_idx}) must run BEFORE the STEP 15 fwupd purge \
         (line {fwupd_purge_idx}) -- every apt-get/dpkg call in STEP 15-17 needs a writable root \
         on an in-place re-run (#599)"
    );
}

// ---------------------------------------------------------------------------------------------
// #1289 -- cam5/cam6/cam7 re-provisioning revealed ensure_root_writable() runs too LATE: STEP 1
// (hostname), STEP 2 (netplan), STEP 3 (binary), and STEP 4-14 (NDI/ALSA/config/systemd/GRUB/
// sysctl) all write under /etc or /usr BEFORE the #599 call site (previously right before
// STEP 15). On a FIRST-provisioning run root is naturally rw so this never showed; on an
// IN-PLACE RE-RUN against an already-booted ro appliance, STEP 1's hostname write is the FIRST
// write in the whole script and dies with "Read-only file system" before ANY of #599's remount
// logic ever executes (live, cam6 10.77.9.66, 2026-09-03 -- `setup-device.sh` line 362). The
// call must move to run BEFORE the pre-flight curl block and STEP 1, not just before STEP 15.
// ---------------------------------------------------------------------------------------------

#[test]
fn setup_device_calls_ensure_root_writable_before_step_1_hostname_1289() {
    let body = read_script();
    let call_idx = first_noncomment_exact_idx(&body, "ensure_root_writable").expect(
        "ensure_root_writable must be CALLED (a bare invocation, not just defined) before STEP 1 \
         (#1289) -- a re-run against an already-booted ro appliance fails at the very first /etc \
         write otherwise",
    );
    let hostname_write_idx = first_noncomment_idx(&body, r#"echo "$DEVICE_NAME" > /etc/hostname"#)
        .expect("the STEP 1 hostname write must be present");
    assert!(
        call_idx < hostname_write_idx,
        "the ensure_root_writable call (line {call_idx}) must run BEFORE STEP 1's hostname write \
         (line {hostname_write_idx}) -- STEP 1 is the FIRST /etc write in the script, so a re-run \
         against an already-booted ro appliance dies with 'Read-only file system' before the \
         #599 remount logic (previously gated only before STEP 15) ever runs (#1289)"
    );
    let preflight_idx = first_noncomment_idx(&body, "[pre-flight] Ensuring curl + CA certificates")
        .expect("the pre-flight curl-install block must be present");
    assert!(
        call_idx < preflight_idx,
        "the ensure_root_writable call (line {call_idx}) must also run BEFORE the pre-flight \
         curl-install block (line {preflight_idx}) -- that block runs `apt-get install` when \
         curl is missing, which also needs a writable root on a re-run (#1289)"
    );
}

#[test]
fn setup_device_calls_restore_root_mode_after_the_fstab_rewrite() {
    let body = read_script();
    let call_idx = first_noncomment_exact_idx(&body, "restore_root_mode").expect(
        "restore_root_mode must be CALLED (a bare invocation, not just defined) after STEP 18 \
         (#599)",
    );
    let fstab_write_idx = first_noncomment_idx(&body, "cat > /etc/fstab << FSTABEOF")
        .expect("the STEP 18 fstab rewrite must be present");
    assert!(
        call_idx > fstab_write_idx,
        "the restore_root_mode call (line {call_idx}) must run AFTER the STEP 18 fstab rewrite \
         (line {fstab_write_idx}) -- STEP 18 also writes /etc/fstab and needs a writable root on \
         an in-place re-run, so root must stay rw through STEP 18, not just STEP 15-17 (#599)"
    );
}

#[test]
fn setup_device_ensure_root_writable_falls_back_to_proc_mounts() {
    // #599 code-review hardening: `findmnt` failing outright (missing binary, unreadable /proc)
    // must not silently read as "not ro" (opts="") -- fall back to /proc/mounts directly, mirroring
    // verify-device.sh's identical fallback for the same read, so a transient findmnt failure on a
    // genuinely-ro box can't skip the remount and reproduce #599.
    let body = read_script();
    assert!(
        on_noncomment_line(&body, r#"awk '$2=="/"{print $4; exit}' /proc/mounts"#),
        "ensure_root_writable's findmnt read must fall back to parsing /proc/mounts directly on \
         failure (#599) -- mirrors verify-device.sh's MOUNT_OPTS read, which has the same fallback"
    );
}

#[test]
fn setup_device_handles_packagekit_around_the_remount_cycle() {
    // rig-timesync-single-authority incident: PackageKit is D-Bus-activated by apt and holds an
    // open write handle on /var/lib/PackageKit/transactions.db, which blocks `mount -o
    // remount,ro /` with EBUSY. Must be stopped/masked so it can't reactivate mid-run.
    let body = read_script();
    assert!(
        body.to_lowercase().contains("packagekit"),
        "setup-device.sh must stop/mask PackageKit around the ro<->rw remount cycle (#599) -- \
         otherwise a D-Bus-reactivated PackageKit can block the ro remount with EBUSY"
    );
}

#[test]
fn setup_device_root_rw_helpers_fail_loud_not_best_effort() {
    // #599 explicitly requires FAIL LOUD (never a silent `|| true`) if the remount itself cannot
    // be applied -- a re-run must never claim success on a still-wrongly-moded root.
    let body = read_script();
    let ensure_start = body
        .find("ensure_root_writable() {")
        .expect("ensure_root_writable definition must be present (#599)");
    let restore_start = body
        .find("restore_root_mode() {")
        .expect("restore_root_mode definition must be present (#599)");
    let ensure_window = &body[ensure_start..restore_start.max(ensure_start)];
    assert!(
        ensure_window.contains("fail "),
        "ensure_root_writable() must call fail() when `mount -o remount,rw /` does not succeed \
         (#599) -- no silent `|| true` on the remount itself"
    );
    let restore_window = &body[restore_start..];
    assert!(
        restore_window.contains("fail "),
        "restore_root_mode() must call fail() when `mount -o remount,ro /` does not succeed (#599) \
         -- no silent `|| true` on the remount itself"
    );
}

// ---------------------------------------------------------------------------------------------
// #679 — /var/log tmpfs bounded against runaway growth, on BOTH provisioners
// ---------------------------------------------------------------------------------------------

#[test]
fn setup_device_sources_log_bound_and_writes_both_files_inside_step_18() {
    let body = read_script();
    assert!(
        on_noncomment_line(&body, ". \"$HERE/lib/log-bound.sh\""),
        "setup-device.sh must source scripts/lib/log-bound.sh (#679) -- the shared size cap + \
         drop-in content generators"
    );
    let write_size_idx = first_noncomment_idx(&body, "log_bound_logrotate_config > ").expect(
        "setup-device.sh must write /etc/logrotate.d/rsyslog via log_bound_logrotate_config (#679)",
    );
    let write_dropin_idx = first_noncomment_idx(&body, "log_bound_timer_dropin > ").expect(
        "setup-device.sh must write the logrotate.timer drop-in via log_bound_timer_dropin (#679)",
    );
    let fstab_write_idx = first_noncomment_idx(&body, "cat > /etc/fstab << FSTABEOF")
        .expect("the STEP 18 fstab rewrite must be present");
    let restore_idx = first_noncomment_exact_idx(&body, "restore_root_mode")
        .expect("restore_root_mode must be called");
    assert!(
        write_size_idx > fstab_write_idx && write_size_idx < restore_idx,
        "the #679 logrotate config write (line {write_size_idx}) must happen AFTER the STEP 18 \
         fstab rewrite (line {fstab_write_idx}) and BEFORE restore_root_mode (line {restore_idx}) \
         -- it needs root still writable"
    );
    assert!(
        write_dropin_idx > fstab_write_idx && write_dropin_idx < restore_idx,
        "the #679 timer drop-in write (line {write_dropin_idx}) must also happen inside the same \
         writable window (fstab={fstab_write_idx}, restore={restore_idx})"
    );
    assert!(
        on_noncomment_line(&body, "systemctl restart logrotate.timer"),
        "setup-device.sh must restart logrotate.timer after writing the #679 drop-in so it takes \
         effect immediately on a re-provisioned already-booted box, not just after next reboot"
    );
}

#[test]
fn create_usb_linux_sources_log_bound_and_writes_both_files_into_the_chroot() {
    let body = read_usb_script();
    assert!(
        on_noncomment_line(&body, ". \"$SCRIPT_DIR/lib/log-bound.sh\""),
        "create-usb-linux.sh must source scripts/lib/log-bound.sh (#679) -- the SAME content \
         generators setup-device.sh uses, so the size cap can never drift between the two \
         provisioners"
    );
    assert!(
        on_noncomment_line(
            &body,
            "log_bound_logrotate_config > \"$MOUNT_ROOT$LOG_BOUND_LOGROTATE_PATH\""
        ),
        "create-usb-linux.sh must write /etc/logrotate.d/rsyslog into the base-image chroot \
         (#679) -- closes the window before setup-device.sh later converts /var/log to a tmpfs"
    );
    assert!(
        on_noncomment_line(
            &body,
            "log_bound_timer_dropin > \"$MOUNT_ROOT$LOG_BOUND_TIMER_DROPIN_PATH\""
        ),
        "create-usb-linux.sh must write the frequent logrotate.timer drop-in into the base-image \
         chroot too (#679)"
    );
}

// ---------------------------------------------------------------------------------------------
// #762 — rsyslog is REDUNDANT on the cam appliances: PURGE it (journald already captures
// everything; nothing reads /var/log/syslog on a ro appliance) + cap journald's own
// RuntimeMaxUse so the journal itself can never fill the SAME tmpfs rsyslog used to. A live
// cam1 incident (2026-07-15) showed rsyslogd enter a write-error feedback loop once the 50MB
// /var/log tmpfs filled -- ~400 lines/s, 42.8% CPU -- starving the camera-box send path badly
// enough to measurably drift NDI delivery timing. Same disable -> purge -> mask discipline as
// the #591/#597 competing-timesync-daemon purge above (rsyslog is architecturally the same
// class: a redundant daemon that must be GONE, not merely masked).
// ---------------------------------------------------------------------------------------------

#[test]
fn setup_device_purges_rsyslog() {
    let body = read_script();
    assert!(
        on_noncomment_line(&body, "apt-get purge -y rsyslog"),
        "setup-device.sh must `apt-get purge` rsyslog (#762) -- journald already captures \
         everything on this read-only appliance; masking alone leaves the package installed"
    );
    assert!(
        on_noncomment_line(&body, "systemctl mask rsyslog"),
        "setup-device.sh must also MASK rsyslog as a backstop so a re-install cannot silently \
         re-activate it (#762, mirrors the #591/#597 competing-daemon discipline)"
    );
    assert!(
        on_noncomment_line(&body, "systemctl disable --now rsyslog"),
        "setup-device.sh must disable+stop rsyslog before purging it (#762)"
    );
}

#[test]
fn setup_device_rsyslog_purge_runs_after_the_timesync_purge_stanza() {
    // Not a HARD ordering requirement the way timesync-before-dantesync is (nothing installs
    // rsyslog later in this script the way dantesync gets installed after its competing purge)
    // -- but the #762 stanza is written immediately after the #591/#597 block for readability,
    // so pin that the rsyslog purge exists at all somewhere after it starts.
    let body = read_script();
    let timesync_idx = first_noncomment_idx(&body, r#"apt-get purge -y "$_ts""#)
        .expect("the #591 competing-timesync purge must be present");
    let rsyslog_idx = first_noncomment_idx(&body, "apt-get purge -y rsyslog")
        .expect("the #762 rsyslog purge must be present");
    assert!(
        rsyslog_idx > timesync_idx,
        "the #762 rsyslog purge (line {rsyslog_idx}) should follow the #591 timesync purge \
         stanza (line {timesync_idx}) for readability"
    );
}

#[test]
fn setup_device_sources_log_diet_and_writes_the_journald_dropin() {
    let body = read_script();
    assert!(
        on_noncomment_line(&body, ". \"$HERE/lib/log-diet.sh\""),
        "setup-device.sh must source scripts/lib/log-diet.sh (#762) -- the shared journald \
         RuntimeMaxUse drop-in content generator"
    );
    assert!(
        on_noncomment_line(
            &body,
            "log_diet_journald_dropin > \"$LOG_DIET_JOURNALD_DROPIN_PATH\""
        ),
        "setup-device.sh must write the journald RuntimeMaxUse drop-in via \
         log_diet_journald_dropin (#762)"
    );
    assert!(
        on_noncomment_line(&body, "systemctl restart systemd-journald"),
        "setup-device.sh must restart systemd-journald after writing the #762 drop-in so the \
         cap takes effect immediately on a re-provisioned already-booted box"
    );
}

#[test]
fn create_usb_purges_rsyslog_from_the_base_image() {
    // #762: mirrors create_usb_purges_timesyncd_from_the_base_image -- purge the redundant
    // daemon in the chroot so a freshly-imaged box never ships it at all.
    let body = read_usb_script();
    assert!(
        on_noncomment_line(&body, "apt-get purge -y rsyslog"),
        "create-usb-linux.sh must `apt-get purge` rsyslog from the base image (#762)"
    );
    assert!(
        on_noncomment_line(&body, "systemctl mask rsyslog"),
        "create-usb-linux.sh must also MASK rsyslog as a backstop in the chroot (#762)"
    );
}

#[test]
fn create_usb_linux_sources_log_diet_and_writes_the_journald_dropin_into_the_chroot() {
    let body = read_usb_script();
    assert!(
        on_noncomment_line(&body, ". \"$SCRIPT_DIR/lib/log-diet.sh\""),
        "create-usb-linux.sh must source scripts/lib/log-diet.sh (#762) -- the SAME content \
         generator setup-device.sh uses, so the RuntimeMaxUse cap can never drift between the \
         two provisioners"
    );
    assert!(
        on_noncomment_line(
            &body,
            "log_diet_journald_dropin > \"$MOUNT_ROOT$LOG_DIET_JOURNALD_DROPIN_PATH\""
        ),
        "create-usb-linux.sh must write the journald RuntimeMaxUse drop-in into the base-image \
         chroot (#762) -- closes the window before setup-device.sh ever runs"
    );
}

/// #930 finding 10 — the STEP 16 ffmpeg install (the lipsync-test-mode runtime dependency) must
/// FAIL LOUD like the rest of this ticket's fail-loud posture (item 3 above), not swallow a real
/// apt failure behind `2>/dev/null || true` and then print "Installed: ffmpeg, ..." regardless.
/// Root is guaranteed writable during STEP 16 (#599's `ensure_root_writable` call runs before
/// STEP 15), so there is no legitimate reason left for this ONE install line to swallow errors.
#[test]
fn setup_device_ffmpeg_install_fails_loud_930() {
    let body = read_script();
    let idx = body
        .find("--no-install-recommends ffmpeg libsdl2-2.0-0")
        .expect("the #930 ffmpeg apt-get install line must be present");
    let line_start = body[..idx].rfind('\n').map(|nl| nl + 1).unwrap_or(0);
    let line_end = body[idx..]
        .find('\n')
        .map(|e| idx + e)
        .unwrap_or(body.len());
    let full_line = &body[line_start..line_end];
    assert!(
        !full_line.contains("|| true") && !full_line.contains("2>/dev/null"),
        "setup-device.sh's #930 ffmpeg install must fail loud (no `2>/dev/null || true` \
         swallowing a real apt failure): {full_line}"
    );
}

/// issue 1187 — the lipsync-test-mode playback path moved off raw fbdev onto DRM/KMS via
/// `mpv --vo=drm`, so mpv is now a runtime dependency of every box that can take cam2's
/// lipsync-test-mode painter role. STEP 16 must install it (on the SAME lipsync-runtime install
/// line as ffmpeg, so it inherits the same fail-loud posture proven above), and the "Installed:"
/// echo must advertise it for operator visibility.
#[test]
fn setup_device_installs_mpv_for_lipsync_drm_playback_1187() {
    let body = read_script();
    let idx = body
        .find("--no-install-recommends ffmpeg libsdl2-2.0-0")
        .expect("the STEP 16 lipsync-runtime apt-get install line must be present");
    let line_start = body[..idx].rfind('\n').map(|nl| nl + 1).unwrap_or(0);
    let line_end = body[idx..]
        .find('\n')
        .map(|e| idx + e)
        .unwrap_or(body.len());
    let full_line = &body[line_start..line_end];
    assert!(
        full_line.split_whitespace().any(|t| t == "mpv"),
        "1187: STEP 16 must install mpv (the DRM/KMS lipsync playback runtime): {full_line}"
    );
    // Anchor on the lipsync-runtime echo specifically: `  Installed:` is a shared prefix used by
    // several STEP-16 install groups (avahi, udev, ...), so the bare prefix would grab the wrong
    // (first) one -- `  Installed: ffmpeg` is unique to the lipsync-runtime line.
    let echo_idx = body
        .find("echo \"  Installed: ffmpeg")
        .expect("the STEP 16 lipsync-runtime 'Installed:' echo must be present");
    let echo_end = body[echo_idx..]
        .find('\n')
        .map(|e| echo_idx + e)
        .unwrap_or(body.len());
    assert!(
        body[echo_idx..echo_end].contains("mpv"),
        "1187: the STEP 16 'Installed:' echo must advertise mpv: {}",
        &body[echo_idx..echo_end]
    );
}

// ============================================================================================
// issue 1234 -- cam5/6/7 sit behind an unmanaged QNAP 2.5G switch that aggregates all three
// onto one uplink. The `optimize-nic` networkd-dispatcher hook (STEP 14) blanket-disables
// ethernet flow control (`ethtool -A "$IFACE" rx off tx off`) on EVERY interface, so the QNAP
// has no way to backpressure the cameras and line-rate NDI bursts overflow its egress queue
// (silent drop, no counter) -- measured live (iperf3 UDP loss 8-14x worse with pause off,
// qr-align spread 163-219 ids with pause off vs 0-2 with pause on). Fix: ADVERTISE flow control
// (`rx on tx on`) instead of forcing it off -- the actual negotiated result is decided by the
// link partner, so a direct CRS310 link (which advertises pause off) still negotiates OFF; only
// the cam5/6/7 links behind the unmanaged aggregator actually change behavior.
// ============================================================================================

/// RED before the fix: `setup-device.sh`'s `optimize-nic` hook must advertise flow control ON
/// at BOTH call sites (the networkd-dispatcher hook body + the immediate current-interface
/// apply loop), and must NOT contain the old disable-it-everywhere literal anywhere.
#[test]
fn setup_device_advertises_flow_control_on_not_off_1234() {
    let body = read_script();
    let on_count = body.matches(r#"ethtool -A "$IFACE" rx on tx on"#).count();
    assert_eq!(
        on_count, 2,
        "1234: expected exactly 2 `ethtool -A \"$IFACE\" rx on tx on` call sites (the \
         networkd-dispatcher hook body + the immediate current-interface apply loop), found {on_count}"
    );
    assert!(
        !body.contains(r#"ethtool -A "$IFACE" rx off tx off"#),
        "1234: setup-device.sh must NOT disable flow control any more -- disabling it prevents \
         the unmanaged QNAP aggregator behind cam5/6/7 from backpressuring the cameras, which \
         overflows its egress buffer under 3x line-rate NDI load and destabilizes qr-align"
    );
    // The EEE-off lines are UNCHANGED by this ticket -- only flow control flips.
    let eee_count = body.matches(r#"eee off"#).count();
    assert!(
        eee_count >= 2,
        "1234: the EEE-off lines must stay untouched (still >= 2 `eee off` occurrences), found {eee_count}"
    );
    assert!(
        body.contains("echo \"  Flow control: advertised\""),
        "1234: the STEP 14 summary echo must say the flow-control line was flipped to advertised, \
         not left claiming 'disabled'"
    );
    assert!(
        !body.contains("echo \"  Flow control: disabled\""),
        "1234: the stale 'Flow control: disabled' summary echo must be gone"
    );
}
