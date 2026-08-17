//! #794 — pure-function guard for `scripts/lib/cadence-health.sh`, the SHARED decision core for the
//! dev1-side NON-60 SOURCE-CADENCE alert watchdog (a fourth sibling of the network-reach (issue
//! 1001) / frozen-input (issue 1052) / bundle-state (issue 732) dev1 alert-watchdog family).
//!
//! Root cause (issue 794, live 2026-07-17): a camera set to 50 fps feeding the 60 fps chain went
//! undetected for weeks. Every fps counter reads the CANVAS rate (60/30), and a grabber that
//! upconverts 50->60 by DUPLICATION delivers a genuine 60 NDI frames/s — so the canonical figures
//! are all clean. The ONE per-source signal that reflects the true NDI DELIVERY rate is the
//! genlock-fifo `received=` cumulative counter on the receiver (strih, for camera inputs): a camera
//! genuinely delivering 50/43 fps advances `received=` at 50/43 per second.
//!
//! THE LOAD-BEARING DECISION — the issue-797 "phantom 50.1 fps" avoidance: the rate's numerator
//! (Δreceived) AND its denominator (Δt) BOTH come from the SAME two matched audit lines' OWN log
//! timestamps — never a wall-clock / pass-interval divisor. The audit line appends every ~5.017 s;
//! a naive 6 s WALL-window spans ~one tick (+301 frames at a true 60) → 301/6 = 50.17 → a phantom
//! "50" at EVERY true-60 source. `cadence_measure_fps` is proven below to read ~60 (NOT ~50) from a
//! single-tick window precisely because it divides Δreceived by the LINES' OWN Δt (5.017 s), not 6.
//!
//! Same convention as `tests/harness_frozen_input_health_1052.rs` /
//! `tests/harness_network_reach_health_1001.rs`: source the REAL lib (source-only, no side effects)
//! and exercise the pure functions directly. RED before the lib exists (sourcing fails / the file is
//! absent, every test fails); GREEN after. `cargo` does NOT run locally here (Tier-0, build-ok
//! DISABLED #477) — the observable local red→green is `bash -c '. scripts/lib/cadence-health.sh'`;
//! CI runs these assertions.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/cadence-health.sh");
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

/// Pull the numeric value of a `key=value` token out of a `cadence_measure_fps` line.
fn field(line: &str, key: &str) -> String {
    for tok in line.split_whitespace() {
        if let Some(v) = tok.strip_prefix(&format!("{key}=")) {
            return v.to_string();
        }
    }
    String::new()
}

fn measured_fps(out: &str) -> f64 {
    let v = field(out, "fps");
    v.parse::<f64>()
        .unwrap_or_else(|_| panic!("fps not numeric in: {out:?}"))
}

// ---------------------------------------------------------------------------------------------
// lib shape — the pure functions must be defined
// ---------------------------------------------------------------------------------------------
#[test]
fn lib_defines_the_pure_functions() {
    for f in [
        "cadence_ts_to_seconds",
        "cadence_measure_fps",
        "cadence_classify",
        "cadence_recovery_decision",
        "cadence_alert_detail",
    ] {
        let out = stdout_of(&format!("type {f} >/dev/null 2>&1 && echo DEFINED"));
        assert_eq!(out, "DEFINED", "{f} is not defined by the lib");
    }
}

// ---------------------------------------------------------------------------------------------
// cadence_ts_to_seconds <HH:MM:SS.mmm[:]> -> float seconds-of-day (empty if unparseable)
// ---------------------------------------------------------------------------------------------
fn ts(s: &str) -> String {
    stdout_of(&format!("cadence_ts_to_seconds \"{s}\""))
}

#[test]
fn ts_to_seconds_parses_the_obs_log_prefix() {
    assert!((ts("14:00:05.017").parse::<f64>().unwrap() - 50405.017).abs() < 1e-3);
    // The OBS log prefix carries a trailing colon (`14:00:05.017:`) — must parse the same.
    assert!((ts("14:00:05.017:").parse::<f64>().unwrap() - 50405.017).abs() < 1e-3);
    assert!((ts("00:00:00.000").parse::<f64>().unwrap() - 0.0).abs() < 1e-3);
    assert!((ts("23:59:59.999").parse::<f64>().unwrap() - 86399.999).abs() < 1e-3);
    // A timestamp with no milliseconds still parses (whole-second precision).
    assert!((ts("01:02:03").parse::<f64>().unwrap() - 3723.0).abs() < 1e-3);
}

#[test]
fn ts_to_seconds_is_empty_for_garbage() {
    assert_eq!(ts(""), "");
    assert_eq!(ts("not-a-timestamp"), "");
    assert_eq!(ts("99:99"), "");
}

// ---------------------------------------------------------------------------------------------
// cadence_measure_fps <prev_ts> <prev_received> <curr_ts> <curr_received>
//   -> `fps=<F> window_s=<W> advanced=<0|1>` (or `fps= window_s= advanced=` when unmeasurable)
// ---------------------------------------------------------------------------------------------
fn measure(pt: &str, pr: &str, ct: &str, cr: &str) -> String {
    stdout_of(&format!("cadence_measure_fps \"{pt}\" {pr} \"{ct}\" {cr}"))
}

#[test]
fn measure_healthy_60fps_over_a_full_pass_window() {
    // 18000 frames over the two lines' own 300 s span => exactly 60.000 fps.
    let out = measure("14:00:00.000", "0", "14:05:00.000", "18000");
    assert!((measured_fps(&out) - 60.0).abs() < 0.05, "want ~60, got {out}");
    assert_eq!(field(&out, "advanced"), "1");
    assert!((field(&out, "window_s").parse::<f64>().unwrap() - 300.0).abs() < 1e-3);
}

#[test]
fn measure_avoids_the_phantom_50_from_a_single_audit_tick_797() {
    // THE load-bearing test. The issue-797 bug divided +301 frames by a 6 s WALL sleep => 50.17.
    // Here the SAME +301 frames are divided by the two audit lines' OWN 5.017 s span => ~60, NOT 50.
    // This proves the rate is computed from the lines' own timestamps, never a wall-clock divisor.
    let out = measure("14:00:00.000", "1000", "14:00:05.017", "1301");
    let fps = measured_fps(&out);
    assert!(
        (59.5..=60.5).contains(&fps),
        "single-tick window must read ~60 (301/5.017), got {fps} — a ~50 here is the phantom-50 bug"
    );
    assert!(
        (fps - 50.17).abs() > 5.0,
        "must NOT read the phantom 50.17 (301/6-wall), got {fps}"
    );
}

#[test]
fn measure_reads_a_genuine_50fps_delivery() {
    // A camera genuinely delivering 50 fps advances received= 15000 over a 300 s span.
    let out = measure("14:00:00.000", "0", "14:05:00.000", "15000");
    assert!((measured_fps(&out) - 50.0).abs() < 0.05, "want ~50, got {out}");
    assert_eq!(field(&out, "advanced"), "1");
}

#[test]
fn measure_reads_a_genuine_43fps_delivery() {
    // The dispatch's own example: "a camera silently delivering 43/50 fps must alarm".
    let out = measure("14:00:00.000", "0", "14:05:00.000", "12900");
    assert!((measured_fps(&out) - 43.0).abs() < 0.05, "want ~43, got {out}");
}

#[test]
fn measure_counter_reset_is_unmeasurable() {
    // curr < prev => the cumulative counter reset (OBS restarted) -> no rate, reseed upstream.
    let out = measure("14:00:00.000", "500000", "14:05:00.000", "12");
    assert_eq!(field(&out, "fps"), "", "reset must be unmeasurable: {out}");
    assert_eq!(field(&out, "advanced"), "");
}

#[test]
fn measure_frozen_source_advances_zero_and_is_flagged_not_advancing() {
    // received= identical across the window => a FROZEN source (issue-1052's job), advanced=0.
    // cadence_classify turns advanced=0 into UNKNOWN, never a "wrong 0 fps" page.
    let out = measure("14:00:00.000", "1000", "14:05:00.000", "1000");
    assert_eq!(field(&out, "advanced"), "0", "frozen source: {out}");
    assert!((measured_fps(&out) - 0.0).abs() < 1e-6);
}

#[test]
fn measure_non_numeric_received_is_unmeasurable() {
    assert_eq!(field(&measure("14:00:00.000", "abc", "14:05:00.000", "18000"), "fps"), "");
    assert_eq!(field(&measure("14:00:00.000", "0", "14:05:00.000", "xx"), "fps"), "");
}

#[test]
fn measure_handles_midnight_wrap() {
    // The OBS log is date-less; a window straddling midnight (curr seconds-of-day < prev) must add
    // 86400 rather than read a negative/absurd window. 23:59:30 -> 00:04:30 is a real 300 s span.
    let out = measure("23:59:30.000", "0", "00:04:30.000", "18000");
    assert!((measured_fps(&out) - 60.0).abs() < 0.05, "wrap must read ~60, got {out}");
    assert!((field(&out, "window_s").parse::<f64>().unwrap() - 300.0).abs() < 1e-3);
}

#[test]
fn measure_stale_prev_beyond_the_cap_is_unmeasurable() {
    // A prev sample many hours old (watchdog was down) -> the window is implausible -> reseed, never
    // compute a rate over a stale span.
    let out = measure("00:00:00.000", "0", "10:00:00.000", "2160000");
    assert_eq!(field(&out, "fps"), "", "stale prev must be unmeasurable: {out}");
}

#[test]
fn measure_no_prior_timestamp_is_unmeasurable() {
    // An empty/absent prev timestamp (first pass / a reseed) -> nothing to measure against.
    let out = measure("", "0", "14:05:00.000", "18000");
    assert_eq!(field(&out, "fps"), "", "no prior ts must be unmeasurable: {out}");
}

// ---------------------------------------------------------------------------------------------
// cadence_classify <fps> <window_s> <advanced> <expected_fps> <tolerance_fps> <min_window_s>
//                  <expected_live> <box_reachable>  -> OK | WRONG | UNKNOWN | SKIP
// ---------------------------------------------------------------------------------------------
fn classify(
    fps: &str,
    win: &str,
    adv: &str,
    exp: &str,
    tol: &str,
    minw: &str,
    live: &str,
    reach: &str,
) -> String {
    stdout_of(&format!(
        "cadence_classify \"{fps}\" \"{win}\" \"{adv}\" {exp} {tol} {minw} {live} {reach}"
    ))
}

#[test]
fn classify_60fps_is_ok() {
    assert_eq!(classify("60.000", "300", "1", "60", "3", "60", "1", "1"), "OK");
}

#[test]
fn classify_ntsc_5994_is_ok_within_tolerance() {
    // 59.94 (NTSC 60000/1001) is a legitimate "60" and must stay inside the ±3 band.
    assert_eq!(classify("59.940", "300", "1", "60", "3", "60", "1", "1"), "OK");
}

#[test]
fn classify_50fps_is_wrong() {
    assert_eq!(classify("50.000", "300", "1", "60", "3", "60", "1", "1"), "WRONG");
}

#[test]
fn classify_43fps_is_wrong() {
    assert_eq!(classify("43.000", "300", "1", "60", "3", "60", "1", "1"), "WRONG");
}

#[test]
fn classify_above_band_is_also_wrong() {
    // The band is symmetric — an implausibly-high rate is wrong too.
    assert_eq!(classify("72.000", "300", "1", "60", "3", "60", "1", "1"), "WRONG");
}

#[test]
fn classify_tolerance_boundary_is_inclusive_ok_exclusive_wrong() {
    // |57-60| == 3 is NOT > tolerance -> OK (edge inclusive); 56.9 (3.1 off) -> WRONG.
    assert_eq!(classify("57.000", "300", "1", "60", "3", "60", "1", "1"), "OK");
    assert_eq!(classify("63.000", "300", "1", "60", "3", "60", "1", "1"), "OK");
    assert_eq!(classify("56.900", "300", "1", "60", "3", "60", "1", "1"), "WRONG");
}

#[test]
fn classify_frozen_advanced_zero_is_unknown_not_wrong() {
    // A frozen source (advanced=0) is issue-1052's job — never double-classified as a cadence fault.
    assert_eq!(classify("0.000", "300", "0", "60", "3", "60", "1", "1"), "UNKNOWN");
}

#[test]
fn classify_short_window_is_unknown() {
    // A window below the minimum trustable span (first pass / reseed) -> UNKNOWN, never a noisy page.
    assert_eq!(classify("60.000", "30", "1", "60", "3", "60", "1", "1"), "UNKNOWN");
}

#[test]
fn classify_unmeasurable_is_unknown() {
    // Empty fps/window (counter reset / unreadable) -> UNKNOWN.
    assert_eq!(classify("", "", "", "60", "3", "60", "1", "1"), "UNKNOWN");
}

#[test]
fn classify_not_expected_live_is_skip() {
    // A source not listed as expected-live is out of scope regardless of its measured rate.
    assert_eq!(classify("50.000", "300", "1", "60", "3", "60", "0", "1"), "SKIP");
}

#[test]
fn classify_box_unreachable_is_skip_never_double_page() {
    // strih down per issue-1001 -> that watchdog owns the page; a cadence reading is out of scope.
    assert_eq!(classify("50.000", "300", "1", "60", "3", "60", "1", "0"), "SKIP");
}

#[test]
fn classify_defensive_non_one_flags_are_skip() {
    assert_eq!(classify("50", "300", "1", "60", "3", "60", "yes", "1"), "SKIP");
    assert_eq!(classify("50", "300", "1", "60", "3", "60", "1", "up"), "SKIP");
}

// ---------------------------------------------------------------------------------------------
// cadence_recovery_decision <was_alerted> <now_ok> -> recover=0|1
// ---------------------------------------------------------------------------------------------
fn recover(was_alerted: &str, now_ok: &str) -> String {
    stdout_of(&format!("cadence_recovery_decision {was_alerted} {now_ok}"))
}

#[test]
fn recovery_only_when_previously_alerted_and_now_ok() {
    assert_eq!(recover("1", "1"), "recover=1"); // paged, cadence back to ~60 -> recovery ping
    assert_eq!(recover("1", "0"), "recover=0"); // still wrong -> no recovery yet
    assert_eq!(recover("0", "1"), "recover=0"); // never paged (healthy all along) -> no ping
    assert_eq!(recover("0", "0"), "recover=0");
    assert_eq!(recover("1", "x"), "recover=0"); // defensive: non-"1" now_ok -> no ping
}

// ---------------------------------------------------------------------------------------------
// cadence_alert_detail <source> <measured_fps> <expected_fps> -> human line for the Discord body
// ---------------------------------------------------------------------------------------------
#[test]
fn alert_detail_names_source_and_measured_vs_expected() {
    let d = stdout_of("cadence_alert_detail \"NDI cam5\" 50.0 60");
    assert!(d.contains("NDI cam5"), "detail must name the source: {d}");
    assert!(d.contains("50"), "detail must show the measured fps: {d}");
    assert!(d.contains("60"), "detail must show the expected fps: {d}");
}
