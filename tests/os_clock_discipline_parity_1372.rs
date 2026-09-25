//! Issue 1372 part A — EXECUTABLE C-vs-Rust parity gate for the disciplined Windows media clock.
//!
//! `os_gettime_ns()` in `vendor/obs-studio/libobs/util/platform-windows.c` integrates QPC deltas
//! scaled by the system-time rate dantesync applies (`GetSystemTimeAdjustmentPrecise`, rate
//! `inc / adj`). `os_sleepto_ns()` waits on that same clock. Both only compile on the Windows CI
//! runner, so this gate buys the verification back on Linux:
//!
//! 1. It lifts the WHOLE `camera-box issue 1372 BEGIN … END` block VERBATIM (the pure helpers,
//!    the static state, the runtime API resolve, the sequence-counter snapshot and
//!    `os_gettime_ns` itself), `os_sleepto_ns`, and the raw-QPC timestamp mapper from
//!    `util/windows/qpc-timestamp.h`. Nothing is retyped.
//! 2. It compiles them with `cc -Wall -Wextra -Wconversion -Werror` against a FAKE Win32 layer:
//!    a scripted QPC, a scripted adjustment API and a `Sleep` that advances the fake QPC.
//! 3. It drives scenarios (live-like dantesync steering, a rate flip between polls, disabled /
//!    missing API, clamping, odd QPC frequencies, a sleep on a clock 100 ms ahead of raw QPC, a
//!    WASAPI raw-QPC stamp). It requires the C output to equal the Tier-0 authority
//!    `src/os_clock_discipline.rs` exactly, read by read.
//! 4. It compiles the same block a SECOND time against a threaded fake (pthreads, GCC atomics,
//!    `CLOCK_MONOTONIC` as QPC, the rate flipping ±1000 ppm on every 250 µs poll) and requires
//!    that no thread ever reads a value below one another thread already returned.
//!
//! The Rust authority is included by `#[path]`, so this file is std-only. It runs under
//! `cargo test` in CI AND standalone, with no cargo, via the vendored-libobs Tier-0 recipe:
//!
//! ```text
//! CARGO_MANIFEST_DIR=<worktree-abs> rustc --test --edition 2021 tests/os_clock_discipline_parity_1372.rs -o /tmp/t && /tmp/t
//! ```
//!
//! `cc` is required. Per the project's test-strictness rule this FAILS LOUDLY rather than
//! skipping when the toolchain is missing.

#[allow(dead_code)]
#[path = "../src/os_clock_discipline.rs"]
mod os_clock_discipline;

use os_clock_discipline::{map_raw_qpc_ns, mul_div64, DisciplinedClock, NS_PER_SEC};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const PLATFORM_WINDOWS: &str = "vendor/obs-studio/libobs/util/platform-windows.c";
const UTIL_UINT64: &str = "vendor/obs-studio/libobs/util/util_uint64.h";
const QPC_TIMESTAMP_H: &str = "vendor/obs-studio/libobs/util/windows/qpc-timestamp.h";
const BEGIN: &str = "/* camera-box issue 1372 BEGIN";
const END: &str = "/* camera-box issue 1372 END */";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn platform_src() -> String {
    let p = repo(PLATFORM_WINDOWS);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The verbatim `camera-box issue 1372 BEGIN … END` block.
fn lift_block(src: &str) -> String {
    let start = src.find(BEGIN).unwrap_or_else(|| {
        panic!(
            "issue 1372: {PLATFORM_WINDOWS} has no `{BEGIN}` block — the disciplined \
             os_gettime_ns() is gone and Windows OBS runs on raw QPC again, off the Dante tick"
        )
    });
    let end = src[start..]
        .find(END)
        .map(|i| start + i + END.len())
        .unwrap_or_else(|| panic!("issue 1372: the BEGIN block has no `{END}` marker"));
    src[start..end].to_string()
}

/// The verbatim definition of one C function: its signature line → the first `\n}\n`.
fn lift_fn(src: &str, sig: &str, file: &str) -> String {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("issue 1372: {file} lost `{sig}`"));
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .unwrap_or_else(|| panic!("issue 1372: `{sig}` has no closing brace"));
    src[start..end].to_string()
}

/// Everything the harnesses compile, lifted verbatim from the shipped files.
fn lifted_c() -> String {
    let src = platform_src();
    let qpc_h = fs::read_to_string(repo(QPC_TIMESTAMP_H))
        .unwrap_or_else(|e| panic!("issue 1372: read {QPC_TIMESTAMP_H}: {e}"));
    let mut c = String::from("\n/* ---- lifted VERBATIM from platform-windows.c ---- */\n");
    c.push_str(&lift_block(&src));
    c.push('\n');
    c.push_str(&lift_fn(
        &src,
        "bool os_sleepto_ns(uint64_t time_target)\n{",
        PLATFORM_WINDOWS,
    ));
    c.push_str("\n/* ---- lifted VERBATIM from util/windows/qpc-timestamp.h ---- */\n");
    c.push_str(&lift_fn(
        &qpc_h,
        "static inline uint64_t os_qpc_ns_map_to_gettime_ns(",
        QPC_TIMESTAMP_H,
    ));
    c.push_str(&lift_fn(
        &qpc_h,
        "static inline uint64_t os_raw_qpc_100ns_to_gettime_ns(",
        QPC_TIMESTAMP_H,
    ));
    c
}

/// The fake Win32 layer the lifted code compiles and runs against.
fn fake_win32() -> String {
    let util = repo(UTIL_UINT64);
    format!(
        r#"#include <stdint.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>
#include "{util}"

#define WINAPI
#define CALLBACK
#define TRUE 1
#define FALSE 0
#define UNUSED_PARAMETER(param) (void)param
typedef int BOOL;
typedef BOOL *PBOOL;
typedef unsigned long DWORD;
typedef uint64_t DWORD64;
typedef DWORD64 *PDWORD64;
typedef long LONG;
typedef long long LONG64;
typedef long long LONGLONG;
typedef void *PVOID;
typedef void *HMODULE;
typedef void (*FARPROC)(void);
typedef struct {{ long long QuadPart; }} LARGE_INTEGER;
typedef struct {{ int done; }} INIT_ONCE, *PINIT_ONCE;
#define INIT_ONCE_STATIC_INIT {{0}}
typedef BOOL(CALLBACK *PINIT_ONCE_FN)(PINIT_ONCE, PVOID, PVOID *);
#define YieldProcessor() ((void)0)
#define MemoryBarrier() __atomic_thread_fence(__ATOMIC_SEQ_CST)

static uint64_t g_freq = 10000000;
static uint64_t g_qpc = 0;
static uint64_t g_qpc_step = 0;
static uint64_t g_slept_ms = 0;
static int g_api_present = 1;
static DWORD64 g_adj = 0, g_inc = 0;
static BOOL g_dis = TRUE;
static int g_polls = 0;

static uint64_t get_clockfreq(void) {{ return g_freq; }}
static BOOL QueryPerformanceCounter(LARGE_INTEGER *c)
{{
	c->QuadPart = (long long)g_qpc;
	g_qpc += g_qpc_step;
	return TRUE;
}}
static void Sleep(DWORD ms)
{{
	g_qpc += (uint64_t)ms * g_freq / 1000;
	g_slept_ms += ms;
}}
static BOOL QueryPerformanceFrequency(LARGE_INTEGER *f)
{{
	f->QuadPart = (long long)g_freq;
	return TRUE;
}}
static BOOL SwitchToThread(void) {{ return TRUE; }}
static LONG64 InterlockedIncrement64(LONG64 volatile *d)
{{
	*d += 1;
	return *d;
}}
static BOOL InitOnceExecuteOnce(PINIT_ONCE o, PINIT_ONCE_FN fn, PVOID p, PVOID *ctx)
{{
	if (o->done) return TRUE;
	o->done = 1;
	return fn(o, p, ctx);
}}
static LONG InterlockedCompareExchange(LONG volatile *d, LONG x, LONG c)
{{
	LONG old = *d;
	if (old == c) *d = x;
	return old;
}}
static LONG InterlockedExchange(LONG volatile *d, LONG v)
{{
	LONG old = *d;
	*d = v;
	return old;
}}
static BOOL WINAPI fake_get_adjustment(PDWORD64 adj, PDWORD64 inc, PBOOL disabled)
{{
	g_polls++;
	*adj = g_adj;
	*inc = g_inc;
	*disabled = g_dis;
	return TRUE;
}}
static HMODULE GetModuleHandleW(const wchar_t *name)
{{
	return wcscmp(name, L"kernelbase.dll") == 0 ? (HMODULE)&g_api_present : NULL;
}}
static FARPROC GetProcAddress(HMODULE mod, const char *name)
{{
	(void)mod;
	if (!g_api_present || strcmp(name, "GetSystemTimeAdjustmentPrecise") != 0)
		return NULL;
	return (FARPROC)fake_get_adjustment;
}}
uint64_t os_gettime_ns(void);
"#,
        util = util.display()
    )
}

/// One scripted read: set the fake QPC + adjustment, call `os_gettime_ns()`.
#[derive(Clone, Copy, Debug)]
struct Step {
    qpc: u64,
    adj: u64,
    inc: u64,
    disabled: bool,
}

struct Scenario {
    name: &'static str,
    freq: u64,
    api_present: bool,
    steps: Vec<Step>,
}

fn lcg(x: &mut u64) -> u64 {
    *x = x
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *x >> 33
}

fn scenarios() -> Vec<Scenario> {
    const F: u64 = 10_000_000;
    let mut out = Vec::new();

    // 1. Live-like: 5 days of uptime, dantesync re-steers every ~1 s within ±20 ppm (the
    //    win-resolume 25.9.2026 range), OBS reads every ~7-40 ms for 90 s.
    let mut x = 0x1372_0925_2026_0001u64;
    let mut steps = Vec::new();
    let mut q = 5 * 86_400 * F;
    let mut adj = F;
    let mut next_steer = q;
    while q < 5 * 86_400 * F + 90 * F {
        if q >= next_steer {
            adj = F - 200 + lcg(&mut x) % 401; // ±20 ppm
            next_steer = q + F - 5_000 + lcg(&mut x) % 10_000;
        }
        steps.push(Step {
            qpc: q,
            adj,
            inc: F,
            disabled: false,
        });
        q += 70_000 + lcg(&mut x) % 330_000;
    }
    out.push(Scenario {
        name: "live",
        freq: F,
        api_present: true,
        steps,
    });

    // 2. The rate flips BETWEEN polls, at odd counts, both directions.
    let mut steps = Vec::new();
    let mut q = 1_000 * F + 3;
    for i in 0..4000u64 {
        let adj = match (i / 170) % 3 {
            0 => F - 100,
            1 => F + 100,
            _ => F - 7,
        };
        steps.push(Step {
            qpc: q,
            adj,
            inc: F,
            disabled: false,
        });
        q += 9_973; // ~1 ms, never a multiple of the poll period
    }
    out.push(Scenario {
        name: "flip",
        freq: F,
        api_present: true,
        steps,
    });

    // 2b. A constant +19.1 ppm (the live win-resolume adj 9_999_809), read every 100 ms for 60 s.
    out.push(Scenario {
        name: "const",
        freq: F,
        api_present: true,
        steps: (0..=600u64)
            .map(|i| Step {
                qpc: 40 * F + i * F / 10,
                adj: 9_999_809,
                inc: F,
                disabled: false,
            })
            .collect(),
    });

    // 3. Adjustment DISABLED (no dantesync) — stock raw QPC.
    out.push(Scenario {
        name: "disabled",
        freq: F,
        api_present: true,
        steps: (0..600u64)
            .map(|i| Step {
                qpc: 77 * F + i * 1_234_567,
                adj: F - 50,
                inc: F,
                disabled: true,
            })
            .collect(),
    });

    // 4. The API does not exist (old Windows) — stock raw QPC.
    out.push(Scenario {
        name: "absent",
        freq: F,
        api_present: false,
        steps: (0..600u64)
            .map(|i| Step {
                qpc: 12 * F + i * 3_333_331,
                adj: F - 50,
                inc: F,
                disabled: false,
            })
            .collect(),
    });

    // 5. Insane adjustments are clamped to ±1000 ppm, then a normal one again.
    let mut steps = Vec::new();
    let mut q = 10 * F;
    for (i, adj) in [1u64, u64::MAX, 0, F + 5 * F / 1000, F - 3, F]
        .iter()
        .enumerate()
    {
        for k in 0..40u64 {
            steps.push(Step {
                qpc: q,
                adj: *adj,
                inc: F,
                disabled: false,
            });
            q += F / 20 + (i as u64) * 17 + k;
        }
    }
    out.push(Scenario {
        name: "clamp",
        freq: F,
        api_present: true,
        steps,
    });

    // 6. Other QPC frequencies (a 24 MHz crystal, the 3.579545 MHz ACPI PM timer, a 2.9 GHz TSC)
    //    with inc == freq, plus an inc that is NOT the QPC frequency.
    for (name, freq, inc) in [
        ("f24m", 24_000_000u64, 24_000_000u64),
        ("fpm", 3_579_545, 3_579_545),
        ("ftsc", 2_918_400_000, 2_918_400_000),
        ("inc-ne-freq", 10_000_000, 15_625_000),
    ] {
        let mut x = freq ^ inc;
        let mut steps = Vec::new();
        let mut q = 3 * 3_600 * freq + 11;
        let mut adj = inc;
        for i in 0..1500u64 {
            if i % 97 == 0 {
                let dev = inc / 50_000; // ±20 ppm
                adj = inc - dev + lcg(&mut x) % (2 * dev + 1);
            }
            steps.push(Step {
                qpc: q,
                adj,
                inc,
                disabled: false,
            });
            q += freq / 100 + lcg(&mut x) % (freq / 50);
        }
        out.push(Scenario {
            name,
            freq,
            api_present: true,
            steps,
        });
    }
    out
}

/// The Rust authority's answer for a scenario, read by read.
fn rust_run(s: &Scenario) -> Vec<u64> {
    let mut c = DisciplinedClock::new(s.freq);
    s.steps
        .iter()
        .map(|st| {
            let adj = s.api_present.then_some((st.adj, st.inc, st.disabled));
            c.now(st.qpc, adj)
        })
        .collect()
}

/// The sleep scenario: the disciplined clock runs +1000 ppm (clamped) for 100 s, so it is
/// 100 ms AHEAD of raw QPC. `os_sleepto_ns(now + 10 ms)` must wake 10 ms later on THAT clock.
/// The pre-fix version converted the target to raw QPC counts and slept ~110 ms.
const SLEEP_ADJ: u64 = 9_990_000; // clamped to inc - inc/1000 → +1000 ppm
const SLEEP_TARGET_AHEAD_NS: u64 = 10_000_000;

fn harness_main(scs: &[Scenario]) -> String {
    let mut c = String::from("\nint main(int argc, char **argv)\n{\n\tif (argc < 2) return 2;\n");
    for s in scs {
        let n = s.steps.len();
        let q: Vec<String> = s.steps.iter().map(|x| format!("{}ULL", x.qpc)).collect();
        let a: Vec<String> = s.steps.iter().map(|x| format!("{}ULL", x.adj)).collect();
        let inc: Vec<String> = s.steps.iter().map(|x| format!("{}ULL", x.inc)).collect();
        let d: Vec<String> = s
            .steps
            .iter()
            .map(|x| if x.disabled { "1" } else { "0" }.to_string())
            .collect();
        c.push_str(&format!(
            "\tif (strcmp(argv[1], \"{name}\") == 0) {{\n\
             \t\tstatic const uint64_t q[{n}] = {{{q}}};\n\
             \t\tstatic const uint64_t a[{n}] = {{{a}}};\n\
             \t\tstatic const uint64_t inc[{n}] = {{{inc}}};\n\
             \t\tstatic const int d[{n}] = {{{d}}};\n\
             \t\tg_freq = {freq}ULL;\n\
             \t\tg_api_present = {api};\n\
             \t\tfor (size_t i = 0; i < {n}; i++) {{\n\
             \t\t\tg_qpc = q[i]; g_adj = a[i]; g_inc = inc[i]; g_dis = d[i];\n\
             \t\t\tprintf(\"%llu\\n\", (unsigned long long)os_gettime_ns());\n\
             \t\t}}\n\
             \t\tprintf(\"END polls=%d seq_odd=%d polling=%ld\\n\", g_polls, (int)(os_clk_seq & 1), (long)os_clk_polling);\n\
             \t\treturn 0;\n\
             \t}}\n",
            name = s.name,
            q = q.join(","),
            a = a.join(","),
            inc = inc.join(","),
            d = d.join(","),
            freq = s.freq,
            api = i32::from(s.api_present),
        ));
    }
    c.push_str(&format!(
        "\tif (strcmp(argv[1], \"sleep\") == 0) {{\n\
         \t\tg_freq = 10000000ULL; g_adj = {SLEEP_ADJ}ULL; g_inc = 10000000ULL; g_dis = FALSE;\n\
         \t\tg_qpc = 0; (void)os_gettime_ns();\n\
         \t\tg_qpc = 1000000000ULL; /* 100 s later */\n\
         \t\tconst uint64_t now = os_gettime_ns();\n\
         \t\tconst uint64_t target = now + {SLEEP_TARGET_AHEAD_NS}ULL;\n\
         \t\tg_qpc_step = 50; /* each counter read advances 5 us */\n\
         \t\tconst bool stall = os_sleepto_ns(target);\n\
         \t\tg_qpc_step = 0;\n\
         \t\tconst uint64_t wake_qpc = g_qpc;\n\
         \t\tconst uint64_t after = os_gettime_ns();\n\
         \t\tconst bool stall_past = os_sleepto_ns(now);\n\
         \t\tprintf(\"%llu %llu %d %llu %llu %llu %d %d\\n\", (unsigned long long)now, (unsigned long long)target, stall ? 1 : 0, (unsigned long long)g_slept_ms, (unsigned long long)wake_qpc, (unsigned long long)after, stall_past ? 1 : 0, (int)(os_clk_seq & 1));\n\
         \t\treturn 0;\n\
         \t}}\n"
    ));
    c.push_str(&format!(
        "\tif (strcmp(argv[1], \"wasapi\") == 0) {{\n\
         \t\tg_freq = 10000000ULL; g_adj = {SLEEP_ADJ}ULL; g_inc = 10000000ULL; g_dis = FALSE;\n\
         \t\tg_qpc = 0; (void)os_gettime_ns();\n\
         \t\tg_qpc = 1000000000ULL; /* 100 s later: the disciplined clock is 100 ms ahead */\n\
         \t\tconst uint64_t now = os_gettime_ns();\n\
         \t\tconst uint64_t raw_now_100ns = g_qpc; /* 10 MHz: one count is 100 ns */\n\
         \t\tconst uint64_t stamp = raw_now_100ns - {WASAPI_AGE_100NS}ULL;\n\
         \t\tprintf(\"%llu %llu %llu\\n\", (unsigned long long)now, (unsigned long long)os_raw_qpc_100ns_to_gettime_ns(stamp), (unsigned long long)(stamp * 100));\n\
         \t\treturn 0;\n\
         \t}}\n"
    ));
    c.push_str("\treturn 3;\n}\n");
    c
}

/// The WASAPI scenario: a device stamp 10 ms old on the raw QPC timeline.
const WASAPI_AGE_100NS: u64 = 100_000;

/// A per-build scratch dir, removed on drop. pid + an in-process counter: unique across processes
/// and across this binary's parallel test threads (never pid + timestamp).
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let base = option_env!("CARGO_TARGET_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let d = base.join(format!(
            "os_clock_parity_1372_{}_{}_{tag}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&d).expect("create the parity scratch dir");
        Scratch(d)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Compile `c` with the gate's flags plus `extra`; return the binary path inside `dir`.
fn compile(dir: &Scratch, c: &str, extra: &[&str]) -> PathBuf {
    let cfile = dir.0.join("os_clock.c");
    let bin = dir.0.join("os_clock.bin");
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
        ])
        .args(extra)
        .arg(&cfile)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "issue 1372: could not run the C compiler `{cc}` ({e}). This gate must FAIL \
                 rather than skip when the toolchain is absent. Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "issue 1372: the lifted os_gettime_ns / os_sleepto_ns / qpc-timestamp.h do NOT COMPILE \
         against the fake Win32 layer under -Wall -Wextra -Wconversion -Werror:\n--- cc stderr \
         ---\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    bin
}

/// The single-threaded scripted harness.
fn build_harness(dir: &Scratch, scs: &[Scenario]) -> PathBuf {
    let mut c = fake_win32();
    c.push_str(&lifted_c());
    c.push_str(&harness_main(scs));
    compile(dir, &c, &["-O1"])
}

fn run(bin: &Path, arg: &str) -> String {
    let out = Command::new(bin)
        .arg(arg)
        .output()
        .unwrap_or_else(|e| panic!("issue 1372: harness `{arg}` failed to execute: {e}"));
    assert!(
        out.status.success(),
        "issue 1372: harness `{arg}` exited {:?}",
        out.status
    );
    String::from_utf8(out.stdout).expect("harness stdout is utf-8")
}

#[test]
fn c_os_gettime_ns_matches_the_rust_authority_read_by_read() {
    let scs = scenarios();
    let dir = Scratch::new("reads");
    let bin = build_harness(&dir, &scs);
    for s in &scs {
        let stdout = run(&bin, s.name);
        let mut lines: Vec<&str> = stdout.lines().collect();
        let tail = lines.pop().expect("harness printed nothing");
        let c_vals: Vec<u64> = lines
            .iter()
            .map(|l| l.parse().expect("a u64 per read"))
            .collect();
        let rs_vals = rust_run(s);
        assert_eq!(c_vals.len(), rs_vals.len(), "{}: read count", s.name);
        let diffs: Vec<String> = c_vals
            .iter()
            .zip(&rs_vals)
            .enumerate()
            .filter(|(_, (c, r))| c != r)
            .take(8)
            .map(|(i, (c, r))| format!("read {i} qpc {}: C {c} vs Rust {r}", s.steps[i].qpc))
            .collect();
        assert!(
            diffs.is_empty(),
            "issue 1372 `{}`: the shipped C os_gettime_ns diverges from \
             src/os_clock_discipline.rs:\n{}",
            s.name,
            diffs.join("\n")
        );
        // Monotonic, read by read.
        for w in c_vals.windows(2) {
            assert!(
                w[1] >= w[0],
                "{}: C clock stepped back {} -> {}",
                s.name,
                w[0],
                w[1]
            );
        }
        // The writer left the sequence even and released the single-poller flag.
        assert!(
            tail.contains("seq_odd=0 polling=0"),
            "issue 1372 `{}`: the writer left the sequence odd or kept the poller flag: {tail}",
            s.name
        );
        // The adjustment is polled ~every 250 ms, never on every read.
        let polls: usize = tail
            .split_whitespace()
            .find_map(|t| t.strip_prefix("polls="))
            .and_then(|v| v.parse().ok())
            .expect("polls=");
        if s.api_present && s.steps.len() > 100 {
            assert!(
                polls > 1 && polls < s.steps.len() / 2,
                "issue 1372 `{}`: {polls} adjustment polls for {} reads — the 250 ms throttle is \
                 broken",
                s.name,
                s.steps.len()
            );
        }
    }
}

#[test]
fn the_rate_follows_the_adjustment_and_rate_one_without_it() {
    let scs = scenarios();
    let dir = Scratch::new("rate");
    let bin = build_harness(&dir, &scs);
    // Disabled / absent = exactly the old raw-QPC nanoseconds.
    for name in ["disabled", "absent"] {
        let s = scs.iter().find(|s| s.name == name).unwrap();
        let stdout = run(&bin, name);
        for (st, line) in s.steps.iter().zip(stdout.lines()) {
            let v: u64 = line.parse().unwrap();
            assert_eq!(
                v,
                mul_div64(st.qpc, NS_PER_SEC, s.freq),
                "issue 1372 `{name}`: without a usable adjustment os_gettime_ns must be raw QPC"
            );
        }
    }
    // A constant adj 9_999_809 (inc 10_000_000): a LARGER adj is SLOWER, so the clock must run
    // +19.1 ppm vs raw QPC (rate = inc/adj), never -19.1 (the adj/inc reading).
    let s = scs.iter().find(|s| s.name == "const").unwrap();
    let stdout = run(&bin, "const");
    let vals: Vec<u64> = stdout.lines().filter_map(|l| l.parse().ok()).collect();
    let (q0, q1) = (s.steps[0].qpc, s.steps[s.steps.len() - 1].qpc);
    let raw = (mul_div64(q1, NS_PER_SEC, s.freq) - mul_div64(q0, NS_PER_SEC, s.freq)) as f64;
    let disc = (vals[vals.len() - 1] - vals[0]) as f64;
    let ppm_c = (disc - raw) / raw * 1e6;
    assert!(
        (ppm_c - 19.1).abs() < 0.01,
        "issue 1372: with adj 9_999_809 / inc 10_000_000 the disciplined clock ran \
         {ppm_c:+.3} ppm vs raw QPC; dantesync's adjustment means +19.1 ppm (rate = inc/adj)"
    );
}

#[test]
fn os_sleepto_ns_waits_on_the_disciplined_clock() {
    let dir = Scratch::new("sleep");
    let bin = build_harness(&dir, &scenarios());
    let stdout = run(&bin, "sleep");
    let f: Vec<u64> = stdout
        .split_whitespace()
        .map(|t| t.parse().expect("sleep fields"))
        .collect();
    let (now, target, stall, slept_ms, wake_qpc, after, stall_past, seq_odd) =
        (f[0], f[1], f[2], f[3], f[4], f[5], f[6], f[7]);
    assert_eq!(target, now + SLEEP_TARGET_AHEAD_NS);
    // The Rust authority for the same clock: +1000 ppm (clamped), 100 s in → 100 ms ahead.
    let mut c = DisciplinedClock::new(10_000_000);
    c.now(0, Some((SLEEP_ADJ, 10_000_000, false)));
    let rs_now = c.now(1_000_000_000, Some((SLEEP_ADJ, 10_000_000, false)));
    assert_eq!(
        now, rs_now,
        "issue 1372: the sleep scenario's clock diverges from Rust"
    );
    assert_eq!(stall, 1, "issue 1372: a future target must report a stall");
    assert_eq!(
        stall_past, 0,
        "issue 1372: a past target must return at once"
    );
    assert_eq!(seq_odd, 0, "issue 1372: the writer left the sequence odd");
    let wake_disc = c.seg.now(wake_qpc, 10_000_000);
    assert!(
        wake_disc >= target && after >= target,
        "issue 1372: os_sleepto_ns returned BEFORE the disciplined target ({wake_disc} < {target})"
    );
    assert!(
        wake_disc - target < 1_000_000 && slept_ms <= 10,
        "issue 1372: os_sleepto_ns overslept: woke {} ns past the target after Sleep({slept_ms} \
         ms) — it is waiting on RAW QPC counts, not on the disciplined os_gettime_ns clock \
         (100 ms apart here)",
        wake_disc - target
    );
}

#[test]
fn the_api_is_resolved_at_runtime_from_kernelbase() {
    // GetSystemTimeAdjustmentPrecise is NOT exported by kernel32.dll (checked live on
    // win-resolume); a static import would fail to load obs.dll there, a kernel32 lookup would
    // silently leave the clock undisciplined.
    let squish = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let block = squish(&lift_block(&platform_src()));
    assert!(
        block.contains("GetModuleHandleW(L\"kernelbase.dll\")")
            && block.contains("GetProcAddress(kernelbase, \"GetSystemTimeAdjustmentPrecise\")"),
        "issue 1372: the adjustment API must be resolved at runtime from kernelbase.dll"
    );
    assert!(
        !block.contains("GetModuleHandleW(L\"kernel32.dll\")"),
        "issue 1372: kernel32.dll does not export GetSystemTimeAdjustmentPrecise"
    );
}

#[test]
fn a_wasapi_raw_qpc_stamp_is_mapped_onto_the_disciplined_clock() {
    // WASAPI stamps capture buffers with a RAW-QPC time. With the disciplined clock 100 ms ahead
    // of raw QPC, a 10 ms-old stamp must land 10 ms before the disciplined now; `ts * 100` (the
    // stock code) would land ~110 ms before it and drift further every hour.
    let dir = Scratch::new("wasapi");
    let bin = build_harness(&dir, &scenarios());
    let stdout = run(&bin, "wasapi");
    let f: Vec<u64> = stdout
        .split_whitespace()
        .map(|t| t.parse().expect("wasapi fields"))
        .collect();
    let (now, mapped, raw_stamp_ns) = (f[0], f[1], f[2]);
    let raw_now_ns = mul_div64(1_000_000_000, NS_PER_SEC, 10_000_000);
    assert_eq!(
        mapped,
        map_raw_qpc_ns(raw_stamp_ns, raw_now_ns, now),
        "issue 1372: the C raw-QPC mapper diverges from src/os_clock_discipline.rs"
    );
    assert_eq!(mapped, now - WASAPI_AGE_100NS * 100);
    assert!(
        now - raw_stamp_ns > 100_000_000,
        "issue 1372: the scenario must put the disciplined clock ahead of raw QPC"
    );
    // The plugin really uses the mapper at both raw-QPC stamp sites.
    let wasapi = fs::read_to_string(repo("vendor/obs-studio/plugins/win-wasapi/win-wasapi.cpp"))
        .expect("read win-wasapi.cpp");
    assert_eq!(
        wasapi.matches("os_raw_qpc_100ns_to_gettime_ns(ts)").count(),
        2,
        "issue 1372: win-wasapi.cpp must map both raw-QPC stamps (process capture + device timing)"
    );
    assert!(
        !wasapi.contains("ts * 100"),
        "issue 1372: a raw `ts * 100` WASAPI stamp is back -- it drifts against os_gettime_ns()"
    );
}

/// The threaded fake: pthreads, GCC atomics, `CLOCK_MONOTONIC` as a 10 MHz QPC running 1000x
/// fast (a 250 ms poll = 250 µs of real time), and an adjustment that flips between the ±1000 ppm
/// clamp on every poll, so rebases are frequent and each one changes the rate by 2000 ppm.
fn fake_win32_threaded() -> String {
    let util = repo(UTIL_UINT64);
    format!(
        r#"#define _GNU_SOURCE
#include <stdint.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>
#include <time.h>
#include <sched.h>
#include <pthread.h>
#include "{util}"

#define WINAPI
#define CALLBACK
#define TRUE 1
#define FALSE 0
#define UNUSED_PARAMETER(param) (void)param
typedef int BOOL;
typedef BOOL *PBOOL;
typedef unsigned long DWORD;
typedef uint64_t DWORD64;
typedef DWORD64 *PDWORD64;
typedef long LONG;
typedef long long LONG64;
typedef long long LONGLONG;
typedef void *PVOID;
typedef void *HMODULE;
typedef void (*FARPROC)(void);
typedef struct {{ long long QuadPart; }} LARGE_INTEGER;
typedef struct {{ int state; }} INIT_ONCE, *PINIT_ONCE;
#define INIT_ONCE_STATIC_INIT {{0}}
typedef BOOL(CALLBACK *PINIT_ONCE_FN)(PINIT_ONCE, PVOID, PVOID *);
#define YieldProcessor() ((void)0)
#define MemoryBarrier() __atomic_thread_fence(__ATOMIC_SEQ_CST)

#define FAKE_FREQ 10000000ULL
static int g_polls = 0;

static uint64_t get_clockfreq(void) {{ return FAKE_FREQ; }}
static uint64_t real_ns(void)
{{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
}}
/* Preemption stand-in: a thread stalls at a random point, the way a real thread is descheduled.
 * Without it the race windows are nanoseconds wide and a broken ordering never shows. */
static __thread uint64_t g_rng = 0;
static __thread int g_poller = 0; /* set when this thread just read the adjustment (the writer) */
static void maybe_stall(unsigned int one_in, uint64_t stall_ns)
{{
	if (!g_rng)
		g_rng = real_ns() | 1;
	g_rng ^= g_rng << 13;
	g_rng ^= g_rng >> 7;
	g_rng ^= g_rng << 17;
	if (g_rng % one_in)
		return;
	const uint64_t until = real_ns() + stall_ns;
	while (real_ns() < until) {{
	}}
}}
static BOOL QueryPerformanceCounter(LARGE_INTEGER *c)
{{
	maybe_stall(16, 20000);
	/* 10 counts per real ns = a 10 MHz QPC running 1000x fast */
	c->QuadPart = (long long)(real_ns() * 10ULL);
	if (g_poller) {{
		/* the writer is descheduled right after sampling its rebase count */
		g_poller = 0;
		maybe_stall(1, 200000);
	}} else {{
		maybe_stall(16, 20000);
	}}
	return TRUE;
}}
static BOOL QueryPerformanceFrequency(LARGE_INTEGER *f)
{{
	f->QuadPart = (long long)FAKE_FREQ;
	return TRUE;
}}
static void Sleep(DWORD ms) {{ (void)ms; }}
static BOOL SwitchToThread(void) {{ return sched_yield() == 0; }}
static BOOL InitOnceExecuteOnce(PINIT_ONCE o, PINIT_ONCE_FN fn, PVOID p, PVOID *ctx)
{{
	int expect = 0;
	if (__atomic_compare_exchange_n(&o->state, &expect, 1, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)) {{
		BOOL r = fn(o, p, ctx);
		__atomic_store_n(&o->state, 2, __ATOMIC_SEQ_CST);
		return r;
	}}
	while (__atomic_load_n(&o->state, __ATOMIC_SEQ_CST) != 2)
		sched_yield();
	return TRUE;
}}
static LONG InterlockedCompareExchange(LONG volatile *d, LONG x, LONG c)
{{
	LONG expect = c;
	__atomic_compare_exchange_n(d, &expect, x, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
	return expect;
}}
static LONG InterlockedExchange(LONG volatile *d, LONG v) {{ return __atomic_exchange_n(d, v, __ATOMIC_SEQ_CST); }}
static LONG64 InterlockedIncrement64(LONG64 volatile *d)
{{
	const LONG64 v = __atomic_add_fetch(d, 1, __ATOMIC_SEQ_CST);
	/* the writer's closing increment: stall before it releases the poller flag */
	if ((v & 1) == 0)
		maybe_stall(2, 50000);
	return v;
}}
static BOOL WINAPI fake_get_adjustment(PDWORD64 adj, PDWORD64 inc, PBOOL disabled)
{{
	/* only the single poller calls this */
	g_polls++;
	g_poller = 1;
	*adj = (g_polls & 1) ? FAKE_FREQ - FAKE_FREQ / 1000 : FAKE_FREQ + FAKE_FREQ / 1000;
	*inc = FAKE_FREQ;
	*disabled = FALSE;
	return TRUE;
}}
static HMODULE GetModuleHandleW(const wchar_t *name)
{{
	static int present = 1;
	return wcscmp(name, L"kernelbase.dll") == 0 ? (HMODULE)&present : NULL;
}}
static FARPROC GetProcAddress(HMODULE mod, const char *name)
{{
	(void)mod;
	return strcmp(name, "GetSystemTimeAdjustmentPrecise") == 0 ? (FARPROC)fake_get_adjustment : NULL;
}}
uint64_t os_gettime_ns(void);
"#,
        util = util.display()
    )
}

const THREADED_MAIN: &str = r#"
#define THREADS 4
#define SECONDS 2
static uint64_t g_max = 0;
static uint64_t g_violations = 0;
static uint64_t g_worst = 0;
static uint64_t g_calls = 0;
static void *worker(void *arg)
{
	(void)arg;
	const uint64_t end = real_ns() + SECONDS * 1000000000ULL;
	uint64_t calls = 0;
	while (real_ns() < end) {
		for (int i = 0; i < 256; i++) {
			const uint64_t before = __atomic_load_n(&g_max, __ATOMIC_SEQ_CST);
			const uint64_t v = os_gettime_ns();
			if (v < before) {
				__atomic_add_fetch(&g_violations, 1, __ATOMIC_SEQ_CST);
				uint64_t back = before - v, w = __atomic_load_n(&g_worst, __ATOMIC_SEQ_CST);
				while (back > w && !__atomic_compare_exchange_n(&g_worst, &w, back, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)) {
				}
			}
			uint64_t cur = __atomic_load_n(&g_max, __ATOMIC_SEQ_CST);
			while (v > cur && !__atomic_compare_exchange_n(&g_max, &cur, v, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)) {
			}
			calls++;
		}
	}
	__atomic_add_fetch(&g_calls, calls, __ATOMIC_SEQ_CST);
	return NULL;
}
int main(void)
{
	pthread_t t[THREADS];
	for (int i = 0; i < THREADS; i++)
		pthread_create(&t[i], NULL, worker, NULL);
	for (int i = 0; i < THREADS; i++)
		pthread_join(t[i], NULL);
	printf("violations=%llu worst_ns=%llu calls=%llu polls=%d seq_odd=%d polling=%ld\n",
	       (unsigned long long)g_violations, (unsigned long long)g_worst, (unsigned long long)g_calls,
	       g_polls, (int)(os_clk_seq & 1), (long)os_clk_polling);
	return 0;
}
"#;

fn threaded_field(out: &str, key: &str) -> u64 {
    out.split_whitespace()
        .find_map(|t| t.strip_prefix(key))
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("issue 1372: threaded harness printed no `{key}`: {out}"))
}

#[test]
fn no_thread_ever_reads_the_clock_backwards_across_a_rebase() {
    let mut c = fake_win32_threaded();
    c.push_str(&lifted_c());
    c.push_str(THREADED_MAIN);
    let dir = Scratch::new("threads");
    let bin = compile(&dir, &c, &["-O2", "-pthread"]);
    let out = String::from_utf8(
        Command::new(&bin)
            .output()
            .expect("issue 1372: the threaded harness failed to execute")
            .stdout,
    )
    .expect("utf-8");
    eprintln!("issue 1372 threaded run: {out}");
    let violations = threaded_field(&out, "violations=");
    let polls = threaded_field(&out, "polls=");
    let calls = threaded_field(&out, "calls=");
    assert!(
        polls > 500 && calls > 20_000,
        "issue 1372: the threaded run exercised too little ({out}) -- it proves nothing"
    );
    assert_eq!(
        violations, 0,
        "issue 1372: a thread read os_gettime_ns() BELOW a value another thread had already \
         returned ({out}) -- the counter must be read inside the validated sequence window"
    );
    assert!(
        out.contains("seq_odd=0 polling=0"),
        "issue 1372: the writer left the sequence odd or kept the poller flag: {out}"
    );
}
