//! issue 1367 (ROZHODNUTÉ 5827497952) — the EXECUTABLE C-vs-Rust parity gate for the SHALLOW
//! N==1 per-lock depth (split out of `tests/genlock_relock_selection_parity.rs`, review round 2: the
//! 1000-line test budget). The lifted `genlock_n1_shallow_*` helpers (inside the contiguous N==1
//! block, compiled by the shared `genlock_n1_lift` module) must match
//! `camera_box::genlock_n1_depth` byte for byte. `cc` is required — per the project's
//! test-strictness rule this FAILS LOUDLY rather than skipping when the toolchain is missing.

use camera_box::genlock_n1_depth::{
    n1_shallow_gap_is_relock, n1_shallow_governs, n1_shallow_hold_due, n1_shallow_shed_due,
    n1_shallow_target_frames, n1_shallow_track, n1_shallow_window_deep, ShallowDepth, ShallowTick,
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
/// it).
#[test]
fn c_n1_shallow_depth_matches_the_rust_authority_1367() {
    let i30 = 33_333_333u64;
    let w = 1_000_000_000_000u64;
    // (base, floor_max, deep, min_latency)
    let targets: [(u64, u64, bool, bool); 11] = [
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
    // One present tick: (n1, relock, on_grid, floor_frames, base, deep, min_latency).
    let mut seq: Vec<ShallowTick> = Vec::new();
    let t = |n1: bool, relock: bool, on_grid: bool, floor: u64| ShallowTick {
        n1,
        relock,
        on_grid,
        floor_frames: floor,
        base_frames: 1,
        deep: false,
        min_latency_box: false,
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

    let b = |v: bool| i32::from(v);
    let mut body = String::new();
    for (base, f, d, m) in &targets {
        body.push_str(&format!(
            "    {{ bool cap = false; unsigned long long d = (unsigned long long)genlock_n1_shallow_target_frames({base}ULL, {f}ULL, {}, {}, &cap); printf(\"%llu %d\\n\", d, cap ? 1 : 0); }}\n",
            b(*d),
            b(*m)
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
    body.push_str(
        "    { uint64_t tf = 0, fm = 0; uint32_t wt = 0, ov = 0, dt = 0; bool me = false, ca = false;\n",
    );
    for x in &seq {
        body.push_str(&format!(
            "      {{ bool l = genlock_n1_shallow_track(&tf, &fm, &wt, &ov, &dt, &me, &ca, {}, {}, {}, {}ULL, {}ULL, {}, {}); printf(\"%d %llu %llu %u %u %u %d %d\\n\", l ? 1 : 0, (unsigned long long)tf, (unsigned long long)fm, (unsigned)wt, (unsigned)ov, (unsigned)dt, me ? 1 : 0, ca ? 1 : 0); }}\n",
            b(x.n1),
            b(x.relock),
            b(x.on_grid),
            x.floor_frames,
            x.base_frames,
            b(x.deep),
            b(x.min_latency_box)
        ));
    }
    body.push_str("    }\n");
    let c_out = compile_and_run_n1_block("genlock_n1_shallow_parity_1367", &body);

    let mut want: Vec<String> = Vec::new();
    for (base, f, d, m) in &targets {
        let (dd, cap) = n1_shallow_target_frames(*base, *f, *d, *m);
        want.push(format!("{dd} {}", b(cap)));
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
            "{} {} {} {} {} {} {} {}",
            b(l),
            s.target_frames,
            s.floor_max_frames,
            s.window_ticks,
            s.over_ticks,
            s.deep_ticks,
            b(s.measuring),
            b(s.capped)
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
    // relock's deeper D, the re-measure after the rise, the auto-window after N>=2, the deep base + 1
    // (twice: the second with a not-deep latch tick), the mid-window relock's fresh D, the capped
    // report (0) and the relatch after it.
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
        vec![3, 3, 5, 2, 31, 31, 2, 0, 2],
        "the track sequence latched {latched:?}"
    );
}
