//! #1066 D6 -- the named `cam-box` UEFI NVRAM boot entry must land on the TARGET box, not the
//! machine BUILDING the stick, and must be certified post-reboot.
//!
//! `create-usb-linux.sh`'s `create_efi_boot_entry` wrote the entry into `$DEVICE`'s NVRAM on the
//! HOST -- when built on dev1 that pollutes dev1's NVRAM and the cam box gets none, so the box
//! depended 100% on the AMI USB auto-entry (which failed on cam2 after a warm reboot). The fix:
//!   (a) create-usb guards its host-side entry to the builder's own boot disk, else a loud WARN;
//!   (b) a new on-box `setup-device.sh` STEP 17d creates the named entry in the box's OWN NVRAM
//!       and makes it lead BootOrder (idempotent);
//!   (c) a `verify-device.sh` check `(al)` certifies the entry exists AND leads BootOrder (FAIL on
//!       any drift / unreadable -- test-strictness).
//! All parsing/decision logic is pure functions in `scripts/lib/efi-boot-entry.sh` (Tier-0 tested
//! by sourcing, the `scripts/lib/ndi-provision.sh` convention).

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn setup() -> String {
    read("scripts/setup-device.sh")
}
fn verify() -> String {
    read("scripts/verify-device.sh")
}
fn create_usb() -> String {
    read("scripts/create-usb-linux.sh")
}

/// True if `needle` appears on a line that is NOT a `#` comment.
fn on_noncomment_line(body: &str, needle: &str) -> bool {
    body.lines()
        .any(|l| l.contains(needle) && !l.trim_start().starts_with('#'))
}

/// Source `scripts/lib/efi-boot-entry.sh` and run `body` against the pure functions.
/// Returns (exit_code, stdout, stderr). Sources under `set -uo pipefail` (no `-e`) so a test can
/// inspect a non-zero return from a predicate function.
fn run_efi(body: &str) -> (i32, String, String) {
    let dir = manifest_dir();
    let harness = format!(
        "set -uo pipefail\n. \"{lib}\"\nset +e\n{body}",
        lib = dir.join("scripts/lib/efi-boot-entry.sh").display(),
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

// A representative `efibootmgr` dump: cam-box is Boot0003 and LEADS BootOrder.
const EFI_LEADS: &str = "BootCurrent: 0003
Timeout: 1 seconds
BootOrder: 0003,0001,0000,0002
Boot0000* Windows Boot Manager   HD(1,GPT,abcd,0x800,0x100000)
Boot0001* UEFI: USB, Partition 1   PciRoot(0x0)/Pci(0x14,0x0)/USB(1,0)
Boot0002* ubuntu   HD(1,GPT,efgh,0x800,0x100000)
Boot0003* cam-box   HD(1,GPT,ijkl,0x800,0x100000)";

// cam-box PRESENT (Boot0003) but does NOT lead (0001 is first).
const EFI_NOT_LEADING: &str = "BootCurrent: 0001
BootOrder: 0001,0003,0000
Boot0000* ubuntu   HD(1,GPT,efgh,0x800,0x100000)
Boot0001* UEFI: USB, Partition 1   PciRoot(0x0)/Pci(0x14,0x0)/USB(1,0)
Boot0003* cam-box   HD(1,GPT,ijkl,0x800,0x100000)";

// No cam-box entry at all (the freshly-flashed box that depends on the AMI auto-entry).
const EFI_ABSENT: &str = "BootCurrent: 0001
BootOrder: 0001,0000
Boot0000* ubuntu   HD(1,GPT,efgh,0x800,0x100000)
Boot0001* UEFI: USB, Partition 1   PciRoot(0x0)/Pci(0x14,0x0)/USB(1,0)";

// ============================================================================
// scripts/lib/efi-boot-entry.sh -- pure functions
// ============================================================================

#[test]
fn efi_whole_disk_of_strips_partition_suffix_1066() {
    let (code, out, err) = run_efi(
        r#"for p in /dev/nvme0n1p2 /dev/mmcblk0p1 /dev/sda2 /dev/sda12 /dev/sda /dev/nvme0n1; do
             printf '%s=%s\n' "$p" "$(efi_whole_disk_of "$p")"
           done"#,
    );
    assert_eq!(code, 0, "pure fns must source + run; stderr: {err}");
    let got: Vec<&str> = out.split_whitespace().collect();
    assert_eq!(
        got,
        vec![
            "/dev/nvme0n1p2=/dev/nvme0n1",
            "/dev/mmcblk0p1=/dev/mmcblk0",
            "/dev/sda2=/dev/sda",
            "/dev/sda12=/dev/sda",
            "/dev/sda=/dev/sda",
            "/dev/nvme0n1=/dev/nvme0n1",
        ],
        "efi_whole_disk_of must map a partition to its whole disk and leave a bare disk unchanged (#1066 D6)"
    );
}

#[test]
fn efi_cam_box_bootnums_finds_the_labelled_entry_1066() {
    let (code, out, err) = run_efi(&format!(
        "efi_cam_box_bootnums {q}{leads}{q}",
        q = "\"",
        leads = EFI_LEADS
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out.trim(),
        "0003",
        "efi_cam_box_bootnums must return the boot number of the `cam-box` entry (#1066 D6)"
    );
    // Absent -> empty.
    let (_c, out2, _e) = run_efi(&format!("efi_cam_box_bootnums \"{}\"", EFI_ABSENT));
    assert!(
        out2.trim().is_empty(),
        "no cam-box entry -> empty bootnums (#1066 D6)"
    );
}

#[test]
fn efi_boot_order_returns_the_csv_1066() {
    let (code, out, err) = run_efi(&format!("efi_boot_order \"{}\"", EFI_LEADS));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out.trim(),
        "0003,0001,0000,0002",
        "efi_boot_order must return the BootOrder CSV (#1066 D6)"
    );
}

#[test]
fn efi_cam_box_leads_is_true_only_when_first_1066() {
    let (c1, _o, _e) = run_efi(&format!("efi_cam_box_leads \"{}\"", EFI_LEADS));
    assert_eq!(
        c1, 0,
        "cam-box first in BootOrder -> leads (exit 0) (#1066 D6)"
    );
    let (c2, _o, _e) = run_efi(&format!("efi_cam_box_leads \"{}\"", EFI_NOT_LEADING));
    assert_ne!(
        c2, 0,
        "cam-box present but not first -> does NOT lead (#1066 D6)"
    );
    let (c3, _o, _e) = run_efi(&format!("efi_cam_box_leads \"{}\"", EFI_ABSENT));
    assert_ne!(c3, 0, "no cam-box entry -> does NOT lead (#1066 D6)");
}

#[test]
fn efi_boot_order_lead_moves_num_to_front_dedup_1066() {
    let (code, out, err) = run_efi("efi_boot_order_lead 0003 0001,0003,0000");
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out.trim(),
        "0003,0001,0000",
        "efi_boot_order_lead must move the num to the front and drop its old position (#1066 D6)"
    );
    // Num not yet in the order -> prepended.
    let (_c, out2, _e) = run_efi("efi_boot_order_lead 0003 0001,0000");
    assert_eq!(
        out2.trim(),
        "0003,0001,0000",
        "a num absent from BootOrder is prepended (#1066 D6)"
    );
}

#[test]
fn efi_entry_verdict_ok_only_when_present_and_leading_1066() {
    let (_c, out, err) = run_efi(&format!("efi_entry_verdict \"{}\"", EFI_LEADS));
    assert_eq!(out.trim(), "ok", "present + leading -> ok; stderr: {err}");

    let (_c, out2, _e) = run_efi(&format!("efi_entry_verdict \"{}\"", EFI_NOT_LEADING));
    assert!(
        out2.starts_with("FAIL:") && out2.contains("lead"),
        "present but not leading -> a FAIL naming the ordering problem (#1066 D6): {out2}"
    );

    let (_c, out3, _e) = run_efi(&format!("efi_entry_verdict \"{}\"", EFI_ABSENT));
    assert!(
        out3.starts_with("FAIL:") && out3.contains("cam-box"),
        "no cam-box entry -> a FAIL naming the missing entry (#1066 D6): {out3}"
    );
}

// ============================================================================
// create-usb-linux.sh -- (a) guard the host-side entry to the builder's own boot disk
// ============================================================================

#[test]
fn create_usb_sources_the_efi_lib_and_guards_on_builder_boot_disk_1066() {
    let body = create_usb();
    assert!(
        on_noncomment_line(&body, ". \"$SCRIPT_DIR/lib/efi-boot-entry.sh\""),
        "create-usb-linux.sh must source scripts/lib/efi-boot-entry.sh (#1066 D6)"
    );
    // The create_efi_boot_entry body must now compare the target to the builder's OWN boot disk
    // (findmnt / efi_whole_disk_of) and skip+WARN when they differ -- never pollute the host NVRAM.
    let start = body
        .find("create_efi_boot_entry()")
        .expect("create_efi_boot_entry must still exist");
    let end = body[start..]
        .find("\n# Cleanup")
        .map(|o| start + o)
        .unwrap_or(body.len());
    let func = &body[start..end];
    assert!(
        func.contains("efi_whole_disk_of") && func.contains("findmnt"),
        "create_efi_boot_entry must derive the builder's own boot disk (findmnt + efi_whole_disk_of) \
         to guard against writing a foreign target's entry into the host NVRAM (#1066 D6)"
    );
    // A WARN path for the "building a stick for another box" case must exist.
    assert!(
        func.to_lowercase().contains("target")
            && func.lines().any(|l| l.contains("warn") && !l.trim_start().starts_with('#')),
        "create_efi_boot_entry must WARN (not write) when the target is not the builder's own boot \
         disk -- the entry must be created ON the target (#1066 D6)"
    );
}

#[test]
fn create_usb_base_image_bakes_efibootmgr_1066() {
    let body = create_usb();
    // Must be in an apt-get install PACKAGE list (the base image), not merely mentioned as the
    // create_efi_boot_entry command -- so a fresh box has efibootmgr for the on-box STEP 17d.
    let ess = body
        .find("# Install essential packages")
        .expect("create-usb must have the essential-packages install block");
    let ess_end = body[ess..]
        .find("# #362:")
        .map(|o| ess + o)
        .unwrap_or(body.len());
    let block = &body[ess..ess_end];
    assert!(
        block.contains("efibootmgr"),
        "create-usb-linux.sh must install efibootmgr in the base-image package list so a fresh box \
         has it for the on-box STEP 17d named-entry step (#1066 D6)"
    );
}

// ============================================================================
// setup-device.sh -- (b) on-box STEP 17d creates the named entry + leads BootOrder
// ============================================================================

#[test]
fn setup_device_step17d_creates_named_entry_on_box_1066() {
    let body = setup();
    assert!(
        on_noncomment_line(&body, "[17d]"),
        "setup-device.sh must have a STEP 17d banner that ensures the named UEFI entry (#1066 D6)"
    );
    assert!(
        on_noncomment_line(&body, ". \"$HERE/lib/efi-boot-entry.sh\""),
        "setup-device.sh must source scripts/lib/efi-boot-entry.sh (#1066 D6)"
    );
    // Scope the assertions to the STEP 17d block (from its banner to STEP 18).
    let start = body
        .find("STEP 17d")
        .expect("STEP 17d block must exist (#1066 D6)");
    let end = body[start..]
        .find("STEP 18:")
        .map(|o| start + o)
        .unwrap_or(body.len());
    let block = &body[start..end];
    // Issue 1311 moved the create + lead-BootOrder logic into the ONE shared
    // `efi_cam_box_ensure` (scripts/lib/efi-boot-entry.sh), which create-usb-linux.sh also calls
    // -- so STEP 17d must call it, and the lib function must carry the create + reorder.
    assert!(
        block.contains("efi_cam_box_ensure") && block.contains("$EFI_CAM_BOX_LABEL"),
        "STEP 17d must create/repair the named entry via the shared efi_cam_box_ensure (#1066 D6, issue 1311)"
    );
    let lib = read("scripts/lib/efi-boot-entry.sh");
    let ensure_start = lib
        .find("efi_cam_box_ensure()")
        .expect("scripts/lib/efi-boot-entry.sh must define efi_cam_box_ensure (issue 1311)");
    let ensure = &lib[ensure_start..];
    assert!(
        ensure.contains("-c -d") && ensure.contains("\"$EFI_CAM_BOX_LABEL\""),
        "efi_cam_box_ensure must create the named entry via efibootmgr -c -d ... -L $EFI_CAM_BOX_LABEL (#1066 D6)"
    );
    assert!(
        ensure.contains("efi_cam_box_repair_plan") && ensure.contains("efi_boot_order_lead"),
        "efi_cam_box_ensure must ensure the entry LEADS BootOrder (repair plan + efi_boot_order_lead) (#1066 D6)"
    );
    // Enable-only convention: no live start/restart of anything here (it just writes NVRAM).
    assert!(
        block.contains("efivars"),
        "STEP 17d must guard on /sys/firmware/efi/efivars (skip on a non-EFI boot) (#1066 D6)"
    );
}

#[test]
fn setup_device_step16_installs_efibootmgr_1066() {
    let body = setup();
    // STEP 16's apt-get package line must include efibootmgr (base image + STEP 16, per D6).
    let step16 = body.find("[16/").expect("STEP 16 banner must exist");
    let end = body[step16..]
        .find("[17/")
        .map(|o| step16 + o)
        .unwrap_or(body.len());
    let block = &body[step16..end];
    assert!(
        block.contains("efibootmgr"),
        "STEP 16 must install efibootmgr so the on-box STEP 17d can create the named entry (#1066 D6)"
    );
}

// ============================================================================
// verify-device.sh -- (c) check (al) certifies the named entry leads BootOrder
// ============================================================================

#[test]
fn verify_device_check_al_certifies_named_entry_before_q_1066() {
    let body = verify();
    let al = body
        .find("# (al)")
        .expect("verify-device.sh must have a check (al) for the named UEFI entry (#1066 D6)");
    let q = body
        .find("# (q) .bak cruft drift")
        .expect("check (q) must still exist");
    assert!(
        al < q,
        "check (al) must be inserted BEFORE (q) -- the (q)-last invariant (#1066 D6)"
    );
    assert!(
        on_noncomment_line(&body, ". \"$HERE/lib/efi-boot-entry.sh\""),
        "verify-device.sh must source scripts/lib/efi-boot-entry.sh for the pure verdict (#1066 D6)"
    );
    // The (al) block (from its marker to (q)) must grade via efi_entry_verdict and FAIL on unreadable.
    let block = &body[al..q];
    assert!(
        block.contains("efi_entry_verdict"),
        "check (al) must grade with the pure efi_entry_verdict (#1066 D6)"
    );
    assert!(
        block.lines().any(|l| l.contains("fail ") && !l.trim_start().starts_with('#')),
        "check (al) must FAIL (never merely warn) on drift / unreadable (test-strictness) (#1066 D6)"
    );
}
