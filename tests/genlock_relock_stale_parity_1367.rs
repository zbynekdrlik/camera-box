//! Issue 1367 — an EXECUTABLE C-vs-Rust parity gate for the stale-burst BACKLOG relock reset.
//!
//! After a network arrival burst the #1003 phase anchor carries the LATE phase; a backlog relock
//! that keeps it sheds only the frames aging past it, and a 60-into-30 source re-fires the branch
//! every tick for minutes (live 25.9.2026 20:50:58: cam6 relocked 5028 times, ~300 ms late).
//! `src/genlock_backlog.rs` `relock_anchor_is_stale` is the Tier-0 authority for the reset
//! decision; `vendor/obs-studio/libobs/obs-source.c` carries its byte-identical twin
//! `genlock_relock_anchor_is_stale` beside `genlock_relock_select_nearest`, plus the
//! configured-latency pick `genlock_relock_select_configured`.
//!
//! The gate lifts the whole contiguous #1003 relock-selection block VERBATIM (the same lift the
//! #1003 gate in `tests/genlock_relock_selection_parity.rs` uses), compiles it under `-Werror`,
//! runs BOTH picks and the stale verdict over the logged bursts, the jitter cases either side of
//! the one-canvas-tick boundary and a deterministic spread, and requires identical output from
//! the Rust authority. It FAILS LOUDLY rather than skipping when no C compiler is present.

use camera_box::genlock_backlog::{
    relock_anchor_age_ns, relock_anchor_is_stale, relock_select_nearest,
};

mod genlock_n1_lift;
use genlock_n1_lift::{
    compile_and_run_c, lift_relock_helpers as lift_helpers, RELOCK_PRELUDE as PRELUDE,
};

/// Issue 1367 — one BACKLOG-relock decision: `(depth, stamp grid, head age, anchor, latency_ms,
/// source multiple n)`. The queue is `depth` frames one `grid` apart whose oldest is `head age`
/// old at [`W_1367`].
type StaleVector = (usize, u64, u64, u64, u32, u32);

const W_1367: u64 = 1_800_000_000_000_000_000;
const I60_1367: u64 = 16_666_667;

/// The logged bursts, the jitter cases either side of the one-canvas-tick boundary, a deep N==1
/// hold, an unset anchor, and a deterministic spread over n = 0..3.
fn stale_vectors() -> Vec<StaleVector> {
    // Newest frame of a 14-deep 60 fps queue whose head is 250 ms old: 250 - 13 * 16.67 ms.
    let newest = 250_000_000 - 13 * I60_1367;
    let mut v: Vec<StaleVector> = vec![
        (17, I60_1367, 300_000_000, 283_636_965, 3, 2), // cam6 20:50:58 -> stale
        (15, I60_1367, 266_666_672, 250_786_331, 3, 2), // cam7 20:50:58 -> stale
        (
            14,
            I60_1367,
            250_000_000,
            newest + I60_1367 + 1_000_000,
            3,
            2,
        ), // 1-frame jitter
        (
            14,
            I60_1367,
            250_000_000,
            newest + 33_333_333 + 1_000_000,
            3,
            2,
        ), // one canvas tick
        (
            14,
            I60_1367,
            250_000_000,
            newest + 50_000_000 + 1_000_000,
            3,
            2,
        ), // past it -> stale
        (14, I60_1367, 250_000_000, 0, 3, 2),           // anchor unset
        (34, 33_333_300, 1_100_000_000, 1_020_000_000, 987, 1), // deep 2ME PGM hold
        (34, 33_333_300, 1_100_000_000, 1_090_000_000, 987, 1), // deep, 3 frames back
        (17, I60_1367, 300_000_000, 283_636_965, 3, 0), // n unmeasured
    ];
    let mut x: u64 = 0x1367_5EED_CAFE_F00D;
    for _ in 0..120 {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let depth = (x >> 33) as usize % 40 + 1;
        let grid = if x & 1 == 0 { 33_333_300 } else { I60_1367 };
        let head = depth as u64 * grid + (x >> 20) % 80_000_000;
        let anchor = if (x >> 17) & 3 == 0 {
            0
        } else {
            (x >> 24) % (head + 100_000_000)
        };
        let latency = if (x >> 9) & 1 == 0 {
            3
        } else {
            ((x >> 40) % 1000) as u32
        };
        let n = ((x >> 50) % 4) as u32;
        v.push((depth, grid, head, anchor, latency, n));
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
    for (depth, grid, head, anchor, latency, n) in &vs {
        c.push_str(&format!(
            "    {{ size_t d={depth}; for (size_t i=0;i<d;i++) {{ f[i].timestamp = \
             {W_1367}ULL - {head}ULL + (uint64_t)i*{grid}ULL; pf[i]=&f[i]; }}\n\
             \x20     s.async_frames.array=pf; s.async_frames.num=d; s.genlock_phase_anchor_ns={anchor}ULL;\n\
             \x20     size_t a = genlock_relock_select_nearest(&s, {W_1367}ULL, {latency});\n\
             \x20     size_t k = genlock_relock_select_configured(&s, {W_1367}ULL, {latency});\n\
             \x20     printf(\"%zu %zu %d\\n\", a, k, genlock_relock_anchor_is_stale(a, k, {n}U) ? 1 : 0);\n    }}\n"
        ));
    }
    // Direct edges: the strict boundary, n = 0, and the reversed order.
    let edges: [(usize, usize, u32); 8] = [
        (10, 12, 2),
        (10, 13, 2),
        (10, 11, 1),
        (10, 12, 1),
        (5, 6, 0),
        (5, 7, 0),
        (9, 4, 1),
        (7, 7, 2),
    ];
    for (a, k, n) in edges {
        c.push_str(&format!(
            "    printf(\"%d\\n\", genlock_relock_anchor_is_stale({a}, {k}, {n}U) ? 1 : 0);\n"
        ));
    }
    c.push_str("    return 0;\n}\n");

    let out = compile_and_run_c("genlock_stale_reset_parity_1367", &c);
    assert_eq!(
        out.len(),
        vs.len() + edges.len(),
        "one line per vector and per edge"
    );

    let mut diffs = Vec::new();
    let mut stale_seen = [false; 2];
    for (i, ((depth, grid, head, anchor, latency, n), line)) in vs.iter().zip(&out).enumerate() {
        let q: Vec<u64> = (0..*depth as u64)
            .map(|j| W_1367 - head + j * grid)
            .collect();
        let a = relock_select_nearest(&q, W_1367, relock_anchor_age_ns(*anchor, *latency));
        let k = relock_select_nearest(&q, W_1367, relock_anchor_age_ns(0, *latency));
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
    assert!(
        diffs.is_empty(),
        "issue 1367: the vendored C stale-anchor reset DIVERGED from the Tier-0 Rust authority:\n{}",
        diffs.join("\n")
    );
    // The logged bursts must read stale and the one-tick jitter must not — on BOTH sides.
    assert_eq!(
        out[0], "1 16 1",
        "cam6 20:50:58 burst: anchor pick 1, configured 16, stale"
    );
    assert_eq!(
        out[1], "1 14 1",
        "cam7 20:50:58 burst: anchor pick 1, configured 14, stale"
    );
    assert!(
        out[3].ends_with(" 0"),
        "a one-canvas-tick anchor gap is jitter, not stale"
    );
    assert!(
        stale_seen[0] && stale_seen[1],
        "the spread must exercise both verdicts"
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
    let configured =
        pos("const size_t sel_cfg_1367 = genlock_relock_select_configured(source, wall_now, reserve_ms);");
    let stale =
        pos("const bool stale_1367 = genlock_relock_anchor_is_stale(sel_1003, sel_cfg_1367, n_for_log);");
    let guard = pos("if ((sel_1003 == 0 || stale_1367) && source->genlock_phase_anchor_ns != 0) {");
    let reset = pos("source->genlock_phase_anchor_ns = 0; sel_1003 = genlock_relock_select_nearest(source, wall_now, reserve_ms); stale_reset_1367 = true;");
    let log = pos("stale_reset=%d");
    assert!(
        n < stale && configured < stale && stale < guard && guard < reset && reset < log,
        "issue 1367: the stale-anchor reset must run in order — measure n, pick the configured \
         phase, decide stale, reset the anchor + re-select, THEN log the relock line"
    );
    assert!(
        b.contains("stale_reset_1367 ? 1 : 0"),
        "issue 1367: the genlock-relock line must print whether this relock dropped the anchor"
    );
}
