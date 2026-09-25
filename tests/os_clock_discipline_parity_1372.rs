//! Issue 1372 part A — EXECUTABLE C-vs-Rust parity gate for the disciplined Windows media clock.
//!
//! `os_gettime_ns()` in `vendor/obs-studio/libobs/util/platform-windows.c` integrates QPC deltas
//! scaled by the system-time rate dantesync applies (`GetSystemTimeAdjustmentPrecise`, rate
//! `inc / adj`). `os_sleepto_ns()` waits on that same clock. Both only compile on the Windows CI
//! runner, so this gate buys the verification back on Linux:
//!
//! 1. It lifts the WHOLE `camera-box issue 1372 BEGIN … END` block VERBATIM (the pure helpers,
//!    the static state, the runtime API resolve and `os_gettime_ns` itself), plus
//!    `os_sleepto_ns`, from the shipped file. Nothing is retyped.
//! 2. It compiles them with `cc -Wall -Wextra -Wconversion -Werror` against a FAKE Win32 layer:
//!    a scripted QPC, a scripted adjustment API, a `Sleep` that advances the fake QPC, and SRW
//!    locks that count and check their pairing.
//! 3. It drives scenarios (live-like dantesync steering, a rate flip between polls, disabled /
//!    missing API, clamping, odd QPC frequencies, a sleep on a clock 100 ms ahead of raw QPC).
//!    It requires the C output to equal the Tier-0 authority `src/os_clock_discipline.rs`
//!    exactly, read by read.
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

use os_clock_discipline::{mul_div64, DisciplinedClock, NS_PER_SEC};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const PLATFORM_WINDOWS: &str = "vendor/obs-studio/libobs/util/platform-windows.c";
const UTIL_UINT64: &str = "vendor/obs-studio/libobs/util/util_uint64.h";
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

/// The verbatim `os_sleepto_ns` definition (signature → first `\n}\n`).
fn lift_sleepto(src: &str) -> String {
    let sig = "bool os_sleepto_ns(uint64_t time_target)\n{";
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("issue 1372: {PLATFORM_WINDOWS} lost `{sig}`"));
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .expect("issue 1372: os_sleepto_ns has no closing brace");
    src[start..end].to_string()
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
typedef void *PVOID;
typedef void *HMODULE;
typedef void (*FARPROC)(void);
typedef struct {{ long long QuadPart; }} LARGE_INTEGER;
typedef struct {{ int unused; }} SRWLOCK;
#define SRWLOCK_INIT {{0}}
typedef struct {{ int done; }} INIT_ONCE, *PINIT_ONCE;
#define INIT_ONCE_STATIC_INIT {{0}}
typedef BOOL(CALLBACK *PINIT_ONCE_FN)(PINIT_ONCE, PVOID, PVOID *);
#define YieldProcessor() ((void)0)

static uint64_t g_freq = 10000000;
static uint64_t g_qpc = 0;
static uint64_t g_qpc_step = 0;
static uint64_t g_slept_ms = 0;
static int g_api_present = 1;
static DWORD64 g_adj = 0, g_inc = 0;
static BOOL g_dis = TRUE;
static int g_polls = 0;
static int g_shared = 0, g_excl = 0, g_lock_errors = 0;

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
static void AcquireSRWLockShared(SRWLOCK *l) {{ (void)l; if (g_excl) g_lock_errors++; g_shared++; }}
static void ReleaseSRWLockShared(SRWLOCK *l) {{ (void)l; if (g_shared <= 0) g_lock_errors++; g_shared--; }}
static void AcquireSRWLockExclusive(SRWLOCK *l) {{ (void)l; if (g_excl || g_shared) g_lock_errors++; g_excl++; }}
static void ReleaseSRWLockExclusive(SRWLOCK *l) {{ (void)l; if (g_excl != 1) g_lock_errors++; g_excl--; }}
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
             \t\tprintf(\"END polls=%d lock_errors=%d shared=%d excl=%d polling=%ld\\n\", g_polls, g_lock_errors, g_shared, g_excl, (long)os_clk_polling);\n\
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
         \t\tprintf(\"%llu %llu %d %llu %llu %llu %d %d\\n\", (unsigned long long)now, (unsigned long long)target, stall ? 1 : 0, (unsigned long long)g_slept_ms, (unsigned long long)wake_qpc, (unsigned long long)after, stall_past ? 1 : 0, g_lock_errors);\n\
         \t\treturn 0;\n\
         \t}}\n"
    ));
    c.push_str("\treturn 3;\n}\n");
    c
}

fn scratch_dir() -> PathBuf {
    let base = option_env!("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let d = base.join(format!("os_clock_parity_1372_{}", std::process::id()));
    fs::create_dir_all(&d).expect("create the parity scratch dir");
    d
}

/// The harness binary, compiled once per test process (the tests run in parallel threads).
fn harness() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| build_harness(&scenarios()))
}

/// Compile the lifted C; return the binary path.
fn build_harness(scs: &[Scenario]) -> PathBuf {
    let src = platform_src();
    let mut c = fake_win32();
    c.push_str("\n/* ---- lifted VERBATIM from platform-windows.c ---- */\n");
    c.push_str(&lift_block(&src));
    c.push('\n');
    c.push_str(&lift_sleepto(&src));
    c.push_str(&harness_main(scs));

    let dir = scratch_dir();
    let cfile = dir.join("os_clock.c");
    let bin = dir.join("os_clock.bin");
    fs::write(&cfile, &c).expect("write the harness");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=gnu99",
            "-Wall",
            "-Wextra",
            "-Wconversion",
            "-Wformat=2",
            "-Werror",
            "-O1",
        ])
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
        "issue 1372: the lifted os_gettime_ns / os_sleepto_ns do NOT COMPILE against the fake \
         Win32 layer under -Wall -Wextra -Wconversion -Werror:\n--- cc stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    bin
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
    let bin = harness();
    for s in &scs {
        let stdout = run(bin, s.name);
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
        // Lock pairing and the single-poller flag.
        assert!(
            tail.contains("lock_errors=0 shared=0 excl=0 polling=0"),
            "issue 1372 `{}`: SRW lock pairing / poller flag broken: {tail}",
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
    let bin = harness();
    // Disabled / absent = exactly the old raw-QPC nanoseconds.
    for name in ["disabled", "absent"] {
        let s = scs.iter().find(|s| s.name == name).unwrap();
        let stdout = run(bin, name);
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
    let stdout = run(bin, "const");
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
    let bin = harness();
    let stdout = run(bin, "sleep");
    let f: Vec<u64> = stdout
        .split_whitespace()
        .map(|t| t.parse().expect("sleep fields"))
        .collect();
    let (now, target, stall, slept_ms, wake_qpc, after, stall_past, lock_errors) =
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
    assert_eq!(
        lock_errors, 0,
        "issue 1372: SRW lock pairing broken in os_sleepto_ns"
    );
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
    let block = lift_block(&platform_src());
    assert!(
        block.contains("GetModuleHandleW(L\"kernelbase.dll\")")
            && block.contains("GetProcAddress(kernelbase, \"GetSystemTimeAdjustmentPrecise\")"),
        "issue 1372: the adjustment API must be resolved at runtime from kernelbase.dll"
    );
    assert!(
        !platform_src().contains("GetModuleHandleW(L\"kernel32.dll\")"),
        "issue 1372: kernel32.dll does not export GetSystemTimeAdjustmentPrecise"
    );
}
