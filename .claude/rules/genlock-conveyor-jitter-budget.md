---
paths:
  - "src/genlock_backlog.rs"
  - "scripts/genlock_audit_snapshot.py"
  - "vendor/obs-studio/libobs/obs-source.c"
---

# N>=2 genlock conveyor arrival-jitter budget + the audit-delta ladder recipe (#1354)

## The seam: `GENLOCK_N2_JITTER_BUDGET_NS` (single-sourced, 3 lock-stepped copies)

The N>=2 phase shed (`should_converge_phase`) parks the conveyor `quantum + budget` above its
arrival floor, where `budget = max(PHASE_PIN_HYSTERESIS_NS, GENLOCK_N2_JITTER_BUDGET_NS)`
(5 ms grid hysteresis vs the 15 ms #1354 arrival-jitter budget). The budget IS the receive-side
arrival-lateness tolerance: a late second frame of a 60→30 pair within the budget never costs a
phase step. Before #1354 the budget was the 5 ms hysteresis alone — 5 ms of headroom right after a
shed — and strih-lx's `#797 slow output_video` events (5–17 ms, ~30/min on every input even idle)
pushed whichever input sat near the mature deadline into a SINGLE-MATURE phase ladder (100–250 ms
per-camera delivery lag until the depth-15 relock).

The constant lives in exactly TWO arithmetic copies kept byte-parallel (the #1201-style discipline):

- `src/genlock_backlog.rs` — `pub const GENLOCK_N2_JITTER_BUDGET_NS` + the `budget` line in
  `should_converge_phase` (the Tier-0 authority).
- `vendor/obs-studio/libobs/obs-source.c` — `#define GENLOCK_N2_JITTER_BUDGET_NS 15000000ULL` +
  the `budget` line in `genlock_phase_converge_due`.
- `src/probe/genlock.rs` `ReleaseCadence::should_converge_phase` is a pure DELEGATOR (no
  arithmetic) — it inherits the change automatically, do NOT edit it.

**When you change the C threshold, the parity gate MUST lift the new `#define`.** The C↔Rust gate
`c_phase_convergence_matches_the_rust_authority_1049` (`tests/genlock_relock_selection_parity.rs`)
lifts the helper VERBATIM and lifts each `#define` it references via `lift_define(...)`. A new
constant the helper uses that is NOT lifted fails the standalone `-Werror` compile. Add the
`lift_define(...)` in the same commit. (Same lesson as `vendored-libobs-change-safety.md`, which
covers the C-side lift-and-compile.)

## FINDING: SimConveyor1049 CANNOT reproduce the live phase ladder — the arithmetic test is the proof

I extended `SimConveyor1049` with a jittered-second-frame-arrival driver + a `single_mature`
counter as #1354's design asked, verified it with the rustc-replica pattern, and swept it across
four faithful model variants (arrival-lateness only; + a 2-frame boundary injection; a sub-frame
boundary-phase injection; and a keys-on-boundary fixed-advance conveyor). **Every variant produced
zero single-mature ticks and zero budget-dependent sheds** at every (reserve, skew, lateness,
budget) point.

Root cause of the non-reproduction: `SimConveyor1049`'s STEADY branch RE-ANCHORS the boundary to
the PRESENTED stamp every tick (`self.boundary = ts + I30`) and buffers ~`I30` above the arrival
edge, so a sub-frame phase perturbation washes out on the next tick — the follower self-corrects.
The live pathology is an emergent equilibrium of the real C `genlock_release_tick` (its exact HOLD
non-advance, the reserve-slaved deadline, the relock/erase selection, the receive-thread arrival
phase) — the interactions the simplified unit simulator abstracts away. A full ladder reproduction
would need a from-scratch keys-on-boundary conveyor with the complete HOLD + reserve-deadline
semantics, which is larger than a threshold fix.

**So the FAITHFUL, budget-dependent proof of a shed-threshold change is the DIRECT arithmetic test
on `should_converge_phase`**, not a SimConveyor scenario: a held age between the old 5 ms margin and
the new 15 ms budget above `reserve + quantum` stays INSIDE the dead-band (no shed); below 5 ms
inert; above 15 ms fires; N==1 inert. That flips RED→GREEN cleanly (verify with the rustc replica:
the module is self-contained, `rustc --edition 2021 --test` a copy with `//!` stripped to `//`, flip
the constant 5 ms↔15 ms). Do NOT ship a `single_mature == 0` SimConveyor assertion the simulator
produces trivially — it is a tautology, not a reproduction.

## RECIPE: name a conveyor ladder from the genlock-fifo audit counters (scope 3)

The `genlock-fifo audit '<name>':` OBS-log line (obs-source.c) carries cumulative
`holds` / `relocks` / `converge_sheds` / `dropped_due` per input. A receive-side arrival-jitter
ladder shows as a HIGH `holds`+`relocks` DELTA on ONE input over the recording window; the budget
fix keeps every input's holds delta low.

- `scripts/genlock_audit_snapshot.py` (pure `parse_audit_counters` + `compute_window_deltas`, pytest
  in `tests/python/test_genlock_audit_snapshot_1354.py`) parses a BEFORE and AFTER audit-tail into
  per-input deltas + the named VICTIM (max holds, tie-break relocks). An input with no baseline is
  `partial`; a counter that went backward is a `restarted` clamp-to-0 (OBS restarted mid-window).
- `scripts/e2e_discord_report.py` `_section_genlock_conveyor` renders it — REPORT-ONLY, in
  `compose_report` (the FULL report) ONLY, never `compose_summary` (the Discord summary stays
  byte-identical). Threaded through `scripts/lib/e2e-discord-report.sh` as the fail-open
  `--genlock-audit-json` 7th arg (same guarded shape as the #756 pins / #761 mv-skew args).
- `dropped_due` STRUCTURALLY advances on a 60→30 strih input (issue 1221) — it is context, never a
  ladder signal by itself. Watch `holds`/`relocks`/`converge_sheds`.

**Supervisor wiring (not done in the #1354 worktree lane — `recording-e2e.sh` is a static-anchor
minefield the lane must not edit):** capture the strih OBS-log audit tail into a per-run file just
before `[5/8] StartRecord` and just after `StopRecord` (reuse the `[4c/8]` received= tap /
`genlock-settle.sh` read), run `genlock_audit_snapshot.py --before-log … --after-log … --out
$GENLOCK_AUDIT_JSON`, and pass `$GENLOCK_AUDIT_JSON` as the 7th arg to `e2e_discord_report_send`
(mirroring the pins/mv-skew snapshot invocations at recording-e2e.sh ~5562/5575). Until then the
7th-arg default keeps the section a safe no-op.
