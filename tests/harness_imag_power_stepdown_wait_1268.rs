//! issue 1268 (branch A) — pure-function + runner + wiring guard for
//! `scripts/lib/imag-power-stepdown-wait.sh`, the bounded pre-gate WAIT that runs BEFORE the imag
//! render-budget family (`[4d1/8]` MV-fps floor preflight + `[4d/8]` render-budget gate) so those
//! STRICT reads never land inside a 25 W thermal step-down episode and falsely abort a ~40-min run.
//!
//! Root cause (issue 1268): the #1162 imag-nb holds 45 W only intermittently; the
//! imag-power-envelope-guard (#1040) steps PL1 down to 25 W ~18x/day (~20% duty, median ~12 min). At
//! 25 W the iGPU is pinned ~400 MHz -> a burns-ON render read is activeFps~57.7 / 15.6 ms, under the
//! 58 fps floor / 16.67 ms budget -> a false abort. The gate thresholds are CORRECT; the defect is
//! WHEN imag is read. So the wait WAITS on the MEASURED clamp signal to clear, never a threshold
//! relaxation and never a blind sleep — the same precondition-wait shape as `genlock-settle.sh`
//! (issue 1221) and the DanteSync settle.
//!
//! THE LOAD-BEARING DECISION (revised per the #1268 review, live-verified 2026-09-02): the PRIMARY
//! signal is the MMIO RAPL PL1 `package-0` `long_term` power_limit_uw — the guard's OWN actuator, a
//! DETERMINISTIC value (25000000 while stepped down, 45000000 = pinned IMAG_PL1_W after RESTORE),
//! world-readable to the non-root E2E ssh (mode 644). It is parsed identity-selected by the SHARED
//! `imag_power_zone_select` (REUSED from imag-power-envelope.sh, never a second copy). `clamped` =
//! long_term != pinned; `clear` = long_term == pinned. This is exactly "wait for the guard RESTORE"
//! and does NOT conflate the #880 chronic punit under-floor clamp (which sits AT the full 45 W
//! envelope with throttle_reason_pl1=1 and no RESTORE — it reads `clear` and proceeds). The guard
//! `STEPPED=` state (shared `imag_power_guard_stepped_from_state`) is a SUPPLEMENT that ORs into
//! clamped and is the fallback when the RAPL read fails; throttle_reason_pl1 is logged CONTEXT only.
//! Neither signal readable -> `unknown` -> the caller FAILS OPEN on the WAIT (proceeds; gate decides).
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
// lib shape — the pure functions + runner must be defined, and the SHARED reused parsers present
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
        "imag_power_zone_select",
        "imag_pl1_watts_to_uw",
        "imag_power_guard_stepped_from_state",
    ] {
        let out = stdout_of(&format!("type {f} >/dev/null 2>&1 && echo DEFINED"));
        assert_eq!(
            out, "DEFINED",
            "{f} must be defined (directly or via the sourced shared lib)"
        );
    }
}

/// The lib must SOURCE the shared imag-power-envelope lib (reuse the RAPL zone selector + the guard
/// state parser + the pinned-envelope helper), never re-copy them; the remote snippet must read the
/// throttle_reason_pl1 CONTEXT by IDENTITY GLOB (never a hardcoded cardN); and the RAPL primary must
/// be the reused zone selector, not a hand-rolled index.
#[test]
fn lib_sources_the_shared_power_envelope_lib_and_reuses_the_rapl_selector() {
    let src = std::fs::read_to_string(lib()).unwrap();
    assert!(
        src.contains("imag-power-envelope.sh"),
        "the lib must source scripts/lib/imag-power-envelope.sh to reuse the shared RAPL selector + state parser"
    );
    assert!(
        src.contains("imag_power_zone_select"),
        "the RAPL PRIMARY must use the shared imag_power_zone_select (identity-selected package-0 long_term)"
    );
    assert!(
        src.contains("/sys/class/drm/card*/gt/gt*/throttle_reason_pl1"),
        "the throttle_reason_pl1 CONTEXT read must identity-glob across card* (never a hardcoded cardN)"
    );
}

// ---------------------------------------------------------------------------------------------
// imag_power_stepdown_pl1_from_block — digits from the first IMAGPWR_PL1| line (CONTEXT only)
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
}

// ---------------------------------------------------------------------------------------------
// imag_power_stepdown_state <guard_stepped> <long_term_uw> <pinned_uw> — clamped / clear / unknown
// ---------------------------------------------------------------------------------------------
fn state(g: &str, lt: &str, pinned: &str) -> String {
    stdout_of(&format!(
        "imag_power_stepdown_state '{g}' '{lt}' '{pinned}'"
    ))
}

#[test]
fn state_core_classifies_the_rapl_primary_and_guard_supplement() {
    // clamped: RAPL long_term != pinned (a 25W step-down) -- the PRIMARY signal.
    assert_eq!(state("not-stepped", "25000000", "45000000"), "clamped");
    assert_eq!(state("unknown", "25000000", "45000000"), "clamped"); // live production (guard state root-600)
                                                                     // clamped: guard CONFIRMED stepped overrides even a full-envelope / unreadable RAPL.
    assert_eq!(state("stepped", "45000000", "45000000"), "clamped");
    assert_eq!(state("stepped", "", ""), "clamped");
    // clear: RAPL long_term == pinned full envelope -> the guard has restored.
    assert_eq!(state("not-stepped", "45000000", "45000000"), "clear");
    assert_eq!(state("unknown", "45000000", "45000000"), "clear"); // #880 chronic clamp sits at 45W -> proceed
                                                                   // fallback when RAPL unreadable: trust the guard state.
    assert_eq!(state("not-stepped", "", "45000000"), "clear");
    // unknown: neither signal readable -> fail-open (proceed).
    assert_eq!(state("unknown", "", "45000000"), "unknown");
}

// ---------------------------------------------------------------------------------------------
// imag_power_stepdown_verdict_from_block — RAPL primary (identity-selected) + guard supplement
// ---------------------------------------------------------------------------------------------
fn verdict(block: &str) -> String {
    stdout_of(&format!(
        "imag_power_stepdown_verdict_from_block \"$(printf '%b' '{block}')\""
    ))
}

#[test]
fn verdict_from_block_uses_rapl_long_term_as_primary() {
    // RAPL long_term 25W (a step-down) -> clamped
    assert_eq!(
        verdict("CONSTRAINT|package-0|0|long_term|25000000"),
        "clamped"
    );
    // RAPL long_term 45W (pinned full envelope) -> clear
    assert_eq!(
        verdict("CONSTRAINT|package-0|0|long_term|45000000"),
        "clear"
    );
    // guard STEPPED=1 overrides even a full-envelope RAPL (keep waiting for RESTORE)
    assert_eq!(
        verdict("CONSTRAINT|package-0|0|long_term|45000000\\nSTEPPED=1"),
        "clamped"
    );
    // #880 chronic clamp: RAPL at 45W, throttle_reason_pl1=1 -> still clear (pl1 is context, not decision)
    assert_eq!(
        verdict("CONSTRAINT|package-0|0|long_term|45000000\\nIMAGPWR_PL1|1"),
        "clear"
    );
    // identity selection: a `core` decoy zone's long_term must NOT be picked over package-0's
    assert_eq!(
        verdict("CONSTRAINT|core|0|long_term|25000000\\nCONSTRAINT|package-0|1|long_term|45000000"),
        "clear"
    );
    // RAPL unreadable, guard confirms stepped -> clamped (fallback)
    assert_eq!(verdict("STEPPED=1"), "clamped");
    // RAPL unreadable, guard confirms not-stepped -> clear (fallback)
    assert_eq!(verdict("STEPPED=0"), "clear");
    // empty block (ssh failed) -> unknown -> fail-open proceed
    assert_eq!(verdict(""), "unknown");
}

// ---------------------------------------------------------------------------------------------
// imag_power_stepdown_wait — the runner (injectable reader/clock/sleep seams; NO ssh, NO waiting)
// ---------------------------------------------------------------------------------------------
/// Run the runner with a scripted snapshot sequence + a fake clock, capturing stdout+stderr and rc.
/// The runner returns 0 to PROCEED and 1 to ABORT (a confirmed clamp held the whole budget). The
/// harness SOURCES the lib (the runner + the reused parsers must be defined) — without the source
/// the runner would be `command not found` (rc 127).
fn run_runner(setup: &str, call: &str) -> (i32, String) {
    let body = format!(
        ". \"$LIB\"\n{setup}\nexport IMAG_POWER_STEPDOWN_SLEEP_CMD=':'\nOUT=$({call} 2>&1); RC=$?\nprintf '%s\\n' \"$OUT\"\nexit $RC",
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

// RAPL-shaped reader fixtures: the verdict parses the shared gather's `CONSTRAINT|package-0|…` line.
const CLAMP_25W: &str = "CONSTRAINT|package-0|0|long_term|25000000";
const CLEAR_45W: &str = "CONSTRAINT|package-0|0|long_term|45000000";

#[test]
fn runner_proceeds_immediately_when_no_episode() {
    // first read = full envelope (45W) -> clear -> proceed, waited 0.
    let setup = format!(
        "export IMAG_POWER_STEPDOWN_READER_CMD='printf \"{CLEAR_45W}\\n\"' IMAG_POWER_STEPDOWN_NOW_CMD='echo 100'"
    );
    let (rc, out) = run_runner(&setup, "imag_power_stepdown_wait u p h 720 30 ''");
    assert_eq!(rc, 0, "no episode must proceed (rc0): {out}");
    assert!(
        out.contains("no 25W clamp episode") && out.contains("waited 0s"),
        "got: {out}"
    );
}

#[test]
fn runner_waits_then_proceeds_when_the_clamp_clears() {
    // pass0/1 clamped (25W), pass2 clear (45W) -> proceed rc0.
    let setup = format!(
        r#"
W="$(mktemp -d)"; echo 0 > "$W/idx"; echo 0 > "$W/clk"
printf '{CLAMP_25W}\n' > "$W/s0"; cp "$W/s0" "$W/s1"
printf '{CLEAR_45W}\n' > "$W/s2"; cp "$W/s2" "$W/s3"
export IMAG_POWER_STEPDOWN_READER_CMD='i=$(cat "'"$W"'/idx"); f="'"$W"'/s$i"; [ -f "$f" ] || f="'"$W"'/s3"; echo $((i+1)) > "'"$W"'/idx"; cat "$f"'
export IMAG_POWER_STEPDOWN_NOW_CMD='c=$(cat "'"$W"'/clk"); echo $((c+1)) > "'"$W"'/clk"; echo "$c"'
"#
    );
    let (rc, out) = run_runner(&setup, "imag_power_stepdown_wait u p h 720 5 ''");
    assert_eq!(rc, 0, "a clearing clamp must proceed (rc0): {out}");
    assert!(
        out.contains("clamp no longer detected") && out.contains("state=clear"),
        "got: {out}"
    );
    assert!(
        !out.contains("ERROR:"),
        "a clearing clamp must NOT abort: {out}"
    );
}

#[test]
fn runner_aborts_on_budget_exhaustion_naming_the_duration() {
    // clamp never clears (25W every read), clock jumps 5/read, budget 20 -> ABORT rc1.
    let setup = format!(
        r#"
W="$(mktemp -d)"; echo 0 > "$W/clk"
export IMAG_POWER_STEPDOWN_READER_CMD='printf "{CLAMP_25W}\n"'
export IMAG_POWER_STEPDOWN_NOW_CMD='c=$(cat "'"$W"'/clk"); echo $((c+5)) > "'"$W"'/clk"; echo "$c"'
"#
    );
    let (rc, out) = run_runner(&setup, "imag_power_stepdown_wait u p h 20 1 ''");
    assert_eq!(rc, 1, "a stuck clamp at budget must ABORT (rc1): {out}");
    assert!(
        out.contains("ERROR:")
            && out.contains("STILL in the 25W thermal step-down clamp")
            && out.contains("aborting BEFORE"),
        "the abort must name the clamp + the duration (never a silent pass): {out}"
    );
}

#[test]
fn runner_fails_open_and_proceeds_when_a_read_goes_unreadable() {
    // pass0 clamped (25W), pass1 EMPTY read (ssh hiccup) -> unknown -> fail-open proceed rc0.
    let setup = format!(
        r#"
W="$(mktemp -d)"; echo 0 > "$W/idx"; echo 0 > "$W/clk"
printf '{CLAMP_25W}\n' > "$W/t0"; : > "$W/t1"
export IMAG_POWER_STEPDOWN_READER_CMD='i=$(cat "'"$W"'/idx"); f="'"$W"'/t$i"; [ -f "$f" ] || f="'"$W"'/t1"; echo $((i+1)) > "'"$W"'/idx"; cat "$f"'
export IMAG_POWER_STEPDOWN_NOW_CMD='c=$(cat "'"$W"'/clk"); echo $((c+1)) > "'"$W"'/clk"; echo "$c"'
"#
    );
    let (rc, out) = run_runner(&setup, "imag_power_stepdown_wait u p h 720 5 ''");
    assert_eq!(
        rc, 0,
        "an unreadable read must fail-open (proceed rc0), never abort: {out}"
    );
    assert!(
        out.contains("state=unknown") && !out.contains("ERROR:"),
        "got: {out}"
    );
}

#[test]
fn runner_cannot_hang_with_a_stuck_clock_pass_ceiling_terminates() {
    // clamp forever + a clock frozen at 0 -> the wall budget can never fire; only the hard pass
    // ceiling can terminate (ABORT rc1). poll=1 so the pass*poll estimate names a non-zero duration
    // (the 🔵 fix) even though the wall clock is wedged; the ceiling (MAX_PASSES=4) fires first.
    let setup = format!(
        "export IMAG_POWER_STEPDOWN_READER_CMD='printf \"{CLAMP_25W}\\n\"' IMAG_POWER_STEPDOWN_NOW_CMD='echo 0' IMAG_POWER_STEPDOWN_MAX_PASSES=4"
    );
    let (rc, out) = run_runner(&setup, "imag_power_stepdown_wait u p h 100000 1 ''");
    assert_eq!(
        rc, 1,
        "a stuck clock must terminate via the pass ceiling (rc1): {out}"
    );
    assert!(
        out.contains("ERROR:") && out.contains("aborting BEFORE") && !out.contains("after ~0s"),
        "a wedged clock must still name a non-zero duration via the pass*poll estimate: {out}"
    );
}

#[test]
fn runner_sanitizes_malformed_numeric_args_never_infinite() {
    // budget/poll garbage -> sanitized to defaults; clamp never clears; a big clock jump -> abort rc1.
    let setup = format!(
        r#"
W="$(mktemp -d)"; echo 0 > "$W/clk"
export IMAG_POWER_STEPDOWN_READER_CMD='printf "{CLAMP_25W}\n"'
export IMAG_POWER_STEPDOWN_NOW_CMD='c=$(cat "'"$W"'/clk"); echo $((c+400)) > "'"$W"'/clk"; echo "$c"'
"#
    );
    let (rc, out) = run_runner(&setup, "imag_power_stepdown_wait u p h xyz bogus ''");
    assert_eq!(
        rc, 1,
        "malformed args must sanitize (default budget 720) and still terminate: {out}"
    );
    assert!(out.contains("ERROR:"), "got: {out}");
}

#[test]
fn runner_writes_the_report_only_sidecar() {
    // a clearing clamp -> the sidecar records the waited seconds (a digit) + the state at gate time.
    let harness = format!(
        r#"
        . "$LIB"
        W="$(mktemp -d)"; echo 0 > "$W/idx"; echo 0 > "$W/clk"; RF="$W/report.txt"
        printf '{CLAMP_25W}\n' > "$W/s0"; printf '{CLEAR_45W}\n' > "$W/s1"; cp "$W/s1" "$W/s2"
        export IMAG_POWER_STEPDOWN_READER_CMD='i=$(cat "'"$W"'/idx"); f="'"$W"'/s$i"; [ -f "$f" ] || f="'"$W"'/s2"; echo $((i+1)) > "'"$W"'/idx"; cat "$f"'
        export IMAG_POWER_STEPDOWN_NOW_CMD='c=$(cat "'"$W"'/clk"); echo $((c+1)) > "'"$W"'/clk"; echo "$c"'
        export IMAG_POWER_STEPDOWN_SLEEP_CMD=':'
        imag_power_stepdown_wait u p h 720 5 "$RF" >/dev/null 2>&1
        cat "$RF"
    "#
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", lib())
        .current_dir(manifest_dir())
        .output()
        .expect("run report harness");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // the waited-seconds line must carry an actual digit, and the state must be `clear`.
    let has_num = stdout.lines().any(|l| {
        l.starts_with("imag_power_stepdown_wait_s=")
            && l["imag_power_stepdown_wait_s=".len()..]
                .chars()
                .next()
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false)
    });
    assert!(
        has_num && stdout.contains("imag_power_stepdown_guard_state_at_gate=clear"),
        "the report-only sidecar must record a numeric waited_s + the state at gate time, got: {stdout}"
    );
}

/// #1133 class: the runner is wired as `if ! imag_power_stepdown_wait …; then <abort>; exit 1; fi`.
/// A PROCEED must return 0 so the caller reaches the render gates; an ABORT returns 1 so the caller's
/// `if !` branch runs. Prove the proceed reaches the line after `if ! …` under `set -euo pipefail`.
#[test]
fn runner_proceed_reaches_after_the_if_not_wrapper_under_set_e() {
    let harness = format!(
        "set -euo pipefail\n. \"$LIB\"\n\
        export IMAG_POWER_STEPDOWN_READER_CMD='printf \"{CLEAR_45W}\\n\"' \
        IMAG_POWER_STEPDOWN_NOW_CMD='echo 100' IMAG_POWER_STEPDOWN_SLEEP_CMD=':'\n\
        if ! imag_power_stepdown_wait u p h 720 30 ''; then echo ABORT_BRANCH; exit 1; fi\n\
        echo REACHED_AFTER_PROCEED"
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
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
// scripts/recording-e2e.sh wiring guards — the wait MUST run BEFORE BOTH imag render gates
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
fn recording_e2e_wait_runs_before_both_imag_render_gates() {
    let s = read("scripts/recording-e2e.sh");
    // FIRST call before the [4d1/8] MV-fps preflight
    let call1 = s
        .find("imag_power_stepdown_wait \"${IMAG_USER")
        .expect("imag_power_stepdown_wait call must exist");
    let mvfps = s
        .find("[4d1/8] #771")
        .expect("[4d1/8] #771 MV-fps preflight banner must exist");
    // SECOND call before the [4d/8] render-budget gate (the review 🟡-4 residual-race close)
    let render = s
        .find("[4d/8] #405")
        .expect("[4d/8] #405 render-budget gate banner must exist");
    let call2 = s[mvfps..]
        .find("imag_power_stepdown_wait \"${IMAG_USER")
        .map(|r| mvfps + r)
        .expect(
            "a SECOND imag_power_stepdown_wait call must precede the [4d/8] render-budget gate",
        );
    assert!(
        call1 < mvfps,
        "the first power-clamp wait must run BEFORE the [4d1/8] MV-fps preflight (call1={call1}, mvfps={mvfps})"
    );
    assert!(
        call2 < render,
        "the second power-clamp wait must run BEFORE the [4d/8] render-budget gate (call2={call2}, render={render})"
    );
}

#[test]
fn recording_e2e_wait_is_gated_and_offline_ack_aware() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("E2E_IMAG_POWER_WAIT=\"${E2E_IMAG_POWER_WAIT:-1}\""),
        "the wait must be gated via E2E_IMAG_POWER_WAIT (default ON) (issue 1268)"
    );
    // the gated block must skip cleanly (report-only note) when imag is acked offline.
    let block = s
        .find("E2E_IMAG_POWER_WAIT=\"${E2E_IMAG_POWER_WAIT:-1}\"")
        .expect("E2E_IMAG_POWER_WAIT gate assignment must exist");
    let region = &s[block..(block + 1400).min(s.len())];
    assert!(
        region.contains("IMAG_OFFLINE_ACKED") && region.contains("imag_leg_skip_note"),
        "the wait block must skip via imag_leg_skip_note when IMAG_OFFLINE_ACKED=1 (issue 1268)"
    );
}
