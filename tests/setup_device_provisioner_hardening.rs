//! #450 (rescoped 2026-07-05) — `setup-device.sh` becomes a name-resolved, fail-loud one-shot
//! provisioner. #455/#457 already landed the peripheral plumbing (genlock/cpu-affinity drop-ins,
//! CI-artifact binary, NDI auto-fetch, dantesync pin, password env-ization); THIS ticket covers
//! the remaining, still-unaddressed core:
//!
//!   1. Name-resolved single-arg invocation — source `scripts/camera-set.sh` and resolve
//!      `DEVICE_NAME -> IP / VBAN stream / genlock FPS`, dropping the free-text 3-positional-arg
//!      form (`setup-device.sh CAM5` alone must work).
//!   2. Canonical PLAIN `ExecStart` — no baked `--display "STRIH-SNV (interkom)"`.
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

fn read_script() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SCRIPT);
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

// ---------------------------------------------------------------------------------------------
// 1. Name-resolved single-arg invocation
// ---------------------------------------------------------------------------------------------

#[test]
fn setup_device_sources_camera_set_and_resolves_by_name() {
    let body = read_script();
    assert!(
        on_noncomment_line(&body, "camera-set.sh"),
        "setup-device.sh must source scripts/camera-set.sh -- the single source of truth for the \
         cam1-7 fleet map (#450/#24)"
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
fn setup_device_execstart_is_canonical_plain() {
    let body = read_script();
    assert!(
        !body.contains(r#"--display "STRIH-SNV"#),
        "setup-device.sh must not bake a hardcoded --display flag into the systemd unit's \
         ExecStart -- the canonical unit must be identical everywhere (#450)"
    );
    assert!(
        body.lines()
            .any(|l| l.trim() == "ExecStart=/usr/local/bin/camera-box"),
        "setup-device.sh must write a canonical PLAIN ExecStart=/usr/local/bin/camera-box (its own \
         line, no baked flags) (#450)"
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
    assert!(
        body.contains("could not auto-detect a USB headset"),
        "setup-device.sh STEP 5 must fail loud when it cannot auto-detect a USB headset card \
         (#450)"
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
    let missing_check_idx =
        first_noncomment_idx(&body, "half-configured box").expect("half-configured box fail call present");
    let complete_idx = first_noncomment_idx(&body, "Setup Complete!")
        .expect("Setup Complete! banner present");
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
