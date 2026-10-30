---
paths:
  - "src/probe/genlock.rs"
  - "src/genlock_backlog.rs"
---

# Re-pinning a probe-gated mirror against OBSERVED output (#1037)

`src/probe/genlock.rs` (`ReleaseCadence` and friends) is `#[cfg(feature = "probe")]`. When a ticket
must change the selection/anchor logic here to mirror the deployed C, DO NOT hand-trace the re-pinned
expectations, and DO NOT guess them — run the tick-driven sims (`run_cadence_sim*`) and pin the
OBSERVED presented/dropped/anchor sequences, documenting the derivation in the test comment.

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
