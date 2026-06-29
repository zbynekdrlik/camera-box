//! Behavioral guard for the clock-offset regression check `scripts/clock-offset-guard.sh` (#8).
//!
//! The software genlock in `src/ndi.rs` aligns every camera's NDI send timecode to absolute
//! wall-clock frame boundaries, which only produces a COMMON boundary across nodes if their
//! wall clocks are synchronized (`src/ndi.rs:25-35` states this assumption verbatim). The
//! cluster is disciplined by DanteSync (strih = master; NTP anchor + PTP fine servo). If a
//! node's clock silently drifts past a fraction of the 16.7 ms (60 fps) frame period, genlock
//! degrades with NO error — exactly the failure mode #8 exists to guard against.
//!
//! `scripts/clock-offset-guard.sh` is the deterministic regression guard: it queries each
//! REACHABLE node's DanteSync-reported absolute clock offset and FAILS LOUDLY (exit non-zero)
//! if any node exceeds the documented bound. Its pure core — parse the offset from the two
//! real DanteSync status formats, and compare |offset| against the bound (OK / DRIFT / UNKNOWN,
//! never a silent pass) — must be correct regardless of network/SSH/MCP state, because that is
//! what decides whether a drifted node is caught or silently ships a broken genlock.
//!
//! These tests source the REAL script (its `BASH_SOURCE != $0` guard skips the executed flow)
//! and exercise the pure functions directly, plus run the script end-to-end for the exit-code
//! contract — the same convention as tests/drift_guard.rs and tests/av_stack_update.rs.
//!
//! The status-line fixtures below are the ACTUAL formats captured read-only from live nodes on
//! 2026-06-15, so the parsers are proven against the real production formats, not a guess:
//! * Linux cameras + stream — DanteSync logs to journald, e.g.
//!   `Jun 15 09:11:53 CAM2 dantesync[3649]: [NTP] offset:+300us (threshold:520us, adaptive)`
//! * Windows OBS boxes (strih/stream) — DanteSync status pipe emits JSON, e.g.
//!   `{"offset_ns":..,"ntp_offset_us":1249,"is_locked":true,"mode":"NANO",...}`

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/clock-offset-guard.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the script and run `body` (which may call the pure functions). Returns stdout.
fn run_sourced(body: &str, extra_env: &[(&str, &str)]) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}", body = body);
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&harness).env("SCRIPT", script());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "sourced harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Run the script as a subprocess; return (exit_code, stdout, stderr).
fn run_script(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(script())
        .args(args)
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run clock-offset-guard.sh");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Real DanteSync journald output captured from cam2 on 2026-06-15. The `[NTP] offset:` line is
/// the periodic ABSOLUTE offset vs the master; the many `[PTP] NANO Drift:` lines are the fine
/// servo's per-second drift RATE (ns/s) — NOT the absolute offset, so the parser must pick the
/// `[NTP] offset:` value, not a drift number.
const JOURNAL_FIXTURE: &str = "\
Jun 15 09:11:51 CAM2 dantesync[3649]: [PTP] NANO  Drift:   -486ns/s  Adj: +6.82ppm
Jun 15 09:11:52 CAM2 dantesync[3649]: [PTP] NANO  Drift:   +253ns/s  Adj: +6.81ppm
Jun 15 09:11:53 CAM2 dantesync[3649]: [PTP] NANO  Drift:  +1501ns/s  Adj: +6.79ppm
Jun 15 09:11:53 CAM2 dantesync[3649]: [NTP] offset:+300us (threshold:520us, adaptive)
Jun 15 09:11:54 CAM2 dantesync[3649]: [PTP] NANO  Drift:   +418ns/s  Adj: +6.80ppm
";

/// Real DanteSync status-pipe JSON captured from strih (the Windows master) on 2026-06-15.
const PIPE_JSON_FIXTURE: &str = "\
{\"offset_ns\":865726859,\"drift_ppm\":17.44,\"gm_source_ip\":\"10.77.9.184\",\"settled\":true,\
\"updated_ts\":1781514780,\"is_locked\":true,\"smoothed_rate_ppm\":0.27,\"ntp_offset_us\":1249,\
\"mode\":\"NANO\",\"ntp_failed\":false,\"accumulated_phase_us\":-227.24}";

#[test]
fn parses_offset_us_from_real_journal() {
    // The Linux nodes report the absolute offset in the periodic `[NTP] offset:+Nus` line. The
    // parser must return the SIGNED microsecond value from the LAST such line, ignoring the many
    // intervening `[PTP] ... Drift: Nns/s` rate lines.
    let out = run_sourced("offset_us_from_journal \"$J\"", &[("J", JOURNAL_FIXTURE)]);
    assert_eq!(
        out.trim(),
        "300",
        "must read +300 from '[NTP] offset:+300us', not a [PTP] drift number: {out:?}"
    );

    // A negative offset must keep its sign (the bound is on |offset|, computed by offset_check).
    let neg =
        "Jun 15 09:00:00 CAM4 dantesync[1]: [NTP] offset:-742us (threshold:550us, adaptive)\n";
    let out = run_sourced("offset_us_from_journal \"$J\"", &[("J", neg)]);
    assert_eq!(out.trim(), "-742", "must preserve the sign: {out:?}");

    // No `[NTP] offset:` line at all -> empty (UNKNOWN), never a silent 0 that looks in-bound.
    let none = "Jun 15 09:00:00 CAM1 dantesync[1]: [PTP] NANO  Drift: +10ns/s  Adj: +6ppm\n";
    let out = run_sourced("offset_us_from_journal \"$J\"", &[("J", none)]);
    assert_eq!(
        out.trim(),
        "",
        "no offset line -> UNKNOWN (empty), not a fake 0: {out:?}"
    );
}

#[test]
fn parses_offset_us_from_real_pipe_json() {
    // The Windows nodes report the absolute offset as the `ntp_offset_us` field of the status
    // JSON. The parser must extract that integer, not `offset_ns` / `accumulated_phase_us`.
    let out = run_sourced(
        "offset_us_from_pipe_json \"$JSON\"",
        &[("JSON", PIPE_JSON_FIXTURE)],
    );
    assert_eq!(
        out.trim(),
        "1249",
        "must read ntp_offset_us=1249, not offset_ns: {out:?}"
    );

    // A negative offset keeps its sign.
    let neg = "{\"ntp_offset_us\":-1880,\"is_locked\":true,\"mode\":\"NANO\"}";
    let out = run_sourced("offset_us_from_pipe_json \"$JSON\"", &[("JSON", neg)]);
    assert_eq!(out.trim(), "-1880", "must preserve the sign: {out:?}");

    // No `ntp_offset_us` field -> empty (UNKNOWN), never a silent pass.
    let none = "{\"offset_ns\":123,\"is_locked\":false}";
    let out = run_sourced("offset_us_from_pipe_json \"$JSON\"", &[("JSON", none)]);
    assert_eq!(
        out.trim(),
        "",
        "missing ntp_offset_us -> UNKNOWN (empty): {out:?}"
    );

    // Anomalous blob with MORE than one ntp_offset_us: the guard must return the WORST (largest
    // |offset|) so a later drifted value cannot be masked by an earlier in-bound one — a guard
    // that "never silently passes an out-of-bound offset" must not pick the first occurrence.
    let multi = "{\"ntp_offset_us\":50,\"peer\":{\"ntp_offset_us\":-99999}}";
    let out = run_sourced("offset_us_from_pipe_json \"$JSON\"", &[("JSON", multi)]);
    assert_eq!(
        out.trim(),
        "-99999",
        "multiple ntp_offset_us -> the worst (largest magnitude), not the first 50: {out:?}"
    );
}

#[test]
fn offset_check_flags_in_bound_out_of_bound_and_missing() {
    // (offset_us, bound_us, want_status_substr, want_rc). The bound is on the ABSOLUTE offset,
    // so a negative offset of the same magnitude as a positive one is treated identically.
    // rc: 0 = OK (|offset| <= bound), 2 = DRIFT (|offset| > bound), 3 = UNKNOWN (unread).
    let cases = [
        ("300", "2000", "OK", 0),      // typical camera, in bound
        ("1249", "2000", "OK", 0),     // strih master-to-GM offset, in bound
        ("-742", "2000", "OK", 0),     // negative, in bound by magnitude
        ("2000", "2000", "OK", 0),     // exactly at the bound -> OK
        ("2001", "2000", "DRIFT", 2),  // one µs over -> DRIFT
        ("-5000", "2000", "DRIFT", 2), // negative drift past the bound
        ("50000", "2000", "DRIFT", 2), // the unsynced failure mode (50 ms)
        ("", "2000", "UNKNOWN", 3),    // unread value is NEVER OK
    ];
    for (offset, bound, want_sub, want_rc) in cases {
        let body = "rc=0; offset_check node \"$OFF\" \"$BOUND\" || rc=$?; echo \"RC=$rc\"";
        let out = run_sourced(body, &[("OFF", offset), ("BOUND", bound)]);
        assert!(
            out.contains(want_sub),
            "offset_check({offset},{bound}) should print {want_sub}: {out:?}"
        );
        assert!(
            out.contains(&format!("RC={want_rc}")),
            "offset_check({offset},{bound}) should return {want_rc}: {out:?}"
        );
    }
}

#[test]
fn offset_check_uses_numeric_not_lexical_comparison() {
    // A lexical (string) compare would rank "9" > "2000" and "100" < "30". The bound check must
    // be NUMERIC so a 9 µs offset passes a 2000 µs bound and a 30000 µs offset fails it.
    let small = run_sourced(
        "rc=0; offset_check n \"$OFF\" \"$B\" || rc=$?; echo RC=$rc",
        &[("OFF", "9"), ("B", "2000")],
    );
    assert!(
        small.contains("OK") && small.contains("RC=0"),
        "9 <= 2000 numerically: {small:?}"
    );

    let big = run_sourced(
        "rc=0; offset_check n \"$OFF\" \"$B\" || rc=$?; echo RC=$rc",
        &[("OFF", "30000"), ("B", "2000")],
    );
    assert!(
        big.contains("DRIFT") && big.contains("RC=2"),
        "30000 > 2000 numerically: {big:?}"
    );

    // The DISCRIMINATING case: offset 100, bound 30. Lexically "100" < "30" (so a string compare
    // would wrongly call it OK); numerically 100 > 30 -> DRIFT. A regression to lexical compare
    // fails HERE, where the 30000-vs-2000 case above (DRIFT under both orderings) would not.
    let lex = run_sourced(
        "rc=0; offset_check n \"$OFF\" \"$B\" || rc=$?; echo RC=$rc",
        &[("OFF", "100"), ("B", "30")],
    );
    assert!(
        lex.contains("DRIFT") && lex.contains("RC=2"),
        "100 > 30 numerically (but lexically '100' < '30') — must be DRIFT: {lex:?}"
    );
}

// --- painter_offset_check (the #326 all-cambox dev1<->painter comparator) -------------------

#[test]
fn painter_offset_check_in_bound_out_of_bound_and_missing() {
    // (dev1_us, painter_us, guard_us, want_status_substr, want_rc). The check is on the RELATIVE
    // offset |dev1 - painter| (both DanteSync offsets on the same strih NTP-master basis).
    // rc: 0 = OK (|Δ| <= guard), 2 = DRIFT (|Δ| > guard), 3 = UNKNOWN (either offset unread).
    let cases = [
        ("300", "280", "200000", "OK", 0), // typical: dev1/painter ~20 us apart, in bound
        ("300", "-280", "200000", "OK", 0), // opposite signs: |580| still in bound
        ("200000", "0", "200000", "OK", 0), // |Δ| exactly at the guard -> OK
        ("200001", "0", "200000", "DRIFT", 2), // one µs over the guard -> DRIFT
        ("0", "300000", "200000", "DRIFT", 2), // painter ahead by 300 ms -> DRIFT
        ("-500000", "0", "200000", "DRIFT", 2), // dev1 behind by 500 ms -> DRIFT
        ("", "300", "200000", "UNKNOWN", 3), // dev1 offset unread -> UNKNOWN, never OK
        ("300", "", "200000", "UNKNOWN", 3), // painter offset unread -> UNKNOWN, never OK
        ("abc", "300", "200000", "UNKNOWN", 3), // malformed dev1 offset -> UNKNOWN
    ];
    for (dev1, painter, guard, want_sub, want_rc) in cases {
        let body = "rc=0; painter_offset_check node \"$D\" \"$P\" \"$G\" || rc=$?; echo \"RC=$rc\"";
        let out = run_sourced(body, &[("D", dev1), ("P", painter), ("G", guard)]);
        assert!(
            out.contains(want_sub),
            "painter_offset_check({dev1},{painter},{guard}) should print {want_sub}: {out:?}"
        );
        assert!(
            out.contains(&format!("RC={want_rc}")),
            "painter_offset_check({dev1},{painter},{guard}) should return {want_rc}: {out:?}"
        );
    }
}

#[test]
fn painter_offset_check_compares_the_relative_offset_not_each_absolute() {
    // THE defining property of the #326 gate: it must trip on dev1<->painter DIVERGENCE, not on
    // either node's absolute offset. Both nodes can sit at a large (but EQUAL) absolute offset vs
    // the NTP master and still be perfectly aligned to EACH OTHER — windows then attribute
    // correctly. dev1 +900000 µs, painter +899990 µs: each absolute (900 ms) is 4.5x the 200 ms
    // guard, yet the RELATIVE |Δ|=10 µs is tiny -> the gate MUST pass. A regression that compared
    // each absolute offset against the guard would wrongly FAIL here.
    let out = run_sourced(
        "rc=0; painter_offset_check n \"$D\" \"$P\" \"$G\" || rc=$?; echo RC=$rc",
        &[("D", "900000"), ("P", "899990"), ("G", "200000")],
    );
    assert!(
        out.contains("OK") && out.contains("RC=0"),
        "relative |Δ|=10 us is in bound even though each absolute offset (900 ms) exceeds it: {out:?}"
    );
}

#[test]
fn painter_offset_check_uses_numeric_not_lexical_comparison() {
    // A lexical compare would rank "100" < "30". The relative-vs-guard check must be NUMERIC:
    // dev1=100, painter=0, guard=30 -> |Δ|=100 > 30 -> DRIFT. A regression to a string compare
    // would wrongly call it OK (lexically "100" < "30").
    let out = run_sourced(
        "rc=0; painter_offset_check n \"$D\" \"$P\" \"$G\" || rc=$?; echo RC=$rc",
        &[("D", "100"), ("P", "0"), ("G", "30")],
    );
    assert!(
        out.contains("DRIFT") && out.contains("RC=2"),
        "100 > 30 numerically (but lexically '100' < '30') — must be DRIFT: {out:?}"
    );
}

#[test]
fn default_bound_is_documented_and_well_under_the_frame_period() {
    // The script exposes its chosen bound as DEFAULT_BOUND_US. It must be a sane value WELL under
    // the 16.7 ms (16667 µs) 60 fps frame period — the genlock boundary divergence a drifted
    // clock would cause — yet above the observed steady-state offsets (cam ~300 µs, strih ~1249
    // µs) so it does not false-positive on the healthy cluster.
    let out = run_sourced("echo \"$DEFAULT_BOUND_US\"", &[]);
    let bound: i64 = out
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("DEFAULT_BOUND_US not an int: {out:?}"));
    assert!(
        bound > 1249,
        "bound must clear strih's real 1249 µs offset: {bound}"
    );
    assert!(
        bound < 16667,
        "bound must be well under the 16.7 ms frame period: {bound}"
    );
}

#[test]
fn missing_target_set_fails_loudly_not_silently() {
    // Running the guard with no reachable targets at all must FAIL (exit non-zero) with a clear
    // message — it must never report "all clear" when it checked nothing (test-strictness: a
    // check that can't run must fail, not silently pass). Empty TARGETS -> loud usage error.
    let (code, _stdout, stderr) = run_script_env(&[], &[("CLOCK_GUARD_TARGETS", "")]);
    assert_ne!(code, 0, "no targets must fail, not pass: stderr={stderr:?}");
    assert!(
        stderr.to_lowercase().contains("no targets") || stderr.to_lowercase().contains("target"),
        "must name the empty-target-set problem: stderr={stderr:?}"
    );
}

/// Run the script with extra env (for driving the flow without live nodes).
fn run_script_env(args: &[&str], extra_env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(script());
    cmd.args(args).current_dir(manifest_dir());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("failed to run clock-offset-guard.sh");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn help_describes_the_offset_check_and_bound() {
    let (code, stdout, _stderr) = run_script(&["--help"]);
    assert_eq!(code, 0, "--help must exit 0");
    let low = stdout.to_lowercase();
    assert!(
        low.contains("offset") && (low.contains("bound") || low.contains("threshold")),
        "help must describe the offset check + its bound: {stdout:?}"
    );
}

// --- PTP-lock parsers (the #7 recording-E2E precondition) -----------------------------------

#[test]
fn ptp_locked_from_journal_detects_running_servo() {
    // A journal whose most recent PTP servo line is `[PTP] NANO Drift:` (or `[PTP] LOCK Drift:`)
    // = the µs-grade fine servo is RUNNING -> LOCKED. This is the real cam1 shape (2026-06-22).
    let running = "Jun 22 16:18:28 CAM1 dantesync[655]: [PTP] NANO  Drift: -67ns/s  Adj:+10.02ppm\n\
                   Jun 22 16:18:41 CAM1 dantesync[655]: [NTP] offset:+16us (threshold:530us)\n\
                   Jun 22 16:18:54 CAM1 dantesync[655]: [PTP] LOCK  Drift: -10ns/s  Adj:+10.01ppm\n";
    let out = run_sourced("ptp_locked_from_journal \"$J\"", &[("J", running)]);
    assert_eq!(
        out.trim(),
        "LOCKED",
        "a running NANO/LOCK servo is LOCKED: {out:?}"
    );
}

#[test]
fn ptp_locked_from_journal_empty_when_ntp_only_fallback() {
    // GM down -> PTP degrades to the NTP-only sawtooth: the `[PTP] (NANO|LOCK) Drift:` servo
    // lines STOP and only `[NTP] offset:` lines remain. No servo line -> UNKNOWN (empty), never
    // a silent LOCKED (test-strictness: an unobserved servo must NOT look locked).
    let ntp_only = "Jun 22 16:18:41 CAM1 dantesync[655]: [NTP] offset:+16us (threshold:530us)\n\
                    Jun 22 16:19:11 CAM1 dantesync[655]: [NTP] offset:+402us (threshold:530us)\n";
    let out = run_sourced("ptp_locked_from_journal \"$J\"", &[("J", ntp_only)]);
    assert_eq!(
        out.trim(),
        "",
        "NTP-only fallback -> not LOCKED (empty/UNKNOWN): {out:?}"
    );
}

#[test]
fn ptp_locked_from_journal_degraded_when_servo_stopped_but_stale_lines_linger() {
    // THE freshness case (the gate's whole point): `journalctl -n N` is count-bounded, so when
    // PTP degrades the servo lines STOP but stale NANO/LOCK lines linger in the window while new
    // `[NTP] offset:` lines accrue. A naive "any servo line anywhere -> LOCKED" would pass this
    // freshly-degraded node. The parser must see the NTP line is NEWER than the last servo line
    // and report DEGRADED.
    let stale = "Jun 22 16:10:00 CAM1 dantesync[655]: [PTP] NANO  Drift: -67ns/s  Adj:+10.02ppm\n\
                 Jun 22 16:10:01 CAM1 dantesync[655]: [PTP] LOCK  Drift: -10ns/s  Adj:+10.01ppm\n\
                 Jun 22 16:11:30 CAM1 dantesync[655]: [NTP] offset:+402us (threshold:530us)\n\
                 Jun 22 16:12:00 CAM1 dantesync[655]: [NTP] offset:+880us (threshold:530us)\n";
    let out = run_sourced("ptp_locked_from_journal \"$J\"", &[("J", stale)]);
    assert_eq!(
        out.trim(),
        "DEGRADED",
        "servo stopped (NTP newer than last servo line) -> DEGRADED, not stale LOCKED: {out:?}"
    );
}

#[test]
fn ptp_locked_from_journal_locked_when_servo_is_newest_among_ntp_lines() {
    // Interleaved NANO Drift + NTP offset, with a servo line as the MOST RECENT clock event ->
    // servo currently ticking -> LOCKED (the steady-state cam1 shape).
    let interleaved = "Jun 22 16:18:41 CAM1 dantesync[655]: [NTP] offset:+16us (threshold:530us)\n\
         Jun 22 16:18:42 CAM1 dantesync[655]: [PTP] NANO  Drift: +5ns/s  Adj:+10.0ppm\n\
         Jun 22 16:18:43 CAM1 dantesync[655]: [PTP] NANO  Drift: -3ns/s  Adj:+10.0ppm\n";
    let out = run_sourced("ptp_locked_from_journal \"$J\"", &[("J", interleaved)]);
    assert_eq!(
        out.trim(),
        "LOCKED",
        "servo is the newest clock event -> LOCKED: {out:?}"
    );
}

#[test]
fn ptp_locked_from_journal_ignores_mode_transition_banner() {
    // The `[PTP] === NANO MODE ===` transition banner is an EVENT, not the steady servo signal;
    // if only a banner (no `Drift:` servo line) is present, that is not proof the servo is running.
    let banner_only = "Jun 22 15:57:32 CAM1 dantesync[655]: [PTP] === NANO MODE === engaged\n";
    let out = run_sourced("ptp_locked_from_journal \"$J\"", &[("J", banner_only)]);
    assert_eq!(
        out.trim(),
        "",
        "a bare MODE banner is not a servo line: {out:?}"
    );
}

#[test]
fn ptp_locked_from_pipe_json_real_strih_status() {
    // Real strih status-pipe JSON (2026-06-22): is_locked=true + mode=NANO -> LOCKED.
    let locked = "{\"offset_ns\":-384280886,\"gm_source_ip\":\"10.77.9.184\",\"settled\":true,\
                  \"is_locked\":true,\"ntp_offset_us\":154,\"mode\":\"NANO\",\"ntp_failed\":false}";
    let out = run_sourced("ptp_locked_from_pipe_json \"$JSON\"", &[("JSON", locked)]);
    assert_eq!(
        out.trim(),
        "LOCKED",
        "is_locked=true + NANO -> LOCKED: {out:?}"
    );
}

#[test]
fn ptp_locked_from_pipe_json_degraded_when_unlocked_or_nonlock_mode() {
    // is_locked=false (or a mode that is not NANO/LOCK) = DEGRADED (NTP-only) -> must fail the gate.
    let unlocked = "{\"is_locked\":false,\"mode\":\"NTP\",\"ntp_offset_us\":154}";
    let out = run_sourced("ptp_locked_from_pipe_json \"$JSON\"", &[("JSON", unlocked)]);
    assert_eq!(
        out.trim(),
        "DEGRADED",
        "is_locked=false -> DEGRADED: {out:?}"
    );

    // is_locked=true but a non-lock mode is still DEGRADED (the mode must be NANO or LOCK).
    let bad_mode = "{\"is_locked\":true,\"mode\":\"COARSE\"}";
    let out = run_sourced("ptp_locked_from_pipe_json \"$JSON\"", &[("JSON", bad_mode)]);
    assert_eq!(
        out.trim(),
        "DEGRADED",
        "non-NANO/LOCK mode -> DEGRADED: {out:?}"
    );
}

#[test]
fn ptp_locked_from_pipe_json_unknown_when_fields_absent() {
    // Neither is_locked nor mode present -> UNKNOWN (empty), never a silent LOCKED.
    let absent = "{\"offset_ns\":1,\"ntp_offset_us\":154}";
    let out = run_sourced("ptp_locked_from_pipe_json \"$JSON\"", &[("JSON", absent)]);
    assert_eq!(
        out.trim(),
        "",
        "no is_locked/mode -> UNKNOWN (empty): {out:?}"
    );
}

#[test]
fn ptp_check_maps_state_to_exit_code() {
    // LOCKED -> rc 0, DEGRADED -> rc 2, anything else (UNKNOWN/empty) -> rc 3. An unread or
    // degraded PTP servo must NEVER map to OK (the gate would otherwise pass a meaningless run).
    let cases = [("LOCKED", 0), ("DEGRADED", 2), ("", 3), ("garbage", 3)];
    for (state, want) in cases {
        // `set +e` first: sourcing the guard re-enables its top-level `set -e`, which would
        // abort the harness the moment ptp_check returns a NON-zero rc (the very thing under
        // test). Capture the rc explicitly instead.
        let out = run_sourced(
            "set +e; ptp_check node \"$S\"; echo \"rc=$?\"",
            &[("S", state)],
        );
        assert!(
            out.contains(&format!("rc={want}")),
            "ptp_check({state:?}) must exit {want}: {out:?}"
        );
    }
}
