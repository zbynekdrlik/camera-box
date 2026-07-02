//! #401 — phase-locked release cadence in the C ts-align genlock release (vendored libobs).
//!
//! Root cause (run 7020001, 2026-07-02): the pre-#401 ts-align release re-derived the
//! presentation deadline from the wall clock EVERY render tick
//! (`present_ts = wall_now - reserve`) and presented the NEWEST due frame, silently erasing
//! the older due ones (`to_drop = due - 1` with NO counter). With render ticks and capture
//! stamps on the same DanteSync 60 Hz grid, a reserve near a multiple of the frame interval
//! puts the deadline ON a stamp: the ±2 ms render-tick slew then flips that frame
//! due/not-due tick-to-tick — alternating HOLD + silent DROP. Measured live (`NDI cam5`):
//! 43.9–57.7 distinct fps of a 60 fps flow, invisible in the audit (8,511 ids lost with
//! zero counter movement).
//!
//! The fix ports `src/probe/genlock.rs` `ReleaseCadence` (CI-green at 73ae3fca) into
//! `vendor/obs-studio/libobs/obs-source.c`: the deadline comes from a LOCKED per-source
//! boundary that advances exactly one interval per presented frame (slew-immune by
//! construction); the wall clock only acquires the lock and detects drift beyond
//! `2*interval + interval/4` (re-lock = stall catch-up, keeping the IMAG latency contract);
//! and EVERY discarded frame is counted (`genlock_dropped_due`) — never silent again.
//!
//! This is a SOURCE-presence guard (same convention as tests/distroav_genlock_lockdown.rs,
//! tests/obs_updater_disabled.rs): it runs on DEFAULT features (per-PR Linux CI + local
//! Tier-0), reads the vendored C as text, and fails loudly if a future `git subtree pull`
//! (#44) — or any edit — silently reverts the cadence to the per-tick wall-compare release.

use std::path::PathBuf;

const OBS_SOURCE: &str = "vendor/obs-studio/libobs/obs-source.c";
const OBS_INTERNAL: &str = "vendor/obs-studio/libobs/obs-internal.h";

fn vendor_file(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Collapse every run of ASCII whitespace to a single space so the assertions survive
/// reformatting (e.g. an upstream merge re-indenting a line).
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn cadence_state_and_counters_declared_on_obs_source() {
    // The per-source cadence state + honest-loss counters must live on obs_source
    // (obs-internal.h), next to the other genlock audit fields, so the 5s audit line can
    // print them and a reconnect/create zeroes them with the rest (bzalloc).
    let internal = squish(&vendor_file(OBS_INTERNAL));
    for field in [
        "uint64_t genlock_locked_next_boundary_ns;",
        "uint64_t genlock_dropped_due;",
        "uint64_t genlock_relocks;",
        "uint64_t genlock_late_holds;",
    ] {
        assert!(
            internal.contains(field),
            "{OBS_INTERNAL}: #401 — cadence field `{field}` missing from obs_source; the \
             phase-locked release cadence (or its honest drop/relock/late-hold audit) \
             reverted. Re-apply (mirror: src/probe/genlock.rs ReleaseCadence)."
        );
    }
}

#[test]
fn release_is_phase_locked_not_per_tick_wall_compare() {
    // The ts-align release block must key steady-state presentation on the LOCKED
    // boundary (genlock_locked_next_boundary_ns), acquired/re-locked from the wall
    // deadline — not re-derive a due/not-due decision from the wall clock every tick
    // (the pre-#401 hold↔silent-drop churn).
    let src = squish(&vendor_file(OBS_SOURCE));
    assert!(
        src.contains("genlock_locked_next_boundary_ns"),
        "{OBS_SOURCE}: #401 — the phase-locked release cadence (locked boundary \
         genlock_locked_next_boundary_ns) is gone from the ts-align release; the per-tick \
         wall-compare release loses ~16 of 60 distinct fps at grid-aligned reserves. \
         Re-apply (mirror: src/probe/genlock.rs ReleaseCadence::tick)."
    );
    // The drift guard threshold must mirror ReleaseCadence::relock_drift_ns EXACTLY:
    // two intervals plus a quarter-interval margin (small enough that a stall catches up
    // within ~2 frames; large enough that the steady lock offset never trips it).
    assert!(
        src.contains("2 * interval + interval / 4"),
        "{OBS_SOURCE}: #401 — the re-lock drift threshold no longer mirrors \
         relock_drift_ns (2*interval + interval/4); keep the C and the Rust mirror in \
         lock-step (src/probe/genlock.rs ReleaseCadence::relock_drift_ns)."
    );
    // The mirror pointer must survive so the next maintainer finds the PROVEN Rust
    // reference (and its three cadence tests) before touching the C.
    assert!(
        src.contains("ReleaseCadence"),
        "{OBS_SOURCE}: #401 — the mirror pointer to src/probe/genlock.rs ReleaseCadence is \
         gone; the C and Rust implementations will drift apart silently. Re-apply."
    );
}

#[test]
fn every_discarded_frame_is_counted_never_silent() {
    // THE #401 bug: `to_drop = due - 1; while (to_drop--) da_erase(...)` erased stale due
    // frames with NO counter — 8,511 ids lost in run 7020001 with zero audit movement.
    // Every erase on the release path must now count into genlock_dropped_due.
    let src = squish(&vendor_file(OBS_SOURCE));
    assert!(
        src.contains("source->genlock_dropped_due++"),
        "{OBS_SOURCE}: #401 — the release-path erase no longer counts each discarded frame \
         (source->genlock_dropped_due++); silent drops are back. Every da_erase on the \
         ts-align release must increment genlock_dropped_due."
    );
    // Re-lock (stall catch-up) and late-hold (frame overdue but not arrived) events must
    // be counted too — the drop/hold classification the run-7020001 diagnosis lacked.
    for inc in ["source->genlock_relocks++", "source->genlock_late_holds++"] {
        assert!(
            src.contains(inc),
            "{OBS_SOURCE}: #401 — the cadence event counter `{inc}` is gone; the \
             relock/late-hold audit signal reverted. Re-apply."
        );
    }
}

#[test]
fn audit_line_exposes_cadence_counters() {
    // The 5s genlock-fifo audit line must surface the new counters (after backward_steps=,
    // existing fields untouched — scripts parse the line by field name) so a live loss is
    // visible in the OBS log alone (comprehensive-logging; the #401 loss was invisible).
    let src = squish(&vendor_file(OBS_SOURCE));
    assert!(
        src.contains("backward_steps=%llu dropped_due=%llu relocks=%llu late_holds=%llu locked=%d"),
        "{OBS_SOURCE}: #401 — the genlock-fifo audit line no longer prints \
         dropped_due=/relocks=/late_holds=/locked= (after backward_steps=); release-path \
         frame loss is invisible in the log again. Re-apply."
    );
}

#[test]
fn backward_step_recovery_survives_the_cadence() {
    // #147/#269: the backward wall-clock step re-anchor (present OLDEST, preserve the
    // buffer, count once per event) must remain intact alongside the cadence — the
    // cadence must not regress it. (tests/genlock_preload.rs guards the full #147
    // contract probe-gated; this default-features guard pins the composition.)
    let src = squish(&vendor_file(OBS_SOURCE));
    for marker in [
        "max_ts > wall_now + interval",
        "source->genlock_backward_steps++",
        "source->genlock_in_backward_step = true",
    ] {
        assert!(
            src.contains(marker),
            "{OBS_SOURCE}: #401/#147 — the backward-step recovery marker `{marker}` is \
             gone; the cadence port regressed the #147/#269 re-anchor. Re-apply."
        );
    }
}
