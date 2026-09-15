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
    /// #1303 — an audio-enabled genlock source's audio is not paired with its video FIFO hold
    /// (residual A/V pairing offset beyond one frame). A DEGRADED reason below the video ones.
    AudioPairing = 9,
    /// #1303 — a genlock source is AUDIBLE (`ndi_audio=true`) when it is silent-by-contract per the
    /// certified per-box audio table (a camera on any box; a Dante-fed box's every NDI input) — the
    /// double-audio hazard. The lowest-precedence DEGRADED reason, below `AudioPairing`.
    AudioUnexpected = 10,
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
    /// #1299 — of `n_inputs`, how many have NO live NDI receiver connection (their sender is not
    /// running): `obs_genlock_stats.connected == false`. `n_connected = n_inputs - n_absent` is the
    /// denominator the DEGRADED gate uses, so a legitimately-idle NDI input never trips a page — a
    /// dead/frozen sender is the #1001/#1052 watchdogs' concern, not the lock decision's. Additive:
    /// an all-zero `n_absent` reproduces every pre-#1299 verdict exactly (`n_connected == n_inputs`).
    pub n_absent: u32,
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
    /// #1303 — an audio-ENABLED genlock source's audio is NOT paired with its video FIFO hold:
    /// the residual `|pairing_offset_ms|` breaches the bound (which also captures a deep-latency
    /// source whose hold never fired — its offset is `-latency_ms`). Audio disabled/absent never
    /// sets this (a camera input with `ndi_audio=false` is silent by design), so it can only
    /// DEGRADE, never take a healthy box off LOCKED spuriously. The widget aggregates the
    /// per-source audio-parity condition into this one scalar — the same reduction it applies to
    /// per-input wall-vs-monotonic drift for [`GenlockFacets::qpc_drift_beyond_bound`] — surfacing
    /// the pairing-offset branch of [`crate::genlock_audio_pairing::decide_audio_health`].
    pub audio_unpaired: bool,
    /// #1303 — a genlock source is AUDIBLE when the certified per-box audio table
    /// (`crate::genlock_forced_table_audit`) expects it SILENT: a camera input on ANY box, or (once
    /// box identity is wired) any NDI input on a Dante-fed box. NDI audio there is the double-audio
    /// hazard (owner ruling 2026-09-15). The widget reduces the per-source
    /// `audio_enabled && is-silent-by-contract` condition into this one scalar (the twin of
    /// [`GenlockFacets::audio_unpaired`]); audio disabled/absent never sets it, so it can only
    /// DEGRADE, never take a healthy box off LOCKED spuriously.
    pub audio_unexpected: bool,
}

/// Decide the genlock lock state and its dominant reason from the scalarised facets.
///
/// UNLOCKED precedence: clock (absent/unlocked) > output (present but not stamping) >
/// no-input-locked. DEGRADED precedence (only once none of the UNLOCKED conditions hold):
/// some-input-unlocked > recent-event > ntp-failed > qpc-drift > audio-pairing. Otherwise LOCKED.
///
/// #1299 — the DEGRADED/no-input decisions judge only CONNECTED inputs (`n_connected = n_inputs -
/// n_absent`): an input whose NDI sender is not running (`n_absent`) is idle, not a fault, so it
/// never DEGRADES the box, and a box whose inputs are ALL senderless (`n_connected == 0` with
/// `n_inputs > 0`) is HEALTHY-idle (LOCKED), not UNLOCKED — a dead sender is the #1001/#1052
/// watchdogs' concern. `n_inputs == 0` (no genlock configured at all) stays UNLOCKED/NoGenlock.
///
/// Mirror of `genlock_decide_lock_state` in `GenlockLockState.hpp` — keep both in lock-step.
pub fn decide(f: &GenlockFacets) -> (LockState, LockReason) {
    // #1299 — CONNECTED inputs (a live NDI receiver) are the only ones the lock decision judges;
    // a senderless input (`n_absent`) is idle, not a fault. `saturating_sub` keeps the decision
    // total even under a transient `n_absent > n_inputs`.
    let n_connected = f.n_inputs.saturating_sub(f.n_absent);

    // --- UNLOCKED (red): clock > output > no-input-locked -------------------------
    if !f.clock_present || !f.clock_locked {
        return (LockState::Unlocked, LockReason::Clock);
    }
    if f.output_present && !f.output_stamping {
        return (LockState::Unlocked, LockReason::Output);
    }
    if f.n_locked == 0 {
        if f.n_inputs == 0 {
            // No genlock inputs configured at all — a real misconfiguration.
            return (LockState::Unlocked, LockReason::NoGenlock);
        }
        if n_connected == 0 {
            // #1299 — inputs exist but EVERY sender is absent: the genlock subsystem is healthy with
            // nothing to lock onto (HEALTHY-idle). Not this watchdog's alarm — a box with all senders
            // gone is the #1001 (reachability) / #1052 (frozen-input) watchdogs' concern. So LOCKED,
            // never UNLOCKED (which would false-page); the 3-state enum has no UNKNOWN to emit, and
            // UNKNOWN is a watchdog facet-absence concept anyway, not a lock state.
            return (LockState::Locked, LockReason::None);
        }
        // Connected senders present but none locking — a genuine fault.
        return (LockState::Unlocked, LockReason::NoInputLocked);
    }

    // --- DEGRADED (amber): some-unlocked > recent-event > ntp > qpc ----------------
    // #1299 — gate on n_connected (not n_inputs): an absent sender is excluded so it never DEGRADES.
    if f.n_locked < n_connected {
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
    // #1303 — audio parity is the LOWEST-precedence DEGRADED axis: only an audio-enabled genlock
    // source with a material pairing-offset breach trips it (audio disabled/absent never sets the
    // facet). Additive: an all-false `audio_unpaired` leaves every pre-#1303 verdict unchanged.
    if f.audio_unpaired {
        return (LockState::Degraded, LockReason::AudioPairing);
    }

    // --- LOCKED (green) -----------------------------------------------------------
    (LockState::Locked, LockReason::None)
}

/// #1299 Part 3 — one genlock input's cumulative event counters as the recent-event aggregation
/// reads them. Plain scalars so the C mirror (`GenlockLockState.hpp`, `genlock_input_phase_events`)
/// ports byte-for-byte; the committed parity gate `tests/genlock_lock_state_parity.rs` keeps the two
/// numerically identical.
#[derive(Debug, Clone, Copy)]
pub struct InputEventCounts {
    /// The DistroAV receiver has a live NDI connection (sender running). A disconnected input
    /// contributes ZERO — its #1096 fresh-finder rebind churn is not a lock event.
    pub connected: bool,
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
    if !c.connected {
        return 0;
    }
    c.relocks
        .saturating_add(c.late_holds)
        .saturating_add(c.backward_steps)
}

/// #1299 Part 3 — the aggregate phase-event counter (summed over CONNECTED inputs) whose INCREASE
/// across the widget's 1 Hz samples sets `recent_event`. Absent + underrun contributions are
/// excluded per [`input_phase_events`], so the existing 60 s recency window ages out normally
/// instead of latching on a continuously-incrementing underrun / absent-sender rebind driver.
pub fn connected_phase_event_sum(inputs: &[InputEventCounts]) -> u64 {
    inputs
        .iter()
        .map(input_phase_events)
        .fold(0u64, |a, e| a.saturating_add(e))
}

/// #1299 Part 3 (b) — the top recent-event offender: the index of the CONNECTED input carrying the
/// most phase events, and that count. `None` when no connected input carries any phase event (so the
/// DEGRADED reason is never enriched with a spurious `:<name>` and the JSON `recent_event_inputs`
/// list stays empty). Ties resolve to the FIRST (lowest index) in scan order — deterministic,
/// mirroring the widget's `unlocked_names.front()` selection. The widget maps the index back to the
/// input's name for `reason=recent_event:<name>`.
pub fn top_phase_event_offender(inputs: &[InputEventCounts]) -> Option<(usize, u64)> {
    let mut best: Option<(usize, u64)> = None;
    for (i, c) in inputs.iter().enumerate() {
        let e = input_phase_events(c);
        if e == 0 {
            continue;
        }
        match best {
            Some((_, be)) if be >= e => {}
            _ => best = Some((i, e)),
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fully healthy box: clock locked, every input locked, output stamping.
    fn healthy() -> GenlockFacets {
        GenlockFacets {
            n_inputs: 7,
            n_locked: 7,
            n_absent: 0,
            recent_event: false,
            qpc_drift_beyond_bound: false,
            clock_present: true,
            clock_locked: true,
            clock_ntp_failed: false,
            output_present: true,
            output_stamping: true,
            audio_unpaired: false,
            audio_unexpected: false,
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

    // ---- #1299: an absent-sender input never grades the box ------------------------

    #[test]
    fn absent_sender_only_unlocked_is_still_locked() {
        // The reopen scenario: 4 genlock inputs, 3 connected+locked, the 4th has NO sender
        // (n_absent=1). n_connected=3, n_locked=3 -> nothing CONNECTED is unlocked -> LOCKED,
        // never a false DEGRADED/input_unlocked page (stream's 'NDIA cg stream', #1299).
        let mut f = healthy();
        f.n_inputs = 4;
        f.n_locked = 3;
        f.n_absent = 1;
        assert_eq!(decide(&f), (LockState::Locked, LockReason::None));
    }

    #[test]
    fn all_senders_absent_is_healthy_idle_locked() {
        // Every genlock input present but senderless: HEALTHY-idle (nothing to lock onto), NOT
        // UNLOCKED. A box with all senders gone is #1001/#1052's alarm, not the lock decision's.
        let mut f = healthy();
        f.n_inputs = 4;
        f.n_locked = 0;
        f.n_absent = 4;
        assert_eq!(decide(&f), (LockState::Locked, LockReason::None));
    }

    #[test]
    fn absent_plus_a_connected_unlocked_still_degrades() {
        // 4 inputs: 1 absent, 3 connected of which only 2 are locked -> a CONNECTED input is
        // genuinely unlocked -> DEGRADED. The absent one is excluded, but a real fault still pages.
        let mut f = healthy();
        f.n_inputs = 4;
        f.n_locked = 2;
        f.n_absent = 1; // n_connected=3, n_locked=2 < 3
        assert_eq!(decide(&f), (LockState::Degraded, LockReason::InputUnlocked));
    }

    #[test]
    fn no_genlock_inputs_at_all_stays_unlocked_no_genlock() {
        // n_inputs==0 (no genlock configured) is a real misconfiguration, UNLOCKED — distinct from
        // "inputs present but all senderless" (HEALTHY-idle above).
        let mut f = healthy();
        f.n_inputs = 0;
        f.n_locked = 0;
        f.n_absent = 0;
        assert_eq!(decide(&f), (LockState::Unlocked, LockReason::NoGenlock));
    }

    #[test]
    fn connected_senders_none_locking_is_unlocked_no_input() {
        // Live senders present (n_connected>0) but none locking -> a genuine fault, UNLOCKED.
        let mut f = healthy();
        f.n_inputs = 3;
        f.n_locked = 0;
        f.n_absent = 1; // n_connected=2 > 0, none locked
        assert_eq!(decide(&f), (LockState::Unlocked, LockReason::NoInputLocked));
    }

    #[test]
    fn absent_never_leaves_unlocked_on_clock_down() {
        // #1299: n_absent is a DEGRADE-suppressor on the input axis only; it never rescues a box
        // whose clock is down (clock precedence wins).
        let mut f = healthy();
        f.n_inputs = 4;
        f.n_locked = 0;
        f.n_absent = 4;
        f.clock_locked = false;
        assert_eq!(decide(&f), (LockState::Unlocked, LockReason::Clock));
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

    #[test]
    fn audio_unpaired_is_degraded_audio() {
        // #1303: an audio-enabled genlock source mispaired with its video FIFO hold degrades an
        // otherwise-LOCKED box.
        let mut f = healthy();
        f.audio_unpaired = true;
        assert_eq!(decide(&f), (LockState::Degraded, LockReason::AudioPairing));
    }

    #[test]
    fn audio_unexpected_is_degraded_audio() {
        // #1303: an audio-enabled source that is silent-by-contract per the certified table
        // (the widget reduces it into audio_unexpected) degrades an otherwise-LOCKED box.
        let mut f = healthy();
        f.audio_unexpected = true;
        assert_eq!(decide(&f), (LockState::Degraded, LockReason::AudioUnexpected));
    }

    #[test]
    fn audio_unexpected_never_leaves_unlocked() {
        // #1303: audio-unexpected is a DEGRADE-only axis — it never rescues an UNLOCKED box.
        let mut f = healthy();
        f.audio_unexpected = true;
        f.clock_locked = false;
        assert_eq!(decide(&f), (LockState::Unlocked, LockReason::Clock));
    }

    #[test]
    fn audio_pairing_beats_audio_unexpected() {
        // #1303: audio_unexpected is the lowest-precedence DEGRADED reason — even audio_pairing
        // (itself the previous lowest) wins over it.
        let mut f = healthy();
        f.audio_unpaired = true;
        f.audio_unexpected = true;
        assert_eq!(decide(&f), (LockState::Degraded, LockReason::AudioPairing));
    }

    #[test]
    fn audio_unpaired_never_leaves_unlocked() {
        // #1303: audio is a DEGRADE-only axis — it never rescues an UNLOCKED box (clock down),
        // and never fires while an input is still unlocked (that reason takes precedence).
        let mut f = healthy();
        f.audio_unpaired = true;
        f.clock_locked = false;
        assert_eq!(decide(&f), (LockState::Unlocked, LockReason::Clock));
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
    fn qpc_beats_audio_pairing() {
        // #1303: audio parity is the lowest-precedence DEGRADED reason — every existing DEGRADED
        // reason (here qpc) wins over it.
        let mut f = healthy();
        f.qpc_drift_beyond_bound = true;
        f.audio_unpaired = true;
        assert_eq!(decide(&f), (LockState::Degraded, LockReason::QpcDrift));
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
        assert_eq!(LockReason::AudioPairing.code(), 9);
        assert_eq!(LockReason::AudioUnexpected.code(), 10);
    }

    // --- #1299 Part 3: the connected-phase-only recent-event feed + offender attribution ----------

    fn ev(connected: bool, relocks: u64, late_holds: u64, backward_steps: u64) -> InputEventCounts {
        InputEventCounts {
            connected,
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
    fn connected_sum_excludes_absent_inputs() {
        let inputs = [ev(true, 1, 0, 0), ev(false, 500, 0, 0), ev(true, 0, 2, 0)];
        assert_eq!(connected_phase_event_sum(&inputs), 3);
    }

    #[test]
    fn offender_is_the_top_connected_phase_input() {
        // cg (index 1) has the most phase events among CONNECTED inputs; the absent input at index 2
        // has more raw counters but is excluded.
        let inputs = [ev(true, 1, 0, 0), ev(true, 20, 5, 0), ev(false, 9999, 0, 0)];
        assert_eq!(top_phase_event_offender(&inputs), Some((1, 25)));
    }

    #[test]
    fn no_offender_when_no_connected_phase_event() {
        // Only an absent input carries counters -> no connected phase event -> None (the reason is
        // never enriched with a spurious :<name>, the JSON list stays empty).
        let inputs = [ev(true, 0, 0, 0), ev(false, 50, 0, 0)];
        assert_eq!(top_phase_event_offender(&inputs), None);
    }

    #[test]
    fn offender_ties_resolve_to_the_first_in_scan_order() {
        let inputs = [ev(true, 3, 0, 0), ev(true, 3, 0, 0)];
        assert_eq!(top_phase_event_offender(&inputs), Some((0, 3)));
    }

    #[test]
    fn saturating_never_overflows_on_a_pathological_count() {
        assert_eq!(input_phase_events(&ev(true, u64::MAX, 5, 0)), u64::MAX);
    }
}
