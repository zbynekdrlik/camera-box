//! issue 1346 -- the DRM-lease HDMI output (issue 1152) gets a selectable VIEW: Program or the
//! BUILT-IN frontend Multiview (owner ROZHODNUTE 24.9.2026, design comment 5810067589).
//!
//! Shape pinned here:
//! - libobs: a small view API in `obs-drm-output.h` (`obs_drm_output_set_view_renderer` /
//!   `obs_drm_output_get_view` / `obs_drm_output_set_view`), implemented in a NEW Linux-only TU
//!   `obs-drm-output-view.c` (the already-over-budget `obs-drm-output.c` only gains the call).
//!   With `"view":"multiview"` the frame hook renders the frontend's registered Multiview INTO
//!   the leased scanout buffer, budget-gated by the existing `obs_aux_sender_should_skip`
//!   (issue 879 over the 278/293/756/776 gate) so the Program always has priority.
//! - frontend: a NEW Linux-only `components/DrmOutputView.cpp` owns ONE built-in `Multiview`
//!   (never a custom scene), refreshed from `OBSProjector::UpdateMultiviewProjectors`, cleared
//!   with the projectors, plus the operator's in-OBS Tools-menu switch.
//! - Windows byte-identical: nothing here is listed in a Windows/cross-platform CMake file.
//!
//! Same verification model as the issue-1152 siblings (`.claude/rules/obs-drm-output.md`,
//! `.claude/rules/vendored-libobs-change-safety.md`): std-only source anchors (Facet A) + a
//! VERBATIM lift-compile of the pure helpers under `-Werror -Wconversion` over truth tables
//! (Facet B). The view grammar table is SHARED with the Python mirror
//! (`tests/fixtures/drm_output_view_parity.tsv`). Runs offline:
//! `CARGO_MANIFEST_DIR=<abs> rustc --test --edition 2021 tests/drm_output_view_1346.rs -o /tmp/t && /tmp/t`.
//! Fails loudly (never skips) when no C compiler is present.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const DRM_H: &str = "vendor/obs-studio/libobs/obs-drm-output.h";
const DRM_C: &str = "vendor/obs-studio/libobs/obs-drm-output.c";
const VIEW_C: &str = "vendor/obs-studio/libobs/obs-drm-output-view.c";
const INTERNAL_H: &str = "vendor/obs-studio/libobs/obs-drm-output-internal.h";
const LIBOBS_LINUX_CMAKE: &str = "vendor/obs-studio/libobs/cmake/os-linux.cmake";
const FE_VIEW_CPP: &str = "vendor/obs-studio/frontend/components/DrmOutputView.cpp";
const FE_VIEW_HPP: &str = "vendor/obs-studio/frontend/components/DrmOutputView.hpp";
const FE_LINUX_CMAKE: &str = "vendor/obs-studio/frontend/cmake/os-linux.cmake";
const FE_COMPONENTS_CMAKE: &str = "vendor/obs-studio/frontend/cmake/ui-components.cmake";
const FE_WINDOWS_CMAKE: &str = "vendor/obs-studio/frontend/cmake/os-windows.cmake";
const FE_PROJECTOR: &str = "vendor/obs-studio/frontend/widgets/OBSProjector.cpp";
const FE_BASIC: &str = "vendor/obs-studio/frontend/widgets/OBSBasic.cpp";
const FE_COLLECTIONS: &str = "vendor/obs-studio/frontend/widgets/OBSBasic_SceneCollections.cpp";
const PARITY_TSV: &str = "tests/fixtures/drm_output_view_parity.tsv";

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

/// Assert `needle` sits inside an `#if defined(__linux__)` block of `src` (no `#endif` between
/// the nearest preceding guard and the needle).
fn assert_linux_guarded(src: &str, needle: &str, what: &str) {
    let at = src
        .find(needle)
        .unwrap_or_else(|| panic!("issue 1346: {what}: `{needle}` not found"));
    let guard = src[..at]
        .rfind("#if defined(__linux__)")
        .unwrap_or_else(|| {
            panic!("issue 1346: {what}: `{needle}` is not under #if defined(__linux__)")
        });
    assert!(
        !src[guard..at].contains("#endif"),
        "issue 1346: {what}: `{needle}` must sit INSIDE the #if defined(__linux__) block \
         (Windows stays byte-identical)"
    );
}

// ----------------------------------------------------------------------------------------------
// Facet A -- libobs view API + the new TU
// ----------------------------------------------------------------------------------------------

#[test]
fn header_declares_the_view_api() {
    let h = squish(&file(DRM_H));
    for token in [
        "enum obs_drm_output_view {",
        "OBS_DRM_OUTPUT_VIEW_PROGRAM = 0,",
        "OBS_DRM_OUTPUT_VIEW_MULTIVIEW = 1,",
        "typedef void (*obs_drm_output_view_render_t)(void *param, uint32_t cx, uint32_t cy);",
        "EXPORT void obs_drm_output_set_view_renderer(obs_drm_output_view_render_t render, void *param);",
        "EXPORT enum obs_drm_output_view obs_drm_output_get_view(void);",
        "EXPORT bool obs_drm_output_set_view(enum obs_drm_output_view view);",
    ] {
        assert!(h.contains(token), "issue 1346: {DRM_H} must declare `{token}`");
    }
}

#[test]
fn view_tu_is_linux_only_and_built_by_libobs_linux_cmake() {
    let v = file(VIEW_C);
    assert!(
        v.contains("#if defined(__linux__)")
            && v.trim_end().ends_with("#endif /* defined(__linux__) */"),
        "issue 1346: {VIEW_C} must be wholly under #if defined(__linux__) (like obs-drm-output.c)"
    );
    let cmake = squish(&file(LIBOBS_LINUX_CMAKE));
    assert!(
        cmake.contains("obs-drm-output-view.c") && cmake.contains("obs-drm-output-internal.h"),
        "issue 1346: {LIBOBS_LINUX_CMAKE} must add obs-drm-output-view.c + obs-drm-output-internal.h \
         to libobs (linux-genlock.yml is ENABLE_PLUGINS=OFF -- the module lives in libobs)"
    );
    let i = squish(&file(INTERNAL_H));
    for token in [
        "int drm_output_claim_render_buf(void);",
        "void drm_output_publish_render_buf(int idx);",
        "int drm_output_view_frame(void);",
        "void drm_output_view_configure(const char *view, const char *config_path);",
    ] {
        assert!(
            i.contains(token),
            "issue 1346: {INTERNAL_H} must declare `{token}`"
        );
    }
}

#[test]
fn multiview_render_is_budget_gated_audited_and_persisted() {
    let v = squish(&file(VIEW_C));
    for (token, why) in [
        (
            "static int drm_output_parse_view(const char *s)",
            "the pure view grammar (lifted below)",
        ),
        (
            "static int drm_output_view_tick_action(int view, bool have_renderer, bool skip)",
            "the pure per-tick decision (lifted below)",
        ),
        (
            "obs_aux_sender_should_skip_excluding(",
            "the EXISTING never-degrade-Program budget gate (issue 879), in its self-excluding form \
             (main design 5840501628: the view's own previous render is not counted twice)",
        ),
        (
            "const uint64_t self_last_ns = g_view.last_render_ns; g_view.last_render_ns = 0;",
            "the view hands its previous render cost over ONCE (0 after a skip or a missed call), so \
             a render is never subtracted from a tick that did not contain it",
        ),
        (
            "g_view.last_render_ns = dt;",
            "a measured render records its cost for the next tick's gate",
        ),
        (
            "self_ns=%llu",
            "the multiview-render line reports the self-exclusion the gate used",
        ),
        (
            "drm-output: multiview-render",
            "the periodic render-cost line (compare vs program-render-audit)",
        ),
        (
            "drm-output: multiview bind LIVE",
            "the one-shot proof the Multiview reached the scanout buffer",
        ),
        (
            "MULTIVIEW_AUDIT_WINDOW_NS",
            "the same ~5 s window as the multiview-audit family",
        ),
        (
            "os_quick_write_utf8_file_safe(",
            "the persisted choice survives an OBS restart",
        ),
        (
            "obs_data_set_string(data, \"view\"",
            "persist ONLY the view key, every other key kept",
        ),
        (
            "obs_data_get_json(data)",
            "one-line COMPACT json (the drm-output.json one-line contract)",
        ),
        (
            "gs_texrender_create(GS_BGRA, GS_ZS_NONE)",
            "the Multiview renders into an sRGB-capable texrender (review: the linear dma-buf \
             scanout target would drop the sRGB encode the built-in Multiview relies on)",
        ),
        (
            "drm_output_blit_raw(gs_texrender_get_texture(",
            "then the SAME raw byte-faithful blit the Program path uses fills the scanout buffer",
        ),
        (
            "drm_output_publish_render_buf(idx);",
            "the shared mailbox publish (no duplicated lock code)",
        ),
    ] {
        assert!(
            v.contains(token),
            "issue 1346: {VIEW_C} must contain `{token}` -- {why}"
        );
    }
    // The renderer setter must serialise with the graphics-thread hook (the callback runs while
    // the hook holds the graphics context), so a frontend teardown never races a render.
    let setter = v
        .find("void obs_drm_output_set_view_renderer(")
        .expect("issue 1346: setter definition missing");
    let body = &v[setter..];
    let enter = body
        .find("obs_enter_graphics();")
        .expect("setter must obs_enter_graphics()");
    let leave = body
        .find("obs_leave_graphics();")
        .expect("setter must obs_leave_graphics()");
    assert!(
        enter < leave,
        "issue 1346: the setter must take the graphics context around the swap"
    );
    // The multiview-render line must be mutually non-substring with the existing audit families.
    assert!(
        !v.contains("multiview-audit:"),
        "issue 1346: never emit a multiview-audit: line from the DRM output"
    );
    assert!(
        !v.contains("program-render-audit"),
        "issue 1346: never emit a program-render-audit line"
    );
    // The line's keys stay mutually non-substring, so a `key=` token scan reads each one alone
    // (main design 5840501628: `self_ns=` joins them).
    let line_at = v
        .find("drm-output: multiview-render ")
        .expect("issue 1346: the multiview-render format string");
    let line_end = v[line_at..].find(",").expect("format string end") + line_at;
    let keys: Vec<&str> = v[line_at..line_end]
        .split(' ')
        .filter_map(|tok| tok.split_once('=').map(|(k, _)| k))
        .map(|k| k.trim_start_matches('"'))
        .collect();
    assert!(
        keys.contains(&"self_ns") && keys.contains(&"rendered_fps"),
        "issue 1346: multiview-render keys {keys:?} must carry self_ns"
    );
    for a in &keys {
        for b in &keys {
            assert!(
                a == b || !format!("{b}=").contains(&format!("{a}=")),
                "issue 1346: multiview-render key `{a}=` is a substring of `{b}=` -- a token scan \
                 would misread it"
            );
        }
    }
}

#[test]
fn multiview_never_binds_the_linear_scanout_buffer_as_its_render_target() {
    let v = squish(&file(VIEW_C));
    assert!(
        !v.contains("gs_set_render_target("),
        "issue 1346 review: the Multiview must never render straight into the linear dma-buf \
         scanout texture (no sRGB encode -> a too-dark Program/scene cell); it renders into the \
         texrender and is blitted raw"
    );
    let c = squish(&file(DRM_C));
    assert!(
        c.contains("bool drm_output_blit_raw(gs_texture_t *src, int idx)"),
        "issue 1346: the raw blit must be ONE shared helper in {DRM_C}"
    );
    assert!(
        c.contains("drm_output_blit_raw(program, idx)"),
        "issue 1346: the Program path must use the same shared raw blit"
    );
    let i = squish(&file(INTERNAL_H));
    for token in [
        "bool drm_output_blit_raw(gs_texture_t *src, int idx);",
        "void drm_output_view_gl_teardown(void);",
    ] {
        assert!(
            i.contains(token),
            "issue 1346: {INTERNAL_H} must declare `{token}`"
        );
    }
    let stop = c
        .find("void obs_drm_output_stop(void)")
        .expect("stop() must exist");
    assert!(
        c[stop..].contains("drm_output_view_gl_teardown();"),
        "issue 1346: stop() must also free the Multiview texrender while graphics is alive"
    );
    for reason in [
        "no drm-output config path",
        "config unreadable",
        "write failed",
    ] {
        assert!(
            v.contains(reason),
            "issue 1346 review: a failed persist must name its real cause (`{reason}`)"
        );
    }
    let fe = file(FE_VIEW_CPP);
    assert!(
        fe.matches("if (!actionProgram || !actionMultiview)")
            .count()
            >= 2,
        "issue 1346 review: the Tools-menu actions can be null (frontend API not ready) -- \
         Init must check them before use, like UpdateMenu"
    );
}

/// Review round 2: the raw blit IS the sRGB fix -- pin its body (byte copy, no decode, no encode,
/// flush) -- and a claimed buffer is published ONLY when the blit really rendered it.
#[test]
fn raw_blit_is_byte_faithful_and_gates_the_publish() {
    let c = file(DRM_C);
    let start = c
        .find("bool drm_output_blit_raw(gs_texture_t *src, int idx)")
        .expect("issue 1346: drm_output_blit_raw must return bool (rendered or not)");
    let end = c[start..].find("\n}\n").map(|i| start + i).unwrap();
    let body = squish(&c[start..end]);
    for token in [
        "gs_effect_set_texture(param, src)",
        "gs_enable_framebuffer_srgb(false)",
        "gs_enable_blending(false)",
        "gs_flush();",
        "return true;",
    ] {
        assert!(
            body.contains(token),
            "issue 1346: the raw blit must contain `{token}`"
        );
    }
    assert!(
        !body.contains("set_texture_srgb"),
        "issue 1346: the raw blit must never sRGB-decode its source (the bytes are already encoded)"
    );
    let cs = squish(&c);
    assert!(
        cs.contains("if (drm_output_blit_raw(program, idx)) drm_output_publish_render_buf(idx);"),
        "issue 1346: the Program path publishes only a buffer the blit rendered"
    );
    let v = squish(&file(VIEW_C));
    assert!(
        v.contains("if (drm_output_blit_raw(gs_texrender_get_texture(g_view.texrender), idx))"),
        "issue 1346: the Multiview path publishes only a buffer the blit rendered"
    );
    assert!(
        v.contains("drm-output: multiview texrender unavailable"),
        "issue 1346 review: a texrender failure must be named once, never a silent frozen HDMI"
    );
    let stop = c.find("void obs_drm_output_stop(void)").unwrap();
    let s = &c[stop..];
    let join = s.find("pthread_join(th, NULL);").unwrap();
    let view_td = s.find("drm_output_view_gl_teardown();").unwrap();
    let drm_td = s.find("drm_output_teardown_locked();").unwrap();
    assert!(
        join < view_td && view_td < drm_td,
        "issue 1346: the texrender is freed after the flip thread joined and before the DRM teardown"
    );
}

#[test]
fn frame_hook_delegates_the_view_before_the_program_copy() {
    let c = squish(&file(DRM_C));
    let hook = c
        .find("void obs_drm_output_on_frame(void)")
        .expect("issue 1346: the frame hook must still exist");
    let body = &c[hook..];
    let view = body.find("drm_output_view_frame()").expect(
        "issue 1346: the frame hook must ask drm_output_view_frame() for this tick's action",
    );
    let program = body
        .find("obs_get_main_texture(")
        .expect("issue 1346: the Program copy (issue 1152 M2) must stay");
    assert!(
        view < program,
        "issue 1346: the view decision must run BEFORE the Program copy, so a multiview tick never \
         also pays the Program copy"
    );
    assert!(
        c.contains("drm_output_view_configure(view, path)"),
        "issue 1346: the autostart must hand the config's \"view\" value + path to the view TU"
    );
    assert!(
        c.contains("int drm_output_claim_render_buf(void)")
            && c.contains("void drm_output_publish_render_buf(int idx)"),
        "issue 1346: the mailbox claim/publish must be ONE shared pair used by both views"
    );
}

// ----------------------------------------------------------------------------------------------
// Facet A -- frontend: the BUILT-IN Multiview, Linux-only, three guarded touch points
// ----------------------------------------------------------------------------------------------

#[test]
fn frontend_component_renders_the_builtin_multiview() {
    let cpp = squish(&file(FE_VIEW_CPP));
    for (token, why) in [
        (
            "new Multiview()",
            "the stock built-in Multiview class, never a custom scene",
        ),
        (
            "->Update(",
            "configured from the same BasicWindow Multiview* settings the projectors use",
        ),
        (
            "->Render(cx, cy)",
            "rendered at the leased connector size handed in by libobs",
        ),
        (
            "obs_drm_output_set_view_renderer(",
            "registers with the libobs view API",
        ),
        (
            "obs_drm_output_set_view(",
            "the operator switch persists through libobs",
        ),
        (
            "obs_frontend_add_tools_menu_qaction(",
            "the in-OBS operator switch (Tools menu)",
        ),
        (
            "obs_frontend_add_event_callback(",
            "follows scene-collection load / finished-loading",
        ),
        (
            "MultiviewDrawNames",
            "labels exactly as the operator's projector Multiview",
        ),
    ] {
        assert!(
            cpp.contains(token),
            "issue 1346: {FE_VIEW_CPP} must contain `{token}` -- {why}"
        );
    }
    assert!(
        cpp.contains("#if defined(__linux__)"),
        "issue 1346: {FE_VIEW_CPP} must be Linux-only"
    );
    let hpp = squish(&file(FE_VIEW_HPP));
    for f in [
        "void DrmOutputViewInit();",
        "void DrmOutputViewRefresh();",
        "void DrmOutputViewClear();",
    ] {
        assert!(
            hpp.contains(f),
            "issue 1346: {FE_VIEW_HPP} must declare `{f}`"
        );
    }
}

#[test]
fn frontend_component_is_built_on_linux_only() {
    assert!(
        squish(&file(FE_LINUX_CMAKE)).contains("components/DrmOutputView.cpp"),
        "issue 1346: {FE_LINUX_CMAKE} must build components/DrmOutputView.cpp"
    );
    for cross in [FE_COMPONENTS_CMAKE, FE_WINDOWS_CMAKE] {
        assert!(
            !file(cross).contains("DrmOutputView"),
            "issue 1346: {cross} must NOT list DrmOutputView (Windows stays byte-identical)"
        );
    }
}

#[test]
fn frontend_touch_points_are_linux_guarded() {
    let proj = file(FE_PROJECTOR);
    assert_linux_guarded(&proj, "DrmOutputViewRefresh();", FE_PROJECTOR);
    let upd = proj
        .find("void OBSProjector::UpdateMultiviewProjectors()")
        .expect("UpdateMultiviewProjectors must exist");
    let call = proj.find("DrmOutputViewRefresh();").unwrap();
    let next_fn = proj[upd + 10..]
        .find("\nvoid OBSProjector::")
        .map(|i| upd + 10 + i)
        .unwrap();
    assert!(
        upd < call && call < next_fn,
        "issue 1346: the DRM Multiview refresh must live in UpdateMultiviewProjectors (the ONE \
         refresh point every scene/setting change already calls)"
    );

    let basic = file(FE_BASIC);
    assert_linux_guarded(&basic, "DrmOutputViewInit();", FE_BASIC);

    let coll = file(FE_COLLECTIONS);
    assert_linux_guarded(&coll, "DrmOutputViewClear();", FE_COLLECTIONS);
    let cp = coll
        .find("ClearProjectors();")
        .expect("ClearSceneData must call ClearProjectors()");
    let cl = coll.find("DrmOutputViewClear();").unwrap();
    let rm = coll
        .find("obs_set_output_source(i, nullptr);")
        .expect("ClearSceneData output-source reset missing");
    assert!(
        cp < cl && cl < rm,
        "issue 1346: the DRM Multiview must be cleared with the projectors, BEFORE the sources go"
    );
}

// ----------------------------------------------------------------------------------------------
// Facet B -- lift-compile the pure helpers and run them over the truth tables
// ----------------------------------------------------------------------------------------------

fn lift(sig: &str) -> String {
    let src = file(VIEW_C);
    let start = src.find(sig).unwrap_or_else(|| {
        panic!("issue 1346: {VIEW_C} no longer defines `{sig}` -- nothing to lift")
    });
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .unwrap_or_else(|| panic!("issue 1346: `{sig}` has no closing `\\n}}\\n`"));
    src[start..end].to_string()
}

fn compile_and_run(c_source: &str, tag: &str) -> String {
    let dir = std::env::temp_dir().join(format!("drm_view_1346_{}_{}", tag, std::process::id()));
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
            "program" => 0,
            "multiview" => 1,
            "unknown" => -1,
            other => panic!("issue 1346: bad expected token `{other}` in {PARITY_TSV}"),
        };
        rows.push((v, e));
    }
    assert!(rows.len() >= 8, "issue 1346: {PARITY_TSV} lost its rows");
    rows
}

#[test]
fn parse_view_computes_the_shared_grammar_table() {
    let helper = lift("static int drm_output_parse_view(const char *s)");
    let rows = parity_rows();
    let mut c = String::from("#include <stddef.h>\n#include <stdio.h>\n#include <string.h>\n");
    c.push_str(&helper);
    c.push_str("\nint main(void){\n");
    for (v, _) in &rows {
        match v {
            None => c.push_str("    printf(\"%d\\n\", drm_output_parse_view(NULL));\n"),
            Some(s) => c.push_str(&format!(
                "    printf(\"%d\\n\", drm_output_parse_view(\"{s}\"));\n"
            )),
        }
    }
    c.push_str("    return 0;\n}\n");
    let got: Vec<i32> = compile_and_run(&c, "parse")
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
        diffs.is_empty(),
        "issue 1346: drm_output_parse_view DIVERGED from {PARITY_TSV}:\n{}",
        diffs.join("\n")
    );
}

/// `(view, have_renderer, skip)` -> 0 NOTHING (keep the last frame), 1 PROGRAM copy,
/// 2 MULTIVIEW render. The Program path is NEVER budget-skipped; the Multiview renders only with
/// a registered renderer on a non-skipped tick; an unknown view fails OPEN to the Program.
fn tick_vectors() -> Vec<((i32, bool, bool), i32)> {
    vec![
        ((0, false, false), 1),
        ((0, true, false), 1),
        ((0, true, true), 1),
        ((0, false, true), 1),
        ((1, false, false), 0),
        ((1, false, true), 0),
        ((1, true, true), 0),
        ((1, true, false), 2),
        ((-1, true, false), 1),
        ((7, true, false), 1),
        ((2, false, false), 1),
    ]
}

#[test]
fn tick_action_computes_the_view_truth_table() {
    let helper =
        lift("static int drm_output_view_tick_action(int view, bool have_renderer, bool skip)");
    let vs = tick_vectors();
    let mut c = String::from("#include <stdbool.h>\n#include <stdio.h>\n");
    c.push_str(&helper);
    c.push_str("\nint main(void){\n");
    for ((view, r, s), _) in &vs {
        c.push_str(&format!(
            "    printf(\"%d\\n\", drm_output_view_tick_action({view}, {r}, {s}));\n"
        ));
    }
    c.push_str("    return 0;\n}\n");
    let got: Vec<i32> = compile_and_run(&c, "tick")
        .lines()
        .map(|l| l.trim().parse().expect("int"))
        .collect();
    let diffs: Vec<String> = vs
        .iter()
        .zip(&got)
        .filter(|((_, want), g)| *g != want)
        .map(|((a, want), g)| format!("  (view,renderer,skip)={a:?} -> C {g}, expected {want}"))
        .collect();
    assert!(
        diffs.is_empty(),
        "issue 1346: drm_output_view_tick_action DIVERGED from the spec:\n{}",
        diffs.join("\n")
    );
}

/// CI linux-genlock strih build (24.9.2026): OBS builds the frontend with context-less connects
/// disabled, so `QObject::connect(sender, &Signal, lambda)` (3 args) has no overload and the
/// DrmOutputView TU failed to compile. Every functor connect there must pass a context object.
#[test]
fn drm_output_view_menu_connects_pass_a_context_object() {
    let src = file("vendor/obs-studio/frontend/components/DrmOutputView.cpp");
    for line in src.lines().filter(|l| l.contains("QObject::connect(")) {
        let args = line.matches(',').count();
        assert!(
            args >= 3,
            "context-less 3-argument QObject::connect does not compile in the OBS frontend: {line}"
        );
    }
}
