//! Guard for #1359 — the OBS CEF (obs-browser) runs on Linux with Chromium's
//! `--password-store=basic`, so a boot-time auto-login never raises a GNOME keyring unlock dialog.
//!
//! Background (measured on strih-lx, 23.9.2026): auto-login does not unlock the login keyring, and
//! the only items in it are Chromium's own (`Chrome Safe Storage Control`, `Chromium Safe Storage`).
//! The browser sources start with OBS at login, Chromium's os_crypt asks libsecret for its storage
//! key, the keyring is locked, and GNOME shows the unlock dialog on the operator screen. macOS
//! already gets the equivalent (`use-mock-keychain`) in the same `OnBeforeCommandLineProcessing`.
//! Fix (design issuecomment-5793018939, Prístup 1): append `password-store=basic` in the
//! non-Windows, non-Apple branch next to `ozone-platform`; `verify-strih.sh` reports the switch.
//!
//! Two facets, both std-only (Tier-0: the vendored C++ compiles only on CI):
//! the vendored-source anchor, and the pure verify-strih decision helpers in
//! `scripts/lib/strih-cef-keyring.sh` executed through a sourced bash harness (the
//! `tests/strih_provision_pure_functions.rs::run_sourced` convention), plus the verify-strih item
//! itself run end-to-end against fake live state.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

const BROWSER_APP: &str = "vendor/obs-studio/plugins/obs-browser/browser-app.cpp";
const LIB: &str = "scripts/lib/strih-cef-keyring.sh";
const VERIFY: &str = "scripts/verify-strih.sh";

/// The `OnBeforeCommandLineProcessing` body (definition line up to the first `\n}\n`).
fn command_line_fn() -> String {
    let src = read(BROWSER_APP);
    let start = src
        .find("void BrowserApp::OnBeforeCommandLineProcessing(")
        .unwrap_or_else(|| panic!("{BROWSER_APP}: OnBeforeCommandLineProcessing not found"));
    let rest = &src[start..];
    let end = rest
        .find("\n}\n")
        .unwrap_or_else(|| panic!("{BROWSER_APP}: end of OnBeforeCommandLineProcessing not found"));
    rest[..end].to_string()
}

/// (Apple branch, non-Windows/non-Apple branch) of the platform `#ifdef` that ends the function.
fn platform_branches() -> (String, String) {
    let body = command_line_fn();
    let apple = body
        .find("#ifdef __APPLE__")
        .unwrap_or_else(|| panic!("{BROWSER_APP}: the `#ifdef __APPLE__` switch block is gone"));
    let elif = body[apple..]
        .find("#elif !defined(_WIN32)")
        .map(|i| apple + i)
        .unwrap_or_else(|| panic!("{BROWSER_APP}: the `#elif !defined(_WIN32)` branch is gone"));
    let endif = body[elif..]
        .find("#endif")
        .map(|i| elif + i)
        .unwrap_or_else(|| panic!("{BROWSER_APP}: the platform switch block has no `#endif`"));
    (body[apple..elif].to_string(), body[elif..endif].to_string())
}

#[test]
fn linux_cef_gets_password_store_basic_next_to_ozone_platform() {
    let (_apple, linux) = platform_branches();
    assert!(
        linux.contains("command_line->AppendSwitchWithValue(\"password-store\", \"basic\");"),
        "{BROWSER_APP}: the `#elif !defined(_WIN32)` (Linux) branch must append \
         `password-store=basic` so Chromium's os_crypt never asks the locked GNOME keyring \
         (#1359). Branch:\n{linux}"
    );
    assert!(
        linux.contains("\"ozone-platform\""),
        "{BROWSER_APP}: the Linux branch must keep its `ozone-platform` switch. Branch:\n{linux}"
    );
}

#[test]
fn password_store_is_linux_only_and_appears_once() {
    let (apple, _linux) = platform_branches();
    assert!(
        !apple.contains("password-store"),
        "{BROWSER_APP}: macOS already uses `use-mock-keychain`; `password-store` belongs only to \
         the Linux branch (#1359). Apple branch:\n{apple}"
    );
    assert!(
        apple.contains("\"use-mock-keychain\""),
        "{BROWSER_APP}: the macOS `use-mock-keychain` precedent must stay in the Apple branch."
    );
    let n = read(BROWSER_APP).matches("\"password-store\"").count();
    assert_eq!(
        n, 1,
        "{BROWSER_APP}: expected exactly ONE `\"password-store\"` switch (the Linux branch), found {n}"
    );
}

/// Source the real keyring lib (and, when `with_provision`, the strih-provision lib) and run
/// `body`. Returns (exit_code, stdout, stderr).
fn run_sourced(env: &[(&str, &str)], with_provision: bool, body: &str) -> (i32, String, String) {
    let lib = manifest_dir().join(LIB);
    assert!(lib.exists(), "{} not found (#1359 lib)", lib.display());
    let provision = manifest_dir().join("scripts/lib/strih-provision.sh");
    let prov_line = if with_provision {
        ". \"$PROVISION\"\n"
    } else {
        ""
    };
    let harness = format!("set -euo pipefail\n{prov_line}. \"$SCRIPT\"\n{body}");
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(&harness)
        .env("SCRIPT", &lib)
        .env("PROVISION", &provision);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn so_state_reads_the_switch_literal_out_of_the_loaded_plugin() {
    let dir = tempfile::tempdir().unwrap();
    let carries = dir.path().join("carries.so");
    // A binary-ish blob with the literal embedded between NULs, like a .rodata string.
    let mut blob = vec![0u8, 1, 2, 0x7f, b'E', b'L', b'F', 0];
    blob.extend_from_slice(b"ozone-platform\0password-store\0basic\0");
    blob.extend_from_slice(&[0xff, 0xfe, 0]);
    std::fs::write(&carries, &blob).unwrap();
    let missing = dir.path().join("missing.so");
    std::fs::write(&missing, [0u8, 1, 2, b'o', b'z', b'o', b'n', b'e', 0, 0xff]).unwrap();
    let absent = dir.path().join("absent.so");
    let (c, out, err) = run_sourced(
        &[
            ("C", carries.to_str().unwrap()),
            ("M", missing.to_str().unwrap()),
            ("A", absent.to_str().unwrap()),
        ],
        false,
        "printf '%s|' \"$(strih_cef_so_password_store_state \"$C\")\" \
         \"$(strih_cef_so_password_store_state \"$M\")\" \
         \"$(strih_cef_so_password_store_state \"$A\")\" \
         \"$(strih_cef_so_password_store_state '')\"",
    );
    assert_eq!(
        c, 0,
        "must survive set -euo pipefail on every input; stderr={err}"
    );
    assert_eq!(out, "carries|missing|absent|absent|");
}

fn verdict(so_state: &str, pages: &str) -> (i32, String) {
    let (c, out, err) = run_sourced(
        &[("SO", so_state), ("PAGES", pages)],
        false,
        "v=\"$(strih_cef_password_store_verdict \"$SO\" <<<\"$PAGES\" || true)\"\n\
         strih_cef_password_store_verdict \"$SO\" <<<\"$PAGES\" >/dev/null && rc=0 || rc=$?\n\
         printf '%s %s' \"$v\" \"$rc\"",
    );
    assert_eq!(c, 0, "harness must survive set -euo pipefail; stderr={err}");
    let (v, rc) = out.rsplit_once(' ').expect("verdict + rc");
    (rc.parse().unwrap(), v.to_string())
}

#[test]
fn verdict_prefers_the_live_command_line_then_the_built_plugin() {
    let live = "4242 /usr/lib/x86_64-linux-gnu/obs-plugins/obs-browser-page --type=utility \
                --password-store=basic --lang=en-US\n";
    let plain = "4243 /usr/lib/x86_64-linux-gnu/obs-plugins/obs-browser-page --type=renderer\n";
    for (so, pages, want, rc) in [
        ("carries", live, "ok-live", 0),
        ("missing", live, "ok-live", 0),
        ("absent", live, "ok-live", 0),
        ("carries", plain, "ok-built", 0),
        ("carries", "", "ok-built", 0),
        ("missing", plain, "missing", 1),
        ("missing", "", "missing", 1),
        ("absent", plain, "unknown", 1),
        ("absent", "", "unknown", 1),
        ("", "", "unknown", 1),
    ] {
        let (got_rc, got) = verdict(so, pages);
        assert_eq!(
            (got.as_str(), got_rc),
            (want, rc),
            "verdict(so={so:?}, pages={pages:?})"
        );
    }
}

#[test]
fn verdict_does_not_match_a_different_password_store_value() {
    // `--password-store=gnome-libsecret` is exactly the behaviour this ticket removes.
    let (rc, v) = verdict(
        "missing",
        "77 obs-browser-page --password-store=gnome-libsecret --type=utility\n",
    );
    assert_eq!((v.as_str(), rc), ("missing", 1));
}

// ---------------------------------------------------------------------------
// verify-strih wiring
// ---------------------------------------------------------------------------

/// The verify-strih item text, from its `# 14b)` header to the next `# 15)` item.
fn verify_item() -> String {
    let v = read(VERIFY);
    let start = v
        .find("# 14b) ")
        .unwrap_or_else(|| panic!("{VERIFY}: the #1359 `# 14b)` CEF keyring item is missing"));
    let end = v[start..]
        .find("\n# 15) ")
        .map(|i| start + i)
        .unwrap_or_else(|| panic!("{VERIFY}: item `# 15)` must follow the `# 14b)` item"));
    v[start..end].to_string()
}

#[test]
fn verify_strih_sources_the_keyring_lib_once() {
    let v = read(VERIFY);
    let n = v.matches(". \"${HERE}/lib/strih-cef-keyring.sh\"").count();
    assert_eq!(
        n, 1,
        "{VERIFY}: must source scripts/lib/strih-cef-keyring.sh exactly once (found {n})"
    );
    let src = v.find(". \"${HERE}/lib/strih-cef-keyring.sh\"").unwrap();
    let guard = v
        .find("# --- source-guard")
        .expect("verify-strih source-guard marker");
    assert!(
        src < guard,
        "{VERIFY}: the keyring lib must be sourced BEFORE the source-guard (the unit tests source \
         verify-strih and stop there)."
    );
}

#[test]
fn verify_strih_item_is_report_only_and_drain_safe() {
    let item = verify_item();
    for needle in [
        "(cef-keyring)",
        "strih_cef_so_password_store_state",
        "strih_cef_password_store_verdict",
        "pgrep -af obs-browser-page 2>/dev/null || true)\"",
        "<<<\"$CEF_PAGES_V\" || true)\"",
        "${STRIH_LIBDIR}/obs-plugins/obs-browser.so",
    ] {
        assert!(
            item.contains(needle),
            "{VERIFY}: the `# 14b)` item is missing `{needle}`. Item:\n{item}"
        );
    }
    // No overstatement: the item checks an argv / a compiled-in string, not the dialog itself.
    assert!(
        !item.contains("no keyring unlock dialog"),
        "{VERIFY}: the PASS lines must say what was checked, not claim the dialog is gone \
         (behaviour proof = the two-reboot acceptance). Item:\n{item}"
    );
    assert!(
        item.contains("two-reboot acceptance"),
        "{VERIFY}: the PASS lines must point at the two-reboot acceptance. Item:\n{item}"
    );
    assert!(
        !item.contains("bad \""),
        "{VERIFY}: the CEF keyring item is REPORT-ONLY (design issuecomment-5793018939) — it must \
         never call bad. Item:\n{item}"
    );
}

/// Run the real `# 14b)` item against fake state: a flags file, a fake `pgrep`, a seamed libdir.
fn run_verify_item(flags: Option<&str>, so: Option<&[u8]>, pages: &str) -> (i32, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let pgrep = bin.join("pgrep");
    std::fs::write(
        &pgrep,
        "#!/bin/bash\nprintf '%s' \"$FAKE_PAGES\"\n[ -n \"$FAKE_PAGES\" ]\n",
    )
    .unwrap();
    let mut perm = std::fs::metadata(&pgrep).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    std::fs::set_permissions(&pgrep, perm).unwrap();
    let genlock = dir.path().join("genlock");
    std::fs::create_dir_all(&genlock).unwrap();
    let flags_file = genlock.join("STRIH_BUILD_FLAGS.txt");
    if let Some(f) = flags {
        std::fs::write(&flags_file, f).unwrap();
    }
    let libdir = dir.path().join("lib");
    std::fs::create_dir_all(libdir.join("obs-plugins")).unwrap();
    if let Some(bytes) = so {
        std::fs::write(libdir.join("obs-plugins/obs-browser.so"), bytes).unwrap();
    }
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let body = format!(
        "FAILS=0\nok() {{ echo \"PASS $1\"; }}\nbad() {{ echo \"FAIL $1\"; FAILS=$((FAILS+1)); }}\n\
         note() {{ echo \"NOTE $1\"; }}\nGENLOCK_DIR=\"$FAKE_GENLOCK\"\n\
         FLAGS_FILE=\"$FAKE_GENLOCK/STRIH_BUILD_FLAGS.txt\"\n{}\necho \"FAILS=$FAILS\"",
        verify_item()
    );
    run_sourced(
        &[
            ("PATH", &path),
            ("FAKE_PAGES", pages),
            ("FAKE_GENLOCK", genlock.to_str().unwrap()),
            ("STRIH_LIBDIR", libdir.to_str().unwrap()),
        ],
        true,
        &body,
    )
}

const BROWSER_ON: &str = "BROWSER-ON\nTARGET-RELEASE: ubuntu-26.04\n";
const SO_WITH_SWITCH: &[u8] = b"\x7fELF\0ozone-platform\0password-store\0basic\0";
const SO_WITHOUT_SWITCH: &[u8] = b"\x7fELF\0ozone-platform\0x11\0";

#[test]
fn verify_item_passes_on_the_live_command_line() {
    let (c, out, err) = run_verify_item(
        Some(BROWSER_ON),
        Some(SO_WITH_SWITCH),
        "9 /usr/lib/x86_64-linux-gnu/obs-plugins/obs-browser-page --type=utility --password-store=basic\n",
    );
    assert_eq!(c, 0, "stderr={err}\nout={out}");
    assert!(out.contains("PASS (cef-keyring)"), "out={out}");
    assert!(out.contains("FAILS=0"), "out={out}");
}

#[test]
fn verify_item_passes_on_the_built_plugin_when_no_page_shows_it() {
    let (c, out, err) = run_verify_item(Some(BROWSER_ON), Some(SO_WITH_SWITCH), "");
    assert_eq!(c, 0, "stderr={err}\nout={out}");
    assert!(out.contains("PASS (cef-keyring)"), "out={out}");
    assert!(out.contains("FAILS=0"), "out={out}");
}

#[test]
fn verify_item_notes_a_pre_fix_bundle_without_failing() {
    let (c, out, err) = run_verify_item(Some(BROWSER_ON), Some(SO_WITHOUT_SWITCH), "");
    assert_eq!(c, 0, "stderr={err}\nout={out}");
    assert!(
        out.contains("NOTE (cef-keyring)") && out.contains("predates"),
        "out={out}"
    );
    assert!(out.contains("FAILS=0"), "report-only: out={out}");
}

#[test]
fn verify_item_notes_a_missing_plugin_and_skips_browser_off() {
    let (c, out, err) = run_verify_item(Some(BROWSER_ON), None, "");
    assert_eq!(c, 0, "stderr={err}\nout={out}");
    assert!(
        out.contains("NOTE (cef-keyring)") && out.contains("not found"),
        "out={out}"
    );
    assert!(out.contains("FAILS=0"), "out={out}");

    let (c, out, err) = run_verify_item(None, Some(SO_WITH_SWITCH), "");
    assert_eq!(c, 0, "stderr={err}\nout={out}");
    assert!(
        out.contains("NOTE (cef-keyring) skipped"),
        "BROWSER-OFF/absent flags must NOTE-skip: out={out}"
    );
    assert!(out.contains("FAILS=0"), "out={out}");
}
