//! issue 1424 (design 6386467303 layer 3) -- the retry SUPERVISOR's per-output step, run for real.
//!
//! `tests/drm_output_retry_1424.rs` pins the pure decision (C header vs Rust twin). This file lifts the
//! supervisor's own code VERBATIM out of `obs-drm-output-retry.c` -- the module's log entry with its
//! per-attempt capture, the capture flush, the reason text and `drm_output_retry_look` -- compiles it
//! (`-Wall -Wextra -Wconversion -Wformat=2 -Werror`) against stand-ins for the registry's start / stop
//! steps and the backend's active / alive reads, and drives a strih-pp-shaped script:
//!
//! * the start at the OBS launch failed: ONE `dormant -- its start at the OBS launch failed` line;
//! * five attempts refused for the same cause: ONE `dormant -- <cause>` line naming the FIRST warning
//!   of the attempt (the vk open's own refusal, never the generic "start FAILED" echo that follows it),
//!   and none of the attempts' own lines;
//! * the cause changes (the monitor came on, now another refusal): one more line;
//! * an attempt that succeeds: its captured lines flushed in order, then `LIVE again after N attempt(s)`;
//! * the present loop dies: `lost its present loop`, the registry stop, then the retry starts it again.
//!
//! It FAILS LOUDLY when no C compiler is present (never skips).

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static SCRATCH: AtomicUsize = AtomicUsize::new(0);

const RETRY_C: &str = "vendor/obs-studio/libobs/obs-drm-output-retry.c";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn file(rel: &str) -> String {
    let p = repo(rel);
    fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("issue 1424: cannot read {}: {e}", p.display()))
}

/// `src[from .. to)` between two anchors that must each occur exactly once.
fn between(src: &str, from: &str, to: &str) -> String {
    assert_eq!(src.matches(from).count(), 1, "issue 1424: anchor {from:?}");
    assert_eq!(src.matches(to).count(), 1, "issue 1424: anchor {to:?}");
    let a = src.find(from).unwrap();
    let b = src.find(to).unwrap();
    assert!(a < b, "issue 1424: {from:?} must come before {to:?}");
    src[a..b].to_string()
}

const STUBS: &str = r#"
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include "obs-drm-output-retry.h"

enum { LOG_ERROR = 100, LOG_WARNING = 200, LOG_INFO = 300, LOG_DEBUG = 400 };

__attribute__((format(printf, 2, 0))) static void blogva(int level, const char *fmt, va_list ap)
{
	printf("LOG %d ", level);
	vprintf(fmt, ap);
	printf("\n");
}

__attribute__((format(printf, 2, 3))) void drm_output_blog(int log_level, const char *format, ...);
#define blog drm_output_blog

static uint64_t g_now;
static uint64_t os_gettime_ns(void)
{
	return g_now;
}

struct drm_output_inst;
struct drm_output_backend_ops {
	bool (*active)(struct drm_output_inst *inst);
	bool (*alive)(struct drm_output_inst *inst);
};
struct drm_output_inst {
	bool started;
	char connector[64];
	const struct drm_output_backend_ops *ops;
};

/* the fake backend: open, loop alive, and what the next start does */
static bool g_open, g_loop_alive;
static int g_next_start; /* 0 = refused (acquire), 1 = refused (no device), 2 = succeeds */

static bool fake_active(struct drm_output_inst *inst)
{
	(void)inst;
	return g_open;
}
static bool fake_alive(struct drm_output_inst *inst)
{
	(void)inst;
	return g_open && g_loop_alive;
}
static const struct drm_output_backend_ops g_ops = {fake_active, fake_alive};

bool drm_output_inst_start_locked(struct drm_output_inst *inst);
void drm_output_inst_stop_locked(struct drm_output_inst *inst);

bool drm_output_inst_start_locked(struct drm_output_inst *inst)
{
	printf("CALL start '%s'\n", inst->connector);
	if (g_next_start == 0) {
		blog(LOG_WARNING, "drm-output: vk-direct: vkAcquireXlibDisplayEXT('%s') FAILED VkResult -13", inst->connector);
		blog(LOG_WARNING, "drm-output: vk-direct start FAILED for '%s' -- output dormant", inst->connector);
		blog(LOG_WARNING, "drm-output: start FAILED for '%s' (vk-direct)", inst->connector);
		inst->started = false;
		return false;
	}
	if (g_next_start == 1) {
		blog(LOG_WARNING, "drm-output: vk-direct: no Vulkan device drives output '%s'", inst->connector);
		blog(LOG_WARNING, "drm-output: start FAILED for '%s' (vk-direct)", inst->connector);
		inst->started = false;
		return false;
	}
	blog(LOG_INFO, "drm-output: vk-direct display acquired output='%s'", inst->connector);
	blog(LOG_INFO, "drm-output: ACTIVE (vk-direct) on '%s'", inst->connector);
	g_open = true;
	g_loop_alive = true;
	inst->started = true;
	return true;
}

void drm_output_inst_stop_locked(struct drm_output_inst *inst)
{
	printf("CALL stop '%s'\n", inst->connector);
	g_open = false;
	inst->started = false;
}
"#;

const DRIVER: &str = r#"
#define S 1000000000ULL
static struct drm_output_inst g_inst = {false, "HDMI-0", &g_ops};
static struct drm_output_retry_slot g_slot;

static void look(uint64_t t, bool connected)
{
	g_now = t;
	printf("LOOK %llu\n", (unsigned long long)(t / S));
	drm_output_retry_look(&g_slot, &g_inst, connected);
}

int main(void)
{
	memset(&g_slot, 0, sizeof(g_slot));
	/* the autostart's start failed: the first look */
	look(0, true);
	/* five refusals for the same cause, 2 s apart */
	g_next_start = 0;
	for (uint64_t t = 2; t <= 10; t += 2)
		look(t * S, true);
	/* the cause changes */
	g_next_start = 1;
	look(15 * S, true);
	look(20 * S, true);
	/* it succeeds */
	g_next_start = 2;
	look(25 * S, true);
	look(26 * S, true);
	/* the present loop dies: released, then started again on the retry schedule */
	g_loop_alive = false;
	look(30 * S, true);
	look(31 * S, true);
	look(32 * S, true);
	return 0;
}
"#;

fn harness() -> String {
    let c = file(RETRY_C);
    let capture = between(
        &c,
        "#define DRM_OUTPUT_LOGCAP_LINES",
        "/* ---- the X RandR connection",
    );
    let look = between(
        &c,
        "struct drm_output_retry_slot {",
        "static void *drm_output_retry_thread(void *arg)",
    );
    format!("{STUBS}\n{capture}\n{look}\n{DRIVER}")
}

fn run() -> String {
    let dir = std::env::temp_dir().join(format!(
        "drm_output_retry_supervisor_1424-{}-{}",
        std::process::id(),
        SCRATCH.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&dir).expect("tmp dir");
    let c = dir.join("harness.c");
    let bin = dir.join("harness");
    fs::write(&c, harness()).expect("write harness");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=gnu11",
            "-Wall",
            "-Wextra",
            "-Wconversion",
            "-Wformat=2",
            "-Werror",
            "-I",
        ])
        .arg(repo("vendor/obs-studio/libobs"))
        .arg(&c)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!("issue 1424: no C compiler ({cc}): {e} -- the supervisor harness cannot run")
        });
    if !out.status.success() {
        let _ = fs::remove_dir_all(&dir);
    }
    assert!(
        out.status.success(),
        "issue 1424: the lifted supervisor must compile cleanly:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r = Command::new(&bin).output().expect("run harness");
    let _ = fs::remove_dir_all(&dir);
    assert!(r.status.success(), "issue 1424: harness failed");
    String::from_utf8(r.stdout).expect("utf8")
}

fn lines_between<'a>(out: &'a str, from: &str, to: &str) -> Vec<&'a str> {
    let a = out
        .find(from)
        .unwrap_or_else(|| panic!("no {from:?} in\n{out}"));
    let b = out[a..]
        .find(to)
        .map(|i| a + i)
        .unwrap_or_else(|| panic!("no {to:?} after {from:?} in\n{out}"));
    out[a..b].lines().collect()
}

#[test]
fn a_failed_launch_start_is_named_once_and_each_cause_once() {
    let out = run();
    // the first look: the launch failure, no attempt yet
    let first = lines_between(&out, "LOOK 0\n", "LOOK 2\n");
    assert_eq!(
        first
            .iter()
            .filter(|l| l.contains("dormant -- its start at the OBS launch failed"))
            .count(),
        1,
        "{out}"
    );
    assert!(!first.iter().any(|l| l.starts_with("CALL start")), "{out}");
    // five refusals for one cause: five attempts, ONE dormant line, naming the vk open's refusal
    let same = lines_between(&out, "LOOK 2\n", "LOOK 15\n");
    assert_eq!(
        same.iter().filter(|l| l.starts_with("CALL start")).count(),
        5,
        "{out}"
    );
    let dormant: Vec<&&str> = same
        .iter()
        .filter(|l| l.contains("drm-output: retry: 'HDMI-0' dormant -- "))
        .collect();
    assert_eq!(dormant.len(), 1, "{out}");
    assert!(
        dormant[0].contains("vk-direct: vkAcquireXlibDisplayEXT('HDMI-0') FAILED VkResult -13"),
        "issue 1424: the line names the CAUSE, the first warning of the attempt: {out}"
    );
    assert!(
        !dormant[0].contains("start FAILED"),
        "issue 1424: never the generic echo that follows the cause: {out}"
    );
    // none of the attempts' own lines reach the log
    assert!(
        !same.iter().any(|l| l.starts_with("LOG ")
            && l.contains("vkAcquireXlibDisplayEXT")
            && !l.contains("retry:")),
        "issue 1424: a failed attempt's own lines are captured, never logged: {out}"
    );
    // the cause changes: one more line, and only one
    let changed = lines_between(&out, "LOOK 15\n", "LOOK 25\n");
    let d2: Vec<&&str> = changed
        .iter()
        .filter(|l| l.contains("retry: 'HDMI-0' dormant -- "))
        .collect();
    assert_eq!(d2.len(), 1, "{out}");
    assert!(
        d2[0].contains("no Vulkan device drives output 'HDMI-0'"),
        "{out}"
    );
}

#[test]
fn a_successful_attempt_flushes_its_lines_then_names_the_recovery() {
    let out = run();
    let ok = lines_between(&out, "LOOK 25\n", "LOOK 26\n");
    let acquired = ok
        .iter()
        .position(|l| l.contains("vk-direct display acquired output='HDMI-0'"))
        .unwrap_or_else(|| panic!("the captured open lines are flushed: {out}"));
    let active = ok
        .iter()
        .position(|l| l.contains("ACTIVE (vk-direct) on 'HDMI-0'"))
        .expect("ACTIVE flushed");
    let again = ok
        .iter()
        .position(|l| l.contains("drm-output: retry: 'HDMI-0' LIVE again after 8 attempt(s)"))
        .unwrap_or_else(|| panic!("the recovery line counts every attempt: {out}"));
    assert!(acquired < active && active < again, "{out}");
    // a live output is left alone
    let live = lines_between(&out, "LOOK 26\n", "LOOK 30\n");
    assert_eq!(
        live.len(),
        1,
        "issue 1424: nothing happens to a live output: {out}"
    );
}

#[test]
fn a_dead_present_loop_is_released_and_started_again() {
    let out = run();
    let dead = lines_between(&out, "LOOK 30\n", "LOOK 31\n");
    let lost = dead
        .iter()
        .position(|l| l.contains("drm-output: retry: 'HDMI-0' lost its present loop"))
        .unwrap_or_else(|| panic!("{out}"));
    let stop = dead
        .iter()
        .position(|l| *l == "CALL stop 'HDMI-0'")
        .unwrap_or_else(|| panic!("the registry stop releases the display: {out}"));
    assert!(lost < stop, "{out}");
    // released at 30 s: the next start is 2 s later, not at 31 s
    let early = lines_between(&out, "LOOK 31\n", "LOOK 32\n");
    assert!(!early.iter().any(|l| l.starts_with("CALL start")), "{out}");
    let tail = &out[out.find("LOOK 32\n").unwrap()..];
    assert!(tail.contains("CALL start 'HDMI-0'"), "{out}");
    assert!(tail.contains("LIVE again after 1 attempt(s)"), "{out}");
}
