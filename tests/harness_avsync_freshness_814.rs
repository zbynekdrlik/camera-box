//! #814 — the grab-freshness gate as a PURE, unit-tested decider.
//!
//! Root incident: after the live stream ended the RTMP relay stopped serving, `ffmpeg -y` failed
//! (rc=-5) and LEFT the previous 35 s clip on disk, so the watchdog re-measured that SAME stale
//! clip for 2h09m and emitted its verdict as if live ("adjust latency by 80 ms" every ~5 min with
//! nothing live). The live hotfix asserted rc==0 + size + mtime-age + ffprobe-duration before any
//! verdict; this file productizes that hotfix as the SINGLE SOURCE OF TRUTH — the pure decider
//! `scripts/avsync_freshness.py` — and proves it FUNCTIONALLY (a stale clip / failed grab MUST
//! yield NO-SIGNAL, never an offset), not just as a `body.contains("200000")` static anchor.
//!
//! The pure decider's CLI contract:
//!   python3 scripts/avsync_freshness.py --grab-rc R --size-bytes S --mtime-age-s A --duration-s D
//!     -> stdout "OK"                (exit 0)  when the grab is proven CURRENT
//!     -> stdout "NO-SIGNAL: <reason>" (exit 10) otherwise (fail-CLOSED on any malformed input)

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const FRESHNESS_PY: &str = "scripts/avsync_freshness.py";
const WATCHDOG_PS1: &str = "scripts/avsync-watchdog.ps1";
const MEASURE_PY: &str = "scripts/av_sync_measure.py";
const INSTALL_SH: &str = "scripts/avsync-watchdog-install.sh";

// Run the pure decider CLI. Returns (exit_code, stdout_trimmed).
fn verdict(grab_rc: &str, size_bytes: &str, mtime_age_s: &str, duration_s: &str) -> (i32, String) {
    let script = manifest_dir().join(FRESHNESS_PY);
    let out = Command::new("python3")
        .arg(&script)
        .args([
            "--grab-rc",
            grab_rc,
            "--size-bytes",
            size_bytes,
            "--mtime-age-s",
            mtime_age_s,
            "--duration-s",
            duration_s,
        ])
        .output()
        .unwrap_or_else(|e| panic!("run {}: {e} (is python3 on PATH?)", script.display()));
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

// ================================================================================================
// The pure decider — the FUNCTIONAL matrix (this is what the ticket's "unit-tested gate" means).
// ================================================================================================

#[test]
fn healthy_fresh_clip_is_allowed() {
    let (code, out) = verdict("0", "500000", "5", "35");
    assert_eq!(
        code, 0,
        "a fresh, big-enough, long-enough clip must be allowed: {out}"
    );
    assert_eq!(out, "OK", "the allowed verdict is exactly 'OK': {out}");
}

#[test]
fn dead_relay_failed_grab_yields_no_signal_never_an_offset() {
    // THE incident: ffmpeg rc=-5, the clip was removed first so there is no file at all.
    let (code, out) = verdict("-5", "-1", "-1", "-1");
    assert_eq!(code, 10, "a failed grab must be NO-SIGNAL: {out}");
    assert!(
        out.starts_with("NO-SIGNAL:"),
        "must be a NO-SIGNAL verdict: {out}"
    );
    assert!(
        out.contains("rc=-5"),
        "the reason must name the ffmpeg rc: {out}"
    );
    assert!(
        out.to_lowercase().contains("relay") || out.to_lowercase().contains("stream down"),
        "the reason must point at the dead relay/stream: {out}"
    );
}

#[test]
fn stale_clip_left_on_disk_after_a_failed_grab_is_no_signal_rc_checked_first() {
    // The subtler shape: ffmpeg failed (rc=-5) but the PREVIOUS clip is still on disk and looks
    // big + long -- ONLY its mtime age is huge. rc must be checked FIRST so this can never slip
    // through as a stale verdict (this is the exact 2h09m frozen-input path).
    let (code, out) = verdict("-5", "500000", "7200", "35");
    assert_eq!(
        code, 10,
        "rc!=0 must win regardless of a plausible-looking stale clip: {out}"
    );
    assert!(
        out.contains("rc=-5"),
        "the reason must name the failed grab, not the size/age: {out}"
    );
}

#[test]
fn rc_zero_but_no_clip_is_no_signal() {
    let (code, out) = verdict("0", "-1", "-1", "-1");
    assert_eq!(
        code, 10,
        "rc==0 but no clip produced must be NO-SIGNAL: {out}"
    );
    assert!(
        out.to_lowercase().contains("no clip"),
        "reason must say no clip: {out}"
    );
}

#[test]
fn clip_too_small_is_no_signal() {
    let (code, out) = verdict("0", "100000", "5", "35");
    assert_eq!(code, 10, "a <200kB clip must be NO-SIGNAL: {out}");
    assert!(
        out.to_lowercase().contains("too small"),
        "reason must say too small: {out}"
    );
}

#[test]
fn clip_stale_by_mtime_age_is_no_signal() {
    let (code, out) = verdict("0", "500000", "200", "35");
    assert_eq!(
        code, 10,
        "a clip older than the age bound must be NO-SIGNAL: {out}"
    );
    assert!(
        out.to_uppercase().contains("STALE"),
        "reason must say STALE: {out}"
    );
}

#[test]
fn clip_too_short_is_no_signal() {
    let (code, out) = verdict("0", "500000", "5", "10");
    assert_eq!(code, 10, "a <20s clip must be NO-SIGNAL: {out}");
    assert!(
        out.to_lowercase().contains("too short"),
        "reason must say too short: {out}"
    );
}

#[test]
fn unknown_duration_minus_one_does_not_penalize() {
    // ffprobe unavailable -> dur=-1. The live gate deliberately does NOT fail on an unknown
    // duration (`$dur -ge 0 -and $dur -lt 20`); only a POSITIVE-but-too-short duration fails.
    let (code, out) = verdict("0", "500000", "5", "-1");
    assert_eq!(
        code, 0,
        "an unknown (negative) duration must not fail the gate: {out}"
    );
    assert_eq!(out, "OK", "{out}");
}

#[test]
fn thresholds_are_inclusive_boundaries() {
    // The exact live thresholds (200000 B, 180 s, 20 s) live in the decider now, tested as the
    // single source of truth. size==min, age==max, dur==min are all still OK (the fails are
    // strictly `< min` / `> max`).
    assert_eq!(
        verdict("0", "200000", "180", "20"),
        (0, "OK".into()),
        "boundaries are inclusive"
    );
    assert_eq!(
        verdict("0", "199999", "180", "20").0,
        10,
        "one byte under the size floor fails"
    );
    assert_eq!(
        verdict("0", "200000", "181", "20").0,
        10,
        "one second over the age ceiling fails"
    );
    assert_eq!(
        verdict("0", "200000", "180", "19").0,
        10,
        "one second under the duration floor fails"
    );
}

#[test]
fn malformed_input_fails_closed_never_ok() {
    // A corrupt/absent value must be treated as NO-SIGNAL, never silently allowed (this repo's
    // standing fail-closed discipline -- cf. avsync_heartbeat_is_stale's "missing/corrupt = stale").
    let (code, out) = verdict("0", "not-a-number", "5", "35");
    assert_eq!(
        code, 10,
        "a malformed size must fail closed to NO-SIGNAL, never OK: {out}"
    );
    assert!(out.starts_with("NO-SIGNAL:"), "{out}");
}

// ================================================================================================
// The three consumers wire to the pure decider (single source of truth) -- static anchors.
// ================================================================================================

#[test]
fn watchdog_ps1_delegates_the_decision_to_the_pure_gate_no_inline_thresholds() {
    let body = read(WATCHDOG_PS1);
    assert!(
        body.contains("avsync_freshness.py"),
        "the ps1 must DELEGATE the fresh/stale decision to the pure decider, not duplicate it"
    );
    assert!(
        body.contains("--grab-rc")
            && body.contains("--size-bytes")
            && body.contains("--mtime-age-s")
            && body.contains("--duration-s"),
        "the ps1 must pass the four grab facts (rc, size, mtime-age, duration) to the decider"
    );
    // The magic numbers must be GONE from the ps1 -- they now live only in the decider.
    assert!(
        !body.contains("200000") && !body.contains("< 20"),
        "the freshness thresholds must no longer be duplicated inline in the ps1 (single source of \
         truth is scripts/avsync_freshness.py)"
    );
    // The grab URL is confirmed correct and must be unchanged.
    assert!(
        body.contains("rtmp://127.0.0.1:1234/live/obs-e2e-test"),
        "the grab URL is the real production broadcast -- must be unchanged"
    );
}

#[test]
fn av_sync_measure_can_require_freshness_before_the_heavy_measurement() {
    let body = read(MEASURE_PY);
    assert!(
        body.contains("--require-fresh") && body.contains("--grab-rc"),
        "av_sync_measure.py must gain an opt-in freshness assert so a direct --media call on a \
         stale clip cannot emit a verdict (the incident's literal root cause)"
    );
    assert!(
        body.contains("avsync_freshness"),
        "it must import the SAME pure decider, never a second copy of the thresholds"
    );
    assert!(
        body.contains("NO-SIGNAL"),
        "the stale path must print a NO-SIGNAL marker, never a measured offset"
    );
}

#[test]
fn installer_deploys_the_decider_and_self_tests_a_dead_relay() {
    let body = read(INSTALL_SH);
    assert!(
        body.contains("avsync_freshness.py"),
        "the installer must deploy the pure decider to the box (the ps1 + av_sync_measure both need it)"
    );
    assert!(
        body.contains("avsync_freshness_go_no_go"),
        "the installer must carry a #813 GO/NO-GO self-test function for the freshness gate"
    );
}

#[test]
fn installer_go_no_go_proves_dead_relay_yields_no_signal() {
    // #813 GO/NO-GO: sourcing the installer and running its self-test must prove that a dead-relay
    // grab (rc=-5) yields NO-SIGNAL rather than a stale verdict.
    let script = manifest_dir().join(INSTALL_SH);
    let out = Command::new("bash")
        .arg("-c")
        .arg(". \"$SCRIPT\"\nset +e\navsync_freshness_go_no_go")
        .env("SCRIPT", &script)
        .output()
        .expect("source installer + run self-test");
    assert!(
        out.status.success(),
        "the freshness GO/NO-GO self-test must pass (dead relay -> NO-SIGNAL): stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("NO-SIGNAL"),
        "the self-test must actually exercise the gate and observe NO-SIGNAL: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}
