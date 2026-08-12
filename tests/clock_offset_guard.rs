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

use std::io::Write;
use std::path::{Path, PathBuf};
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

// --- FRESHNESS-aware offset reading: dantesync_offset_verdict / freshest_offset_us (#550/#591/
// #595) --------------------------------------------------------------------------------------
//
// offset_us_from_journal (above) is AGE-BLIND: `journalctl -n N` is COUNT-bounded, not time-
// bounded, so a died/hung dantesync or a long gap between the adaptive-cadence `[NTP] offset:`
// samples can leave only a stale multi-hour-old boot-STEP line in the window — grading THAT value
// as "current" was the #550 bug. dantesync_offset_verdict (originally added to verify-device.sh
// for #591, MOVED here for #595 so every caller shares one implementation) rejects a stale offset
// line by comparing its OWN `-o short-iso` timestamp against the newest journal line's timestamp
// (both from the SAME box — no host wall-clock ever enters the comparison). freshest_offset_us is
// its value-returning sibling: it returns the raw fresh offset (or "" if stale/absent) rather than
// a bound-graded verdict, for callers (the #326 painter gate) that must compare TWO boxes' fresh
// offsets against EACH OTHER rather than one absolute bound. Fixtures use the real
// `journalctl -o short-iso` timestamp form (colon in the TZ offset, e.g. +02:00) — copied from
// tests/verify_device_pure_functions.rs:259-321 (the same fixtures that pinned #591's fix there).

// Fresh, in-bound: the freshest [NTP] offset line is +14us, ~1s behind the newest PTP line.
const DS_FRESH_OK: &str = "\
2026-07-07T18:36:44+02:00 CAM5 dantesync[900]: [PTP] NANO  Drift:   -486ns/s  Adj: +6.82ppm
2026-07-07T18:36:45+02:00 CAM5 dantesync[900]: [NTP] offset:+14us (threshold:535us, adaptive)
2026-07-07T18:36:46+02:00 CAM5 dantesync[900]: [PTP] NANO  Drift:   +253ns/s  Adj: +6.81ppm
";

// The real cam5/6 desync: a FRESH [NTP] offset:-5280959us (a competing timesyncd stepping the
// clock -> a genuine 5.28s error), ~1s behind the newest PTP line.
const DS_FRESH_DRIFT: &str = "\
2026-07-07T18:36:44+02:00 CAM5 dantesync[900]: [PTP] NANO  Drift:   -486ns/s  Adj: +6.82ppm
2026-07-07T18:36:45+02:00 CAM5 dantesync[900]: [NTP] offset:-5280959us
2026-07-07T18:36:46+02:00 CAM5 dantesync[900]: [PTP] NANO  Drift:   +253ns/s  Adj: +6.81ppm
";

// A STALE boot-step offset line (#550): the ONLY [NTP] offset line is >1h behind the newest PTP
// line, so it must NOT be read as the current offset (the pre-#591 tail -1 bug graded on exactly
// this stale value).
const DS_STALE: &str = "\
2026-07-07T17:20:13+02:00 CAM6 dantesync[900]: [NTP] offset:-4357480us
2026-07-07T18:36:44+02:00 CAM6 dantesync[900]: [PTP] NANO  Drift:   -486ns/s  Adj: +6.82ppm
2026-07-07T18:36:46+02:00 CAM6 dantesync[900]: [PTP] NANO  Drift:   +253ns/s  Adj: +6.81ppm
";

// No [NTP] offset line at all — only PTP servo lines.
const DS_ABSENT: &str = "\
2026-07-07T18:36:44+02:00 CAM6 dantesync[900]: [PTP] NANO  Drift:   -486ns/s  Adj: +6.82ppm
2026-07-07T18:36:46+02:00 CAM6 dantesync[900]: [PTP] NANO  Drift:   +253ns/s  Adj: +6.81ppm
";

fn offset_verdict(journal: &str) -> String {
    run_sourced(
        &format!(
            "TEXT='{}'\ndantesync_offset_verdict \"$TEXT\" 300 2000",
            journal.replace('\'', "'\\''")
        ),
        &[],
    )
    .trim()
    .to_string()
}

#[test]
fn dantesync_offset_verdict_ok_on_fresh_in_bound_offset() {
    assert_eq!(offset_verdict(DS_FRESH_OK), "ok");
}

#[test]
fn dantesync_offset_verdict_drift_on_fresh_large_offset() {
    // The cam5/6 5.28s desync — a FRESH out-of-bound offset is a hard DRIFT (#591). A bare
    // "[NTP] offset:-5280959us" (no adaptive-threshold suffix) is the out-of-range fallback form.
    assert_eq!(offset_verdict(DS_FRESH_DRIFT), "drift");
}

#[test]
fn dantesync_offset_verdict_rejects_stale_boot_step_line() {
    // #550/#595: the freshest [NTP] offset line is a >1h-old boot STEP; it must NOT be read as the
    // current offset (the pre-#591 tail -1 bug graded on exactly this stale value). Verdict is
    // "stale", never "drift" (a stale value must never look like a real current desync) nor "ok".
    assert_eq!(offset_verdict(DS_STALE), "stale");
}

#[test]
fn dantesync_offset_verdict_absent_when_no_offset_line() {
    assert_eq!(offset_verdict(DS_ABSENT), "absent");
}

// #767-era measurement noise (live, 2026-07-15, E2E runs 29413733037/29419195600): under E2E
// network load the per-sample [NTP] offset MEASUREMENT spikes to ~2-3ms for a SINGLE sample
// (cam5 -2787us, cam7 -2316us) while PTP stays NANO-locked and the surrounding samples read
// tens of us -- the clock is fine, the one measurement is noisy. Grading the single freshest
// sample makes the gate flake exactly during E2E load; the verdict therefore grades the MEDIAN
// of the fresh samples among the last 5 offset lines. The bound (2000us) is UNCHANGED and a
// SUSTAINED out-of-bound offset (the real cam5/6 5.28s class) still hard-fails.
const DS_FRESH_SPIKE_AMID_OK: &str = "\
2026-07-07T18:36:20+02:00 CAM5 dantesync[900]: [NTP] offset:-20us (threshold:535us, adaptive)
2026-07-07T18:36:28+02:00 CAM5 dantesync[900]: [NTP] offset:+34us (threshold:535us, adaptive)
2026-07-07T18:36:36+02:00 CAM5 dantesync[900]: [NTP] offset:-15us (threshold:535us, adaptive)
2026-07-07T18:36:44+02:00 CAM5 dantesync[900]: [PTP] NANO  Drift:   -486ns/s  Adj: +6.82ppm
2026-07-07T18:36:45+02:00 CAM5 dantesync[900]: [NTP] offset:-2787us
2026-07-07T18:36:46+02:00 CAM5 dantesync[900]: [PTP] NANO  Drift:   +253ns/s  Adj: +6.81ppm
";

const DS_FRESH_SUSTAINED_DRIFT: &str = "\
2026-07-07T18:36:20+02:00 CAM5 dantesync[900]: [NTP] offset:+2624us
2026-07-07T18:36:28+02:00 CAM5 dantesync[900]: [NTP] offset:+2508us
2026-07-07T18:36:36+02:00 CAM5 dantesync[900]: [NTP] offset:+2865us
2026-07-07T18:36:44+02:00 CAM5 dantesync[900]: [PTP] NANO  Drift:   -486ns/s  Adj: +6.82ppm
2026-07-07T18:36:46+02:00 CAM5 dantesync[900]: [PTP] NANO  Drift:   +253ns/s  Adj: +6.81ppm
";

#[test]
fn dantesync_offset_verdict_tolerates_a_single_fresh_measurement_spike() {
    // One fresh ~2.8ms sample amid fresh tens-of-us samples = measurement noise, PTP still
    // locked -> the median is in-bound -> "ok". (Pre-#767-fix this graded the single freshest
    // sample and returned "drift" -- the exact E2E-load flake.)
    assert_eq!(offset_verdict(DS_FRESH_SPIKE_AMID_OK), "ok");
}

// A SAME-SIGN NOISE BURST: the 3 freshest samples spike high in one direction (live cam5
// 12:26-12:30: +2624, +2508, +2865 consecutive; run 29420477560 graded cam7 "drift" on median
// 2113us) while the 8 samples just before read tens of us. A 5-sample window is
// majority-covered by such a burst; ping RTT on the loaded rig LAN jitters to ~3.5ms on EVERY
// box, so a short burst is measurement physics, not a clock step (PTP stays NANO). An
// 11-sample window (~5min at the ~30s cadence; the journal gather is -n 400, so depth exists)
// keeps the median on the in-bound bulk while a genuine step shifts ALL samples and still
// drifts.
const DS_FRESH_BURST_AMID_OK: &str = "\
2026-07-07T18:33:20+02:00 CAM7 dantesync[900]: [NTP] offset:-20us (threshold:535us, adaptive)
2026-07-07T18:33:50+02:00 CAM7 dantesync[900]: [NTP] offset:+34us (threshold:535us, adaptive)
2026-07-07T18:34:20+02:00 CAM7 dantesync[900]: [NTP] offset:-15us (threshold:535us, adaptive)
2026-07-07T18:34:50+02:00 CAM7 dantesync[900]: [NTP] offset:+109us (threshold:535us, adaptive)
2026-07-07T18:35:20+02:00 CAM7 dantesync[900]: [NTP] offset:-103us (threshold:535us, adaptive)
2026-07-07T18:35:50+02:00 CAM7 dantesync[900]: [NTP] offset:+55us (threshold:535us, adaptive)
2026-07-07T18:36:05+02:00 CAM7 dantesync[900]: [NTP] offset:-130us (threshold:535us, adaptive)
2026-07-07T18:36:15+02:00 CAM7 dantesync[900]: [NTP] offset:+88us (threshold:535us, adaptive)
2026-07-07T18:36:25+02:00 CAM7 dantesync[900]: [NTP] offset:+2113us
2026-07-07T18:36:35+02:00 CAM7 dantesync[900]: [NTP] offset:+2316us
2026-07-07T18:36:45+02:00 CAM7 dantesync[900]: [NTP] offset:+2787us
2026-07-07T18:36:46+02:00 CAM7 dantesync[900]: [PTP] NANO  Drift:   +253ns/s  Adj: +6.81ppm
";

#[test]
fn dantesync_offset_verdict_tolerates_a_fresh_noise_burst_at_the_window_edge() {
    assert_eq!(offset_verdict(DS_FRESH_BURST_AMID_OK), "ok");
}

#[test]
fn dantesync_offset_verdict_still_drifts_on_sustained_out_of_bound_offset() {
    // EVERY fresh sample out of bound -> the median is out of bound -> "drift". The median is
    // noise-rejection, never a weakening of the 2000us bound.
    assert_eq!(offset_verdict(DS_FRESH_SUSTAINED_DRIFT), "drift");
}

#[test]
fn dantesync_offset_verdict_fails_closed_on_a_malformed_freshness_window() {
    // Same fail-closed property as freshest_offset_us, proven through the actual verdict every
    // gate (verify-device.sh/dantesync-gate.sh/clock-offset-painter-gate.sh) consumes: a
    // malformed FRESHNESS_S must never let a fresh, in-bound offset (DS_FRESH_OK) read as "ok" —
    // it must report "stale" (never a silent pass on an unvalidatable freshness knob).
    let out = run_sourced(
        &format!(
            "TEXT='{}'\ndantesync_offset_verdict \"$TEXT\" abc 2000",
            DS_FRESH_OK.replace('\'', "'\\''")
        ),
        &[],
    );
    assert_eq!(out.trim(), "stale");
}

fn freshest_offset(journal: &str, freshness_s: &str) -> String {
    run_sourced(
        &format!(
            "TEXT='{}'\nfreshest_offset_us \"$TEXT\" \"$FRESH\"",
            journal.replace('\'', "'\\''")
        ),
        &[("FRESH", freshness_s)],
    )
    .trim()
    .to_string()
}

#[test]
fn freshest_offset_us_returns_the_value_when_fresh() {
    // The #326 painter gate (clock-offset-painter-gate.sh) needs the RAW fresh offset, not a
    // bound-graded verdict, to compare two boxes' offsets against EACH OTHER (#595).
    assert_eq!(freshest_offset(DS_FRESH_OK, "300"), "14");
    assert_eq!(freshest_offset(DS_FRESH_DRIFT, "300"), "-5280959");
}

#[test]
fn freshest_offset_us_empty_when_stale_or_absent() {
    // A stale boot-step line or a wholly-absent offset line must NEVER return a value that could
    // be silently fed into a relative comparison (test-strictness: no silent pass on a possibly
    // stale reading).
    assert_eq!(freshest_offset(DS_STALE, "300"), "");
    assert_eq!(freshest_offset(DS_ABSENT, "300"), "");
}

#[test]
fn freshest_offset_us_fails_closed_on_a_malformed_freshness_window() {
    // FRESHNESS_S is caller-configurable ONLY via an unchecked env var
    // (DANTESYNC_OFFSET_FRESHNESS_S / GATE_OFFSET_FRESHNESS_S / PAINTER_GATE_FRESHNESS_S) — unlike
    // BOUND_US, which every caller validates via its own --bound-us/--guard-us CLI parsing before
    // ever calling in. Without an explicit guard, a malformed value (a typo'd env var: "abc", a
    // negative number, empty) would make the `-gt "$fresh"` arithmetic comparison throw a bash
    // "integer expression expected" error — which evaluates as a FAILED test in the `||` staleness
    // chain, silently making every reading look "fresh" regardless of true age. A fresh, in-bound
    // offset (DS_FRESH_OK) must still be REJECTED (empty) when the freshness window itself cannot
    // be trusted — never a silent pass on an unvalidatable knob.
    for bad_fresh in ["abc", "-1", "", "300.5", "1 ; rm -rf /"] {
        assert_eq!(
            freshest_offset(DS_FRESH_OK, bad_fresh),
            "",
            "a malformed freshness window ({bad_fresh:?}) must refuse to certify freshness, not silently pass"
        );
    }
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

// --- #607: the CLI's OWN main() loop (the original #8 guard invocation) must grade through the
// freshness-aware dantesync_offset_verdict, not the age-blind offset_us_from_journal+offset_check
// pairing it still used after #595 fixed the OTHER two callers (dantesync-gate.sh #7,
// clock-offset-painter-gate.sh #326). Fixture shapes + the CLOCK_GUARD_JOURNAL_OVERRIDE test-seam
// convention are copied from
// tests/clock_offset_painter_gate.rs::gate_incomplete_when_the_only_offset_line_is_a_stale_boot_step.

/// Write a one-line DanteSync journald fixture (`journalctl -o short-iso` ISO timestamp form)
/// with the given signed µs offset and return its path. A single-line journal's own timestamp IS
/// the journal's newest line, so the freshness check always reads it as fresh (age 0s) regardless
/// of the configured freshness window.
fn write_journal_fresh(name: &str, offset_us: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("clock-offset-guard-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.log"));
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(
        f,
        "2026-07-08T18:36:44+02:00 NODE dantesync[1]: [NTP] offset:{offset_us}us (threshold:520us, adaptive)"
    )
    .unwrap();
    path
}

/// Write a MULTI-LINE ISO journal whose ONLY `[NTP] offset:` line is STALE (#550-class): it sits
/// ~76 minutes behind the newer `[PTP]` servo lines that follow it — well past the default 300s
/// freshness window — even though the offset VALUE itself is in-bound. Proves staleness alone,
/// not magnitude, must trip the guard: the age-blind `offset_us_from_journal` (tail -1) would
/// still read this exact line as "the current offset" and pass it.
fn write_journal_stale(name: &str, offset_us: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("clock-offset-guard-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.log"));
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(
        f,
        "2026-07-08T17:20:13+02:00 NODE dantesync[1]: [NTP] offset:{offset_us}us (threshold:520us, adaptive)\n\
2026-07-08T18:36:44+02:00 NODE dantesync[1]: [PTP] NANO  Drift:   -486ns/s  Adj: +6.82ppm\n\
2026-07-08T18:36:46+02:00 NODE dantesync[1]: [PTP] NANO  Drift:   +253ns/s  Adj: +6.81ppm"
    )
    .unwrap();
    path
}

/// Run the CLI guard against a SINGLE `cam1` target whose journal is the given override file
/// (CLOCK_GUARD_JOURNAL_OVERRIDE — no live SSH/sshpass involved).
fn run_guard_with_journal_override(journal: &Path) -> (i32, String, String) {
    run_script_env(
        &["--targets", "cam1=10.77.9.61"],
        &[(
            "CLOCK_GUARD_JOURNAL_OVERRIDE",
            journal.to_str().expect("utf8 temp path"),
        )],
    )
}

#[test]
fn cli_reports_incomplete_not_ok_on_a_stale_but_in_bound_offset() {
    // #550-class staleness: cam1's ONLY `[NTP] offset:` line is a >1h-old boot-step sample
    // (+300us, in-bound). The age-blind offset_us_from_journal (tail -1) reads it as "the current
    // offset" and offset_check passes it (OK, exit 0) purely because the VALUE is in-bound --
    // exactly the false-pass #595 fixed in the OTHER two callers but left standing in this CLI's
    // own main() loop. The CLI must instead reject it as STALE and refuse to certify the node --
    // INCOMPLETE (exit 11), never a silent OK.
    let journal = write_journal_stale("cam1_stale_boot_step", "+300");
    let (code, stdout, stderr) = run_guard_with_journal_override(&journal);
    assert_eq!(
        code, 11,
        "a stale-but-in-bound offset line must be INCOMPLETE (11), never a silent OK/exit 0. stdout: {stdout} stderr: {stderr}"
    );
    assert!(
        stderr.to_uppercase().contains("INCOMPLETE"),
        "stderr must say the cluster status is incomplete: {stderr}"
    );
}

#[test]
fn cli_still_passes_on_a_fresh_in_bound_offset() {
    // Non-regression: a FRESH, in-bound offset line must still certify the node OK (exit 0) — the
    // freshness-aware grading must not turn into a false-fail on a healthy, current reading.
    let journal = write_journal_fresh("cam1_fresh_ok", "+300");
    let (code, stdout, stderr) = run_guard_with_journal_override(&journal);
    assert_eq!(
        code, 0,
        "a fresh in-bound offset must still PASS (0). stdout: {stdout} stderr: {stderr}"
    );
    assert!(stdout.contains("ALL CLEAR"), "stdout: {stdout}");
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

// --- gm_source_ip_from_pipe_json / gm_matches_expected / gm_check (#834) --------------------
//
// #834 (2026-07-28): the stream box reported is_locked=true/settled=true while PTP-locked to a
// FOREIGN grandmaster (10.77.7.109 instead of the rig's 10.77.9.184) and sat 14.7ms out. The
// fixtures below (HTTP_STATUS_STRIH_FIXTURE / HTTP_STATUS_STREAM_FIXTURE, declared further down
// in this file for the #648 freshness tests) are the REAL captured payloads from that exact
// incident — strih on the rig's own grandmaster, stream locked to the foreign one.

#[test]
fn gm_source_ip_from_pipe_json_reads_the_real_field() {
    let out = run_sourced(
        "gm_source_ip_from_pipe_json \"$JSON\"",
        &[("JSON", HTTP_STATUS_STRIH_FIXTURE)],
    );
    assert_eq!(
        out.trim(),
        "10.77.9.184",
        "must read strih's gm_source_ip: {out:?}"
    );

    let out = run_sourced(
        "gm_source_ip_from_pipe_json \"$JSON\"",
        &[("JSON", HTTP_STATUS_STREAM_FIXTURE)],
    );
    assert_eq!(
        out.trim(),
        "10.77.7.109",
        "must read stream's FOREIGN gm_source_ip (#834): {out:?}"
    );
}

#[test]
fn gm_source_ip_from_pipe_json_empty_when_field_absent() {
    let out = run_sourced(
        "gm_source_ip_from_pipe_json \"$JSON\"",
        &[("JSON", "{\"is_locked\":true,\"mode\":\"NANO\"}")],
    );
    assert_eq!(
        out.trim(),
        "",
        "no gm_source_ip field -> UNKNOWN (empty), never a guessed match: {out:?}"
    );
}

#[test]
fn gm_matches_expected_true_only_on_exact_nonempty_match() {
    let out = run_sourced(
        r#"
        for a in "10.77.9.184:10.77.9.184:YES" "10.77.7.109:10.77.9.184:NO" ":10.77.9.184:NO" "10.77.9.184::NO"; do
          actual="${a%%:*}"; rest="${a#*:}"; expected="${rest%%:*}"; want="${rest#*:}"
          if gm_matches_expected "$actual" "$expected"; then got=YES; else got=NO; fi
          [ "$got" = "$want" ] && echo "OK $a" || echo "MISMATCH $a got=$got"
        done
        "#,
        &[],
    );
    assert!(
        !out.contains("MISMATCH"),
        "gm_matches_expected produced a mismatch: {out}"
    );
}

#[test]
fn gm_check_ok_foreign_and_unknown() {
    // strih locked to the rig's own GM -> OK (rc 0).
    let out = run_sourced(
        "set +e; gm_check strih \"$ACTUAL\" 10.77.9.184; echo \"rc=$?\"",
        &[("ACTUAL", "10.77.9.184")],
    );
    assert!(
        out.contains("GM OK") && out.contains("rc=0"),
        "matching grandmaster must be OK: {out:?}"
    );

    // stream locked to a FOREIGN grandmaster -> FOREIGN (rc 2), the #834 shape — this must fail
    // even though the node's own offset/is_locked might read healthy in isolation.
    let out = run_sourced(
        "set +e; gm_check stream \"$ACTUAL\" 10.77.9.184; echo \"rc=$?\"",
        &[("ACTUAL", "10.77.7.109")],
    );
    assert!(
        out.contains("GM FOREIGN") && out.contains("rc=2"),
        "a foreign grandmaster must FAIL (#834), never look OK: {out:?}"
    );

    // gm_source_ip unread -> UNKNOWN (rc 3), never a silent pass.
    let out = run_sourced(
        "set +e; gm_check node \"$ACTUAL\" 10.77.9.184; echo \"rc=$?\"",
        &[("ACTUAL", "")],
    );
    assert!(
        out.contains("GM UNKNOWN") && out.contains("rc=3"),
        "an unread grandmaster must be UNKNOWN, never OK: {out:?}"
    );
}

// --- updated_ts_from_pipe_json / pipe_json_freshness_verdict (#648) -------------------------
//
// dantesync#47 gave every managed box a network status endpoint (http://<box>:8898/status)
// serving the SAME JSON the status pipe emits, PLUS "updated_ts" (unix epoch seconds of the
// daemon's last self-report). Fixtures below are the REAL payload shapes curled live from strih
// (10.77.9.202) and stream (10.77.9.204) on 2026-07-10 (see #648's own issue comment / dispatch
// context for the exact captured bytes).

const HTTP_STATUS_STRIH_FIXTURE: &str = "\
{\"offset_ns\":164707,\"drift_ppm\":-7.675453123651787,\"gm_uuid\":[0,0,0,0,1,0],\
\"gm_source_ip\":\"10.77.9.184\",\"settled\":true,\"updated_ts\":1783647854,\"is_locked\":true,\
\"smoothed_rate_ppm\":0.8499261548115417,\"ntp_offset_us\":0,\"mode\":\"NANO\",\
\"ntp_failed\":false,\"accumulated_phase_us\":164.86878554117786}";

const HTTP_STATUS_STREAM_FIXTURE: &str = "\
{\"offset_ns\":2100422,\"drift_ppm\":-4.368687646861424,\"gm_uuid\":[0,0,0,0,1,0],\
\"gm_source_ip\":\"10.77.7.109\",\"settled\":true,\"updated_ts\":1783647854,\"is_locked\":true,\
\"smoothed_rate_ppm\":-1.0190139408153804,\"ntp_offset_us\":189,\"mode\":\"NANO\",\
\"ntp_failed\":false,\"accumulated_phase_us\":6.691978464014342}";

#[test]
fn parses_updated_ts_from_real_http_status_payloads() {
    let out = run_sourced(
        "updated_ts_from_pipe_json \"$JSON\"",
        &[("JSON", HTTP_STATUS_STRIH_FIXTURE)],
    );
    assert_eq!(
        out.trim(),
        "1783647854",
        "must read strih's updated_ts: {out:?}"
    );

    let out = run_sourced(
        "updated_ts_from_pipe_json \"$JSON\"",
        &[("JSON", HTTP_STATUS_STREAM_FIXTURE)],
    );
    assert_eq!(
        out.trim(),
        "1783647854",
        "must read stream's updated_ts: {out:?}"
    );

    // No updated_ts field at all -> empty (UNKNOWN), never a silent value.
    let none = "{\"ntp_offset_us\":10,\"is_locked\":true}";
    let out = run_sourced("updated_ts_from_pipe_json \"$JSON\"", &[("JSON", none)]);
    assert_eq!(out.trim(), "", "missing updated_ts -> empty: {out:?}");
}

#[test]
fn pipe_json_freshness_verdict_fresh_when_within_the_window() {
    // updated_ts=1783647854, now=1783647860 (6s later), freshness bound 300s -> well within.
    let out = run_sourced(
        "pipe_json_freshness_verdict \"$JSON\" 1783647860 300",
        &[("JSON", HTTP_STATUS_STRIH_FIXTURE)],
    );
    assert_eq!(out.trim(), "fresh", "6s old, 300s bound -> fresh: {out:?}");
}

#[test]
fn pipe_json_freshness_verdict_stale_when_older_than_the_window() {
    // Same reading, but "now" is 1000s later than updated_ts -- past the 300s freshness bound.
    // This is the box-died-but-http-server-kept-serving-a-cached-snapshot case #648 must catch.
    let out = run_sourced(
        "pipe_json_freshness_verdict \"$JSON\" 1783648854 300",
        &[("JSON", HTTP_STATUS_STRIH_FIXTURE)],
    );
    assert_eq!(
        out.trim(),
        "stale",
        "1000s old, 300s bound -> stale: {out:?}"
    );
}

#[test]
fn pipe_json_freshness_verdict_stale_at_exactly_one_second_past_the_bound() {
    // updated_ts=1783647854, freshness bound 300s -> exactly at the bound (delta=300) is fresh,
    // one second past it (delta=301) must flip to stale. Pins the boundary is <= not <.
    let at_bound = run_sourced(
        "pipe_json_freshness_verdict \"$JSON\" 1783648154 300",
        &[("JSON", HTTP_STATUS_STRIH_FIXTURE)],
    );
    assert_eq!(
        at_bound.trim(),
        "fresh",
        "delta exactly 300s == bound -> fresh: {at_bound:?}"
    );

    let past_bound = run_sourced(
        "pipe_json_freshness_verdict \"$JSON\" 1783648155 300",
        &[("JSON", HTTP_STATUS_STRIH_FIXTURE)],
    );
    assert_eq!(
        past_bound.trim(),
        "stale",
        "delta 301s > bound -> stale: {past_bound:?}"
    );
}

#[test]
fn pipe_json_freshness_verdict_absent_when_updated_ts_is_missing() {
    let no_ts = "{\"ntp_offset_us\":10,\"is_locked\":true,\"mode\":\"NANO\"}";
    let out = run_sourced(
        "pipe_json_freshness_verdict \"$JSON\" 1783647860 300",
        &[("JSON", no_ts)],
    );
    assert_eq!(
        out.trim(),
        "absent",
        "no updated_ts field -> absent: {out:?}"
    );
}

#[test]
fn pipe_json_freshness_verdict_fails_closed_on_a_malformed_now_or_freshness_window() {
    // A malformed NOW_EPOCH or FRESHNESS_S must never let a plainly-fresh reading grade as
    // "fresh" -- test-strictness: no silent pass on a value the check cannot prove. Both bad
    // "now" and bad "freshness" collapse to "absent" (never "fresh", never a bash arithmetic
    // error that would abort the caller under set -e).
    for (bad_now, bad_fresh) in [
        ("abc", "300"),
        ("1783647860", "abc"),
        ("", "300"),
        ("-1", "300"),
    ] {
        let out = run_sourced(
            &format!("pipe_json_freshness_verdict \"$JSON\" '{bad_now}' '{bad_fresh}'"),
            &[("JSON", HTTP_STATUS_STRIH_FIXTURE)],
        );
        assert_eq!(
            out.trim(),
            "absent",
            "malformed now={bad_now:?} fresh={bad_fresh:?} must refuse to certify freshness, not silently pass: {out:?}"
        );
    }
}

#[test]
fn pipe_json_freshness_verdict_stale_when_updated_ts_is_in_the_future() {
    // now BEFORE updated_ts (clock skew / a bogus future timestamp) must also be stale, not
    // fresh -- the check is on |delta|, not a one-sided "not yet expired" comparison.
    let out = run_sourced(
        "pipe_json_freshness_verdict \"$JSON\" 1783647000 300",
        &[("JSON", HTTP_STATUS_STRIH_FIXTURE)],
    );
    assert_eq!(
        out.trim(),
        "stale",
        "updated_ts 854s in the caller's future must be stale, not fresh: {out:?}"
    );
}

// --- Multi-sample offset + stability grading (#836) ------------------------------------------
//
// offset_check grades a SINGLE read of "ntp_offset_us" against the bound -- close to a coin flip
// on a noisy Windows/HTTP node (live data, #836: 22 reads 25s apart on the stream box, only 2/22
// individually land inside the existing 2000us bound). These pure functions turn a SEQUENCE of
// raw status-JSON reads of the SAME node into a graded verdict: the MEDIAN of the samples that
// are DISTINCT by "updated_ts" against the existing bound, PLUS a NEW spread/stability check a
// single-read gate could never make at all. No network, no sleep -- every payload sequence below
// is a fixture built at test time.

/// A minimal DanteSync status-pipe JSON blob carrying just updated_ts + ntp_offset_us -- enough
/// for distinct_offset_samples_us/median_of_ints/spread_of_ints, which read only those two
/// fields.
fn pipe_json(ts: i64, offset_us: i64) -> String {
    format!("{{\"updated_ts\":{ts},\"ntp_offset_us\":{offset_us},\"is_locked\":true,\"mode\":\"NANO\"}}")
}

#[test]
fn distinct_offset_samples_us_dedupes_a_repeated_updated_ts() {
    // Three reads: the 2nd repeats the 1st's updated_ts EXACTLY (the daemon re-serving its own
    // cached value before its next refresh, #836 point 5) -- must be skipped, never counted as a
    // second independent sample, even though its VALUE differs from the 1st.
    let payloads = format!(
        "{}\n{}\n{}\n",
        pipe_json(1000, 500),
        pipe_json(1000, 999), // same ts as the line above -> not independent -> skipped
        pipe_json(1025, 700), // ts advanced -> a genuinely new sample
    );
    let out = run_sourced(
        "distinct_offset_samples_us \"$P\"",
        &[("P", payloads.as_str())],
    );
    let vals: Vec<&str> = out.lines().collect();
    assert_eq!(
        vals,
        vec!["500", "700"],
        "the ts-repeated read must be dropped, not double-counted: {out:?}"
    );
}

#[test]
fn distinct_offset_samples_us_skips_unparseable_reads_without_disturbing_the_ts_tracker() {
    // A malformed/empty line in the middle (a failed individual read within a gathered sequence)
    // must contribute nothing -- it neither counts as a sample NOR resets the last-accepted-ts
    // tracker, so the read right after it is still correctly compared against the LAST GOOD ts.
    let payloads = format!("{}\n\n{}\n", pipe_json(1000, 111), pipe_json(1000, 222));
    let out = run_sourced(
        "distinct_offset_samples_us \"$P\"",
        &[("P", payloads.as_str())],
    );
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        vec!["111"],
        "empty line must be skipped, and the SAME ts right after it must still dedupe against \
         the last good sample: {out:?}"
    );
}

#[test]
fn median_of_ints_computes_the_lower_median() {
    let out = run_sourced("median_of_ints \"$L\"", &[("L", "5\n1\n3")]);
    assert_eq!(out.trim(), "3", "median of [1,3,5] must be 3: {out:?}");

    // Even count -> LOWER median (position int((4+1)/2)=2 of the sorted list), same convention
    // as the journal path's _fresh_offset_median_us.
    let out = run_sourced("median_of_ints \"$L\"", &[("L", "4\n1\n3\n2")]);
    assert_eq!(
        out.trim(),
        "2",
        "lower median of [1,2,3,4] must be 2: {out:?}"
    );

    let out = run_sourced("median_of_ints \"$L\"", &[("L", "")]);
    assert_eq!(out.trim(), "", "empty list -> empty median: {out:?}");
}

#[test]
fn spread_of_ints_is_max_minus_min_or_empty_under_two_values() {
    let out = run_sourced("spread_of_ints \"$L\"", &[("L", "5\n-3\n10")]);
    assert_eq!(
        out.trim(),
        "13",
        "spread of [-3,5,10] must be 10-(-3)=13: {out:?}"
    );

    let out = run_sourced("spread_of_ints \"$L\"", &[("L", "42")]);
    assert_eq!(
        out.trim(),
        "",
        "spread is undefined with fewer than 2 values: {out:?}"
    );

    let out = run_sourced("spread_of_ints \"$L\"", &[("L", "")]);
    assert_eq!(out.trim(), "", "spread of an empty list -> empty: {out:?}");
}

#[test]
fn sampled_offset_verdict_ok_when_median_and_spread_both_in_bound() {
    let payloads = format!(
        "{}\n{}\n{}\n",
        pipe_json(1000, 150),
        pipe_json(1025, 200),
        pipe_json(1050, 175),
    );
    let out = run_sourced(
        "sampled_offset_verdict \"$P\" 2000 2000 3",
        &[("P", payloads.as_str())],
    );
    assert_eq!(
        out.trim(),
        "ok",
        "tight, in-bound samples must verdict ok: {out:?}"
    );
}

#[test]
fn sampled_offset_verdict_drift_when_median_exceeds_bound_but_spread_is_tight() {
    let payloads = format!(
        "{}\n{}\n{}\n",
        pipe_json(1000, 8000),
        pipe_json(1025, 8200),
        pipe_json(1050, 8100),
    );
    let out = run_sourced(
        "sampled_offset_verdict \"$P\" 2000 2000 3",
        &[("P", payloads.as_str())],
    );
    assert_eq!(
        out.trim(),
        "drift",
        "median clearly over bound, samples tight together -> drift only: {out:?}"
    );
}

#[test]
fn sampled_offset_verdict_unstable_when_median_is_fine_but_samples_scatter_wildly() {
    // THE new failure mode (#836 point 3): a node whose median offset looks perfect (well within
    // the 2000us bound) but whose individual readings scatter across tens of milliseconds --
    // "a node scattering +-20ms around a median of 200us must also fail". A single-read gate can
    // never see this at all; it can only ever grade whichever ONE value it happened to draw.
    let payloads = format!(
        "{}\n{}\n{}\n{}\n{}\n",
        pipe_json(1000, -19800),
        pipe_json(1025, 20100),
        pipe_json(1050, 200), // the median
        pipe_json(1075, -19500),
        pipe_json(1100, 19900),
    );
    let out = run_sourced(
        "sampled_offset_verdict \"$P\" 2000 2000 3",
        &[("P", payloads.as_str())],
    );
    assert_eq!(
        out.trim(),
        "unstable",
        "median in-bound but wildly scattered samples must FAIL as unstable, never look ok: {out:?}"
    );
}

#[test]
fn sampled_offset_verdict_drift_unstable_when_both_median_and_spread_fail() {
    let payloads = format!(
        "{}\n{}\n{}\n",
        pipe_json(1000, 25000),
        pipe_json(1025, -25000),
        pipe_json(1050, 30000),
    );
    let out = run_sourced(
        "sampled_offset_verdict \"$P\" 2000 2000 3",
        &[("P", payloads.as_str())],
    );
    assert_eq!(
        out.trim(),
        "drift_unstable",
        "both median AND spread failing must report both, not collapse to one: {out:?}"
    );
}

#[test]
fn sampled_offset_verdict_insufficient_when_too_few_distinct_samples() {
    // Every read repeats the SAME updated_ts (a static/stuck fixture, or a node whose refresh
    // interval is longer than the whole sampling window) -> only 1 distinct sample, below the
    // required minimum of 3 -> "insufficient", NEVER a silent pass even though that one value is
    // comfortably in-bound (#836 point 5, second half: "too few distinct samples must itself be
    // a failure, never a silent pass on one").
    let payloads = format!(
        "{}\n{}\n{}\n",
        pipe_json(1000, 50),
        pipe_json(1000, 50),
        pipe_json(1000, 50)
    );
    let out = run_sourced(
        "sampled_offset_verdict \"$P\" 2000 2000 3",
        &[("P", payloads.as_str())],
    );
    assert_eq!(
        out.trim(),
        "insufficient",
        "1 distinct sample of the required 3 must never grade as ok: {out:?}"
    );
}

#[test]
fn sampled_offset_verdict_fails_closed_on_malformed_thresholds() {
    let payloads = format!(
        "{}\n{}\n{}\n",
        pipe_json(1000, 100),
        pipe_json(1025, 110),
        pipe_json(1050, 90),
    );
    for (bound, stability, min_distinct) in [
        ("abc", "2000", "3"),
        ("2000", "abc", "3"),
        ("2000", "2000", "abc"),
    ] {
        let out = run_sourced(
            &format!("sampled_offset_verdict \"$P\" '{bound}' '{stability}' '{min_distinct}'"),
            &[("P", payloads.as_str())],
        );
        assert_eq!(
            out.trim(),
            "insufficient",
            "malformed threshold bound={bound:?} stability={stability:?} min_distinct={min_distinct:?} \
             must refuse to grade, not silently pass: {out:?}"
        );
    }
}

#[test]
fn sampled_offset_report_prints_distinct_count_median_and_spread() {
    let payloads = format!(
        "{}\n{}\n{}\n",
        pipe_json(1000, 100),
        pipe_json(1025, 300),
        pipe_json(1050, 200),
    );
    let out = run_sourced("sampled_offset_report \"$P\"", &[("P", payloads.as_str())]);
    assert_eq!(
        out.trim(),
        "3 200 200",
        "distinct=3, median=200 (lower median of [100,200,300]), spread=300-100=200: {out:?}"
    );

    // Fewer than 2 samples -> spread is "NA"; zero samples -> both are "NA".
    let one = pipe_json(1000, 42);
    let out = run_sourced("sampled_offset_report \"$P\"", &[("P", one.as_str())]);
    assert_eq!(
        out.trim(),
        "1 42 NA",
        "a single sample has a median but no defined spread: {out:?}"
    );
    let out = run_sourced("sampled_offset_report \"$P\"", &[("P", "")]);
    assert_eq!(
        out.trim(),
        "0 NA NA",
        "zero samples -> both median and spread are NA: {out:?}"
    );
}

#[test]
fn sampled_offset_check_reports_median_and_spread_on_every_outcome_and_returns_the_matching_rc() {
    // OK: rc 0, and the line still carries both numbers.
    let ok_payloads = format!(
        "{}\n{}\n{}\n",
        pipe_json(1000, 100),
        pipe_json(1025, 150),
        pipe_json(1050, 120),
    );
    let out = run_sourced(
        "set +e; sampled_offset_check strih \"$P\" 2000 2000 3; echo \"rc=$?\"",
        &[("P", ok_payloads.as_str())],
    );
    assert!(
        out.contains("OK")
            && out.contains("median")
            && out.contains("spread")
            && out.contains("rc=0"),
        "an OK verdict must still print median+spread: {out:?}"
    );

    // UNSTABLE: rc 2 (a hard failure, exactly like DRIFT -- never easier), and the line names it.
    let unstable_payloads = format!(
        "{}\n{}\n{}\n",
        pipe_json(1000, -19800),
        pipe_json(1025, 200),
        pipe_json(1050, 20100),
    );
    let out = run_sourced(
        "set +e; sampled_offset_check stream \"$P\" 2000 2000 3; echo \"rc=$?\"",
        &[("P", unstable_payloads.as_str())],
    );
    assert!(
        out.contains("UNSTABLE") && out.contains("rc=2"),
        "a scattered-but-in-bound-median node must be reported UNSTABLE with a hard-fail rc=2, \
         so it is never any easier to pass than a plain DRIFT: {out:?}"
    );

    // insufficient distinct samples: rc 3 (UNKNOWN), never a silent pass.
    let dup_payloads = format!("{}\n{}\n", pipe_json(2000, 50), pipe_json(2000, 50));
    let out = run_sourced(
        "set +e; sampled_offset_check cam1 \"$P\" 2000 2000 3; echo \"rc=$?\"",
        &[("P", dup_payloads.as_str())],
    );
    assert!(
        out.contains("UNKNOWN") && out.contains("rc=3"),
        "too few distinct samples must be UNKNOWN (rc=3), never a silent OK: {out:?}"
    );
}

#[test]
fn sampled_offset_verdict_on_the_live_836_stream_box_trail_fails_on_both_axes() {
    // The EXACT #834/#836 regression data: 22 reads, 25s apart, from the live stream box. Only
    // 2/22 individual samples land inside the 2000us bound -- the single-read gate this ticket
    // replaces would have passed roughly 1 run in 10 on this UNCHANGED, genuinely-bad node.
    // Consecutive duplicate VALUES (2014,2014 / 7482,7482 / 14862,14862) are modeled with the
    // SAME updated_ts as their predecessor (the daemon's refresh interval hadn't advanced yet,
    // #836 point 5) so distinct_offset_samples_us must drop them -- 22 raw reads, 3 dropped as
    // non-independent duplicates -> 19 distinct samples.
    let raw: [(i64, i64); 22] = [
        (0, -15913),
        (25, -18750),
        (50, 3982),
        (75, 19344),
        (100, 632),
        (125, 21205),
        (150, 7737),
        (175, 10481),
        (200, 2014),
        (200, 2014), // duplicate ts+value of the line above -- not independent
        (225, 22860),
        (250, 4784),
        (275, 19515),
        (300, 4223),
        (325, 7482),
        (325, 7482), // duplicate ts+value of the line above -- not independent
        (350, 22421),
        (375, 5404),
        (400, 1998),
        (425, 10040),
        (450, 14862),
        (450, 14862), // duplicate ts+value of the line above -- not independent
    ];
    let mut payloads = String::new();
    for (ts, off) in raw {
        payloads.push_str(&pipe_json(ts, off));
        payloads.push('\n');
    }

    let distinct = run_sourced(
        "distinct_offset_samples_us \"$P\"",
        &[("P", payloads.as_str())],
    );
    assert_eq!(
        distinct.lines().count(),
        19,
        "22 raw reads with 3 same-ts duplicates must yield exactly 19 distinct samples: {distinct:?}"
    );

    let verdict = run_sourced(
        "sampled_offset_verdict \"$P\" 2000 2000 3",
        &[("P", payloads.as_str())],
    );
    assert_eq!(
        verdict.trim(),
        "drift_unstable",
        "the live #836 stream-box trail must fail on BOTH the median (well over 2000us) AND the \
         spread (samples range from -18750 to 22860) -- the exact node the single-read gate could \
         pass ~10% of the time must now fail every time: {verdict:?}"
    );
}

// --- NTP-measurement freshness (#1014, dantesync v1.8.30 / dantesync issue 68 and issue 71) ---
//
// updated_ts (above) is PTP-driven and stays fresh even when the NTP measurement itself is dead
// or (dantesync issue 68) intentionally free-running after a one-time startup sync. dantesync
// v1.8.30 added "ntp_age_s" (an integer, or JSON null = never measured) and "ntp_updated_ts" so
// the NTP measurement's OWN freshness can be graded independently. These fixtures are the LIVE
// shape curled from strih (10.77.9.202) and stream (10.77.9.204) on 2026-08-11, trimmed to the
// fields these parsers read.

/// A live-shaped payload with the v1.8.30 NTP-freshness fields present.
fn pipe_json_ntp(ts: i64, offset_us: i64, ntp_age_s_raw: &str, ntp_failed: bool) -> String {
    format!(
        "{{\"updated_ts\":{ts},\"ntp_offset_us\":{offset_us},\"is_locked\":true,\"mode\":\"NANO\",\
         \"ntp_failed\":{ntp_failed},\"ntp_updated_ts\":{ts},\"ntp_age_s\":{ntp_age_s_raw}}}"
    )
}

#[test]
fn ntp_age_s_raw_from_pipe_json_reads_numeric_null_and_absent() {
    let numeric = pipe_json_ntp(1000, 100, "4", false);
    let out = run_sourced(
        "ntp_age_s_raw_from_pipe_json \"$JSON\"",
        &[("JSON", numeric.as_str())],
    );
    assert_eq!(out.trim(), "4", "must read a numeric ntp_age_s: {out:?}");

    let never = pipe_json_ntp(1000, 100, "null", false);
    let out = run_sourced(
        "ntp_age_s_raw_from_pipe_json \"$JSON\"",
        &[("JSON", never.as_str())],
    );
    assert_eq!(
        out.trim(),
        "null",
        "must read the literal null, never coerce it to empty or 0: {out:?}"
    );

    let old_shape =
        "{\"updated_ts\":1000,\"ntp_offset_us\":100,\"is_locked\":true,\"mode\":\"NANO\"}";
    let out = run_sourced(
        "ntp_age_s_raw_from_pipe_json \"$JSON\"",
        &[("JSON", old_shape)],
    );
    assert_eq!(
        out.trim(),
        "",
        "a pre-1.8.30 payload with no ntp_age_s field at all -> empty (absent): {out:?}"
    );
}

#[test]
fn ntp_updated_ts_and_ntp_failed_from_pipe_json() {
    let text = pipe_json_ntp(1786449281, 1170, "4", false);
    let out = run_sourced(
        "ntp_updated_ts_from_pipe_json \"$JSON\"",
        &[("JSON", text.as_str())],
    );
    assert_eq!(
        out.trim(),
        "1786449281",
        "must read ntp_updated_ts: {out:?}"
    );
    let out = run_sourced(
        "ntp_failed_from_pipe_json \"$JSON\"",
        &[("JSON", text.as_str())],
    );
    assert_eq!(out.trim(), "false", "must read ntp_failed=false: {out:?}");

    let failed = pipe_json_ntp(1786449281, 1170, "4", true);
    let out = run_sourced(
        "ntp_failed_from_pipe_json \"$JSON\"",
        &[("JSON", failed.as_str())],
    );
    assert_eq!(out.trim(), "true", "must read ntp_failed=true: {out:?}");

    let old_shape = "{\"ntp_offset_us\":10,\"is_locked\":true}";
    let out = run_sourced(
        "ntp_updated_ts_from_pipe_json \"$JSON\"",
        &[("JSON", old_shape)],
    );
    assert_eq!(out.trim(), "", "no ntp_updated_ts field -> empty: {out:?}");
    let out = run_sourced(
        "ntp_failed_from_pipe_json \"$JSON\"",
        &[("JSON", old_shape)],
    );
    assert_eq!(out.trim(), "", "no ntp_failed field -> empty: {out:?}");
}

#[test]
fn ntp_freshness_verdict_fresh_when_age_within_window() {
    // The live strih capture: ntp_age_s=4, well within a 300s window.
    let text = pipe_json_ntp(1786449281, 1170, "4", false);
    let out = run_sourced(
        "ntp_freshness_verdict \"$JSON\" 300",
        &[("JSON", text.as_str())],
    );
    assert_eq!(out.trim(), "fresh", "age 4s, 300s window -> fresh: {out:?}");
}

#[test]
fn ntp_freshness_verdict_stale_when_age_exceeds_window() {
    // #1014's ORIGINAL incident shape: a large stale age (the NTP measurement froze while the
    // general updated_ts kept advancing).
    let text = pipe_json_ntp(1786449281, -34718, "99999", false);
    let out = run_sourced(
        "ntp_freshness_verdict \"$JSON\" 300",
        &[("JSON", text.as_str())],
    );
    assert_eq!(
        out.trim(),
        "stale",
        "age 99999s, 300s window -> stale, regardless of how large the offset value looks: {out:?}"
    );
}

#[test]
fn ntp_freshness_verdict_stale_at_exactly_one_second_past_the_bound() {
    let at_bound = pipe_json_ntp(1000, 100, "300", false);
    let out = run_sourced(
        "ntp_freshness_verdict \"$JSON\" 300",
        &[("JSON", at_bound.as_str())],
    );
    assert_eq!(
        out.trim(),
        "fresh",
        "age exactly == window -> fresh: {out:?}"
    );

    let past_bound = pipe_json_ntp(1000, 100, "301", false);
    let out = run_sourced(
        "ntp_freshness_verdict \"$JSON\" 300",
        &[("JSON", past_bound.as_str())],
    );
    assert_eq!(out.trim(), "stale", "age one past window -> stale: {out:?}");
}

#[test]
fn ntp_freshness_verdict_ntp_failed_is_an_independent_stale_signal() {
    // #1014: dantesync issue 68 widened ntp_failed to ALSO mean "no fresh measurement within
    // window" -- a payload with a comfortably-fresh age but ntp_failed:true must still refuse,
    // proving the two signals are checked independently, not merely OR'd into one age check.
    let text = pipe_json_ntp(1000, 100, "2", true);
    let out = run_sourced(
        "ntp_freshness_verdict \"$JSON\" 300",
        &[("JSON", text.as_str())],
    );
    assert_eq!(
        out.trim(),
        "stale",
        "ntp_failed:true must refuse even with a fresh ntp_age_s: {out:?}"
    );
}

#[test]
fn ntp_freshness_verdict_never_when_age_is_null() {
    let text = pipe_json_ntp(1000, 999999, "null", false);
    let out = run_sourced(
        "ntp_freshness_verdict \"$JSON\" 300",
        &[("JSON", text.as_str())],
    );
    assert_eq!(
        out.trim(),
        "never",
        "ntp_age_s:null means NEVER measured, distinct from a stale numeric age: {out:?}"
    );
}

#[test]
fn ntp_freshness_verdict_absent_when_field_is_missing_entirely() {
    let old_shape =
        "{\"updated_ts\":1000,\"ntp_offset_us\":100,\"is_locked\":true,\"mode\":\"NANO\"}";
    let out = run_sourced(
        "ntp_freshness_verdict \"$JSON\" 300",
        &[("JSON", old_shape)],
    );
    assert_eq!(
        out.trim(),
        "absent",
        "no ntp_age_s field at all (pre-1.8.30 payload) -> absent, caller falls back: {out:?}"
    );
}

#[test]
fn ntp_freshness_verdict_fails_closed_on_a_malformed_freshness_window() {
    let text = pipe_json_ntp(1000, 100, "4", false);
    for bad_fresh in ["abc", "", "-1"] {
        let out = run_sourced(
            &format!("ntp_freshness_verdict \"$JSON\" '{bad_fresh}'"),
            &[("JSON", text.as_str())],
        );
        assert_eq!(
            out.trim(),
            "absent",
            "malformed freshness={bad_fresh:?} must refuse to certify freshness, not silently \
             pass: {out:?}"
        );
    }
}

// --- frozen_sample_verdict (#1014 pre-1.8.30 backward-compat fallback) -----------------------

#[test]
fn frozen_sample_verdict_frozen_when_every_distinct_sample_is_byte_identical() {
    // #1014's ORIGINAL incident, reproduced exactly: several distinct-by-updated_ts reads that
    // all report the SAME ntp_offset_us -- the signature a dead/free-running NTP measurement
    // leaves in a payload with no ntp_age_s field to check directly.
    let payloads = format!(
        "{}\n{}\n{}\n",
        pipe_json(1000, -34718),
        pipe_json(1030, -34718),
        pipe_json(1060, -34718),
    );
    let out = run_sourced(
        "frozen_sample_verdict \"$P\" 3",
        &[("P", payloads.as_str())],
    );
    assert_eq!(
        out.trim(),
        "frozen",
        "identical offset across 3 distinct samples must be frozen: {out:?}"
    );
}

#[test]
fn frozen_sample_verdict_live_when_samples_vary() {
    let payloads = format!(
        "{}\n{}\n{}\n",
        pipe_json(1000, 100),
        pipe_json(1030, 150),
        pipe_json(1060, 120),
    );
    let out = run_sourced(
        "frozen_sample_verdict \"$P\" 3",
        &[("P", payloads.as_str())],
    );
    assert_eq!(
        out.trim(),
        "live",
        "varying samples must never be called frozen: {out:?}"
    );
}

#[test]
fn frozen_sample_verdict_insufficient_when_too_few_distinct_samples() {
    let payloads = format!("{}\n{}\n", pipe_json(1000, 50), pipe_json(1000, 50));
    let out = run_sourced(
        "frozen_sample_verdict \"$P\" 3",
        &[("P", payloads.as_str())],
    );
    assert_eq!(
        out.trim(),
        "insufficient",
        "1 distinct sample of the required 3 must never be graded frozen or live: {out:?}"
    );

    let out = run_sourced(
        "frozen_sample_verdict \"$P\" abc",
        &[("P", payloads.as_str())],
    );
    assert_eq!(
        out.trim(),
        "insufficient",
        "malformed min_distinct must refuse to grade: {out:?}"
    );
}

// --- sampled_offset_verdict / sampled_offset_check "median-only" MODE (#1014 Part 2) ----------
//
// The NTP master's own spread is a by-design correction-lag sawtooth (dantesync issue 71), not a
// fleet-coherence signal -- "median-only" mode must skip the spread/stability check entirely
// while leaving the median (location) bound fully enforced.

#[test]
fn sampled_offset_verdict_median_only_ignores_wild_scatter_when_median_is_in_bound() {
    // The EXACT scattered fixture that verdicts "unstable" in full mode (see
    // sampled_offset_verdict_unstable_when_median_is_fine_but_samples_scatter_wildly above) must
    // verdict "ok" in median-only mode.
    let payloads = format!(
        "{}\n{}\n{}\n{}\n{}\n",
        pipe_json(1000, -19800),
        pipe_json(1025, 20100),
        pipe_json(1050, 200),
        pipe_json(1075, -19500),
        pipe_json(1100, 19900),
    );
    let out = run_sourced(
        "sampled_offset_verdict \"$P\" 2000 2000 3 median-only",
        &[("P", payloads.as_str())],
    );
    assert_eq!(
        out.trim(),
        "ok",
        "median-only mode must never grade unstable, regardless of spread: {out:?}"
    );
}

#[test]
fn sampled_offset_verdict_median_only_still_fails_on_genuine_drift() {
    let payloads = format!(
        "{}\n{}\n{}\n",
        pipe_json(1000, 8000),
        pipe_json(1025, 8050),
        pipe_json(1050, 8100),
    );
    let out = run_sourced(
        "sampled_offset_verdict \"$P\" 2000 2000 3 median-only",
        &[("P", payloads.as_str())],
    );
    assert_eq!(
        out.trim(),
        "drift",
        "median-only mode still grades the LOCATION bound -- a genuinely drifted master must \
         still fail: {out:?}"
    );
}

#[test]
fn sampled_offset_verdict_median_only_still_requires_min_distinct() {
    let payloads = format!("{}\n{}\n", pipe_json(1000, 50), pipe_json(1000, 50));
    let out = run_sourced(
        "sampled_offset_verdict \"$P\" 2000 2000 3 median-only",
        &[("P", payloads.as_str())],
    );
    assert_eq!(
        out.trim(),
        "insufficient",
        "median-only mode does not relax the distinct-sample-count requirement: {out:?}"
    );
}

#[test]
fn sampled_offset_verdict_default_mode_is_unchanged_full_grading() {
    // Every pre-#1014 4-arg call site must behave byte-for-byte the same -- proves MODE truly
    // defaults to "full" when omitted, not silently to "median-only".
    let payloads = format!(
        "{}\n{}\n{}\n{}\n{}\n",
        pipe_json(1000, -19800),
        pipe_json(1025, 20100),
        pipe_json(1050, 200),
        pipe_json(1075, -19500),
        pipe_json(1100, 19900),
    );
    let out = run_sourced(
        "sampled_offset_verdict \"$P\" 2000 2000 3",
        &[("P", payloads.as_str())],
    );
    assert_eq!(
        out.trim(),
        "unstable",
        "omitting MODE must still grade the spread (full mode): {out:?}"
    );
}

#[test]
fn sampled_offset_check_median_only_reports_an_inline_master_note_never_a_second_line() {
    let payloads = format!(
        "{}\n{}\n{}\n{}\n{}\n",
        pipe_json(1000, -19800),
        pipe_json(1025, 20100),
        pipe_json(1050, 200),
        pipe_json(1075, -19500),
        pipe_json(1100, 19900),
    );
    let out = run_sourced(
        "set +e; sampled_offset_check strih \"$P\" 2000 2000 3 median-only; echo \"rc=$?\"",
        &[("P", payloads.as_str())],
    );
    assert!(
        out.contains("OK") && out.contains("rc=0"),
        "median-only mode must report OK on a scattered-but-in-bound-median node: {out:?}"
    );
    assert!(
        !out.contains("UNSTABLE"),
        "median-only mode must never print UNSTABLE: {out:?}"
    );
    assert_eq!(
        out.lines()
            .filter(|l| l.trim_start().starts_with("strih"))
            .count(),
        1,
        "exactly ONE line for this node -- callers locate 'the' report via \
         .lines().find(starts_with(name)), so a second line would silently break them: {out:?}"
    );
    assert!(
        out.contains("NTP MASTER") && out.contains("not gated"),
        "the inline note must explain why no stability verdict was made: {out:?}"
    );
}

#[test]
fn sampled_offset_check_accepts_a_caller_supplied_extra_note() {
    let payloads = format!(
        "{}\n{}\n{}\n",
        pipe_json(1000, 100),
        pipe_json(1025, 150),
        pipe_json(1050, 120),
    );
    let out = run_sourced(
        "sampled_offset_check strih \"$P\" 2000 2000 3 full ' -- extra caller note'",
        &[("P", payloads.as_str())],
    );
    assert!(
        out.contains("extra caller note"),
        "a 7th positional NOTE argument must be appended verbatim: {out:?}"
    );
}

// --- ntp_deadband_us_from_pipe_json / ntp_master_effective_bound_us (#1021) --------------------
//
// dantesync PR #84/#86 (closes dantesync issue 83): a genuinely PTP-locked NTP master now
// deliberately DEFERS its periodic UTC-phase step to a "deadband" (live-tuned to 2500us, the
// supervisor's 2026-08-12 comment on #1021 -- NOT the 25ms originally filed) instead of the old
// tight ~200us threshold, and additively reports the currently-active threshold as
// "ntp_deadband_us" in its own /status. A healthy master's own ntp_offset_us therefore legitimately
// ramps up toward roughly that value between corrections -- the fixed GATE_BOUND_US (2000us, sized
// for a client's tight NTP-vs-LAN-master offset) would false-DRIFT on this by-design behavior.
// These tests pin the two new pure functions that let the NTP-master row's median bound adapt to
// the live deadband while leaving every other node's bound (and the master's own bound when the
// field is absent/null) exactly as before.

/// A minimal DanteSync status-pipe JSON payload carrying only the fields these two new parsers
/// care about (updated_ts/offset are irrelevant noise for this narrow test, included only so the
/// payload still parses like a real capture).
fn pipe_json_deadband(deadband_raw: &str) -> String {
    format!(
        "{{\"updated_ts\":1000,\"ntp_offset_us\":100,\"is_locked\":true,\"mode\":\"NANO\",\
         \"ntp_deadband_us\":{deadband_raw}}}"
    )
}

#[test]
fn ntp_deadband_us_from_pipe_json_reads_numeric_null_and_absent_1021() {
    let numeric = pipe_json_deadband("2500");
    let out = run_sourced(
        "ntp_deadband_us_from_pipe_json \"$JSON\"",
        &[("JSON", numeric.as_str())],
    );
    assert_eq!(
        out.trim(),
        "2500",
        "must read a numeric ntp_deadband_us: {out:?}"
    );

    let never = pipe_json_deadband("null");
    let out = run_sourced(
        "ntp_deadband_us_from_pipe_json \"$JSON\"",
        &[("JSON", never.as_str())],
    );
    assert_eq!(
        out.trim(),
        "null",
        "must read the literal null verbatim (a client node's own reported shape), never coerce \
         it to empty or 0: {out:?}"
    );

    let absent = "{\"updated_ts\":1000,\"ntp_offset_us\":100,\"is_locked\":true,\"mode\":\"NANO\"}";
    let out = run_sourced(
        "ntp_deadband_us_from_pipe_json \"$JSON\"",
        &[("JSON", absent)],
    );
    assert_eq!(
        out.trim(),
        "",
        "a pre-dantesync-#84 payload with no ntp_deadband_us field at all -> empty (absent): {out:?}"
    );
}

#[test]
fn ntp_master_effective_bound_us_widens_the_bound_when_deadband_present_1021() {
    let status = pipe_json_deadband("2500");
    let out = run_sourced(
        "ntp_master_effective_bound_us \"$JSON\" 2000 1000",
        &[("JSON", status.as_str())],
    );
    assert_eq!(
        out.trim(),
        "3500",
        "max(2000, 2500+1000) = 3500 -- the deadband+margin floor wins over the fixed bound: {out:?}"
    );
}

#[test]
fn ntp_master_effective_bound_us_never_lowers_below_the_fixed_bound_1021() {
    // A tiny deadband+margin must never DROP the effective bound below the caller's own
    // GATE_BOUND_US -- the widening is a FLOOR, never a ceiling override.
    let status = pipe_json_deadband("50");
    let out = run_sourced(
        "ntp_master_effective_bound_us \"$JSON\" 2000 100",
        &[("JSON", status.as_str())],
    );
    assert_eq!(
        out.trim(),
        "2000",
        "max(2000, 50+100=150) = 2000 -- the fixed bound must never be lowered: {out:?}"
    );
}

#[test]
fn ntp_master_effective_bound_us_falls_back_on_null_absent_and_negative_deadband_1021() {
    let cases = [
        pipe_json_deadband("null"),
        "{\"updated_ts\":1000,\"ntp_offset_us\":100}".to_string(),
        pipe_json_deadband("-5"),
    ];
    for status in cases {
        let out = run_sourced(
            "ntp_master_effective_bound_us \"$JSON\" 2000 1000",
            &[("JSON", status.as_str())],
        );
        assert_eq!(
            out.trim(),
            "2000",
            "null/absent/negative ntp_deadband_us must fall back to the unmodified fixed bound, \
             the exact pre-#1021 behavior (status={status}): {out:?}"
        );
    }
}

#[test]
fn ntp_master_effective_bound_us_fails_closed_on_malformed_bound_or_margin_1021() {
    // #595's bash gotcha: an unvalidated numeric input fed into a `[ N -gt M ]` comparison can
    // silently misbehave rather than error. Both BOUND_US and MARGIN_US are validated with
    // `grep -qE '^[0-9]+$'` BEFORE any arithmetic -- a malformed one must fall back to the raw
    // (unmodified) bound text, never crash and never silently invent a number.
    let status = pipe_json_deadband("2500");
    for (bound, margin) in [("abc", "1000"), ("2000", "abc"), ("-5", "1000")] {
        let out = run_sourced(
            &format!("ntp_master_effective_bound_us \"$JSON\" '{bound}' '{margin}'"),
            &[("JSON", status.as_str())],
        );
        assert_eq!(
            out.trim(),
            bound,
            "a malformed BOUND_US/MARGIN_US must fall back to the RAW (unmodified) bound text, \
             never guess a number (bound={bound} margin={margin}): {out:?}"
        );
    }
}

// --- client_chase_bound_us (#1022) --------------------------------------------------------------
//
// #1021 (above) widens ONLY the NTP-master row's own median bound against ITS OWN
// self-reported ntp_deadband_us. #1022 (dantesync-gate: client rows false-DRIFT during the
// master's deadband step-chase window) found live that a CLIENT node's own ntp_offset_us ALSO
// legitimately ramps during that same window -- "when the master finally steps, every fleet
// client steps by the same amount within its next NTP measurement cycle" -- because a client
// measures its offset against the master over the LAN, and the master's own accumulated-but-
// not-yet-corrected phase shows up there too. A client always reports its OWN
// "ntp_deadband_us":null, so the widening must be derived from a DIFFERENT node's (the master's)
// status. client_chase_bound_us is that sibling of ntp_master_effective_bound_us: same shape,
// but (a) reads a caller-supplied MASTER status (not the node being graded), and (b) CAPS the
// deadband component at a CEILING_US (the ticket's own cited "upstream hard per-step ceiling",
// 5000us) before adding the margin -- since this can widen MANY client rows per gate run (not
// just the one master row), an unbounded floor would be a far bigger blast radius than #1021's
// single-row widening if ntp_deadband_us were ever misreported/misconfigured.

#[test]
fn client_chase_bound_us_widens_when_master_deadband_present_1022() {
    let master_status = pipe_json_deadband("2500");
    let out = run_sourced(
        "client_chase_bound_us \"$JSON\" 2000 1000 5000",
        &[("JSON", master_status.as_str())],
    );
    assert_eq!(
        out.trim(),
        "3500",
        "max(2000, min(2500,5000)+1000) = 3500 -- the master's own deadband+margin floor wins \
         over the fixed client bound: {out:?}"
    );
}

#[test]
fn client_chase_bound_us_never_lowers_below_the_fixed_bound_1022() {
    let master_status = pipe_json_deadband("50");
    let out = run_sourced(
        "client_chase_bound_us \"$JSON\" 2000 100 5000",
        &[("JSON", master_status.as_str())],
    );
    assert_eq!(
        out.trim(),
        "2000",
        "max(2000, min(50,5000)+100=150) = 2000 -- the fixed client bound must never be lowered: \
         {out:?}"
    );
}

#[test]
fn client_chase_bound_us_falls_back_on_null_absent_and_negative_master_deadband_1022() {
    let cases = [
        pipe_json_deadband("null"),
        "{\"updated_ts\":1000,\"ntp_offset_us\":100}".to_string(),
        pipe_json_deadband("-5"),
    ];
    for status in cases {
        let out = run_sourced(
            "client_chase_bound_us \"$JSON\" 2000 1000 5000",
            &[("JSON", status.as_str())],
        );
        assert_eq!(
            out.trim(),
            "2000",
            "null/absent/negative master ntp_deadband_us -- e.g. a pre-dantesync-#84 master, or \
             the master's HTTP read simply failing -- must fall back to the unmodified fixed \
             client bound, never a blind widen (status={status}): {out:?}"
        );
    }
}

#[test]
fn client_chase_bound_us_caps_the_deadband_component_at_the_hard_ceiling_1022() {
    // An (unrealistic, defensive-test) absurdly large master deadband must NOT blindly widen
    // every client's bound to match it -- the ceiling caps the deadband component FIRST, so a
    // client median that clears the capped envelope still DRIFTs even though it would have
    // PASSED under an uncapped (blind) widen.
    let master_status = pipe_json_deadband("50000");
    let out = run_sourced(
        "client_chase_bound_us \"$JSON\" 2000 1000 5000",
        &[("JSON", master_status.as_str())],
    );
    assert_eq!(
        out.trim(),
        "6000",
        "max(2000, min(50000,5000)+1000) = 6000 -- the 5000us ceiling caps the deadband \
         component before the margin is added, never the raw 50000us: {out:?}"
    );
}

#[test]
fn client_chase_bound_us_fails_closed_on_malformed_bound_margin_or_ceiling_1022() {
    // Same #595 bash-gotcha discipline as ntp_master_effective_bound_us above: BOUND_US,
    // MARGIN_US, and CEILING_US are each validated with `grep -qE '^[0-9]+$'` BEFORE any
    // arithmetic -- a malformed one falls back to the RAW (unmodified) bound text, never a crash
    // and never a silently-invented number.
    let master_status = pipe_json_deadband("2500");
    let cases = [
        ("abc", "1000", "5000"),
        ("2000", "abc", "5000"),
        ("2000", "1000", "abc"),
        ("-5", "1000", "5000"),
    ];
    for (bound, margin, ceiling) in cases {
        let out = run_sourced(
            &format!("client_chase_bound_us \"$JSON\" '{bound}' '{margin}' '{ceiling}'"),
            &[("JSON", master_status.as_str())],
        );
        assert_eq!(
            out.trim(),
            bound,
            "a malformed BOUND_US/MARGIN_US/CEILING_US must fall back to the RAW (unmodified) \
             bound text, never guess a number \
             (bound={bound} margin={margin} ceiling={ceiling}): {out:?}"
        );
    }
}

// --- #1022 review hardening: leading-zero numeric input must never crash the arithmetic --------
//
// Both client_chase_bound_us and its master-row sibling validate BOUND_US/MARGIN_US/CEILING_US/
// the parsed deadband with `grep -qE '^[0-9]+$'` (or `^-?[0-9]+$'` for the possibly-negative
// deadband) -- a regex that happily ACCEPTS a leading-zero digit string like "0900". Bash's
// `$((...))` arithmetic expansion (unlike its `[ -gt/-lt ]` test comparisons, which stay decimal)
// treats a leading "0" as an OCTAL prefix -- "0900" contains the digit 9, which is not valid
// octal, so `$((capped + margin))` aborts the WHOLE sourcing shell under `set -e` with "value too
// great for base" instead of reaching the documented graceful fallback. A live repro (found in
// review): `client_chase_bound_us '{"ntp_deadband_us":2500}' 2000 0900 5000` crashes the calling
// shell outright. Reachable via an operator's zero-padded --deadband-margin-us (or a client
// mistakenly configured with --client-chase-ceiling-us zero-padded), and in principle via a
// deadband value extracted from a malformed/adversarial status payload (well-formed JSON itself
// disallows a leading-zero numeric literal, but the grep-based extractor here does not enforce
// that). The fix normalizes every validated numeric operand to canonical base-10 (`10#$var`)
// immediately before the ONE arithmetic expression each function contains -- never changing the
// MEANING of a valid decimal string (leading zeros are conventionally decimal, never octal, to a
// human reading a CLI flag), only how bash's arithmetic evaluator parses it.

#[test]
fn client_chase_bound_us_normalizes_leading_zero_margin_and_deadband_never_crashes_1022() {
    // margin="0900" must mean decimal 900, not trip bash's octal-prefix parsing (which would
    // abort on the digit 9) and not silently mean something else either.
    let master_status = pipe_json_deadband("2500");
    let out = run_sourced(
        "client_chase_bound_us \"$JSON\" 2000 0900 5000",
        &[("JSON", master_status.as_str())],
    );
    assert_eq!(
        out.trim(),
        "3400",
        "max(2000, min(2500,5000)+900) = 3400 -- a leading-zero margin must be read as decimal \
         900, and must never crash the calling shell: {out:?}"
    );

    // A leading-zero DEADBAND (the value this function caps and adds the margin to) is the same
    // hazard from the OTHER operand -- exercised via the ceiling path too (capped can end up
    // holding either the deadband's or the ceiling's raw text).
    let deadband_leading_zero = pipe_json_deadband("0900");
    let out = run_sourced(
        "client_chase_bound_us \"$JSON\" 2000 1000 5000",
        &[("JSON", deadband_leading_zero.as_str())],
    );
    assert_eq!(
        out.trim(),
        "2000",
        "max(2000, min(900,5000)+1000=1900) = 2000 -- a leading-zero deadband must be read as \
         decimal 900, never crash: {out:?}"
    );

    let out = run_sourced(
        "client_chase_bound_us \"$JSON\" 2000 0100 05000",
        &[("JSON", master_status.as_str())],
    );
    assert_eq!(
        out.trim(),
        "2600",
        "max(2000, min(2500,5000)+100) = 2600 -- a leading-zero margin AND ceiling together must \
         still compute the correct decimal result: {out:?}"
    );
}

#[test]
fn ntp_master_effective_bound_us_normalizes_leading_zero_margin_and_deadband_never_crashes_1022() {
    let status_deadband = pipe_json_deadband("2500");
    let out = run_sourced(
        "ntp_master_effective_bound_us \"$JSON\" 2000 0900",
        &[("JSON", status_deadband.as_str())],
    );
    assert_eq!(
        out.trim(),
        "3400",
        "max(2000, 2500+900) = 3400 -- a leading-zero margin must be read as decimal 900, and \
         must never crash the calling shell: {out:?}"
    );

    let status_leading_zero_deadband = pipe_json_deadband("0900");
    let out = run_sourced(
        "ntp_master_effective_bound_us \"$JSON\" 2000 1000",
        &[("JSON", status_leading_zero_deadband.as_str())],
    );
    assert_eq!(
        out.trim(),
        "2000",
        "max(2000, 900+1000=1900) = 2000 -- a leading-zero deadband must be read as decimal 900, \
         never crash: {out:?}"
    );
}

// --- #1022 spread-side completion: max_abs_of_ints / should_resample_for_chase -----------------
//
// client_chase_bound_us (above) only ever widens a CLIENT row's MEDIAN check -- the spread/
// stability check stays fully active by design. Live evidence from a merged round's real E2E
// rerun showed the SAME master step that the median fix already handles can ALSO inflate a
// client's SPREAD past the fixed 2000us stability bound (one elevated sample inside an otherwise-
// baseline window), and because the step is on ONE clock shared by the fleet, the SAME step can
// trip MULTIPLE clients in the SAME run. should_resample_for_chase decides whether an "unstable"
// verdict is worth ONE fresh resample before failing -- these tests pin the pure DECISION only;
// the actual re-gather + delay is exercised end-to-end in tests/dantesync_gate.rs.

#[test]
fn max_abs_of_ints_returns_the_largest_magnitude_regardless_of_sign() {
    let out = run_sourced("max_abs_of_ints \"$L\"", &[("L", "100\n-2682\n50\n")]);
    assert_eq!(
        out.trim(),
        "2682",
        "the negative value's magnitude must win: {out:?}"
    );
}

#[test]
fn max_abs_of_ints_empty_on_no_valid_values() {
    let out = run_sourced("max_abs_of_ints \"$L\"", &[("L", "")]);
    assert_eq!(
        out.trim(),
        "",
        "no samples -> empty, never a guessed number: {out:?}"
    );

    let out = run_sourced(
        "max_abs_of_ints \"$L\"",
        &[("L", "not-a-number\nalso-bad\n")],
    );
    assert_eq!(
        out.trim(),
        "",
        "non-integer lines must be ignored, not crash: {out:?}"
    );
}

#[test]
fn max_abs_of_ints_single_value() {
    let out = run_sourced("max_abs_of_ints \"$L\"", &[("L", "-42\n")]);
    assert_eq!(out.trim(), "42", "a single value's own magnitude: {out:?}");
}

#[test]
fn should_resample_for_chase_yes_when_unstable_and_worst_sample_fits_the_bound() {
    // The live cam1 shape: median 0us (two near-zero samples), one elevated sample (2682us) that
    // pushes spread past the 2000us stability bound -- but 2682 <= the (already #1022-widened)
    // 3500us bound, so this looks like a plausible single step-chase excursion, worth a resample.
    let payloads = format!(
        "{}\n{}\n{}\n",
        pipe_json(1000, 0),
        pipe_json(1025, 0),
        pipe_json(1050, 2682),
    );
    let out = run_sourced(
        "should_resample_for_chase \"$P\" 3500 2000 3 full",
        &[("P", payloads.as_str())],
    );
    assert_eq!(
        out.trim(),
        "yes",
        "unstable + worst sample within bound -> yes: {out:?}"
    );
}

#[test]
fn should_resample_for_chase_no_when_verdict_is_ok() {
    let payloads = format!(
        "{}\n{}\n{}\n",
        pipe_json(1000, 10),
        pipe_json(1025, 20),
        pipe_json(1050, 15),
    );
    let out = run_sourced(
        "should_resample_for_chase \"$P\" 3500 2000 3 full",
        &[("P", payloads.as_str())],
    );
    assert_eq!(
        out.trim(),
        "no",
        "a healthy ok verdict has nothing to resample for: {out:?}"
    );
}

#[test]
fn should_resample_for_chase_no_when_verdict_is_drift_or_drift_unstable() {
    // drift: median itself exceeds the bound -- no resample can fix a real out-of-bound median.
    let drift_payloads = format!(
        "{}\n{}\n{}\n",
        pipe_json(1000, 9000),
        pipe_json(1025, 9100),
        pipe_json(1050, 8900),
    );
    let out = run_sourced(
        "should_resample_for_chase \"$P\" 3500 2000 3 full",
        &[("P", drift_payloads.as_str())],
    );
    assert_eq!(
        out.trim(),
        "no",
        "drift (median out of bound) must never resample: {out:?}"
    );

    // drift_unstable: both median AND spread fail. median([5000,5100,9000])=5100 > bound(2000)
    // -> drift; spread(9000-5000=4000) > stability(1000) -> unstable too. Verified directly by
    // sourcing the script (sampled_offset_verdict returns "drift_unstable" for these exact
    // inputs) -- a review finding caught an earlier version of this fixture that accidentally
    // produced a plain "unstable" verdict instead, so this sub-case never exercised the
    // drift_unstable branch of the verdict gate at all.
    let drift_unstable_payloads = format!(
        "{}\n{}\n{}\n",
        pipe_json(1000, 5000),
        pipe_json(1025, 5100),
        pipe_json(1050, 9000),
    );
    let out = run_sourced(
        "should_resample_for_chase \"$P\" 2000 1000 3 full",
        &[("P", drift_unstable_payloads.as_str())],
    );
    assert_eq!(
        out.trim(),
        "no",
        "drift_unstable (median ALSO out of bound) must never resample: {out:?}"
    );
}

#[test]
fn should_resample_for_chase_no_when_insufficient_distinct_samples() {
    let payloads = format!("{}\n", pipe_json(1000, 0));
    let out = run_sourced(
        "should_resample_for_chase \"$P\" 3500 2000 3 full",
        &[("P", payloads.as_str())],
    );
    assert_eq!(
        out.trim(),
        "no",
        "too few distinct samples -> no resample: {out:?}"
    );
}

#[test]
fn should_resample_for_chase_no_for_the_master_median_only_mode() {
    // The exact SAME sample shape that returns "yes" under "full" mode above must return "no"
    // under "median-only" -- the master's own row never has a spread verdict to begin with
    // (sampled_offset_verdict skips the spread check entirely in median-only mode), so this must
    // be excluded both by the explicit mode check AND structurally (verdict can never be
    // "unstable" in that mode).
    let payloads = format!(
        "{}\n{}\n{}\n",
        pipe_json(1000, 0),
        pipe_json(1025, 0),
        pipe_json(1050, 2682),
    );
    let out = run_sourced(
        "should_resample_for_chase \"$P\" 3500 2000 3 median-only",
        &[("P", payloads.as_str())],
    );
    assert_eq!(
        out.trim(),
        "no",
        "the master's median-only row must never resample: {out:?}"
    );
}

#[test]
fn should_resample_for_chase_no_when_the_worst_sample_exceeds_the_bound() {
    // median 0us (in bound), spread 3600us (over stability) -- but the worst sample (3600) itself
    // exceeds the 3500us bound: this looks bigger than any legitimate single step-chase excursion
    // could produce, so it must fail immediately, never get a second chance via resample (the
    // #836 genuine-scatter class, or a real clock fault, is never masked).
    let payloads = format!(
        "{}\n{}\n{}\n",
        pipe_json(1000, 0),
        pipe_json(1025, 0),
        pipe_json(1050, 3600),
    );
    let out = run_sourced(
        "should_resample_for_chase \"$P\" 3500 2000 3 full",
        &[("P", payloads.as_str())],
    );
    assert_eq!(
        out.trim(),
        "no",
        "a worst sample beyond the bound must never resample, even though median stays in bound: \
         {out:?}"
    );
}

#[test]
fn should_resample_for_chase_no_on_malformed_bound() {
    let payloads = format!(
        "{}\n{}\n{}\n",
        pipe_json(1000, 0),
        pipe_json(1025, 0),
        pipe_json(1050, 2682),
    );
    let out = run_sourced(
        "should_resample_for_chase \"$P\" abc 2000 3 full",
        &[("P", payloads.as_str())],
    );
    assert_eq!(
        out.trim(),
        "no",
        "a malformed BOUND_US must never resample: {out:?}"
    );
}
