//! issue 1424 (owner 13.10.2026: "a vies ty potom na ten vestibul hdmi dat nejakych 25fps alebo
//! mensie rozlisenie ? lebo ten sdi kabel to nezvlada") -- the vestibule chain is HDMI -> a Blackmagic
//! HDMI-to-SDI converter -> a long SDI cable that does not carry 1080p60 (3G-SDI). The main's ruling:
//! never 25 fps (the program is 30 fps; 25 would drop/repeat frames). The vk-direct mode picker
//! (`drm_output_vk_pick_mode`) prefers, for the PROGRAM view, the CANVAS size at the CANVAS frame rate
//! (1920x1080 @ 30 Hz, 29.97 accepted when 30 is absent: one render tick = one scanout, the HD-SDI
//! 1.5G rate), else today's rule (native size, refresh closest to 60 Hz). The Multiview view keeps
//! today's rule, so the BenQ does not change.
//!
//! The canvas exists only after the frontend's video reset (after the obs_startup autostart), so the
//! output opens with today's rule and the graphics thread hands the wanted mode to the present thread,
//! which re-modes through the existing surface rebuild when the pick changes (never a size change).
//!
//! Facet A: std-only source anchors. Facet B: the picker lifted VERBATIM from
//! obs-drm-output-vk-setup.c, compiled under -Werror -Wconversion over a truth table. Runs offline:
//! `CARGO_MANIFEST_DIR=<abs> rustc --test --edition 2021 tests/drm_output_vk_program_mode_1424.rs`.
//! Fails loudly (never skips) without a C compiler.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const VK_SETUP_C: &str = "vendor/obs-studio/libobs/obs-drm-output-vk-setup.c";
const VK_C: &str = "vendor/obs-studio/libobs/obs-drm-output-vk.c";
const VK_INTERNAL_H: &str = "vendor/obs-studio/libobs/obs-drm-output-vk-internal.h";
const VK_H: &str = "vendor/obs-studio/libobs/obs-drm-output-vk.h";
const BACKEND_C: &str = "vendor/obs-studio/libobs/obs-drm-output-backend.c";

fn file(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("issue 1424: cannot read {}: {e}", p.display()))
}

fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The body of the function whose definition starts with `sig` (up to the first `\n}\n`).
fn body_of(src: &str, sig: &str) -> String {
    let start = src.find(sig).unwrap_or_else(|| panic!("issue 1424: `{sig}` not found"));
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .unwrap_or_else(|| panic!("issue 1424: `{sig}` has no closing brace line"));
    src[start..end].to_string()
}

// ------------------------------------------------------------------------------------------------
// Facet A -- the wiring
// ------------------------------------------------------------------------------------------------

#[test]
fn the_surface_picks_with_the_wanted_program_mode() {
    let c = squish(&file(VK_SETUP_C));
    assert!(
        c.contains("drm_output_vk_pick_mode(mw, mh, mr, (int)nm, native.width, native.height, want_w, want_h, want_mhz)"),
        "issue 1424: the surface's pick passes the wanted program mode"
    );
    let h = squish(&file(VK_INTERNAL_H));
    for f in ["volatile long want_w;", "volatile long want_h;", "volatile long want_mhz;", "volatile bool remode;"] {
        assert!(h.contains(f), "issue 1424: the vk state carries `{f}`");
    }
    // the hand-over is backend-facing (obs-drm-output-backend.c includes only the vk header)
    let vk_h = squish(&file(VK_H));
    assert!(vk_h.contains("void drm_output_vk_want_mode(uint32_t w, uint32_t h, uint32_t mhz);"));
    assert!(h.contains("bool drm_output_vk_remode_if_needed(void);"));
}

#[test]
fn the_graphics_thread_wants_the_canvas_only_for_the_program_view() {
    let b = file(BACKEND_C);
    let hook = squish(&body_of(&b, "static bool vkd_on_frame(void)"));
    assert!(hook.contains("vkd_want_program_mode();"), "issue 1424: every tick hands the wanted mode over");
    let want = squish(&body_of(&b, "static void vkd_want_program_mode(void)"));
    assert!(want.contains("obs_drm_output_get_view() == OBS_DRM_OUTPUT_VIEW_PROGRAM"));
    assert!(want.contains("obs_get_video_info(&ovi)"), "issue 1424: the canvas from the live video info");
    assert!(want.contains("ovi.base_width") && want.contains("ovi.base_height"), "issue 1424: the CANVAS size");
    assert!(want.contains("drm_output_vk_want_mode("));
}

#[test]
fn the_present_thread_remodes_through_the_surface_rebuild_never_a_size_change() {
    let v = file(VK_C);
    let loop_ = squish(&body_of(&v, "static void *drm_output_vk_present_thread(void *arg)"));
    let remode = loop_.find("drm_output_vk_remode_if_needed()").expect("issue 1424: the loop re-modes");
    let acquire = loop_.find("vkAcquireNextImageKHR").expect("acquire");
    assert!(remode < acquire, "issue 1424: a re-mode runs at the top of a vblank, before the acquire");
    let s = file(VK_SETUP_C);
    let r = squish(&body_of(&s, "bool drm_output_vk_remode_if_needed(void)"));
    assert!(r.contains("drm_output_vk_rebuild_presentation(true)"), "issue 1424: the existing surface rebuild");
    assert!(r.contains("g_drm_vk.mode_w") && r.contains("g_drm_vk.mode_h"), "issue 1424: the size is checked");
    assert!(r.contains("g_drm_vk.refresh_mhz"), "issue 1424: an unchanged pick rebuilds nothing");
}

// ------------------------------------------------------------------------------------------------
// Facet B -- the picker's truth table
// ------------------------------------------------------------------------------------------------

/// (modes (w, h, mHz), native (w, h), wanted (w, h, mHz), expected index).
type ModeVector = (Vec<(u32, u32, u32)>, (u32, u32), (u32, u32, u32), i32);

const HD30: (u32, u32, u32) = (1920, 1080, 30000);
const NONE: (u32, u32, u32) = (0, 0, 0);

fn vectors() -> Vec<ModeVector> {
    let snv = vec![(1920, 1080, 60000), (1920, 1080, 50000), (1920, 1080, 30000), (1920, 1080, 25000)];
    vec![
        // the owner's case: 60/50/30/25 offered, the Program canvas 1080p30 -> 30 (never 25, never 60)
        (snv.clone(), (1920, 1080), HD30, 2),
        // the Multiview view keeps today's rule: native size, closest to 60
        (snv.clone(), (1920, 1080), NONE, 0),
        // a 29.97 canvas takes 29.97, a 30 canvas takes 30.000 when both are offered
        (vec![(1920, 1080, 29970), (1920, 1080, 30000), (1920, 1080, 60000)], (1920, 1080), HD30, 1),
        (vec![(1920, 1080, 30000), (1920, 1080, 29970)], (1920, 1080), (1920, 1080, 29970), 1),
        // only 29.97 offered: accepted for a 30 canvas
        (vec![(1920, 1080, 60000), (1920, 1080, 29970)], (1920, 1080), HD30, 1),
        // no canvas-size mode at the canvas rate: today's rule
        (vec![(1920, 1080, 60000), (1920, 1080, 25000), (3840, 2160, 30000)], (1920, 1080), HD30, 0),
        (vec![(1920, 1080, 60000), (1920, 1080, 29900)], (1920, 1080), HD30, 0),
        // a 30 Hz mode at another size never wins over the canvas size rule's fallback
        (vec![(3840, 2160, 60000), (1920, 1080, 60000), (1280, 720, 30000)], (3840, 2160), HD30, 0),
        // the canvas size differs from native but is offered at the rate: the canvas mode
        (vec![(3840, 2160, 60000), (1920, 1080, 60000), (1920, 1080, 30000)], (3840, 2160), HD30, 2),
        // a half-wanted mode (no rate) is no preference
        (snv.clone(), (1920, 1080), (1920, 1080, 0), 0),
        // the Blackmagic HDMI-1 EDID base block (13.10.2026) lists 1920x1080 at 60 Hz (148.5 MHz DTD)
        // and an interlaced 74.25 MHz DTD a progressive direct-display mode list never carries; its
        // CTA extension block was not captured, so 30 Hz is not proven offered: nothing at 30 -> 60
        (vec![(1920, 1080, 60000)], (1920, 1080), HD30, 0),
        (vec![], (1920, 1080), HD30, -1),
    ]
}

#[test]
fn pick_mode_prefers_the_program_canvas_at_its_rate() {
    let helper = body_of(&file(VK_SETUP_C), "static int drm_output_vk_pick_mode(");
    let vs = vectors();
    let mut c = String::from("#include <stdbool.h>\n#include <stdint.h>\n#include <stdio.h>\n");
    c.push_str(&helper);
    c.push_str("\nint main(void){\n");
    for (i, (modes, (nw, nh), (ww, wh, wr), _)) in vs.iter().enumerate() {
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
            "  {{ const uint32_t w[{cap}] = {{{}}}; const uint32_t h[{cap}] = {{{}}}; const uint32_t r[{cap}] = {{{}}};\n    printf(\"%d\\n\", drm_output_vk_pick_mode(w, h, r, {n}, {nw}u, {nh}u, {ww}u, {wh}u, {wr}u)); }}\n",
            list(&|m| m.0),
            list(&|m| m.1),
            list(&|m| m.2),
        ));
    }
    c.push_str("  return 0;\n}\n");

    let dir = std::env::temp_dir().join(format!("drm_vk_mode_1424_{}", std::process::id()));
    fs::create_dir_all(&dir).expect("scratch dir");
    let src = dir.join("h.c");
    let bin = dir.join("h.bin");
    fs::write(&src, &c).expect("write harness");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args(["-std=gnu99", "-Wall", "-Wextra", "-Wformat=2", "-Wconversion", "-Werror", "-O1"])
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| panic!("issue 1424: cannot run `{cc}` ({e}); this gate must FAIL, never skip"));
    assert!(
        out.status.success(),
        "issue 1424: the lifted picker does not compile:\n{}\n---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin).output().expect("run harness");
    let _ = fs::remove_dir_all(&dir);
    assert!(run.status.success(), "issue 1424: harness exited non-zero");
    let got: Vec<i32> = String::from_utf8_lossy(&run.stdout)
        .lines()
        .map(|l| l.trim().parse().expect("int"))
        .collect();
    let diffs: Vec<String> = vs
        .iter()
        .zip(&got)
        .filter(|((_, _, _, want), g)| *g != want)
        .map(|((m, n, w, want), g)| format!("  modes={m:?} native={n:?} want={w:?} -> C {g}, expected {want}"))
        .collect();
    assert!(diffs.is_empty(), "issue 1424: drm_output_vk_pick_mode DIVERGED:\n{}", diffs.join("\n"));
}
