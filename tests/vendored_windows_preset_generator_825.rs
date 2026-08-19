//! Patch-presence guard for #825 — the vendored Windows CMake generator.
//!
//! OBS 32.2's upstream `CMakePresets.json` pins the Windows presets to the
//! `"Visual Studio 18 2026"` generator. No GitHub Actions runner image provides VS 2026
//! (they ship VS 17 2022), so `cmake --preset windows-x64` — which both
//! `windows-genlock.yml` and `windows-genlock-fast.yml` run — fails at CONFIGURE with
//! `CMake Error: Could not create named generator Visual Studio 18 2026` (live: run
//! 32138592528). CMake refuses a `-G` override when a preset names a generator, so the
//! fix pins the vendored preset itself to `"Visual Studio 17 2022"` (windows-x64 +
//! windows-arm64). A future `git subtree pull` of upstream OBS reverts it — this guard
//! then fails loudly (the alternative is a fresh confusing CI configure failure).
//! Re-pin here (and drop the pin) when the runner image ships a VS the preset names.

use std::path::PathBuf;

fn vendor_file(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

const PRESETS: &str = "vendor/obs-studio/CMakePresets.json";

#[test]
fn windows_presets_do_not_use_the_unavailable_vs18_2026_generator() {
    let src = vendor_file(PRESETS);
    assert!(
        !src.contains("Visual Studio 18 2026"),
        "{PRESETS}: #825 — a preset is back on the \"Visual Studio 18 2026\" generator no \
         runner can create; re-pin the windows-x64/windows-arm64 generator to \
         \"Visual Studio 17 2022\" (a subtree pull of OBS 32.2 likely reverted it)."
    );
}

#[test]
fn both_windows_presets_pin_vs17_2022() {
    let src = vendor_file(PRESETS);
    let n = src
        .matches("\"generator\": \"Visual Studio 17 2022\"")
        .count();
    assert!(
        n >= 2,
        "{PRESETS}: #825 — expected >= 2 \"Visual Studio 17 2022\" generators (windows-x64 + \
         windows-arm64), found {n}."
    );
}
