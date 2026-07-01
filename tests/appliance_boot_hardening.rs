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

// ---------------------------------------------------------------------------------------------
// #362 — bake the NDI/audio RUNTIME deps into the image builders so a fresh clone can RUN
// camera-box without hand-provisioning. The fresh CAM3 USB clone (built for the #301 re-image)
// booted but camera-box crash-looped: libndi.so could not dlopen — the ALSA runtime + the avahi
// client/common libs were missing, /usr/lib/ndi was not on the dynamic-linker path, and there was
// no avahi-daemon for the mDNS NDI-source discovery libndi performs (a black --display receiver
// even though the source was reachable at TCP level). CAM3 was brought to parity by hand; these
// guards pin that the BUILDERS install + enable it so the next re-image never repeats the brick.
// avahi-daemon is mDNS only — no conflict with DanteSync's clock ownership (cam4 runs both).
// Style mirrors the #295/#307 guards above: read the REAL scripts and assert on the REAL contract.

/// Every path that builds a runnable cam box must carry the NDI/audio runtime deps:
///   - `create-usb-linux.sh` — the base-image builder (the fresh-clone source; the #362 incident),
///   - `setup.sh` / `setup-device.sh` — the two provisioning paths.
///
/// A gap in ANY one leaves a path that produces a camera-box which cannot dlopen libndi.
const NDI_RUNTIME_SCRIPTS: [&str; 3] = [
    "scripts/create-usb-linux.sh",
    "scripts/setup.sh",
    "scripts/setup-device.sh",
];

/// The exact runtime packages camera-box needs to load libndi + run intercom audio — each confirmed
/// MISSING on the fresh CAM3 clone (#362), each crash-looping camera-box in turn:
///   libasound2t64    — ALSA runtime (intercom); camera-box exited on missing libasound.so.2
///   libavahi-client3 — libndi links it; NDI load failed on libavahi-client.so.3 without it
///   libavahi-common3 — libndi links it; NDI load failed on libavahi-common.so.3 without it
///   avahi-daemon     — the mDNS daemon libndi browses for NDI source discovery
///   avahi-utils      — avahi-browse etc. for diagnosing discovery
const NDI_RUNTIME_PACKAGES: [&str; 5] = [
    "libasound2t64",
    "libavahi-client3",
    "libavahi-common3",
    "avahi-daemon",
    "avahi-utils",
];

/// True if `needle` appears on a "live command" line — not in a `#` comment and not in an `echo`
/// output line. Makes a content assertion revert-proof: deleting the real install/ldconfig command
/// must fail the test even when the name still appears in a nearby comment or an `echo "Installed:
/// ..."` summary (the #362 review caught this false-pass on setup-device.sh).
fn appears_on_active_line(body: &str, needle: &str) -> bool {
    body.lines().any(|l| {
        let t = l.trim_start();
        l.contains(needle) && !t.starts_with('#') && !l.contains("echo ")
    })
}

/// True if `needle` appears on a non-`#`-comment line. Like `appears_on_active_line` but WITHOUT the
/// echo-exclusion — for needles whose real command is itself an `echo … > file` write (the ndi.conf
/// write), where excluding `echo` lines would wrongly reject the real command. Still revert-proof
/// against a comment that merely mentions the needle.
fn on_noncomment_line(body: &str, needle: &str) -> bool {
    body.lines()
        .any(|l| l.contains(needle) && !l.trim_start().starts_with('#'))
}

/// 10. Every builder must INSTALL the full NDI/audio runtime package set. Without any one of them
///     camera-box crash-loops on a fresh box (missing libasound.so.2 / libavahi-*.so / no NDI find).
#[test]
fn builders_install_the_ndi_runtime_packages() {
    for script in NDI_RUNTIME_SCRIPTS {
        let body = read(script);
        for pkg in NDI_RUNTIME_PACKAGES {
            assert!(
                appears_on_active_line(&body, pkg),
                "{script} must install the NDI/audio runtime package `{pkg}` on a real command line \
                 (not just a comment/echo) — it was missing on the fresh CAM3 clone and crash-looped \
                 camera-box (#362)"
            );
        }
    }
}

/// 11. Every builder must put /usr/lib/ndi on the dynamic-linker path. Without
///     `/etc/ld.so.conf.d/ndi.conf` (= /usr/lib/ndi) + `ldconfig`, dlopen("libndi.so") fails with
///     "cannot open shared object file" even though the lib is present at /usr/lib/ndi (it is not on
///     the default linker path).
#[test]
fn builders_put_usr_lib_ndi_on_the_linker_path() {
    for script in NDI_RUNTIME_SCRIPTS {
        let body = read(script);
        assert!(
            on_noncomment_line(&body, "/etc/ld.so.conf.d/ndi.conf"),
            "{script} must write /etc/ld.so.conf.d/ndi.conf so libndi.so is on the linker path (#362)"
        );
        assert!(
            on_noncomment_line(&body, "/usr/lib/ndi"),
            "{script} must register /usr/lib/ndi as the NDI library directory (#362)"
        );
        assert!(
            appears_on_active_line(&body, "ldconfig"),
            "{script} must run ldconfig (on a real command line, not just a comment) after writing \
             ndi.conf so the linker cache picks up /usr/lib/ndi (#362)"
        );
    }
}

/// 12. Every builder must ENABLE avahi-daemon (not merely install it). libndi browses mDNS via
///     avahi-daemon for NDI source discovery — with no daemon running, NDI `find()` returns nothing
///     and a `--display` receiver stays black even when the source is reachable at TCP level (#362).
#[test]
fn builders_enable_avahi_daemon_for_ndi_discovery() {
    for script in NDI_RUNTIME_SCRIPTS {
        let body = read(script);
        assert!(
            on_noncomment_line(&body, "systemctl enable avahi-daemon"),
            "{script} must `systemctl enable avahi-daemon` so NDI mDNS source discovery works on a \
             fresh box — libndi find() returns nothing without it (#362)"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// #369 — "cam-box image: auto-grow root on first boot + uniform 512M /var/cache + fleet
// disk-space guard — prevent ENOSPC bricks."
//
// ROOT CAUSE: cam4 shipped a 3.5G/92%-full root partition (never grown) on a 57G disk;
// cam1's /var/cache was 100M/100%-full (old image; #306 fix didn't reach all boxes). Both
// the base-image builder (create-usb-linux.sh) and the ro-root builder (build-image.sh)
// lacked (a) a first-boot auto-grow mechanism and (b) the uniform 512M /var/cache fstab line.
//
// Fixes tested here:
//   13. Both image builders install cloud-guest-utils (provides growpart) so auto-grow can run.
//   14. Both image builders enable camera-box-grow-root.service so the root partition auto-
//       expands to fill the disk on first boot — no manual setup.sh run required.
//   15. Both image builders write a ≥512M /var/cache tmpfs line to fstab — matching the
//       setup.sh / setup-device.sh provisioning paths that already carry this line (#295).
//   16. The fleet disk-space guard threshold decision (scripts/lib/disk-guard-thresholds.sh)
//       is behaviorally correct: ≥80% triggers an alert, <80% does not.

/// The two image builders that were MISSING the grow-root + 512M /var/cache. The setup-script
/// paths (setup.sh + setup-device.sh) already carry these; these tests extend coverage to the
/// builders.
const IMAGE_BUILDERS: [&str; 2] = ["scripts/create-usb-linux.sh", "scripts/build-image.sh"];

/// 13. Both image builders must INSTALL cloud-guest-utils (growpart) so the first-boot
///     auto-grow service can run. Without growpart, the grow-root service silently falls back
///     but the package must be present for the happy path.
#[test]
fn image_builders_install_cloud_guest_utils() {
    for script in IMAGE_BUILDERS {
        let body = read(script);
        assert!(
            appears_on_active_line(&body, "cloud-guest-utils"),
            "{script} must install cloud-guest-utils (provides growpart) on a real command line \
             (not just a comment) so the first-boot root auto-grow service can run (#369, cam4 \
             shipped 3.5G/92%-full on a 57G disk because the partition was never grown)"
        );
    }
}

/// 14. Both image builders must ENABLE camera-box-grow-root.service so the root partition
///     auto-expands to fill the disk on first boot — no manual setup.sh run required. Without
///     this, a fresh USB key written to a large disk ships with the image's small root partition
///     and will fill up prematurely (the exact cam4 ENOSPC brick path, #369).
#[test]
fn image_builders_enable_grow_root_service() {
    for script in IMAGE_BUILDERS {
        let body = read(script);
        assert!(
            on_noncomment_line(&body, "camera-box-grow-root.service"),
            "{script} must reference camera-box-grow-root.service on a non-comment line \
             (copy/install + enable) so the root partition auto-grows on first boot (#369)"
        );
        assert!(
            on_noncomment_line(&body, "systemctl enable camera-box-grow-root"),
            "{script} must `systemctl enable camera-box-grow-root.service` so the service \
             is active on first boot — without it the partition never grows (#369)"
        );
    }
}

/// 15. Both image builders must write a ≥512M /var/cache tmpfs line to fstab. The setup
///     provisioning scripts (setup.sh + setup-device.sh) already carry this line (#295/#306);
///     the image builders were missing it, leaving cam1 with a 100M /var/cache that filled up
///     and triggered an apt ENOSPC → initrd-less kernel → brick (#369 follow-on to #295).
///
///     Reuses the same size-parsing logic as the existing provisioning_sizes_var_cache_adequately
///     test so the assertion is consistent across ALL provisioning paths.
#[test]
fn image_builders_size_var_cache_512m() {
    for script in IMAGE_BUILDERS {
        let body = read(script);
        let line = body
            .lines()
            .find(|l| l.contains("/var/cache") && l.contains("tmpfs") && l.contains("size="))
            .unwrap_or_else(|| {
                panic!(
                    "{script} must mount /var/cache as a sized tmpfs in fstab — the builder was \
                     missing this line (uniform 512M /var/cache prevents apt ENOSPC, #369/#295)"
                )
            });
        let size_tok = line
            .split("size=")
            .nth(1)
            .and_then(|s| s.split([',', ' ']).next())
            .unwrap_or("")
            .to_uppercase();
        let mib: u32 = if let Some(g) = size_tok.strip_suffix('G') {
            g.parse::<u32>().unwrap_or(0) * 1024
        } else if let Some(m) = size_tok.strip_suffix('M') {
            m.parse::<u32>().unwrap_or(0)
        } else if let Some(k) = size_tok.strip_suffix('K') {
            k.parse::<u32>().unwrap_or(0) / 1024
        } else {
            size_tok
                .parse::<u64>()
                .map(|b| (b / (1024 * 1024)) as u32)
                .unwrap_or(0)
        };
        assert!(
            mib >= MIN_VAR_CACHE_MIB,
            "{script} sizes /var/cache tmpfs to `{size_tok}` (= {mib} MiB) — it must be \
             ≥{MIN_VAR_CACHE_MIB}M so apt can never ENOSPC and leave a kernel without an \
             initrd (#369/#295). Line: {line}"
        );
    }
}

/// 16. The fleet disk-space guard's pure threshold function must be behaviourally correct.
///     cam_disk_alert_needed(used_pct, threshold): returns 0 (alert) when used_pct ≥ threshold;
///     returns 1 (ok) when used_pct < threshold. This is the ONLY logic that decides "alert or
///     not" — unit-testing it prevents a threshold regression that would silence real alerts
///     (the exact gap that allowed cam4 to reach 92%-full with no notification, #369).
#[test]
fn disk_guard_threshold_decision_correct() {
    let thresholds_sh = manifest_dir().join("scripts/lib/disk-guard-thresholds.sh");
    assert!(
        thresholds_sh.exists(),
        "scripts/lib/disk-guard-thresholds.sh must exist — it holds the pure threshold \
         decision for the fleet disk-space guard (#369)"
    );
    // Helper: run bash and return "YES" or "NO"
    let check = |used_pct: u8, threshold: u8| -> String {
        let cmd = format!(
            ". {path} && cam_disk_alert_needed {used} {thr} && echo YES || echo NO",
            path = thresholds_sh.display(),
            used = used_pct,
            thr = threshold,
        );
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(&cmd)
            .output()
            .expect("bash available");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    // Boundary: exactly at threshold → alert.
    assert_eq!(
        check(80, 80),
        "YES",
        "cam_disk_alert_needed(80, 80) must return 0 (alert) — at 80% exactly the threshold fires"
    );
    // One below threshold → no alert.
    assert_eq!(
        check(79, 80),
        "NO",
        "cam_disk_alert_needed(79, 80) must return 1 (ok) — below threshold, no alert"
    );
    // Well over threshold → alert.
    assert_eq!(check(92, 80), "YES",
        "cam_disk_alert_needed(92, 80) must return 0 (alert) — cam4 was 92%-full and must have alerted");
    // At 0% → no alert.
    assert_eq!(
        check(0, 80),
        "NO",
        "cam_disk_alert_needed(0, 80) must return 1 (ok) — empty disk never alerts"
    );
    // Full disk (100%) → alert.
    assert_eq!(
        check(100, 80),
        "YES",
        "cam_disk_alert_needed(100, 80) must return 0 (alert) — full /var/cache must alert"
    );
}
