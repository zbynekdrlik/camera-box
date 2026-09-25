//! issue 1346 -- the DRM-output HDMI Multiview ran at HALF rate (15 fps instead of 30) on strih-lx
//! (main design comment 5840501628).
//!
//! Root cause: `drm_output_view_frame()` gates each Multiview render with the aux budget gate
//! `obs_aux_sender_should_skip()` (`vendor/obs-studio/libobs/obs.c`). Its "already consumed" term is
//! `max(elapsed, obs->video.last_tick_total_ns)` (#1063), and the previous tick's total INCLUDES the
//! view's own Multiview render when it rendered -- so that cost was counted twice (once there, once
//! as `ewma`), and a view that fits the budget skipped every other tick.
//!
//! Fix: `obs_aux_sender_should_skip_excluding(..., self_last_ns)` subtracts the caller's own previous
//! render (saturating) from the previous-tick total; `obs_aux_sender_should_skip()` is its
//! `self_last_ns = 0` case, so the #879 aux ndi_filter senders are unchanged.
//!
//! This gate lifts BOTH functions VERBATIM from the shipped obs.c, compiles them against the real
//! `obs-display-budget.h` + a tiny `obs` global stub, and proves:
//! 1. parity with the Tier-0 authority `camera_box::render_budget::aux_sender_should_skip_excluding`
//!    over a grid of vectors (every guard, both sides of the budget, self 0 / small / over-total);
//! 2. the wrapper is byte-identical to `_excluding(..., 0)` on every vector (the #879 senders);
//! 3. the live strih-lx numbers: pre 15 ms + mv 14 ms at a 33.3 ms interval renders 30/30 ticks,
//!    pre 25 ms + mv 14 ms still skips to the #293 floor.
//!
//! Fails loudly (never skips) when no C compiler is present.

use camera_box::render_budget::{aux_sender_should_skip_excluding, AuxTickClock};
use std::path::PathBuf;
use std::process::Command;

fn libobs() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vendor/obs-studio/libobs")
}

/// `obs_aux_sender_should_skip_excluding()` + the `obs_aux_sender_should_skip()` wrapper that must
/// follow it, lifted VERBATIM (both bodies are brace-free apart from their own braces, so the first
/// `"\n}"` after the wrapper's signature closes the pair).
fn lift_aux_seam() -> String {
    let obs_c = std::fs::read_to_string(libobs().join("obs.c")).expect("read obs.c");
    let ex_sig = "bool obs_aux_sender_should_skip_excluding(";
    let start = obs_c.find(ex_sig).unwrap_or_else(|| {
        panic!("issue 1346: obs.c does not define {ex_sig} -- the self-excluding aux budget gate is missing")
    });
    let wrap_sig = "bool obs_aux_sender_should_skip(";
    let wrap = obs_c[start..].find(wrap_sig).unwrap_or_else(|| {
        panic!("issue 1346: obs.c must define {wrap_sig} right after the _excluding form")
    }) + start;
    let end = obs_c[wrap..]
        .find("\n}")
        .expect("issue 1346: end of obs_aux_sender_should_skip")
        + wrap
        + 2;
    obs_c[start..end].to_string()
}

const INTERVALS: [u64; 3] = [0, 16_666_666, 33_333_333];
const TICK_STARTS: [u64; 2] = [0, 1000];
const ELAPSED: [u64; 3] = [3_000_000, 15_000_000, 28_000_000];
const LAST_TOTALS: [u64; 4] = [0, 15_000_000, 29_000_000, 39_000_000];
const SELF_LAST: [u64; 4] = [0, 5_000_000, 14_000_000, 40_000_000];
const DIVISORS: [u32; 4] = [0, 1, 2, 3];
const COUNTERS: [u32; 3] = [1, 2, 3];
const EWMAS: [u64; 3] = [0, 5_000_000, 14_000_000];
const SKIPS: [u32; 3] = [0, 2, 3];

fn c_array_u64(a: &[u64]) -> String {
    a.iter()
        .map(|v| format!("{v}ULL"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn c_array_u32(a: &[u32]) -> String {
    a.iter()
        .map(|v| format!("{v}u"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn harness(lifted: &str) -> String {
    format!(
        r#"
#include "obs-display-budget.h"
#include <stdint.h>
#include <stdbool.h>
#include <stdio.h>

static uint64_t g_now;
static uint64_t os_gettime_ns(void) {{ return g_now; }}
struct video_stub {{ uint64_t video_frame_interval_ns; uint64_t graphics_frame_start_ns; uint64_t last_tick_total_ns; }};
struct obs_stub {{ struct video_stub video; }};
static struct obs_stub _obs = {{{{0, 0, 0}}}};
static struct obs_stub *obs = &_obs;

/* ---- lifted VERBATIM from obs.c ---- */
{lifted}
/* ---- end lifted ---- */

static const uint64_t IVS[] = {{ {ivs} }};
static const uint64_t TSS[] = {{ {tss} }};
static const uint64_t ELS[] = {{ {els} }};
static const uint64_t LTS[] = {{ {lts} }};
static const uint64_t SLS[] = {{ {sls} }};
static const uint32_t DVS[] = {{ {dvs} }};
static const uint32_t FCS[] = {{ {fcs} }};
static const uint64_t EWS[] = {{ {ews} }};
static const uint32_t CSS[] = {{ {css} }};
#define N(a) (sizeof(a) / sizeof((a)[0]))

/* The DRM-output view's per-tick loop (called every tick): the gate sees elapsed = pre, the
 * previous tick's total (pre + mv when it rendered), ewma = mv, and -- with exclude_self -- its render
 * of the previous tick (0 after a skip; the view's tick-exact rule is drm_output_view_self_last_ns). */
static unsigned sim(uint64_t pre, uint64_t mv, int exclude_self)
{{
    const uint64_t iv = 33333333ULL;
    uint64_t t = 1000, last_render = 0;
    uint32_t fc = 0, cs = 0;
    unsigned renders = 0;
    obs->video.video_frame_interval_ns = iv;
    obs->video.last_tick_total_ns = 0;
    for (int k = 0; k < 30; k++) {{
        obs->video.graphics_frame_start_ns = t;
        g_now = t + pre;
        fc++;
        const uint64_t self_last = exclude_self ? last_render : 0;
        last_render = 0;
        const bool skip = exclude_self ? obs_aux_sender_should_skip_excluding(2, fc, mv, cs, self_last)
                                       : obs_aux_sender_should_skip(2, fc, mv, cs);
        uint64_t total = pre;
        if (skip) {{
            cs++;
        }} else {{
            renders++;
            cs = 0;
            total += mv;
            last_render = mv;
        }}
        obs->video.last_tick_total_ns = total;
        t += iv;
    }}
    return renders;
}}

int main(void)
{{
    unsigned wrapper_mismatch = 0;
    for (size_t a = 0; a < N(IVS); a++)
    for (size_t b = 0; b < N(TSS); b++)
    for (size_t c = 0; c < N(ELS); c++)
    for (size_t d = 0; d < N(LTS); d++)
    for (size_t e = 0; e < N(SLS); e++)
    for (size_t f = 0; f < N(DVS); f++)
    for (size_t g = 0; g < N(FCS); g++)
    for (size_t h = 0; h < N(EWS); h++)
    for (size_t i = 0; i < N(CSS); i++) {{
        obs->video.video_frame_interval_ns = IVS[a];
        obs->video.graphics_frame_start_ns = TSS[b];
        g_now = TSS[b] + ELS[c];
        obs->video.last_tick_total_ns = LTS[d];
        const bool s = obs_aux_sender_should_skip_excluding(DVS[f], FCS[g], EWS[h], CSS[i], SLS[e]);
        putchar(s ? '1' : '0');
        if (SLS[e] == 0 && obs_aux_sender_should_skip(DVS[f], FCS[g], EWS[h], CSS[i]) != s)
            wrapper_mismatch++;
    }}
    putchar('\n');
    printf("wrapper_mismatch=%u\n", wrapper_mismatch);
    printf("live_excluding=%u\n", sim(15000000ULL, 14000000ULL, 1));
    printf("live_wrapper=%u\n", sim(15000000ULL, 14000000ULL, 0));
    printf("heavy_excluding=%u\n", sim(25000000ULL, 14000000ULL, 1));
    return 0;
}}
"#,
        ivs = c_array_u64(&INTERVALS),
        tss = c_array_u64(&TICK_STARTS),
        els = c_array_u64(&ELAPSED),
        lts = c_array_u64(&LAST_TOTALS),
        sls = c_array_u64(&SELF_LAST),
        dvs = c_array_u32(&DIVISORS),
        fcs = c_array_u32(&COUNTERS),
        ews = c_array_u64(&EWMAS),
        css = c_array_u32(&SKIPS),
    )
}

fn rust_decisions() -> String {
    let mut out = String::new();
    for &iv in &INTERVALS {
        for &ts in &TICK_STARTS {
            for &el in &ELAPSED {
                for &lt in &LAST_TOTALS {
                    for &sl in &SELF_LAST {
                        for &dv in &DIVISORS {
                            for &fc in &COUNTERS {
                                for &ew in &EWMAS {
                                    for &cs in &SKIPS {
                                        let clock = AuxTickClock {
                                            interval_ns: iv,
                                            tick_start_ns: ts,
                                            now_ns: ts + el,
                                            last_tick_total_ns: lt,
                                        };
                                        let s = aux_sender_should_skip_excluding(
                                            dv, fc, ew, cs, sl, clock,
                                        );
                                        out.push(if s { '1' } else { '0' });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

fn value<'a>(stdout: &'a str, key: &str) -> &'a str {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("issue 1346: harness printed no `{key}=`:\n{stdout}"))
}

#[test]
fn c_self_excluding_gate_matches_rust_and_renders_the_live_view_every_tick_1346() {
    let lifted = lift_aux_seam();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let work = std::env::temp_dir().join(format!("mv_budget_1346_{}_{stamp}", std::process::id()));
    std::fs::create_dir_all(&work).expect("create temp workdir");
    let src = work.join("mv_budget.c");
    let bin = work.join("mv_budget");
    std::fs::write(&src, harness(&lifted)).expect("write harness");

    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-Wconversion",
            "-Wformat=2",
            "-O1",
            "-I",
        ])
        .arg(libobs())
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!("issue 1346: could not run the C compiler `{cc}` ({e}) -- this gate compiles the shipped obs.c seam")
        });
    assert!(
        out.status.success(),
        "issue 1346: the lifted aux seam failed to compile:\n{}\n--- harness ---\n{}",
        String::from_utf8_lossy(&out.stderr),
        std::fs::read_to_string(&src).unwrap_or_default()
    );
    let run = Command::new(&bin).output().expect("run harness");
    let _ = std::fs::remove_dir_all(&work);
    assert!(run.status.success(), "issue 1346: harness exited nonzero");
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();

    let c_bits = stdout.lines().next().unwrap_or_default();
    let rust_bits = rust_decisions();
    assert_eq!(
        c_bits.len(),
        rust_bits.len(),
        "issue 1346: C printed {} decisions for {} vectors",
        c_bits.len(),
        rust_bits.len()
    );
    let diverged = c_bits
        .bytes()
        .zip(rust_bits.bytes())
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(
        diverged, 0,
        "issue 1346 PARITY DIVERGENCE: {diverged} of {} vectors differ between \
         obs_aux_sender_should_skip_excluding (C) and render_budget::aux_sender_should_skip_excluding",
        rust_bits.len()
    );
    assert!(
        c_bits.contains('0') && c_bits.contains('1'),
        "issue 1346: the vector grid must exercise both render and skip"
    );

    assert_eq!(
        value(&stdout, "wrapper_mismatch"),
        "0",
        "issue 1346: obs_aux_sender_should_skip() must stay the self_last_ns = 0 case (the #879 senders)"
    );
    assert_eq!(
        value(&stdout, "live_excluding"),
        "30",
        "issue 1346: pre 15 ms + mv 14 ms fits the 30 ms budget -- the view must render every tick (30 fps)"
    );
    assert_eq!(
        value(&stdout, "live_wrapper"),
        "15",
        "issue 1346: without the self term the same numbers alternate (the live 15 fps)"
    );
    assert_eq!(
        value(&stdout, "heavy_excluding"),
        "7",
        "issue 1346: pre 25 ms + mv 14 ms is over budget -- only the #293 floor renders (every 4th tick)"
    );
}
