//! #1060 — dev1-side fresh-OBS-start burn-reconcile watchdog
//! (scripts/obs-burn-reconcile-watchdog.sh + scripts/lib/obs-burn-reconcile-decision.sh).
//!
//! Background: issue 1057 closed the burn-resurrection window for the DELIBERATE dev1-driven
//! relaunch (launch-obs-genlock.sh's PLAN now directs a post-launch obs_burn_filter.py sweep-off).
//! Still open — the UNATTENDED strih/stream OBS start paths (box boot autostart, NL_STARTUP.ahk
//! obs64 auto-respawn, the issue-411 self-heal Task-Scheduler relaunch, all reusing
//! launch-obs-genlock.sh's emitted PowerShell which never touches the burn), where a saved
//! genlock_burn=true reloads onto the LIVE program and there is no on-box python/WS client to
//! clear it. This ONE dev1 watchdog covers all three at once because it keys on the OBS RESTART
//! (renderTotalFrames reset over the SAME WS obs_burn_filter speaks), not on which path caused it.
//!
//! Pure-shell / content tests — no rig, no real OBS/WS, no ssh. The pure decision lib is sourced
//! and its truth table asserted directly (mirrors harness_obs_session_watchdog_979.rs's style +
//! scripts/lib/obs-watchdog-decision.sh's own test discipline).

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const WATCHDOG: &str = "scripts/obs-burn-reconcile-watchdog.sh";
const DECISION_LIB: &str = "scripts/lib/obs-burn-reconcile-decision.sh";
const SERVICE_UNIT: &str = "systemd/obs-burn-reconcile-watchdog.service";
const TIMER_UNIT: &str = "systemd/obs-burn-reconcile-watchdog.timer";
const README: &str = "systemd/obs-burn-reconcile-watchdog.README.md";

// ================================================================================================
// Pure decision lib: source it and assert the full truth table (the CI-side twin of
// tests/python/test_obs_burn_reconcile_decision.py).
// ================================================================================================

fn decide(fresh: u8, coordinated: u8, burn: u8) -> String {
    let lib = manifest_dir().join(DECISION_LIB);
    let script = format!(
        "set -uo pipefail\n. \"{}\"\nobs_burn_reconcile_decide {fresh} {coordinated} {burn}\n",
        lib.display()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run decide");
    assert!(
        out.status.success(),
        "decide exited nonzero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn is_fresh_start(prev: &str, cur: &str) -> i32 {
    let lib = manifest_dir().join(DECISION_LIB);
    let script = format!(
        "set -uo pipefail\n. \"{}\"\nobs_burn_reconcile_is_fresh_start \"{prev}\" \"{cur}\"\n",
        lib.display()
    );
    Command::new("bash")
        .arg("-c")
        .arg(&script)
        .status()
        .expect("run is_fresh_start")
        .code()
        .unwrap_or(-1)
}

#[test]
fn not_a_fresh_start_is_always_noop() {
    // The KEY guard: a persistent TEST-mode burn (no restart) is never cleared, regardless of
    // coordination or whether a burn is present.
    for coord in [0u8, 1] {
        for burn in [0u8, 1] {
            assert_eq!(decide(0, coord, burn), "NOOP", "coord={coord} burn={burn}");
        }
    }
}

#[test]
fn fresh_start_but_coordinated_defers() {
    assert_eq!(decide(1, 1, 0), "DEFER");
    assert_eq!(decide(1, 1, 1), "DEFER");
}

#[test]
fn fresh_start_uncoordinated_with_burn_sweeps() {
    assert_eq!(decide(1, 0, 1), "SWEEP");
}

#[test]
fn fresh_start_uncoordinated_no_burn_is_clean() {
    assert_eq!(decide(1, 0, 0), "CLEAN");
}

#[test]
fn fresh_start_detection_tracks_render_total_frames_reset() {
    assert_eq!(is_fresh_start("", "5000"), 0, "unknown baseline => reconcile once");
    assert_eq!(is_fresh_start("500000", "1200"), 0, "counter reset => restart");
    assert_eq!(is_fresh_start("1200", "500000"), 1, "monotone climb => same session");
    assert_eq!(is_fresh_start("500000", "500000"), 1, "steady => same session");
    assert_eq!(is_fresh_start("500000", ""), 1, "unreadable current => not provably fresh");
}

// ================================================================================================
// Content / wiring: ONE shared mechanism — never a second detector, never a hand-rolled WS client.
// ================================================================================================

#[test]
fn watchdog_uses_the_pure_decision_lib_never_inlined_logic() {
    let body = read(WATCHDOG);
    assert!(
        body.contains("lib/obs-burn-reconcile-decision.sh"),
        "must source the pure decision lib (obs_burn_reconcile_decide / _is_fresh_start)"
    );
    assert!(
        body.contains("obs_burn_reconcile_decide"),
        "must route the sweep/defer/noop decision through the pure function, not inline if/else"
    );
}

#[test]
fn watchdog_reuses_existing_coordination_primitives_never_a_new_one() {
    let body = read(WATCHDOG);
    assert!(
        body.contains("lib/rig-heartbeat.sh") && body.contains("rig_heartbeat_active"),
        "coordination must reuse the #281 rig-active heartbeat (a live TEST/E2E harness), \
         never a new signal"
    );
    assert!(
        body.contains("lib/rig-lease.sh"),
        "coordination must ALSO honor the #830 rig lease (a live CI gate holding the rig)"
    );
}

#[test]
fn watchdog_routes_all_burn_and_ws_interaction_through_obs_burn_filter() {
    let body = read(WATCHDOG);
    // The fresh-start signal, the burn presence check, and the clear ALL go through the ONE
    // existing WS/enumerator tool — never a hand-rolled on-box WS client (issue 866 rejected that).
    assert!(body.contains("obs_burn_filter.py"), "must use obs_burn_filter.py");
    assert!(body.contains("session-probe"), "fresh-start signal via obs_burn_filter session-probe");
    assert!(body.contains("sweep-check"), "burn presence via obs_burn_filter sweep-check");
    assert!(body.contains("sweep-off"), "the clear via obs_burn_filter sweep-off (#938/#1011)");
}

#[test]
fn watchdog_fails_closed_on_an_unenumerable_box() {
    let body = read(WATCHDOG);
    // A failed GetInputList (sweep-check exit 2 = SWEEP_ENUM_FAILED) must NOT read as "clean" — an
    // out-of-set burn would be invisible (burn-target-enumeration rule, guard class #246/#844).
    assert!(
        body.contains("SWEEP_ENUM_FAILED") || body.contains("== 2") || body.contains("-eq 2"),
        "must handle sweep-check's enum-failure (exit 2) fail-closed, never as clean"
    );
}

#[test]
fn watchdog_fires_through_the_same_airuleset_notify_path() {
    let body = read(WATCHDOG);
    assert!(
        body.contains("airuleset.py") && body.contains("notify --body"),
        "must alert through the SAME airuleset.py notify path #391/#979 use"
    );
}

#[test]
fn watchdog_state_file_default_is_its_own() {
    let body = read(WATCHDOG);
    assert!(
        body.contains("camera-box-obs-burn-reconcile-watchdog.state"),
        "must use its OWN default state file, distinct from #391/#979's — it persists the \
         per-box renderTotalFrames baseline, not their confirm/throttle counters"
    );
}

#[test]
fn watchdog_processes_both_broadcast_boxes() {
    let body = read(WATCHDOG);
    assert!(
        body.contains("strih") && body.contains("stream"),
        "main() must reconcile both strih and stream"
    );
}

#[test]
fn watchdog_supports_dry_run() {
    let body = read(WATCHDOG);
    assert!(
        body.contains("--dry-run") && body.contains("DRY_RUN"),
        "must support --dry-run (measure + decide + LOG only, never sweep/alert) for live-verify"
    );
}

// ================================================================================================
// systemd units: committed but SHIP DISABLED (supervisor installs + live-verifies), same as
// #391/#979.
// ================================================================================================

#[test]
fn service_unit_execs_the_watchdog_script() {
    let svc = read(SERVICE_UNIT);
    assert!(svc.contains("obs-burn-reconcile-watchdog.sh"), "ExecStart must run the watchdog");
    assert!(svc.contains("Type=oneshot"), "one measure/decide/reconcile pass per timer tick");
}

#[test]
fn timer_unit_is_installable_and_periodic() {
    let timer = read(TIMER_UNIT);
    assert!(timer.contains("OnUnitActiveSec="), "must fire periodically");
    assert!(
        timer.contains("[Install]") && timer.contains("WantedBy=timers.target"),
        "must be enableable as a --user timer"
    );
}

#[test]
fn readme_documents_ships_disabled_and_supervisor_verify() {
    let doc = read(README).to_lowercase();
    assert!(doc.contains("disabled"), "README must state it ships DISABLED");
    assert!(
        doc.contains("supervisor") && doc.contains("live-verif"),
        "README must state the supervisor installs + live-verifies before enabling"
    );
}
