//! #1320 — a scene-switch-coincident reattach must NOT freeze the OBS PROGRAM render
//! (`vendor/distroav/src/ndi-source.cpp`).
//!
//! Root cause (traced + confirmed live, read-only strih OBS logs 15.9.2026): `ndi_source` is
//! `OBS_SOURCE_ASYNC_VIDEO` (⇒ `OBS_SOURCE_VIDEO`), so `obs_source_update()` DEFERS the update
//! (`defer_update_count++`) and the deferred `ndi_source_update` runs from `obs_source_video_tick`
//! → `obs_source_deferred_update`, which `obs-video.c` calls ON THE OBS GRAPHICS/RENDER THREAD
//! (`obs_graphics_thread_loop`). When a CLEAR-then-SET reattach clears the NDI source name to `""`,
//! `ndi_source_update` → `ndi_source_thread_stop` → `pthread_join`, and the av-thread's exit-path
//! `NDIlib_recv_destroy()` blocks ~7.5 s (an SDK-internal teardown timeout). The graphics thread
//! sits in that join the whole time → PROGRAM render freeze (`program-render-audit lagged=228
//! avg_frame_ms=782`) → `2ME PGM` starved → stream FIFO underrun → 462-relock storm → presented
//! video +2/+3 frames late for ~40 min. Seven severe freezes in one afternoon share this exact
//! signature (stop → exit `recv_destroy` → ~7.5 s → `Reset NDI Receiver`).
//!
//! Fix: hand the av-thread's exit-path receiver+framesync teardown to a DETACHED reaper thread so
//! the blocking `NDIlib_recv_destroy` never holds up the `pthread_join` (and therefore never
//! freezes the render thread). The eligibility is the pure `ndi_reap_should_defer(...)` predicate;
//! the reaper falls back to a synchronous destroy if the thread cannot be spawned (never crash).
//!
//! Why std-only + offline (per `.claude/rules/vendored-libobs-change-safety.md`, the #767 / #1026
//! pattern): camera-box's `# airuleset:build-ok` bypass is disabled and the vendored C++ compiles
//! only on CI. So this file (a) SOURCE-ANCHORS the tokens with a `fs::read_to_string` guard
//! runnable via `rustc --test` (revert protection against a `git subtree pull`), and (b) LIFTS the
//! pure `ndi_reap_should_defer` helper VERBATIM, compiles it with the C toolchain against a tiny
//! stub, and runs it over a hand-written truth table (the truth table IS the spec — nothing in the
//! Rust appliance consumes it). The lift-compile FAILS LOUDLY if no C compiler is present.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const NDI_SOURCE: &str = "vendor/distroav/src/ndi-source.cpp";
const REAP_MARKER: &str = "genlock-reap:";

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

// ----------------------------------------------------------------------------------------------
// Facet A — source anchors (revert protection; a `git subtree pull` re-importing stock DistroAV
// would silently drop the whole patch and CI must fail loudly here).
// ----------------------------------------------------------------------------------------------

#[test]
fn reap_predicate_and_reaper_present() {
    let src = squish(&vendor_file(NDI_SOURCE));
    assert!(
        src.contains(
            "static inline bool ndi_reap_should_defer(bool have_framesync, bool have_receiver, bool have_ndilib)"
        ),
        "{NDI_SOURCE}: #1320 patch missing — the pure `ndi_reap_should_defer(...)` predicate is \
         gone. Without it the exit-path teardown is not gated for the detached reaper. A `git \
         subtree pull` likely reverted it."
    );
    assert!(
        src.contains("static void ndi_reap_receiver_detached("),
        "{NDI_SOURCE}: #1320 patch missing — the `ndi_reap_receiver_detached(...)` reaper is gone, \
         so the blocking NDIlib_recv_destroy runs inline again and can freeze the render thread."
    );
    // The reaper must actually spawn a DETACHED thread (not just call destroy synchronously).
    assert!(
        src.contains("std::thread(") && src.contains(".detach();"),
        "{NDI_SOURCE}: #1320 patch weakened — the reaper no longer spawns a detached std::thread, \
         so the teardown is back on the caller (the graphics thread's pthread_join)."
    );
    // Defensive fallback so a thread-spawn failure never crashes/leaks.
    assert!(
        src.contains("recv_destroy") && src.contains("catch (...)"),
        "{NDI_SOURCE}: #1320 patch weakened — the reaper's synchronous fallback (catch (...)) is \
         gone; a std::thread spawn failure must fall back to a synchronous destroy, never throw."
    );
}

#[test]
fn exit_path_routes_teardown_through_the_reaper() {
    let src = squish(&vendor_file(NDI_SOURCE));
    // The av-thread exit path must hand its captured handles to the reaper.
    assert!(
        src.contains("ndi_reap_receiver_detached(ndiLib, reap_frame_sync, reap_receiver);"),
        "{NDI_SOURCE}: #1320 patch missing — the receiver-loop exit path no longer calls \
         ndi_reap_receiver_detached(ndiLib, reap_frame_sync, reap_receiver), so the blocking \
         recv_destroy runs on the av-thread's exit and the graphics-thread pthread_join stalls."
    );
    // The exit path must capture the handles BEFORE nulling the locals (so the reaper owns them).
    assert!(
        src.contains("NDIlib_recv_instance_t reap_receiver = ndi_receiver;"),
        "{NDI_SOURCE}: #1320 patch missing — the exit path must snapshot ndi_receiver into \
         reap_receiver before nulling it, so the reaper owns the handle."
    );
    // The distinctive report-only marker (one per teardown handoff).
    assert!(
        src.contains("genlock-reap: #1320 detached receiver teardown"),
        "{NDI_SOURCE}: #1320 patch missing — the `genlock-reap: #1320 ...` observability marker is \
         gone (the dev1 watchdog + program_render_lagged facet rely on the render never freezing)."
    );
}

#[test]
fn reap_marker_is_mutually_non_substring_vs_existing_families() {
    // A NEW audit/observability marker MUST keep the mutually-non-substring property so each
    // parser family runs over one log independently (program-render-audit.md).
    let families = [
        "genlock-fifo audit",
        "multiview-audit:",
        "program-render-audit:",
        "genlock-relock",
        "genlock-ndi-output",
        "recv-timing #797",
        "genlock: NDI receiver keep-alive",
    ];
    for fam in families {
        assert!(
            !fam.contains(REAP_MARKER) && !REAP_MARKER.contains(fam),
            "#1320: the new marker `{REAP_MARKER}` collides (substring) with the existing family \
             `{fam}` — pick a mutually-non-substring token."
        );
    }
    // And it must actually be present in the source.
    let src = vendor_file(NDI_SOURCE);
    assert!(
        src.contains(REAP_MARKER),
        "{NDI_SOURCE}: #1320 — the `{REAP_MARKER}` marker is not present in the source."
    );
}

// ----------------------------------------------------------------------------------------------
// Facet B — lift the pure predicate VERBATIM, compile it under the strict C flags, run its truth
// table. Proves the SHIPPED bytes COMPUTE (not just say) the reap-eligibility spec.
// ----------------------------------------------------------------------------------------------

/// Lift `ndi_reap_should_defer` VERBATIM from the vendored C (never retype it).
fn lift_reap_predicate() -> String {
    let src = vendor_file(NDI_SOURCE);
    let start = src
        .find("static inline bool ndi_reap_should_defer(")
        .unwrap_or_else(|| {
            panic!(
                "#1320: {NDI_SOURCE} no longer defines ndi_reap_should_defer — there is nothing to \
                 compile/behaviour-check."
            )
        });
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .expect("#1320: ndi_reap_should_defer has no closing brace `\\n}\\n`");
    src[start..end].to_string()
}

/// `(have_framesync, have_receiver, have_ndilib, expected)`. The full 8-row truth table:
/// reap iff the NDI lib is present AND at least one handle exists to destroy.
fn vectors() -> Vec<(bool, bool, bool, bool)> {
    vec![
        (false, false, false, false),
        (false, false, true, false), // lib present but nothing to destroy
        (false, true, false, false), // receiver but no lib -> cannot destroy
        (false, true, true, true),   // receiver + lib -> reap
        (true, false, false, false), // framesync but no lib
        (true, false, true, true),   // framesync + lib -> reap
        (true, true, false, false),  // both handles but no lib
        (true, true, true, true),    // both handles + lib -> reap
    ]
}

#[test]
fn reap_predicate_computes_the_spec_truth_table() {
    let helper = lift_reap_predicate();
    let vs = vectors();

    let mut c = String::from("#include <stdbool.h>\n#include <stdio.h>\n");
    c.push_str(&helper);
    c.push_str("\nint main(void){\n");
    for (fs_h, recv_h, lib_h, _) in &vs {
        c.push_str(&format!(
            "    printf(\"%d\\n\", ndi_reap_should_defer({}, {}, {}) ? 1 : 0);\n",
            *fs_h as u8, *recv_h as u8, *lib_h as u8
        ));
    }
    c.push_str("    return 0;\n}\n");

    let dir = std::env::temp_dir().join("distroav_scene_switch_reinit_1320");
    fs::create_dir_all(&dir).expect("create the scratch dir");
    let cfile = dir.join("reap.c");
    let bin = dir.join("reap.bin");
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
                "#1320: could not run the C compiler `{cc}` ({e}). This gate compiles the vendored \
                 ndi_reap_should_defer to prove the C both COMPILES and computes the spec; it must \
                 FAIL rather than skip when the toolchain is absent. Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "#1320: ndi_reap_should_defer lifted from {NDI_SOURCE} does NOT COMPILE standalone under \
         -Wall -Wextra -Wformat=2 -Wconversion -Werror — very likely a real compile error heading \
         for CI:\n--- cc stderr ---\n{}\n--- harness ---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );

    let run = Command::new(&bin)
        .output()
        .expect("#1320: the compiled harness failed to execute");
    assert!(run.status.success(), "#1320: the harness exited non-zero");
    let stdout = String::from_utf8(run.stdout).expect("harness stdout is utf-8");
    let got: Vec<bool> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim() == "1")
        .collect();
    assert_eq!(
        got.len(),
        vs.len(),
        "#1320: harness printed {} of {} rows",
        got.len(),
        vs.len()
    );

    let mut diffs = Vec::new();
    for ((fs_h, recv_h, lib_h, want), g) in vs.iter().zip(&got) {
        if g != want {
            diffs.push(format!(
                "  ndi_reap_should_defer(fs={fs_h}, recv={recv_h}, lib={lib_h}) -> C {g}, expected {want}"
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "#1320: reap-eligibility truth table mismatch:\n{}",
        diffs.join("\n")
    );
}
