//! issue 1346 -- the strih-lx BUILT-IN HDMI (NVIDIA, X output HDMI-0) as a fixed, indestructible
//! output like imag's: a second backend of the vendored DRM-output module, NVIDIA Vulkan direct
//! display (`VK_EXT_acquire_xlib_display` + `VK_KHR_display`), selected by `"backend":"vk-direct"`
//! in `~/.camera-box/drm-output.json` (owner rulings 5838578002 + 5838662632, main design
//! 5838663570; STEP 0 + the GPU interop proven live on strih-lx, 5838745730 + 5839007372).
//!
//! Shape pinned here:
//! - libobs: `enum obs_drm_output_backend` + a `backend` field in the config; the lease backend
//!   (`obs-drm-output.c`, issue 1152) keeps its code and becomes the default path; the vk-direct
//!   backend is reached through `struct drm_output_backend_ops` at the module's entry points and
//!   its mailbox seam, so the view renderer, the budget gate, the Tools-menu switch and the
//!   `drm-output:` log family stay shared.
//! - three new Linux-only TUs: `obs-drm-output-backend.c` (the backend grammar + the OBS side),
//!   `obs-drm-output-vk.c` (present thread, lifecycle, GL side), `obs-drm-output-vk-setup.c`
//!   (Vulkan/X setup + teardown); Vulkan HEADERS only -- `libvulkan.so.1` is dlopen'd.
//! - CI: `libvulkan-dev` in linux-genlock.yml `OBS_APT_PACKAGES`, and `find_package(Vulkan REQUIRED)`
//!   with `Vulkan::Headers` in libobs os-linux.cmake.
//!
//! Same verification model as the issue-1152/1346 siblings (`.claude/rules/obs-drm-output.md`):
//! std-only source anchors (Facet A) + VERBATIM lifts of the pure helpers compiled under
//! `-Werror -Wconversion` over truth tables (Facet B). The backend grammar table is SHARED with the
//! Python mirror (`tests/fixtures/drm_output_backend_parity.tsv`). The GPU path itself is proven on
//! the rig by `tests/c/drm_output_vk_rig_harness.c` (it cannot run in CI -- no NVIDIA display).
//! Runs offline: `CARGO_MANIFEST_DIR=<abs> rustc --test --edition 2021
//! tests/drm_output_vk_direct_1346.rs -o /tmp/t && /tmp/t`. Fails loudly when no C compiler exists.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const DRM_H: &str = "vendor/obs-studio/libobs/obs-drm-output.h";
const DRM_C: &str = "vendor/obs-studio/libobs/obs-drm-output.c";
const INTERNAL_H: &str = "vendor/obs-studio/libobs/obs-drm-output-internal.h";
const BACKEND_C: &str = "vendor/obs-studio/libobs/obs-drm-output-backend.c";
const VK_H: &str = "vendor/obs-studio/libobs/obs-drm-output-vk.h";
const VK_INTERNAL_H: &str = "vendor/obs-studio/libobs/obs-drm-output-vk-internal.h";
const VK_C: &str = "vendor/obs-studio/libobs/obs-drm-output-vk.c";
const VK_SETUP_C: &str = "vendor/obs-studio/libobs/obs-drm-output-vk-setup.c";
const LIBOBS_LINUX_CMAKE: &str = "vendor/obs-studio/libobs/cmake/os-linux.cmake";
const LINUX_GENLOCK_YML: &str = ".github/workflows/linux-genlock.yml";
const VERIFY_STRIH: &str = "scripts/verify-strih.sh";
const RIG_HARNESS: &str = "tests/c/drm_output_vk_rig_harness.c";
const PARITY_TSV: &str = "tests/fixtures/drm_output_backend_parity.tsv";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn file(rel: &str) -> String {
    let p = repo(rel);
    fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("issue 1346: cannot read {}: {e}", p.display()))
}

/// Collapse every run of ASCII whitespace to one space so anchors survive reformatting.
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The body of the function whose definition starts with `sig` (up to the first `\n}\n`).
fn body_of(src: &str, sig: &str, what: &str) -> String {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("issue 1346: {what}: `{sig}` not found"));
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .unwrap_or_else(|| panic!("issue 1346: {what}: `{sig}` has no closing `\\n}}\\n`"));
    src[start..end].to_string()
}

fn wholly_linux(rel: &str) {
    let s = file(rel);
    assert!(
        s.contains("#if defined(__linux__)") && s.trim_end().ends_with("#endif /* defined(__linux__) */"),
        "issue 1346: {rel} must be wholly under #if defined(__linux__) (Windows stays byte-identical)"
    );
}

// ----------------------------------------------------------------------------------------------
// Facet A -- the API, the build wiring, the backend routing
// ----------------------------------------------------------------------------------------------

#[test]
fn header_declares_the_backend_enum_and_config_field() {
    let h = squish(&file(DRM_H));
    for token in [
        "enum obs_drm_output_backend {",
        "OBS_DRM_OUTPUT_BACKEND_LEASE = 0,",
        "OBS_DRM_OUTPUT_BACKEND_VK_DIRECT = 1,",
        "enum obs_drm_output_backend backend;",
    ] {
        assert!(
            h.contains(token),
            "issue 1346: {DRM_H} must declare `{token}`"
        );
    }
    let i = squish(&file(INTERNAL_H));
    for token in [
        "struct drm_output_backend_ops {",
        "bool (*start)(const struct obs_drm_output_config *cfg);",
        "void (*stop)(void);",
        "bool (*on_frame)(void);",
        "bool (*owns)(void);",
        "int (*claim)(void);",
        "void (*publish)(int idx);",
        "gs_texture_t *(*texture)(int idx);",
        "void (*mode_size)(uint32_t *w, uint32_t *h);",
        "extern const struct drm_output_backend_ops drm_output_vk_direct_backend;",
        "int drm_output_backend_from_config(const char *value);",
    ] {
        assert!(
            i.contains(token),
            "issue 1346: {INTERNAL_H} must declare `{token}`"
        );
    }
}

#[test]
fn the_new_tus_are_linux_only_and_built_by_libobs_with_vulkan_headers_only() {
    for rel in [BACKEND_C, VK_C, VK_SETUP_C] {
        wholly_linux(rel);
    }
    let cmake = squish(&file(LIBOBS_LINUX_CMAKE));
    for token in [
        "obs-drm-output-backend.c",
        "obs-drm-output-vk-internal.h",
        "obs-drm-output-vk-setup.c",
        "obs-drm-output-vk.c",
        "obs-drm-output-vk.h",
        "find_package(Vulkan REQUIRED)",
        "Vulkan::Headers",
    ] {
        assert!(
            cmake.contains(token),
            "issue 1346: {LIBOBS_LINUX_CMAKE} must carry `{token}` (the module lives in libobs -- \
             linux-genlock.yml is ENABLE_PLUGINS=OFF)"
        );
    }
    assert!(
        !cmake.contains("Vulkan::Vulkan"),
        "issue 1346: libobs must NOT link the Vulkan loader -- the backend dlopens libvulkan.so.1, so \
         a box without it keeps the output dormant instead of failing to load libobs"
    );
    let yml = file(LINUX_GENLOCK_YML);
    assert!(
        yml.contains("libvulkan-dev"),
        "issue 1346: {LINUX_GENLOCK_YML} OBS_APT_PACKAGES must install libvulkan-dev (the headers + \
         the CMake finder's library for find_package(Vulkan))"
    );
    let vk = file(VK_INTERNAL_H);
    assert!(
        vk.contains("#define VK_NO_PROTOTYPES 1"),
        "issue 1346: no link-time Vulkan symbol may be referenced (VK_NO_PROTOTYPES)"
    );
    let setup = file(VK_SETUP_C);
    assert!(
        setup.contains("dlopen(\"libvulkan.so.1\""),
        "issue 1346: the loader is dlopen'd by its SONAME"
    );
}

#[test]
fn the_vk_core_takes_the_display_off_x_and_presents_fifo() {
    let setup = squish(&file(VK_SETUP_C));
    let hdr = squish(&file(VK_INTERNAL_H));
    for (token, why) in [
        (
            "\"VK_EXT_acquire_xlib_display\"",
            "the NVIDIA way to take a display away from X",
        ),
        (
            "VK_EXT_DIRECT_MODE_DISPLAY_EXTENSION_NAME",
            "direct-mode display",
        ),
        (
            "VK_KHR_DISPLAY_EXTENSION_NAME",
            "VK_KHR_display planes + modes",
        ),
    ] {
        assert!(
            setup.contains(token) || hdr.contains(token),
            "issue 1346: the vk core must use {token} -- {why}"
        );
    }
    for (token, why) in [
        (
            "g_drm_vk.vk.vkGetRandROutputDisplayEXT(",
            "the X RandR output -> VkDisplayKHR",
        ),
        ("g_drm_vk.vk.vkAcquireXlibDisplayEXT(", "the acquire itself"),
        (
            "vkCreateDisplayPlaneSurfaceKHR(",
            "a display-plane surface, never an X window",
        ),
        (
            ".presentMode = VK_PRESENT_MODE_FIFO_KHR,",
            "FIFO = vblank-locked, tear-free",
        ),
        (
            "g_drm_vk.vk.vkReleaseDisplayEXT(",
            "a clean release on stop",
        ),
        (
            "VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT",
            "GL-shareable images",
        ),
        (
            "VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_OPAQUE_FD_BIT",
            "GL-shareable semaphores",
        ),
        (
            "VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO",
            "dedicated = GL_DEDICATED_MEMORY_OBJECT_EXT",
        ),
    ] {
        assert!(
            setup.contains(token),
            "issue 1346: {VK_SETUP_C} must contain `{token}` -- {why}"
        );
    }
    let core = squish(&file(VK_C));
    for (token, why) in [
        (
            "GL_DEDICATED_MEMORY_OBJECT_EXT",
            "the GL import must match the dedicated allocation",
        ),
        ("->ImportMemoryFdEXT(", "GL_EXT_memory_object_fd"),
        ("->ImportSemaphoreFdEXT(", "GL_EXT_semaphore_fd"),
        (
            "->CopyImageSubData(",
            "a byte copy touching no bound GL state (the libobs state cache)",
        ),
        ("->SignalSemaphoreEXT(", "GL -> Vulkan ordering"),
        (
            "->WaitSemaphoreEXT(",
            "an overwritten READY image's signal is consumed, never doubled",
        ),
        (
            "VK_QUEUE_FAMILY_EXTERNAL",
            "the ownership bounce with the GL side",
        ),
        (
            "vkCmdBlitImage(",
            "RGBA8 -> the swapchain BGRA8, component-wise",
        ),
        (
            "DRM_OUTPUT_VK_WAIT_NS",
            "every blocking Vulkan wait is bounded (stop never hangs)",
        ),
    ] {
        assert!(
            core.contains(token),
            "issue 1346: {VK_C} must contain `{token}` -- {why}"
        );
    }
    let destroy = body_of(
        &file(VK_SETUP_C),
        "void drm_output_vk_destroy_all(void)",
        VK_SETUP_C,
    );
    let wait = destroy
        .find("vkDeviceWaitIdle(")
        .expect("destroy waits idle first");
    let release = destroy
        .find("vkReleaseDisplayEXT(")
        .expect("destroy releases the display");
    let inst = destroy
        .find("vkDestroyInstance(")
        .expect("destroy frees the instance");
    assert!(
        wait < release && release < inst,
        "issue 1346: teardown order = wait idle -> release the display -> destroy the instance"
    );
}

#[test]
fn the_lease_module_routes_to_vk_direct_and_keeps_the_lease_default() {
    let c = file(DRM_C);
    let start = body_of(
        &c,
        "bool obs_drm_output_start(const struct obs_drm_output_config *cfg)",
        DRM_C,
    );
    let route = start
        .find("if (cfg->backend == OBS_DRM_OUTPUT_BACKEND_VK_DIRECT)")
        .expect("start() must route the vk-direct backend");
    let lease = start
        .find("pthread_mutex_lock(&g_drm.lock);")
        .expect("the lease start");
    assert!(
        route < lease,
        "issue 1346: the backend is chosen BEFORE any lease work"
    );
    assert!(start.contains("return drm_output_vk_direct_backend.start(cfg);"));

    let stop = body_of(&c, "void obs_drm_output_stop(void)", DRM_C);
    let vk_stop = stop
        .find("drm_output_vk_direct_backend.stop();")
        .expect("stop() stops vk-direct");
    let join = stop
        .find("pthread_join(th, NULL);")
        .expect("the lease join");
    assert!(
        vk_stop < join,
        "issue 1346: vk-direct is stopped before the lease path runs"
    );

    let frame = body_of(&c, "void obs_drm_output_on_frame(void)", DRM_C);
    let vk_frame = frame
        .find("if (drm_output_vk_direct_backend.on_frame())")
        .expect("the frame hook routes vk-direct");
    let lease_gate = frame
        .find("if (!os_atomic_load_bool(&g_drm.program_want))")
        .expect("the lease fast gate");
    assert!(
        vk_frame < lease_gate,
        "issue 1346: the vk-direct hook runs first and returns"
    );

    for (sig, call) in [
        (
            "int drm_output_claim_render_buf(void)",
            "return drm_output_vk_direct_backend.claim();",
        ),
        (
            "void drm_output_publish_render_buf(int idx)",
            "drm_output_vk_direct_backend.publish(idx);",
        ),
        (
            "gs_texture_t *drm_output_render_buf_texture(int idx)",
            "return drm_output_vk_direct_backend.texture(idx);",
        ),
        (
            "void drm_output_mode_size(uint32_t *w, uint32_t *h)",
            "drm_output_vk_direct_backend.mode_size(w, h);",
        ),
    ] {
        let b = body_of(&c, sig, DRM_C);
        assert!(
            b.contains("if (drm_output_vk_direct_backend.owns())") && b.contains(call),
            "issue 1346: the seam `{sig}` must route to vk-direct while it owns the output"
        );
    }
    let blit = body_of(
        &c,
        "bool drm_output_blit_raw(gs_texture_t *src, int idx)",
        DRM_C,
    );
    assert!(
        blit.contains("drm_output_mode_size(&dst_w, &dst_h);")
            && !blit.contains("g_drm.mode_w, g_drm.mode_h"),
        "issue 1346: the ONE raw blit fits into the OWNING backend's mode, not the lease mode"
    );

    let auto = body_of(&c, "void obs_drm_output_maybe_autostart(void)", DRM_C);
    for token in [
        "obs_data_get_string(data, \"backend\")",
        "drm_output_backend_from_config(backend_s)",
        "cfg.backend = (enum obs_drm_output_backend)backend;",
    ] {
        assert!(
            auto.contains(token),
            "issue 1346: autostart must contain `{token}`"
        );
    }
    let unknown = auto
        .find("if (backend < 0) {")
        .expect("an unknown backend is refused");
    let started = auto
        .find("(void)obs_drm_output_start(&cfg);")
        .expect("the start call");
    assert!(
        unknown < started && auto[unknown..started].contains("dormant"),
        "issue 1346: an unknown \"backend\" keeps the output DORMANT (never a guessed backend)"
    );
}

#[test]
fn the_obs_side_stops_in_order_and_fails_open() {
    let b = file(BACKEND_C);
    let stop = body_of(&b, "static void vkd_stop(void)", BACKEND_C);
    let order = [
        "drm_output_vk_halt();",
        "obs_enter_graphics();",
        "drm_output_vk_gl_unbind();",
        "gs_texture_destroy(g_vkd.mid);",
        "obs_leave_graphics();",
        "drm_output_view_gl_teardown();",
        "drm_output_vk_close();",
    ];
    let mut last = 0usize;
    for token in order {
        let at = stop[last..]
            .find(token)
            .map(|i| last + i)
            .unwrap_or_else(|| panic!("issue 1346: vkd_stop must run {order:?} in this order; `{token}` is missing or early"));
        last = at + token.len();
    }
    let frame = body_of(&b, "static bool vkd_on_frame(void)", BACKEND_C);
    for token in [
        "obs_enter_graphics();",
        "drm_output_view_frame() != DRM_OUTPUT_TICK_PROGRAM",
        "obs_get_main_texture()",
        "drm_output_claim_render_buf()",
        "drm_output_blit_raw(program, idx)",
        "drm_output_publish_render_buf(idx);",
        "os_atomic_set_bool(&g_vkd.program, false);",
    ] {
        assert!(
            frame.contains(token),
            "issue 1346: vkd_on_frame must contain `{token}`"
        );
    }
    assert!(
        b.contains("gs_texture_create(w, h, GS_BGRA, 1, NULL, GS_RENDER_TARGET)"),
        "issue 1346: the intermediate is a GS_BGRA render target at the display mode size (the \
         storage the rig harness proved byte-exact)"
    );
}

#[test]
fn the_log_family_is_shared_with_verify_strih() {
    let v = file(VERIFY_STRIH);
    let core = file(VK_C);
    let marker = "drm-output: program scanout LIVE";
    assert!(v.contains(marker), "verify-strih greps `{marker}`");
    assert!(
        core.contains(marker),
        "issue 1346: the vk-direct backend must emit `{marker}` so verify-strih's drm-output item \
         grades it exactly like the lease backend"
    );
    for (new, old) in [
        (
            "drm-output: program-present #",
            "drm-output: program-flip #",
        ),
        ("drm-output: solid-present #", "drm-output: page-flip #"),
    ] {
        assert!(core.contains(new), "issue 1346: {VK_C} must log `{new}`");
        assert!(
            !new.contains(old) && !old.contains(new),
            "issue 1346: `{new}` and `{old}` must be mutually non-substring (the jitter_audit rule)"
        );
    }
    let h = file(RIG_HARNESS);
    for token in [
        "drm_output_vk_open(",
        "drm_output_vk_gl_bind()",
        "drm_output_vk_publish_gl(",
        "drm_output_vk_close();",
    ] {
        assert!(
            h.contains(token),
            "issue 1346: the rig harness must drive the REAL core via `{token}`"
        );
    }
    assert!(file(VK_H).contains(
        "bool drm_output_vk_publish_gl(int idx, unsigned int src_gl_name, uint32_t w, uint32_t h);"
    ));
}

// ----------------------------------------------------------------------------------------------
// Facet B -- the pure helpers, lifted verbatim and compiled
// ----------------------------------------------------------------------------------------------

fn lift(rel: &str, sig: &str) -> String {
    body_of(&file(rel), sig, rel)
}

fn compile_and_run(c_source: &str, tag: &str) -> String {
    let dir = std::env::temp_dir().join(format!("drm_vk_1346_{}_{}", tag, std::process::id()));
    fs::create_dir_all(&dir).expect("scratch dir");
    let cfile = dir.join("h.c");
    let bin = dir.join("h.bin");
    fs::write(&cfile, c_source).expect("write harness");
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
            panic!("issue 1346: cannot run `{cc}` ({e}); this gate must FAIL, never skip")
        });
    assert!(
        out.status.success(),
        "issue 1346: the lifted helper does NOT compile under -Werror -Wconversion:\n{}\n---\n{c_source}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin).output().expect("run harness");
    assert!(run.status.success(), "issue 1346: harness exited non-zero");
    let _ = fs::remove_dir_all(&dir);
    String::from_utf8(run.stdout).expect("utf-8")
}

/// The shared grammar table: (C literal or NULL, expected C code 0/1/-1).
fn parity_rows() -> Vec<(Option<String>, i32)> {
    let mut rows = Vec::new();
    for line in file(PARITY_TSV).lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let (value, expected) = line
            .split_once('\t')
            .expect("TSV row must be <value>\\t<expected>");
        let v = match value {
            "<absent>" => None,
            "<empty>" => Some(String::new()),
            other => Some(other.to_string()),
        };
        let e = match expected {
            "lease" => 0,
            "vk-direct" => 1,
            "unknown" => -1,
            other => panic!("issue 1346: bad expected token `{other}` in {PARITY_TSV}"),
        };
        rows.push((v, e));
    }
    assert!(rows.len() >= 8, "issue 1346: {PARITY_TSV} lost its rows");
    rows
}

#[test]
fn parse_backend_computes_the_shared_grammar_table() {
    let helper = lift(
        BACKEND_C,
        "static int drm_output_parse_backend(const char *s)",
    );
    let rows = parity_rows();
    let mut c = String::from("#include <stddef.h>\n#include <stdio.h>\n#include <string.h>\n");
    c.push_str(&helper);
    c.push_str("\nint main(void){\n");
    for (v, _) in &rows {
        match v {
            None => c.push_str("    printf(\"%d\\n\", drm_output_parse_backend(NULL));\n"),
            Some(s) => c.push_str(&format!(
                "    printf(\"%d\\n\", drm_output_parse_backend(\"{s}\"));\n"
            )),
        }
    }
    c.push_str("    return 0;\n}\n");
    let got: Vec<i32> = compile_and_run(&c, "backend")
        .lines()
        .map(|l| l.trim().parse().expect("int"))
        .collect();
    let diffs: Vec<String> = rows
        .iter()
        .zip(&got)
        .filter(|((_, want), g)| *g != want)
        .map(|((v, want), g)| format!("  {v:?} -> C {g}, expected {want}"))
        .collect();
    assert!(
        diffs.is_empty() && got.len() == rows.len(),
        "issue 1346: drm_output_parse_backend DIVERGED from {PARITY_TSV}:\n{}",
        diffs.join("\n")
    );
}

/// `(modes [(w, h, refresh mHz)], native (w, h))` -> the chosen index. Native size first (the
/// physical resolution stands in for the connector's preferred mode), then the refresh closest to
/// 60 Hz, the first on a tie; all modes when the native size is unknown or unmatched; -1 when empty.
/// One mode-pick row: the modes `(w, h, refresh mHz)`, the native size, the expected index.
type ModeVector = (Vec<(u32, u32, u32)>, (u32, u32), i32);

fn mode_vectors() -> Vec<ModeVector> {
    vec![
        (
            vec![
                (1920, 1080, 119880),
                (1920, 1080, 60000),
                (1920, 1080, 59940),
                (1280, 720, 60000),
            ],
            (1920, 1080),
            1,
        ),
        (
            vec![
                (1920, 1080, 119880),
                (1920, 1080, 59940),
                (1280, 720, 60000),
            ],
            (1920, 1080),
            1,
        ),
        (
            vec![(3840, 2160, 30000), (1280, 720, 60000), (1920, 1080, 50000)],
            (0, 0),
            1,
        ),
        (
            vec![(1920, 1080, 60000), (1920, 1080, 50000)],
            (2560, 1440),
            0,
        ),
        (
            vec![(1920, 1080, 59000), (1920, 1080, 61000)],
            (1920, 1080),
            0,
        ),
        (
            vec![(3840, 2160, 30000), (1920, 1080, 60000)],
            (3840, 2160),
            0,
        ),
        (
            vec![
                (1920, 1080, 50000),
                (1920, 1080, 60000),
                (1920, 1080, 60000),
            ],
            (1920, 1080),
            1,
        ),
        (vec![], (1920, 1080), -1),
    ]
}

#[test]
fn pick_mode_computes_the_mode_truth_table() {
    let helper = lift(VK_SETUP_C, "static int drm_output_vk_pick_mode(");
    let vs = mode_vectors();
    let mut c = String::from("#include <stdbool.h>\n#include <stdint.h>\n#include <stdio.h>\n");
    c.push_str(&helper);
    c.push_str("\nint main(void){\n");
    for (i, (modes, (nw, nh), _)) in vs.iter().enumerate() {
        let n = modes.len();
        let cap = n.max(1);
        let list = |f: &dyn Fn(&(u32, u32, u32)) -> u32| {
            let mut v: Vec<String> = modes.iter().map(|m| format!("{}u", f(m))).collect();
            if v.is_empty() {
                v.push("0u".into());
            }
            v.join(", ")
        };
        c.push_str(&format!(
            "    {{ const uint32_t w{i}[{cap}] = {{{}}}; const uint32_t h{i}[{cap}] = {{{}}}; const uint32_t r{i}[{cap}] = {{{}}};\n      printf(\"%d\\n\", drm_output_vk_pick_mode(w{i}, h{i}, r{i}, {n}, {nw}u, {nh}u)); }}\n",
            list(&|m| m.0),
            list(&|m| m.1),
            list(&|m| m.2),
        ));
    }
    c.push_str("    return 0;\n}\n");
    let got: Vec<i32> = compile_and_run(&c, "mode")
        .lines()
        .map(|l| l.trim().parse().expect("int"))
        .collect();
    let diffs: Vec<String> = vs
        .iter()
        .zip(&got)
        .filter(|((_, _, want), g)| *g != want)
        .map(|((m, n, want), g)| format!("  modes={m:?} native={n:?} -> C {g}, expected {want}"))
        .collect();
    assert!(
        diffs.is_empty(),
        "issue 1346: drm_output_vk_pick_mode DIVERGED:\n{}",
        diffs.join("\n")
    );
}

/// `(front, pending, ready)` -> `(returned src, pending', ready', took_new)`.
/// One present-pick row: `(front, pending, ready, armed[ready])` and the expected
/// `(src, pending', ready', took_new, wait_gl, armed images left)`. Taking a READY image takes its GL
/// signal (waits it once, clears it); re-copying the FRONT waits nothing.
type PresentVector = ((i32, i32, i32, bool), (i32, i32, i32, i32, i32, i32));

fn present_vectors() -> Vec<PresentVector> {
    vec![
        ((-1, -1, -1, false), (-1, -1, -1, 0, 0, 0)),
        ((-1, -1, 2, true), (2, 2, -1, 1, 1, 0)),
        ((-1, -1, 2, false), (2, 2, -1, 1, 0, 0)),
        ((0, -1, 1, true), (1, 1, -1, 1, 1, 0)),
        ((0, -1, -1, false), (0, -1, -1, 0, 0, 0)),
        ((2, -1, -1, false), (2, -1, -1, 0, 0, 0)),
        ((1, -1, 0, true), (0, 0, -1, 1, 1, 0)),
    ]
}

#[test]
fn present_pick_takes_the_newest_with_its_signal_or_recopies_the_front() {
    let helper = lift(
        VK_C,
        "static int drm_output_vk_present_pick(int front, int *pending, int *ready, bool *armed, bool *took_new, bool *wait_gl)",
    );
    let vs = present_vectors();
    let mut c = String::from("#include <stdbool.h>\n#include <stdio.h>\n");
    c.push_str(&helper);
    c.push_str("\nint main(void){\n");
    for ((f, p, r, a), _) in &vs {
        c.push_str(&format!(
            "    {{ int p = {p}, r = {r}; bool a[3] = {{false, false, false}}; bool t = false, w = false;\n      if (r >= 0) a[r] = {a};\n      int s = drm_output_vk_present_pick({f}, &p, &r, a, &t, &w);\n      printf(\"%d %d %d %d %d %d\\n\", s, p, r, t ? 1 : 0, w ? 1 : 0, (a[0] ? 1 : 0) + (a[1] ? 1 : 0) + (a[2] ? 1 : 0)); }}\n"
        ));
    }
    c.push_str("    return 0;\n}\n");
    let out = compile_and_run(&c, "present");
    let got: Vec<(i32, i32, i32, i32, i32, i32)> = out
        .lines()
        .map(|l| {
            let v: Vec<i32> = l.split_whitespace().map(|x| x.parse().unwrap()).collect();
            (v[0], v[1], v[2], v[3], v[4], v[5])
        })
        .collect();
    for ((args, want), g) in vs.iter().zip(&got) {
        assert_eq!(g, want, "issue 1346: drm_output_vk_present_pick{args:?}");
    }
    assert_eq!(got.len(), vs.len());
}

/// The mailbox + binary-semaphore protocol, model-checked over 200000 random interleavings of the
/// GL side (claim + publish) and the present thread (pick, then the fenced done) with the REAL lifted
/// helpers. A binary semaphore must never be signalled while it holds a signal, never be waited without
/// one, a taken image with a pending signal must be waited, and GL must never write front/pending. The
/// interleaving forces the overwrite path (GL re-claims a READY image), so consumes > 0 is required.
#[test]
fn mailbox_semaphore_protocol_holds_over_random_interleavings() {
    let mut c = String::from("#include <stdbool.h>\n#include <stdio.h>\n");
    for sig in [
        "static int drm_output_vk_pick_claim(int front, int pending, int ready, int n)",
        "static int drm_output_vk_present_pick(int front, int *pending, int *ready, bool *armed, bool *took_new, bool *wait_gl)",
        "static void drm_output_vk_present_done(int src, bool took_new, int *front, int *pending)",
        "static bool drm_output_vk_publish_arm(bool *armed_idx)",
    ] {
        c.push_str(&lift(VK_C, sig));
        c.push('\n');
    }
    c.push_str(
        r#"int main(void)
{
	int front = -1, pending = -1, ready = -1, src = -1;
	bool armed[3] = {false, false, false}, inflight = false, took = false, wait = false;
	int sem[3] = {0, 0, 0};
	unsigned long long seed = 12345u, consumes = 0u, bad = 0u, takes = 0u;
	for (int step = 0; step < 200000; step++) {
		seed = seed * 6364136223846793005ull + 1442695040888963407ull;
		unsigned r = (unsigned)(seed >> 33) % 4u;
		if (r == 0u && !inflight) {
			src = drm_output_vk_present_pick(front, &pending, &ready, armed, &took, &wait);
			if (wait) {
				if (sem[src] != 1)
					bad++;
				sem[src] = 0;
			} else if (took && sem[src] != 0) {
				bad++;
			}
			if (took)
				takes++;
			inflight = true;
		} else if (r == 1u && inflight) {
			drm_output_vk_present_done(src, took, &front, &pending);
			inflight = false;
		} else {
			int idx = drm_output_vk_pick_claim(front, pending, ready, 3);
			if (idx < 0)
				continue;
			if (idx == front || idx == pending)
				bad++;
			if (idx == ready)
				ready = -1;
			if (drm_output_vk_publish_arm(&armed[idx])) {
				if (sem[idx] != 1)
					bad++;
				sem[idx] = 0;
				consumes++;
			}
			if (sem[idx] != 0)
				bad++;
			sem[idx] = 1;
			ready = idx;
		}
	}
	printf("bad %llu consumes %llu takes %llu\n", bad, consumes, takes);
	return 0;
}
"#,
    );
    let out = compile_and_run(&c, "model");
    let v: Vec<u64> = out
        .split_whitespace()
        .filter_map(|t| t.parse().ok())
        .collect();
    assert_eq!(v.len(), 3, "issue 1346: model output `{out}`");
    assert_eq!(
        v[0], 0,
        "issue 1346: the mailbox/semaphore protocol broke an invariant: {out}"
    );
    assert!(
        v[1] > 0 && v[2] > 0,
        "issue 1346: the model must exercise the overwrite (consume) path and the takes: {out}"
    );
}

/// `(offered formats)` -> the chosen index: B8G8R8A8_UNORM, else R8G8B8A8_UNORM, else -1 (never an
/// sRGB format -- the blit between two UNORM formats is the byte-faithful copy).
#[test]
fn surface_format_pick_never_takes_an_srgb_format() {
    let helper = lift(
        VK_SETUP_C,
        "static int drm_output_vk_pick_surface_format(const VkFormat *fmts, uint32_t n)",
    );
    let vs: &[(&[u32], i32)] = &[
        (&[50, 44], 1),
        (&[44, 37], 0),
        (&[43, 37], 1),
        (&[50, 43], -1),
        (&[64], -1),
        (&[], -1),
    ];
    let mut c = String::from(
        "#include <stdint.h>\n#include <stdio.h>\ntypedef enum { VK_FORMAT_R8G8B8A8_UNORM = 37, VK_FORMAT_R8G8B8A8_SRGB = 43, VK_FORMAT_B8G8R8A8_UNORM = 44, VK_FORMAT_B8G8R8A8_SRGB = 50, VK_FORMAT_A2B10G10R10_UNORM_PACK32 = 64 } VkFormat;\n",
    );
    c.push_str(&helper);
    c.push_str("\nint main(void){\n");
    for (i, (fmts, _)) in vs.iter().enumerate() {
        let mut list: Vec<String> = fmts.iter().map(|f| format!("(VkFormat){f}")).collect();
        if list.is_empty() {
            list.push("(VkFormat)0".into());
        }
        c.push_str(&format!(
            "    {{ const VkFormat f{i}[] = {{{}}}; printf(\"%d\\n\", drm_output_vk_pick_surface_format(f{i}, {}u)); }}\n",
            list.join(", "),
            fmts.len()
        ));
    }
    c.push_str("    return 0;\n}\n");
    let got: Vec<i32> = compile_and_run(&c, "fmt")
        .lines()
        .map(|l| l.trim().parse().expect("int"))
        .collect();
    let want: Vec<i32> = vs.iter().map(|(_, w)| *w).collect();
    assert_eq!(got, want, "issue 1346: drm_output_vk_pick_surface_format");
}

#[test]
fn a_lost_display_is_rebuilt_bounded_and_the_teardown_never_hangs() {
    let core = file(VK_C);
    let thread = body_of(
        &core,
        "static void *drm_output_vk_present_thread(void *arg)",
        VK_C,
    );
    assert_eq!(
        thread.matches("drm_output_vk_rebuild_or_give_up(").count(),
        2,
        "issue 1346: both the acquire and the present rebuild an out-of-date / lost display"
    );
    assert!(
        thread.contains("VK_ERROR_OUT_OF_DATE_KHR") && thread.contains("VK_ERROR_SURFACE_LOST_KHR"),
        "issue 1346: an HDMI replug (out of date / surface lost) is rebuilt, not the end of the output"
    );
    let fence = thread
        .find("if (!drm_output_vk_wait_fence())")
        .expect("the fence wait");
    let done = thread
        .find("drm_output_vk_present_done(")
        .expect("the role update");
    assert!(
        fence < done,
        "issue 1346: roles change only after a SIGNALLED fence (a failed wait breaks first)"
    );
    let policy = body_of(
        &core,
        "static bool drm_output_vk_rebuild_or_give_up(VkResult why, unsigned *rebuilds)",
        VK_C,
    );
    assert!(
        policy.contains("DRM_OUTPUT_VK_REBUILD_TRIES") && policy.contains("giving up"),
        "issue 1346: the rebuild is bounded and names its give-up"
    );
    let setup = file(VK_SETUP_C);
    assert!(
        setup.contains("bool drm_output_vk_rebuild_presentation(bool surface_lost)"),
        "issue 1346: the rebuild lives with the setup code"
    );
    let destroy = body_of(&setup, "void drm_output_vk_destroy_all(void)", VK_SETUP_C);
    let quiesce = destroy
        .find("g_drm_vk.submit_outstanding")
        .expect("the teardown quiesces an outstanding copy");
    let idle = destroy.find("vkDeviceWaitIdle(").expect("the idle wait");
    assert!(
        quiesce < idle && destroy.contains("leaking the Vulkan objects"),
        "issue 1346: an outstanding copy is waited BOUNDED before the idle wait; a wedged GPU leaks \
         instead of hanging the OBS shutdown"
    );
    let unbind = body_of(&core, "void drm_output_vk_gl_unbind(void)", VK_C);
    let finish = unbind
        .find("g->Finish();")
        .expect("glFinish before the deletes");
    let delete = unbind.find("g->DeleteTextures(").expect("the deletes");
    assert!(
        finish < delete,
        "issue 1346: in-flight GL copy/signal work finishes before the GL objects go"
    );
    assert!(
        core.contains("RTLD_NOLOAD"),
        "issue 1346: the GL lookup also asks an already (privately) loaded libEGL"
    );
    let h = file(RIG_HARNESS);
    assert!(
        h.contains("PHASE burst") && h.contains("consumes > 0"),
        "issue 1346: the rig harness forces the overwrite path and requires it ran"
    );
}

#[test]
fn vk_claim_rule_equals_the_lease_mailbox_rule() {
    let vk = lift(
        VK_C,
        "static int drm_output_vk_pick_claim(int front, int pending, int ready, int n)",
    );
    let lease = lift(
        DRM_C,
        "static int drm_output_pick_render_buf(int front, int pending, int ready, int n)",
    );
    let mut c = String::from("#include <stdio.h>\n");
    c.push_str(&vk);
    c.push('\n');
    c.push_str(&lease);
    c.push_str(
        "\nint main(void){\n    int bad = 0, n = 0;\n    for (int f = -1; f < 3; f++) for (int p = -1; p < 3; p++) for (int r = -1; r < 3; r++) {\n        n++;\n        if (drm_output_vk_pick_claim(f, p, r, 3) != drm_output_pick_render_buf(f, p, r, 3)) { printf(\"DIFF %d %d %d\\n\", f, p, r); bad++; }\n    }\n    printf(\"checked %d bad %d\\n\", n, bad);\n    return 0;\n}\n",
    );
    let out = compile_and_run(&c, "claim");
    assert!(
        out.contains("checked 64 bad 0"),
        "issue 1346: the vk-direct claim rule must equal the lease mailbox rule over every role \
         combination (a GL write never lands on front/pending):\n{out}"
    );
}
