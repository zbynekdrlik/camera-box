---
paths:
  - "src/genlock_backlog.rs"
  - "scripts/genlock_audit_snapshot.py"
  - "scripts/lib/genlock-audit-snapshot.sh"
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
- Issue 1367 (ROZHODNUTÉ 5842640404): the SAME constant also budgets the shallow N==1 latch
  (`n1_shallow_latch_floor_frames` / `genlock_n1_shallow_latch_floor_frames`, the receive-time
  arrival lag + budget, rounded up). Changing it moves which shallow feeds latch one frame deeper —
  re-run the shallow A/V bench and see `genlock-n1-pin-derived-depth.md`.

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

**Producer wiring (landed):** `scripts/lib/genlock-audit-snapshot.sh`, sourced once by
`recording-e2e.sh` — `genlock_audit_snapshot_capture before` just before `[5/8] StartRecord`,
`… after` just after the strih StopRecord, `genlock_audit_snapshot_compute` after the merge, the JSON
passed as `e2e_discord_report_send`'s 7th arg (anchors pinned in
`tests/harness_genlock_audit_snapshot_wiring_1354.rs`). Report-only and fail-open end to end. The
strih read goes through the shared `strih_log_tail` reader (a raw 3000-line window + a LOCAL audit
filter, both strih platforms) — `.claude/rules/strih-log-read.md`; the `GENLOCK_AUDIT_SNAPSHOT_READER_CMD`
seam replaces the whole read in tests.
