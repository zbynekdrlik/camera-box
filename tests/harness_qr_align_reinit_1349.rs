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
    let got: Vec<&str> = out.trim().split_whitespace().collect();
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
