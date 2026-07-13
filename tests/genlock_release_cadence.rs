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
//! The fix ports `src/probe/genlock.rs` `ReleaseCadence` (v2, cc815e73e) into
//! `vendor/obs-studio/libobs/obs-source.c`: the deadline comes from a LOCKED per-source
//! boundary that advances exactly one interval per presented frame (slew-immune by
//! construction); the wall clock only acquires the lock; and EVERY discarded frame is
//! counted (`genlock_dropped_due`) — never silent again.
//!
//! v2 (live canary of v1, 2026-07-02, strih `NDI cam5`): v1's wall-based drift guard
//! (`present_ts > boundary + 2*interval + interval/4`) EMBEDS the constant stamp→arrival
//! skew (59 ms live at the 3 ms reserve) and relock-stormed — dropped_due 2918 of 4202
//! received (69 %), relocks 1076. v2 replaces it with a QUEUE-DEPTH backlog guard
//! (`GENLOCK_QDEPTH_RELOCK`): steady depth is ~1–2 at ANY skew (the boundary paces
//! arrivals), so depth is skew-immune where wall−boundary drift is not; the steady path
//! presents the OLDEST matured frame (strict FIFO, transient 2-frame maturation drains
//! losslessly) and a GAP RESYNC re-anchors past upstream-skipped stamps.
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
    // v2: the v1 WALL-DRIFT relock guard must be GONE. It compared `present_ts` against
    // `boundary + 2*interval + interval/4`, which EMBEDS the constant stamp→arrival skew
    // (59 ms on the live rig) — the 2026-07-02 canary relock-stormed: dropped_due 2918 of
    // 4202 received, relocks 1076. Any wall−boundary drift threshold reintroduces that
    // skew dependence; the backlog guard must be queue-relative (depth), never wall-based.
    assert!(
        !src.contains("2 * interval + interval / 4"),
        "{OBS_SOURCE}: #401 v2 — the v1 wall-drift relock guard (2*interval + interval/4 \
         vs present_ts − boundary) is BACK; it embeds the constant stamp→arrival skew and \
         relock-storms live (canary: dropped_due 2918/4202, relocks 1076). Use the \
         queue-depth guard (GENLOCK_QDEPTH_RELOCK) — mirror: src/probe/genlock.rs \
         ReleaseCadence::tick (v2)."
    );
    // v2: the QUEUE-DEPTH backlog guard must be present — a named constant so the
    // rationale travels with the threshold. Steady depth is ~1–2 at any arrival skew
    // (the locked boundary paces arrivals), so depth > GENLOCK_QDEPTH_RELOCK is
    // unambiguous backlog; the re-lock jumps to the newest due frame counting every
    // jumped frame (visible catch-up, IMAG latency contract kept).
    assert!(
        src.contains("GENLOCK_QDEPTH_RELOCK"),
        "{OBS_SOURCE}: #401 v2 — the queue-depth backlog guard (GENLOCK_QDEPTH_RELOCK) is \
         missing; without it a genuine stall's backlog never re-locks to the live edge. \
         Mirror src/probe/genlock.rs ReleaseCadence::QDEPTH_RELOCK."
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

#[test]
fn steady_multi_consumes_at_an_integer_source_multiple_of_the_canvas() {
    // #726 — the live-event "like 15fps" judder. At a STRUCTURAL N>=2 source:canvas ratio (a
    // 60fps NDI camera into strih's 30fps canvas) the STEADY path presented the OLDEST matured
    // frame and re-anchored the boundary to it; one canvas interval (33_333_333 ns) lands a HAIR
    // under 2 source intervals (33_333_334 ns), so the boundary matured only ONE frame per tick
    // while N arrived — content crawled +1 source frame/tick, the queue grew, and the backlog
    // storm jumped +7 (~5x/s). The fix: for a structural N>=2 source (detected from the stamp
    // grid), mature every frame up to the boundary PLUS a half-interval slack, release the NEWEST
    // and retire the older matured one(s) into genlock_dropped_due — a uniform every-Nth-frame
    // cadence tracking real time, slew-immune (keys on the boundary, not the wall). N==1 keeps the
    // present-oldest lossless-drain path byte-identical. Behavioral RED->GREEN proof is
    // src/probe/genlock.rs `cadence_60_into_30_presents_uniform_every_second_frame`; this
    // default-features guard pins the C port so a subtree-pull or edit can't silently revert it.
    let src = squish(&vendor_file(OBS_SOURCE));
    assert!(
        src.contains("static inline bool genlock_source_is_integer_multiple("),
        "{OBS_SOURCE}: #726 — the integer-multiple detector \
         (genlock_source_is_integer_multiple, derived from the stamp grid) is gone; the 60->30 \
         STEADY cadence would crawl+jump again. Mirror: src/probe/genlock.rs \
         ReleaseCadence::source_is_integer_multiple."
    );
    assert!(
        src.contains("if (genlock_source_is_integer_multiple(source, interval)) {"),
        "{OBS_SOURCE}: #726 — the STEADY release no longer branches on \
         genlock_source_is_integer_multiple; the 60->30 multi-consume (present newest matured, \
         retire older) reverted to present-oldest (the crawl). Re-apply."
    );
    assert!(
        src.contains("mature_deadline")
            && src.contains("source->genlock_locked_next_boundary_ns + interval / 2"),
        "{OBS_SOURCE}: #726 — the STEADY N>=2 maturation slack (boundary + interval/2, so the \
         frame ~one canvas interval ahead matures despite canvas_interval being a hair under \
         N*src_interval) is gone; the multi-consume would mature only 1 frame and still crawl."
    );
}
