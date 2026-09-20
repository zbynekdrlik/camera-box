//! issue 1349 -- the `[4i/8align]` re-init loop (`scripts/lib/qr-align-reinit.sh`).
//!
//! ## Why this exists (design 5742993777)
//!
//! Each cambox's capture lag `k` (source frames) is DRAWN when the V4L2 device is opened at
//! `[2/8]`/`[2b/8]` and then HOLDS. With 7 boxes of mixed grabbers that is a per-open lottery: the
//! floor-aware plan's owner-mandated 94 ms ceiling can absorb at most ~1 frame, so the run passes
//! only when all 7 draws land within one frame. Rather than ABORT on a bad draw, a bounded re-init
//! loop runs immediately BEFORE `[4i/8align]`: it measures the per-source frame lag (measure-only),
//! restarts the BURN instance of any box more than `REINIT_OK_FRAMES` behind the fastest (re-drawing
//! its `k`), settles, and re-measures -- at most `REINIT_MAX_ROUNDS` rounds, then falls through to
//! the existing floor-aware plan whose HARD-FAIL stays the arbiter. The loop NEVER aborts the run.
//!
//! Same PURE-BASH + static-anchor model as tests/rig_mode.rs: source the lib (no top-level side
//! effects), exercise the pure pickers over fixtures, and drive the orchestrator with injected
//! fake measure/restart seams (never the rig). A separate anchor test reads recording-e2e.sh's text
//! and pins the ONE additive call site immediately before the `[4i/8align]` banner.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/qr-align-reinit.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the lib and run `body` with the given extra env, returning (exit_code, stdout, stderr).
fn run_sourced(body: &str, envs: &[(&str, &str)]) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$LIB\"\n{body}");
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&harness).env("LIB", lib());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

// ---- pure picker ---------------------------------------------------------------------------

#[test]
fn pick_laggards_empty_when_spread_zero() {
    let json = r#"{"NDI cam1": 0, "NDI cam3": 0, "spread_frames": 0, "rounds_used": 4}"#;
    let (code, out, err) = run_sourced(&format!("qr_align_reinit_pick_laggards '{json}' 1"), &[]);
    assert_eq!(code, 0, "stderr={err}");
    assert_eq!(out.trim(), "", "no box is behind -> no laggards");
}

#[test]
fn pick_laggards_empty_when_within_ok_frames() {
    // cam3 one frame behind, ok_frames=1 -> NOT a laggard (more-than-ok is the bar).
    let json = r#"{"NDI cam1": 0, "NDI cam3": -1, "spread_frames": 1, "rounds_used": 4}"#;
    let (code, out, _err) = run_sourced(&format!("qr_align_reinit_pick_laggards '{json}' 1"), &[]);
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "");
}

#[test]
fn pick_laggards_single_box_behind() {
    let json = r#"{"NDI cam4": 0, "NDI cam7": -3, "spread_frames": 3, "rounds_used": 4}"#;
    let (code, out, err) = run_sourced(&format!("qr_align_reinit_pick_laggards '{json}' 1"), &[]);
    assert_eq!(code, 0, "stderr={err}");
    assert_eq!(out.trim(), "NDI cam7");
}

#[test]
fn pick_laggards_two_boxes_behind() {
    let json =
        r#"{"NDI cam4": 0, "NDI cam3": -3, "NDI cam7": -2, "spread_frames": 3, "rounds_used": 4}"#;
    let (code, out, err) = run_sourced(&format!("qr_align_reinit_pick_laggards '{json}' 1"), &[]);
    assert_eq!(code, 0, "stderr={err}");
    let got: Vec<&str> = out.split_whitespace().collect();
    assert!(got.contains(&"NDI"), "output was {out:?}");
    // both cam3 and cam7 are > 1 behind the fastest cam4
    assert!(out.contains("NDI cam3"), "output was {out:?}");
    assert!(out.contains("NDI cam7"), "output was {out:?}");
}

#[test]
fn pick_laggards_skips_garbage_field_never_crashes() {
    // A malformed / non-integer source value must be SKIPPED, not crash. cam3 is still a valid
    // laggard alongside the garbage cam5 entry.
    let json = r#"{"NDI cam4": 0, "NDI cam3": -3, "NDI cam5": "oops", "spread_frames": 3}"#;
    let (code, out, err) = run_sourced(&format!("qr_align_reinit_pick_laggards '{json}' 1"), &[]);
    assert_eq!(code, 0, "stderr={err}");
    assert!(out.contains("NDI cam3"), "output was {out:?}");
    assert!(
        !out.contains("cam5"),
        "garbage cam5 must be skipped: {out:?}"
    );
}

#[test]
fn pick_laggards_empty_json_is_empty_not_crash() {
    let (code, out, err) = run_sourced("qr_align_reinit_pick_laggards '{}' 1", &[]);
    assert_eq!(code, 0, "stderr={err}");
    assert_eq!(out.trim(), "");
}

#[test]
fn cam_of_source_strips_the_ndi_prefix() {
    let (code, out, err) = run_sourced("qr_align_reinit_cam_of_source 'NDI cam7'", &[]);
    assert_eq!(code, 0, "stderr={err}");
    assert_eq!(out.trim(), "cam7");
}

// ---- orchestrator loop (fake measure + restart seams, never the rig) -----------------------

/// Build a harness that defines a counter-driven fake measure (`fake_measure`) that returns a
/// laggard for the first `laggard_rounds` calls then a converged table, and a `fake_restart` that
/// appends each re-init'd cam to $RLOG. Runs the loop and returns (code, stdout, stderr).
fn run_loop(laggard_rounds: u32, restart_log: &str, counter: &str) -> (i32, String, String) {
    let body = format!(
        r#"
fake_measure() {{
  local n; n=$(cat "$CNT" 2>/dev/null || echo 0); n=$((n+1)); echo "$n" > "$CNT"
  if [ "$n" -le {laggard_rounds} ]; then
    echo '{{"NDI cam1": 0, "NDI cam3": -3, "spread_frames": 3, "rounds_used": 4}}'
  else
    echo '{{"NDI cam1": 0, "NDI cam3": 0, "spread_frames": 0, "rounds_used": 4}}'
  fi
}}
fake_restart() {{ echo "$1" >> "$RLOG"; }}
qr_align_reinit_loop strih "NDI cam1,NDI cam3"
"#,
    );
    run_sourced(
        &body,
        &[
            ("QR_ALIGN_REINIT_MEASURE_CMD", "fake_measure"),
            ("QR_ALIGN_REINIT_RESTART_CMD", "fake_restart"),
            ("QR_ALIGN_SETTLE_S", "0"),
            ("REINIT_OK_FRAMES", "1"),
            ("REINIT_MAX_ROUNDS", "3"),
            ("RLOG", restart_log),
            ("CNT", counter),
        ],
    )
}

#[test]
fn loop_converges_after_reiniting_the_laggard() {
    let dir = std::env::temp_dir().join(format!("qr_reinit_conv_{}", std::process::id()));
    let _ = fs::create_dir_all(&dir);
    let rlog = dir.join("restarts.txt");
    let cnt = dir.join("cnt.txt");
    let _ = fs::remove_file(&rlog);
    let _ = fs::remove_file(&cnt);

    // round 1 laggard, round 2 converged.
    let (code, out, err) = run_loop(1, rlog.to_str().unwrap(), cnt.to_str().unwrap());
    assert_eq!(code, 0, "the loop MUST exit 0. stdout={out}\nstderr={err}");
    assert!(
        out.contains("[qr-align-reinit] round 1: spread=3 re-init=cam3"),
        "round-1 re-init line missing: {out:?}"
    );
    assert!(
        out.contains("[qr-align-reinit] converged round 2"),
        "convergence line missing: {out:?}"
    );
    let restarts = fs::read_to_string(&rlog).unwrap_or_default();
    let calls: Vec<&str> = restarts.split_whitespace().collect();
    assert_eq!(
        calls,
        vec!["cam3"],
        "restart must be called EXACTLY for cam3 once: {restarts:?}"
    );
}

#[test]
fn loop_gives_up_after_max_rounds_and_still_returns_zero() {
    let dir = std::env::temp_dir().join(format!("qr_reinit_giveup_{}", std::process::id()));
    let _ = fs::create_dir_all(&dir);
    let rlog = dir.join("restarts.txt");
    let cnt = dir.join("cnt.txt");
    let _ = fs::remove_file(&rlog);
    let _ = fs::remove_file(&cnt);

    // never converges (laggard on every measure).
    let (code, out, err) = run_loop(99, rlog.to_str().unwrap(), cnt.to_str().unwrap());
    assert_eq!(
        code, 0,
        "gave-up path MUST still exit 0 under the caller's set -e. stderr={err}"
    );
    assert!(
        out.contains("[qr-align-reinit] gave up after 3 rounds"),
        "gave-up line missing: {out:?}"
    );
    let restarts = fs::read_to_string(&rlog).unwrap_or_default();
    let n = restarts.split_whitespace().count();
    assert_eq!(n, 3, "one restart per round for 3 rounds: {restarts:?}");
}

// ---- static anchor: ONE additive call site immediately before the [4i/8align] banner --------

#[test]
fn recording_e2e_calls_the_reinit_loop_once_immediately_before_4i_align() {
    let script = manifest_dir().join("scripts/recording-e2e.sh");
    let text = fs::read_to_string(&script).expect("read recording-e2e.sh");

    let n_calls = text.matches("qr_align_reinit_loop").count();
    assert_eq!(
        n_calls, 1,
        "expected EXACTLY one qr_align_reinit_loop call site, found {n_calls}"
    );

    let banner = "[4i/8align] #1003 floor-3 camera alignment via simultaneous painter-QR spread";
    let lines: Vec<&str> = text.lines().collect();
    let call_idx = lines
        .iter()
        .position(|l| l.contains("qr_align_reinit_loop"))
        .expect("the re-init call line");
    let banner_idx = lines
        .iter()
        .position(|l| l.contains(banner))
        .expect("the [4i/8align] banner line");
    assert!(
        banner_idx == call_idx + 1,
        "the re-init call (line {call_idx}) must be IMMEDIATELY before the [4i/8align] banner \
         (line {banner_idx})"
    );
}

// ---- issue 1349 lane: modal-frame picker + fastest lever + next-set + measure retry ----------
//
// Run 4 (E2E 35461955101) showed the slowest-only picker chasing the current laggard forever:
// round after round it re-inited ONE box and the spread stayed 2 (with 7 boxes and k in {0,1,2},
// spread 2 is the MODAL state and one-box re-draws never reach 0). The lane replaces the picker
// with a MODAL-frame round (re-draw every box off the most-frequent k in ONE round) and a second
// lottery lever (re-init the FASTEST box when a repeated set does not improve the spread). Runs 3+5
// also showed the measure-only call failing with its stderr SWALLOWED, so the loop now retries once
// and logs the stderr tail.

#[test]
fn pick_off_modal_returns_only_laggards_when_modal_at_fast_end() {
    // modal is 0 (the fast end, 5 boxes); cam6 is 2 behind -> only cam6 is off-modal.
    let json = r#"{"NDI cam1": 0, "NDI cam2": 0, "NDI cam3": 0, "NDI cam4": 0, "NDI cam5": 0, "NDI cam6": -2, "spread_frames": 2}"#;
    let (code, out, err) = run_sourced(&format!("qr_align_reinit_pick_off_modal '{json}' 1"), &[]);
    assert_eq!(code, 0, "stderr={err}");
    let got: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(got, vec!["NDI cam6"], "only the off-modal laggard: {out:?}");
}

#[test]
fn pick_off_modal_returns_leaders_when_ahead_of_the_modal() {
    // 5 boxes at the modal -3, two LEADERS at -1 (2 frames AHEAD of the modal). The modal picker
    // re-inits the MINORITY off-modal set = the two leaders, NOT the 5-box majority the old
    // slowest-only picker (behind-the-fastest) would have chased.
    let json = r#"{"NDI cam1": -3, "NDI cam2": -3, "NDI cam3": -3, "NDI cam4": -3, "NDI cam5": -3, "NDI cam6": -1, "NDI cam7": -1, "spread_frames": 2}"#;
    let (code, out, err) = run_sourced(&format!("qr_align_reinit_pick_off_modal '{json}' 1"), &[]);
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        out.contains("NDI cam6"),
        "leader cam6 must be off-modal: {out:?}"
    );
    assert!(
        out.contains("NDI cam7"),
        "leader cam7 must be off-modal: {out:?}"
    );
    assert!(
        !out.contains("NDI cam1"),
        "modal-majority box must NOT be re-inited: {out:?}"
    );
    assert!(
        !out.contains("NDI cam3"),
        "modal-majority box must NOT be re-inited: {out:?}"
    );
}

#[test]
fn pick_off_modal_tie_break_prefers_the_higher_value() {
    // tie between modal 0 (x2) and -2 (x2) -> prefer the HIGHER (closest to fastest) = 0, so the
    // -2 boxes are the off-modal set.
    let json =
        r#"{"NDI cam1": 0, "NDI cam2": 0, "NDI cam3": -2, "NDI cam4": -2, "spread_frames": 2}"#;
    let (code, out, err) = run_sourced(&format!("qr_align_reinit_pick_off_modal '{json}' 1"), &[]);
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        out.contains("NDI cam3") && out.contains("NDI cam4"),
        "off-modal = the -2 boxes: {out:?}"
    );
    assert!(
        !out.contains("NDI cam1") && !out.contains("NDI cam2"),
        "tie must resolve to the higher modal 0: {out:?}"
    );
}

#[test]
fn pick_off_modal_ok_frames_zero_vs_one() {
    let json = r#"{"NDI cam1": 0, "NDI cam2": 0, "NDI cam3": -1, "spread_frames": 1}"#;
    // ok_frames 1: cam3 exactly one behind the modal -> NOT off-modal (more-than-ok is the bar).
    let (c1, o1, _e1) = run_sourced(&format!("qr_align_reinit_pick_off_modal '{json}' 1"), &[]);
    assert_eq!(c1, 0);
    assert_eq!(o1.trim(), "", "ok_frames=1 tolerates one frame: {o1:?}");
    // ok_frames 0: cam3 is off-modal.
    let (c0, o0, _e0) = run_sourced(&format!("qr_align_reinit_pick_off_modal '{json}' 0"), &[]);
    assert_eq!(c0, 0);
    assert_eq!(o0.trim(), "NDI cam3", "ok_frames=0 flags one frame: {o0:?}");
}

#[test]
fn fastest_source_is_the_max_lag_value() {
    let json = r#"{"NDI cam1": -3, "NDI cam4": 0, "NDI cam7": -2, "spread_frames": 3}"#;
    let (code, out, err) = run_sourced(&format!("qr_align_reinit_fastest '{json}'"), &[]);
    assert_eq!(code, 0, "stderr={err}");
    assert_eq!(out.trim(), "NDI cam4", "cam4 (0) is the fastest: {out:?}");
}

#[test]
fn next_set_switches_to_fastest_when_stuck() {
    // identical off-modal set AND the spread did not improve (2 -> 2) -> pull the fastest lever.
    let (code, out, err) = run_sourced(
        "qr_align_reinit_next_set 'NDI cam3' 'NDI cam3' 2 2 'NDI cam1'",
        &[],
    );
    assert_eq!(code, 0, "stderr={err}");
    assert_eq!(out.trim(), "NDI cam1", "stuck -> fastest: {out:?}");
}

#[test]
fn next_set_keeps_off_modal_when_spread_improved() {
    // same set but the spread improved (3 -> 2) -> keep re-initing the off-modal set.
    let (code, out, err) = run_sourced(
        "qr_align_reinit_next_set 'NDI cam3' 'NDI cam3' 3 2 'NDI cam1'",
        &[],
    );
    assert_eq!(code, 0, "stderr={err}");
    assert_eq!(
        out.trim(),
        "NDI cam3",
        "improved -> keep off-modal: {out:?}"
    );
}

#[test]
fn next_set_keeps_off_modal_when_set_changed() {
    // a different set this round -> re-init it (the modal shifted, not stuck).
    let (code, out, err) = run_sourced(
        "qr_align_reinit_next_set 'NDI cam3' 'NDI cam7' 2 2 'NDI cam1'",
        &[],
    );
    assert_eq!(code, 0, "stderr={err}");
    assert_eq!(out.trim(), "NDI cam7", "changed set -> re-init it: {out:?}");
}

/// Small helper for the loop tests: a fresh temp dir with a restart-log path and a call-counter path.
fn loop_dir(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("qr_reinit_{tag}_{}", std::process::id()));
    let _ = fs::create_dir_all(&dir);
    let rlog = dir.join("restarts.txt");
    let cnt = dir.join("cnt.txt");
    let _ = fs::remove_file(&rlog);
    let _ = fs::remove_file(&cnt);
    (dir, rlog, cnt)
}

#[test]
fn loop_retries_measure_once_on_failure_and_logs_stderr_tail() {
    let (_dir, rlog, cnt) = loop_dir("retry");
    let body = r#"
fake_measure() {
  local n; n=$(cat "$CNT" 2>/dev/null || echo 0); n=$((n+1)); echo "$n" > "$CNT"
  if [ "$n" -eq 1 ]; then
    printf 'boom-a\nboom-b\nboom-c\nboom-tail\n' >&2
    echo '{"error": "kaboom", "spread_frames": null}'
  else
    echo '{"NDI cam1": 0, "NDI cam3": 0, "spread_frames": 0, "rounds_used": 4}'
  fi
}
fake_restart() { echo "$1" >> "$RLOG"; }
qr_align_reinit_loop strih "NDI cam1,NDI cam3"
"#;
    let (code, out, err) = run_sourced(
        body,
        &[
            ("QR_ALIGN_REINIT_MEASURE_CMD", "fake_measure"),
            ("QR_ALIGN_REINIT_RESTART_CMD", "fake_restart"),
            ("QR_ALIGN_SETTLE_S", "0"),
            ("QR_ALIGN_REINIT_MEASURE_RETRY_S", "0"),
            ("REINIT_OK_FRAMES", "1"),
            ("REINIT_MAX_ROUNDS", "3"),
            ("RLOG", rlog.to_str().unwrap()),
            ("CNT", cnt.to_str().unwrap()),
        ],
    );
    assert_eq!(code, 0, "stdout={out}\nstderr={err}");
    // The measure-failed diagnostic goes to STDERR (stdout is the JSON return channel of the
    // measure helper); it is NEVER swallowed to /dev/null (the runs 3+5 gap).
    assert!(
        err.contains("measure failed round 1"),
        "no measure-failed log: {err:?}"
    );
    assert!(err.contains("boom-tail"), "stderr tail not logged: {err:?}");
    assert!(
        !err.contains("boom-a"),
        "only the LAST 3 stderr lines belong in the log: {err:?}"
    );
    assert!(
        out.contains("converged round 1"),
        "the retry should succeed and converge: {out:?}"
    );
    let restarts = fs::read_to_string(&rlog).unwrap_or_default();
    assert_eq!(
        restarts.trim(),
        "",
        "converged after retry -> no box re-inited: {restarts:?}"
    );
}

#[test]
fn loop_measure_unavailable_after_two_failures_returns_zero() {
    let (_dir, rlog, cnt) = loop_dir("unavail");
    let body = r#"
fake_measure() { printf 'still-broken\n' >&2; echo '{"error": "down", "spread_frames": null}'; }
fake_restart() { echo "$1" >> "$RLOG"; }
qr_align_reinit_loop strih "NDI cam1,NDI cam3"
"#;
    let (code, out, err) = run_sourced(
        body,
        &[
            ("QR_ALIGN_REINIT_MEASURE_CMD", "fake_measure"),
            ("QR_ALIGN_REINIT_RESTART_CMD", "fake_restart"),
            ("QR_ALIGN_SETTLE_S", "0"),
            ("QR_ALIGN_REINIT_MEASURE_RETRY_S", "0"),
            ("REINIT_OK_FRAMES", "1"),
            ("REINIT_MAX_ROUNDS", "3"),
            ("RLOG", rlog.to_str().unwrap()),
            ("CNT", cnt.to_str().unwrap()),
        ],
    );
    assert_eq!(code, 0, "the loop MUST exit 0. stdout={out}\nstderr={err}");
    assert!(
        err.contains("measure failed round 1"),
        "the first failure must be logged (stderr): {err:?}"
    );
    assert!(
        out.contains("measure unavailable round 1"),
        "after the retry also fails: {out:?}"
    );
    let restarts = fs::read_to_string(&rlog).unwrap_or_default();
    assert_eq!(
        restarts.trim(),
        "",
        "unmeasurable -> nothing re-inited: {restarts:?}"
    );
}

#[test]
fn loop_reinits_exactly_the_off_modal_set_then_converges() {
    let (_dir, rlog, cnt) = loop_dir("offmodal");
    let body = r#"
fake_measure() {
  local n; n=$(cat "$CNT" 2>/dev/null || echo 0); n=$((n+1)); echo "$n" > "$CNT"
  if [ "$n" -le 1 ]; then
    echo '{"NDI cam1": 0, "NDI cam2": 0, "NDI cam3": 0, "NDI cam4": 0, "NDI cam5": 0, "NDI cam6": -2, "NDI cam7": -2, "spread_frames": 2, "rounds_used": 4}'
  else
    echo '{"NDI cam1": 0, "NDI cam2": 0, "NDI cam3": 0, "NDI cam4": 0, "NDI cam5": 0, "NDI cam6": 0, "NDI cam7": 0, "spread_frames": 0, "rounds_used": 4}'
  fi
}
fake_restart() { echo "$1" >> "$RLOG"; }
qr_align_reinit_loop strih "NDI cam1,NDI cam2,NDI cam3,NDI cam4,NDI cam5,NDI cam6,NDI cam7"
"#;
    let (code, out, err) = run_sourced(
        body,
        &[
            ("QR_ALIGN_REINIT_MEASURE_CMD", "fake_measure"),
            ("QR_ALIGN_REINIT_RESTART_CMD", "fake_restart"),
            ("QR_ALIGN_SETTLE_S", "0"),
            ("REINIT_OK_FRAMES", "1"),
            ("REINIT_MAX_ROUNDS", "4"),
            ("RLOG", rlog.to_str().unwrap()),
            ("CNT", cnt.to_str().unwrap()),
        ],
    );
    assert_eq!(code, 0, "stdout={out}\nstderr={err}");
    assert!(
        out.contains("round 1: spread=2 re-init=cam6,cam7"),
        "round-1 must re-init the off-modal set cam6,cam7: {out:?}"
    );
    assert!(
        out.contains("converged round 2"),
        "convergence line missing: {out:?}"
    );
    let restarts = fs::read_to_string(&rlog).unwrap_or_default();
    let mut got: Vec<&str> = restarts.split_whitespace().collect();
    got.sort_unstable();
    assert_eq!(
        got,
        vec!["cam6", "cam7"],
        "exactly the off-modal set re-inited once each: {restarts:?}"
    );
}

#[test]
fn loop_pulls_the_fastest_lever_when_the_off_modal_set_is_stuck() {
    // Run-4 scenario: the spread stays 2 with the SAME off-modal box every round. After the first
    // stuck repeat the loop re-inits the FASTEST box (second lever) instead of the same laggard.
    let (_dir, rlog, cnt) = loop_dir("stuck");
    let body = r#"
fake_measure() { echo '{"NDI cam1": 0, "NDI cam2": 0, "NDI cam3": 0, "NDI cam4": 0, "NDI cam5": 0, "NDI cam6": -2, "spread_frames": 2, "rounds_used": 4}'; }
fake_restart() { echo "$1" >> "$RLOG"; }
qr_align_reinit_loop strih "NDI cam1,NDI cam2,NDI cam3,NDI cam4,NDI cam5,NDI cam6"
"#;
    let (code, out, err) = run_sourced(
        body,
        &[
            ("QR_ALIGN_REINIT_MEASURE_CMD", "fake_measure"),
            ("QR_ALIGN_REINIT_RESTART_CMD", "fake_restart"),
            ("QR_ALIGN_SETTLE_S", "0"),
            ("REINIT_OK_FRAMES", "1"),
            ("REINIT_MAX_ROUNDS", "3"),
            ("RLOG", rlog.to_str().unwrap()),
            ("CNT", cnt.to_str().unwrap()),
        ],
    );
    assert_eq!(code, 0, "stdout={out}\nstderr={err}");
    assert!(
        out.contains("gave up after 3 rounds"),
        "gave-up line missing: {out:?}"
    );
    let restarts = fs::read_to_string(&rlog).unwrap_or_default();
    let got: Vec<&str> = restarts.split_whitespace().collect();
    // round 1 re-inits the off-modal cam6; rounds 2+3 (same stuck set, no improvement) pull the
    // fastest lever = cam1 (lowest-numbered box at the modal).
    assert_eq!(
        got,
        vec!["cam6", "cam1", "cam1"],
        "fastest lever after stuck: {restarts:?}"
    );
}
