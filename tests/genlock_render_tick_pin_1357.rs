//! Issue 1357 — EXECUTABLE gates for the vendored genlock render-tick CPU pin.
//!
//! The pin lives in `vendor/obs-studio/libobs/obs-video.c`, inside the block
//! `/* camera-box issue 1357 render-tick pin BEGIN` … `/* camera-box issue 1357 render-tick pin END */`.
//! This file lifts that block VERBATIM (nothing retyped) and compiles it three ways:
//!
//! 1. PARITY: the pure decision `genlock_render_tick_pin_set` (isolated AND nohz_full, parsed by the
//!    shipped `genlock_parse_cpulist_into_set`) over a spread of vectors, byte-identical to the Tier-0
//!    authority `src/genlock_render_tick_pin.rs`.
//! 2. DECISION TRACE: the startup + per-tick code against recording stubs of
//!    `pthread_setaffinity_np` / `pthread_getaffinity_np` / `sched_setscheduler`, the sysfs paths
//!    pointed at fixture files. It proves: no isolated core = no syscall at all + one "not pinned"
//!    line (the strih-lx case, no `{10,11}` fallback); the pin is held only around the sleep; FIFO is
//!    raised after the affinity narrows and dropped before it widens; a failure disarms the pin.
//! 3. INHERITANCE on REAL threads: a thread created after the tick woke up has the process mask and
//!    SCHED_OTHER; a control thread created inside the sleep window has the pin mask, which proves
//!    the harness sees inheritance at all.
//!
//! The Rust authority is included by `#[path]`, so this file is std-only. It runs under `cargo test`
//! in CI AND standalone with no cargo:
//!
//! ```text
//! CARGO_MANIFEST_DIR=<worktree-abs> rustc --test --edition 2021 tests/genlock_render_tick_pin_1357.rs -o /tmp/t && /tmp/t
//! ```
//!
//! `cc` is required. Per the project's test-strictness rule this FAILS LOUDLY rather than skipping
//! when the toolchain is missing.

#[allow(dead_code)]
#[path = "../src/genlock_render_tick_pin.rs"]
mod genlock_render_tick_pin;

use genlock_render_tick_pin::{parse_cpulist, render_tick_pin_cores};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const OBS_VIDEO: &str = "vendor/obs-studio/libobs/obs-video.c";
const BEGIN: &str = "/* camera-box issue 1357 render-tick pin BEGIN";
const END: &str = "/* camera-box issue 1357 render-tick pin END */";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// The whole marked block, VERBATIM.
fn lifted_block() -> String {
    let p = repo(OBS_VIDEO);
    let src = fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
    assert_eq!(
        src.matches(BEGIN).count(),
        1,
        "issue 1357: {OBS_VIDEO} must carry exactly one `{BEGIN}` marker (the render-tick pin block)"
    );
    let start = src.find(BEGIN).unwrap();
    let end = src[start..]
        .find(END)
        .map(|i| start + i + END.len())
        .unwrap_or_else(|| panic!("issue 1357: {OBS_VIDEO} has no `{END}` marker"));
    src[start..end].to_string()
}

/// The prelude every harness shares: the libc headers obs-video.c gets under `_GNU_SOURCE`, and a
/// printf-format `blog` so `-Wformat=2` checks every shipped log call against its arguments.
const PRELUDE: &str = r#"#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#define LOG_WARNING 200
#define LOG_INFO 300
static void blog(int level, const char *fmt, ...) __attribute__((format(printf, 2, 3)));
static void blog(int level, const char *fmt, ...)
{
	va_list ap;
	va_start(ap, fmt);
	printf("LOG %d ", level);
	vprintf(fmt, ap);
	printf("\n");
	va_end(ap);
}
static void print_set(const char *tag, const cpu_set_t *s)
{
	int first = 1;
	printf("%s ", tag);
	for (int c = 0; c < CPU_SETSIZE; c++) {
		if (CPU_ISSET(c, s)) {
			printf(first ? "%d" : ",%d", c);
			first = 0;
		}
	}
	printf("\n");
}
"#;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let base = option_env!("CARGO_TARGET_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let d = base.join(format!(
            "render_tick_pin_1357_{}_{}_{tag}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&d).expect("create the scratch dir");
        Scratch(d)
    }

    fn file(&self, name: &str, content: &str) -> PathBuf {
        let p = self.0.join(name);
        fs::write(&p, content).expect("write a fixture file");
        p
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn compile(dir: &Scratch, c: &str) -> PathBuf {
    let cfile = dir.0.join("pin.c");
    let bin = dir.0.join("pin.bin");
    fs::write(&cfile, c).expect("write the harness");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=gnu99",
            "-Wall",
            "-Wextra",
            "-Wconversion",
            "-Wformat=2",
            "-Werror",
            "-pthread",
        ])
        .arg(&cfile)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "issue 1357: could not run the C compiler `{cc}` ({e}). This gate must FAIL rather \
                 than skip when the toolchain is absent. Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "issue 1357: the lifted render-tick pin block does NOT COMPILE under -Wall -Wextra \
         -Wconversion -Wformat=2 -Werror:\n--- cc stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    bin
}

fn run(bin: &Path, args: &[&str]) -> String {
    let out = Command::new(bin)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("run {}: {e}", bin.display()));
    assert!(
        out.status.success(),
        "issue 1357: harness exited {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("harness stdout is UTF-8")
}

fn c_string(s: &str) -> String {
    let mut o = String::from("\"");
    for ch in s.chars() {
        match ch {
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn join(v: &[usize]) -> String {
    v.iter()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

// ---------------------------------------------------------------------------------------------
// 1. PARITY — the pure decision.
// ---------------------------------------------------------------------------------------------

/// `(isolated, nohz_full)` vectors: the live strih-lx read, every "one side empty" shape, the
/// historic imag layout, intersections, and the malformed / out-of-range inputs the C parser
/// tolerates without overflow.
fn parity_vectors() -> Vec<(String, String)> {
    let fixed: &[(&str, &str)] = &[
        ("", ""),
        ("\n", "\n"),
        ("2-11", ""),
        ("", "10,11"),
        ("2-11\n", "(null)\n"),
        ("2-11\n", "10-11\n"),
        ("2-11", "10,11"),
        ("3", "3"),
        ("3", "4"),
        ("0-3,8-11", "2-9"),
        ("10 - 11", "10-11"),
        ("10-11", "10 - 11"),
        ("3,x,5", "0-7"),
        ("5-3", "0-7"),
        ("1023,1024", "0-2000"),
        ("99999999999999999999,2", "2"),
        ("1022-5000", "1000-1023"),
        ("\t4,\n6", "4,6\n"),
        ("0", "0"),
        ("0-1023", "511-513"),
        (",,7,,", "7"),
        ("7-", "7"),
    ];
    let mut v: Vec<(String, String)> = fixed
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
    // Deterministic pseudo-random range lists (xorshift), so a parser/intersection slip that the
    // hand-picked vectors miss still shows up.
    let mut x: u64 = 0x1357_2026_0927_0001;
    let mut next = |m: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % m
    };
    for _ in 0..200 {
        let mut side = || {
            let n = next(4);
            let mut parts = Vec::new();
            for _ in 0..n {
                let a = next(40);
                if next(2) == 0 {
                    parts.push(a.to_string());
                } else {
                    parts.push(format!("{a}-{}", a + next(8)));
                }
            }
            parts.join(",")
        };
        let a = side();
        let b = side();
        v.push((a, b));
    }
    v
}

#[test]
fn pin_cores_c_matches_the_rust_authority() {
    let vectors = parity_vectors();
    let mut c = String::from(PRELUDE);
    c.push_str(&lifted_block());
    c.push_str("\nstatic const char *V[][2] = {\n");
    for (a, b) in &vectors {
        c.push_str(&format!("\t{{{}, {}}},\n", c_string(a), c_string(b)));
    }
    c.push_str(
        "};\nint main(void)\n{\n\tfor (size_t i = 0; i < sizeof(V) / sizeof(V[0]); i++) {\n\
         \t\tcpu_set_t out;\n\t\tgenlock_render_tick_pin_set(V[i][0], V[i][1], &out);\n\
         \t\tprint_set(\"PIN\", &out);\n\t}\n\treturn 0;\n}\n",
    );
    let dir = Scratch::new("parity");
    let bin = compile(&dir, &c);
    let out = run(&bin, &[]);
    let lines: Vec<&str> = out.lines().filter(|l| l.starts_with("PIN")).collect();
    assert_eq!(lines.len(), vectors.len(), "one C result per vector");
    let mut diffs = Vec::new();
    for ((a, b), line) in vectors.iter().zip(&lines) {
        let want = format!("PIN {}", join(&render_tick_pin_cores(a, b)))
            .trim_end()
            .to_string();
        if line.trim_end() != want {
            diffs.push(format!(
                "isolated={a:?} nohz_full={b:?}: C `{line}` vs Rust `{want}`"
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "issue 1357: the C render-tick pin decision diverges from src/genlock_render_tick_pin.rs \
         on {} of {} vectors:\n{}",
        diffs.len(),
        vectors.len(),
        diffs.join("\n")
    );
}

// ---------------------------------------------------------------------------------------------
// 2. DECISION TRACE — stubbed syscalls.
// ---------------------------------------------------------------------------------------------

/// Recording stubs, macro-substituted into the lifted block. The fake thread mask ("home") is
/// 0-3. `H_FAIL_AFF` / `H_FAIL_FIFO` make the matching call fail with EPERM.
const TRACE_STUBS: &str = r#"
static const char *h_isolated_path;
static const char *h_nohz_path;
static int h_fail_aff, h_fail_fifo;
static int h_setaffinity(pthread_t t, size_t n, const cpu_set_t *s)
{
	(void)t;
	(void)n;
	print_set("SETAFF", s);
	return h_fail_aff ? EPERM : 0;
}
static int h_getaffinity(pthread_t t, size_t n, cpu_set_t *s)
{
	(void)t;
	(void)n;
	CPU_ZERO(s);
	for (int c = 0; c < 4; c++)
		CPU_SET(c, s);
	printf("GETAFF\n");
	return 0;
}
static int h_setscheduler(pid_t pid, int policy, const struct sched_param *p)
{
	const int base = policy & ~SCHED_RESET_ON_FORK;
	printf("SCHED pid=%d %s%s prio=%d\n", (int)pid, base == SCHED_FIFO ? "FIFO" : base == SCHED_OTHER ? "OTHER" : "?",
	       (policy & SCHED_RESET_ON_FORK) ? "|RESET_ON_FORK" : "", p->sched_priority);
	if (base == SCHED_FIFO && h_fail_fifo) {
		errno = EPERM;
		return -1;
	}
	return 0;
}
#define pthread_setaffinity_np h_setaffinity
#define pthread_getaffinity_np h_getaffinity
#define sched_setscheduler h_setscheduler
#define GENLOCK_SYSFS_ISOLATED h_isolated_path
#define GENLOCK_SYSFS_NOHZ_FULL h_nohz_path
"#;

const TRACE_MAIN: &str = r#"
int main(int argc, char **argv)
{
	if (argc != 5)
		return 2;
	h_isolated_path = argv[1];
	h_nohz_path = argv[2];
	h_fail_aff = atoi(argv[3]);
	h_fail_fifo = atoi(argv[4]);
	printf("== START\n");
	genlock_pin_render_tick_thread();
	for (int tick = 0; tick < 2; tick++) {
		printf("== TICK\n");
		genlock_tick_pin_sleep_begin();
		printf("SLEEP\n");
		genlock_tick_pin_sleep_end();
	}
	return 0;
}
"#;

fn trace_harness(dir: &Scratch) -> PathBuf {
    let mut c = String::from(PRELUDE);
    c.push_str(TRACE_STUBS);
    c.push_str(&lifted_block());
    c.push_str(TRACE_MAIN);
    compile(dir, &c)
}

/// Run the trace harness for one sysfs state; returns (startup lines, tick lines of ONE tick).
fn trace(
    isolated: &str,
    nohz: &str,
    fail_aff: bool,
    fail_fifo: bool,
) -> (Vec<String>, Vec<String>) {
    let dir = Scratch::new("trace");
    let bin = trace_harness(&dir);
    let iso = dir.file("isolated", isolated);
    let nohz_f = dir.file("nohz_full", nohz);
    let out = run(
        &bin,
        &[
            iso.to_str().unwrap(),
            nohz_f.to_str().unwrap(),
            if fail_aff { "1" } else { "0" },
            if fail_fifo { "1" } else { "0" },
        ],
    );
    let sections: Vec<Vec<String>> = out
        .split("== ")
        .skip(1)
        .map(|s| s.lines().skip(1).map(str::to_string).collect())
        .collect();
    assert_eq!(sections.len(), 3, "START + two ticks:\n{out}");
    assert_eq!(
        sections[1], sections[2],
        "every tick must do the same thing:\n{out}"
    );
    (sections[0].clone(), sections[1].clone())
}

fn syscalls(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|l| !l.starts_with("LOG "))
        .cloned()
        .collect()
}

#[test]
fn no_isolated_core_means_no_pin_syscall_and_one_not_pinned_line() {
    // The strih-lx state (27.9.2026), and each "one side empty" state. The old code pinned {10,11}.
    for (iso, nohz) in [
        ("\n", "\n"),
        ("2-11\n", "\n"),
        ("\n", "10,11\n"),
        ("2-11\n", "(null)\n"),
    ] {
        let (start, tick) = trace(iso, nohz, false, false);
        assert!(
            syscalls(&start).is_empty(),
            "isolated={iso:?} nohz_full={nohz:?}: no affinity or scheduler call at startup, got \
             {start:?}"
        );
        assert_eq!(
            tick,
            vec!["SLEEP".to_string()],
            "isolated={iso:?} nohz_full={nohz:?}: an unpinned tick makes no syscall"
        );
        let not_pinned: Vec<&String> = start
            .iter()
            .filter(|l| l.contains("render-tick thread not pinned: no isolated cores"))
            .collect();
        assert_eq!(
            not_pinned.len(),
            1,
            "exactly one not-pinned line: {start:?}"
        );
        assert!(
            not_pinned[0].starts_with("LOG 300 "),
            "at LOG_INFO: {start:?}"
        );
        assert!(
            start.len() == 1,
            "the not-pinned line is the only startup output: {start:?}"
        );
    }
}

#[test]
fn the_pin_is_held_only_around_the_sleep_fifo_inside_the_narrowed_mask() {
    let (start, tick) = trace("2-11\n", "10-11\n", false, false);
    let fifo = "SCHED pid=0 FIFO|RESET_ON_FORK prio=10".to_string();
    let other = "SCHED pid=0 OTHER|RESET_ON_FORK prio=0".to_string();
    assert_eq!(
        syscalls(&start),
        vec![
            "GETAFF".to_string(),
            "SETAFF 10,11".to_string(),
            fifo.clone(),
            other.clone(),
            "SETAFF 0,1,2,3".to_string(),
        ],
        "startup: save the mask, try the pin + FIFO once, then return to the saved mask"
    );
    assert!(
        start
            .iter()
            .any(|l| l.contains("render-tick thread set SCHED_FIFO prio 10 on the isolated core")),
        "the success line drift-guard reads must stay: {start:?}"
    );
    assert_eq!(
        tick,
        vec![
            "SETAFF 10,11".to_string(),
            fifo,
            "SLEEP".to_string(),
            other,
            "SETAFF 0,1,2,3".to_string(),
        ],
        "each tick: narrow, FIFO, sleep, SCHED_OTHER, widen — never FIFO on the shared mask, and \
         never pinned while the tick works (and creates threads)"
    );
}

#[test]
fn without_an_rtprio_grant_the_tick_is_pinned_but_stays_sched_other() {
    let (start, tick) = trace("2-11\n", "10-11\n", false, true);
    assert!(
        start.iter().any(|l| l.starts_with("LOG 200 ")
            && l.contains("could NOT set render-tick thread SCHED_FIFO")
            && l.contains("continuing SCHED_OTHER")),
        "the FIFO failure is a WARNING and the failure line drift-guard reads stays: {start:?}"
    );
    assert_eq!(
        tick,
        vec![
            "SETAFF 10,11".to_string(),
            "SLEEP".to_string(),
            "SETAFF 0,1,2,3".to_string(),
        ],
        "no FIFO attempt per tick once it failed at startup"
    );
}

#[test]
fn an_affinity_failure_leaves_the_tick_unpinned() {
    let (start, tick) = trace("2-11\n", "10-11\n", true, false);
    assert!(
        start.iter().any(|l| l.starts_with("LOG 200 ")
            && l.contains("could NOT pin render-tick thread")
            && l.contains("continuing SCHED_OTHER")),
        "{start:?}"
    );
    assert!(
        !syscalls(&start).iter().any(|l| l.starts_with("SCHED")),
        "no FIFO without the pin: {start:?}"
    );
    assert_eq!(
        tick,
        vec!["SLEEP".to_string()],
        "no per-tick pin after the failure"
    );
}

// ---------------------------------------------------------------------------------------------
// 3. INHERITANCE — real threads, real affinity.
// ---------------------------------------------------------------------------------------------

const INHERIT_MAIN: &str = r#"
static void *child(void *arg)
{
	cpu_set_t s;
	CPU_ZERO(&s);
	if (pthread_getaffinity_np(pthread_self(), sizeof(s), &s) != 0)
		return NULL;
	char tag[64];
	snprintf(tag, sizeof(tag), "CHILD %s policy=%d mask", (const char *)arg, sched_getscheduler(0));
	print_set(tag, &s);
	return NULL;
}
static void spawn(const char *label)
{
	pthread_t t;
	if (pthread_create(&t, NULL, child, (void *)label) != 0) {
		printf("SPAWN-FAILED %s\n", label);
		return;
	}
	pthread_join(t, NULL);
}
static void *tick(void *arg)
{
	(void)arg;
	cpu_set_t s;
	CPU_ZERO(&s);
	pthread_getaffinity_np(pthread_self(), sizeof(s), &s);
	print_set("HOME", &s);
	genlock_pin_render_tick_thread();
	printf("ARMED %d FIFO %d\n", genlock_tick_pin.armed ? 1 : 0, genlock_tick_pin.fifo ? 1 : 0);
	genlock_tick_pin_sleep_begin();
	spawn("inside-sleep-window");
	genlock_tick_pin_sleep_end();
	spawn("after-wake");
	CPU_ZERO(&s);
	pthread_getaffinity_np(pthread_self(), sizeof(s), &s);
	print_set("TICK-AFTER", &s);
	return NULL;
}
int main(int argc, char **argv)
{
	if (argc != 3)
		return 2;
	h_isolated_path = argv[1];
	h_nohz_path = argv[2];
	pthread_t t;
	if (pthread_create(&t, NULL, tick, NULL) != 0)
		return 3;
	pthread_join(t, NULL);
	return 0;
}
"#;

fn field<'a>(out: &'a str, prefix: &str) -> &'a str {
    out.lines()
        .find_map(|l| l.strip_prefix(prefix))
        .unwrap_or_else(|| panic!("no `{prefix}` line in:\n{out}"))
        .trim()
}

#[test]
fn a_thread_created_after_the_tick_wakes_keeps_the_process_mask_and_sched_other() {
    let status = fs::read_to_string("/proc/self/status").expect("read /proc/self/status");
    let allowed = status
        .lines()
        .find_map(|l| l.strip_prefix("Cpus_allowed_list:"))
        .expect("Cpus_allowed_list in /proc/self/status")
        .trim()
        .to_string();
    let cpus = parse_cpulist(&allowed);
    assert!(
        cpus.len() >= 2,
        "issue 1357: this gate needs at least two usable CPUs to tell the pin mask from the process \
         mask (Cpus_allowed_list={allowed}); it must FAIL rather than skip on a one-CPU runner"
    );
    let pin_cpu = *cpus.last().unwrap();

    let dir = Scratch::new("inherit");
    let mut c = String::from(PRELUDE);
    c.push_str(
        "static const char *h_isolated_path;\nstatic const char *h_nohz_path;\n\
         #define GENLOCK_SYSFS_ISOLATED h_isolated_path\n#define GENLOCK_SYSFS_NOHZ_FULL h_nohz_path\n",
    );
    c.push_str(&lifted_block());
    c.push_str(INHERIT_MAIN);
    let bin = compile(&dir, &c);
    let iso = dir.file("isolated", &format!("{pin_cpu}\n"));
    let nohz = dir.file("nohz_full", &format!("{pin_cpu}\n"));
    let out = run(&bin, &[iso.to_str().unwrap(), nohz.to_str().unwrap()]);

    let home = field(&out, "HOME");
    assert_eq!(
        parse_cpulist(home),
        cpus,
        "the tick thread starts with the process mask:\n{out}"
    );
    assert!(
        out.lines().any(|l| l.starts_with("ARMED 1 ")),
        "the pin must arm on a box whose isolated+nohz_full core is usable:\n{out}"
    );
    let inside = field(&out, "CHILD inside-sleep-window policy=0 mask");
    assert_eq!(
        inside,
        pin_cpu.to_string(),
        "CONTROL: a thread created inside the pinned window inherits the pin mask — if it does \
         not, this harness cannot see inheritance and the next assertion proves nothing:\n{out}"
    );
    let after = field(&out, "CHILD after-wake policy=0 mask");
    assert_eq!(
        parse_cpulist(after),
        cpus,
        "issue 1357: a thread the render tick creates after it woke up must have the PROCESS mask \
         (and SCHED_OTHER), never the pin — the strih-lx 10-11 leak:\n{out}"
    );
    assert_eq!(
        parse_cpulist(field(&out, "TICK-AFTER")),
        cpus,
        "the tick thread itself is back on the process mask while it works:\n{out}"
    );
}
