//! (#889) dupe-preferring decimation for the genlock capture->emit gate.
//!
//! Root cause (rig-validated, issue 889): a fast/over-rate USB grabber (ShadowCast 2 measured
//! ~64.14 fps captured against a 60 Hz HDMI source) runs its own capture clock faster than the
//! genlock target rate and repeats its internal buffer to keep up — an exact BYTE-IDENTICAL
//! duplicate frame roughly once every ~15 captures, always an ISOLATED pair (never a triple),
//! every other captured frame genuinely unique (camera sensor noise + painter motion). The
//! pre-existing genlock decimation gate (`genlock_pacing::genlock_emit_gate`) decides purely from
//! WALL-CLOCK TIME which captured frame to emit at each target-rate boundary — it has no notion
//! of frame CONTENT, so it sometimes keeps the grabber's dupe (because it happened to be the
//! frame that crossed the boundary) and drops the unique tick captured just before it. That is
//! the exact mechanism behind the per-cambox-window `copies`/`gaps` failures this ticket fixes.
//!
//! The fix: pacing still decides WHEN a frame must be shed (unchanged —
//! [`crate::genlock_pacing::genlock_emit_gate`]); this module decides WHICH captured frame is the victim.
//! [`DecimationGate::poll`] prefers to shed a captured frame that is content-identical to the
//! immediately preceding capture (a grabber dupe), deferring emission by exactly ONE more
//! capture — bounded to a single deferral per boundary so the emitted rate is never affected
//! (validated: dupes are always isolated pairs, never triples, so a second consecutive dupe is
//! not expected on real hardware; the bound protects every model regardless). When the frame
//! that crossed the boundary is NOT a dupe, or a dupe was already deferred once for this
//! boundary, behavior is IDENTICAL to the pre-fix blind pacing drop.
//!
//! (#1111) A deferral holds the wall-clock boundary for one extra capture, which is lag-neutral
//! ONLY in the on-time/surplus regime (the replacement capture still lands inside the SAME
//! interval). At a genuine over-rate like ~62 fps, a dupe often arrives while the gate is already
//! in the CATCH-UP regime (the frame is late); deferring THERE holds the boundary while the wall
//! clock runs on, ratcheting the gate's lag +1 interval per deferral until it trips
//! `genlock_pacing::genlock_emit_gate`'s #707 resync (~9 boundaries leapt at once) — the issue-1110 CAM1
//! judder. So the deferral is gated on `genlock_pacing::genlock_emit_on_time`: a dupe is deferred only when
//! on-time; a LATE dupe is EMITTED instead (a repeated frame — invisible, and the mathematically
//! unavoidable ~2 copies/s when a ~58-unique-fps grabber must feed a steady 60), keeping the emit
//! grid locked to wall-clock. That emitted-copy is counted in [`DupeShedLog`] for live visibility.
//!
//! (#1145) SUPERSEDED at a genuine over-rate: the ~2 copies/s above are the floor ONLY when the
//! source's UNIQUE rate is genuinely below the target (a 58-unique grabber, a 50->60 pulldown). A
//! plain over-rate on a true-60 source has ~60 unique fps, so ZERO copies are needed and the LATE
//! dupe above was a jitter-driven bug: it presents as the strih 15fps-judder. v2 RETIRES a late
//! over-rate dupe instead (shed it AND advance the already-stale boundary, emitting nothing —
//! [`dupe_shed_action`] / [`ShedAction::Retire`]), gated on a measured trailing UNIQUE rate so a
//! genuinely starved OR frozen source still falls back to the late-dupe copy valve above.
//! `genlock_pacing::genlock_emit_on_time` is retained only as the lag==0 equivalence anchor;
//! production keys on the numeric `genlock_pacing::genlock_lag_intervals` instead.
//!
//! Default ON, every grabber model, no env knob (the standing "a needed feature is always on,
//! never a forgettable toggle" rule) — self-neutralizing on a healthy card: shedding only
//! happens when the pacing gate would shed ANYWAY (over-rate forcing a drop), and dupe
//! preference only changes WHICH captured frame within that already-required shed is the
//! victim.
//!
//! Linux-gated in lock-step with capture/ndi (calls into [`crate::genlock_pacing::genlock_emit_gate`] and
//! is shaped around a raw V4L2 YUYV422 frame); pure logic, unit-tests Tier-0 on the Linux `test`
//! CI job (default features).

// (#1165) File split for the ~1000-line #414 budget — a MOVE-only refactor: every item below
// keeps its public path `camera_box::dupe_decimation::X` via the glob re-exports, and the
// submodules carry byte-identical logic. Dependency direction (acyclic): `signature` (pure
// hash/sig) and `shed` (shed-decision logic + constants) are independent leaves — neither has a
// code dependency on the other or on `gate`; `gate` (the DecimationGate state machine +
// DupeShedLog + summary) depends on both via `use super::*`. See
// `.claude/rules/genlock-emit-gate-pacing.md`.
mod gate;
mod shed;
mod signature;

pub use gate::*;
pub use shed::*;
pub use signature::*;

#[cfg(test)]
mod tests;
