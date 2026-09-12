//! #1298 — the pure LOCKED / DEGRADED / UNLOCKED decision for the in-OBS genlock
//! lock indicator (the statusbar widget in `vendor/obs-studio/frontend/widgets/`).
//!
//! Three producers feed ONE decision: per-source FIFO counters (`obs_genlock_stats`,
//! filled in `obs-source.c`), the NDI output's wall-clock stamping state
//! (`obs_genlock_output_stats`, set by DistroAV's genlock sender), and the dantesync
//! clock facet polled from `:8898/status`. The Qt widget scalarises all three into a
//! [`GenlockFacets`] and asks [`decide`] for the state + a dominant reason; it renders
//! the verdict, and the #1299 fleet bundle-state facet will read the same structs over
//! obs-websocket — so the DECISION is defined ONCE, here, as a pure crate-root module.
//!
//! Pure + crate-root (not under `src/probe/`) so it is Tier-0 verifiable locally — the
//! `src/genlock_backlog.rs` / `src/colour_scale.rs` pattern. The C port lives verbatim in
//! `vendor/obs-studio/frontend/widgets/GenlockLockState.hpp` (`genlock_decide_lock_state`)
//! and MUST stay numerically identical; the committed C-vs-Rust parity gate
//! `tests/genlock_lock_state_parity.rs` lifts the C function, compiles it with `cc`, and
//! requires byte-identical `(state, reason)` on a vector spread. A divergence on either
//! side fails there in seconds instead of surviving to a live rig.

/// The three lock states the statusbar shows, green / amber / red.
///
/// Discriminants match the C `genlock_lock_state` enum and the statusbar's colour order
/// (the parity gate compares `state as u8` against the C return value).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockState {
    /// Red: clock not disciplined, a genlock output present but not stamping wall time,
    /// or no input locked.
    Unlocked = 0,
    /// Amber: some (not all) inputs unlocked, a relock/underrun in the last 60 s, clock
    /// NTP phase failed, or wall-vs-monotonic drift beyond bound.
    Degraded = 1,
    /// Green: clock locked, every genlock input held by the FIFO, output stamping wall time.
    Locked = 2,
}

/// The dominant reason behind a non-green state — the text the statusbar appends and the
/// machine-readable code #1299 reads. Discriminants match the C `genlock_lock_reason` enum.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockReason {
    /// No non-green reason (the state is LOCKED).
    None = 0,
    /// No genlock inputs are configured on this box at all.
    NoGenlock = 1,
    /// The dantesync clock endpoint is absent or reports not-locked.
    Clock = 2,
    /// A genlock NDI output exists but is NOT stamping wall-clock timecodes.
    Output = 3,
    /// Genlock inputs exist but none is currently locked.
    NoInputLocked = 4,
    /// At least one — but not all — genlock inputs are unlocked.
    InputUnlocked = 5,
    /// A relock / underrun / late-hold / backward-step occurred in the last 60 s.
    RecentEvent = 6,
    /// Clock discipline is up but its NTP phase step failed.
    NtpFailed = 7,
    /// Wall-clock vs monotonic (QPC) drift is beyond the allowed bound.
    QpcDrift = 8,
}

impl LockState {
    /// The integer the C `genlock_decide_lock_state` returns — used by the parity gate.
    pub fn code(self) -> u8 {
        self as u8
    }
}

impl LockReason {
    /// The integer the C `genlock_decide_lock_state` writes to `*reason_out`.
    pub fn code(self) -> u8 {
        self as u8
    }
}

/// The scalarised inputs to the decision — one struct the Qt widget fills by enumerating
/// genlock sources/outputs and reading the cached clock facet. Every field is a plain
/// scalar so the C mirror is a byte-for-byte port (no pointers into OBS types).
#[derive(Debug, Clone, Copy)]
pub struct GenlockFacets {
    /// Number of genlock-FIFO inputs present on the box.
    pub n_inputs: u32,
    /// Of those, how many are currently locked (FIFO cadence locked onto a boundary).
    pub n_locked: u32,
    /// A relock / underrun / late-hold / backward-step was observed in the last 60 s
    /// (the widget tracks counter deltas across its 1 Hz samples to compute this).
    pub recent_event: bool,
    /// Wall-clock vs monotonic drift exceeded the allowed bound on any input.
    pub qpc_drift_beyond_bound: bool,
    /// The dantesync `:8898/status` endpoint answered this poll.
    pub clock_present: bool,
    /// `is_locked` from the clock facet.
    pub clock_locked: bool,
    /// `ntp_failed` from the clock facet.
    pub clock_ntp_failed: bool,
    /// A genlock-aware NDI sender is active on this box (absent on a pure receiver like imag).
    pub output_present: bool,
    /// …and it is currently stamping real wall-clock timecodes.
    pub output_stamping: bool,
}

/// Decide the genlock lock state and its dominant reason from the scalarised facets.
///
/// UNLOCKED precedence: clock (absent/unlocked) > output (present but not stamping) >
/// no-input-locked. DEGRADED precedence (only once none of the UNLOCKED conditions hold):
/// some-input-unlocked > recent-event > ntp-failed > qpc-drift. Otherwise LOCKED.
///
/// Mirror of `genlock_decide_lock_state` in `GenlockLockState.hpp` — keep both in lock-step.
pub fn decide(f: &GenlockFacets) -> (LockState, LockReason) {
    // --- UNLOCKED (red): clock > output > no-input-locked -------------------------
    if !f.clock_present || !f.clock_locked {
        return (LockState::Unlocked, LockReason::Clock);
    }
    if f.output_present && !f.output_stamping {
        return (LockState::Unlocked, LockReason::Output);
    }
    if f.n_locked == 0 {
        let reason = if f.n_inputs == 0 {
            LockReason::NoGenlock
        } else {
            LockReason::NoInputLocked
        };
        return (LockState::Unlocked, reason);
    }

    // --- DEGRADED (amber): some-unlocked > recent-event > ntp > qpc ----------------
    if f.n_locked < f.n_inputs {
        return (LockState::Degraded, LockReason::InputUnlocked);
    }
    if f.recent_event {
        return (LockState::Degraded, LockReason::RecentEvent);
    }
    if f.clock_ntp_failed {
        return (LockState::Degraded, LockReason::NtpFailed);
    }
    if f.qpc_drift_beyond_bound {
        return (LockState::Degraded, LockReason::QpcDrift);
    }

    // --- LOCKED (green) -----------------------------------------------------------
    (LockState::Locked, LockReason::None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fully healthy box: clock locked, every input locked, output stamping.
    fn healthy() -> GenlockFacets {
        GenlockFacets {
            n_inputs: 7,
            n_locked: 7,
            recent_event: false,
            qpc_drift_beyond_bound: false,
            clock_present: true,
            clock_locked: true,
            clock_ntp_failed: false,
            output_present: true,
            output_stamping: true,
        }
    }

    #[test]
    fn all_good_is_locked() {
        assert_eq!(decide(&healthy()), (LockState::Locked, LockReason::None));
    }

    #[test]
    fn receiver_box_with_no_output_is_still_locked() {
        // imag: a pure receiver, no genlock NDI sender — the output facet is ABSENT and
        // must NOT force UNLOCKED.
        let mut f = healthy();
        f.output_present = false;
        f.output_stamping = false;
        assert_eq!(decide(&f), (LockState::Locked, LockReason::None));
    }

    #[test]
    fn clock_absent_is_unlocked_clock() {
        let mut f = healthy();
        f.clock_present = false;
        f.clock_locked = false;
        assert_eq!(decide(&f), (LockState::Unlocked, LockReason::Clock));
    }

    #[test]
    fn clock_present_but_not_locked_is_unlocked_clock() {
        let mut f = healthy();
        f.clock_locked = false;
        assert_eq!(decide(&f), (LockState::Unlocked, LockReason::Clock));
    }

    #[test]
    fn output_present_not_stamping_is_unlocked_output() {
        let mut f = healthy();
        f.output_stamping = false;
        assert_eq!(decide(&f), (LockState::Unlocked, LockReason::Output));
    }

    #[test]
    fn inputs_exist_none_locked_is_unlocked_no_input() {
        let mut f = healthy();
        f.n_locked = 0;
        assert_eq!(decide(&f), (LockState::Unlocked, LockReason::NoInputLocked));
    }

    #[test]
    fn no_inputs_at_all_is_unlocked_no_genlock() {
        let mut f = healthy();
        f.n_inputs = 0;
        f.n_locked = 0;
        assert_eq!(decide(&f), (LockState::Unlocked, LockReason::NoGenlock));
    }

    #[test]
    fn some_input_unlocked_is_degraded_input() {
        let mut f = healthy();
        f.n_locked = 5; // 5 of 7
        assert_eq!(decide(&f), (LockState::Degraded, LockReason::InputUnlocked));
    }

    #[test]
    fn recent_event_is_degraded_recent() {
        let mut f = healthy();
        f.recent_event = true;
        assert_eq!(decide(&f), (LockState::Degraded, LockReason::RecentEvent));
    }

    #[test]
    fn ntp_failed_is_degraded_ntp() {
        let mut f = healthy();
        f.clock_ntp_failed = true;
        assert_eq!(decide(&f), (LockState::Degraded, LockReason::NtpFailed));
    }

    #[test]
    fn qpc_drift_is_degraded_qpc() {
        let mut f = healthy();
        f.qpc_drift_beyond_bound = true;
        assert_eq!(decide(&f), (LockState::Degraded, LockReason::QpcDrift));
    }

    // ---- precedence ----------------------------------------------------------------

    #[test]
    fn clock_beats_output_and_input() {
        // clock down AND output not stamping AND no input locked -> clock wins.
        let mut f = healthy();
        f.clock_locked = false;
        f.output_stamping = false;
        f.n_locked = 0;
        assert_eq!(decide(&f), (LockState::Unlocked, LockReason::Clock));
    }

    #[test]
    fn output_beats_no_input_locked() {
        let mut f = healthy();
        f.output_stamping = false;
        f.n_locked = 0;
        assert_eq!(decide(&f), (LockState::Unlocked, LockReason::Output));
    }

    #[test]
    fn input_unlocked_beats_recent_event_ntp_and_qpc() {
        let mut f = healthy();
        f.n_locked = 6; // some unlocked
        f.recent_event = true;
        f.clock_ntp_failed = true;
        f.qpc_drift_beyond_bound = true;
        assert_eq!(decide(&f), (LockState::Degraded, LockReason::InputUnlocked));
    }

    #[test]
    fn recent_event_beats_ntp_and_qpc() {
        let mut f = healthy();
        f.recent_event = true;
        f.clock_ntp_failed = true;
        f.qpc_drift_beyond_bound = true;
        assert_eq!(decide(&f), (LockState::Degraded, LockReason::RecentEvent));
    }

    #[test]
    fn ntp_beats_qpc() {
        let mut f = healthy();
        f.clock_ntp_failed = true;
        f.qpc_drift_beyond_bound = true;
        assert_eq!(decide(&f), (LockState::Degraded, LockReason::NtpFailed));
    }

    #[test]
    fn codes_match_the_c_enum_values() {
        assert_eq!(LockState::Unlocked.code(), 0);
        assert_eq!(LockState::Degraded.code(), 1);
        assert_eq!(LockState::Locked.code(), 2);
        assert_eq!(LockReason::None.code(), 0);
        assert_eq!(LockReason::NoGenlock.code(), 1);
        assert_eq!(LockReason::Clock.code(), 2);
        assert_eq!(LockReason::Output.code(), 3);
        assert_eq!(LockReason::NoInputLocked.code(), 4);
        assert_eq!(LockReason::InputUnlocked.code(), 5);
        assert_eq!(LockReason::RecentEvent.code(), 6);
        assert_eq!(LockReason::NtpFailed.code(), 7);
        assert_eq!(LockReason::QpcDrift.code(), 8);
    }
}
