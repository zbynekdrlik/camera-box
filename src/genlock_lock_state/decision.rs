//! #1298 — the LOCKED / DEGRADED / UNLOCKED state decision: [`LockState`], [`LockReason`], the
//! scalarised [`GenlockFacets`] the widget fills, and [`decide`]. The C port is
//! `genlock_decide_lock_state` in `vendor/obs-studio/frontend/widgets/GenlockLockState.hpp`,
//! parity-gated by `tests/genlock_lock_state_parity.rs`. Split out of `genlock_lock_state.rs`
//! (issue 1302); every item is re-exported at `crate::genlock_lock_state`.

use super::media_clock::MediaClock;

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
    /// NTP phase failed, the wall clock stepped by more than one frame (#1357), or the media
    /// (audio) clock does not follow the disciplined wall clock (issue 1372 part D).
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
    /// The wall clock STEPPED by more than one frame against the monotonic (QPC) timebase (#1357:
    /// the step only — a steady wall-vs-monotonic rate is never a fault, on any box).
    QpcDrift = 8,
    /// #1303 — an audio-enabled genlock source's audio is not paired with its video FIFO hold
    /// (residual A/V pairing offset beyond one frame). A DEGRADED reason below the video ones.
    AudioPairing = 9,
    /// #1303 — a genlock source is AUDIBLE (`ndi_audio=true`) when it is silent-by-contract per the
    /// certified per-box audio table (a camera on any box; a Dante-fed box's every NDI input) — the
    /// double-audio hazard. The lowest-precedence DEGRADED reason, below `AudioPairing`.
    AudioUnexpected = 10,
    /// Issue 1372 part D — OBS's MEDIA clock (`os_gettime_ns`, which paces the audio mixer and every
    /// output) does not tick with the dantesync-disciplined wall clock: the wall-vs-media offset grew
    /// beyond [`GENLOCK_MEDIA_CLOCK_DRIFT_BOUND_US`] over [`GENLOCK_MEDIA_CLOCK_WINDOW_S`], or (Windows)
    /// the disciplined clock fell back to raw QPC while dantesync runs. Below `QpcDrift`, above the
    /// audio-pairing axes; never UNLOCKED on its own.
    ///
    /// [`GENLOCK_MEDIA_CLOCK_DRIFT_BOUND_US`]: super::GENLOCK_MEDIA_CLOCK_DRIFT_BOUND_US
    /// [`GENLOCK_MEDIA_CLOCK_WINDOW_S`]: super::GENLOCK_MEDIA_CLOCK_WINDOW_S
    MediaClock = 11,
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
    /// #1341 — of `n_inputs`, how many are CONNECTED (a live NDI receiver) but IDLE: their
    /// received-frame DELTA over the widget's 60 s window is below `GENLOCK_IDLE_INPUT_MIN_FRAMES`
    /// (a keep-alive-only SongPlayer playlist input sends ~1 frame / 11 s; a live source at
    /// ≥ 23.98 fps delivers ≥ 1400). An idle input's FIFO churns relocks/late-holds every time a
    /// keep-alive frame re-acquires a boundary, which would falsely feed `recent_event` and blame it
    /// as unlocked — so it is excluded from `n_locked` by the widget scan, contributes 0 phase
    /// events ([`InputEventCounts::idle`]), and drops out of the DEGRADED-gate denominator:
    /// `n_connected = n_inputs - n_absent - n_idle` (saturating, the `n_absent` shape). A box whose
    /// inputs are ALL idle/absent stays HEALTHY-idle LOCKED. Additive: an all-zero `n_idle`
    /// reproduces every pre-#1341 verdict exactly.
    ///
    /// [`InputEventCounts::idle`]: super::InputEventCounts::idle
    pub n_idle: u32,
    /// A relock / late-hold / backward-step of a connected, non-idle input was observed in the last
    /// 60 s. Issue 1302: the widget counts each input's NEW events against its own baseline
    /// ([`input_new_phase_events`]), so a reattaching input never re-counts its lifetime total.
    ///
    /// [`input_new_phase_events`]: super::input_new_phase_events
    pub recent_event: bool,
    /// The wall clock stepped by more than one frame (the step-only `qpc_drift_beyond_bound`).
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
    /// the wall-step verdict for [`GenlockFacets::qpc_drift_beyond_bound`] — surfacing
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
    /// Issue 1372 part D — the media-clock verdict ([`media_clock_verdict`]) the widget reduced this
    /// tick. Anything but [`MediaClock::Ok`] DEGRADES (reason [`LockReason::MediaClock`]); it never
    /// makes the box UNLOCKED on its own. `Ok` reproduces every earlier verdict exactly.
    ///
    /// [`media_clock_verdict`]: super::media_clock_verdict
    pub media_clock: MediaClock,
}

/// Decide the genlock lock state and its dominant reason from the scalarised facets.
///
/// UNLOCKED precedence: clock (absent/unlocked) > output (present but not stamping) >
/// no-input-locked. DEGRADED precedence (only once none of the UNLOCKED conditions hold):
/// some-input-unlocked > recent-event > ntp-failed > qpc-drift > media-clock > audio-pairing >
/// audio-unexpected. Otherwise LOCKED.
///
/// #1341 — the DEGRADED/no-input decisions judge only CONNECTED-non-idle inputs (`n_connected =
/// n_inputs - n_absent - n_idle`): a senderless (`n_absent`) OR a keep-alive-only idle (`n_idle`)
/// input is excluded, so neither trips a page; a box whose inputs are ALL absent/idle is HEALTHY-idle.
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
    // a senderless input (`n_absent`) is idle, not a fault. #1341 — a CONNECTED-but-IDLE input
    // (`n_idle`, keep-alive-only) is ALSO excluded: its FIFO churns relocks on each keep-alive frame
    // but it is not a live locking target. `saturating_sub` keeps the decision total even under a
    // transient `n_absent + n_idle > n_inputs`.
    let n_connected = f
        .n_inputs
        .saturating_sub(f.n_absent)
        .saturating_sub(f.n_idle);

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
    // Issue 1372 part D — the media (audio) clock does not follow the disciplined wall clock. A
    // clock-class cause, so it outranks the audio-pairing symptoms below; DEGRADED only.
    if f.media_clock != MediaClock::Ok {
        return (LockState::Degraded, LockReason::MediaClock);
    }
    // #1303 — audio parity is the LOWEST-precedence DEGRADED axis: only an audio-enabled genlock
    // source with a material pairing-offset breach trips it (audio disabled/absent never sets the
    // facet). Additive: an all-false `audio_unpaired` leaves every pre-#1303 verdict unchanged.
    if f.audio_unpaired {
        return (LockState::Degraded, LockReason::AudioPairing);
    }
    // #1303 — the lowest-precedence DEGRADED axis: a source audible when the certified per-box
    // audio table expects it silent (a camera anywhere; the double-audio hazard). Additive: an
    // all-false `audio_unexpected` leaves every pre-this-change verdict unchanged.
    if f.audio_unexpected {
        return (LockState::Degraded, LockReason::AudioUnexpected);
    }

    // --- LOCKED (green) -----------------------------------------------------------
    (LockState::Locked, LockReason::None)
}

#[cfg(test)]
mod tests {
    use super::super::{qpc_drift_beyond_bound, GENLOCK_QPC_STEP_BOUND_MS};
    use super::*;

    /// A fully healthy box: clock locked, every input locked, output stamping.
    fn healthy() -> GenlockFacets {
        GenlockFacets {
            n_inputs: 7,
            n_locked: 7,
            n_absent: 0,
            n_idle: 0,
            recent_event: false,
            qpc_drift_beyond_bound: false,
            clock_present: true,
            clock_locked: true,
            clock_ntp_failed: false,
            output_present: true,
            output_stamping: true,
            audio_unpaired: false,
            audio_unexpected: false,
            media_clock: MediaClock::Ok,
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

    // ---- #1341: a connected-but-IDLE input never grades the box --------------------

    #[test]
    fn idle_sender_only_unlocked_is_still_locked() {
        // The cg-OBS scenario: 12 inputs, 2 live+locked, 10 idle SongPlayer keep-alive inputs. The
        // idle ones are excluded from n_connected AND n_locked, so 2/2 CONNECTED-non-idle are locked
        // -> LOCKED, never the chronic DEGRADED/recent_event the idle relock churn produced (#1341).
        let mut f = healthy();
        f.n_inputs = 12;
        f.n_locked = 2;
        f.n_idle = 10; // n_connected = 12 - 0 - 10 = 2, n_locked = 2
        assert_eq!(decide(&f), (LockState::Locked, LockReason::None));
    }

    #[test]
    fn all_idle_is_healthy_idle_locked() {
        // Every input present but idle (keep-alive only): HEALTHY-idle, NOT UNLOCKED — nothing live
        // to lock onto, exactly like the all-absent arm.
        let mut f = healthy();
        f.n_inputs = 4;
        f.n_locked = 0;
        f.n_idle = 4; // n_connected = 0 -> HEALTHY-idle
        assert_eq!(decide(&f), (LockState::Locked, LockReason::None));
    }

    #[test]
    fn idle_plus_a_connected_live_unlocked_still_degrades() {
        // 12 inputs: 10 idle, 2 live of which only 1 is locked -> a LIVE input is genuinely unlocked
        // -> DEGRADED. Idle inputs are excluded, but a real fault on a live input still pages.
        let mut f = healthy();
        f.n_inputs = 12;
        f.n_locked = 1;
        f.n_idle = 10; // n_connected = 2, n_locked = 1 < 2
        assert_eq!(decide(&f), (LockState::Degraded, LockReason::InputUnlocked));
    }

    #[test]
    fn n_idle_and_n_absent_together_saturate_n_connected() {
        // n_absent + n_idle > n_inputs (a transient over-count) saturates n_connected to 0 -> the
        // decision stays total and reads HEALTHY-idle, never a panic or a wrapped huge denominator.
        let mut f = healthy();
        f.n_inputs = 3;
        f.n_locked = 0;
        f.n_absent = 2;
        f.n_idle = 3; // 3 - 2 - 3 saturates to 0
        assert_eq!(decide(&f), (LockState::Locked, LockReason::None));
    }

    #[test]
    fn idle_never_leaves_unlocked_on_clock_down() {
        // n_idle must never flip an UNLOCKED clock verdict — clock precedence still wins.
        let mut f = healthy();
        f.n_idle = 5;
        f.clock_locked = false;
        assert_eq!(decide(&f), (LockState::Unlocked, LockReason::Clock));
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
        assert_eq!(
            decide(&f),
            (LockState::Degraded, LockReason::AudioUnexpected)
        );
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
        assert_eq!(LockReason::MediaClock.code(), 11);
    }

    // ---- #1299 Part 4 / #1357 scope C: the qpc step verdict feeds the decision ------------

    #[test]
    fn windowed_verdict_feeds_the_three_state_decision() {
        // The verdict's bool is what `decide` consumes for the qpc term.
        let v = qpc_drift_beyond_bound(false, 0, 0, 40, GENLOCK_QPC_STEP_BOUND_MS);
        let mut f = healthy();
        f.qpc_drift_beyond_bound = v.beyond_bound;
        assert_eq!(decide(&f), (LockState::Degraded, LockReason::QpcDrift));
    }

    // --- Issue 1372 part D: the media-clock (audio clock) term ---------------------------------

    #[test]
    fn media_clock_drift_is_degraded_media_clock() {
        let mut f = healthy();
        f.media_clock = MediaClock::Drift;
        assert_eq!(decide(&f), (LockState::Degraded, LockReason::MediaClock));
        f.media_clock = MediaClock::Undisciplined;
        assert_eq!(decide(&f), (LockState::Degraded, LockReason::MediaClock));
    }

    #[test]
    fn media_clock_never_leaves_unlocked_and_never_masks_an_unlock() {
        // The term only DEGRADES: a clock-down box stays UNLOCKED/clock, a no-input box stays
        // UNLOCKED/no_input_locked, whatever the media clock says.
        for mc in [MediaClock::Drift, MediaClock::Undisciplined] {
            let mut f = healthy();
            f.media_clock = mc;
            f.clock_present = false;
            assert_eq!(decide(&f), (LockState::Unlocked, LockReason::Clock));
            let mut f = healthy();
            f.media_clock = mc;
            f.n_locked = 0;
            assert_eq!(decide(&f), (LockState::Unlocked, LockReason::NoInputLocked));
        }
    }

    #[test]
    fn qpc_step_beats_media_clock_and_media_clock_beats_audio() {
        let mut f = healthy();
        f.media_clock = MediaClock::Drift;
        f.qpc_drift_beyond_bound = true;
        assert_eq!(decide(&f), (LockState::Degraded, LockReason::QpcDrift));
        let mut f = healthy();
        f.media_clock = MediaClock::Drift;
        f.audio_unpaired = true;
        f.audio_unexpected = true;
        assert_eq!(decide(&f), (LockState::Degraded, LockReason::MediaClock));
    }
}
