//! #411 — Windows-local unattended self-heal for the #391 OBS liveness watchdog (Tier-0,
//! default features).
//!
//! #391 shipped DETECT + ALERT only: a dev1 systemd timer polls OBS WebSocket `GetStats` on
//! both broadcast boxes, runs the strict `obs_watchdog::classify` verdict, and fires a Discord
//! alert once a wedge is confirmed — but RECOVERY still needs an agent to see that alert and run
//! `scripts/launch-obs-genlock.sh --force` via the win-* MCP. That fails the exact
//! overnight/unattended case the watchdog exists to cover (the founding incident: stream OBS was
//! wedged for ~25 HOURS and nothing acted on it because nobody was watching Discord). This module
//! is the RECOVERY-decision + AHK-sequencing kernel for a per-box Windows Task Scheduler job that
//! runs the SAME `obs_watchdog::classify` verdict locally (via the `obs-watchdog-gate` binary fed
//! LOCAL process signals — `Get-Process obs64`'s count / `Responding` / CPU%, no OBS WebSocket
//! round-trip needed) and force-kills + relaunches obs64 itself when a wedge is CONFIRMED — no
//! agent, no ssh, no MCP, no human watching Discord required.
//!
//! **This module does NOT re-decide "is the box wedged".** That stays exactly where it is —
//! `obs_watchdog::classify` — reused unchanged by the Windows-side mechanism (see
//! `scripts/obs-self-heal-install.sh`, which pipes a local sample to `obs-watchdog-gate` and reads
//! its exit code). This module decides "given a wedged/healthy verdict THIS PASS, WHEN to act":
//!
//! - a **confirm-threshold** (never force-kill a healthy box off one transient blip),
//! - a **min-interval throttle** (never hammer a box that a restart structurally cannot fix — see
//!   `.claude/skills/obs-ops` "GPU wedge on stream box"),
//! - a **single-recovery-in-flight lock** (never let two recoveries race each other — or, on
//!   strih, race the EXISTING `AutoHotkey64` auto-respawn watcher; see `.claude/skills/obs-ops`
//!   "AHK on strih", which documents a real previously-hit double-launch/DLL-lock failure mode),
//! - the **ordered recovery step plan** that makes an AHK/self-heal double-launch of obs64
//!   IMPOSSIBLE BY CONSTRUCTION — stop AHK before ever touching obs64, only restart AHK after the
//!   relaunch has been checked, mirroring the documented obs-ops hot-swap sequence verbatim
//!   ("Stop-Process AutoHotkey64 FIRST → kill obs64 → relaunch → confirm → THEN restart AHK"),
//! - the **post-recovery verification rule** ("exactly one obs64 AND render tick ENABLED").
//!
//! Pure so it unit-tests on default features (Tier-0) — no OBS, no network, no MCP, no Windows
//! host. The Windows-side mechanism (the recovery PowerShell + the Task Scheduler XML) is emitted
//! by the pure bash planner `scripts/obs-self-heal-install.sh` (mirrors
//! `scripts/launch-obs-genlock.sh`'s `build_launch_program` shape, and literally REUSES that
//! function for the kill+relaunch+verify step — no second launch path), structurally asserted by
//! `tests/obs_self_heal_install.rs` (same pattern as `tests/launch_obs_genlock.rs`). Ships
//! disabled; the supervisor installs + live-verifies on strih + stream via the win-* MCP.

/// Ordered recovery steps (#411). The order IS the AHK-race fix: AutoHotkey64's `SafeLoop` on
/// strih auto-respawns obs64 within ~1s of its window disappearing, so killing obs64 while AHK is
/// still running risks AHK respawning a SECOND obs64 concurrently with this job's own relaunch —
/// corrupting the exactly-one-obs64 invariant this whole gate depends on (a documented, previously
/// hit failure mode — see `.claude/skills/obs-ops` "AHK on strih" hot-swap gotcha). Stopping AHK
/// FIRST and restarting it only LAST (after the relaunch is checked) makes a double-launch
/// impossible by construction, for any confirmed-wedge recovery, every time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryStep {
    /// `Stop-Process AutoHotkey64` — MUST run before obs64 is ever touched.
    StopAhk,
    /// Force-kill the wedged obs64 + relaunch it via the EXISTING
    /// `launch-obs-genlock.sh`/`build_launch_program` planner (`--force`) — reused verbatim, never
    /// a second hand-rolled launch path. Includes that planner's own log-verify.
    KillAndRelaunchObs,
    /// Explicit post-recovery check: exactly one obs64 process AND the OBS log shows the genlock
    /// render tick ENABLED (`recovery_verified` below states the rule both sides agree on).
    VerifyRecovered,
    /// Restart AutoHotkey64 so crash/reboot auto-respawn is restored. Runs LAST, and always —
    /// regardless of whether `VerifyRecovered` passed — because AHK's own crash-respawn duty is
    /// strictly more valuable always-on than conditionally withheld; a failed verify is instead
    /// logged and left for the confirm/throttle cycle to retry (or an agent to investigate), never
    /// masked, but also never a reason to leave the box with NO respawn safety net at all.
    RestartAhk,
}

/// The fixed, ordered recovery plan (#411): always these four steps, always in this order. A
/// `Vec` (not a const array) so callers can log/iterate uniformly with the rest of this codebase's
/// planner shape (`launch_obs_genlock`'s `build_launch_program` also returns an owned `String`).
pub fn recovery_plan() -> Vec<RecoveryStep> {
    vec![
        RecoveryStep::StopAhk,
        RecoveryStep::KillAndRelaunchObs,
        RecoveryStep::VerifyRecovered,
        RecoveryStep::RestartAhk,
    ]
}

/// Consecutive wedged passes required before acting. Mirrors
/// `OBS_WATCHDOG_CONFIRM_THRESHOLD` (2, the #391 alert watchdog's own default) so the self-heal
/// job never force-kills a healthy box off one transient process-sampling blip.
pub const DEFAULT_CONFIRM_THRESHOLD: u32 = 2;

/// Minimum seconds between two recovery ATTEMPTS on the same box. A structural wedge a restart
/// cannot fix (e.g. a hung GPU driver — `.claude/skills/obs-ops` "GPU wedge on stream box") would
/// otherwise get force-killed + relaunched every ~2 min forever. 600s (10 min) is long enough for
/// a healthy relaunch (`launch-obs-genlock.sh`'s own verify loop takes ~30s) to prove itself before
/// retrying, short enough that an overnight wedge still self-heals fast relative to the 25h
/// founding incident.
pub const DEFAULT_MIN_RECOVERY_INTERVAL_S: u64 = 600;

/// A held recovery lock older than this is treated as ABANDONED (the process that set it crashed
/// before calling `clear_recovery_lock`) rather than a live in-flight recovery — otherwise one
/// crashed run would permanently disable self-heal for that box. Comfortably above the ~30s
/// `launch-obs-genlock.sh` verify loop.
pub const DEFAULT_STALE_LOCK_S: u64 = 900;

/// Persisted per-box state the Windows-local job carries between ~2-minute Task Scheduler passes
/// (mirrors `scripts/lib/obs-watchdog-decision.sh`'s state-file shape, but for the RECOVERY/ACTION
/// decision rather than the alert decision).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SelfHealState {
    /// Consecutive wedged passes observed so far. Reset to 0 by any healthy pass.
    pub confirm_count: u32,
    /// Epoch-seconds of the last recovery ATTEMPT start (`None` = never attempted).
    pub last_attempt_epoch_s: Option<u64>,
    /// `true` while a recovery is in progress: set the INSTANT a recovery is decided (before step
    /// 1 runs), cleared only by `clear_recovery_lock` after the step plan finishes (success or
    /// failure). The single-writer lock — while held, `decide` NEVER returns `Recover` again.
    pub recovery_in_progress: bool,
}

/// What the self-heal job should do this pass.
#[derive(Debug, Clone, PartialEq)]
pub enum SelfHealDecision {
    /// The box is healthy this pass. No action.
    Healthy,
    /// The box looks wedged but has not yet been seen wedged `threshold` times in a row.
    Confirming { count: u32, threshold: u32 },
    /// Confirmed wedged, but the LAST recovery attempt was too recent — wait.
    Throttled { seconds_remaining: u64 },
    /// A recovery from a PRIOR pass is still marked in-flight (the lock is held) — never start a
    /// second one concurrently, whatever this pass's verdict is.
    AlreadyRecovering,
    /// Confirmed wedged, not throttled, lock free — ACT. Carries the ordered step plan.
    Recover(Vec<RecoveryStep>),
}

/// Decide this pass's action AND the next persisted state.
///
/// `wedged` MUST be the result of a real `obs_watchdog::classify` call this pass
/// (`!verdict.is_healthy()`) — this function never re-derives or second-guesses that boolean, it
/// only decides WHEN to act on it. Fail-closed: callers must never invent `wedged = true`
/// speculatively; a missing/failed sample is the CALLER's problem to resolve (report unhealthy or
/// skip the pass), never silently coerced to false here.
///
/// The lock (`prev.recovery_in_progress`) is checked FIRST and wins over every other signal —
/// a recovery already in flight is never joined by a second one, healthy or not.
pub fn decide(
    prev: SelfHealState,
    wedged: bool,
    now_epoch_s: u64,
    threshold: u32,
    min_interval_s: u64,
) -> (SelfHealDecision, SelfHealState) {
    if prev.recovery_in_progress {
        return (SelfHealDecision::AlreadyRecovering, prev);
    }

    if !wedged {
        return (
            SelfHealDecision::Healthy,
            SelfHealState {
                confirm_count: 0,
                ..prev
            },
        );
    }

    let confirm_count = prev.confirm_count.saturating_add(1);
    let effective_threshold = threshold.max(1);
    if confirm_count < effective_threshold {
        return (
            SelfHealDecision::Confirming {
                count: confirm_count,
                threshold,
            },
            SelfHealState {
                confirm_count,
                ..prev
            },
        );
    }

    if let Some(last) = prev.last_attempt_epoch_s {
        let elapsed = now_epoch_s.saturating_sub(last);
        if elapsed < min_interval_s {
            return (
                SelfHealDecision::Throttled {
                    seconds_remaining: min_interval_s - elapsed,
                },
                SelfHealState {
                    confirm_count,
                    ..prev
                },
            );
        }
    }

    (
        SelfHealDecision::Recover(recovery_plan()),
        SelfHealState {
            confirm_count,
            last_attempt_epoch_s: Some(now_epoch_s),
            recovery_in_progress: true,
        },
    )
}

/// The caller marks a recovery attempt complete (success OR failure) by clearing the lock — the
/// ONLY way `recovery_in_progress` goes back to `false`. A crash mid-recovery (the script dies
/// before calling this) leaves the lock HELD, which is deliberately fail-safe (see
/// `lock_is_stale` below for how a truly abandoned lock eventually clears itself).
pub fn clear_recovery_lock(state: SelfHealState) -> SelfHealState {
    SelfHealState {
        recovery_in_progress: false,
        ..state
    }
}

/// `true` when `state`'s recovery lock has been held longer than `stale_lock_s` — i.e. the process
/// that set it almost certainly crashed before calling `clear_recovery_lock`. Callers should
/// `clear_recovery_lock` before calling `decide` when this is `true`, so one abandoned run does
/// not permanently disable self-heal for the box.
pub fn lock_is_stale(state: &SelfHealState, now_epoch_s: u64, stale_lock_s: u64) -> bool {
    state.recovery_in_progress
        && state
            .last_attempt_epoch_s
            .map(|started| now_epoch_s.saturating_sub(started) > stale_lock_s)
            .unwrap_or(false)
}

/// Post-recovery success rule (#411 spec): recovery is VERIFIED only when there is exactly one
/// obs64 process AND the freshly-relaunched OBS log shows the genlock render tick ENABLED (the
/// SAME proof `launch-obs-genlock.sh`'s own verify step checks). Stated once here so the Rust
/// tests and the emitted PowerShell agree on exactly what "recovered" means.
pub fn recovery_verified(obs64_count: u32, render_tick_enabled: bool) -> bool {
    obs64_count == 1 && render_tick_enabled
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_000_000;

    #[test]
    fn confirmed_wedge_yields_a_recovery_plan() {
        // Two consecutive wedged passes (threshold=2, the #391 default) -> Recover.
        let (d1, s1) = decide(SelfHealState::default(), true, T0, 2, 600);
        assert_eq!(
            d1,
            SelfHealDecision::Confirming {
                count: 1,
                threshold: 2
            },
            "first wedged pass must only confirm, never act"
        );
        assert!(!s1.recovery_in_progress);

        let (d2, s2) = decide(s1, true, T0 + 10, 2, 600);
        match d2 {
            SelfHealDecision::Recover(steps) => {
                assert_eq!(
                    steps,
                    recovery_plan(),
                    "a confirmed wedge must yield the canonical recovery plan"
                );
            }
            other => panic!("second consecutive wedged pass must Recover, got {other:?}"),
        }
        assert!(
            s2.recovery_in_progress,
            "deciding to Recover must set the lock immediately"
        );
        assert_eq!(s2.last_attempt_epoch_s, Some(T0 + 10));
    }

    #[test]
    fn healthy_box_never_yields_action() {
        // Even with a high prior confirm count, a healthy pass must be a strict no-op —
        // never a false force-kill of a box that is actually fine.
        let prev = SelfHealState {
            confirm_count: 5,
            last_attempt_epoch_s: None,
            recovery_in_progress: false,
        };
        let (decision, next) = decide(prev, false, T0, 2, 600);
        assert_eq!(decision, SelfHealDecision::Healthy);
        assert_eq!(
            next.confirm_count, 0,
            "a healthy pass must reset the confirm counter"
        );
        assert!(!next.recovery_in_progress);
    }

    #[test]
    fn throttle_blocks_a_too_soon_second_attempt() {
        let (_, after_first) = decide(SelfHealState::default(), true, T0, 1, 600);
        let after_first = clear_recovery_lock(after_first); // simulate step-plan completion
        assert_eq!(after_first.last_attempt_epoch_s, Some(T0));

        // Still wedged, only 30s later (min_interval=600) -> Throttled, NOT another Recover.
        let (decision, next) = decide(after_first, true, T0 + 30, 1, 600);
        assert_eq!(
            decision,
            SelfHealDecision::Throttled {
                seconds_remaining: 570
            },
            "a too-soon retry must be throttled, never re-fire the recovery plan"
        );
        assert!(!next.recovery_in_progress);

        // Past the interval -> allowed to act again.
        let (decision, _) = decide(after_first, true, T0 + 600, 1, 600);
        assert!(
            matches!(decision, SelfHealDecision::Recover(_)),
            "once past min_interval_s a confirmed wedge must be allowed to recover again"
        );
    }

    #[test]
    fn lock_prevents_concurrent_recovery() {
        let locked = SelfHealState {
            confirm_count: 3,
            last_attempt_epoch_s: Some(T0 - 5),
            recovery_in_progress: true,
        };
        // Whatever this pass's verdict is, a held lock always wins.
        let (decision_wedged, next_wedged) = decide(locked, true, T0, 1, 0);
        assert_eq!(decision_wedged, SelfHealDecision::AlreadyRecovering);
        assert_eq!(
            next_wedged, locked,
            "state must pass through unchanged while the lock is held"
        );

        let (decision_healthy, next_healthy) = decide(locked, false, T0, 1, 0);
        assert_eq!(
            decision_healthy,
            SelfHealDecision::AlreadyRecovering,
            "even a healthy-looking pass must not clear the lock or reset confirm_count \
             out from under an in-flight recovery"
        );
        assert_eq!(next_healthy, locked);
    }

    #[test]
    fn recovery_plan_ahk_race_ordering_is_fixed() {
        let plan = recovery_plan();
        assert_eq!(
            plan,
            vec![
                RecoveryStep::StopAhk,
                RecoveryStep::KillAndRelaunchObs,
                RecoveryStep::VerifyRecovered,
                RecoveryStep::RestartAhk,
            ],
            "the AHK-race fix requires this EXACT order: AHK must be stopped before obs64 is \
             ever touched, and restarted only after the relaunch has been verified"
        );
        assert_eq!(
            plan.first(),
            Some(&RecoveryStep::StopAhk),
            "StopAhk must be the very first step — obs64 must never be touched while AHK \
             could still respawn it concurrently"
        );
        assert_eq!(
            plan.last(),
            Some(&RecoveryStep::RestartAhk),
            "RestartAhk must be the very last step — restoring crash-respawn only after \
             the box's own recovery has been checked"
        );
        let kill_pos = plan
            .iter()
            .position(|s| *s == RecoveryStep::KillAndRelaunchObs)
            .unwrap();
        let verify_pos = plan
            .iter()
            .position(|s| *s == RecoveryStep::VerifyRecovered)
            .unwrap();
        assert!(
            kill_pos < verify_pos,
            "the kill+relaunch must happen before the explicit post-recovery verify"
        );
    }

    #[test]
    fn stale_lock_is_detected_and_clearable() {
        let held = SelfHealState {
            confirm_count: 2,
            last_attempt_epoch_s: Some(T0),
            recovery_in_progress: true,
        };
        assert!(
            !lock_is_stale(&held, T0 + 100, 900),
            "a lock held for only 100s must NOT be considered stale (budget 900s)"
        );
        assert!(
            lock_is_stale(&held, T0 + 901, 900),
            "a lock held past the stale budget must be considered abandoned"
        );

        let cleared = clear_recovery_lock(held);
        assert!(!cleared.recovery_in_progress);
        assert!(
            !lock_is_stale(&cleared, T0 + 10_000, 900),
            "a cleared lock is never stale (nothing to abandon)"
        );

        // Composition: a stale lock must be clearable and then re-enter Confirming/Recover
        // normally, never stuck AlreadyRecovering forever.
        let (decision, _) = decide(cleared, true, T0 + 10_000, 1, 0);
        assert!(
            matches!(decision, SelfHealDecision::Recover(_)),
            "after clearing a stale lock, a confirmed wedge must be able to recover again"
        );
    }

    #[test]
    fn recovery_verified_requires_exactly_one_obs64_and_render_tick_enabled() {
        assert!(recovery_verified(1, true));
        assert!(
            !recovery_verified(0, true),
            "zero obs64 processes must never read as verified, even if the log is fine"
        );
        assert!(
            !recovery_verified(2, true),
            "a double-launched obs64 must never read as verified — the AHK-race case this \
             module exists to make impossible"
        );
        assert!(
            !recovery_verified(1, false),
            "one obs64 process with no genlock render tick is NOT verified — could be a \
             stock/wrong OBS build"
        );
    }

    #[test]
    fn confirm_threshold_of_one_acts_on_first_wedged_pass() {
        let (decision, state) = decide(SelfHealState::default(), true, T0, 1, 600);
        assert!(
            matches!(decision, SelfHealDecision::Recover(_)),
            "threshold=1 must act on the very first wedged pass"
        );
        assert!(state.recovery_in_progress);
    }

    #[test]
    fn zero_threshold_is_treated_as_one_never_acts_with_zero_confirmations() {
        // A misconfigured threshold=0 must never mean "act before observing even one wedged
        // pass" — decide() clamps it to 1 (fail toward the safer, slower behavior).
        let (decision, _) = decide(SelfHealState::default(), true, T0, 0, 600);
        assert!(
            matches!(decision, SelfHealDecision::Recover(_)),
            "threshold clamps to >= 1, so one wedged pass is still enough to act, but never \
             on ZERO observed passes"
        );
    }
}
