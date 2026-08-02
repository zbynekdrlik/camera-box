---
paths:
  - "src/av_sync_dock.rs"
  - "vendor/av-sync-dock/src/camera-box-audio.hpp"
  - "tests/av_sync_dock_lock_926.rs"
  - "tests/av_sync_dock_cpp_mirror_gate.rs"
---

# `DockLockCorrector` hold-band tuning (#942)

**Before narrowing/widening/closing an interval in `DockLockCorrector::decide()` (or its C++
twin), verify what the EXISTING test suite actually pins — a fix for one edge case can silently
regress a DIFFERENT, deliberately-tested behavior at ordinary inputs.**

## The #942 lesson

The dock's hold band is `[margin, 2*margin)` — half-open, deliberately NOT closed at the upper
edge. A code review found that at the narrowest possible margin (`margin ==
DOCK_LOCK_MIN_MARGIN_MS == 1.0`, i.e. `mad_ms <= 1.0` or the non-finite fallback), `round()`-to-
mid targeting can land `ts_new` EXACTLY at the band's upper edge (`2*margin`) — a value the
closed-form proof's `[mid-0.5, mid+0.5]` guarantee explicitly allows. The half-open Hold check
then treats that landed value as "still outside" on the very next measurement, costing one extra
actuator write.

**The tempting fix — make the upper edge inclusive (`[margin, 2*margin]`) — is WRONG and was
tried + reverted.** It breaks `corrector_respects_the_hardware_clamp_at_the_ceiling`, which pins
the DELIBERATE behavior that a value sitting exactly at `2*margin` at an ORDINARY margin (e.g.
`mad_ms=5.0`, `offset=10.0`) still gets nudged toward the middle rather than being treated as
already-settled. Closing the interval fixes the rare degenerate case (`margin==1.0`) by breaking
the common, already-tested one.

**What was actually true, and the right fix:** the narrow-margin edge case is NOT a recurrence of
the #942 limit-cycle. It costs exactly ONE extra correction (the same middle-nudge mechanism used
at any other margin), which then lands solidly inside the band and Holds permanently. The correct
test is `corrector_settles_within_one_extra_tick_when_a_landed_correction_hits_the_bands_exact_edge`
— it pins BOUNDED CONVERGENCE (the sequence terminates within one extra tick), not "must Hold
immediately on this landed value". Mirrored in the C++ twin harness's check (14).

**The general rule for this file:** before changing ANY band/threshold boundary condition
(`>=`/`>`/`<`/`<=`), run the FULL `av_sync_dock::` test module first to see what's pinned at that
exact boundary, and think about whether the boundary is being hit by a DEGENERATE input (an
extreme/edge parameter value, rare in production) vs. an ORDINARY one (a realistic `mad_ms` in the
10-25ms production range) — a fix that's correct for one can be wrong for the other.
