//! Functional (execution) guard for `scripts/install-imag-nb.sh`'s PURE helpers (#815).
//!
//! The script itself installs an OS onto a real disk (partitions, rsync, chroot, grub) — none of
//! that is testable off-rig. What IS testable, and what actually decides whether the install is
//! correct, are its pure decisions: WHICH squashfs layers of the live ISO get copied (skipping the
//! `.live` layer is the whole difference between "a real installed system" and "a copy of the live
//! session"), how partition device names are derived (`/dev/nvme0n1` -> `p1`, `/dev/sda` -> `1`),
//! and the exact fstab / NetworkManager keyfile text written into the target.
//!
//! Same convention as `tests/setup_device_pure_functions.rs` / `tests/setup_imag_pure_functions.rs`:
//! SOURCE the real script (its `BASH_SOURCE[0] != $0` guard skips the destructive install flow) and
//! run the pure functions directly.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/install-imag-nb.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the REAL script and run `body` against its pure functions.
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

/// Build a fake casper dir carrying the FULL layer set a real Ubuntu 24.04.2 desktop ISO ships
/// (verified live on the 10.77.9.187 live-USB, 2026-07-27): the base + standard layers, the
/// live-only layer, the enhanced-secureboot variants, and a pile of per-language layers.
fn fake_casper(dir: &std::path::Path) {
    let names = [
        "minimal.squashfs",
        "minimal.standard.squashfs",
        "minimal.standard.live.squashfs",
        "minimal.enhanced-secureboot.squashfs",
        "minimal.standard.enhanced-secureboot.squashfs",
        "minimal.no-languages.squashfs",
        "minimal.de.squashfs",
        "minimal.en.squashfs",
        "minimal.standard.fr.squashfs",
        "minimal.standard.enhanced-secureboot.ru.squashfs",
        "minimal.standard.zh.squashfs",
    ];
    std::fs::create_dir_all(dir).unwrap();
    for n in names {
        std::fs::write(dir.join(n), b"x").unwrap();
    }
    // non-squashfs siblings that must never be picked up
    std::fs::write(dir.join("initrd"), b"x").unwrap();
    std::fs::write(dir.join("filesystem.manifest"), b"x").unwrap();
}

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("imag-nb-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Secure Boot OFF: the install layers are base + standard, LOWEST FIRST, and NOTHING else — no
/// `.live` layer (that one carries casper/the live session and would make the installed system a
/// live-session clone), no per-language deltas, no enhanced-secureboot variant.
#[test]
fn layer_chain_without_secureboot_is_base_then_standard_only() {
    let d = tmpdir("sb-off");
    let casper = d.join("casper");
    fake_casper(&casper);
    let (code, out, err) =
        run_sourced(&format!("imag_layer_chain '{}' disabled", casper.display()));
    assert_eq!(code, 0, "layer chain should resolve. stderr: {err}");
    let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
    let base = casper.join("minimal.squashfs");
    let std_l = casper.join("minimal.standard.squashfs");
    assert_eq!(
        lines,
        vec![base.display().to_string(), std_l.display().to_string()],
        "expected exactly base then standard, lowest first; got {lines:?}"
    );
    assert!(
        !out.contains(".live."),
        "the .live layer must NEVER be installed: {out}"
    );
}

/// Secure Boot ON: the enhanced-secureboot delta is appended LAST (it is a delta on
/// minimal.standard, so it must sit highest in the overlay stack).
#[test]
fn layer_chain_with_secureboot_appends_enhanced_layer_last() {
    let d = tmpdir("sb-on");
    let casper = d.join("casper");
    fake_casper(&casper);
    let (code, out, err) = run_sourced(&format!("imag_layer_chain '{}' enabled", casper.display()));
    assert_eq!(code, 0, "layer chain should resolve. stderr: {err}");
    let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 3, "expected 3 layers, got {lines:?}");
    assert!(
        lines[2].ends_with("minimal.standard.enhanced-secureboot.squashfs"),
        "enhanced-secureboot must be the TOP layer; got {lines:?}"
    );
    assert!(
        !out.contains(".live."),
        "the .live layer must NEVER be installed: {out}"
    );
    assert!(
        !lines
            .iter()
            .any(|l| l.ends_with("minimal.enhanced-secureboot.squashfs")),
        "the minimal-only secureboot variant is the wrong chain branch; got {lines:?}"
    );
}

/// A casper dir missing the base layer is a broken/unknown ISO — fail LOUD rather than install a
/// half rootfs (script-failure-policy).
#[test]
fn layer_chain_fails_loud_when_base_layer_missing() {
    let d = tmpdir("nolayers");
    let casper = d.join("casper");
    std::fs::create_dir_all(&casper).unwrap();
    std::fs::write(casper.join("minimal.standard.squashfs"), b"x").unwrap();
    let (code, _out, err) =
        run_sourced(&format!("imag_layer_chain '{}' disabled", casper.display()));
    assert_ne!(code, 0, "missing base layer must fail");
    assert!(
        err.to_lowercase().contains("minimal.squashfs"),
        "the error must name what is missing: {err}"
    );
}

/// Partition device naming differs between NVMe (`p` infix) and SATA/USB — getting this wrong
/// formats the WRONG node (or nothing).
#[test]
fn partition_names_follow_the_disk_kind() {
    let (code, out, err) = run_sourced(
        r#"printf '%s %s %s %s\n' "$(imag_part_name /dev/nvme0n1 1)" "$(imag_part_name /dev/nvme0n1 2)" "$(imag_part_name /dev/sda 1)" "$(imag_part_name /dev/mmcblk0 2)""#,
    );
    assert_eq!(code, 0, "part naming should succeed. stderr: {err}");
    assert_eq!(
        out.trim(),
        "/dev/nvme0n1p1 /dev/nvme0n1p2 /dev/sda1 /dev/mmcblk0p2"
    );
}

/// fstab must mount BY UUID (a device-path fstab breaks the moment a USB stick shifts the naming)
/// and carry both the ext4 root and the vfat ESP.
#[test]
fn fstab_is_uuid_based_and_covers_root_and_esp() {
    let (code, out, err) = run_sourced("imag_fstab ROOT-UUID-1111 ESP-UUID-2222");
    assert_eq!(code, 0, "fstab render should succeed. stderr: {err}");
    assert!(
        out.contains("UUID=ROOT-UUID-1111 / ext4"),
        "root line missing/wrong: {out}"
    );
    assert!(
        out.contains("UUID=ESP-UUID-2222 /boot/efi vfat"),
        "ESP line missing/wrong: {out}"
    );
    assert!(
        !out.contains("/dev/"),
        "fstab must not reference device paths: {out}"
    );
}

/// The install layers carry NO kernel — verified live on a real Ubuntu 24.04.2 desktop ISO
/// (2026-07-27, 10.77.9.187): `minimal.squashfs` has an empty `/boot` (memtest only) and no
/// `/lib/modules` at all; the kernel lives in the `.live` layer we deliberately skip, and in the
/// ISO pool. So the installed system MUST get its kernel from apt inside the chroot, and the
/// "is there a kernel" assertion must run AFTER that install — asserting it right after the rsync
/// (the first cut of this script) aborts every install on a correctly-copied rootfs.
#[test]
fn kernel_is_installed_in_the_chroot_not_expected_from_the_layers() {
    let (code, chroot_fn, err) = run_sourced("declare -f configure_in_chroot");
    assert_eq!(code, 0, "configure_in_chroot must exist. stderr: {err}");
    assert!(
        chroot_fn.contains("linux-generic") || chroot_fn.contains("linux-image-generic"),
        "the chroot step must install a kernel package: {chroot_fn}"
    );
    assert!(
        chroot_fn.contains("vmlinuz"),
        "the chroot step must verify a kernel actually landed: {chroot_fn}"
    );

    let (code, copy_fn, err) = run_sourced("declare -f copy_rootfs");
    assert_eq!(code, 0, "copy_rootfs must exist. stderr: {err}");
    assert!(
        !copy_fn.contains("vmlinuz"),
        "copy_rootfs must NOT gate on a kernel — the layers never carry one: {copy_fn}"
    );
    assert!(
        copy_fn.contains("os-release"),
        "copy_rootfs should still prove it copied a real rootfs (os-release): {copy_fn}"
    );
}

/// setup-imag.sh REQUIRES NetworkManager (`nmcli`) and a static address — the installed system must
/// come up on the intended IP by itself, without a manual nmcli session after first boot.
#[test]
fn nm_keyfile_pins_the_static_address() {
    let (code, out, err) =
        run_sourced("imag_nm_keyfile imag-lan 10.77.9.187 23 10.77.9.1 10.77.9.1");
    assert_eq!(code, 0, "keyfile render should succeed. stderr: {err}");
    assert!(out.contains("method=manual"), "not static: {out}");
    assert!(
        out.contains("address1=10.77.9.187/23,10.77.9.1"),
        "address/prefix/gateway line missing: {out}"
    );
    assert!(out.contains("dns=10.77.9.1"), "dns missing: {out}");
    assert!(
        out.contains("[connection]") && out.contains("id=imag-lan"),
        "keyfile must be a valid NM connection profile: {out}"
    );
}

/// issue 1317 (owner ROZHODNUTÉ 18.9.): the strih-lx notebook runs Ubuntu 26.04, so the chroot
/// kernel meta can no longer be hardcoded to noble's `linux-generic-hwe-24.04` (that package does
/// not exist on 26.04 — the install would fail or ship no kernel). The chroot step must DERIVE the
/// kernel meta from the target release via the pure `imag_kernel_meta_package` decision, reading
/// the target's `VERSION_ID` from `/etc/os-release`. The #819 HWE intent is preserved
/// (`linux-generic-hwe-<VERSION_ID>` when available) — it just follows the release now.
#[test]
fn the_chroot_kernel_is_release_derived_not_hardcoded_noble() {
    let (code, chroot_fn, err) = run_sourced("declare -f configure_in_chroot");
    assert_eq!(code, 0, "configure_in_chroot must exist. stderr: {err}");
    assert!(
        chroot_fn.contains("imag_kernel_meta_package"),
        "the chroot step must derive the kernel meta via imag_kernel_meta_package: {chroot_fn}"
    );
    assert!(
        chroot_fn.contains("os-release"),
        "the chroot step must read the target VERSION_ID from /etc/os-release: {chroot_fn}"
    );
    assert!(
        !chroot_fn.contains("linux-generic-hwe-24.04"),
        "the hardcoded noble kernel meta must be gone (the release is derived now): {chroot_fn}"
    );
    assert!(
        chroot_fn.contains("vmlinuz"),
        "the chroot step must still verify a kernel actually landed: {chroot_fn}"
    );
    assert!(
        !regex_lite_has_bare_ga_install(&chroot_fn),
        "a bare `apt-get install … linux-generic` literal (the GA line) must not reappear — the \
         install uses the derived meta variable: {chroot_fn}"
    );
}

/// `apt-get install … linux-generic …` with the literal `linux-generic` argument (not the derived
/// `$kernel_meta` variable). The release-derived install passes the variable, so this must stay
/// false — a reappearance means someone re-hardcoded a GA/HWE literal.
fn regex_lite_has_bare_ga_install(body: &str) -> bool {
    body.lines()
        .any(|l| l.contains("apt-get install") && l.contains("linux-generic"))
}

/// issue 1317: `imag_kernel_meta_package VERSION_ID AVAILABLE` derives the apt kernel meta from the
/// target release. AVAILABLE is the newline-separated `apt-cache pkgnames linux-generic` list from
/// the TARGET's apt cache. It prefers the HWE meta `linux-generic-hwe-<VERSION_ID>` when that exact
/// name is available (26.04 ships `linux-generic-hwe-26.04`; noble ships `-hwe-24.04`), falls back
/// to the GA `linux-generic` when it is not, and fails loud (non-zero) when the release is unknown —
/// never a blind default.
#[test]
fn kernel_meta_package_prefers_hwe_when_available_else_ga_and_needs_a_version() {
    // 26.04 with the HWE meta available -> the 26.04 HWE meta.
    let (code, out, err) = run_sourced(
        "imag_kernel_meta_package 26.04 $'linux-generic\\nlinux-generic-hwe-26.04\\nlinux-image-generic'",
    );
    assert_eq!(code, 0, "26.04 derive should succeed. stderr: {err}");
    assert_eq!(out.trim(), "linux-generic-hwe-26.04");

    // 24.04 with its HWE meta available -> the 24.04 HWE meta (noble sticks still work).
    let (code, out, _e) =
        run_sourced("imag_kernel_meta_package 24.04 $'linux-generic\\nlinux-generic-hwe-24.04'");
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "linux-generic-hwe-24.04");

    // release known but the HWE meta is NOT in the available list -> the GA fallback.
    let (code, out, _e) =
        run_sourced("imag_kernel_meta_package 26.04 $'linux-generic\\nlinux-image-generic'");
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "linux-generic");

    // empty available list -> GA fallback (never crash).
    let (code, out, _e) = run_sourced("imag_kernel_meta_package 26.04 ''");
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "linux-generic");

    // empty VERSION_ID -> non-zero (a release must be known, never a silent default).
    let (code, _out, _e) = run_sourced("imag_kernel_meta_package '' $'linux-generic-hwe-26.04'");
    assert_ne!(
        code, 0,
        "an unknown release must fail loud, not default silently"
    );
}
