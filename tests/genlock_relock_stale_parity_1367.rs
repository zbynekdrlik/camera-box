//! Issue 1367 — an EXECUTABLE C-vs-Rust parity gate for the stale-burst BACKLOG relock reset.
//!
//! After a network arrival burst the #1003 phase anchor carries the LATE phase; a backlog relock
//! that keeps it sheds only the frames aging past it, and a 60-into-30 source re-fires the branch
//! every tick for minutes (live 25.9.2026 20:50:58: cam6 relocked 5028 times, ~300 ms late).
//! ROZHODNUTÉ 5840479751: the anchor pick is compared with the pick at the depth the source is
//! SUPPOSED to hold — `base + 1` on a deep N==1 source, the latched D on a governed shallow one,
//! else the configured latency — and is stale when more than `n + 1` source frames behind it.
//!
//! Authority → C twin:
//! - `src/genlock_backlog.rs` `relock_anchor_is_stale` → `genlock_relock_anchor_is_stale`, and
//!   `relock_select_nearest(q, wall, relock_expected_age_ns(..))` → `genlock_relock_select_expected`
//!   (both in the contiguous #1003 relock-selection block, lifted by `lift_relock_helpers`);
//! - `src/genlock_n1_depth.rs` `n1_expected_depth_frames` → `genlock_n1_expected_depth_frames`
//!   (in the contiguous issue-1367 N==1 block, lifted by `lift_converge_helper`), built only from
//!   the N==1 governor's own predicates.
//!
//! Each block is compiled under `-Werror` and run over the logged bursts, the must-not-reset deep
//! `base + 1` / shallow D anchors on a late tick, the jitter cases and a deterministic spread; the
//! Rust authority must print identical output. It FAILS LOUDLY rather than skipping when no C
//! compiler is present.

use camera_box::genlock_backlog::{
    relock_anchor_age_ns, relock_anchor_is_stale, relock_expected_age_ns, relock_select_nearest,
};
use camera_box::genlock_n1_depth::n1_expected_depth_frames;

mod genlock_n1_lift;
use genlock_n1_lift::{
    compile_and_run_c, compile_and_run_n1_block, lift_relock_helpers as lift_helpers,
    RELOCK_PRELUDE as PRELUDE,
};

/// Issue 1367 — one BACKLOG-relock decision: `(depth, stamp grid, head age, anchor, latency_ms,
/// source multiple n, expected depth frames)`. The queue is `depth` frames one `grid` apart whose
/// oldest is `head age` old at [`W_1367`]; the expected depth is in canvas intervals ([`I30`]).
type StaleVector = (usize, u64, u64, u64, u32, u32, u64);

const W_1367: u64 = 1_800_000_000_000_000_000;
const I60: u64 = 16_666_667;
const I30: u64 = 33_333_333;
const MS: u64 = 1_000_000;

/// The logged bursts, the must-not-reset N==1 anchors on a late tick, the jitter cases either
/// side of the 60-into-30 boundary, an unset anchor, and a deterministic spread over n = 0..3.
fn stale_vectors() -> Vec<StaleVector> {
    // Newest frame of a 14-deep 60 fps queue whose head is 250 ms old: 250 - 13 * 16.67 ms.
    let newest = 250 * MS - 13 * I60;
    let mut v: Vec<StaleVector> = vec![
        (17, I60, 300 * MS, 283_636_965, 3, 2, 0), // [0] cam6 20:50:58 -> stale
        (15, I60, 266_666_672, 250_786_331, 3, 2, 0), // [1] cam7 20:50:58 -> stale
        (40, I30, 40 * I30 + 5 * MS, 31 * I30 + 2 * MS, 987, 1, 31), // [2] 2ME PGM base+1, 5 ms late
        (40, I30, 40 * I30 + 10 * MS, 31 * I30 + 2 * MS, 987, 1, 31), // [3] same, 10 ms late
        (8, I30, 8 * I30 + 10 * MS, 4 * I30 + 3 * MS, 3, 1, 4),      // [4] shallow D = base+3, late
        (40, I30, 40 * I30 + 5 * MS, 36 * I30, 987, 1, 31), // [5] deep, 5 frames past base+1
        (14, I60, 250 * MS, newest + 2 * I30 + MS, 3, 2, 0), // [6] 60-into-30, 4 frames -> stale
        (14, I60, 250 * MS, newest + I30 + 17 * MS, 3, 2, 0), // [7] 60-into-30, 3 frames: kept
        (14, I60, 250 * MS, 0, 3, 2, 0),                    // [8] anchor unset
        (17, I60, 300 * MS, 283_636_965, 3, 0, 0),          // [9] n unmeasured
        (40, I30, 40 * I30 + 5 * MS, 31 * I30 + 2 * MS, 987, 1, 0), // [10] same anchor, no depth
    ];
    let mut x: u64 = 0x1367_5EED_CAFE_F00D;
    for _ in 0..120 {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let depth = (x >> 33) as usize % 40 + 1;
        let grid = if x & 1 == 0 { I30 } else { I60 };
        let head = depth as u64 * grid + (x >> 20) % (80 * MS);
        let anchor = if (x >> 17) & 3 == 0 {
            0
        } else {
            (x >> 24) % (head + 100 * MS)
        };
        let latency = if (x >> 9) & 1 == 0 {
            3
        } else {
            ((x >> 40) % 1000) as u32
        };
        let n = ((x >> 50) % 4) as u32;
        let expected = if (x >> 13) & 1 == 0 {
            0
        } else {
            (x >> 54) % 40
        };
        v.push((depth, grid, head, anchor, latency, n, expected));
    }
    v
}

#[test]
fn c_backlog_stale_reset_matches_the_rust_authority_1367() {
    let vs = stale_vectors();
    let mut c = String::new();
    c.push_str(PRELUDE);
    c.push_str(&lift_helpers());
    c.push_str("int main(void){\n    struct obs_source_frame f[64];\n    struct obs_source_frame *pf[64];\n    obs_source_t s;\n");
    for (depth, grid, head, anchor, latency, n, expected) in &vs {
        c.push_str(&format!(
            "    {{ size_t d={depth}; for (size_t i=0;i<d;i++) {{ f[i].timestamp = \
             {W_1367}ULL - {head}ULL + (uint64_t)i*{grid}ULL; pf[i]=&f[i]; }}\n\
             \x20     s.async_frames.array=pf; s.async_frames.num=d; s.genlock_phase_anchor_ns={anchor}ULL;\n\
             \x20     size_t a = genlock_relock_select_nearest(&s, {W_1367}ULL, {latency});\n\
             \x20     size_t k = genlock_relock_select_expected(&s, {W_1367}ULL, {latency}, {expected}ULL, {I30}ULL);\n\
             \x20     printf(\"%zu %zu %d\\n\", a, k, genlock_relock_anchor_is_stale(a, k, {n}U) ? 1 : 0);\n    }}\n"
        ));
    }
    // Direct edges: the strict n + 1 boundary, n = 0, the reversed order, and a saturating age.
    let edges: [(usize, usize, u32); 9] = [
        (10, 13, 2),
        (10, 14, 2),
        (10, 12, 1),
        (10, 13, 1),
        (5, 7, 0),
        (5, 8, 0),
        (9, 4, 1),
        (7, 7, 2),
        (0, 64, 62),
    ];
    for (a, k, n) in edges {
        c.push_str(&format!(
            "    printf(\"%d\\n\", genlock_relock_anchor_is_stale({a}, {k}, {n}U) ? 1 : 0);\n"
        ));
    }
    c.push_str("    printf(\"%zu\\n\", genlock_relock_select_expected(&s, 5ULL, 3, 0xFFFFFFFFFFFFFFFFULL, 7ULL));\n");
    c.push_str("    return 0;\n}\n");

    let out = compile_and_run_c("genlock_stale_reset_parity_1367", &c);
    assert_eq!(
        out.len(),
        vs.len() + edges.len() + 1,
        "one line per vector, per edge and the saturation probe"
    );

    let mut diffs = Vec::new();
    let mut stale_seen = [false; 2];
    for (i, ((depth, grid, head, anchor, latency, n, expected), line)) in
        vs.iter().zip(&out).enumerate()
    {
        let q: Vec<u64> = (0..*depth as u64)
            .map(|j| W_1367 - head + j * grid)
            .collect();
        let a = relock_select_nearest(&q, W_1367, relock_anchor_age_ns(*anchor, *latency));
        let k = relock_select_nearest(&q, W_1367, relock_expected_age_ns(*expected, *latency, I30));
        let stale = relock_anchor_is_stale(a, k, *n);
        stale_seen[stale as usize] = true;
        let want = format!("{a} {k} {}", stale as u8);
        if *line != want {
            diffs.push(format!(
                "  vector {i} {:?}: C `{line}`, Rust `{want}`",
                vs[i]
            ));
        }
    }
    for (j, (a, k, n)) in edges.iter().enumerate() {
        let want = (relock_anchor_is_stale(*a, *k, *n) as u8).to_string();
        let line = &out[vs.len() + j];
        if *line != want {
            diffs.push(format!("  edge ({a},{k},{n}): C `{line}`, Rust `{want}`"));
        }
    }
    // The last queue left in `s` (the final spread vector): a saturating expected age targets the
    // oldest frame, index 0, never a wrapped-around young target.
    let sat = &out[vs.len() + edges.len()];
    if sat != "0" {
        diffs.push(format!(
            "  saturating expected age: C picked `{sat}`, want `0`"
        ));
    }
    assert!(
        diffs.is_empty(),
        "issue 1367: the vendored C stale-anchor reset DIVERGED from the Tier-0 Rust authority:\n{}",
        diffs.join("\n")
    );
    // The decided acceptance, on BOTH sides.
    assert_eq!(
        out[0], "1 16 1",
        "cam6 burst: anchor pick 1, expected 16, stale"
    );
    assert_eq!(
        out[1], "1 14 1",
        "cam7 burst: anchor pick 1, expected 14, stale"
    );
    assert_eq!(
        out[2], "9 9 0",
        "2ME PGM base + 1 anchor on a 5 ms late tick: kept"
    );
    assert_eq!(
        out[3], "9 9 0",
        "2ME PGM base + 1 anchor on a 10 ms late tick: kept"
    );
    assert_eq!(out[4], "4 4 0", "shallow latched D = base + 3 anchor: kept");
    assert_eq!(
        out[5], "4 9 1",
        "a deep anchor 5 frames past base + 1: stale"
    );
    assert!(out[6].ends_with(" 1") && out[7].ends_with(" 0"));
    assert_eq!(
        out[10], "9 11 0",
        "without the governor depth the same anchor reads 2 frames back (still within n + 1)"
    );
    assert!(
        stale_seen[0] && stale_seen[1],
        "the spread must exercise both verdicts"
    );
}

/// `(arrival floor, latency_ms, interval, latched shallow D)` for the N==1 expected depth.
fn expected_depth_vectors() -> Vec<(u64, u32, u64, u64)> {
    let mut v = vec![
        (I30 + 5 * MS, 987, I30, 0), // deep 2ME PGM -> base + 1 = 31
        (I30 + 5 * MS, 987, I30, 4), // deep wins over a stale latched D
        (I30 + 10 * MS, 3, I30, 4),  // shallow governed -> D
        (I30, 3, I30, 0),            // shallow, nothing latched -> 0
        (29 * I30, 987, I30, 0),     // floor too close to the pin: not deep -> 0
        (29 * I30, 987, I30, 31),    // ... -> its latched D
        (28 * I30, 987, I30, 0),     // exactly base - margin: deep -> 31
        (I30, 987, 0, 4),            // degenerate interval -> 0
        (0, 1000, I30, 0),           // whole-frame pin: base 30 (the 1 us tolerance) -> 31
        (u64::MAX, 987, I30, 7),     // absurd floor: not deep -> D
    ];
    let mut x: u64 = 0x0E17_D0E5_1367_0001;
    for _ in 0..200 {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let floor = (x >> 20) % 1_200_000_000;
        let latency = ((x >> 40) % 1200) as u32;
        let interval = if x & 1 == 0 { I30 } else { I60 };
        let d = if (x >> 7) & 1 == 0 { 0 } else { (x >> 50) % 8 };
        v.push((floor, latency, interval, d));
    }
    v
}

#[test]
fn c_n1_expected_depth_matches_the_rust_authority_1367() {
    let vs = expected_depth_vectors();
    let mut body = String::new();
    for (floor, latency, interval, d) in &vs {
        body.push_str(&format!(
            "    printf(\"%llu\\n\", (unsigned long long)genlock_n1_expected_depth_frames({floor}ULL, {latency}, {interval}ULL, {d}ULL));\n"
        ));
    }
    let out = compile_and_run_n1_block("genlock_n1_expected_depth_1367", &body);
    assert_eq!(out.len(), vs.len());
    let mut diffs = Vec::new();
    let mut seen = [false; 3];
    for (i, ((floor, latency, interval, d), line)) in vs.iter().zip(&out).enumerate() {
        let want = n1_expected_depth_frames(*floor, *latency, *interval, *d);
        seen[if want == 0 {
            0
        } else if want == *d {
            1
        } else {
            2
        }] = true;
        if *line != want.to_string() {
            diffs.push(format!(
                "  vector {i} {:?}: C `{line}`, Rust `{want}`",
                vs[i]
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "issue 1367: the C N==1 expected depth DIVERGED from the Rust governor authority:\n{}",
        diffs.join("\n")
    );
    let head: Vec<&str> = out.iter().take(10).map(String::as_str).collect();
    assert_eq!(
        head,
        ["31", "31", "4", "0", "0", "31", "31", "0", "31", "7"],
        "the governor depths: deep base + 1, shallow D, else none"
    );
    assert!(
        seen.iter().all(|s| *s),
        "the spread must hit none / D / base + 1"
    );
}

/// The BACKLOG STORM branch of `genlock_release_tick`, whitespace-squished: from the relock
/// counter to the branch's terminal `release = sel_1003 + 1;`.
fn backlog_branch() -> String {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("vendor/obs-studio/libobs/obs-source.c");
    let raw = std::fs::read_to_string(&path).expect("read obs-source.c");
    let start = raw
        .find("source->genlock_relocks++;")
        .expect("the BACKLOG STORM branch (genlock_relocks++) must be present");
    let end = start
        + raw[start..]
            .find("release = sel_1003 + 1;")
            .expect("the BACKLOG STORM branch must end in `release = sel_1003 + 1;`");
    raw[start..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn backlog_branch_resets_a_stale_burst_anchor_1367() {
    let b = backlog_branch();
    let pos = |needle: &str| {
        b.find(needle).unwrap_or_else(|| {
            panic!(
                "issue 1367: the BACKLOG STORM branch no longer contains `{needle}` — the \
                 stale-burst anchor reset is unwired; after an arrival burst the relock would \
                 again hold the late phase and fire every tick for minutes."
            )
        })
    };
    let n = pos("const uint32_t n_for_log =");
    let depth = pos(
        "const uint64_t expected_frames_1367 = (n_for_log < 2 && source->genlock_last_known_n < 2) \
         ? genlock_n1_expected_depth_frames(",
    );
    let latched = pos("source->genlock_shallow_target_frames) : 0;");
    let expected = pos(
        "const size_t sel_exp_1367 = genlock_relock_select_expected(source, wall_now, reserve_ms, \
         expected_frames_1367, interval);",
    );
    let stale = pos(
        "const bool stale_1367 = genlock_relock_anchor_is_stale(sel_1003, sel_exp_1367, n_for_log);",
    );
    let guard = pos("if ((sel_1003 == 0 || stale_1367) && source->genlock_phase_anchor_ns != 0) {");
    let reset = pos("source->genlock_phase_anchor_ns = 0; sel_1003 = genlock_relock_select_nearest(source, wall_now, reserve_ms); stale_reset_1367 = true;");
    let log = pos("stale_reset=%d");
    assert!(
        n < depth
            && depth < latched
            && latched < expected
            && expected < stale
            && stale < guard
            && guard < reset
            && reset < log,
        "issue 1367: the stale-anchor reset must run in order — measure n, read the N==1 \
         governor's expected depth, pick it, decide stale, reset the anchor + re-select, THEN log"
    );
    assert!(
        b.contains("expected_frames=%llu") && b.contains("(unsigned long long)expected_frames_1367"),
        "issue 1367: the genlock-relock line must print the expected depth the anchor was judged by"
    );
    assert!(
        b.contains("stale_reset_1367 ? 1 : 0"),
        "issue 1367: the genlock-relock line must print whether this relock dropped the anchor"
    );
}
