//! #1152 M1 — in-OBS vendored DRM-lease output guard (`vendor/obs-studio/libobs/obs-drm-output.c`).
//!
//! Background (owner KOREKCIA 2026-08-20, supersedes the design spec's NDI-loopback P1): the imag
//! HDMI Program output must leave the Xorg desktop entirely. The binding shape is a **vendored OBS
//! DRM output** — our forked OBS acquires DRM master of the HDMI connector through an X RandR
//! output LEASE (`xcb_randr_create_lease`) and page-flips onto it DIRECTLY (`drmModePageFlip`),
//! render→scanout, with NO NDI encode/decode hop and NO external presenter process. M1 proves the
//! mechanism: lease acquire + a solid-color page-flip from the OBS process (not yet bound to the
//! Program render texture). Activation is DEFAULT-OFF (a config file absent → dormant no-op).
//!
//! Why this test is std-only + runs offline: camera-box's `# airuleset:build-ok` bypass is disabled
//! and the vendored C compiles only on the linux-genlock.yml workflow (its FIRST compiler), so per
//! `.claude/rules/vendored-libobs-change-safety.md` this file (a) SOURCE-ANCHORS the C/CMake tokens
//! with a std-only `fs::read_to_string` guard runnable via `rustc --test` (revert protection against
//! a future `git subtree pull` re-importing stock OBS, and against an accidental relink drop), and
//! (b) LIFTS the pure `drm_output_pick_free_crtc` decision helper VERBATIM, compiles it with the C
//! toolchain against a tiny stub, and runs it over a hand-written truth table encoding the exact
//! CRTC-selection contract at every boundary — proving the SHIPPED bytes COMPUTE, not just SAY, the
//! right thing (mirrors `tests/distroav_ndi_reconnect_767.rs`; the helper is the sole authority here
//! — nothing in the Rust appliance consumes it — so the truth table IS the spec). Per the project's
//! test-strictness rule the lift-compile FAILS LOUDLY if no C compiler is present, never skips.
//!
//! No pwsh mirror in windows-genlock*.yml: this is a pure Linux/EGL/DRM path — strih+stream are
//! libobs-d3d11 (Windows), where xcb/libdrm/RandR-lease do not exist. The module is wholly under
//! `#if defined(__linux__)` and added only in `libobs/cmake/os-linux.cmake`, so it never compiles
//! or links on Windows; the Windows workflows have nothing here to assert.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const OBS_DRM_OUTPUT_C: &str = "vendor/obs-studio/libobs/obs-drm-output.c";
const OBS_DRM_OUTPUT_H: &str = "vendor/obs-studio/libobs/obs-drm-output.h";
const OBS_C: &str = "vendor/obs-studio/libobs/obs.c";
const OS_LINUX_CMAKE: &str = "vendor/obs-studio/libobs/cmake/os-linux.cmake";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn vendor_file(rel: &str) -> String {
    let p = repo(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Collapse every run of ASCII whitespace to a single space so the anchors survive reformatting
/// (an upstream merge re-indenting a line, a clang-format wrap move).
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ----------------------------------------------------------------------------------------------
// Facet A — source anchors. The mechanism must be present in the vendored tree (revert protection),
// AND the KOREKCIA's binding shape must hold: DIRECT in-OBS DRM page-flip, NOT an NDI hop.
// ----------------------------------------------------------------------------------------------

#[test]
fn drm_output_module_carries_the_lease_and_pageflip_mechanism() {
    let src = squish(&vendor_file(OBS_DRM_OUTPUT_C));

    // Isolation: DRM master is obtained through an X RandR output LEASE (not full SET_MASTER).
    assert!(
        src.contains("xcb_randr_create_lease("),
        "#1152 M1: {OBS_DRM_OUTPUT_C} must acquire the HDMI connector via xcb_randr_create_lease() \
         — the lease is what lets OBS become DRM master of ONLY the leased connector+CRTC without \
         fighting the running X server."
    );
    // Deactivation seam: the lease MUST be released so the connector returns to Xorg.
    assert!(
        src.contains("xcb_randr_free_lease("),
        "#1152 M1: {OBS_DRM_OUTPUT_C} must release the lease via xcb_randr_free_lease() on stop — \
         a leaked lease would strand the HDMI connector away from X permanently."
    );
    // Scanout: OBS itself page-flips DIRECTLY (render->scanout), the owner's binding decision.
    assert!(
        src.contains("drmModePageFlip("),
        "#1152 M1: {OBS_DRM_OUTPUT_C} must page-flip directly via drmModePageFlip() — the owner \
         KOREKCIA requires OBS to draw onto the leased connector itself, no external presenter."
    );
    // The pure CRTC-selection helper must be the one that is lift-compiled below.
    assert!(
        src.contains("static int drm_output_pick_free_crtc("),
        "#1152 M1: {OBS_DRM_OUTPUT_C} must define the pure `static int drm_output_pick_free_crtc(` \
         helper used to choose the free CRTC to lease (Tier-0 unit-tested below)."
    );
    // Observability: a UNIQUE `drm-output:` log prefix (distinct from `genlock:` and `presenter:`).
    assert!(
        src.contains("\"drm-output:"),
        "#1152 M1: {OBS_DRM_OUTPUT_C} must emit `drm-output:`-prefixed blog lines (a unique marker \
         so the jitter_audit-family parsers stay independent — the 3-consumer gotcha)."
    );

    // KOREKCIA lock: the module must NOT reintroduce an NDI hop (the rejected NDI-loopback path).
    assert!(
        !src.contains("NDIlib"),
        "#1152 M1: {OBS_DRM_OUTPUT_C} must NOT depend on the NDI SDK (NDIlib*) — the owner rejected \
         the NDI-loopback feed; the Program reaches HDMI render->scanout inside OBS, no NDI."
    );
}

#[test]
fn drm_output_public_api_present() {
    let hdr = squish(&vendor_file(OBS_DRM_OUTPUT_H));
    assert!(
        hdr.contains("obs_drm_output_start("),
        "#1152 M1: {OBS_DRM_OUTPUT_H} must declare obs_drm_output_start()."
    );
    assert!(
        hdr.contains("obs_drm_output_stop("),
        "#1152 M1: {OBS_DRM_OUTPUT_H} must declare obs_drm_output_stop() (the deactivation entry)."
    );
    assert!(
        hdr.contains("obs_drm_output_maybe_autostart("),
        "#1152 M1: {OBS_DRM_OUTPUT_H} must declare obs_drm_output_maybe_autostart() (the \
         default-OFF config-driven autostart)."
    );
}

#[test]
fn obs_startup_calls_the_default_off_autostart() {
    let obs = squish(&vendor_file(OBS_C));
    assert!(
        obs.contains("#include \"obs-drm-output.h\""),
        "#1152 M1: {OBS_C} must include obs-drm-output.h to reach the autostart entry."
    );
    let call = obs.find("obs_drm_output_maybe_autostart();").unwrap_or_else(|| {
        panic!("#1152 M1: {OBS_C} must call obs_drm_output_maybe_autostart() once at startup — the \
                DEFAULT-OFF activation (no-op when the config file is absent).")
    });
    // The call MUST be Linux-guarded so Windows/macOS libobs never references the symbol — prove
    // the call sits INSIDE a `#if defined(__linux__)` … `#endif` block (a bare file-wide __linux__
    // search would go vacuous the day an unrelated upstream __linux__ appears; a fixed byte window
    // rots when the comment grows — so bound by the guard/endif structure instead).
    let guard = obs[..call]
        .rfind("#if defined(__linux__)")
        .unwrap_or_else(|| {
            panic!(
                "#1152 M1: {OBS_C} must guard obs_drm_output_maybe_autostart() with \
                #if defined(__linux__) — the module is Linux-only."
            )
        });
    assert!(
        !obs[guard..call].contains("#endif"),
        "#1152 M1: {OBS_C} the obs_drm_output_maybe_autostart() call must be INSIDE the \
         #if defined(__linux__) block (no #endif between the guard and the call)."
    );
    // obs_shutdown MUST stop the output before libobs teardown (flip thread must not outlive the
    // log sink; lease returned to Xorg deterministically, not only by process death).
    assert!(
        obs.contains("obs_drm_output_stop();"),
        "#1152 M1: {OBS_C} obs_shutdown must call obs_drm_output_stop() so the flip thread and the \
         HDMI lease are torn down before libobs shuts down."
    );
}

#[test]
fn os_linux_cmake_builds_and_links_the_module() {
    let cmake = squish(&vendor_file(OS_LINUX_CMAKE));
    assert!(
        cmake.contains("obs-drm-output.c"),
        "#1152 M1: {OS_LINUX_CMAKE} must add obs-drm-output.c to the libobs target_sources (else \
         linux-genlock.yml — the module's FIRST compiler — never compiles it)."
    );
    assert!(
        cmake.contains("find_package(Libdrm REQUIRED)"),
        "#1152 M1: {OS_LINUX_CMAKE} must find_package(Libdrm REQUIRED) for the page-flip path."
    );
    assert!(
        cmake.contains("Libdrm::Libdrm"),
        "#1152 M1: {OS_LINUX_CMAKE} must link Libdrm::Libdrm into libobs."
    );
    assert!(
        cmake.contains("XCB::RANDR"),
        "#1152 M1: {OS_LINUX_CMAKE} must request+link the XCB RANDR component (XCB::RANDR) for \
         xcb_randr_create_lease."
    );
}

// ----------------------------------------------------------------------------------------------
// Facet B — lift-compile the pure CRTC-selection helper and run it over a truth table.
// ----------------------------------------------------------------------------------------------

/// Lift `drm_output_pick_free_crtc` VERBATIM from the vendored C (never retype it — a retyped copy
/// verifies your typing, not the shipped bytes).
fn lift_pick_free_crtc() -> String {
    let src = vendor_file(OBS_DRM_OUTPUT_C);
    let start = src
        .find("static int drm_output_pick_free_crtc(")
        .unwrap_or_else(|| {
            panic!(
                "#1152: {OBS_DRM_OUTPUT_C} no longer defines drm_output_pick_free_crtc — there is \
                 nothing to compile/behaviour-check."
            )
        });
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .expect("#1152: drm_output_pick_free_crtc has no closing brace `\\n}\\n`");
    src[start..end].to_string()
}

/// `(busy_mask, n)` — the helper's arguments (bit i set = CRTC i currently in use).
type PickArgs = (u32, i32);

/// `(args, expected_index)`. `-1` = no free CRTC. Each row pins one boundary of the contract, and
/// the set varies BOTH parameters so a helper that hardcoded a constant (ignoring `busy_mask` or
/// `n`) fails at least one row.
fn vectors() -> Vec<(PickArgs, i32)> {
    vec![
        ((0x0, 4), 0),  // all free -> the FIRST CRTC (index 0)
        ((0x1, 4), 1),  // crtc0 busy -> 1
        ((0x3, 4), 2),  // crtc0,1 busy -> 2
        ((0x7, 4), 3),  // crtc0,1,2 busy -> 3 (last free)
        ((0xF, 4), -1), // all 4 busy -> none
        ((0xF, 0), -1), // n=0 -> none (nothing to scan)
        ((0x2, 4), 0), // crtc1 busy but crtc0 free -> 0 (returns the FIRST free, not "the free one")
        ((0xB, 4), 2), // 0b1011: bit2 clear -> 2 (a free CRTC in the middle)
        ((0x3, 2), -1), // n bounds: bits 0,1 busy AND n=2 -> nothing free in [0,2)
        ((0x3, 3), 2), // same mask, n=3 -> bit2 is now in range and free -> 2 (proves n matters)
    ]
}

#[test]
fn pick_free_crtc_computes_the_spec_truth_table() {
    let helper = lift_pick_free_crtc();
    let vs = vectors();

    let mut c = String::from("#include <stdint.h>\n#include <stdio.h>\n");
    c.push_str(&helper);
    c.push_str("\nint main(void){\n");
    for ((busy, n), _) in &vs {
        c.push_str(&format!(
            "    printf(\"%d\\n\", drm_output_pick_free_crtc({busy}u, {n}));\n"
        ));
    }
    c.push_str("    return 0;\n}\n");

    let dir = std::env::temp_dir().join(format!("drm_output_pick_1152_{}", std::process::id()));
    fs::create_dir_all(&dir).expect("create the scratch dir");
    let cfile = dir.join("pick.c");
    let bin = dir.join("pick.bin");
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
                "#1152: could not run the C compiler `{cc}` ({e}). This gate compiles the vendored \
                 drm_output_pick_free_crtc to prove the C both COMPILES and computes the spec; it \
                 must FAIL rather than skip when the toolchain is absent (a gate that silently \
                 passes without running is worse than none). Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "#1152: drm_output_pick_free_crtc lifted from {OBS_DRM_OUTPUT_C} does NOT COMPILE \
         standalone under -Wall -Wextra -Wformat=2 -Wconversion -Werror. The vendored tree is \
         otherwise compiled only by linux-genlock.yml, so this is very likely a real compile error \
         heading for CI:\n--- cc stderr ---\n{}\n--- harness ---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );

    let run = Command::new(&bin)
        .output()
        .expect("#1152: the compiled harness failed to execute");
    assert!(
        run.status.success(),
        "#1152: the harness exited non-zero: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8(run.stdout).expect("harness stdout is utf-8");
    let got: Vec<i32> = stdout
        .lines()
        .map(|l| {
            l.trim()
                .parse::<i32>()
                .expect("harness printed a non-integer")
        })
        .collect();
    assert_eq!(
        got.len(),
        vs.len(),
        "#1152: the harness printed {} results for {} vectors",
        got.len(),
        vs.len()
    );

    let mut diffs = Vec::new();
    for (((busy, n), want), g) in vs.iter().zip(&got) {
        if g != want {
            diffs.push(format!(
                "  busy_mask={busy:#06x} n={n} -> C {g}, expected {want}"
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "#1152: the vendored C drm_output_pick_free_crtc DIVERGED from the intended CRTC-selection \
         spec on {} of {} vectors:\n{}",
        diffs.len(),
        vs.len(),
        diffs.join("\n")
    );
}
