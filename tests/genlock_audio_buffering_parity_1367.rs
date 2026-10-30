//! Issue 1367 (ROZHODNUTÉ 5857354949) — an EXECUTABLE C-vs-Rust parity gate for the genlock
//! audio-buffering FLOOR (0 since issue 1425 E2 slice 3, design 6220191978).
//!
//! - `vendor/obs-studio/libobs/obs-genlock-audio-buffering.h` is pure `<stdint.h>`/`<stdbool.h>`, so
//!   it is `#include`d as-is (never a retyped copy), compiled under `-Wall -Wextra -Wconversion
//!   -Wformat=2 -Werror`, and driven over the tick rounding, the reset plan and the
//!   floor-then-dynamic action. Every printed value must be byte-identical to the Tier-0 Rust
//!   authority `camera_box::genlock_audio_buffering`. `cc` is required — it FAILS LOUDLY rather than
//!   skips.
//! - The header's defines are lifted and held equal to the Rust constants, and the three #786 launch
//!   gates' 100 ms bound is lifted: a launch's own draw (the floor, plus the one or two ticks a late
//!   source adds at startup) must read as clean on every gate.
//! - Issue 1425 (E2 slice 2, ROZHODNUTÉ 6217557472): the FREEZE rides the same gate -- the action's
//!   `frozen` input and the decision (`frozen`: an audio encoder runs, nothing else). Slice 3
//!   removed the band (the gap to the retired absolute ASRC level target) the floor was sized for,
//!   and (ROZHODNUTÉ 6221252305) the freeze waits out the launch settle window (`launch_settled`).
//!   The plan is also driven at non-zero floors (`plan_with_floor` vs
//!   `genlock_audio_buffering_make_plan_floor`), so the override and the max lift stay pinned.

use camera_box::genlock_audio_buffering::{
    action, frozen, frozen_plan, launch_settled, parse_box_ms, plan, plan_box, plan_with_floor,
    ticks_for_ms, ticks_ns, BufferingAction, AUDIO_OUTPUT_FRAMES, BOX_MAX_MS, DEFAULT_MAX_TICKS,
    FLOOR_MS, LAUNCH_SETTLE_NS,
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

/// The plan at non-zero floors (ms): the override of a request below the floor and the lift of the
/// maximum to a floor above the default maximum, which the shipped floor 0 never reaches.
fn plan_floor_vectors() -> Vec<(u32, u32, bool, u32, u32)> {
    let mut v = Vec::new();
    for floor in [0_u32, 21, 85, 960, 1200] {
        for (max, fixed, rate) in [
            (0_u32, false, 48_000_u32),
            (20, false, 48_000),
            (50, false, 48_000),
            (85, false, 48_000),
            (500, false, 44_100),
            (20, true, 48_000),
            (0, false, 768_000),
            (0, false, 0),
        ] {
            v.push((floor, max, fixed, rate, 1024));
        }
    }
    v
}

/// Issue 1425 slice 3: the launch settle window over its edges (first tick, mix time).
fn settled_vectors() -> Vec<(u64, u64)> {
    let first = 1_000_000_000_u64;
    vec![
        (0, 0),
        (0, first),
        (0, u64::MAX),
        (first, first),
        (first, first + LAUNCH_SETTLE_NS - 1),
        (first, first + LAUNCH_SETTLE_NS),
        (first, first + 3_600_000_000_000),
        (first, first - 1),
        (u64::MAX - 1, u64::MAX),
        (1, LAUNCH_SETTLE_NS),
        (1, LAUNCH_SETTLE_NS + 1),
    ]
}

fn action_vectors() -> Vec<(i32, i32, i32, bool, bool)> {
    let mut v = Vec::new();
    for total in [-1, 0, 1, 3, 4, 5, 44, 45, 46] {
        for (floor, max) in [(0, 45), (4, 45), (4, 4), (5, 8), (45, 45), (60, 45)] {
            for behind in [false, true] {
                for frozen in [false, true] {
                    v.push((total, floor, max, behind, frozen));
                }
            }
        }
    }
    v
}

/// Issue 1425: the freeze decision over its inputs, an audio encoder running and the launch
/// settled.
fn frozen_vectors() -> Vec<(bool, bool)> {
    vec![(false, false), (false, true), (true, false), (true, true)]
}

/// Issue 1425 (design 6443440513): the per-box FIXED buffering -- the plan from the marker's ms
/// (None = no valid marker, today's plan) at both production rates and the edges.
fn plan_box_vectors() -> Vec<(Option<u32>, u32, bool, u32)> {
    let mut v = Vec::new();
    for ms in [
        None,
        Some(0),
        Some(1),
        Some(21),
        Some(22),
        Some(64),
        Some(85),
        Some(959),
        Some(960),
    ] {
        for (max, fixed) in [(0_u32, false), (20, true), (50, false), (500, false)] {
            for rate in [44_100_u32, 48_000] {
                v.push((ms, max, fixed, rate));
            }
        }
    }
    v
}

/// Issue 1425: the freeze under a plan -- a fixed plan is frozen from the first tick, whatever the
/// encoder and the launch settle window say.
fn frozen_plan_vectors() -> Vec<(bool, bool, bool)> {
    let mut v = Vec::new();
    for fixed in [false, true] {
        for enc in [false, true] {
            for settled in [false, true] {
                v.push((fixed, enc, settled));
            }
        }
    }
    v
}

/// Issue 1425: the marker file's bytes. Valid = 1..3 ASCII digits, an optional trailing `\n`,
/// value 0..BOX_MAX_MS.
pub const BOX_MS_BYTES: [&[u8]; 24] = [
    b"",
    b"0",
    b"0\n",
    b"00",
    b"7",
    b"85\n",
    b"960",
    b"960\n",
    b"961",
    b"999\n",
    b"1000",
    b"0960",
    b"\n",
    b"85\n\n",
    b"85\r\n",
    b" 85",
    b"85 ",
    b"-1",
    b"+5",
    b"8a",
    b"0x5",
    b"\xd9\xa1",
    b"12345678",
    b"85\x00",
];

fn bytes_token(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<String>()
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
    for (floor, max, fixed, rate, frames) in plan_floor_vectors() {
        let p = plan_with_floor(floor, max, fixed, rate, frames);
        out.push(format!(
            "planf {floor} {max} {} {rate} {frames} {} {} {} {}",
            u8::from(fixed),
            p.floor_ticks,
            p.max_ticks,
            u8::from(p.fixed),
            u8::from(p.overridden)
        ));
    }
    for (first, now) in settled_vectors() {
        out.push(format!(
            "settled {first} {now} {}",
            u8::from(launch_settled(first, now))
        ));
    }
    for (total, floor, max, behind, fr) in action_vectors() {
        out.push(format!(
            "action {total} {floor} {max} {} {} {}",
            u8::from(behind),
            u8::from(fr),
            action_token(action(total, floor, max, behind, fr))
        ));
    }
    for (enc, settled) in frozen_vectors() {
        out.push(format!(
            "frozen {} {} {}",
            u8::from(enc),
            u8::from(settled),
            u8::from(frozen(enc, settled))
        ));
    }
    for (ms, max, fixed, rate) in plan_box_vectors() {
        let p = plan_box(ms, max, fixed, rate, AUDIO_OUTPUT_FRAMES);
        out.push(format!(
            "planb {} {} {max} {} {rate} {} {} {} {} {}",
            u8::from(ms.is_some()),
            ms.unwrap_or(0),
            u8::from(fixed),
            p.floor_ticks,
            p.max_ticks,
            u8::from(p.fixed),
            u8::from(p.overridden),
            u8::from(p.box_fixed)
        ));
    }
    for (fixed, enc, settled) in frozen_plan_vectors() {
        out.push(format!(
            "frozenp {} {} {} {}",
            u8::from(fixed),
            u8::from(enc),
            u8::from(settled),
            u8::from(frozen_plan(fixed, enc, settled))
        ));
    }
    for b in BOX_MS_BYTES {
        let r = parse_box_ms(b);
        out.push(format!(
            "parse {} {} {}",
            bytes_token(b),
            u8::from(r.is_some()),
            r.unwrap_or(0)
        ));
    }
    out
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
    for (floor, max, fixed, rate, frames) in plan_floor_vectors() {
        body.push_str(&format!(
            "\tplanf_line({floor}u, {max}u, {}, {rate}u, {frames}u);\n",
            if fixed { "true" } else { "false" }
        ));
    }
    for (first, now) in settled_vectors() {
        body.push_str(&format!("\tsettled_line({first}ULL, {now}ULL);\n"));
    }
    for (total, floor, max, behind, fr) in action_vectors() {
        body.push_str(&format!(
            "\taction_line({total}, {floor}, {max}, {}, {});\n",
            if behind { "true" } else { "false" },
            if fr { "true" } else { "false" }
        ));
    }
    for (enc, settled) in frozen_vectors() {
        body.push_str(&format!(
            "\tfrozen_line({}, {});\n",
            if enc { "true" } else { "false" },
            if settled { "true" } else { "false" }
        ));
    }
    let tf = |b: bool| if b { "true" } else { "false" };
    for (ms, max, fixed, rate) in plan_box_vectors() {
        body.push_str(&format!(
            "\tplanb_line({}, {}u, {max}u, {}, {rate}u);\n",
            tf(ms.is_some()),
            ms.unwrap_or(0),
            tf(fixed)
        ));
    }
    for (fixed, enc, settled) in frozen_plan_vectors() {
        body.push_str(&format!(
            "\tfrozenp_line({}, {}, {});\n",
            tf(fixed),
            tf(enc),
            tf(settled)
        ));
    }
    for b in BOX_MS_BYTES {
        let lit: String = b.iter().map(|x| format!("\\x{x:02x}")).collect();
        body.push_str(&format!("\tparse_line(\"{lit}\", {}u);\n", b.len()));
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

static void planf_line(uint32_t floor, uint32_t max, bool fixed, uint32_t rate, uint32_t frames)
{{
	const struct genlock_audio_buffering_plan p =
		genlock_audio_buffering_make_plan_floor(floor, max, fixed, rate, frames);
	printf("planf %" PRIu32 " %" PRIu32 " %d %" PRIu32 " %" PRIu32 " %" PRIu32 " %" PRIu32 " %d %d\n", floor, max,
	       fixed ? 1 : 0, rate, frames, p.floor_ticks, p.max_ticks, p.fixed ? 1 : 0, p.overridden ? 1 : 0);
}}

static void settled_line(uint64_t first, uint64_t now)
{{
	printf("settled %" PRIu64 " %" PRIu64 " %d\n", first, now,
	       genlock_audio_buffering_launch_settled(first, now) ? 1 : 0);
}}

static void action_line(int total, int floor, int max, bool behind, bool frozen)
{{
	printf("action %d %d %d %d %d %d\n", total, floor, max, behind ? 1 : 0, frozen ? 1 : 0,
	       genlock_audio_buffering_action(total, floor, max, behind, frozen));
}}

static void frozen_line(bool enc, bool settled)
{{
	printf("frozen %d %d %d\n", enc ? 1 : 0, settled ? 1 : 0, genlock_audio_buffering_frozen(enc, settled) ? 1 : 0);
}}

static void planb_line(bool has, uint32_t ms, uint32_t max, bool fixed, uint32_t rate)
{{
	const struct genlock_audio_buffering_plan p =
		genlock_audio_buffering_make_plan_box(has, ms, max, fixed, rate, 1024u);
	printf("planb %d %" PRIu32 " %" PRIu32 " %d %" PRIu32 " %" PRIu32 " %" PRIu32 " %d %d %d\n", has ? 1 : 0, ms,
	       max, fixed ? 1 : 0, rate, p.floor_ticks, p.max_ticks, p.fixed ? 1 : 0, p.overridden ? 1 : 0,
	       p.box_fixed ? 1 : 0);
}}

static void frozenp_line(bool fixed, bool enc, bool settled)
{{
	printf("frozenp %d %d %d %d\n", fixed ? 1 : 0, enc ? 1 : 0, settled ? 1 : 0,
	       genlock_audio_buffering_frozen_plan(fixed, enc, settled) ? 1 : 0);
}}

static void parse_line(const char *buf, size_t len)
{{
	uint32_t ms = 12345u;
	const bool ok = genlock_audio_buffering_parse_box_ms(buf, len, &ms);
	printf("parse ");
	for (size_t i = 0; i < len; i++)
		printf("%02x", (unsigned)(unsigned char)buf[i]);
	printf(" %d %" PRIu32 "\n", ok ? 1 : 0, ok ? ms : 0u);
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
        has("plan 20 1 48000 1024 0 45 0 1")
            && has("plan 0 0 48000 1024 0 45 0 0")
            && has("plan 85 0 48000 1024 0 4 0 0")
            && has("plan 0 0 768000 1024 0 45 0 0")
            && has("action 0 0 45 1 0 2")
            && has("action 0 0 45 1 1 0")
            && has("action 0 0 45 0 0 0")
            && has("action 0 4 45 0 0 1")
            && has("action 0 4 45 1 1 1")
            && has("action 4 4 45 1 0 2")
            && has("action 4 4 45 1 1 0")
            && has("action 45 4 45 1 0 0")
            && has("planf 85 50 0 48000 1024 4 45 0 1")
            && has("planf 85 0 0 768000 1024 64 64 0 1")
            && has("planf 21 0 0 48000 1024 1 45 0 0")
            && has("settled 1000000000 30999999999 0")
            && has("settled 1000000000 31000000000 1")
            && has("settled 1000000000 999999999 0")
            && has("settled 0 1000000000 0")
            && has("frozen 0 1 0")
            && has("frozen 1 0 0")
            && has("frozen 1 1 1")
            && has("planb 1 0 0 0 48000 0 45 0 0 1")
            && has("planb 1 85 0 0 48000 4 45 0 0 1")
            && has("planb 1 960 0 0 48000 45 45 0 0 1")
            && has("planb 0 0 0 0 48000 0 45 0 0 0")
            && has("frozenp 1 0 0 1")
            && has("frozenp 0 1 0 0")
            && has("parse 3936300a 1 960")
            && has("parse 393631 0 0")
            && has("parse 38350d0a 0 0"),
        "issue 1367: the parity vectors no longer reach the 0 floor's stock growth, a floor raise, \
         the override, the maximum, the dynamic increase, the freeze and the launch settle window"
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
        lift_define(&h, "GENLOCK_AUDIO_BUFFERING_LAUNCH_SETTLE_NS"),
        LAUNCH_SETTLE_NS.to_string()
    );
    assert_eq!(
        lift_define(&h, "GENLOCK_AUDIO_BUFFERING_DEFAULT_MAX_TICKS"),
        DEFAULT_MAX_TICKS.to_string()
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
    // Issue 1425 E2 slice 3: no floor.
    assert_eq!(lift_define(&h, "GENLOCK_AUDIO_BUFFERING_FLOOR_MS"), "0");
    // Issue 1425 (design 6443440513): the per-box fixed buffering's ceiling = OBS's own maximum.
    assert_eq!(
        lift_define(&h, "GENLOCK_AUDIO_BUFFERING_BOX_MAX_MS"),
        BOX_MAX_MS.to_string()
    );
    assert_eq!(BOX_MAX_MS, 960);
}

#[test]
fn a_fixed_plan_never_grows_from_its_first_tick_1425() {
    // Issue 1425 (design 6443440513): under a box plan the action is never DYNAMIC -- at tick 0 the
    // floor is raised (or nothing at 0 ms), afterwards a late source is never a growth, with or
    // without an encoder, inside or after the launch settle window.
    for ms in [0_u32, 21, 85, 960] {
        for rate in [44_100_u32, 48_000] {
            let p = plan_box(Some(ms), 0, false, rate, AUDIO_OUTPUT_FRAMES);
            assert!(p.box_fixed && !p.fixed);
            for enc in [false, true] {
                for settled in [false, true] {
                    let fr = frozen_plan(true, enc, settled);
                    assert!(
                        fr,
                        "a fixed plan must be frozen (enc={enc} settled={settled})"
                    );
                    for total in 0..=p.max_ticks as i32 {
                        let a = action(total, p.floor_ticks as i32, p.max_ticks as i32, true, fr);
                        assert_ne!(a, BufferingAction::Dynamic, "ms={ms} total={total}");
                        if total < p.floor_ticks as i32 {
                            assert_eq!(a, BufferingAction::Floor);
                        }
                    }
                }
            }
        }
    }
    // No marker: today's plan exactly.
    for (max, fixed) in [(0_u32, false), (20, true)] {
        let mut want = plan(max, fixed, 48_000, AUDIO_OUTPUT_FRAMES);
        want.box_fixed = false;
        assert_eq!(
            plan_box(None, max, fixed, 48_000, AUDIO_OUTPUT_FRAMES),
            want
        );
    }
    assert!(!frozen_plan(false, false, true));
    assert!(!frozen_plan(false, true, false));
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
    // launch BAD above their bound. The floor itself, and the up to four ticks a late ASIO /
    // PulseAudio start adds on top of it (issue 1425: stock OBS drew 0, 64 or 85 ms per launch),
    // must read as a clean draw on every one of them at both production rates, or every launch
    // would be redrawn (the guarded launcher kills OBS up to three times).
    let bounds = [
        launch_gate_bound("scripts/rig-health-audit.py", "AUDIO_BUF_BOUND_MS = "),
        launch_gate_bound("scripts/obs-guarded-launch.ps1", "$threshold  = "),
        launch_gate_bound("scripts/launch-obs-genlock.sh", "$bufPeak -le "),
        launch_gate_bound("scripts/launch-obs-genlock.sh", "$d.Peak -le "),
    ];
    for rate in [44_100, 48_000] {
        let floor = plan(0, false, rate, AUDIO_OUTPUT_FRAMES).floor_ticks;
        for ticks in [floor, floor + 1, floor + 2, floor + 3, floor + 4] {
            // obs-audio.c logs the total as ticks * frames * 1000 / rate, truncated.
            let logged =
                (u64::from(ticks) * u64::from(AUDIO_OUTPUT_FRAMES) * 1000 / u64::from(rate)) as u32;
            for b in bounds {
                assert!(
                    logged <= b,
                    "issue 1367: a {logged} ms launch draw at {rate} Hz is over a #786 launch-gate \
                     bound of {b} ms"
                );
            }
        }
    }
}
