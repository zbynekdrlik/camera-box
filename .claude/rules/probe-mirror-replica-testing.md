---
paths:
  - "src/probe/genlock.rs"
  - "src/genlock_backlog.rs"
---

# Re-pinning a probe-gated mirror against OBSERVED output — the authority-importing replica (#1037)

`src/probe/genlock.rs` (`ReleaseCadence` and friends) is `#[cfg(feature = "probe")]`, so its ~dozen
tick-driven cadence tests compile and run on **CI only** — there is no local RED→GREEN, not even a
compile check (see the CLAUDE.md Local Build Policy). When a ticket must change the selection/anchor
logic here to mirror the deployed C, DO NOT hand-trace the re-pinned expectations, and DO NOT guess
them — build a **default-feature scratch replica** and RUN it. This is the Rust-probe analogue of
`vendored-libobs-change-safety.md`'s lift-and-compile-standalone recipe (that rule covers the C side;
this covers the Rust probe side).

## The recipe

1. Scratch cargo crate in the scratchpad with a path dependency on this worktree:
   `camera-box = { path = "/…/.claude/worktrees/<name>" }`.
2. `main.rs`: `use camera_box::genlock_backlog as auth;` — import the REAL Tier-0 authority
   (`relock_select_nearest` / `relock_anchor_age_ns` / `phase_anchor_from_present` /
   `backlog_relock_threshold` / `should_drain_one` / `source_interval_from_stamps`). This is the only
   non-trivial arithmetic; importing it means ZERO copy-drift.
3. Paste a byte-faithful copy of the probe struct's `tick` + helper methods (they already route
   through `crate::genlock_backlog::*`, which becomes `camera_box::genlock_backlog::*` in the replica)
   AND the `#[cfg(test)] mod tests` sim harnesses (`run_cadence_sim*`). Inline the tiny probe-only
   helpers (`genlock_present_ts_reserve` = `wall - reserve_ms*1e6`).
4. `cargo run --release` and print the observed presented/dropped/anchor sequences per scenario. Pin
   THOSE, documenting the derivation in the test comment.

The `camera-box` lib builds default-features-only as a dep (fast, no probe deps), so the replica
compiles in ~1 min and re-runs instantly.

## The non-obvious finding this ticket produced

The phase-anchored relock selection (issue 1003) is **identical to the old newest-due** whenever the
anchor is UNSET or the reserve is shallow: on a cold ACQUIRE `relock_anchor_age_ns(0, reserve)` ==
configured latency, so `target == wall − reserve == the raw deadline`, and among frames all at/under
the deadline the nearest == the newest. So NONE of the existing tick-driven tests changed — they cold-
acquire (anchor unset) and their relock cases assert INVARIANTS (relocked / dropped>0 / ordered /
mean-Δ), not exact stamps. The phase difference only appears at a relock with a **SET deep anchor**,
which the existing suite never exercised → add DEMONSTRATIVE tests that manually set
`phase_anchor_ns` + `locked_next_boundary_ns` (private, same-module test access) and assert the
nearest-anchor pick differs from newest-due (e.g. present index 12 not 39, keep the ~27-frame
conveyor). Expect the SAME shape for the still-open #940 phase-pinned-deadline harness adoption.

## Cheap local gates that DO cover probe code

`cargo fmt --all --check` parses `#[cfg(feature="probe")]` code (rustfmt ignores cfg), so a syntax
error in the probe file fails fmt locally — a real compile-adjacent gate. Also do brace/paren/bracket
delta vs `origin/dev` (the file carries a pre-existing ±1 paren/bracket imbalance in PROSE; measure
the DELTA, not the absolute). Everything else (real type-checking of the probe module) is CI-first.
