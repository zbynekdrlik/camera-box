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
    // Assert the EXACT hold command (all three meta-packages in one line), not a bare
    // `apt-mark hold` substring — the latter also matches the explanatory `// ... apt-mark hold`
    // comment, so it would false-pass even if the real command were deleted.
    const HOLD_CMD: &str = "apt-mark hold linux-image-generic linux-headers-generic linux-generic";
    for script in SETUP_SCRIPTS {
        let body = read(script);
        assert!(
            body.contains(HOLD_CMD),
            "{script} must run `{HOLD_CMD}` so a surprise kernel can never be installed on the \
             appliance (the auto-installed 6.8.0-124 kernel bricked CAM3/CAM4, #295)"
        );
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
        // Ordering: the initrd guarantee must be INVOKED before update-grub, or grub could still be
        // regenerated while an initrd-less kernel is present. Index over non-comment COMMAND lines
        // only (a comment may merely *mention* update-grub). Crucially, key on the INVOCATION, not a
        // mere textual mention: a script that defines a helper function (whose body holds
        // `update-initramfs -c -k`) but CALLS it after update-grub (or not at all) must still fail.
        let cmds: Vec<&str> = body
            .lines()
            .map(str::trim_start)
            .filter(|t| !t.starts_with('#'))
            .collect();
        let grub_idx = cmds
            .iter()
            .position(|t| t.contains("update-grub"))
            .expect("update-grub command present");
        // What counts as "guarantee initrds for all kernels" being invoked: either the inline
        // `update-initramfs -c -k` loop, or a CALL to the helper (its name WITHOUT the `() {`
        // definition suffix). The function DEFINITION line is excluded so it can't satisfy this.
        let is_initrd_guarantee_invocation = |t: &str| -> bool {
            t.contains("update-initramfs -c -k")
                || (t.contains("camera_box_ensure_all_kernels_have_initrd") && !t.contains("() {"))
        };
        // The function-body `update-initramfs -c -k` (a definition, sorts near the top of the file)
        // must NOT be what satisfies the ordering: require an invocation that is itself before grub
        // AND is not merely the definition body. We do this by checking that among the command lines
        // BEFORE update-grub there is an invocation, and that the LAST invocation before grub is a
        // real call/inline step rather than only the definition body.
        let invocation_before_grub = cmds[..grub_idx].iter().any(|t| {
            is_initrd_guarantee_invocation(t)
                // exclude the helper's own definition body lines: the inline loop and the call both
                // run unconditionally; the definition body only runs when called. We treat a bare
                // `update-initramfs -c -k` as an invocation ONLY in scripts that do not DEFINE the
                // helper (inline style); scripts that define the helper must show the CALL.
                && !(body.contains("camera_box_ensure_all_kernels_have_initrd() {")
                    && t.contains("update-initramfs -c -k"))
        });
        assert!(
            invocation_before_grub,
            "{script} must INVOKE the initrd guarantee (inline `update-initramfs -c -k` loop, or a \
             call to camera_box_ensure_all_kernels_have_initrd) BEFORE update-grub — otherwise \
             update-grub can still default-boot an initrd-less kernel (#295)"
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
        // The guard must (a) read the generated grub.cfg and extract its default menuentry, and
        // (b) grep THAT entry for an initrd line — not merely mention "initrd" somewhere (the ensure
        // loop references /boot/initrd.img independently). A specific grep-for-initrd validation line
        // is what catches a regression that drops the brick-prevention check.
        assert!(
            body.contains("grub.cfg") && body.contains("menuentry "),
            "{script} must read /boot/grub/grub.cfg and extract its default menuentry to validate it (#295)"
        );
        let validates_default_has_initrd = body
            .lines()
            .any(|l| l.contains("grep") && l.contains("initrd"));
        assert!(
            validates_default_has_initrd,
            "{script} must grep the extracted grub default entry for an initrd line and abort if absent \
             — without it grub could still default-boot an initrd-less kernel (#295)"
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
            .unwrap_or("")
            .to_uppercase();
        let size_tok = size_tok.as_str();
        // Accept a K/M/G suffix (case-insensitive) or a bare byte count; convert to MiB.
        let mib: u32 = if let Some(g) = size_tok.strip_suffix('G') {
            g.parse::<u32>().unwrap_or(0) * 1024
        } else if let Some(m) = size_tok.strip_suffix('M') {
            m.parse::<u32>().unwrap_or(0)
        } else if let Some(k) = size_tok.strip_suffix('K') {
            k.parse::<u32>().unwrap_or(0) / 1024
        } else {
            // bare byte count (e.g. size=536870912) → MiB
            size_tok
                .parse::<u64>()
                .map(|b| (b / (1024 * 1024)) as u32)
                .unwrap_or(0)
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

// ---------------------------------------------------------------------------------------------
// #307 — extend the #295 boot-hardening to the two builders the setup scripts do NOT cover:
//   (1) `scripts/create-usb-linux.sh` — the MASTER base-image builder. It builds the "clean Ubuntu
//       + SSH only" image that `setup.sh` later hardens, so there is a narrow first-boot window
//       (before setup.sh runs) where the original brick exposure exists. It must carry the same
//       kernel-pin + unattended-upgrades-off + saved-grub-default hardening at the source.
//   (2) `scripts/build-image.sh` `install_bootloader` — the durable ro-root+overlay image. It pins
//       a saved grub default but never VALIDATES (like the setup scripts do) that the generated
//       default entry has BOTH a kernel image AND an initrd before pinning. Add the same fail-closed
//       guard so the durable image stays consistent with setup.sh / setup-device.sh.
// Style mirrors the #295 tests above: read the REAL scripts and assert on the REAL contract.

/// The master base-image builder ("clean Ubuntu + SSH" image that setup.sh later hardens).
const BASE_IMAGE_BUILDER: &str = "scripts/create-usb-linux.sh";

/// The durable read-only-root + overlay image builder (the long-term appliance target, #301).
const RO_IMAGE_BUILDER: &str = "scripts/build-image.sh";

/// Extract a top-level shell function body (`name() { ... }`) from a script. Returns the text from
/// the `name() {` line through the matching closing `}` (these scripts close their functions with a
/// bare `}` at column 0). Lets a test scope its assertions to ONE function so a mention elsewhere in
/// the file cannot false-pass — the guard must live where the default is actually pinned.
fn extract_shell_function(script: &str, name: &str) -> Option<String> {
    let lines: Vec<&str> = script.lines().collect();
    let start = lines.iter().position(|l| {
        let t = l.trim_start();
        t.starts_with(&format!("{name}()")) || t.starts_with(&format!("{name} ()"))
    })?;
    let mut out = Vec::new();
    for line in &lines[start..] {
        out.push(*line);
        if *line == "}" {
            break;
        }
    }
    Some(out.join("\n"))
}

/// 6. The base-image builder must PIN the appliance kernel — the same `apt-mark hold` the setup
///    scripts use. Without it the base image can gain a surprise kernel in the window before setup.sh
///    runs, recreating the #295 exposure at the source.
#[test]
fn base_image_builder_holds_the_appliance_kernel() {
    const HOLD_CMD: &str = "apt-mark hold linux-image-generic linux-headers-generic linux-generic";
    let body = read(BASE_IMAGE_BUILDER);
    assert!(
        body.contains(HOLD_CMD),
        "{BASE_IMAGE_BUILDER} must run `{HOLD_CMD}` so the base image can never gain a surprise \
         kernel before setup.sh hardens the box (#307, extends #295)"
    );
}

/// 7. The base-image builder must disable unattended (automatic) upgrades — the same mechanism the
///    setup scripts use (`/etc/apt/apt.conf.d/20auto-upgrades` with periodic = 0). An active
///    unattended-upgrades is HOW the brick kernel auto-installed.
#[test]
fn base_image_builder_disables_automatic_kernel_upgrades() {
    let body = read(BASE_IMAGE_BUILDER);
    assert!(
        body.contains("20auto-upgrades"),
        "{BASE_IMAGE_BUILDER} must write /etc/apt/apt.conf.d/20auto-upgrades to turn OFF unattended \
         upgrades on the base image — an active unattended-upgrades auto-installed the kernel that \
         bricked CAM3/CAM4 (#307, extends #295)"
    );
    assert!(
        body.contains(r#"APT::Periodic::Unattended-Upgrade "0""#),
        "{BASE_IMAGE_BUILDER} must set `APT::Periodic::Unattended-Upgrade \"0\"` so the base image \
         never auto-installs a kernel before setup.sh runs (#307, extends #295)"
    );
}

/// 8. The base-image builder must pin a SAVED grub default instead of the hardcoded `GRUB_DEFAULT=0`
///    (boot-newest). Boot-newest is exactly how an initrd-less auto-installed kernel became the
///    default and bricked CAM3/CAM4. Replace it with `GRUB_DEFAULT=saved` + `grub-set-default 0`.
#[test]
fn base_image_builder_pins_a_saved_grub_default() {
    let body = read(BASE_IMAGE_BUILDER);
    assert!(
        body.contains("GRUB_DEFAULT=saved"),
        "{BASE_IMAGE_BUILDER} must set GRUB_DEFAULT=saved so the base image pins an explicitly-saved \
         known-good kernel as the default, not whatever is newest (#307, extends #295)"
    );
    assert!(
        !body.contains("GRUB_DEFAULT=0"),
        "{BASE_IMAGE_BUILDER} must NOT keep the hardcoded GRUB_DEFAULT=0 (boot-newest) — that is how \
         an initrd-less kernel became the default and bricked CAM3/CAM4 (#307, extends #295)"
    );
    assert!(
        body.contains("grub-set-default 0"),
        "{BASE_IMAGE_BUILDER} must `grub-set-default 0` after update-grub to pin the known-good \
         kernel as the saved default (#307, extends #295)"
    );
}

/// 9. The ro-root+overlay image builder's `install_bootloader` must carry the SAME fail-closed
///    grub-default validation the setup scripts use: before pinning the saved default, read the
///    generated /boot/grub/grub.cfg, extract its default menuentry, and assert it references BOTH a
///    kernel image AND an initrd — aborting loudly otherwise. Scope the assertions to the function
///    body so a mention elsewhere cannot satisfy the guard.
#[test]
fn ro_image_builder_validates_grub_default_before_pinning() {
    let body = read(RO_IMAGE_BUILDER);
    let func = extract_shell_function(&body, "install_bootloader")
        .expect("build-image.sh must define an install_bootloader function");
    assert!(
        func.contains("grub.cfg") && func.contains("menuentry "),
        "install_bootloader must read the generated /boot/grub/grub.cfg and extract its default \
         menuentry to validate it before pinning (#307, mirrors setup.sh)"
    );
    let validates_kernel_image = func
        .lines()
        .any(|l| l.contains("grep") && (l.contains("vmlinuz") || l.contains("linux ")));
    assert!(
        validates_kernel_image,
        "install_bootloader must grep the extracted grub default entry for a kernel image line \
         (vmlinuz / linux) before pinning (#307)"
    );
    let validates_initrd = func
        .lines()
        .any(|l| l.contains("grep") && l.contains("initrd"));
    assert!(
        validates_initrd,
        "install_bootloader must grep the extracted grub default entry for an initrd line and abort \
         if absent — without it the image could default-boot an initrd-less kernel (#307)"
    );
    assert!(
        func.contains("error ") || func.contains("exit 1"),
        "install_bootloader's grub-default validation must FAIL CLOSED — abort loudly (error/exit) \
         when the default entry lacks a kernel image or initrd (#307)"
    );
    // Ordering: the validation must run BEFORE grub-set-default pins the default, or a brickable
    // default could still be pinned.
    let validate_idx = func
        .find("initrd")
        .expect("initrd validation present in install_bootloader");
    let pin_idx = func
        .find("grub-set-default")
        .expect("grub-set-default present in install_bootloader");
    assert!(
        validate_idx < pin_idx,
        "install_bootloader must validate the grub default (kernel image + initrd) BEFORE \
         grub-set-default pins it (#307)"
    );
}
