//! #739 — driver-level guard for `scripts/splitter-port-alert-watchdog.sh`'s `main()` fleet
//! aggregation: the sibling-count arithmetic (`healthy_siblings = total_healthy − self`), the
//! parse-once consumption of the lib's `key=value` record, and the per-box verdict → action wiring.
//! The pure lib is pinned by `tests/harness_splitter_port_health_739.rs`; this file pins the IMPURE
//! driver that composes it — the two most bug-prone pieces a reviewer flagged as otherwise untested
//! (an off-by-one in the sibling math, or a drift between the lib's record format and the driver's
//! re-read, is exactly the "a real dead port would NOT page" failure mode).
//!
//! Method: the driver guards `main` behind `[[ "${BASH_SOURCE[0]}" == "$0" ]]`, so sourcing it only
//! DEFINES its functions. We source it in `--dry-run`, override `probe_box` with a canned fleet and
//! `sshpass` with a no-op (so the tool preflight passes with no real ssh), run `main` N times against
//! a per-test temp state file, and assert on the log (stderr). No rig, no network, no notify.

use std::path::PathBuf;
use std::process::Command;
use tempfile::tempdir;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/splitter-port-alert-watchdog.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the watchdog, override `probe_box`/`sshpass`, run `main` `passes` times in dry-run against
/// the canned per-IP fleet in `probe_cases` (a bash `case "$1" in … esac` body). Returns stderr (the
/// `log()` stream). A per-test tempdir isolates the state file (never a shared host path — #975).
fn run_driver(probe_cases: &str, passes: usize) -> String {
    let dir = tempdir().expect("tempdir");
    let state = dir.path().join("splitter.state");
    let mut mains = String::new();
    for _ in 0..passes {
        mains.push_str("main\n");
    }
    let body = format!(
        "set -uo pipefail\n\
         export CAMERA_ACTIVE_SET='cam1 cam2 cam3'\n\
         export SPLITTER_WATCH_STATE_FILE='{state}'\n\
         . \"$SCRIPT\" --dry-run\n\
         sshpass() {{ :; }}\n\
         probe_box() {{ case \"$1\" in {cases} esac; }}\n\
         {mains}",
        state = state.display(),
        cases = probe_cases,
        mains = mains,
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&body)
        .env("SCRIPT", script())
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness");
    // The watchdog logs (incl. the dry-run "WOULD alert" line) to stderr via log().
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "watchdog main() exited non-zero:\n{stderr}"
    );
    stderr
}

// A canned box output: PROBE_OK + a colour / grayscale chroma line, or empty (ssh fail).
const COLOUR: &str = r"printf 'PROBE_OK\ncapture chroma: u_dev=6.1 v_dev=8.8 -> colour\n'";
const GREY: &str = r"printf 'PROBE_OK\ncapture chroma: u_dev=0.5 v_dev=0.4 -> grayscale (source likely monochrome)\n'";
const NO_LINE: &str = r"printf 'PROBE_OK\n'"; // reachable, but no fresh capture line
const SSH_FAIL: &str = r"printf ''"; // ssh connect failed -> empty

// #1079: a colour box whose frame ALSO carries the new rough= spatial-roughness metric.
const COLOUR_ROUGH: &str =
    r"printf 'PROBE_OK\ncapture chroma: u_dev=6.1 v_dev=8.8 rough=52.3 -> colour\n'";

#[test]
fn driver_mixed_fleet_pages_dead_port_for_the_grey_box() {
    // cam2 flat-grey while cam1+cam3 deliver the shared camera in colour -> DEAD_PORT for cam2, which
    // pages after the 2-pass confirm. This is the exact 2026-07-13 masquerade.
    let cases = format!("10.77.9.62) {GREY} ;; *) {COLOUR} ;;");
    let log = run_driver(&cases, 2);
    assert!(
        log.contains("fleet proven-good count = 2 of 3"),
        "cam1+cam3 are the 2 proven-good: {log}"
    );
    assert!(
        log.contains("cam2 (10.77.9.62)") && log.contains("DEAD_PORT"),
        "cam2 => DEAD_PORT: {log}"
    );
    assert!(
        log.contains("WOULD alert: cam2 CONFIRMED DEAD_PORT"),
        "cam2 must page after 2 passes: {log}"
    );
    assert!(
        log.contains("splitter port"),
        "the page must name the splitter port as suspect: {log}"
    );
    // cam1/cam3 stay OK and never page.
    assert!(
        log.contains("cam1 (10.77.9.61)") && log.contains("-> OK"),
        "cam1 OK: {log}"
    );
    assert!(
        !log.contains("WOULD alert: cam1"),
        "cam1 must not page: {log}"
    );
    assert!(
        !log.contains("WOULD alert: cam3"),
        "cam3 must not page: {log}"
    );
}

#[test]
fn driver_surfaces_rough_metric_and_purple_noise_report_only_1099() {
    // #1099: the threshold is now CALIBRATED + WIRED report-only. A colour box whose rough EXCEEDS
    // the threshold (52.3 > 40.0) classifies PURPLE_NOISE (the Elgato structureless-static no-signal
    // mode) — but it is REPORT-ONLY: the per-box log surfaces the rough= number AND the PURPLE_NOISE
    // verdict, yet it NEVER pages (the live page is deferred until a real positive-class episode
    // calibrates the noise floor + adds the sibling self-anchor). Two passes prove even a "sustained"
    // report-only verdict never escalates to a page.
    let cases = format!("*) {COLOUR_ROUGH} ;;");
    let log = run_driver(&cases, 2);
    assert!(
        log.contains("rough=52.3"),
        "the per-box log must surface the rough= metric: {log}"
    );
    assert!(
        log.contains("PURPLE_NOISE"),
        "colour + rough>threshold => PURPLE_NOISE report-only: {log}"
    );
    assert!(
        log.to_uppercase().contains("NOISE-SUSPECT"),
        "the per-box report-only line must flag NOISE-SUSPECT: {log}"
    );
    assert!(
        !log.contains("WOULD alert"),
        "report-only: PURPLE_NOISE must never page: {log}"
    );
}

#[test]
fn driver_purple_noise_box_alongside_clean_siblings_pages_nothing_1099() {
    // a mixed fleet: cam2 reads colour+high-rough (PURPLE_NOISE), cam1/cam3 read clean colour (OK).
    // The noise box is surfaced report-only; NOTHING in the fleet pages (report-only phase 2).
    let cases = format!("10.77.9.62) {COLOUR_ROUGH} ;; *) {COLOUR} ;;");
    let log = run_driver(&cases, 2);
    assert!(
        log.contains("cam2 (10.77.9.62)") && log.contains("PURPLE_NOISE"),
        "cam2 => PURPLE_NOISE: {log}"
    );
    assert!(
        log.lines()
            .any(|l| l.contains("cam1 (10.77.9.61)") && l.contains("-> OK")),
        "cam1 clean colour => OK on its OWN log line: {log}"
    );
    assert!(
        !log.contains("WOULD alert"),
        "report-only: no box pages this phase: {log}"
    );
}

#[test]
fn driver_first_pass_holds_dead_port_below_confirm_threshold() {
    // ONE pass: cam2 is DEAD_PORT but the 2-pass confirm must HOLD it (no page yet) — a single
    // transient grey read must never page.
    let cases = format!("10.77.9.62) {GREY} ;; *) {COLOUR} ;;");
    let log = run_driver(&cases, 1);
    assert!(
        log.contains("DEAD_PORT"),
        "cam2 classified DEAD_PORT: {log}"
    );
    assert!(
        log.contains("not yet CONFIRMED") && !log.contains("WOULD alert"),
        "one pass must hold, not page: {log}"
    );
}

#[test]
fn driver_all_grey_is_source_wide_never_pages() {
    // every reachable box grayscale => no proven-good sibling => SOURCE_WIDE (shared camera/AWB/idle),
    // report-only. Two passes prove it never crosses into a page.
    let cases = format!("*) {GREY} ;;");
    let log = run_driver(&cases, 2);
    assert!(
        log.contains("fleet proven-good count = 0 of 3"),
        "none proven-good: {log}"
    );
    assert!(
        log.contains("SOURCE_WIDE"),
        "all-grey => SOURCE_WIDE: {log}"
    );
    assert!(
        !log.contains("WOULD alert"),
        "SOURCE_WIDE must never page: {log}"
    );
    assert!(
        !log.contains("DEAD_PORT"),
        "no box may be DEAD_PORT when none is proven-good: {log}"
    );
}

#[test]
fn driver_not_capturing_is_no_capture_never_pages() {
    // reachable but no fresh capture line on every box => NO_CAPTURE (routine cambox-down/E2E-stop),
    // report-only. Must never page a splitter-port suspicion.
    let cases = format!("*) {NO_LINE} ;;");
    let log = run_driver(&cases, 2);
    assert!(
        log.contains("NO_CAPTURE"),
        "no capture line => NO_CAPTURE: {log}"
    );
    assert!(
        !log.contains("WOULD alert"),
        "NO_CAPTURE must never page: {log}"
    );
    assert!(
        !log.contains("DEAD_PORT"),
        "NO_CAPTURE is not a DEAD_PORT: {log}"
    );
}

#[test]
fn driver_unreachable_box_is_nodata_and_not_a_sibling() {
    // cam3 ssh-fails (empty) => NODATA (never a per-port claim for it), and it does NOT count toward
    // the proven-good fleet: cam1+cam2 colour => 2 proven-good, cam3 NODATA.
    let cases = format!("10.77.9.63) {SSH_FAIL} ;; *) {COLOUR} ;;");
    let log = run_driver(&cases, 2);
    assert!(
        log.contains("fleet proven-good count = 2 of 3"),
        "cam3 unreachable not counted: {log}"
    );
    assert!(
        log.contains("cam3 (10.77.9.63)") && log.contains("NODATA"),
        "cam3 => NODATA: {log}"
    );
    assert!(
        !log.contains("WOULD alert"),
        "no page when the only degraded box is NODATA: {log}"
    );
}

#[test]
fn driver_sibling_math_grey_box_with_a_lone_colour_sibling_pages() {
    // Pins the sibling arithmetic against an off-by-one: cam2 grey, cam1 colour (the ONLY proven-good
    // box), cam3 unreachable. cam2's proven-good siblings = total_healthy(1) − self(0) = 1 => DEAD_PORT
    // must still fire (a single healthy sibling is enough). Conversely cam1's own siblings = 1 − 1 = 0
    // but it's OK anyway, so the self-exclusion never suppresses cam2's real sibling.
    let cases = format!("10.77.9.61) {COLOUR} ;; 10.77.9.62) {GREY} ;; 10.77.9.63) {SSH_FAIL} ;;");
    let log = run_driver(&cases, 2);
    assert!(
        log.contains("fleet proven-good count = 1 of 3"),
        "only cam1 proven-good: {log}"
    );
    assert!(
        log.contains("WOULD alert: cam2 CONFIRMED DEAD_PORT"),
        "a single colour sibling must still trigger cam2's page (no off-by-one): {log}"
    );
}

#[test]
fn driver_lone_grey_box_with_no_reachable_sibling_never_pages() {
    // The off-by-one's dangerous direction: cam2 grey, cam1+cam3 unreachable. cam2's proven-good
    // siblings = 0 => SOURCE_WIDE (no proof the shared camera is delivering), NEVER a DEAD_PORT page.
    let cases = format!("10.77.9.62) {GREY} ;; *) {SSH_FAIL} ;;");
    let log = run_driver(&cases, 2);
    assert!(
        log.contains("fleet proven-good count = 0 of 3"),
        "none proven-good: {log}"
    );
    assert!(
        log.contains("cam2 (10.77.9.62)") && log.contains("SOURCE_WIDE"),
        "cam2 => SOURCE_WIDE: {log}"
    );
    assert!(
        !log.contains("WOULD alert"),
        "a lone grey box with no reachable sibling must not page: {log}"
    );
    assert!(
        !log.contains("DEAD_PORT"),
        "must not be DEAD_PORT without a proven-good sibling: {log}"
    );
}

#[test]
fn noise_threshold_watchdog_mirror_matches_rust_const_1099() {
    // #1099: the watchdog's NOISE_ROUGHNESS_THRESHOLD default MUST stay in lock-step with the Rust
    // source-of-truth src/capture.rs::NOISE_ROUGHNESS_THRESHOLD (bash cannot read the const, so it
    // mirrors the literal). A future const change that forgets the mirror would silently desync the
    // dev1 gate from the appliance classifier; this pins them (mirrors the repo's other const-mirror
    // parity gates). Normalised numerically so 40.0 == 40.
    let rust =
        std::fs::read_to_string(manifest_dir().join("src/capture.rs")).expect("read capture.rs");
    let sh =
        std::fs::read_to_string(manifest_dir().join("scripts/splitter-port-alert-watchdog.sh"))
            .expect("read watchdog");
    let rust_val = rust
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("pub const NOISE_ROUGHNESS_THRESHOLD: f32 = ")
                .and_then(|s| s.strip_suffix(';'))
        })
        .expect("Rust NOISE_ROUGHNESS_THRESHOLD const not found");
    let sh_val = sh
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("NOISE_ROUGHNESS_THRESHOLD=\"${SPLITTER_WATCH_NOISE_THRESHOLD:-")
                .and_then(|s| s.strip_suffix("}\""))
        })
        .expect("watchdog NOISE_ROUGHNESS_THRESHOLD mirror not found");
    let rn: f32 = rust_val.parse().expect("Rust const value numeric");
    let sn: f32 = sh_val.parse().expect("shell mirror value numeric");
    assert_eq!(
        rn, sn,
        "watchdog mirror ({sh_val}) must equal the Rust const ({rust_val})"
    );
}
