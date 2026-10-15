//! issue 1424 part 2, the two LIVE vk-direct defects on strih-pp (13.10.2026, RTX 3060, HDMI-0 =
//! the cutter's Multiview, HDMI-1 = the vestibule Program through a Blackmagic HDMI-to-SDI):
//!
//! 1. The SECOND display never acquired (`vkAcquireXlibDisplayEXT FAILED VkResult -13`, whichever
//!    connector was second). Bisected on the rig with the real core: NOT the shared X connection (a
//!    connection per display fails the same way) -- the second acquire ran inside the window in which
//!    the first display's present thread brings its new presentation up (its first presents = the mode
//!    set on the other head). Open, open 0 us later = -13; 50 ms later = both acquired. So the acquire
//!    waits, bounded, until every other presenting display has presented
//!    `DRM_OUTPUT_VK_SETTLE_PRESENTS` times on its current surface, and a re-mode (a new surface = a
//!    new mode set) waits while another display is acquiring.
//! 2. HDMI-1 went dark after the Program re-mode to 29.97 Hz: the rebuild RETIRED the old display-plane
//!    surface and destroyed it 4 fences later, while the new surface on the same display + plane was
//!    already presenting -- the next present returned -13. A rebuild with a new surface now quiesces
//!    the display's queue (bounded) and destroys the old swapchain + surface BEFORE it creates the new
//!    surface; no surface is ever retired.
//!
//! Facet A: std-only source anchors. Facet B: the settle helper lifted VERBATIM from
//! obs-drm-output-vk-setup.c, compiled under -Werror -Wconversion over a truth table. Runs offline:
//! `CARGO_MANIFEST_DIR=<abs> rustc --test --edition 2021 tests/drm_output_vk_two_displays_1424.rs`.
//! Fails loudly (never skips) without a C compiler. The live proof is the rig harness
//! (`tests/c/drm_output_vk_rig_harness.c HDMI-0 HDMI-1=1920x1080@30000`).

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static SCRATCH: AtomicUsize = AtomicUsize::new(0);

const VK_SETUP_C: &str = "vendor/obs-studio/libobs/obs-drm-output-vk-setup.c";
const VK_C: &str = "vendor/obs-studio/libobs/obs-drm-output-vk.c";
const VK_INTERNAL_H: &str = "vendor/obs-studio/libobs/obs-drm-output-vk-internal.h";
/// The settle wait, the re-mode and the rebuild (split out of the setup TU, the 1000-line budget).
const VK_REMODE_C: &str = "vendor/obs-studio/libobs/obs-drm-output-vk-remode.c";
const HARNESS_C: &str = "tests/c/drm_output_vk_rig_harness.c";

fn file(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("issue 1424: cannot read {}: {e}", p.display()))
}

fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The body of the function whose definition starts with `sig` (up to the first `\n}\n`).
fn body_of(src: &str, sig: &str) -> String {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("issue 1424: `{sig}` not found"));
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .unwrap_or_else(|| panic!("issue 1424: `{sig}` has no closing brace line"));
    src[start..end].to_string()
}

fn pos(hay: &str, needle: &str, what: &str) -> usize {
    hay.find(needle)
        .unwrap_or_else(|| panic!("issue 1424: {what}: `{needle}` not found"))
}

// ------------------------------------------------------------------------------------------------
// Facet A -- defect 1: the acquire waits for the other display's presentation to settle
// ------------------------------------------------------------------------------------------------

#[test]
fn the_acquire_waits_for_every_other_presentation_to_settle() {
    let s = file(VK_SETUP_C);
    let setup = squish(&body_of(
        &s,
        "bool drm_output_vk_setup(struct drm_output_vk_disp *d)",
    ));
    let raise = pos(
        &setup,
        "os_atomic_set_bool(&dev->acquiring, true)",
        "setup raises the acquire flag",
    );
    let wait = pos(
        &setup,
        "drm_output_vk_wait_others_settled(d)",
        "setup waits for the other displays",
    );
    let acquire = pos(&setup, "drm_output_vk_acquire(d, output)", "setup acquires");
    let lower = pos(
        &setup,
        "os_atomic_set_bool(&dev->acquiring, false)",
        "setup lowers the acquire flag",
    );
    assert!(
        raise < wait && wait < acquire && acquire < lower,
        "issue 1424: raise the flag -> wait (the -13 window is the other display's bring-up) -> acquire \
         -> lower the flag (no other display may start a mode set during the wait or the acquire)"
    );
    let w = squish(&body_of(
        &file(VK_REMODE_C),
        "void drm_output_vk_wait_others_settled(struct drm_output_vk_disp *d)",
    ));
    assert!(
        w.contains("drm_output_vk_first_unsettled("),
        "issue 1424: the wait asks the pure settle helper"
    );
    assert!(
        w.contains("DRM_OUTPUT_VK_SETTLE_WAIT_MS"),
        "issue 1424: the wait is bounded"
    );
    assert!(
        w.contains("presents_since_surface") && w.contains("drm_output_vk_settle_need("),
        "issue 1424: settled = presents on the CURRENT surface, against the threshold its swapchain needs"
    );
    assert!(
        w.contains("need[i] = drm_output_vk_settle_need(os_atomic_load_long(&o->settle_images));"),
        "issue 1424 review: the threshold is fed the other display's CURRENT swapchain image count"
    );
    assert!(
        w.contains("os_atomic_load_bool(&o->want_frames)"),
        "issue 1424 review: a display whose present loop died on its own (frame gate down) never blocks"
    );
}

#[test]
fn a_new_surface_restarts_the_settle_count_and_each_present_advances_it() {
    let s = file(VK_SETUP_C);
    let cs = squish(&body_of(
        &s,
        "bool drm_output_vk_create_surface(struct drm_output_vk_disp *d, bool rebuild)",
    ));
    assert!(
        cs.contains("os_atomic_set_long(&d->presents_since_surface, 0)"),
        "issue 1424: a committed surface is a new mode set -- unsettled again"
    );
    let v = file(VK_C);
    let loop_ = squish(&body_of(
        &v,
        "static void *drm_output_vk_present_thread(void *arg)",
    ));
    let ok = pos(
        &loop_,
        "rebuilds = 0; d->presents++;",
        "a successful present",
    );
    let inc = pos(
        &loop_,
        "os_atomic_inc_long(&d->presents_since_surface)",
        "the settle count advances",
    );
    assert!(inc > ok, "issue 1424: only a successful present counts");
    let h = file(VK_INTERNAL_H);
    assert!(
        h.contains("volatile long presents_since_surface;"),
        "issue 1424: the settle count is an os_atomic long in the display"
    );
    assert!(
        h.contains("volatile bool acquiring;"),
        "issue 1424: the device carries the acquire flag"
    );
    assert!(
        h.contains("volatile long settle_images;")
            && s.contains("os_atomic_set_long(&d->settle_images, (long)n);"),
        "issue 1424 review: the settle threshold follows the current swapchain's image count"
    );
}

#[test]
fn a_remode_waits_while_another_display_acquires() {
    let r = squish(&body_of(
        &file(VK_REMODE_C),
        "bool drm_output_vk_remode_if_needed(struct drm_output_vk_disp *d)",
    ));
    let flag = pos(
        &r,
        "os_atomic_load_bool(&d->dev->acquiring)",
        "the re-mode reads the acquire flag",
    );
    let read = pos(&r, "drm_output_vk_mode_list_read(d, &ml)", "the mode read");
    assert!(
        flag < read,
        "issue 1424: the deferral comes first (no mode set while another display acquires)"
    );
    assert!(
        r.contains("os_atomic_set_bool(&d->remode, true)"),
        "issue 1424: a deferred re-mode stays pending for the next vblank"
    );
    // the load-bearing pair: reset the settle count, THEN re-check the flag (seq-cst both), before the
    // re-mode line and the rebuild -- an open and a re-mode can never miss each other
    let tail = &r[read..];
    let reset = pos(
        tail,
        "os_atomic_set_long(&d->presents_since_surface, 0);",
        "the re-mode resets its settle count",
    );
    let recheck = reset
        + pos(
            &tail[reset..],
            "os_atomic_load_bool(&d->dev->acquiring)",
            "the re-mode re-checks the flag after the reset",
        );
    let line = pos(
        tail,
        "drm-output: vk-direct: re-mode %ux%u",
        "the re-mode line",
    );
    let rebuild = pos(
        tail,
        "drm_output_vk_rebuild_presentation(d, true)",
        "the rebuild",
    );
    let rearm = recheck
        + pos(
            &tail[recheck..],
            "os_atomic_set_bool(&d->remode, true);",
            "the deferred re-mode re-arms after the re-check",
        );
    assert_eq!(
        r.matches("os_atomic_set_bool(&d->remode, true)").count(),
        2,
        "issue 1424 review: BOTH deferrals re-arm the re-mode (the graphics thread hands a wanted mode \
         over only when it changes, so a lost re-arm keeps the Program at 60 Hz until an OBS restart)"
    );
    assert!(
        rearm < line,
        "issue 1424 review: the deferral after the reset re-arms before the re-mode line"
    );
    assert!(
        reset < recheck && recheck < line && line < rebuild,
        "issue 1424: reset -> re-check the acquire flag -> log the re-mode -> rebuild (a deferred re-mode \
         prints nothing)"
    );
}

// ------------------------------------------------------------------------------------------------
// Facet A -- defect 2: a new surface never coexists with the old one
// ------------------------------------------------------------------------------------------------

#[test]
fn a_surface_rebuild_destroys_the_old_presentation_before_the_new_surface() {
    let rb = squish(&body_of(
        &file(VK_REMODE_C),
        "bool drm_output_vk_rebuild_presentation(struct drm_output_vk_disp *d, bool surface_lost)",
    ));
    assert!(
        !rb.contains("d->retired_surface = d->surface"),
        "issue 1424: a surface is never retired -- destroying a retiree after its replacement \
         presents ends the new presentation on NVIDIA (-13, live strih-pp)"
    );
    let drain = pos(
        &rb,
        "drm_output_vk_drain_queue_bounded(d)",
        "the rebuild quiesces the display's queue",
    );
    let destroy = pos(
        &rb,
        "vkDestroySurfaceKHR(d->dev->instance, d->surface, NULL)",
        "the old surface goes",
    );
    let create = pos(
        &rb,
        "drm_output_vk_create_surface(d, true)",
        "the new surface",
    );
    assert!(
        drain < destroy && destroy < create,
        "issue 1424: drain -> destroy the old swapchain + surface -> create the new surface"
    );
    assert!(
        rb.contains("vkResetFences(d->dev->device, 1, &d->fence)"),
        "issue 1424: the drain signals the per-vblank fence; the next submit needs it unsignalled"
    );
    // issue 1424 review: a drain that timed out leaves the empty submit's fence pending -- no retry may
    // reset or re-submit it, the teardown waits it bounded first
    assert!(
        rb.contains("if (d->queue_wedged) return false;")
            && rb.contains("d->queue_wedged = true; d->submit_outstanding = true;"),
        "issue 1424 review: a timed-out drain wedges the display's rebuilds"
    );
    assert!(
        pos(&rb, "if (d->queue_wedged) return false;", "the wedge guard") < drain,
        "issue 1424 review: the wedge guard runs BEFORE the drain (never a second submit on a pending fence)"
    );
    let v = squish(&file(VK_C));
    assert!(
        v.contains("while (os_atomic_load_bool(&d->running) && !d->queue_wedged && *rebuilds < DRM_OUTPUT_VK_REBUILD_TRIES)"),
        "issue 1424 review: the rebuild retries stop on a wedged queue"
    );
    let h = file(VK_INTERNAL_H);
    assert!(
        !h.contains("retired_surface") && !file(VK_SETUP_C).contains("retired_surface"),
        "issue 1424 review: the retired-surface state is gone (no surface is ever retired)"
    );
}

#[test]
fn the_rig_harness_hands_over_a_wanted_mode_and_grades_it() {
    let h = squish(&file(HARNESS_C));
    assert!(
        h.contains("drm_output_vk_want_mode(disp[i], want_w[i], want_h[i], want_mhz[i])"),
        "issue 1424: the harness re-modes like the OBS frame hook (`HDMI-1=1920x1080@30000`)"
    );
    assert!(
        h.contains("modes_ok"),
        "issue 1424: a wanted mode not on scanout fails the run"
    );
    assert!(
        h.contains("uint64_t os_gettime_ns(void)"),
        "issue 1424: the standalone build links the core's present clock"
    );
}

// ------------------------------------------------------------------------------------------------
// Facet B -- the settle helper's truth table
// ------------------------------------------------------------------------------------------------

/// (presenting per slot, presents on the current surface per slot, self) -> first unsettled other;
/// every slot needs 3 presents here (a 2-image swapchain), `settle_need_vectors` pins the threshold.
type SettleVector = (Vec<bool>, Vec<u64>, i32, i32);

fn settle_vectors() -> Vec<SettleVector> {
    vec![
        // the live case: HDMI-0 just opened (0 presents), HDMI-1 opens -> wait for slot 0
        (vec![true, false], vec![0, 0], 1, 0),
        (vec![true, false], vec![1, 0], 1, 0),
        (vec![true, false], vec![2, 0], 1, 0),
        // 3 presents on a 2-image FIFO = the first image left the screen: the mode set is done
        (vec![true, false], vec![3, 0], 1, -1),
        (vec![true, false], vec![500, 0], 1, -1),
        // order swapped (HDMI-1 first)
        (vec![false, true], vec![0, 0], 0, 1),
        (vec![false, true], vec![0, 3], 0, -1),
        // the display being opened never blocks itself (its own slot is skipped)
        (vec![true, true], vec![0, 900], 0, -1),
        (vec![true, true], vec![900, 0], 0, 1),
        // a display that is not presenting (closed, failed, halted) never blocks
        (vec![false, false], vec![0, 0], 1, -1),
        (vec![false], vec![0], 0, -1),
        // no other display at all
        (vec![true], vec![0], 0, -1),
    ]
}

/// (swapchain images, presents needed): the (N+1)-th acquire of an N-image FIFO returns only after the
/// first presented image left the screen; never below 3, also before any swapchain exists.
fn settle_need_vectors() -> Vec<(i64, i32)> {
    vec![(0, 3), (1, 3), (2, 3), (3, 4), (4, 5), (8, 9), (-1, 3)]
}

#[test]
fn first_unsettled_names_the_other_display_still_bringing_its_presentation_up() {
    let s = file(VK_REMODE_C);
    let helper = body_of(&s, "static int drm_output_vk_first_unsettled(");
    let need_fn = body_of(&s, "static unsigned long long drm_output_vk_settle_need(");
    let h = file(VK_INTERNAL_H);
    let define = h
        .lines()
        .find(|l| l.starts_with("#define DRM_OUTPUT_VK_SETTLE_PRESENTS "))
        .expect("issue 1424: DRM_OUTPUT_VK_SETTLE_PRESENTS in the internal header");
    assert_eq!(
        define.split_whitespace().nth(2),
        Some("3ULL"),
        "issue 1424: 3 presents = the third acquire of a 2-image FIFO returned, the first image left the screen"
    );
    let vs = settle_vectors();
    let mut c = String::from("#include <stdbool.h>\n#include <stdint.h>\n#include <stdio.h>\n");
    c.push_str(define);
    c.push('\n');
    c.push_str(&need_fn);
    c.push_str(&helper);
    c.push_str("\nint main(void){\n");
    for (pres, since, me, _) in vs.iter() {
        let n = pres.len();
        let p: Vec<String> = pres.iter().map(|b| b.to_string()).collect();
        let q: Vec<String> = since.iter().map(|v| format!("{v}ULL")).collect();
        let need: Vec<String> = pres
            .iter()
            .map(|_| "drm_output_vk_settle_need(2)".to_string())
            .collect();
        c.push_str(&format!(
            "  {{ const bool p[{n}] = {{{}}}; const unsigned long long s[{n}] = {{{}}}; const unsigned long long nd[{n}] = {{{}}};\n    printf(\"%d\\n\", drm_output_vk_first_unsettled(p, s, nd, {n}, {me})); }}\n",
            p.join(", "),
            q.join(", "),
            need.join(", ")
        ));
    }
    // the threshold per swapchain image count (no swapchain yet = the floor)
    for (images, _) in settle_need_vectors() {
        c.push_str(&format!(
            "  printf(\"%d\\n\", (int)drm_output_vk_settle_need({images}L));\n"
        ));
    }
    c.push_str("  return 0;\n}\n");

    let dir = std::env::temp_dir().join(format!(
        "drm_vk_settle_1424-{}-{}",
        std::process::id(),
        SCRATCH.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&dir).expect("scratch dir");
    let src = dir.join("h.c");
    let bin = dir.join("h.bin");
    fs::write(&src, &c).expect("write harness");
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
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!("issue 1424: cannot run `{cc}` ({e}); this gate must FAIL, never skip")
        });
    if !out.status.success() {
        let _ = fs::remove_dir_all(&dir);
    }
    assert!(
        out.status.success(),
        "issue 1424: the lifted settle helper does not compile:\n{}\n---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin).output().expect("run harness");
    let _ = fs::remove_dir_all(&dir);
    assert!(run.status.success(), "issue 1424: harness exited non-zero");
    let got: Vec<i32> = String::from_utf8_lossy(&run.stdout)
        .lines()
        .map(|l| l.trim().parse().expect("int"))
        .collect();
    let nv = settle_need_vectors();
    assert_eq!(
        got.len(),
        vs.len() + nv.len(),
        "issue 1424: one answer per vector"
    );
    let need_diffs: Vec<String> = nv
        .iter()
        .zip(&got[vs.len()..])
        .filter(|((_, want), g)| *g != want)
        .map(|((images, want), g)| format!("  images={images} -> C {g}, expected {want}"))
        .collect();
    assert!(
        need_diffs.is_empty(),
        "issue 1424: the settle threshold diverges:\n{}",
        need_diffs.join("\n")
    );
    let diffs: Vec<String> = vs
        .iter()
        .zip(&got)
        .filter(|((_, _, _, want), g)| *g != want)
        .map(|((p, s, me, want), g)| {
            format!("  presenting={p:?} since={s:?} self={me} -> C {g}, expected {want}")
        })
        .collect();
    assert!(
        diffs.is_empty(),
        "issue 1424: the settle helper diverges:\n{}",
        diffs.join("\n")
    );
}
