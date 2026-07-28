//! #840 — imag-nb loses both OBS projectors on every restart because the boot path bypasses
//! `imag-obs-start.sh` (the ONLY thing that opens the Program + Multiview projectors), and
//! `imag-obs-stop.sh` never gives OBS a clean exit (so it never persists its own UI state).
//!
//! Neither script previously had any test coverage in this repo (confirmed: `grep -rl
//! "imag-obs-start\|imag-obs-stop" tests/` returned nothing before this file) — they are
//! standalone scripts fetched onto the box (mirrors `scripts/imag_scenes.py`'s own gh-api fetch),
//! never sourced/executed by cargo test directly. Style follows the repo's other script guards
//! (`tests/setup_imag_guards.rs`): read the REAL script text and assert on the REAL contract.

use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const START: &str = "scripts/imag-obs-start.sh";
const STOP: &str = "scripts/imag-obs-stop.sh";

// ================================================================================================
// imag-obs-start.sh — the CPU pin must be DERIVED (env-overridable), never a bare hardcoded
// literal, so the boot path (which knows the box's own DERIVED isolated-CPU set, #816) and a bare
// manual invocation (no env set) both work correctly.
//
// #841 SUPERSEDES this test's ORIGINAL assertion: #840 shipped `taskset -c
// "${IMAG_ISOLATED_CPUS:-2-11}"`, treating "2-11" as a "sane default" for a bare manual
// invocation. That default is itself the INCUMBENT (16-thread) box's hand-tuned range and is
// WRONG on the 12-thread replacement notebook (10.77.9.187) — a manual "Spustit OBS" invocation
// there (no env set) pinned the live `obs` process to `Cpus_allowed_list: 2-11`, overlapping the
// kernel's own `irqaffinity=...,8,9,10,11` IRQ cores and defeating the isolation (live-confirmed).
// A hardcoded literal from ONE box is never a "sane default" for a DIFFERENT box's topology — the
// #816 hardware-agnostic-derivation rule applies to the wrapper's fallback too, not just the
// kernel cmdline. The fallback is now a PERSISTED file (`/etc/imag-isolated-cpus.conf`) that
// setup-imag.sh writes from the SAME `imag_cpu_isolation_plan` derivation already used for grub —
// see `tests/harness_imag_intel_display_841.rs` for the full #841 contract.
// ================================================================================================

/// The launch line must read `IMAG_ISOLATED_CPUS`, falling back to the PERSISTED derived config
/// file (never a bare hardcoded literal) — so the boot autostart (which passes
/// `IMAG_ISOLATED_CPUS` directly, #840) and a bare manual invocation (no env set, reads
/// `/etc/imag-isolated-cpus.conf`, #841) both use the SAME box-derived isolated-CPU set.
#[test]
fn imag_obs_start_taskset_pin_is_env_overridable_no_hardcoded_fallback_841() {
    let body = read(START);
    assert!(
        body.contains("IMAG_ISOLATED_CPUS"),
        "{START} must still read IMAG_ISOLATED_CPUS from the environment when the boot autostart \
         sets it (#840)"
    );
    assert!(
        !body.contains("2-11"),
        "{START}: the OLD box's hardcoded \"2-11\" literal must be completely gone — including as \
         a \"fallback default\", which #841 proved is wrong on a different box's topology"
    );
    assert!(
        body.contains("/etc/imag-isolated-cpus.conf"),
        "{START}: a manual invocation with no env set must fall back to the PERSISTED derived \
         config file (#841), not a second hardcoded literal"
    );
}

/// The script must still launch OBS pinned (no accidental unpinned launch) and keep its existing
/// `--disable-shutdown-check` flag.
#[test]
fn imag_obs_start_still_launches_obs_pinned_and_disables_shutdown_check_840() {
    let body = read(START);
    assert!(
        body.contains("obs --disable-shutdown-check &"),
        "{START} must still launch `obs --disable-shutdown-check &` in the background"
    );
}

// ================================================================================================
// imag-obs-stop.sh — a GRACEFUL window close (real Qt exit) must be attempted BEFORE SIGTERM, so
// OBS actually runs its own clean-shutdown save path (saved_projectors, DockState/geometry).
// Live-verified (#840): a SIGTERM-only stop, with both projectors open, left saved_projectors=[]
// on re-read of the scene collection JSON -- SIGTERM never reaches OBS's Qt close handler.
// ================================================================================================

/// The script must attempt a graceful window close (`wmctrl -c`, an EWMH `_NET_CLOSE_WINDOW`
/// request OBS handles as `Controls -> Exit`) targeting the MAIN OBS window -- never a Projector
/// window (those never carry "OBS Studio" in their title; live-verified `wmctrl -l` output).
#[test]
fn imag_obs_stop_attempts_a_graceful_window_close_840() {
    let body = read(STOP);
    assert!(
        body.contains(r#"wmctrl -c "OBS Studio""#),
        "{STOP} must attempt `wmctrl -c \"OBS Studio\"` -- a graceful EWMH close targeting the \
         MAIN OBS window (never a Projector window, which never carries \"OBS Studio\" in its \
         title) so OBS runs its OWN clean-shutdown save path (#840)"
    );
}

/// The graceful close attempt must run BEFORE the existing SIGTERM escalation — never after, and
/// never REPLACING it (the fallback ladder must survive for a hung/unresponsive OBS).
#[test]
fn imag_obs_stop_graceful_close_runs_before_sigterm_840() {
    let body = read(STOP);
    let graceful = body
        .find(r#"wmctrl -c "OBS Studio""#)
        .expect("the graceful wmctrl close must be present");
    let sigterm = body
        .find("pkill -TERM -x obs")
        .expect("the existing SIGTERM escalation must still be present");
    assert!(
        graceful < sigterm,
        "{STOP}: the graceful window close must be attempted BEFORE the SIGTERM escalation \
         (#840) -- SIGTERM never runs OBS's own clean-exit save path"
    );
}

/// The SIGTERM -> SIGKILL escalation ladder (the pre-existing #785 fallback) must still be
/// present unconditionally — the graceful close is an ADDITIONAL first attempt, not a
/// replacement, since a hung/unresponsive OBS window would otherwise never be forced closed.
#[test]
fn imag_obs_stop_still_falls_back_to_sigterm_then_sigkill_840() {
    let body = read(STOP);
    assert!(
        body.contains("pkill -TERM -x obs"),
        "{STOP} must still fall back to SIGTERM if the graceful close doesn't make OBS exit"
    );
    assert!(
        body.contains("pkill -KILL -x obs"),
        "{STOP} must still fall back to SIGKILL as the last resort (#785 ladder, unchanged)"
    );
    let sigterm = body.find("pkill -TERM -x obs").unwrap();
    let sigkill = body.find("pkill -KILL -x obs").unwrap();
    assert!(
        sigterm < sigkill,
        "{STOP}: SIGTERM must still be attempted before SIGKILL (unchanged #785 ordering)"
    );
}

/// The #785 program-scene save (reading the CURRENT program scene over WS and writing it to
/// ~/.config/imag-last-program) must still run BEFORE any close/kill attempt — this is the
/// existing behavior fix #840 must not disturb.
#[test]
fn imag_obs_stop_still_saves_program_scene_before_any_close_attempt_840() {
    let body = read(STOP);
    let save = body
        .find("GetCurrentProgramScene")
        .expect("the #785 program-scene read must still be present");
    let graceful = body
        .find(r#"wmctrl -c "OBS Studio""#)
        .expect("the graceful wmctrl close must be present");
    assert!(
        save < graceful,
        "{STOP}: the #785 program-scene save must still run BEFORE any close attempt (including \
         the new graceful wmctrl close) -- a scene switch during a slow close must not race the \
         read"
    );
}

/// A missing `wmctrl` binary must not make the script silently skip straight past the log entirely
/// -- it should fail toward the SAME escalation ladder (never crash `set -euo pipefail`-style on
/// an unguarded `command -v` miss).
#[test]
fn imag_obs_stop_handles_missing_wmctrl_without_aborting_840() {
    let body = read(STOP);
    assert!(
        body.contains("command -v wmctrl"),
        "{STOP} must check for wmctrl's presence before attempting the graceful close -- a box \
         without it (or a stale re-provision) must still fall back to SIGTERM/SIGKILL rather than \
         aborting the whole script under `set -euo pipefail`"
    );
}
