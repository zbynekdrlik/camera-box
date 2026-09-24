//! #1355 — EXECUTABLE C-vs-Rust parity gates for the ONE per-second genlock grid and the
//! arrival-side stamp tracker (split out of `tests/genlock_relock_selection_parity.rs`, the same
//! discipline: compile the SHIPPED C, run it over a spread of vectors, require byte-identical
//! results from the Tier-0 Rust authority `camera_box::genlock_grid`).
//!
//! The header `vendor/obs-studio/libobs/obs-genlock-grid.h` is pure `<stdint.h>`, so it is
//! `#include`d as-is (no lift); the DistroAV sender floor is lifted VERBATIM from
//! `vendor/distroav/src/ndi-output.cpp`. `cc` is required — per the project's test-strictness rule
//! this FAILS LOUDLY rather than skipping when the toolchain is missing.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

// ---------------------------------------------------------------------------------------------
// #1355 — the ONE per-second genlock grid. The receiver deadline (obs-source.c
// genlock_phase_pin_deadline) and the render tick (obs-video.c genlock_next_deadline) both call
// the shared header vendor/obs-studio/libobs/obs-genlock-grid.h. This gate compiles that REAL
// header (an #include, never a retyped copy), lifts the DistroAV sender's stamp floor
// (genlock_floor_boundary_100ns) VERBATIM from ndi-output.cpp, and requires:
//   1. the C floor / next-boundary byte-identical to camera_box::genlock_grid over vectors that
//      span a WHOLE DAY (the 10 ns/s drift of the old 1970 grid is invisible in short vectors)
//      plus every exactly-on-boundary / one-before case of a second;
//   2. the C sender stamp byte-identical to the Rust per_second_floor in 100 ns units;
//   3. the coincidence the fix exists for: every sender stamp has a receiver grid point within
//      [stamp, stamp + 100 ns) — on the pre-#1355 1970 grid this is off by milliseconds.
// ---------------------------------------------------------------------------------------------

const GENLOCK_GRID_H: &str = "vendor/obs-studio/libobs/obs-genlock-grid.h";
const NDI_OUTPUT: &str = "vendor/distroav/src/ndi-output.cpp";
/// 2026-09-23 17:16:33 UTC — the second of the live stream audit line #1355 measured.
const SEC_1355: u64 = 1_790_176_593;

/// Lift the DistroAV sender's per-second FLOOR stamp helper + the units constant it reads.
fn lift_sender_floor() -> String {
    let path = repo(NDI_OUTPUT);
    let src = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let units = src
        .lines()
        .find(|l| l.starts_with("static const int64_t GENLOCK_UNITS_PER_SECOND"))
        .unwrap_or_else(|| {
            panic!("#1355: {NDI_OUTPUT} no longer defines GENLOCK_UNITS_PER_SECOND")
        });
    let start = src
        .find("static int64_t genlock_floor_boundary_100ns(")
        .unwrap_or_else(|| {
            panic!("#1355: {NDI_OUTPUT} no longer defines genlock_floor_boundary_100ns")
        });
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .expect("#1355: genlock_floor_boundary_100ns has no closing brace");
    format!("{units}\n{}", &src[start..end])
}

/// `(t_ns, interval_ns)` vectors: a whole day at two sub-second phases, every slot boundary of a
/// second (ON, one before, one after) at 30 and 60 fps, second roll-overs on several dates, and
/// the fractional / degenerate intervals that must keep the old arithmetic.
fn grid_vectors() -> Vec<(u64, u64)> {
    const NS: u64 = 1_000_000_000;
    let intervals = [
        33_333_333u64, // 30 fps canvas
        16_666_666,    // 60 fps canvas
        16_666_667,    // a rounded-up 60 fps interval
        40_000_000,    // 25 fps (per-second == 1970 grid, both exact)
        41_666_666,    // 24 fps
        33_366_666,    // 29.97 — fractional, no per-second grid
        30,            // tiny interval (legacy unit-test scale)
        0,             // unknown video info
    ];
    let mut v = Vec::new();
    let day0 = SEC_1355 * NS;
    let mut x: u64 = 0x1355_0000_2026_0924;
    for &iv in &intervals {
        // A whole day, every ~7.2 minutes, at a fixed and at a random sub-second phase.
        for i in 0..200u64 {
            let t = day0 + i * (86_400 * NS / 200);
            v.push((t + 123_456_789, iv));
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            v.push((t + (x >> 11) % NS, iv));
        }
        // Whole seconds and their neighbours, on several dates.
        for days in [0u64, 1, 17, 365] {
            let s = (SEC_1355 + days * 86_400) * NS;
            v.push((s, iv));
            v.push((s - 1, iv));
            v.push((s + 1, iv));
        }
    }
    // Every slot boundary of one second, exactly ON, one before and one after.
    for (fps, iv) in [(30u64, 33_333_333u64), (60, 16_666_666), (60, 16_666_667)] {
        let sec = day0 + NS;
        for k in 0..=fps {
            let b = sec + k * NS / fps;
            v.push((b, iv));
            v.push((b - 1, iv));
            v.push((b + 1, iv));
        }
    }
    v.push((0, 33_333_333));
    v.push((1, 33_333_333));
    v
}

#[test]
fn c_genlock_grid_matches_the_rust_authority_and_the_sender_grid_1355() {
    use camera_box::genlock_grid::{
        grid_floor_ns, grid_next_boundary_ns, integer_fps, per_second_floor, UNITS_100NS_PER_SECOND,
    };

    let header = repo(GENLOCK_GRID_H);
    assert!(
        header.exists(),
        "#1355: {GENLOCK_GRID_H} is missing — the receiver deadline and the render tick have no \
         shared per-second grid helper, so they cannot coincide with the senders' stamps."
    );
    let sender = lift_sender_floor();
    let vs = grid_vectors();

    let mut c = String::new();
    c.push_str("#include <stdint.h>\n#include <stdio.h>\n");
    c.push_str(&format!("#include \"{}\"\n", header.display()));
    c.push_str(&sender);
    c.push_str("\nint main(void){\n");
    for (t, iv) in &vs {
        // The sender stamp at the canvas's own integer rate (0 = no per-second grid -> 0).
        let fps = integer_fps(*iv).unwrap_or(0);
        c.push_str(&format!(
            "    printf(\"%llu %llu %lld\\n\", \
             (unsigned long long)genlock_grid_floor_ns({t}ULL, {iv}ULL), \
             (unsigned long long)genlock_grid_next_boundary_ns({t}ULL, {iv}ULL), \
             (long long)({fps}LL > 0 ? genlock_floor_boundary_100ns((int64_t){}LL, {fps}LL) * 100LL : 0LL));\n",
            t / 100
        ));
    }
    c.push_str("    return 0;\n}\n");

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("genlock_parity_1355");
    fs::create_dir_all(&dir).expect("create the parity scratch dir");
    let cfile = dir.join("grid.c");
    let bin = dir.join("grid.bin");
    fs::write(&cfile, &c).expect("write the grid harness");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=gnu99",
            "-Wall",
            "-Wextra",
            "-Wconversion",
            "-Werror",
            "-O1",
        ])
        .arg(&cfile)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "#1355: could not run the C compiler `{cc}` ({e}). This gate must FAIL rather \
                 than skip when the toolchain is absent. Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "#1355: {GENLOCK_GRID_H} + the lifted sender floor do NOT COMPILE standalone under \
         -Wall -Wextra -Wconversion -Werror:\n--- cc stderr ---\n{}\n--- harness ---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin)
        .output()
        .expect("#1355: the compiled grid harness failed to execute");
    let stdout = String::from_utf8(run.stdout).expect("harness stdout is utf-8");
    let rows: Vec<(u64, u64, i64)> = stdout
        .lines()
        .map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (
                f[0].parse().expect("floor"),
                f[1].parse().expect("next"),
                f[2].parse().expect("stamp"),
            )
        })
        .collect();
    assert_eq!(
        rows.len(),
        vs.len(),
        "#1355: harness printed the wrong count"
    );

    let mut diffs = Vec::new();
    let mut coincide = Vec::new();
    for (i, ((t, iv), (c_floor, c_next, c_stamp))) in vs.iter().zip(&rows).enumerate() {
        let rs_floor = grid_floor_ns(*t, *iv);
        let rs_next = grid_next_boundary_ns(*t, *iv);
        if rs_floor != *c_floor || rs_next != *c_next {
            diffs.push(format!(
                "  vector {i}: t={t} interval={iv} -> C floor {c_floor} next {c_next}, \
                 Rust floor {rs_floor} next {rs_next}"
            ));
        }
        // Sender stamps only exist on a real canvas rate (the 30 ns vector is unit-test scale).
        let Some(fps) = integer_fps(*iv).filter(|_| *iv >= 1_000_000) else {
            continue;
        };
        let rs_stamp = per_second_floor(t / 100, fps, UNITS_100NS_PER_SECOND) * 100;
        if rs_stamp as i64 != *c_stamp {
            diffs.push(format!(
                "  vector {i}: t={t} fps={fps} -> C sender stamp {c_stamp}, Rust {rs_stamp}"
            ));
        }
        // The coincidence: a receiver grid point in [stamp, stamp + 100 ns).
        let s = *c_stamp as u64;
        let r = if s == 0 {
            grid_floor_ns(0, *iv)
        } else {
            grid_next_boundary_ns(s - 1, *iv)
        };
        if !(r >= s && r - s < 100) {
            coincide.push(format!(
                "  vector {i}: t={t} fps={fps}: sender stamp {s}, nearest receiver grid point \
                 at-or-after it {r} (off by {} ns)",
                r as i128 - s as i128
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "#1355: the vendored C per-second grid DIVERGED from the Tier-0 Rust authority on {} of \
         {} vectors:\n{}",
        diffs.len(),
        vs.len(),
        diffs.join("\n")
    );
    assert!(
        coincide.is_empty(),
        "#1355: the receiver grid does NOT coincide with the senders' per-second stamps on {} \
         vectors — the release deadline / render tick would walk against the stamps with the \
         date:\n{}",
        coincide.len(),
        coincide.join("\n")
    );
}

/// #1355 part 3 — the arrival-side stamp tracker. Stamp sequences (0 = the flush seam's timeline
/// reset) driven through the C `genlock_stamp_track_observe` (obs-genlock-grid.h, #included) and
/// the Rust `StampTrack` must end with byte-identical (last_ts, min_delta, dups, gaps).
fn stamp_sequences() -> Vec<Vec<u64>> {
    const NS: u64 = 1_000_000_000;
    const MS: u64 = 1_000_000;
    let t0 = SEC_1355 * NS;
    let s30 = |k: u64| t0 + (k * 10_000_000 / 30) * 100; // the real 30 fps 100 ns sender grid
    let s60 = |k: u64| t0 + (k * 10_000_000 / 60) * 100;
    let mut v: Vec<Vec<u64>> = vec![
        (0..300).map(s30).collect(), // clean 30 fps
        (0..300).map(s60).collect(), // clean 60 fps
        [0u64, 1, 2, 3, 5, 5, 6, 7]
            .iter()
            .map(|&k| s30(k))
            .collect(), // gap-then-duplicate
        [0u64, 1, 2, 5, 6, 6, 6, 7]
            .iter()
            .map(|&k| s60(k))
            .collect(), // multi-slot gap + dups
        vec![t0, t0 + 10 * MS, t0 + 25 * MS, t0 + 40 * MS], // 1.5-step boundary: 15 = 1.5 x 10 ms
        vec![t0, t0 + 10 * MS, t0 + 26 * MS], // just over 1.5 steps
        vec![t0, t0 + 33_333_300, t0 + 33_333_300 + NS], // exactly 1 s: discontinuity
        vec![t0, t0 + 33_333_300, t0 + 33_333_300 + NS - 1], // 1 s - 1: counted gap run
        vec![s30(10), s30(11), s30(3), s30(4), s30(4)], // backward step, then a dup
        vec![s30(0), s30(1), 0, s30(9), s30(9), s30(11)], // flush reset mid-stream
        vec![s30(0), s30(2), s30(3), s30(4)], // first positive delta is a gap
        vec![s30(0), s30(1), s30(1) + 1_000_000, s30(3), s30(4)], // a sub-step (1 ms) hiccup
    ];
    let mut x: u64 = 0x5eed_1355;
    for _ in 0..40 {
        let mut seq = Vec::new();
        let mut k = 0u64;
        for _ in 0..200 {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            k += match (x >> 33) % 100 {
                0..=2 => 0,                 // duplicate
                3..=5 => 2 + (x >> 40) % 3, // gap of 1..3
                6 => 40,                    // > 1 s jump at 30 fps
                _ => 1,
            };
            seq.push(if (x >> 20).is_multiple_of(97) {
                0
            } else {
                s30(k)
            });
        }
        v.push(seq);
    }
    v
}

#[test]
fn c_stamp_tracker_matches_the_rust_authority_1355() {
    use camera_box::genlock_grid::StampTrack;

    let header = repo(GENLOCK_GRID_H);
    let seqs = stamp_sequences();
    let mut c = String::new();
    c.push_str("#include <stdint.h>\n#include <stdio.h>\n");
    c.push_str(&format!("#include \"{}\"\n", header.display()));
    c.push_str("int main(void){\n");
    for seq in &seqs {
        c.push_str("    { uint64_t last = 0, min = 0, dups = 0, gaps = 0;\n");
        for ts in seq {
            if *ts == 0 {
                c.push_str("      last = 0; min = 0;\n");
            } else {
                c.push_str(&format!(
                    "      genlock_stamp_track_observe(&last, &min, &dups, &gaps, {ts}ULL);\n"
                ));
            }
        }
        c.push_str(
            "      printf(\"%llu %llu %llu %llu\\n\", (unsigned long long)last, \
             (unsigned long long)min, (unsigned long long)dups, (unsigned long long)gaps); }\n",
        );
    }
    c.push_str("    return 0;\n}\n");

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("genlock_parity_1355_stamp");
    fs::create_dir_all(&dir).expect("create the parity scratch dir");
    let cfile = dir.join("stamp.c");
    let bin = dir.join("stamp.bin");
    fs::write(&cfile, &c).expect("write the stamp harness");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=gnu99",
            "-Wall",
            "-Wextra",
            "-Wconversion",
            "-Werror",
            "-O1",
        ])
        .arg(&cfile)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "#1355: could not run the C compiler `{cc}` ({e}). This gate must FAIL rather \
                 than skip when the toolchain is absent. Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "#1355: genlock_stamp_track_observe ({GENLOCK_GRID_H}) does NOT COMPILE standalone \
         under -Wall -Wextra -Wconversion -Werror:\n--- cc stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin)
        .output()
        .expect("#1355: the compiled stamp harness failed to execute");
    let stdout = String::from_utf8(run.stdout).expect("harness stdout is utf-8");
    let rows: Vec<Vec<u64>> = stdout
        .lines()
        .map(|l| {
            l.split_whitespace()
                .map(|f| f.parse().expect("u64"))
                .collect()
        })
        .collect();
    assert_eq!(
        rows.len(),
        seqs.len(),
        "#1355: harness printed the wrong count"
    );

    let mut diffs = Vec::new();
    for (i, (seq, c_row)) in seqs.iter().zip(&rows).enumerate() {
        let mut t = StampTrack::default();
        for ts in seq {
            if *ts == 0 {
                t.reset_timeline();
            } else {
                t.observe(*ts);
            }
        }
        let rs_row = vec![t.last_ts, t.min_delta_ns, t.dups, t.gaps];
        if rs_row != *c_row {
            diffs.push(format!("  sequence {i}: C {c_row:?}, Rust {rs_row:?}"));
        }
    }
    assert!(
        diffs.is_empty(),
        "#1355: the vendored C stamp tracker DIVERGED from the Tier-0 Rust StampTrack on {} of {} \
         sequences:\n{}",
        diffs.len(),
        seqs.len(),
        diffs.join("\n")
    );
    // The hand-written sequences pin the rules themselves (not just C == Rust).
    let expect = |i: usize| (rows[i][2], rows[i][3]);
    assert_eq!(expect(0), (0, 0), "clean 30 fps");
    assert_eq!(expect(1), (0, 0), "clean 60 fps");
    assert_eq!(expect(2), (1, 1), "gap-then-duplicate");
    assert_eq!(expect(3), (2, 2), "multi-slot gap + dups");
    assert_eq!(expect(4), (0, 0), "exactly 1.5 steps is not a gap");
    assert_eq!(expect(5), (0, 1), "just over 1.5 steps is one gap");
    assert_eq!(expect(6), (0, 0), "a 1 s jump is a discontinuity");
    assert_eq!(
        (expect(11), rows[11][1]),
        ((0, 1), 33_333_300),
        "a 1 ms hiccup is ignored: it neither becomes the step nor turns later intervals into gaps"
    );
}

#[test]
fn stamp_tracker_rules_are_pinned_on_the_rust_side_1355() {
    // The same pins without a C compiler, so a Rust-only regression is named precisely.
    use camera_box::genlock_grid::StampTrack;
    let seqs = stamp_sequences();
    let run = |seq: &Vec<u64>| {
        let mut t = StampTrack::default();
        for &ts in seq {
            if ts == 0 {
                t.reset_timeline();
            } else {
                t.observe(ts);
            }
        }
        (t.dups, t.gaps)
    };
    assert_eq!(run(&seqs[2]), (1, 1));
    assert_eq!(run(&seqs[4]), (0, 0));
    assert_eq!(run(&seqs[5]), (0, 1));
    assert_eq!(run(&seqs[7]).0, 0);
    assert_eq!(
        run(&seqs[8]),
        (1, 0),
        "backward step not counted, the dup after it is"
    );
    // Reset: 9 is not compared with 1; 9->9 is a dup; the step is re-learned after the reset, so
    // the first positive interval (9->11) sets it and cannot itself be judged a gap.
    assert_eq!(run(&seqs[9]), (1, 0));
    assert_eq!(run(&seqs[11]), (0, 1), "sub-step hiccup ignored");
}
