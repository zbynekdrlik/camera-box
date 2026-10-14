//! issue 1424 (design 6164790116) -- the vk-direct DEADLINE PICK. The owner measured the strih-pp
//! vestibule output (HDMI-1, 1920x1080@29.970) at 171 ms render -> cam1 capture and called it far
//! too much; the audio there must never be delayed, so the picture path has to get shorter.
//!
//! The present thread used to take the newest READY frame right after the vblank the acquire
//! returned on, then hold it a whole refresh in the FIFO queue. Now it waits until `period - margin`
//! after the acquire and picks then, just before the vblank the frame is shown on (up to one refresh
//! less on screen). A missed vblank (the next acquire 1.5..3.5 periods later) grows the margin by one
//! step, at most to half a period. FIFO itself stays (the only present mode the spec guarantees;
//! the 1346 test pins it).
//!
//! Facet A: source anchors (the call order in the present loop, the open-time reset, the minute
//! line, the present-mode log). Facet B: the pure helpers lifted VERBATIM and compiled under
//! `-Werror -Wconversion` over a truth table. Runs offline:
//! `CARGO_MANIFEST_DIR=<abs> rustc --test --edition 2021 tests/drm_output_vk_deadline_1424.rs -o
//! /tmp/t && /tmp/t`. Fails loudly when no C compiler exists.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const VK_C: &str = "vendor/obs-studio/libobs/obs-drm-output-vk.c";
const VK_SETUP_C: &str = "vendor/obs-studio/libobs/obs-drm-output-vk-setup.c";
const VK_INTERNAL_H: &str = "vendor/obs-studio/libobs/obs-drm-output-vk-internal.h";

fn file(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("issue 1424: cannot read {}: {e}", p.display()))
}

/// The function whose definition starts with `sig`, up to the first `\n}\n`.
fn body_of(src: &str, sig: &str) -> String {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("issue 1424: `{sig}` not found"));
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .unwrap_or_else(|| panic!("issue 1424: `{sig}` has no closing brace"));
    src[start..end].to_string()
}

/// The one `#define NAME ...` line of `src`.
fn define_line(src: &str, name: &str) -> String {
    let lines: Vec<&str> = src
        .lines()
        .filter(|l| l.starts_with(&format!("#define {name} ")))
        .collect();
    assert_eq!(lines.len(), 1, "issue 1424: exactly one `#define {name}`");
    format!("{}\n", lines[0])
}

static SEQ: AtomicUsize = AtomicUsize::new(0);

fn compile_and_run(c_source: &str) -> String {
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("drm_vk_deadline_1424_{}_{n}", std::process::id()));
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
            panic!("issue 1424: cannot run `{cc}` ({e}); this gate must FAIL, never skip")
        });
    assert!(
        out.status.success(),
        "issue 1424: the lifted helpers do NOT compile under -Werror -Wconversion:\n{}\n---\n{c_source}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin).output().expect("run harness");
    let _ = fs::remove_dir_all(&dir);
    assert!(run.status.success(), "issue 1424: harness exited non-zero");
    String::from_utf8(run.stdout).expect("utf-8")
}

fn lifted_helpers() -> String {
    let src = file(VK_C);
    let mut c = String::from("#include <stdbool.h>\n#include <stdint.h>\n#include <stdio.h>\n");
    c.push_str(&define_line(&src, "DRM_OUTPUT_VK_PICK_MARGIN_START_NS"));
    c.push_str(&define_line(&src, "DRM_OUTPUT_VK_PICK_MARGIN_STEP_NS"));
    c.push_str(&define_line(&src, "DRM_OUTPUT_VK_PICK_QUIET_NS"));
    for sig in [
        "static uint64_t drm_output_vk_period_ns(uint32_t refresh_mhz)",
        "static uint64_t drm_output_vk_pick_deadline_ns(uint64_t acquire_ns, uint64_t period_ns, uint64_t margin_ns)",
        "static bool drm_output_vk_missed_vblank(uint64_t gap_ns, uint64_t period_ns)",
        "static bool drm_output_vk_miss_counts(uint64_t now_ns, uint64_t disrupt_ns)",
        "static uint64_t drm_output_vk_margin_after_miss(uint64_t margin_ns, uint64_t period_ns)",
    ] {
        c.push_str(&body_of(&src, sig));
    }
    c
}

// ----------------------------------------------------------------------------------------------
// Facet B -- the pure helpers
// ----------------------------------------------------------------------------------------------

/// 29.970 Hz (the vestibule) and ~60 Hz (the Multiview monitors).
const P30: u64 = 33_366_700;
const P60: u64 = 16_666_666;

#[test]
fn the_helpers_compute_the_truth_table() {
    let mut c = lifted_helpers();
    c.push_str("int main(void){\n");
    for mhz in [29970u32, 60000, 59951, 0] {
        c.push_str(&format!(
            "  printf(\"P %llu\\n\", (unsigned long long)drm_output_vk_period_ns({mhz}u));\n"
        ));
    }
    // (acquire, period, margin)
    let deadlines: [(u64, u64, u64); 5] = [
        (1_000, P30, 5_000_000),
        (1_000, P60, 5_000_000),
        (1_000, P60, 10_000_000), // the margin clamps to half a period
        (1_000, 0, 5_000_000),    // unknown period: no wait
        (9_000_000_000_000, P30, 0),
    ];
    for (a, p, m) in deadlines {
        c.push_str(&format!(
            "  printf(\"D %llu\\n\", (unsigned long long)drm_output_vk_pick_deadline_ns({a}ULL, {p}ULL, {m}ULL));\n"
        ));
    }
    // (gap, period)
    let gaps: [(u64, u64); 8] = [
        (P30, P30),                   // on time
        (P30 + P30 / 2 - 1, P30),     // jitter, still on time
        (P30 + P30 / 2, P30),         // one refresh late = a miss
        (2 * P30, P30),               // a miss
        (3 * P30 + P30 / 2 - 1, P30), // still a miss
        (3 * P30 + P30 / 2, P30),     // a rebuild / stall, not the pick
        (1_000_000_000, P30),         // a re-mode pause
        (40_000_000, 0),              // unknown period
    ];
    for (g, p) in gaps {
        c.push_str(&format!(
            "  printf(\"M %d\\n\", drm_output_vk_missed_vblank({g}ULL, {p}ULL) ? 1 : 0);\n"
        ));
    }
    // (margin, period)
    let margins: [(u64, u64); 4] = [
        (5_000_000, P30),
        (16_000_000, P30), // capped at half a period
        (5_000_000, P60),
        (9_000_000, P60), // above the cap already: back to the cap
    ];
    for (m, p) in margins {
        c.push_str(&format!(
            "  printf(\"G %llu\\n\", (unsigned long long)drm_output_vk_margin_after_miss({m}ULL, {p}ULL));\n"
        ));
    }
    c.push_str("  printf(\"S %llu %llu\\n\", (unsigned long long)DRM_OUTPUT_VK_PICK_MARGIN_START_NS, (unsigned long long)DRM_OUTPUT_VK_PICK_MARGIN_STEP_NS);\n");
    // a miss counts only outside the 3 s quiet window after the device's last bring-up (0 = none)
    for (now, disrupt) in [
        (5_000_000_000u64, 0u64),
        (5_000_000_000, 4_000_000_000),
        (7_000_000_000, 4_000_000_000),
        (6_999_999_999, 4_000_000_000),
    ] {
        c.push_str(&format!(
            "  printf(\"Q %d\\n\", drm_output_vk_miss_counts({now}ULL, {disrupt}ULL) ? 1 : 0);\n"
        ));
    }
    c.push_str("  return 0;\n}\n");
    let out = compile_and_run(&c);
    let want = vec![
        "P 33366700".to_string(),
        "P 16666666".to_string(),
        "P 16680288".to_string(),
        "P 0".to_string(),
        format!("D {}", 1_000 + P30 - 5_000_000),
        format!("D {}", 1_000 + P60 - 5_000_000),
        format!("D {}", 1_000 + P60 - P60 / 2),
        "D 1000".to_string(),
        format!("D {}", 9_000_000_000_000 + P30),
        "M 0".to_string(),
        "M 0".to_string(),
        "M 1".to_string(),
        "M 1".to_string(),
        "M 1".to_string(),
        "M 0".to_string(),
        "M 0".to_string(),
        "M 0".to_string(),
        "G 6000000".to_string(),
        format!("G {}", P30 / 2),
        "G 6000000".to_string(),
        format!("G {}", P60 / 2),
        "S 5000000 1000000".to_string(),
        "Q 1".to_string(),
        "Q 0".to_string(),
        "Q 1".to_string(),
        "Q 0".to_string(),
    ];
    let got: Vec<String> = out.lines().map(str::to_string).collect();
    assert_eq!(got, want, "issue 1424: the deadline-pick helpers");
}

/// The pick lands inside the refresh it is meant for: with the start margin the frame is taken at
/// least 4/5 of a period after the vblank on both modes, and never closer to the next vblank than
/// the margin. The saving on the vestibule mode is at least 25 ms per frame.
#[test]
fn the_start_margin_saves_most_of_a_refresh_on_both_modes() {
    let mut c = lifted_helpers();
    c.push_str("int main(void){\n");
    c.push_str("  (void)drm_output_vk_period_ns; (void)drm_output_vk_missed_vblank; (void)drm_output_vk_margin_after_miss; (void)drm_output_vk_miss_counts;\n");
    for p in [P30, P60] {
        c.push_str(&format!(
            "  printf(\"%llu\\n\", (unsigned long long)(drm_output_vk_pick_deadline_ns(0ULL, {p}ULL, DRM_OUTPUT_VK_PICK_MARGIN_START_NS)));\n"
        ));
    }
    c.push_str("  return 0;\n}\n");
    let out = compile_and_run(&c);
    let picks: Vec<u64> = out.lines().map(|l| l.parse().unwrap()).collect();
    assert!(
        picks[0] >= 25_000_000,
        "issue 1424: 29.97 Hz pick waits {} ns",
        picks[0]
    );
    assert!(P30 - picks[0] >= 5_000_000);
    assert!(
        picks[1] * 5 >= P60 * 3,
        "issue 1424: 60 Hz pick waits {} ns",
        picks[1]
    );
    assert!(P60 - picks[1] >= 5_000_000);
}

// ----------------------------------------------------------------------------------------------
// Facet A -- the wiring
// ----------------------------------------------------------------------------------------------

#[test]
fn the_present_loop_waits_for_the_pick_between_the_acquire_and_the_pick() {
    let src = file(VK_C);
    let thread = body_of(&src, "static void *drm_output_vk_present_thread(void *arg)");
    let acquire = thread
        .find("vk.vkAcquireNextImageKHR(")
        .expect("the FIFO acquire");
    let wait = thread
        .find("drm_output_vk_wait_for_pick(d);")
        .expect("issue 1424: the present loop must wait for the deadline pick");
    let pick = thread
        .find("drm_output_vk_present_pick(")
        .expect("the mailbox pick");
    assert!(
        acquire < wait && wait < pick,
        "issue 1424: acquire -> deadline wait -> pick (the wait must sit after the acquire's error \
         handling and before the pick)"
    );
    assert_eq!(
        thread.matches("drm_output_vk_wait_for_pick(").count(),
        1,
        "issue 1424: one deadline wait per vblank"
    );
    let wait_fn = body_of(
        &src,
        "static void drm_output_vk_wait_for_pick(struct drm_output_vk_disp *d)",
    );
    for token in [
        "os_gettime_ns()",
        "drm_output_vk_period_ns(d->refresh_mhz)",
        "drm_output_vk_missed_vblank(acquired_ns - d->last_acquire_ns, period_ns)",
        "d->late_presents++;",
        "drm_output_vk_margin_after_miss(d->pick_margin_ns, period_ns)",
        "d->last_acquire_ns = acquired_ns;",
        "drm_output_vk_sleep_until(drm_output_vk_pick_deadline_ns(acquired_ns, period_ns, d->pick_margin_ns));",
    ] {
        assert!(wait_fn.contains(token), "issue 1424: the deadline wait must contain `{token}`");
    }
    let sleep = body_of(
        &src,
        "static void drm_output_vk_sleep_until(uint64_t when_ns)",
    );
    assert!(
        sleep.contains("clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME,"),
        "issue 1424: an absolute sleep on the clock os_gettime_ns reads"
    );
}

#[test]
fn the_open_resets_the_margin_and_the_minute_line_reports_it() {
    let src = file(VK_C);
    for token in [
        "d->pick_margin_ns = DRM_OUTPUT_VK_PICK_MARGIN_START_NS;",
        "d->last_acquire_ns = 0;",
        "d->late_presents = 0;",
        "\"drm-output: program-present #%llu (vk-direct FIFO, '%s') pick margin %.1f ms, %llu \"",
        "drm-output: vk-direct pick margin on '%s' -> %.1f ms",
    ] {
        assert!(
            src.contains(token),
            "issue 1424: {VK_C} must contain `{token}`"
        );
    }
    let h = file(VK_INTERNAL_H);
    for token in [
        "uint64_t pick_margin_ns;",
        "uint64_t last_acquire_ns;",
        "unsigned long long late_presents;",
        "X(vkGetPhysicalDeviceSurfacePresentModesKHR)",
    ] {
        assert!(
            h.contains(token),
            "issue 1424: {VK_INTERNAL_H} must contain `{token}`"
        );
    }
    assert!(
        src.contains("drm_output_vk_miss_counts(acquired_ns, disrupt_ns)"),
        "issue 1424: a miss inside a bring-up's quiet window never grows the margin"
    );
    assert!(
        h.contains("volatile long disrupt_ns;")
            && h.contains("static inline void drm_output_vk_mark_disruption("),
        "issue 1424: the device carries its last bring-up time"
    );
    let setup = file(VK_SETUP_C);
    assert_eq!(
        setup
            .matches("drm_output_vk_mark_disruption(d->dev);")
            .count(),
        2,
        "issue 1424: an open marks a bring-up before its settle wait and after its acquire"
    );
    let remode = file("vendor/obs-studio/libobs/obs-drm-output-vk-remode.c");
    assert_eq!(
        remode
            .matches("drm_output_vk_mark_disruption(d->dev);")
            .count(),
        2,
        "issue 1424: a presentation rebuild marks a bring-up at its start and its end"
    );
    assert!(
        setup.contains("drm-output: vk-direct present modes offered on '%s': %s"),
        "issue 1424: the swapchain setup logs the present modes the surface offers"
    );
    assert!(
        setup.contains(".presentMode = VK_PRESENT_MODE_FIFO_KHR,"),
        "issue 1424: FIFO stays (the deadline pick works inside it)"
    );
}
