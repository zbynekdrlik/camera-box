//! Issue 1311 -- the five provisioning defects the cam1-cam4 M.2 migration (24.9.2026) hit, each
//! pinned here RED -> GREEN (std-only harness: plain `bash` + the real scripts, no crate code).
//!
//!   1. `setup-device.sh` STEP 18's UNQUOTED `FSTABEOF` heredoc ran the backticked comment word
//!      `nofail` as a command (`nofail: command not found`) and dropped it from the written fstab.
//!      The same class sat in `verify-device.sh`'s `usage()` heredoc (it EXECUTED `v4l2-ctl`).
//!   2. The named `cam-box` UEFI entry was never read back: cam2's was stored with a
//!      firmware-mangled `VenHw(...)` device path and not first in BootOrder. The shared
//!      `efi_cam_box_ensure` (scripts/lib/efi-boot-entry.sh) now verifies an `HD(...)` path that
//!      leads BootOrder, deletes+recreates a mangled/stale entry, and fails loud when it can't.
//!   3. `setup-device.sh` needed an interactive `y`; `--yes|-y` is now the create-usb contract.
//!   4. `verify-device.sh`'s ssh kept dev1's known_hosts, so a reflashed box's NEW host key turned
//!      every check into `ssh rc=255`; every ssh now passes `-o UserKnownHostsFile=/dev/null`.
//!   5. A fresh image booted after a CMOS reset ran apt with its clock two months behind;
//!      `setup-device.sh` now sets the clock FORWARD from the Ubuntu archive's HTTP `Date` header
//!      before the first apt/curl when it is more than one day behind (never backward).

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// True if `needle` appears on a line that is NOT a `#` comment.
fn on_noncomment_line(body: &str, needle: &str) -> bool {
    body.lines()
        .any(|l| l.contains(needle) && !l.trim_start().starts_with('#'))
}

/// Run `script` under `bash -c` with `envs`; returns (exit_code, stdout, stderr).
fn run_bash(script: &str, envs: &[(&str, String)]) -> (i32, String, String) {
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(script);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Source the real `setup-device.sh` (its `BASH_SOURCE != $0` guard skips the provisioning flow).
fn run_setup_sourced(body: &str) -> (i32, String, String) {
    run_setup_sourced_env(body, &[])
}

fn run_setup_sourced_env(body: &str, envs: &[(&str, String)]) -> (i32, String, String) {
    let script = manifest_dir().join("scripts/setup-device.sh");
    let mut all: Vec<(&str, String)> = vec![("SCRIPT", script.display().to_string())];
    all.extend(envs.iter().cloned());
    run_bash(
        &format!("set -uo pipefail\n. \"$SCRIPT\"\nset +e\n{body}"),
        &all,
    )
}

/// Source `scripts/lib/efi-boot-entry.sh` and run `body`.
fn run_efi(body: &str, envs: &[(&str, String)]) -> (i32, String, String) {
    let lib = manifest_dir().join("scripts/lib/efi-boot-entry.sh");
    run_bash(
        &format!("set -uo pipefail\n. \"{}\"\nset +e\n{body}", lib.display()),
        envs,
    )
}

/// A fresh, unique scratch directory under the OS temp dir.
fn scratch_dir(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let d = std::env::temp_dir().join(format!(
        "cb-1311-{tag}-{}-{}-{nanos}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&d).expect("create scratch dir");
    d
}

fn write_exec(path: &std::path::Path, content: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, content).expect("write stub");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod stub");
}

// ================================================================================================
// Fix 1 -- no unquoted heredoc may carry an unescaped backtick (bash command-substitutes it)
// ================================================================================================

/// Position just after a heredoc operator (`<<` / `<<-`, never the `<<<` here-string) on `line`.
fn heredoc_op_end(line: &str) -> Option<usize> {
    let b = line.as_bytes();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'<' && b[i + 1] == b'<' {
            let prev_lt = i > 0 && b[i - 1] == b'<';
            let next_lt = i + 2 < b.len() && b[i + 2] == b'<';
            if !prev_lt && !next_lt {
                let mut j = i + 2;
                if j < b.len() && b[j] == b'-' {
                    j += 1;
                }
                return Some(j);
            }
            i += 3;
            continue;
        }
        i += 1;
    }
    None
}

fn has_unescaped_backtick(line: &str) -> bool {
    let mut prev_backslash = false;
    for c in line.chars() {
        if c == '`' && !prev_backslash {
            return true;
        }
        prev_backslash = c == '\\' && !prev_backslash;
    }
    false
}

/// (line number, text) of every body line of an UNQUOTED heredoc that carries an unescaped
/// backtick -- bash runs it as a command substitution.
fn backticks_in_unquoted_heredocs(body: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = body.lines().collect();
    let mut hits = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        if !l.trim_start().starts_with('#') {
            if let Some(pos) = heredoc_op_end(l) {
                let rest = l[pos..].trim_start();
                let quoted =
                    rest.starts_with('\'') || rest.starts_with('"') || rest.starts_with('\\');
                let tag: String = rest
                    .trim_start_matches(['\'', '"', '\\'])
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if !tag.is_empty() {
                    if let Some(end) = (i + 1..lines.len()).find(|&j| lines[j].trim() == tag) {
                        if !quoted {
                            for (k, line) in lines.iter().enumerate().take(end).skip(i + 1) {
                                if has_unescaped_backtick(line) {
                                    hits.push((k + 1, line.to_string()));
                                }
                            }
                        }
                        i = end + 1;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    hits
}

#[test]
fn no_unquoted_heredoc_carries_an_unescaped_backtick_1311() {
    for script in [
        "scripts/setup-device.sh",
        "scripts/verify-device.sh",
        "scripts/create-usb-linux.sh",
    ] {
        let hits = backticks_in_unquoted_heredocs(&read(script));
        assert!(
            hits.is_empty(),
            "{script}: an UNQUOTED heredoc body carries an unescaped backtick -- bash runs it as a \
             command (the `nofail: command not found` class, issue 1311). Escape it (\\`) or quote \
             the delimiter. Offending lines: {hits:?}"
        );
    }
}

#[test]
fn heredoc_scanner_catches_the_original_fstab_shape_1311() {
    // Self-test of the scanner, so a green result above can never mean "the scanner saw nothing".
    let bad = "cat > /etc/fstab << FSTABEOF\n# a `nofail` word\nFSTABEOF\n";
    assert_eq!(backticks_in_unquoted_heredocs(bad).len(), 1);
    let escaped = "cat > /etc/fstab << FSTABEOF\n# a \\`nofail\\` word\nFSTABEOF\n";
    assert!(backticks_in_unquoted_heredocs(escaped).is_empty());
    let quoted = "cat > /x << 'EOF'\n# a `word`\nEOF\n";
    assert!(backticks_in_unquoted_heredocs(quoted).is_empty());
}

/// The STEP 18 fstab heredoc, lifted verbatim and run with stubs: it must run NO command from
/// its comments and write the `nofail` word back, with the real mount lines unchanged.
#[test]
fn setup_device_fstab_heredoc_runs_no_comment_command_and_keeps_nofail_1311() {
    let body = read("scripts/setup-device.sh");
    let lines: Vec<&str> = body.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.starts_with("cat > /etc/fstab << FSTABEOF"))
        .expect("STEP 18 fstab heredoc must exist");
    let end = (start + 1..lines.len())
        .find(|&j| lines[j] == "FSTABEOF")
        .expect("FSTABEOF terminator must exist");
    let mut block = lines[start..=end].join("\n");
    block = block.replacen("cat > /etc/fstab", "cat", 1);
    let harness = format!(
        "set -uo pipefail\nROOT_UUID=TESTUUID\nLOG_DIET_JOURNAL_PART_LABEL=cambox-journal\n\
         log_diet_journal_fstab_line() {{ echo JOURNALLINE; }}\nblkid() {{ return 1; }}\n\
         grep() {{ return 1; }}\n{block}\n"
    );
    let (code, out, err) = run_bash(&harness, &[]);
    assert_eq!(code, 0, "the fstab heredoc must run clean; stderr: {err}");
    assert!(
        !err.contains("command not found"),
        "the fstab heredoc must not execute any word of its comments (issue 1311): {err}"
    );
    assert!(
        out.contains(
            "# orders /var/log first by path prefix). `nofail` -> a box WITHOUT the partition"
        ),
        "the written fstab must keep the `nofail` comment word (issue 1311): {out}"
    );
    for want in [
        "UUID=TESTUUID / ext4 ro 0 1",
        "# No EFI partition",
        "tmpfs /tmp tmpfs defaults,noatime,nosuid,nodev,mode=1777,size=100M 0 0",
        "tmpfs /var/log tmpfs defaults,noatime,nosuid,nodev,mode=0755,size=50M 0 0",
        "tmpfs /var/cache tmpfs defaults,noatime,nosuid,nodev,mode=0755,size=512M 0 0",
        "tmpfs /var/spool tmpfs defaults,noatime,nosuid,nodev,mode=0755,size=10M 0 0",
    ] {
        assert!(
            out.lines().any(|l| l == want),
            "fstab mount line must be byte-identical: missing `{want}` in:\n{out}"
        );
    }
}

/// `verify-device.sh --help` must print the usage without executing anything from it.
#[test]
fn verify_device_usage_runs_no_command_1311() {
    let script = manifest_dir().join("scripts/verify-device.sh");
    let (code, out, err) = run_bash(
        "bash \"$SCRIPT\" --help",
        &[("SCRIPT", script.display().to_string())],
    );
    assert_eq!(code, 0, "--help must exit 0; stderr: {err}");
    assert!(
        !err.contains("command not found"),
        "verify-device.sh usage() must not run its backticked words as commands: {err}"
    );
    assert!(
        out.contains("efibootmgr reports a `cam-box` entry"),
        "usage() must print the (al) line with its `cam-box` word intact: {out}"
    );
    assert!(
        out.contains("(`v4l2-ctl --version`)"),
        "usage() must print `v4l2-ctl --version` literally, never run it: {out}"
    );
}

// ================================================================================================
// Fix 2 -- the named `cam-box` UEFI entry is read back: HD() path, leads BootOrder, else repaired
// ================================================================================================

// `efibootmgr -v` shapes (efibootmgr separates the label from the device path with a TAB).
const V_HEALTHY_FIRST: &str = "BootCurrent: 0003
Timeout: 1 seconds
BootOrder: 0003,0001,0000
Boot0000* UEFI OS\tHD(1,GPT,11111111-aaaa-bbbb-cccc-000000000000,0x800,0x100000)/File(\\EFI\\BOOT\\BOOTX64.EFI)..BO
Boot0001* UEFI: USB, Partition 1\tPciRoot(0x0)/Pci(0x14,0x0)/USB(2,0)/HD(1,MBR,0x0,0x800,0x100000)..BO
Boot0003* cam-box\tHD(1,GPT,5bd6d8d8-1c2e-4a55-9f1e-0123456789ab,0x800,0x100000)/File(\\EFI\\BOOT\\BOOTX64.EFI)";

// cam2, 24.9.2026: the stored entry carries a firmware-mangled VenHw() path and is not first.
const V_MANGLED: &str = "BootCurrent: 0001
BootOrder: 0001,0000
Boot0000* cam-box\tVenHw(99e275e7-75a0-4b37-a2e6-c5385e6c00cb,00000000)/File(\\EFI\\BOOT\\BOOTX64.EFI)
Boot0001* UEFI OS\tHD(1,GPT,11111111-aaaa-bbbb-cccc-000000000000,0x800,0x100000)/File(\\EFI\\BOOT\\BOOTX64.EFI)..BO";

// A healthy HD() entry that is NOT first.
const V_HEALTHY_NOT_FIRST: &str = "BootCurrent: 0001
BootOrder: 0001,0003
Boot0001* UEFI OS\tHD(1,GPT,11111111-aaaa-bbbb-cccc-000000000000,0x800,0x100000)/File(\\EFI\\BOOT\\BOOTX64.EFI)..BO
Boot0003* cam-box\tHD(1,GPT,5bd6d8d8-1c2e-4a55-9f1e-0123456789ab,0x800,0x100000)/File(\\EFI\\BOOT\\BOOTX64.EFI)";

const V_ABSENT: &str = "BootCurrent: 0001
BootOrder: 0001
Boot0001* UEFI OS\tHD(1,GPT,11111111-aaaa-bbbb-cccc-000000000000,0x800,0x100000)/File(\\EFI\\BOOT\\BOOTX64.EFI)..BO";

const ESP_UUID: &str = "5bd6d8d8-1c2e-4a55-9f1e-0123456789ab";

fn efi_call(func: &str, dump: &str, extra: &str) -> (i32, String, String) {
    run_efi(
        &format!("{func} \"$DUMP\" {extra}"),
        &[("DUMP", dump.to_string())],
    )
}

#[test]
fn efi_cam_box_entry_path_reads_the_stored_device_path_1311() {
    let (code, out, err) = efi_call("efi_cam_box_entry_path", V_HEALTHY_FIRST, "0003");
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out.trim(),
        "HD(1,GPT,5bd6d8d8-1c2e-4a55-9f1e-0123456789ab,0x800,0x100000)/File(\\EFI\\BOOT\\BOOTX64.EFI)"
    );
    let (_c, out2, _e) = efi_call("efi_cam_box_entry_path", V_MANGLED, "0000");
    assert!(
        out2.trim().starts_with("VenHw("),
        "mangled path read back verbatim: {out2}"
    );
    // A number that is not a cam-box entry reads nothing (0001 is `UEFI OS`).
    let (_c, out3, _e) = efi_call("efi_cam_box_entry_path", V_MANGLED, "0001");
    assert!(out3.trim().is_empty(), "non-cam-box entry -> empty: {out3}");
}

#[test]
fn efi_device_path_healthy_requires_hd_never_venhw_1311() {
    let cases: &[(&str, &str, bool)] = &[
        ("HD(1,GPT,5bd6d8d8-1c2e-4a55-9f1e-0123456789ab,0x800,0x100000)/File(\\EFI\\BOOT\\BOOTX64.EFI)", "", true),
        ("HD(1,GPT,5bd6d8d8-1c2e-4a55-9f1e-0123456789ab,0x800,0x100000)/File(\\EFI\\BOOT\\BOOTX64.EFI)", ESP_UUID, true),
        // Case-insensitive PARTUUID compare.
        ("HD(1,GPT,5BD6D8D8-1C2E-4A55-9F1E-0123456789AB,0x800,0x100000)/\\EFI\\BOOT\\BOOTX64.EFI", ESP_UUID, true),
        // A full firmware-expanded path that still ends in the right HD() node is fine.
        ("PciRoot(0x0)/Pci(0x17,0x0)/Sata(0,65535,0)/HD(1,GPT,5bd6d8d8-1c2e-4a55-9f1e-0123456789ab,0x800,0x100000)/File(\\EFI\\BOOT\\BOOTX64.EFI)", ESP_UUID, true),
        // Stale entry from a previous install: HD() but a different partition GUID.
        ("HD(1,GPT,deadbeef-0000-0000-0000-000000000000,0x800,0x100000)/File(\\EFI\\BOOT\\BOOTX64.EFI)", ESP_UUID, false),
        ("VenHw(99e275e7-75a0-4b37-a2e6-c5385e6c00cb,00000000)/File(\\EFI\\BOOT\\BOOTX64.EFI)", "", false),
        ("VenHw(99e275e7-75a0-4b37-a2e6-c5385e6c00cb)/HD(1,GPT,5bd6d8d8-1c2e-4a55-9f1e-0123456789ab,0x800,0x100000)", ESP_UUID, false),
        ("", "", false),
    ];
    for (path, want, healthy) in cases {
        let (code, _o, err) = run_efi(
            "efi_device_path_healthy \"$P\" \"$W\"",
            &[("P", path.to_string()), ("W", want.to_string())],
        );
        assert_eq!(
            code == 0,
            *healthy,
            "efi_device_path_healthy({path:?}, want={want:?}) should be {healthy}; stderr: {err}"
        );
    }
}

#[test]
fn efi_cam_box_repair_plan_decides_create_recreate_reorder_ok_1311() {
    let cases: &[(&str, &str, &str)] = &[
        (V_HEALTHY_FIRST, ESP_UUID, "ok"),
        (V_HEALTHY_FIRST, "", "ok"),
        (V_ABSENT, ESP_UUID, "create"),
        (V_MANGLED, ESP_UUID, "recreate 0000"),
        (V_MANGLED, "", "recreate 0000"),
        (V_HEALTHY_NOT_FIRST, ESP_UUID, "reorder 0003"),
        // Healthy + first, but it points at ANOTHER partition than this disk's ESP -> stale.
        (
            V_HEALTHY_FIRST,
            "deadbeef-0000-0000-0000-000000000000",
            "recreate 0003",
        ),
    ];
    for (dump, want, plan) in cases {
        let (code, out, err) = efi_call("efi_cam_box_repair_plan", dump, &format!("\"{want}\""));
        assert_eq!(code, 0, "stderr: {err}");
        assert_eq!(out.trim(), *plan, "plan for want={want:?} dump:\n{dump}");
    }
}

#[test]
fn efi_esp_partition_of_disk_names_partition_one_1311() {
    let (code, out, err) = run_efi(
        r#"for d in /dev/sda /dev/nvme0n1 /dev/mmcblk0; do printf '%s ' "$(efi_esp_partition_of_disk "$d")"; done"#,
        &[],
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "/dev/sda1 /dev/nvme0n1p1 /dev/mmcblk0p1");
}

/// A fake `efibootmgr` over a small NVRAM state dir: `-v`, `-c -d D -p P -L L -l F`, `-b N -B`,
/// `-o CSV`. `mangle` = how many upcoming creates the "firmware" stores as VenHw(); `noprepend`
/// = the firmware APPENDS a new entry to BootOrder instead of prepending it.
const FAKE_EFIBOOTMGR: &str = r##"#!/usr/bin/env bash
set -euo pipefail
S="$FAKE_EFI_STATE"
touch "$S/entries" "$S/order"
dump() {
    printf 'BootCurrent: 0001\n'
    printf 'BootOrder: %s\n' "$(cat "$S/order")"
    while IFS='|' read -r n l p; do
        [ -n "$n" ] || continue
        printf 'Boot%s* %s\t%s\n' "$n" "$l" "$p"
    done < "$S/entries"
}
mode=""; disk=""; part=""; label=""; loader=""; num=""; neworder=""
while [ $# -gt 0 ]; do
    case "$1" in
        -v) shift ;;
        -c) mode=create; shift ;;
        -d) disk="$2"; shift 2 ;;
        -p) part="$2"; shift 2 ;;
        -L) label="$2"; shift 2 ;;
        -l) loader="$2"; shift 2 ;;
        -b) num="$2"; shift 2 ;;
        -B) mode=delete; shift ;;
        -o) mode=order; neworder="$2"; shift 2 ;;
        *) echo "fake efibootmgr: unexpected arg $1" >&2; exit 2 ;;
    esac
done
case "$mode" in
    create)
        echo "create $disk $part $label" >> "$S/calls"
        max=$(awk -F'|' '{print $1}' "$S/entries" | sort | tail -1)
        new=$(printf '%04X' $(( 16#${max:-0} + 1 )))
        m=$(cat "$S/mangle" 2>/dev/null || echo 0)
        if [ "$m" -gt 0 ]; then
            echo $((m - 1)) > "$S/mangle"
            path="VenHw(99e275e7-75a0-4b37-a2e6-c5385e6c00cb,00000000)/File($loader)"
        else
            path="HD($part,GPT,$(cat "$S/partuuid"),0x800,0x100000)/File($loader)"
        fi
        echo "$new|$label|$path" >> "$S/entries"
        cur=$(cat "$S/order")
        if [ -f "$S/noprepend" ]; then
            echo "${cur:+$cur,}$new" > "$S/order"
        else
            echo "$new${cur:+,$cur}" > "$S/order"
        fi
        dump ;;
    delete)
        echo "delete $num" >> "$S/calls"
        grep -v "^$num|" "$S/entries" > "$S/entries.tmp" || true
        mv "$S/entries.tmp" "$S/entries"
        tr ',' '\n' < "$S/order" | grep -vx "$num" | paste -sd, - > "$S/order.tmp" || true
        mv "$S/order.tmp" "$S/order"
        dump ;;
    order)
        echo "order $neworder" >> "$S/calls"
        echo "$neworder" > "$S/order" ;;
    *) dump ;;
esac
"##;

struct FakeNvram {
    dir: PathBuf,
}

impl FakeNvram {
    fn new(entries: &str, order: &str, mangle: u32, noprepend: bool) -> Self {
        let dir = scratch_dir("nvram");
        write_exec(&dir.join("efibootmgr"), FAKE_EFIBOOTMGR);
        let state = dir.join("state");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("entries"), entries).unwrap();
        std::fs::write(state.join("order"), format!("{order}\n")).unwrap();
        std::fs::write(state.join("mangle"), format!("{mangle}\n")).unwrap();
        std::fs::write(state.join("partuuid"), format!("{ESP_UUID}\n")).unwrap();
        std::fs::write(state.join("calls"), "").unwrap();
        if noprepend {
            std::fs::write(state.join("noprepend"), "").unwrap();
        }
        FakeNvram { dir }
    }

    /// Run `efi_cam_box_ensure /dev/sdz <want> [mode]`; returns (rc, stdout, final `-v` dump, calls).
    fn ensure(&self, want: &str, mode: &str) -> (i32, String, String, String) {
        let bin = self.dir.join("efibootmgr").display().to_string();
        let state = self.dir.join("state").display().to_string();
        let (code, out, err) = run_efi(
            &format!("efi_cam_box_ensure /dev/sdz \"{want}\" {mode}\necho \"rc=$?\"\necho ===\n\"$EFI_BOOTMGR_BIN\" -v"),
            &[("EFI_BOOTMGR_BIN", bin), ("FAKE_EFI_STATE", state.clone())],
        );
        assert_eq!(code, 0, "harness must run; stderr: {err}");
        let (head, dump) = out.split_once("===\n").expect("harness separator");
        let rc = head
            .lines()
            .filter_map(|l| l.strip_prefix("rc="))
            .next_back()
            .and_then(|v| v.parse::<i32>().ok())
            .expect("rc line");
        let calls =
            std::fs::read_to_string(PathBuf::from(&state).join("calls")).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&self.dir);
        (rc, head.to_string(), dump.to_string(), calls)
    }
}

fn cam_box_lines(dump: &str) -> Vec<String> {
    dump.lines()
        .filter(|l| l.split_whitespace().nth(1) == Some("cam-box"))
        .map(str::to_string)
        .collect()
}

fn leads(dump: &str, line: &str) -> bool {
    let num = line
        .split_whitespace()
        .next()
        .unwrap()
        .trim_start_matches("Boot")
        .trim_end_matches('*')
        .to_string();
    dump.lines()
        .find_map(|l| l.strip_prefix("BootOrder: "))
        .map(|o| o.split(',').next() == Some(num.as_str()))
        .unwrap_or(false)
}

#[test]
fn efi_ensure_creates_a_missing_entry_that_leads_1311() {
    let nv = FakeNvram::new(
        "0001|UEFI OS|HD(1,GPT,x,0x800,0x100000)\n",
        "0001",
        0,
        false,
    );
    let (rc, out, dump, calls) = nv.ensure(ESP_UUID, "");
    assert_eq!(rc, 0, "ensure must succeed: {out}");
    let cb = cam_box_lines(&dump);
    assert_eq!(cb.len(), 1, "exactly one cam-box entry: {dump}");
    assert!(
        cb[0].contains("HD(1,GPT,") && leads(&dump, &cb[0]),
        "HD() + leads: {dump}"
    );
    assert_eq!(calls.matches("create ").count(), 1, "one create: {calls}");
}

/// cam2, 24.9.2026: the firmware stored the first create as VenHw(); ensure must detect it,
/// delete it and recreate -- ending with ONE HD() entry that leads, no VenHw() left behind.
#[test]
fn efi_ensure_repairs_a_firmware_mangled_entry_1311() {
    let nv = FakeNvram::new(
        "0001|UEFI OS|HD(1,GPT,x,0x800,0x100000)\n",
        "0001",
        1,
        false,
    );
    let (rc, out, dump, calls) = nv.ensure(ESP_UUID, "fresh");
    assert_eq!(
        rc, 0,
        "ensure must repair the mangled entry: {out}\n{calls}"
    );
    let cb = cam_box_lines(&dump);
    assert_eq!(
        cb.len(),
        1,
        "exactly one cam-box entry after repair: {dump}"
    );
    assert!(
        !dump.contains("VenHw("),
        "no mangled entry may remain: {dump}"
    );
    assert!(
        cb[0].contains("HD(1,GPT,") && leads(&dump, &cb[0]),
        "HD() + leads: {dump}"
    );
    assert!(
        calls.contains("delete "),
        "the mangled entry must be deleted: {calls}"
    );
    assert_eq!(
        calls.matches("create ").count(),
        2,
        "created, then recreated: {calls}"
    );
}

/// An existing mangled entry (setup-device STEP 17d on cam2) is repaired, not kept.
#[test]
fn efi_ensure_replaces_an_existing_mangled_entry_1311() {
    let nv = FakeNvram::new(
        "0000|cam-box|VenHw(99e275e7-75a0-4b37-a2e6-c5385e6c00cb,00000000)/File(x)\n0001|UEFI OS|HD(1,GPT,x,0x800,0x100000)\n",
        "0001,0000",
        0,
        false,
    );
    let (rc, out, dump, calls) = nv.ensure(ESP_UUID, "");
    assert_eq!(rc, 0, "ensure must repair: {out}");
    assert!(!dump.contains("VenHw("), "mangled entry removed: {dump}");
    let cb = cam_box_lines(&dump);
    assert_eq!(cb.len(), 1, "{dump}");
    assert!(leads(&dump, &cb[0]), "the new entry leads: {dump}");
    assert!(calls.contains("delete 0000"), "{calls}");
}

/// Firmware that mangles EVERY write: ensure must give up after its bounded retries and fail loud.
#[test]
fn efi_ensure_fails_loud_when_the_firmware_keeps_mangling_1311() {
    let nv = FakeNvram::new(
        "0001|UEFI OS|HD(1,GPT,x,0x800,0x100000)\n",
        "0001",
        99,
        false,
    );
    let (rc, out, _dump, calls) = nv.ensure(ESP_UUID, "fresh");
    assert_ne!(
        rc, 0,
        "ensure must FAIL when it cannot get an HD() entry: {out}"
    );
    assert!(
        out.lines()
            .any(|l| l.starts_with("FAIL:") && l.contains("VenHw(")),
        "the failure must say what was stored: {out}"
    );
    assert!(
        calls.matches("create ").count() <= 3,
        "bounded retries: {calls}"
    );
}

/// A healthy entry that is not first is REORDERED, never recreated.
#[test]
fn efi_ensure_reorders_a_healthy_demoted_entry_without_recreating_1311() {
    let nv = FakeNvram::new(
        &format!("0001|UEFI OS|HD(1,GPT,x,0x800,0x100000)\n0003|cam-box|HD(1,GPT,{ESP_UUID},0x800,0x100000)/File(x)\n"),
        "0001,0003",
        0,
        false,
    );
    let (rc, out, dump, calls) = nv.ensure(ESP_UUID, "");
    assert_eq!(rc, 0, "{out}");
    assert!(
        !calls.contains("create "),
        "no recreate for a healthy entry: {calls}"
    );
    assert!(calls.contains("order 0003,0001"), "{calls}");
    let cb = cam_box_lines(&dump);
    assert!(leads(&dump, &cb[0]), "{dump}");
}

/// A healthy entry that already leads is left alone (idempotent re-run of STEP 17d).
#[test]
fn efi_ensure_is_a_no_op_on_a_correct_entry_1311() {
    let nv = FakeNvram::new(
        &format!("0003|cam-box|HD(1,GPT,{ESP_UUID},0x800,0x100000)/File(x)\n0001|UEFI OS|HD(1,GPT,x,0x800,0x100000)\n"),
        "0003,0001",
        0,
        false,
    );
    let (rc, out, _dump, calls) = nv.ensure(ESP_UUID, "");
    assert_eq!(rc, 0, "{out}");
    assert!(
        calls.trim().is_empty(),
        "a correct entry must not be touched: {calls}"
    );
}

/// A stale entry from a PREVIOUS install (HD() but another partition GUID) is replaced.
#[test]
fn efi_ensure_replaces_a_stale_entry_from_a_previous_install_1311() {
    let nv = FakeNvram::new(
        "0003|cam-box|HD(1,GPT,deadbeef-0000-0000-0000-000000000000,0x800,0x100000)/File(x)\n0001|UEFI OS|HD(1,GPT,x,0x800,0x100000)\n",
        "0003,0001",
        0,
        false,
    );
    let (rc, out, dump, calls) = nv.ensure(ESP_UUID, "");
    assert_eq!(rc, 0, "{out}");
    assert!(calls.contains("delete 0003"), "{calls}");
    assert!(!dump.contains("deadbeef"), "stale entry removed: {dump}");
    let cb = cam_box_lines(&dump);
    assert!(cb[0].contains(ESP_UUID) && leads(&dump, &cb[0]), "{dump}");
}

/// Firmware that APPENDS the new entry to BootOrder: ensure creates it, then reorders it first.
#[test]
fn efi_ensure_reorders_after_a_create_the_firmware_appended_1311() {
    let nv = FakeNvram::new("0001|UEFI OS|HD(1,GPT,x,0x800,0x100000)\n", "0001", 0, true);
    let (rc, out, dump, calls) = nv.ensure(ESP_UUID, "");
    assert_eq!(rc, 0, "{out}\n{calls}");
    let cb = cam_box_lines(&dump);
    assert!(leads(&dump, &cb[0]), "{dump}");
    assert!(calls.contains("order "), "{calls}");
}

/// `fresh` (create-usb, a brand-new install) always replaces a prior entry, even a healthy one.
#[test]
fn efi_ensure_fresh_replaces_even_a_healthy_prior_entry_1311() {
    let nv = FakeNvram::new(
        &format!("0003|cam-box|HD(1,GPT,{ESP_UUID},0x800,0x100000)/File(x)\n"),
        "0003",
        0,
        false,
    );
    let (rc, out, dump, calls) = nv.ensure(ESP_UUID, "fresh");
    assert_eq!(rc, 0, "{out}");
    assert!(
        calls.contains("delete 0003") && calls.contains("create "),
        "{calls}"
    );
    assert_eq!(cam_box_lines(&dump).len(), 1, "{dump}");
}

fn function_body(script: &str, name: &str) -> String {
    let lines: Vec<&str> = script.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.trim_start().starts_with(&format!("{name}()")))
        .unwrap_or_else(|| panic!("{name}() must exist"));
    let mut out = Vec::new();
    for line in &lines[start..] {
        out.push(*line);
        if *line == "}" {
            break;
        }
    }
    out.join("\n")
}

#[test]
fn create_usb_verifies_the_entry_and_fails_loud_1311() {
    let body = read("scripts/create-usb-linux.sh");
    let func = function_body(&body, "create_efi_boot_entry");
    assert!(
        on_noncomment_line(&func, "efi_cam_box_ensure \"$DEVICE\"") && func.contains("fresh"),
        "create_efi_boot_entry must create + read back the entry via the shared \
         efi_cam_box_ensure ... fresh (issue 1311): {func}"
    );
    assert!(
        func.lines()
            .any(|l| !l.trim_start().starts_with('#') && l.trim_start().starts_with("error ")),
        "create_efi_boot_entry must FAIL LOUD (error) when the entry cannot be made correct"
    );
    assert!(
        !on_noncomment_line(&func, "efibootmgr -c -d \"$DEVICE\""),
        "the bare unverified efibootmgr -c must be gone -- the create lives in efi_cam_box_ensure"
    );
    // main() must not claim success unconditionally.
    let main = function_body(&body, "main");
    assert!(
        !main.contains("named 'cam-box' UEFI entry created"),
        "main() must report the VERIFIED outcome, never an unconditional 'created' (issue 1311)"
    );
}

#[test]
fn setup_device_step17d_repairs_via_the_shared_ensure_and_fails_loud_1311() {
    let body = read("scripts/setup-device.sh");
    let start = body.find("STEP 17d").expect("STEP 17d block");
    let end = body[start..].find("STEP 18:").map(|o| start + o).unwrap();
    let block = &body[start..end];
    assert!(
        on_noncomment_line(block, "efi_cam_box_ensure \"$EFI_ROOT_DISK\""),
        "STEP 17d must use the shared efi_cam_box_ensure (issue 1311): {block}"
    );
    assert!(
        block
            .lines()
            .any(|l| !l.trim_start().starts_with('#') && l.trim_start().starts_with("fail ")),
        "STEP 17d must fail loud when the entry cannot be made correct (issue 1311)"
    );
    assert!(
        !block.contains("not recreating"),
        "STEP 17d must no longer keep ANY existing cam-box entry blind (issue 1311)"
    );
}

// ================================================================================================
// Fix 3 -- setup-device.sh --yes|-y
// ================================================================================================

#[test]
fn setup_device_confirm_setup_honours_yes_and_never_hangs_1311() {
    let (code, out, err) = run_setup_sourced(
        r#"confirm_setup 1 </dev/null; echo "yes=$?"
           printf 'y' | confirm_setup 0; echo "y=$?"
           printf 'n' | confirm_setup 0; echo "n=$?"
           confirm_setup 0 </dev/null; echo "eof=$?""#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    for want in ["yes=0", "y=0", "n=1", "eof=1"] {
        assert!(
            out.lines().any(|l| l.trim() == want),
            "missing {want} in:\n{out}\n{err}"
        );
    }
}

#[test]
fn setup_device_parses_yes_and_uses_confirm_setup_1311() {
    let body = read("scripts/setup-device.sh");
    assert!(
        on_noncomment_line(&body, "--yes|-y)") && on_noncomment_line(&body, "ASSUME_YES=1"),
        "setup-device.sh must accept --yes|-y (the create-usb-linux.sh contract, issue 1311)"
    );
    assert!(
        on_noncomment_line(&body, "confirm_setup \"$ASSUME_YES\""),
        "the live flow must confirm via confirm_setup \"$ASSUME_YES\" (issue 1311)"
    );
    let reads = body
        .lines()
        .filter(|l| !l.trim_start().starts_with('#') && l.contains("read -p"))
        .count();
    assert_eq!(
        reads, 1,
        "exactly one prompt, inside confirm_setup (issue 1311)"
    );
}

// ================================================================================================
// Fix 4 -- verify-device.sh never trusts dev1's known_hosts
// ================================================================================================

#[test]
fn verify_device_every_ssh_ignores_known_hosts_1311() {
    let body = read("scripts/verify-device.sh");
    let ssh_lines: Vec<&str> = body
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !t.starts_with('#')
                && (t.contains(" ssh -")
                    || t.starts_with("ssh -")
                    || t.starts_with("scp ")
                    || t.contains(" scp "))
        })
        .collect();
    assert!(
        !ssh_lines.is_empty(),
        "verify-device.sh must have an ssh invocation"
    );
    for l in ssh_lines {
        assert!(
            l.contains("-o UserKnownHostsFile=/dev/null"),
            "every ssh/scp in verify-device.sh must pass -o UserKnownHostsFile=/dev/null so a \
             reflashed box's new host key never reads as ssh rc=255 (issue 1311): {l}"
        );
    }
}

#[test]
fn verify_device_ssh_box_passes_the_known_hosts_override_1311() {
    let body = read("scripts/verify-device.sh");
    let func = function_body(&body, "ssh_box");
    let dir = scratch_dir("sshpass");
    write_exec(
        &dir.join("sshpass"),
        "#!/usr/bin/env bash\nprintf '%s\\n' \"$@\"\n",
    );
    let harness = format!(
        "set -uo pipefail\nPATH=\"$STUB:$PATH\"\nCAM_PW=pw\nSSH_TIMEOUT=5\nSSH_USER=root\nIP=10.0.0.9\n{func}\nssh_box 'true'\n"
    );
    let (code, out, err) = run_bash(&harness, &[("STUB", dir.display().to_string())]);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(code, 0, "stderr: {err}");
    let argv: Vec<&str> = out.lines().collect();
    let has_opt = |v: &str| argv.windows(2).any(|w| w[0] == "-o" && w[1] == v);
    assert!(has_opt("UserKnownHostsFile=/dev/null"), "argv: {argv:?}");
    assert!(has_opt("StrictHostKeyChecking=no"), "argv: {argv:?}");
}

// ================================================================================================
// Fix 5 -- early clock sanity from the Ubuntu archive's HTTP Date header, forward-only
// ================================================================================================

const HEADERS: &str = "HTTP/1.1 200 OK\r\nServer: Apache\r\ndate: Thu, 24 Sep 2026 14:00:00 GMT\r\nContent-Type: text/html\r\n\r\n";

#[test]
fn http_date_header_value_extracts_the_date_1311() {
    let (code, out, err) = run_setup_sourced_env(
        r#"printf '[%s]\n' "$(http_date_header_value "$HDRS")"
           printf '[%s]\n' "$(http_date_header_value "$NODATE")""#,
        &[
            ("HDRS", HEADERS.to_string()),
            ("NODATE", "HTTP/1.1 200 OK\r\nServer: x\r\n\r\n".to_string()),
        ],
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["[Thu, 24 Sep 2026 14:00:00 GMT]", "[]"],
        "stderr: {err}"
    );
}

#[test]
fn clock_sanity_decision_only_moves_forward_past_one_day_1311() {
    let server: i64 = 1_790_258_400; // Thu, 24 Sep 2026 14:00:00 GMT
    let cases: &[(String, String, &str)] = &[
        // CMOS reset: the box thinks it is 28.7.2026 -> set.
        ("1785229200".into(), server.to_string(), "set"),
        // Exactly one day behind is still tolerated; one second more is not.
        ((server - 86_400).to_string(), server.to_string(), "ok"),
        ((server - 86_401).to_string(), server.to_string(), "set"),
        // Box AHEAD of the archive: never move the clock backward.
        ((server + 999_999).to_string(), server.to_string(), "ok"),
        // No / garbage server time -> unknown (never act).
        (server.to_string(), String::new(), "unknown"),
        (server.to_string(), "abc".into(), "unknown"),
    ];
    for (now, srv, want) in cases {
        let (code, out, err) = run_setup_sourced(&format!("clock_sanity_decision '{now}' '{srv}'"));
        assert_eq!(code, 0, "stderr: {err}");
        assert_eq!(out.trim(), *want, "now={now} server={srv}");
    }
}

/// The orchestration, with the network fetch / clock read / clock set seams overridden.
fn run_clock_fix(now: &str, headers: Option<&str>) -> (String, String) {
    let fetch = match headers {
        Some(_) => "clock_sanity_fetch_headers() { printf '%s' \"$HDRS\"; }",
        None => "clock_sanity_fetch_headers() { return 1; }",
    };
    let (code, out, err) = run_setup_sourced_env(
        &format!(
            "{fetch}\nclock_sanity_now_epoch() {{ echo {now}; }}\n\
             clock_sanity_set_clock() {{ echo \"SET_CLOCK $1\"; }}\n\
             clock_sanity_fix_from_archive; echo \"rc=$?\""
        ),
        &[("HDRS", headers.unwrap_or("").to_string())],
    );
    assert_eq!(code, 0, "stderr: {err}");
    (out, err)
}

#[test]
fn clock_sanity_fix_sets_a_far_behind_clock_forward_and_logs_it_1311() {
    let (out, err) = run_clock_fix("1785229200", Some(HEADERS));
    assert!(out.contains("SET_CLOCK 1790258400"), "{out}\n{err}");
    assert!(out.contains("rc=0"), "{out}");
    assert!(
        out.to_lowercase().contains("clock") && out.contains("Thu, 24 Sep 2026 14:00:00 GMT"),
        "the forward step must be logged loudly with the archive date: {out}"
    );
}

#[test]
fn clock_sanity_fix_leaves_a_sane_or_ahead_clock_alone_1311() {
    for now in ["1790258000", "1790999999"] {
        let (out, err) = run_clock_fix(now, Some(HEADERS));
        assert!(
            !out.contains("SET_CLOCK"),
            "now={now} must not be touched: {out}\n{err}"
        );
        assert!(out.contains("rc=0"), "{out}");
    }
}

#[test]
fn clock_sanity_fix_is_non_fatal_when_the_archive_is_unreachable_1311() {
    let (out, err) = run_clock_fix("1785229200", None);
    assert!(!out.contains("SET_CLOCK"), "{out}\n{err}");
    assert!(
        out.contains("rc=0"),
        "an unreachable archive must warn, never abort: {out}"
    );
}

#[test]
fn setup_device_runs_clock_sanity_before_the_first_apt_and_curl_1311() {
    let body = read("scripts/setup-device.sh");
    let noncomment_idx = |needle: &str| -> Option<usize> {
        body.lines()
            .position(|l| !l.trim_start().starts_with('#') && l.trim_start().starts_with(needle))
    };
    let call = noncomment_idx("clock_sanity_fix_from_archive")
        .expect("the live flow must call clock_sanity_fix_from_archive (issue 1311)");
    let confirm = body
        .lines()
        .position(|l| {
            !l.trim_start().starts_with('#') && l.contains("confirm_setup \"$ASSUME_YES\"")
        })
        .expect("confirm_setup call");
    let first_apt = noncomment_idx("apt-get update").expect("pre-flight apt-get update");
    let first_curl = body
        .lines()
        .enumerate()
        .skip(confirm)
        .find(|(_, l)| !l.trim_start().starts_with('#') && l.contains("curl -fsSL"))
        .map(|(i, _)| i)
        .expect("a curl download");
    assert!(
        confirm < call && call < first_apt && call < first_curl,
        "clock sanity must run after the confirm and BEFORE the first apt/curl: confirm={confirm} \
         call={call} apt={first_apt} curl={first_curl}"
    );
    let set_fn = function_body(&body, "clock_sanity_set_clock");
    assert!(
        set_fn.contains("date -u -s \"@$1\""),
        "sets the clock by epoch: {set_fn}"
    );
}
