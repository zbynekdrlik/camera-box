//! issue 1424 (display hotplug, independent review finding): NVIDIA refuses (-13) to re-acquire a
//! display this process RELEASED while the shared VkInstance lives on for another display (live
//! strih-pp, bisected in the real core). With the in-OBS retry supervisor that made two paths
//! permanent until an OBS restart:
//!
//! 1. a retried open that ACQUIRED the display and then failed a later step (the surface while the
//!    monitor wakes, an empty mode list, the swapchain) released it beside the other output;
//! 2. a present loop that died beside the other output was released by the supervisor.
//!
//! So a display's teardown never releases its acquisition while another display of the process is
//! open: it PARKS the VkDisplayKHR on the shared device, a re-open of the same output reuses it
//! without a new acquire, and the device releases every parked display before it destroys the
//! instance (the last display closed). A leaked (wedged) teardown keeps leaking, never releases.
//!
//! Facet A: std-only source anchors. Facet B: the pure choice lifted VERBATIM from
//! obs-drm-output-vk-setup.c, compiled under -Werror -Wconversion over a truth table. Runs offline:
//! `CARGO_MANIFEST_DIR=<abs> rustc --test --edition 2021 tests/drm_output_vk_park_1424.rs`. Fails
//! loudly (never skips) without a C compiler. Proof on the rig: the supervisor's runbook in
//! `.claude/rules/display-hotplug.md` (switch the Multiview monitor off and on while the Program runs).

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static SCRATCH: AtomicUsize = AtomicUsize::new(0);

const VK_SETUP_C: &str = "vendor/obs-studio/libobs/obs-drm-output-vk-setup.c";
const VK_INTERNAL_H: &str = "vendor/obs-studio/libobs/obs-drm-output-vk-internal.h";

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
// Facet A -- the teardown parks, the acquire reuses, the device releases
// ------------------------------------------------------------------------------------------------

#[test]
fn the_device_holds_the_parked_displays() {
    let h = squish(&file(VK_INTERNAL_H));
    let dev = &h[pos(&h, "struct drm_output_vk_device {", "the device struct")..];
    let dev = &dev[..pos(dev, "};", "the device struct end")];
    assert!(
        dev.contains("parked[DRM_OUTPUT_VK_MAX_DISPLAYS]"),
        "issue 1424: the shared device keeps one parked display per slot: {dev}"
    );
}

#[test]
fn the_teardown_parks_instead_of_releasing_while_another_display_is_open() {
    let s = file(VK_SETUP_C);
    let body = squish(&body_of(
        &s,
        "void drm_output_vk_destroy_display(struct drm_output_vk_disp *d)",
    ));
    let choice = pos(&body, "drm_output_vk_park_choice(", "the teardown's choice");
    assert!(
        body.contains("dev->refs - (d->open ? 1 : 0)"),
        "issue 1424: the other open displays = the device refs minus this display when it counted: {body}"
    );
    let park = pos(&body, "DRM_OUTPUT_VK_CHOICE_PARK)", "the park branch");
    let release = pos(&body, "vkReleaseDisplayEXT(", "the release call");
    assert!(
        choice < park && choice < release,
        "issue 1424: the choice decides before any park or release: {body}"
    );
    let rel_branch = pos(&body, "DRM_OUTPUT_VK_CHOICE_RELEASE)", "the release branch");
    assert!(
        rel_branch < release,
        "issue 1424: the display is released only on the RELEASE choice: {body}"
    );
    assert!(
        body.contains("kept acquired"),
        "issue 1424: a parked display is named in the log: {body}"
    );
}

#[test]
fn a_reopen_reuses_the_parked_display_before_any_new_acquire() {
    let s = file(VK_SETUP_C);
    let body = squish(&body_of(
        &s,
        "static bool drm_output_vk_acquire(struct drm_output_vk_disp *d, unsigned long output)",
    ));
    let lookup = pos(
        &body,
        "vkGetRandROutputDisplayEXT(",
        "the output's display lookup",
    );
    let unpark = pos(&body, "drm_output_vk_unpark(", "the parked-display reuse");
    let acquire = pos(&body, "vkAcquireXlibDisplayEXT(", "the new acquire");
    assert!(
        lookup < unpark && unpark < acquire,
        "issue 1424: the reuse runs after the display lookup and before a new acquire: {body}"
    );
    let unpark_fn = squish(&body_of(&s, "static bool drm_output_vk_unpark("));
    // review round 2: the handle may differ after a replug or a different screen -- an entry of the SAME
    // connector with another handle is released by name and freed, never left holding a park slot
    assert!(
        unpark_fn.contains("strcmp(d->dev->parked[i].output_name, d->output_name) == 0")
            && unpark_fn.contains("vkReleaseDisplayEXT(")
            && unpark_fn.contains("new handle")
            // review round 3: an INFO line, never the captured cause of a failed retry (the first warning)
            && unpark_fn.contains("blog(LOG_INFO, \"drm-output: vk-direct: '%s' has a new handle"),
        "issue 1424: a stale parked entry of the same connector is released and freed by name: {unpark_fn}"
    );
    assert!(
        unpark_fn.contains("d->acquired = true")
            && unpark_fn.contains("VK_NULL_HANDLE")
            && unpark_fn.contains("re-used"),
        "issue 1424: a reuse takes the parked display off the device, marks it acquired, logs it: {unpark_fn}"
    );
}

#[test]
fn the_device_releases_every_parked_display_before_the_instance_goes() {
    let s = file(VK_SETUP_C);
    let body = squish(&body_of(
        &s,
        "static void drm_output_vk_destroy_device(struct drm_output_vk_device *dev)",
    ));
    let wedged = pos(&body, "if (dev->wedged)", "the wedged branch");
    let release = pos(&body, "vkReleaseDisplayEXT(", "the parked release");
    let instance = pos(&body, "vkDestroyInstance(", "the instance destroy");
    assert!(
        wedged < release && release < instance,
        "issue 1424: a parked display is released (never on a wedged device) before the instance goes: {body}"
    );
    assert!(
        body.contains("memset(dev->parked, 0, sizeof(dev->parked))"),
        "issue 1424: the parked list is cleared with the device: {body}"
    );
}

// ------------------------------------------------------------------------------------------------
// Facet B -- the pure choice, lifted verbatim
// ------------------------------------------------------------------------------------------------

/// (acquired, leak, other open displays, a free park slot) -> 0 none / 1 release / 2 park / 3 leak.
fn choice_vectors() -> Vec<(bool, bool, i32, bool, i32)> {
    vec![
        (false, false, 0, true, 0),
        (false, true, 1, true, 0),
        (true, true, 1, true, 3),
        (true, true, 0, false, 3),
        (true, false, 1, true, 2),
        (true, false, 2, true, 2),
        (true, false, 1, false, 1),
        (true, false, 0, true, 1),
        (true, false, 0, false, 1),
        (true, false, -1, true, 1),
    ]
}

#[test]
fn the_park_choice_follows_its_truth_table() {
    let s = file(VK_SETUP_C);
    let helper = body_of(&s, "static int drm_output_vk_park_choice(");
    let h = file(VK_INTERNAL_H);
    let defines: Vec<&str> = h
        .lines()
        .filter(|l| l.starts_with("#define DRM_OUTPUT_VK_CHOICE_"))
        .collect();
    assert_eq!(
        defines.len(),
        4,
        "issue 1424: the four choices are defined in the internal header: {defines:?}"
    );
    let vs = choice_vectors();
    let mut c = String::from("#include <stdbool.h>\n#include <stdio.h>\n");
    for d in &defines {
        c.push_str(d);
        c.push('\n');
    }
    c.push_str(&helper);
    c.push_str("\nint main(void){\n");
    for (acq, leak, others, free, _) in &vs {
        c.push_str(&format!(
            "  printf(\"%d\\n\", drm_output_vk_park_choice({acq}, {leak}, {others}, {free}));\n"
        ));
    }
    c.push_str("  return 0;\n}\n");

    let dir = std::env::temp_dir().join(format!(
        "drm_vk_park_1424-{}-{}",
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
        "issue 1424: the lifted park choice does not compile:\n{}\n---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin).output().expect("run harness");
    let _ = fs::remove_dir_all(&dir);
    assert!(run.status.success(), "issue 1424: harness exited non-zero");
    let got: Vec<i32> = String::from_utf8_lossy(&run.stdout)
        .lines()
        .map(|l| l.trim().parse().expect("int"))
        .collect();
    assert_eq!(got.len(), vs.len(), "issue 1424: one answer per vector");
    let diffs: Vec<String> = vs
        .iter()
        .zip(&got)
        .filter(|((.., want), g)| *g != want)
        .map(|((acq, leak, others, free, want), g)| {
            format!("  acquired={acq} leak={leak} others={others} free={free} -> C {g}, expected {want}")
        })
        .collect();
    assert!(
        diffs.is_empty(),
        "issue 1424: the park choice diverges:\n{}",
        diffs.join("\n")
    );
}
