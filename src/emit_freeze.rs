//! #944 — emit/output-liveness self-watchdog (pure decision, Tier-0).
//!
//! The EMIT-side sibling of [`crate::capture_wedge`] (#945). `#945` watches *capture-thread*
//! liveness — "did the blocking `VIDIOC_DQBUF` return?" — by stamping a heartbeat immediately
//! after every `process_frame()` return (`src/main.rs`), and fires only when the dequeue never
//! returns at all. But `VideoCapture::process_frame` returns `Ok(())` on a `V4L2_BUF_FLAG_ERROR`
//! buffer (`src/capture.rs`: it logs the corrupted-buffer WARN, records the sequence, and returns
//! *before* the emit callback is reached). So on a stream where every dequeue returns an
//! Ok-but-corrupted buffer, the `#945` heartbeat keeps advancing (the loop is not wedged) while
//! `emit_count` never moves — the NDI sender keeps its last good frame registered and a consumer
//! sees a *live source showing a frozen picture*. Live incident (cam4, 10.77.9.64, 2026-08-02):
//! every health signal (systemd `active`, the process, mDNS, `#656` captured-fps, and — on today's
//! code — `#945`) reads green while a frozen frame is published; the only true signal is "no good
//! frame has reached NDI in N seconds", and nothing watched it.
//!
//! This watchdog adds that missing signal: a SECOND monotonic heartbeat, stamped by the capture
//! loop only when a frame is actually EMITTED (`emitted_this > 0`), polled by the SAME separate
//! watchdog thread `#945` already runs (which the capture loop can never block). The verdict is
//! DISCRIMINATED against a true wedge so the two watchdogs never fight over one event: emit-freeze
//! fires only when the emit heartbeat is stale AND the *capture-return* heartbeat is still FRESH
//! (the capture thread is alive, just producing no usable output). In a true wedge both heartbeats
//! freeze together, so the capture-return staleness also grows past the freshness bound and
//! emit-freeze suppresses itself — `#945` owns that case. On a real emit-freeze the watchdog logs
//! a uniquely grep-able CRITICAL line + its own distinct exit code and `std::process::exit`s, so
//! systemd's `Restart=always` tears the NDI sender down (the frozen source goes *gone*, not
//! frozen — the maintainer's resolved design decision) and re-opens the device. Recovery is
//! external, exactly like `#945`.
//!
//! Per `.claude/rules/self-heal-frozen-leg-attribution.md`'s "one distinct event per condition"
//! rule, this module owns a distinctly-worded CRITICAL line and a dedicated exit code
//! ([`EMIT_FREEZE_EXIT_CODE`], distinct from `capture_wedge`'s 79, `painter_wedge`'s 80, and
//! `capture_rate_selfheal`'s 77/78) — never overloading the `#945` wedge or `#663` self-heal
//! shapes existing forensics (issue 946 correlation) key on.
//!
//! Pure decision + message-formatting half only (Tier-0, default features, no I/O) — mirrors
//! [`crate::capture_wedge`]'s split. The atomics, the poll, and the `std::process::exit` live in
//! `src/main.rs`.

/// How many seconds the capture loop may go WITHOUT EMITTING A SINGLE GOOD FRAME — while its
/// blocking dequeue is still returning (see [`CAPTURE_FRESH_BOUND_S`]) — before the watchdog
/// treats the output as frozen. 15 s is 3× the existing 5 s "Streaming:" stats-tick cadence: well
/// above the ~849 ms `#707` startup dequeue stall and any brief self-recovering corrupted burst (a
/// healthy box logs a transient `-71` / corrupted buffer and keeps streaming), short enough to
/// stop a genuine frozen source in well under the `#945` wedge threshold.
pub const EMIT_FREEZE_THRESHOLD_S: f64 = 15.0;

/// The capture-return heartbeat (the `#945` "did the dequeue return" signal) must be YOUNGER than
/// this for a stale emit to count as an emit-freeze rather than a thread wedge. Equal to the
/// watchdog poll interval (5 s). Load-bearing: it must be strictly less than
/// [`EMIT_FREEZE_THRESHOLD_S`] so that in a TRUE wedge — where both heartbeats freeze at the same
/// instant — the capture-return staleness has already grown past this bound by the time the emit
/// staleness reaches the threshold, making emit-freeze suppress itself and leave the wedge to
/// `#945`.
pub const CAPTURE_FRESH_BOUND_S: f64 = 5.0;

// Compile-time guarantee (clippy-safe `const _` form, per `.claude/rules/wedge-watchdog-pattern.md`
// — a runtime assert on a const trips `clippy::assertions_on_constants` under `-D warnings`): the
// freshness bound must be a positive value strictly below the freeze threshold, or the wedge/
// emit-freeze discrimination above breaks.
const _: () = assert!(CAPTURE_FRESH_BOUND_S > 0.0);
const _: () = assert!(EMIT_FREEZE_THRESHOLD_S > CAPTURE_FRESH_BOUND_S);

/// Process exit code used when the watchdog forces a restart because the capture output is frozen
/// (dequeue still returning, no good frame emitted). Distinct from every other liveness exit code
/// so `systemctl status`/journal forensics can always tell an emit-freeze restart apart from a
/// `#945` capture-wedge (79), a `#936` painter-wedge (80), a `#663` USB-reset self-heal (77/78),
/// or a generic crash.
pub const EMIT_FREEZE_EXIT_CODE: i32 = 81;

/// The watchdog's verdict for THIS poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmitFreezeVerdict {
    /// A good frame reached NDI recently enough (or the capture thread itself has wedged, which is
    /// `#945`'s domain, not this one) — nothing to do.
    Publishing,
    /// The capture thread is alive (its dequeue returned recently) but no good frame has been
    /// emitted for at least the threshold — the NDI output is frozen.
    Frozen,
}

/// Pure decision. `Frozen` iff BOTH:
/// - `seconds_since_last_emit >= emit_threshold_s` (no good frame emitted for at least the
///   threshold), AND
/// - `seconds_since_last_capture_return < capture_fresh_bound_s` (the capture thread's blocking
///   dequeue returned recently — it is NOT wedged; a wedge is `#945`'s job).
///
/// Any non-finite or negative input, or a non-positive threshold/bound (a disabled/misconfigured
/// watchdog), yields `Publishing` — never interpreted as "always frozen". Mirrors
/// [`crate::capture_wedge::evaluate_wedge`]'s defensive guard shape.
pub fn evaluate_emit_freeze(
    seconds_since_last_emit: f64,
    seconds_since_last_capture_return: f64,
    emit_threshold_s: f64,
    capture_fresh_bound_s: f64,
) -> EmitFreezeVerdict {
    // RED STUB (#944): the real two-input discriminator is not implemented yet — always claims
    // the output is Publishing, so the watchdog can never fire. The tests below encode the
    // required behavior and MUST fail against this stub (proving they catch the missing detection)
    // before the GREEN commit implements it.
    let _ = (
        seconds_since_last_emit,
        seconds_since_last_capture_return,
        emit_threshold_s,
        capture_fresh_bound_s,
    );
    EmitFreezeVerdict::Publishing
}

/// Build the CRITICAL, uniquely grep-able message the watchdog logs right before it exits the
/// process. Pure string formatting so the exact wording is unit-tested. Names issue 944, the
/// `CRITICAL` keyword, and the exact exit code so forensics (issue 946 correlation) can never
/// confuse it with a `#945` wedge, a `#663` self-heal, or a generic crash.
pub fn emit_freeze_message(
    seconds_since_last_emit: f64,
    seconds_since_last_capture_return: f64,
    emit_threshold_s: f64,
) -> String {
    format!(
        "CRITICAL #944: NDI output FROZEN — no good frame has been emitted in \
         {seconds_since_last_emit:.1}s (>= {emit_threshold_s:.1}s threshold) while the capture \
         thread is still alive (its blocking V4L2 dequeue returned {seconds_since_last_capture_return:.1}s \
         ago — NOT a #945 wedge). The device keeps returning unusable buffers (e.g. corrupted \
         V4L2_BUF_FLAG_ERROR), so the NDI sender is publishing a stale/frozen frame while every \
         health signal reads green — exiting now (code {EMIT_FREEZE_EXIT_CODE}) so systemd's \
         Restart=always tears the sender down (the source goes gone, not frozen) and re-opens the \
         device. See #944."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- evaluate_emit_freeze — the two-input discriminator -------------------------------------

    #[test]
    fn recent_emit_is_publishing() {
        // Emit happened well within the threshold, capture fresh — clearly fine.
        assert_eq!(
            evaluate_emit_freeze(0.0, 0.0, EMIT_FREEZE_THRESHOLD_S, CAPTURE_FRESH_BOUND_S),
            EmitFreezeVerdict::Publishing
        );
        assert_eq!(
            evaluate_emit_freeze(5.0, 0.05, EMIT_FREEZE_THRESHOLD_S, CAPTURE_FRESH_BOUND_S),
            EmitFreezeVerdict::Publishing
        );
    }

    #[test]
    fn just_under_the_emit_threshold_is_publishing() {
        assert_eq!(
            evaluate_emit_freeze(
                EMIT_FREEZE_THRESHOLD_S - 0.01,
                0.05,
                EMIT_FREEZE_THRESHOLD_S,
                CAPTURE_FRESH_BOUND_S
            ),
            EmitFreezeVerdict::Publishing
        );
    }

    #[test]
    fn stale_emit_while_capture_returning_is_frozen() {
        // THE #944 case: dequeue still returning (0.02s ago — a corrupted-buffer stream at ~60fps)
        // but no good frame emitted for 15s. This is what a stub that always says Publishing must
        // FAIL on.
        assert_eq!(
            evaluate_emit_freeze(
                EMIT_FREEZE_THRESHOLD_S,
                0.02,
                EMIT_FREEZE_THRESHOLD_S,
                CAPTURE_FRESH_BOUND_S
            ),
            EmitFreezeVerdict::Frozen
        );
        // Well past the threshold, capture still fresh — still frozen.
        assert_eq!(
            evaluate_emit_freeze(120.0, 0.1, EMIT_FREEZE_THRESHOLD_S, CAPTURE_FRESH_BOUND_S),
            EmitFreezeVerdict::Frozen
        );
    }

    #[test]
    fn stale_emit_but_capture_also_wedged_is_not_our_case() {
        // TRUE WEDGE: both heartbeats frozen together, so the capture-return staleness has grown
        // past the freshness bound. emit-freeze must SUPPRESS ITSELF and leave this to #945.
        assert_eq!(
            evaluate_emit_freeze(
                EMIT_FREEZE_THRESHOLD_S + 5.0,
                EMIT_FREEZE_THRESHOLD_S + 5.0, // capture returned just as long ago == wedge
                EMIT_FREEZE_THRESHOLD_S,
                CAPTURE_FRESH_BOUND_S
            ),
            EmitFreezeVerdict::Publishing
        );
        // capture-return staleness exactly at the freshness bound is NOT fresh (strict `<`).
        assert_eq!(
            evaluate_emit_freeze(
                100.0,
                CAPTURE_FRESH_BOUND_S,
                EMIT_FREEZE_THRESHOLD_S,
                CAPTURE_FRESH_BOUND_S
            ),
            EmitFreezeVerdict::Publishing
        );
    }

    #[test]
    fn frozen_needs_both_conditions() {
        // Capture fresh but emit not yet stale -> Publishing.
        assert_eq!(
            evaluate_emit_freeze(1.0, 0.02, EMIT_FREEZE_THRESHOLD_S, CAPTURE_FRESH_BOUND_S),
            EmitFreezeVerdict::Publishing
        );
        // Emit stale but capture also stale (wedge) -> Publishing (leave to #945).
        assert_eq!(
            evaluate_emit_freeze(100.0, 100.0, EMIT_FREEZE_THRESHOLD_S, CAPTURE_FRESH_BOUND_S),
            EmitFreezeVerdict::Publishing
        );
    }

    #[test]
    fn non_finite_or_negative_inputs_never_freeze() {
        assert_eq!(
            evaluate_emit_freeze(f64::NAN, 0.0, EMIT_FREEZE_THRESHOLD_S, CAPTURE_FRESH_BOUND_S),
            EmitFreezeVerdict::Publishing
        );
        assert_eq!(
            evaluate_emit_freeze(
                f64::INFINITY,
                0.0,
                EMIT_FREEZE_THRESHOLD_S,
                CAPTURE_FRESH_BOUND_S
            ),
            EmitFreezeVerdict::Publishing
        );
        assert_eq!(
            evaluate_emit_freeze(-1.0, 0.0, EMIT_FREEZE_THRESHOLD_S, CAPTURE_FRESH_BOUND_S),
            EmitFreezeVerdict::Publishing
        );
        assert_eq!(
            evaluate_emit_freeze(100.0, f64::NAN, EMIT_FREEZE_THRESHOLD_S, CAPTURE_FRESH_BOUND_S),
            EmitFreezeVerdict::Publishing
        );
        assert_eq!(
            evaluate_emit_freeze(100.0, -1.0, EMIT_FREEZE_THRESHOLD_S, CAPTURE_FRESH_BOUND_S),
            EmitFreezeVerdict::Publishing
        );
    }

    #[test]
    fn non_positive_threshold_or_bound_never_freeze() {
        // A disabled/misconfigured watchdog must never read as "always frozen".
        assert_eq!(
            evaluate_emit_freeze(100.0, 0.0, 0.0, CAPTURE_FRESH_BOUND_S),
            EmitFreezeVerdict::Publishing
        );
        assert_eq!(
            evaluate_emit_freeze(100.0, 0.0, -5.0, CAPTURE_FRESH_BOUND_S),
            EmitFreezeVerdict::Publishing
        );
        assert_eq!(
            evaluate_emit_freeze(100.0, 0.0, EMIT_FREEZE_THRESHOLD_S, 0.0),
            EmitFreezeVerdict::Publishing
        );
        assert_eq!(
            evaluate_emit_freeze(100.0, 0.0, EMIT_FREEZE_THRESHOLD_S, -1.0),
            EmitFreezeVerdict::Publishing
        );
    }

    // ---- emit_freeze_message — the CRITICAL forensic line ---------------------------------------

    #[test]
    fn message_names_ticket_critical_and_exit_code() {
        let m = emit_freeze_message(18.3, 0.02, EMIT_FREEZE_THRESHOLD_S);
        assert!(m.contains("#944"), "must name the ticket: {m}");
        assert!(m.contains("CRITICAL"), "must be CRITICAL: {m}");
        assert!(
            m.contains(&EMIT_FREEZE_EXIT_CODE.to_string()),
            "must name the exit code: {m}"
        );
        assert!(m.contains("18.3"), "must report the emit staleness: {m}");
        assert!(
            m.contains("FROZEN"),
            "must state the output is frozen: {m}"
        );
        // Must distinguish itself from a #945 wedge so forensics never confuse the two.
        assert!(m.contains("#945"), "must contrast with the #945 wedge: {m}");
    }

    #[test]
    fn exit_code_is_distinct_from_the_other_liveness_codes() {
        assert_ne!(EMIT_FREEZE_EXIT_CODE, crate::capture_wedge::CAPTURE_WEDGE_EXIT_CODE);
        assert_ne!(
            EMIT_FREEZE_EXIT_CODE,
            crate::capture_rate_selfheal::SELF_HEAL_EXIT_CODE
        );
        assert_ne!(
            EMIT_FREEZE_EXIT_CODE,
            crate::capture_rate_selfheal::SELF_HEAL_RESET_FAILED_EXIT_CODE
        );
        assert_ne!(
            EMIT_FREEZE_EXIT_CODE,
            crate::painter_wedge::PAINTER_WEDGE_EXIT_CODE
        );
    }
}
