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
    pub n_idle: u32,
    /// A relock / underrun / late-hold / backward-step was observed in the last 60 s
    /// (the widget tracks counter deltas across its 1 Hz samples to compute this).
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

/// #1299 Part 3 — one genlock input's cumulative event counters as the recent-event aggregation
/// reads them. Plain scalars so the C mirror (`GenlockLockState.hpp`, `genlock_input_phase_events`)
/// ports byte-for-byte; the committed parity gate `tests/genlock_lock_state_parity.rs` keeps the two
/// numerically identical.
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

// #1299 Part 4 + #1357 scope C — the wall-vs-monotonic `qpc_drift` term. The CUMULATIVE offset must
// never gate: on a dantesync-disciplined Windows box the wall ran at the grandmaster rate vs the free
// QPC crystal and the offset grew ~50 ms/h (38 false pages overnight 15./16.9.2026; issue 1372 has
// since disciplined the Windows `os_gettime_ns()`). The RATE must not
// gate either (#1357): on Linux `CLOCK_MONOTONIC` is kernel-disciplined together with `CLOCK_REALTIME`,
// so the measured wall-vs-monotonic rate is 0 by construction, while on Windows it was the free crystal —
// a rate check therefore meant a different thing on every box, and comparing a windowed rate with one
// instantaneous dantesync `f_ptp + f_phase` sample false-DEGRADED both (28 samples on strih-lx, 4 on
// stream, 24.9.2026, none a step). A rate is also no genlock hazard: the render tick re-derives every
// deadline from the wall clock and absorbs up to 2 ms per tick. The one clock hazard for genlock — the
// same on every box — is a wall STEP: it moves every wall-keyed FIFO release / ts-align deadline by more
// than a frame at once (the render tick itself only slews through it). The windowed rate stays
// report-only telemetry. (Since issue 1372 the Windows `os_gettime_ns()` also runs at the
// dantesync-disciplined rate, so the measured rate is ~0 on every box; the step verdict is unchanged.)
/// A single-sample wall STEP beyond this (ms) DEGRADES immediately — one 30 fps frame, the coarsest
/// fleet frame interval (same value as the audio-pairing bound), so a sub-frame wobble never trips.
pub const GENLOCK_QPC_STEP_BOUND_MS: i64 = 33;
/// The rolling window (s) the widget measures the report-only drift RATE telemetry over. 300 s so the
/// integer-ms cumulative drift resolves the rate: at 14 ppm the window accrues ≈ 4.2 ms.
pub const GENLOCK_QPC_WINDOW_S: i64 = 300;

/// #1299 Part 4 — the wall-vs-QPC drift verdict plus the measured rate it read (for telemetry).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QpcDriftVerdict {
    /// Whether the wall clock STEPPED by more than one frame (#1357: the step is the whole verdict).
    pub beyond_bound: bool,
    /// The windowed drift rate in ppm (0.0 until the rate window has filled).
    pub measured_ppm: f64,
}

/// #1299 Part 4 — the windowed drift RATE in ppm from an integer-ms cumulative-drift delta over an
/// integer-ms elapsed span. `delta_ms / elapsed_ms` is dimensionless; × 1e6 is ppm. 0.0 for a
/// non-positive span (not-ready / degenerate). Byte-for-byte the arithmetic the C mirror
/// `genlock_qpc_drift_beyond_bound` performs internally, kept as a named Tier-0-tested helper so the
/// JSON `qpc_drift_ppm` telemetry and the parity harness share one formula.
pub fn qpc_window_rate_ppm(drift_delta_ms: i64, elapsed_ms: i64) -> f64 {
    if elapsed_ms <= 0 {
        return 0.0;
    }
    drift_delta_ms as f64 / elapsed_ms as f64 * 1_000_000.0
}

/// #1299 Part 4 + #1357 scope C — decide whether the wall clock STEPPED against the monotonic sleep
/// timebase, and report the measured windowed rate as telemetry. DEGRADED only when a single-sample
/// STEP exceeds `step_bound_ms` (judged as soon as two samples exist, whether or not the rate window
/// has filled). The rate (`measured_ppm`, reported once `rate_ready`) never feeds the verdict — it
/// means a different thing on Linux (disciplined monotonic, 0 by construction) and on Windows (free
/// QPC crystal), and it is no genlock hazard. One semantics on every box.
///
/// Byte-for-byte mirror of `genlock_qpc_drift_beyond_bound` in `GenlockLockState.hpp` — the committed
/// parity gate `tests/genlock_lock_state_parity.rs` keeps the two numerically identical over a spread
/// of int vectors (verdict + measured rate).
pub fn qpc_drift_beyond_bound(
    rate_ready: bool,
    drift_delta_ms: i64,
    elapsed_ms: i64,
    max_step_ms: i64,
    step_bound_ms: i64,
) -> QpcDriftVerdict {
    let measured_ppm = if rate_ready {
        qpc_window_rate_ppm(drift_delta_ms, elapsed_ms)
    } else {
        0.0
    };
    // A STEP is the one clock hazard for genlock, judged even before the rate window fills.
    let beyond_bound = max_step_ms.saturating_abs() > step_bound_ms;
    QpcDriftVerdict {
        beyond_bound,
        measured_ppm,
    }
}

/// Issue 1372 — the largest single-sample wall jump (ms) the widget BOOKS as a coordinated
/// dantesync fleet DATE step: two 30 fps frames. dantesync 1.9.0 steps the fleet date when its error
/// passes 50 ms, so a date step lands at ~50 ms plus the drift of one poll (the live one was
/// 51.039 ms); 66 ms keeps that with margin and nothing more. A bigger jump (a clock SET, an NTP
/// fallback step, another clock writer) stays in the history and DEGRADES — the hazard the step
/// verdict exists for (#1357).
pub const GENLOCK_QPC_WALL_STEP_BOOK_MAX_MS: i64 = 66;
/// Issue 1372 — how many wall steps the widget books inside one [`GENLOCK_QPC_WINDOW_S`]. A second
/// step in the window is a step STORM (dantesync steps the date every ~1.8 h) and keeps DEGRADING.
pub const GENLOCK_QPC_WALL_STEPS_PER_WINDOW: i64 = 1;

/// Issue 1372 — whether a single-sample wall jump is a BOOKED date step: returns the jump to
/// re-baseline the widget's qpc history by, or `0` to leave it in the history.
///
/// A coordinated dantesync fleet date step (−51 ms live, 25.9.2026 23:17:07 UTC) moves the wall
/// clock at once while the media clock follows only the dantesync RATE (issue 1372 part A, by
/// design), and the render tick re-grids onto the stepped wall in ONE tick (`crate::genlock_wall_step`).
/// Such a step is no genlock hazard any more, so the widget re-baselines its history by the jump,
/// logs one `genlock-wall-step:` line and stays LOCKED — before issue 1372 it read the step as a
/// `qpc_drift` hazard for the whole 300 s window (resolume DEGRADED from the step on). Booked only
/// when `step_bound_ms < |jump| <= book_max_ms` and fewer than `steps_per_window` steps were booked in
/// the window: a bigger jump and a step STORM stay in the history and still DEGRADE through
/// [`qpc_drift_beyond_bound`]; a jump within the bound never degrades, so it is not booked either.
///
/// Byte-for-byte mirror of `genlock_qpc_wall_step_rebase_ms` in `GenlockLockState.hpp` — the parity
/// gate `tests/genlock_qpc_wall_step_parity_1372.rs` lifts that function.
pub fn qpc_wall_step_rebase_ms(
    jump_ms: i64,
    step_bound_ms: i64,
    book_max_ms: i64,
    booked_in_window: i64,
    steps_per_window: i64,
) -> i64 {
    let mag = jump_ms.saturating_abs();
    if mag > step_bound_ms && mag <= book_max_ms && booked_in_window < steps_per_window {
        jump_ms
    } else {
        0
    }
}

// Issue 1372 part D — the MEDIA-clock (audio clock) term. `os_gettime_ns()` paces OBS's audio mixer,
// its video thread and every output timestamp. Once issue 1372 part A made the Windows
// `os_gettime_ns()` run at the dantesync-disciplined system-time rate (Linux's `CLOCK_MONOTONIC` is
// kernel-disciplined already), the wall-vs-media offset must stay FLAT on every box apart from wall
// steps: 0 ms over 47 min on stream after the part-A deploy, 0 on strih-lx for hours, while the
// undisciplined stream mixer walked 67 ms in 83 min (≈ 8 ms per 10 min) before it — green on the
// indicator the whole time. So the offset's RATE over a window is a real fault signal again, unlike
// the #1357-removed rate term: that one compared the rate with an instantaneous dantesync
// `f_ptp + f_phase` sample, which meant a different thing per box; this one compares with 0, which
// means the same thing everywhere.
//
// The widget samples the offset itself in µs each 1 Hz tick (the libobs `wall_qpc_drift_ms` is integer
// ms truncated toward zero). The centre rate is the MEDIAN of the per-pair rates; a pair whose offset
// change differs from what the centre rate predicts over its interval by more than
// [`GENLOCK_MEDIA_CLOCK_BAND_US`] (100 µs) is a wall STEP and is left out; the rate is the TIME-WEIGHTED
// rate of the kept pairs (Σ change / Σ interval). dantesync requests steps of ≥ 200 µs (server) /
// ≥ 500 µs (client), so they are out whatever their pattern, as long as steps touch fewer than half the
// pairs. On Windows a step lands up to one timer tick short of the request (dantesync targets the
// coarse `GetSystemTimeAsFileTime`), so some remnants fall inside the band: they are unbiased and
// ≤ 100 µs each, well under 1 ms per window. A drift present in only part of the seconds moves a ~1 s
// pair by its rate in µs, so up to ~95 ppm (with ±50 ms tick jitter) it is kept and counted at its
// true share (a pure median would drop it below half coverage). A pair more than [`GENLOCK_MEDIA_CLOCK_MAX_GAP_MS`] apart (a stalled UI) is not
// a sample, and the window counts as ready only once the counted pairs cover ≥ 90 % of it. The second input is the
// Windows discipline state libobs publishes (`os_gettime_discipline()`): a fallback to raw QPC while
// dantesync answers is DEGRADED at once. The term never makes a box UNLOCKED on its own.

/// The window (s) the media-clock rate is measured over — the design's "per 10 min".
pub const GENLOCK_MEDIA_CLOCK_WINDOW_S: i64 = 600;
/// The drift (µs per window) beyond which the media clock DEGRADES: > 2 ms per 10 min (> 3.3 ppm).
/// The undisciplined stream accrued ≈ 8 ms per 10 min; a disciplined box stays at 0.
pub const GENLOCK_MEDIA_CLOCK_DRIFT_BOUND_US: i64 = 2000;
/// A pair of samples further apart than this (ms) — a stalled UI thread — is not a rate sample.
pub const GENLOCK_MEDIA_CLOCK_MAX_GAP_MS: i64 = 5000;
/// A pair whose offset change differs from the centre rate's prediction by more than this (µs) is a
/// wall STEP. The deviation of a pair that spans a step IS the step, whatever the interval; dantesync
/// requests steps of ≥ 200 µs (server) / ≥ 500 µs (client) — a Windows step can land up to one timer
/// tick short, so a small remnant may count (unbiased, ≤ 100 µs each); ±1 µs sampling noise stays far
/// inside. A drift that differs from the centre by R ppm moves a 1 s pair by R µs, so a partial drift up
/// to ~95 ppm is kept with ±50 ms tick jitter (up to 20 ppm on a 5 s pair); a faster one present in
/// fewer than half the seconds reads as steps — a raw-QPC fallback, the fast case, is the discipline
/// outcome's.
pub const GENLOCK_MEDIA_CLOCK_BAND_US: i64 = 100;

/// What the Windows `os_gettime_ns()` last read from the system-time adjustment. Discriminants match
/// libobs `enum os_gettime_discipline_state` (`util/platform.h`) and the C mirror
/// `genlock_media_discipline` in `GenlockLockState.hpp`; `NotApplicable` is the widget's value on a
/// Linux box, whose kernel disciplines `CLOCK_MONOTONIC` together with the wall clock (macOS
/// `os_gettime_ns` is the raw `CLOCK_UPTIME_RAW`; no fleet box runs it, and the drift input still
/// covers it).
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaDiscipline {
    /// Not polled yet (the first ~250 ms after OBS start).
    Unknown = 0,
    /// The adjustment is read and enabled: the clock runs at the disciplined rate.
    Active = 1,
    /// The adjustment is disabled or zero: raw QPC.
    Disabled = 2,
    /// `GetSystemTimeAdjustmentPrecise` returned FALSE: raw QPC.
    ReadFailed = 3,
    /// The API is not exported (old Windows): raw QPC.
    ApiMissing = 4,
    /// Not a Windows box: there is no discipline outcome to read (the drift input still applies).
    NotApplicable = 5,
}

impl MediaDiscipline {
    /// The integer libobs and the C mirror use.
    pub fn code(self) -> i32 {
        self as i32
    }

    /// True for the three raw-QPC fallback outcomes.
    pub fn is_raw_fallback(self) -> bool {
        matches!(
            self,
            MediaDiscipline::Disabled | MediaDiscipline::ReadFailed | MediaDiscipline::ApiMissing
        )
    }
}

/// The media-clock verdict. Discriminants match the C `genlock_media_clock` enum.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaClock {
    /// The media clock follows the disciplined wall clock (or there is not yet enough data).
    Ok = 0,
    /// The wall-vs-media offset grew beyond the bound over the window.
    Drift = 1,
    /// Windows: the disciplined clock fell back to raw QPC while dantesync runs.
    Undisciplined = 2,
}

impl MediaClock {
    /// The integer the C `genlock_media_clock_verdict` returns.
    pub fn code(self) -> u8 {
        self as u8
    }
}

/// The media-clock rate over one window, as the widget reduces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaClockWindow {
    /// The time-weighted rate of the non-step pairs scaled to the window: µs of wall-vs-media drift per
    /// `window_s`.
    pub drift_us: i64,
    /// The total interval (ms) of the pairs that counted (gaps and non-increasing times left out);
    /// the window is ready once this covers ≥ 90 % of `window_s`.
    pub counted_ms: i64,
}

/// One pair's rate in ppb (ns of offset change per s): `change_us × 1e6 / dt_ms`, truncated toward
/// zero, saturating. `dt_ms` must be positive.
pub fn media_clock_pair_rate_ppb(change_us: i64, dt_ms: i64) -> i64 {
    change_us.saturating_mul(1_000_000) / dt_ms
}

/// The wall-vs-media rate across a window of `(t_ms, offset_us)` samples (oldest first; `t_ms` the
/// widget's monotonic ms, `offset_us` the wall-minus-media offset in µs). Each consecutive pair with
/// `0 < dt ≤ max_gap_ms` yields one rate ([`media_clock_pair_rate_ppb`]); their MEDIAN (the mean of the
/// two middle rates for an even count, `a + (b − a) / 2`) is the centre. A pair is kept when its change
/// is within `band_us` of `centre × dt / 1e6` µs (a negative band keeps none); the rate is
/// `Σ kept change × 1e6 / Σ kept dt` ppb (the centre itself when none is kept), truncated toward zero,
/// scaled to `window_s`: `rate × window_s / 1000` µs. Every step saturates. No pair, or a non-positive
/// `window_s`, gives 0 drift (`counted_ms` is still reported).
///
/// Byte-for-byte mirror of `genlock_media_clock_window_drift_us` in `GenlockLockState.hpp` — the
/// parity gate `tests/genlock_lock_state_parity.rs` keeps the two (and the python mirror) identical.
pub fn media_clock_window(
    samples: &[(i64, i64)],
    window_s: i64,
    max_gap_ms: i64,
    band_us: i64,
) -> MediaClockWindow {
    let pairs: Vec<(i64, i64)> = samples
        .windows(2)
        .map(|w| (w[1].0.saturating_sub(w[0].0), w[1].1.saturating_sub(w[0].1)))
        .filter(|&(dt, _)| dt > 0 && dt <= max_gap_ms)
        .collect();
    let counted_ms = pairs
        .iter()
        .fold(0i64, |acc, &(dt, _)| acc.saturating_add(dt));
    if pairs.is_empty() || window_s <= 0 {
        return MediaClockWindow {
            drift_us: 0,
            counted_ms,
        };
    }
    let mut rates: Vec<i64> = pairs
        .iter()
        .map(|&(dt, change)| media_clock_pair_rate_ppb(change, dt))
        .collect();
    rates.sort_unstable();
    let m = rates.len();
    let centre = if m % 2 == 1 {
        rates[m / 2]
    } else {
        let (a, b) = (rates[m / 2 - 1], rates[m / 2]);
        a.saturating_add(b.saturating_sub(a) / 2)
    };
    let (mut change_sum, mut dt_sum) = (0i64, 0i64);
    for &(dt, change) in &pairs {
        let expected = centre.saturating_mul(dt) / 1_000_000;
        if change.saturating_sub(expected).saturating_abs() <= band_us {
            change_sum = change_sum.saturating_add(change);
            dt_sum = dt_sum.saturating_add(dt);
        }
    }
    let rate = if dt_sum == 0 {
        centre
    } else {
        change_sum.saturating_mul(1_000_000) / dt_sum
    };
    MediaClockWindow {
        drift_us: rate.saturating_mul(window_s) / 1000,
        counted_ms,
    }
}

/// True once the counted pairs cover ≥ 90 % of the `window_s` window; a non-positive window is never
/// ready. Byte-for-byte mirror of `genlock_media_clock_window_ready` in `GenlockLockState.hpp`.
pub fn media_clock_window_ready(counted_ms: i64, window_s: i64) -> bool {
    window_s > 0 && counted_ms >= window_s.saturating_mul(1000) / 10 * 9
}

/// Decide the media-clock verdict. `Undisciplined` when the Windows discipline fell back to raw QPC
/// while dantesync answers (`clock_present`); else `Drift` when the window is ready (spans ≥ 90 % of
/// [`GENLOCK_MEDIA_CLOCK_WINDOW_S`]) and `|drift_us|` exceeds `drift_bound_us`; else `Ok`.
///
/// Byte-for-byte mirror of `genlock_media_clock_verdict` in `GenlockLockState.hpp` (parity-gated).
pub fn media_clock_verdict(
    window_ready: bool,
    drift_us: i64,
    drift_bound_us: i64,
    discipline: MediaDiscipline,
    clock_present: bool,
) -> MediaClock {
    if clock_present && discipline.is_raw_fallback() {
        return MediaClock::Undisciplined;
    }
    if window_ready && drift_us.saturating_abs() > drift_bound_us {
        return MediaClock::Drift;
    }
    MediaClock::Ok
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
    fn connected_sum_excludes_idle_inputs() {
        // #1341 — only the connected, NON-idle input contributes; the idle one (raw counters high)
        // is dropped exactly like the absent one.
        let inputs = [ev(true, 1, 0, 0), ev_idle(500, 0, 0), ev(true, 0, 2, 0)];
        assert_eq!(connected_phase_event_sum(&inputs), 3);
    }

    #[test]
    fn offender_excludes_idle_inputs() {
        // #1341 — an idle SongPlayer input carries the most raw counters but must never be named the
        // recent-event offender; the top CONNECTED-non-idle input wins.
        let inputs = [ev(true, 2, 0, 0), ev_idle(9999, 0, 0), ev(true, 5, 0, 0)];
        assert_eq!(top_phase_event_offender(&inputs), Some((2, 5)));
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

    // ---- #1299 Part 4 / #1357 scope C: the qpc_drift term is a wall STEP, one semantics per box ----
    //
    // Live fixtures from 24.9.2026 (issue 1357 validation): every `qpc_drift` DEGRADED on both boxes
    // came from the removed RATE-vs-instantaneous-slew branch, none from a step. strih-lx (Linux,
    // `CLOCK_MONOTONIC` is kernel-disciplined): measured 0.0 on all 689 samples while dantesync's
    // `f_ptp + f_phase` swung -160..+171 ppm. stream (Windows, free QPC): measured 23.4 ppm while the
    // instantaneous `f_ptp + f_phase` read 109.8. Neither is a genlock hazard; a STEP is, on both.

    #[test]
    fn window_rate_ppm_matches_the_overnight_strih_slope() {
        // 742 − 101 = 641 ms over 12.5 h (45_000_000 ms) ≈ 14.24 ppm.
        assert!((qpc_window_rate_ppm(641, 45_000_000) - 14.2444).abs() < 0.001);
    }

    #[test]
    fn window_rate_ppm_is_zero_for_a_nonpositive_span() {
        assert_eq!(qpc_window_rate_ppm(5, 0), 0.0);
        assert_eq!(qpc_window_rate_ppm(5, -10), 0.0);
    }

    #[test]
    fn steady_disciplined_slew_does_not_degrade() {
        // A Windows box: the wall runs ≈14 ppm against the free QPC crystal, no step.
        let v = qpc_drift_beyond_bound(true, 641, 45_000_000, 0, GENLOCK_QPC_STEP_BOUND_MS);
        assert!((v.measured_ppm - 14.2444).abs() < 0.001);
        assert!(!v.beyond_bound);
    }

    #[test]
    fn a_step_within_the_window_degrades_even_before_ready() {
        // A 40 ms single-sample jump (an NTP RTC step) > one 30 fps frame (33 ms).
        let v = qpc_drift_beyond_bound(false, 0, 0, 40, GENLOCK_QPC_STEP_BOUND_MS);
        assert!(v.beyond_bound);
        let back = qpc_drift_beyond_bound(true, -40, 300_000, -40, GENLOCK_QPC_STEP_BOUND_MS);
        assert!(back.beyond_bound, "a backward step degrades too");
    }

    #[test]
    fn a_step_at_the_bound_is_not_beyond_and_one_over_is_1357() {
        assert!(
            !qpc_drift_beyond_bound(true, 0, 300_000, 33, GENLOCK_QPC_STEP_BOUND_MS).beyond_bound
        );
        assert!(
            qpc_drift_beyond_bound(true, 0, 300_000, 34, GENLOCK_QPC_STEP_BOUND_MS).beyond_bound
        );
    }

    #[test]
    fn a_large_rate_alone_no_longer_degrades_1357() {
        // ≈133 ppm over a filled window with a sub-frame step. The render tick re-derives every
        // deadline from the wall clock and absorbs up to 2 ms per tick, so a rate is not a genlock
        // hazard; it stays REPORT-ONLY telemetry (measured_ppm) and never feeds the verdict.
        let v = qpc_drift_beyond_bound(true, 6, 45_000, 1, GENLOCK_QPC_STEP_BOUND_MS);
        assert!(
            v.measured_ppm > 120.0,
            "the rate is still measured for telemetry"
        );
        assert!(!v.beyond_bound);
    }

    #[test]
    fn linux_disciplined_monotonic_window_never_degrades_1357() {
        // strih-lx 24.9. 01:10:49: `CLOCK_MONOTONIC` shares the kernel frequency discipline, so the
        // windowed wall-vs-monotonic delta is 0 by construction (dantesync reported -68.5 ppm, later
        // +170.9 — a servo excursion, not a wall-vs-monotonic hazard).
        let v = qpc_drift_beyond_bound(true, 0, 300_000, 0, GENLOCK_QPC_STEP_BOUND_MS);
        assert_eq!(v.measured_ppm, 0.0);
        assert!(!v.beyond_bound);
    }

    #[test]
    fn windows_and_linux_windows_give_the_same_verdict_1357() {
        // stream 24.9. 05:09:00 (Windows): 7 ms accrued over a 299 s window = 23.4 ppm, no step.
        // strih-lx the same minute (Linux): 0 ms accrued. ONE semantics: the same (no-step) verdict,
        // and the same (step) verdict once either window carries a 40 ms wall step.
        let win = qpc_drift_beyond_bound(true, 7, 299_000, 1, GENLOCK_QPC_STEP_BOUND_MS);
        let lx = qpc_drift_beyond_bound(true, 0, 299_000, 0, GENLOCK_QPC_STEP_BOUND_MS);
        assert!((win.measured_ppm - 23.4114).abs() < 0.001);
        assert_eq!(win.beyond_bound, lx.beyond_bound);
        assert!(!win.beyond_bound);
        let win_step = qpc_drift_beyond_bound(true, 47, 299_000, 41, GENLOCK_QPC_STEP_BOUND_MS);
        let lx_step = qpc_drift_beyond_bound(true, 40, 299_000, 40, GENLOCK_QPC_STEP_BOUND_MS);
        assert!(win_step.beyond_bound && lx_step.beyond_bound);
    }

    #[test]
    fn not_ready_window_reports_no_rate_and_never_degrades_on_it() {
        let v = qpc_drift_beyond_bound(false, 999, 1000, 0, GENLOCK_QPC_STEP_BOUND_MS);
        assert_eq!(v.measured_ppm, 0.0);
        assert!(!v.beyond_bound);
    }

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

    /// `(t_ms, offset_us)` samples every `dt_ms`, offset = `f(i)`.
    fn ramp(n: i64, dt_ms: i64, f: impl Fn(i64) -> i64) -> Vec<(i64, i64)> {
        (0..=n).map(|i| (i * dt_ms, f(i))).collect()
    }

    fn window(samples: &[(i64, i64)]) -> MediaClockWindow {
        media_clock_window(
            samples,
            GENLOCK_MEDIA_CLOCK_WINDOW_S,
            GENLOCK_MEDIA_CLOCK_MAX_GAP_MS,
            GENLOCK_MEDIA_CLOCK_BAND_US,
        )
    }

    /// A deterministic ±`amp` µs noise sequence (splitmix64 of the index), for offsets that wobble
    /// but do not drift.
    fn noise(i: i64, amp: i64) -> i64 {
        let mut z = (i as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z % (2 * amp as u64 + 1)) as i64 - amp
    }

    #[test]
    fn the_window_rate_leaves_steps_out_and_counts_a_partial_drift() {
        // The pre-part-A stream: 13.5 ppm = 13.5 µs per 1 Hz pair -> 8.1 ms per 600 s.
        let w = window(&ramp(600, 1000, |i| i * 27 / 2));
        assert_eq!(w.drift_us, 8100);
        assert_eq!(w.counted_ms, 600_000);
        // A disciplined box whose dantesync steps — a locked master with phase_slew off
        // (1000 µs + 2 × 23 ppm × 10 s = 1460 µs), a client (600 µs), the not-locked master's 250 µs
        // every 25 s, the −146 µs step seen on win-resolume — all one direction: 0.
        assert_eq!(window(&ramp(600, 1000, |i| (i / 60) * 1460)).drift_us, 0);
        assert_eq!(window(&ramp(600, 1000, |i| (i / 30) * 600)).drift_us, 0);
        assert_eq!(window(&ramp(600, 1000, |i| (i / 25) * 250)).drift_us, 0);
        assert_eq!(window(&ramp(600, 1000, |i| (i / 20) * -146)).drift_us, 0);
        // The same steps on top of the undisciplined rate: the rate (each step pair also carried
        // ~14 µs of rate, which leaves with it: (300 × 13 000 + 290 × 14 000) / 590 ppb).
        assert_eq!(
            window(&ramp(600, 1000, |i| i * 27 / 2 + (i / 60) * 1460)).drift_us,
            8094
        );
        // A late UI tick carrying a step, and jittered ticks: still the rate.
        let mut late: Vec<(i64, i64)> = ramp(600, 1000, |i| i * 27 / 2);
        for (k, s) in late.iter_mut().enumerate().skip(300) {
            s.0 += 500; // one 1.5 s tick at 300
            s.1 += 520 + if k >= 450 { 520 } else { 0 };
        }
        assert_eq!(window(&late).drift_us, 8098);
        // A 20 ppm drift in only 45 % of the seconds (the rest flat): a pure median would read 0; the
        // time-weighted rate reads its true share, 20 × 0.45 × 600 = 5400 µs -> DRIFT.
        let partial = ramp(600, 1000, |i| (i * 9 / 20) * 20);
        assert_eq!(window(&partial).drift_us, 5400);
        // The same at 40 ppm (a 40 µs change per 1 s pair, inside the 100 µs band): 10 800 µs, not 0.
        let partial40 = ramp(600, 1000, |i| (i * 9 / 20) * 40);
        assert_eq!(window(&partial40).drift_us, 10_800);
        // A drift sampled by short (16 ms) pairs, each change 0 or 1 µs, stays inside the band.
        let short = ramp(3000, 16, |i| i * 16 * 20 / 1000);
        assert_eq!(window(&short).drift_us, 12_000);
        // Time-weighted: 300 flat 1 s pairs + 60 stalled 4 s pairs carrying 15 ppm (60 µs each). The
        // rate is Σ change / Σ dt = 3600 µs / 540 s = 6666 ppb -> 3999 µs; an unweighted mean of the
        // per-pair rates would read 2500 ppb -> 1500 µs.
        let mut mixed: Vec<(i64, i64)> = ramp(300, 1000, |_| 0);
        for k in 1..=60i64 {
            mixed.push((300_000 + k * 4000, 60 * k));
        }
        assert_eq!(window(&mixed).drift_us, 3999);
        // The band edge: a change exactly `band` µs off the centre's prediction is kept, 1 µs more is not.
        let edge = |far: i64| {
            let mut v = vec![(0i64, 0i64)];
            for k in 1..=5i64 {
                v.push((k * 1000, 0));
            }
            v.push((6000, far));
            window(&v).drift_us
        };
        // 100 µs is kept (100 µs / 6 s = 16 666 ppb × 600 / 1000); 101 µs is dropped.
        assert_eq!(edge(100), 9999);
        assert_eq!(edge(101), 0);
        // Sampling wobble (5× the ±1 µs measured on dev1) that does not drift stays far below 2 ms.
        let wobble = window(&ramp(600, 1000, |i| noise(i, 5)));
        assert!(wobble.drift_us.abs() < 200, "{wobble:?}");
        // A pair more than 5 s apart (a stalled UI) is not a sample, and does not count as covered.
        let gap = window(&[(0, 0), (1000, 20), (121_000, -14_000), (122_000, -13_980)]);
        assert_eq!(
            gap,
            MediaClockWindow {
                drift_us: 12_000,
                counted_ms: 2000
            }
        );
        assert!(!media_clock_window_ready(gap.counted_ms, 600));
        // Negative drift; an even count averages the two middle rates, and an odd, negative middle
        // gap rounds toward the lower rate exactly like the C (a + (b - a) / 2).
        assert_eq!(window(&ramp(3, 1000, |i| -20 * i)).drift_us, -12_000);
        assert_eq!(window(&[(0, 0), (1000, 10), (2000, 30)]).drift_us, 9000);
        // (a negative band keeps no pair, so the centre itself is the result)
        // centre -333 333 + 333 333 / 2 = -166 667 ppb:
        let odd_neg = media_clock_window(&[(0, 0), (3, -1), (6, -1)], 600, 5000, -1);
        assert_eq!(odd_neg.drift_us, -100_000);
        // centre 333 333 + 333 333 / 2 = 499 999 ppb:
        let odd_pos = media_clock_window(&[(0, 0), (3, 1), (6, 3)], 600, 5000, -1);
        assert_eq!(odd_pos.drift_us, 299_999);
        // ...and kept, the time-weighted rate: 3 µs / 6 ms = 500 000 ppb.
        let kept = media_clock_window(&[(0, 0), (3, 1), (6, 3)], 600, 5000, 1);
        assert_eq!(kept.drift_us, 300_000);

        // No pair, a non-increasing time, a non-positive window.
        assert_eq!(
            window(&[(0, 7)]),
            MediaClockWindow {
                drift_us: 0,
                counted_ms: 0
            }
        );
        assert_eq!(window(&[]).drift_us, 0);
        assert_eq!(window(&[(1000, 0), (1000, 50)]).counted_ms, 0);
        assert_eq!(
            media_clock_window(&[(0, 0), (1000, 5)], 0, 5000, GENLOCK_MEDIA_CLOCK_BAND_US).drift_us,
            0
        );
        // Saturating at the extremes, never a panic.
        assert_eq!(
            media_clock_window(
                &[(0, i64::MIN), (1, i64::MAX)],
                i64::MAX,
                i64::MAX,
                i64::MAX
            )
            .drift_us,
            i64::MAX / 1000
        );
        // an even count of MIN and MAX: the centre MIN + MAX / 2 < 0 keeps neither (band 100 µs), so
        // the centre is the rate; times a huge window it saturates to MIN
        assert_eq!(
            media_clock_window(
                &[(0, 0), (1, i64::MIN), (2, i64::MAX)],
                i64::MAX,
                i64::MAX,
                GENLOCK_MEDIA_CLOCK_BAND_US
            )
            .drift_us,
            i64::MIN / 1000
        );
        // two saturated (MAX) rates kept: the change sum × 1e6 saturates, / 2 ms = MAX / 2, and the
        // window scale saturates again
        assert_eq!(
            media_clock_window(&[(0, 0), (1, 1 << 62), (2, i64::MAX)], 1000, 5000, i64::MAX)
                .drift_us,
            i64::MAX / 1000
        );
    }

    #[test]
    fn the_window_is_ready_at_ninety_percent_coverage() {
        assert!(!media_clock_window_ready(539_999, 600));
        assert!(media_clock_window_ready(540_000, 600));
        assert!(media_clock_window_ready(600_000, 600));
        assert!(!media_clock_window_ready(0, 600));
        assert!(!media_clock_window_ready(0, 0));
        assert!(!media_clock_window_ready(i64::MAX, -1));
        assert!(media_clock_window_ready(i64::MAX, i64::MAX));
    }

    #[test]
    fn drift_verdict_follows_the_bound_and_waits_for_the_window() {
        let b = GENLOCK_MEDIA_CLOCK_DRIFT_BOUND_US;
        let a = MediaDiscipline::Active;
        // Pre-part-A stream (≈ 8 ms / 10 min) -> DRIFT; a disciplined box (0) -> OK.
        assert_eq!(
            media_clock_verdict(true, 8100, b, a, true),
            MediaClock::Drift
        );
        assert_eq!(
            media_clock_verdict(true, -2001, b, a, true),
            MediaClock::Drift
        );
        for d in [-2000, -1, 0, 1, 2000] {
            assert_eq!(media_clock_verdict(true, d, b, a, true), MediaClock::Ok);
        }
        // Not ready (the window is not yet ~full) -> never judged on drift.
        assert_eq!(
            media_clock_verdict(false, 99_000, b, a, true),
            MediaClock::Ok
        );
        // Linux: not applicable, judged on drift only.
        let na = MediaDiscipline::NotApplicable;
        assert_eq!(media_clock_verdict(true, 0, b, na, true), MediaClock::Ok);
        assert_eq!(
            media_clock_verdict(true, 5000, b, na, true),
            MediaClock::Drift
        );
    }

    #[test]
    fn a_raw_qpc_fallback_while_dantesync_runs_is_undisciplined() {
        let b = GENLOCK_MEDIA_CLOCK_DRIFT_BOUND_US;
        for d in [
            MediaDiscipline::Disabled,
            MediaDiscipline::ReadFailed,
            MediaDiscipline::ApiMissing,
        ] {
            assert!(d.is_raw_fallback());
            // Immediately, before any drift accrues and before the window is ready.
            assert_eq!(
                media_clock_verdict(false, 0, b, d, true),
                MediaClock::Undisciplined
            );
            // No dantesync answering: raw QPC is the right clock, judged on drift only.
            assert_eq!(media_clock_verdict(false, 0, b, d, false), MediaClock::Ok);
            assert_eq!(
                media_clock_verdict(true, 9000, b, d, false),
                MediaClock::Drift
            );
        }
        for d in [
            MediaDiscipline::Unknown,
            MediaDiscipline::Active,
            MediaDiscipline::NotApplicable,
        ] {
            assert!(!d.is_raw_fallback());
            assert_eq!(media_clock_verdict(false, 0, b, d, true), MediaClock::Ok);
        }
    }

    #[test]
    fn media_codes_match_the_c_and_libobs_values() {
        assert_eq!(MediaClock::Ok.code(), 0);
        assert_eq!(MediaClock::Drift.code(), 1);
        assert_eq!(MediaClock::Undisciplined.code(), 2);
        assert_eq!(MediaDiscipline::Unknown.code(), 0);
        assert_eq!(MediaDiscipline::Active.code(), 1);
        assert_eq!(MediaDiscipline::Disabled.code(), 2);
        assert_eq!(MediaDiscipline::ReadFailed.code(), 3);
        assert_eq!(MediaDiscipline::ApiMissing.code(), 4);
        assert_eq!(MediaDiscipline::NotApplicable.code(), 5);
    }
}
