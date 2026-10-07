//! #1299 Part 3 + #1341 + issue 1302 — one genlock input's phase events and its idle class: the
//! counters the recent-event feed reads ([`InputEventCounts`], [`input_phase_events`]), the
//! per-input baseline ([`PhaseEventSample`], [`input_new_phase_events`]) and the fast first idle
//! classification ([`InputIdleClass`], [`input_idle_class`] and the `GENLOCK_IDLE_*` constants).
//! Mirrored in `GenlockLockState.hpp`, parity-gated by `tests/genlock_lock_state_parity.rs`,
//! `tests/genlock_phase_baseline_1302.rs` and `tests/genlock_idle_class_1302.rs`. Split out of
//! `genlock_lock_state.rs` (issue 1302); every item is re-exported at `crate::genlock_lock_state`.

/// #1299 Part 3 — one genlock input's cumulative event counters as the recent-event driver reads
/// them (issue 1302: through [`PhaseEventSample::of`], one input at a time). Plain scalars so the C
/// mirror (`GenlockLockState.hpp`, `genlock_input_phase_events`) ports byte-for-byte; the committed
/// parity gate `tests/genlock_lock_state_parity.rs` keeps the two numerically identical.
#[derive(Debug, Clone, Copy)]
pub struct InputEventCounts {
    /// The DistroAV receiver has a live NDI connection (sender running). A disconnected input
    /// contributes ZERO — its #1096 fresh-finder rebind churn is not a lock event.
    pub connected: bool,
    /// #1341 — the input is CONNECTED but IDLE (keep-alive-only, received-frame rate below
    /// `GENLOCK_IDLE_INPUT_MIN_FRAMES` over the window). Contributes ZERO phase events — exactly the
    /// `connected == false` path — so a SongPlayer playlist input's relock churn on each ~11 s
    /// keep-alive frame never feeds `recent_event`. Additive: `idle == false` is the pre-#1341 shape.
    pub idle: bool,
    /// FIFO relock count (a boundary was re-acquired — a phase-discipline event).
    pub relocks: u64,
    /// Late-hold count (a hold fired after its deadline — a phase-discipline event).
    pub late_holds: u64,
    /// Backward-step count (the phase stepped back — a phase-discipline event).
    pub backward_steps: u64,
}

/// #1299 Part 3 — the "phase event" count for ONE input feeding `recent_event`: the clock/phase
/// class a genlock LOCK verdict owns — `relocks + late_holds + backward_steps`.
///
/// UNDERRUNS ARE EXCLUDED (decision (c)): an underrun is a LATENCY-BUDGET miss (the FIFO ran dry
/// because the upstream frame arrived after the certified `latency_ms` pin), which the
/// `genlock-fifo audit` line + cg-chain-verify (issue 1302) already surface and gate; it is also
/// bursty, so counting it here latched `recent_event` DEGRADED chronically. An ABSENT input
/// (`connected == false`) contributes 0 — its rebind/reset churn is idle, not a fault. Saturating
/// so a synthetic near-`u64::MAX` parity vector can never overflow (the C mirror clamps identically).
pub fn input_phase_events(c: &InputEventCounts) -> u64 {
    // #1341 — a disconnected OR a connected-but-idle input contributes 0: an idle input's keep-alive
    // relock churn is not a phase-discipline event to feed recent_event.
    if !c.connected || c.idle {
        return 0;
    }
    c.relocks
        .saturating_add(c.late_holds)
        .saturating_add(c.backward_steps)
}

/// Issue 1302 — one input's phase-event sample, as the widget's per-input event BASELINE remembers
/// it from one 1 Hz tick to the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhaseEventSample {
    /// The input's phase total as [`input_phase_events`] reads it: the cumulative
    /// `relocks + late_holds + backward_steps` (saturating) while it contributes, 0 while it does not.
    pub total: u64,
    /// The input feeds `recent_event` this tick: CONNECTED and not IDLE.
    pub contributing: bool,
}

impl PhaseEventSample {
    /// The sample of one input's counters this tick.
    pub fn of(c: &InputEventCounts) -> PhaseEventSample {
        PhaseEventSample {
            total: input_phase_events(c),
            contributing: c.connected && !c.idle,
        }
    }
}

/// Issue 1302 — the NEW phase events of ONE input since the widget's previous tick (`prev`, `None` =
/// the widget has no sample of it: first sight, or it left the scan and was forgotten). This is the
/// per-input baseline that replaced the #1299 aggregate compare. That compare summed every input's
/// LIFETIME total and raised `recent_event` on any rise, so a reconnecting or waking input added its
/// whole total at once and held the box DEGRADED for 60 s after every reattach.
///
/// The cases:
///
/// - it contributed in BOTH samples and its total rose: the rise;
/// - it just started contributing (a reconnect, a wake from idle, or first sight): 0, and the current
///   total becomes its baseline;
/// - its total went backward (a counter reset): 0, re-baseline;
/// - it does not contribute now: 0.
///
/// Mirrored byte-for-byte by `genlock_input_new_phase_events` in `GenlockLockState.hpp`,
/// parity-gated by `tests/genlock_phase_baseline_1302.rs`.
pub fn input_new_phase_events(prev: Option<PhaseEventSample>, cur: PhaseEventSample) -> u64 {
    match prev {
        Some(p) if p.contributing && cur.contributing && cur.total >= p.total => {
            cur.total - p.total
        }
        _ => 0,
    }
}

/// #1341 — the window (ms) of the widget's per-input received-frame ring; older samples are pruned.
pub const GENLOCK_IDLE_WINDOW_MS: i64 = 60_000;
/// #1341 — the full-window rule reads the ring once it spans 90 % of the window (54 s).
pub const GENLOCK_IDLE_FULL_SPAN_MS: i64 = GENLOCK_IDLE_WINDOW_MS * 9 / 10;
/// #1341 — fewer received frames than this over the full window = IDLE (keep-alive-only, ~1 frame
/// per 11 s); a live source at >= 23.98 fps delivers >= 1400.
pub const GENLOCK_IDLE_INPUT_MIN_FRAMES: u64 = 60;
/// Issue 1302 — the fast first classification: LIVE once the ring spans at least this long ...
pub const GENLOCK_IDLE_FAST_SPAN_MS: i64 = 5_000;
/// ... with at least this many frames in that span (>= 12 fps over 5 s; a keep-alive input delivers
/// at most 1 frame in 5 s).
pub const GENLOCK_IDLE_FAST_MIN_FRAMES: u64 = 60;

/// Issue 1302 — the widget's class of ONE connected genlock input.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputIdleClass {
    /// Just (re)connected or first seen: not yet proven live. Contributes nothing (no phase
    /// events, no lock count), the idle path.
    Unclassified = 0,
    /// Delivers frames at a live rate: graded and feeds `recent_event`.
    Live = 1,
    /// #1341 keep-alive-only over the full window: the idle path.
    Idle = 2,
}

impl InputIdleClass {
    /// The integer the C `genlock_input_idle_class` returns (`genlock_input_idle_class_t`).
    pub fn code(self) -> u8 {
        self as u8
    }
}

/// Issue 1302 — classify ONE connected input from its received-frame ring: `span_ms` = the ring's
/// newest minus oldest sample time, `delta_frames` = the frames received across it, `prev` = the
/// class of the previous tick (`Unclassified` after a (re)connect, a first sight or a counter reset).
///
/// The rules, in order:
///
/// - the ring spans the full window: the #1341 rule decides, `Idle` below
///   [`GENLOCK_IDLE_INPUT_MIN_FRAMES`], else `Live`;
/// - the fast rule: a ring spanning [`GENLOCK_IDLE_FAST_SPAN_MS`] with at least
///   [`GENLOCK_IDLE_FAST_MIN_FRAMES`] in it is `Live`, whatever the previous class (a keep-alive
///   input can never meet it, so an `Idle` input that went live during a long widget stall is
///   counted 5 s after it, not 54 s);
/// - otherwise the previous class holds: `Live` is never demoted on a short ring (a ring a stalled
///   widget timer pruned to one sample), `Idle` stays `Idle`, `Unclassified` stays `Unclassified`
///   (the fast stage never says `Idle`).
///
/// A live source therefore contributes after ~5 s, a keep-alive one (~1 frame per 11 s) never; the
/// widget's per-input event baseline is taken at the first `Live` tick ([`input_new_phase_events`]
/// re-baselines on a not-contributing to contributing step).
///
/// Mirrored byte-for-byte by `genlock_input_idle_class` in `GenlockLockState.hpp`, parity-gated by
/// `tests/genlock_idle_class_1302.rs`.
pub fn input_idle_class(span_ms: i64, delta_frames: u64, prev: InputIdleClass) -> InputIdleClass {
    if span_ms >= GENLOCK_IDLE_FULL_SPAN_MS {
        return if delta_frames < GENLOCK_IDLE_INPUT_MIN_FRAMES {
            InputIdleClass::Idle
        } else {
            InputIdleClass::Live
        };
    }
    if span_ms >= GENLOCK_IDLE_FAST_SPAN_MS && delta_frames >= GENLOCK_IDLE_FAST_MIN_FRAMES {
        return InputIdleClass::Live;
    }
    prev
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- #1299 Part 3: the connected-phase-only recent-event feed + offender attribution ----------

    fn ev(connected: bool, relocks: u64, late_holds: u64, backward_steps: u64) -> InputEventCounts {
        InputEventCounts {
            connected,
            idle: false,
            relocks,
            late_holds,
            backward_steps,
        }
    }

    // #1341 — an idle (connected-but-keep-alive-only) input, otherwise carrying phase counters.
    fn ev_idle(relocks: u64, late_holds: u64, backward_steps: u64) -> InputEventCounts {
        InputEventCounts {
            connected: true,
            idle: true,
            relocks,
            late_holds,
            backward_steps,
        }
    }

    #[test]
    fn phase_events_sum_the_three_phase_classes() {
        assert_eq!(input_phase_events(&ev(true, 2, 3, 4)), 9);
    }

    #[test]
    fn phase_events_exclude_underruns_by_construction() {
        // InputEventCounts has NO underrun field — an underrun can never contribute to a phase-event
        // count. Two inputs differing only in (hypothetical) underruns compute identically here.
        assert_eq!(input_phase_events(&ev(true, 1, 0, 0)), 1);
    }

    #[test]
    fn absent_input_contributes_no_phase_events() {
        // The #1096 rebind churn of a senderless input (its relocks climb) must NOT count.
        assert_eq!(input_phase_events(&ev(false, 99, 88, 77)), 0);
    }

    #[test]
    fn idle_input_contributes_no_phase_events() {
        // #1341 — a connected-but-idle input's relock/late-hold churn (a keep-alive frame re-acquires
        // a FIFO boundary every ~11 s) must NOT feed recent_event: exactly the connected==false path.
        assert_eq!(input_phase_events(&ev_idle(60, 30, 5)), 0);
    }

    #[test]
    fn saturating_never_overflows_on_a_pathological_count() {
        assert_eq!(input_phase_events(&ev(true, u64::MAX, 5, 0)), u64::MAX);
    }

    // --- issue 1302: the per-input event baseline (a reattach never counts old events as new) ------

    fn sample(total: u64, contributing: bool) -> PhaseEventSample {
        PhaseEventSample {
            total,
            contributing,
        }
    }

    #[test]
    fn the_sample_reads_the_total_and_the_contribution_1302() {
        assert_eq!(PhaseEventSample::of(&ev(true, 2, 3, 4)), sample(9, true));
        // an absent or an idle input contributes nothing, so its total reads 0
        assert_eq!(PhaseEventSample::of(&ev(false, 2, 3, 4)), sample(0, false));
        assert_eq!(PhaseEventSample::of(&ev_idle(2, 3, 4)), sample(0, false));
    }

    #[test]
    fn a_steadily_contributing_input_adds_its_rise_1302() {
        assert_eq!(
            input_new_phase_events(Some(sample(40, true)), sample(43, true)),
            3
        );
        assert_eq!(
            input_new_phase_events(Some(sample(40, true)), sample(40, true)),
            0
        );
    }

    #[test]
    fn a_reconnect_rebaselines_instead_of_counting_the_lifetime_total_1302() {
        // the songplayer probe: 40 lifetime relocks, disconnected last tick, connected now
        let was = PhaseEventSample::of(&ev(false, 40, 0, 0));
        let now = PhaseEventSample::of(&ev(true, 40, 0, 0));
        assert_eq!(input_new_phase_events(Some(was), now), 0);
        // the next tick counts only what happened after the attach
        let next = PhaseEventSample::of(&ev(true, 41, 0, 0));
        assert_eq!(input_new_phase_events(Some(now), next), 1);
    }

    #[test]
    fn a_wake_from_idle_rebaselines_1302() {
        let was = PhaseEventSample::of(&ev_idle(60, 30, 5));
        let now = PhaseEventSample::of(&ev(true, 60, 30, 5));
        assert_eq!(input_new_phase_events(Some(was), now), 0);
    }

    #[test]
    fn first_sight_rebaselines_1302() {
        // the live strih-lx offender of 7.10.2026: 567 lifetime relocks on CG-obs
        let now = PhaseEventSample::of(&ev(true, 567, 0, 0));
        assert_eq!(input_new_phase_events(None, now), 0);
    }

    #[test]
    fn a_backward_total_rebaselines_1302() {
        assert_eq!(
            input_new_phase_events(Some(sample(50, true)), sample(3, true)),
            0
        );
        // the next rise counts from the new baseline
        assert_eq!(
            input_new_phase_events(Some(sample(3, true)), sample(5, true)),
            2
        );
    }

    #[test]
    fn a_vanished_input_returns_as_first_sight_1302() {
        // The widget forgets an input that left the scan. It relocked 50 times while away; when it
        // returns, the widget has no sample of it (None), so the 50 never count.
        let left = sample(40, true);
        assert_eq!(input_new_phase_events(Some(left), sample(40, true)), 0);
        assert_eq!(input_new_phase_events(None, sample(90, true)), 0);
    }

    #[test]
    fn an_input_that_stops_contributing_adds_nothing_1302() {
        let was = sample(40, true);
        let gone = PhaseEventSample::of(&ev(false, 45, 0, 0));
        let idle = PhaseEventSample::of(&ev_idle(45, 0, 0));
        assert_eq!(input_new_phase_events(Some(was), gone), 0);
        assert_eq!(input_new_phase_events(Some(was), idle), 0);
    }

    #[test]
    fn new_events_never_overflow_1302() {
        assert_eq!(
            input_new_phase_events(Some(sample(0, true)), sample(u64::MAX, true)),
            u64::MAX
        );
    }
}

#[cfg(test)]
#[path = "idle_tests.rs"]
mod idle_tests;
