//! #773 — OBS Properties dialog crash `c0000005` on close: `OBSBasicProperties::CheckSettings()`
//! passed the results of `obs_data_get_json(...)` straight into `strcmp()` with NO NULL guard, and
//! `obs_data_get_json()` returns NULL when `json_dumps()` fails (libobs/obs-data.c) — and
//! `obs_source_get_settings()` returns NULL for a source that became invalid mid-dialog, which
//! `obs_data_get_json()` then also maps to NULL. Proven by the live strih crash log
//! `Crash 2026-07-15 18-46-03.txt`:
//!
//! ```text
//! Fault address: 7FFC036AD0E0 (ucrtbase.dll)          <- strcmp lives in the UCRT
//! Thread 1D94 (Crashed):
//!   ucrtbase.dll!0x...  Arg0=0x0                       <- NULL first arg into strcmp
//!   obs64.exe!OBSBasicProperties::CheckSettings+0x3b
//!   obs64.exe!OBSBasicProperties::reject+0x14
//!   obs64.exe!OBSBasicProperties::closeEvent+0x16
//!   ... obs64.exe!OBSBasicProperties::on_buttonBox_clicked+0x51d   (Cancel/RejectRole)
//! ```
//!
//! Fix: a file-local `static int settings_json_diff(const char *current, const char *old)` that
//! treats an unreadable (NULL) current/old JSON as "no detectable change" (0) — so the dialog
//! closes cleanly instead of dereferencing NULL (and without popping a Save/Discard prompt on
//! settings that cannot even be serialised). `CheckSettings()` routes through it, and the sibling
//! save path (`on_buttonBox_clicked` AcceptRole) — which built `std::string(obs_data_get_json(...))`
//! (UB / crash on a NULL) — is NULL-coalesced to `""`.
//!
//! Why this test is std-only + runs offline: camera-box's `# airuleset:build-ok` bypass is disabled
//! and the vendored OBS frontend compiles only on CI, so per
//! `.claude/rules/vendored-libobs-change-safety.md` (the #793 `video_io_null_guard.rs` / #1026
//! pattern) this file (a) SOURCE-ANCHORS the tokens with a std-only `fs::read_to_string` guard
//! runnable via `rustc --test` (revert protection against a future `git subtree pull`), and (b)
//! LIFTS the pure `settings_json_diff` helper VERBATIM, compiles it with the C toolchain, and runs
//! it over a hand-written truth table — proving the SHIPPED bytes COMPILE and COMPUTE the NULL-safe
//! comparison, not just that they SAY it. Nothing in the Rust appliance consumes the helper, so the
//! truth table IS the spec. Per test-strictness it FAILS LOUDLY when no C compiler is present.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const DIALOG: &str = "vendor/obs-studio/frontend/dialogs/OBSBasicProperties.cpp";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn vendor_file(rel: &str) -> String {
    let p = repo(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Collapse every run of ASCII whitespace to a single space so anchors survive reformatting.
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Extract the body of a top-level C/C++ function starting at its exact signature line.
fn fn_body<'a>(src: &'a str, signature: &str) -> &'a str {
    let start = src
        .find(signature)
        .unwrap_or_else(|| panic!("signature `{signature}` not found in {DIALOG}"));
    let rest = &src[start..];
    let open = rest.find('{').expect("opening brace");
    let mut depth = 0usize;
    for (i, c) in rest[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &rest[..open + i + 1];
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced braces after `{signature}`");
}

// ----------------------------------------------------------------------------------------------
// Facet A — source anchors (revert protection).
// ----------------------------------------------------------------------------------------------

#[test]
fn settings_json_diff_helper_is_null_safe_773() {
    let raw = vendor_file(DIALOG);
    let src = squish(&raw);
    assert!(
        src.contains("static int settings_json_diff(const char *currentJson, const char *oldJson)"),
        "{DIALOG}: #773 patch missing — the NULL-safe `settings_json_diff(...)` helper is gone. \
         Without it CheckSettings dereferences a NULL obs_data_get_json() result in strcmp \
         (c0000005 on dialog close). A `git subtree pull` likely reverted it."
    );
    // The NULL guard must come BEFORE the strcmp inside the helper body.
    let body = fn_body(
        &raw,
        "static int settings_json_diff(const char *currentJson, const char *oldJson)",
    );
    let guard = body.find("if (!currentJson || !oldJson)").unwrap_or_else(|| {
        panic!("#773 regression: settings_json_diff lost its `if (!currentJson || !oldJson)` guard")
    });
    let deref = body
        .find("strcmp(")
        .expect("#773: settings_json_diff still compares via strcmp");
    assert!(
        guard < deref,
        "#773 regression: settings_json_diff must check both pointers for NULL BEFORE strcmp"
    );
}

#[test]
fn check_settings_routes_through_the_null_safe_helper_773() {
    let raw = vendor_file(DIALOG);
    let body = fn_body(&raw, "int OBSBasicProperties::CheckSettings()");
    let squished = squish(body);
    assert!(
        squished.contains("return settings_json_diff(currentSettingsJson, oldSettingsJson);"),
        "#773 regression: CheckSettings must return settings_json_diff(currentSettingsJson, \
         oldSettingsJson), not a raw strcmp. Body:\n{body}"
    );
    assert!(
        !squished.contains("strcmp("),
        "#773 regression: CheckSettings must NOT call strcmp directly — a NULL json (json_dumps \
         failure, or obs_source_get_settings on an invalid source) crashes c0000005 in \
         ucrtbase!strcmp. Route through settings_json_diff. Body:\n{body}"
    );
}

#[test]
fn accept_path_json_construction_is_null_safe_773() {
    let raw = vendor_file(DIALOG);
    let body = fn_body(
        &raw,
        "void OBSBasicProperties::on_buttonBox_clicked(QAbstractButton *button)",
    );
    let squished = squish(body);
    // The old crash-prone forms constructed std::string directly from obs_data_get_json(...) —
    // std::string(NULL) is undefined behaviour (crash). Those must be gone.
    assert!(
        !squished.contains("std::string undo_data(obs_data_get_json(")
            && !squished.contains("std::string redo_data(obs_data_get_json("),
        "#773 regression: the AcceptRole path still builds std::string directly from \
         obs_data_get_json(...), which is UB / crash on a NULL json. NULL-coalesce it first. \
         Body:\n{body}"
    );
    // The NULL-coalesced locals must be present.
    assert!(
        squished.contains("undo_json ? undo_json :") && squished.contains("redo_json ? redo_json :"),
        "#773 regression: the AcceptRole undo/redo json must be NULL-coalesced \
         (`undo_json ? undo_json : \"\"`). Body:\n{body}"
    );
}

// ----------------------------------------------------------------------------------------------
// Facet B — lift the pure helper, compile it standalone under -Werror, run a truth table.
// ----------------------------------------------------------------------------------------------

/// Lift `settings_json_diff` VERBATIM from the vendored source (never retype it — a retyped copy
/// verifies your typing, not the shipped bytes).
fn lift_helper() -> String {
    let src = vendor_file(DIALOG);
    let start = src
        .find("static int settings_json_diff(const char *currentJson, const char *oldJson)")
        .unwrap_or_else(|| {
            panic!("#773: {DIALOG} no longer defines settings_json_diff — nothing to compile.")
        });
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .expect("#773: settings_json_diff has no closing brace `\\n}\\n`");
    src[start..end].to_string()
}

/// `(current, old, expect_zero)`. NULL is encoded as `None`. A NULL on either side must fold to
/// "no change" (0); two equal strings compare 0; two different strings compare non-zero.
fn vectors() -> Vec<(Option<&'static str>, Option<&'static str>, bool)> {
    vec![
        (None, Some("x"), true),          // current NULL (json_dumps failed / invalid source)
        (Some("x"), None, true),          // old NULL
        (None, None, true),               // both NULL
        (Some("abc"), Some("abc"), true), // equal -> no change
        (Some("abc"), Some("abd"), false),// differ
        (Some("abd"), Some("abc"), false),// differ (other direction)
        (Some(""), Some(""), true),       // empty equal
        (Some(""), Some("x"), false),     // empty vs non-empty differ
    ]
}

#[test]
fn settings_json_diff_computes_the_null_safe_truth_table_773() {
    let helper = lift_helper();
    let vs = vectors();

    // Encode each vector's args as C literals; NULL is a real null pointer.
    let arg = |o: Option<&str>| match o {
        None => "(const char *)0".to_string(),
        Some(s) => format!("\"{s}\""),
    };

    let mut c = String::from("#include <string.h>\n#include <stdio.h>\n");
    c.push_str(&helper);
    c.push_str("\nint main(void){\n");
    for (cur, old, _) in &vs {
        c.push_str(&format!(
            "    printf(\"%d\\n\", settings_json_diff({}, {}));\n",
            arg(*cur),
            arg(*old)
        ));
    }
    c.push_str("    return 0;\n}\n");

    let dir = std::env::temp_dir().join("prop_dialog_checksettings_null_guard_773");
    fs::create_dir_all(&dir).expect("create the scratch dir");
    let cfile = dir.join("diff.c");
    let bin = dir.join("diff.bin");
    fs::write(&cfile, &c).expect("write the harness");

    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=gnu99",
            "-Wall",
            "-Wextra",
            "-Wformat=2",
            "-Wconversion",
            "-Werror",
            "-O1",
        ])
        .arg(&cfile)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "#773: could not run the C compiler `{cc}` ({e}). This gate compiles the vendored \
                 settings_json_diff to prove the C both COMPILES and computes the NULL-safe \
                 comparison; it must FAIL rather than skip when the toolchain is absent (a gate \
                 that silently passes without running is worse than none). Install a C compiler or \
                 set CC."
            )
        });
    assert!(
        out.status.success(),
        "#773: settings_json_diff lifted from {DIALOG} does NOT COMPILE standalone under \
         -Werror. The vendored frontend is otherwise compiled only on CI, so this is very likely \
         a real compile error heading for CI:\n--- cc stderr ---\n{}\n--- harness ---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );

    let run = Command::new(&bin)
        .output()
        .expect("#773: the compiled harness failed to execute");
    assert!(run.status.success(), "#773: the harness exited non-zero");
    let stdout = String::from_utf8(run.stdout).expect("harness stdout is utf-8");
    let got: Vec<i32> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim().parse().expect("harness printed a non-integer"))
        .collect();
    assert_eq!(got.len(), vs.len(), "#773: harness printed {} of {} results", got.len(), vs.len());

    let mut diffs = Vec::new();
    for ((cur, old, expect_zero), g) in vs.iter().zip(&got) {
        let ok = if *expect_zero { *g == 0 } else { *g != 0 };
        if !ok {
            diffs.push(format!(
                "  settings_json_diff({cur:?}, {old:?}) = {g}, expected {}",
                if *expect_zero { "0" } else { "non-zero" }
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "#773: the vendored settings_json_diff DIVERGED from the NULL-safe spec on {} of {} \
         vectors:\n{}",
        diffs.len(),
        vs.len(),
        diffs.join("\n")
    );
}
