//! Regression guard for `scripts/lib/leg-health-guard.sh` (#1133) — the per-box capture-LEG-HEALTH
//! preflight signal set, and its wiring into `scripts/recording-e2e.sh`'s `[0/8]` preflight (so a
//! doomed 30-minute E2E run is never even started against a genuinely sick capture leg, instead of
//! the #656 preflight silently saying "ok" while the box stalls/skips/EPROTOs — the #1130/#1110
//! incident).
//!
//! Tier-0 (camera-box #477/#557): ALL local cargo compilation is blocked, so this harness RUNS ON
//! CI only. Its assertions are pure BASH logic (it just shells out to `bash` to source the pure lib
//! and to `grep` fixtures), so the identical behaviour is verified LOCALLY by running the lib
//! functions directly against these same fixtures — `cargo fmt --all --check` (allowed locally)
//! proves this file parses + is formatted. The lib + fixtures are read at RUNTIME.

use std::path::PathBuf;
use std::process::Command;

fn lib_script() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/lib/leg-health-guard.sh");
    assert!(p.exists(), "{} not found", p.display());
    p
}

fn fixture(name: &str) -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/leg_health_1133")
        .join(name);
    assert!(p.exists(), "fixture {} not found", p.display());
    p
}

/// Source the shared lib and run `body`, returning (stdout, success). Never asserts the exit code
/// itself, so callers can test BOTH `leg_health_classify`'s ok(0) and unhealthy(1) paths.
fn run_sourced_status(body: &str) -> (String, bool) {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib_script())
        .output()
        .expect("failed to run bash harness");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
    )
}

/// Convenience: run `body`, assert it exited 0, return trimmed stdout.
fn run_ok(body: &str) -> String {
    let (out, ok) = run_sourced_status(body);
    assert!(
        ok,
        "sourced harness exited non-zero.\nbody={body}\nstdout={out:?}"
    );
    out
}

/// Source the lib and run `body` under the CALLER's EXACT `set -euo pipefail` context (what
/// recording-e2e.sh uses) — NOT the `-uo`-only context `run_sourced_status` uses. This is what
/// exposes a `set -e`-abort phantom-fail (a report-only probe that returns non-zero on an empty
/// read would `set -e`-kill the whole run). Returns (stdout, success).
fn run_under_set_e(body: &str) -> (String, bool) {
    let harness = format!("set -euo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib_script())
        .output()
        .expect("failed to run bash harness");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
    )
}

/// grep -Ec the pattern the lib function `pattern_fn` returns, against fixture `fx`.
fn grep_count(pattern_fn: &str, fx: &str) -> u32 {
    let body = format!(
        "PAT=\"$({pattern_fn})\"; grep -Ec \"$PAT\" \"{}\" || true",
        fixture(fx).display()
    );
    let out = run_ok(&body);
    out.trim()
        .parse::<u32>()
        .unwrap_or_else(|_| panic!("grep_count non-numeric: {out:?}"))
}

// ---------------------------------------------------------------------------------------------
// 1. Each grep pattern matches the EXACT real producer line (src/*.rs / kernel), verbatim shape.
// ---------------------------------------------------------------------------------------------

#[test]
fn dequeue_stall_pattern_matches_real_capture_stall_warn() {
    let pat = run_ok("leg_health_dequeue_stall_grep_pattern");
    assert_eq!(pat.trim(), "#707 V4L2 capture DEQUEUE STALL");
    // the real src/capture_stall.rs::capture_stall_warning line shape:
    let line = "WARN camera_box: #707 V4L2 capture DEQUEUE STALL: 26.4ms (configured frame interval 16.7ms @ 60.0fps, >= 1.5x budget) — the blocking V4L2 dequeue (VIDIOC_DQBUF) itself took this long (see #707)";
    let (_o, ok) = run_sourced_status(&format!(
        "printf '%s\\n' '{line}' | grep -E \"$(leg_health_dequeue_stall_grep_pattern)\" >/dev/null"
    ));
    assert!(ok, "dequeue-stall pattern must match the real WARN line");
}

#[test]
fn emit_skip_pattern_matches_real_emit_skip_log_warn() {
    let pat = run_ok("leg_health_emit_skip_grep_pattern");
    assert_eq!(pat.trim(), "#707 genlock emit-gate SKIPPED boundaries");
    let line = "WARN camera_box: #707 genlock emit-gate SKIPPED boundaries in 1 gate call(s) totalling 9 boundary interval(s) over the last ~5s (rate-limited #752 aggregate ...)";
    let (_o, ok) = run_sourced_status(&format!(
        "printf '%s\\n' '{line}' | grep -E \"$(leg_health_emit_skip_grep_pattern)\" >/dev/null"
    ));
    assert!(
        ok,
        "emit-skip pattern must match the real aggregate WARN line"
    );
}

#[test]
fn eproto_pattern_matches_real_uvcvideo_kernel_line() {
    let pat = run_ok("leg_health_eproto_grep_pattern");
    assert_eq!(pat.trim(), "uvcvideo.*Non-zero status");
    let line =
        "CAM1 kernel: uvcvideo 2-1:1.1: Non-zero status (-71) in video buffer completion handler.";
    let (_o, ok) = run_sourced_status(&format!(
        "printf '%s\\n' '{line}' | grep -E \"$(leg_health_eproto_grep_pattern)\" >/dev/null"
    ));
    assert!(
        ok,
        "EPROTO pattern must match the real uvcvideo kernel line"
    );
}

// ---------------------------------------------------------------------------------------------
// 2. grep-count of each pattern against the real-shape fixtures gives the expected counts.
// ---------------------------------------------------------------------------------------------

#[test]
fn fixture_counts_match_expected() {
    assert_eq!(
        grep_count("leg_health_dequeue_stall_grep_pattern", "sick_journal.txt"),
        12
    );
    assert_eq!(
        grep_count("leg_health_emit_skip_grep_pattern", "sick_journal.txt"),
        10
    );
    assert_eq!(
        grep_count("leg_health_eproto_grep_pattern", "sick_kmsg.txt"),
        9
    );
    assert_eq!(
        grep_count(
            "leg_health_dequeue_stall_grep_pattern",
            "residual_journal.txt"
        ),
        2
    );
    assert_eq!(
        grep_count("leg_health_emit_skip_grep_pattern", "residual_journal.txt"),
        1
    );
    assert_eq!(
        grep_count(
            "leg_health_dequeue_stall_grep_pattern",
            "healthy_journal.txt"
        ),
        0
    );
    assert_eq!(
        grep_count("leg_health_eproto_grep_pattern", "healthy_kmsg.txt"),
        0
    );
}

// ---------------------------------------------------------------------------------------------
// 3. leg_health_classify — THE decision. Sick -> abort naming box+signals; residual/healthy -> ok.
//    This is the #1133 core: the exact case the #656 preflight was blind to must now FAIL.
// ---------------------------------------------------------------------------------------------

#[test]
fn classify_sick_leg_aborts_and_names_box_and_every_count_signal() {
    // #1133: leg_health_classify is now 3-arg (box skip eproto) — the DEQUEUE STALL count was
    // dropped (it gated on a quantity anti-correlated with real health, issue 1198). sick
    // count-based signals (10 skip, 9 eproto) — both over threshold (8, 6).
    let (out, ok) = run_sourced_status("leg_health_classify cam1 10 9");
    assert!(
        !ok,
        "a leg over the emit-skip / EPROTO thresholds MUST fail (return non-zero), not report ok"
    );
    assert!(out.contains("cam1"), "message must name the box: {out:?}");
    assert!(
        out.contains("SKIPPED"),
        "must name the emit-skip signal: {out:?}"
    );
    assert!(
        out.to_lowercase().contains("eproto") || out.contains("Non-zero status"),
        "must name the EPROTO signal: {out:?}"
    );
    assert!(
        !out.contains("DEQUEUE"),
        "#1133: the DEQUEUE STALL count is NO LONGER a classify signal: {out:?}"
    );
    assert!(
        out.contains("10") && out.contains("9"),
        "must carry the real counts: {out:?}"
    );
    assert!(
        out.contains("CAMERA_ACTIVE_SET"),
        "must give the escalation path: {out:?}"
    );
    assert!(out.contains("#1133"), "must reference the ticket: {out:?}");
    // ONE line (greppable), not a multi-line block.
    assert_eq!(
        out.trim().lines().count(),
        1,
        "abort message must be a single line: {out:?}"
    );
}

#[test]
fn classify_residual_and_healthy_legs_pass() {
    // post-.481 residual (1 skip, 0 eproto) — genuinely below thresholds -> ok. (A high STALL count
    // no longer even enters classify — it is not an argument here, #1133.)
    let (out, ok) = run_sourced_status("leg_health_classify cam1 1 0");
    assert!(
        ok,
        "the post-.481 residual leg must PASS (below thresholds), not false-fail: {out:?}"
    );
    assert!(
        out.trim().is_empty(),
        "a healthy leg prints nothing: {out:?}"
    );
    // stone-cold healthy.
    let (_o2, ok2) = run_sourced_status("leg_health_classify cam3 0 0");
    assert!(ok2, "a 0/0 leg must pass");
}

#[test]
fn classify_each_signal_fails_alone_at_its_threshold_and_passes_just_below() {
    // exactly-at-threshold fails; one-below passes — proves the boundary + per-signal isolation.
    // #1133: classify is now 3-arg (box skip eproto); the STALL count is gone.
    // SKIP threshold 8:
    let (o, bad) = run_sourced_status("leg_health_classify cam1 8 0");
    assert!(!bad, "skip==8 must fail");
    assert!(
        o.contains("SKIPPED") && !o.contains("DEQUEUE"),
        "skip-only fail must name ONLY the skip signal: {o:?}"
    );
    let (o, ok) = run_sourced_status("leg_health_classify cam1 7 0");
    assert!(ok, "skip==7 must pass");
    assert!(o.trim().is_empty());
    // EPROTO threshold 6 (recalibrated 2026-08-20: the chronic ShadowCast-model baseline is
    // 0.66-1.05/hr arriving in 2-3-event clumps — 3-in-an-hour is routine on a functionally
    // healthy leg; the real sick burst was ~6+/hr WITH stalls. Original 3 chronically
    // false-aborted two live MEQ runs back-to-back.):
    let (o, bad) = run_sourced_status("leg_health_classify cam1 0 6");
    assert!(!bad, "eproto==6 must fail");
    assert!(
        o.contains("Non-zero status") && !o.contains("SKIPPED"),
        "eproto-only fail must name ONLY the EPROTO signal: {o:?}"
    );
    assert!(
        run_sourced_status("leg_health_classify cam1 0 5").1,
        "eproto==5 must pass (chronic ShadowCast clump baseline tolerated)"
    );
}

#[test]
fn classify_treats_empty_or_garbled_counts_as_zero_never_a_phantom_fail() {
    // a failed ssh read (empty / non-numeric capture) must NOT manufacture a leg-health defect.
    assert!(
        run_sourced_status("leg_health_classify cam1 '' ''").1,
        "empty counts -> ok"
    );
    assert!(
        run_sourced_status("leg_health_classify cam1 y z").1,
        "garbled counts -> ok"
    );
}

// ---------------------------------------------------------------------------------------------
// 3b. #1133 HARD frame-loss gate — the capture-health signal that REPLACES the DEQUEUE STALL gate.
//     This is the ticket's core: gate on captured frame LOSS (sent-vs-captured from the
//     `Streaming:` lines), calibrated so the four healthy boxes PASS and both historical defective
//     reads FAIL, with a sustain guard so a single bad window never fails a run.
// ---------------------------------------------------------------------------------------------

/// Build STREAMING_TEXT of `n` real-shape `Streaming:` lines, each `sent`/`captured` as given.
fn streaming_lines(windows: &[(u32, u32)]) -> String {
    let mut s = String::new();
    for (sent, cap) in windows {
        let drop = sent.saturating_sub(*cap);
        s.push_str(&format!(
            "Aug 20 12:05:00 CAM1 camera-box[74588]: INFO camera_box: Streaming: 60.0 fps emitted / 59.0 fps captured ({sent} sent, {cap} captured, {drop} capture-dropped, 0 corrupted)\n"
        ));
    }
    s
}

#[test]
fn frame_loss_stats_parses_sent_captured_and_counts_bad_windows() {
    // 3 windows: 300/299 (0.33%, not bad), 300/292 (2.67%, bad), 300/315 (over-rate -> 0 loss).
    let text = streaming_lines(&[(300, 299), (300, 292), (300, 315)]);
    let out = run_ok(&format!(
        "leg_health_frame_loss_stats \"$(cat <<'EOF'\n{text}EOF\n)\""
    ));
    // stats = "N SENT LOST BAD": N=3, SENT=900, LOST=1+8+0=9, BAD=1 (only the 2.67% window).
    assert_eq!(out.trim(), "3 900 9 1", "frame-loss stats parse: {out:?}");
}

#[test]
fn frame_loss_calibration_healthy_boxes_pass_defective_reads_fail() {
    // The ticket's calibration. is_unhealthy N SENT LOST BAD returns 0 (fail) iff unhealthy.
    let unhealthy =
        |args: &str| !run_sourced_status(&format!("leg_health_frame_loss_is_unhealthy {args}")).1;
    // Four healthy boxes (aggregate 0.000-0.277%, no sustained bad windows) -> PASS.
    assert!(!unhealthy("12 3605 10 0"), "cam1 healthy 0.277% must pass");
    assert!(!unhealthy("12 3605 8 0"), "cam2 healthy 0.222% must pass");
    assert!(!unhealthy("12 3607 1 0"), "cam3 healthy 0.028% must pass");
    assert!(!unhealthy("12 3607 0 0"), "cam4 healthy 0.000% must pass");
    // Both HISTORICAL DEFECTIVE reads (2.53% and 7.60%, sustained across every window) -> FAIL.
    // 58.48/60 over 3600 sent ≈ 91 lost = 2.53%; 55.44/60 ≈ 274 lost = 7.60%.
    assert!(unhealthy("12 3600 91 12"), "defective 2.53% must FAIL");
    assert!(unhealthy("12 3600 274 12"), "defective 7.60% must FAIL");
}

#[test]
fn frame_loss_single_bad_window_passes_sustain_guard() {
    // 12 windows, one catastrophic (50% loss) window, the rest clean. Aggregate is over threshold
    // (150/3600 = 4.17%) BUT only 1 window is individually elevated (< min_bad_windows=3) -> PASS.
    // This is the mandated "a single bad window must not fail a run".
    assert!(
        run_sourced_status("leg_health_frame_loss_is_unhealthy 12 3600 150 1").1,
        "single-bad-window (bad=1 < 3) must PASS despite a high aggregate"
    );
}

#[test]
fn frame_loss_insufficient_windows_and_over_rate_pass() {
    // fewer than min_windows (5) -> insufficient data -> PASS (never judge a just-restarted box).
    assert!(
        run_sourced_status("leg_health_frame_loss_is_unhealthy 4 1200 200 4").1,
        "n<min_windows must PASS (insufficient data)"
    );
    // over-rate (captured>sent) contributes 0 loss -> PASS (issue #909, benign).
    let text = streaming_lines(&[
        (300, 315),
        (300, 316),
        (300, 314),
        (300, 315),
        (300, 315),
        (300, 316),
    ]);
    let (o, ok) = run_sourced_status(&format!(
        "leg_health_frame_loss_classify cam1 \"$(cat <<'EOF'\n{text}EOF\n)\""
    ));
    assert!(ok, "over-rate must PASS: {o:?}");
    assert!(o.trim().is_empty());
}

#[test]
fn frame_loss_classify_message_names_loss_not_a_hardware_fault() {
    // A defective read (every window 300/292 = 2.67%) must abort with a message that NAMES FRAME
    // LOSS and does NOT assert a cable/port/grabber fault (issue 1198 disproved that attribution).
    let text = streaming_lines(&[(300, 292); 12]);
    let (out, ok) = run_sourced_status(&format!(
        "leg_health_frame_loss_classify cam1 \"$(cat <<'EOF'\n{text}EOF\n)\""
    ));
    assert!(!ok, "defective frame loss must abort: {out:?}");
    assert!(out.contains("cam1"), "must name the box: {out:?}");
    assert!(
        out.to_lowercase().contains("stratené snímky") || out.to_lowercase().contains("snímk"),
        "must name FRAME LOSS: {out:?}"
    );
    assert!(
        out.contains("CAMERA_ACTIVE_SET"),
        "must give the escalation path: {out:?}"
    );
    assert!(out.contains("#1133"), "must cite the ticket: {out:?}");
    // Must NOT assert the hardware misattribution the old DEQUEUE STALL message did — the phrase
    // "NIE nutne kábel/port/grabber" explicitly disavows it; the message must never READ as an
    // unqualified "replace the cable/port/grabber" instruction.
    assert!(
        out.contains("NIE nutne") || !out.to_lowercase().contains("oprav hardvér"),
        "must NOT assert a cable/port/grabber fault for frame loss: {out:?}"
    );
    assert!(
        out.contains("1198"),
        "must cite the disproof evidence (issue 1198): {out:?}"
    );
    assert_eq!(
        out.trim().lines().count(),
        1,
        "must be a single line: {out:?}"
    );
}

#[test]
fn frame_loss_empty_read_is_healthy_never_a_phantom_fail() {
    // an empty / no-parse Streaming read must classify HEALTHY (a failed ssh read never fails).
    assert!(
        run_sourced_status("leg_health_frame_loss_classify cam1 ''").1,
        "empty streaming read -> ok"
    );
    assert!(
        run_sourced_status("leg_health_frame_loss_classify cam1 'not a streaming line'").1,
        "garbage streaming read -> ok"
    );
}

#[test]
fn frame_loss_classify_aborts_the_run_under_set_e() {
    // The exact production shape: a defective streaming read fails the run under `set -euo pipefail`
    // (the caller invokes it as an `if !` condition, so a non-zero return aborts intentionally).
    let text = streaming_lines(&[(300, 292); 12]);
    let (out, ok) = run_under_set_e(&format!(
        "T=\"$(cat <<'EOF'\n{text}EOF\n)\"; if ! M=$(leg_health_frame_loss_classify cam1 \"$T\"); then echo \"ABORT_OK\"; exit 1; fi; echo WRONGLY_PASSED"
    ));
    assert!(!ok, "a defective frame-loss read must fail the run");
    assert!(
        out.contains("ABORT_OK") && !out.contains("WRONGLY_PASSED"),
        "must abort on the defective read: {out:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// 3c. DEQUEUE STALL — now REPORT-ONLY. It must never abort, and it must cite the anti-correlation.
// ---------------------------------------------------------------------------------------------

#[test]
fn dequeue_stall_report_is_report_only_over_threshold_and_silent_below() {
    // at/over the report threshold (8) -> a report-only WARN line, exit 0 (never aborts).
    let (out, ok) = run_sourced_status("leg_health_dequeue_stall_report cam1 12");
    assert!(
        ok,
        "the stall report is report-only — it must never change the exit code: {out:?}"
    );
    assert!(
        out.contains("WARNING #1133"),
        "over-threshold stall must warn: {out:?}"
    );
    assert!(
        out.contains("REPORT-ONLY") || out.contains("NEabortuje"),
        "the warn must state it does not abort: {out:?}"
    );
    assert!(
        out.contains("1198"),
        "must cite the anti-correlation evidence (issue 1198): {out:?}"
    );
    // below the threshold -> silent.
    let (out2, ok2) = run_sourced_status("leg_health_dequeue_stall_report cam1 7");
    assert!(
        ok2 && out2.trim().is_empty(),
        "stall<8 must be silent: {out2:?}"
    );
}

#[test]
fn dequeue_stall_report_never_aborts_the_run_under_set_e() {
    // called as a BARE statement under `set -euo pipefail` (like leg_health_cap1s_band_warn), it
    // must always return 0 — a non-zero return would set-e-kill the whole E2E run.
    let (out, ok) = run_under_set_e(
        "leg_health_dequeue_stall_report cam1 12 >/dev/null; echo REACHED_AFTER_STALL_REPORT",
    );
    assert!(ok, "bare stall-report under set -e must not abort: {out:?}");
    assert!(
        out.contains("REACHED_AFTER_STALL_REPORT"),
        "the run must continue past the stall report: {out:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// 3d. Streaming read command + extract + pattern (the read path feeding the frame-loss gate).
// ---------------------------------------------------------------------------------------------

#[test]
fn streaming_grep_pattern_matches_genlock_form_only() {
    let pat = run_ok("leg_health_streaming_grep_pattern");
    assert_eq!(pat.trim(), "Streaming:.*[0-9]+ sent, [0-9]+ captured");
    // matches the genlock-box `(<n> sent, <m> captured, ...)` form:
    let genlock = "INFO camera_box: Streaming: 60.0 fps emitted / 59.2 fps captured (3607 sent, 3560 captured, 47 capture-dropped, 0 corrupted)";
    assert!(
        run_sourced_status(&format!(
            "printf '%s\\n' '{genlock}' | grep -E \"$(leg_health_streaming_grep_pattern)\" >/dev/null"
        ))
        .1,
        "must match the genlock Streaming form"
    );
    // does NOT match the non-genlock `Streaming: <c> fps (<f> frames, ...)` form (no ` sent, `):
    let nongenlock =
        "INFO camera_box: Streaming: 59.9 fps (3595 frames, 2 capture-dropped, 0 corrupted)";
    assert!(
        !run_sourced_status(&format!(
            "printf '%s\\n' '{nongenlock}' | grep -E \"$(leg_health_streaming_grep_pattern)\" >/dev/null"
        ))
        .1,
        "must NOT match the non-genlock Streaming form"
    );
}

#[test]
fn streaming_read_cmd_is_invocation_scoped_and_windowed() {
    let out = run_ok("leg_health_streaming_read_cmd INV9 1000 2000");
    assert!(
        out.contains("_SYSTEMD_INVOCATION_ID=INV9"),
        "must scope to the instance (#693): {out}"
    );
    assert!(
        out.contains("--since=@1000") && out.contains("--until=@2000"),
        "must window: {out}"
    );
    assert!(
        out.contains("Streaming:") && out.contains("tail -80"),
        "must grep the Streaming lines and cap the read: {out}"
    );
}

#[test]
fn extract_streaming_pulls_only_the_streaming_block() {
    let out = run_ok(
        "OUT=$(printf '%b' 'LEGHEALTH_STALL=1\\nLEGHEALTH_CAP1S_BEGIN\\ncap line\\nLEGHEALTH_CAP1S_END\\nLEGHEALTH_STREAMING_BEGIN\\nStreaming: (300 sent, 293 captured, 7 capture-dropped, 0 corrupted)\\nLEGHEALTH_STREAMING_END'); \
         leg_health_extract_streaming \"$OUT\"",
    );
    assert!(
        out.contains("300 sent, 293 captured")
            && !out.contains("cap line")
            && !out.contains("LEGHEALTH_"),
        "must extract only the streaming lines: {out:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// 4. cap-1s report-only band WARN — sustained over-rate warns; chronic wobble / in-band is silent;
//    and it NEVER changes the exit code (report-only, issue #909).
// ---------------------------------------------------------------------------------------------

#[test]
fn cap1s_sustained_over_rate_warns_report_only() {
    let text = "#707 emit-1s: [60, 60, 60, 60, 60] cap-1s: [62, 63, 62, 63, 62] (1-second buckets, oldest first)";
    let (out, ok) = run_sourced_status(&format!("leg_health_cap1s_band_warn cam1 '{text}'"));
    assert!(
        ok,
        "cap-1s warn is report-only — it must NEVER change the exit code"
    );
    assert!(
        out.contains("WARNING #1133"),
        "sustained 62-63 must warn: {out:?}"
    );
    assert!(
        out.contains("REPORT-ONLY") || out.contains("NEabortuje"),
        "the warn must state it does not abort: {out:?}"
    );
    assert!(
        out.contains("#909"),
        "must cite the over-rate-is-benign rationale (#909): {out:?}"
    );
}

#[test]
fn cap1s_chronic_wobble_and_in_band_are_silent() {
    // the live chronic ShadowCast wobble (1/5 out of band) must NOT warn.
    let wobble = "#707 emit-1s: [60,60,60,60,60] cap-1s: [60, 62, 61, 60, 61] (1-second buckets, oldest first)";
    let (out, ok) = run_sourced_status(&format!("leg_health_cap1s_band_warn cam1 '{wobble}'"));
    assert!(ok);
    assert!(
        out.trim().is_empty(),
        "chronic wobble (1/5 out) must NOT warn: {out:?}"
    );
    // fully in-band -> silent.
    let inband = "#707 emit-1s: [60,60,60,60,60] cap-1s: [60, 60, 61, 60, 60] (1-second buckets, oldest first)";
    let (out2, _ok2) = run_sourced_status(&format!("leg_health_cap1s_band_warn cam3 '{inband}'"));
    assert!(
        out2.trim().is_empty(),
        "in-band cap-1s must NOT warn: {out2:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// 5. Remote read-command builders — pure strings, InvocationID-scoped + windowed, with fallback.
// ---------------------------------------------------------------------------------------------

#[test]
fn journal_count_cmd_is_invocation_scoped_and_windowed() {
    let out = run_ok("leg_health_journal_count_cmd ABC123 1000 2000 'FOO'");
    assert!(
        out.contains("_SYSTEMD_INVOCATION_ID=ABC123"),
        "must scope to the instance (#693): {out}"
    );
    assert!(
        out.contains("--since=@1000") && out.contains("--until=@2000"),
        "must window: {out}"
    );
    assert!(
        out.contains("grep -Ec 'FOO'"),
        "must count the pattern: {out}"
    );
}

#[test]
fn journal_count_cmd_falls_back_to_unit_when_invocation_id_empty() {
    let out = run_ok("leg_health_journal_count_cmd '' 1000 2000 'FOO'");
    assert!(
        out.contains("journalctl -u camera-box"),
        "empty id -> unscoped unit read: {out}"
    );
    assert!(
        !out.contains("_SYSTEMD_INVOCATION_ID"),
        "must not emit an empty invocation id: {out}"
    );
    assert!(out.contains("--since=@1000") && out.contains("grep -Ec 'FOO'"));
}

#[test]
fn kmsg_count_cmd_reads_kernel_window() {
    let out = run_ok("leg_health_kmsg_count_cmd 1000 2000 'uvcvideo.*Non-zero status'");
    assert!(
        out.contains("journalctl -k"),
        "EPROTO reads the kernel log: {out}"
    );
    assert!(
        out.contains("--since=@1000") && out.contains("--until=@2000"),
        "kernel window: {out}"
    );
    assert!(out.contains("grep -Ec 'uvcvideo.*Non-zero status'"));
    assert!(
        !out.contains("_SYSTEMD_INVOCATION_ID"),
        "kernel read is not instance-scoped: {out}"
    );
}

#[test]
fn end_to_end_sick_journal_over_threshold_healthy_under() {
    // Prove the whole path: grep the fixture with the lib pattern -> feed classify -> verdict.
    let sick_stall = grep_count("leg_health_dequeue_stall_grep_pattern", "sick_journal.txt");
    let sick_skip = grep_count("leg_health_emit_skip_grep_pattern", "sick_journal.txt");
    let sick_eproto = grep_count("leg_health_eproto_grep_pattern", "sick_kmsg.txt");
    let (_o, ok) = run_sourced_status(&format!(
        "leg_health_classify cam1 {sick_stall} {sick_skip} {sick_eproto}"
    ));
    assert!(!ok, "the sick fixture leg must abort end-to-end");

    let res_stall = grep_count(
        "leg_health_dequeue_stall_grep_pattern",
        "residual_journal.txt",
    );
    let res_skip = grep_count("leg_health_emit_skip_grep_pattern", "residual_journal.txt");
    let (_o2, ok2) = run_sourced_status(&format!(
        "leg_health_classify cam1 {res_stall} {res_skip} 0"
    ));
    assert!(
        ok2,
        "the post-.481 residual fixture leg must pass end-to-end"
    );
}

/// #1133 review 🔴 regression: a report-only cap-1s band warn must NEVER abort the run under the
/// caller's `set -euo pipefail`, on an EMPTY read (failed/timed-out ssh, or a just-restarted box
/// with no cap-1s dump yet) OR a non-empty read with no cap-1s line. Before the fix, the grep
/// pipeline exited 1 on no-match and `set -e` killed the whole E2E run before the `ok:` line.
#[test]
fn cap1s_band_warn_never_aborts_the_run_under_set_e_on_an_empty_or_nomatch_read() {
    // empty read -> must reach the sentinel (not die).
    let (out, ok) =
        run_under_set_e("leg_health_cap1s_band_warn cam1 \"\"; echo REACHED_AFTER_EMPTY");
    assert!(
        ok,
        "band_warn on an empty read must NOT set-e-abort: {out:?}"
    );
    assert!(
        out.contains("REACHED_AFTER_EMPTY"),
        "the run must continue past band_warn: {out:?}"
    );
    // non-empty but no cap-1s line -> also must not abort.
    let (out2, ok2) = run_under_set_e(
        "leg_health_cap1s_band_warn cam3 'a stall line, no cap dump'; echo REACHED2",
    );
    assert!(
        ok2 && out2.contains("REACHED2"),
        "no-cap-1s-match must not abort: {out2:?}"
    );
}

/// #1133 review 🔴: the FULL per-box wiring sequence (extract -> classify -> band_warn) on a
/// failed/empty ssh read (`_lhout=""`) must classify HEALTHY and reach the `ok:` line under
/// `set -euo pipefail`, never phantom-fail. This is the exact production sequence recording-e2e.sh
/// runs per box.
#[test]
fn empty_ssh_read_flows_through_the_whole_wiring_to_ok_under_set_e() {
    // #1133 wiring: extract counts + streaming -> classify(skip,eproto) HARD -> frame_loss HARD ->
    // cap1s report-only -> stall report-only -> ok line. On an empty read everything is healthy.
    let (out, ok) = run_under_set_e(
        "OUT=''; \
         S=$(leg_health_extract STALL \"$OUT\"); K=$(leg_health_extract SKIP \"$OUT\"); E=$(leg_health_extract EPROTO \"$OUT\"); \
         STR=$(leg_health_extract_streaming \"$OUT\"); \
         if ! M=$(leg_health_classify cam1 \"$K\" \"$E\"); then echo \"ABORT:$M\"; exit 1; fi; \
         if ! M=$(leg_health_frame_loss_classify cam1 \"$STR\"); then echo \"ABORT:$M\"; exit 1; fi; \
         leg_health_cap1s_band_warn cam1 \"$(leg_health_extract_cap1s \"$OUT\")\"; \
         leg_health_dequeue_stall_report cam1 \"$S\"; \
         echo \"OK_LINE stall=$S skip=$K eproto=$E\"",
    );
    assert!(ok, "an empty ssh read must not fail the run: {out:?}");
    assert!(
        out.contains("OK_LINE stall=0 skip=0 eproto=0"),
        "empty read -> healthy, ok line reached: {out:?}"
    );
    assert!(
        !out.contains("ABORT:"),
        "must not classify an empty read as a defect: {out:?}"
    );
}

/// And the same sequence on a SICK read still ABORTS under `set -euo pipefail` (the fix must not
/// have neutered the real gate). Two independent sick shapes: a count-based one (emit-skip/EPROTO
/// over threshold) and a frame-loss one (sustained sent-vs-captured loss).
#[test]
fn sick_read_still_aborts_under_set_e() {
    // (a) count-based sick read: skip=10 (>=8), eproto=9 (>=6) — classify aborts. A high STALL
    // count is present too, but it no longer participates (#1133).
    let sick = "LEGHEALTH_STALL=12\\nLEGHEALTH_SKIP=10\\nLEGHEALTH_EPROTO=9\\nLEGHEALTH_CAP1S_BEGIN\\nx cap-1s: [62, 63]\\nLEGHEALTH_CAP1S_END";
    let (out, ok) = run_under_set_e(&format!(
        "OUT=$(printf '%b' '{sick}'); \
         K=$(leg_health_extract SKIP \"$OUT\"); E=$(leg_health_extract EPROTO \"$OUT\"); \
         if ! M=$(leg_health_classify cam1 \"$K\" \"$E\"); then echo \"ABORT_OK\"; exit 1; fi; \
         echo \"WRONGLY_PASSED\""
    ));
    assert!(!ok, "a count-based sick read must still fail the run");
    assert!(
        out.contains("ABORT_OK") && !out.contains("WRONGLY_PASSED"),
        "must abort on the count-based sick read: {out:?}"
    );

    // (b) a STALL-ONLY read (high stall, zero skip/eproto/loss) must NOT abort — the whole #1133
    // point: the DEQUEUE STALL count no longer gates. It should reach the ok line.
    let stall_only = "LEGHEALTH_STALL=24\\nLEGHEALTH_SKIP=0\\nLEGHEALTH_EPROTO=0\\nLEGHEALTH_STREAMING_BEGIN\\nLEGHEALTH_STREAMING_END";
    let (out2, ok2) = run_under_set_e(&format!(
        "OUT=$(printf '%b' '{stall_only}'); \
         S=$(leg_health_extract STALL \"$OUT\"); K=$(leg_health_extract SKIP \"$OUT\"); E=$(leg_health_extract EPROTO \"$OUT\"); \
         STR=$(leg_health_extract_streaming \"$OUT\"); \
         if ! leg_health_classify cam1 \"$K\" \"$E\"; then echo \"WRONG_ABORT_CLASSIFY\"; exit 1; fi; \
         if ! leg_health_frame_loss_classify cam1 \"$STR\"; then echo \"WRONG_ABORT_LOSS\"; exit 1; fi; \
         leg_health_dequeue_stall_report cam1 \"$S\" >/dev/null; \
         echo \"STALL_ONLY_OK\""
    ));
    assert!(
        ok2,
        "a STALL-ONLY read must NOT abort (the #1133 fix): {out2:?}"
    );
    assert!(
        out2.contains("STALL_ONLY_OK") && !out2.contains("WRONG_ABORT"),
        "high stall alone must reach the ok line: {out2:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// 5b. Composite one-ssh read builder + its parse helpers (the shape recording-e2e.sh actually
//     uses per box: ONE remote read -> extract counts -> classify + report-only cap-1s warn).
// ---------------------------------------------------------------------------------------------

#[test]
fn read_all_cmd_embeds_every_signal_read_in_one_script() {
    let out = run_ok("leg_health_read_all_cmd INV9 1000 1300 100 3700");
    assert!(
        out.contains("LEGHEALTH_STALL=$("),
        "must emit a stall count line: {out}"
    );
    assert!(
        out.contains("LEGHEALTH_SKIP=$("),
        "must emit a skip count line: {out}"
    );
    assert!(
        out.contains("LEGHEALTH_EPROTO=$("),
        "must emit an eproto count line: {out}"
    );
    assert!(
        out.contains("LEGHEALTH_CAP1S_BEGIN") && out.contains("LEGHEALTH_CAP1S_END"),
        "must delimit the cap-1s block: {out}"
    );
    assert!(
        out.contains("LEGHEALTH_STREAMING_BEGIN") && out.contains("LEGHEALTH_STREAMING_END"),
        "must delimit the #1133 frame-loss Streaming block: {out}"
    );
    assert!(
        out.contains("Streaming:"),
        "must read the Streaming lines for the frame-loss gate: {out}"
    );
    // journal reads scope to the instance + 5-min window; the kernel read uses the 1-hr window.
    assert!(
        out.contains("_SYSTEMD_INVOCATION_ID=INV9"),
        "journal reads instance-scoped: {out}"
    );
    assert!(
        out.contains("--since=@1000") && out.contains("--until=@1300"),
        "journal window: {out}"
    );
    assert!(
        out.contains("journalctl -k --since=@100 --until=@3700"),
        "kernel EPROTO window: {out}"
    );
}

#[test]
fn extract_parses_counts_and_defaults_missing_to_zero() {
    // build the multi-line read_all output on the bash side with printf '%b', then parse it.
    let out = run_ok(
        "OUT=$(printf '%b' 'LEGHEALTH_STALL=12\\nLEGHEALTH_SKIP=10\\nLEGHEALTH_EPROTO=9'); \
         echo \"$(leg_health_extract STALL \"$OUT\") $(leg_health_extract SKIP \"$OUT\") $(leg_health_extract EPROTO \"$OUT\")\"",
    );
    assert_eq!(out.trim(), "12 10 9", "must parse each count line");
    // absent field / empty output -> 0 (a truncated ssh read must not manufacture a non-zero count).
    assert_eq!(
        run_ok("leg_health_extract EPROTO 'nothing here'").trim(),
        "0"
    );
    assert_eq!(run_ok("leg_health_extract SKIP ''").trim(), "0");
}

#[test]
fn extract_cap1s_returns_only_the_lines_between_the_markers() {
    let out = run_ok(
        "OUT=$(printf '%b' 'LEGHEALTH_STALL=0\\nLEGHEALTH_CAP1S_BEGIN\\nfoo cap-1s: [62, 63]\\nbar cap-1s: [60, 61]\\nLEGHEALTH_CAP1S_END'); \
         leg_health_extract_cap1s \"$OUT\"",
    );
    assert!(
        out.contains("foo cap-1s: [62, 63]") && out.contains("bar cap-1s: [60, 61]"),
        "must return the cap-1s lines: {out:?}"
    );
    assert!(
        !out.contains("LEGHEALTH_"),
        "must strip the marker lines: {out:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// 6. Wiring guard: recording-e2e.sh sources the lib AND has a leg-health preflight block that
//    respects offline-ack. (Static-anchor style, like harness_capture_rate_guard.rs's wiring test.)
// ---------------------------------------------------------------------------------------------

fn read_recording_e2e() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/recording-e2e.sh");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read recording-e2e.sh: {e}"))
}

#[test]
fn recording_e2e_sources_the_lib_and_wires_the_preflight() {
    let s = read_recording_e2e();
    assert!(
        s.contains("lib/leg-health-guard.sh"),
        "recording-e2e.sh must source the new lib"
    );
    assert!(
        s.contains("leg-health preflight"),
        "must have a [0/8] leg-health preflight step"
    );
    assert!(
        s.contains("leg_health_classify"),
        "must call the decision fn"
    );
    // must respect offline-ack (a box acked-offline is not checked) — reuses cambox_offline_ack.
    assert!(
        s.contains("cambox_offline_ack_is_acked"),
        "must skip acked-offline boxes"
    );
}
