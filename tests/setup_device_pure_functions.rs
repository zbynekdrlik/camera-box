//! Functional (execution) guard for `scripts/setup-device.sh`'s pure #450 name-resolution helper
//! (`resolve_device_name`).
//!
//! `tests/setup_device_provisioner_hardening.rs` pins the CONTRACT textually (sourcing
//! camera-set.sh, calling camera_resolve, no baked positional args) but a purely textual guard
//! cannot catch a silent LOGIC bug (e.g. the uppercase/lowercase normalization flipped, or the
//! wrong camera-set.sh field copied into DEVICE_IP/VBAN_STREAM). This file closes that gap by
//! actually SOURCING the real script and RUNNING `resolve_device_name` against the REAL
//! `scripts/camera-set.sh` fleet map -- same convention as `tests/setup_imag_pure_functions.rs` /
//! `tests/genlock_manifest.rs::run_sourced`.
//!
//! setup-device.sh's `BASH_SOURCE[0] != $0` guard (right after the pure function definitions)
//! makes sourcing safe: everything below the guard (the destructive one-shot provisioning flow --
//! `[ "$EUID" -eq 0 ] || fail ...`, `apt-get`, `netplan`, grub edits, etc.) is skipped, and only
//! `fail()` / `resolve_device_name()` / the sourced `camera-set.sh` functions are defined in the
//! sourcing shell.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/setup-device.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the REAL script (its `BASH_SOURCE != $0` guard skips the destructive provisioning
/// flow) and run `body` against its pure functions. Returns (exit_code, stdout, stderr).
fn run_sourced(body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", script())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// resolve_device_name must accept the canonical uppercase hostname form and derive IP / VBAN
/// stream / genlock FPS from the real camera-set.sh fleet map (cam5 -> 10.77.9.65, #451).
#[test]
fn resolve_device_name_resolves_uppercase_name() {
    let (code, out, err) = run_sourced(
        r#"resolve_device_name CAM5
           printf 'NAME=%s IP=%s STREAM=%s FPS=%s\n' "$DEVICE_NAME" "$DEVICE_IP" "$VBAN_STREAM" "$CAMERA_GENLOCK_FPS""#,
    );
    assert_eq!(
        code, 0,
        "resolve_device_name CAM5 should succeed. stderr: {err}"
    );
    assert_eq!(
        out.trim(),
        "NAME=CAM5 IP=10.77.9.65 STREAM=cam5 FPS=60",
        "resolve_device_name must derive DEVICE_NAME (uppercase hostname) / DEVICE_IP / \
         VBAN_STREAM (lowercase stream) / CAMERA_GENLOCK_FPS from the real camera-set.sh map"
    );
}

/// Case-insensitivity: a lowercase or mixed-case input must resolve identically to the uppercase
/// form -- the historical hostname convention is uppercase, but camera-set.sh's own table keys
/// are lowercase (cam1..cam7, #753).
#[test]
fn resolve_device_name_is_case_insensitive() {
    for input in ["cam3", "Cam3", "CAM3", "cAm3"] {
        let (code, out, err) = run_sourced(&format!(
            r#"resolve_device_name {input}
               printf 'NAME=%s IP=%s STREAM=%s\n' "$DEVICE_NAME" "$DEVICE_IP" "$VBAN_STREAM""#
        ));
        assert_eq!(
            code, 0,
            "resolve_device_name {input} should succeed. stderr: {err}"
        );
        assert_eq!(
            out.trim(),
            "NAME=CAM3 IP=10.77.9.63 STREAM=cam3",
            "resolve_device_name {input} must resolve identically regardless of input case"
        );
    }
}

/// An unknown camera name must fail loud (non-zero exit), never silently fall through to a
/// default box -- camera-set.sh's own fail-closed `case` rejects it and resolve_device_name must
/// propagate that as a hard exit, not let a careless caller ignore a nonzero return.
#[test]
fn resolve_device_name_fails_loud_on_unknown_name() {
    let (code, out, _err) = run_sourced(
        r#"resolve_device_name bogus9
           echo "UNREACHABLE"
           echo "$DEVICE_NAME""#,
    );
    assert_ne!(
        code, 0,
        "resolve_device_name bogus9 must exit non-zero on an unknown camera name"
    );
    assert!(
        !out.contains("UNREACHABLE"),
        "resolve_device_name must exit immediately on an unknown name -- no code after the call \
         should run. stdout: {out}"
    );
}

/// An empty name must also fail loud (never silently resolve to some default camera).
#[test]
fn resolve_device_name_fails_loud_on_empty_name() {
    let (code, out, _err) = run_sourced(
        r#"resolve_device_name ""
           echo "UNREACHABLE""#,
    );
    assert_ne!(code, 0, "resolve_device_name '' must exit non-zero");
    assert!(!out.contains("UNREACHABLE"));
}

/// Every fleet camera (cam1-cam7) must resolve through the real setup-device.sh + camera-set.sh
/// pairing -- a broad sweep so a future fleet-map edit (#451 added cam5-6, #753 added cam7)
/// can't silently break one name while the others still pass.
#[test]
fn resolve_device_name_resolves_the_whole_fleet() {
    let expected = [
        ("cam1", "CAM1", "10.77.9.61"),
        ("cam2", "CAM2", "10.77.9.62"),
        ("cam3", "CAM3", "10.77.9.63"),
        ("cam4", "CAM4", "10.77.9.64"),
        ("cam5", "CAM5", "10.77.9.65"),
        ("cam6", "CAM6", "10.77.9.66"),
        ("cam7", "CAM7", "10.77.9.67"),
    ];
    for (input, want_name, want_ip) in expected {
        let (code, out, err) = run_sourced(&format!(
            r#"resolve_device_name {input}
               printf '%s %s\n' "$DEVICE_NAME" "$DEVICE_IP""#
        ));
        assert_eq!(
            code, 0,
            "resolve_device_name {input} should succeed. stderr: {err}"
        );
        assert_eq!(
            out.trim(),
            format!("{want_name} {want_ip}"),
            "resolve_device_name {input} resolved incorrectly"
        );
    }
}

// #593's "cam7 was never built" negative test (resolve_device_name_rejects_cam7_not_yet_built)
// is REMOVED here (#753, 2026-07-14): cam7 is a real, provisioned box now and resolves like
// every other fleet member (folded into resolve_device_name_resolves_the_whole_fleet above).
// The general "unknown camera name fails loud" behavior stays covered by
// resolve_device_name_fails_loud_on_unknown_name ("bogus9") below -- no coverage is lost.

// --- #568: color block deduped onto scripts/lib/cli-log.sh --------------------------------------
//
// setup-device.sh used to declare its own RED/GREEN/YELLOW/NC block (no log()/info()/warn()/err()
// functions -- it uses raw `echo -e "${COLOR}...${NC}"` everywhere, plus its own exit-on-call
// `fail()`). #568 replaces that local block with a `. "$HERE/lib/cli-log.sh"` source, keeping
// `fail()` itself untouched (different message shape / behavior from cli-log.sh's `err()`, so it
// stays script-local per the issue's own guidance). These tests prove the color values -- and
// therefore every existing `echo -e "${RED}...${NC}"` call site -- render EXACTLY the same bytes
// as the old local declaration (RED='\033[0;31m', GREEN='\033[0;32m', YELLOW='\033[1;33m',
// NC='\033[0m').

#[test]
fn color_vars_are_byte_identical_to_the_pre_568_local_declaration() {
    // Route through `echo -e` (like every real call site in the script), NOT a raw `printf '%s'`
    // -- RED/GREEN/YELLOW/NC hold LITERAL backslash-octal text (`'\033[0;31m'`, single-quoted, no
    // ANSI-C `$'...'` quoting); only `echo -e`'s own escape processing turns that into the actual
    // ESC byte at render time, exactly as fail() and every `echo -e "${RED}...${NC}"` line do.
    let (code, out, err) = run_sourced(r#"echo -en "${RED}|${GREEN}|${YELLOW}|${NC}""#);
    assert_eq!(
        code, 0,
        "sourcing setup-device.sh must succeed. stderr: {err}"
    );
    assert_eq!(
        out, "\x1b[0;31m|\x1b[0;32m|\x1b[1;33m|\x1b[0m",
        "RED/GREEN/YELLOW/NC must keep the exact escape codes setup-device.sh declared locally \
         before #568, now sourced from scripts/lib/cli-log.sh instead"
    );
}

#[test]
fn fail_renders_the_exact_pre_568_bytes_after_sourcing_the_shared_lib() {
    let (code, _out, err) = run_sourced("fail 'boom'");
    assert_eq!(code, 1, "fail() must still exit 1");
    assert_eq!(
        err, "\x1b[0;31mFAIL: boom\x1b[0m\n",
        "fail()'s rendered bytes must be unchanged by #568 (RED + 'FAIL: msg' + NC, to stderr)"
    );
}

// --- #453: fleet cruft self-heal -- .bak cleanup during provisioning ----------------------------
//
// Live fleet fingerprint (2026-07-06, issue #453) found inert `.bak` leftovers from a manual NDI
// upgrade / a stale drop-in edit on cam1/cam2/cam4: `/usr/lib/ndi/libndi.so.6*.bak` and
// `camera-box.service.d/*.bak*`. Neither is loaded by anything (ldconfig never resolves a `.bak`
// suffix; systemd only reads `*.conf` drop-ins) -- pure inert cruft that setup-device.sh should
// self-heal on every (re-)provisioning pass instead of carrying forward forever.
//
// `cleanup_bak_cruft` is exact glob-scoped to the given DIR + PATTERN(s) only -- these tests use a
// real tempdir (never touching the host filesystem) to prove it removes ONLY matching cruft and
// leaves everything else (including a real, non-cruft same-prefixed file) untouched.

#[test]
fn cleanup_bak_cruft_removes_matching_files_and_is_idempotent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    std::fs::write(dir.join("libndi.so.6.2.1.bak"), b"stale").unwrap();
    std::fs::write(dir.join("libndi.so.6.bak"), b"stale2").unwrap();
    std::fs::write(dir.join("libndi.so.6"), b"live").unwrap(); // must survive -- not a .bak

    let (code, out, err) = run_sourced(&format!(
        r#"cleanup_bak_cruft '{dir}' 'libndi.so.6*.bak'
           cleanup_bak_cruft '{dir}' 'libndi.so.6*.bak'"#, // idempotent: run twice, no error
        dir = dir.display()
    ));
    assert_eq!(code, 0, "cleanup_bak_cruft must succeed. stderr: {err}");
    assert!(
        out.contains("libndi.so.6.2.1.bak") && out.contains("libndi.so.6.bak"),
        "cleanup_bak_cruft should report each removed file; got: {out:?}"
    );
    assert!(!dir.join("libndi.so.6.2.1.bak").exists());
    assert!(!dir.join("libndi.so.6.bak").exists());
    assert!(
        dir.join("libndi.so.6").exists(),
        "cleanup_bak_cruft must NEVER remove a real (non-.bak) file, even with a matching prefix"
    );
}

#[test]
fn cleanup_bak_cruft_is_a_silent_noop_when_nothing_matches() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    std::fs::write(dir.join("libndi.so.6"), b"live").unwrap();

    let (code, out, err) = run_sourced(&format!(
        "cleanup_bak_cruft '{}' 'libndi.so.6*.bak'",
        dir.display()
    ));
    assert_eq!(code, 0, "no-op run must still succeed. stderr: {err}");
    assert_eq!(out, "", "no cruft present -- nothing should be reported");
    assert!(dir.join("libndi.so.6").exists());
}

#[test]
fn cleanup_bak_cruft_supports_multiple_glob_patterns_scoped_to_the_dir() {
    // The systemd drop-in dir cleanup needs BOTH `*.bak` and `*.bak-*` (cam1's live
    // `genlock.conf.bak-30`), and must never touch a real `*.conf` drop-in.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    std::fs::write(dir.join("genlock.conf.bak-30"), b"stale").unwrap();
    std::fs::write(dir.join("cpu-affinity.conf.bak"), b"stale").unwrap();
    std::fs::write(dir.join("genlock.conf"), b"live").unwrap();
    std::fs::write(dir.join("cpu-affinity.conf"), b"live").unwrap();

    let (code, _out, err) = run_sourced(&format!(
        "cleanup_bak_cruft '{}' '*.bak' '*.bak-*'",
        dir.display()
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert!(!dir.join("genlock.conf.bak-30").exists());
    assert!(!dir.join("cpu-affinity.conf.bak").exists());
    assert!(dir.join("genlock.conf").exists());
    assert!(dir.join("cpu-affinity.conf").exists());
}

#[test]
fn cleanup_bak_cruft_skips_a_bak_named_directory_without_aborting() {
    // A `.bak`-suffixed DIRECTORY matches the glob but `rm -f` cannot remove it and would exit 1,
    // aborting the whole `set -e` provisioner uncontrolled. The cleanup must SKIP a non-regular /
    // non-symlink match, still remove the real `.bak` file beside it, and exit 0.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    std::fs::create_dir(dir.join("oops.bak")).unwrap(); // a directory that matches `*.bak`
    std::fs::write(dir.join("genlock.conf.bak"), b"stale").unwrap(); // a real file that must go

    let (code, _out, err) = run_sourced(&format!("cleanup_bak_cruft '{}' '*.bak'", dir.display()));
    assert_eq!(
        code, 0,
        "a stray .bak-named directory must NOT abort provisioning. stderr: {err}"
    );
    assert!(
        dir.join("oops.bak").is_dir(),
        "the .bak-named directory must be skipped, not removed"
    );
    assert!(
        !dir.join("genlock.conf.bak").exists(),
        "a real .bak file beside the skipped directory must still be cleaned"
    );
}

// ---------------------------------------------------------------------------------------------
// Wiring — the cleanup must actually be CALLED from STEP 4 (NDI dir) and STEP 7 (systemd
// drop-in dir), not just defined as a dead pure function nobody invokes.
// ---------------------------------------------------------------------------------------------

#[test]
fn bak_cruft_cleanup_is_wired_into_ndi_and_dropin_provisioning_steps() {
    let body = std::fs::read_to_string(script()).unwrap();
    let guard_pos = body
        .find("stop here -- never run the destructive")
        .expect("source-guard comment must still be present");
    let live_flow = &body[guard_pos..];
    assert!(
        live_flow.contains("cleanup_bak_cruft /usr/lib/ndi"),
        "STEP 4 (NDI library) must call cleanup_bak_cruft on /usr/lib/ndi (#453)"
    );
    assert!(
        live_flow.contains("cleanup_bak_cruft /etc/systemd/system/camera-box.service.d"),
        "STEP 7 (systemd service) must call cleanup_bak_cruft on the drop-in dir (#453)"
    );
}

// ---------------------------------------------------------------------------------------------
// #1087 — STEP 7 bakes the publish-30p.conf drop-in so a re-provisioned box keeps the secondary
// "CAMn (30p)" 30fps blend stream (issue 792). The binary defaults the feature OFF; this env
// drop-in is what enables it, byte-faithful to the live fleet file. Enable-only (written now,
// effective on the box's next reboot) -- proven post-reboot by verify-device.sh's (y) check.
// ---------------------------------------------------------------------------------------------

#[test]
fn publish_30p_dropin_is_written_in_step_7() {
    let body = std::fs::read_to_string(script()).unwrap();
    let guard_pos = body
        .find("stop here -- never run the destructive")
        .expect("source-guard comment must still be present");
    let live_flow = &body[guard_pos..];
    assert!(
        live_flow.contains("/etc/systemd/system/camera-box.service.d/publish-30p.conf"),
        "STEP 7 must write the publish-30p.conf drop-in so a re-provisioned box keeps the (30p) \
         blend stream (issue 792 / #1087)"
    );
    assert!(
        live_flow.contains("CAMERA_BOX_PUBLISH_30P=1"),
        "the publish-30p.conf drop-in must set CAMERA_BOX_PUBLISH_30P=1 (issue 792 / #1087)"
    );
}

// ---------------------------------------------------------------------------------------------
// #599 — ensure_root_writable() / restore_root_mode(): STEP 15-18 run apt-get/dpkg/systemctl +
// write files under /etc, all of which require a writable root. On a FIRST provisioning run root
// is naturally rw (the ro fstab STEP 18 writes only takes effect on the NEXT reboot), but on an
// IN-PLACE RE-RUN against an already-booted ro appliance, root is `ro` -- every apt-get/dpkg call
// in STEP 15-17 then fails and is swallowed by the `|| true` guards, silently leaving a
// purge/install that never took effect while the script still reports success (#599).
//
// `findmnt`/`mount`/`systemctl` are stubbed as bash FUNCTIONS defined AFTER sourcing the real
// script (function definitions shadow same-named binaries for the rest of the shell), so these
// tests exercise the REAL decision + side-effect functions from setup-device.sh without touching
// the host's actual mount state.
// ---------------------------------------------------------------------------------------------

#[test]
fn root_mount_is_readonly_matches_verify_device_semantics() {
    // Mirrors verify-device.sh's function of the same name/contract (#547/#599): only a mount
    // options string whose FIRST comma-token is exactly "ro" counts as read-only; "errors=remount-ro"
    // on an rw mount must NOT be misread as ro.
    for (opts, want) in [
        ("ro", "RO"),
        ("ro,relatime", "RO"),
        ("rw,relatime", "NOT_RO"),
        ("rw,relatime,errors=remount-ro", "NOT_RO"),
        ("", "NOT_RO"),
    ] {
        let (code, out, err) = run_sourced(&format!(
            r#"root_mount_is_readonly '{opts}' && echo RO || echo NOT_RO"#
        ));
        assert_eq!(
            code, 0,
            "harness itself must succeed for opts='{opts}'. stderr: {err}"
        );
        assert_eq!(
            out.trim(),
            want,
            "root_mount_is_readonly('{opts}') should report {want}"
        );
    }
}

#[test]
fn ensure_root_writable_remounts_rw_and_masks_packagekit_when_root_is_ro() {
    let (code, out, err) = run_sourced(
        r#"
        findmnt() { echo "ro,relatime"; }
        MOUNT_CALLS=""
        mount() { MOUNT_CALLS="$MOUNT_CALLS|$*"; return 0; }
        SYSTEMCTL_CALLS=""
        systemctl() { SYSTEMCTL_CALLS="$SYSTEMCTL_CALLS|$*"; return 0; }
        ensure_root_writable
        printf 'ROOT_WAS_RO=%s\nMOUNT_CALLS=%s\nSYSTEMCTL_CALLS=%s\n' "$ROOT_WAS_RO" "$MOUNT_CALLS" "$SYSTEMCTL_CALLS"
        "#,
    );
    assert_eq!(
        code, 0,
        "ensure_root_writable must succeed on a ro root it can remount. stderr: {err}"
    );
    assert!(
        out.contains("ROOT_WAS_RO=true"),
        "ROOT_WAS_RO must be set true when root started ro (#599). out: {out}"
    );
    assert!(
        out.contains("remount,rw /"),
        "ensure_root_writable must call `mount -o remount,rw /` when root is ro (#599). out: {out}"
    );
    assert!(
        out.contains("packagekit"),
        "ensure_root_writable must stop/mask packagekit before the apt steps run, or a later ro \
         remount can be blocked EBUSY by a D-Bus-reactivated PackageKit (rig-timesync-single-\
         authority incident, #599). out: {out}"
    );
}

#[test]
fn ensure_root_writable_is_noop_when_root_already_rw() {
    let (code, out, err) = run_sourced(
        r#"
        findmnt() { echo "rw,relatime"; }
        MOUNT_CALLS=""
        mount() { MOUNT_CALLS="$MOUNT_CALLS|$*"; return 0; }
        ensure_root_writable
        printf 'ROOT_WAS_RO=%s MOUNT_CALLS=%s\n' "$ROOT_WAS_RO" "$MOUNT_CALLS"
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out.trim(),
        "ROOT_WAS_RO=false MOUNT_CALLS=",
        "a first-provisioning run (root already rw) must never remount and must leave ROOT_WAS_RO \
         false -- STEP 18's ro fstab only takes effect on the next reboot (#599)"
    );
}

#[test]
fn ensure_root_writable_fails_loud_when_remount_rw_fails() {
    let (code, out, err) = run_sourced(
        r#"
        findmnt() { echo "ro,relatime"; }
        mount() { return 1; }
        systemctl() { return 0; }
        ensure_root_writable
        echo "UNREACHABLE"
        "#,
    );
    assert_ne!(
        code, 0,
        "ensure_root_writable must exit non-zero when `mount -o remount,rw /` fails -- a re-run \
         must never silently proceed on a still-ro root and later report success (#599)"
    );
    assert!(
        !out.contains("UNREACHABLE"),
        "fail() must stop execution immediately. stdout: {out}"
    );
    assert!(
        err.contains("FAIL:"),
        "ensure_root_writable must fail loud via fail(). stderr: {err}"
    );
}

#[test]
fn restore_root_mode_remounts_ro_when_this_run_remounted_rw() {
    let (code, out, err) = run_sourced(
        r#"
        ROOT_WAS_RO=true
        MOUNT_CALLS=""
        mount() { MOUNT_CALLS="$MOUNT_CALLS|$*"; return 0; }
        systemctl() { return 0; }
        restore_root_mode
        printf 'MOUNT_CALLS=%s\n' "$MOUNT_CALLS"
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("remount,ro /"),
        "restore_root_mode must remount root back to ro when THIS run had remounted rw (#599). \
         out: {out}"
    );
}

#[test]
fn restore_root_mode_is_noop_when_root_was_not_remounted() {
    let (code, out, err) = run_sourced(
        r#"
        MOUNT_CALLS=""
        mount() { MOUNT_CALLS="$MOUNT_CALLS|$*"; return 0; }
        restore_root_mode
        printf 'MOUNT_CALLS=%s\n' "$MOUNT_CALLS"
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out.trim(),
        "MOUNT_CALLS=",
        "a first-provisioning run must never be force-remounted ro at STEP 18 time -- ro only \
         takes effect via fstab on the next reboot (#599)"
    );
}

#[test]
fn restore_root_mode_fails_loud_when_remount_ro_fails() {
    let (code, out, err) = run_sourced(
        r#"
        ROOT_WAS_RO=true
        mount() { return 1; }
        systemctl() { return 0; }
        restore_root_mode
        echo "UNREACHABLE"
        "#,
    );
    assert_ne!(
        code, 0,
        "restore_root_mode must exit non-zero when the remount back to ro fails -- a box silently \
         left rw after \"Setup Complete!\" is exactly the false-success #599 exists to close"
    );
    assert!(!out.contains("UNREACHABLE"));
    assert!(err.contains("FAIL:"));
}

// ---------------------------------------------------------------------------------------------
// #782 -- interkom audio provisioning bake-in: setup-device.sh sources scripts/lib/interkom-audio.sh
// and (STEP 5) writes the by-NAME asound.conf + (STEP 16) applies the per-box Mic/PCM mixer gain.
// ---------------------------------------------------------------------------------------------

fn usb_script_body() -> String {
    let p = manifest_dir().join("scripts/create-usb-linux.sh");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// True if `needle` appears on a line that is NOT a `#` comment (mirrors the helper of the same
/// name in `setup_device_provisioner_hardening.rs`; each test binary is its own crate).
fn on_noncomment_line(body: &str, needle: &str) -> bool {
    body.lines()
        .any(|l| l.contains(needle) && !l.trim_start().starts_with('#'))
}

/// setup-device.sh must SOURCE the interkom-audio lib (the single source of truth) -- not inline a
/// second copy of the canonical asound.conf / per-box table (that duplication is the drift #782
/// exists to kill).
#[test]
fn setup_device_sources_interkom_audio_lib() {
    let body = std::fs::read_to_string(script()).unwrap();
    assert!(
        body.contains(r#". "$HERE/lib/interkom-audio.sh""#),
        "setup-device.sh must source scripts/lib/interkom-audio.sh"
    );
}

/// STEP 5 must write the asound.conf from the lib's canonical by-NAME generator, and must NOT bake
/// the old enumeration-time card NUMBER (`hw:$USB_CARD,0`) that dangles on re-enumeration (#728).
#[test]
fn step5_writes_by_name_asound_conf_via_lib_never_a_card_number() {
    let body = std::fs::read_to_string(script()).unwrap();
    assert!(
        body.contains("interkom_asound_conf_content > /etc/asound.conf"),
        "STEP 5 must write /etc/asound.conf from interkom_asound_conf_content (the lib SoT)"
    );
    // Negative check on NON-comment lines only: an explanatory comment may name the old form in
    // prose without it being a real WRITE (the #832 self-collision class).
    for bad in ["hw:$USB_CARD,0", "card $USB_CARD"] {
        assert!(
            !on_noncomment_line(&body, bad),
            "setup-device.sh must NOT write the old enumeration-time card-NUMBER asound.conf ('{bad}')"
        );
    }
}

/// STEP 5 must fail loud (never silently write a dangling config) if the HID card is not present --
/// the #450 fail-loud posture, but keyed on the card NAME now.
#[test]
fn step5_fails_loud_when_hid_card_absent() {
    let body = std::fs::read_to_string(script()).unwrap();
    assert!(
        body.contains(r"grep -qE '\[HID"),
        "STEP 5 must confirm a card NAMED HID exists on /proc/asound/cards"
    );
    // The guard's failure path is a `fail` call mentioning the HID headset.
    assert!(
        on_noncomment_line(&body, "no ALSA card named 'HID'"),
        "STEP 5 must `fail` with a clear message when the HID card is absent"
    );
}

/// STEP 16 must apply the per-box Mic/PCM gain via `amixer -c HID sset` and persist it with
/// `alsactl store`, reading the values from the lib's per-box table (never hard-coded literals).
#[test]
fn step16_applies_per_box_mixer_gain_and_persists() {
    let body = std::fs::read_to_string(script()).unwrap();
    for needle in [
        r#"interkom_mic_pct "$DEVICE_NAME""#,
        r#"interkom_pcm_pct "$DEVICE_NAME""#,
        r#"amixer -c HID sset Mic "${MIC_PCT}%""#,
        r#"amixer -c HID sset PCM "${PCM_PCT}%""#,
        "alsactl store",
    ] {
        assert!(
            body.contains(needle),
            "STEP 16 must contain `{needle}` to bake the per-box interkom gain"
        );
    }
}

/// The mixer gain MUST be applied AFTER alsa-utils is installed (amixer/alsactl land in STEP 16's
/// apt install) and BEFORE STEP 18 flips the root filesystem read-only (a late write to a ro root
/// fails). This ordering is the whole reason the gain lives in STEP 16, not STEP 5.
#[test]
fn mixer_gain_applied_after_alsa_utils_install_and_before_ro_flip() {
    let body = std::fs::read_to_string(script()).unwrap();
    let apt = body
        .find("apt-get install -y -qq avahi-daemon")
        .expect("STEP 16 apt install line");
    let amixer = body
        .find("amixer -c HID sset Mic")
        .expect("mixer-gain apply");
    let ro_flip = body
        .find("STEP 18: Configure read-only")
        .expect("STEP 18 ro-flip banner");
    assert!(
        apt < amixer && amixer < ro_flip,
        "the amixer gain apply must sit AFTER the alsa-utils apt install and BEFORE the STEP 18 \
         ro-flip (apt={apt} amixer={amixer} ro_flip={ro_flip})"
    );
}

/// alsa-utils must be installed by provisioning (setup-device.sh STEP 16 apt list) -- the cam1/cam3
/// drift was that they predate alsa-utils being in the list at all.
#[test]
fn alsa_utils_is_installed_in_provisioning() {
    let body = std::fs::read_to_string(script()).unwrap();
    assert!(
        on_noncomment_line(&body, "alsa-utils"),
        "setup-device.sh STEP 16 must apt-install alsa-utils (provides amixer/alsactl)"
    );
    // ...and the base image (create-usb-linux.sh) carries it too, so a fresh clone is not bare.
    let usb = usb_script_body();
    assert!(
        on_noncomment_line(&usb, "alsa-utils"),
        "create-usb-linux.sh base image must also install alsa-utils (#782 dual-bake)"
    );
}

/// COMPOSITION: sourcing the REAL setup-device.sh actually WIRES the lib in (not just a comment) --
/// the per-box table resolves through the sourced function. Proves the source line is live.
#[test]
fn setup_device_wires_per_box_gain_table() {
    let (code, out, err) = run_sourced(
        r#"printf '%s %s / %s %s\n' \
             "$(interkom_mic_pct CAM1)" "$(interkom_pcm_pct CAM1)" \
             "$(interkom_mic_pct CAM5)" "$(interkom_pcm_pct CAM5)""#,
    );
    assert_eq!(
        code, 0,
        "sourcing setup-device.sh must expose the lib functions. stderr: {err}"
    );
    assert_eq!(
        out.trim(),
        "75 79 / 80 94",
        "cam1-4 = Mic 75/PCM 79, cam5-7 = Mic 80/PCM 94 (owner's per-box table)"
    );
}

// ---------------------------------------------------------------------------------------------
// #1155 -- both netplan writers must pin the LAN stanza to the PCI NIC name (`enp*`), never a
// `driver: "*"` wildcard that also claims a USB CDC-NCM camera link and hands it the box IP + a
// duplicate default route (cam1 went PTP-deaf when a BMPCC camera was USB-plugged, 2026-08-20).
// ---------------------------------------------------------------------------------------------

/// BOTH writers -- setup-device.sh STEP 2 (static IP) and create-usb-linux.sh chroot base image
/// (DHCP) -- must carry `match: name: "enp*"` on a real (non-comment) config line and must NOT
/// carry `driver: "*"` on any non-comment line.
#[test]
fn both_netplan_writers_pin_lan_stanza_to_enp_never_driver_wildcard() {
    let setup = std::fs::read_to_string(script()).unwrap();
    let usb = usb_script_body();
    for (name, body) in [("setup-device.sh", &setup), ("create-usb-linux.sh", &usb)] {
        assert!(
            on_noncomment_line(body, r#"name: "enp*""#),
            "{name} netplan LAN stanza must pin `match: name: \"enp*\"` (the PCI NIC) (#1155)"
        );
        assert!(
            !on_noncomment_line(body, r#"driver: "*""#),
            "{name} must NOT match the LAN stanza with `driver: \"*\"` -- it also claims a USB \
             CDC-NCM camera link and steals the box IP (#1155)"
        );
    }
}
