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

// ================================================================================================
// #882 -- imag-obs-start.sh: build-sha-at-startup logging + wait-based supervision (so a NEW
// systemd unit tracking THIS script's own pid can Restart=on-failure on a genuine segfault,
// without reintroducing the stood-down watchdog's "fights the operator on a manual quit" bug --
// see the design comment on #882 for the full history/reasoning).
// ================================================================================================

/// The script must log the deployed genlock build sha at every start -- so a future incident
/// never again needs cross-referencing git history + a separately-read file to know what's
/// running (the #882 archaeology this ticket's own investigation had to do by hand).
#[test]
fn imag_obs_start_logs_the_genlock_build_sha_at_startup_882() {
    let body = read(START);
    assert!(
        body.contains("/opt/obs-genlock/GENLOCK_BUILD_SHA.txt"),
        "{START} must read /opt/obs-genlock/GENLOCK_BUILD_SHA.txt and log it at startup (#882)"
    );
}

/// The script must capture obs's own PID right after backgrounding it, `wait` on it at the very
/// end (AFTER the seed/projector steps), and exit with obs's OWN exit status -- this makes obs
/// itself the tracked "main process" for a `Type=simple` systemd unit, so `Restart=on-failure`
/// fires exactly on an abnormal (segfault/signal) death and never on a clean `exit(0)`.
#[test]
fn imag_obs_start_waits_on_the_backgrounded_obs_pid_and_propagates_its_exit_882() {
    let body = read(START);
    let launch = body
        .find("obs --disable-shutdown-check &")
        .expect("the pinned launch line must still be present (#840)");
    let wait_pos = body
        .find("wait \"$OBS_PID\"")
        .expect("{START} must `wait \"$OBS_PID\"` on the backgrounded obs process (#882)");
    assert!(
        launch < wait_pos,
        "{START}: the wait must come AFTER the launch line, never before"
    );
    let seed = body
        .find("OK: OBS bezi")
        .expect("the existing seed-complete echo must still be present");
    assert!(
        seed < wait_pos,
        "{START}: the wait must come AFTER the seed/projector steps finish, not race them"
    );
    assert!(
        body.contains("exit \"$OBS_EXIT\""),
        "{START} must exit with obs's OWN propagated exit status (#882) -- a clean exit(0) must \
         never look like a failure to systemd's Restart=on-failure"
    );
}

/// The idempotent "already running" early-exit path must be UNCHANGED (still exits 0 immediately,
/// never waits on a process that isn't this invocation's own child).
#[test]
fn imag_obs_start_idempotent_already_running_path_is_unchanged_882() {
    let body = read(START);
    assert!(
        body.contains("OBS uz bezi -- nic nerobim."),
        "{START} must still print the existing idempotent no-op message"
    );
    let idempotent = body
        .find("OBS uz bezi")
        .expect("idempotent message present");
    let launch = body
        .find("obs --disable-shutdown-check &")
        .expect("launch line present");
    assert!(
        idempotent < launch,
        "{START}: the idempotent already-running check must still run BEFORE the launch line"
    );
}

// ================================================================================================
// #882 -- imag-obs-stop.sh: route a MANUAL stop through `systemctl --user stop imag-obs.service`
// FIRST when that unit is active, so systemd treats the stop as AUTHORIZED and never fights it
// with Restart=on-failure (the exact historical bug: the stood-down imag-obs-watchdog.py
// relaunched OBS after every deliberate manual quit -- issue 788). `--exec-stop` is the mode the
// unit's own ExecStop= passes, which must SKIP the delegation (systemd is already the one
// stopping it -- delegating again would recurse) and run the existing ladder directly, UNCHANGED.
// ================================================================================================

/// A plain (no `--exec-stop`) invocation must check whether imag-obs.service is active and, if
/// so, delegate to `systemctl --user stop` -- BEFORE the pre-existing program-scene-save /
/// graceful-close / SIGTERM / SIGKILL ladder even runs.
#[test]
fn imag_obs_stop_delegates_to_systemctl_when_the_unit_is_active_882() {
    let body = read(STOP);
    assert!(
        body.contains("systemctl --user is-active --quiet imag-obs.service"),
        "{STOP} must check whether imag-obs.service is active (#882)"
    );
    assert!(
        body.contains("systemctl --user stop imag-obs.service"),
        "{STOP} must delegate to `systemctl --user stop` when the unit is active (#882) -- this \
         is what makes systemd treat the stop as AUTHORIZED and suppress Restart=on-failure"
    );
    let delegate = body
        .find("systemctl --user stop imag-obs.service")
        .expect("delegation call present");
    let save = body
        .find("GetCurrentProgramScene")
        .expect("the pre-existing #785 program-scene save must still be present");
    assert!(
        delegate < save,
        "{STOP}: the systemctl delegation check must run BEFORE the existing ladder (#882)"
    );
}

/// `--exec-stop` mode (the unit's own ExecStop=) must SKIP the systemctl delegation -- otherwise
/// systemd stopping the unit would call this script, which would call `systemctl stop` again,
/// which is already in progress (recursion / a no-op at best, a hang at worst).
#[test]
fn imag_obs_stop_exec_stop_mode_skips_the_systemctl_delegation_882() {
    let body = read(STOP);
    assert!(
        body.contains("--exec-stop"),
        "{STOP} must recognize an --exec-stop flag (#882) -- the mode the unit's own ExecStop= \
         passes so it never re-delegates to systemctl (which is already stopping it)"
    );
}

// ================================================================================================
// #1156 — an IMPORT PREFLIGHT before the OBS launch. imag-obs-start.sh's ExecStart launches OBS
// and only AFTER that seeds via `python3 imag_scenes.py --bootstrap`, so a missing imported sibling
// (the #1143 imag_record_encoder setup-imag.sh forgot to install) killed a HEALTHY OBS 1737× in an
// 8.5h Restart-loop. Preflighting the seed's import chain BEFORE launching OBS fails the unit
// cleanly on a broken import instead of flapping OBS up/down on the live IMAG projection.
// ================================================================================================

/// A `python3 -c "... import imag_scenes"` preflight must run BEFORE the `obs ... &` launch, add
/// /usr/local/bin to sys.path (the REAL on-box install location), and on failure echo a NAMED FAIL
/// line + exit 1 — so a missing sibling fails the unit without ever launching (then looping) OBS.
#[test]
fn imag_obs_start_import_preflights_before_launching_obs_1156() {
    let body = read(START);
    let preflight = body
        .find("import imag_scenes")
        .expect("{START} must run a `python3 -c ... import imag_scenes` preflight (#1156)");
    let launch = body
        .find("obs --disable-shutdown-check &")
        .expect("{START} must still launch obs (#840)");
    assert!(
        preflight < launch,
        "{START}: the import preflight must run BEFORE the obs launch (#1156) — otherwise a broken \
         import still launches OBS and then seed-fails, Restart-looping a healthy OBS 1700×"
    );
    assert!(
        body.contains("import sys") && body.contains("/usr/local/bin"),
        "{START}: the preflight must insert /usr/local/bin into sys.path so it validates the REAL \
         on-box install location's import chain (#1156)"
    );
    assert!(
        body.contains("FAIL: imag_scenes import preflight"),
        "{START}: a failed preflight must echo a NAMED FAIL line so the unit's log names the cause \
         (#1156)"
    );
}
