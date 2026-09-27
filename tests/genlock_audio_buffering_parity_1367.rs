//! Issue 1367 (ROZHODNUTÉ 5857354949) — an EXECUTABLE C-vs-Rust parity gate for the genlock
//! audio-buffering FLOOR, and the band invariant held on the values the shipped code really uses.
//!
//! - `vendor/obs-studio/libobs/obs-genlock-audio-buffering.h` is pure `<stdint.h>`/`<stdbool.h>`, so
//!   it is `#include`d as-is (never a retyped copy), compiled under `-Wall -Wextra -Wconversion
//!   -Wformat=2 -Werror`, and driven over the tick rounding, the reset plan, the floor-then-dynamic
//!   action and the band. Every printed value must be byte-identical to the Tier-0 Rust authority
//!   `camera_box::genlock_audio_buffering`. `cc` is required — it FAILS LOUDLY rather than skips.
//! - The invariant is held on the LIFTED definitions: the header's defines, obs-source.c
//!   `TS_SMOOTHING_THRESHOLD`, media-io `AUDIO_OUTPUT_FRAMES`, asrc-compensator.h
//!   `ASRC_LEVEL_TARGET_MS`, the #1333 split's `AUDIO_OFFSET_CLAMP_MS`, and the three #786 launch
//!   gates' 100 ms bound. A target change that leaves the band, a floor the launch gates would read
//!   as a bad draw, or a threshold change all go RED here.

use camera_box::asrc_bench::LEVEL_TARGET_MS;
use camera_box::genlock_audio_buffering::{
    action, band_error_ns, band_ok, floor_holds_band, level_gap_ns, plan, ticks_for_ms, ticks_ns,
    BufferingAction, AUDIO_OUTPUT_FRAMES, DEFAULT_MAX_TICKS, FLOOR_MS, LEVEL_BASE_MAX_NS,
    LEVEL_BASE_NOMINAL_NS, LEVEL_REACH_NS, TS_SMOOTHING_THRESHOLD_NS,
};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const HEADER: &str = "vendor/obs-studio/libobs/obs-genlock-audio-buffering.h";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn read(rel: &str) -> String {
    fs::read_to_string(repo(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// The value of `#define NAME <value>` in `text`, with a C integer suffix stripped.
fn lift_define(text: &str, name: &str) -> String {
    let needle = format!("#define {name} ");
    let line = text
        .lines()
        .find(|l| l.starts_with(&needle))
        .unwrap_or_else(|| panic!("issue 1367: `{needle}` not found"));
    line[needle.len()..]
        .split_whitespace()
        .next()
        .unwrap()
        .trim_end_matches(|c: char| c.is_ascii_alphabetic())
        .to_string()
}

const RATES: [u32; 5] = [0, 8_000, 44_100, 48_000, 96_000];
const MS_VECTORS: [u32; 12] = [0, 1, 20, 21, 22, 63, 64, 85, 86, 150, 960, 4_000_000];
const FRAMES: [u32; 3] = [0, 1024, 480];

fn plan_vectors() -> Vec<(u32, bool, u32, u32)> {
    let mut v = Vec::new();
    for rate in RATES {
        for frames in FRAMES {
            for max in [0_u32, 20, 50, 84, 85, 86, 150, 960, 5000] {
                for fixed in [false, true] {
                    v.push((max, fixed, rate, frames));
                }
            }
        }
    }
    v.push((0, false, 768_000, 1024));
    v
}

fn action_vectors() -> Vec<(i32, i32, i32, bool)> {
    let mut v = Vec::new();
    for total in [-1, 0, 1, 3, 4, 5, 44, 45, 46] {
        for (floor, max) in [(0, 45), (4, 45), (4, 4), (5, 8), (45, 45), (60, 45)] {
            for behind in [false, true] {
                v.push((total, floor, max, behind));
            }
        }
    }
    v
}

fn band_vectors() -> Vec<(u64, u64, i64)> {
    let mut v = Vec::new();
    let t = 100_000_000_i64;
    for buf in [
        0_u64,
        64_000_000,
        64_999_999,
        65_000_000,
        85_333_333,
        106_666_666,
        126_000_000,
        126_000_001,
        128_000_000,
        u64::MAX,
    ] {
        for base in [0_u64, 9_000_000, 25_000_000] {
            for target in [t, 0, -t, i64::MIN, i64::MAX] {
                v.push((buf, base, target));
            }
        }
    }
    v
}

fn action_token(a: BufferingAction) -> i32 {
    a as i32
}

fn rust_trace() -> Vec<String> {
    let mut out = Vec::new();
    for rate in RATES {
        for frames in FRAMES {
            for ms in MS_VECTORS {
                out.push(format!(
                    "ticks {ms} {rate} {frames} {}",
                    ticks_for_ms(ms, rate, frames)
                ));
            }
            for ticks in [0_u32, 1, 4, 45, u32::MAX] {
                out.push(format!(
                    "ns {ticks} {frames} {rate} {}",
                    ticks_ns(ticks, frames, rate)
                ));
            }
        }
    }
    for (max, fixed, rate, frames) in plan_vectors() {
        let p = plan(max, fixed, rate, frames);
        out.push(format!(
            "plan {max} {} {rate} {frames} {} {} {} {}",
            u8::from(fixed),
            p.floor_ticks,
            p.max_ticks,
            u8::from(p.fixed),
            u8::from(p.overridden)
        ));
    }
    for (total, floor, max, behind) in action_vectors() {
        out.push(format!(
            "action {total} {floor} {max} {} {}",
            u8::from(behind),
            action_token(action(total, floor, max, behind))
        ));
    }
    for (buf, base, target) in band_vectors() {
        let e = band_error_ns(buf, base, target);
        out.push(format!(
            "band {buf} {base} {target} {e} {}",
            u8::from(band_ok(e))
        ));
    }
    out
}

/// An i64 as a C expression (`INT64_MIN` has no literal form).
fn c_i64(v: i64) -> String {
    if v == i64::MIN {
        "INT64_MIN".to_string()
    } else {
        format!("{v}LL")
    }
}

fn c_harness() -> String {
    let mut body = String::new();
    for rate in RATES {
        for frames in FRAMES {
            for ms in MS_VECTORS {
                body.push_str(&format!("\tticks_line({ms}u, {rate}u, {frames}u);\n"));
            }
            for ticks in [0_u32, 1, 4, 45, u32::MAX] {
                body.push_str(&format!("\tns_line({ticks}u, {frames}u, {rate}u);\n"));
            }
        }
    }
    for (max, fixed, rate, frames) in plan_vectors() {
        body.push_str(&format!(
            "\tplan_line({max}u, {}, {rate}u, {frames}u);\n",
            if fixed { "true" } else { "false" }
        ));
    }
    for (total, floor, max, behind) in action_vectors() {
        body.push_str(&format!(
            "\taction_line({total}, {floor}, {max}, {});\n",
            if behind { "true" } else { "false" }
        ));
    }
    for (buf, base, target) in band_vectors() {
        body.push_str(&format!(
            "\tband_line({buf}ULL, {base}ULL, {});\n",
            c_i64(target)
        ));
    }
    format!(
        r#"#include <inttypes.h>
#include <stdio.h>
#include "obs-genlock-audio-buffering.h"

static void ticks_line(uint32_t ms, uint32_t rate, uint32_t frames)
{{
	printf("ticks %" PRIu32 " %" PRIu32 " %" PRIu32 " %" PRIu32 "\n", ms, rate, frames,
	       genlock_audio_buffering_ticks(ms, rate, frames));
}}

static void ns_line(uint32_t ticks, uint32_t frames, uint32_t rate)
{{
	printf("ns %" PRIu32 " %" PRIu32 " %" PRIu32 " %" PRIu64 "\n", ticks, frames, rate,
	       genlock_audio_buffering_ticks_ns(ticks, frames, rate));
}}

static void plan_line(uint32_t max, bool fixed, uint32_t rate, uint32_t frames)
{{
	const struct genlock_audio_buffering_plan p = genlock_audio_buffering_make_plan(max, fixed, rate, frames);
	printf("plan %" PRIu32 " %d %" PRIu32 " %" PRIu32 " %" PRIu32 " %" PRIu32 " %d %d\n", max, fixed ? 1 : 0, rate,
	       frames, p.floor_ticks, p.max_ticks, p.fixed ? 1 : 0, p.overridden ? 1 : 0);
}}

static void action_line(int total, int floor, int max, bool behind)
{{
	printf("action %d %d %d %d %d\n", total, floor, max, behind ? 1 : 0,
	       genlock_audio_buffering_action(total, floor, max, behind));
}}

static void band_line(uint64_t buf, uint64_t base, int64_t target)
{{
	const int64_t e = genlock_audio_buffering_band_error_ns(buf, base, target);
	printf("band %" PRIu64 " %" PRIu64 " %" PRId64 " %" PRId64 " %d\n", buf, base, target, e,
	       genlock_audio_buffering_band_ok(e) ? 1 : 0);
}}

int main(void)
{{
{body}	return 0;
}}
"#
    )
}

fn c_trace() -> Vec<String> {
    let dir =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("genlock_audio_buffering_parity_1367");
    fs::create_dir_all(&dir).expect("create the parity scratch dir");
    let harness = dir.join("harness.c");
    let bin = dir.join("harness.bin");
    fs::write(&harness, c_harness()).expect("write the parity harness");
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
        .arg(repo("vendor/obs-studio/libobs"))
        .arg(&harness)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "issue 1367: could not run the C compiler `{cc}` ({e}). This gate compiles the \
                 vendored {HEADER} to prove the C and the Rust authority agree; it must FAIL rather \
                 than skip. Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "issue 1367: {HEADER} (+ the parity driver) does NOT COMPILE standalone under -Wall \
         -Wextra -Wconversion -Wformat=2 -Werror:\n--- cc stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin)
        .output()
        .expect("issue 1367: the compiled parity harness failed to execute");
    assert!(
        run.status.success(),
        "issue 1367: the parity harness exited non-zero"
    );
    String::from_utf8(run.stdout)
        .expect("harness stdout is utf-8")
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn c_audio_buffering_matches_the_rust_authority_1367() {
    let c = c_trace();
    let r = rust_trace();
    assert_eq!(
        c.len(),
        r.len(),
        "issue 1367: C {} vs Rust {} trace lines",
        c.len(),
        r.len()
    );
    for (i, (cl, rl)) in c.iter().zip(&r).enumerate() {
        assert_eq!(
            cl, rl,
            "issue 1367: {HEADER} diverges from src/genlock_audio_buffering.rs at line {i}"
        );
    }
    // The vectors reach every path they claim to cover.
    let has = |needle: &str| r.iter().any(|l| l.starts_with(needle));
    assert!(
        has("plan 20 1 48000 1024 4 45 0 1")
            && has("plan 0 0 48000 1024 4 45 0 0")
            && has("plan 85 0 48000 1024 4 4 0 0")
            && has("plan 0 0 768000 1024 64 64 0 1")
            && has("action 0 4 45 0 1")
            && has("action 4 4 45 1 2")
            && has("action 45 4 45 1 0")
            && r.iter()
                .any(|l| l.starts_with("band ") && l.ends_with(" 0"))
            && r.iter()
                .any(|l| l.starts_with("band ") && l.ends_with(" 1"))
            && has("band 65000000 0 100000000 35000000 1")
            && has("band 64999999 0 100000000 35000001 0"),
        "issue 1367: the parity vectors no longer reach the floor, the override, the maximum, the \
         dynamic increase and both band outcomes"
    );
}

#[test]
fn the_header_defines_are_the_rust_constants_1367() {
    let h = read(HEADER);
    assert_eq!(
        lift_define(&h, "GENLOCK_AUDIO_BUFFERING_FLOOR_MS"),
        FLOOR_MS.to_string()
    );
    assert_eq!(
        lift_define(&h, "GENLOCK_AUDIO_BUFFERING_DEFAULT_MAX_TICKS"),
        DEFAULT_MAX_TICKS.to_string()
    );
    assert_eq!(
        lift_define(&h, "GENLOCK_AUDIO_LEVEL_REACH_NS"),
        LEVEL_REACH_NS.to_string()
    );
    assert_eq!(
        lift_define(&h, "GENLOCK_AUDIO_LEVEL_BASE_NOMINAL_NS"),
        LEVEL_BASE_NOMINAL_NS.to_string()
    );
    for (name, v) in [
        ("GENLOCK_AUDIO_BUFFERING_ACTION_NONE", BufferingAction::None),
        (
            "GENLOCK_AUDIO_BUFFERING_ACTION_FLOOR",
            BufferingAction::Floor,
        ),
        (
            "GENLOCK_AUDIO_BUFFERING_ACTION_DYNAMIC",
            BufferingAction::Dynamic,
        ),
    ] {
        assert_eq!(lift_define(&h, name), (v as i32).to_string(), "{name}");
    }
    // The mixer tick is the real libobs AUDIO_OUTPUT_FRAMES.
    assert_eq!(
        lift_define(
            &read("vendor/obs-studio/libobs/media-io/audio-io.h"),
            "AUDIO_OUTPUT_FRAMES"
        ),
        AUDIO_OUTPUT_FRAMES.to_string()
    );
    // The band is half the real re-placement threshold.
    let ts: i64 = lift_define(
        &read("vendor/obs-studio/libobs/obs-source.c"),
        "TS_SMOOTHING_THRESHOLD",
    )
    .parse()
    .unwrap();
    assert_eq!(ts, TS_SMOOTHING_THRESHOLD_NS);
    assert_eq!(LEVEL_REACH_NS * 2, ts);
}

/// The shipped ASRC level target, lifted from the C and held equal to the Rust mirror, ns.
fn shipped_target_ns() -> i64 {
    let c: f64 = lift_define(
        &read("vendor/obs-studio/libobs/media-io/asrc-compensator.h"),
        "ASRC_LEVEL_TARGET_MS",
    )
    .parse()
    .unwrap();
    assert_eq!(
        c, LEVEL_TARGET_MS,
        "issue 1367: asrc-compensator.h ASRC_LEVEL_TARGET_MS and src/asrc_bench.rs LEVEL_TARGET_MS disagree"
    );
    (c * 1e6).round() as i64
}

#[test]
fn the_shipped_floor_keeps_the_shipped_level_target_in_reach_1367() {
    // A change of the floor, the target or the threshold that leaves the band fails here, at
    // both production rates, for every base of the band.
    let target = shipped_target_ns();
    for rate in [44_100, 48_000] {
        let p = plan(0, false, rate, AUDIO_OUTPUT_FRAMES);
        assert!(
            floor_holds_band(p.floor_ticks, rate, AUDIO_OUTPUT_FRAMES, target),
            "issue 1367: the {} ms floor ({} ticks at {rate} Hz) no longer holds the {} ms ASRC \
             level target within +/-{} ms for a 0..{} ms base",
            FLOOR_MS,
            p.floor_ticks,
            target / 1_000_000,
            LEVEL_REACH_NS / 1_000_000,
            LEVEL_BASE_MAX_NS / 1_000_000
        );
        // And the nominal mbc base sits well inside it.
        let e = band_error_ns(
            ticks_ns(p.floor_ticks, AUDIO_OUTPUT_FRAMES, rate),
            LEVEL_BASE_NOMINAL_NS,
            target,
        );
        assert!(band_ok(e));
    }
    // The band has teeth on the shipped target: no buffering (the live 12-18-15 session) and
    // 6 ticks at 48 kHz both leave it.
    assert!(!floor_holds_band(0, 48_000, AUDIO_OUTPUT_FRAMES, target));
    assert!(!floor_holds_band(6, 48_000, AUDIO_OUTPUT_FRAMES, target));
}

#[test]
fn the_sync_offset_clamp_cannot_break_reach_1367() {
    // Over the WHOLE range the #1333 split may write (its own clamp, lifted), the gap the servo
    // must bridge is the offset-free band error: the offset moves the depth and the target 1:1.
    let text = read("scripts/av_sync_calibrate.py");
    let line = text
        .lines()
        .find(|l| l.starts_with("AUDIO_OFFSET_CLAMP_MS = "))
        .expect("issue 1367: AUDIO_OFFSET_CLAMP_MS not found in scripts/av_sync_calibrate.py");
    let clamp_ms: i64 = line["AUDIO_OFFSET_CLAMP_MS = ".len()..]
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(clamp_ms > 0);
    let target = shipped_target_ns();
    let buffering = ticks_ns(
        plan(0, false, 48_000, AUDIO_OUTPUT_FRAMES).floor_ticks,
        AUDIO_OUTPUT_FRAMES,
        48_000,
    );
    for base in [0, LEVEL_BASE_NOMINAL_NS, LEVEL_BASE_MAX_NS] {
        let e = band_error_ns(buffering, base, target);
        for x_ms in (-clamp_ms..=clamp_ms)
            .step_by(7)
            .chain([-clamp_ms, clamp_ms])
        {
            assert_eq!(
                level_gap_ns(buffering, base, target, x_ms * 1_000_000),
                e,
                "issue 1367: a {x_ms} ms sync offset changed the reach gap"
            );
        }
    }
}

/// The #786 launch-gate bound of one consumer, lifted from its source.
fn launch_gate_bound(rel: &str, prefix: &str) -> u32 {
    let text = read(rel);
    let at = text
        .find(prefix)
        .unwrap_or_else(|| panic!("issue 1367: `{prefix}` not found in {rel}"));
    text[at + prefix.len()..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap_or_else(|_| panic!("issue 1367: no number after `{prefix}` in {rel}"))
}

#[test]
fn the_floor_reads_clean_on_every_786_launch_gate_1367() {
    // The three #786 launch gates parse "total audio buffering is now N milliseconds" and call a
    // launch BAD above their bound. The floor itself must read as a clean draw on every one of
    // them at both production rates, or every launch would be redrawn (the guarded launcher kills
    // OBS up to three times).
    let bounds = [
        launch_gate_bound("scripts/rig-health-audit.py", "AUDIO_BUF_BOUND_MS = "),
        launch_gate_bound("scripts/obs-guarded-launch.ps1", "$threshold  = "),
        launch_gate_bound("scripts/launch-obs-genlock.sh", "$bufPeak -le "),
        launch_gate_bound("scripts/launch-obs-genlock.sh", "$d.Peak -le "),
    ];
    for rate in [44_100, 48_000] {
        let floor = plan(0, false, rate, AUDIO_OUTPUT_FRAMES).floor_ticks;
        // obs-audio.c logs the total as floor_ticks * frames * 1000 / rate, truncated.
        let logged =
            (u64::from(floor) * u64::from(AUDIO_OUTPUT_FRAMES) * 1000 / u64::from(rate)) as u32;
        for b in bounds {
            assert!(
                logged <= b,
                "issue 1367: the {logged} ms floor at {rate} Hz is over a #786 launch-gate bound of {b} ms"
            );
        }
    }
}
