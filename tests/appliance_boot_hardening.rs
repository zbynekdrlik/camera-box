//! #295 — "make it impossible to brick a cam box again" boot-hardening guard.
//!
//! Two cam boxes (CAM3 + CAM4) became unbootable after an `update-grub` defaulted (GRUB_DEFAULT=0
//! → newest) to an auto-installed `6.8.0-124` kernel that had NO generated initrd → the kernel
//! could not mount root. Root causes, confirmed against the live survivors (.61/.62/.64):
//!   1. `unattended-upgrades` was ACTIVE → it auto-installed a new kernel on an appliance.
//!   2. That kernel never got an initrd (a FULL 100M `/var/cache` tmpfs broke apt with ENOSPC),
//!      yet a later `update-grub` happily made it the default boot entry.
//!   3. `/var/cache` tmpfs was sized inconsistently across the fleet (100M vs 500M) — provisioning
//!      drift that made the ENOSPC failure box-specific.
//!
//! The durable fix lives in the PROVISIONING scripts (`setup.sh` + `setup-device.sh`) so a
//! re-provisioned box can NEVER recreate the brick — not a one-off live edit (live grub edits are
//! exactly what bricked the boxes). These tests pin the load-bearing contract of that fix:
//!   - the appliance kernel is PINNED (`apt-mark hold`) AND automatic kernel upgrades are disabled,
//!   - every installed kernel is GUARANTEED an initrd before `update-grub` runs, and future kernel
//!     installs regenerate a missing initrd via a `/etc/kernel/postinst.d` hook,
//!   - grub never default-boots a kernel lacking an initrd: `GRUB_DEFAULT=saved` + `grub-set-default`
//!     to the known-good kernel, with a validation guard on the generated default entry,
//!   - `/var/cache` is sized uniformly and adequately (≥512M) so apt can never ENOSPC.
//!
//! RED before the hardening (none of the above present in the scripts); GREEN after.
//!
//! Style follows the repo's other script guards (`cluster_clock_setup.rs`,
//! `harness_deploy_fleet.rs`): read the REAL provisioning scripts and assert on the REAL contract.

use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Both provisioning paths that build a live (rw-root) cam box. The brick reproduced on boxes
/// provisioned by these scripts, so BOTH must carry the hardening (a fix in only one leaves the
/// other path able to recreate the brick).
const SETUP_SCRIPTS: [&str; 2] = ["scripts/setup.sh", "scripts/setup-device.sh"];

/// Minimum `/var/cache` tmpfs size (MiB). 100M filled up on .61 and broke apt with ENOSPC, which
/// is what left the auto-installed kernel without an initrd. The fix sizes it uniformly ≥512M.
const MIN_VAR_CACHE_MIB: u32 = 512;

/// 1. The appliance kernel must be PINNED so a surprise kernel can never be installed.
///    `apt-mark hold` on the kernel meta-packages stops even a manual `apt upgrade` from moving the
///    kernel — the strongest guarantee that the box keeps booting the kernel it was provisioned with.
#[test]
fn provisioning_holds_the_appliance_kernel() {
    for script in SETUP_SCRIPTS {
        let body = read(script);
        assert!(
            body.contains("apt-mark hold"),
            "{script} must `apt-mark hold` the kernel meta-packages so a surprise kernel can never \
             be installed on the appliance (the auto-installed 6.8.0-124 kernel bricked CAM3/CAM4, #295)"
        );
        for pkg in [
            "linux-image-generic",
            "linux-headers-generic",
            "linux-generic",
        ] {
            assert!(
                body.contains(pkg),
                "{script}'s kernel hold must cover {pkg} so the whole kernel meta-package set is \
                 pinned (#295)"
            );
        }
    }
}

/// 2. Automatic KERNEL upgrades must be disabled. `unattended-upgrades` being active is HOW the bad
///    kernel auto-installed; an appliance must never silently gain a new kernel. The provisioning
///    must turn the periodic unattended-upgrade off in `/etc/apt/apt.conf.d/20auto-upgrades`.
#[test]
fn provisioning_disables_automatic_kernel_upgrades() {
    for script in SETUP_SCRIPTS {
        let body = read(script);
        assert!(
            body.contains("20auto-upgrades"),
            "{script} must write /etc/apt/apt.conf.d/20auto-upgrades to turn OFF unattended upgrades \
             — an active unattended-upgrades auto-installed the kernel that bricked CAM3/CAM4 (#295)"
        );
        assert!(
            body.contains(r#"APT::Periodic::Unattended-Upgrade "0""#),
            "{script} must set `APT::Periodic::Unattended-Upgrade \"0\"` so kernels (and everything \
             else) are never auto-installed on the appliance (#295)"
        );
    }
}

/// 3. Every installed kernel must be GUARANTEED an initrd before `update-grub` runs, and future
///    kernel installs must regenerate a missing initrd. A kernel without an initrd that becomes the
///    grub default is the exact brick — `update-grub` must never be reachable with an initrd-less
///    kernel present.
#[test]
fn provisioning_guarantees_initrd_for_every_kernel_before_grub() {
    for script in SETUP_SCRIPTS {
        let body = read(script);
        assert!(
            body.contains("update-initramfs -c -k"),
            "{script} must generate a missing initrd for every installed kernel \
             (`update-initramfs -c -k <ver>`) — a kernel without an initrd bricked CAM3/CAM4 (#295)"
        );
        // A future kernel install must also self-heal: a /etc/kernel/postinst.d hook regenerates a
        // missing initrd (it runs before grub's own zz-update-grub postinst hook).
        assert!(
            body.contains("/etc/kernel/postinst.d/"),
            "{script} must install a /etc/kernel/postinst.d hook so any future kernel install always \
             gets an initrd before grub is regenerated (#295)"
        );
        // Ordering: the initrd guarantee must run BEFORE update-grub, or grub could still be
        // regenerated while an initrd-less kernel is present. Compare the first *command* occurrence
        // of each — ignore comment lines, since a comment may merely *mention* "update-grub".
        let first_cmd_line = |needle: &str| -> Option<usize> {
            body.lines().position(|l| {
                let t = l.trim_start();
                !t.starts_with('#') && t.contains(needle)
            })
        };
        let initrd_line = first_cmd_line("update-initramfs -c -k")
            .expect("update-initramfs -c -k command present");
        let grub_line = first_cmd_line("update-grub").expect("update-grub command present");
        assert!(
            initrd_line < grub_line,
            "{script} must guarantee initrds (update-initramfs -c -k) BEFORE it runs update-grub — \
             otherwise update-grub can still default-boot an initrd-less kernel (#295)"
        );
    }
}

/// 4. grub must never default-boot a kernel lacking an initrd. The provisioning pins the default to
///    the known-good kernel (`GRUB_DEFAULT=saved` + `grub-set-default`) and validates the generated
///    default entry references BOTH a kernel image AND an initrd.
#[test]
fn provisioning_pins_a_safe_grub_default() {
    for script in SETUP_SCRIPTS {
        let body = read(script);
        assert!(
            body.contains("GRUB_DEFAULT=saved"),
            "{script} must set GRUB_DEFAULT=saved so the default boot entry is the explicitly pinned \
             known-good kernel, not whatever happens to be newest (#295)"
        );
        assert!(
            body.contains("grub-set-default"),
            "{script} must `grub-set-default` to the known-good kernel after regenerating grub (#295)"
        );
        // The generated default entry must be validated to contain both a kernel image and an initrd
        // line — the guard reads the produced grub.cfg.
        assert!(
            body.contains("grub.cfg"),
            "{script} must validate the generated /boot/grub/grub.cfg default entry (#295)"
        );
        assert!(
            body.contains("initrd"),
            "{script} must validate the default grub entry references an initrd (#295)"
        );
    }
}

/// 5. `/var/cache` must be sized uniformly and adequately (≥512M) so apt can never ENOSPC and leave
///    a kernel without an initrd. The provisioning drift (100M vs 500M) is removed.
#[test]
fn provisioning_sizes_var_cache_adequately() {
    for script in SETUP_SCRIPTS {
        let body = read(script);
        // Find the fstab tmpfs line for /var/cache and read its size= value.
        let line = body
            .lines()
            .find(|l| l.contains("/var/cache") && l.contains("tmpfs") && l.contains("size="))
            .unwrap_or_else(|| {
                panic!("{script} must mount /var/cache as a sized tmpfs in fstab (#295)")
            });
        let size_tok = line
            .split("size=")
            .nth(1)
            .and_then(|s| s.split([',', ' ']).next())
            .unwrap_or("");
        // Accept M or G suffix; convert to MiB.
        let mib: u32 = if let Some(g) = size_tok.strip_suffix('G') {
            g.parse::<u32>().unwrap_or(0) * 1024
        } else if let Some(m) = size_tok.strip_suffix('M') {
            m.parse::<u32>().unwrap_or(0)
        } else {
            0
        };
        assert!(
            mib >= MIN_VAR_CACHE_MIB,
            "{script} sizes /var/cache tmpfs to `{size_tok}` (= {mib} MiB) — it must be ≥{MIN_VAR_CACHE_MIB}M \
             so apt can never ENOSPC and leave a kernel without an initrd (#295). Line: {line}"
        );
    }
}

/// 5b. The long-term target (booting from the existing ro-root+overlay image) must be documented so
///     the operational re-image is tracked, not lost. (The live re-image itself is out of scope —
///     #301 — but the direction must be written down.)
#[test]
fn setup_md_documents_the_ro_root_overlay_target() {
    let raw = read("SETUP.md");
    let doc = raw.to_lowercase();
    assert!(
        doc.contains("build-image.sh"),
        "SETUP.md must reference scripts/build-image.sh as the long-term ro-root+overlay image (#295)"
    );
    assert!(
        doc.contains("read-only") || doc.contains("ro-root") || doc.contains("overlay"),
        "SETUP.md must document the read-only-root + overlay direction as the durable appliance target (#295)"
    );
    assert!(
        raw.contains("#295"),
        "SETUP.md must tie the hardening section to #295 so the rationale is traceable"
    );
}
