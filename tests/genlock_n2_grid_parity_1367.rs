//! Issue 1367 slice D1 — the EXECUTABLE C-vs-Rust parity gate for the grid-exact N>=2 conveyor.
//!
//! `src/genlock_n2_grid.rs` is the Tier-0 authority; the contiguous `genlock_n2_*` block in
//! `vendor/obs-studio/libobs/obs-source.c` is the production port. This gate lifts that block
//! VERBATIM (from `struct genlock_n2_pick {` to the end of `genlock_n2_select`) plus the five
//! `#define`s it reads, compiles it standalone under `-Werror` against the REAL `obs-genlock-grid.h`
//! and the minimal `obs_source_t` stub of the #1003 gate, runs it over a spread of vectors, and
//! requires byte-identical results from the Rust authority — the tick instant, the target stamp,
//! the pick (index + kind) and the drop-cap headroom. Same discipline as `tests/genlock_relock_selection_parity.rs`; it FAILS
//! LOUDLY rather than skipping when no C compiler is present.

use camera_box::genlock_grid::{grid_advance_ns, per_second_floor, UNITS_100NS_PER_SECOND};
use camera_box::genlock_n2_grid::{
    n2_drop_cap_extra_frames, n2_select, n2_source_interval_ns, n2_target_stamp_ns, n2_tick_ns,
};
use std::fs;
use std::path::PathBuf;

mod genlock_n1_lift;
use genlock_n1_lift::{compile_and_run_c, lift_define, RELOCK_PRELUDE};

const OBS_SOURCE: &str = "vendor/obs-studio/libobs/obs-source.c";
const GRID_H: &str = "vendor/obs-studio/libobs/obs-genlock-grid.h";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// Lift the contiguous `genlock_n2_*` block verbatim.
fn lift_n2_block() -> String {
    let src = fs::read_to_string(repo(OBS_SOURCE)).expect("read obs-source.c");
    let start = src.find("struct genlock_n2_pick {").unwrap_or_else(|| {
        panic!(
            "issue 1367 D1: {OBS_SOURCE} no longer defines struct genlock_n2_pick — the grid-exact \
             N>=2 conveyor block is gone, so there is nothing to check parity against."
        )
    });
    let last = src[start..]
        .find("static inline struct genlock_n2_pick genlock_n2_select(")
        .map(|i| start + i)
        .unwrap_or_else(|| {
            panic!("issue 1367 D1: genlock_n2_select is gone or no longer follows its struct")
        });
    let end = src[last..]
        .find("\n}\n")
        .map(|i| last + i + 3)
        .expect("issue 1367 D1: genlock_n2_select has no closing brace");
    src[start..end].to_string()
}

fn prelude() -> String {
    let defines = [
        "GENLOCK_N2_AGE_BASE_NS",
        "GENLOCK_N2_HOLD",
        "GENLOCK_N2_ON_TARGET",
        "GENLOCK_N2_EARLY",
        "GENLOCK_MAX_SOURCE_FPS",
    ]
    .iter()
    .map(|name| lift_define(name))
    .collect::<Vec<_>>()
    .join("\n");
    format!(
        "{RELOCK_PRELUDE}#include \"{}\"\n{defines}\n",
        repo(GRID_H).display()
    )
}

const I30: u64 = 33_333_333;
const I60: u64 = 16_666_666;
const I2997: u64 = 33_366_666;
/// A whole second.
const S0: u64 = 1_790_640_000_000_000_000;

/// The camera sender stamp of the 60 fps slot `slot` counted from `S0`, captured `into` ns into it.
fn stamp(slot: u64, into: u64) -> u64 {
    let capture = grid_advance_ns(S0, slot, I60) + into;
    per_second_floor(capture / 100, 60, UNITS_100NS_PER_SECOND) * 100
}

/// `(tick_wall, wall_now, canvas_interval, on_grid, pin_ns, n)`.
fn target_vectors() -> Vec<(u64, u64, u64, bool, u64, u32)> {
    let mut v = Vec::new();
    for canvas in [I30, 16_666_667, I2997] {
        for j in [0u64, 1, 2, 7, 29, 30, 31, 59] {
            let g = grid_advance_ns(S0, j, canvas);
            for off in [
                -2_000_000i64,
                -3_000,
                -1,
                0,
                1,
                700_000,
                1_999_999,
                2_000_000,
                12_000_000,
            ] {
                let tw = g.saturating_add_signed(off);
                for on_grid in [true, false] {
                    for pin_ms in [1u64, 3, 16, 17, 19, 50, 987, 2000] {
                        for n in [2u32, 3] {
                            v.push((tw, tw + 400_000, canvas, on_grid, pin_ms * 1_000_000, n));
                        }
                    }
                }
            }
        }
    }
    // The pin edges one ns either side of an exact source-slot age (50 ms + pin == k slots).
    let g = grid_advance_ns(S0, 3, I30);
    for k in 4u64..8 {
        let exact = k * 1_000_000_000 / 60 - 50_000_000;
        for pin in [exact - 1, exact, exact + 1] {
            v.push((g, g, I30, true, pin, 2));
        }
    }
    // Saturation: a tick younger than the age, n == 0, interval 0, the top of the range.
    v.push((10_000_000, 10_000_000, I30, true, 3_000_000, 2));
    v.push((S0, S0, I30, true, 3_000_000, 0));
    v.push((S0, S0, 0, true, 3_000_000, 2));
    v.push((u64::MAX, u64::MAX, I30, true, 3_000_000, 2));
    v.push((u64::MAX, u64::MAX, I30, false, u64::MAX, 2));
    v
}

#[test]
fn c_n2_tick_and_target_match_the_rust_authority_1367() {
    let vs = target_vectors();
    let mut c = prelude();
    c.push_str(&lift_n2_block());
    c.push_str("int main(void){\n");
    for (tw, wall, canvas, on_grid, pin, n) in &vs {
        c.push_str(&format!(
            "    {{ const uint64_t t = genlock_n2_tick_ns({tw}ULL, {wall}ULL, {canvas}ULL, {});\n\
             \x20     printf(\"%llu %llu %llu\\n\", (unsigned long long)t, \
             (unsigned long long)genlock_n2_target_stamp_ns(t, {pin}ULL, {canvas}ULL, {n}u), \
             (unsigned long long)genlock_n2_source_interval_ns({canvas}ULL, {n}u)); }}\n",
            if *on_grid { "true" } else { "false" }
        ));
    }
    c.push_str("    return 0;\n}\n");
    let out = compile_and_run_c("genlock_n2_target_parity_1367", &c);
    assert_eq!(out.len(), vs.len(), "the harness printed the wrong count");
    let mut diffs = Vec::new();
    let mut distinct_targets = std::collections::BTreeSet::new();
    for (i, ((tw, wall, canvas, on_grid, pin, n), got_c)) in vs.iter().zip(&out).enumerate() {
        let t = n2_tick_ns(*tw, *wall, *canvas, *on_grid);
        let target = n2_target_stamp_ns(t, *pin, *canvas, *n);
        distinct_targets.insert(target);
        let want = format!("{t} {target} {}", n2_source_interval_ns(*canvas, *n));
        if *got_c != want {
            diffs.push(format!(
                "  vector {i}: tick_wall={tw} wall={wall} canvas={canvas} on_grid={on_grid} \
                 pin={pin} n={n} -> C `{got_c}`, Rust `{want}`"
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "issue 1367 D1: the vendored C N>=2 tick / target DIVERGED from the Tier-0 Rust authority on \
         {} of {} vectors:\n{}",
        diffs.len(),
        vs.len(),
        diffs.join("\n")
    );
    assert!(
        distinct_targets.len() > 100,
        "the vectors must exercise many targets"
    );
}

/// `(queue stamps, target, source interval)`.
fn select_vectors() -> Vec<(Vec<u64>, u64, u64)> {
    let mut v = Vec::new();
    for slot in [96u64, 97, 120, 121, 179] {
        let target = grid_advance_ns(S0, slot, I60);
        // Queues around the target: which slots arrived, and how far into its slot each was
        // captured (the 100 ns stamp floor sits up to 99 ns before the grid point).
        for (first, count) in [
            (-3i64, 5u64),
            (-1, 4),
            (-2, 2),
            (-1, 1),
            (1, 3),
            (0, 1),
            (-17, 17),
            (-4, 1),
        ] {
            for into in [1u64, 999_999, 16_000_000] {
                let q: Vec<u64> = (0..count)
                    .map(|k| stamp((slot as i64 + first + k as i64) as u64, into))
                    .collect();
                v.push((q, target, I60));
            }
        }
        let half = I60 / 2;
        // The slack edges, both sides.
        v.push((vec![target - I60, target + half], target, I60));
        v.push((vec![target - I60, target + half + 1], target, I60));
        v.push((vec![target - half], target, I60));
        v.push((vec![target - half - 1], target, I60));
        // Duplicate stamps (a grabber beat) at and before the target.
        v.push((
            vec![target - I60, target, target, target + I60],
            target,
            I60,
        ));
        v.push((vec![target - I60, target - I60, target + I60], target, I60));
        // A non-monotonic seam: the leading run ends at the first stamp past the limit.
        v.push((vec![target - I60, target + I60, target], target, I60));
        // Another multiple (N == 3 of a 30 fps canvas).
        v.push((
            vec![target - 11_111_111, target, target + 11_111_111],
            target,
            11_111_111,
        ));
    }
    v.push((Vec::new(), S0, I60));
    v.push((vec![u64::MAX - 5], u64::MAX - 3, I60));
    v.push((vec![u64::MAX], u64::MAX, I60));
    v.push((vec![0, 1, 2], 0, 0));
    v
}

#[test]
fn c_n2_select_matches_the_rust_authority_1367() {
    let vs = select_vectors();
    let mut c = prelude();
    c.push_str(&lift_n2_block());
    c.push_str(
        "int main(void){\n    struct obs_source_frame f[64];\n    struct obs_source_frame *pf[64];\n    obs_source_t s;\n    s.genlock_phase_anchor_ns = 0;\n",
    );
    for (q, target, si) in &vs {
        let fill: String = q
            .iter()
            .enumerate()
            .map(|(i, ts)| format!("f[{i}].timestamp = {ts}ULL; pf[{i}] = &f[{i}]; "))
            .collect();
        c.push_str(&format!(
            "    {{ {fill}s.async_frames.array = pf; s.async_frames.num = {};\n\
             \x20     const struct genlock_n2_pick p = genlock_n2_select(&s, {target}ULL, {si}ULL);\n\
             \x20     printf(\"%zu %d\\n\", p.index, p.kind); }}\n",
            q.len()
        ));
    }
    c.push_str("    return 0;\n}\n");
    let out = compile_and_run_c("genlock_n2_select_parity_1367", &c);
    assert_eq!(out.len(), vs.len(), "the harness printed the wrong count");
    let mut diffs = Vec::new();
    let mut kinds = [0usize; 3];
    for (i, ((q, target, si), got_c)) in vs.iter().zip(&out).enumerate() {
        let pick = n2_select(q, *target, *si);
        kinds[pick.kind.code() as usize] += 1;
        let want = format!("{} {}", pick.index, pick.kind.code());
        if *got_c != want {
            diffs.push(format!(
                "  vector {i}: queue={q:?} target={target} si={si} -> C `{got_c}`, Rust `{want}`"
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "issue 1367 D1: the vendored C N>=2 pick DIVERGED from the Tier-0 Rust authority on {} of {} \
         vectors:\n{}",
        diffs.len(),
        vs.len(),
        diffs.join("\n")
    );
    assert!(
        kinds.iter().all(|&k| k > 5),
        "every kind must be exercised (hold, on target, early): {kinds:?}"
    );
}

/// `(fps_num, fps_den)` of the canvas rates, the degenerate ones and the u32 edges.
fn drop_cap_vectors() -> Vec<(u32, u32)> {
    let mut v = vec![
        (30, 1),
        (30_000, 1001),
        (25, 1),
        (60, 1),
        (24_000, 1001),
        (60_000, 1001),
        (50, 1),
        (1, 1),
        (0, 1),
        (0, 0),
        (30, 0),
        (u32::MAX, 1),
        (1, u32::MAX),
        (u32::MAX, u32::MAX),
    ];
    for num in [7u32, 29, 31, 59, 61, 119, 120, 240] {
        for den in [1u32, 2, 3, 1001] {
            v.push((num, den));
        }
    }
    v
}

#[test]
fn c_n2_drop_cap_extra_matches_the_rust_authority_1367() {
    let vs = drop_cap_vectors();
    let mut c = prelude();
    c.push_str(&lift_n2_block());
    c.push_str("int main(void){\n");
    for (num, den) in &vs {
        c.push_str(&format!(
            "    printf(\"%u\\n\", genlock_n2_drop_cap_extra_frames({num}u, {den}u));\n"
        ));
    }
    c.push_str("    return 0;\n}\n");
    let out = compile_and_run_c("genlock_n2_drop_cap_parity_1367", &c);
    assert_eq!(out.len(), vs.len(), "the harness printed the wrong count");
    let mut diffs = Vec::new();
    let mut distinct = std::collections::BTreeSet::new();
    for (i, ((num, den), got_c)) in vs.iter().zip(&out).enumerate() {
        let want = n2_drop_cap_extra_frames(*num, *den);
        distinct.insert(want);
        if *got_c != want.to_string() {
            diffs.push(format!(
                "  vector {i}: fps {num}/{den} -> C `{got_c}`, Rust `{want}`"
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "issue 1367 D1: the vendored C N>=2 drop-cap headroom DIVERGED from the Tier-0 Rust \
         authority on {} of {} vectors:\n{}",
        diffs.len(),
        vs.len(),
        diffs.join("\n")
    );
    assert!(
        distinct.len() > 8,
        "the vectors must exercise many headroom values: {distinct:?}"
    );
}
