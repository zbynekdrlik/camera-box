//! #739 — pure-function guard for `scripts/lib/splitter-health.sh`, the SHARED decision core for the
//! dev1-side per-cambox HDMI-splitter-port no-signal recurrence watch.
//!
//! Root cause (issue 739, live 2026-07-13): the rig feeds ONE camera through an HDMI splitter to every
//! cambox, so per-cambox capture can only differ by each box's INDIVIDUAL leg (its splitter output
//! port + cable/grabber). When 4/6 splitter ports died, the boxes on those ports saw no signal while
//! siblings saw the shared camera — but each grabber renders no-signal differently (Elgato 4K S =
//! purple noise; ShadowCast 2 = flat grey), so the failures MASQUERADED as per-camera "colour" bugs
//! and burned two days of tint-hunting. The masquerade happened because each box's colour was judged
//! IN ISOLATION instead of COMPARED against the fleet consensus — the one comparison that isolates a
//! per-port fault (one box degraded) from a shared-source fault (all boxes degraded).
//!
//! The discriminator this lib encodes: a box is a SPLITTER-PORT suspect iff it is degraded AND at
//! least one SIBLING is proven-good (reachable + capturing + colour). A proven-good sibling proves the
//! shared camera is delivering and dev1's path to the rig is up, so the only element that can differ
//! for the bad box is its own output port. If EVERY reachable box is equally degraded → shared source
//! (camera off / AWB / idle rig), NOT a per-port fault → never a false page.
//!
//! Same convention as `tests/harness_network_reach_health_1001.rs`: source the REAL lib (source-only,
//! no side effects) and exercise the pure functions directly. RED before the lib exists (sourcing
//! fails, every test fails); GREEN after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/splitter-health.sh");
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

// ---------------------------------------------------------------------------------------------
// lib shape — the pure functions must be defined
// ---------------------------------------------------------------------------------------------
#[test]
fn lib_defines_the_pure_functions() {
    for f in [
        "splitter_health_parse_probe",
        "splitter_health_is_healthy",
        "splitter_health_classify",
        "splitter_health_alert_detail",
    ] {
        let out = stdout_of(&format!("type {f} >/dev/null 2>&1 && echo DEFINED"));
        assert_eq!(out, "DEFINED", "{f} is not defined by the lib");
    }
}

// ---------------------------------------------------------------------------------------------
// splitter_health_parse_probe <raw> -> reachable=.. capturing=.. colour=.. u_dev=.. v_dev=..
//   Parses one box's raw ssh probe output. The remote command echoes `PROBE_OK` on a successful
//   connection, then optionally the box's last `capture chroma:` journal line (last 90s).
// ---------------------------------------------------------------------------------------------
#[test]
fn parse_ssh_failure_empty_output_is_not_reachable() {
    // ssh connect failed / box off the wire -> empty output -> NODATA, never a false signal.
    let out = stdout_of("splitter_health_parse_probe \"\"");
    assert!(
        out.contains("reachable=0"),
        "empty probe must be unreachable: {out}"
    );
    assert!(
        out.contains("capturing=0"),
        "unreachable => not capturing: {out}"
    );
    assert!(out.contains("colour=0"), "unreachable => colour 0: {out}");
}

#[test]
fn parse_reachable_and_capturing_colour() {
    // healthy: connected + a fresh colour chroma line (the #299 metric).
    let out = stdout_of(
        "splitter_health_parse_probe $'PROBE_OK\\ncapture chroma: u_dev=7.2 v_dev=15.9 -> colour'",
    );
    assert!(out.contains("reachable=1"), "PROBE_OK => reachable: {out}");
    assert!(
        out.contains("capturing=1"),
        "fresh chroma line => capturing: {out}"
    );
    assert!(out.contains("colour=1"), "'-> colour' => colour=1: {out}");
    assert!(out.contains("u_dev=7.2"), "must carry u_dev: {out}");
    assert!(out.contains("v_dev=15.9"), "must carry v_dev: {out}");
}

#[test]
fn parse_reachable_capturing_but_grayscale() {
    // the ShadowCast dead-port mode: frames flow but the content is flat grey.
    let out = stdout_of(
        "splitter_health_parse_probe $'PROBE_OK\\ncapture chroma: u_dev=0.5 v_dev=0.4 -> grayscale (source likely monochrome)'",
    );
    assert!(out.contains("reachable=1"), "PROBE_OK => reachable: {out}");
    assert!(
        out.contains("capturing=1"),
        "fresh chroma line => capturing: {out}"
    );
    assert!(
        out.contains("colour=0"),
        "'-> grayscale' => colour=0: {out}"
    );
    assert!(out.contains("u_dev=0.5"), "must carry u_dev: {out}");
}

#[test]
fn parse_reachable_but_no_recent_capture_line() {
    // connected but no `capture chroma:` line in the window: capture stalled or camera-box down.
    let out = stdout_of("splitter_health_parse_probe $'PROBE_OK\\n'");
    assert!(out.contains("reachable=1"), "PROBE_OK => reachable: {out}");
    assert!(
        out.contains("capturing=0"),
        "no chroma line => not capturing: {out}"
    );
    assert!(out.contains("colour=0"), "not capturing => colour=0: {out}");
}

// ---------------------------------------------------------------------------------------------
// splitter_health_is_healthy <reachable> <capturing> <colour> -> 1 | 0
//   A "proven-good sibling": reachable AND capturing AND colour.
// ---------------------------------------------------------------------------------------------
#[test]
fn is_healthy_only_when_all_three_present() {
    assert_eq!(stdout_of("splitter_health_is_healthy 1 1 1"), "1");
    assert_eq!(stdout_of("splitter_health_is_healthy 1 1 0"), "0"); // grayscale
    assert_eq!(stdout_of("splitter_health_is_healthy 1 0 0"), "0"); // not capturing
    assert_eq!(stdout_of("splitter_health_is_healthy 0 1 1"), "0"); // unreachable
    assert_eq!(stdout_of("splitter_health_is_healthy x \"\" -"), "0"); // garbage -> 0 defensively
}

// ---------------------------------------------------------------------------------------------
// splitter_health_classify <reachable> <capturing> <colour> <healthy_siblings> -> verdict=..
//   NODATA (unreachable), OK, DEAD_PORT (degraded + a proven-good sibling exists),
//   SOURCE_WIDE (degraded + NO proven-good sibling => shared source / idle, not a port).
// ---------------------------------------------------------------------------------------------
fn classify(r: &str, c: &str, k: &str, sib: &str) -> String {
    stdout_of(&format!("splitter_health_classify {r} {c} {k} {sib}"))
}

#[test]
fn classify_unreachable_is_nodata() {
    // never a per-port claim for a box we could not read (box off / network) — mirrors the sibling
    // watchdogs' "no probe output = nothing to decide".
    assert_eq!(classify("0", "0", "0", "2"), "verdict=NODATA");
    assert_eq!(classify("0", "1", "1", "0"), "verdict=NODATA");
}

#[test]
fn classify_capturing_colour_is_ok() {
    assert_eq!(classify("1", "1", "1", "0"), "verdict=OK");
    assert_eq!(classify("1", "1", "1", "2"), "verdict=OK");
}

#[test]
fn classify_grayscale_with_healthy_sibling_is_dead_port() {
    // the exact 2026-07-13 masquerade: one box flat-grey while ≥1 sibling shows the shared camera in
    // colour => that box's own leg (splitter port) is the suspect.
    assert_eq!(classify("1", "1", "0", "1"), "verdict=DEAD_PORT");
    assert_eq!(classify("1", "1", "0", "2"), "verdict=DEAD_PORT");
}

#[test]
fn classify_not_capturing_is_no_capture_report_only_regardless_of_siblings() {
    // capturing=0 is an AMBIGUOUS class (camera-box down / device-busy / E2E-stop / grabber stall),
    // ROUTINE on this rig — it must NEVER page as a splitter-port fault (that would be a
    // mis-attribution). NO_CAPTURE regardless of how many siblings are healthy. The ORIGINAL dead-port
    // failure kept the grabber PRODUCING frames (capturing=1) with bad content; that is DEAD_PORT below.
    assert_eq!(classify("1", "0", "0", "2"), "verdict=NO_CAPTURE");
    assert_eq!(classify("1", "0", "0", "1"), "verdict=NO_CAPTURE");
    assert_eq!(classify("1", "0", "0", "0"), "verdict=NO_CAPTURE");
}

#[test]
fn classify_grayscale_without_healthy_sibling_is_source_wide() {
    // every reachable box capturing-but-grayscale => shared camera/source (AWB on a B&W pattern) or
    // idle rig, NOT a per-port fault.
    assert_eq!(classify("1", "1", "0", "0"), "verdict=SOURCE_WIDE");
}

#[test]
fn classify_garbage_sibling_count_treated_as_zero() {
    // a non-numeric sibling count must never be read as "a healthy sibling exists" -> SOURCE_WIDE, not
    // a false DEAD_PORT page. (grayscale + garbage sibling => SOURCE_WIDE, the report-only side.)
    assert_eq!(classify("1", "1", "0", "x"), "verdict=SOURCE_WIDE");
    assert_eq!(classify("1", "1", "0", "\"\""), "verdict=SOURCE_WIDE");
}

// ---------------------------------------------------------------------------------------------
// splitter_health_alert_detail <box> <capturing> <colour> <u_dev> <v_dev> -> one human line
// ---------------------------------------------------------------------------------------------
#[test]
fn alert_detail_not_capturing_names_box_and_ambiguous_cause_not_splitter_port() {
    // the NOT-capturing line is the NO_CAPTURE (report-only) detail — it must describe the ambiguous
    // cause honestly and must NOT attribute the fault to the splitter port (that is only the paged
    // grayscale line's claim).
    let d = stdout_of("splitter_health_alert_detail cam3 0 0 - -");
    assert!(d.contains("cam3"), "must name the box: {d}");
    assert!(
        d.to_lowercase().contains("not capturing") || d.to_lowercase().contains("no fresh"),
        "must state the not-capturing reason: {d}"
    );
    assert!(
        d.to_lowercase().contains("ambiguous") || d.to_lowercase().contains("not attributed"),
        "not-capturing must NOT be attributed to the splitter port: {d}"
    );
}

#[test]
fn alert_detail_grayscale_names_box_port_and_chroma() {
    let d = stdout_of("splitter_health_alert_detail cam2 1 0 0.5 0.4");
    assert!(d.contains("cam2"), "must name the box: {d}");
    assert!(
        d.to_lowercase().contains("splitter port"),
        "must name the splitter port as suspect: {d}"
    );
    assert!(
        d.to_lowercase().contains("grayscale"),
        "must state grayscale: {d}"
    );
    assert!(
        d.contains("0.5") && d.contains("0.4"),
        "must carry the chroma numbers: {d}"
    );
}
