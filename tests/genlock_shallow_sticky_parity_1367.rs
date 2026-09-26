//! issue 1367 (design 5844353368) — the EXECUTABLE C-vs-Rust parity gate for the shallow latch's
//! STICKY content floor. The lifted `genlock_n1_shallow_sticky_track` (inside the contiguous N==1
//! block, compiled by the shared `genlock_n1_lift` module) must match
//! `camera_box::genlock_n1_depth::n1_shallow_sticky_track` tick by tick. The latch reading the
//! sticky floor (`max(p90, sticky) + 1`, ignored on the min-latency marker, clamped) is covered by
//! the tracker sequence in `tests/genlock_shallow_depth_parity_1367.rs`. `cc` is required — per the
//! project's test-strictness rule this FAILS LOUDLY rather than skipping when the toolchain is
//! missing.

use camera_box::genlock_n1_depth::{
    n1_shallow_sticky_track, ShallowSticky, ShallowTick, N1_SHALLOW_SETTLE_TICKS,
    N1_SHALLOW_STICKY_DECAY_NS,
};

mod genlock_n1_lift;
use genlock_n1_lift::compile_and_run_n1_block;

/// One sticky-track input: the tick, whether the audio flows, and the scheduled tick instant.
struct Step {
    t: ShallowTick,
    audio: bool,
    wall: u64,
}

#[test]
fn c_n1_shallow_sticky_floor_matches_the_rust_authority_1367() {
    let i30 = 33_333_333u64;
    let min = 60_000_000_000u64;
    let t0 = 1_790_000_000_000_000_000u64;
    let blk = u64::from(N1_SHALLOW_SETTLE_TICKS);
    let tick = |relock: bool, latch: u64| ShallowTick {
        n1: true,
        relock,
        on_grid: true,
        floor_frames: 1,
        latch_floor_frames: latch,
        base_frames: 1,
        deep: false,
        min_latency_box: false,
        realized_frames: u64::MAX,
        backlog_relock: false,
        sticky_floor_frames: 0,
    };
    let mut seq: Vec<Step> = Vec::new();
    let mut wall = t0;
    let push = |seq: &mut Vec<Step>, t: ShallowTick, audio: bool, wall: &mut u64| {
        seq.push(Step {
            t,
            audio,
            wall: *wall,
        });
        *wall += i30;
    };
    // no audio: nothing observed; then a content block (latch floor 2) while the audio flows.
    for _ in 0..blk + 7 {
        push(&mut seq, tick(false, 2), false, &mut wall);
    }
    for k in 0..blk {
        push(&mut seq, tick(k == 40, 2), true, &mut wall);
    }
    for _ in 0..blk {
        push(&mut seq, tick(false, 2), true, &mut wall);
    }
    // idle blocks never lower it, off-grid ticks are not sampled, a deeper block raises it (each
    // phase starts on a relock, which restarts the observation block).
    for k in 0..2 * blk {
        push(
            &mut seq,
            ShallowTick {
                on_grid: k % 7 != 3,
                ..tick(k == 0, 1)
            },
            true,
            &mut wall,
        );
    }
    for k in 0..blk {
        push(&mut seq, tick(k == 0, 3), true, &mut wall);
    }
    // a transient block (spread 3), an over-clamp block and a floor at base (base 30) never count.
    for k in 0..blk {
        push(
            &mut seq,
            tick(k == 0, if k < 30 { 4 } else { 1 }),
            true,
            &mut wall,
        );
    }
    for k in 0..blk {
        push(&mut seq, tick(k == 0, 11), true, &mut wall);
    }
    for k in 0..blk {
        push(
            &mut seq,
            ShallowTick {
                base_frames: 30,
                ..tick(k == 0, 1)
            },
            true,
            &mut wall,
        );
    }
    // the decay: 30 min without an observation at the level drops one frame, and one more exactly
    // 30 min after that (the boundary itself decays); a wall stepped back never decays; a block at
    // the level resets the clock.
    wall += N1_SHALLOW_STICKY_DECAY_NS - blk * i30;
    for _ in 0..3 {
        push(&mut seq, tick(false, 1), false, &mut wall);
    }
    wall += N1_SHALLOW_STICKY_DECAY_NS - 3 * i30;
    push(&mut seq, tick(false, 1), false, &mut wall);
    wall -= 10 * min;
    push(&mut seq, tick(false, 1), true, &mut wall);
    wall += 20 * min;
    for _ in 0..blk {
        push(&mut seq, tick(false, 2), true, &mut wall);
    }
    // a block AT the level 10 min later refreshes the clock, so 25 min after it nothing decays
    // (35 min after the raise).
    wall += 10 * min;
    for _ in 0..blk {
        push(&mut seq, tick(false, 2), true, &mut wall);
    }
    wall += 25 * min;
    push(&mut seq, tick(false, 1), true, &mut wall);
    // an N>=2 tick clears it; the min-latency marker keeps none.
    push(
        &mut seq,
        ShallowTick {
            n1: false,
            ..tick(false, 2)
        },
        true,
        &mut wall,
    );
    // a fresh block with a short burst (8 of 90 ticks one frame deeper): the p90 ignores it.
    for k in 0..blk {
        push(
            &mut seq,
            tick(false, if k < 8 { 3 } else { 2 }),
            true,
            &mut wall,
        );
    }
    for _ in 0..blk + 3 {
        push(
            &mut seq,
            ShallowTick {
                min_latency_box: true,
                ..tick(false, 3)
            },
            true,
            &mut wall,
        );
    }

    let b = |v: bool| u32::from(v);
    let col = |f: &dyn Fn(&Step) -> String| seq.iter().map(f).collect::<Vec<_>>().join(",");
    let mut body = String::new();
    body.push_str(&format!(
        "    {{ static const unsigned char F[] = {{{}}};\n      static const unsigned long long LA[] = {{{}}};\n      static const unsigned long long BA[] = {{{}}};\n      static const unsigned long long WA[] = {{{}}};\n",
        col(&|x| (b(x.t.n1) | (b(x.t.relock) << 1) | (b(x.t.on_grid) << 2) | (b(x.audio) << 3) | (b(x.t.min_latency_box) << 4)).to_string()),
        col(&|x| format!("{}ULL", x.t.latch_floor_frames)),
        col(&|x| format!("{}ULL", x.t.base_frames)),
        col(&|x| format!("{}ULL", x.wall)),
    ));
    body.push_str(&format!(
        "      uint64_t fl = 0, se = 0; uint32_t ot = 0; uint32_t oh[4] = {{0u, 0u, 0u, 0u}};\n      for (int k = 0; k < {}; k++) {{\n        const unsigned f = F[k];\n        const uint64_t r = genlock_n1_shallow_sticky_track(&fl, &se, &ot, oh, (f & 1u) != 0, (f & 2u) != 0, (f & 4u) != 0, (f & 8u) != 0, (f & 16u) != 0, LA[k], BA[k], WA[k]);\n        printf(\"%llu %llu %llu %u %u %u %u %u\\n\", (unsigned long long)r, (unsigned long long)fl, (unsigned long long)se, (unsigned)ot, (unsigned)oh[0], (unsigned)oh[1], (unsigned)oh[2], (unsigned)oh[3]);\n      }}\n    }}\n",
        seq.len()
    ));
    let c_out = compile_and_run_n1_block("genlock_n1_shallow_sticky_parity_1367", &body);

    let mut s = ShallowSticky::default();
    let mut want = Vec::new();
    let mut floors = Vec::new();
    for x in &seq {
        let r = n1_shallow_sticky_track(&mut s, &x.t, x.audio, x.wall);
        if floors.last() != Some(&r) {
            floors.push(r);
        }
        want.push(format!(
            "{r} {} {} {} {} {} {} {}",
            s.floor_frames,
            s.seen_ns,
            s.obs_ticks,
            s.obs_hist[0],
            s.obs_hist[1],
            s.obs_hist[2],
            s.obs_hist[3]
        ));
    }
    assert_eq!(c_out.len(), want.len(), "harness printed the wrong count");
    let diffs: Vec<String> = c_out
        .iter()
        .zip(&want)
        .enumerate()
        .filter(|(_, (c, r))| c != r)
        .map(|(k, (c, r))| format!("  line {k}: C `{c}` vs Rust `{r}`"))
        .take(12)
        .collect();
    assert!(
        diffs.is_empty(),
        "issue 1367: the vendored C sticky content floor DIVERGED from the Tier-0 Rust authority:\n{}",
        diffs.join("\n")
    );
    // every branch the sequence scripts: the content block (2), the deeper block (3), the two
    // decays (2, 1), the refresh at the level (2), the N>=2 clear (0), a fresh block (2) and the
    // min-latency clear (0).
    assert_eq!(
        floors,
        vec![0, 2, 3, 2, 1, 2, 0, 2, 0],
        "the sticky sequence produced {floors:?}"
    );
}
