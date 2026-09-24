//! #1367 — an EXECUTABLE C-vs-Rust parity gate for the per-source ASRC servo.
//!
//! `src/asrc_bench.rs` `RealtimeAsrcCompensator` is the Tier-0 authority; the vendored
//! `vendor/obs-studio/libobs/media-io/asrc-compensator.c` is its line-by-line C port, and the two
//! are required to be numerically identical. Until now that was asserted only by static text
//! anchors (`tests/genlock_preload.rs`) plus one-off scratch lifts recorded in
//! `.claude/rules/asrc-bench-harness.md` — nothing EXECUTED the shipped C, because libobs is
//! otherwise compiled only by the genlock workflows.
//!
//! This gate compiles the REAL `asrc-compensator.c` (the whole file, its own header, no stub)
//! under `-Wall -Wextra -Wconversion -Wformat=2 -Werror`, drives it through three closed-loop
//! scenarios from a small C `main`, and requires the printed 9-decimal trace to be byte-identical
//! to the Rust authority driven the same way:
//!
//! - `tick`: the live stream `mbc` operating point — +20 ppm source, 128-sample callbacks into a
//!   buffer the mixer drains in 1024-sample (21.33 ms) ticks, 1 ms bursty delivery from a shared
//!   LCG — so every call reads a different point of the tick sawtooth and the per-window level
//!   MEAN (#1367) is exercised at full resolution, 2 h.
//! - `shift`: a non-absolute source, four 0.25 s callbacks per window over a ±6 ms per-callback
//!   pattern, a deliberate +12 ms shift landing MID-window (the open window sum must move with
//!   it), then a 40 ms sample loss with no residual step (the sustained arm → restore burst).
//! - `step`: an absolute source with a 50 ms input sample loss (re-base + level-corroborated
//!   restore), a starved window (rejected, flushed — the window level sum must reset; the mixer
//!   pads, so the depth holds), and a duplicate wall read (a zero master block, flushed, the next
//!   block carrying both intervals), each followed by a relock.
//!
//! Per the project's test-strictness rule it FAILS LOUDLY rather than skipping when the C
//! toolchain is missing — a parity test that silently passes without running is worse than none.

use camera_box::asrc_bench::RealtimeAsrcCompensator;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const MEDIA_IO: &str = "vendor/obs-studio/libobs/media-io";
const C_SRC: &str = "vendor/obs-studio/libobs/media-io/asrc-compensator.c";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// The C driver. It mirrors `rust_trace()` below statement for statement — the same constants,
/// the same LCG, the same evaluation order — so any divergence is the compensator, not the driver.
const HARNESS_C: &str = r##"#include <stdint.h>
#include <stdio.h>
#include <stdbool.h>
#include "asrc-compensator.h"

static void line(const char *tag, long k, const struct asrc_compensator *c)
{
	printf("%s k=%ld est=%.9f app=%.9f lvl=%.9f avg=%.9f tgt=%.9f int=%.9f ema=%.9f rst=%d steps=%u fb=%u\n", tag,
	       k, c->estimated_ppm, c->applied_ppm, c->level_last_ms, c->level_avg_ms, c->level_target_ms,
	       c->level_integral_ppm, c->level_err_ema_ms, c->level_restore ? 1 : 0, c->step_count,
	       c->level_fallback_count);
}

static void tick_scenario(void)
{
	const double block_s = 128.0 / 48000.0;
	const double tick_ms = 1024.0 / 48000.0 * 1000.0;
	const double tick_s = tick_ms / 1000.0;
	const double period_s = block_s / (1.0 + 20.0 / 1000000.0);
	const double jitter_s = 0.001;
	struct asrc_compensator c;
	asrc_compensator_init(&c);
	double buffer_ms = 100.0 + tick_ms / 2.0;
	double t = 0.0;
	double next_tick_s = tick_s;
	double prev_jitter = 0.0;
	double next_print_s = 60.0;
	uint64_t seed = 0x1367u;
	double applied = 0.0;
	while (t < 7200.0) {
		seed = seed * 6364136223846793005ull + 1442695040888963407ull;
		const double jitter = (double)(seed >> 11) / 9007199254740992.0 * jitter_s;
		const double master_s = period_s + jitter - prev_jitter;
		prev_jitter = jitter;
		t += master_s;
		while (next_tick_s <= t) {
			buffer_ms -= tick_ms;
			next_tick_s += tick_s;
		}
		const double corrected_s = asrc_compensator_compensate(&c, block_s, master_s, buffer_ms, &applied);
		buffer_ms += corrected_s * 1000.0;
		if (t >= next_print_s) {
			line("tick", (long)next_print_s, &c);
			next_print_s += 60.0;
		}
	}
}

static void shift_scenario(void)
{
	const double master_s = 0.25;
	const double raw_s = master_s * (1.0 + -5.0 / 1000000.0);
	const double pattern[4] = {-6.0, -2.0, 2.0, 6.0};
	struct asrc_compensator c;
	asrc_compensator_init(&c);
	asrc_compensator_set_level_absolute(&c, false);
	double buffer_ms = 90.0;
	double applied = 0.0;
	for (long w = 0; w < 3600; w++) {
		for (int i = 0; i < 4; i++) {
			if (w == 600 && i == 2) {
				asrc_compensator_shift_level_target(&c, 12.0);
				buffer_ms += 12.0;
			}
			if (w == 1200 && i == 1)
				buffer_ms -= 40.0;
			const double corrected_s =
				asrc_compensator_compensate(&c, raw_s, master_s, buffer_ms + pattern[i], &applied);
			buffer_ms += (corrected_s - master_s) * 1000.0;
		}
		if (w % 60 == 59 || (w >= 598 && w <= 604) || (w >= 1198 && w <= 1215))
			line("shift", w, &c);
	}
}

static void step_scenario(void)
{
	const double master_s = 0.25;
	const double raw_s = master_s * (1.0 + -5.0 / 1000000.0);
	struct asrc_compensator c;
	asrc_compensator_init(&c);
	asrc_compensator_set_level_offset_ms(&c, 24.0);
	double buffer_ms = 110.0;
	double applied = 0.0;
	for (long w = 0; w < 3000; w++) {
		for (int i = 0; i < 4; i++) {
			double raw = raw_s;
			double master = master_s;
			if (w == 900 && i == 1) {
				raw = raw_s - 0.05;
				buffer_ms -= 50.0;
			}
			if (w == 1500)
				raw = 0.0;
			if (w == 2000 && i == 2)
				master = 0.0;
			if (w == 2000 && i == 3)
				master = 2.0 * master_s;
			const double corrected_s = asrc_compensator_compensate(&c, raw, master, buffer_ms, &applied);
			if (w != 1500)
				buffer_ms += (corrected_s - master) * 1000.0;
		}
		if (w % 50 == 49 || (w >= 898 && w <= 906) || (w >= 1498 && w <= 1503) || (w >= 1998 && w <= 2003))
			line("step", w, &c);
	}
}

int main(void)
{
	tick_scenario();
	shift_scenario();
	step_scenario();
	return 0;
}
"##;

fn line(tag: &str, k: i64, c: &RealtimeAsrcCompensator) -> String {
    format!(
        "{tag} k={k} est={:.9} app={:.9} lvl={:.9} avg={:.9} tgt={:.9} int={:.9} ema={:.9} rst={} steps={} fb={}",
        c.estimated_ppm(),
        c.applied_ppm(),
        c.level_last_ms(),
        c.level_avg_ms(),
        c.level_target_ms(),
        c.level_integral_ppm(),
        c.level_err_ema_ms(),
        u8::from(c.level_restore()),
        c.step_count(),
        c.level_fallback_count()
    )
}

/// The Rust authority driven exactly like `HARNESS_C` (statement for statement).
fn rust_trace() -> Vec<String> {
    let mut out = Vec::new();

    // tick
    {
        let block_s = 128.0 / 48000.0;
        let tick_ms = 1024.0 / 48000.0 * 1000.0;
        let tick_s = tick_ms / 1000.0;
        let period_s = block_s / (1.0 + 20.0 / 1000000.0);
        let jitter_s = 0.001;
        let mut c = RealtimeAsrcCompensator::new();
        let mut buffer_ms = 100.0 + tick_ms / 2.0;
        let mut t = 0.0_f64;
        let mut next_tick_s = tick_s;
        let mut prev_jitter = 0.0_f64;
        let mut next_print_s = 60.0_f64;
        let mut seed: u64 = 0x1367;
        while t < 7200.0 {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let jitter = (seed >> 11) as f64 / 9_007_199_254_740_992.0 * jitter_s;
            let master_s = period_s + jitter - prev_jitter;
            prev_jitter = jitter;
            t += master_s;
            while next_tick_s <= t {
                buffer_ms -= tick_ms;
                next_tick_s += tick_s;
            }
            let corrected_s = c.compensate_with_level(block_s, master_s, buffer_ms);
            buffer_ms += corrected_s * 1000.0;
            if t >= next_print_s {
                out.push(line("tick", next_print_s as i64, &c));
                next_print_s += 60.0;
            }
        }
    }

    // shift
    {
        let master_s = 0.25;
        let raw_s = master_s * (1.0 + -5.0 / 1000000.0);
        let pattern = [-6.0, -2.0, 2.0, 6.0];
        let mut c = RealtimeAsrcCompensator::new();
        c.set_level_absolute(false);
        let mut buffer_ms = 90.0_f64;
        for w in 0..3600_i64 {
            for (i, p) in pattern.iter().enumerate() {
                if w == 600 && i == 2 {
                    c.shift_level_target(12.0);
                    buffer_ms += 12.0;
                }
                if w == 1200 && i == 1 {
                    buffer_ms -= 40.0;
                }
                let corrected_s = c.compensate_with_level(raw_s, master_s, buffer_ms + p);
                buffer_ms += (corrected_s - master_s) * 1000.0;
            }
            if w % 60 == 59 || (598..=604).contains(&w) || (1198..=1215).contains(&w) {
                out.push(line("shift", w, &c));
            }
        }
    }

    // step
    {
        let master_s = 0.25;
        let raw_s = master_s * (1.0 + -5.0 / 1000000.0);
        let mut c = RealtimeAsrcCompensator::new();
        c.set_level_offset_ms(24.0);
        let mut buffer_ms = 110.0_f64;
        for w in 0..3000_i64 {
            for i in 0..4 {
                let mut raw = raw_s;
                let mut master = master_s;
                if w == 900 && i == 1 {
                    raw = raw_s - 0.05;
                    buffer_ms -= 50.0;
                }
                if w == 1500 {
                    raw = 0.0;
                }
                if w == 2000 && i == 2 {
                    master = 0.0;
                }
                if w == 2000 && i == 3 {
                    master = 2.0 * master_s;
                }
                let corrected_s = c.compensate_with_level(raw, master, buffer_ms);
                if w != 1500 {
                    buffer_ms += (corrected_s - master) * 1000.0;
                }
            }
            if w % 50 == 49
                || (898..=906).contains(&w)
                || (1498..=1503).contains(&w)
                || (1998..=2003).contains(&w)
            {
                out.push(line("step", w, &c));
            }
        }
    }
    out
}

fn c_trace() -> Vec<String> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("asrc_compensator_parity_1367");
    fs::create_dir_all(&dir).expect("create the parity scratch dir");
    let harness = dir.join("harness.c");
    let bin = dir.join("harness.bin");
    fs::write(&harness, HARNESS_C).expect("write the parity harness");

    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=gnu11",
            "-Wall",
            "-Wextra",
            "-Wconversion",
            "-Wformat=2",
            "-Werror",
            "-O1",
        ])
        .arg("-I")
        .arg(repo(MEDIA_IO))
        .arg(repo(C_SRC))
        .arg(&harness)
        .arg("-lm")
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "#1367: could not run the C compiler `{cc}` ({e}). This gate compiles the vendored \
                 asrc-compensator.c to prove the C and the Rust authority agree numerically; it \
                 must FAIL rather than skip when the toolchain is absent. Install a C compiler or \
                 set CC."
            )
        });
    assert!(
        out.status.success(),
        "#1367: {C_SRC} (+ the parity driver) does NOT COMPILE standalone under -Wall -Wextra \
         -Wconversion -Wformat=2 -Werror. libobs is otherwise built only by the genlock workflows, \
         so this is very likely a real compile error heading for CI:\n--- cc stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin)
        .output()
        .expect("#1367: the compiled parity harness failed to execute");
    assert!(
        run.status.success(),
        "#1367: the parity harness exited non-zero: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    String::from_utf8(run.stdout)
        .expect("harness stdout is utf-8")
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn c_asrc_compensator_matches_the_rust_authority_1367() {
    let c = c_trace();
    let r = rust_trace();
    assert_eq!(
        c.len(),
        r.len(),
        "#1367: the C and Rust drivers printed a different number of trace lines (C {} vs Rust {})",
        c.len(),
        r.len()
    );
    for (i, (cl, rl)) in c.iter().zip(&r).enumerate() {
        assert_eq!(
            cl, rl,
            "#1367: asrc-compensator.c diverges from src/asrc_bench.rs at trace line {i}:\n  C:    \
             {cl}\n  Rust: {rl}"
        );
    }
    // The scenarios must actually reach the paths they claim to cover — a parity pass over a trace
    // that never re-based, restored or fell out of lock would prove much less than it says.
    let has = |needle: &str| r.iter().any(|l| l.contains(needle));
    assert!(
        r.len() >= 150
            && has("rst=1")
            && r.iter()
                .any(|l| l.starts_with("step") && !l.contains("steps=0")),
        "#1367: the parity scenarios no longer exercise the restore burst and a step re-base \
         ({} lines) — the gate would pass on a trace that skips the paths it guards",
        r.len()
    );
}
