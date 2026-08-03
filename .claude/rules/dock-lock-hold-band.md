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

## General acceptance pattern: proving "stopped writing" needs evidence the actuator still WANTED to write (#942 deploy session)

When a fix DISABLES an actuator — makes it monitor-only, turns off a periodic corrector — the
natural acceptance test is "the value it used to write is now constant". **That test alone is
worthless.** A constant value is equally consistent with the disabled actuator working correctly
AND with the whole feature being dead, mis-deployed, or never having loaded in the first place —
the flat line looks identical either way (see `.claude/rules/rig-state-inspection.md` trap 1 for
the live incident where a "the fix looks live" belief was wrong for exactly this reason: the
build simply hadn't deployed).

The honest acceptance has TWO halves, gathered over the SAME time window:

1. **The value did not change.** #942: 5 reads of `genlock_latency_ms_src` on stream's `NDI 2ME
   PGM`, ~45 s apart — `836/836/836/836/836`.
2. **The disabled actuator was ALIVE, measuring, and repeatedly WANTED to change it.** #942: 6
   `LOCK-CORRECT SUGGESTED genlock_latency_ms_src 836 -> 841ms (measured offset=51-64ms)
   [monitor-only -- #942 gate is the sole writer]` lines inside the same 3 minutes, alongside
   `diag ... locked=yes` and `UPDATED offset=... source=cluster matched=98/99 mad=21-25ms`.

Only the PAIR proves "it stopped writing" rather than "it stopped running" (or never started).
Half 1 alone is exactly what a mis-deployed / never-loaded build ALSO produces.

**Apply this to any future disable-an-actuator / make-it-monitor-only change on this dock — and
to the same shape anywhere else on the rig:** gather BOTH halves in the same measurement window
before calling it accepted. A flat value with no corroborating "still alive, still wanting to
act" log evidence is not proof the fix worked; it might just be silence.
