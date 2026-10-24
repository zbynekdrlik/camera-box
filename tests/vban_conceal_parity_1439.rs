//! Issue 1439 — EXECUTABLE C-vs-Rust parity gate for the concealment math of the obs-vban receive
//! source (ROZHODNUTÉ 6352253147 item (b)).
//!
//! The patched source (`vendor/obs-vban/src/vban-source.c`) builds a concealed slot from the pure
//! math in `vendor/obs-vban/src/vban-conceal.h`; the plugin only compiles in the genlock CI builds,
//! so this gate `#include`s the SHIPPED header by its absolute path into a driver, compiles it with
//! `cc -std=c11 -ffp-contract=off -Wall -Wextra -Wconversion -Wsign-conversion -Wformat=2 -Werror`
//! and requires every result to equal the Tier-0 authority `src/vban_conceal.rs` bit for bit: the
//! fade length over packet sizes and rates (1 ms, a quarter of the slot, one frame), the cosine over
//! a grid of [0, 1] with both sides of 1/2, the weights of every fade length used, and whole
//! concealed slots of sines (mbc's 239 frames, lv1's 256, a one-frame slot whose two fades overlap) with and
//! without the fade toward the next packet.
//!
//! Std-only; standalone with no cargo:
//!
//! ```text
//! CARGO_MANIFEST_DIR=<worktree-abs> rustc --test --edition 2021 tests/vban_conceal_parity_1439.rs -o /tmp/t && /tmp/t
//! ```
//!
//! `cc` is required; a missing compiler FAILS, never skips.

#[path = "../src/vban_conceal.rs"]
mod vban_conceal;

#[allow(dead_code)]
#[path = "support/vban_c_1439.rs"]
mod vban_c;

use std::fmt::Write as _;
use vban_c::{compile_and_run, repo};
use vban_conceal::{cospi, fade_frames, sample, weight};

const HEADER: &str = "vendor/obs-vban/src/vban-conceal.h";

const FADES: [(u32, u32); 9] = [
    (239, 48_000),
    (256, 48_000),
    (128, 48_000),
    (256, 44_100),
    (64, 8_000),
    (3, 48_000),
    (1, 48_000),
    (256, 500),
    (103, 96_000),
];

/// The cosine's inputs: a grid of [0, 1] (both sides of 1/2, its edges) and a few odd values.
fn cos_inputs() -> Vec<f64> {
    let mut v: Vec<f64> = (0..=64).map(|k| f64::from(k) / 64.0).collect();
    v.extend([0.499_999_999, 0.500_000_001, 1.0 / 3.0, 0.987_654_321, 1e-9]);
    v
}

/// One concealed slot's inputs: the previous packet (a sine), the last sample handed out, the next
/// packet's first sample, the fade toward it or not, the fade length.
struct Slot {
    rep: Vec<f64>,
    last: f64,
    next: f64,
    toward: bool,
    fade: u32,
}

fn slots() -> Vec<Slot> {
    let mut v = Vec::new();
    for (frames, f, phase) in [(239u32, 1_000.0, 0.3), (256, 3_100.0, 1.7), (1, 440.0, 2.2)] {
        let rep: Vec<f64> = (0..frames)
            .map(|n| 0.6 * (2.0 * std::f64::consts::PI * f * f64::from(n) / 48_000.0 + phase).sin())
            .collect();
        for toward in [true, false] {
            v.push(Slot {
                last: rep[rep.len() - 1] * 0.97,
                next: -0.41,
                toward,
                fade: fade_frames(frames, 48_000),
                rep: rep.clone(),
            });
        }
    }
    v
}

fn rust_trace() -> Vec<String> {
    let mut out = Vec::new();
    for (frames, rate) in FADES {
        out.push(format!(
            "fade {frames} {rate} {}",
            fade_frames(frames, rate)
        ));
    }
    for u in cos_inputs() {
        out.push(format!("cospi {} {}", u.to_bits(), cospi(u).to_bits()));
    }
    for n in [1u32, 2, 12, 44, 48, 59] {
        for i in 0..n {
            out.push(format!("weight {i} {n} {}", weight(i, n).to_bits()));
        }
    }
    for (k, s) in slots().iter().enumerate() {
        let frames = s.rep.len() as u32;
        for i in 0..frames {
            let x = sample(
                s.rep[i as usize],
                s.last,
                s.next,
                s.toward,
                i,
                frames,
                s.fade,
            );
            out.push(format!("sample {k} {i} {}", x.to_bits()));
        }
    }
    out
}

fn c_driver() -> String {
    let mut c = String::new();
    writeln!(
        c,
        "#include <stdio.h>\n#include <inttypes.h>\n#include <string.h>\n#include \"{}\"",
        repo(HEADER).display()
    )
    .unwrap();
    c.push_str(
        "static uint64_t bits(double x)\n{\n\tuint64_t u;\n\tmemcpy(&u, &x, sizeof(u));\n\treturn u;\n}\n\
         static double num(uint64_t u)\n{\n\tdouble x;\n\tmemcpy(&x, &u, sizeof(x));\n\treturn x;\n}\n",
    );
    for (k, s) in slots().iter().enumerate() {
        let reps: Vec<String> = s
            .rep
            .iter()
            .map(|x| format!("{}ULL", x.to_bits()))
            .collect();
        writeln!(
            c,
            "static const uint64_t rep_{k}[] = {{{}}};",
            reps.join(", ")
        )
        .unwrap();
    }
    c.push_str("int main(void)\n{\n");
    for (frames, rate) in FADES {
        writeln!(
            c,
            "\tprintf(\"fade {frames} {rate} %\" PRIu32 \"\\n\", vban_conceal_fade_frames({frames}u, {rate}u));"
        )
        .unwrap();
    }
    for u in cos_inputs() {
        writeln!(
            c,
            "\tprintf(\"cospi {b} %\" PRIu64 \"\\n\", bits(vban_conceal_cospi(num({b}ULL))));",
            b = u.to_bits()
        )
        .unwrap();
    }
    for n in [1u32, 2, 12, 44, 48, 59] {
        writeln!(
            c,
            "\tfor (uint32_t i = 0; i < {n}u; i++)\n\
             \t\tprintf(\"weight %\" PRIu32 \" {n} %\" PRIu64 \"\\n\", i, bits(vban_conceal_weight(i, {n}u)));"
        )
        .unwrap();
    }
    for (k, s) in slots().iter().enumerate() {
        writeln!(
            c,
            "\tfor (uint32_t i = 0; i < {frames}u; i++)\n\
             \t\tprintf(\"sample {k} %\" PRIu32 \" %\" PRIu64 \"\\n\", i, bits(vban_conceal_sample(num(rep_{k}[i]), \
             num({last}ULL), num({next}ULL), {toward}, i, {frames}u, {fade}u)));",
            frames = s.rep.len(),
            last = s.last.to_bits(),
            next = s.next.to_bits(),
            toward = if s.toward { "true" } else { "false" },
            fade = s.fade,
        )
        .unwrap();
    }
    c.push_str("\treturn 0;\n}\n");
    c
}

#[test]
fn c_conceal_math_matches_the_rust_authority_1439() {
    assert!(
        repo(HEADER).is_file(),
        "issue 1439: {HEADER} is missing -- a lost slot cannot be concealed"
    );
    let c = compile_and_run("conceal", &c_driver(), &[]);
    let r = rust_trace();
    assert_eq!(
        c.len(),
        r.len(),
        "issue 1439: C and Rust traces differ in length"
    );
    let diffs: Vec<String> = c
        .iter()
        .zip(&r)
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .take(5)
        .map(|(i, (a, b))| format!("line {i}:\n  C    `{a}`\n  Rust `{b}`"))
        .collect();
    assert!(
        diffs.is_empty(),
        "issue 1439: the vendored concealment math diverges from src/vban_conceal.rs:\n{}",
        diffs.join("\n")
    );
}

/// The gate is only as good as what it visits: overlapping fades (a 3-frame slot), fades of 1 ms and
/// of a quarter slot, both halves of the cosine, both fade directions.
#[test]
fn the_parity_inputs_reach_every_branch_1439() {
    let fades: Vec<u32> = FADES.iter().map(|&(f, r)| fade_frames(f, r)).collect();
    assert!(
        fades.contains(&48) && fades.contains(&32) && fades.contains(&1),
        "{fades:?}"
    );
    let u = cos_inputs();
    assert!(
        u.iter().any(|&x| x < 0.5) && u.iter().any(|&x| x > 0.5),
        "{u:?}"
    );
    let s = slots();
    assert!(s.iter().any(|s| s.toward) && s.iter().any(|s| !s.toward));
    assert!(
        s.iter().any(|s| 2 * s.fade > s.rep.len() as u32 - 1),
        "a slot whose two fades overlap"
    );
}
