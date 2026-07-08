//! Behavioral guard for `scripts/w32time-gate.sh` + `scripts/lib/w32time-authority.sh` — the
//! Windows W32Time verify-gate for strih + stream (#598). dantesync is the SOLE clock authority
//! on the whole rig; #591/#596/#597 already made a 2nd timesync daemon a hard FAIL on the LINUX
//! cam appliances. This gate closes the same gap on the WINDOWS OBS boxes: W32Time (the built-in
//! Windows Time service) can act as a competing NTP/domain client on strih/stream, exactly the
//! boxes doing the genlock. Both boxes were fixed live 2026-07-07 (W32Time Stopped + Disabled)
//! but until this gate that was a manual, unverified invariant.
//!
//! Mirrors `tests/dantesync_gate.rs`'s shape: `run_sourced` exercises the pure verdict/extraction
//! functions directly; `run_gate`/`run_gate_env` drive the actual gate SCRIPT end-to-end over
//! `--win-status NAME=FILE` fixture files — the same offline fixture-injection seam #608 added
//! for dantesync-gate.sh's Linux path, applied here to the ENTIRE Windows-only gate (ssh to
//! Windows is denied, so this gate is offline-fixture-only, with no live-SSH branch at all).
//!
//! Fixture text for the OK cases is the REAL output live-probed on strih and stream via the
//! win-* MCP on 2026-07-08 (both already fixed: STOPPED + DISABLED). The FAIL-case fixtures use
//! the well-documented standard `sc query`/`sc qc`/`reg query`/`w32tm /query /status` output
//! shapes with the STATE/START_TYPE/Type/Source fields substituted to the failing values.

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/w32time-gate.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the gate (its BASH_SOURCE!=$0 guard skips main, and it in turn sources
/// scripts/lib/w32time-authority.sh) and run `body`, returning stdout.
fn run_sourced(body: &str, extra_env: &[(&str, &str)]) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&harness).env("SCRIPT", script());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run bash harness");
    assert!(
        out.status.success(),
        "sourced harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Run the gate as a subprocess; return (exit_code, stdout, stderr).
fn run_gate(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(script())
        .args(args)
        .current_dir(manifest_dir())
        .output()
        .expect("run w32time-gate.sh");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Write a combined W32Time status-text fixture and return its path.
fn write_status(name: &str, text: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("w32time-gate-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.txt"));
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(text.as_bytes()).unwrap();
    path
}

// ---------------------------------------------------------------------------------------------
// Real fixtures, live-probed via the win-* MCP on strih (10.77.9.202) and stream (10.77.9.204),
// 2026-07-08. Both boxes are the ALREADY-FIXED (2026-07-07) steady state: W32Time STOPPED +
// DISABLED. strih's leftover registry Type is NoSync; stream's is NTP — proving the gate does NOT
// key on Type alone: a DISABLED box passes regardless of its leftover Type value.
// ---------------------------------------------------------------------------------------------

/// The real 2026-07-08 live-probed `sc query` + `sc qc` + `reg query` text for a STOPPED+DISABLED
/// box, parameterized ONLY by its leftover registry Type value (the one field that differs
/// between strih and stream) -- both boxes are otherwise byte-identical, so a single generator
/// replaces the two near-duplicate constants a review pass flagged (#598: ~21 lines repeated for
/// a one-word difference, the exact class `running_ntp_client_fixture` below already avoids).
fn ok_live_fixture(reg_type: &str) -> String {
    format!(
        "SERVICE_NAME: w32time \n\
        TYPE               : 20  WIN32_SHARE_PROCESS  \n\
        STATE              : 1  STOPPED \n\
        WIN32_EXIT_CODE    : 1077  (0x435)\n\
        SERVICE_EXIT_CODE  : 0  (0x0)\n\
        CHECKPOINT         : 0x0\n\
        WAIT_HINT          : 0x0\n\
[SC] QueryServiceConfig SUCCESS\n\
\n\
SERVICE_NAME: w32time\n\
        TYPE               : 20  WIN32_SHARE_PROCESS \n\
        START_TYPE         : 4   DISABLED\n\
        ERROR_CONTROL      : 1   NORMAL\n\
        BINARY_PATH_NAME   : C:\\WINDOWS\\system32\\svchost.exe -k LocalService\n\
        LOAD_ORDER_GROUP   : \n\
        TAG                : 0\n\
        DISPLAY_NAME       : Windows Time\n\
        DEPENDENCIES       : \n\
        SERVICE_START_NAME : NT AUTHORITY\\LocalService\n\
HKEY_LOCAL_MACHINE\\SYSTEM\\CurrentControlSet\\Services\\W32Time\\Parameters\n\
    Type    REG_SZ    {reg_type}\n\
The following error occurred: The service has not been started. (0x80070426)\n"
    )
}

/// A RUNNING, AUTO_START, NTP-client box actively synced to a real external peer — the standard
/// documented `w32tm /query /status` shape with an in-LAN NTP source substituted.
fn running_ntp_client_fixture(source: &str) -> String {
    format!(
        "SERVICE_NAME: w32time \n\
        TYPE               : 20  WIN32_SHARE_PROCESS  \n\
        STATE              : 4  RUNNING \n\
        WIN32_EXIT_CODE    : 0  (0x0)\n\
        SERVICE_EXIT_CODE  : 0  (0x0)\n\
        CHECKPOINT         : 0x0\n\
        WAIT_HINT          : 0x0\n\
[SC] QueryServiceConfig SUCCESS\n\
\n\
SERVICE_NAME: w32time\n\
        TYPE               : 20  WIN32_SHARE_PROCESS \n\
        START_TYPE         : 2   AUTO_START\n\
        ERROR_CONTROL      : 1   NORMAL\n\
        BINARY_PATH_NAME   : C:\\WINDOWS\\system32\\svchost.exe -k LocalService\n\
        LOAD_ORDER_GROUP   : \n\
        TAG                : 0\n\
        DISPLAY_NAME       : Windows Time\n\
        DEPENDENCIES       : \n\
        SERVICE_START_NAME : NT AUTHORITY\\LocalService\n\
HKEY_LOCAL_MACHINE\\SYSTEM\\CurrentControlSet\\Services\\W32Time\\Parameters\n\
    Type    REG_SZ    NTP\n\
Leap Indicator: 0(no warning)\n\
Stratum: 3 (secondary reference - syncd by (S)NTP)\n\
Precision: -23 (119.209ns per tick)\n\
Root Delay: 0.0158025s\n\
Root Dispersion: 8.9836757s\n\
ReferenceId: 0x0A4D0919 (source IP:  10.77.9.25)\n\
Last Successful Sync Time: 7/8/2026 6:00:00 AM\n\
Source: {source}\n\
Poll Interval: 1024 (17.1 mins)\n"
    )
}

#[test]
fn gate_passes_on_the_real_live_strih_and_stream_steady_state() {
    // The ACTUAL 2026-07-08 win-* MCP probe of both boxes -- both already fixed, both must PASS.
    let strih = write_status("strih_live_ok", &ok_live_fixture("NoSync"));
    let stream = write_status("stream_live_ok", &ok_live_fixture("NTP"));
    let (code, stdout, stderr) = run_gate(&[
        "--win-status",
        &format!("strih={}", strih.display()),
        "--win-status",
        &format!("stream={}", stream.display()),
    ]);
    assert_eq!(
        code, 0,
        "the real live strih+stream steady state must PASS. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout: {stdout}");
    assert!(
        stdout.contains("strih") && stdout.contains("OK"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("stream") && stdout.contains("OK"),
        "stdout: {stdout}"
    );
}

#[test]
fn gate_fails_when_w32time_is_running_as_an_active_ntp_client_to_a_real_source() {
    // RUNNING + Type=NTP + a real, non-local Source -- the #598 spec's literal HARD-FAIL case:
    // W32Time is actively pulling time from somewhere other than dantesync right now.
    let fixture = running_ntp_client_fixture("pool.ntp.org,0x9");
    let p = write_status("strih_active_2nd_authority", &fixture);
    let (code, _o, stderr) = run_gate(&["--win-status", &format!("strih={}", p.display())]);
    assert_eq!(
        code, 20,
        "an active NTP-syncing W32Time must FAIL (20). stderr: {stderr}"
    );
    assert!(stderr.contains("GATE FAILED"), "stderr: {stderr}");
}

#[test]
fn gate_ok_when_running_but_source_is_local_cmos_clock_not_a_real_peer() {
    // RUNNING + Type=NTP but Source is the purely-local fallback ("Local CMOS Clock") -- W32Time
    // has nothing real to fight dantesync over, so this must NOT be graded as a 2nd authority.
    let fixture = running_ntp_client_fixture("Local CMOS Clock");
    let p = write_status("strih_running_local_source", &fixture);
    let (code, stdout, stderr) = run_gate(&["--win-status", &format!("strih={}", p.display())]);
    assert_eq!(
        code, 0,
        "a RUNNING W32Time with only a local/free-running Source must be OK. \
         stdout={stdout} stderr={stderr}"
    );
}

#[test]
fn gate_fails_on_a_latent_2nd_authority_auto_start_ntp_but_currently_stopped() {
    // Not RUNNING right now, but START_TYPE=AUTO_START with Type=NTP -- it will resurrect as a
    // competing authority on the very next reboot. Mirrors #591's "masking is not enough, an
    // installed-but-disabled daemon still fails" philosophy on the start-type axis.
    let fixture = "SERVICE_NAME: w32time\n\
        STATE              : 1  STOPPED\n\
[SC] QueryServiceConfig SUCCESS\n\
        START_TYPE         : 2   AUTO_START\n\
HKEY_LOCAL_MACHINE\\SYSTEM\\CurrentControlSet\\Services\\W32Time\\Parameters\n\
    Type    REG_SZ    NTP\n\
The following error occurred: The service has not been started. (0x80070426)\n";
    let p = write_status("stream_latent_2nd_authority", fixture);
    let (code, stdout, stderr) = run_gate(&["--win-status", &format!("stream={}", p.display())]);
    assert_eq!(
        code, 20,
        "an AUTO_START NTP client that is merely stopped RIGHT NOW must still FAIL (20) as a \
         latent 2nd authority. stdout={stdout} stderr={stderr}"
    );
    assert!(stderr.contains("GATE FAILED"), "stderr: {stderr}");
}

#[test]
fn gate_ok_when_disabled_and_stopped_even_with_a_leftover_ntp_type() {
    // The REAL stream fixture already covers this end-to-end (Type=NTP but DISABLED+STOPPED ->
    // OK), but pin the DIRECT verdict function too since it is the core safety property: DISABLED
    // means it can never self-start, so a leftover Type value is inert.
    let out = run_sourced("w32time_daemon_verdict STOPPED DISABLED NTP \"\"", &[]);
    assert_eq!(
        out.trim(),
        "ok",
        "disabled+stopped must be ok regardless of Type: {out:?}"
    );
}

#[test]
fn gate_ok_when_type_is_nosync_even_if_hypothetically_running() {
    // #598 spec: "OK if ... Type: NoSync (not acting as an authority)" -- NoSync never syncs, so
    // even a (hypothetical) RUNNING+NoSync box is not a 2nd authority.
    let out = run_sourced(
        "w32time_daemon_verdict RUNNING AUTO_START NoSync example.ntp.org",
        &[],
    );
    assert_eq!(
        out.trim(),
        "ok",
        "NoSync must never be graded as an authority: {out:?}"
    );
}

#[test]
fn gate_incomplete_when_a_windows_status_file_is_missing() {
    // No status file for a gated box -> UNKNOWN -> exit 11 (incomplete, NOT a silent pass).
    let (code, _o, stderr) = run_gate(&[
        "--win-status",
        "stream=/tmp/definitely-not-a-real-w32time-status.txt",
    ]);
    assert_eq!(
        code, 11,
        "missing status -> INCOMPLETE (11). stderr: {stderr}"
    );
    assert!(stderr.contains("INCOMPLETE"), "stderr: {stderr}");
}

#[test]
fn gate_fails_closed_on_a_completely_unreadable_status_file() {
    // Garbage text with no STATE/START_TYPE/Type/Source lines at all must be UNKNOWN (never a
    // silent OK) -- test-strictness: an unreadable status must never default to "clean".
    let p = write_status("garbage", "not a real w32time status blob at all\n");
    let (code, _o, stderr) = run_gate(&["--win-status", &format!("strih={}", p.display())]);
    assert_eq!(
        code, 11,
        "a completely unreadable status file must be INCOMPLETE (11), never a silent PASS. \
         stderr: {stderr}"
    );
}

#[test]
fn gate_with_no_boxes_refuses_to_pass() {
    // Zero boxes to check must be a usage error (1), never "all clear".
    let (code, _o, stderr) = run_gate(&[]);
    assert_eq!(code, 1, "zero boxes -> usage error (1). stderr: {stderr}");
    assert!(
        stderr.contains("zero boxes") || stderr.contains("no boxes"),
        "stderr: {stderr}"
    );
}

#[test]
fn help_describes_the_2nd_authority_requirement() {
    let (code, stdout, _e) = run_gate(&["--help"]);
    assert_eq!(code, 0, "--help must exit 0");
    let low = stdout.to_lowercase();
    assert!(
        low.contains("w32time"),
        "help must mention W32Time: {stdout}"
    );
    assert!(
        low.contains("dantesync"),
        "help must describe dantesync as the sole authority: {stdout}"
    );
}

// ---------------------------------------------------------------------------------------------
// Direct unit tests of the pure verdict/extraction functions (mirrors the verify-device
// pure-function test style + tests/dantesync_gate.rs's node_verdict test).
// ---------------------------------------------------------------------------------------------

#[test]
fn w32time_state_known_accepts_only_real_service_states() {
    let cases = [
        ("RUNNING", "0"),
        ("STOPPED", "0"),
        ("START_PENDING", "0"),
        ("STOP_PENDING", "0"),
        ("PAUSED", "0"),
        ("PAUSE_PENDING", "0"),
        ("CONTINUE_PENDING", "0"),
        ("", "1"),
        ("GARBAGE", "1"),
    ];
    for (state, want_rc) in cases {
        // `if`'s condition is exempt from `set -e` (the sourced w32time-gate.sh sets it), so this
        // is the safe way to observe a boolean-returning function's exit code without aborting
        // the harness on a "false" (nonzero) result -- a bare `w32time_state_known "$S"; echo $?`
        // would kill the script on the FIRST failing case before `echo $?` ever ran.
        let out = run_sourced(
            "if w32time_state_known \"$S\"; then echo 0; else echo 1; fi",
            &[("S", state)],
        );
        assert_eq!(
            out.trim(),
            want_rc,
            "w32time_state_known({state:?}) must return {want_rc}: {out:?}"
        );
    }
}

#[test]
fn w32time_daemon_verdict_unknown_on_unreadable_state_never_defaults_to_ok() {
    let out = run_sourced("w32time_daemon_verdict \"\" \"\" \"\" \"\"", &[]);
    assert!(
        out.trim().starts_with("UNKNOWN:"),
        "an unreadable STATE must be UNKNOWN, never ok: {out:?}"
    );
}

#[test]
fn w32time_daemon_verdict_unknown_when_running_with_unreadable_type() {
    // RUNNING but we could not read the Type at all -- cannot certify it is inert.
    let out = run_sourced("w32time_daemon_verdict RUNNING AUTO_START \"\" \"\"", &[]);
    assert!(
        out.trim().starts_with("UNKNOWN:"),
        "RUNNING with unreadable Type must be UNKNOWN, never ok: {out:?}"
    );
}

#[test]
fn w32time_verdict_class_maps_ok_fail_unknown() {
    let cases = [
        ("ok", "OK"),
        ("FAIL: something bad", "BAD"),
        ("UNKNOWN: something unread", "UNKNOWN"),
    ];
    for (verdict, want) in cases {
        let out = run_sourced("w32time_verdict_class \"$V\"", &[("V", verdict)]);
        assert_eq!(
            out.trim(),
            want,
            "w32time_verdict_class({verdict:?}) must be {want}: {out:?}"
        );
    }
}

#[test]
fn extraction_parses_the_real_live_strih_fixture_fields() {
    let fixture = ok_live_fixture("NoSync");
    let out = run_sourced(
        "w32time_state_from_text \"$T\"; \
         echo '---'; \
         w32time_start_type_from_text \"$T\"; \
         echo '---'; \
         w32time_reg_type_from_text \"$T\"; \
         echo '---'; \
         w32time_source_from_text \"$T\"",
        &[("T", &fixture)],
    );
    let parts: Vec<&str> = out.split("---\n").collect();
    assert_eq!(parts[0].trim(), "STOPPED", "state: {out:?}");
    assert_eq!(parts[1].trim(), "DISABLED", "start_type: {out:?}");
    assert_eq!(parts[2].trim(), "NoSync", "reg_type: {out:?}");
    assert_eq!(
        parts[3].trim(),
        "",
        "source must be empty while stopped: {out:?}"
    );
}

#[test]
fn extraction_parses_a_running_ntp_client_fixtures_source_line() {
    let fixture = running_ntp_client_fixture("10.77.9.184");
    let out = run_sourced("w32time_source_from_text \"$T\"", &[("T", &fixture)]);
    assert_eq!(out.trim(), "10.77.9.184", "source: {out:?}");
}

// ---------------------------------------------------------------------------------------------
// Review-hardened cases (#598 code review, three independent passes): every one of these pins a
// REAL gap the pure verdict/extraction logic had before the fix, not a hypothetical.
// ---------------------------------------------------------------------------------------------

#[test]
fn gate_fails_when_running_as_an_allsync_client_to_a_real_source() {
    // AllSync ("use every available time provider") is documented in this file's own header as a
    // real `reg query` Type value alongside NTP/NT5DS, but the original w32time_syncing_type()
    // omitted it -- a RUNNING+AllSync+real-Source box silently passed as "ok". AllSync pulls from
    // configured NTP peers too, so it is exactly as much a 2nd authority as plain NTP.
    // NOTE: Rust's `\`-newline string continuation strips ALL leading whitespace off the next
    // line, so the fixture's actual bytes have NO leading spaces before "Type" despite the source
    // indentation above looking like it does -- match without them.
    let fixture = running_ntp_client_fixture("pool.ntp.org,0x9")
        .replace("Type    REG_SZ    NTP\n", "Type    REG_SZ    AllSync\n");
    assert!(
        fixture.contains("AllSync"),
        "fixture setup sanity: {fixture}"
    );
    let p = write_status("strih_allsync_active", &fixture);
    let (code, _o, stderr) = run_gate(&["--win-status", &format!("strih={}", p.display())]);
    assert_eq!(
        code, 20,
        "RUNNING+AllSync+real-Source must FAIL (20), not silently pass. stderr: {stderr}"
    );
}

#[test]
fn gate_unknown_when_running_as_a_syncing_client_but_source_is_unreadable() {
    // RUNNING + Type=NTP but NO Source line at all (distinct from "Source: Local CMOS Clock",
    // which IS readable and IS confirmed-benign) -- we cannot certify the box is inert, so this
    // must be UNKNOWN, never a silent "ok". Before the fix, an empty/unreadable Source was graded
    // identically to a confirmed-local one.
    let fixture = "SERVICE_NAME: w32time\n\
        STATE              : 4  RUNNING\n\
[SC] QueryServiceConfig SUCCESS\n\
        START_TYPE         : 2   AUTO_START\n\
HKEY_LOCAL_MACHINE\\SYSTEM\\CurrentControlSet\\Services\\W32Time\\Parameters\n\
    Type    REG_SZ    NTP\n\
The following error occurred: some transient w32tm query failure, no Source line at all\n";
    let p = write_status("strih_running_unreadable_source", fixture);
    let (code, stdout, stderr) = run_gate(&["--win-status", &format!("strih={}", p.display())]);
    assert_eq!(
        code, 11,
        "RUNNING+syncing-Type with an unreadable Source must be UNKNOWN/INCOMPLETE (11), never a \
         silent PASS. stdout={stdout} stderr={stderr}"
    );
    assert!(stderr.contains("INCOMPLETE"), "stderr: {stderr}");
}

#[test]
fn gate_unknown_when_stopped_but_start_type_is_unreadable() {
    // STATE is readable (STOPPED) but START_TYPE could not be parsed at all -- we cannot rule out
    // AUTO_START, so this must be UNKNOWN, never a silent "ok" (the not-running-branch analogue
    // of the RUNNING branch's own fail-closed handling).
    let fixture = "SERVICE_NAME: w32time\n\
        STATE              : 1  STOPPED\n\
[SC] QueryServiceConfig SUCCESS -- but the START_TYPE line itself is missing/garbled here\n\
HKEY_LOCAL_MACHINE\\SYSTEM\\CurrentControlSet\\Services\\W32Time\\Parameters\n\
    Type    REG_SZ    NTP\n\
The following error occurred: The service has not been started. (0x80070426)\n";
    let p = write_status("stream_stopped_unreadable_start_type", fixture);
    let (code, _o, stderr) = run_gate(&["--win-status", &format!("stream={}", p.display())]);
    assert_eq!(
        code, 11,
        "STOPPED with an unreadable START_TYPE must be UNKNOWN/INCOMPLETE (11), never a silent \
         PASS. stderr: {stderr}"
    );
    assert!(stderr.contains("INCOMPLETE"), "stderr: {stderr}");
}

#[test]
fn gate_unknown_when_auto_start_but_reg_type_is_unreadable() {
    // Not running, START_TYPE is a KNOWN autostart-class value, but the registry Type could not
    // be parsed at all -- we know it will start itself on the next boot, but not whether it will
    // actually sync anywhere. Must be UNKNOWN, never a silent "ok".
    let fixture = "SERVICE_NAME: w32time\n\
        STATE              : 1  STOPPED\n\
[SC] QueryServiceConfig SUCCESS\n\
        START_TYPE         : 2   AUTO_START\n\
HKEY_LOCAL_MACHINE\\SYSTEM\\CurrentControlSet\\Services\\W32Time\\Parameters -- Type value missing/garbled here\n\
The following error occurred: The service has not been started. (0x80070426)\n";
    let p = write_status("stream_auto_start_unreadable_type", fixture);
    let (code, _o, stderr) = run_gate(&["--win-status", &format!("stream={}", p.display())]);
    assert_eq!(
        code, 11,
        "AUTO_START with an unreadable Type must be UNKNOWN/INCOMPLETE (11), never a silent \
         PASS. stderr: {stderr}"
    );
    assert!(stderr.contains("INCOMPLETE"), "stderr: {stderr}");
}

#[test]
fn extraction_strips_a_trailing_carriage_return_from_the_source_line() {
    // This file's own gather snippet documents that `sc query`/`sc qc` had to be routed through
    // `cmd /c ... | Out-String` because the bare PowerShell-native invocation returned EMPTY
    // output over the win-* MCP Shell -- the same console-transport quirk can just as easily leave
    // a trailing CR on w32tm's own console output. An untrimmed \r would ride into the printed
    // FAIL diagnostic and corrupt the operator's terminal output (verified: printing a string
    // containing a bare \r resets the cursor to column 0 on any real terminal).
    let fixture = "Source: pool.ntp.org,0x9\r\nPoll Interval: 1024 (17.1 mins)\r\n";
    let out = run_sourced(
        "w32time_source_from_text \"$T\" | xxd | head -1",
        &[("T", fixture)],
    );
    assert!(
        !out.contains("0d"),
        "extracted Source must not contain a trailing \\r (hex 0d): {out}"
    );
    let out2 = run_sourced("w32time_source_from_text \"$T\"", &[("T", fixture)]);
    assert_eq!(
        out2.trim_end_matches('\n'),
        "pool.ntp.org,0x9",
        "source: {out2:?}"
    );
}

#[test]
fn w32time_syncing_type_recognizes_allsync() {
    let out = run_sourced("w32time_syncing_type AllSync && echo yes || echo no", &[]);
    assert_eq!(out.trim(), "yes", "AllSync must be a syncing type: {out:?}");
}

#[test]
fn w32time_start_type_known_rejects_empty_and_garbage() {
    let cases = [
        ("AUTO_START", "yes"),
        ("DEMAND_START", "yes"),
        ("DISABLED", "yes"),
        ("", "no"),
        ("GARBAGE", "no"),
    ];
    for (start_type, want) in cases {
        let out = run_sourced(
            "w32time_start_type_known \"$S\" && echo yes || echo no",
            &[("S", start_type)],
        );
        assert_eq!(
            out.trim(),
            want,
            "w32time_start_type_known({start_type:?}) must be {want}: {out:?}"
        );
    }
}
