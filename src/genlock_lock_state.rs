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
//!
//! The module is split by concern (issue 1302); every item keeps its path here through the
//! re-exports below:
//!
//! - `decision.rs`: the state decision ([`decide`], [`GenlockFacets`], [`LockState`], [`LockReason`]);
//! - `phase_events.rs`: one input's phase events, the per-input baseline and the idle class;
//! - `qpc_step.rs`: the wall-clock step verdict and the fleet date-step booking;
//! - `media_clock.rs`: the media (audio) clock term.

// Explicit paths, relative to this file's directory, so the submodules are found the same way
// whether the crate loads this file normally or a Tier-0 standalone replica mounts it with
// `#[path = ".../src/genlock_lock_state.rs"]`. rustc treats a `#[path]`-mounted file like a
// mod.rs and would look for a bare `mod decision;` at `src/decision.rs`.
#[path = "genlock_lock_state/decision.rs"]
mod decision;
#[path = "genlock_lock_state/media_clock.rs"]
mod media_clock;
#[path = "genlock_lock_state/phase_events.rs"]
mod phase_events;
#[path = "genlock_lock_state/qpc_step.rs"]
mod qpc_step;

pub use decision::{decide, GenlockFacets, LockReason, LockState};
pub use media_clock::{
    media_clock_pair_rate_ppb, media_clock_verdict, media_clock_window, media_clock_window_ready,
    MediaClock, MediaClockWindow, MediaDiscipline, GENLOCK_MEDIA_CLOCK_BAND_US,
    GENLOCK_MEDIA_CLOCK_DRIFT_BOUND_US, GENLOCK_MEDIA_CLOCK_MAX_GAP_MS,
    GENLOCK_MEDIA_CLOCK_WINDOW_S,
};
pub use phase_events::{
    input_idle_class, input_new_phase_events, input_phase_events, InputEventCounts, InputIdleClass,
    PhaseEventSample, GENLOCK_IDLE_FAST_MIN_FRAMES, GENLOCK_IDLE_FAST_SPAN_MS,
    GENLOCK_IDLE_FULL_SPAN_MS, GENLOCK_IDLE_INPUT_MIN_FRAMES, GENLOCK_IDLE_WINDOW_MS,
};
pub use qpc_step::{
    qpc_drift_beyond_bound, qpc_wall_step_rebase_ms, qpc_window_rate_ppm, QpcDriftVerdict,
    GENLOCK_QPC_STEP_BOUND_MS, GENLOCK_QPC_WALL_STEPS_PER_WINDOW,
    GENLOCK_QPC_WALL_STEP_BOOK_MAX_MS, GENLOCK_QPC_WINDOW_S,
};
