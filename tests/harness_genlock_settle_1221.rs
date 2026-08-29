//! issue 1221 — pure-function + wiring guard for `scripts/lib/genlock-settle.sh`, the measured
//! genlock-FIFO SETTLE-WAIT that runs AFTER the `[4i/8align]` per-source latency-pin writes and
//! BEFORE `[5/8] StartRecord`, so the recording measures steady-state instead of the FIFO relock
//! era the align itself induces.
//!
//! Root cause (issue 1221, verdict 950927573, 2026-08-29): each per-source latency-pin write in
//! `[4i/8align]` re-parameterises that input's genlock FIFO -> a relock/drain/regain episode (the
//! genlock-fifo-limit-cycle class). StartRecord fired straight after, so the first ~60-90s of the
//! recording measured the transient: per-window derived_uniform_fraction 0.644 -> 0.967 monotone
//! convergence, strict-contiguity faults concentrated in win0-win2, tail already >= the issue-1142
//! floor. The FIFO carries the direct steady-state signal itself -- the `genlock-fifo audit '<src>':`
//! line (src/jitter_audit.rs) appends ~every 5.017s with cumulative relocks/underruns/dropped_due/
//! late_holds counters -- so the settle WAITS ON A MEASURED signal, it is NOT a blind sleep.
//!
//! THE LOAD-BEARING DECISION -- the issue-797 phantom-rate avoidance: the quiet verdict compares
//! each counter's raw cumulative value between two consecutive snapshots (delta == 0 ?); there is no
//! rate and no wall-clock / poll-interval divisor at all, so the single-tick phantom-50 trap cannot
//! apply.
//!
//! Same convention as `tests/harness_cadence_health_794.rs`: source the REAL lib (source-only, no
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
    let s = manifest_dir().join("scripts/lib/genlock-settle.sh");
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
// lib shape — the pure functions + runner must be defined
// ---------------------------------------------------------------------------------------------
#[test]
fn lib_defines_the_functions() {
    for f in [
        "genlock_settle_latest_counters",
        "genlock_settle_pass_verdict",
        "genlock_settle_all_settled",
        "genlock_settle_wait",
    ] {
        let out = stdout_of(&format!("type {f} >/dev/null 2>&1 && echo DEFINED"));
        assert_eq!(out, "DEFINED", "{f} must be defined by the lib");
    }
}

// ---------------------------------------------------------------------------------------------
// genlock_settle_latest_counters — LAST audit line's four counters, empty for an absent source
// ---------------------------------------------------------------------------------------------
const FIXTURE_LOG: &str = "14:00:00.017: genlock-fifo audit 'NDI cam1': received=300 consumed=300 underruns=1 holds=0 dropped_due=2 relocks=1 late_holds=0 locked=1 latency_ms=3 ts_present=10\n14:00:05.034: genlock-fifo audit 'NDI cam1': received=600 consumed=600 underruns=1 holds=0 dropped_due=2 relocks=1 late_holds=0 locked=1 latency_ms=3 ts_present=11\n14:00:00.020: genlock-fifo audit 'NDI cam3': received=299 consumed=299 underruns=0 holds=0 dropped_due=0 relocks=0 late_holds=0 locked=1 latency_ms=3 ts_present=9\n14:00:05.040: genlock-fifo audit 'NDI cam3': received=598 consumed=598 underruns=0 holds=0 dropped_due=0 relocks=3 late_holds=1 locked=1 latency_ms=3 ts_present=10";

/// Read one source's LAST-line counters from the fixture log (passed via $L to avoid quoting the
/// multi-line text into the harness body).
fn latest(source: &str) -> String {
    stdout_of(&format!(
        "L={q}{fx}{q}; genlock_settle_latest_counters \"$L\" '{src}'",
        q = "\"",
        fx = FIXTURE_LOG,
        src = source
    ))
}

#[test]
fn latest_counters_reads_the_last_line_per_source() {
    // r u d l order; cam1's LAST tick, cam3's LAST tick
    assert_eq!(latest("NDI cam1"), "1 1 2 0");
    assert_eq!(latest("NDI cam3"), "3 0 0 1");
    // absent source -> empty
    assert_eq!(latest("NDI cam9"), "");
}

// ---------------------------------------------------------------------------------------------
// genlock_settle_pass_verdict — quiet / noisy / reset / unmeasurable
// ---------------------------------------------------------------------------------------------
fn verdict(prev: &str, curr: &str) -> String {
    stdout_of(&format!("genlock_settle_pass_verdict '{prev}' '{curr}'"))
}

#[test]
fn pass_verdict_classifies_the_relock_deltas() {
    assert_eq!(verdict("1 1 2 0", "1 1 2 0"), "quiet");
    assert_eq!(verdict("1 1 2 0", "2 1 2 0"), "noisy"); // relock advanced
    assert_eq!(verdict("1 1 2 0", "1 2 2 0"), "noisy"); // underrun advanced
    assert_eq!(verdict("1 1 2 0", "1 1 3 0"), "noisy"); // dropped_due advanced
    assert_eq!(verdict("1 1 2 0", "1 1 2 1"), "noisy"); // late_hold advanced
    assert_eq!(verdict("5 5 5 5", "0 0 0 0"), "reset"); // counter reset (OBS restart)
    assert_eq!(verdict("", "1 1 2 0"), "unmeasurable"); // first observation
    assert_eq!(verdict("1 1 2", "1 1 2 0"), "unmeasurable"); // short prev
    assert_eq!(verdict("1 1 2 0 9", "1 1 2 0"), "unmeasurable"); // 5-field prev
    assert_eq!(verdict("1 1 x 0", "1 1 2 0"), "unmeasurable"); // non-integer
}

// ---------------------------------------------------------------------------------------------
// genlock_settle_all_settled — SETTLED iff >=1 seen input and every seen streak >= N
// ---------------------------------------------------------------------------------------------
fn all_settled(args: &str) -> String {
    stdout_of(&format!("genlock_settle_all_settled {args}"))
}

#[test]
fn all_settled_requires_every_seen_input_at_n() {
    assert_eq!(all_settled("2"), "CONTINUE"); // none seen
    assert_eq!(all_settled("2 2 1 2"), "CONTINUE"); // one below N
    assert_eq!(all_settled("2 2 2 2"), "SETTLED");
    assert_eq!(all_settled("2 3 2"), "SETTLED"); // above N ok
    assert_eq!(all_settled("2 2"), "SETTLED"); // single seen input
    assert_eq!(all_settled("2 2 x"), "CONTINUE"); // non-integer streak
}

// ---------------------------------------------------------------------------------------------
// genlock_settle_wait — runner: converge -> SETTLED, and never-quiet -> budget -> fail-open WARN
// (injectable reader/clock/sleep seams; NO ssh, NO real waiting)
// ---------------------------------------------------------------------------------------------
/// Run the runner with a scripted snapshot sequence + a fake clock, capturing stdout + rc.
fn run_runner(setup: &str, call: &str) -> (i32, String) {
    let body = format!(
        "{setup}\nexport GENLOCK_SETTLE_SLEEP_CMD=':'\nOUT=$({call}); RC=$?\nprintf '%s\\n' \"$OUT\"\nexit $RC",
        setup = setup,
        call = call
    );
    let (rc, out, err) = run_sourced(&body);
    assert!(
        rc == 0,
        "runner must ALWAYS exit 0 (fail-open, report-only), got rc={rc}\nstdout={out}\nstderr={err}"
    );
    (rc, out)
}

#[test]
fn runner_proceeds_after_n_quiet_passes() {
    // reader: pass0 seed, pass1 noisy (relocks climb), pass2/3 quiet -> SETTLED at N=2.
    let setup = r#"
W="$(mktemp -d)"; echo 0 > "$W/idx"; echo 0 > "$W/clk"
printf "genlock-fifo audit 'NDI cam1': relocks=1 underruns=0 dropped_due=0 late_holds=0\n" > "$W/s0"
printf "genlock-fifo audit 'NDI cam1': relocks=2 underruns=0 dropped_due=0 late_holds=0\n" > "$W/s1"
cp "$W/s1" "$W/s2"; cp "$W/s1" "$W/s3"; cp "$W/s1" "$W/s4"
export GENLOCK_SETTLE_READER_CMD='i=$(cat "'"$W"'/idx"); f="'"$W"'/s$i"; [ -f "$f" ] || f="'"$W"'/s4"; echo $((i+1)) > "'"$W"'/idx"; cat "$f"'
export GENLOCK_SETTLE_NOW_CMD='c=$(cat "'"$W"'/clk"); echo $((c+1)) > "'"$W"'/clk"; echo "$c"'
"#;
    let (_rc, out) = run_runner(setup, "genlock_settle_wait u p h 'NDI cam1' 2 1000 0");
    assert!(
        out.contains("[settle]") && out.contains("quiet"),
        "expected a settle line, got: {out}"
    );
    assert!(
        !out.contains("WARNING"),
        "a converging run must NOT print the fail-open WARN, got: {out}"
    );
}

#[test]
fn runner_fails_open_with_warn_on_budget_exhaustion() {
    // reader: relocks climb every pass forever -> never quiet -> budget -> loud WARN, rc0.
    let setup = r#"
W="$(mktemp -d)"; echo 0 > "$W/idx"; echo 0 > "$W/clk"
export GENLOCK_SETTLE_READER_CMD='i=$(cat "'"$W"'/idx"); echo $((i+1)) > "'"$W"'/idx"; printf "genlock-fifo audit '"'"'NDI cam1'"'"': relocks=%s underruns=0 dropped_due=0 late_holds=0\n" "$i"'
export GENLOCK_SETTLE_NOW_CMD='c=$(cat "'"$W"'/clk"); echo $((c+5)) > "'"$W"'/clk"; echo "$c"'
"#;
    let (_rc, out) = run_runner(setup, "genlock_settle_wait u p h 'NDI cam1' 2 20 0");
    assert!(
        out.contains("WARNING") && out.contains("fail-open"),
        "expected a fail-open WARN, got: {out}"
    );
}

#[test]
fn runner_cannot_hang_even_with_a_stuck_clock() {
    // A pathological clock that never advances (a broken NOW seam / a wedged date) must NOT hang the
    // loop: the hard pass ceiling terminates it fail-open. relocks climb every pass so it never
    // settles; the wall budget can never fire (clock frozen at 0), so only the ceiling can stop it.
    let setup = r#"
export GENLOCK_SETTLE_READER_CMD='printf "genlock-fifo audit '"'"'NDI cam1'"'"': relocks=$RANDOM underruns=0 dropped_due=0 late_holds=0\n"'
export GENLOCK_SETTLE_NOW_CMD='echo 0'
export GENLOCK_SETTLE_MAX_PASSES=5
"#;
    let (_rc, out) = run_runner(setup, "genlock_settle_wait u p h 'NDI cam1' 2 100000 0");
    assert!(
        out.contains("WARNING") && out.contains("fail-open"),
        "a stuck clock must terminate via the pass ceiling (fail-open), got: {out}"
    );
}

#[test]
fn runner_never_aborts_the_caller_under_set_euo_pipefail() {
    // #1133 class: genlock_settle_wait is called as a BARE statement under recording-e2e.sh's
    // `set -euo pipefail`, so it must ALWAYS exit 0 — even with a broken clock (returns non-zero)
    // and an empty reader. Source under the caller's EXACT mode and assert the line AFTER the bare
    // call is reached (a `set -uo`-only harness would be blind to an errexit abort, so use -e here).
    let harness = "set -euo pipefail\n. \"$LIB\"\n\
        export GENLOCK_SETTLE_READER_CMD='echo' GENLOCK_SETTLE_NOW_CMD='false' \
        GENLOCK_SETTLE_SLEEP_CMD=':' GENLOCK_SETTLE_MAX_PASSES=3\n\
        genlock_settle_wait u p h 'NDI cam1' 2 5 0\n\
        echo REACHED_AFTER_SETTLE";
    let out = Command::new("bash")
        .arg("-c")
        .arg(harness)
        .env("LIB", lib())
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "the runner must not abort the caller under set -euo pipefail (rc={:?})\nstdout={stdout}\nstderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("REACHED_AFTER_SETTLE"),
        "the caller must reach the line after the bare genlock_settle_wait call, got: {stdout}"
    );
}

#[test]
fn runner_skips_gracefully_with_no_watched_inputs() {
    let setup = "export GENLOCK_SETTLE_READER_CMD='echo' GENLOCK_SETTLE_NOW_CMD='echo 0'";
    let (_rc, out) = run_runner(setup, "genlock_settle_wait u p h '' 2 20 0");
    assert!(
        out.contains("no aligned inputs to watch"),
        "empty watched set must skip gracefully, got: {out}"
    );
}

// ---------------------------------------------------------------------------------------------
// issue 1221 review fixes (Fable adversarial pass) — reader exec, env sanitize, bounds, stale streak
// ---------------------------------------------------------------------------------------------

/// 🔴-1: the DEFAULT reader must chain `timeout bash -c '. win-ssh-exec.sh; win_ssh_run …'` —
/// `timeout win_ssh_run` directly can never work (`timeout` execvp()s a real binary, not a shell
/// function). Prove the whole default chain end-to-end with a PATH-stubbed `sshpass` (the leaf
/// win_ssh_run calls); RED on the old `timeout <fn>` form (empty output), GREEN on the fix.
#[test]
fn default_reader_chains_through_bash_c_resource_not_timeout_of_a_function() {
    let stub = std::env::temp_dir().join(format!(
        "genlock-settle-sshpass-stub-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&stub).unwrap();
    let sshpass = stub.join("sshpass");
    std::fs::write(
        &sshpass,
        "#!/usr/bin/env bash\nprintf \"genlock-fifo audit 'NDI cam1': relocks=0 underruns=0 dropped_due=0 late_holds=0\\n\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sshpass, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let harness =
        "set -uo pipefail\n. \"$LIB\"\nunset GENLOCK_SETTLE_READER_CMD 2>/dev/null || true\n\
        _genlock_settle_read_snapshot u p h";
    let out = Command::new("bash")
        .arg("-c")
        .arg(harness)
        .env("LIB", lib())
        .env(
            "PATH",
            format!(
                "{}:{}",
                stub.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .current_dir(manifest_dir())
        .output()
        .expect("run reader harness");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let _ = std::fs::remove_dir_all(&stub);
    assert!(
        stdout.contains("genlock-fifo audit"),
        "the default reader must chain timeout->bash -c->win_ssh_run->sshpass and return the tail, got: {stdout:?}"
    );
}

/// 🔴-1 static guard: the lib source carries the re-source form and NOT the broken direct form.
#[test]
fn lib_source_uses_the_resource_reader_form() {
    let src = std::fs::read_to_string(lib()).unwrap();
    assert!(
        src.contains(r#"win_ssh_run "$2" "$3" "$4" "$5""#),
        "the reader must re-source win-ssh-exec.sh inside bash -c and call win_ssh_run with positional args"
    );
    assert!(
        !src.contains(r#"SSH_TIMEOUT:-20}" win_ssh_run"#),
        "the reader must NOT `timeout … win_ssh_run` a shell function directly (rc 127)"
    );
}

/// 🔴-2 / 🟡-1: malformed numeric env (n/budget/poll) must be SANITIZED — never a `printf`/`[ ]`
/// abort of the whole run under set -euo pipefail, and never an infinite loop.
#[test]
fn malformed_numeric_args_are_sanitized_never_abort() {
    let setup = "export GENLOCK_SETTLE_READER_CMD='printf \"genlock-fifo audit '\\''NDI cam1'\\'': relocks=$RANDOM underruns=0 dropped_due=0 late_holds=0\\n\"'\n\
        export GENLOCK_SETTLE_NOW_CMD='echo 0' GENLOCK_SETTLE_MAX_PASSES=8";
    let (_rc, out) = run_runner(setup, "genlock_settle_wait u p h 'NDI cam1' xyz bogus oops");
    assert!(
        out.contains("WARNING") && out.contains("fail-open"),
        "malformed n/budget/poll must sanitize and fail-open (not abort), got: {out}"
    );
}

/// 🟡-2: a wedged clock (never advances) must still be bounded at ~budget by the pass*poll
/// estimate, NOT run to the 1000 pass ceiling.
#[test]
fn wedged_clock_is_bounded_by_the_pass_poll_estimate() {
    let setup = "export GENLOCK_SETTLE_READER_CMD='printf \"genlock-fifo audit '\\''NDI cam1'\\'': relocks=$RANDOM underruns=0 dropped_due=0 late_holds=0\\n\"'\n\
        export GENLOCK_SETTLE_NOW_CMD='echo 0'";
    // budget 5, poll 1 -> est = pass*1 reaches 5 at pass 5, long before the 1000 ceiling
    let (_rc, out) = run_runner(setup, "genlock_settle_wait u p h 'NDI cam1' 2 5 1");
    let polls: u32 = out
        .split("after ")
        .nth(1)
        .and_then(|s| s.split(" poll").next())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(9999);
    assert!(
        polls <= 10,
        "a wedged clock must be bounded near budget by the pass*poll estimate (<=10 polls), got {polls}: {out}"
    );
}

/// 🔵-1: a source that goes quiet then VANISHES from the log must NOT count as settled — its stale
/// streak is reset, so the run waits (or budgets/ceilings out) rather than SETTLING on stale data.
#[test]
fn a_vanished_source_does_not_settle_on_a_stale_streak() {
    // s0 seed, s1 quiet (streak would build), s2 EMPTY (vanished) -> reset; ceiling 3 -> WARN not SETTLED
    let setup = r#"
W="$(mktemp -d)"; echo 0 > "$W/idx"; echo 0 > "$W/clk"
printf "genlock-fifo audit 'NDI cam1': relocks=5 underruns=0 dropped_due=0 late_holds=0\n" > "$W/s0"
cp "$W/s0" "$W/s1"; : > "$W/s2"; cp "$W/s0" "$W/s3"; cp "$W/s0" "$W/s4"
export GENLOCK_SETTLE_READER_CMD='i=$(cat "'"$W"'/idx"); f="'"$W"'/s$i"; [ -f "$f" ] || f="'"$W"'/s4"; echo $((i+1)) > "'"$W"'/idx"; cat "$f"'
export GENLOCK_SETTLE_NOW_CMD='c=$(cat "'"$W"'/clk"); echo $((c+1)) > "'"$W"'/clk"; echo "$c"'
export GENLOCK_SETTLE_MAX_PASSES=3
"#;
    let (_rc, out) = run_runner(setup, "genlock_settle_wait u p h 'NDI cam1' 2 100000 0");
    assert!(
        out.contains("WARNING"),
        "a source that vanishes after building a streak must NOT settle on the stale streak, got: {out}"
    );
}

// ---------------------------------------------------------------------------------------------
// scripts/recording-e2e.sh wiring guards — the settle MUST sit between [4i/8align] and StartRecord
// ---------------------------------------------------------------------------------------------
#[test]
fn recording_e2e_sources_the_settle_lib_and_calls_the_runner() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains(". \"$HERE/lib/genlock-settle.sh\""),
        "recording-e2e.sh must source scripts/lib/genlock-settle.sh (issue 1221)"
    );
    assert!(
        s.contains("genlock_settle_wait \"$STRIH_USER\" \"$STRIH_PW\" \"$STRIH\""),
        "recording-e2e.sh must invoke genlock_settle_wait against strih (issue 1221)"
    );
}

#[test]
fn recording_e2e_settle_is_positioned_after_align_and_before_startrecord() {
    let s = read("scripts/recording-e2e.sh");
    let align = s
        .find("qr_align_run \"$STRIH\"")
        .expect("[4i/8align] qr_align_run call must exist");
    let settle = s
        .find("[4j/8settle] issue 1221 waiting for genlock FIFO to settle")
        .expect("[4j/8settle] settle step must exist");
    let call = s
        .find("genlock_settle_wait \"$STRIH_USER\"")
        .expect("genlock_settle_wait call must exist");
    let start_record = s
        .find("[5/8] StartRecord")
        .expect("[5/8] StartRecord step must exist");
    assert!(
        align < settle && settle < call && call < start_record,
        "issue 1221 settle-wait must sit AFTER [4i/8align] and BEFORE [5/8] StartRecord \
         (align={align}, settle={settle}, call={call}, start_record={start_record})"
    );
}

#[test]
fn recording_e2e_settle_is_gated_and_disableable() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("E2E_GENLOCK_SETTLE=\"${E2E_GENLOCK_SETTLE:-1}\""),
        "the settle step must default ON via E2E_GENLOCK_SETTLE (issue 1221)"
    );
    // gated on the ALL_CAMBOX path (same path align + the sweep run on)
    let gate_region = &s[s
        .find("[4j/8settle] issue 1221 — measured genlock-FIFO")
        .expect("settle comment block must exist")..];
    assert!(
        gate_region
            .contains("[ \"$E2E_GENLOCK_SETTLE\" = \"1\" ] && [ \"${ALL_CAMBOX:-0}\" = \"1\" ]"),
        "settle must be gated on E2E_GENLOCK_SETTLE=1 && ALL_CAMBOX=1 (issue 1221)"
    );
}
