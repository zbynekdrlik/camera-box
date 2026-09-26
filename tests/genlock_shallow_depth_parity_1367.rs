//! issue 1367 (ROZHODNUTÉ 5827497952) — the EXECUTABLE C-vs-Rust parity gate for the SHALLOW
//! N==1 per-lock depth (split out of `tests/genlock_relock_selection_parity.rs`, review round 2: the
//! 1000-line test budget). The lifted `genlock_n1_shallow_*` helpers (inside the contiguous N==1
//! block, compiled by the shared `genlock_n1_lift` module) must match
//! `camera_box::genlock_n1_depth` byte for byte. `cc` is required — per the project's
//! test-strictness rule this FAILS LOUDLY rather than skipping when the toolchain is missing.

use camera_box::genlock_n1_depth::{
    n1_shallow_gap_hold_due, n1_shallow_gap_is_relock, n1_shallow_governs, n1_shallow_hist_bin,
    n1_shallow_hold_due, n1_shallow_latch_floor_frames, n1_shallow_percentile_bin,
    n1_shallow_shed_due, n1_shallow_target_frames, n1_shallow_track, n1_shallow_window_deep,
    ShallowDepth, ShallowTick, N1_SHALLOW_HIST_BINS,
};

mod genlock_n1_lift;
use genlock_n1_lift::compile_and_run_n1_block;

/// design 5833339163 — one GAP-hold vector: (tick_wall, head, next, boundary, floor, latency,
/// interval, target).
type GapHoldVector = (u64, u64, u64, u64, u64, u32, u64, u64);

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
/// an unreachable D (the realized depth stays under it) and a backlog relock storm. Design 5833339163
/// (the song change) adds the GAP hold at every edge: the head's age around D, a short gap vs a
/// sender-restart gap, a duplicated head, an unlocked boundary, no latched D and a deep source.
/// ROZHODNUTÉ 5842640404 adds the budgeted LATCH floor of the receive-time arrival lag (its edges,
/// both intervals, a degenerate interval, the saturation) and a sequence whose latch floor differs
/// from the raw tick floor: the histogram latches the budgeted one, the rise watch keeps the raw.
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
    // ROZHODNUTÉ 5842640404: (receive-time arrival lag, interval).
    let mut latch_floors: Vec<(u64, u64)> = Vec::new();
    for lag in [
        0u64,
        8_000_000,
        i30 - 15_000_000,
        i30 - 15_000_000 + 1,
        25_000_000,
        36_000_000,
        60_000_000,
        2 * i30 - 15_000_000,
        u64::MAX - 15_000_000,
        u64::MAX,
    ] {
        for iv in [i30, 16_666_667u64, 0, 1] {
            latch_floors.push((lag, iv));
        }
    }
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
        // design 5830750134: a clamp under a slow arrival (the newest frame already 4.5 / 5 / 6
        // frames old at D 4) -- the shed guard's both sides.
        for (target, latency, floor) in [
            (3u64, 3u32, i30),
            (2, 3, i30),
            (0, 3, i30),
            (31, 987, i30),
            (4, 3, 4 * i30 + i30 / 2),
            (4, 3, 5 * i30),
            (4, 3, 6 * i30),
        ] {
            for ticks in [29u64, 30] {
                sheds.push((w, w - age, floor, latency, i30, target, ticks));
                holds.push((w, w - age, floor, latency, i30, target, ticks));
            }
        }
    }
    // an unlocked boundary never sheds
    sheds.push((w, 0, i30, 3, i30, 3, 100));
    // design 5833339163: (tick_wall, head, next, boundary, floor, latency, interval, target).
    let mut gap_holds: Vec<GapHoldVector> = Vec::new();
    for step in 0..60u64 {
        let head = w - (i30 / 4 + step * (4 * i30 / 60));
        for (boundary, next, latency, target, floor) in [
            (head - i30, 0u64, 3u32, 3u64, i30),
            (head - i30, head + i30, 3, 3, i30),
            (head - i30, head, 3, 3, i30),
            (head - i30, head - 1, 3, 3, i30),
            (head - i30, 0, 3, 2, i30),
            (head - i30, 0, 3, 4, i30),
            (head - i30, 0, 3, 0, i30),
            (0, 0, 3, 3, i30),
            (head - 999_999_999, 0, 3, 3, i30),
            (head - 1_000_000_000, 0, 3, 3, i30),
            (head - i30, 0, 987, 31, i30),
            (head - i30, 0, 987, 31, 29 * i30),
        ] {
            gap_holds.push((w, head, next, boundary, floor, latency, i30, target));
        }
    }
    // a degenerate interval never holds
    gap_holds.push((w, w - i30, 0, w - 2 * i30, i30, 3, 0, 3));
    // an unlocked boundary never holds, even where its "gap" to a head near the epoch is short
    // (so the relock check alone would not catch it).
    gap_holds.push((3 * i30, i30, 0, 0, i30, 3, i30, 3));
    // One present tick: (n1, relock, on_grid, floor_frames, base, deep, min_latency, realized,
    // backlog_relock). The realized depth sits over any D unless a block says otherwise.
    let mut seq: Vec<ShallowTick> = Vec::new();
    let t = |n1: bool, relock: bool, on_grid: bool, floor: u64| ShallowTick {
        n1,
        relock,
        on_grid,
        floor_frames: floor,
        latch_floor_frames: floor,
        base_frames: 1,
        deep: false,
        min_latency_box: false,
        realized_frames: u64::MAX,
        backlog_relock: false,
        sticky_floor_frames: 0,
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
    // ROZHODNUTÉ 5842640404: a relock on an idle lag whose budgeted latch floor (2) sits a frame
    // over the raw tick floor (1) latches D 3; a content rise of the raw floor to 2 (latch floor 3)
    // never re-measures; a raw floor at D for a whole window does, onto the clamp.
    for k in 0..90u64 {
        seq.push(ShallowTick {
            latch_floor_frames: 2,
            ..t(true, k == 0, true, 1)
        });
    }
    for k in 0..270u64 {
        seq.push(ShallowTick {
            latch_floor_frames: 3,
            ..t(true, false, k % 11 != 7, 2)
        });
    }
    for k in 0..200u64 {
        seq.push(ShallowTick {
            latch_floor_frames: 4,
            ..t(true, false, k % 11 != 7, 3)
        });
    }
    // ROZHODNUTÉ 5842848307: on the min-latency marker the histogram reads the RAW floor -- the
    // same idle window latches D 2 governed (not the budgeted 3, which the guard would cap).
    for k in 0..90u64 {
        seq.push(ShallowTick {
            latch_floor_frames: 2,
            min_latency_box: true,
            ..t(true, k == 0, true, 1)
        });
    }
    // design 5844353368: an idle relock (latch floor 1) whose source carries a sticky content floor
    // of 2 latches D 3 -- max(p90, sticky) + 1; the same window on the min-latency marker ignores
    // the sticky floor (D 2); a sticky floor past the clamp is clamped and reported (capped).
    for k in 0..90u64 {
        seq.push(ShallowTick {
            sticky_floor_frames: 2,
            ..t(true, k == 0, true, 1)
        });
    }
    for k in 0..90u64 {
        seq.push(ShallowTick {
            sticky_floor_frames: 3,
            min_latency_box: true,
            ..t(true, k == 0, true, 1)
        });
    }
    for k in 0..90u64 {
        seq.push(ShallowTick {
            sticky_floor_frames: 9,
            ..t(true, k == 0, true, 1)
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
    for (lag, iv) in &latch_floors {
        body.push_str(&format!(
            "    printf(\"%llu\\n\", (unsigned long long)genlock_n1_shallow_latch_floor_frames({lag}ULL, {iv}ULL));\n"
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
    for (tw, h, nx, bd, f, l, iv, tg) in &gap_holds {
        body.push_str(&format!(
            "    printf(\"%d\\n\", genlock_n1_shallow_gap_hold_due({tw}ULL, {h}ULL, {nx}ULL, {bd}ULL, {f}ULL, {l}u, {iv}ULL, {tg}ULL) ? 1 : 0);\n"
        ));
    }
    // The tick sequence goes in as C arrays driven by one loop (a statement per tick made the
    // harness take minutes to compile at -O1 once the sequence passed 3000 ticks).
    let col = |f: &dyn Fn(&ShallowTick) -> String| seq.iter().map(f).collect::<Vec<_>>().join(",");
    body.push_str(&format!(
        "    {{ static const unsigned char F[] = {{{}}};\n      static const unsigned long long FL[] = {{{}}};\n      static const unsigned long long LA[] = {{{}}};\n      static const unsigned long long BA[] = {{{}}};\n      static const unsigned long long RE[] = {{{}}};\n      static const unsigned long long ST[] = {{{}}};\n",
        col(&|x| (b(x.n1) | (b(x.relock) << 1) | (b(x.on_grid) << 2) | (b(x.deep) << 3) | (b(x.min_latency_box) << 4) | (b(x.backlog_relock) << 5)).to_string()),
        col(&|x| format!("{}ULL", x.floor_frames)),
        col(&|x| format!("{}ULL", x.latch_floor_frames)),
        col(&|x| format!("{}ULL", x.base_frames)),
        col(&|x| format!("{}ULL", x.realized_frames)),
        col(&|x| format!("{}ULL", x.sticky_floor_frames)),
    ));
    body.push_str(&format!(
        "      uint64_t tf = 0, fm = 0; uint32_t wt = 0, ov = 0, dt = 0, un = 0, ch = 0, qu = 0, rj = 0; uint32_t hi[4] = {{0u, 0u, 0u, 0u}}; bool me = false, ca = false;\n      for (int k = 0; k < {}; k++) {{\n        const unsigned f = F[k];\n        bool l = genlock_n1_shallow_track(&tf, &fm, &wt, &ov, &dt, &me, &ca, (f & 1u) != 0, (f & 2u) != 0, (f & 4u) != 0, FL[k], LA[k], BA[k], (f & 8u) != 0, (f & 16u) != 0, RE[k], (f & 32u) != 0, ST[k], hi, &un, &ch, &qu, &rj);\n        printf(\"%d %llu %llu %u %u %u %d %d %u %u %u %u %u %u %u %u\\n\", l ? 1 : 0, (unsigned long long)tf, (unsigned long long)fm, (unsigned)wt, (unsigned)ov, (unsigned)dt, me ? 1 : 0, ca ? 1 : 0, (unsigned)un, (unsigned)ch, (unsigned)qu, (unsigned)rj, (unsigned)hi[0], (unsigned)hi[1], (unsigned)hi[2], (unsigned)hi[3]);\n      }}\n    }}\n",
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
    for (lag, iv) in &latch_floors {
        want.push(n1_shallow_latch_floor_frames(*lag, *iv).to_string());
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
    let mut gap_fired = 0usize;
    for (tw, h, nx, bd, f, l, iv, tg) in &gap_holds {
        let r = n1_shallow_gap_hold_due(*tw, *h, *nx, *bd, *f, *l, *iv, *tg);
        gap_fired += usize::from(r);
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
    // the floor fell (3), the unreachable D's re-measure (3) and the relock storm's (3); then
    // (ROZHODNUTÉ 5842640404) the budgeted idle latch (3) and the rise past the budget (the clamp, 4),
    // then (ROZHODNUTÉ 5842848307) the min-latency box's raw-floor latch (2, governed), then (design
    // 5844353368) the sticky floor's idle latch (3), the marker ignoring it (2) and its clamp (4).
    assert!(
        fired.0 > 0 && fired.0 < sheds.len(),
        "shed vectors one-sided: {fired:?}"
    );
    assert!(
        fired.1 > 0 && fired.1 < holds.len(),
        "hold vectors one-sided: {fired:?}"
    );
    assert!(
        gap_fired > 0 && gap_fired < gap_holds.len(),
        "GAP hold vectors one-sided: {gap_fired} of {}",
        gap_holds.len()
    );
    assert_eq!(
        latched,
        vec![3, 3, 4, 2, 31, 31, 2, 0, 2, 2, 2, 4, 4, 3, 3, 3, 3, 4, 2, 3, 2, 4],
        "the track sequence latched {latched:?}"
    );
}
