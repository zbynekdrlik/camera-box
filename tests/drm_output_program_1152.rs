//! #1152 M2 — Program texture → dma-buf → zero-copy KMS page-flip guard
//! (`vendor/obs-studio/libobs/obs-drm-output.c` + the obs-video.c graphics-thread hook).
//!
//! M1 proved the mechanism (lease + solid-color vblank-locked flip, live-verified on imag-nb).
//! M2 binds the OBS PROGRAM to the leased connector: GBM scanout buffer objects
//! (`GBM_BO_USE_SCANOUT` — scanout-compatible modifier BY CONSTRUCTION) are imported into the
//! OBS GL context through the UPSTREAM dma-buf import (`gs_texture_create_from_dmabuf`, which
//! returns a GS_RENDER_TARGET texture), the graphics thread raw-copies the Program into the
//! back buffer right after `output_frames` completes, and the M1 flip thread page-flips the
//! mailbox's latest-ready buffer (`drmModeAddFB2WithModifiers` FBs on the lease fd). The M1
//! solid dumb-buffer path stays as the initial image + fail-open fallback.
//!
//! Same verification model as the M1 sibling (`tests/drm_output_lease_1152.rs`, per
//! `.claude/rules/vendored-libobs-change-safety.md`): std-only source anchors runnable via
//! `rustc --test` (revert protection), plus a VERBATIM lift-compile of the pure decision
//! helpers under `-Werror -Wconversion` over hand-written truth tables (the helpers have no
//! Rust consumer, so the truth table IS the spec). Fails loudly when no C compiler is present.
//!
//! No pwsh mirror in windows-genlock*.yml: pure Linux/EGL/DRM path (strih+stream are
//! libobs-d3d11); the module compiles only via libobs/cmake/os-linux.cmake on linux-genlock.yml.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const OBS_DRM_OUTPUT_C: &str = "vendor/obs-studio/libobs/obs-drm-output.c";
const OBS_DRM_OUTPUT_H: &str = "vendor/obs-studio/libobs/obs-drm-output.h";
const OBS_VIDEO_C: &str = "vendor/obs-studio/libobs/obs-video.c";
const OS_LINUX_CMAKE: &str = "vendor/obs-studio/libobs/cmake/os-linux.cmake";
const LINUX_GENLOCK_YML: &str = ".github/workflows/linux-genlock.yml";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn vendor_file(rel: &str) -> String {
    let p = repo(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Collapse every run of ASCII whitespace to a single space so the anchors survive reformatting.
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ----------------------------------------------------------------------------------------------
// Facet A — source anchors: the M2 binding must be present, in the owner-mandated shape.
// ----------------------------------------------------------------------------------------------

#[test]
fn drm_output_allocates_scanout_buffers_via_gbm() {
    let src = squish(&vendor_file(OBS_DRM_OUTPUT_C));

    assert!(
        src.contains("gbm_create_device("),
        "#1152 M2: {OBS_DRM_OUTPUT_C} must create a GBM device on the LEASED drm fd — GBM is \
         the standard Mesa allocator for scanout-capable buffers (kmscube/wlroots pattern)."
    );
    assert!(
        src.contains("gbm_bo_create("),
        "#1152 M2: {OBS_DRM_OUTPUT_C} must allocate the Program scanout buffers via \
         gbm_bo_create() — dumb buffers are CPU-only and can never carry a GPU render."
    );
    assert!(
        src.contains("GBM_BO_USE_SCANOUT"),
        "#1152 M2: {OBS_DRM_OUTPUT_C} must request GBM_BO_USE_SCANOUT so the allocated \
         modifier is scanout-compatible BY CONSTRUCTION (the reason the EGL-export alternative \
         was rejected — a render-chosen CCS modifier can be unscannable)."
    );
    assert!(
        src.contains("drmModeAddFB2WithModifiers("),
        "#1152 M2: {OBS_DRM_OUTPUT_C} must register the GBM buffers with \
         drmModeAddFB2WithModifiers() — the modifier-aware FB registration on the lease fd."
    );
}

#[test]
fn drm_output_binds_the_program_via_upstream_dmabuf_import() {
    let src = squish(&vendor_file(OBS_DRM_OUTPUT_C));

    assert!(
        src.contains("gs_texture_create_from_dmabuf("),
        "#1152 M2: {OBS_DRM_OUTPUT_C} must import the GBM buffers through the UPSTREAM \
         gs_texture_create_from_dmabuf() (EGL_EXT_image_dma_buf_import; returns a \
         GS_RENDER_TARGET texture) — zero new graphics vtable exports."
    );
    assert!(
        src.contains("obs_get_main_texture("),
        "#1152 M2: {OBS_DRM_OUTPUT_C} must source the frame from obs_get_main_texture() — \
         the Program canvas texture, copied raw (SDR byte-faithful) into the scanout buffer."
    );
    assert!(
        src.contains("gs_flush("),
        "#1152 M2: {OBS_DRM_OUTPUT_C} must gs_flush() after rendering into the scanout \
         buffer — the submit that hands the BO's implicit fence to the kernel page-flip."
    );
    assert!(
        src.contains("drm_output_fill_solid("),
        "#1152 M2: {OBS_DRM_OUTPUT_C} must KEEP the M1 solid fill — the initial image before \
         the first Program frame and the fail-open fallback when the GL bind fails."
    );
    assert!(
        src.contains("obs_data_get_bool(data, \"program\")"),
        "#1152 M2: {OBS_DRM_OUTPUT_C} autostart must honour an optional \"program\" config \
         key (default true; false = the M1 solid diagnostic pattern)."
    );
    assert!(
        src.contains("static int drm_output_pick_render_buf("),
        "#1152 M2: {OBS_DRM_OUTPUT_C} must define the pure mailbox helper \
         `static int drm_output_pick_render_buf(` (Tier-0 truth-tabled below)."
    );
    assert!(
        src.contains("static void drm_output_fit_rect("),
        "#1152 M2: {OBS_DRM_OUTPUT_C} must define the pure aspect-fit helper \
         `static void drm_output_fit_rect(` (Tier-0 truth-tabled below)."
    );
}

#[test]
fn drm_output_public_surface_declares_the_frame_hook() {
    let hdr = squish(&vendor_file(OBS_DRM_OUTPUT_H));
    assert!(
        hdr.contains("obs_drm_output_on_frame("),
        "#1152 M2: {OBS_DRM_OUTPUT_H} must declare obs_drm_output_on_frame() — the \
         graphics-thread entry that renders the Program into the mailbox back buffer."
    );
}

#[test]
fn graphics_thread_loop_calls_the_frame_hook_after_output_frames() {
    let obs = squish(&vendor_file(OBS_VIDEO_C));

    assert!(
        obs.contains("#include \"obs-drm-output.h\""),
        "#1152 M2: {OBS_VIDEO_C} must include obs-drm-output.h to reach the frame hook."
    );

    let call = obs.find("obs_drm_output_on_frame();").unwrap_or_else(|| {
        panic!(
            "#1152 M2: {OBS_VIDEO_C} obs_graphics_thread_loop must call \
             obs_drm_output_on_frame() once per tick — the Program scanout hook."
        )
    });

    // Ordering: the hook runs AFTER the Program is composited (output_frames) and BEFORE the
    // displays render — minimum render->scanout latency, ahead of the monitoring-display cost.
    let of = obs
        .find("output_frames();")
        .expect("#1152 M2: obs_graphics_thread_loop no longer calls output_frames()?");
    let rd = obs
        .find("render_displays();")
        .expect("#1152 M2: obs_graphics_thread_loop no longer calls render_displays()?");
    assert!(
        of < call && call < rd,
        "#1152 M2: {OBS_VIDEO_C} the frame hook must sit between the output_frames call and \
         the render_displays call (found positions of={of} call={call} rd={rd})."
    );

    // The call MUST be Linux-guarded (module is Linux-only; obs-video.c compiles everywhere).
    let guard = obs[..call]
        .rfind("#if defined(__linux__)")
        .unwrap_or_else(|| {
            panic!(
                "#1152 M2: {OBS_VIDEO_C} the frame hook must be guarded by #if defined(__linux__)."
            )
        });
    assert!(
        !obs[guard..call].contains("#endif"),
        "#1152 M2: {OBS_VIDEO_C} the frame hook call must be INSIDE the #if defined(__linux__) \
         block (no #endif between the guard and the call)."
    );
}

#[test]
fn build_system_carries_the_gbm_dependency() {
    let cmake = squish(&vendor_file(OS_LINUX_CMAKE));
    assert!(
        cmake.contains("pkg_check_modules(Gbm REQUIRED IMPORTED_TARGET gbm)"),
        "#1152 M2: {OS_LINUX_CMAKE} must find GBM (pkg_check_modules IMPORTED_TARGET gbm) for \
         the scanout-buffer allocation."
    );
    assert!(
        cmake.contains("PkgConfig::Gbm"),
        "#1152 M2: {OS_LINUX_CMAKE} must link PkgConfig::Gbm into libobs."
    );

    let yml = squish(&vendor_file(LINUX_GENLOCK_YML));
    assert!(
        yml.contains("libgbm-dev"),
        "#1152 M2: {LINUX_GENLOCK_YML} OBS_APT_PACKAGES must install libgbm-dev — \
         linux-genlock.yml is the module's FIRST compiler and needs the GBM headers."
    );
}

// ----------------------------------------------------------------------------------------------
// Facet B — lift-compile the pure helpers and run them over truth tables.
// ----------------------------------------------------------------------------------------------

/// Lift a `static` helper VERBATIM from the vendored C by its definition signature (never
/// retype it — a retyped copy verifies your typing, not the shipped bytes).
fn lift_helper(sig: &str) -> String {
    let src = vendor_file(OBS_DRM_OUTPUT_C);
    let start = src.find(sig).unwrap_or_else(|| {
        panic!("#1152 M2: {OBS_DRM_OUTPUT_C} no longer defines `{sig}` — nothing to lift.")
    });
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .unwrap_or_else(|| panic!("#1152 M2: `{sig}` has no closing brace `\\n}}\\n`"));
    src[start..end].to_string()
}

fn compile_and_run(c_source: &str, tag: &str) -> String {
    let dir =
        std::env::temp_dir().join(format!("drm_output_m2_1152_{}_{}", tag, std::process::id()));
    fs::create_dir_all(&dir).expect("create the scratch dir");
    let cfile = dir.join("harness.c");
    let bin = dir.join("harness.bin");
    fs::write(&cfile, c_source).expect("write the harness");

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
                "#1152 M2: could not run the C compiler `{cc}` ({e}). This gate compiles the \
                 vendored helpers to prove the shipped bytes COMPILE and COMPUTE the spec; it \
                 must FAIL rather than skip when the toolchain is absent. Install cc or set CC."
            )
        });
    assert!(
        out.status.success(),
        "#1152 M2: the lifted helper does NOT COMPILE standalone under -Wall -Wextra \
         -Wformat=2 -Wconversion -Werror (very likely a real compile error heading for CI):\n\
         --- cc stderr ---\n{}\n--- harness ---\n{c_source}",
        String::from_utf8_lossy(&out.stderr)
    );

    let run = Command::new(&bin)
        .output()
        .expect("#1152 M2: the compiled harness failed to execute");
    assert!(
        run.status.success(),
        "#1152 M2: the harness exited non-zero: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    String::from_utf8(run.stdout).expect("harness stdout is utf-8")
}

/// `(front, pending, ready, n)` → expected render-buffer index (-1 = nothing writable).
/// The mailbox contract: NEVER return front (on scanout) or pending (flip queued); prefer a
/// role-free buffer; else overwrite ready (latest-wins mailbox); -1 only when nothing is
/// writable. Vectors vary EVERY parameter so a helper hardcoding any answer fails a row.
fn pick_vectors() -> Vec<((i32, i32, i32, i32), i32)> {
    vec![
        ((0, -1, -1, 3), 1),  // skip front -> first free
        ((0, 1, -1, 3), 2),   // skip front AND pending
        ((1, 0, -1, 3), 2),   // roles swapped -> both still excluded independently
        ((0, 1, 2, 3), 2),    // all roles taken -> overwrite ready (latest-wins)
        ((0, -1, 1, 3), 2),   // a free buffer exists -> prefer it over overwriting ready
        ((2, -1, -1, 3), 0),  // front elsewhere -> index 0 is usable
        ((-1, -1, -1, 3), 0), // pre-arm (no roles yet) -> first buffer
        ((0, 1, -1, 2), -1),  // n=2, no free, no ready -> nothing writable
        ((0, -1, 1, 2), 1),   // n=2, no free -> overwrite ready
        ((0, 1, 2, 0), -1),   // n=0 degenerate -> nothing
        ((0, 2, 1, 4), 3),    // n=4 -> the free index past every role
        ((0, 1, 0, 2), -1),   // ready ALIASES front -> never hand back the on-scanout buffer
        ((0, 1, 1, 2), -1),   // ready ALIASES pending -> never hand back the flip-queued buffer
    ]
}

#[test]
fn pick_render_buf_computes_the_mailbox_truth_table() {
    let helper = lift_helper("static int drm_output_pick_render_buf(");
    let vs = pick_vectors();

    let mut c = String::from("#include <stdint.h>\n#include <stdio.h>\n");
    c.push_str(&helper);
    c.push_str("\nint main(void){\n");
    for ((front, pending, ready, n), _) in &vs {
        c.push_str(&format!(
            "    printf(\"%d\\n\", drm_output_pick_render_buf({front}, {pending}, {ready}, {n}));\n"
        ));
    }
    c.push_str("    return 0;\n}\n");

    let stdout = compile_and_run(&c, "pick");
    let got: Vec<i32> = stdout
        .lines()
        .map(|l| {
            l.trim()
                .parse::<i32>()
                .expect("harness printed a non-integer")
        })
        .collect();
    assert_eq!(got.len(), vs.len(), "#1152 M2: result count mismatch");

    let mut diffs = Vec::new();
    for ((args, want), g) in vs.iter().zip(&got) {
        if g != want {
            diffs.push(format!(
                "  (front,pending,ready,n)={args:?} -> C {g}, expected {want}"
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "#1152 M2: drm_output_pick_render_buf DIVERGED from the mailbox spec on {} of {} \
         vectors:\n{}",
        diffs.len(),
        vs.len(),
        diffs.join("\n")
    );
}

/// `(src_w, src_h, dst_w, dst_h)` → expected `(x, y, w, h)` aspect-fit rect (letterbox
/// centred; any zero input fails open to the full destination).
type FitQuad = (u32, u32, u32, u32);

fn fit_vectors() -> Vec<(FitQuad, FitQuad)> {
    vec![
        ((1920, 1080, 1920, 1080), (0, 0, 1920, 1080)), // 1:1 (the rig case)
        ((1280, 720, 1920, 1080), (0, 0, 1920, 1080)),  // same aspect, upscale
        ((1920, 1080, 1920, 1200), (0, 60, 1920, 1080)), // taller dst -> letterbox
        ((1440, 1080, 1920, 1080), (240, 0, 1440, 1080)), // 4:3 into 16:9 -> pillarbox
        ((1920, 1080, 1280, 1024), (0, 152, 1280, 720)), // downscale + letterbox
        ((1080, 1920, 1920, 1080), (656, 0, 607, 1080)), // portrait src -> pillarbox
        ((0, 1080, 1920, 1080), (0, 0, 1920, 1080)),    // degenerate src -> full dst
        ((1920, 1080, 0, 0), (0, 0, 0, 0)),             // degenerate dst -> full (empty) dst
    ]
}

#[test]
fn fit_rect_computes_the_aspect_fit_truth_table() {
    let helper = lift_helper("static void drm_output_fit_rect(");
    let vs = fit_vectors();

    let mut c = String::from("#include <stdint.h>\n#include <stdio.h>\n");
    c.push_str(&helper);
    c.push_str("\nint main(void){\n    uint32_t x, y, w, h;\n");
    for ((sw, sh, dw, dh), _) in &vs {
        c.push_str(&format!(
            "    drm_output_fit_rect({sw}u, {sh}u, {dw}u, {dh}u, &x, &y, &w, &h);\n    \
             printf(\"%u %u %u %u\\n\", x, y, w, h);\n"
        ));
    }
    c.push_str("    return 0;\n}\n");

    let stdout = compile_and_run(&c, "fit");
    let got: Vec<Vec<u32>> = stdout
        .lines()
        .map(|l| {
            l.split_whitespace()
                .map(|t| t.parse::<u32>().expect("harness printed a non-integer"))
                .collect()
        })
        .collect();
    assert_eq!(got.len(), vs.len(), "#1152 M2: result count mismatch");

    let mut diffs = Vec::new();
    for ((args, want), g) in vs.iter().zip(&got) {
        let w = [want.0, want.1, want.2, want.3];
        if g != &w {
            diffs.push(format!(
                "  (sw,sh,dw,dh)={args:?} -> C {g:?}, expected {w:?}"
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "#1152 M2: drm_output_fit_rect DIVERGED from the aspect-fit spec on {} of {} \
         vectors:\n{}",
        diffs.len(),
        vs.len(),
        diffs.join("\n")
    );
}
