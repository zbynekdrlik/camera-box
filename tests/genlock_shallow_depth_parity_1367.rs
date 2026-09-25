//! issue 1367 (ROZHODNUTÉ 5827497952) — the EXECUTABLE C-vs-Rust parity gate for the SHALLOW
//! N==1 per-lock depth (split out of `tests/genlock_relock_selection_parity.rs`, review round 2: the
//! 1000-line test budget). The lifted `genlock_n1_shallow_*` helpers (inside the contiguous N==1
//! block, compiled by the shared `genlock_n1_lift` module) must match
//! `camera_box::genlock_n1_depth` byte for byte. `cc` is required — per the project's
//! test-strictness rule this FAILS LOUDLY rather than skipping when the toolchain is missing.

use camera_box::genlock_n1_depth::{
    n1_shallow_gap_is_relock, n1_shallow_governs, n1_shallow_hist_bin, n1_shallow_hold_due,
    n1_shallow_percentile_bin, n1_shallow_shed_due, n1_shallow_target_frames, n1_shallow_track,
    n1_shallow_window_deep, ShallowDepth, ShallowTick, N1_SHALLOW_HIST_BINS,
};

mod genlock_n1_lift;
use genlock_n1_lift::compile_and_run_n1_block;

/// issue 1367 (ROZHODNUTÉ 5827497952) — the SHALLOW per-lock depth: the lifted
/// `genlock_n1_shallow_*` helpers (inside the same contiguous N==1 block) must match
/// [`camera_box::genlock_n1_depth`] on the latch target (deep, imag cap = report-only 0), the relock
/// gap, the governs guard, the shed and hold halves at every edge, and a tick-by-tick
/// `genlock_n1_shallow_track` sequence (a latch, a constant D, a relock that finds a deeper floor,
/// a floor that rises over D and re-measures, an N>=2 reset and the N==1 auto-window after it,
/// off-grid ticks, a deep latch, a deep window whose stall runs on the latch tick (the majority
/// decides), a relock in the middle of an open window, the min-latency report and a relock after
/// it). Design 5830750134 adds the floor histogram + its percentile, the base + 3 clamp, and the
/// sequence cases for a short burst (p90 ignores it), a transient window (rejected, re-measured),
/// the bounded rejects, a whole-window transient (clamped, then re-measured once the floor falls),
/// an unreachable D (the realized depth stays under it) and a backlog relock storm.
#[test]
fn c_n1_shallow_depth_matches_the_rust_authority_1367() {
    let i30 = 33_333_333u64;
    let w = 1_000_000_000_000u64;
    // (base, floor_max, deep, min_latency)
    let targets: [(u64, u64, bool, bool); 15] = [
        (1, 1, false, false),
        (1, 2, false, false),
        (1, 0, false, false),
        (30, 1, false, false),
        (30, 33, true, false),
        (1, 2, false, true),
        (1, 1, false, true),
        (1, 5, true, true),
        (u64::MAX, 0, false, false),
        (u64::MAX, 0, true, false),
        (3, u64::MAX, false, true),
        (1, 3, false, false),
        (1, 4, false, false),
        (1, 11, false, false),
        (1, 11, false, true),
    ];
    let bins: [(u64, u64); 7] = [
        (0, 1),
        (1, 1),
        (3, 1),
        (4, 1),
        (11, 1),
        (u64::MAX, 0),
        (5, 30),
    ];
    let hists: [([u32; N1_SHALLOW_HIST_BINS], u32, u32); 7] = [
        ([81, 0, 0, 9], 90, 90),
        ([80, 0, 0, 10], 90, 90),
        ([80, 0, 0, 10], 90, 10),
        ([0, 0, 0, 90], 90, 10),
        ([0, 0, 0, 0], 0, 90),
        ([10, 20, 30, 30], 90, 50),
        ([u32::MAX, 0, 0, 0], u32::MAX, 100),
    ];
    let gaps: [u64; 5] = [i30, 999_999_999, 1_000_000_000, 3_000_000_000, 0];
    let majorities: [(u32, u32); 7] = [
        (45, 90),
        (46, 90),
        (0, 0),
        (1, 1),
        (0, 1),
        (u32::MAX, u32::MAX),
        (u32::MAX / 2 + 1, u32::MAX),
    ];
    let governs: [(u64, u64, u32, u64); 6] = [
        (2, i30, 3, i30),
        (0, i30, 3, i30),
        (2, i30, 3, 0),
        (31, i30, 987, i30),
        (31, 29 * i30, 987, i30),
        (3, 2 * i30, 100, i30),
    ];
    let mut sheds = Vec::new();
    let mut holds = Vec::new();
    for step in 0..120u64 {
        let age = i30 / 2 + step * (5 * i30 / 120);
        for (target, latency, floor) in
            [(3u64, 3u32, i30), (2, 3, i30), (0, 3, i30), (31, 987, i30)]
        {
            for ticks in [29u64, 30] {
                sheds.push((w, w - age, floor, latency, i30, target, ticks));
                holds.push((w, w - age, floor, latency, i30, target, ticks));
            }
        }
    }
    // an unlocked boundary never sheds
    sheds.push((w, 0, i30, 3, i30, 3, 100));
    // One present tick: (n1, relock, on_grid, floor_frames, base, deep, min_latency, realized,
    // backlog_relock). The realized depth sits over any D unless a block says otherwise.
    let mut seq: Vec<ShallowTick> = Vec::new();
    let t = |n1: bool, relock: bool, on_grid: bool, floor: u64| ShallowTick {
        n1,
        relock,
        on_grid,
        floor_frames: floor,
        base_frames: 1,
        deep: false,
        min_latency_box: false,
        realized_frames: u64::MAX,
        backlog_relock: false,
    };
    for k in 0..110u64 {
        seq.push(t(true, k == 0, k % 11 != 5, 1 + u64::from(k % 7 == 3)));
    }
    for k in 0..95u64 {
        seq.push(t(true, k == 0, true, 2));
    }
    // the floor rises to D (3) and over: a whole window re-measures without a relock.
    for k in 0..200u64 {
        seq.push(t(true, false, k % 13 != 4, 3 + u64::from(k % 5 == 0)));
    }
    // an N>=2 tick clears; back on N==1 without a relock a window opens by itself.
    seq.push(t(false, false, true, 1));
    for _ in 0..95u64 {
        seq.push(t(true, false, true, 1));
    }
    // a deep source latches base + 1 whatever its floor did.
    for k in 0..92u64 {
        seq.push(ShallowTick {
            base_frames: 30,
            deep: true,
            ..t(true, k == 0, true, if k < 10 { 33 } else { 1 })
        });
    }
    // a deep window whose stall still runs on the latch tick (not deep there): the majority wins.
    for k in 0..90u64 {
        seq.push(ShallowTick {
            base_frames: 30,
            deep: k != 89,
            ..t(true, k == 0, true, if k == 89 { 33 } else { 1 })
        });
    }
    // a relock in the MIDDLE of an open window restarts it (the first 40 ticks' floor 4 is gone).
    for k in 0..40u64 {
        seq.push(t(true, k == 0, true, 4));
    }
    for k in 0..90u64 {
        seq.push(t(true, k == 0, true, 1));
    }
    // the imag report-only cap, then no auto re-measure churn while capped.
    for k in 0..200u64 {
        seq.push(ShallowTick {
            min_latency_box: true,
            ..t(true, k == 0, true, 3)
        });
    }
    // a relock after the capped report re-measures; a floor at base latches base + 1 there.
    for k in 0..90u64 {
        seq.push(ShallowTick {
            min_latency_box: true,
            ..t(true, k == 0, true, 1)
        });
    }
    // design 5830750134: a short burst (8 of 90 ticks at 11) -- the p90 latch ignores it.
    for k in 0..90u64 {
        seq.push(t(true, k == 0, true, if k < 8 { 11 } else { 1 }));
    }
    // a transient in progress (30 of 90) is rejected; the clean window after it latches.
    for k in 0..90u64 {
        seq.push(t(true, k == 0, true, if k < 30 { 11 } else { 1 }));
    }
    for _ in 0..90u64 {
        seq.push(t(true, false, true, 1));
    }
    // a feed that never settles: the rejects are bounded and the clamp latches.
    for w in 0..4u64 {
        for k in 0..90u64 {
            seq.push(t(true, w == 0 && k == 0, true, if k < 30 { 11 } else { 1 }));
        }
    }
    // a whole-window transient latches the clamp; a floor still at it never re-measures; a whole
    // window two frames under it does.
    for k in 0..90u64 {
        seq.push(t(true, k == 0, true, 11));
    }
    for k in 0..120u64 {
        seq.push(t(true, false, k % 9 != 2, 3 + u64::from(k % 4 == 0) * 8));
    }
    for _ in 0..185u64 {
        seq.push(t(true, false, true, 2));
    }
    // an unreachable D: the realized depth stays under it, touching it once, then for good.
    for k in 0..400u64 {
        seq.push(ShallowTick {
            realized_frames: if k == 100 { 3 } else { 1 },
            ..t(true, false, k % 17 != 3, 2)
        });
    }
    // relocks: sporadic ones (181 ticks apart) never re-measure, a storm (every 40) does.
    for k in 0..600u64 {
        seq.push(ShallowTick {
            backlog_relock: k % 181 == 0,
            ..t(true, false, true, 2)
        });
    }
    for k in 0..220u64 {
        seq.push(ShallowTick {
            backlog_relock: k % 40 == 0,
            ..t(true, false, k % 13 != 6, 2)
        });
    }

    let b = |v: bool| i32::from(v);
    let mut body = String::new();
    for (base, f, d, m) in &targets {
        body.push_str(&format!(
            "    {{ bool cap = false; unsigned long long d = (unsigned long long)genlock_n1_shallow_target_frames({base}ULL, {f}ULL, {}, {}, &cap); printf(\"%llu %d\\n\", d, cap ? 1 : 0); }}\n",
            b(*d),
            b(*m)
        ));
    }
    for (f, base) in &bins {
        body.push_str(&format!(
            "    printf(\"%u\\n\", (unsigned)genlock_n1_shallow_hist_bin({f}ULL, {base}ULL));\n"
        ));
    }
    for (h, w, pct) in &hists {
        body.push_str(&format!(
            "    {{ const uint32_t h[4] = {{{}u, {}u, {}u, {}u}}; printf(\"%llu\\n\", (unsigned long long)genlock_n1_shallow_percentile_bin(h, {w}u, {pct}u)); }}\n",
            h[0], h[1], h[2], h[3]
        ));
    }
    for g in &gaps {
        body.push_str(&format!(
            "    printf(\"%d\\n\", genlock_n1_shallow_gap_is_relock({g}ULL) ? 1 : 0);\n"
        ));
    }
    for (d, w) in &majorities {
        body.push_str(&format!(
            "    printf(\"%d\\n\", genlock_n1_shallow_window_deep({d}u, {w}u) ? 1 : 0);\n"
        ));
    }
    for (tg, f, l, iv) in &governs {
        body.push_str(&format!(
            "    printf(\"%d\\n\", genlock_n1_shallow_governs({tg}ULL, {f}ULL, {l}u, {iv}ULL) ? 1 : 0);\n"
        ));
    }
    for (tw, bd, f, l, iv, tg, k) in &sheds {
        body.push_str(&format!(
            "    printf(\"%d\\n\", genlock_n1_shallow_shed_due({tw}ULL, {bd}ULL, {f}ULL, {l}u, {iv}ULL, {tg}ULL, {k}ULL) ? 1 : 0);\n"
        ));
    }
    for (tw, h, f, l, iv, tg, k) in &holds {
        body.push_str(&format!(
            "    printf(\"%d\\n\", genlock_n1_shallow_hold_due({tw}ULL, {h}ULL, {f}ULL, {l}u, {iv}ULL, {tg}ULL, {k}ULL) ? 1 : 0);\n"
        ));
    }
    // The tick sequence goes in as C arrays driven by one loop (a statement per tick made the
    // harness take minutes to compile at -O1 once the sequence passed 3000 ticks).
    let col = |f: &dyn Fn(&ShallowTick) -> String| seq.iter().map(f).collect::<Vec<_>>().join(",");
    body.push_str(&format!(
        "    {{ static const unsigned char F[] = {{{}}};\n      static const unsigned long long FL[] = {{{}}};\n      static const unsigned long long BA[] = {{{}}};\n      static const unsigned long long RE[] = {{{}}};\n",
        col(&|x| (b(x.n1) | (b(x.relock) << 1) | (b(x.on_grid) << 2) | (b(x.deep) << 3) | (b(x.min_latency_box) << 4) | (b(x.backlog_relock) << 5)).to_string()),
        col(&|x| format!("{}ULL", x.floor_frames)),
        col(&|x| format!("{}ULL", x.base_frames)),
        col(&|x| format!("{}ULL", x.realized_frames)),
    ));
    body.push_str(&format!(
        "      uint64_t tf = 0, fm = 0; uint32_t wt = 0, ov = 0, dt = 0, un = 0, ch = 0, qu = 0, rj = 0; uint32_t hi[4] = {{0u, 0u, 0u, 0u}}; bool me = false, ca = false;\n      for (int k = 0; k < {}; k++) {{\n        const unsigned f = F[k];\n        bool l = genlock_n1_shallow_track(&tf, &fm, &wt, &ov, &dt, &me, &ca, (f & 1u) != 0, (f & 2u) != 0, (f & 4u) != 0, FL[k], BA[k], (f & 8u) != 0, (f & 16u) != 0, RE[k], (f & 32u) != 0, hi, &un, &ch, &qu, &rj);\n        printf(\"%d %llu %llu %u %u %u %d %d %u %u %u %u %u %u %u %u\\n\", l ? 1 : 0, (unsigned long long)tf, (unsigned long long)fm, (unsigned)wt, (unsigned)ov, (unsigned)dt, me ? 1 : 0, ca ? 1 : 0, (unsigned)un, (unsigned)ch, (unsigned)qu, (unsigned)rj, (unsigned)hi[0], (unsigned)hi[1], (unsigned)hi[2], (unsigned)hi[3]);\n      }}\n    }}\n",
        seq.len()
    ));
    let c_out = compile_and_run_n1_block("genlock_n1_shallow_parity_1367", &body);

    let mut want: Vec<String> = Vec::new();
    for (base, f, d, m) in &targets {
        let (dd, cap) = n1_shallow_target_frames(*base, *f, *d, *m);
        want.push(format!("{dd} {}", b(cap)));
    }
    for (f, base) in &bins {
        want.push(n1_shallow_hist_bin(*f, *base).to_string());
    }
    for (h, w, pct) in &hists {
        want.push(n1_shallow_percentile_bin(h, *w, *pct).to_string());
    }
    for g in &gaps {
        want.push(b(n1_shallow_gap_is_relock(*g)).to_string());
    }
    for (d, w) in &majorities {
        want.push(b(n1_shallow_window_deep(*d, *w)).to_string());
    }
    for (tg, f, l, iv) in &governs {
        want.push(b(n1_shallow_governs(*tg, *f, *l, *iv)).to_string());
    }
    let mut fired = (0usize, 0usize);
    for (tw, bd, f, l, iv, tg, k) in &sheds {
        let r = n1_shallow_shed_due(*tw, *bd, *f, *l, *iv, *tg, *k);
        fired.0 += usize::from(r);
        want.push(b(r).to_string());
    }
    for (tw, h, f, l, iv, tg, k) in &holds {
        let r = n1_shallow_hold_due(*tw, *h, *f, *l, *iv, *tg, *k);
        fired.1 += usize::from(r);
        want.push(b(r).to_string());
    }
    let mut s = ShallowDepth::default();
    let mut latched = Vec::new();
    for x in &seq {
        let l = n1_shallow_track(&mut s, *x);
        if l {
            latched.push(s.target_frames);
        }
        want.push(format!(
            "{} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {}",
            b(l),
            s.target_frames,
            s.floor_max_frames,
            s.window_ticks,
            s.over_ticks,
            s.deep_ticks,
            b(s.measuring),
            b(s.capped),
            s.under_ticks,
            s.churn_relocks,
            s.churn_quiet_ticks,
            s.rejects,
            s.hist[0],
            s.hist[1],
            s.hist[2],
            s.hist[3]
        ));
    }
    let diffs: Vec<String> = c_out
        .iter()
        .zip(&want)
        .enumerate()
        .filter(|(_, (c, r))| c != r)
        .map(|(k, (c, r))| format!("  line {k}: C `{c}` vs Rust `{r}`"))
        .take(12)
        .collect();
    assert_eq!(
        c_out.len(),
        want.len(),
        "issue 1367: harness printed the wrong count"
    );
    assert!(
        diffs.is_empty(),
        "issue 1367: the vendored C shallow-depth helpers DIVERGED from the Tier-0 Rust authority:\n{}",
        diffs.join("\n")
    );
    // both outcomes of every decision, and every latch kind the sequence scripts: the first D, the
    // relock's deeper D, the re-measure after the rise (design 5830750134: its p90 floor asks for
    // base + 4, clamped to 4), the auto-window after N>=2, the deep base + 1 (twice: the second with a
    // not-deep latch tick), the mid-window relock's fresh D, the capped report (0) and the relatch
    // after it; then the short burst the p90 ignores (2), the rejected transient's clean re-latch (2),
    // the bounded rejects' clamp (4), the whole-window transient's clamp (4) and its re-measure once
    // the floor fell (3), the unreachable D's re-measure (3) and the relock storm's (3).
    assert!(
        fired.0 > 0 && fired.0 < sheds.len(),
        "shed vectors one-sided: {fired:?}"
    );
    assert!(
        fired.1 > 0 && fired.1 < holds.len(),
        "hold vectors one-sided: {fired:?}"
    );
    assert_eq!(
        latched,
        vec![3, 3, 4, 2, 31, 31, 2, 0, 2, 2, 2, 4, 4, 3, 3, 3],
        "the track sequence latched {latched:?}"
    );
}
