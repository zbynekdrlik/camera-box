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

#[path = "support/temp_path.rs"]
mod temp_path;
use temp_path::TempPath;

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

fn tmpdir(tag: &str) -> TempPath {
    let dir = tempfile::Builder::new()
        .prefix(&format!("imag-nb-test-{tag}-"))
        .tempdir()
        .unwrap();
    TempPath {
        path: dir.path().to_path_buf(),
        _dir: dir,
    }
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

/// Secure Boot ON installs the SAME two layers as Secure Boot off (issue 1412). The
/// enhanced-secureboot delta is Ubuntu's TPM-backed FDE variant: it ships `boot-managed-by-snapd`,
/// which conflicts with grub-efi-amd64, so the chroot's GRUB install failed on stream-pp
/// (11.10.2026, Secure Boot on). Secure Boot is served by shim-signed + grub-efi-amd64-signed.
#[test]
fn layer_chain_with_secureboot_never_stacks_the_snapd_boot_layer_1412() {
    let d = tmpdir("sb-on");
    let casper = d.join("casper");
    fake_casper(&casper);
    let (code, out, err) = run_sourced(&format!("imag_layer_chain '{}' enabled", casper.display()));
    assert_eq!(code, 0, "layer chain should resolve. stderr: {err}");
    let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2, "expected the 2 base layers, got {lines:?}");
    assert!(
        lines[0].ends_with("/minimal.squashfs") && lines[1].ends_with("/minimal.standard.squashfs"),
        "minimal then minimal.standard; got {lines:?}"
    );
    assert!(
        !out.contains("enhanced-secureboot"),
        "the snapd-boot (TPM FDE) layer must never be installed: {out}"
    );
    assert!(
        !out.contains(".live."),
        "the .live layer must NEVER be installed: {out}"
    );
}

/// The chroot installs the signed boot chain that serves Secure Boot (issue 1412).
#[test]
fn chroot_installs_the_signed_shim_and_grub_for_secure_boot_1412() {
    let body = std::fs::read_to_string(script()).unwrap();
    let line = body
        .lines()
        .find(|l| l.contains("apt-get install") && l.contains("grub-efi-amd64-signed"))
        .expect("the chroot boot-chain apt-get install line");
    assert!(line.contains("shim-signed"), "{line}");
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

/// issue 1412: the chroot installed linux-generic-hwe-26.04 7.0.0-30 while the archive already had
/// 7.0.0-38, and the lowlatency config meta exists ONLY at 7.0.0-38, so setup's step 11 failed on
/// every fresh box (new strih notebook, stream-pp, strih-pp). The kernel meta version is the newest
/// one its lowlatency twin also has.
#[test]
fn kernel_meta_version_is_the_newest_the_lowlatency_meta_also_has_1412() {
    let cases = [
        ("7.0.0-38.38\n7.0.0-30.30", "7.0.0-38.38", "7.0.0-38.38"),
        (
            "7.0.0-40.40\n7.0.0-38.38\n7.0.0-30.30",
            "7.0.0-38.38\n7.0.0-30.30",
            "7.0.0-38.38",
        ),
        ("7.0.0-30.30", "", ""),
        ("7.0.0-30.30", "7.0.0-38.38", ""),
    ];
    for (meta, ll, want) in cases {
        let (code, out, err) = run_sourced(&format!(
            "imag_kernel_meta_version \"$(printf '{meta}')\" \"$(printf '{ll}')\""
        ));
        assert_eq!(code, 0, "{meta} / {ll}: {err}");
        assert_eq!(out.trim(), want, "{meta} / {ll}");
    }
    let chroot = std::fs::read_to_string(script()).unwrap();
    assert!(
        chroot.contains("imag_kernel_meta_version \"\\$meta_vers\" \"\\$ll_vers\""),
        "the chroot must pick the kernel meta version through imag_kernel_meta_version"
    );
}

/// issue 1412: the NetworkManager profile is named after the box (`<hostname up to the first ->-lan`),
/// never the imag literal: stream-pp came up with an `imag-lan` profile and the owner asked why.
#[test]
fn nm_profile_name_follows_the_hostname_1412() {
    for (host, want) in [
        ("stream-pp", "stream-lan"),
        ("strih-pp", "strih-lan"),
        ("strih-lx", "strih-lan"),
        ("imag-nb", "imag-lan"),
        ("imag", "imag-lan"),
    ] {
        let (code, out, err) = run_sourced(&format!("imag_nm_con_id {host}"));
        assert_eq!(code, 0, "{host}: {err}");
        assert_eq!(out.trim(), want, "{host}");
    }
    let body = std::fs::read_to_string(script()).unwrap();
    assert!(
        !body.contains("NM_CON_ID=\"imag-lan\""),
        "the profile name must not be the imag literal"
    );
}

/// issue 1412: the live layers ship cloud-init. On the installed stream-pp its
/// cloud-init-network.service waited 4 min for a datasource on every boot (the owner saw a spinner),
/// and its 50-cloud-init.yaml netplan added a second DHCP profile. The installed system disables it.
#[test]
fn installed_system_disables_cloud_init_1412() {
    let d = tmpdir("cloudinit");
    let root = d.join("root");
    std::fs::create_dir_all(root.join("etc/cloud")).unwrap();
    std::fs::create_dir_all(root.join("etc/netplan")).unwrap();
    std::fs::write(root.join("etc/netplan/50-cloud-init.yaml"), "network: {}\n").unwrap();
    std::fs::write(
        root.join("etc/netplan/01-network-manager-all.yaml"),
        "network: {}\n",
    )
    .unwrap();
    let (code, _out, err) = run_sourced(&format!("imag_disable_cloud_init '{}'", root.display()));
    assert_eq!(code, 0, "{err}");
    assert!(root.join("etc/cloud/cloud-init.disabled").exists());
    assert!(!root.join("etc/netplan/50-cloud-init.yaml").exists());
    assert!(root
        .join("etc/netplan/01-network-manager-all.yaml")
        .exists());
    // a root without cloud-init stays fine
    let bare = d.join("bare");
    std::fs::create_dir_all(bare.join("etc")).unwrap();
    let (code, _out, err) = run_sourced(&format!("imag_disable_cloud_init '{}'", bare.display()));
    assert_eq!(code, 0, "{err}");
    let body = std::fs::read_to_string(script()).unwrap();
    assert!(
        body.contains("imag_disable_cloud_init \"$MOUNT_ROOT\""),
        "the install flow must call it on the target root"
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

/// issue 1317 (live failure 18.9. 19:57): the Ubuntu 26.04 live layers ship
/// `/etc/apt/sources.list.d/cdrom.sources` (deb822, `URIs: file:///cdrom`, `Suites: resolute`) and
/// `copy_rootfs` copies it into the target. Inside the chroot `/cdrom` is not mounted, so apt aborts
/// with `E: The repository 'file:/cdrom resolute Release' does not have a Release file` and the
/// chroot script dies before `apt-get install openssh-server …` (no sshd, no bootloader). The pure
/// `imag_apt_drop_cdrom_sources <etc-apt-dir>` helper removes every `*.sources` whose `URIs:` line
/// points at `file:///cdrom` (or `cdrom:`) and strips `deb cdrom:` / `deb-src cdrom:` lines from a
/// classic `sources.list` — the archive sources are left byte-identical.
#[test]
fn drop_cdrom_sources_removes_the_live_iso_apt_sources_only() {
    let d = tmpdir("cdrom-drop");
    let etc_apt = d.join("etc/apt");
    let sld = etc_apt.join("sources.list.d");
    std::fs::create_dir_all(&sld).unwrap();

    // The 26.04 live-ISO cdrom source (deb822, URIs: file:///cdrom).
    let cdrom_sources = sld.join("cdrom.sources");
    std::fs::write(
        &cdrom_sources,
        "Types: deb\nURIs: file:///cdrom\nSuites: resolute\nComponents: main restricted\n",
    )
    .unwrap();

    // The archive deb822 source must be left byte-identical.
    let ubuntu_sources = sld.join("ubuntu.sources");
    std::fs::write(
        &ubuntu_sources,
        "Types: deb\nURIs: http://archive.ubuntu.com/ubuntu/\nSuites: resolute\nComponents: main universe\n",
    )
    .unwrap();
    let ubuntu_before = std::fs::read(&ubuntu_sources).unwrap();

    // A classic sources.list carrying one cdrom line and one http line.
    let list = etc_apt.join("sources.list");
    std::fs::write(
        &list,
        "deb cdrom:[Ubuntu 26.04]/ resolute main\ndeb http://archive.ubuntu.com/ubuntu resolute main\n",
    )
    .unwrap();

    let (code, out, err) = run_sourced(&format!(
        "imag_apt_drop_cdrom_sources '{}'",
        etc_apt.display()
    ));
    assert_eq!(code, 0, "drop should succeed. stderr: {err}");

    assert!(
        !cdrom_sources.exists(),
        "the live-ISO cdrom.sources must be removed"
    );
    assert!(
        ubuntu_sources.exists(),
        "the archive ubuntu.sources must survive"
    );
    assert_eq!(
        std::fs::read(&ubuntu_sources).unwrap(),
        ubuntu_before,
        "the archive ubuntu.sources must be byte-identical"
    );
    assert!(
        out.contains("dropped") && out.contains("cdrom.sources"),
        "it must report the dropped file on stdout: {out}"
    );

    let list_after = std::fs::read_to_string(&list).unwrap();
    assert!(
        !list_after.lines().any(|l| l.contains("cdrom:")),
        "the `deb cdrom:` line must be stripped: {list_after}"
    );
    assert!(
        list_after
            .lines()
            .any(|l| l.contains("deb http://archive.ubuntu.com/ubuntu resolute main")),
        "the http archive line must survive: {list_after}"
    );
}

/// A target apt dir with no cdrom source is the steady state after this runs once — it must be a
/// no-op (idempotent) and exit 0, never touch the archive sources.
#[test]
fn drop_cdrom_sources_is_idempotent_when_nothing_matches() {
    let d = tmpdir("cdrom-none");
    let etc_apt = d.join("etc/apt");
    let sld = etc_apt.join("sources.list.d");
    std::fs::create_dir_all(&sld).unwrap();
    let ubuntu = sld.join("ubuntu.sources");
    std::fs::write(
        &ubuntu,
        "Types: deb\nURIs: http://archive.ubuntu.com/ubuntu/\nSuites: resolute\nComponents: main\n",
    )
    .unwrap();
    let list = etc_apt.join("sources.list");
    std::fs::write(
        &list,
        "deb http://archive.ubuntu.com/ubuntu resolute main\n",
    )
    .unwrap();
    let ubuntu_before = std::fs::read(&ubuntu).unwrap();
    let list_before = std::fs::read(&list).unwrap();

    let (code, _out, err) = run_sourced(&format!(
        "imag_apt_drop_cdrom_sources '{}'",
        etc_apt.display()
    ));
    assert_eq!(code, 0, "a clean apt dir must exit 0. stderr: {err}");
    assert_eq!(
        std::fs::read(&ubuntu).unwrap(),
        ubuntu_before,
        "ubuntu.sources must be untouched"
    );
    assert_eq!(
        std::fs::read(&list).unwrap(),
        list_before,
        "sources.list must be untouched"
    );
}

/// A missing apt dir is a broken/unexpected target — fail LOUD (non-zero), never silently pass
/// (script-failure-policy).
#[test]
fn drop_cdrom_sources_fails_loud_on_a_missing_dir() {
    let d = tmpdir("cdrom-missing");
    let etc_apt = d.join("etc/apt-does-not-exist");
    let (code, _out, err) = run_sourced(&format!(
        "imag_apt_drop_cdrom_sources '{}'",
        etc_apt.display()
    ));
    assert_ne!(
        code, 0,
        "a missing apt dir must fail loud, not silently pass"
    );
    assert!(
        err.to_lowercase().contains("apt"),
        "the error should name the apt-dir requirement: {err}"
    );
}

/// Static anchor: the chroot step must DROP the live-ISO cdrom apt source BEFORE it runs the apt
/// update — otherwise the update aborts on the unreadable `file:///cdrom` source and the whole
/// chroot configuration dies. The call must sit on `/etc/apt` and must be unique.
#[test]
fn the_chroot_drops_the_cdrom_apt_source_before_updating() {
    let (code, chroot_fn, err) = run_sourced("declare -f configure_in_chroot");
    assert_eq!(code, 0, "configure_in_chroot must exist. stderr: {err}");
    assert_eq!(
        chroot_fn
            .matches("imag_apt_drop_cdrom_sources /etc/apt")
            .count(),
        1,
        "the cdrom-drop call anchor must appear exactly once: {chroot_fn}"
    );
    let call = chroot_fn
        .find("imag_apt_drop_cdrom_sources /etc/apt")
        .expect("the chroot step must drop the live-ISO cdrom apt source on /etc/apt");
    let update = chroot_fn
        .find("apt-get update -qq")
        .expect("the chroot step must run the apt update");
    assert!(
        call < update,
        "the cdrom apt source must be dropped BEFORE the apt update (call at {call}, update at {update})"
    );
}

/// issue 1317 (review hardening): the helper runs inside the chroot under `set -euo pipefail`
/// (`configure_in_chroot`'s heredoc), NOT the `set -uo pipefail` of `run_sourced`. A `grep` that
/// finds nothing must not abort the chroot mid-configuration. Prove it directly: source under
/// `set -euo pipefail` and assert a clean (no-match) apt dir still exits 0, and a matching dir is
/// still cleaned without aborting.
#[test]
fn drop_cdrom_sources_survives_set_e_in_the_chroot() {
    fn run_set_e(etc_apt: &std::path::Path) -> (i32, String) {
        let harness = format!(
            "set -euo pipefail\n. \"$SCRIPT\"\nimag_apt_drop_cdrom_sources '{}'\n",
            etc_apt.display()
        );
        let out = Command::new("bash")
            .arg("-c")
            .arg(&harness)
            .env("SCRIPT", script())
            .output()
            .expect("failed to run set -e harness");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    // Clean dir: a `grep` finding nothing must NOT abort under set -e.
    let d = tmpdir("cdrom-set-e");
    let etc_apt = d.join("etc/apt");
    let sld = etc_apt.join("sources.list.d");
    std::fs::create_dir_all(&sld).unwrap();
    std::fs::write(
        sld.join("ubuntu.sources"),
        "Types: deb\nURIs: http://archive.ubuntu.com/ubuntu/\nSuites: resolute\nComponents: main\n",
    )
    .unwrap();
    std::fs::write(
        etc_apt.join("sources.list"),
        "deb http://archive.ubuntu.com/ubuntu resolute main\n",
    )
    .unwrap();
    let (code, err) = run_set_e(&etc_apt);
    assert_eq!(
        code, 0,
        "clean dir must not abort under set -e. stderr: {err}"
    );

    // Matching dir: still cleans and exits 0 under set -e.
    let cdrom = sld.join("cdrom.sources");
    std::fs::write(
        &cdrom,
        "Types: deb\nURIs: file:///cdrom\nSuites: resolute\nComponents: main restricted\n",
    )
    .unwrap();
    let (code, err) = run_set_e(&etc_apt);
    assert_eq!(
        code, 0,
        "matching dir must exit 0 under set -e. stderr: {err}"
    );
    assert!(
        !cdrom.exists(),
        "the cdrom source must be removed under set -e"
    );
}
