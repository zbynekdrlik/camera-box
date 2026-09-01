//! issue 1268 (branch A) — pure-function + runner + wiring guard for
//! `scripts/lib/imag-power-stepdown-wait.sh`, the bounded guard-state-aware PRE-GATE WAIT that runs
//! BEFORE the imag render-budget family (`[4d1/8]` MV-fps floor preflight + `[4d/8]` render-budget
//! gate) so those STRICT reads never land inside a 25 W thermal step-down episode and falsely abort
//! a ~40-min run.
//!
//! Root cause (issue 1268): the #1162 imag-nb holds 45 W only intermittently; the
//! imag-power-envelope-guard (#1040) steps PL1 down to 25 W ~18x/day (~20% duty, median ~12 min). At
//! 25 W the iGPU is pinned ~400 MHz -> a burns-ON render read is activeFps~57.7 / 15.6 ms, under the
//! 58 fps / 16.67 ms budget -> a false abort. The gate thresholds are CORRECT; the defect is WHEN
//! imag is read. So the wait WAITS on the MEASURED clamp signal to clear (RESTORE +
//! throttle_reason_pl1=0), never a threshold relaxation and never a blind sleep — the same
//! precondition-wait shape as `genlock-settle.sh` (issue 1221) and the DanteSync settle.
//!
//! THE LOAD-BEARING DECISION: `throttle_reason_pl1` (a world-readable i915 sysfs) is the PRIMARY
//! clamp signal because the guard's /run state file is root-owned and NOT readable to the non-root
//! E2E ssh (verified live 2026-09-02). The guard `STEPPED=` state (parsed by the SHARED
//! imag_power_guard_stepped_from_state, REUSED from imag-power-envelope.sh) is a supplement. The
//! verdict treats pl1=1 OR guard STEPPED as `clamped`; `clear` needs guard not-stepped AND pl1=0;
//! everything else is `unknown` -> the caller FAILS OPEN on the WAIT (proceeds; the gate decides).
//!
//! Same convention as `tests/harness_genlock_settle_1221.rs`: source the REAL lib (source-only, no
//! side effects) and exercise the pure functions + the injectable-seam runner directly. RED before
//! the lib exists (the file is absent, every test fails); GREEN after. `cargo` does NOT run locally
//! here (Tier-0, build-ok DISABLED #557) -- the observable local red->green is a bash replica
//! sourcing the lib; CI runs these assertions.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/imag-power-stepdown-wait.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the REAL lib and run `body` against its pure functions. Returns (exit, stdout, stderr).
fn run_sourced(body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$LIB\"\n{body}", body = body);
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", lib())
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn stdout_of(body: &str) -> String {
    let (rc, out, err) = run_sourced(body);
    assert_eq!(rc, 0, "body failed (rc={rc}): {body}\nstderr={err}");
    out.trim().to_string()
}

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

// ---------------------------------------------------------------------------------------------
// lib shape — the pure functions + runner must be defined, and the SHARED reused parser present
// ---------------------------------------------------------------------------------------------
#[test]
fn lib_defines_the_functions() {
    for f in [
        "imag_power_stepdown_remote_snippet",
        "imag_power_stepdown_pl1_from_block",
        "imag_power_stepdown_state",
        "imag_power_stepdown_verdict_from_block",
        "imag_power_stepdown_write_report",
        "imag_power_stepdown_wait",
        // REUSED from the sibling shared lib (sourced by this lib), never a second driftable copy:
        "imag_power_guard_stepped_from_state",
    ] {
        let out = stdout_of(&format!("type {f} >/dev/null 2>&1 && echo DEFINED"));
        assert_eq!(out, "DEFINED", "{f} must be defined (directly or via the sourced shared lib)");
    }
}

/// The lib must SOURCE the shared imag-power-envelope lib (reuse the state parser + the guard
/// state-file path constant), never re-copy them.
#[test]
fn lib_sources_the_shared_power_envelope_lib() {
    let src = std::fs::read_to_string(lib()).unwrap();
    assert!(
        src.contains("imag-power-envelope.sh"),
        "the lib must source scripts/lib/imag-power-envelope.sh to reuse the shared state parser + path constant"
    );
    // The remote snippet must read throttle_reason_pl1 by IDENTITY GLOB (never a hardcoded cardN).
    assert!(
        src.contains("/sys/class/drm/card*/gt/gt*/throttle_reason_pl1"),
        "the remote snippet must identity-glob throttle_reason_pl1 across card* (never a hardcoded cardN)"
    );
}

// ---------------------------------------------------------------------------------------------
// imag_power_stepdown_pl1_from_block — digits from the first IMAGPWR_PL1| line
// ---------------------------------------------------------------------------------------------
fn pl1(block: &str) -> String {
    stdout_of(&format!("imag_power_stepdown_pl1_from_block '{block}'"))
}

#[test]
fn pl1_from_block_reads_the_marker_line() {
    assert_eq!(pl1("IMAGPWR_PL1|1"), "1");
    assert_eq!(pl1("IMAGPWR_PL1|0"), "0");
    assert_eq!(pl1(""), ""); // empty block
    assert_eq!(pl1("IMAGPWR_PL1|x"), ""); // non-numeric -> empty
    assert_eq!(pl1("HOT=0\nSTEPPED=1"), ""); // no marker line -> empty
    // a full block: pl1 line first, then the guard state body -> reads only the pl1 line
    assert_eq!(
        stdout_of("imag_power_stepdown_pl1_from_block $'IMAGPWR_PL1|1\\nHOT=1\\nSTEPPED=1\\nGUARD_STEPDOWN_W=25'"),
        "1"
    );
}

// ---------------------------------------------------------------------------------------------
// imag_power_stepdown_state — clamped / clear / unknown
// ---------------------------------------------------------------------------------------------
fn state(g: &str, p: &str) -> String {
    stdout_of(&format!("imag_power_stepdown_state '{g}' '{p}'"))
}

#[test]
fn state_core_classifies_the_two_signals() {
    // clamped: guard STEPPED, OR pl1==1 (either alone).
    assert_eq!(state("stepped", "0"), "clamped");
    assert_eq!(state("stepped", ""), "clamped");
    assert_eq!(state("not-stepped", "1"), "clamped"); // #880 silent punit clamp, no guard step-down
    assert_eq!(state("unknown", "1"), "clamped");
    // clear: guard not-stepped AND pl1==0 (RESTORE + throttle 0).
    assert_eq!(state("not-stepped", "0"), "clear");
    // unknown: an unreadable read -> fail-open (proceed).
    assert_eq!(state("unknown", "0"), "unknown"); // the live reality (state file absent) + pl1 clean
    assert_eq!(state("unknown", ""), "unknown");
    assert_eq!(state("not-stepped", ""), "unknown"); // pl1 unreadable -> cannot confirm clear
}

// ---------------------------------------------------------------------------------------------
// imag_power_stepdown_verdict_from_block — fuse the pl1 sample with the shared guard parser
// ---------------------------------------------------------------------------------------------
fn verdict(block: &str) -> String {
    stdout_of(&format!("imag_power_stepdown_verdict_from_block \"$(printf '%b' '{block}')\""))
}

#[test]
fn verdict_from_block_fuses_pl1_and_guard_state() {
    // pl1=1, no state file (the live reality) -> clamped
    assert_eq!(verdict("IMAGPWR_PL1|1"), "clamped");
    // pl1=0 but guard STEPPED=1 -> clamped (wait for RESTORE)
    assert_eq!(verdict("IMAGPWR_PL1|0\\nSTEPPED=1\\nGUARD_STEPDOWN_W=25"), "clamped");
    // pl1=0 and guard not-stepped -> clear
    assert_eq!(verdict("IMAGPWR_PL1|0\\nSTEPPED=0"), "clear");
    // pl1=0, no state file -> unknown (proceed; the state file is unreadable to the non-root ssh)
    assert_eq!(verdict("IMAGPWR_PL1|0"), "unknown");
    // empty (ssh failed) -> unknown
    assert_eq!(verdict(""), "unknown");
}

// ---------------------------------------------------------------------------------------------
// imag_power_stepdown_wait — the runner (injectable reader/clock/sleep seams; NO ssh, NO waiting)
// ---------------------------------------------------------------------------------------------
/// Run the runner with a scripted snapshot sequence + a fake clock, capturing stdout+stderr and rc.
/// The runner returns 0 to PROCEED and 1 to ABORT (a confirmed clamp held the whole budget).
fn run_runner(setup: &str, call: &str) -> (i32, String) {
    let body = format!(
        "{setup}\nexport IMAG_POWER_STEPDOWN_SLEEP_CMD=':'\nOUT=$({call} 2>&1); RC=$?\nprintf '%s\\n' \"$OUT\"\nexit $RC",
        setup = setup,
        call = call
    );
    // rc may be 0 (proceed) or 1 (abort) — do NOT assert success here; return both.
    let out = Command::new("bash")
        .arg("-c")
        .arg(&body)
        .env("LIB", lib())
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn runner_proceeds_immediately_when_no_episode() {
    // first read pl1=0, no state file -> unknown -> proceed, waited 0.
    let setup = "export IMAG_POWER_STEPDOWN_READER_CMD='printf \"IMAGPWR_PL1|0\\n\"' IMAG_POWER_STEPDOWN_NOW_CMD='echo 100'";
    let (rc, out) = run_runner(setup, "imag_power_stepdown_wait u p h 1200 30 ''");
    assert_eq!(rc, 0, "no episode must proceed (rc0): {out}");
    assert!(out.contains("no 25W clamp episode") && out.contains("waited 0s"), "got: {out}");
}

#[test]
fn runner_waits_then_proceeds_when_the_clamp_clears() {
    // pass0/1 clamped (pl1=1), pass2 clear (pl1=0 STEPPED=0) -> proceed rc0.
    let setup = r#"
W="$(mktemp -d)"; echo 0 > "$W/idx"; echo 0 > "$W/clk"
printf 'IMAGPWR_PL1|1\n' > "$W/s0"; cp "$W/s0" "$W/s1"
printf 'IMAGPWR_PL1|0\nSTEPPED=0\n' > "$W/s2"; cp "$W/s2" "$W/s3"
export IMAG_POWER_STEPDOWN_READER_CMD='i=$(cat "'"$W"'/idx"); f="'"$W"'/s$i"; [ -f "$f" ] || f="'"$W"'/s3"; echo $((i+1)) > "'"$W"'/idx"; cat "$f"'
export IMAG_POWER_STEPDOWN_NOW_CMD='c=$(cat "'"$W"'/clk"); echo $((c+1)) > "'"$W"'/clk"; echo "$c"'
"#;
    let (rc, out) = run_runner(setup, "imag_power_stepdown_wait u p h 1200 5 ''");
    assert_eq!(rc, 0, "a clearing clamp must proceed (rc0): {out}");
    assert!(out.contains("clamp cleared") && out.contains("state=clear"), "got: {out}");
    assert!(!out.contains("ERROR:"), "a clearing clamp must NOT abort: {out}");
}

#[test]
fn runner_aborts_on_budget_exhaustion_naming_the_duration() {
    // clamp never clears (pl1=1 every read), clock jumps 5/read, budget 20 -> ABORT rc1.
    let setup = r#"
W="$(mktemp -d)"; echo 0 > "$W/clk"
export IMAG_POWER_STEPDOWN_READER_CMD='printf "IMAGPWR_PL1|1\n"'
export IMAG_POWER_STEPDOWN_NOW_CMD='c=$(cat "'"$W"'/clk"); echo $((c+5)) > "'"$W"'/clk"; echo "$c"'
"#;
    let (rc, out) = run_runner(setup, "imag_power_stepdown_wait u p h 20 1 ''");
    assert_eq!(rc, 1, "a stuck clamp at budget must ABORT (rc1): {out}");
    assert!(
        out.contains("ERROR:") && out.contains("STILL in the 25W thermal step-down clamp") && out.contains("aborting BEFORE"),
        "the abort must name the clamp + the duration (never a silent pass): {out}"
    );
}

#[test]
fn runner_fails_open_and_proceeds_when_a_read_goes_unreadable() {
    // pass0 clamped (pl1=1), pass1 EMPTY read (ssh hiccup) -> unknown -> fail-open proceed rc0.
    let setup = r#"
W="$(mktemp -d)"; echo 0 > "$W/idx"; echo 0 > "$W/clk"
printf 'IMAGPWR_PL1|1\n' > "$W/t0"; : > "$W/t1"
export IMAG_POWER_STEPDOWN_READER_CMD='i=$(cat "'"$W"'/idx"); f="'"$W"'/t$i"; [ -f "$f" ] || f="'"$W"'/t1"; echo $((i+1)) > "'"$W"'/idx"; cat "$f"'
export IMAG_POWER_STEPDOWN_NOW_CMD='c=$(cat "'"$W"'/clk"); echo $((c+1)) > "'"$W"'/clk"; echo "$c"'
"#;
    let (rc, out) = run_runner(setup, "imag_power_stepdown_wait u p h 1200 5 ''");
    assert_eq!(rc, 0, "an unreadable read must fail-open (proceed rc0), never abort: {out}");
    assert!(out.contains("state=unknown") && !out.contains("ERROR:"), "got: {out}");
}

#[test]
fn runner_cannot_hang_with_a_stuck_clock_pass_ceiling_terminates() {
    // clamp forever + a clock frozen at 0 -> only the hard pass ceiling can terminate (ABORT rc1).
    let setup = "export IMAG_POWER_STEPDOWN_READER_CMD='printf \"IMAGPWR_PL1|1\\n\"' IMAG_POWER_STEPDOWN_NOW_CMD='echo 0' IMAG_POWER_STEPDOWN_MAX_PASSES=4";
    let (rc, out) = run_runner(setup, "imag_power_stepdown_wait u p h 100000 0 ''");
    assert_eq!(rc, 1, "a stuck clock must terminate via the pass ceiling (rc1): {out}");
    assert!(out.contains("ERROR:") && out.contains("aborting BEFORE"), "got: {out}");
}

#[test]
fn runner_sanitizes_malformed_numeric_args_never_infinite() {
    // budget/poll garbage -> sanitized to defaults; clamp never clears; a big clock jump -> abort rc1.
    let setup = r#"
W="$(mktemp -d)"; echo 0 > "$W/clk"
export IMAG_POWER_STEPDOWN_READER_CMD='printf "IMAGPWR_PL1|1\n"'
export IMAG_POWER_STEPDOWN_NOW_CMD='c=$(cat "'"$W"'/clk"); echo $((c+700)) > "'"$W"'/clk"; echo "$c"'
"#;
    let (rc, out) = run_runner(setup, "imag_power_stepdown_wait u p h xyz bogus ''");
    assert_eq!(rc, 1, "malformed args must sanitize (default budget 1200) and still terminate: {out}");
    assert!(out.contains("ERROR:"), "got: {out}");
}

#[test]
fn runner_writes_the_report_only_sidecar() {
    // a clearing clamp -> the sidecar records the waited seconds + the state at gate time.
    let harness = r#"
        . "$LIB"
        W="$(mktemp -d)"; echo 0 > "$W/idx"; echo 0 > "$W/clk"; RF="$W/report.txt"
        printf 'IMAGPWR_PL1|1\n' > "$W/s0"; printf 'IMAGPWR_PL1|0\nSTEPPED=0\n' > "$W/s1"; cp "$W/s1" "$W/s2"
        export IMAG_POWER_STEPDOWN_READER_CMD='i=$(cat "'"$W"'/idx"); f="'"$W"'/s$i"; [ -f "$f" ] || f="'"$W"'/s2"; echo $((i+1)) > "'"$W"'/idx"; cat "$f"'
        export IMAG_POWER_STEPDOWN_NOW_CMD='c=$(cat "'"$W"'/clk"); echo $((c+1)) > "'"$W"'/clk"; echo "$c"'
        export IMAG_POWER_STEPDOWN_SLEEP_CMD=':'
        imag_power_stepdown_wait u p h 1200 5 "$RF" >/dev/null 2>&1
        cat "$RF"
    "#;
    let out = Command::new("bash")
        .arg("-c")
        .arg(harness)
        .env("LIB", lib())
        .current_dir(manifest_dir())
        .output()
        .expect("run report harness");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("imag_power_stepdown_wait_s=") && stdout.contains("imag_power_stepdown_guard_state_at_gate=clear"),
        "the report-only sidecar must record the waited seconds + the state at gate time, got: {stdout}"
    );
}

/// #1133 class: the runner is wired as `if ! imag_power_stepdown_wait …; then <abort>; exit 1; fi`.
/// A PROCEED must return 0 so the caller reaches the render gates; an ABORT returns 1 so the caller's
/// `if !` branch runs. Prove both under the caller's EXACT `set -euo pipefail`.
#[test]
fn runner_proceed_reaches_after_the_if_not_wrapper_under_set_e() {
    let harness = "set -euo pipefail\n. \"$LIB\"\n\
        export IMAG_POWER_STEPDOWN_READER_CMD='printf \"IMAGPWR_PL1|0\\n\"' \
        IMAG_POWER_STEPDOWN_NOW_CMD='echo 100' IMAG_POWER_STEPDOWN_SLEEP_CMD=':'\n\
        if ! imag_power_stepdown_wait u p h 1200 30 ''; then echo ABORT_BRANCH; exit 1; fi\n\
        echo REACHED_AFTER_PROCEED";
    let out = Command::new("bash")
        .arg("-c")
        .arg(harness)
        .env("LIB", lib())
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("REACHED_AFTER_PROCEED"),
        "a proceed must reach the line after `if ! …` under set -euo pipefail (rc={:?})\nstdout={stdout}\nstderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---------------------------------------------------------------------------------------------
// scripts/recording-e2e.sh wiring guards — the wait MUST sit BEFORE the imag render-budget family
// ---------------------------------------------------------------------------------------------
#[test]
fn recording_e2e_sources_the_lib_and_calls_the_runner() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains(". \"$HERE/lib/imag-power-stepdown-wait.sh\""),
        "recording-e2e.sh must source scripts/lib/imag-power-stepdown-wait.sh (issue 1268)"
    );
    assert!(
        s.contains("imag_power_stepdown_wait \"${IMAG_USER"),
        "recording-e2e.sh must invoke imag_power_stepdown_wait against imag (issue 1268)"
    );
}

#[test]
fn recording_e2e_wait_runs_before_the_imag_render_gates() {
    let s = read("scripts/recording-e2e.sh");
    let call = s
        .find("imag_power_stepdown_wait \"${IMAG_USER")
        .expect("imag_power_stepdown_wait call must exist");
    let mvfps = s
        .find("[4d1/8] #771")
        .expect("[4d1/8] #771 MV-fps preflight banner must exist");
    let render = s
        .find("[4d/8] #405")
        .expect("[4d/8] #405 render-budget gate banner must exist");
    assert!(
        call < mvfps && mvfps < render,
        "issue 1268: the power-clamp wait must run BEFORE the [4d1/8] MV-fps preflight and the [4d/8] \
         render-budget gate (call={call}, mvfps={mvfps}, render={render})"
    );
}

#[test]
fn recording_e2e_wait_is_gated_and_offline_ack_aware() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("E2E_IMAG_POWER_WAIT"),
        "the wait must be gated via E2E_IMAG_POWER_WAIT (default ON) (issue 1268)"
    );
    // the gated block must skip cleanly (report-only note) when imag is acked offline.
    let block = s
        .find("E2E_IMAG_POWER_WAIT")
        .expect("E2E_IMAG_POWER_WAIT gate must exist");
    let region = &s[block..(block + 1400).min(s.len())];
    assert!(
        region.contains("IMAG_OFFLINE_ACKED") && region.contains("imag_leg_skip_note"),
        "the wait block must skip via imag_leg_skip_note when IMAG_OFFLINE_ACKED=1 (issue 1268)"
    );
}
